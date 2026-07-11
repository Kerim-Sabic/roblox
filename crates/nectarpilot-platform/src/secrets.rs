//! Current-user secret protection.
//!
//! Ciphertext is intentionally opaque to the core and database. On Windows it
//! is bound to the current user through DPAPI and additionally scoped with
//! `NectarPilot` entropy. Plaintext buffers are zeroed immediately after use.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("secret is empty")]
    Empty,
    #[error("secret is too large")]
    TooLarge,
    #[error("Windows DPAPI failed: {0}")]
    Platform(String),
    #[error("DPAPI secret protection is supported only on Windows")]
    UnsupportedPlatform,
}

pub fn protect_secret(secret: &[u8]) -> Result<Vec<u8>, SecretError> {
    if secret.is_empty() {
        return Err(SecretError::Empty);
    }
    #[cfg(windows)]
    {
        windows_dpapi::protect(secret)
    }
    #[cfg(not(windows))]
    {
        let _ = secret;
        Err(SecretError::UnsupportedPlatform)
    }
}

pub fn unprotect_secret(ciphertext: &[u8]) -> Result<Vec<u8>, SecretError> {
    if ciphertext.is_empty() {
        return Err(SecretError::Empty);
    }
    #[cfg(windows)]
    {
        windows_dpapi::unprotect(ciphertext)
    }
    #[cfg(not(windows))]
    {
        let _ = ciphertext;
        Err(SecretError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod windows_dpapi {
    use std::slice;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use zeroize::Zeroize;

    use super::SecretError;

    const ENTROPY: &[u8] = b"NectarPilot.current-user-secret.v1";

    pub fn protect(secret: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut plaintext = secret.to_vec();
        let result = crypt(&mut plaintext, true);
        plaintext.zeroize();
        result
    }

    pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut encrypted = ciphertext.to_vec();
        let result = crypt(&mut encrypted, false);
        encrypted.zeroize();
        result
    }

    fn crypt(input: &mut [u8], protect: bool) -> Result<Vec<u8>, SecretError> {
        let input_length = u32::try_from(input.len()).map_err(|_| SecretError::TooLarge)?;
        let entropy_length = u32::try_from(ENTROPY.len()).expect("entropy length fits u32");
        let mut entropy = ENTROPY.to_vec();
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: input_length,
            pbData: input.as_mut_ptr(),
        };
        let entropy_blob = CRYPT_INTEGER_BLOB {
            cbData: entropy_length,
            pbData: entropy.as_mut_ptr(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: both input blobs point to initialized buffers for the duration
        // of this synchronous call. `output` is initialized by DPAPI and freed
        // with LocalFree after copying its bytes.
        let operation = unsafe {
            if protect {
                CryptProtectData(
                    &raw const input_blob,
                    windows::core::PCWSTR::null(),
                    Some(&raw const entropy_blob),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &raw mut output,
                )
            } else {
                CryptUnprotectData(
                    &raw const input_blob,
                    None,
                    Some(&raw const entropy_blob),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &raw mut output,
                )
            }
        };
        entropy.zeroize();
        operation.map_err(|error| SecretError::Platform(error.to_string()))?;
        if output.pbData.is_null() || output.cbData == 0 {
            return Err(SecretError::Platform(
                "DPAPI returned an empty output buffer".to_owned(),
            ));
        }
        // SAFETY: DPAPI returned `cbData` initialized bytes at `pbData`.
        let mut result =
            unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        // SAFETY: DPAPI allocates output with LocalAlloc and transfers ownership
        // to the caller; LocalFree is the documented matching deallocator.
        let not_freed = unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
        if !not_freed.0.is_null() {
            result.zeroize();
            return Err(SecretError::Platform(
                "DPAPI output buffer could not be released".to_owned(),
            ));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip_is_opaque_and_user_scoped() {
        let plaintext = b"private-server-token-example";
        let encrypted = protect_secret(plaintext).expect("protect");
        assert_ne!(encrypted, plaintext);
        assert_eq!(unprotect_secret(&encrypted).expect("unprotect"), plaintext);
    }

    #[test]
    fn empty_secrets_are_rejected() {
        assert!(matches!(protect_secret(&[]), Err(SecretError::Empty)));
        assert!(matches!(unprotect_secret(&[]), Err(SecretError::Empty)));
    }
}
