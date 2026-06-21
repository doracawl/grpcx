// N-API binding entry — exposes the C++ wire codec and
// the minimal protobuf encoder / decoder to JavaScript.

#include <napi.h>

#include <cstdint>
#include <vector>

#include "protobuf_codec.h"
#include "transport_shmem.h"
#include "wire_v2.h"

namespace {

using grpcx::wire_v2::FrameType;
using grpcx::wire_v2::Header;
using grpcx::wire_v2::MetadataEntry;
using grpcx::wire_v2::WireError;

constexpr std::size_t kHeaderLen = grpcx::wire_v2::kHeaderLen;

std::string WireErrorMessage(WireError e) {
  switch (e) {
    case WireError::Ok:
      return "ok";
    case WireError::BufferTooShort:
      return "wire_v2: buffer too short";
    case WireError::UnknownFrameType:
      return "wire_v2: unknown frame type";
    case WireError::InvalidMetadataNameLen:
      return "wire_v2: invalid metadata name_len";
    case WireError::TruncatedMetadata:
      return "wire_v2: truncated metadata block";
  }
  return "wire_v2: unknown error";
}

// ---- encodeHeader(header: HeaderObj): Buffer ----
Napi::Value EncodeHeader(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsObject()) {
    throw Napi::TypeError::New(env, "encodeHeader expects one object argument");
  }
  Napi::Object obj = info[0].As<Napi::Object>();
  Header h;
  h.stream_id = obj.Get("streamId").ToNumber().Uint32Value();
  h.frame_type = static_cast<FrameType>(
      obj.Get("frameType").ToNumber().Uint32Value() & 0xff);
  h.flags = static_cast<std::uint8_t>(
      obj.Get("flags").ToNumber().Uint32Value() & 0xff);
  h.metadata_count = static_cast<std::uint16_t>(
      obj.Get("metadataCount").ToNumber().Uint32Value() & 0xffff);
  h.payload_len = obj.Get("payloadLen").ToNumber().Uint32Value();

  std::uint8_t buf[kHeaderLen];
  grpcx::wire_v2::EncodeHeader(h, buf);
  return Napi::Buffer<std::uint8_t>::Copy(env, buf, kHeaderLen);
}

// ---- decodeHeader(buf: Buffer): HeaderObj ----
Napi::Value DecodeHeader(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || (!info[0].IsBuffer() && !info[0].IsTypedArray())) {
    throw Napi::TypeError::New(env, "decodeHeader expects a Buffer / Uint8Array");
  }
  const std::uint8_t* data;
  std::size_t len;
  if (info[0].IsBuffer()) {
    Napi::Buffer<std::uint8_t> b = info[0].As<Napi::Buffer<std::uint8_t>>();
    data = b.Data();
    len = b.Length();
  } else {
    Napi::Uint8Array a = info[0].As<Napi::Uint8Array>();
    data = a.Data();
    len = a.ByteLength();
  }
  Header h;
  WireError err = grpcx::wire_v2::DecodeHeader(data, len, &h);
  if (err != WireError::Ok) {
    throw Napi::Error::New(env, WireErrorMessage(err));
  }
  Napi::Object out = Napi::Object::New(env);
  out.Set("streamId", Napi::Number::New(env, h.stream_id));
  out.Set("frameType",
          Napi::Number::New(env, static_cast<std::uint8_t>(h.frame_type)));
  out.Set("flags", Napi::Number::New(env, h.flags));
  out.Set("metadataCount", Napi::Number::New(env, h.metadata_count));
  out.Set("payloadLen", Napi::Number::New(env, h.payload_len));
  return out;
}

// ---- encodeMetadataBlock(entries: { name: Uint8Array, value: Uint8Array }[])
//      → Buffer ----
Napi::Value EncodeMetadataBlock(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsArray()) {
    throw Napi::TypeError::New(env, "encodeMetadataBlock expects an array");
  }
  Napi::Array arr = info[0].As<Napi::Array>();
  std::vector<MetadataEntry> entries;
  entries.reserve(arr.Length());
  for (std::uint32_t i = 0; i < arr.Length(); ++i) {
    Napi::Object entry = arr.Get(i).As<Napi::Object>();
    Napi::Uint8Array name = entry.Get("name").As<Napi::Uint8Array>();
    Napi::Uint8Array value = entry.Get("value").As<Napi::Uint8Array>();
    MetadataEntry me;
    me.name.assign(name.Data(), name.Data() + name.ByteLength());
    me.value.assign(value.Data(), value.Data() + value.ByteLength());
    entries.push_back(std::move(me));
  }
  std::size_t total = grpcx::wire_v2::BlockWireLen(entries);
  std::vector<std::uint8_t> buf(total);
  grpcx::wire_v2::EncodeMetadataBlock(entries, buf.data());
  return Napi::Buffer<std::uint8_t>::Copy(env, buf.data(), buf.size());
}

// ---- decodeMetadataBlock(buf: Buffer, count: number)
//      → { entries: [...], consumed: number } ----
Napi::Value DecodeMetadataBlock(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 2 || (!info[0].IsBuffer() && !info[0].IsTypedArray()) ||
      !info[1].IsNumber()) {
    throw Napi::TypeError::New(
        env, "decodeMetadataBlock(buf, count) signature mismatch");
  }
  const std::uint8_t* data;
  std::size_t len;
  if (info[0].IsBuffer()) {
    Napi::Buffer<std::uint8_t> b = info[0].As<Napi::Buffer<std::uint8_t>>();
    data = b.Data();
    len = b.Length();
  } else {
    Napi::Uint8Array a = info[0].As<Napi::Uint8Array>();
    data = a.Data();
    len = a.ByteLength();
  }
  std::uint16_t count = static_cast<std::uint16_t>(
      info[1].ToNumber().Uint32Value() & 0xffff);

  std::vector<MetadataEntry> entries;
  std::size_t consumed = 0;
  WireError err = grpcx::wire_v2::DecodeMetadataBlock(data, len, count,
                                                     &entries, &consumed);
  if (err != WireError::Ok) {
    throw Napi::Error::New(env, WireErrorMessage(err));
  }
  Napi::Array out_arr = Napi::Array::New(env, entries.size());
  for (std::size_t i = 0; i < entries.size(); ++i) {
    Napi::Object e = Napi::Object::New(env);
    Napi::Uint8Array n = Napi::Uint8Array::New(env, entries[i].name.size());
    std::memcpy(n.Data(), entries[i].name.data(), entries[i].name.size());
    e.Set("name", n);
    Napi::Uint8Array v = Napi::Uint8Array::New(env, entries[i].value.size());
    std::memcpy(v.Data(), entries[i].value.data(), entries[i].value.size());
    e.Set("value", v);
    out_arr.Set(static_cast<std::uint32_t>(i), e);
  }
  Napi::Object result = Napi::Object::New(env);
  result.Set("entries", out_arr);
  result.Set("consumed", Napi::Number::New(env, consumed));
  return result;
}

Napi::Object Init(Napi::Env env, Napi::Object exports) {
  exports.Set("HEADER_LEN", Napi::Number::New(env, kHeaderLen));
  exports.Set("encodeHeader",
              Napi::Function::New(env, EncodeHeader, "encodeHeader"));
  exports.Set("decodeHeader",
              Napi::Function::New(env, DecodeHeader, "decodeHeader"));
  exports.Set(
      "encodeMetadataBlock",
      Napi::Function::New(env, EncodeMetadataBlock, "encodeMetadataBlock"));
  exports.Set(
      "decodeMetadataBlock",
      Napi::Function::New(env, DecodeMetadataBlock, "decodeMetadataBlock"));
  exports.Set("encodeMessage",
              Napi::Function::New(env, grpcx::pb::EncodeMessage,
                                  "encodeMessage"));
  exports.Set("decodeMessage",
              Napi::Function::New(env, grpcx::pb::DecodeMessage,
                                  "decodeMessage"));

  // transport-shmem (T3.3) — client-side primitives.
  exports.Set("connectShmem",
              Napi::Function::New(env, grpcx::shmem::ConnectShmem,
                                  "connectShmem"));
  exports.Set("ringPush",
              Napi::Function::New(env, grpcx::shmem::RingPush, "ringPush"));
  exports.Set("ringPop",
              Napi::Function::New(env, grpcx::shmem::RingPop, "ringPop"));
  exports.Set("wakeWrite",
              Napi::Function::New(env, grpcx::shmem::WakeWrite, "wakeWrite"));
  exports.Set("wakeRead",
              Napi::Function::New(env, grpcx::shmem::WakeRead, "wakeRead"));
  exports.Set("closeFd",
              Napi::Function::New(env, grpcx::shmem::CloseFd, "closeFd"));
  exports.Set("SHMEM_SLOT_STRIDE",
              Napi::Number::New(env, grpcx::shmem::kSlotStride));
  exports.Set("SHMEM_HEADER_LEN",
              Napi::Number::New(env, grpcx::shmem::kHeaderLen));
  return exports;
}

}  // namespace

NODE_API_MODULE(grpcx_native, Init)
