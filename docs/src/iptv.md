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

## streamtop check

`streamtop check` is the scriptable version of `--audit`. It takes a playlist file or URL (M3U, JSON, YAML, or a single HLS/DASH URL), probes every entry concurrently with a per-entry time limit, and prints a table, JSON, or CSV. It writes no files unless you ask for them.

```bash
streamtop check ./channels.m3u
streamtop check "https://example.com/playlist.m3u" --timeout 5 --concurrency 50
streamtop check ./channels.m3u --format json > report.json
streamtop check ./channels.m3u --format csv --output report.csv
streamtop check ./channels.m3u --alive-out working.m3u
streamtop check ./channels.m3u --fail-on-dead      # exit 1 if any entry is dead
```

| Option | Default | Meaning |
|---|---|---|
| `-t, --timeout SECS` | 10 | Time limit for one entry (playlist, variant, and segment probe together). An entry that runs over is `ERROR` with `timeout after Ns` |
| `-j, --concurrency N` | 25 | Entries probed at the same time |
| `-f, --format` | `table` | `table`, `json`, or `csv` |
| `-o, --output FILE` | stdout | Write the report to a file. With `table`, writes JSON |
| `--alive-out FILE` | | Write an M3U with the entries that are not dead, keeping `tvg-id`, `tvg-logo`, and `group-title` |
| `--fail-on-dead` | off | Exit 1 when any entry is `ERROR` |
| `--strict` | off | Treat `STALL` as dead for `--fail-on-dead` and `--alive-out` |
| `-H, --header` | | Extra HTTP header, repeatable |
| `-A, --user-agent` | | User-Agent override |

JSON and CSV reports redact URLs and tokens, like the `--audit` files. The `--alive-out` playlist keeps the original URLs, because it is meant to be played.

Without `--fail-on-dead`, `check` exits 0 even when entries are dead, so it can be used for reporting. `--audit` always exits 1 on any `ERROR` or `STALL`.

## Notes

* A `LIVE` verdict means the playlist and one segment were reachable. It does not decode video, so a channel that serves a black frame still counts as live.
* Some IPTV servers block unknown User-Agents or require a Referer. Pass them with `-A` and `-H`.
* `--audit` always uses 25 workers and a 5 second limit per HTTP request, so on large lists with many unreachable hosts the run time is set mostly by those timeouts. `check` lets you set the worker count with `-j` and a limit per entry with `--timeout`.
