// Minimal protobuf wire encoder / decoder driven by a
// `FieldDescriptor[]` table. v1 subset — scalar fields
// (string / bytes / int32 / uint32 / int64 / uint64 /
// bool) only. Float / double / fixed* / sint* / nested
// messages / maps / enums / repeated remain T2.2.x
// follow-ups.

#pragma once

#include <napi.h>

namespace grpcx {
namespace pb {

// JS: encodeMessage(descriptor: FieldDescriptor[],
//                   msg: object) -> Buffer
Napi::Value EncodeMessage(const Napi::CallbackInfo& info);

// JS: decodeMessage(descriptor: FieldDescriptor[],
//                   buf: Buffer) -> object
Napi::Value DecodeMessage(const Napi::CallbackInfo& info);

}  // namespace pb
}  // namespace grpcx
