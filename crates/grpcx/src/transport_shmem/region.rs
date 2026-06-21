//! POSIX shared-memory region helper.
//!
//! Zero-dep: all syscalls are declared via raw
//! `extern "C"` blocks, mirroring the `grpcx-io` /
//! `transport_uds::peer` posture.
//!
//! Region names are short identifiers (`/gx-<8hex>`)
//! to fit Darwin's 30-character shm name limit; the
//! same name maps to `/dev/shm/gx-<8hex>` on Linux.

use core::ffi::{c_char, c_int, c_void};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use alloc::string::String;
use alloc::vec::Vec;

use crate::RpcError;

const O_RDWR: c_int = 0x0002;

#[cfg(any(target_os = "linux", target_os = "android"))]
const O_CREAT: c_int = 0o100;
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_EXCL: c_int = 0o200;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const O_CREAT: c_int = 0x0200;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const O_EXCL: c_int = 0x0800;

const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_SHARED: c_int = 0x01;

// MAP_FAILED is `(void *)-1` everywhere POSIX matters.
const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

#[cfg(any(target_os = "linux", target_os = "android"))]
const MADV_POPULATE_WRITE: c_int = 23;

// `mode_t` differs: u16 on Darwin / iOS / FreeBSD,
// u32 on Linux. We declare it as the platform-native
// width so the C ABI on the third arg of `shm_open`
// + `fchmod` matches.
#[cfg(any(target_os = "linux", target_os = "android"))]
type ModeT = u32;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
type ModeT = u16;

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, mode: ModeT) -> c_int;
    fn shm_unlink(name: *const c_char) -> c_int;
    fn ftruncate(fd: c_int, len: i64) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
}

/// A live `mmap`ed POSIX shm region. Drops the mapping
/// on `Drop`; the underlying shm name is unlinked
/// separately via [`Region::unlink`] (typically right
/// after the peer has opened it).
pub struct Region {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    fd: OwnedFd,
    addr: *mut u8,
    len: usize,
}

// Region holds raw `*mut` but is logically owned + safe
// to send between threads (mmap addr is process-global
// for the lifetime of the OwnedFd).
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

impl Region {
    /// Create a new region with the given short `name`
    /// (must begin with `/`, ≤ 30 chars to fit Darwin),
    /// size `len` bytes (rounded by the kernel to the
    /// nearest page).
    pub fn create(name: &str, len: usize) -> Result<Self, RpcError> {
        let c_name = c_str(name);
        // SAFETY: c_name lives for the duration of the
        // syscall; mode 0600 is uid-locked.
        let fd_raw = unsafe {
            shm_open(
                c_name.as_ptr().cast::<c_char>(),
                O_RDWR | O_CREAT | O_EXCL,
                0o600,
            )
        };
        if fd_raw < 0 {
            return Err(RpcError::Transport("shmem: shm_open create failed"));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd_raw) };
        if unsafe { ftruncate(fd.as_raw_fd(), len as i64) } != 0 {
            return Err(RpcError::Transport("shmem: ftruncate failed"));
        }
        // `shm_open` honours umask; we don't fchmod
        // here because Darwin's POSIX shm fchmod
        // (counter-intuitively) flips access to "no
        // access" for subsequent shm_open in the same
        // process — observed on macOS 14+. The mode arg
        // to shm_open is authoritative; rely on it.
        let addr = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if addr == MAP_FAILED {
            return Err(RpcError::Transport("shmem: mmap failed"));
        }
        // Pre-fault on Linux so first send sees no minor
        // faults. Best-effort — failure (e.g. kernel <
        // 5.14) does not abort.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        unsafe {
            let _ = madvise(addr, len, MADV_POPULATE_WRITE);
        }
        Ok(Region {
            name: String::from(name),
            fd,
            addr: addr.cast::<u8>(),
            len,
        })
    }

    /// Open an existing region attached to `fd` — the
    /// peer sent its region fd via `SCM_RIGHTS`, we mmap
    /// it directly. Avoids `shm_open` by name entirely
    /// (Mac POSIX shm has a documented quirk where a
    /// second `shm_open(O_RDWR)` after the creator
    /// `mmap`s returns EACCES regardless of mode).
    pub fn from_fd(fd: OwnedFd, len: usize) -> Result<Self, RpcError> {
        let addr = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if addr == MAP_FAILED {
            return Err(RpcError::Transport("shmem: open mmap failed"));
        }
        Ok(Region {
            name: String::new(),
            fd,
            addr: addr.cast::<u8>(),
            len,
        })
    }

    /// The region's short identifier (begins with `/`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mapped length in bytes.
    #[allow(clippy::len_without_is_empty)] // shm regions
    // are not "empty" in the Vec sense — they're a
    // sized mmap; len > 0 is invariant.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Raw mapped pointer. Caller must respect `len`.
    pub fn as_ptr(&self) -> *mut u8 {
        self.addr
    }

    /// The region's backing fd. Sent to the peer via
    /// `SCM_RIGHTS` at rendezvous time; the kernel
    /// duplicates so this side retains its own.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// View the region as a byte slice. Mutation through
    /// it is racey unless the caller has the SPSC
    /// invariant.
    ///
    /// # Safety
    ///
    /// The mapped pages may be concurrently written by
    /// the peer. Caller is responsible for the SPSC
    /// access pattern.
    #[allow(dead_code, clippy::mut_from_ref)]
    pub unsafe fn as_slice(&self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.addr, self.len) }
    }

    /// Unlink the shm name from the system table.
    /// Subsequent peers cannot `shm_open` it; existing
    /// fd holders keep their mapping alive.
    ///
    /// Typically called immediately after the peer has
    /// attached (POSIX `unlink-then-keep-fd` idiom).
    pub fn unlink(name: &str) -> Result<(), RpcError> {
        let c_name = c_str(name);
        if unsafe { shm_unlink(c_name.as_ptr().cast::<c_char>()) } != 0 {
            return Err(RpcError::Transport("shmem: shm_unlink failed"));
        }
        Ok(())
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        if !self.addr.is_null() {
            unsafe {
                let _ = munmap(self.addr.cast::<c_void>(), self.len);
            }
            self.addr = core::ptr::null_mut();
        }
        // fd dropped by OwnedFd.
        let _ = &self.fd;
    }
}

fn c_str(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 1);
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    out
}

/// Helper used to clean up a leaked shm name after a
/// failed handshake.
pub(crate) fn shm_unlink_silent(name: &str) {
    let c_name = c_str(name);
    let _ = unsafe { shm_unlink(c_name.as_ptr().cast::<c_char>()) };
    drop(c_name);
}

/// Drop the close syscall noise on a raw fd that was
/// already moved into an `OwnedFd` elsewhere.
#[allow(dead_code)]
pub(crate) fn close_raw(fd: RawFd) {
    let _ = unsafe { close(fd) };
}
