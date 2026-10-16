//! Windows DPAPI protection for persisted session state (current user scope).

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn run(data: &[u8], decrypt: bool) -> Result<Vec<u8>, String> {
        let len =
            u32::try_from(data.len()).map_err(|_| "Session data is too large.".to_string())?;
        let input = CRYPT_INTEGER_BLOB {
            cbData: len,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: input points to `len` readable bytes for the duration of the call;
        // DPAPI allocates output with LocalAlloc, which is copied and then freed once.
        let ok = unsafe {
            if decrypt {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.pbData.is_null() {
            return Err(if decrypt {
                "Windows could not unlock the saved session.".to_string()
            } else {
                "Windows could not protect the session.".to_string()
            });
        }
        // SAFETY: DPAPI returned a buffer of cbData bytes that we own until LocalFree.
        unsafe {
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData as _);
            Ok(bytes)
        }
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, false)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, true)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("Session protection currently supports Windows only.".to_string())
    }

    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("Session protection currently supports Windows only.".to_string())
    }
}

pub use imp::{protect, unprotect};

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn round_trip() {
        let sealed = super::protect(b"secret").unwrap();
        assert_ne!(sealed, b"secret");
        assert_eq!(super::unprotect(&sealed).unwrap(), b"secret");
    }
}
