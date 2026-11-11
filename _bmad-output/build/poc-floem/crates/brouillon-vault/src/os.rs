//! OS keyslot: wraps and unwraps the 32-byte master key.
//!
//! FFI choice: direct `extern "system"` declarations for the two Crypt32 calls instead of
//! `windows-sys`. It is two functions, zero extra crates, zero extra compile time and nothing
//! to keep in step with the several `windows-sys` versions already in the lock file. The only
//! `unsafe` code in the crate is this file and `fsutil.rs`.

use std::sync::Arc;

use crate::crypto::{Key32, KEY_LEN};
use crate::error::VaultError;

pub const SLOT_DPAPI: u8 = 1;
pub const SLOT_PASSPHRASE: u8 = 2;
pub const SLOT_RECOVERY: u8 = 3;
pub const SLOT_FIDO2: u8 = 4;
pub const SLOT_TPM: u8 = 5;

/// A keyslot provider for "the OS can unwrap this for the current user".
///
/// Implementations must NOT fall back to anything weaker on failure: return an error.
pub trait OsWrap: Send + Sync {
    /// Slot kind byte written to the file.
    fn slot_kind(&self) -> u8;
    /// True when the wrapped blob only opens on this machine/user (refused in portable mode).
    fn machine_bound(&self) -> bool;
    fn wrap(&self, mk: &[u8; KEY_LEN], entropy: &[u8]) -> Result<Vec<u8>, VaultError>;
    fn unwrap(&self, blob: &[u8], entropy: &[u8]) -> Result<Key32, VaultError>;
}

/// The platform default: DPAPI on Windows, a stub returning `Unsupported` elsewhere.
pub fn default_os_wrap() -> Arc<dyn OsWrap> {
    #[cfg(windows)]
    {
        Arc::new(dpapi::Dpapi)
    }
    #[cfg(not(windows))]
    {
        Arc::new(Unsupported)
    }
}

/// Non-Windows stub (Keychain / Secret Service are future work).
#[cfg(not(windows))]
pub(crate) struct Unsupported;

#[cfg(not(windows))]
impl OsWrap for Unsupported {
    fn slot_kind(&self) -> u8 {
        SLOT_DPAPI
    }
    fn machine_bound(&self) -> bool {
        true
    }
    fn wrap(&self, _: &[u8; KEY_LEN], _: &[u8]) -> Result<Vec<u8>, VaultError> {
        Err(VaultError::Unsupported)
    }
    fn unwrap(&self, _: &[u8], _: &[u8]) -> Result<Key32, VaultError> {
        Err(VaultError::Unsupported)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod dpapi {
    use super::*;
    use std::ffi::c_void;
    use zeroize::{Zeroize, Zeroizing};

    #[repr(C)]
    struct DataBlob {
        cb: u32,
        pb: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(h: *mut c_void) -> *mut c_void;
    }

    /// Never show a dialog from a background thread.
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    pub(crate) struct Dpapi;

    fn blob_of(b: &[u8]) -> DataBlob {
        DataBlob { cb: b.len() as u32, pb: b.as_ptr() as *mut u8 }
    }

    /// Copies the OS-allocated output into a zeroizing buffer, wipes and frees the original.
    fn take_out(out: DataBlob) -> Zeroizing<Vec<u8>> {
        // SAFETY: on success the OS returns a LocalAlloc'd buffer of `cb` bytes at `pb`.
        let v = unsafe { std::slice::from_raw_parts(out.pb, out.cb as usize) }.to_vec();
        // SAFETY: same buffer, exclusively ours now; wipe before handing back to the OS.
        unsafe {
            std::slice::from_raw_parts_mut(out.pb, out.cb as usize).zeroize();
            LocalFree(out.pb as *mut c_void);
        }
        Zeroizing::new(v)
    }

    impl OsWrap for Dpapi {
        fn slot_kind(&self) -> u8 {
            SLOT_DPAPI
        }
        fn machine_bound(&self) -> bool {
            true
        }
        fn wrap(&self, mk: &[u8; KEY_LEN], entropy: &[u8]) -> Result<Vec<u8>, VaultError> {
            let input = blob_of(mk);
            let ent = blob_of(entropy);
            let mut out = DataBlob { cb: 0, pb: std::ptr::null_mut() };
            // SAFETY: pointers reference live buffers for the duration of the call.
            let ok = unsafe {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    &ent,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            };
            if ok == 0 || out.pb.is_null() {
                return Err(VaultError::Unavailable("DPAPI protect failed"));
            }
            // The protected blob is ciphertext, but reuse the same wipe-and-free path.
            Ok(take_out(out).to_vec())
        }
        fn unwrap(&self, blob: &[u8], entropy: &[u8]) -> Result<Key32, VaultError> {
            let input = blob_of(blob);
            let ent = blob_of(entropy);
            let mut out = DataBlob { cb: 0, pb: std::ptr::null_mut() };
            // SAFETY: as above.
            let ok = unsafe {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &ent,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            };
            if ok == 0 || out.pb.is_null() {
                return Err(VaultError::Unavailable("DPAPI unprotect failed"));
            }
            let plain = take_out(out);
            if plain.len() != KEY_LEN {
                return Err(VaultError::Corrupt);
            }
            let mut k = Zeroizing::new([0u8; KEY_LEN]);
            k.copy_from_slice(&plain);
            Ok(k)
        }
    }
}
