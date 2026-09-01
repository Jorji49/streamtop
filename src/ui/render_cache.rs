//! Pre-formatted TUI text cache; rebuilt on `StreamEvent`, not on draw ticks.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::engine::redact::redact_url;
use crate::models::{
    format_dvr_window, format_url_mid_ellipsis, DlToDurState, LatencyState, StreamStatusKind,
};
use crate::ui::app::App;

fn dl_dur_style(state: DlToDurState) -> Style {
    match state {
        DlToDurState::Normal => Style::default().fg(Color::LightGreen),
        DlToDurState::Elevated => Style::default().fg(Color::Yellow),
        DlToDurState::Draining => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn status_badge(app: &App) -> (&'static str, Color, Color) {
    match app.status.kind {
        StreamStatusKind::Live if app.active_ad.as_ref().is_some_and(|a| a.active) => {
            ("DAI", Color::White, Color::Magenta)
        }
        StreamStatusKind::Live if app.latency.is_estimated() || !app.uses_pdt() => {
            ("ESTIMATED", Color::Black, Color::Cyan)
        }
        StreamStatusKind::Live => ("LIVE", Color::Black, Color::LightGreen),
        StreamStatusKind::Error => ("ERROR", Color::White, Color::Red),
        StreamStatusKind::Degraded => ("DEGRADED", Color::Black, Color::Yellow),
    }
}

fn health_colors(score: u8) -> (Color, Color) {
    if score >= 90 {
        (Color::Black, Color::LightGreen)
    } else if score >= 70 {
        (Color::Black, Color::LightYellow)
    } else {
        (Color::White, Color::Red)
    }
}

fn latency_display(state: LatencyState) -> (String, Color) {
    match state {
        LatencyState::Unknown => ("-".into(), Color::DarkGray),
        LatencyState::Estimated(ms) => {
            let secs = ms as f64 / 1000.0;
            (
                format!("estimated ~{secs:.2}s"),
                if ms < 8_000 {
                    Color::Cyan
                } else if ms < 18_000 {
                    Color::LightYellow
                } else {
                    Color::Red
                },
            )
        }
        LatencyState::Measured(ms) => {
            let secs = ms as f64 / 1000.0;
            (
                format!("{secs:.3}s"),
                if ms < 8_000 {
                    Color::LightGreen
                } else if ms < 18_000 {
                    Color::LightYellow
                } else {
                    Color::Red
                },
            )
        }
    }
}

/// Cached header/footer lines for zero-allocation draw ticks.
#[derive(Debug)]
pub struct UiRenderCache {
    pub header_lines: Vec<Line<'static>>,
    pub footer_transport: String,
    dirty: bool,
}

impl Default for UiRenderCache {
    fn default() -> Self {
        Self {
            header_lines: Vec::new(),
            footer_transport: String::new(),
            dirty: true,
        }
    }
}

impl UiRenderCache {
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn rebuild_if_dirty(&mut self, app: &App, url_width: usize) {
        if !self.dirty {
            return;
        }
        self.rebuild(app, url_width);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn take_and_rebuild(app: &mut App, url_width: usize) {
        if !app.render_cache.dirty {
            return;
        }
        let mut cache = std::mem::take(&mut app.render_cache);
        cache.rebuild(app, url_width);
        app.render_cache = cache;
    }

    pub fn rebuild(&mut self, app: &App, url_width: usize) {
        self.dirty = false;
        let (latency_text, latency_color) = latency_display(app.latency);
        let (badge_text, badge_fg, badge_back) = status_badge(app);
        let (score_fg, score_back) = health_colors(app.health.score);

        let seq = app
            .last_segment
            .as_ref()
            .map(|s| s.media_sequence.to_string())
            .or_else(|| app.playlist.as_ref().map(|p| p.media_sequence.to_string()))
            .unwrap_or_else(|| "-".into());
        let target = app
            .playlist
            .as_ref()
            .map_or_else(|| "-".into(), |p| format!("{}s", p.target_duration));

        let cdn_badge = app.cdn.last.as_ref().map_or_else(
            || "UNKNOWN".into(),
            crate::models::stream::CdnEdgeInfo::badge,
        );
        let mut cdn_line = app.cdn.hit_ratio_pct().map_or_else(
            || format!("CDN: {cdn_badge}"),
            |pct| {
                format!(
                    "CDN: {cdn_badge}  hit {:.0}% ({}/{})",
                    pct,
                    app.cdn.hits,
                    app.cdn.hits + app.cdn.misses
                )
            },
        );
        if let Some(c) = &app.cdn.last {
            let d = c.edge_detail();
            if !d.is_empty() {
                cdn_line = format!("{cdn_line}  {d}");
            }
        }

        let url = format_url_mid_ellipsis(&redact_url(&app.active_url), url_width);
        let buf = app.buffer.display();
        let buf_color = if app.buffer.stall_risk_pct >= 50 {
            Color::Red
        } else if app.buffer.stall_risk_pct > 0 {
            Color::LightYellow
        } else {
            Color::LightGreen
        };

        let video_fps = app
            .variants
            .iter()
            .find(|v| v.selected)
            .or_else(|| app.variants.first())
            .and_then(|v| v.frame_rate);
        let fps_label = match video_fps {
            Some(f) if f > 0.0 => {
                let s = if (f - f.round()).abs() < 0.05 {
                    format!("{:.0}", f.round())
                } else {
                    format!("{f:.2}")
                };
                format!(" {s} FPS ")
            }
            _ => " - FPS ".into(),
        };
        let (fps_fg, fps_back) = if video_fps.is_some() {
            (Color::Black, Color::LightCyan)
        } else {
            (Color::DarkGray, Color::Black)
        };

        let wire = app.last_segment.as_ref().and_then(|s| s.wire.as_ref());
        let mut status_row2: Vec<Span<'static>> = vec![
            Span::raw(" "),
            Span::styled(
                format!(" {badge_text} "),
                Style::default()
                    .fg(badge_fg)
                    .bg(badge_back)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!(" SHI {:>3} {} ", app.health.score, app.health.label),
                Style::default()
                    .fg(score_fg)
                    .bg(score_back)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                fps_label,
                Style::default()
                    .fg(fps_fg)
                    .bg(fps_back)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Seq "),
            Span::styled(
                seq,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Target "),
            Span::styled(
                target,
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if app.dl_dur_hud.is_visible() {
            status_row2.push(Span::raw("  "));
            status_row2.push(Span::styled(
                app.dl_dur_hud.as_str().to_string(),
                dl_dur_style(app.dl_dur_hud.state).add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(w) = wire {
            if let Some(gop) = w.gop_badge() {
                let (gop_fg, gop_back) = if gop == "IDR" {
                    (Color::Black, Color::LightGreen)
                } else {
                    (Color::Black, Color::Yellow)
                };
                status_row2.push(Span::raw(" "));
                status_row2.push(Span::styled(
                    format!(" {gop} "),
                    Style::default()
                        .fg(gop_fg)
                        .bg(gop_back)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(audio) = w.audio_badge() {
                status_row2.push(Span::raw(" "));
                status_row2.push(Span::styled(
                    format!(" {audio} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightMagenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        }

        let channel_prefix = app
            .channel_name
            .as_deref()
            .map(|n| format!("{n}  "))
            .unwrap_or_default();

        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    " streamtop ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    channel_prefix,
                    Style::default()
                        .fg(Color::LightYellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(url, Style::default().fg(Color::White)),
            ]),
            Line::from(status_row2),
            Line::from(vec![
                Span::raw(" Latency "),
                Span::styled(
                    latency_text,
                    Style::default()
                        .fg(latency_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  │  "),
                Span::styled(cdn_line, Style::default().fg(Color::Magenta)),
                Span::raw("  "),
                Span::styled(
                    app.playlist
                        .as_ref()
                        .filter(|p| p.drm.present)
                        .map(|p| format!(" {} ", p.drm.badge))
                        .unwrap_or_default(),
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    buf,
                    Style::default().fg(buf_color).add_modifier(Modifier::BOLD),
                ),
                app.playlist.as_ref().map_or_else(
                    || Span::raw(String::new()),
                    |p| {
                        Span::styled(
                            format!(
                                "  {}",
                                format_dvr_window(p.window_segments, p.window_secs)
                            ),
                            Style::default().fg(Color::DarkGray),
                        )
                    },
                ),
            ]),
        ];

        if !app.g2g.is_empty() {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled(
                app.g2g.display(),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )]));
        }
        if let Some(ad) = &app.active_ad {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled(
                format!(" AD {} ", ad.summary),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )]));
        }
        if let Some(pat) = &app.log_filter {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled(
                format!(" filter: /{pat}/ "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )]));
        }
        if let Some(ll_badge) = app.playlist.as_ref().and_then(|p| p.ll_hls.header_badge()) {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled(
                format!(" {ll_badge} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            )]));
        }
        if let Some(ver) = app.transport.http_version {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled(
                format!(" HTTP {} ", ver.as_str()),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            )]));
        }

        self.footer_transport = app.transport.display_line();
        self.header_lines = lines;
    }
}

#[cfg(test)]
pub fn rebuild_lines_for_test(app: &App) -> Vec<Line<'static>> {
    let mut cache = UiRenderCache::default();
    cache.rebuild(app, 80);
    cache.header_lines
}
