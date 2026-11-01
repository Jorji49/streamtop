//! Colored text on stdout only when it is a terminal and `NO_COLOR` is unset.

use std::io::IsTerminal;

use crossterm::style::{Color, ResetColor, SetForegroundColor};

/// True when stdout is a TTY and `NO_COLOR` is not set (<https://no-color.org>).
pub fn stdout_color_enabled() -> bool {
    color_allowed(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stdout().is_terminal(),
    )
}

fn color_allowed(no_color: Option<&std::ffi::OsStr>, is_tty: bool) -> bool {
    is_tty && no_color.is_none_or(std::ffi::OsStr::is_empty)
}

/// Print `text` in `color` when allowed, plain otherwise. No newline.
pub fn print_colored(text: &str, color: Color) {
    let mut out = std::io::stdout();
    if stdout_color_enabled() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
        print!("{text}");
        let _ = crossterm::execute!(out, ResetColor);
    } else {
        print!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_and_pipes_disable_color() {
        assert!(color_allowed(None, true));
        assert!(!color_allowed(None, false));
        assert!(!color_allowed(Some(std::ffi::OsStr::new("1")), true));
        assert!(color_allowed(Some(std::ffi::OsStr::new("")), true));
    }
}
