//! Binary SCTE-35 splice_info_section decoder (Base64 / Hex / raw bytes).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpliceCommandType {
    SpliceNull,
    SpliceSchedule,
    SpliceInsert,
    TimeSignal,
    BandwidthReservation,
    Private(u8),
    Unknown(u8),
}

impl SpliceCommandType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x00 => Self::SpliceNull,
            0x04 => Self::SpliceSchedule,
            0x05 => Self::SpliceInsert,
            0x06 => Self::TimeSignal,
            0x07 => Self::BandwidthReservation,
            0xff => Self::Private(0xff),
            other => Self::Unknown(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SpliceNull => "SpliceNull",
            Self::SpliceSchedule => "SpliceSchedule",
            Self::SpliceInsert => "SpliceInsert",
            Self::TimeSignal => "TimeSignal",
            Self::BandwidthReservation => "BandwidthReservation",
            Self::Private(_) => "Private",
            Self::Unknown(_) => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentationDescriptor {
    pub segmentation_event_id: u32,
    pub segmentation_type_id: u8,
    pub segmentation_type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segmentation_duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpliceInfoSection {
    pub table_id: u8,
    pub protocol_version: u8,
    pub splice_command_type: SpliceCommandType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_of_network_indicator: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splice_event_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_time: Option<u64>,
    pub descriptors: Vec<SegmentationDescriptor>,
}

impl SpliceInfoSection {
    /// Human-readable log line: `[SCTE-35 BINARY] TimeSignal | Provider Ad Start | Duration: 60.0s | EventID: 10482`
    pub fn summary_line(&self) -> String {
        let cmd = self.splice_command_type.as_str();
        let seg = self.descriptors.first();
        let kind = seg
            .map(|d| d.segmentation_type_name.as_str())
            .unwrap_or_else(|| {
                match self.out_of_network_indicator {
                    Some(true) => "Out of Network",
                    Some(false) => "Return to Network",
                    None => "—",
                }
            });
        let dur = seg
            .and_then(|d| d.segmentation_duration_secs)
            .map(|s| format!("Duration: {s:.1}s"))
            .unwrap_or_else(|| "Duration: —".into());
        let event = self
            .splice_event_id
            .or_else(|| seg.map(|d| d.segmentation_event_id))
            .map(|id| format!("EventID: {id}"))
            .unwrap_or_else(|| "EventID: —".into());
        format!("[SCTE-35 BINARY] {cmd} | {kind} | {dur} | {event}")
    }
}

pub fn segmentation_type_name(id: u8) -> String {
    match id {
        0x00 => "Not Indicated".into(),
        0x10 => "Program Start".into(),
        0x11 => "Program End".into(),
        0x20 => "Chapter Start".into(),
        0x21 => "Chapter End".into(),
        0x30 => "Provider Ad Start".into(),
        0x31 => "Provider Ad End".into(),
        0x32 => "Distributor Ad Start".into(),
        0x33 => "Distributor Ad End".into(),
        0x34 => "Placement Opportunity Start".into(),
        0x35 => "Placement Opportunity End".into(),
        0x36 => "Break Start".into(),
        0x37 => "Break End".into(),
        0x40 => "Program Start In Progress".into(),
        other => format!("Type 0x{other:02X}"),
    }
}

/// Decode Base64 or Hex SCTE-35 payload into raw bytes.
pub fn decode_scte35_payload(raw: &str) -> Option<Vec<u8>> {
    let s = raw.trim().trim_matches('"');
    if s.is_empty() {
        return None;
    }
    if let Ok(bytes) = B64.decode(s) {
        if !bytes.is_empty() && (bytes[0] == 0xfc || bytes[0] == 0x00) {
            return Some(bytes);
        }
        if bytes.len() >= 14 {
            return Some(bytes);
        }
    }
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 28 && hex.len().is_multiple_of(2) {
        let mut out = Vec::with_capacity(hex.len() / 2);
        let chars: Vec<char> = hex.chars().collect();
        for chunk in chars.chunks(2) {
            let byte = u8::from_str_radix(&format!("{}{}", chunk[0], chunk[1]), 16).ok()?;
            out.push(byte);
        }
        return Some(out);
    }
    None
}

/// Extract SCTE-35 payload string from a playlist tag line.
pub fn extract_payload_from_tag(line: &str) -> Option<String> {
    let t = line.trim();
    if t.starts_with("#EXT-OATCLS-SCTE35") || t.starts_with("#EXT-X-SCTE35") {
        return t.split_once(':').map(|(_, rest)| rest.trim().to_string());
    }
    if t.starts_with("#EXT-X-DATERANGE") {
        for key in ["SCTE35-CMD", "SCTE35-OUT", "SCTE35-IN", "SCTE35-REQ"] {
            if let Some(v) = attr_quoted(t, key) {
                return Some(v);
            }
        }
    }
    None
}

fn attr_quoted(line: &str, key: &str) -> Option<String> {
    let upper = line.to_ascii_uppercase();
    let key_u = key.to_ascii_uppercase();
    let needle = format!("{key_u}=");
    let idx = upper.find(&needle)?;
    let rest = &line[idx + needle.len()..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some(stripped[..end].to_string());
    }
    Some(
        rest.split(',')
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_matches('"')
            .to_string(),
    )
}

pub fn parse_scte35_bytes(data: &[u8]) -> Option<SpliceInfoSection> {
    if data.len() < 14 {
        return None;
    }
    let table_id = data[0];
    if table_id != 0xfc && table_id != 0x00 && data.len() < 18 {
        return None;
    }
    let protocol_version = data[5];
    let splice_command_length = (((data[11] as usize) & 0x0f) << 8) | data[12] as usize;
    let splice_command_type = SpliceCommandType::from_u8(data[13]);
    let mut off = 14usize;
    let cmd_end = (off + splice_command_length).min(data.len());

    let mut out_of_network = None;
    let mut splice_event_id = None;
    let mut pts_time = None;

    match splice_command_type {
        SpliceCommandType::SpliceInsert => {
            if off + 5 <= cmd_end {
                splice_event_id = Some(u32::from_be_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]));
                let cancel = data[off + 4] & 0x80 != 0;
                off += 5;
                if !cancel && off < cmd_end {
                    out_of_network = Some(data[off] & 0x80 != 0);
                    let program_splice = data[off] & 0x40 != 0;
                    let duration_flag = data[off] & 0x20 != 0;
                    let _splice_immediate = data[off] & 0x10 != 0;
                    off += 1;
                    if program_splice && off < cmd_end {
                        let time_specified = data[off] & 0x80 != 0;
                        if time_specified && off + 5 <= cmd_end {
                            pts_time = Some(
                                (((data[off] as u64) & 0x01) << 32)
                                    | ((data[off + 1] as u64) << 24)
                                    | ((data[off + 2] as u64) << 16)
                                    | ((data[off + 3] as u64) << 8)
                                    | data[off + 4] as u64,
                            );
                            off += 5;
                        } else {
                            off += 1;
                        }
                    }
                    if duration_flag && off + 5 <= cmd_end {
                        let _ = off.saturating_add(5);
                    }
                }
            }
        }
        SpliceCommandType::TimeSignal if off < cmd_end => {
            let time_specified = data[off] & 0x80 != 0;
            if time_specified && off + 5 <= cmd_end {
                pts_time = Some(
                    (((data[off] as u64) & 0x01) << 32)
                        | ((data[off + 1] as u64) << 24)
                        | ((data[off + 2] as u64) << 16)
                        | ((data[off + 3] as u64) << 8)
                        | data[off + 4] as u64,
                );
            }
        }
        _ => {}
    }

    let desc_loop_start = 14 + splice_command_length;
    let mut descriptors = Vec::new();
    if desc_loop_start + 2 <= data.len() {
        let desc_loop_len =
            ((data[desc_loop_start] as usize) << 8) | data[desc_loop_start + 1] as usize;
        let mut d_off = desc_loop_start + 2;
        let d_end = (desc_loop_start + 2 + desc_loop_len).min(data.len());
        while d_off + 2 <= d_end {
            let tag = data[d_off];
            let len = data[d_off + 1] as usize;
            if d_off + 2 + len > d_end {
                break;
            }
            let body = &data[d_off + 2..d_off + 2 + len];
            if tag == 0x02 {
                if let Some(seg) = parse_segmentation_descriptor(body) {
                    descriptors.push(seg);
                }
            }
            d_off += 2 + len;
        }
    }

    let _ = protocol_version;
    Some(SpliceInfoSection {
        table_id,
        protocol_version,
        splice_command_type,
        out_of_network_indicator: out_of_network,
        splice_event_id,
        pts_time,
        descriptors,
    })
}

fn parse_segmentation_descriptor(body: &[u8]) -> Option<SegmentationDescriptor> {
    if body.len() < 12 {
        return None;
    }
    let segmentation_event_id = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let cancel = body[8] & 0x80 != 0;
    if cancel {
        return Some(SegmentationDescriptor {
            segmentation_event_id,
            segmentation_type_id: 0,
            segmentation_type_name: "Cancel".into(),
            segmentation_duration_secs: None,
        });
    }
    let mut off = 9usize;
    if off >= body.len() {
        return None;
    }
    let program_seg = body[off] & 0x80 != 0;
    let has_duration = body[off] & 0x40 != 0;
    off += 1;
    if !program_seg {
        if off >= body.len() {
            return None;
        }
        let n = body[off] as usize;
        off += 1 + n * 6;
    }
    let mut duration_secs = None;
    if has_duration {
        if off + 5 > body.len() {
            return None;
        }
        let ticks = ((body[off] as u64) << 32)
            | ((body[off + 1] as u64) << 24)
            | ((body[off + 2] as u64) << 16)
            | ((body[off + 3] as u64) << 8)
            | body[off + 4] as u64;
        duration_secs = Some(ticks as f64 / 90_000.0);
        off += 5;
    }
    if off >= body.len() {
        return None;
    }
    let type_id = body[off];
    Some(SegmentationDescriptor {
        segmentation_event_id,
        segmentation_type_id: type_id,
        segmentation_type_name: segmentation_type_name(type_id),
        segmentation_duration_secs: duration_secs,
    })
}

/// Parse a tag line into a decoded section when a binary payload is present.
pub fn parse_scte35_tag(line: &str) -> Option<SpliceInfoSection> {
    let payload = extract_payload_from_tag(line)?;
    let bytes = decode_scte35_payload(&payload)?;
    parse_scte35_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_base64_timesignal_minimal() {
        let mut data = [0u8; 22];
        data[0] = 0xfc;
        data[5] = 0;
        data[11] = 0;
        data[12] = 1;
        data[13] = 0x06;
        data[14] = 0x00;
        data[15] = 0;
        data[16] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert_eq!(parsed.splice_command_type, SpliceCommandType::TimeSignal);
        assert!(parsed.summary_line().contains("TimeSignal"));
    }

    #[test]
    fn segmentation_names() {
        assert_eq!(segmentation_type_name(0x30), "Provider Ad Start");
        assert_eq!(segmentation_type_name(0x34), "Placement Opportunity Start");
    }

    #[test]
    fn extract_oatcls() {
        let line = "#EXT-OATCLS-SCTE35:/DAlAAAAAAAAAP/wFAUAAAABf+/+AYwGTAACAADQAAAA";
        let p = extract_payload_from_tag(line).unwrap();
        assert!(p.starts_with("/DAl"));
    }
}
