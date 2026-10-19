//! Re-implementation of the password encryption of the Windows RDP-Manager.
//!
//! The original uses `System.Security.Cryptography.PasswordDeriveBytes`
//! (Microsoft's PBKDF1 extension with SHA-1, 100 iterations) and AES-256-CBC with
//! PKCS#7 padding over UTF-16LE plaintext. The key material is a fixed constant,
//! the MD5 hash of the app password, the app password itself and optionally the
//! content of the file `DisableShowPw`.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use md5::{Digest as _, Md5};
use sha1::Sha1;

/// Constant from `Cinspiration.RdpManager.Resources.Consts.EncryptionPassword`.
pub const BASE_PASSWORD: &str = "243ki\u{00df}20r2k39034k93emsdkm";

/// Salt from `SecurityCrypt.EncryptAesString` ("Ivan Medvedev").
const SALT: &[u8] = b"Ivan Medvedev";

/// Assembles the effective key password the way the original uses it after unlocking.
pub fn effective_password(app_password: Option<&str>, disable_show_pw: Option<&str>) -> String {
    let mut pw = BASE_PASSWORD.to_string();
    if let Some(app) = app_password.filter(|p| !p.is_empty()) {
        pw.push_str(&md5_upper_hex(app));
        pw.push_str(app);
    }
    if let Some(extra) = disable_show_pw.filter(|p| !p.is_empty()) {
        pw.push_str(extra);
    }
    pw
}

/// `SecurityHash.GetMd5`: MD5 over `Encoding.Default` (Windows-1252), upper-case hex.
pub fn md5_upper_hex(text: &str) -> String {
    let digest = Md5::digest(encode_cp1252(text));
    digest.iter().map(|b| format!("{b:02X}")).collect()
}

fn encode_cp1252(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c as u32 {
            0..=0x7F | 0xA0..=0xFF => c as u8,
            0x20AC => 0x80,
            0x201A => 0x82,
            0x0192 => 0x83,
            0x201E => 0x84,
            0x2026 => 0x85,
            0x2020 => 0x86,
            0x2021 => 0x87,
            0x02C6 => 0x88,
            0x2030 => 0x89,
            0x0160 => 0x8A,
            0x2039 => 0x8B,
            0x0152 => 0x8C,
            0x017D => 0x8E,
            0x2018 => 0x91,
            0x2019 => 0x92,
            0x201C => 0x93,
            0x201D => 0x94,
            0x2022 => 0x95,
            0x2013 => 0x96,
            0x2014 => 0x97,
            0x02DC => 0x98,
            0x2122 => 0x99,
            0x0161 => 0x9A,
            0x203A => 0x9B,
            0x0153 => 0x9C,
            0x017E => 0x9E,
            0x0178 => 0x9F,
            _ => b'?',
        })
        .collect()
}

/// Bit-exact re-implementation of .NET `PasswordDeriveBytes` (SHA-1, 100 iterations),
/// including the known bug in the second `GetBytes` call.
pub struct PasswordDeriveBytes {
    base_value: [u8; 20],
    prefix: u32,
    extra: Option<Vec<u8>>,
    extra_count: usize,
}

impl PasswordDeriveBytes {
    pub fn new(password: &str, salt: &[u8]) -> Self {
        const ITERATIONS: usize = 100;
        let mut hasher = Sha1::new();
        hasher.update(password.as_bytes());
        hasher.update(salt);
        let mut base: [u8; 20] = hasher.finalize().into();
        for _ in 1..(ITERATIONS - 1) {
            base = Sha1::digest(base).into();
        }
        Self { base_value: base, prefix: 0, extra: None, extra_count: 0 }
    }

    fn hash_prefix(&mut self, hasher: &mut Sha1) {
        if self.prefix > 0 {
            hasher.update(self.prefix.to_string().as_bytes());
        }
        self.prefix += 1;
    }

    fn compute_bytes(&mut self, cb: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(cb.div_ceil(20) * 20);
        while out.len() < cb {
            let mut hasher = Sha1::new();
            self.hash_prefix(&mut hasher);
            hasher.update(self.base_value);
            out.extend_from_slice(&hasher.finalize());
        }
        out
    }

    pub fn get_bytes(&mut self, cb: usize) -> Vec<u8> {
        let mut ib = 0;
        let mut out = vec![0u8; cb];
        if let Some(extra) = self.extra.take() {
            ib = extra.len() - self.extra_count;
            if ib >= cb {
                out.copy_from_slice(&extra[self.extra_count..self.extra_count + cb]);
                if ib > cb {
                    self.extra_count += cb;
                    self.extra = Some(extra);
                }
                return out;
            }
            // .NET reads from offset `ib` instead of `extra_count` here – kept on
            // purpose so the keys match the original.
            out[..ib].copy_from_slice(&extra[ib..ib + ib]);
        }
        let rgb = self.compute_bytes(cb - ib);
        out[ib..].copy_from_slice(&rgb[..cb - ib]);
        if rgb.len() + ib > cb {
            self.extra_count = cb - ib;
            self.extra = Some(rgb);
        }
        out
    }
}

fn key_iv(password: &str) -> (Vec<u8>, Vec<u8>) {
    let mut pdb = PasswordDeriveBytes::new(password, SALT);
    let key = pdb.get_bytes(32);
    let iv = pdb.get_bytes(16);
    (key, iv)
}

/// `SecurityCrypt.DecryptAesString`
pub fn decrypt_aes_string(cipher_b64: &str, password: &str) -> Result<String> {
    let data = B64.decode(cipher_b64.trim()).context("Invalid Base64")?;
    let (key, iv) = key_iv(password);
    let plain = cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, &iv)
        .map_err(|e| anyhow!("{e}"))?
        .decrypt_padded_vec_mut::<Pkcs7>(&data)
        .map_err(|_| anyhow!("Decryption failed (wrong password?)"))?;
    if plain.len() % 2 != 0 {
        return Err(anyhow!("Plaintext is not UTF-16"));
    }
    let units: Vec<u16> = plain.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).context("Plaintext is not valid UTF-16")
}

/// `SecurityCrypt.EncryptAesString` – only needed for tests and export.
#[allow(dead_code)]
pub fn encrypt_aes_string(clear: &str, password: &str) -> String {
    let (key, iv) = key_iv(password);
    let plain: Vec<u8> = clear.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let data = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv)
        .expect("fixed key length")
        .encrypt_padded_vec_mut::<Pkcs7>(&plain);
    B64.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // Reference values were generated with .NET 8 (PasswordDeriveBytes, Rijndael).
    #[test]
    fn derive_bytes_matches_dotnet() {
        let mut pdb = PasswordDeriveBytes::new(BASE_PASSWORD, SALT);
        assert_eq!(hex(&pdb.get_bytes(32)), "d07fe35fdea35c44e79a5cca0616d4b43d6cc6e5fdc1313c6559c2bdce89f9d7");
        assert_eq!(hex(&pdb.get_bytes(16)), "e79a5cca0616d4b40a78d90c24d500aa");
    }

    #[test]
    fn decrypt_matches_dotnet() {
        assert_eq!(decrypt_aes_string("syQd3LRodgSDgf6YdCuI9MehPoRX4/R90Iwq6NUZF2A=", BASE_PASSWORD).unwrap(), "Geheim123!");
        assert_eq!(decrypt_aes_string("ia69C1R6SSZLEMDK+PMlmg==", BASE_PASSWORD).unwrap(), "äöü ß €");
    }

    #[test]
    fn app_password_key() {
        assert_eq!(md5_upper_hex("master"), "EB0A191797624DD3A48FA681D3061212");
        let pw = effective_password(Some("master"), None);
        assert_eq!(decrypt_aes_string("mgtxznKfL3zospjdweUyw2ipky1EhOdz+4W9OyrM2EM=", &pw).unwrap(), "pw-with-master");
        assert!(decrypt_aes_string("mgtxznKfL3zospjdweUyw2ipky1EhOdz+4W9OyrM2EM=", BASE_PASSWORD).is_err());
    }

    #[test]
    fn roundtrip() {
        let c = encrypt_aes_string("Hallo Welt", BASE_PASSWORD);
        assert_eq!(c, encrypt_aes_string("Hallo Welt", BASE_PASSWORD));
        assert_eq!(decrypt_aes_string(&c, BASE_PASSWORD).unwrap(), "Hallo Welt");
    }
}
