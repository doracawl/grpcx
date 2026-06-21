// Shared-memory transport — client-side N-API binding.
//
// Mirrors the Rust `transport_shmem` layout
// byte-for-byte:
//
//   SLOT_STRIDE       = 4096
//   SLOTS_PER_RING    = 16   (v1.3 bump: was 4; gives the
//                             busy-poll producer enough
//                             ring depth to stop spinning
//                             on Full every 4 pushes)
//   CACHE_LINE        = 64
//   CONTROL_BYTES     = 256 (4 cache lines)
//   RING_DATA_BYTES   = 65536
//   C2S_DATA_OFFSET   = 256
//   S2C_DATA_OFFSET   = 65792
//   MIN_REGION_BYTES  = 131328
//
// Wire rendezvous (T3.1):
//   client connects AF_UNIX path → writes "GXR1" magic
//   server replies with 16-byte payload (region_len u64
//   LE + slot_count u32 LE + slot_stride u32 LE) plus
//   three fds via SCM_RIGHTS:
//     fds[0] = region fd
//     fds[1] = wait fd (read end of server→client pipe)
//     fds[2] = signal fd (write end of client→server pipe)

#pragma once

#include <napi.h>

namespace grpcx {
namespace shmem {

constexpr std::size_t kSlotStride = 4096;
constexpr std::size_t kSlotsPerRing = 16;
constexpr std::size_t kCacheLine = 64;
constexpr std::size_t kControlBytes = 4 * kCacheLine;
constexpr std::size_t kRingDataBytes = kSlotsPerRing * kSlotStride;
constexpr std::size_t kC2sDataOffset = kControlBytes;
constexpr std::size_t kS2cDataOffset = kControlBytes + kRingDataBytes;
constexpr std::size_t kMinRegionBytes = kControlBytes + 2 * kRingDataBytes;
constexpr std::size_t kHeaderLen = 16;

// connectShmem(path: string) ->
//   { regionBuffer: Buffer,
//     waitFd: number,
//     signalFd: number,
//     slotCount: number,
//     slotStride: number,
//     regionLen: number }
Napi::Value ConnectShmem(const Napi::CallbackInfo& info);

// ringPush(region: Buffer, role: 0|1, header: HeaderObj, payload: Buffer)
//   role 0 = client (push to c2s)
//   role 1 = server (push to s2c) — exists for symmetry; v1 caller uses 0
// returns: { ok: bool, full: bool, tooLarge: bool }
Napi::Value RingPush(const Napi::CallbackInfo& info);

// ringPop(region: Buffer, role: 0|1, outPayload: Buffer)
//   role 0 = client (pop from s2c)
//   role 1 = server (pop from c2s)
// outPayload must be at least SLOT_STRIDE bytes.
// returns: { ok: bool, empty: bool, header: HeaderObj,
//            payloadLen: number }
Napi::Value RingPop(const Napi::CallbackInfo& info);

// wakeWrite(fd: number) — writes one byte to the wake
// fd, ignoring EAGAIN/EWOULDBLOCK.
Napi::Value WakeWrite(const Napi::CallbackInfo& info);

// wakeRead(fd: number) — blocks until one byte is read
// from the wake fd. Drains exactly one byte.
Napi::Value WakeRead(const Napi::CallbackInfo& info);

// closeFd(fd: number) — convenience for releasing a raw
// fd handed back from connectShmem.
Napi::Value CloseFd(const Napi::CallbackInfo& info);

// munmapBuffer — finalizer registered alongside the
// region buffer; exposed only via the buffer's own
// finalize callback. No N-API surface.

}  // namespace shmem
}  // namespace grpcx
