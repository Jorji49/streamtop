//! In-memory AES-128-CBC probe decryption for HLS `#EXT-X-KEY` segments.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use aes::Aes128;
use cbc::Decryptor;
use color_eyre::eyre::{eyre, Result, WrapErr};
use reqwest::Client;

type Aes128CbcDec = Decryptor<Aes128>;

const KEY_BYTES: usize = 16;
const IV_BYTES: usize = 16;
const KEY_CACHE_TTL: Duration = Duration::from_secs(300);
const KEY_CACHE_MAX: usize = 32;

#[derive(Debug, Clone)]
pub struct HlsAes128KeyInfo {
    pub uri: String,
    pub iv: Option<[u8; IV_BYTES]>,
}

#[derive(Debug, Clone)]
struct CachedKey {
    key: [u8; KEY_BYTES],
    fetched_at: Instant,
}

#[derive(Debug, Default)]
pub struct Aes128KeyCache {
    entries: HashMap<String, CachedKey>,
}

impl Aes128KeyCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, uri: &str) -> Option<[u8; KEY_BYTES]> {
        self.prune_expired();
        self.entries.get(uri).map(|e| e.key)
    }

    fn insert(&mut self, uri: String, key: [u8; KEY_BYTES]) {
        self.prune_expired();
        if self.entries.len() >= KEY_CACHE_MAX {
            if let Some(old) = self.entries.keys().next().cloned() {
                self.entries.remove(&old);
            }
        }
        self.entries.insert(
            uri,
            CachedKey {
                key,
                fetched_at: Instant::now(),
            },
        );
    }

    fn prune_expired(&mut self) {
        self.entries
            .retain(|_, v| v.fetched_at.elapsed() < KEY_CACHE_TTL);
    }
}

/// Parse `#EXT-X-KEY:METHOD=AES-128,...` from a manifest line.
pub fn parse_aes128_key_line(line: &str) -> Option<HlsAes128KeyInfo> {
    let t = line.trim();
    if !t.starts_with("#EXT-X-KEY") {
        return None;
    }
    let method = attr_value(t, "METHOD")?;
    if !method.eq_ignore_ascii_case("AES-128") {
        return None;
    }
    let uri = attr_value(t, "URI")?;
    let iv = attr_value(t, "IV").and_then(|s| parse_iv_hex(&s).ok());
    Some(HlsAes128KeyInfo { uri, iv })
}

fn attr_value(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let rest = line.split(&needle).nth(1)?;
    let raw = rest.split(',').next()?.trim();
    Some(raw.trim_matches('"').to_string())
}

/// IV from `#EXT-X-KEY` hex or big-endian `media_sequence`.
pub fn derive_iv(media_sequence: u64, explicit: Option<[u8; IV_BYTES]>) -> [u8; IV_BYTES] {
    if let Some(iv) = explicit {
        return iv;
    }
    let mut iv = [0u8; IV_BYTES];
    iv[8..].copy_from_slice(&media_sequence.to_be_bytes());
    iv
}

pub fn parse_iv_hex(s: &str) -> Result<[u8; IV_BYTES]> {
    let hex = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != IV_BYTES * 2 {
        return Err(eyre!("IV hex must be {} chars", IV_BYTES * 2));
    }
    let mut out = [0u8; IV_BYTES];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).map_err(|_| eyre!("invalid IV hex"))?;
        out[i] = u8::from_str_radix(pair, 16).map_err(|_| eyre!("invalid IV hex digit"))?;
    }
    Ok(out)
}

pub async fn fetch_aes128_key(
    client: &Client,
    uri: &str,
    cache: &Mutex<Aes128KeyCache>,
) -> Result<[u8; KEY_BYTES]> {
    if let Ok(mut guard) = cache.lock() {
        if let Some(key) = guard.get(uri) {
            return Ok(key);
        }
    }
    let body = client
        .get(uri)
        .send()
        .await
        .wrap_err_with(|| format!("AES-128 key GET failed: {uri}"))?
        .bytes()
        .await
        .wrap_err("AES-128 key body read failed")?;
    if body.len() != KEY_BYTES {
        return Err(eyre!(
            "AES-128 key length {} != {KEY_BYTES} at {uri}",
            body.len()
        ));
    }
    let mut key = [0u8; KEY_BYTES];
    key.copy_from_slice(&body);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(uri.to_string(), key);
    }
    Ok(key)
}

/// Decrypt probe ciphertext in-memory (PKCS7 padding).
pub fn decrypt_aes128_cbc_probe(
    key: &[u8; KEY_BYTES],
    iv: &[u8; IV_BYTES],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(eyre!(
            "AES-128 ciphertext length must be a positive multiple of 16"
        ));
    }
    let mut buf = ciphertext.to_vec();
    let dec = Aes128CbcDec::new(key.into(), iv.into());
    let plain = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| eyre!("AES-128-CBC PKCS7 unpad failed"))?;
    Ok(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_iv_from_media_sequence() {
        let iv = derive_iv(42, None);
        assert_eq!(&iv[0..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&iv[8..], &42u64.to_be_bytes());
    }

    #[test]
    fn parse_aes128_key_line_extracts_uri_and_iv() {
        let info = parse_aes128_key_line(
            r#"#EXT-X-KEY:METHOD=AES-128,URI="https://keys.example/a.key",IV=0x0123456789abcdef0123456789abcdef"#,
        )
        .unwrap();
        assert_eq!(info.uri, "https://keys.example/a.key");
        assert!(info.iv.is_some());
    }

    #[test]
    fn decrypt_synthetic_ts_payload() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        use cbc::Encryptor;
        type Aes128CbcEnc = Encryptor<Aes128>;

        let key = [0x2bu8; 16];
        let iv = [0u8; 16];
        let plain = vec![0x47u8; 32];
        let enc = Aes128CbcEnc::new((&key).into(), (&iv).into());
        let mut ct = vec![0u8; 48];
        ct[..32].copy_from_slice(&plain);
        let ct_len = enc.encrypt_padded_mut::<Pkcs7>(&mut ct, 32).unwrap().len();
        let out = decrypt_aes128_cbc_probe(&key, &iv, &ct[..ct_len]).unwrap();
        assert_eq!(&out[..32], &plain[..32]);
        assert_eq!(out[0], 0x47);
    }
}
