//! ISO-BMFF `pssh` (Protection System Specific Header) parsing.

use crate::models::{PsshEntry, PsshProbeInfo};

pub const WIDEVINE_SYSTEM_ID: &str = "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
pub const PLAYREADY_SYSTEM_ID: &str = "9a04f079-9840-4286-abab-2c844c5f2e65";
pub const FAIRPLAY_SYSTEM_ID: &str = "94ce86fb-07ff-4f43-adb4-93fb26514845";
pub const CLEARKEY_SYSTEM_ID: &str = "1077efec-c0b2-4d02-ace3-3c48c139a369";

impl PsshProbeInfo {
    pub fn merge(&mut self, other: PsshProbeInfo) {
        for e in other.entries {
            if !self
                .entries
                .iter()
                .any(|x| x.system_id == e.system_id && x.key_ids == e.key_ids)
            {
                self.entries.push(e);
            }
        }
    }
}

pub fn classify_system_id(uuid: &str) -> &'static str {
    match uuid.to_ascii_lowercase().as_str() {
        WIDEVINE_SYSTEM_ID => "Widevine",
        PLAYREADY_SYSTEM_ID => "PlayReady",
        FAIRPLAY_SYSTEM_ID => "FairPlay",
        CLEARKEY_SYSTEM_ID => "ClearKey",
        _ => "Unknown",
    }
}

/// Parse raw `pssh` box payload (bytes after the 8-byte box header).
pub fn parse_pssh_payload(payload: &[u8]) -> Option<PsshEntry> {
    if payload.len() < 4 {
        return None;
    }
    let version = payload[0];
    let mut issues = Vec::new();

    let (system_id, rest) = if payload.len() >= 20 {
        let sid = format_system_id(&payload[4..20]);
        (sid, &payload[20..])
    } else {
        issues.push("truncated system id".into());
        return Some(PsshEntry {
            system_id: String::new(),
            drm_system: "Unknown".into(),
            version,
            key_ids: Vec::new(),
            data_len: 0,
            valid: false,
            encryption_scheme: None,
            issues,
        });
    };

    let drm_system = classify_system_id(&system_id).to_string();
    let mut key_ids = Vec::new();
    let mut data_slice = rest;

    if version == 1 {
        if rest.len() < 4 {
            issues.push("missing KID count".into());
        } else {
            let kid_count = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let need = 4 + kid_count.saturating_mul(16);
            if rest.len() < need {
                issues.push("truncated KID list".into());
            } else {
                for i in 0..kid_count {
                    let start = 4 + i * 16;
                    let end = start + 16;
                    if end <= rest.len() {
                        key_ids.push(format_kid(&rest[start..end]));
                    }
                }
                data_slice = &rest[need..];
            }
        }
    }

    let (data_len, valid) = if data_slice.len() >= 4 {
        let dl = u32::from_be_bytes([data_slice[0], data_slice[1], data_slice[2], data_slice[3]]);
        let have = data_slice.len().saturating_sub(4);
        if dl as usize > have {
            issues.push("truncated PSSH data".into());
            (dl, false)
        } else {
            (dl, issues.is_empty())
        }
    } else {
        issues.push("missing data length".into());
        (0, false)
    };

    Some(PsshEntry {
        system_id: system_id.clone(),
        drm_system,
        version,
        key_ids,
        data_len,
        valid,
        encryption_scheme: infer_encryption_scheme(payload),
        issues,
    })
}

/// Scan fMP4 bytes for `pssh` boxes.
pub fn scan_pssh_boxes(bytes: &[u8]) -> PsshProbeInfo {
    let mut info = PsshProbeInfo::default();
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        if name == b"pssh" {
            if let Some(entry) = parse_pssh_payload(payload) {
                info.entries.push(entry);
            }
        }
    });
    info
}

/// Decode base64 PSSH from DASH `ContentProtection` or HLS `EXT-X-KEY`.
pub fn parse_pssh_base64(b64: &str) -> Option<PsshEntry> {
    use base64::Engine;
    let trimmed = b64.trim();
    if trimmed.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .ok()?;
    if bytes.len() < 12 {
        return None;
    }
    if &bytes[4..8] != b"pssh" {
        return None;
    }
    parse_pssh_payload(&bytes[8..])
}

fn format_system_id(raw: &[u8]) -> String {
    if raw.len() != 16 {
        return String::new();
    }
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[0],
        raw[1],
        raw[2],
        raw[3],
        raw[4],
        raw[5],
        raw[6],
        raw[7],
        raw[8],
        raw[9],
        raw[10],
        raw[11],
        raw[12],
        raw[13],
        raw[14],
        raw[15]
    )
}

fn format_kid(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

fn infer_encryption_scheme(payload: &[u8]) -> Option<String> {
    if payload.len() >= 28 && payload[0] == 0 {
        Some("cenc".into())
    } else {
        None
    }
}

fn walk_boxes<F>(data: &[u8], start: usize, end: usize, f: &mut F)
where
    F: FnMut(&[u8; 4], &[u8]),
{
    let mut pos = start;
    while pos + 8 <= end {
        let size = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        if size < 8 {
            break;
        }
        let box_end = pos.saturating_add(size as usize);
        if box_end > end || box_end <= pos {
            break;
        }
        let mut name = [0u8; 4];
        name.copy_from_slice(&data[pos + 4..pos + 8]);
        let header = if size == 1 && pos + 16 <= box_end {
            u64::from_be_bytes([
                data[pos + 8],
                data[pos + 9],
                data[pos + 10],
                data[pos + 11],
                data[pos + 12],
                data[pos + 13],
                data[pos + 14],
                data[pos + 15],
            ]) as usize
        } else {
            size as usize
        };
        let payload_start = if size == 1 { pos + 16 } else { pos + 8 };
        let payload_end = pos.saturating_add(header).min(box_end);
        if payload_start <= payload_end {
            let payload = &data[payload_start..payload_end];
            f(&name, payload);
            if &name == b"moov" || &name == b"moof" || &name == b"traf" || &name == b"sinf" {
                walk_boxes(data, payload_start, payload_end, f);
            }
        }
        pos = box_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_pssh_v1(system: &[u8; 16], kids: &[&[u8; 16]]) -> Vec<u8> {
        let mut payload = vec![1u8, 0, 0, 0];
        payload.extend_from_slice(system);
        payload.extend_from_slice(&(kids.len() as u32).to_be_bytes());
        for k in kids {
            payload.extend_from_slice(*k);
        }
        payload.extend_from_slice(&0u32.to_be_bytes());
        let size = (8 + payload.len()) as u32;
        let mut box_bytes = size.to_be_bytes().to_vec();
        box_bytes.extend_from_slice(b"pssh");
        box_bytes.extend(payload);
        box_bytes
    }

    #[test]
    fn parses_widevine_pssh() {
        let sys = [
            0xed, 0xef, 0x8b, 0xa9, 0x79, 0xd6, 0x4a, 0xce, 0xa3, 0xc8, 0x27, 0xdc, 0xd5, 0x1d,
            0x21, 0xed,
        ];
        let kid = [0x01; 16];
        let bytes = build_pssh_v1(&sys, &[&kid]);
        let info = scan_pssh_boxes(&bytes);
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].drm_system, "Widevine");
        assert_eq!(info.entries[0].key_ids.len(), 1);
    }
}
