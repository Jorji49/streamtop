//! Staging ClearKey probe: KID match and cenc/cbcs sample validation.

use std::fmt::Write as _;

use aes::Aes128;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
use color_eyre::eyre::{eyre, Result};
use ctr::cipher::StreamCipher;
use ctr::Ctr128BE;
use serde_json::{json, Value};

use crate::engine::pssh::{scan_pssh_boxes, CLEARKEY_SYSTEM_ID};
use crate::models::WireProbeInfo;

type Aes128Ctr = Ctr128BE<Aes128>;
type Aes128CbcDec = Decryptor<Aes128>;

/// Parsed `--clearkey KID_HEX:KEY_HEX` (32 hex chars each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearKeySpec {
    pub kid: [u8; 16],
    pub key: [u8; 16],
}

impl ClearKeySpec {
    pub fn parse(raw: &str) -> Result<Self> {
        let (kid_s, key_s) = raw
            .split_once(':')
            .ok_or_else(|| eyre!("clearkey format: KID_HEX:KEY_HEX"))?;
        let kid = hex16(kid_s.trim())?;
        let key = hex16(key_s.trim())?;
        Ok(Self { kid, key })
    }
}

fn hex16(s: &str) -> Result<[u8; 16]> {
    let s = s.trim_start_matches("0x");
    if s.len() != 32 {
        return Err(eyre!("expected 32 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 16];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).map_err(|_| eyre!("invalid hex"))?;
        out[i] = u8::from_str_radix(pair, 16).map_err(|_| eyre!("invalid hex digit"))?;
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClearKeyProbeResult {
    pub kid_matched: bool,
    pub clearkey_system_found: bool,
    pub cenc_boxes_seen: bool,
    pub decrypt_ok: bool,
    pub encryption_scheme: Option<String>,
    pub message: String,
}

/// JSON body for ClearKey license POST (`type=temporary`).
pub fn clearkey_license_body(spec: &ClearKeySpec) -> Value {
    json!({
        "kids": [base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            spec.kid
        )],
        "type": "temporary"
    })
}

/// Validate ClearKey against wire PSSH and cenc subsample decrypt in probe window.
pub fn probe_clearkey(bytes: &[u8], spec: &ClearKeySpec) -> ClearKeyProbeResult {
    let pssh = scan_pssh_boxes(bytes);
    let kid_hex = hex_encode(&spec.kid);

    let clearkey_system_found = pssh.entries.iter().any(|e| {
        e.drm_system.eq_ignore_ascii_case("ClearKey")
            || e.system_id
                .replace('-', "")
                .eq_ignore_ascii_case(&CLEARKEY_SYSTEM_ID.replace('-', ""))
    });

    let kid_matched = pssh.entries.iter().any(|e| {
        e.key_ids
            .iter()
            .any(|k| k.replace('-', "").eq_ignore_ascii_case(&kid_hex))
    }) || bytes_windows_match_kid(bytes, spec.kid);

    let scheme = read_schm_scheme(bytes);
    let cenc_boxes_seen = contains_box(bytes, b"tenc")
        || contains_box(bytes, b"senc")
        || contains_box(bytes, b"schm");

    let decrypt_ok = match scheme.as_deref() {
        Some("cbcs") => try_cbcs_decrypt(bytes, &spec.key),
        _ => try_cenc_decrypt(bytes, &spec.key),
    };

    let message = if scheme.as_deref() == Some("cbcs") {
        if decrypt_ok {
            "ClearKey KID matched; cbcs 1:9 pattern decrypt OK (staging)".into()
        } else if !kid_matched {
            "KID not found in PSSH or tenc default_KID".into()
        } else {
            "ClearKey matched; cbcs pattern decrypt not verified".into()
        }
    } else if !clearkey_system_found && !cenc_boxes_seen {
        "No ClearKey PSSH or cenc boxes in probe window".into()
    } else if !kid_matched {
        "KID not found in PSSH or tenc default_KID".into()
    } else if decrypt_ok {
        "ClearKey KID matched; cenc subsample decrypt OK (staging)".into()
    } else if cenc_boxes_seen {
        "ClearKey matched; cenc subsample decrypt not verified".into()
    } else {
        "ClearKey matched but cenc layout missing".into()
    };

    ClearKeyProbeResult {
        kid_matched,
        clearkey_system_found,
        cenc_boxes_seen,
        decrypt_ok,
        encryption_scheme: scheme,
        message,
    }
}

pub fn apply_clearkey_to_wire(wire: &mut WireProbeInfo, result: &ClearKeyProbeResult) {
    if result.kid_matched {
        wire.pssh.entries.push(crate::models::PsshEntry {
            system_id: CLEARKEY_SYSTEM_ID.into(),
            drm_system: "ClearKey(staging)".into(),
            version: 0,
            key_ids: vec![],
            data_len: 0,
            valid: result.decrypt_ok,
            encryption_scheme: result
                .encryption_scheme
                .clone()
                .or_else(|| Some("cenc".into())),
            issues: vec![],
        });
    }
}

fn read_schm_scheme(bytes: &[u8]) -> Option<String> {
    let payload = find_box_payload_recursive(bytes, b"schm")?;
    let scheme = crate::engine::slice_util::subslice_len(payload, 8, 4)?;
    Some(
        String::from_utf8_lossy(scheme)
            .trim_end_matches('\0')
            .into(),
    )
}

fn try_cbcs_decrypt(bytes: &[u8], key: &[u8; 16]) -> bool {
    let Some((iv, clear_bytes)) = senc_first_iv_and_clear(bytes) else {
        return false;
    };
    let Some(mdat) = find_box_payload_recursive(bytes, b"mdat") else {
        return false;
    };
    let start = clear_bytes as usize;
    let Some(block_slice) = crate::engine::slice_util::subslice_len(mdat, start, 16) else {
        return false;
    };
    let mut block = [0u8; 16];
    block.copy_from_slice(block_slice);
    let Ok(decryptor) = Aes128CbcDec::new_from_slices(key, &iv[..16]) else {
        return false;
    };
    let Ok(plain) = decryptor.decrypt_padded_mut::<NoPadding>(&mut block) else {
        return false;
    };
    looks_like_decrypted_sample(plain)
}

fn senc_first_iv_and_clear(bytes: &[u8]) -> Option<([u8; 16], u32)> {
    let senc = find_box_payload_recursive(bytes, b"senc")?;
    if senc.len() < 16 {
        return None;
    }
    let flags = u32::from_be_bytes([0, senc[1], senc[2], senc[3]]);
    let iv_size = if flags & 0x2 != 0 { 8usize } else { 16usize };
    let mut off = 12usize;
    if off + 4 + iv_size > senc.len() {
        return None;
    }
    off += 4;
    let mut iv = [0u8; 16];
    iv[..iv_size].copy_from_slice(&senc[off..off + iv_size]);
    off += iv_size;
    let clear = if flags & 0x2 != 0 && off + 2 <= senc.len() {
        let subs = u16::from_be_bytes(senc[off..off + 2].try_into().ok()?);
        off += 2;
        if subs == 0 || off + 2 > senc.len() {
            0
        } else {
            u16::from_be_bytes(senc[off..off + 2].try_into().ok()?) as u32
        }
    } else {
        0
    };
    Some((iv, clear))
}

fn try_cenc_decrypt(bytes: &[u8], key: &[u8; 16]) -> bool {
    let Some(senc) = find_box_payload_recursive(bytes, b"senc") else {
        return false;
    };
    if senc.len() < 16 {
        return false;
    }
    let version = senc[0];
    let flags = u32::from_be_bytes([0, senc[1], senc[2], senc[3]]);
    let iv_size = if flags & 0x2 != 0 { 8usize } else { 16usize };
    let mut off = 12usize;
    if off + 4 > senc.len() {
        return false;
    }
    let sample_count = match senc[off..off + 4].try_into().ok() {
        Some(b) => u32::from_be_bytes(b) as usize,
        None => return false,
    };
    off += 4;
    if sample_count == 0 {
        return false;
    }
    if off + iv_size > senc.len() {
        return false;
    }
    let mut iv = [0u8; 16];
    iv[..iv_size].copy_from_slice(&senc[off..off + iv_size]);
    off += iv_size;

    let (clear_bytes, enc_len) = if flags & 0x2 != 0 {
        if off + 2 > senc.len() {
            return false;
        }
        let subs = match senc[off..off + 2].try_into().ok() {
            Some(b) => u16::from_be_bytes(b) as usize,
            None => return false,
        };
        off += 2;
        if subs == 0 || off + 6 > senc.len() {
            return false;
        }
        let clear = match senc[off..off + 2].try_into().ok() {
            Some(b) => u16::from_be_bytes(b) as u32,
            None => return false,
        };
        let enc = match senc[off + 2..off + 6].try_into().ok() {
            Some(b) => u32::from_be_bytes(b),
            None => return false,
        };
        (clear, enc)
    } else {
        (0, 16)
    };

    let Some(mdat) = find_box_payload_recursive(bytes, b"mdat") else {
        return false;
    };
    let start = clear_bytes as usize;
    let take = enc_len.min(32) as usize;
    if start + take > mdat.len() {
        return false;
    }
    let cipher_in = &mdat[start..start + take];
    let mut out = cipher_in.to_vec();
    if version != 0 {
        // version 1 IV semantics differ; staging skips
        return false;
    }
    let mut cipher = Aes128Ctr::new(key.into(), &iv.into());
    cipher.apply_keystream(&mut out);
    looks_like_decrypted_sample(&out)
}

fn looks_like_decrypted_sample(dec: &[u8]) -> bool {
    if dec.is_empty() {
        return false;
    }
    if dec.windows(4).any(|w| w == [0, 0, 0, 1]) || dec.windows(3).any(|w| w == [0, 0, 1]) {
        return true;
    }
    // fMP4 length-prefixed NAL
    if dec.len() >= 5 {
        let n = u32::from_be_bytes(dec[0..4].try_into().unwrap_or([0; 4])) as usize;
        if (1..=dec.len()).contains(&n) {
            let nal = dec[4];
            let typ = nal & 0x1f;
            if (1..=23).contains(&typ) || typ == 0 {
                return true;
            }
        }
    }
    false
}

#[allow(clippy::trivially_copy_pass_by_ref)] // ISO BMFF box walk
fn find_box_payload_recursive<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let size = read_box_size(data, i)?;
        if size < 8 {
            break;
        }
        let end = i.saturating_add(size).min(data.len());
        if &data[i + 4..i + 8] == tag {
            return Some(&data[i + 8..end]);
        }
        let inner = &data[i + 8..end];
        if is_container_box(&data[i + 4..i + 8]) {
            if let Some(found) = find_box_payload_recursive(inner, tag) {
                return Some(found);
            }
        }
        i = end;
    }
    None
}

fn read_box_size(data: &[u8], off: usize) -> Option<usize> {
    if off + 8 > data.len() {
        return None;
    }
    let size32 = u32::from_be_bytes(data[off..off + 4].try_into().ok()?);
    if size32 == 1 {
        if off + 16 > data.len() {
            return None;
        }
        Some(u64::from_be_bytes(data[off + 8..off + 16].try_into().ok()?) as usize)
    } else if size32 == 0 {
        Some(data.len().saturating_sub(off))
    } else {
        Some(size32 as usize)
    }
}

fn is_container_box(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"moov" | b"moof" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"traf" | b"mfra" | b"edts"
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[allow(clippy::trivially_copy_pass_by_ref)] // fixed 16-byte KID window scan
fn bytes_windows_match_kid(bytes: &[u8], kid: [u8; 16]) -> bool {
    bytes.windows(16).any(|w| w == kid)
}

#[allow(clippy::trivially_copy_pass_by_ref)] // ISO BMFF box tag scan
fn contains_box(bytes: &[u8], tag: &[u8; 4]) -> bool {
    bytes.windows(4).any(|w| w == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clearkey_spec() {
        let kid = "0123456789abcdef0123456789abcdef";
        let key = "fedcba9876543210fedcba9876543210";
        let spec = ClearKeySpec::parse(&format!("{kid}:{key}")).unwrap();
        assert_eq!(spec.kid[0], 0x01);
        assert_eq!(spec.key[0], 0xfe);
    }

    #[test]
    fn rejects_bad_length() {
        assert!(ClearKeySpec::parse("abc:def").is_err());
    }

    #[test]
    fn clearkey_license_json_has_kid() {
        let spec = ClearKeySpec::parse(
            "0123456789abcdef0123456789abcdef:fedcba9876543210fedcba9876543210",
        )
        .unwrap();
        let body = clearkey_license_body(&spec);
        assert_eq!(body["type"], "temporary");
        assert!(body["kids"].is_array());
    }

    #[test]
    fn cbcs_scheme_selects_pattern_path() {
        let kid = "0123456789abcdef0123456789abcdef";
        let key = "fedcba9876543210fedcba9876543210";
        let spec = ClearKeySpec::parse(&format!("{kid}:{key}")).unwrap();
        let mut bytes = Vec::new();
        // schm box (size 12 + payload)
        let schm_payload = [0, 0, 0, 0, 0, 0, 0, 0, b'c', b'b', b'c', b's'];
        let schm_len = (8 + schm_payload.len()) as u32;
        bytes.extend_from_slice(&schm_len.to_be_bytes());
        bytes.extend_from_slice(b"schm");
        bytes.extend_from_slice(&schm_payload);
        let result = probe_clearkey(&bytes, &spec);
        assert_eq!(result.encryption_scheme.as_deref(), Some("cbcs"));
    }

    #[test]
    fn decrypted_nal_detected() {
        assert!(looks_like_decrypted_sample(&[0, 0, 0, 1, 0x65, 0x88]));
    }
}
