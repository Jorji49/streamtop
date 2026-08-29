//! Codec / resolution / FPS probe for fMP4 and MPEG-TS.

use std::collections::HashMap;

use crate::models::{ContainerKind, InbandEmsgInfo, WireProbeInfo, WireTimingInfo};

/// Probe segment bytes for codec / resolution / FPS.
pub fn deep_wire_probe(bytes: &[u8]) -> WireProbeInfo {
    let kind = classify_container(bytes);
    let mut info = match kind {
        ContainerKind::Fmp4 => probe_fmp4(bytes),
        ContainerKind::Ts => probe_mpeg_ts(bytes),
        ContainerKind::Unknown => WireProbeInfo::default(),
    };
    info.container = kind;
    let codec_hint = info.codec.clone();
    info.keyframe_pts_sec = extract_keyframe_pts_sec(bytes, kind, codec_hint.as_deref());
    info.timing = probe_wire_timing(bytes, kind);
    probe_audio_wire(bytes, kind, &mut info);
    info.pssh = crate::engine::pssh::scan_pssh_boxes(bytes);
    if kind == ContainerKind::Fmp4 {
        info.inband_emsg = scan_inband_emsg(bytes);
    }
    info
}

/// Scan fMP4 probe window for DASH `emsg` inband event boxes (SCTE schemes only).
pub fn scan_inband_emsg(bytes: &[u8]) -> Vec<InbandEmsgInfo> {
    let mut out = Vec::new();
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        if name == b"emsg" {
            if let Some(ev) = parse_emsg_box(payload) {
                if ev.is_scte_related() {
                    out.push(ev);
                }
            }
        }
        true
    });
    out
}

/// Parse ISO BMFF `emsg` fullbox payload (v0 or v1).
pub fn parse_emsg_box(payload: &[u8]) -> Option<InbandEmsgInfo> {
    if payload.len() < 12 {
        return None;
    }
    let version = payload[0];
    match version {
        0 => parse_emsg_v0(payload),
        1 => parse_emsg_v1(payload),
        _ => None,
    }
}

fn parse_emsg_v0(payload: &[u8]) -> Option<InbandEmsgInfo> {
    let mut off = 12usize;
    let (scheme, next) = read_cstr(payload, off)?;
    off = next;
    let (value, next) = read_cstr(payload, off)?;
    off = next;
    if off + 16 > payload.len() {
        return None;
    }
    let timescale = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    let presentation_time_delta =
        u64::from(u32::from_be_bytes(payload[off..off + 4].try_into().ok()?));
    off += 4;
    let event_duration = u64::from(u32::from_be_bytes(payload[off..off + 4].try_into().ok()?));
    off += 4;
    let id = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    Some(InbandEmsgInfo {
        version: 0,
        scheme_id_uri: scheme,
        value: if value.is_empty() { None } else { Some(value) },
        timescale,
        presentation_time_delta,
        event_duration,
        id,
        message_data: copy_tail(payload, off),
    })
}

fn copy_tail(data: &[u8], off: usize) -> Vec<u8> {
    data.get(off..).unwrap_or(&[]).to_vec()
}

fn parse_emsg_v1(payload: &[u8]) -> Option<InbandEmsgInfo> {
    let mut off = 12usize;
    if off + 24 > payload.len() {
        return None;
    }
    let timescale = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    let presentation_time = u64::from_be_bytes(payload[off..off + 8].try_into().ok()?);
    off += 8;
    let event_duration = u64::from(u32::from_be_bytes(payload[off..off + 4].try_into().ok()?));
    off += 4;
    let id = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?);
    off += 4;
    let (scheme, next) = read_cstr(payload, off)?;
    off = next;
    let (value, next) = read_cstr(payload, off)?;
    off = next;
    Some(InbandEmsgInfo {
        version: 1,
        scheme_id_uri: scheme,
        value: if value.is_empty() { None } else { Some(value) },
        timescale,
        presentation_time_delta: presentation_time,
        event_duration,
        id,
        message_data: copy_tail(payload, off),
    })
}

fn read_cstr(data: &[u8], start: usize) -> Option<(String, usize)> {
    if start >= data.len() {
        return None;
    }
    let end = data[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| start + i)
        .unwrap_or(data.len());
    let s = String::from_utf8_lossy(&data[start..end]).into_owned();
    let next = if end < data.len() { end + 1 } else { end };
    Some((s, next))
}

fn probe_audio_wire(bytes: &[u8], kind: ContainerKind, info: &mut WireProbeInfo) {
    if kind == ContainerKind::Ts || bytes.windows(2).any(|w| w == [0xff, 0xf0]) {
        info.adts_sync_valid = validate_adts_sync(bytes);
        info.audio_silent_suspect = detect_silent_audio(bytes);
    }
}

fn validate_adts_sync(data: &[u8]) -> bool {
    for i in 0..data.len().saturating_sub(7) {
        if data[i] == 0xff && (data[i + 1] & 0xf6) == 0xf0 {
            return true;
        }
    }
    false
}

fn detect_silent_audio(data: &[u8]) -> bool {
    if data.len() < 64 {
        return false;
    }
    let sample = &data[..data.len().min(4096)];
    let zeros = sample.iter().filter(|&&b| b == 0).count();
    zeros * 100 / sample.len().max(1) > 92
}

const NTP_UNIX_EPOCH_OFFSET: u64 = 2208988800;

fn apply_prft(payload: &[u8], timing: &mut WireTimingInfo) {
    if payload.len() < 24 {
        return;
    }
    let _version = payload[0];
    let ntp_secs = u64::from_be_bytes([
        payload[8],
        payload[9],
        payload[10],
        payload[11],
        payload[12],
        payload[13],
        payload[14],
        payload[15],
    ]);
    let media_time = u64::from_be_bytes([
        payload[16],
        payload[17],
        payload[18],
        payload[19],
        payload[20],
        payload[21],
        payload[22],
        payload[23],
    ]);
    timing.prft_media_time_ticks = Some(media_time);
    if ntp_secs >= NTP_UNIX_EPOCH_OFFSET {
        let unix_ms = (ntp_secs - NTP_UNIX_EPOCH_OFFSET).saturating_mul(1000);
        timing.prft_ntp_unix_ms = Some(unix_ms);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(unix_ms);
        timing.glass_to_glass_ms = Some(now_ms as i64 - unix_ms as i64);
    }
}

fn classify_container(bytes: &[u8]) -> ContainerKind {
    if bytes
        .windows(4)
        .any(|w| w == b"ftyp" || w == b"styp" || w == b"moof" || w == b"moov" || w == b"traf")
    {
        return ContainerKind::Fmp4;
    }
    if bytes.first() == Some(&0x47) {
        return ContainerKind::Ts;
    }
    if bytes.len() >= 188 && bytes.iter().step_by(188).take(3).all(|b| *b == 0x47) {
        return ContainerKind::Ts;
    }
    ContainerKind::Unknown
}

fn probe_fmp4(bytes: &[u8]) -> WireProbeInfo {
    let mut info = WireProbeInfo::default();
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        match name {
            b"avc1" | b"avc3" => {
                if info.codec.is_none() {
                    info.codec = Some("avc1".into());
                }
                if payload.len() >= 24 {
                    let w = u16::from_be_bytes([payload[16], payload[17]]);
                    let h = u16::from_be_bytes([payload[18], payload[19]]);
                    if w > 0 && h > 0 {
                        info.width = Some(u32::from(w));
                        info.height = Some(u32::from(h));
                    }
                }
                if let Some(avcc) = find_child_box(payload, b"avcC") {
                    parse_avcc(avcc, &mut info);
                }
            }
            b"hvc1" | b"hev1" => {
                if info.codec.is_none() {
                    info.codec = Some("hvc1".into());
                }
                if payload.len() >= 24 {
                    let w = u16::from_be_bytes([payload[16], payload[17]]);
                    let h = u16::from_be_bytes([payload[18], payload[19]]);
                    if w > 0 && h > 0 {
                        info.width = Some(u32::from(w));
                        info.height = Some(u32::from(h));
                    }
                }
                if let Some(hvcc) = find_child_box(payload, b"hvcC") {
                    parse_hvcc(hvcc, &mut info);
                }
            }
            b"mp4a" => {
                if info.audio_codec.is_none() {
                    info.audio_codec = Some("aac".into());
                }
                if info.audio_sample_rate.is_none() && payload.len() >= 28 {
                    let sr = u16::from_be_bytes([payload[24], payload[25]]);
                    let ch = payload.get(17).copied().unwrap_or(0);
                    if sr > 0 {
                        info.audio_sample_rate = Some(u32::from(sr));
                    }
                    if ch > 0 {
                        info.audio_channels = Some(ch);
                    }
                }
            }
            b"esds" => parse_esds(payload, &mut info),
            b"avcC" => parse_avcc(payload, &mut info),
            b"hvcC" => parse_hvcc(payload, &mut info),
            b"trun" if info.sync_sample.is_none() => {
                info.sync_sample = trun_first_sample_sync(payload);
            }
            _ => {}
        }
        true
    });
    let codec_hint = info.codec.clone();
    apply_gop_scan(bytes, codec_hint.as_deref(), &mut info);
    info
}

/// First keyframe presentation time in seconds, from fMP4 tfdt/trun or MPEG-TS PES PTS.
pub fn extract_keyframe_pts_sec(
    bytes: &[u8],
    container: ContainerKind,
    codec: Option<&str>,
) -> Option<f64> {
    match container {
        ContainerKind::Fmp4 => fmp4_keyframe_pts_sec(bytes),
        ContainerKind::Ts => ts_keyframe_pts_sec(bytes, codec),
        ContainerKind::Unknown => None,
    }
}

fn fmp4_keyframe_pts_sec(bytes: &[u8]) -> Option<f64> {
    let mut base: Option<u64> = None;
    let mut timescale = 90000u32;
    let mut default_duration = 0u32;
    let mut default_flags = 0u32;
    let mut trun: Option<Vec<u8>> = None;
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        match name {
            b"tfdt" => base = parse_tfdt(payload),
            b"tfhd" => {
                let (dur, flags) = parse_tfhd(payload);
                if dur > 0 {
                    default_duration = dur;
                }
                default_flags = flags;
            }
            b"mdhd" if payload.len() >= 20 => {
                timescale =
                    u32::from_be_bytes([payload[16], payload[17], payload[18], payload[19]]);
            }
            b"trun" if trun.is_none() => trun = Some(payload.to_vec()),
            _ => {}
        }
        true
    });
    let base = base?;
    let trun = trun?;
    let ticks = trun_first_sync_decode_ticks(&trun, base, default_duration, default_flags)?;
    let scale = timescale.max(1);
    Some(ticks as f64 / scale as f64)
}

fn parse_tfdt(payload: &[u8]) -> Option<u64> {
    if payload.is_empty() {
        return None;
    }
    if payload[0] == 1 {
        if payload.len() < 12 {
            return None;
        }
        Some(u64::from_be_bytes([
            payload[4],
            payload[5],
            payload[6],
            payload[7],
            payload[8],
            payload[9],
            payload[10],
            payload[11],
        ]))
    } else if payload.len() >= 8 {
        Some(u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as u64)
    } else {
        None
    }
}

fn parse_tfhd(payload: &[u8]) -> (u32, u32) {
    if payload.len() < 8 {
        return (0, 0);
    }
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 8;
    }
    if flags & 0x000002 != 0 {
        off += 4;
    }
    let mut default_duration = 0u32;
    let mut default_flags = 0u32;
    if flags & 0x000008 != 0 && off + 4 <= payload.len() {
        default_duration = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
    } else if flags & 0x000008 != 0 {
        off += 4;
    }
    if flags & 0x000010 != 0 && off + 4 <= payload.len() {
        default_flags = u32::from_be_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
    }
    (default_duration, default_flags)
}

fn trun_first_sync_decode_ticks(
    trun: &[u8],
    base: u64,
    default_duration: u32,
    default_flags: u32,
) -> Option<u64> {
    if trun.len() < 8 {
        return None;
    }
    let flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let sample_count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]) as usize;
    if sample_count == 0 {
        return trun_first_sample_sync(trun).filter(|&s| s).map(|_| base);
    }
    let has_duration = flags & 0x000100 != 0;
    let has_composition = flags & 0x000200 != 0;
    let has_sample_flags = flags & 0x000400 != 0;
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 4;
    }
    let mut first_flags = default_flags;
    if flags & 0x000004 != 0 {
        if off + 4 > trun.len() {
            return None;
        }
        first_flags = u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
        off += 4;
    }
    let mut decode = base;
    for i in 0..sample_count {
        let mut duration = default_duration;
        let mut sample_flags = first_flags;
        if has_duration {
            if off + 4 > trun.len() {
                break;
            }
            duration = u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
            off += 4;
        }
        if has_composition {
            off = off.saturating_add(4);
            if off > trun.len() {
                break;
            }
        }
        if has_sample_flags {
            if off + 4 > trun.len() {
                break;
            }
            sample_flags =
                u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
            off += 4;
        }
        let non_sync = (sample_flags >> 16) & 1 == 1;
        if !non_sync {
            return Some(decode);
        }
        decode = decode.saturating_add(u64::from(duration));
        let _ = i;
    }
    trun_first_sample_sync(trun).filter(|&s| s).map(|_| base)
}

fn ts_keyframe_pts_sec(bytes: &[u8], codec: Option<&str>) -> Option<f64> {
    let hevc = codec.is_some_and(|c| c.starts_with("hvc") || c.starts_with("hev"));
    let packets: Vec<&[u8]> = bytes
        .chunks(188)
        .filter(|p| p.len() == 188 && p[0] == 0x47)
        .collect();
    let mut pes_buf = Vec::new();
    let mut current_pts: Option<u64> = None;
    for pkt in packets {
        let Some(payload) = ts_payload(pkt) else {
            continue;
        };
        if payload.len() >= 6
            && payload[0] == 0
            && payload[1] == 0
            && payload[2] == 1
            && (0xE0..=0xEF).contains(&payload[3])
        {
            if let Some(pts) = parse_pes_pts(payload) {
                current_pts = Some(pts);
            }
            pes_buf.clear();
            pes_buf.extend_from_slice(payload);
        } else if !pes_buf.is_empty() {
            pes_buf.extend_from_slice(payload);
        }
        if current_pts.is_some() && ts_payload_has_idr(&pes_buf, hevc) {
            return current_pts.map(|pts| pts as f64 / 90_000.0);
        }
    }
    None
}

fn parse_pes_pts(pes: &[u8]) -> Option<u64> {
    if pes.len() < 14 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
        return None;
    }
    if pes[7] & 0x80 == 0 {
        return None;
    }
    let pts = &pes[9..14];
    Some(parse_pts5(pts))
}

fn parse_pts5(b: &[u8]) -> u64 {
    if b.len() < 5 {
        return 0;
    }
    ((u64::from(b[0] >> 1) & 0x07) << 30)
        | (u64::from(b[1]) << 22)
        | ((u64::from(b[2] >> 1) & 0x7f) << 15)
        | (u64::from(b[3]) << 7)
        | (u64::from(b[4] >> 1) & 0x7f)
}

fn ts_payload_has_idr(pes: &[u8], hevc: bool) -> bool {
    let start = if pes.len() > 9 {
        9 + (pes[8] as usize)
    } else {
        9
    };
    if start >= pes.len() {
        return false;
    }
    let video = &pes[start..];
    for nal in split_annexb(video) {
        if nal.is_empty() {
            continue;
        }
        if hevc {
            if is_h265_idr_nal(nal[0]) {
                return true;
            }
        } else if is_h264_idr_nal(nal[0]) {
            return true;
        }
    }
    false
}

fn walk_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    visit: &mut dyn FnMut(&[u8; 4], &[u8]) -> bool,
) {
    const MAX_BOX_BYTES: usize = 16 * 1024 * 1024;
    let mut off = start;
    while off + 8 <= end && off + 8 <= data.len() {
        let size32 = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let mut name = [0u8; 4];
        name.copy_from_slice(&data[off + 4..off + 8]);
        let (header, box_size) = if size32 == 1 {
            if off + 16 > data.len() {
                break;
            }
            let size64 = u64::from_be_bytes([
                data[off + 8],
                data[off + 9],
                data[off + 10],
                data[off + 11],
                data[off + 12],
                data[off + 13],
                data[off + 14],
                data[off + 15],
            ]);
            (16usize, size64 as usize)
        } else if size32 == 0 {
            (8usize, end.saturating_sub(off))
        } else {
            (8usize, size32 as usize)
        };
        if box_size > MAX_BOX_BYTES {
            break;
        }
        if box_size < header || off + box_size > data.len() || off + box_size > end {
            break;
        }
        let payload = &data[off + header..off + box_size];
        let descend = visit(&name, payload);
        if descend
            && matches!(
                &name,
                b"moov"
                    | b"trak"
                    | b"mdia"
                    | b"minf"
                    | b"stbl"
                    | b"stsd"
                    | b"moof"
                    | b"traf"
                    | b"mdat"
                    | b"edts"
                    | b"mvex"
            )
        {
            let nest_off = if &name == b"stsd" && payload.len() >= 8 {
                8
            } else {
                0
            };
            walk_boxes(data, off + header + nest_off, off + box_size, visit);
        } else if descend && matches!(&name, b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"mp4a") {
            let skip = if &name == b"mp4a" {
                28
            } else {
                78.min(payload.len())
            };
            if skip < payload.len() {
                walk_boxes(data, off + header + skip, off + box_size, visit);
            }
        }
        off += box_size;
    }
}

fn find_child_box<'a>(payload: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    let mut found = None;
    let start = if payload.len() > 86 { 78 } else { 0 };
    let slice = &payload[start.min(payload.len())..];
    let mut off = 0usize;
    while off + 8 <= slice.len() {
        let size = u32::from_be_bytes([slice[off], slice[off + 1], slice[off + 2], slice[off + 3]])
            as usize;
        if size < 8 || off + size > slice.len() {
            break;
        }
        if &slice[off + 4..off + 8] == want {
            found = Some(&slice[off + 8..off + size]);
            break;
        }
        off += size;
    }
    found
}

fn parse_avcc(avcc: &[u8], info: &mut WireProbeInfo) {
    if avcc.len() < 7 {
        return;
    }
    let profile = avcc[1];
    let level = avcc[3];
    info.profile_level = Some(format!("avc1.{profile:02x}{level:02x}"));
    if info.codec.is_none() {
        info.codec = Some("avc1".into());
    }
    let num_sps = (avcc[5] & 0x1f) as usize;
    let mut off = 6usize;
    for _ in 0..num_sps {
        if off + 2 > avcc.len() {
            break;
        }
        let len = u16::from_be_bytes([avcc[off], avcc[off + 1]]) as usize;
        off += 2;
        if off + len > avcc.len() {
            break;
        }
        apply_h264_sps(&avcc[off..off + len], info);
        off += len;
    }
}

fn parse_hvcc(hvcc: &[u8], info: &mut WireProbeInfo) {
    if hvcc.len() < 23 {
        return;
    }
    let general_profile = hvcc[1];
    let general_level = hvcc[12];
    info.profile_level = Some(format!("hvc1.{general_profile}.{general_level}"));
    if info.codec.is_none() {
        info.codec = Some("hvc1".into());
    }
    let mut off = 22usize;
    if off >= hvcc.len() {
        return;
    }
    let num_arrays = hvcc[off] as usize;
    off += 1;
    for _ in 0..num_arrays {
        if off + 3 > hvcc.len() {
            break;
        }
        let nal_type = hvcc[off] & 0x3f;
        let num_nalus = u16::from_be_bytes([hvcc[off + 1], hvcc[off + 2]]) as usize;
        off += 3;
        for _ in 0..num_nalus {
            if off + 2 > hvcc.len() {
                return;
            }
            let len = u16::from_be_bytes([hvcc[off], hvcc[off + 1]]) as usize;
            off += 2;
            if off + len > hvcc.len() {
                return;
            }
            if nal_type == 33 {
                apply_h265_sps(&hvcc[off..off + len], info);
            }
            off += len;
        }
    }
}

fn trun_first_sample_sync(trun: &[u8]) -> Option<bool> {
    if trun.len() < 8 {
        return None;
    }
    let version = trun[0];
    let flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 4;
    }
    if flags & 0x000004 != 0 {
        if off + 4 > trun.len() {
            return None;
        }
        let sample_flags =
            u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
        let non_sync = (sample_flags >> 16) & 1 == 1;
        return Some(!non_sync);
    }
    let _ = version;
    if flags & 0x000400 != 0 {
        let sample_count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]) as usize;
        if sample_count == 0 {
            return None;
        }
        let mut so = off;
        if flags & 0x000100 != 0 {
            so += 4;
        }
        if flags & 0x000200 != 0 {
            so += 4;
        }
        if flags & 0x000400 != 0 {
            if so + 4 > trun.len() {
                return None;
            }
            let sample_flags =
                u32::from_be_bytes([trun[so], trun[so + 1], trun[so + 2], trun[so + 3]]);
            let non_sync = (sample_flags >> 16) & 1 == 1;
            return Some(!non_sync);
        }
    }
    None
}

fn probe_mpeg_ts(bytes: &[u8]) -> WireProbeInfo {
    let mut info = WireProbeInfo::default();
    let packets: Vec<&[u8]> = bytes
        .chunks(188)
        .filter(|p| p.len() == 188 && p[0] == 0x47)
        .collect();
    if packets.is_empty() {
        return info;
    }

    let mut pmt_pid: Option<u16> = None;
    let mut video_pid: Option<u16> = None;
    let mut audio_pid: Option<u16> = None;
    let mut audio_stream_type: Option<u8> = None;
    let mut video_stream_type: Option<u8> = None;

    for pkt in &packets {
        let pid = packet_pid(pkt);
        if pid == 0 {
            if let Some(pmt) = parse_pat_pmt_pid(pkt) {
                pmt_pid = Some(pmt);
            }
        }
    }

    if let Some(pmt) = pmt_pid {
        for pkt in &packets {
            if packet_pid(pkt) == pmt {
                if let Some((vpid, st, apid, ast)) = parse_pmt(pkt) {
                    video_pid = Some(vpid);
                    video_stream_type = Some(st);
                    audio_pid = apid;
                    audio_stream_type = ast;
                    break;
                }
            }
        }
    }

    let mut video_payload = Vec::new();
    let mut audio_payload = Vec::new();
    for pkt in &packets {
        let pid = packet_pid(pkt);
        if video_pid.map(|v| v == pid).unwrap_or(false)
            || (video_pid.is_none() && has_pes_start(pkt))
        {
            if let Some(payload) = ts_payload(pkt) {
                if payload
                    .windows(4)
                    .any(|w| w == [0, 0, 1, 0xe0] || w == [0, 0, 0, 1])
                    || video_pid.is_some()
                {
                    video_payload.extend_from_slice(payload);
                }
            }
        }
        if audio_pid.map(|a| a == pid).unwrap_or(false) {
            if let Some(payload) = ts_payload(pkt) {
                audio_payload.extend_from_slice(payload);
            }
        }
    }

    if video_payload.is_empty() {
        for pkt in &packets {
            if let Some(payload) = ts_payload(pkt) {
                video_payload.extend_from_slice(payload);
            }
        }
    }

    match video_stream_type {
        Some(0x1b) | None => {
            if info.codec.is_none() {
                info.codec = Some("avc1".into());
            }
            extract_h264_from_annexb(&video_payload, &mut info);
        }
        Some(0x24) => {
            info.codec = Some("hvc1".into());
            extract_h265_from_annexb(&video_payload, &mut info);
        }
        _ => {
            extract_h264_from_annexb(&video_payload, &mut info);
            if info.width.is_none() {
                extract_h265_from_annexb(&video_payload, &mut info);
            }
        }
    }

    if let Some(st) = audio_stream_type {
        if info.audio_codec.is_none() {
            info.audio_codec = ts_audio_codec(st).map(str::to_string);
        }
    }

    if !audio_payload.is_empty() {
        parse_adts_header(&audio_payload, &mut info);
    } else {
        parse_adts_header(&video_payload, &mut info);
    }

    let codec_hint = info.codec.clone();
    apply_gop_scan(&video_payload, codec_hint.as_deref(), &mut info);
    info.timing = probe_wire_timing(bytes, ContainerKind::Ts);
    info
}

fn packet_pid(pkt: &[u8]) -> u16 {
    (((pkt[1] as u16) & 0x1f) << 8) | pkt[2] as u16
}

fn has_pes_start(pkt: &[u8]) -> bool {
    pkt[1] & 0x40 != 0
}

fn ts_payload(pkt: &[u8]) -> Option<&[u8]> {
    let adaptation = (pkt[3] >> 4) & 0x3;
    let mut off = 4usize;
    if adaptation == 2 || adaptation == 3 {
        let len = pkt[4] as usize;
        off = 5 + len;
    }
    if adaptation == 0 || off >= 188 {
        return None;
    }
    Some(&pkt[off..188])
}

fn parse_pat_pmt_pid(pkt: &[u8]) -> Option<u16> {
    let payload = ts_payload(pkt)?;
    let pointer = payload.first().copied()? as usize;
    let section = payload.get(1 + pointer..)?;
    if section.len() < 12 || section[0] != 0x00 {
        return None;
    }
    let mut off = 8usize;
    while off + 4 <= section.len() {
        let program = u16::from_be_bytes([section[off], section[off + 1]]);
        let pid = (((section[off + 2] as u16) & 0x1f) << 8) | section[off + 3] as u16;
        if program != 0 {
            return Some(pid);
        }
        off += 4;
    }
    None
}

fn parse_pmt(pkt: &[u8]) -> Option<(u16, u8, Option<u16>, Option<u8>)> {
    let payload = ts_payload(pkt)?;
    let pointer = payload.first().copied()? as usize;
    let section = payload.get(1 + pointer..)?;
    if section.len() < 12 || section[0] != 0x02 {
        return None;
    }
    let section_len = (((section[1] as usize) & 0x0f) << 8) | section[2] as usize;
    let program_info_len = (((section[10] as usize) & 0x0f) << 8) | section[11] as usize;
    let mut off = 12 + program_info_len;
    let end = (3 + section_len).min(section.len()).saturating_sub(4);
    let mut video = None;
    let mut audio = None;
    let mut audio_st = None;
    while off + 5 <= end {
        let stream_type = section[off];
        let es_pid = (((section[off + 1] as u16) & 0x1f) << 8) | section[off + 2] as u16;
        let es_info_len = (((section[off + 3] as usize) & 0x0f) << 8) | section[off + 4] as usize;
        match stream_type {
            0x1b | 0x24 | 0xea if video.is_none() => {
                video = Some((es_pid, stream_type));
            }
            0x0f | 0x03 | 0x04 | 0x11 | 0x81 | 0x87 if audio.is_none() => {
                audio = Some(es_pid);
                audio_st = Some(stream_type);
            }
            _ => {}
        }
        off += 5 + es_info_len;
    }
    let (vpid, st) = video?;
    Some((vpid, st, audio, audio_st))
}

fn extract_h264_from_annexb(data: &[u8], info: &mut WireProbeInfo) {
    for nal in split_annexb(data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1f;
        if nal_type == 7 {
            apply_h264_sps(nal, info);
        }
    }
}

fn extract_h265_from_annexb(data: &[u8], info: &mut WireProbeInfo) {
    for nal in split_annexb(data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = (nal[0] >> 1) & 0x3f;
        if nal_type == 33 {
            apply_h265_sps(nal, info);
        }
    }
}

fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        let start = if data[i..].starts_with(&[0, 0, 0, 1]) {
            i + 4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            i + 3
        } else {
            i += 1;
            continue;
        };
        let mut end = start;
        while end + 3 < data.len() {
            if data[end..].starts_with(&[0, 0, 0, 1]) || data[end..].starts_with(&[0, 0, 1]) {
                break;
            }
            end += 1;
        }
        if end + 3 >= data.len() {
            end = data.len();
        }
        if end > start {
            nals.push(&data[start..end]);
        }
        i = end;
    }
    nals
}

fn parse_adts_header(data: &[u8], info: &mut WireProbeInfo) {
    if data.len() < 7 {
        return;
    }
    for i in 0..=data.len() - 7 {
        if data[i] == 0xff && data[i + 1] & 0xf0 == 0xf0 {
            let profile = (data[i + 2] >> 6) & 0x03;
            if info.audio_codec.is_none() {
                info.audio_codec = Some(match profile {
                    0 => "aac-lc".into(),
                    1 => "aac-main".into(),
                    2 => "aac-ssr".into(),
                    _ => "aac".into(),
                });
            }
            let sr_idx = ((data[i + 2] & 0x3c) >> 2) as usize;
            let rates = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];
            let channels = ((data[i + 2] & 1) << 2) | ((data[i + 3] & 0xc0) >> 6);
            if sr_idx < rates.len() {
                info.audio_sample_rate = Some(rates[sr_idx]);
            }
            if channels > 0 {
                info.audio_channels = Some(channels);
            }
            break;
        }
    }
}

fn ts_audio_codec(stream_type: u8) -> Option<&'static str> {
    match stream_type {
        0x03 => Some("mp3"),
        0x04 | 0x81 => Some("ac-3"),
        0x0f => Some("aac-adts"),
        0x11 | 0x87 => Some("aac-lc"),
        _ => None,
    }
}

fn parse_esds(payload: &[u8], info: &mut WireProbeInfo) {
    if payload.len() < 5 {
        return;
    }
    let data = &payload[4..];
    if let Some(oti) = esds_object_type(data) {
        info.audio_codec = Some(audio_object_type_name(oti));
    } else if info.audio_codec.is_none() {
        info.audio_codec = Some("aac".into());
    }
}

fn esds_object_type(data: &[u8]) -> Option<u8> {
    let mut off = 0usize;
    while off + 2 <= data.len() {
        let tag = data[off];
        off += 1;
        let (len, advance) = mp4_descriptor_len(data, off)?;
        off += advance;
        let end = off + len;
        if end > data.len() {
            return None;
        }
        if tag == 0x03 && len >= 1 {
            return Some(data[off]);
        }
        off = end;
    }
    None
}

fn mp4_descriptor_len(data: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut len = 0usize;
    let mut advance = 0usize;
    for i in 0..4 {
        let idx = start + i;
        if idx >= data.len() {
            return None;
        }
        let b = data[idx];
        len = (len << 7) | usize::from(b & 0x7f);
        advance += 1;
        if b & 0x80 == 0 {
            return Some((len, advance));
        }
    }
    None
}

fn audio_object_type_name(oti: u8) -> String {
    match oti {
        0x40 | 0x66 | 0x67 => "aac-lc".into(),
        0x6a..=0x6c => "aac".into(),
        0x69 => "aac-he".into(),
        0x6d | 0x6e => "mp3".into(),
        0xdd => "ac-3".into(),
        other => format!("0x{other:02x}"),
    }
}

fn apply_gop_scan(data: &[u8], codec: Option<&str>, info: &mut WireProbeInfo) {
    let hevc = codec.is_some_and(|c| c.starts_with("hvc") || c.starts_with("hev"));
    let stats = scan_keyframes(data, hevc);
    if stats.count > 0 {
        info.keyframe_count = Some(stats.count);
    }
    if info.sync_sample.is_none() {
        info.sync_sample = stats.first_is_keyframe;
    }
}

struct KeyframeStats {
    count: u32,
    first_is_keyframe: Option<bool>,
}

fn scan_keyframes(data: &[u8], hevc: bool) -> KeyframeStats {
    let annex_nals: Vec<&[u8]> = split_annexb(data);
    if !annex_nals.is_empty() {
        return scan_nal_list(&annex_nals, hevc);
    }
    let mut nals = Vec::new();
    let mut off = 0usize;
    while off + 4 <= data.len() {
        let len =
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        if len == 0 || off + 4 + len > data.len() {
            break;
        }
        nals.push(&data[off + 4..off + 4 + len]);
        off += 4 + len;
    }
    scan_nal_list(&nals, hevc)
}

fn scan_nal_list(nals: &[&[u8]], hevc: bool) -> KeyframeStats {
    let mut count = 0u32;
    let mut first_video: Option<bool> = None;
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let key = if hevc {
            is_h265_idr_nal(nal[0])
        } else {
            is_h264_idr_nal(nal[0])
        };
        if key {
            count += 1;
            if first_video.is_none() {
                first_video = Some(true);
            }
        } else if is_video_nal(nal[0], hevc) && first_video.is_none() {
            first_video = Some(false);
        }
    }
    KeyframeStats {
        count,
        first_is_keyframe: first_video,
    }
}

fn is_h264_idr_nal(header: u8) -> bool {
    (header & 0x1f) == 5
}

fn is_h265_idr_nal(header: u8) -> bool {
    matches!((header >> 1) & 0x3f, 19 | 20)
}

fn is_video_nal(header: u8, hevc: bool) -> bool {
    if hevc {
        let t = (header >> 1) & 0x3f;
        (0..32).contains(&t)
    } else {
        let t = header & 0x1f;
        matches!(t, 1..=5)
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.bit / 8 >= self.data.len() {
            return None;
        }
        let b = self.data[self.bit / 8];
        let v = ((b >> (7 - (self.bit % 8))) & 1) as u32;
        self.bit += 1;
        Some(v)
    }

    fn read_bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Some(v)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut zeros = 0usize;
        while self.read_bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let info = self.read_bits(zeros).unwrap_or(0);
        Some(((1u32 << zeros) - 1).saturating_add(info))
    }

    fn read_se(&mut self) -> Option<i32> {
        let v = self.read_ue()? as i32;
        Some(if v & 1 != 0 { (v + 1) / 2 } else { -(v / 2) })
    }
}

/// Remove emulation prevention bytes (0x000003 → 0x0000).
fn rbsp_from_nal(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut i = 0usize;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

fn apply_h264_sps(nal: &[u8], info: &mut WireProbeInfo) {
    let body = if !nal.is_empty() && nal[0] & 0x1f == 7 {
        &nal[1..]
    } else {
        nal
    };
    let rbsp = rbsp_from_nal(body);
    let mut br = BitReader::new(&rbsp);
    let profile_idc = match br.read_bits(8) {
        Some(v) => v as u8,
        None => return,
    };
    let _constraint = br.read_bits(8);
    let level_idc = match br.read_bits(8) {
        Some(v) => v as u8,
        None => return,
    };
    info.profile_level = Some(format!("avc1.{profile_idc:02x}{level_idc:02x}"));
    if info.codec.is_none() {
        info.codec = Some("avc1".into());
    }
    let _sps_id = br.read_ue();
    let mut chroma_format_idc = 1u32;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    ) {
        chroma_format_idc = br.read_ue().unwrap_or(1);
        if chroma_format_idc == 3 {
            let _ = br.read_bit();
        }
        let _ = br.read_ue();
        let _ = br.read_ue();
        let _ = br.read_bit();
        if br.read_bit().unwrap_or(0) == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..count {
                if br.read_bit().unwrap_or(0) == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let mut next = 8i32;
                    for _ in 0..size {
                        let delta = br.read_se().unwrap_or(0);
                        next = (next + delta + 256) % 256;
                    }
                }
            }
        }
    }
    let _log2_max_frame_num = br.read_ue();
    let poc_type = br.read_ue().unwrap_or(0);
    if poc_type == 0 {
        let _ = br.read_ue();
    } else if poc_type == 1 {
        let _ = br.read_bit();
        let _ = br.read_se();
        let _ = br.read_se();
        let n = br.read_ue().unwrap_or(0) as usize;
        for _ in 0..n {
            let _ = br.read_se();
        }
    }
    let _ = br.read_ue();
    let _ = br.read_bit();
    let w_mbs = br.read_ue().unwrap_or(0);
    let h_map = br.read_ue().unwrap_or(0);
    let frame_mbs_only = br.read_bit().unwrap_or(1);
    if frame_mbs_only == 0 {
        let _ = br.read_bit();
    }
    let _ = br.read_bit();
    let mut crop_l = 0u32;
    let mut crop_r = 0u32;
    let mut crop_t = 0u32;
    let mut crop_b = 0u32;
    if br.read_bit().unwrap_or(0) == 1 {
        crop_l = br.read_ue().unwrap_or(0);
        crop_r = br.read_ue().unwrap_or(0);
        crop_t = br.read_ue().unwrap_or(0);
        crop_b = br.read_ue().unwrap_or(0);
    }
    let width = ((w_mbs + 1) * 16).saturating_sub((crop_l + crop_r) * 2);
    let height = ((2 - frame_mbs_only) * (h_map + 1) * 16).saturating_sub((crop_t + crop_b) * 2);
    if width > 0 && height > 0 {
        info.width = Some(width);
        info.height = Some(height);
    }
    if br.read_bit().unwrap_or(0) == 1 {
        if br.read_bit().unwrap_or(0) == 1 {
            let aspect = br.read_bits(8).unwrap_or(0);
            if aspect == 255 {
                let _ = br.read_bits(16);
                let _ = br.read_bits(16);
            }
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_bit();
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_bits(3);
            let _ = br.read_bit();
            if br.read_bit().unwrap_or(0) == 1 {
                let _ = br.read_bits(8);
                let _ = br.read_bits(8);
                let _ = br.read_bits(8);
            }
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_ue();
            let _ = br.read_ue();
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let num_units = br.read_bits(32).unwrap_or(0);
            let time_scale = br.read_bits(32).unwrap_or(0);
            let fixed = br.read_bit().unwrap_or(0);
            if num_units > 0 && time_scale > 0 {
                let mut fps = time_scale as f64 / (2.0 * num_units as f64);
                if fixed == 0 {
                    fps = time_scale as f64 / num_units as f64;
                }
                if fps > 0.0 && fps < 120.0 {
                    info.frame_rate = Some(fps);
                }
            }
        }
    }
    let _ = chroma_format_idc;
}

fn apply_h265_sps(nal: &[u8], info: &mut WireProbeInfo) {
    if nal.len() <= 2 {
        return;
    }
    let body = &nal[2..];
    let rbsp = rbsp_from_nal(body);
    let mut br = BitReader::new(&rbsp);
    let _vps_id = br.read_bits(4);
    let max_sub_layers = br.read_bits(3).unwrap_or(0);
    let _ = br.read_bit();
    let _ = br.read_bits(2);
    let _ = br.read_bit();
    let profile = br.read_bits(5).unwrap_or(0);
    let _ = br.read_bits(32);
    let _ = br.read_bits(48);
    let level = br.read_bits(8).unwrap_or(0);
    info.profile_level = Some(format!("hvc1.{profile}.{level}"));
    if info.codec.is_none() {
        info.codec = Some("hvc1".into());
    }
    for _ in 0..max_sub_layers {
        let _ = br.read_bit();
        let _ = br.read_bit();
    }
    let _sps_id = br.read_ue();
    let chroma = br.read_ue().unwrap_or(1);
    if chroma == 3 {
        let _ = br.read_bit();
    }
    let width = br.read_ue().unwrap_or(0);
    let height = br.read_ue().unwrap_or(0);
    if width > 0 && height > 0 && width < 8192 && height < 8192 {
        info.width = Some(width);
        info.height = Some(height);
    }
}

/// Compare manifest ABR fields against wire probe; returns human-readable mismatch lines.
pub fn manifest_wire_mismatches(
    manifest_res: Option<&str>,
    manifest_fps: Option<f64>,
    manifest_codecs: Option<&str>,
    wire: &WireProbeInfo,
) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(mres), Some(w), Some(h)) = (manifest_res, wire.width, wire.height) {
        let wire_res = format!("{w}x{h}");
        let norm = |s: &str| s.to_ascii_lowercase().replace('×', "x");
        if norm(mres) != norm(&wire_res) && (mres.contains('x') || mres.contains('×')) {
            out.push(format!(
                "[MISMATCH] Manifest declares {mres}, wire stream is {wire_res}"
            ));
        }
    }
    if let (Some(mfps), Some(wfps)) = (manifest_fps, wire.frame_rate) {
        if (mfps - wfps).abs() > 0.5 {
            out.push(format!(
                "[MISMATCH] Manifest declares {mfps:.2} FPS, wire stream is {wfps:.2} FPS"
            ));
        }
    }
    if let (Some(mc), Some(wc)) = (manifest_codecs, wire.codec.as_deref()) {
        let m = mc.to_ascii_lowercase();
        if !m.contains(wc) && !wc.contains(m.split('.').next().unwrap_or("")) {
            let m_avc = m.contains("avc") || m.contains("h264");
            let m_hevc = m.contains("hvc") || m.contains("hev") || m.contains("h265");
            let w_avc = wc.contains("avc");
            let w_hevc = wc.contains("hvc") || wc.contains("hev");
            if (m_avc && w_hevc) || (m_hevc && w_avc) {
                out.push(format!(
                    "[MISMATCH] Manifest declares codec {mc}, wire stream is {wc}"
                ));
            }
        }
    }
    out
}

/// Fill missing ABR display fields from wire; returns whether any field was wire-sourced.
pub fn fill_abr_from_wire(
    resolution: &mut Option<String>,
    frame_rate: &mut Option<f64>,
    codecs: &mut Option<String>,
    wire: &WireProbeInfo,
) -> bool {
    let mut filled = false;
    if resolution.is_none() {
        if let (Some(w), Some(h)) = (wire.width, wire.height) {
            *resolution = Some(format!("{w}x{h}"));
            filled = true;
        }
    }
    if frame_rate.is_none() {
        if let Some(fps) = wire.frame_rate {
            *frame_rate = Some(fps);
            filled = true;
        }
    }
    if codecs.is_none() {
        if let Some(c) = wire.profile_level.as_ref().or(wire.codec.as_ref()) {
            *codecs = Some(c.clone());
            filled = true;
        }
    }
    filled
}

/// Extract ISO-BMFF / MPEG-TS timing signals from the probe buffer.
pub fn probe_wire_timing(bytes: &[u8], kind: ContainerKind) -> WireTimingInfo {
    match kind {
        ContainerKind::Fmp4 => probe_fmp4_timing(bytes),
        ContainerKind::Ts => probe_ts_timing(bytes),
        ContainerKind::Unknown => WireTimingInfo::default(),
    }
}

fn probe_fmp4_timing(bytes: &[u8]) -> WireTimingInfo {
    let mut timing = WireTimingInfo::default();
    let mut moof_timescale = 90000u32;
    let mut default_duration = 0u32;
    let mut default_flags = 0u32;
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        match name {
            b"sidx" => apply_sidx(payload, &mut timing),
            b"mdhd" if payload.len() >= 20 => {
                moof_timescale =
                    u32::from_be_bytes([payload[16], payload[17], payload[18], payload[19]]);
            }
            b"tfdt" => {
                timing.moof_base_decode_time = parse_tfdt(payload);
                timing.moof_timescale = Some(moof_timescale);
            }
            b"tfhd" => {
                let (dur, flags) = parse_tfhd(payload);
                if dur > 0 {
                    default_duration = dur;
                }
                default_flags = flags;
            }
            b"trun" => {
                let (count, total) = parse_trun_timeline(payload, default_duration, default_flags);
                timing.trun_sample_count = Some(count);
                timing.trun_total_duration_ticks = Some(total);
                if timing.wire_duration_sec.is_none() && moof_timescale > 0 && total > 0 {
                    timing.wire_duration_sec =
                        Some(total as f64 / f64::from(moof_timescale.max(1)));
                }
            }
            b"prft" => apply_prft(payload, &mut timing),
            _ => {}
        }
        true
    });
    if timing.moof_timescale.is_none() && moof_timescale > 0 {
        timing.moof_timescale = Some(moof_timescale);
    }
    timing
}

fn apply_sidx(payload: &[u8], timing: &mut WireTimingInfo) {
    if payload.len() < 24 {
        return;
    }
    let version = payload[0];
    timing.sidx_timescale = Some(u32::from_be_bytes([
        payload[8],
        payload[9],
        payload[10],
        payload[11],
    ]));
    let (earliest, ref_count_off) = if version == 1 {
        if payload.len() < 32 {
            return;
        }
        (
            u64::from_be_bytes([
                payload[12],
                payload[13],
                payload[14],
                payload[15],
                payload[16],
                payload[17],
                payload[18],
                payload[19],
            ]),
            30usize,
        )
    } else {
        (
            u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]) as u64,
            22usize,
        )
    };
    timing.sidx_earliest_presentation_time = Some(earliest);
    if payload.len() < ref_count_off + 2 {
        return;
    }
    let ref_count = u16::from_be_bytes([payload[ref_count_off], payload[ref_count_off + 1]]) as u32;
    timing.sidx_reference_count = Some(ref_count);
    let first_ref = ref_count_off + 2;
    if ref_count > 0 && payload.len() >= first_ref + 12 {
        timing.sidx_first_subsegment_duration_ticks = Some(u32::from_be_bytes([
            payload[first_ref + 8],
            payload[first_ref + 9],
            payload[first_ref + 10],
            payload[first_ref + 11],
        ]));
    }
}

fn parse_trun_timeline(trun: &[u8], default_duration: u32, default_flags: u32) -> (u32, u64) {
    if trun.len() < 8 {
        return (0, 0);
    }
    let flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let sample_count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]);
    let has_duration = flags & 0x000100 != 0;
    let has_sample_flags = flags & 0x000400 != 0;
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 4;
    }
    let mut first_flags = default_flags;
    if flags & 0x000004 != 0 && off + 4 <= trun.len() {
        first_flags = u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
        off += 4;
    }
    let mut total = 0u64;
    for _ in 0..sample_count {
        let mut duration = default_duration;
        if has_duration {
            if off + 4 > trun.len() {
                break;
            }
            duration = u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
            off += 4;
        }
        if flags & 0x000200 != 0 {
            off = off.saturating_add(4);
        }
        if has_sample_flags {
            off = off.saturating_add(4);
        }
        total = total.saturating_add(u64::from(duration));
        let _ = first_flags;
    }
    (sample_count, total)
}

fn probe_ts_timing(bytes: &[u8]) -> WireTimingInfo {
    let mut timing = WireTimingInfo::default();
    let packets: Vec<&[u8]> = bytes
        .chunks(188)
        .filter(|p| p.len() == 188 && p[0] == 0x47)
        .collect();
    if packets.is_empty() {
        return timing;
    }
    timing.ts_continuity_errors = Some(ts_continuity_errors(&packets));
    if let Some(drift) = ts_pcr_pts_drift_ms(&packets) {
        timing.pcr_pts_drift_ms = Some(drift);
    }
    timing
}

fn ts_continuity_errors(packets: &[&[u8]]) -> u32 {
    let mut last_cc: HashMap<u16, u8> = HashMap::new();
    let mut errors = 0u32;
    for pkt in packets {
        let adaptation = (pkt[3] >> 4) & 0x3;
        if adaptation == 0 || adaptation == 2 {
            continue;
        }
        let pid = packet_pid(pkt);
        let cc = pkt[3] & 0x0f;
        if let Some(prev) = last_cc.get(&pid) {
            let expected = (*prev + 1) & 0x0f;
            if cc != *prev && cc != expected {
                errors = errors.saturating_add(1);
            }
        }
        last_cc.insert(pid, cc);
    }
    errors
}

fn ts_pcr_pts_drift_ms(packets: &[&[u8]]) -> Option<f64> {
    let mut pcr_ticks: Option<u64> = None;
    let mut pts_ticks: Option<u64> = None;
    for pkt in packets {
        if let Some(pcr) = parse_ts_pcr(pkt) {
            pcr_ticks = Some(pcr);
        }
        if let Some(payload) = ts_payload(pkt) {
            if payload.len() >= 14
                && payload[0] == 0
                && payload[1] == 0
                && payload[2] == 1
                && (0xE0..=0xEF).contains(&payload[3])
            {
                if let Some(pts) = parse_pes_pts(payload) {
                    pts_ticks = Some(pts);
                }
            }
        }
        if pcr_ticks.is_some() && pts_ticks.is_some() {
            break;
        }
    }
    let pcr = pcr_ticks?;
    let pts = pts_ticks?;
    let drift_ticks = pts.abs_diff(pcr);
    Some(drift_ticks as f64 / 90.0)
}

fn parse_ts_pcr(pkt: &[u8]) -> Option<u64> {
    let adaptation = (pkt[3] >> 4) & 0x3;
    if adaptation != 2 && adaptation != 3 {
        return None;
    }
    let afl = *pkt.get(4)? as usize;
    if afl < 7 || pkt.len() < 5 + afl {
        return None;
    }
    let flags = pkt[5];
    if flags & 0x10 == 0 {
        return None;
    }
    let base = u64::from(pkt[6]) << 25
        | u64::from(pkt[7]) << 17
        | u64::from(pkt[8]) << 9
        | u64::from(pkt[9]) << 1
        | u64::from(pkt[10] >> 7);
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_emsg_v0_scte_scheme() {
        let mut payload = vec![0u8; 12];
        payload.extend_from_slice(b"urn:scte:scte35:2014:bin\0");
        payload.extend_from_slice(b"1\0");
        payload.extend_from_slice(&90000u32.to_be_bytes());
        payload.extend_from_slice(&100u32.to_be_bytes());
        payload.extend_from_slice(&30000u32.to_be_bytes());
        payload.extend_from_slice(&7u32.to_be_bytes());
        payload.extend_from_slice(&[0xde, 0xad]);
        let ev = parse_emsg_box(&payload).expect("emsg v0");
        assert_eq!(ev.version, 0);
        assert!(ev.scheme_id_uri.contains("scte35"));
        assert_eq!(ev.id, 7);
        assert_eq!(ev.message_data, vec![0xde, 0xad]);
        assert!(ev.is_scte_related());
    }

    #[test]
    fn scan_inband_emsg_finds_box() {
        let mut payload = vec![0u8; 12];
        payload.extend_from_slice(b"urn:scte:scte35:2013:xml\0");
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        let box_len = (8 + payload.len()) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&box_len.to_be_bytes());
        bytes.extend_from_slice(b"emsg");
        bytes.extend_from_slice(&payload);
        let found = scan_inband_emsg(&bytes);
        assert_eq!(found.len(), 1);
        assert!(found[0].scheme_id_uri.contains("2013:xml"));
    }

    #[test]
    fn classify_ts_sync() {
        let mut buf = vec![0u8; 188];
        buf[0] = 0x47;
        assert_eq!(classify_container(&buf), ContainerKind::Ts);
    }

    #[test]
    fn classify_fmp4_ftyp() {
        let mut buf = vec![0u8; 32];
        buf[4..8].copy_from_slice(b"ftyp");
        assert_eq!(classify_container(&buf), ContainerKind::Fmp4);
    }

    #[test]
    fn mismatch_resolution() {
        let wire = WireProbeInfo {
            width: Some(1280),
            height: Some(720),
            ..Default::default()
        };
        let msgs = manifest_wire_mismatches(Some("1920x1080"), None, None, &wire);
        assert!(msgs[0].contains("MISMATCH"));
        assert!(msgs[0].contains("1280x720"));
    }

    #[test]
    fn fill_missing_fps() {
        let wire = WireProbeInfo {
            frame_rate: Some(25.0),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };
        let mut res = None;
        let mut fps = None;
        let mut codecs = None;
        assert!(fill_abr_from_wire(&mut res, &mut fps, &mut codecs, &wire));
        assert_eq!(res.as_deref(), Some("1920x1080"));
        assert_eq!(fps, Some(25.0));
    }

    #[test]
    fn adts_sample_rate() {
        let hdr = [0xff, 0xf1, 0x4c, 0x80, 0x01, 0xbf, 0xfc];
        let mut info = WireProbeInfo::default();
        parse_adts_header(&hdr, &mut info);
        assert_eq!(info.audio_sample_rate, Some(48000));
        assert_eq!(info.audio_codec.as_deref(), Some("aac-main"));
    }

    #[test]
    fn h264_idr_annexb_detected() {
        let mut data = vec![0, 0, 0, 1, 0x65, 0x88, 0x84];
        data.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x9a]);
        let mut info = WireProbeInfo::default();
        apply_gop_scan(&data, Some("avc1"), &mut info);
        assert_eq!(info.sync_sample, Some(true));
        assert_eq!(info.keyframe_count, Some(1));
    }

    #[test]
    fn h264_delta_annexb_detected() {
        let data = vec![0, 0, 0, 1, 0x41, 0x9a, 0x24];
        let mut info = WireProbeInfo::default();
        apply_gop_scan(&data, Some("avc1"), &mut info);
        assert_eq!(info.sync_sample, Some(false));
        assert_eq!(info.keyframe_count, None);
    }

    #[test]
    fn cross_segment_gop_interval_from_mock_pts() {
        use crate::engine::gop_tracker::GopCadenceTracker;
        let mut tracker = GopCadenceTracker::default();
        let mut wire = WireProbeInfo::default();
        for pts in [0.0, 2.0, 4.0, 6.0] {
            wire.keyframe_pts_sec = Some(pts);
            tracker.observe_keyframe(wire.keyframe_pts_sec);
            tracker.apply(&mut wire);
        }
        assert_eq!(wire.gop_duration_sec, Some(2.0));
        assert!(wire.is_fixed_cadence);
        assert_eq!(wire.gop_label().as_deref(), Some("2.00s (Fixed)"));
    }

    #[test]
    fn wire_probe_labels() {
        let wire = WireProbeInfo {
            sync_sample: Some(true),
            keyframe_count: Some(2),
            audio_codec: Some("aac-lc".into()),
            audio_sample_rate: Some(48000),
            audio_channels: Some(2),
            ..Default::default()
        };
        assert!(wire.gop_label().unwrap().contains("Keyframe"));
        assert!(wire.audio_label().unwrap().contains("aac-lc"));
        assert_eq!(wire.gop_badge(), Some("IDR"));
        assert!(wire.audio_badge().unwrap().contains("48k"));
    }

    #[test]
    fn prft_sets_glass_to_glass() {
        let ntp_unix = 1_700_000_000u64 + NTP_UNIX_EPOCH_OFFSET;
        let mut prft_payload = vec![0u8; 24];
        prft_payload[0] = 0;
        prft_payload[8..16].copy_from_slice(&ntp_unix.to_be_bytes());
        prft_payload[16..24].copy_from_slice(&90000u64.to_be_bytes());
        let mut buf = vec![0u8; 8 + prft_payload.len()];
        let total = buf.len() as u32;
        buf[0..4].copy_from_slice(&total.to_be_bytes());
        buf[4..8].copy_from_slice(b"prft");
        buf[8..].copy_from_slice(&prft_payload);
        let timing = probe_wire_timing(&buf, ContainerKind::Fmp4);
        assert!(timing.prft_ntp_unix_ms.is_some());
        assert!(timing.glass_to_glass_ms.is_some());
    }

    #[test]
    fn sidx_timing_parsed() {
        let mut payload = vec![0u8; 36];
        payload[0] = 0; // version
        payload[8..12].copy_from_slice(&90000u32.to_be_bytes());
        payload[12..16].copy_from_slice(&1000u32.to_be_bytes());
        payload[22..24].copy_from_slice(&1u16.to_be_bytes());
        payload[24..28].copy_from_slice(&1u32.to_be_bytes());
        payload[28..32].copy_from_slice(&4096u32.to_be_bytes());
        payload[32..36].copy_from_slice(&180000u32.to_be_bytes());
        let total_len = 8 + payload.len();
        let mut buf = vec![0u8; total_len];
        buf[0..4].copy_from_slice(&(total_len as u32).to_be_bytes());
        buf[4..8].copy_from_slice(b"sidx");
        buf[8..].copy_from_slice(&payload);
        let timing = probe_wire_timing(&buf, ContainerKind::Fmp4);
        assert_eq!(timing.sidx_timescale, Some(90000));
        assert_eq!(timing.sidx_earliest_presentation_time, Some(1000));
        assert_eq!(timing.sidx_reference_count, Some(1));
        assert_eq!(timing.sidx_first_subsegment_duration_ticks, Some(180000));
    }

    #[test]
    fn trun_total_duration_computed() {
        let mut trun = vec![0u8; 20];
        trun[1] = 0x00;
        trun[2] = 0x01;
        trun[3] = 0x01; // flags 0x000101
        trun[4..8].copy_from_slice(&2u32.to_be_bytes());
        trun[8..12].copy_from_slice(&0u32.to_be_bytes());
        trun[12..16].copy_from_slice(&45000u32.to_be_bytes());
        trun[16..20].copy_from_slice(&45000u32.to_be_bytes());
        let (count, total) = parse_trun_timeline(&trun, 0, 0);
        assert_eq!(count, 2);
        assert_eq!(total, 90000);
    }

    #[test]
    fn ts_continuity_error_detected() {
        let mut p1 = vec![0u8; 188];
        p1[0] = 0x47;
        p1[1] = 0x00;
        p1[2] = 0x10;
        p1[3] = 0x10; // payload only, cc=0
        let mut p2 = p1.clone();
        p2[3] = 0x13; // cc=3, expected 1
        let errors = ts_continuity_errors(&[&p1[..], &p2[..]]);
        assert!(errors >= 1);
    }
}
