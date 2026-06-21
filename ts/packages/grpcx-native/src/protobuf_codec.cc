// Minimal protobuf wire-format codec. Drives encode /
// decode from a `FieldDescriptor[]` table emitted by
// `grpcx-pb::codegen_ts`.
//
// v1 scalar subset:
//   string, bytes, int32, uint32, int64, uint64, bool.
//
// Out of scope for v1 (panics with descriptive error if
// encountered):
//   float, double, fixed*, sint*, message (nested),
//   enum, map, repeated.

#include "protobuf_codec.h"

#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

namespace grpcx {
namespace pb {

namespace {

constexpr std::uint8_t kWireVarint = 0;
constexpr std::uint8_t kWireFixed64 = 1;  // unused in v1 subset
constexpr std::uint8_t kWireLengthDelim = 2;
constexpr std::uint8_t kWireFixed32 = 5;  // unused in v1 subset

// ---- varint primitives ----

void WriteVarint(std::uint64_t value, std::vector<std::uint8_t>* out) {
  while (value >= 0x80) {
    out->push_back(static_cast<std::uint8_t>((value & 0x7f) | 0x80));
    value >>= 7;
  }
  out->push_back(static_cast<std::uint8_t>(value));
}

bool ReadVarint(const std::uint8_t* buf, std::size_t buf_len,
                std::size_t* off, std::uint64_t* out) {
  std::uint64_t result = 0;
  int shift = 0;
  while (*off < buf_len) {
    std::uint8_t b = buf[*off];
    *off += 1;
    result |= static_cast<std::uint64_t>(b & 0x7f) << shift;
    if ((b & 0x80) == 0) {
      *out = result;
      return true;
    }
    shift += 7;
    if (shift >= 64) {
      return false;
    }
  }
  return false;
}

void WriteTag(std::uint32_t field_num, std::uint8_t wire_type,
              std::vector<std::uint8_t>* out) {
  WriteVarint((static_cast<std::uint64_t>(field_num) << 3) |
                  static_cast<std::uint64_t>(wire_type),
              out);
}

void WriteLengthDelim(const std::uint8_t* data, std::size_t len,
                      std::vector<std::uint8_t>* out) {
  WriteVarint(static_cast<std::uint64_t>(len), out);
  out->insert(out->end(), data, data + len);
}

// ---- type identification ----

enum class FieldKind {
  kString,
  kBytes,
  kInt32,
  kUint32,
  kInt64,
  kUint64,
  kBool,
  kUnsupported,
};

FieldKind ParseKind(const std::string& s) {
  if (s == "string") return FieldKind::kString;
  if (s == "bytes") return FieldKind::kBytes;
  if (s == "int32" || s == "sint32") return FieldKind::kInt32;
  if (s == "uint32") return FieldKind::kUint32;
  if (s == "int64" || s == "sint64") return FieldKind::kInt64;
  if (s == "uint64") return FieldKind::kUint64;
  if (s == "bool") return FieldKind::kBool;
  return FieldKind::kUnsupported;
}

std::uint8_t WireTypeFor(FieldKind k) {
  switch (k) {
    case FieldKind::kString:
    case FieldKind::kBytes:
      return kWireLengthDelim;
    case FieldKind::kInt32:
    case FieldKind::kUint32:
    case FieldKind::kInt64:
    case FieldKind::kUint64:
    case FieldKind::kBool:
      return kWireVarint;
    default:
      return 0xff;
  }
}

struct Descriptor {
  std::uint32_t tag;
  std::string name;
  FieldKind kind;
  // label is unused in v1 (no repeated support); reserved
  // for future use.
  std::string label;
};

std::vector<Descriptor> ParseDescriptor(const Napi::Array& arr) {
  std::vector<Descriptor> out;
  out.reserve(arr.Length());
  for (std::uint32_t i = 0; i < arr.Length(); ++i) {
    Napi::Value v = arr[i];
    if (!v.IsObject()) {
      throw Napi::TypeError::New(arr.Env(),
                                 "FieldDescriptor[] entry must be an object");
    }
    Napi::Object obj = v.As<Napi::Object>();
    Descriptor d;
    d.tag = obj.Get("tag").ToNumber().Uint32Value();
    d.name = obj.Get("name").ToString().Utf8Value();
    d.kind = ParseKind(obj.Get("type").ToString().Utf8Value());
    d.label = obj.Get("label").ToString().Utf8Value();
    out.push_back(d);
  }
  return out;
}

// ---- encode ----

void EncodeField(const Descriptor& d, const Napi::Value& fv,
                 std::vector<std::uint8_t>* out, Napi::Env env) {
  std::uint8_t wire_type = WireTypeFor(d.kind);
  if (wire_type == 0xff) {
    throw Napi::TypeError::New(env,
                               "grpcx-native v1 protobuf codec does not "
                               "support field type: " +
                                   d.label);
  }

  switch (d.kind) {
    case FieldKind::kString: {
      std::string s = fv.ToString().Utf8Value();
      WriteTag(d.tag, kWireLengthDelim, out);
      WriteLengthDelim(reinterpret_cast<const std::uint8_t*>(s.data()),
                       s.size(), out);
      break;
    }
    case FieldKind::kBytes: {
      if (!fv.IsBuffer() && !fv.IsTypedArray()) {
        throw Napi::TypeError::New(env, "expected Buffer / Uint8Array for `" +
                                            d.name + "`");
      }
      Napi::Uint8Array arr = fv.As<Napi::Uint8Array>();
      WriteTag(d.tag, kWireLengthDelim, out);
      WriteLengthDelim(arr.Data(), arr.ByteLength(), out);
      break;
    }
    case FieldKind::kInt32: {
      std::int32_t n = fv.ToNumber().Int32Value();
      WriteTag(d.tag, kWireVarint, out);
      // proto3 int32 wire encoding: cast to uint64 (sign-
      // extended); decoders truncate to 32 bits.
      WriteVarint(static_cast<std::uint64_t>(static_cast<std::int64_t>(n)),
                  out);
      break;
    }
    case FieldKind::kUint32: {
      std::uint32_t n = fv.ToNumber().Uint32Value();
      WriteTag(d.tag, kWireVarint, out);
      WriteVarint(static_cast<std::uint64_t>(n), out);
      break;
    }
    case FieldKind::kInt64: {
      bool lossless = false;
      std::int64_t n = fv.As<Napi::BigInt>().Int64Value(&lossless);
      WriteTag(d.tag, kWireVarint, out);
      WriteVarint(static_cast<std::uint64_t>(n), out);
      break;
    }
    case FieldKind::kUint64: {
      bool lossless = false;
      std::uint64_t n = fv.As<Napi::BigInt>().Uint64Value(&lossless);
      WriteTag(d.tag, kWireVarint, out);
      WriteVarint(n, out);
      break;
    }
    case FieldKind::kBool: {
      bool b = fv.ToBoolean().Value();
      WriteTag(d.tag, kWireVarint, out);
      WriteVarint(b ? 1u : 0u, out);
      break;
    }
    case FieldKind::kUnsupported:
      // already handled above
      break;
  }
}

// ---- decode ----

void DecodeField(const std::vector<Descriptor>& desc,
                 std::uint32_t field_num, std::uint8_t wire_type,
                 const std::uint8_t* buf, std::size_t buf_len,
                 std::size_t* off, Napi::Object& msg, Napi::Env env) {
  // Skip unknown fields per protobuf spec.
  const Descriptor* d = nullptr;
  for (const auto& entry : desc) {
    if (entry.tag == field_num) {
      d = &entry;
      break;
    }
  }
  if (d == nullptr) {
    // Skip per wire_type.
    if (wire_type == kWireVarint) {
      std::uint64_t dummy;
      if (!ReadVarint(buf, buf_len, off, &dummy)) {
        throw Napi::Error::New(env, "varint underflow while skipping field");
      }
    } else if (wire_type == kWireLengthDelim) {
      std::uint64_t len;
      if (!ReadVarint(buf, buf_len, off, &len) || *off + len > buf_len) {
        throw Napi::Error::New(env, "length-delim underflow while skipping");
      }
      *off += len;
    } else {
      throw Napi::Error::New(env,
                             "grpcx-native protobuf codec: cannot skip wire "
                             "type " + std::to_string(wire_type));
    }
    return;
  }

  switch (d->kind) {
    case FieldKind::kString: {
      std::uint64_t len;
      if (!ReadVarint(buf, buf_len, off, &len) || *off + len > buf_len) {
        throw Napi::Error::New(env, "string length-delim underflow");
      }
      std::string s(reinterpret_cast<const char*>(buf + *off), len);
      *off += len;
      msg.Set(d->name, Napi::String::New(env, s));
      break;
    }
    case FieldKind::kBytes: {
      std::uint64_t len;
      if (!ReadVarint(buf, buf_len, off, &len) || *off + len > buf_len) {
        throw Napi::Error::New(env, "bytes length-delim underflow");
      }
      Napi::Uint8Array arr = Napi::Uint8Array::New(env, len);
      std::memcpy(arr.Data(), buf + *off, len);
      *off += len;
      msg.Set(d->name, arr);
      break;
    }
    case FieldKind::kInt32: {
      std::uint64_t v;
      if (!ReadVarint(buf, buf_len, off, &v)) {
        throw Napi::Error::New(env, "varint underflow on int32");
      }
      msg.Set(d->name,
              Napi::Number::New(env, static_cast<double>(
                                         static_cast<std::int32_t>(v))));
      break;
    }
    case FieldKind::kUint32: {
      std::uint64_t v;
      if (!ReadVarint(buf, buf_len, off, &v)) {
        throw Napi::Error::New(env, "varint underflow on uint32");
      }
      msg.Set(d->name,
              Napi::Number::New(env, static_cast<double>(
                                         static_cast<std::uint32_t>(v))));
      break;
    }
    case FieldKind::kInt64: {
      std::uint64_t v;
      if (!ReadVarint(buf, buf_len, off, &v)) {
        throw Napi::Error::New(env, "varint underflow on int64");
      }
      msg.Set(d->name,
              Napi::BigInt::New(env, static_cast<std::int64_t>(v)));
      break;
    }
    case FieldKind::kUint64: {
      std::uint64_t v;
      if (!ReadVarint(buf, buf_len, off, &v)) {
        throw Napi::Error::New(env, "varint underflow on uint64");
      }
      msg.Set(d->name, Napi::BigInt::New(env, v));
      break;
    }
    case FieldKind::kBool: {
      std::uint64_t v;
      if (!ReadVarint(buf, buf_len, off, &v)) {
        throw Napi::Error::New(env, "varint underflow on bool");
      }
      msg.Set(d->name, Napi::Boolean::New(env, v != 0));
      break;
    }
    case FieldKind::kUnsupported:
      throw Napi::Error::New(env, "unsupported field kind on decode");
  }
}

}  // namespace

Napi::Value EncodeMessage(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 2 || !info[0].IsArray() || !info[1].IsObject()) {
    throw Napi::TypeError::New(env,
                               "encodeMessage(descriptor[], message) signature "
                               "mismatch");
  }
  Napi::Array desc_arr = info[0].As<Napi::Array>();
  Napi::Object msg = info[1].As<Napi::Object>();
  auto desc = ParseDescriptor(desc_arr);

  std::vector<std::uint8_t> out;
  out.reserve(64);
  for (const auto& d : desc) {
    Napi::Value fv = msg.Get(d.name);
    if (fv.IsUndefined() || fv.IsNull()) {
      continue;  // proto3 implicit-presence: skip default
    }
    EncodeField(d, fv, &out, env);
  }
  Napi::Buffer<std::uint8_t> result =
      Napi::Buffer<std::uint8_t>::Copy(env, out.data(), out.size());
  return result;
}

Napi::Value DecodeMessage(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  if (info.Length() < 2 || !info[0].IsArray() ||
      (!info[1].IsBuffer() && !info[1].IsTypedArray())) {
    throw Napi::TypeError::New(env,
                               "decodeMessage(descriptor[], buffer) signature "
                               "mismatch");
  }
  Napi::Array desc_arr = info[0].As<Napi::Array>();
  auto desc = ParseDescriptor(desc_arr);

  const std::uint8_t* buf;
  std::size_t buf_len;
  if (info[1].IsBuffer()) {
    Napi::Buffer<std::uint8_t> b = info[1].As<Napi::Buffer<std::uint8_t>>();
    buf = b.Data();
    buf_len = b.Length();
  } else {
    Napi::Uint8Array a = info[1].As<Napi::Uint8Array>();
    buf = a.Data();
    buf_len = a.ByteLength();
  }

  Napi::Object out = Napi::Object::New(env);
  std::size_t off = 0;
  while (off < buf_len) {
    std::uint64_t tag;
    if (!ReadVarint(buf, buf_len, &off, &tag)) {
      throw Napi::Error::New(env, "varint underflow on tag");
    }
    std::uint32_t field_num = static_cast<std::uint32_t>(tag >> 3);
    std::uint8_t wire_type = static_cast<std::uint8_t>(tag & 0x07);
    DecodeField(desc, field_num, wire_type, buf, buf_len, &off, out, env);
  }
  return out;
}

}  // namespace pb
}  // namespace grpcx
