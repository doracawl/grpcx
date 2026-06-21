// Wire format v2 codec — C++ mirror of the Rust
// `wire_v2` module. Normative reference:
// ../../../../../_dev/SPEC-wire-v2.md.

#pragma once

#include <cstdint>
#include <cstddef>
#include <string>
#include <vector>

namespace grpcx {
namespace wire_v2 {

constexpr std::size_t kHeaderLen = 16;
constexpr std::uint16_t kNameLenMax = 1024;

// Frame type byte values (SPEC §4).
enum class FrameType : std::uint8_t {
  Begin = 0x01,
  Data = 0x02,
  End = 0x03,
  Reset = 0x04,
  Ping = 0x05,
  Pong = 0x06,
};

// Flag bits (SPEC §2.2).
namespace flags {
constexpr std::uint8_t kEndStream = 0x01;
constexpr std::uint8_t kHasMetadata = 0x02;
constexpr std::uint8_t kContinuation = 0x04;
}  // namespace flags

struct Header {
  std::uint32_t stream_id;
  FrameType frame_type;
  std::uint8_t flags;
  std::uint16_t metadata_count;
  std::uint32_t payload_len;
};

enum class WireError {
  Ok = 0,
  BufferTooShort,
  UnknownFrameType,
  InvalidMetadataNameLen,
  TruncatedMetadata,
};

// Decode a 16-byte header. `buf` must point to at least
// kHeaderLen bytes; returns Ok on success and fills
// `out`. The `reserved` field is silently ignored per
// SPEC §2.1 forward-compatibility.
WireError DecodeHeader(const std::uint8_t* buf, std::size_t buf_len,
                       Header* out);

// Encode a header into a 16-byte buffer. `reserved` is
// written as zero.
void EncodeHeader(const Header& header, std::uint8_t* out);

struct MetadataEntry {
  std::vector<std::uint8_t> name;
  std::vector<std::uint8_t> value;
};

// Decode `count` metadata entries from `payload`. Stores
// the entries in `out_entries`. On success, sets
// `out_consumed` to the byte count read so callers can
// slice the remaining payload (message body).
WireError DecodeMetadataBlock(const std::uint8_t* payload,
                              std::size_t payload_len,
                              std::uint16_t count,
                              std::vector<MetadataEntry>* out_entries,
                              std::size_t* out_consumed);

// Byte size of a single metadata entry on the wire.
inline std::size_t EntryWireLen(std::size_t name_len,
                                std::size_t value_len) {
  return 6 + name_len + value_len;
}

// Byte size of an entire metadata block.
std::size_t BlockWireLen(const std::vector<MetadataEntry>& entries);

// Encode a metadata block into `out`. Returns bytes
// written. Caller must allocate `out` with at least
// BlockWireLen() bytes.
std::size_t EncodeMetadataBlock(const std::vector<MetadataEntry>& entries,
                                std::uint8_t* out);

}  // namespace wire_v2
}  // namespace grpcx
