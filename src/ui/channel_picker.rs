//! Channel picker and in-session switcher.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::models::ChannelEntry;

pub enum PickerAction {
    None,
    Select(usize),
    Cancel,
    Quit,
}

#[derive(Debug, Clone)]
pub struct ChannelPicker {
    pub channels: Vec<ChannelEntry>,
    pub selected: usize,
    pub query: String,
    pub searching: bool,
    list_state: ListState,
}

impl ChannelPicker {
    pub fn new(channels: Vec<ChannelEntry>) -> Self {
        let mut list_state = ListState::default();
        if !channels.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            channels,
            selected: 0,
            query: String::new(),
            searching: false,
            list_state,
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        let q = self.query.to_ascii_lowercase();
        self.channels
            .iter()
            .enumerate()
            .filter(|(_, ch)| {
                if q.is_empty() {
                    return true;
                }
                ch.name.to_ascii_lowercase().contains(&q)
                    || ch
                        .group
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn sync_visual(&mut self) {
        let idx = self.filtered_indices();
        if idx.is_empty() {
            self.list_state.select(None);
            return;
        }
        let pos = idx.iter().position(|&i| i == self.selected).unwrap_or(0);
        self.selected = idx[pos];
        self.list_state.select(Some(pos));
    }

    pub fn move_by(&mut self, delta: i32) {
        let idx = self.filtered_indices();
        if idx.is_empty() {
            return;
        }
        let cur = idx.iter().position(|&i| i == self.selected).unwrap_or(0);
        let next = (cur as i32 + delta).clamp(0, idx.len() as i32 - 1) as usize;
        self.selected = idx[next];
        self.list_state.select(Some(next));
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> PickerAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return PickerAction::Quit;
        }

        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.query.clear();
                    self.sync_visual();
                    return PickerAction::None;
                }
                KeyCode::Enter => {
                    self.searching = false;
                    return self.select_current();
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.jump_first_filtered();
                    return PickerAction::None;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(c);
                    self.jump_first_filtered();
                    return PickerAction::None;
                }
                _ => return PickerAction::None,
            }
        }

        match key.code {
            KeyCode::Char('q' | 'Q') => PickerAction::Quit,
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Char('/') => {
                self.searching = true;
                PickerAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                PickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                PickerAction::None
            }
            KeyCode::PageUp => {
                self.move_by(-10);
                PickerAction::None
            }
            KeyCode::PageDown => {
                self.move_by(10);
                PickerAction::None
            }
            KeyCode::Enter => self.select_current(),
            _ => PickerAction::None,
        }
    }

    fn jump_first_filtered(&mut self) {
        let idx = self.filtered_indices();
        if let Some(&first) = idx.first() {
            self.selected = first;
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    fn select_current(&self) -> PickerAction {
        let idx = self.filtered_indices();
        if idx.is_empty() {
            return PickerAction::None;
        }
        if idx.contains(&self.selected) {
            PickerAction::Select(self.selected)
        } else {
            PickerAction::Select(idx[0])
        }
    }

    pub fn current(&self) -> Option<&ChannelEntry> {
        self.channels.get(self.selected)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, overlay: bool) {
        let block_area = if overlay {
            let popup = centered(area, 80, 80);
            frame.render_widget(Clear, popup);
            popup
        } else {
            area
        };

        let title = if overlay {
            " Channel Switcher "
        } else {
            " Channel Picker "
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(4),
                Constraint::Length(1),
            ])
            .split(block_area);

        let outer = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if overlay { Color::Magenta } else { Color::Cyan }))
            .title(title)
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(outer, block_area);

        let inner_head = shrink(chunks[0], 1);
        let inner_list = shrink(chunks[1], 1);
        let inner_detail = shrink(chunks[2], 1);
        let inner_foot = shrink(chunks[3], 1);

        let search_line = if self.searching {
            format!(" / {}", self.query)
        } else if self.query.is_empty() {
            " / search  ·  ↑↓ j/k  ·  Enter select".into()
        } else {
            format!(" filter: {}  (Esc clears)", self.query)
        };
        frame.render_widget(
            Paragraph::new(search_line).style(Style::default().fg(if self.searching {
                Color::LightYellow
            } else {
                Color::DarkGray
            })),
            inner_head,
        );

        let filtered = self.filtered_indices();
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| {
                let ch = &self.channels[i];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<28}", truncate(&ch.name, 28)),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!(" {:<14}", truncate(ch.group_label(), 14)),
                        Style::default().fg(Color::Magenta),
                    ),
                    Span::styled(
                        format!(" {}", ch.url_summary(42)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::LightGreen)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, inner_list, &mut self.list_state);

        let detail = self.current().map_or_else(
            || vec![Line::from("No channels match the filter.")],
            |ch| {
                vec![
                    Line::from(format!(" {}  [{}]", ch.name, ch.group_label())),
                    Line::from(Span::styled(
                        ch.url.clone(),
                        Style::default().fg(Color::Cyan),
                    )),
                ]
            },
        );
        frame.render_widget(Paragraph::new(detail), inner_detail);

        let n = self.channels.len();
        let shown = filtered.len();
        frame.render_widget(
            Paragraph::new(format!(
                " {shown}/{n} channels  |  Enter live diagnostics  |  Esc back"
            ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
            inner_foot,
        );
    }
}

fn shrink(area: Rect, pad: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(pad),
        y: area.y.saturating_add(pad / 2).max(area.y),
        width: area.width.saturating_sub(pad * 2),
        height: area.height.saturating_sub(pad),
    }
}

fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
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
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
