#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = m3u8_rs::parse_playlist_res(text);
        let _ = streamtop::engine::linter::scan_ll_hls(text);
        for line in text.lines() {
            let _ = streamtop::engine::scte35::extract_payload_from_tag(line);
        }
    }
});
