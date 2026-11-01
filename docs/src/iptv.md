# IPTV playlist checking

streamtop reads IPTV channel lists in M3U (`#EXTINF` with `tvg-name`, `tvg-id`, `tvg-logo`, `group-title`), JSON, and YAML. A list can be a local file or a URL.

## Browse channels

```bash
streamtop ./channels.m3u
streamtop "https://example.com/playlist.m3u"
```

This opens a channel picker with search. Selecting a channel opens the normal dashboard for it.

## Check every channel

```bash
streamtop ./channels.m3u --audit
```

`--audit` probes every channel concurrently (25 workers) and prints a table:

```text
#    Channel                      Group        HTTP   PDT       TTFB CDN                    Verdict
1    News HD                      News         200    yes      182ms Cloudflare HIT         LIVE
2    Sports 2                     Sports       404    no           - -                      ERROR
     404 Not Found
```

For each channel it fetches the playlist (following a master playlist to its highest-bandwidth variant), then range-requests the last segment. For DASH it probes a segment URL taken from the MPD.

Verdicts:

| Verdict | Meaning |
|---------|---------|
| `LIVE` | Playlist and segment returned 2xx/3xx and the segment TTFB was under 2500 ms |
| `STALL` | Segment TTFB was 2500 ms or more, or the media playlist had no segments |
| `ERROR` | HTTP 401, 403, 404, 5xx, a network error, a timeout, or a playlist that could not be parsed |

Each request has a 3 second connect timeout and a 5 second total timeout. Results are also written to `audit_report.json` and `audit_report.csv` in the current directory, with URLs and tokens redacted. The exit code is non-zero when any channel is `ERROR` or `STALL`.

Headers and User-Agent apply to every channel:

```bash
streamtop ./channels.m3u --audit -H "Referer: https://example.com" -A "Mozilla/5.0"
```

## Notes

* A `LIVE` verdict means the playlist and one segment were reachable. It does not decode video, so a channel that serves a black frame still counts as live.
* Some IPTV servers block unknown User-Agents or require a Referer. Pass them with `-A` and `-H`.
* The worker count is fixed at 25, so on large lists with many unreachable hosts the run time is set mostly by the 5 second timeout.
