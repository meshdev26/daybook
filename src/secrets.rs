//! At-rest secret encryption for values stored in the SQLite `config` table.
//!
//! Security contract: a secret written through [`Db::try_set_secret_config`](crate::db::Db::try_set_secret_config)
//! is sealed with AES-256-GCM under a 32-byte `MASTER_KEY` and persisted as a
//! versioned, self-describing string:
//!
//! ```text
//! enc:v1:<base64( nonce[12] || ciphertext || tag[16] )>
//! ```
//!
//! The nonce is fresh from the OS CSPRNG for every write. The AEAD tag is
//! verified in constant time by the cipher, so a tampered blob or a wrong key
//! is indistinguishable from random data and never yields plaintext.
//!
//! Values without the `enc:` prefix are treated as **legacy plaintext** and
//! returned verbatim: deployments that predate this module keep working and
//! can migrate by simply re-saving each secret once MASTER_KEY is set.
//!
//! Neither the key nor any plaintext is ever written to logs or error strings.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use zeroize::Zeroize;

/// Prefix of an encrypted config value. `v1` is the AEAD envelope version.
pub const SECRET_PREFIX: &str = "enc:v1:";

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error(
        "MASTER_KEY is not set; set a 32-byte key (base64 or 64 hex chars) before storing secrets"
    )]
    MissingMasterKey,
    #[error("invalid MASTER_KEY: expected 32 bytes as base64 or 64 hex characters")]
    InvalidMasterKey,
    #[error("secret encryption failed")]
    Encrypt,
    #[error("secret cannot be decrypted (tampered, corrupt, or wrong key)")]
    Decrypt,
    #[error("decrypted secret is not valid UTF-8")]
    InvalidUtf8,
    #[error("this config key is not a secret")]
    NotSensitiveKey,
}

/// A 32-byte AES-256 key. Dropping it wipes the buffer.
#[derive(Clone)]
pub struct MasterKey([u8; KEY_LEN]);

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl MasterKey {
    /// Construct from decoded key bytes. Only `config::parse_master_key`
    /// (the MASTER_KEY parser) creates these in production.
    pub(crate) fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

/// Whether `stored` is a versioned encrypted secret rather than a legacy
/// plaintext value.
pub fn is_encrypted(stored: &str) -> bool {
    stored.starts_with(SECRET_PREFIX)
}

/// Base64 (standard alphabet, padded) decode for key material and envelopes.
/// Rejects non-alphabet bytes, misplaced padding and lengths that are not a
/// multiple of four. Unpadded 43-character keys are accepted by the parser
/// (it adds the single missing `=` before calling this).
pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>, SecretError> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(SecretError::InvalidMasterKey);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (chunk_index, chunk) in bytes.chunks(4).enumerate() {
        let last = (chunk_index + 1) * 4 == bytes.len();
        let mut sextets = [0u8; 4];
        let mut pad_at: Option<usize> = None;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                // A second '=' is valid only immediately after the first
                // (the "==" padding of a one-byte final group).
                if !last || i < 2 || pad_at.is_some_and(|p| p != i - 1) {
                    return Err(SecretError::InvalidMasterKey);
                }
                // Keep the FIRST pad position: "vw==" is a one-byte group
                // (pad_at 2), while "vwA=" is a two-byte group (pad_at 3).
                if pad_at.is_none() {
                    pad_at = Some(i);
                }
                sextets[i] = 64;
            } else {
                if pad_at.is_some() {
                    return Err(SecretError::InvalidMasterKey);
                }
                sextets[i] = B64_ALPHABET
                    .iter()
                    .position(|&a| a == c)
                    .ok_or(SecretError::InvalidMasterKey)? as u8;
            }
        }
        if pad_at == Some(2) && sextets[3] != 64 {
            return Err(SecretError::InvalidMasterKey);
        }
        let bits = ((sextets[0] as u32) << 18)
            | ((sextets[1] as u32) << 12)
            | (((sextets[2] & 63) as u32) << 6)
            | ((sextets[3] & 63) as u32);
        out.push((bits >> 16) as u8);
        if pad_at != Some(2) {
            out.push((bits >> 8) as u8);
        }
        if pad_at.is_none() {
            out.push(bits as u8);
        }
    }
    Ok(out)
}

fn encode_base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let bits = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[((bits >> 18) & 63) as usize] as char);
        out.push(B64_ALPHABET[((bits >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Hexadecimal decode for the 64-character MASTER_KEY form.
pub(crate) fn decode_hex(input: &str) -> Result<[u8; KEY_LEN], SecretError> {
    if input.len() != KEY_LEN * 2 {
        return Err(SecretError::InvalidMasterKey);
    }
    let bytes = input.as_bytes();
    let mut out = [0u8; KEY_LEN];
    for (i, pair) in bytes.chunks(2).enumerate() {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or(SecretError::InvalidMasterKey)?;
        let lo = (pair[1] as char)
            .to_digit(16)
            .ok_or(SecretError::InvalidMasterKey)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

fn random_nonce() -> Result<[u8; NONCE_LEN], SecretError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| SecretError::Encrypt)?;
    Ok(nonce)
}

/// Seal `plaintext` under `key` into a versioned, self-describing string.
pub fn encrypt_secret(key: &MasterKey, plaintext: &[u8]) -> Result<String, SecretError> {
    let nonce = random_nonce()?;
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| SecretError::Encrypt)?;
    let mut sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| SecretError::Encrypt)?;
    let mut blob = Vec::with_capacity(NONCE_LEN + sealed.len());
    blob.extend_from_slice(&nonce);
    blob.append(&mut sealed);
    Ok(format!("{SECRET_PREFIX}{}", encode_base64(&blob)))
}

/// Open `stored`. Unprefixed values are legacy plaintext and are returned
/// verbatim; any `enc:` value that is not a well-formed v1 envelope, or that
/// fails authentication, is an error. The AEAD tag check is constant time.
pub fn decrypt_secret(key: &MasterKey, stored: &str) -> Result<Vec<u8>, SecretError> {
    if !is_encrypted(stored) {
        if stored.starts_with("enc:") {
            return Err(SecretError::Decrypt);
        }
        return Ok(stored.as_bytes().to_vec());
    }
    let payload = &stored[SECRET_PREFIX.len()..];
    let blob = decode_base64(payload).map_err(|_| SecretError::Decrypt)?;
    if blob.len() <= NONCE_LEN {
        return Err(SecretError::Decrypt);
    }
    let (nonce, sealed) = blob.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| SecretError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| SecretError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_master_key;

    fn key() -> MasterKey {
        parse_master_key(&"11".repeat(32)).unwrap()
    }

    #[test]
    fn base64_key_form_parses() {
        let hex = "22".repeat(32);
        // 32 zero-padded bytes encoded with the standard alphabet.
        let b64 = encode_base64(&[0x22; KEY_LEN]);
        assert_eq!(decode_base64(&b64).unwrap(), vec![0x22; KEY_LEN]);
        assert_eq!(parse_master_key(&b64).unwrap().as_bytes(), &[0x22; KEY_LEN]);
        assert_eq!(parse_master_key(&hex).unwrap().as_bytes(), &[0x22; KEY_LEN]);
        assert_eq!(
            parse_master_key(b64.trim_end_matches('='))
                .unwrap()
                .as_bytes(),
            &[0x22; KEY_LEN]
        );
    }

    #[test]
    fn invalid_keys_are_rejected() {
        for bad in [
            "",
            "abc",
            &"zz".repeat(32),
            &"11".repeat(31),
            "not-a-key",
            &"-".repeat(43),
        ] {
            assert!(parse_master_key(bad).is_err(), "accepted bad key: {bad}");
        }
    }

    #[test]
    fn round_trip_and_fresh_nonce() {
        let mk = key();
        let a = encrypt_secret(&mk, b"token-A").unwrap();
        let b = encrypt_secret(&mk, b"token-A").unwrap();
        assert!(a.starts_with(SECRET_PREFIX));
        assert_ne!(a, b, "nonce must be fresh per write");
        assert_eq!(decrypt_secret(&mk, &a).unwrap(), b"token-A");
        assert_eq!(decrypt_secret(&mk, &b).unwrap(), b"token-A");
    }

    #[test]
    fn round_trip_with_padded_base64_lengths() {
        let mk = key();
        // Lengths that produce one or two '=' characters in the envelope's
        // base64 must decode back to the exact same bytes.
        for n in 0..16 {
            let plaintext = vec![0x5a; n];
            let stored = encrypt_secret(&mk, &plaintext).unwrap();
            assert_eq!(decrypt_secret(&mk, &stored).unwrap(), plaintext, "len {n}");
        }
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mk = key();
        let stored = encrypt_secret(&mk, b"token-A").unwrap();
        let mut chars: Vec<char> = stored.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert_eq!(decrypt_secret(&mk, &tampered), Err(SecretError::Decrypt));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let other = parse_master_key(&"33".repeat(32)).unwrap();
        let stored = encrypt_secret(&key(), b"token-A").unwrap();
        assert_eq!(decrypt_secret(&other, &stored), Err(SecretError::Decrypt));
    }

    #[test]
    fn legacy_plaintext_is_returned_verbatim() {
        assert_eq!(
            decrypt_secret(&key(), "plain-token").unwrap(),
            b"plain-token"
        );
        assert_eq!(
            decrypt_secret(&key(), "enc:v0:AAAA").unwrap_err(),
            SecretError::Decrypt
        );
    }

    #[test]
    fn corrupt_envelopes_are_rejected() {
        for bad in [SECRET_PREFIX, "enc:v1:!!!", "enc:v1:AAAA", "enc:v1:AA=="] {
            assert_eq!(
                decrypt_secret(&key(), bad),
                Err(SecretError::Decrypt),
                "{bad}"
            );
        }
    }
}
