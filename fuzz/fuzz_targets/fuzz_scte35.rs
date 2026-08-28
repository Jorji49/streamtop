#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = streamtop::engine::scte35::parse_scte35_bytes(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = streamtop::engine::scte35::decode_scte35_payload(text);
        for line in text.lines() {
            let _ = streamtop::engine::scte35::parse_scte35_tag(line);
        }
    }
});
