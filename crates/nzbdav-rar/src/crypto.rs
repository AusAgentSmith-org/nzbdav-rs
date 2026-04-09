//! RAR5 password-protected archive decryption.
//!
//! RAR5 uses PBKDF2-HMAC-SHA256 to derive an AES-256-CBC key from a password.
//! The encryption header (type 4) contains KDF params (salt, iteration count,
//! optional password check value). All headers after the encryption header are
//! encrypted as a single AES-256-CBC block.

use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{RarError, Result};

type Aes256CbcDec = cbc::Decryptor<Aes256>;
type HmacSha256 = Hmac<Sha256>;

/// Parameters parsed from a RAR5 encryption header (type 4).
#[derive(Debug, Clone)]
pub struct Rar5EncryptionHeader {
    pub lg2_count: u8,
    pub salt: Vec<u8>,
    pub has_psw_check: bool,
    /// 8-byte password check value (only present if `has_psw_check`).
    pub psw_check: Vec<u8>,
    /// 4-byte password check sum (only present if `has_psw_check`).
    pub psw_check_sum: Vec<u8>,
}

/// AES-256-CBC key + IV derived from the password and encryption header.
#[derive(Debug, Clone)]
pub struct DerivedKey {
    /// 32-byte AES-256 key.
    pub key: [u8; 32],
    /// 16-byte IV from the encryption header (stored externally, not derived).
    pub iv: [u8; 16],
}

/// Derive the RAR5 AES-256 key from a password and encryption header params.
///
/// The RAR5 KDF works as follows:
/// 1. Extend the salt with a 4-byte block counter: `salt_extended = salt ++ [0,0,0,1]`
/// 2. Run PBKDF2-like iterations using HMAC-SHA256:
///    - Round 0: `iterations = 1 << lg2_count` iterations -> `result[0]` (32 bytes, the AES key)
///    - Round 1: 16 more iterations -> `result[1]` (unused here)
///    - Round 2: 16 more iterations -> `result[2]` (used for password check)
/// 3. For password verification: XOR-fold `result[2]` (32 bytes) down to 8 bytes.
pub fn derive_rar5_key(password: &str, header: &Rar5EncryptionHeader) -> Result<DerivedKey> {
    let iterations = 1u32 << (header.lg2_count as u32);
    let password_bytes = password.as_bytes();

    // Extend salt with block counter [0, 0, 0, 1]
    let mut salt_extended = header.salt.clone();
    salt_extended.extend_from_slice(&[0, 0, 0, 1]);

    // Run the 3 rounds of the KDF
    let round_counts = [iterations, 17, 17];
    let mut results: Vec<[u8; 32]> = Vec::with_capacity(3);

    // First HMAC: U_1 = HMAC(password, salt_extended)
    let mut mac = HmacSha256::new_from_slice(password_bytes)
        .map_err(|e| RarError::DecryptionError(format!("HMAC init error: {e}")))?;
    mac.update(&salt_extended);
    let mut block: [u8; 32] = mac.finalize().into_bytes().into();

    let mut final_hash = block;

    for &count in &round_counts {
        for _ in 1..count {
            let mut mac = HmacSha256::new_from_slice(password_bytes)
                .map_err(|e| RarError::DecryptionError(format!("HMAC init error: {e}")))?;
            mac.update(&block);
            block = mac.finalize().into_bytes().into();

            // XOR into final_hash
            for (f, b) in final_hash.iter_mut().zip(block.iter()) {
                *f ^= *b;
            }
        }

        results.push(final_hash);
    }

    // Verify password if check value is present
    if header.has_psw_check && header.psw_check.len() == 8 {
        let mut derived_check = [0u8; 8];
        for (i, &byte) in results[2].iter().enumerate() {
            derived_check[i % 8] ^= byte;
        }
        if derived_check != header.psw_check.as_slice() {
            return Err(RarError::IncorrectPassword);
        }
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&results[0]);

    // IV is not derived here — it comes from the encrypted header's data area.
    // We'll store a zeroed IV; the caller sets it from the first 16 bytes of
    // the encrypted data block.
    Ok(DerivedKey { key, iv: [0u8; 16] })
}

/// Decrypt a RAR5 encrypted data block using AES-256-CBC.
///
/// RAR5 encrypted headers are padded to a 16-byte boundary. The decrypted
/// data may contain padding bytes at the end; the caller should parse headers
/// from the decrypted data and stop when no more valid headers are found.
pub fn decrypt_rar5_headers(data: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    // AES-256-CBC requires data to be a multiple of 16 bytes.
    // When we only fetch partial data (first few segments), the buffer
    // may not be aligned. Truncate to the last full 16-byte block.
    let aligned_len = data.len() - (data.len() % 16);
    if aligned_len == 0 {
        return Err(RarError::DecryptionError(
            "encrypted data too short for decryption".to_string(),
        ));
    }

    let mut buf = data[..aligned_len].to_vec();

    // Decrypt in place
    Aes256CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| RarError::DecryptionError(format!("AES-256-CBC decrypt error: {e}")))?;

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_key_deterministic() {
        let header = Rar5EncryptionHeader {
            lg2_count: 15,
            salt: vec![0u8; 16],
            has_psw_check: false,
            psw_check: Vec::new(),
            psw_check_sum: Vec::new(),
        };

        let key1 = derive_rar5_key("test", &header).unwrap();
        let key2 = derive_rar5_key("test", &header).unwrap();
        assert_eq!(key1.key, key2.key);
    }

    #[test]
    fn derive_key_different_passwords() {
        let header = Rar5EncryptionHeader {
            lg2_count: 10,
            salt: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            has_psw_check: false,
            psw_check: Vec::new(),
            psw_check_sum: Vec::new(),
        };

        let key1 = derive_rar5_key("password1", &header).unwrap();
        let key2 = derive_rar5_key("password2", &header).unwrap();
        assert_ne!(key1.key, key2.key);
    }

    #[test]
    fn decrypt_empty_data() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        let result = decrypt_rar5_headers(&[], &key, &iv).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn decrypt_rejects_non_aligned_data() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        let data = vec![0u8; 15]; // not a multiple of 16
        let result = decrypt_rar5_headers(&data, &key, &iv);
        assert!(result.is_err());
    }

    #[test]
    fn derive_key_known_values() {
        // Known good values from debugging a real RAR5 encrypted archive.
        let salt = hex::decode("8246bf4a50c80674189774196e0551b3").unwrap();
        let psw_check = hex::decode("c0fa7586afb0c24c").unwrap();
        let header = Rar5EncryptionHeader {
            lg2_count: 15,
            salt,
            has_psw_check: true,
            psw_check,
            psw_check_sum: Vec::new(),
        };

        let result = derive_rar5_key("U0b7258526OROQY", &header);
        assert!(
            result.is_ok(),
            "derive_rar5_key should succeed with correct password: {result:?}"
        );
    }

    #[test]
    fn derive_key_wrong_password() {
        let salt = hex::decode("8246bf4a50c80674189774196e0551b3").unwrap();
        let psw_check = hex::decode("c0fa7586afb0c24c").unwrap();
        let header = Rar5EncryptionHeader {
            lg2_count: 15,
            salt,
            has_psw_check: true,
            psw_check,
            psw_check_sum: Vec::new(),
        };

        let result = derive_rar5_key("wrong_password", &header);
        assert!(
            result.is_err(),
            "derive_rar5_key should fail with wrong password"
        );
        assert!(
            matches!(result, Err(RarError::IncorrectPassword)),
            "expected IncorrectPassword error, got: {result:?}"
        );
    }

    #[test]
    fn derive_key_no_check() {
        // When has_psw_check is false, key derivation always succeeds
        // (no password verification is performed).
        let header = Rar5EncryptionHeader {
            lg2_count: 15,
            salt: vec![0xAA; 16],
            has_psw_check: false,
            psw_check: Vec::new(),
            psw_check_sum: Vec::new(),
        };

        let result = derive_rar5_key("anything", &header);
        assert!(
            result.is_ok(),
            "derive_rar5_key with no check should always succeed: {result:?}"
        );

        // Even with a different password it succeeds (no verification)
        let result2 = derive_rar5_key("totally_different", &header);
        assert!(result2.is_ok());

        // But the derived keys should differ
        assert_ne!(result.unwrap().key, result2.unwrap().key);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        use aes::Aes256;
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};

        type Aes256CbcEnc = cbc::Encryptor<Aes256>;

        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"hello world 1234"; // exactly 16 bytes

        // Encrypt
        let mut buf = plaintext.to_vec();
        let enc = Aes256CbcEnc::new(&key.into(), &iv.into());
        enc.encrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf, 16)
            .unwrap();

        // Decrypt
        let decrypted = decrypt_rar5_headers(&buf, &key, &iv).unwrap();
        assert_eq!(&decrypted, plaintext);
    }
}
