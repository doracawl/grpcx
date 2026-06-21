// Shared-memory transport — client-side implementation.

#include "transport_shmem.h"

#include <atomic>
#include <cerrno>
#include <cstdint>
#include <cstring>
#include <string>

#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include "wire_v2.h"

namespace grpcx {
namespace shmem {

namespace {

constexpr const char kMagic[4] = {'G', 'X', 'R', '1'};
constexpr std::size_t kReplyBytes = 8 + 4 + 4;  // region_len u64 + counts

// Cache-line offsets in the control block.
constexpr std::size_t kC2sWriterOff = 0;
constexpr std::size_t kC2sReaderOff = kCacheLine;
constexpr std::size_t kS2cWriterOff = 2 * kCacheLine;
constexpr std::size_t kS2cReaderOff = 3 * kCacheLine;

struct RingView {
  std::atomic<std::uint64_t>* writer_pos;
  std::atomic<std::uint64_t>* reader_pos;
  std::uint8_t* slots_base;
};

// `role 0` = client; `kind=send` is the c2s ring.
// `role 1` = server; `kind=send` is the s2c ring.
RingView GetSendRing(std::uint8_t* region, int role) {
  if (role == 0) {
    return RingView{
        reinterpret_cast<std::atomic<std::uint64_t>*>(region + kC2sWriterOff),
        reinterpret_cast<std::atomic<std::uint64_t>*>(region + kC2sReaderOff),
        region + kC2sDataOffset,
    };
  }
  return RingView{
      reinterpret_cast<std::atomic<std::uint64_t>*>(region + kS2cWriterOff),
      reinterpret_cast<std::atomic<std::uint64_t>*>(region + kS2cReaderOff),
      region + kS2cDataOffset,
  };
}

RingView GetRecvRing(std::uint8_t* region, int role) {
  if (role == 0) {
    return RingView{
        reinterpret_cast<std::atomic<std::uint64_t>*>(region + kS2cWriterOff),
        reinterpret_cast<std::atomic<std::uint64_t>*>(region + kS2cReaderOff),
        region + kS2cDataOffset,
    };
  }
  return RingView{
      reinterpret_cast<std::atomic<std::uint64_t>*>(region + kC2sWriterOff),
      reinterpret_cast<std::atomic<std::uint64_t>*>(region + kC2sReaderOff),
      region + kC2sDataOffset,
  };
}

// SCM_RIGHTS CMSG layout — matches the Rust rendezvous
// module. `struct cmsghdr` differs across platforms:
// - Linux 64-bit glibc: cmsg_len is size_t (8 B). Header
//   16 B + 12 B fd data, aligned to size_t → 32 B total.
// - Darwin / FreeBSD: cmsg_len is socklen_t (4 B). Header
//   12 B + 12 B fd data, aligned to int → 24 B total.
//
// Mismatching this on Linux silently undersizes
// msg_controllen, the kernel sets MSG_CTRUNC and the fds
// never arrive (`recvmsg failed: Success`).
#if defined(__linux__)
constexpr std::size_t kCmsgBufLen3Fd = 32;
#else
constexpr std::size_t kCmsgBufLen3Fd = 24;
#endif

void MunmapFinalizer(Napi::Env env, std::uint8_t* data, std::size_t* len_box) {
  (void)env;
  if (data && *len_box > 0) {
    ::munmap(data, *len_box);
  }
  delete len_box;
}

}  // namespace

Napi::Value ConnectShmem(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsString()) {
    throw Napi::TypeError::New(env, "connectShmem(path: string)");
  }
  std::string path = info[0].ToString().Utf8Value();

  int sockfd = ::socket(AF_UNIX, SOCK_STREAM, 0);
  if (sockfd < 0) {
    throw Napi::Error::New(env, std::string("socket(AF_UNIX) failed: ") +
                                    std::strerror(errno));
  }

  struct sockaddr_un addr;
  std::memset(&addr, 0, sizeof(addr));
  addr.sun_family = AF_UNIX;
  if (path.size() >= sizeof(addr.sun_path)) {
    ::close(sockfd);
    throw Napi::TypeError::New(env, "connectShmem: path too long");
  }
  std::memcpy(addr.sun_path, path.data(), path.size());

  if (::connect(sockfd, reinterpret_cast<struct sockaddr*>(&addr),
                sizeof(addr)) != 0) {
    int e = errno;
    ::close(sockfd);
    throw Napi::Error::New(env, std::string("connect failed: ") +
                                    std::strerror(e));
  }

  // Send 4-byte magic.
  ssize_t n = ::send(sockfd, kMagic, sizeof(kMagic), 0);
  if (n != static_cast<ssize_t>(sizeof(kMagic))) {
    int e = errno;
    ::close(sockfd);
    throw Napi::Error::New(env, std::string("magic send failed: ") +
                                    std::strerror(e));
  }

  // recvmsg reply + 3 fds.
  std::uint8_t reply[kReplyBytes];
  std::uint8_t cmsg_buf[kCmsgBufLen3Fd];
  struct iovec iov;
  iov.iov_base = reply;
  iov.iov_len = sizeof(reply);
  struct msghdr msg;
  std::memset(&msg, 0, sizeof(msg));
  msg.msg_iov = &iov;
  msg.msg_iovlen = 1;
  msg.msg_control = cmsg_buf;
  msg.msg_controllen = sizeof(cmsg_buf);

  n = ::recvmsg(sockfd, &msg, 0);
  if (n != static_cast<ssize_t>(sizeof(reply))) {
    int e = errno;
    ::close(sockfd);
    throw Napi::Error::New(env, std::string("recvmsg failed: ") +
                                    std::strerror(e));
  }
  ::close(sockfd);  // rendezvous done.

  struct cmsghdr* cmsg = CMSG_FIRSTHDR(&msg);
  if (!cmsg || cmsg->cmsg_level != SOL_SOCKET ||
      cmsg->cmsg_type != SCM_RIGHTS ||
      cmsg->cmsg_len < CMSG_LEN(sizeof(int) * 3)) {
    throw Napi::Error::New(env, "rendezvous: missing SCM_RIGHTS payload");
  }
  int fds[3];
  std::memcpy(fds, CMSG_DATA(cmsg), sizeof(fds));
  int region_fd = fds[0];
  int wait_fd = fds[1];
  int signal_fd = fds[2];

  // Decode reply.
  std::uint64_t region_len = 0;
  std::memcpy(&region_len, reply, 8);
  std::uint32_t slot_count = 0;
  std::memcpy(&slot_count, reply + 8, 4);
  std::uint32_t slot_stride = 0;
  std::memcpy(&slot_stride, reply + 12, 4);
  if (region_len < kMinRegionBytes) {
    ::close(region_fd);
    ::close(wait_fd);
    ::close(signal_fd);
    throw Napi::Error::New(env, "rendezvous: region smaller than ring layout");
  }

  void* addr_p =
      ::mmap(nullptr, region_len, PROT_READ | PROT_WRITE, MAP_SHARED, region_fd, 0);
  ::close(region_fd);
  if (addr_p == MAP_FAILED) {
    int e = errno;
    ::close(wait_fd);
    ::close(signal_fd);
    throw Napi::Error::New(env, std::string("mmap failed: ") +
                                    std::strerror(e));
  }

  auto* len_box = new std::size_t(static_cast<std::size_t>(region_len));
  Napi::Buffer<std::uint8_t> region_buf =
      Napi::Buffer<std::uint8_t>::New(env, reinterpret_cast<std::uint8_t*>(addr_p),
                                      static_cast<std::size_t>(region_len),
                                      MunmapFinalizer, len_box);

  Napi::Object out = Napi::Object::New(env);
  out.Set("regionBuffer", region_buf);
  out.Set("waitFd", Napi::Number::New(env, wait_fd));
  out.Set("signalFd", Napi::Number::New(env, signal_fd));
  out.Set("slotCount", Napi::Number::New(env, slot_count));
  out.Set("slotStride", Napi::Number::New(env, slot_stride));
  out.Set("regionLen", Napi::Number::New(env, region_len));
  return out;
}

Napi::Value RingPush(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 4 || !info[0].IsBuffer() || !info[1].IsNumber() ||
      !info[2].IsObject() ||
      (!info[3].IsBuffer() && !info[3].IsTypedArray())) {
    throw Napi::TypeError::New(env,
                               "ringPush(region, role, header, payload)");
  }
  Napi::Buffer<std::uint8_t> region = info[0].As<Napi::Buffer<std::uint8_t>>();
  int role = info[1].ToNumber().Int32Value();
  Napi::Object header_obj = info[2].As<Napi::Object>();
  std::uint8_t* payload_data;
  std::size_t payload_len;
  if (info[3].IsBuffer()) {
    Napi::Buffer<std::uint8_t> b = info[3].As<Napi::Buffer<std::uint8_t>>();
    payload_data = b.Data();
    payload_len = b.Length();
  } else {
    Napi::Uint8Array a = info[3].As<Napi::Uint8Array>();
    payload_data = a.Data();
    payload_len = a.ByteLength();
  }
  if (payload_len + kHeaderLen > kSlotStride) {
    Napi::Object res = Napi::Object::New(env);
    res.Set("ok", Napi::Boolean::New(env, false));
    res.Set("full", Napi::Boolean::New(env, false));
    res.Set("tooLarge", Napi::Boolean::New(env, true));
    return res;
  }

  RingView ring = GetSendRing(region.Data(), role);
  std::uint64_t rpos = ring.reader_pos->load(std::memory_order_acquire);
  std::uint64_t wpos = ring.writer_pos->load(std::memory_order_relaxed);
  if (wpos - rpos >= kSlotsPerRing) {
    Napi::Object res = Napi::Object::New(env);
    res.Set("ok", Napi::Boolean::New(env, false));
    res.Set("full", Napi::Boolean::New(env, true));
    res.Set("tooLarge", Napi::Boolean::New(env, false));
    return res;
  }

  // Encode header from JS object.
  grpcx::wire_v2::Header hdr;
  hdr.stream_id = header_obj.Get("streamId").ToNumber().Uint32Value();
  hdr.frame_type = static_cast<grpcx::wire_v2::FrameType>(
      header_obj.Get("frameType").ToNumber().Uint32Value() & 0xff);
  hdr.flags = static_cast<std::uint8_t>(
      header_obj.Get("flags").ToNumber().Uint32Value() & 0xff);
  hdr.metadata_count = static_cast<std::uint16_t>(
      header_obj.Get("metadataCount").ToNumber().Uint32Value() & 0xffff);
  hdr.payload_len = header_obj.Get("payloadLen").ToNumber().Uint32Value();

  std::size_t slot_idx = static_cast<std::size_t>(wpos) & (kSlotsPerRing - 1);
  std::uint8_t* slot = ring.slots_base + slot_idx * kSlotStride;
  grpcx::wire_v2::EncodeHeader(hdr, slot);
  if (payload_len > 0) {
    std::memcpy(slot + kHeaderLen, payload_data, payload_len);
  }

  ring.writer_pos->store(wpos + 1, std::memory_order_release);

  Napi::Object res = Napi::Object::New(env);
  res.Set("ok", Napi::Boolean::New(env, true));
  res.Set("full", Napi::Boolean::New(env, false));
  res.Set("tooLarge", Napi::Boolean::New(env, false));
  return res;
}

Napi::Value RingPop(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 3 || !info[0].IsBuffer() || !info[1].IsNumber() ||
      (!info[2].IsBuffer() && !info[2].IsTypedArray())) {
    throw Napi::TypeError::New(env, "ringPop(region, role, outPayload)");
  }
  Napi::Buffer<std::uint8_t> region = info[0].As<Napi::Buffer<std::uint8_t>>();
  int role = info[1].ToNumber().Int32Value();
  std::uint8_t* out_data;
  std::size_t out_cap;
  if (info[2].IsBuffer()) {
    Napi::Buffer<std::uint8_t> b = info[2].As<Napi::Buffer<std::uint8_t>>();
    out_data = b.Data();
    out_cap = b.Length();
  } else {
    Napi::Uint8Array a = info[2].As<Napi::Uint8Array>();
    out_data = a.Data();
    out_cap = a.ByteLength();
  }

  RingView ring = GetRecvRing(region.Data(), role);
  std::uint64_t wpos = ring.writer_pos->load(std::memory_order_acquire);
  std::uint64_t rpos = ring.reader_pos->load(std::memory_order_relaxed);
  if (wpos == rpos) {
    Napi::Object res = Napi::Object::New(env);
    res.Set("ok", Napi::Boolean::New(env, false));
    res.Set("empty", Napi::Boolean::New(env, true));
    return res;
  }

  std::size_t slot_idx = static_cast<std::size_t>(rpos) & (kSlotsPerRing - 1);
  std::uint8_t* slot = ring.slots_base + slot_idx * kSlotStride;

  grpcx::wire_v2::Header hdr;
  if (grpcx::wire_v2::DecodeHeader(slot, kSlotStride, &hdr) !=
      grpcx::wire_v2::WireError::Ok) {
    throw Napi::Error::New(env, "ringPop: corrupt slot header");
  }
  std::size_t plen = static_cast<std::size_t>(hdr.payload_len);
  if (plen + kHeaderLen > kSlotStride) {
    throw Napi::Error::New(env, "ringPop: payload exceeds slot");
  }
  if (plen > out_cap) {
    throw Napi::Error::New(env, "ringPop: outPayload buffer too small");
  }
  if (plen > 0) {
    std::memcpy(out_data, slot + kHeaderLen, plen);
  }
  ring.reader_pos->store(rpos + 1, std::memory_order_release);

  Napi::Object header_out = Napi::Object::New(env);
  header_out.Set("streamId", Napi::Number::New(env, hdr.stream_id));
  header_out.Set(
      "frameType",
      Napi::Number::New(env, static_cast<std::uint8_t>(hdr.frame_type)));
  header_out.Set("flags", Napi::Number::New(env, hdr.flags));
  header_out.Set("metadataCount", Napi::Number::New(env, hdr.metadata_count));
  header_out.Set("payloadLen", Napi::Number::New(env, hdr.payload_len));

  Napi::Object res = Napi::Object::New(env);
  res.Set("ok", Napi::Boolean::New(env, true));
  res.Set("empty", Napi::Boolean::New(env, false));
  res.Set("header", header_out);
  res.Set("payloadLen", Napi::Number::New(env, plen));
  return res;
}

Napi::Value WakeWrite(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsNumber()) {
    throw Napi::TypeError::New(env, "wakeWrite(fd: number)");
  }
  int fd = info[0].ToNumber().Int32Value();
  std::uint8_t b = 1;
  ssize_t n = ::write(fd, &b, 1);
  if (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK) {
    throw Napi::Error::New(env, std::string("wakeWrite failed: ") +
                                    std::strerror(errno));
  }
  return env.Undefined();
}

Napi::Value WakeRead(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsNumber()) {
    throw Napi::TypeError::New(env, "wakeRead(fd: number)");
  }
  int fd = info[0].ToNumber().Int32Value();
  std::uint8_t b;
  ssize_t n;
  do {
    n = ::read(fd, &b, 1);
  } while (n < 0 && errno == EINTR);
  if (n != 1) {
    throw Napi::Error::New(env, std::string("wakeRead failed: ") +
                                    std::strerror(errno));
  }
  return env.Undefined();
}

Napi::Value CloseFd(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsNumber()) {
    throw Napi::TypeError::New(env, "closeFd(fd: number)");
  }
  int fd = info[0].ToNumber().Int32Value();
  ::close(fd);
  return env.Undefined();
}

}  // namespace shmem
}  // namespace grpcx
