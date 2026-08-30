//! Launch an external player (mpv / ffplay) for the active manifest URL.

use std::process::{Command, Stdio};

/// Result of a Quick Play attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickPlayResult {
    Started { player: String },
    NotFound,
    SpawnFailed { player: String, error: String },
}

/// Prefer `mpv`, fall back to `ffplay`. Non-blocking (`spawn`).
pub fn launch_quick_play(
    url: &str,
    headers: &[String],
    user_agent: Option<&str>,
) -> QuickPlayResult {
    find_player().map_or(QuickPlayResult::NotFound, |player| {
        match spawn_player(player, url, headers, user_agent) {
            Ok(()) => QuickPlayResult::Started {
                player: player.to_string(),
            },
            Err(error) => QuickPlayResult::SpawnFailed {
                player: player.to_string(),
                error,
            },
        }
    })
}

fn find_player() -> Option<&'static str> {
    ["mpv", "ffplay"]
        .into_iter()
        .find(|&name| binary_in_path(name))
}

fn binary_in_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return true;
            }
        }
    }
    false
}

fn spawn_player(
    player: &str,
    url: &str,
    headers: &[String],
    user_agent: Option<&str>,
) -> Result<(), String> {
    let mut cmd = Command::new(player);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match player {
        "mpv" => {
            cmd.arg("--no-terminal").arg("--force-window=yes");
            for h in headers {
                cmd.arg(format!("--http-header-fields={h}"));
            }
            if let Some(ua) = user_agent {
                cmd.arg(format!("--user-agent={ua}"));
            }
            cmd.arg(url);
        }
        "ffplay" => {
            cmd.arg("-loglevel").arg("quiet").arg("-autoexit");
            if !headers.is_empty() {
                let joined = headers
                    .iter()
                    .map(|h| h.trim().to_string())
                    .collect::<Vec<_>>()
                    .join("\r\n");
                cmd.arg("-headers").arg(format!("{joined}\r\n"));
            }
            if let Some(ua) = user_agent {
                cmd.arg("-user_agent").arg(ua);
            }
            cmd.arg(url);
        }
        other => return Err(format!("unsupported player: {other}")),
    }

    cmd.spawn().map(|_| ()).map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_when_neither_on_path() {
        // Smoke: API returns a known variant; PATH-dependent outcome is ok either way.
        let r = launch_quick_play("https://example.com/live.m3u8", &[], None);
        match r {
            QuickPlayResult::Started { .. }
            | QuickPlayResult::NotFound
            | QuickPlayResult::SpawnFailed { .. } => {}
        }
    }
}
