//! Encrypted at-rest storage for the personal-account MTProto session blob.
//!
//! Security contract (ratified user-identity design, §5): the grammers
//! session is an account-password-equivalent credential. It is stored ONLY as
//! one AES-256-GCM blob at `data_dir()/identity/session.bin`, mode 0600,
//! written through a sibling temp file + atomic rename (the same pattern as
//! `config.rs`'s `.env` writer). The blob never touches the database, the
//! `backups/` scripts, or the logs.
//!
//! Key handling: the AEAD key is derived from an operator-entered unlock
//! passphrase (PBKDF2-HMAC-SHA256, 600k iterations — the security normative
//! floor, with a fresh random 16-byte salt per save). The passphrase itself
//! is never persisted anywhere; a restart re-prompts via the unlock banner.
//! The derived key is zeroized after each operation.
//!
//! On-disk layout (binary, no base64):
//!
//! ```text
//! salt[16] || nonce[12] || AES-256-GCM(ciphertext || 16-byte tag)
//! ```
//!
//! The salt is not secret (it only forces per-save key uniqueness); the
//! nonce is fresh random per save, which is safe under GCM because every
//! save derives a fresh key from a fresh salt. Inside the AEAD sits a JSON
//! envelope:
//!
//! ```json
//! { "schema_version": 1,
//!   "grammers_version": "0.11.0",
//!   "session": { ...SessionData as serde_json::Value... } }
//! ```
//!
//! A blob written by a different grammers version refuses to load
//! ([`SessionStoreError::VersionMismatch`]) — the refuse-and-re-pair
//! decision — because grammers' session serialization is not semver-stable.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12; // GCM's standard 96-bit nonce
const KEY_LEN: usize = 32; // AES-256
const GCM_TAG_LEN: usize = 16; // GCM's 128-bit authentication tag

/// security normative floor for passphrase stretching.
const PBKDF2_ITERATIONS: u32 = 600_000;

const SCHEMA_VERSION: u32 = 1;

/// The grammers-client version this crate is built against. `grammers-client`
/// does not export its own version as a constant, and `env!("CARGO_PKG_VERSION")`
/// would report *this* crate's version, so the exact `=0.11.0` pin from
/// `Cargo.toml` is mirrored here. A unit test asserts the mirror still matches
/// the manifest, so a dependency bump cannot silently desync the check.
const CRATE_GRAMMERS_VERSION: &str = "0.11.0";

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("no personal-identity session is stored on this machine yet")]
    NotFound,
    #[error(
        "wrong unlock passphrase (or the session blob was tampered with); \
         the blob was left untouched"
    )]
    WrongPassphrase,
    #[error(
        "session blob was written by grammers {blob_grammers} but this build \
         embeds {crate_grammers}; refusing to load it — re-pair the personal \
         identity to continue"
    )]
    VersionMismatch {
        blob_grammers: String,
        crate_grammers: String,
    },
    #[error("session blob is corrupt (not a valid encrypted envelope)")]
    Corrupt,
    #[error("session store io error: {0}")]
    Io(#[from] io::Error),
    #[error("session store crypto error: {0}")]
    Crypto(String),
}

/// The decrypted inner envelope. Serde-visible shape of the AEAD plaintext;
/// `session` stays an opaque [`serde_json::Value`] so the store never has to
/// know grammers' `SessionData` layout (keeps this module compilable and
/// testable without a real session or a live Telegram connection).
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    schema_version: u32,
    grammers_version: String,
    session: serde_json::Value,
}

/// Filesystem handle for the encrypted session blob. Cheap to clone-construct;
/// all methods are synchronous and blocking (called from the identity actor).
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// Store at `<data_dir>/identity/session.bin`. Callers pass
    /// `config::data_dir()`; the `identity/` subdirectory is created on save.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("identity").join("session.bin"),
        }
    }

    /// Whether a session blob is present on disk. `true` only for a regular
    /// file — a leftover directory or dangling path reads as "not set up".
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Best-effort removal of the session blob (the re-pair/wipe path).
    /// Returns whether a blob file existed before the call; an unlink
    /// failure is logged (path and io error kind only — never session
    /// material) and still reports `true`, so callers that must guarantee
    /// removal re-check [`Self::exists`] afterwards.
    pub fn remove(&self) -> bool {
        if !self.exists() {
            return false;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::info!(
                    path = %self.path.display(),
                    "personal-identity session blob removed"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "personal-identity session blob removal failed"
                );
                true
            }
        }
    }

    /// Encrypt and atomically persist the session. A fresh salt (and therefore
    /// a fresh derived key) is used on every call, so re-saving the same
    /// session still rotates the blob on disk.
    pub fn save(
        &self,
        passphrase: &str,
        session_json: &serde_json::Value,
    ) -> Result<(), SessionStoreError> {
        let salt = random_bytes::<SALT_LEN>()?;
        let nonce_bytes = random_bytes::<NONCE_LEN>()?;
        let envelope = Envelope {
            schema_version: SCHEMA_VERSION,
            grammers_version: CRATE_GRAMMERS_VERSION.to_string(),
            session: session_json.clone(),
        };
        let plaintext = serde_json::to_vec(&envelope)
            .map_err(|e| SessionStoreError::Crypto(format!("serialize envelope: {e}")))?;
        let mut key = derive_key(passphrase, &salt);
        let sealed = seal(&key, &nonce_bytes, &plaintext);
        key.zeroize();
        let ciphertext = sealed?;

        let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&salt);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        self.write_atomically(&blob)?;
        // Path and size only — never the passphrase, the key, or any session
        // material (security: session material must never reach the logs).
        tracing::info!(
            path = %self.path.display(),
            bytes = blob.len(),
            "personal-identity session stored"
        );
        Ok(())
    }

    /// Decrypt and return the stored session JSON. Errors are discriminated
    /// for the unlock UX: [`SessionStoreError::NotFound`] (not set up),
    /// [`SessionStoreError::WrongPassphrase`] (wrong passphrase or tampered
    /// blob — AES-GCM cannot tell the two apart, and must not pretend to),
    /// [`SessionStoreError::VersionMismatch`] (written by another grammers
    /// version → re-pair), [`SessionStoreError::Corrupt`], plus `Io`/`Crypto`.
    pub fn load(&self, passphrase: &str) -> Result<serde_json::Value, SessionStoreError> {
        if !self.exists() {
            return Err(SessionStoreError::NotFound);
        }
        let blob = fs::read(&self.path)?;
        if blob.len() < SALT_LEN + NONCE_LEN + GCM_TAG_LEN {
            tracing::debug!(
                path = %self.path.display(),
                len = blob.len(),
                "session blob shorter than its fixed header"
            );
            return Err(SessionStoreError::Corrupt);
        }
        let (salt, rest) = blob.split_at(SALT_LEN);
        let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);

        let mut key = derive_key(passphrase, salt);
        let opened = open(&key, nonce_bytes, ciphertext);
        key.zeroize();
        let plaintext = opened?;

        let envelope: Envelope = serde_json::from_slice(&plaintext).map_err(|e| {
            tracing::debug!(error = %e, "session blob decrypted to a non-envelope payload");
            SessionStoreError::Corrupt
        })?;
        if envelope.schema_version != SCHEMA_VERSION {
            tracing::debug!(
                schema = envelope.schema_version,
                "session blob has an unknown schema_version"
            );
            return Err(SessionStoreError::Corrupt);
        }
        if envelope.grammers_version != CRATE_GRAMMERS_VERSION {
            return Err(SessionStoreError::VersionMismatch {
                blob_grammers: envelope.grammers_version,
                crate_grammers: CRATE_GRAMMERS_VERSION.to_string(),
            });
        }
        Ok(envelope.session)
    }

    /// Temp file + atomic rename, mirroring `config.rs`'s `.env` writer: the
    /// sibling temp is CREATED with mode 0600 (unix) so the credential never
    /// exists world-readable even for an instant, `sync_all`'d for durability
    /// (a crash between rename and page flush must not resurrect a torn or
    /// zero pre-image), then renamed over the target — rename(2) within one
    /// directory is atomic. Post-rename the final mode is verified and, if a
    /// filesystem ignored it, re-applied once; if it still cannot be
    /// restricted, save fails rather than leaving credential material
    /// group/world-readable.
    fn write_atomically(&self, blob: &[u8]) -> Result<(), SessionStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = self.path.with_extension("bin.tmp");
        use std::io::Write as _;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut tmp = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            tmp.write_all(blob)?;
            tmp.flush()?;
            tmp.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let mut tmp = fs::File::create(&tmp_path)?;
            tmp.write_all(blob)?;
            tmp.flush()?;
            tmp.sync_all()?;
        }
        // Belt-and-braces for a stale temp left by an earlier run: open(2)
        // mode only applies at creation, never to an existing file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        if let Err(e) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(io::Error::new(
                e.kind(),
                format!("cannot write {}: {e}", self.path.display()),
            )
            .into());
        }
        #[cfg(unix)]
        self.verify_owner_only_permissions()?;
        Ok(())
    }

    /// Post-write permission enforcement (unix). Returns an error if the blob
    /// cannot be held at 0600 — a credential store must not degrade silently.
    #[cfg(unix)]
    fn verify_owner_only_permissions(&self) -> Result<(), SessionStoreError> {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&self.path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
            let mode = fs::metadata(&self.path)?.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "cannot restrict {} to owner-only (0600); refusing to \
                         store credential material there",
                        self.path.display()
                    ),
                )
                .into());
            }
        }
        Ok(())
    }
}

/// PBKDF2-HMAC-SHA256 over the unlock passphrase. PBKDF2 itself is infallible
/// (fixed 32-byte output buffer), so no Result here.
fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), salt, PBKDF2_ITERATIONS, &mut key);
    key
}

/// AES-256-GCM encrypt. Errors are opaque by construction (the AEAD must not
/// leak structure), surfaced as [`SessionStoreError::Crypto`].
fn seal(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, SessionStoreError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| SessionStoreError::Crypto(format!("aes key init: {e}")))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|e| SessionStoreError::Crypto(format!("encrypt: {e}")))
}

/// AES-256-GCM decrypt. An authentication failure means the ciphertext (or
/// the derived key) does not match what was sealed — indistinguishable
/// between "wrong passphrase" and "tampered blob", reported as
/// [`SessionStoreError::WrongPassphrase`]. A blob that authenticates but does
/// not parse is [`SessionStoreError::Corrupt`] instead, so the unlock UX can
/// say "wrong passphrase" with confidence.
fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, SessionStoreError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| SessionStoreError::Crypto(format!("aes key init: {e}")))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| SessionStoreError::WrongPassphrase)
}

/// OS randomness for salts and nonces. Entropy failure is a crypto failure,
/// not an io failure — the filesystem is fine, the key material is not.
fn random_bytes<const N: usize>() -> Result<[u8; N], SessionStoreError> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf)
        .map_err(|e| SessionStoreError::Crypto(format!("os entropy source: {e}")))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PASS: &str = "correct horse battery staple #1";

    fn store(dir: &Path) -> SessionStore {
        SessionStore::new(dir)
    }

    /// A stand-in for grammers' SessionData: the store treats it as an opaque
    /// JSON value, so a fake one exercises the full save/load path without a
    /// real login.
    fn fake_session() -> serde_json::Value {
        json!({
            "dc": 2,
            "addr": "149.154.167.40:443",
            "auth_key_b64": "ZmFrZS1hdXRoLWtleS1ub3QtcmVhbA==",
            "user_id": 8_675_309
        })
    }

    fn blob_path(dir: &Path) -> PathBuf {
        dir.join("identity").join("session.bin")
    }

    #[test]
    fn round_trip_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert!(!store.exists(), "nothing stored yet");
        store.save(PASS, &fake_session()).unwrap();
        assert!(store.exists());
        let loaded = store.load(PASS).unwrap();
        assert_eq!(loaded, fake_session());
    }

    #[test]
    fn load_without_save_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert!(matches!(store.load(PASS), Err(SessionStoreError::NotFound)));
        assert!(!store.exists());
    }

    /// `remove()` reports existence honestly and is idempotent: `true` the
    /// first time (blob gone afterwards), `false` on every further call.
    #[test]
    fn remove_reports_existence_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert!(!store.remove(), "no blob yet: remove must report false");
        store.save(PASS, &fake_session()).unwrap();
        assert!(store.remove(), "blob existed: remove must report true");
        assert!(!store.exists(), "blob must be gone after remove");
        assert!(!store.remove(), "second remove must report false");
        // The identity/ directory itself stays (harmless empty dir).
        assert!(dir.path().join("identity").is_dir());
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(PASS, &fake_session()).unwrap();
        assert!(matches!(
            store.load("a different passphrase"),
            Err(SessionStoreError::WrongPassphrase)
        ));
        // The original blob must survive a wrong-passphrase attempt.
        store.load(PASS).unwrap();
    }

    #[test]
    fn tampered_ciphertext_is_never_silent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(PASS, &fake_session()).unwrap();
        let path = blob_path(dir.path());
        let mut blob = fs::read(&path).unwrap();
        // Flip one bit in the tag region (last byte of the file): GCM
        // authentication must fail. A successful decrypt of a tampered blob
        // would be the worst possible outcome, so the assertion below accepts
        // only the two explicit failure variants.
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        fs::write(&path, &blob).unwrap();
        match store.load(PASS) {
            Err(SessionStoreError::WrongPassphrase) => {}
            Err(SessionStoreError::Corrupt) => {}
            other => panic!("tampered blob must not load: {other:?}"),
        }
    }

    #[test]
    fn truncated_or_garbage_file_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        // Shorter than the fixed header — cannot even locate salt/nonce/tag.
        fs::create_dir_all(dir.path().join("identity")).unwrap();
        fs::write(blob_path(dir.path()), b"\x00\x01\x02").unwrap();
        assert!(matches!(store.load(PASS), Err(SessionStoreError::Corrupt)));
        // Valid length but random content: GCM auth fails, which reads as
        // tamper (WrongPassphrase) — also acceptable, never a silent Ok.
        let mut garbage = vec![0u8; SALT_LEN + NONCE_LEN + 64];
        getrandom::fill(&mut garbage).unwrap();
        fs::write(blob_path(dir.path()), &garbage).unwrap();
        match store.load(PASS) {
            Err(SessionStoreError::WrongPassphrase) => {}
            Err(SessionStoreError::Corrupt) => {}
            other => panic!("garbage blob must not load: {other:?}"),
        }
    }

    #[test]
    fn version_mismatch_refuses_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        // Seal an envelope that claims to come from a different grammers,
        // exactly as a future post-bump blob would look.
        let envelope = Envelope {
            schema_version: SCHEMA_VERSION,
            grammers_version: "9.9.9".to_string(),
            session: fake_session(),
        };
        let plaintext = serde_json::to_vec(&envelope).unwrap();
        let salt = random_bytes::<SALT_LEN>().unwrap();
        let nonce = random_bytes::<NONCE_LEN>().unwrap();
        let mut key = derive_key(PASS, &salt);
        let sealed = seal(&key, &nonce, &plaintext);
        key.zeroize();
        let ciphertext = sealed.unwrap();
        let mut blob = Vec::new();
        blob.extend_from_slice(&salt);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);
        fs::create_dir_all(dir.path().join("identity")).unwrap();
        fs::write(blob_path(dir.path()), &blob).unwrap();

        match store.load(PASS) {
            Err(SessionStoreError::VersionMismatch {
                blob_grammers,
                crate_grammers,
            }) => {
                assert_eq!(blob_grammers, "9.9.9");
                assert_eq!(crate_grammers, CRATE_GRAMMERS_VERSION);
            }
            other => panic!("foreign-version blob must refuse to load: {other:?}"),
        }
    }

    #[test]
    fn each_save_uses_a_fresh_salt_and_nonce() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(PASS, &fake_session()).unwrap();
        let first = fs::read(blob_path(dir.path())).unwrap();
        store.save(PASS, &fake_session()).unwrap();
        let second = fs::read(blob_path(dir.path())).unwrap();
        assert_ne!(
            first, second,
            "a fresh salt+nonce per save must rotate the on-disk bytes"
        );
        // The rotated blob still loads with the same passphrase.
        assert_eq!(store.load(PASS).unwrap(), fake_session());
    }

    #[test]
    fn grammers_version_const_matches_manifest() {
        let manifest = include_str!("../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("grammers-client"))
            .expect("grammers-client must stay declared in Cargo.toml");
        assert!(
            line.contains("\"=0.11.0\""),
            "grammers-client was re-versioned in Cargo.toml; update \
             CRATE_GRAMMERS_VERSION in src/identity/session.rs to match and \
             re-decide the refuse-and-re-pair semantics"
        );
        assert!(
            line.contains("default-features = false"),
            "grammers-client must keep default-features off (feature \
             discipline: no libsql, no tokio/fs)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn saved_blob_is_owner_only_and_temp_is_cleaned_up() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(PASS, &fake_session()).unwrap();
        let mode = fs::metadata(blob_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "credential blob must be owner-only");
        let tmp = dir.path().join("identity").join("session.bin.tmp");
        assert!(!tmp.exists(), "temp file must be gone after the rename");
    }
}
