//! `EncryptedStore` — the v4 replacement for the Python `storage.py`.
//!
//! A password-locked key/value store on disk: no plaintext secrets ever land on
//! the customer's machine. It holds the remembered login and settings (not VPN
//! credentials — those are typed each connect). Mirrors the Python design:
//! PBKDF2-HMAC-SHA256 (200k iterations) derives the key from the launch
//! password; here the payload is sealed with AES-256-GCM (authenticated, so a
//! wrong password or a tampered file fails cleanly rather than returning junk).
//!
//! On-disk layout (one file):  MAGIC(4) | salt(16) | nonce(12) | ciphertext
//! The ciphertext is a JSON object of string→string.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;
use sha2::Sha256;

use dreamnail_core::ports::{EngineError, Result};

const MAGIC: &[u8; 4] = b"DNV4";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const PBKDF2_ITERS: u32 = 200_000;

fn store_err(msg: impl Into<String>) -> EngineError {
    EngineError::Proxy(msg.into()) // reuse a generic variant; storage isn't a port
}

fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ITERS, &mut key);
    key
}

pub struct EncryptedStore {
    path: PathBuf,
    salt: [u8; SALT_LEN],
    key: [u8; 32],
    data: BTreeMap<String, String>,
}

impl EncryptedStore {
    /// Open (or create) the store at `path`, unlocking it with `password`.
    ///
    /// If the file exists, the password must decrypt it (a wrong password is a
    /// clean error, not corruption). If it doesn't exist, a new empty store is
    /// created in memory with a fresh salt and written on the first `save`.
    pub fn open(path: impl AsRef<Path>, password: &str) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            let blob = std::fs::read(&path).map_err(|e| store_err(format!("read store: {e}")))?;
            // Only a genuine, COMPLETE DNV4 store is decrypted here. Two kinds of
            // file fall through to "start fresh" instead, because neither can
            // possibly hold data of ours:
            //   * not a DNV4 store at all — e.g. an old Python/Fernet `config.enc`
            //     (magic "gAAA…") left by a prior version. Failing here meant
            //     unlock failed forever and nothing persisted across the upgrade.
            //   * too short to even hold the header — a torn write from a crash or
            //     power-cut mid-save. Failing here was a permanent lockout.
            // A COMPLETE store with the WRONG password still errors below, so a
            // typo never silently wipes real data.
            let header = MAGIC.len() + SALT_LEN + NONCE_LEN;
            if blob.len() >= header && &blob[..MAGIC.len()] == MAGIC {
                Self::from_blob(path, password, &blob)
            } else {
                Ok(Self::fresh(path, password))
            }
        } else {
            Ok(Self::fresh(path, password))
        }
    }

    /// A brand-new empty store with a fresh salt, held in memory until `save`.
    fn fresh(path: PathBuf, password: &str) -> Self {
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        let key = derive_key(password, &salt);
        Self {
            path,
            salt,
            key,
            data: BTreeMap::new(),
        }
    }

    fn from_blob(path: PathBuf, password: &str, blob: &[u8]) -> Result<Self> {
        let min = MAGIC.len() + SALT_LEN + NONCE_LEN;
        if blob.len() < min || &blob[..MAGIC.len()] != MAGIC {
            return Err(store_err("store file is not a valid Dreamnail store"));
        }
        let mut off = MAGIC.len();
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&blob[off..off + SALT_LEN]);
        off += SALT_LEN;
        let nonce_bytes = &blob[off..off + NONCE_LEN];
        off += NONCE_LEN;
        let ciphertext = &blob[off..];

        let key = derive_key(password, &salt);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            // A wrong password or tampered file fails the GCM tag here.
            .map_err(|_| store_err("wrong password or corrupted store"))?;
        let data: BTreeMap<String, String> =
            serde_json::from_slice(&plaintext).map_err(|e| store_err(format!("decode: {e}")))?;
        Ok(Self {
            path,
            salt,
            key,
            data,
        })
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.data.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.data.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.data.remove(key);
    }

    /// Encrypt + write the store atomically (write temp, then rename).
    pub fn save(&self) -> Result<()> {
        let plaintext =
            serde_json::to_vec(&self.data).map_err(|e| store_err(format!("encode: {e}")))?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_ref())
            .map_err(|_| store_err("encryption failed"))?;

        let mut blob = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(MAGIC);
        blob.extend_from_slice(&self.salt);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| store_err(format!("mkdir: {e}")))?;
        }
        // Write → FSYNC → rename. Without the fsync the rename can land before the
        // bytes do, so a crash/power-cut leaves a half-written file under the real
        // name — and the store is the only copy of the saved login + settings.
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| store_err(format!("write: {e}")))?;
            f.write_all(&blob).map_err(|e| store_err(format!("write: {e}")))?;
            f.sync_all().map_err(|e| store_err(format!("sync: {e}")))?;
        }
        // Atomic replace — but on Windows a freshly-written temp or the existing
        // target can be briefly locked by antivirus / the search indexer, which
        // makes a single rename fail and (previously) silently lose the saved
        // login. Retry a few times, then fall back to an in-place overwrite so the
        // data still persists (GCM still detects a torn file on read). On Unix the
        // first rename succeeds, so this is a no-op there.
        let mut renamed = false;
        let mut last_err: Option<std::io::Error> = None;
        for attempt in 0..5 {
            match std::fs::rename(&tmp, &self.path) {
                Ok(()) => {
                    renamed = true;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(60 * (attempt + 1)));
                }
            }
        }
        if !renamed {
            let detail = last_err.map(|e| e.to_string()).unwrap_or_default();
            std::fs::write(&self.path, &blob)
                .map_err(|e| store_err(format!("rename failed ({detail}) and direct write failed: {e}")))?;
            let _ = std::fs::remove_file(&tmp);
        }
        // Best-effort: fsync the DIRECTORY too, so the rename itself is durable.
        // Not supported everywhere (and meaningless on Windows), hence best-effort.
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("dn-store-test-{}-{}.bin", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn round_trip_persists_across_reopen() {
        let path = tmp_path("rt");
        {
            let mut s = EncryptedStore::open(&path, "launch-pw").unwrap();
            s.set("username", "portkey-bob");
            s.set("remember", "true");
            s.save().unwrap();
        }
        let s = EncryptedStore::open(&path, "launch-pw").unwrap();
        assert_eq!(s.get("username"), Some("portkey-bob"));
        assert_eq!(s.get("remember"), Some("true"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_password_is_rejected_not_garbage() {
        let path = tmp_path("wp");
        {
            let mut s = EncryptedStore::open(&path, "correct").unwrap();
            s.set("secret", "value");
            s.save().unwrap();
        }
        // Not unwrap_err() — that needs Debug on the store, which we deliberately
        // don't derive (it holds the key).
        match EncryptedStore::open(&path, "WRONG") {
            Ok(_) => panic!("a wrong password must be rejected"),
            Err(e) => assert!(format!("{e}").contains("wrong password")),
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn foreign_store_file_starts_fresh_not_stuck() {
        // A pre-existing config.enc from the old Python client is a Fernet token
        // ("gAAA…"), not a DNV4 store. Opening it must NOT fail forever (which
        // blocked all saves across a Python→v4 upgrade) — it should start fresh
        // and overwrite on save.
        let path = tmp_path("foreign");
        std::fs::write(&path, b"gAAAAABmustnotmatterjustnotDNV4magic").unwrap();
        let mut s = EncryptedStore::open(&path, "launch-pw")
            .expect("a foreign/old-format file must open as a fresh store");
        assert_eq!(s.get("username"), None, "fresh store starts empty");
        s.set("username", "portkey-bob");
        s.save().unwrap();
        // Reopen: it's now a real DNV4 store that round-trips.
        let s2 = EncryptedStore::open(&path, "launch-pw").unwrap();
        assert_eq!(s2.get("username"), Some("portkey-bob"));
        assert_eq!(&std::fs::read(&path).unwrap()[..4], MAGIC);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_torn_write_starts_fresh_instead_of_locking_the_user_out() {
        // Power cut mid-save: the magic is there but the file is truncated. It
        // can't hold real data, so it must not become a permanent lockout.
        let path = tmp_path("torn");
        std::fs::write(&path, b"DNV4short").unwrap();
        let mut s = EncryptedStore::open(&path, "pw").expect("a torn store opens fresh");
        assert_eq!(s.get("username"), None);
        s.set("username", "portkey-bob");
        s.save().unwrap();
        assert_eq!(
            EncryptedStore::open(&path, "pw").unwrap().get("username"),
            Some("portkey-bob")
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tamper_is_detected() {
        let path = tmp_path("tamper");
        {
            let mut s = EncryptedStore::open(&path, "pw").unwrap();
            s.set("k", "v");
            s.save().unwrap();
        }
        // Flip a ciphertext byte → GCM tag must fail.
        let mut blob = std::fs::read(&path).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xFF;
        std::fs::write(&path, &blob).unwrap();
        assert!(EncryptedStore::open(&path, "pw").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn no_plaintext_on_disk() {
        let path = tmp_path("plain");
        let mut s = EncryptedStore::open(&path, "pw").unwrap();
        s.set("username", "SECRET-MARKER-123");
        s.save().unwrap();
        let blob = std::fs::read(&path).unwrap();
        assert!(
            !blob.windows(17).any(|w| w == b"SECRET-MARKER-123"),
            "plaintext must not appear on disk"
        );
        std::fs::remove_file(&path).ok();
    }
}
