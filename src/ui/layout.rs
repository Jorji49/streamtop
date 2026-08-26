use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use crate::models::{
    format_dvr_window, format_url_mid_ellipsis, DiagCategory, LatencyState, LogLevel,
    StreamStatusKind,
};
use crate::ui::app::App;

fn rounded(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.into())
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Tiny terminals: collapse fixed chrome so Min panes do not overflow.
    let (header_h, mid_pct, log_min, footer_h) = if area.height < 18 {
        (4u16, 40u16, 3u16, 1u16)
    } else if area.height < 28 {
        (5, 45, 5, 1)
    } else {
        (7, 48, 8, 1)
    };

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_h.min(area.height.saturating_sub(2))),
            Constraint::Percentage(mid_pct),
            Constraint::Min(log_min),
            Constraint::Length(footer_h.min(1)),
        ])
        .split(area);

    draw_header(frame, app, root[0]);

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    draw_left(frame, app, mid[0]);
    draw_right(frame, app, mid[1]);
    draw_log(frame, app, root[2]);
    draw_footer(frame, app, root[3]);
    if let Some((msg, _)) = &app.toast {
        draw_toast(frame, frame.area(), msg);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let (latency_text, latency_color) = latency_display(app.latency);
    let (badge_text, badge_fg, badge_bg) = status_badge(app);
    let (score_fg, score_bg) = health_colors(app.health.score);

    let seq = app
        .last_segment
        .as_ref()
        .map(|s| s.media_sequence.to_string())
        .or_else(|| app.playlist.as_ref().map(|p| p.media_sequence.to_string()))
        .unwrap_or_else(|| "-".into());
    let target = app
        .playlist
        .as_ref()
        .map(|p| format!("{}s", p.target_duration))
        .unwrap_or_else(|| "-".into());

    let cdn_badge = app
        .cdn
        .last
        .as_ref()
        .map(|c| c.badge())
        .unwrap_or_else(|| "UNKNOWN".into());
    let cdn_line = match app.cdn.hit_ratio_pct() {
        Some(pct) => format!(
            "CDN: {cdn_badge}  hit {:.0}% ({}/{})",
            pct,
            app.cdn.hits,
            app.cdn.hits + app.cdn.misses
        ),
        None => format!("CDN: {cdn_badge}"),
    };

    let window = app
        .playlist
        .as_ref()
        .map(|p| format_dvr_window(p.window_segments, p.window_secs));

    let url = format_url_mid_ellipsis(&app.active_url, area.width.saturating_sub(14) as usize);
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
    let (fps_fg, fps_bg) = if video_fps.is_some() {
        (Color::Black, Color::LightCyan)
    } else {
        (Color::DarkGray, Color::Black)
    };

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
                app.channel_name
                    .as_deref()
                    .map(|n| format!("{n}  "))
                    .unwrap_or_default(),
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(url, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!(" {badge_text} "),
                Style::default()
                    .fg(badge_fg)
                    .bg(badge_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!(" SHI {:>3} {} ", app.health.score, app.health.label),
                Style::default()
                    .fg(score_fg)
                    .bg(score_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                fps_label,
                Style::default()
                    .fg(fps_fg)
                    .bg(fps_bg)
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
        ]),
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
            window
                .map(|w| Span::styled(format!("  {w}"), Style::default().fg(Color::DarkGray)))
                .unwrap_or_else(|| Span::raw("")),
        ]),
    ];

    if let Some(ad) = &app.active_ad {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!(" AD {} ", ad.summary),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    if let Some(ll_badge) = app.playlist.as_ref().and_then(|p| p.ll_hls.header_badge()) {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!(" {ll_badge} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).block(rounded(" Status ")), area);
}

fn status_badge(app: &App) -> (&'static str, Color, Color) {
    match app.status.kind {
        StreamStatusKind::Live if app.active_ad.as_ref().map(|a| a.active).unwrap_or(false) => {
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

fn draw_left(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(5)])
        .split(area);

    draw_segment_panel(frame, app, chunks[0]);
    draw_abr_panel(frame, app, chunks[1]);
}

fn draw_segment_panel(frame: &mut Frame, app: &App, area: Rect) {
    let lines = if let Some(seg) = &app.last_segment {
        let probe = if seg.probed { "range-probe" } else { "full" };
        let size_line = if seg.probed {
            format!(
                " Declared size  : {:.1} KB (not fully downloaded)",
                seg.size_bytes as f64 / 1024.0
            )
        } else {
            format!(
                " Duration/Size  : {:.3}s · {:.1} KB",
                seg.duration_secs,
                seg.size_bytes as f64 / 1024.0
            )
        };
        let net = seg
            .network
            .as_ref()
            .map(|n| n.display_line())
            .unwrap_or_else(|| format!("TTFB: {}ms", seg.ttfb_ms));
        let mut lines = vec![
            Line::from(format!(" Media Sequence : {}", seg.media_sequence)),
            Line::from(size_line),
            Line::from(format!(
                " TTFB / Transfer: {} ms / {} ms ({probe})",
                seg.ttfb_ms, seg.download_ms
            )),
            Line::from(Span::styled(
                format!(" {net}"),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(format!(
                " Rate / Type    : {} · {}",
                seg.rate_label(),
                seg.container.as_str()
            )),
            Line::from(format!(" CDN            : {}", seg.cdn.badge())),
        ];
        if let Some(wire) = &seg.wire {
            if let Some(res) = wire.resolution_label() {
                lines.push(Line::from(format!(
                    " Wire           : {res} · {} fps · {}",
                    wire.frame_rate
                        .map(|f| format!("{f:.2}"))
                        .unwrap_or_else(|| "-".into()),
                    wire.codec.as_deref().unwrap_or("-")
                )));
            }
        }
        lines
    } else {
        vec![Line::from(" No segment sample yet…")]
    };

    frame.render_widget(
        Paragraph::new(lines).block(rounded(" Last Segment / Edge ")),
        area,
    );
}

fn draw_abr_panel(frame: &mut Frame, app: &App, area: Rect) {
    let has_profiles = !app.variants.is_empty();
    if !has_profiles {
        frame.render_widget(
            Paragraph::new(" Single media stream (no master playlist) ")
                .style(Style::default().fg(Color::Gray))
                .block(rounded(" ABR Ladder ")),
            area,
        );
        return;
    }

    let warn_lines: usize = if app.abr_health.warnings.is_empty() {
        0
    } else {
        app.abr_health.warnings.len().min(3)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(warn_lines as u16)])
        .split(area);

    let header = Row::new(vec!["Bandwidth", "Resolution", "FPS", "Codecs", ""])
        .style(
            Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);

    let rows = app.variants.iter().map(|v| {
        let res = v.resolution_label();
        let codecs = truncate(v.codecs.as_deref().unwrap_or("-"), 14);
        let selected = if v.selected { "●" } else { "" };
        let style = if v.mismatch.is_some() {
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD)
        } else if v.selected {
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD)
        } else if v.from_wire {
            Style::default().fg(Color::LightCyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        Row::new(vec![
            Cell::from(if v.bandwidth > 0 {
                format!("{} kbps", v.bandwidth / 1000)
            } else {
                "wire".into()
            }),
            Cell::from(res),
            Cell::from(v.fps_label()),
            Cell::from(codecs),
            Cell::from(selected),
        ])
        .style(style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Min(6),
            Constraint::Length(2),
        ],
    )
    .header(header)
    .block(rounded(" ABR Ladder "))
    .column_spacing(1);

    frame.render_widget(table, chunks[0]);

    if !app.abr_health.warnings.is_empty() && chunks[1].height > 0 {
        let lines: Vec<Line> = app
            .abr_health
            .warnings
            .iter()
            .take(2)
            .map(|w| {
                Line::from(Span::styled(
                    format!(" ! {w}"),
                    Style::default().fg(Color::LightYellow),
                ))
            })
            .chain(app.variants.iter().filter_map(|v| {
                v.mismatch.as_ref().map(|m| {
                    Line::from(Span::styled(
                        format!(" {m}"),
                        Style::default().fg(Color::LightRed),
                    ))
                })
            }))
            .take(3)
            .collect();
        frame.render_widget(Paragraph::new(lines), chunks[1]);
    } else if chunks[1].height > 0 {
        let mismatches: Vec<Line> = app
            .variants
            .iter()
            .filter_map(|v| v.mismatch.as_ref())
            .take(3)
            .map(|m| {
                Line::from(Span::styled(
                    format!(" {m}"),
                    Style::default().fg(Color::LightRed),
                ))
            })
            .collect();
        if !mismatches.is_empty() {
            frame.render_widget(Paragraph::new(mismatches), chunks[1]);
        }
    }
}

fn draw_right(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    if app.uses_pdt() && app.latency.is_measured() {
        let data = app.latency_history.to_vec();
        let secs_data: Vec<u64> = data.iter().map(|ms| (ms / 100).max(1)).collect();
        let title = match app.latency {
            LatencyState::Measured(ms) => {
                format!(" Latency Trend ({:.2}s) ", ms as f64 / 1000.0)
            }
            _ => " Latency Trend (s) ".into(),
        };
        render_spark_or_placeholder(frame, chunks[0], &title, &secs_data, Color::Cyan);
    } else {
        let data = app.ttfb_history.to_vec();
        let last = data.last().copied().unwrap_or(0);
        let title = format!(" Segment TTFB / Response ({last} ms) ");
        render_spark_or_placeholder(frame, chunks[0], &title, &data, Color::Cyan);
    }

    let bitrate_data = app.bitrate_history.to_vec();
    let transfer_data = app.transfer_history.to_vec();

    if app.probe_mode || app.last_segment.as_ref().map(|s| s.probed).unwrap_or(false) {
        let last_ms = transfer_data.last().copied().unwrap_or(0);
        let title = format!(" Transfer Time ({last_ms} ms) ");
        render_spark_or_placeholder(frame, chunks[1], &title, &transfer_data, Color::Magenta);
    } else {
        let last_kbps = bitrate_data.last().copied().unwrap_or(0);
        render_spark_or_placeholder(
            frame,
            chunks[1],
            &format!(" Download Rate ({last_kbps} kbps) "),
            &bitrate_data,
            Color::Magenta,
        );
    }
}

fn render_spark_or_placeholder(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    data: &[u64],
    color: Color,
) {
    let block = rounded(title.to_string());
    if data.is_empty() {
        frame.render_widget(
            Paragraph::new("Collecting data...")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
    } else {
        frame.render_widget(
            Sparkline::default()
                .block(block)
                .data(data)
                .style(Style::default().fg(color)),
            area,
        );
    }
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let visible = area.height.saturating_sub(2) as usize;
    let total = app.log.len();
    let max_scroll = total.saturating_sub(visible);
    let scroll = (app.log_scroll as usize).min(max_scroll);
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(visible);

    let lines: Vec<Line> = app.log[start..end]
        .iter()
        .map(|entry| {
            let (tag, color) = category_style(entry.category, entry.level);
            Line::from(vec![
                Span::styled(
                    format!(" {} ", entry.time),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("[{tag}] "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    entry.message.clone(),
                    if entry.category == DiagCategory::Ad {
                        Style::default().fg(Color::Magenta)
                    } else {
                        Style::default()
                    },
                ),
            ])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(rounded(" Diagnostics / Event Log ")),
        area,
    );
}

fn category_style(cat: DiagCategory, level: LogLevel) -> (&'static str, Color) {
    match cat {
        DiagCategory::Info => match level {
            LogLevel::Error => ("ERR", Color::Red),
            LogLevel::Warn => ("WARN", Color::LightYellow),
            LogLevel::Info => ("INFO", Color::Gray),
        },
        other => (
            other.tag(),
            match other {
                DiagCategory::Rfc => Color::Red,
                DiagCategory::Stalling => Color::LightYellow,
                DiagCategory::Cdn => Color::Cyan,
                DiagCategory::Ad => Color::Magenta,
                DiagCategory::Abr => Color::LightYellow,
                DiagCategory::Segment => Color::LightGreen,
                DiagCategory::Buffer => Color::LightYellow,
                DiagCategory::LlHls => Color::Cyan,
                DiagCategory::AvSync => Color::LightYellow,
                DiagCategory::Drm => Color::Magenta,
                DiagCategory::Info => Color::Gray,
            },
        ),
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " ? help ",
            Style::default().fg(Color::Black).bg(Color::White),
        ),
        Span::raw(" "),
        Span::styled(
            " q quit ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(
            " c curl ",
            Style::default().fg(Color::Black).bg(Color::LightCyan),
        ),
        Span::raw(" "),
        Span::styled(
            " p play ",
            Style::default().fg(Color::Black).bg(Color::LightMagenta),
        ),
        Span::raw(" "),
        Span::styled(
            " Space save JSON ",
            Style::default().fg(Color::Black).bg(Color::LightYellow),
        ),
        Span::raw(" "),
        Span::styled(
            " r reset ",
            Style::default().fg(Color::Black).bg(Color::LightGreen),
        ),
    ];
    if app.has_catalog() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            " Tab channels ",
            Style::default().fg(Color::White).bg(Color::Magenta),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            " Esc picker ",
            Style::default().fg(Color::Black).bg(Color::Gray),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

pub fn draw_help(frame: &mut Frame, area: Rect, picker_context: bool) {
    let popup = centered_rect(area, 62, 70);
    frame.render_widget(Clear, popup);
    let lines = if picker_context {
        vec![
            Line::from(Span::styled(
                " Keyboard Shortcuts ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("  ↑↓ / j k     Navigate channels"),
            Line::from("  /            Live search (name / group)"),
            Line::from("  Enter        Open live diagnostics"),
            Line::from("  Esc          Clear search / quit"),
            Line::from("  ?            Help (any key closes)"),
            Line::from("  q / Ctrl+C   Quit"),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                " Keyboard Shortcuts ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("  c            Copy curl (headers + Range) to clipboard"),
            Line::from("  p            Quick Play (mpv / ffplay, non-blocking)"),
            Line::from("  Space        Save diagnostics/<channel>_<time>.json"),
            Line::from("  Tab          Channel switcher overlay"),
            Line::from("  Esc          Back to channel picker"),
            Line::from("  r            Reset metrics / ring buffers"),
            Line::from("  ?            Help (any key closes)"),
            Line::from("  j/k ↑↓       Scroll event log"),
            Line::from("  q / Ctrl+C   Quit (restore terminal)"),
        ]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Help ")
                    .title_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .alignment(Alignment::Left),
        popup,
    );
}

fn draw_toast(frame: &mut Frame, area: Rect, msg: &str) {
    let width = (msg.len() as u16).saturating_add(4).min(area.width);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + 1;
    let rect = Rect {
        x,
        y,
        width,
        height: 3,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(format!(" {msg} "))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::LightGreen)),
            )
            .style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        rect,
    );
}

fn centered_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(popup[1])[1]
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

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let trimmed: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{trimmed}…")
}
