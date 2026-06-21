//! UDS-based rendezvous protocol — client connects an
//! AF_UNIX socket, server responds with a
//! [`RegionDescriptor`] (region name + dimensions) plus
//! two wake fds via `SCM_RIGHTS`.
//!
//! Wire shape is intentionally minimal for T3.1:
//! - Client sends a 4-byte ASCII magic `"GXR1"` then
//!   nothing else.
//! - Server replies with a 24-byte fixed payload
//!   (region_name length-prefixed up to 16, region_len
//!   u64 LE, slot_count u32 LE, slot_stride u32 LE) plus
//!   2 fds via `SCM_RIGHTS`.
//!
//! Switching this to v2 wire frames (per PLAN § 3.2) is
//! a T3.x cleanup once we wire the shmem data plane —
//! T3.1's job is to prove fd-passing + region attach.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use super::region::{Region, shm_unlink_silent};
use super::wake::WakePair;
use crate::RpcError;

/// Magic bytes sent by the client to open a rendezvous.
const MAGIC: &[u8; 4] = b"GXR1";

/// Total bytes of the server's wire reply, post-magic
/// validation:
///   - 8 bytes region_len u64 LE
///   - 4 bytes slot_count u32 LE
///   - 4 bytes slot_stride u32 LE
///
/// The region fd itself rides over `SCM_RIGHTS` — no
/// `shm_open`-by-name on the client side, sidestepping
/// Mac's POSIX-shm second-open quirk.
const REPLY_BYTES: usize = 8 + 4 + 4;

#[cfg(any(target_os = "linux", target_os = "android"))]
const SOL_SOCKET: c_int = 1;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const SOL_SOCKET: c_int = 0xffff;

const SCM_RIGHTS: c_int = 1;

// --------------------------------------------------------
// raw sendmsg / recvmsg
// --------------------------------------------------------

#[repr(C)]
struct IoVec {
    iov_base: *mut c_void,
    iov_len: usize,
}

// `struct msghdr` layout differs across OS:
// - Linux (glibc): msg_iovlen / msg_controllen are
//   size_t (= usize on 64-bit).
// - Darwin / FreeBSD: msg_iovlen is int (= c_int),
//   msg_controllen is socklen_t (= u32). No padding
//   between fields.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[repr(C)]
struct MsgHdr {
    msg_name: *mut c_void,
    msg_namelen: u32,
    #[cfg(target_pointer_width = "64")]
    _pad1: u32,
    msg_iov: *mut IoVec,
    msg_iovlen: usize,
    msg_control: *mut c_void,
    msg_controllen: usize,
    msg_flags: c_int,
    #[cfg(target_pointer_width = "64")]
    _pad2: u32,
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[repr(C)]
struct MsgHdr {
    msg_name: *mut c_void,
    msg_namelen: u32,
    #[cfg(target_pointer_width = "64")]
    _pad1: u32,
    msg_iov: *mut IoVec,
    msg_iovlen: c_int,
    #[cfg(target_pointer_width = "64")]
    _pad_iovlen: u32,
    msg_control: *mut c_void,
    msg_controllen: u32,
    msg_flags: c_int,
}

// `struct cmsghdr` layout — `cmsg_len` is `size_t` on
// Linux glibc (8 B on 64-bit), `socklen_t = u32` on
// Darwin / FreeBSD (4 B). Mismatching the field width
// shifts every subsequent byte and the SCM_RIGHTS fd
// extraction returns garbage.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[repr(C)]
struct CmsgHdr {
    cmsg_len: usize,
    cmsg_level: c_int,
    cmsg_type: c_int,
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[repr(C)]
struct CmsgHdr {
    cmsg_len: u32,
    cmsg_level: c_int,
    cmsg_type: c_int,
}

unsafe extern "C" {
    fn sendmsg(sockfd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn recvmsg(sockfd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
}

/// Total cmsg buffer size for passing exactly three fds
/// (region fd + 2 wake fds): `CMSG_SPACE(sizeof(int)*3)`.
///
/// - Linux 64-bit glibc: header 16 B (size_t + int + int) +
///   12 B fd data, aligned to size_t → 32 B.
/// - Darwin / FreeBSD: header 12 B (socklen_t + int + int) +
///   12 B fd data, aligned to int → 24 B.
#[cfg(any(target_os = "linux", target_os = "android"))]
const CMSG_BUF_LEN_3FD: usize = 32;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const CMSG_BUF_LEN_3FD: usize = 24;

/// `CMSG_LEN(sizeof(int)*3)` — the value written into
/// `CmsgHdr::cmsg_len`. Per-platform width matches the
/// `cmsg_len` field type (`size_t` on Linux, `socklen_t`
/// on Darwin/FreeBSD).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[inline]
fn cmsg_len_for_3fds() -> usize {
    core::mem::size_of::<CmsgHdr>() + 12
}
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[inline]
fn cmsg_len_for_3fds() -> u32 {
    (core::mem::size_of::<CmsgHdr>() as u32) + 12
}

// --------------------------------------------------------
// Public surface
// --------------------------------------------------------

/// What the client side sees of a successful rendezvous —
/// the [`Region`] it mmap'd and the two wake fds it owns:
/// `wait_fd` (the read end of the server→client wake
/// channel) and `signal_fd` (the write end of the
/// client→server wake channel).
pub struct RegionDescriptor {
    /// The mmap'd shared region.
    pub region: Region,
    /// Negotiated slot count for the ringbuf data plane.
    pub slot_count: u32,
    /// Negotiated slot stride in bytes.
    pub slot_stride: u32,
    /// Read end of the wake channel the client waits on
    /// for server pokes.
    pub wait_fd: OwnedFd,
    /// Write end of the wake channel the client signals
    /// the server with.
    pub signal_fd: OwnedFd,
}

/// What the server side keeps post-rendezvous — the
/// [`Region`] (mmap'd; name already `shm_unlink`'d) and
/// the two wake fds it owns: `wait_fd` (read end of the
/// client→server wake channel) and `signal_fd` (write end
/// of the server→client wake channel).
pub struct ServerSideRegion {
    /// The server-side mmap'd shared region.
    pub region: Region,
    /// Negotiated slot count for the ringbuf data plane.
    pub slot_count: u32,
    /// Negotiated slot stride in bytes.
    pub slot_stride: u32,
    /// Read end of the wake channel the server waits on
    /// for client pokes.
    pub wait_fd: OwnedFd,
    /// Write end of the wake channel the server signals
    /// the client with.
    pub signal_fd: OwnedFd,
}

// --------------------------------------------------------
// Client side
// --------------------------------------------------------

/// Connect to a shmem-rendezvous UDS server at `path`,
/// negotiate a region, and return a fully-attached
/// [`RegionDescriptor`].
pub fn connect_rendezvous(path: &std::path::Path) -> Result<RegionDescriptor, RpcError> {
    use std::io::Write;
    let mut stream = UnixStream::connect(path)
        .map_err(|_| RpcError::Transport("shmem-rendezvous: connect failed"))?;
    stream
        .write_all(MAGIC)
        .map_err(|_| RpcError::Transport("shmem-rendezvous: write magic failed"))?;

    // Receive reply + 3 fds via recvmsg (region fd +
    // wait fd + signal fd).
    let mut reply = [0u8; REPLY_BYTES];
    let mut cmsg_buf = [0u8; CMSG_BUF_LEN_3FD];
    let mut fds_out = [0i32; 3];
    let n = recv_with_fds(stream.as_raw_fd(), &mut reply, &mut cmsg_buf, &mut fds_out)?;
    if n != REPLY_BYTES as isize {
        return Err(RpcError::Transport(
            "shmem-rendezvous: server reply truncated",
        ));
    }

    let region_len = u64::from_le_bytes(reply[0..8].try_into().unwrap()) as usize;
    let slot_count = u32::from_le_bytes(reply[8..12].try_into().unwrap());
    let slot_stride = u32::from_le_bytes(reply[12..16].try_into().unwrap());

    // SAFETY: kernel filled `fds_out` from SCM_RIGHTS; we
    // own them now.
    let region_fd = unsafe { OwnedFd::from_raw_fd(fds_out[0]) };
    let wait_fd = unsafe { OwnedFd::from_raw_fd(fds_out[1]) };
    let signal_fd = unsafe { OwnedFd::from_raw_fd(fds_out[2]) };
    let region = Region::from_fd(region_fd, region_len)?;

    Ok(RegionDescriptor {
        region,
        slot_count,
        slot_stride,
        wait_fd,
        signal_fd,
    })
}

// --------------------------------------------------------
// Server side
// --------------------------------------------------------

/// Accept a rendezvous on `stream` — reads the 4-byte
/// magic, creates a region of `region_len` bytes (rounded
/// up to a page), creates two wake-fd pairs, sends the
/// reply + the two fds the client needs.
pub fn accept_rendezvous(
    mut stream: UnixStream,
    slot_count: u32,
    slot_stride: u32,
) -> Result<ServerSideRegion, RpcError> {
    use std::io::Read;
    let mut magic = [0u8; 4];
    stream
        .read_exact(&mut magic)
        .map_err(|_| RpcError::Transport("shmem-rendezvous: read magic failed"))?;
    if &magic != MAGIC {
        return Err(RpcError::Transport("shmem-rendezvous: bad client magic"));
    }

    let region_len = (slot_count as usize)
        .checked_mul(slot_stride as usize)
        .ok_or(RpcError::Transport(
            "shmem-rendezvous: region size overflow",
        ))?;
    let region_name = generate_region_name();
    let region = Region::create(&region_name, region_len)?;

    // Two wake pairs:
    //   s2c — server signals client. Server keeps write,
    //         client keeps read.
    //   c2s — client signals server. Client keeps write,
    //         server keeps read.
    let s2c = WakePair::create()?;
    let c2s = WakePair::create()?;

    // Build reply bytes (region_len + slot_count +
    // slot_stride). Region fd rides over SCM_RIGHTS.
    let mut reply = [0u8; REPLY_BYTES];
    reply[0..8].copy_from_slice(&(region_len as u64).to_le_bytes());
    reply[8..12].copy_from_slice(&slot_count.to_le_bytes());
    reply[12..16].copy_from_slice(&slot_stride.to_le_bytes());

    // Send: reply bytes + [region.fd, s2c.read_fd (client
    // waits), c2s.write_fd (client signals)]. Sending the
    // region fd via SCM_RIGHTS sidesteps Mac's
    // `shm_open`-after-mmap quirk; the kernel duplicates
    // the fd so server retains its own.
    let send_fds = [
        region.as_raw_fd(),
        s2c.read_fd.as_raw_fd(),
        c2s.write_fd.as_raw_fd(),
    ];
    send_with_fds(stream.as_raw_fd(), &reply, &send_fds)?;

    // The shm name leaks in the kernel registry —
    // intentionally, for T3.1. The region itself is
    // reclaimed when both sides drop their fds (POSIX
    // semantics); the name table entry persists until
    // process exit unless `shm_unlink`'d. T3.2 will add
    // a client-ack handshake before unlinking.
    let _ = region_name;

    Ok(ServerSideRegion {
        region,
        slot_count,
        slot_stride,
        wait_fd: c2s.read_fd,
        signal_fd: s2c.write_fd,
    })
}

// --------------------------------------------------------
// helpers
// --------------------------------------------------------

fn send_with_fds(sockfd: RawFd, bytes: &[u8], fds: &[RawFd; 3]) -> Result<(), RpcError> {
    let mut iov = IoVec {
        iov_base: bytes.as_ptr() as *mut c_void,
        iov_len: bytes.len(),
    };
    let mut cmsg_buf = [0u8; CMSG_BUF_LEN_3FD];
    // SAFETY: cmsg_buf is sized via `CMSG_BUF_LEN_3FD` to
    // the platform's `CMSG_SPACE(sizeof(int)*3)` — 24 B
    // on Darwin/FreeBSD, 32 B on Linux 64-bit glibc. The
    // header is laid out via the platform-conditional
    // `CmsgHdr` (size_t cmsg_len on Linux, u32 on
    // Darwin/FreeBSD); writing past `size_of::<CmsgHdr>() +
    // 12` would still stay within the alloc'd 32-B Linux
    // buffer.
    {
        let hdr = CmsgHdr {
            cmsg_len: cmsg_len_for_3fds(),
            cmsg_level: SOL_SOCKET,
            cmsg_type: SCM_RIGHTS,
        };
        unsafe {
            core::ptr::write_unaligned(cmsg_buf.as_mut_ptr().cast::<CmsgHdr>(), hdr);
            let data_ptr = cmsg_buf
                .as_mut_ptr()
                .add(core::mem::size_of::<CmsgHdr>())
                .cast::<i32>();
            core::ptr::write_unaligned(data_ptr, fds[0]);
            core::ptr::write_unaligned(data_ptr.add(1), fds[1]);
            core::ptr::write_unaligned(data_ptr.add(2), fds[2]);
        }
    }

    let msg = make_msg(
        &mut iov,
        cmsg_buf.as_mut_ptr().cast::<c_void>(),
        cmsg_buf.len(),
    );
    let n = unsafe { sendmsg(sockfd, &msg as *const _, 0) };
    if n != bytes.len() as isize {
        return Err(RpcError::Transport("shmem-rendezvous: sendmsg failed"));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn make_msg(iov: &mut IoVec, ctrl: *mut c_void, ctrl_len: usize) -> MsgHdr {
    MsgHdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        #[cfg(target_pointer_width = "64")]
        _pad1: 0,
        msg_iov: iov as *mut _,
        msg_iovlen: 1,
        msg_control: ctrl,
        msg_controllen: ctrl_len,
        msg_flags: 0,
        #[cfg(target_pointer_width = "64")]
        _pad2: 0,
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn make_msg(iov: &mut IoVec, ctrl: *mut c_void, ctrl_len: usize) -> MsgHdr {
    MsgHdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        #[cfg(target_pointer_width = "64")]
        _pad1: 0,
        msg_iov: iov as *mut _,
        msg_iovlen: 1,
        #[cfg(target_pointer_width = "64")]
        _pad_iovlen: 0,
        msg_control: ctrl,
        msg_controllen: ctrl_len as u32,
        msg_flags: 0,
    }
}

/// Pull `msg_controllen` back from a `MsgHdr` post
/// `recvmsg` in a width-aware way.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn msg_controllen(m: &MsgHdr) -> usize {
    m.msg_controllen
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn msg_controllen(m: &MsgHdr) -> usize {
    m.msg_controllen as usize
}

fn recv_with_fds(
    sockfd: RawFd,
    out_bytes: &mut [u8],
    cmsg_buf: &mut [u8],
    fds_out: &mut [i32; 3],
) -> Result<isize, RpcError> {
    let mut iov = IoVec {
        iov_base: out_bytes.as_mut_ptr().cast::<c_void>(),
        iov_len: out_bytes.len(),
    };
    let mut msg = make_msg(
        &mut iov,
        cmsg_buf.as_mut_ptr().cast::<c_void>(),
        cmsg_buf.len(),
    );
    let n = unsafe { recvmsg(sockfd, &mut msg as *mut _, 0) };
    if n <= 0 {
        return Err(RpcError::Transport("shmem-rendezvous: recvmsg failed"));
    }
    // Walk control message — expect exactly one cmsghdr
    // with SCM_RIGHTS + 2 fds.
    if msg_controllen(&msg) < core::mem::size_of::<CmsgHdr>() {
        return Err(RpcError::Transport(
            "shmem-rendezvous: no cmsg in server reply",
        ));
    }
    let hdr: CmsgHdr = unsafe { core::ptr::read_unaligned(cmsg_buf.as_ptr().cast::<CmsgHdr>()) };
    if hdr.cmsg_level != SOL_SOCKET || hdr.cmsg_type != SCM_RIGHTS {
        return Err(RpcError::Transport(
            "shmem-rendezvous: cmsg is not SCM_RIGHTS",
        ));
    }
    if (hdr.cmsg_len as usize) < core::mem::size_of::<CmsgHdr>() + 12 {
        return Err(RpcError::Transport("shmem-rendezvous: cmsg fd count != 3"));
    }
    let data_ptr = unsafe {
        cmsg_buf
            .as_ptr()
            .add(core::mem::size_of::<CmsgHdr>())
            .cast::<i32>()
    };
    fds_out[0] = unsafe { core::ptr::read_unaligned(data_ptr) };
    fds_out[1] = unsafe { core::ptr::read_unaligned(data_ptr.add(1)) };
    fds_out[2] = unsafe { core::ptr::read_unaligned(data_ptr.add(2)) };
    Ok(n)
}

/// `/gx-XXXXXXXX` where X is a hex byte derived from a
/// rotating counter + the current pid — guarantees
/// uniqueness within a process without needing a real
/// RNG dependency.
fn generate_region_name() -> String {
    use core::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid: u32 = std::process::id();
    let mix = pid.wrapping_mul(2_654_435_761) ^ n;
    let mut s = String::with_capacity(16);
    s.push_str("/gx-");
    for shift in (0..32).step_by(4).rev() {
        let nibble = ((mix >> shift) & 0xf) as u8;
        s.push(hex_char(nibble));
    }
    s
}

fn hex_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

/// Convenience for tests / failure paths: unlink an
/// orphaned region name without surfacing errors.
#[allow(dead_code)]
pub(crate) fn cleanup_orphan(name: &str) {
    shm_unlink_silent(name);
}

/// Suppress unused-import warnings for items only used
/// in some cfg branches.
#[allow(dead_code)]
fn _force_used() -> Vec<u8> {
    Vec::new()
}
