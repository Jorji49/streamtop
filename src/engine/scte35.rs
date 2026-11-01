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
            Self::SpliceNull => "Splice Null",
            Self::SpliceSchedule => "Splice Schedule",
            Self::SpliceInsert => "Splice Insert",
            Self::TimeSignal => "Time Signal",
            Self::BandwidthReservation => "Bandwidth Reservation",
            Self::Private(_) => "Private",
            Self::Unknown(_) => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SegmentationDescriptor {
    pub segmentation_event_id: u32,
    pub segmentation_type_id: u8,
    pub segmentation_type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segmentation_duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upid_type: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upid_type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upid_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_num: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_expected: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_segment_num: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_segments_expected: Option<u8>,
    #[serde(default)]
    pub sub_segment_alignment: bool,
    #[serde(default)]
    pub delivery_not_restricted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_delivery_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_regional_blackout: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_allowed: Option<bool>,
    /// `device_restrictions` (0 = group 0, 1 = group 1, 2 = group 2, 3 = none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_restrictions: Option<u8>,
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
    /// PTS time in 90 kHz ticks (SCTE-35 splice_time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_time: Option<u64>,
    /// `pts_time / 90_000` seconds when PTS is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_time_secs: Option<f64>,
    /// SpliceInsert / SpliceSchedule break_duration (90 kHz ticks → seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_auto_return: Option<bool>,
    /// Number of scheduled splice events (SpliceSchedule).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splice_count: Option<u8>,
    pub descriptors: Vec<SegmentationDescriptor>,
    /// Structured notes for unhandled / partial command parsing (metrics / logs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_notes: Vec<String>,
}

impl SpliceInfoSection {
    /// Human-readable log line for the TUI event panel.
    pub fn summary_line(&self) -> String {
        let cmd = self.splice_command_type.as_str();
        let seg_names: Vec<&str> = self
            .descriptors
            .iter()
            .map(|d| d.segmentation_type_name.as_str())
            .collect();
        let kind = if seg_names.is_empty() {
            match self.out_of_network_indicator {
                Some(true) => "Out of Network".to_string(),
                Some(false) => "Return to Network".to_string(),
                None => "-".to_string(),
            }
        } else {
            seg_names.join(", ")
        };
        let seg = self.descriptors.first();
        let dur = self
            .break_duration_secs
            .or_else(|| {
                self.descriptors
                    .first()
                    .and_then(|d| d.segmentation_duration_secs)
            })
            .map_or_else(|| "Duration: -".into(), |s| format!("Duration: {s:.1}s"));
        let event = self
            .splice_event_id
            .or_else(|| seg.map(|d| d.segmentation_event_id))
            .map_or_else(|| "EventID: -".into(), |id| format!("EventID: {id}"));
        let pts = self
            .pts_time_secs
            .map_or_else(|| "PTS: -".into(), |s| format!("PTS: {s:.3}s"));
        let sched = self
            .splice_count
            .map(|n| format!("ScheduleN: {n}"))
            .unwrap_or_default();
        let notes = if self.parse_notes.is_empty() {
            String::new()
        } else {
            format!(" | {}", self.parse_notes.join("; "))
        };
        format!(
            "[SCTE-35 BINARY] {cmd} | {kind} | {dur} | {event} | {pts}{sched}{notes}",
            sched = if sched.is_empty() {
                String::new()
            } else {
                format!(" | {sched}")
            }
        )
    }
}

pub fn upid_type_name(id: u8) -> &'static str {
    // SCTE-35 Table 21 (segmentation_upid_type).
    match id {
        0x00 => "Not Used",
        0x01 => "User Defined",
        0x02 => "ISCI",
        0x03 => "Ad-ID",
        0x04 => "UMID",
        0x05 => "ISAN (deprecated)",
        0x06 => "ISAN",
        0x07 => "TID",
        0x08 => "TI",
        0x09 => "ADI",
        0x0a => "EIDR",
        0x0b => "ATSC Content Identifier",
        0x0c => "MPU",
        0x0d => "MID",
        0x0e => "ADS Information",
        0x0f => "URI",
        0x10 => "UUID",
        0x11 => "SCR",
        _ => "Reserved",
    }
}

pub fn segmentation_type_name(id: u8) -> String {
    // SCTE-35 Table 23 (segmentation_type_id). 0x34/0x35 keep the short provider name.
    let name = match id {
        0x00 => "Not Indicated",
        0x01 => "Content Identification",
        0x02 => "Call Ad Server",
        0x10 => "Program Start",
        0x11 => "Program End",
        0x12 => "Program Early Termination",
        0x13 => "Program Breakaway",
        0x14 => "Program Resumption",
        0x15 => "Program Runover Planned",
        0x16 => "Program Runover Unplanned",
        0x17 => "Program Overlap Start",
        0x18 => "Program Blackout Override",
        0x19 => "Program Start In Progress",
        0x20 => "Chapter Start",
        0x21 => "Chapter End",
        0x22 => "Break Start",
        0x23 => "Break End",
        0x24 => "Opening Credit Start",
        0x25 => "Opening Credit End",
        0x26 => "Closing Credit Start",
        0x27 => "Closing Credit End",
        0x30 => "Provider Ad Start",
        0x31 => "Provider Ad End",
        0x32 => "Distributor Ad Start",
        0x33 => "Distributor Ad End",
        0x34 => "Provider Placement Opportunity Start",
        0x35 => "Provider Placement Opportunity End",
        0x36 => "Distributor Placement Opportunity Start",
        0x37 => "Distributor Placement Opportunity End",
        0x38 => "Provider Overlay Placement Opportunity Start",
        0x39 => "Provider Overlay Placement Opportunity End",
        0x3a => "Distributor Overlay Placement Opportunity Start",
        0x3b => "Distributor Overlay Placement Opportunity End",
        0x3c => "Provider Promo Start",
        0x3d => "Provider Promo End",
        0x3e => "Distributor Promo Start",
        0x3f => "Distributor Promo End",
        0x40 => "Unscheduled Event Start",
        0x41 => "Unscheduled Event End",
        0x42 => "Alternate Content Opportunity Start",
        0x43 => "Alternate Content Opportunity End",
        0x44 => "Provider Ad Block Start",
        0x45 => "Provider Ad Block End",
        0x46 => "Distributor Ad Block Start",
        0x47 => "Distributor Ad Block End",
        0x50 => "Network Start",
        0x51 => "Network End",
        other => return format!("Type 0x{other:02X}"),
    };
    name.into()
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
    let hex: String = s.chars().filter(char::is_ascii_hexdigit).collect();
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
/// Decode user input for `streamtop scte35`: hex (with or without `0x`), Base64, or an
/// HLS tag line such as `#EXT-OATCLS-SCTE35:...`.
///
/// Unlike [`decode_scte35_payload`], an all-hex string is always read as hex, because
/// many hex strings are also valid Base64.
pub fn decode_cli_input(raw: &str) -> Result<Vec<u8>, String> {
    let mut s = raw.trim().trim_matches('"').trim().to_string();
    if s.starts_with('#') {
        if let Some(p) = extract_payload_from_tag(&s) {
            s = p;
        }
    }
    for key in ["SCTE35-OUT=", "SCTE35-IN=", "SCTE35-CMD=", "CUE="] {
        if let Some((_, v)) = s.split_once(key) {
            s = v
                .split(',')
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            break;
        }
    }
    if s.starts_with('#') {
        return Err("no SCTE-35 payload found in tag line".into());
    }
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let body = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
        .unwrap_or(&compact);
    if body.is_empty() {
        return Err("empty payload".into());
    }
    if body.chars().all(|c| c.is_ascii_hexdigit()) {
        if !body.len().is_multiple_of(2) {
            return Err("hex payload has an odd number of digits".into());
        }
        return (0..body.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&body[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect();
    }
    B64.decode(body)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(body))
        .map_err(|e| format!("not valid hex or Base64: {e}"))
}

/// Multi-line, human-readable view of a decoded section for `streamtop scte35`.
pub fn render_text(sec: &SpliceInfoSection) -> String {
    use std::fmt::Write as _;
    fn yn(b: bool) -> &'static str {
        if b {
            "yes"
        } else {
            "no"
        }
    }
    let mut o = String::new();
    let _ = writeln!(o, "table_id            0x{:02x}", sec.table_id);
    let _ = writeln!(o, "protocol_version    {}", sec.protocol_version);
    let cmd_id = match sec.splice_command_type {
        SpliceCommandType::SpliceNull => 0x00,
        SpliceCommandType::SpliceSchedule => 0x04,
        SpliceCommandType::SpliceInsert => 0x05,
        SpliceCommandType::TimeSignal => 0x06,
        SpliceCommandType::BandwidthReservation => 0x07,
        SpliceCommandType::Private(v) | SpliceCommandType::Unknown(v) => v,
    };
    let _ = writeln!(
        o,
        "splice_command      {} (0x{cmd_id:02x})",
        sec.splice_command_type.as_str()
    );
    if let Some(id) = sec.splice_event_id {
        let _ = writeln!(o, "splice_event_id     {id} (0x{id:08x})");
    }
    if let Some(v) = sec.out_of_network_indicator {
        let _ = writeln!(o, "out_of_network      {}", yn(v));
    }
    if let (Some(ticks), Some(secs)) = (sec.pts_time, sec.pts_time_secs) {
        let _ = writeln!(o, "pts_time            {ticks} ({secs:.6}s)");
    }
    if let Some(d) = sec.break_duration_secs {
        let _ = writeln!(o, "break_duration      {d:.6}s");
    }
    if let Some(v) = sec.break_auto_return {
        let _ = writeln!(o, "break_auto_return   {}", yn(v));
    }
    if let Some(n) = sec.splice_count {
        let _ = writeln!(o, "splice_count        {n}");
    }
    for (i, d) in sec.descriptors.iter().enumerate() {
        let _ = writeln!(o, "segmentation_descriptor[{i}]");
        let _ = writeln!(
            o,
            "  event_id          {} (0x{:08x})",
            d.segmentation_event_id, d.segmentation_event_id
        );
        let _ = writeln!(
            o,
            "  type              {} (0x{:02x})",
            d.segmentation_type_name, d.segmentation_type_id
        );
        if let Some(v) = d.segmentation_duration_secs {
            let _ = writeln!(o, "  duration          {v:.6}s");
        }
        if let Some(t) = d.upid_type {
            let name = d.upid_type_name.as_deref().unwrap_or("-");
            let _ = writeln!(o, "  upid_type         {name} (0x{t:02x})");
        }
        if let Some(h) = &d.upid_hex {
            let _ = writeln!(o, "  upid              {h}");
        }
        if let (Some(n), Some(e)) = (d.segment_num, d.segments_expected) {
            let _ = writeln!(o, "  segment_num       {n} (expected {e})");
        }
        if let (Some(n), Some(e)) = (d.sub_segment_num, d.sub_segments_expected) {
            let _ = writeln!(o, "  sub_segment_num   {n} (expected {e})");
        }
        let _ = writeln!(
            o,
            "  delivery_not_restricted {}",
            yn(d.delivery_not_restricted)
        );
        if let Some(v) = d.web_delivery_allowed {
            let _ = writeln!(o, "  web_delivery_allowed    {}", yn(v));
        }
        if let Some(v) = d.no_regional_blackout {
            let _ = writeln!(o, "  no_regional_blackout    {}", yn(v));
        }
        if let Some(v) = d.archive_allowed {
            let _ = writeln!(o, "  archive_allowed         {}", yn(v));
        }
        if let Some(v) = d.device_restrictions {
            let label = match v {
                0 => "Restrict Group 0",
                1 => "Restrict Group 1",
                2 => "Restrict Group 2",
                _ => "None",
            };
            let _ = writeln!(o, "  device_restrictions     {label}");
        }
    }
    for n in &sec.parse_notes {
        let _ = writeln!(o, "note                {n}");
    }
    o
}

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
    if data.len() > crate::models::MAX_SCTE35_BYTES {
        return None;
    }
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
    let mut break_duration_secs = None;
    let mut break_auto_return = None;
    let mut splice_count = None;
    let mut parse_notes = Vec::new();

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
                    let _ = data[off] & 0x10;
                    off += 1;
                    if program_splice && off < cmd_end {
                        let (pts, consumed) = parse_splice_time(&data[off..cmd_end]);
                        pts_time = pts;
                        off += consumed;
                    }
                    if duration_flag && off + 5 <= cmd_end {
                        if let Some((ticks, auto_ret)) = read_break_duration(&data[off..]) {
                            break_duration_secs = Some(ticks as f64 / 90_000.0);
                            if break_auto_return.is_none() {
                                break_auto_return = Some(auto_ret);
                            }
                            off += 5;
                        } else {
                            parse_notes.push("break_duration truncated".into());
                        }
                    }
                    let _ = off;
                }
            } else {
                parse_notes.push("SpliceInsert truncated".into());
            }
        }
        SpliceCommandType::TimeSignal => {
            if off < cmd_end {
                let (pts, consumed) = parse_splice_time(&data[off..cmd_end]);
                pts_time = pts;
                off += consumed;
                if pts_time.is_none() {
                    parse_notes.push("TimeSignal without time_specified_flag".into());
                }
            } else {
                parse_notes.push("TimeSignal empty command body".into());
            }
            let _ = off;
        }
        SpliceCommandType::SpliceSchedule => {
            if off < cmd_end {
                let count = data[off];
                splice_count = Some(count);
                off += 1;
                for i in 0..count {
                    if off + 5 > cmd_end {
                        parse_notes.push(format!("SpliceSchedule event {i} truncated"));
                        break;
                    }
                    let eid = u32::from_be_bytes([
                        data[off],
                        data[off + 1],
                        data[off + 2],
                        data[off + 3],
                    ]);
                    if splice_event_id.is_none() {
                        splice_event_id = Some(eid);
                    }
                    let cancel = data[off + 4] & 0x80 != 0;
                    off += 5;
                    if cancel {
                        continue;
                    }
                    if off >= cmd_end {
                        parse_notes.push(format!("SpliceSchedule event {i} missing flags"));
                        break;
                    }
                    out_of_network = Some(data[off] & 0x80 != 0);
                    let program_splice = data[off] & 0x40 != 0;
                    let duration_flag = data[off] & 0x20 != 0;
                    off += 1;
                    if program_splice && off < cmd_end {
                        let (pts, consumed) = parse_splice_time(&data[off..cmd_end]);
                        if pts_time.is_none() {
                            pts_time = pts;
                        }
                        off += consumed;
                    }
                    if duration_flag && off + 5 <= cmd_end {
                        if let Some((ticks, auto_ret)) = read_break_duration(&data[off..]) {
                            if break_duration_secs.is_none() {
                                break_duration_secs = Some(ticks as f64 / 90_000.0);
                                break_auto_return = Some(auto_ret);
                            }
                            off += 5;
                        } else {
                            parse_notes
                                .push(format!("SpliceSchedule event {i} truncated break_duration"));
                            break;
                        }
                    }
                    // unique_program_id (16) + avail_num (8) + avails_expected (8)
                    if off + 4 <= cmd_end {
                        off += 4;
                    } else {
                        parse_notes.push(format!("SpliceSchedule event {i} missing avail fields"));
                        break;
                    }
                }
            } else {
                parse_notes.push("SpliceSchedule empty command body".into());
            }
        }
        SpliceCommandType::SpliceNull => {}
        SpliceCommandType::BandwidthReservation => {
            parse_notes.push("BandwidthReservation command (no splice timing)".into());
        }
        SpliceCommandType::Private(v) => {
            parse_notes.push(format!("Private splice command 0x{v:02X}"));
        }
        SpliceCommandType::Unknown(v) => {
            parse_notes.push(format!("Unknown splice_command_type 0x{v:02X}"));
        }
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
                parse_notes.push(format!("descriptor tag=0x{tag:02X} truncated"));
                break;
            }
            let body = &data[d_off + 2..d_off + 2 + len];
            if tag == 0x02 {
                if let Some(seg) = parse_segmentation_descriptor(body) {
                    descriptors.push(seg);
                } else {
                    parse_notes.push("segmentation_descriptor parse failed".into());
                }
            }
            d_off += 2 + len;
        }
    }

    let pts_time_secs = pts_time.map(|t| t as f64 / 90_000.0);
    let _ = protocol_version;
    Some(SpliceInfoSection {
        table_id,
        protocol_version,
        splice_command_type,
        out_of_network_indicator: out_of_network,
        splice_event_id,
        pts_time,
        pts_time_secs,
        break_duration_secs,
        break_auto_return,
        splice_count,
        descriptors,
        parse_notes,
    })
}

fn parse_splice_time(data: &[u8]) -> (Option<u64>, usize) {
    if data.is_empty() {
        return (None, 0);
    }
    let time_specified = data[0] & 0x80 != 0;
    if time_specified {
        if data.len() < 5 {
            return (None, data.len());
        }
        let pts = (((data[0] as u64) & 0x01) << 32)
            | ((data[1] as u64) << 24)
            | ((data[2] as u64) << 16)
            | ((data[3] as u64) << 8)
            | data[4] as u64;
        (Some(pts), 5)
    } else {
        (None, 1)
    }
}

fn read_break_duration(data: &[u8]) -> Option<(u64, bool)> {
    let bytes = crate::engine::slice_util::subslice_len(data, 0, 5)?;
    let auto_return = bytes[0] & 0x80 != 0;
    let ticks = (((bytes[0] as u64) & 0x01) << 32)
        | ((bytes[1] as u64) << 24)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 8)
        | bytes[4] as u64;
    Some((ticks, auto_return))
}

fn parse_segmentation_descriptor(body: &[u8]) -> Option<SegmentationDescriptor> {
    if body.len() < 12 {
        return None;
    }
    let segmentation_event_id = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    if body[8] & 0x80 != 0 {
        return Some(SegmentationDescriptor {
            segmentation_event_id,
            segmentation_type_name: "Cancel".into(),
            ..Default::default()
        });
    }
    let mut off = 9usize;
    if off >= body.len() {
        return None;
    }
    let flags = body[off];
    off += 1;
    let program_seg = flags & 0x80 != 0;
    let has_duration = flags & 0x40 != 0;
    let delivery_not_restricted = flags & 0x20 != 0;
    // SCTE-35 10.3.3.1: these flags exist only when delivery_not_restricted_flag is 0.
    let (web_delivery_allowed, no_regional_blackout, archive_allowed, device_restrictions) =
        if delivery_not_restricted {
            (None, None, None, None)
        } else {
            (
                Some(flags & 0x10 != 0),
                Some(flags & 0x08 != 0),
                Some(flags & 0x04 != 0),
                Some(flags & 0x03),
            )
        };

    if !program_seg {
        if off >= body.len() {
            return None;
        }
        let n = body[off] as usize;
        off += 1;
        off = off.saturating_add(n.saturating_mul(6));
        if off > body.len() {
            return None;
        }
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

    if off + 2 > body.len() {
        return None;
    }
    let upid_type = body[off];
    let upid_len = body[off + 1] as usize;
    off += 2;
    if off + upid_len > body.len() {
        return None;
    }
    let upid_bytes = &body[off..off + upid_len];
    off += upid_len;

    if off + 3 > body.len() {
        return None;
    }
    let type_id = body[off];
    let segment_num = body[off + 1];
    let segments_expected = body[off + 2];
    off += 3;

    let mut sub_segment_num = None;
    let mut sub_segments_expected = None;
    let sub_segment_alignment = segments_expected > 0 && off + 2 <= body.len();
    if sub_segment_alignment {
        sub_segment_num = Some(body[off]);
        sub_segments_expected = Some(body[off + 1]);
    }

    let upid_hex = if upid_len == 0 {
        None
    } else {
        Some(
            upid_bytes
                .iter()
                .fold(String::with_capacity(upid_len * 2), |mut s, b| {
                    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
                    s
                }),
        )
    };

    Some(SegmentationDescriptor {
        segmentation_event_id,
        segmentation_type_id: type_id,
        segmentation_type_name: segmentation_type_name(type_id),
        segmentation_duration_secs: duration_secs,
        upid_type: Some(upid_type),
        upid_type_name: Some(upid_type_name(upid_type).into()),
        upid_hex,
        segment_num: Some(segment_num),
        segments_expected: Some(segments_expected),
        sub_segment_num,
        sub_segments_expected,
        sub_segment_alignment,
        delivery_not_restricted,
        web_delivery_allowed,
        no_regional_blackout,
        archive_allowed,
        device_restrictions,
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
        assert!(parsed.summary_line().contains("Time Signal"));
        assert!(!parsed.parse_notes.is_empty() || parsed.pts_time.is_none());
    }

    #[test]
    fn timesignal_pts_90khz() {
        let mut data = vec![0u8; 22];
        data[0] = 0xfc;
        data[11] = 0;
        data[12] = 5; // command length
        data[13] = 0x06;
        // time_specified + pts = 180_000 ticks = 2.0s
        let ticks: u64 = 180_000;
        data[14] = 0x80 | ((ticks >> 32) & 0x01) as u8;
        data[15] = ((ticks >> 24) & 0xff) as u8;
        data[16] = ((ticks >> 16) & 0xff) as u8;
        data[17] = ((ticks >> 8) & 0xff) as u8;
        data[18] = (ticks & 0xff) as u8;
        data[19] = 0;
        data[20] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert_eq!(parsed.pts_time, Some(180_000));
        let secs = parsed.pts_time_secs.expect("pts secs");
        assert!((secs - 2.0).abs() < 0.001, "secs={secs}");
    }

    #[test]
    fn splice_schedule_parses_count_and_duration() {
        // Minimal schedule: 1 event, duration only, no program splice
        let mut data = vec![0u8; 40];
        data[0] = 0xfc;
        data[11] = 0;
        data[12] = 15;
        data[13] = 0x04; // SpliceSchedule
        data[14] = 1; // splice_count
        data[15..19].copy_from_slice(&7u32.to_be_bytes()); // event id
        data[19] = 0; // cancel=0
        data[20] = 0x20; // duration_flag, !program_splice, out_of_network=0
        let ticks: u64 = 450_000; // 5.0s
        data[21] = ((ticks >> 32) & 0x01) as u8;
        data[22] = ((ticks >> 24) & 0xff) as u8;
        data[23] = ((ticks >> 16) & 0xff) as u8;
        data[24] = ((ticks >> 8) & 0xff) as u8;
        data[25] = (ticks & 0xff) as u8;
        // unique_program_id + avails
        data[26] = 0;
        data[27] = 1;
        data[28] = 0;
        data[29] = 0;
        data[30] = 0;
        data[31] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert_eq!(
            parsed.splice_command_type,
            SpliceCommandType::SpliceSchedule
        );
        assert_eq!(parsed.splice_count, Some(1));
        assert_eq!(parsed.splice_event_id, Some(7));
        let dur = parsed.break_duration_secs.expect("dur");
        assert!((dur - 5.0).abs() < 0.001, "dur={dur}");
    }

    #[test]
    fn unknown_command_emits_parse_note() {
        let mut data = vec![0u8; 18];
        data[0] = 0xfc;
        data[11] = 0;
        data[12] = 0;
        data[13] = 0x42;
        data[14] = 0;
        data[15] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert!(parsed
            .parse_notes
            .iter()
            .any(|n| n.contains("Unknown splice_command_type")));
    }

    #[test]
    fn segmentation_names() {
        assert_eq!(segmentation_type_name(0x30), "Provider Ad Start");
        assert_eq!(
            segmentation_type_name(0x34),
            "Provider Placement Opportunity Start"
        );
    }

    #[test]
    fn extract_oatcls() {
        let line = "#EXT-OATCLS-SCTE35:/DAlAAAAAAAAAP/wFAUAAAABf+/+AYwGTAACAADQAAAA";
        let p = extract_payload_from_tag(line).unwrap();
        assert!(p.starts_with("/DAl"));
    }

    #[test]
    fn splice_insert_break_duration_90khz() {
        let mut data = vec![0u8; 32];
        data[0] = 0xfc;
        data[11] = 0;
        data[12] = 12;
        data[13] = 0x05;
        data[14..18].copy_from_slice(&1u32.to_be_bytes());
        data[18] = 0;
        data[19] = 0x20;
        let ticks: u64 = 900_000;
        data[20] = ((ticks >> 32) & 0x01) as u8;
        data[21] = ((ticks >> 24) & 0xff) as u8;
        data[22] = ((ticks >> 16) & 0xff) as u8;
        data[23] = ((ticks >> 8) & 0xff) as u8;
        data[24] = (ticks & 0xff) as u8;
        data[25] = 0;
        data[26] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert_eq!(parsed.splice_command_type, SpliceCommandType::SpliceInsert);
        let dur = parsed.break_duration_secs.expect("break_duration");
        assert!((dur - 10.0).abs() < 0.001, "dur={dur}");
    }

    #[test]
    fn splice_insert_auto_return_flag() {
        let mut data = vec![0u8; 32];
        data[0] = 0xfc;
        data[11] = 0;
        data[12] = 12;
        data[13] = 0x05;
        data[14..18].copy_from_slice(&1u32.to_be_bytes());
        data[18] = 0;
        data[19] = 0xA0; // duration_flag + auto_return on break_duration
        let ticks: u64 = 900_000;
        data[20] = 0x80 | ((ticks >> 32) & 0x01) as u8;
        data[21] = ((ticks >> 24) & 0xff) as u8;
        data[22] = ((ticks >> 16) & 0xff) as u8;
        data[23] = ((ticks >> 8) & 0xff) as u8;
        data[24] = (ticks & 0xff) as u8;
        data[25] = 0;
        data[26] = 0;
        let parsed = parse_scte35_bytes(&data).expect("parse");
        assert_eq!(parsed.break_auto_return, Some(true));
    }

    #[test]
    fn truncated_and_empty_payloads_do_not_panic() {
        assert!(parse_scte35_bytes(&[]).is_none());
        assert!(parse_scte35_bytes(&[0xfc]).is_none());
        assert!(read_break_duration(&[]).is_none());
        assert!(read_break_duration(&[0x80, 0, 0, 0]).is_none());
        for len in 0..20 {
            let data = vec![0xfc; len];
            let _ = parse_scte35_bytes(&data);
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    // SCTE-35 2023 spec, sample 14.2 (time_signal, Provider Placement Opportunity Start).
    const TS_B64: &str =
        "/DA0AAAAAAAA///wBQb+cr0AUAAeAhxDVUVJSAAAjn/PAAGlmbAICAAAAAAsoKGKNAIAmsnRfg==";
    // SCTE-35 2023 spec, sample 14.1 (splice_insert).
    const INSERT_B64: &str = "/DAvAAAAAAAA///wFAVIAACPf+/+c2nALv4AUsz1AAAAAAAKAAhDVUVJAAABNWLbowo=";

    fn hex_of(b64: &str) -> String {
        use std::fmt::Write as _;
        B64.decode(b64)
            .unwrap()
            .iter()
            .fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            })
    }

    /// Field values cross-checked with threefive 2.x on the same payload.
    #[test]
    fn time_signal_descriptor_flags_match_reference_decoder() {
        let sec = parse_scte35_bytes(&decode_cli_input(TS_B64).unwrap()).unwrap();
        let d = &sec.descriptors[0];
        assert_eq!(d.segmentation_event_id, 0x4800_008e);
        assert_eq!(d.segmentation_type_id, 0x34);
        assert_eq!(
            d.segmentation_type_name,
            "Provider Placement Opportunity Start"
        );
        assert_eq!(d.segmentation_duration_secs, Some(307.0));
        assert_eq!(d.upid_type, Some(0x08));
        assert_eq!(d.segment_num, Some(2));
        assert_eq!(d.segments_expected, Some(0));
        assert!(!d.delivery_not_restricted);
        assert_eq!(d.web_delivery_allowed, Some(false));
        assert_eq!(d.no_regional_blackout, Some(true));
        assert_eq!(d.archive_allowed, Some(true));
        assert_eq!(d.device_restrictions, Some(3));
    }

    #[test]
    fn hex_base64_and_tag_inputs_match() {
        let from_b64 = decode_cli_input(TS_B64).unwrap();
        let hex = hex_of(TS_B64);
        assert_eq!(decode_cli_input(&hex).unwrap(), from_b64);
        assert_eq!(
            decode_cli_input(&format!("0x{}", hex.to_uppercase())).unwrap(),
            from_b64
        );
        assert_eq!(
            decode_cli_input(&format!("#EXT-OATCLS-SCTE35:{TS_B64}")).unwrap(),
            from_b64
        );
        assert_eq!(
            decode_cli_input(&format!("SCTE35-OUT=0x{hex},X-FOO=1")).unwrap(),
            from_b64
        );
        assert_eq!(
            decode_cli_input(&format!("#EXT-X-DATERANGE:ID=\"1\",SCTE35-OUT=0x{hex}")).unwrap(),
            from_b64
        );
        assert_eq!(
            decode_cli_input(&format!("#EXT-X-SCTE35:CUE=\"{TS_B64}\",ID=\"1\"")).unwrap(),
            from_b64
        );
        assert!(decode_cli_input("#EXT-X-CUE-IN").is_err());
        assert!(decode_cli_input("abc").is_err());
        assert!(decode_cli_input("  ").is_err());
    }

    #[test]
    fn decodes_time_signal_sample() {
        let sec = parse_scte35_bytes(&decode_cli_input(TS_B64).unwrap()).unwrap();
        assert_eq!(sec.table_id, 0xfc);
        assert_eq!(sec.splice_command_type, SpliceCommandType::TimeSignal);
        assert_eq!(sec.pts_time, Some(0x0000_72bd_0050));
        let d = &sec.descriptors[0];
        assert_eq!(d.segmentation_event_id, 0x4800_008e);
        assert_eq!(d.segmentation_type_id, 0x34);
        assert!((d.segmentation_duration_secs.unwrap() - 307.0).abs() < 1e-6);
        let text = render_text(&sec);
        assert!(text.contains("Time Signal (0x06)"));
        assert!(text.contains("(0x34)"));
    }

    #[test]
    fn decodes_splice_insert_sample() {
        let sec = parse_scte35_bytes(&decode_cli_input(INSERT_B64).unwrap()).unwrap();
        assert_eq!(sec.splice_command_type, SpliceCommandType::SpliceInsert);
        assert_eq!(sec.splice_event_id, Some(0x4800_008f));
        assert_eq!(sec.out_of_network_indicator, Some(true));
        assert_eq!(sec.break_auto_return, Some(true));
        assert!((sec.break_duration_secs.unwrap() - 60.293_566_666).abs() < 1e-3);
        assert!(render_text(&sec).contains("out_of_network      yes"));
    }
}
