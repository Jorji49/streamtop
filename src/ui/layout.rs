use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use crate::models::{
    DiagCategory, LatencyState, LogLevel,
};
use crate::ui::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagnosticPanel {
    #[default]
    None,
    Tr101290,
    Sei,
    Qoe,
}

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

pub fn draw_regex_modal(frame: &mut Frame, area: Rect, app: &App) {
    let w = area.width.clamp(24, 72);
    let h = 5u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h + 2) / 2;
    let modal = Rect::new(x, y, w, h);
    frame.render_widget(Clear, modal);
    let draft = &app.log_filter_draft;
    let (hint, hint_color) = if draft.is_empty() {
        ("Enter regex; Esc clears", Color::DarkGray)
    } else if regex::Regex::new(draft).is_ok() {
        ("valid regex", Color::LightGreen)
    } else {
        ("invalid regex syntax", Color::Red)
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                " Regex search ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("/{draft}_")),
            Line::from(Span::styled(hint, Style::default().fg(hint_color))),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        modal,
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(&app.render_cache.header, area);
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
    frame.render_widget(&app.render_cache.segment, area);
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

    if app.probe_mode || app.last_segment.as_ref().is_some_and(|s| s.probed) {
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
    let filtered: Vec<&crate::models::LogEntry> = app.log_filter_regex.as_ref().map_or_else(
        || {
            app.log_filter.as_ref().map_or_else(
                || app.log.iter().collect(),
                |pat| {
                    let needle = pat.to_ascii_lowercase();
                    app.log
                        .iter()
                        .filter(|e| {
                            e.message.to_ascii_lowercase().contains(&needle)
                                || e.category.tag().to_ascii_lowercase().contains(&needle)
                        })
                        .collect()
                },
            )
        },
        |re| {
            app.log
                .iter()
                .filter(|e| {
                    re.is_match(&e.message) || re.is_match(e.category.tag()) || re.is_match(&e.time)
                })
                .collect()
        },
    );
    let total = filtered.len();
    let max_scroll = total.saturating_sub(visible);
    let scroll = (app.log_scroll as usize).min(max_scroll);
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(visible);

    let lines: Vec<Line> = filtered[start..end]
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
        Paragraph::new(lines).block(rounded(log_block_title(app))),
        area,
    );
}

fn log_block_title(app: &App) -> String {
    if app.log_filter_edit {
        return format!(" Log filter: /{}_ ", app.log_filter_draft);
    }
    app.log_filter.as_ref().map_or_else(
        || " Diagnostics / Event Log ".into(),
        |pat| format!(" Diagnostics / Event Log [regex: /{pat}/] "),
    )
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
                DiagCategory::Stalling
                | DiagCategory::Abr
                | DiagCategory::Buffer
                | DiagCategory::AvSync => Color::LightYellow,
                DiagCategory::Cdn | DiagCategory::LlHls => Color::Cyan,
                DiagCategory::Ad | DiagCategory::Drm => Color::Magenta,
                DiagCategory::Segment => Color::LightGreen,
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
            Line::from("  e            Export incident bundle (HTTP tx + spec violations)"),
            Line::from("  f / F        Cycle preset log filter / clear regex filter"),
            Line::from("  /            Regex log filter modal (Enter lock, Esc clear)"),
            Line::from("  Tab          Channel switcher overlay"),
            Line::from("  Esc          Back to channel picker"),
            Line::from("  r            Reset metrics / ring buffers"),
            Line::from("  t            TR 101 290 compliance table"),
            Line::from("  s            SEI / HDR / caption probe"),
            Line::from("  y            Synthetic QoE simulator"),
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

pub fn draw_diagnostic_panel(frame: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(area, 70, 60);
    frame.render_widget(Clear, popup);
    match app.diagnostic_panel {
        DiagnosticPanel::Tr101290 => draw_tr101290_panel(frame, popup, app),
        DiagnosticPanel::Sei => draw_sei_panel(frame, popup, app),
        DiagnosticPanel::Qoe => draw_qoe_panel(frame, popup, app),
        DiagnosticPanel::None => {}
    }
}

fn draw_tr101290_panel(frame: &mut Frame, area: Rect, app: &App) {
    let r = &app.tr101290;
    let header = Row::new(vec!["Pri", "Code", "Message"]).style(
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD),
    );
    let mut rows: Vec<Row> = r
        .checks
        .iter()
        .map(|c| {
            Row::new(vec![
                Cell::from(c.priority.to_string()),
                Cell::from(c.code.clone()),
                Cell::from(truncate(&c.message, 48)),
            ])
        })
        .collect();
    if rows.is_empty() {
        rows.push(Row::new(vec![
            Cell::from("-"),
            Cell::from("OK"),
            Cell::from("No P1/P2 violations in probe window"),
        ]));
    }
    let summary = format!(
        "P1={} P2={} sync={} cc={}",
        r.p1_violations, r.p2_violations, r.sync_errors, r.cc_errors,
    );
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(12),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(rounded(format!(" TR 101 290 Compliance ({summary}) ")));
    frame.render_widget(table, area);
}

fn draw_sei_panel(frame: &mut Frame, area: Rect, app: &App) {
    let s = &app.sei_probe;
    let lines = vec![
        Line::from(format!(
            " CEA-608        : {}",
            if s.cea608_present { "yes" } else { "no" }
        )),
        Line::from(format!(
            " CEA-708        : {}",
            if s.cea708_present { "yes" } else { "no" }
        )),
        Line::from(format!(
            " HDR10 (ST2086) : {}",
            if s.hdr10_present { "yes" } else { "no" }
        )),
        Line::from(format!(
            " HLG (VUI)      : {}",
            if s.hlg_present { "yes" } else { "no" }
        )),
        Line::from(format!(
            " MaxCLL/MaxFALL : {}/{}",
            s.max_cll.map_or_else(|| "-".into(), |v| v.to_string()),
            s.max_fall.map_or_else(|| "-".into(), |v| v.to_string()),
        )),
        Line::from(format!(
            " Caption lang   : {}",
            s.caption_language.as_deref().unwrap_or("-")
        )),
        Line::from(format!(" NAL units      : {}", s.nal_units_scanned)),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(rounded(" SEI / HDR / Captions ")),
        area,
    );
}

fn draw_qoe_panel(frame: &mut Frame, area: Rect, app: &App) {
    let q = &app.synthetic_qoe;
    let lines = vec![
        Line::from(format!(" TDR            : {:.3}", q.tdr)),
        Line::from(format!(" Rebuffer risk  : {} / 100", q.rebuffer_risk_score)),
        Line::from(format!(
            " TTFF           : {} ms",
            q.ttff_ms.map_or_else(|| "-".into(), |v| v.to_string())
        )),
        Line::from(format!(
            " Selected ABR   : {} bps",
            q.selected_bitrate_bps
                .map_or_else(|| "-".into(), |v| v.to_string())
        )),
        Line::from(format!(
            " Buffer 2s/4s/6s: {}% / {}% / {}%",
            q.buffer_2s_rebuffer_pct, q.buffer_4s_rebuffer_pct, q.buffer_6s_rebuffer_pct
        )),
        Line::from(format!(
            " Throttle       : {} kbps",
            q.throttle_kbps
                .map_or_else(|| "-".into(), |v| v.to_string())
        )),
        Line::from(format!(
            " Simulated RTT  : {} ms",
            q.simulated_rtt_ms
                .map_or_else(|| "-".into(), |v| v.to_string())
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(rounded(" Synthetic Player QoE ")),
        area,
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
