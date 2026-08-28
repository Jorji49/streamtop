#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = streamtop::engine::container_probe::deep_wire_probe(data);
    let _ = streamtop::engine::linter::inspect_container(data);
});
