//! Encryption of stored passwords (ChaCha20-Poly1305).
//!
//! Without a master password a random key lives in `vault.key` (mode 0600);
//! with a master password the key is derived with Argon2id and never stored.

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
use rand::RngCore;
use std::path::Path;

// Kept from the first versions (named "RDP Manager") so existing vaults still unlock.
const VERIFIER: &[u8] = b"rdp-manager-vault-ok";

#[derive(Clone)]
pub struct Vault {
    cipher: ChaCha20Poly1305,
}

impl Vault {
    pub fn from_key(key: &[u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new(key.into()) }
    }

    pub fn from_password(password: &str, salt: &[u8]) -> Result<Self> {
        let mut key = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow!("Key derivation failed: {e}"))?;
        Ok(Self::from_key(&key))
    }

    /// Loads the local key or creates a new one.
    pub fn from_key_file(path: &Path) -> Result<Self> {
        if let Ok(bytes) = std::fs::read(path) {
            let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow!("{} is corrupt", path.display()))?;
            return Ok(Self::from_key(&key));
        }
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        write_private(path, &key)?;
        Ok(Self::from_key(&key))
    }

    pub fn encrypt(&self, plain: &str) -> Vec<u8> {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut out = nonce.to_vec();
        out.extend(self.cipher.encrypt(Nonce::from_slice(&nonce), plain.as_bytes()).expect("encryption cannot fail"));
        out
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<String> {
        if data.len() < 12 {
            return Err(anyhow!("Ciphertext too short"));
        }
        let plain = self
            .cipher
            .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
            .map_err(|_| anyhow!("Decryption failed"))?;
        String::from_utf8(plain).context("Not UTF-8")
    }

    pub fn verifier(&self) -> Vec<u8> {
        self.encrypt(std::str::from_utf8(VERIFIER).unwrap())
    }

    pub fn check_verifier(&self, data: &[u8]) -> bool {
        self.decrypt(data).map(|s| s.as_bytes() == VERIFIER).unwrap_or(false)
    }
}

pub fn random_salt() -> Vec<u8> {
    let mut salt = vec![0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

/// Writes a file only the current user may read (key file, overwritten if present).
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    write_private_impl(path, data, false)
}

/// Creates a new secret file; fails if the path already exists (also refuses to follow a
/// symlink planted at that path).
pub fn create_private(path: &Path, data: &[u8]) -> Result<()> {
    write_private_impl(path, data, true)
}

fn write_private_impl(path: &Path, data: &[u8], new: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).mode(0o600);
        if new {
            opts.create_new(true);
        } else {
            opts.create(true).truncate(true);
        }
        let mut f = opts.open(path)?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if new && path.exists() {
            anyhow::bail!("{} already exists", path.display());
        }
        std::fs::write(path, data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_key() {
        let salt = random_salt();
        let v = Vault::from_password("secret", &salt).unwrap();
        let c = v.encrypt("Passwört");
        assert_eq!(v.decrypt(&c).unwrap(), "Passwört");
        assert!(v.check_verifier(&v.verifier()));
        let other = Vault::from_password("wrong", &salt).unwrap();
        assert!(other.decrypt(&c).is_err());
        assert!(!other.check_verifier(&v.verifier()));
    }
}
