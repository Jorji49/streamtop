#!/usr/bin/env python3
"""Hermetic HTTP/SRT/RTMP mock feeds for streamtop E2E (no paid services)."""

from __future__ import annotations

import http.server
import socket
import struct
import threading
import time
from pathlib import Path
from typing import Tuple

HTTP_PORT = 8765
SRT_PORT = 9000
RTMP_PORT = 1935
ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"

TS = 188
SYNC = 0x47
SRT_HS_MAGIC = 0x4A17


def ts_packet(pid: int, cc: int, payload: bytes, pcr_base: int | None = None) -> bytes:
    pkt = bytearray(TS)
    pkt[0] = SYNC
    pkt[1] = (pid >> 8) & 0x1F
    pkt[2] = pid & 0xFF
    if pcr_base is not None:
        pkt[3] = 0x30 | (cc & 0x0F)
        pkt[4] = 183
        pkt[5] = 0x10
        pkt[6] = (pcr_base >> 25) & 0xFF
        pkt[7] = (pcr_base >> 17) & 0xFF
        pkt[8] = (pcr_base >> 9) & 0xFF
        pkt[9] = (pcr_base >> 1) & 0xFF
        pkt[10] = ((pcr_base & 1) << 7) | 0x7E
        start = 13
    else:
        pkt[3] = 0x10 | (cc & 0x0F)
        start = 4
    copy = payload[: TS - start]
    pkt[start : start + len(copy)] = copy
    return bytes(pkt)


def pat_section() -> bytes:
    return bytes(
        [
            0x00,
            0xB0,
            0x0D,
            0x00,
            0x01,
            0xC1,
            0x00,
            0x00,
            0x00,
            0x01,
            0xE0,
            0x10,
        ]
    )


def pmt_section() -> bytes:
    return bytes(
        [
            0x02,
            0xB0,
            0x12,
            0x00,
            0x01,
            0xC1,
            0x00,
            0x00,
            0xE1,
            0x00,
            0xF0,
            0x00,
            0x1B,
            0xE1,
            0x00,
            0xF0,
            0x00,
        ]
    )


def tr101290_broken_ts() -> bytes:
    out = bytearray()
    bad = bytearray(TS)
    bad[0] = 0x00
    out.extend(bad)
    out.extend(ts_packet(0x0000, 0, pat_section()))
    out.extend(ts_packet(0x0010, 0, pmt_section()))
    out.extend(ts_packet(0x0100, 0, bytes([0xFF] * 8)))
    pes = bytes([0, 0, 0, 1, 0xE0, 0, 0, 0x80, 0x05, 0x21, 0, 0, 0, 1, 0x09, 0x10])
    out.extend(ts_packet(0x0101, 0, pes))
    out.extend(ts_packet(0x0101, 7, pes))
    out.extend(ts_packet(0x0101, 1, b"", pcr_base=1_000_000))
    out.extend(ts_packet(0x0101, 2, b"", pcr_base=1_005_000))
    return bytes(out)


def sei_nal(payload_type: int, payload: bytes) -> bytes:
    body = bytes([0x06, payload_type, len(payload)]) + payload + bytes([0x80])
    return b"\x00\x00\x00\x01" + body


def sei_caption_hdr_ts() -> bytes:
    atsc = bytes([0xB5, 0x00, 0x31, 0x81, 0x00, 0x00])
    cll = bytes([0x03, 0xE8, 0x01, 0xF4])
    nals = sei_nal(4, atsc) + sei_nal(144, cll)
    pes = bytes([0, 0, 0, 1, 0xE0, 0, 0, 0x80, 0x05]) + nals
    out = bytearray()
    out.extend(ts_packet(0x0000, 0, pat_section()))
    out.extend(ts_packet(0x0010, 0, pmt_section()))
    out.extend(ts_packet(0x0101, 0, pes))
    return bytes(out)


def sei_fmp4_m4s() -> bytes:
    atsc = bytes([0xB5, 0x00, 0x31, 0x81, 0x00, 0x00])
    nal = sei_nal(4, atsc)
    mdat = struct.pack(">I", len(nal)) + nal
    ftyp = b"\x00\x00\x00\x20ftypisom\x00\x00\x00\x00isomiso2"
    mdat_box = struct.pack(">I", 8 + len(mdat)) + b"mdat" + mdat
    return ftyp + mdat_box


def parse_range(header: str, size: int) -> Tuple[int, int] | None:
    if not header.startswith("bytes="):
        return None
    part = header[6:].strip()
    if "-" not in part:
        return None
    start_s, end_s = part.split("-", 1)
    start = int(start_s) if start_s else 0
    end = int(end_s) if end_s else size - 1
    return start, min(end, size - 1)


class FeedHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args) -> None:  # noqa: D401
        return

    def do_GET(self) -> None:  # noqa: N802
        path = self.path.split("?", 1)[0]
        body, ctype = route_http(path)
        rng = self.headers.get("Range")
        if rng and body:
            parsed = parse_range(rng, len(body))
            if parsed:
                start, end = parsed
                chunk = body[start : end + 1]
                self.send_response(206)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(body)}")
                self.send_header("Content-Length", str(len(chunk)))
                self.end_headers()
                self.wfile.write(chunk)
                return
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)


def route_http(path: str) -> Tuple[bytes, str]:
    if path.endswith(".mpd") or "/dash/" in path:
        mpd = (FIXTURES / "dash_live.mpd").read_bytes()
        return mpd, "application/dash+xml"
    if path.endswith("master.m3u8"):
        pl = (
            b"#EXTM3U\n"
            b"#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n"
            b"360.m3u8\n"
            b"#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\n"
            b"720.m3u8\n"
        )
        return pl, "application/vnd.apple.mpegurl"
    if "/ll-hls/" in path and path.endswith(".m3u8"):
        pl = (
            b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n"
            b"#EXT-X-MEDIA-SEQUENCE:1\n"
            b"#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=0.5\n"
            b"#EXT-X-PART-INF:PART-TARGET=0.5\n"
            b"#EXT-X-MAP:URI=\"init.m4s\"\n"
            b"#EXT-X-PART:DURATION=0.5,URI=\"part0.m4s\"\n"
            b"#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part1.m4s\"\n"
            b"#EXTINF:2.0,\nseg.m4s\n"
        )
        return pl, "application/vnd.apple.mpegurl"
    if "/tr101290/" in path and path.endswith(".m3u8"):
        pl = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\ntr101290/seg.ts\n"
        return pl, "application/vnd.apple.mpegurl"
    if "/sei/" in path and path.endswith(".m3u8"):
        pl = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nsei/sei.ts\n"
        return pl, "application/vnd.apple.mpegurl"
    if path.endswith("live.m3u8") or path.endswith("hls.m3u8"):
        pl = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n"
        return pl, "application/vnd.apple.mpegurl"
    if path.endswith("seg.ts") or path.endswith("tr101290/seg.ts"):
        if "tr101290" in path:
            return tr101290_broken_ts(), "video/mp2t"
        return ts_packet(0x0101, 1, bytes([0xFF] * 8)), "video/mp2t"
    if path.endswith("sei.ts") or path.endswith("sei/sei.ts"):
        return sei_caption_hdr_ts(), "video/mp2t"
    if path.endswith(".m4s") or path.endswith(".mp4") or path.endswith("init.mp4"):
        return sei_fmp4_m4s(), "video/mp4"
    return b"", "text/plain"


def srt_listener() -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", SRT_PORT))
    while True:
        data, addr = sock.recvfrom(2048)
        if len(data) >= 4 and struct.unpack(">I", data[:4])[0] == SRT_HS_MAGIC:
            reply = bytearray(64)
            struct.pack_into(">I", reply, 0, SRT_HS_MAGIC)
            sock.sendto(reply, addr)


def rtmp_listener() -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", RTMP_PORT))
    srv.listen(8)
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=rtmp_session, args=(conn,), daemon=True).start()


def recv_exact(conn: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            break
        buf.extend(chunk)
    return bytes(buf)


def rtmp_session(conn: socket.socket) -> None:
    try:
        c0c1 = recv_exact(conn, 1537)
        if len(c0c1) < 1537:
            return
        s0 = bytes([0x03])
        s1 = bytearray(1536)
        s1[4:8] = struct.pack(">I", int(time.time()))
        s1[8:1528] = b"H264" + bytes([0x42] * 100) + b"AAC" + bytes([0x24] * 100)
        s2 = bytes(1536)
        conn.sendall(s0 + bytes(s1) + s2)
        try:
            conn.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        _ = recv_exact(conn, 1536)
    finally:
        conn.close()


def main() -> None:
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", HTTP_PORT), FeedHandler)
    threading.Thread(target=srt_listener, daemon=True).start()
    threading.Thread(target=rtmp_listener, daemon=True).start()
    print(f"HTTP mock http://127.0.0.1:{HTTP_PORT}", flush=True)
    print(f"SRT mock srt://127.0.0.1:{SRT_PORT}", flush=True)
    print(f"RTMP mock rtmp://127.0.0.1:{RTMP_PORT}/live/stream", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
