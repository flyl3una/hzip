//! C ABI for the qcow2 reader.
//!
//! Single entry point: [`qcow2_open`] returns a generic
//! [`fs_core::ffi::FsCoreDevice`] handle so the rest of the
//! work (partition probe, FS sniff, mount) goes through the same handle
//! type every sister crate expects. There is intentionally no
//! qcow2-specific handle type at the C level — once the file is opened
//! it's just a block device.

#![allow(clippy::missing_safety_doc)]

use crate::Qcow2Reader;
use fs_core::ffi::{set_last_error, FsCoreDevice};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::Arc;

/// Open `path` (NUL-terminated UTF-8) as a QCOW2 image and return a
/// generic device handle. On failure returns NULL; consult
/// `fs_core_last_error_message()` for detail.
///
/// The caller owns the handle and frees it via `fs_core_device_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qcow2_open(path: *const c_char) -> *mut FsCoreDevice {
    open_path(path, false)
}

/// Open `path` read-write. Writes allocate clusters, copy shared ones
/// before changing them, and copy up from the backing chain; the few
/// refusals left are listed on [`Qcow2Reader::write_at`] and return
/// `FS_CORE_CUSTOM` with detail in `fs_core_last_error_message()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qcow2_open_rw(path: *const c_char) -> *mut FsCoreDevice {
    open_path(path, true)
}

/// Open a QCOW2 image whose backing storage is an existing
/// [`FsCoreDevice`] handle. Use this when the caller already holds the
/// device (e.g. an FSKit `FSBlockDeviceResource` lifted into an
/// `FsCoreDevice` via `fs_core_device_from_callbacks`) and wants the
/// qcow2 layer to sit on top of it.
///
/// Takes ownership of the input `inner` handle on success — the caller
/// must NOT call `fs_core_device_close` on it afterwards. On failure the
/// input is freed automatically and the function returns NULL.
///
/// Backing-file resolution is unavailable through this entry point
/// because there is no path to anchor a relative parent against; an
/// image with `backing_file_size != 0` is rejected with
/// `FS_CORE_CUSTOM`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qcow2_open_on_device(inner: *mut FsCoreDevice) -> *mut FsCoreDevice {
    unsafe { open_on_device(inner, false) }
}

/// Read-write variant of [`qcow2_open_on_device`]. The input device must
/// report `is_writable()`; otherwise the open fails with
/// `FS_CORE_READ_ONLY` and the input is freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qcow2_open_rw_on_device(inner: *mut FsCoreDevice) -> *mut FsCoreDevice {
    unsafe { open_on_device(inner, true) }
}

unsafe fn open_on_device(inner: *mut FsCoreDevice, writable: bool) -> *mut FsCoreDevice {
    if inner.is_null() {
        set_last_error("inner device handle is null");
        return ptr::null_mut();
    }
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // Reclaim ownership of the boxed handle; Arc::clone the inner
        // device so we can stack it under the qcow2 reader. The original
        // handle box is dropped at the end of this scope (releasing the
        // FsCoreDevice wrapper), but the underlying Arc<dyn BlockDevice>
        // lives on inside the new Qcow2Reader.
        let boxed = unsafe { Box::from_raw(inner) };
        let dev_arc = boxed.inner().clone();
        drop(boxed);

        let reader = if writable {
            Qcow2Reader::open_rw_on_device(dev_arc)
        } else {
            Qcow2Reader::open_on_device(dev_arc)
        };
        match reader {
            Ok(r) => FsCoreDevice::into_handle(Arc::new(r)),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    }));
    match res {
        Ok(p) => p,
        Err(panic) => {
            set_last_error(format!(
                "panic in qcow2_open_on_device: {}",
                fs_core::ffi::panic_message(&panic)
            ));
            ptr::null_mut()
        }
    }
}

fn open_path(path: *const c_char, writable: bool) -> *mut FsCoreDevice {
    if path.is_null() {
        set_last_error("path is null");
        return ptr::null_mut();
    }
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let cstr = unsafe { CStr::from_ptr(path) };
        let s = match cstr.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("path is not valid UTF-8");
                return ptr::null_mut();
            }
        };
        let reader = if writable {
            Qcow2Reader::open_rw(s)
        } else {
            Qcow2Reader::open(s)
        };
        match reader {
            Ok(r) => FsCoreDevice::into_handle(Arc::new(r)),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    }));
    match res {
        Ok(p) => p,
        Err(panic) => {
            set_last_error(format!(
                "panic in qcow2_open: {}",
                fs_core::ffi::panic_message(&panic)
            ));
            ptr::null_mut()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::ffi::{
        fs_core_device_close, fs_core_device_read_at, fs_core_device_size_bytes,
        fs_core_last_error_message, FsCoreErrorCode,
    };
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::path::PathBuf;

    const QCOW2_MAGIC: u32 = 0x5146_49fb;

    /// Build the minimal 16 KB qcow2 v3 image used by the existing
    /// integration tests, just inline here so the FFI smoke is
    /// self-contained.
    fn build_image(path: &PathBuf) {
        const COPIED: u64 = 1u64 << 63;
        // Bits 9..55, the host-offset field. Spelled out here rather
        // than imported from `reader`: this fixture is meant to be an
        // independent statement of the format.
        const HOST_OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
        const CLUSTER: u64 = 4096;
        const VIRT: u64 = CLUSTER * 4;
        const L1_OFF: u64 = CLUSTER;
        const L2_OFF: u64 = CLUSTER * 2;
        const D0_OFF: u64 = CLUSTER * 3;

        let mut f = File::create(path).unwrap();
        f.set_len(CLUSTER * 4).unwrap();

        let mut hdr = [0u8; 4096];
        hdr[0..4].copy_from_slice(&QCOW2_MAGIC.to_be_bytes());
        hdr[4..8].copy_from_slice(&3u32.to_be_bytes());
        hdr[20..24].copy_from_slice(&12u32.to_be_bytes());
        hdr[24..32].copy_from_slice(&VIRT.to_be_bytes());
        hdr[36..40].copy_from_slice(&1u32.to_be_bytes());
        hdr[40..48].copy_from_slice(&L1_OFF.to_be_bytes());
        hdr[96..100].copy_from_slice(&4u32.to_be_bytes());
        hdr[100..104].copy_from_slice(&104u32.to_be_bytes());
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&hdr).unwrap();

        let mut l1 = [0u8; 4096];
        l1[0..8].copy_from_slice(&((L2_OFF & HOST_OFFSET_MASK) | COPIED).to_be_bytes());
        f.seek(SeekFrom::Start(L1_OFF)).unwrap();
        f.write_all(&l1).unwrap();

        let mut l2 = [0u8; 4096];
        l2[0..8].copy_from_slice(&((D0_OFF & HOST_OFFSET_MASK) | COPIED).to_be_bytes());
        f.seek(SeekFrom::Start(L2_OFF)).unwrap();
        f.write_all(&l2).unwrap();

        let d0 = [0xAAu8; 4096];
        f.seek(SeekFrom::Start(D0_OFF)).unwrap();
        f.write_all(&d0).unwrap();
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "qcow2_capi_{}_{}_{name}.qcow2",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn open_then_read_via_fs_core_handle() {
        let path = tmp_path("smoke");
        build_image(&path);
        let cpath = CString::new(path.to_string_lossy().to_string()).unwrap();

        let h = unsafe { qcow2_open(cpath.as_ptr()) };
        assert!(!h.is_null(), "qcow2_open returned NULL");

        unsafe {
            assert_eq!(fs_core_device_size_bytes(h), 16384);

            let mut buf = [0u8; 4096];
            let rc = fs_core_device_read_at(h, 0, buf.as_mut_ptr(), buf.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert!(buf.iter().all(|&b| b == 0xAA));

            // Read past the virtual disk -> OutOfBounds.
            let rc = fs_core_device_read_at(h, 16384, buf.as_mut_ptr(), 16);
            assert_eq!(rc, FsCoreErrorCode::OutOfBounds);

            fs_core_device_close(h);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nonexistent_path_returns_null_with_message() {
        let cpath = CString::new("/no/such/file/we/hope.qcow2").unwrap();
        let h = unsafe { qcow2_open(cpath.as_ptr()) };
        assert!(h.is_null());
        let msg = fs_core_last_error_message();
        assert!(!msg.is_null());
    }

    #[test]
    fn rw_open_writes_to_allocated_cluster_via_fs_core_handle() {
        use fs_core::ffi::{
            fs_core_device_flush, fs_core_device_is_writable, fs_core_device_write_at,
        };

        let path = tmp_path("rw_smoke");
        build_image(&path);
        let cpath = CString::new(path.to_string_lossy().to_string()).unwrap();

        let h = unsafe { qcow2_open_rw(cpath.as_ptr()) };
        assert!(!h.is_null(), "qcow2_open_rw returned NULL");

        unsafe {
            assert!(fs_core_device_is_writable(h));

            // Overwrite four bytes inside virt cluster 0 (originally 0xAA).
            let payload = [0xDEu8, 0xAD, 0xBE, 0xEF];
            let rc = fs_core_device_write_at(h, 200, payload.as_ptr(), payload.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert_eq!(fs_core_device_flush(h), FsCoreErrorCode::Ok);

            // Read it back via the same handle.
            let mut readback = [0u8; 4];
            let rc = fs_core_device_read_at(h, 200, readback.as_mut_ptr(), readback.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert_eq!(readback, payload);

            // Writing into the unallocated cluster errors as Custom because
            // this fixture has no refcount table — the allocator has nowhere
            // to claim a fresh cluster from. (The full-fixture allocator
            // path is exercised by the Phase B unit tests.)
            let rc = fs_core_device_write_at(h, 4096, payload.as_ptr(), payload.len());
            assert_eq!(rc, FsCoreErrorCode::Custom);

            fs_core_device_close(h);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_on_device_stacks_qcow2_over_arbitrary_fs_core_device() {
        use fs_core::ffi::fs_core_file_open;

        let path = tmp_path("on_device_smoke");
        build_image(&path);
        let cpath = CString::new(path.to_string_lossy().to_string()).unwrap();

        // Open the file as a generic FsCoreDevice, then stack the qcow2
        // reader on top via the device-based entry point.
        let inner = unsafe { fs_core_file_open(cpath.as_ptr(), false) };
        assert!(!inner.is_null(), "fs_core_file_open returned NULL");

        let h = unsafe { qcow2_open_on_device(inner) };
        assert!(!h.is_null(), "qcow2_open_on_device returned NULL");

        unsafe {
            assert_eq!(fs_core_device_size_bytes(h), 16384);

            let mut buf = [0u8; 4096];
            let rc = fs_core_device_read_at(h, 0, buf.as_mut_ptr(), buf.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert!(buf.iter().all(|&b| b == 0xAA));

            fs_core_device_close(h);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_rw_on_device_supports_writes() {
        use fs_core::ffi::{
            fs_core_device_flush, fs_core_device_is_writable, fs_core_device_write_at,
            fs_core_file_open,
        };

        let path = tmp_path("rw_on_device_smoke");
        build_image(&path);
        let cpath = CString::new(path.to_string_lossy().to_string()).unwrap();

        let inner = unsafe { fs_core_file_open(cpath.as_ptr(), true) };
        assert!(!inner.is_null());

        let h = unsafe { qcow2_open_rw_on_device(inner) };
        assert!(!h.is_null(), "qcow2_open_rw_on_device returned NULL");

        unsafe {
            assert!(fs_core_device_is_writable(h));
            let payload = [0xCAu8, 0xFE, 0xBA, 0xBE];
            let rc = fs_core_device_write_at(h, 100, payload.as_ptr(), payload.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert_eq!(fs_core_device_flush(h), FsCoreErrorCode::Ok);

            let mut readback = [0u8; 4];
            let rc = fs_core_device_read_at(h, 100, readback.as_mut_ptr(), readback.len());
            assert_eq!(rc, FsCoreErrorCode::Ok);
            assert_eq!(readback, payload);

            fs_core_device_close(h);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_rw_on_device_rejects_readonly_inner() {
        use fs_core::ffi::fs_core_file_open;

        let path = tmp_path("ro_inner");
        build_image(&path);
        let cpath = CString::new(path.to_string_lossy().to_string()).unwrap();

        // Open the file RO, then try to wrap it with qcow2 RW — the
        // wrapper must refuse so callers don't silently get a useless
        // handle.
        let inner = unsafe { fs_core_file_open(cpath.as_ptr(), false) };
        assert!(!inner.is_null());

        let h = unsafe { qcow2_open_rw_on_device(inner) };
        assert!(h.is_null(), "expected NULL when wrapping a RO device RW");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_on_device_null_input_returns_null() {
        let h = unsafe { qcow2_open_on_device(std::ptr::null_mut()) };
        assert!(h.is_null());
        let msg = fs_core_last_error_message();
        assert!(!msg.is_null());
    }
}
