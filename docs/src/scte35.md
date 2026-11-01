# SCTE-35 decoding

While monitoring, streamtop decodes SCTE-35 cues from HLS tags (`#EXT-OATCLS-SCTE35`, `#EXT-X-SCTE35`, `#EXT-X-DATERANGE` with `SCTE35-OUT`, `SCTE35-IN`, `SCTE35-CMD`, and `#EXT-X-CUE-OUT` / `CUE-IN`) and from DASH `emsg` boxes. They appear in the event log and in the summary JSON.

## streamtop scte35

To decode a single cue without a stream, pass it to `streamtop scte35`:

```bash
streamtop scte35 "/DAvAAAAAAAA///wFAVIAACPf+/+c2nALv4AUsz1AAAAAAAKAAhDVUVJAAABNWLbowo="
```

```text
table_id            0xfc
protocol_version    0
splice_command      Splice Insert (0x05)
splice_event_id     1207959695 (0x4800008f)
out_of_network      yes
pts_time            1936310318 (21514.559089s)
break_duration      60.293567s
break_auto_return   yes
```

Accepted input:

* Base64 (standard or URL-safe alphabet)
* Hex, with or without `0x`. A string made only of hex digits is always read as hex
* An HLS tag line: `#EXT-OATCLS-SCTE35:...`, `#EXT-X-SCTE35:CUE="..."`, or `#EXT-X-DATERANGE` with `SCTE35-OUT`, `SCTE35-IN`, or `SCTE35-CMD`
* `-` to read the payload from stdin

```bash
streamtop scte35 0xFC302F000000000000FFFFF014054800008F7FEFFE7369C02EFE0052CCF500000000000A0008435545490000013562DBA30A
grep -m1 OATCLS playlist.m3u8 | streamtop scte35 -
streamtop scte35 --json "/DA0AAAAAAAA///wBQb+cr0AUAAeAhxDVUVJSAAAjn/PAAGlmbAICAAAAAAsoKGKNAIAmsnRfg=="
```

The text output covers `splice_insert`, `time_signal`, `splice_schedule`, `splice_null`, and `bandwidth_reservation`, and each segmentation descriptor (event id, type, duration, UPID, segment numbers, delivery flags). `--json` prints the same fields as the summary JSON. The command exits 1 when the bytes are not a valid `splice_info_section`.

It does not verify the CRC, decrypt encrypted sections, or encode new cues. For encoding and injection, see [threefive](https://github.com/superkabuki/threefive) or TSDuck.
