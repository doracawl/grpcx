// Wire format v2 codec — C++ implementation.

#include "wire_v2.h"

#include <cstring>

namespace grpcx {
namespace wire_v2 {

namespace {

inline std::uint16_t ReadU16LE(const std::uint8_t* p) {
  return static_cast<std::uint16_t>(p[0]) |
         (static_cast<std::uint16_t>(p[1]) << 8);
}

inline std::uint32_t ReadU32LE(const std::uint8_t* p) {
  return static_cast<std::uint32_t>(p[0]) |
         (static_cast<std::uint32_t>(p[1]) << 8) |
         (static_cast<std::uint32_t>(p[2]) << 16) |
         (static_cast<std::uint32_t>(p[3]) << 24);
}

inline void WriteU16LE(std::uint16_t v, std::uint8_t* p) {
  p[0] = static_cast<std::uint8_t>(v & 0xff);
  p[1] = static_cast<std::uint8_t>((v >> 8) & 0xff);
}

inline void WriteU32LE(std::uint32_t v, std::uint8_t* p) {
  p[0] = static_cast<std::uint8_t>(v & 0xff);
  p[1] = static_cast<std::uint8_t>((v >> 8) & 0xff);
  p[2] = static_cast<std::uint8_t>((v >> 16) & 0xff);
  p[3] = static_cast<std::uint8_t>((v >> 24) & 0xff);
}

}  // namespace

WireError DecodeHeader(const std::uint8_t* buf, std::size_t buf_len,
                       Header* out) {
  if (buf_len < kHeaderLen) {
    return WireError::BufferTooShort;
  }
  std::uint8_t ft_byte = buf[4];
  switch (ft_byte) {
    case 0x01:
    case 0x02:
    case 0x03:
    case 0x04:
    case 0x05:
    case 0x06:
      break;
    default:
      return WireError::UnknownFrameType;
  }
  out->stream_id = ReadU32LE(buf);
  out->frame_type = static_cast<FrameType>(ft_byte);
  out->flags = buf[5];
  out->metadata_count = ReadU16LE(buf + 6);
  out->payload_len = ReadU32LE(buf + 8);
  // reserved at buf[12..16] — ignored.
  return WireError::Ok;
}

void EncodeHeader(const Header& header, std::uint8_t* out) {
  WriteU32LE(header.stream_id, out);
  out[4] = static_cast<std::uint8_t>(header.frame_type);
  out[5] = header.flags;
  WriteU16LE(header.metadata_count, out + 6);
  WriteU32LE(header.payload_len, out + 8);
  // reserved
  out[12] = 0;
  out[13] = 0;
  out[14] = 0;
  out[15] = 0;
}

WireError DecodeMetadataBlock(const std::uint8_t* payload,
                              std::size_t payload_len,
                              std::uint16_t count,
                              std::vector<MetadataEntry>* out_entries,
                              std::size_t* out_consumed) {
  out_entries->clear();
  out_entries->reserve(count);
  std::size_t cursor = 0;
  for (std::uint16_t i = 0; i < count; ++i) {
    if (payload_len - cursor < 6) {
      return WireError::TruncatedMetadata;
    }
    std::uint16_t name_len = ReadU16LE(payload + cursor);
    std::uint32_t value_len = ReadU32LE(payload + cursor + 2);
    if (name_len == 0 || name_len > kNameLenMax) {
      return WireError::InvalidMetadataNameLen;
    }
    cursor += 6;
    std::size_t need =
        static_cast<std::size_t>(name_len) + static_cast<std::size_t>(value_len);
    if (payload_len - cursor < need) {
      return WireError::TruncatedMetadata;
    }
    MetadataEntry entry;
    entry.name.assign(payload + cursor, payload + cursor + name_len);
    entry.value.assign(payload + cursor + name_len,
                       payload + cursor + name_len + value_len);
    out_entries->push_back(std::move(entry));
    cursor += need;
  }
  *out_consumed = cursor;
  return WireError::Ok;
}

std::size_t BlockWireLen(const std::vector<MetadataEntry>& entries) {
  std::size_t total = 0;
  for (const auto& e : entries) {
    total += EntryWireLen(e.name.size(), e.value.size());
  }
  return total;
}

std::size_t EncodeMetadataBlock(const std::vector<MetadataEntry>& entries,
                                std::uint8_t* out) {
  std::size_t off = 0;
  for (const auto& e : entries) {
    WriteU16LE(static_cast<std::uint16_t>(e.name.size()), out + off);
    WriteU32LE(static_cast<std::uint32_t>(e.value.size()), out + off + 2);
    off += 6;
    std::memcpy(out + off, e.name.data(), e.name.size());
    off += e.name.size();
    std::memcpy(out + off, e.value.data(), e.value.size());
    off += e.value.size();
  }
  return off;
}

}  // namespace wire_v2
}  // namespace grpcx
