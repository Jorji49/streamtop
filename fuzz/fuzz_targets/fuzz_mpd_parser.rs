#![no_main]

use libfuzzer_sys::fuzz_target;
use url::Url;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if text.contains("<MPD") || text.contains("<mpd") {
            if let Ok(base) = Url::parse("https://example.com/live.mpd") {
                let _ = streamtop::engine::dash::parse_dash_mpd(text, &base);
                let _ = streamtop::engine::dash::scan_ll_dash_xml(text);
            }
        }
    }
});
