/*
BSD 2-Clause License

Copyright (c) 2026, Mike Larkin <mlarkin@nested.page>

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
INCLUDING NEGLIGENCE OR OTHERWISE ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

use crate::config::Config;
use crate::mbox::MessageSummary;
use crate::review::{self, MailboxKind};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Frame, Terminal};
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Preview,
    Reason,
}

struct App {
    config: Config,
    active: MailboxKind,
    inbox: Vec<MessageSummary>,
    spam: Vec<MessageSummary>,
    inbox_state: TableState,
    spam_state: TableState,
    mode: InputMode,
    preview: String,
    preview_scroll: u16,
    reason: String,
    status: String,
}

/// Runs the local terminal review interface.
///
/// Preconditions: stdin/stdout are attached to an interactive terminal. This
/// function owns raw-mode and alternate-screen setup while running and attempts
/// to restore both before returning.
pub fn run(config_path: Option<&Path>) -> Result<(), String> {
    let config = review::load_config(config_path)?;
    let mut app = App::new(config)?;

    enable_raw_mode().map_err(|err| format!("failed to enable raw mode: {err}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|err| format!("failed to enter alternate screen: {err}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|err| format!("failed to create terminal: {err}"))?;

    let result = run_loop(&mut terminal, &mut app);

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

/// Draws the current app state, handles one key event at a time, and returns
/// only when the user quits or a terminal operation fails.
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<(), String> {
    loop {
        terminal
            .draw(|frame| draw(frame, app))
            .map_err(|err| format!("failed to draw TUI: {err}"))?;

        if event::poll(Duration::from_millis(200))
            .map_err(|err| format!("failed to poll terminal event: {err}"))?
        {
            let Event::Key(key) =
                event::read().map_err(|err| format!("failed to read terminal event: {err}"))?
            else {
                continue;
            };

            if handle_key(app, key)? {
                return Ok(());
            }
        }
    }
}

impl App {
    fn new(config: Config) -> Result<Self, String> {
        let mut app = Self {
            config,
            active: MailboxKind::Inbox,
            inbox: Vec::new(),
            spam: Vec::new(),
            inbox_state: TableState::default(),
            spam_state: TableState::default(),
            mode: InputMode::Normal,
            preview: String::new(),
            preview_scroll: 0,
            reason: String::new(),
            status: String::new(),
        };
        app.refresh()?;
        Ok(app)
    }

    /// Reloads both mailbox lists from disk and clamps selections to valid
    /// indexes. Must be called after moves because scan ids are ephemeral.
    fn refresh(&mut self) -> Result<(), String> {
        self.inbox = review::summaries(&self.config, MailboxKind::Inbox)?;
        self.spam = review::summaries(&self.config, MailboxKind::Spam)?;
        clamp_state(&mut self.inbox_state, self.inbox.len());
        clamp_state(&mut self.spam_state, self.spam.len());
        self.status = format!(
            "ok: loaded {} inbox / {} spam messages",
            self.inbox.len(),
            self.spam.len()
        );
        Ok(())
    }

    fn active_summaries(&self) -> &[MessageSummary] {
        match self.active {
            MailboxKind::Inbox => &self.inbox,
            MailboxKind::Spam => &self.spam,
        }
    }

    fn active_state(&self) -> &TableState {
        match self.active {
            MailboxKind::Inbox => &self.inbox_state,
            MailboxKind::Spam => &self.spam_state,
        }
    }

    fn active_state_mut(&mut self) -> &mut TableState {
        match self.active {
            MailboxKind::Inbox => &mut self.inbox_state,
            MailboxKind::Spam => &mut self.spam_state,
        }
    }

    fn selected(&self) -> Option<&MessageSummary> {
        self.active_state()
            .selected()
            .and_then(|index| self.active_summaries().get(index))
    }

    /// Moves the selected message to the opposite mailbox using the shared
    /// correction-aware review path, then refreshes both mailbox lists.
    fn move_selected(&mut self) -> Result<(), String> {
        let Some(id) = self.selected().map(|summary| summary.id) else {
            self.status = "no message selected".to_string();
            return Ok(());
        };
        let from = self.active;
        let to = self.active.other();
        let subject = review::move_between(&self.config, from, id, to, self.reason.trim())?.subject;
        self.reason.clear();
        self.mode = InputMode::Normal;
        self.refresh()?;
        self.status = format!("ok: moved {from} message {id} to {to}: {}", compact(&subject, 80));
        Ok(())
    }

    /// Loads the selected message into the preview buffer. Read failures are
    /// shown in the pane rather than unwinding the whole TUI.
    fn update_preview(&mut self) {
        let Some(id) = self.selected().map(|summary| summary.id) else {
            self.preview = "No message selected.".to_string();
            return;
        };
        self.preview = match review::read(&self.config, self.active, id) {
            Ok(raw) => preview_text(&raw),
            Err(err) => format!("Failed to read message: {err}"),
        };
        self.preview_scroll = 0;
    }
}

impl std::fmt::Display for MailboxKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool, String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }

    match app.mode {
        InputMode::Reason => handle_reason_key(app, key),
        InputMode::Preview => handle_preview_key(app, key),
        InputMode::Normal => handle_normal_key(app, key),
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent) -> Result<bool, String> {
    match key.code {
        KeyCode::Char('q') => Ok(true),
        KeyCode::Tab => {
            app.active = app.active.other();
            Ok(false)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_selection(app, 1);
            Ok(false)
        }
        KeyCode::Up | KeyCode::Char('k') => {
            move_selection(app, -1);
            Ok(false)
        }
        KeyCode::PageDown => {
            move_selection(app, page_step(app));
            Ok(false)
        }
        KeyCode::PageUp => {
            move_selection(app, -page_step(app));
            Ok(false)
        }
        KeyCode::Enter => {
            app.update_preview();
            app.mode = InputMode::Preview;
            Ok(false)
        }
        KeyCode::Char('m') => {
            if app.selected().is_some() {
                app.reason.clear();
                app.mode = InputMode::Reason;
                app.status = format!(
                    "moving selected {} message to {}",
                    app.active,
                    app.active.other()
                );
            } else {
                app.status = "no message selected".to_string();
            }
            Ok(false)
        }
        KeyCode::Char('r') => {
            app.refresh()?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn handle_preview_key(app: &mut App, key: KeyEvent) -> Result<bool, String> {
    match key.code {
        KeyCode::Char('q') => {
            app.mode = InputMode::Normal;
            Ok(false)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.preview_scroll = app.preview_scroll.saturating_add(1);
            Ok(false)
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.preview_scroll = app.preview_scroll.saturating_sub(1);
            Ok(false)
        }
        KeyCode::PageDown => {
            app.preview_scroll = app.preview_scroll.saturating_add(12);
            Ok(false)
        }
        KeyCode::PageUp => {
            app.preview_scroll = app.preview_scroll.saturating_sub(12);
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn handle_reason_key(app: &mut App, key: KeyEvent) -> Result<bool, String> {
    match key.code {
        KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.reason.clear();
            app.status = "move cancelled".to_string();
            Ok(false)
        }
        KeyCode::Enter => {
            app.move_selected()?;
            Ok(false)
        }
        KeyCode::Backspace => {
            app.reason.pop();
            Ok(false)
        }
        KeyCode::Char(ch) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                app.reason.push(ch);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn move_selection(app: &mut App, delta: isize) {
    let len = app.active_summaries().len();
    if len == 0 {
        app.active_state_mut().select(None);
        return;
    }
    let current = app.active_state().selected().unwrap_or(0);
    let next = if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        (current + delta as usize).min(len - 1)
    };
    app.active_state_mut().select(Some(next));
}

fn page_step(app: &App) -> isize {
    let len = app.active_summaries().len();
    if len < 2 {
        1
    } else {
        len.min(12) as isize
    }
}

fn clamp_state(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let selected = state.selected().unwrap_or(0).min(len - 1);
    state.select(Some(selected));
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, root[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(root[1]);

    draw_mailboxes(frame, body[0], app);
    draw_detail(frame, body[1], app);
    draw_footer(frame, root[2], app);

    if app.mode == InputMode::Reason {
        draw_reason_popup(frame, centered_rect(70, 25, frame.area()), app);
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = Line::from(vec![
        Span::styled(" SLAC ", Style::default().fg(Color::Black).bg(Color::Cyan).bold()),
        Span::raw(" Spam Limiter And Classifier "),
        Span::styled("-", Color::Green),
        Span::raw(format!(" active: {} ", app.active.as_str())),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn draw_mailboxes(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_table(
        frame,
        chunks[0],
        "Inbox",
        MailboxKind::Inbox,
        &app.inbox,
        &mut app.inbox_state,
        app.active == MailboxKind::Inbox,
    );
    draw_table(
        frame,
        chunks[1],
        "Spam",
        MailboxKind::Spam,
        &app.spam,
        &mut app.spam_state,
        app.active == MailboxKind::Spam,
    );
}

fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    mailbox: MailboxKind,
    summaries: &[MessageSummary],
    state: &mut TableState,
    active: bool,
) {
    let border = if active { Color::Cyan } else { Color::DarkGray };
    let rows = summaries.iter().map(|summary| {
        let verdict_style = match summary.slac_verdict.as_str() {
            "spam" => Style::default().fg(Color::Red).bold(),
            "ham" => Style::default().fg(Color::Green),
            "unsure" => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Gray),
        };
        Row::new(vec![
            Cell::from(summary.id.to_string()),
            Cell::from(summary.slac_probability.clone()).style(verdict_style),
            Cell::from(summary.slac_verdict.clone()).style(verdict_style),
            Cell::from(compact(&summary.from, 32)),
            Cell::from(compact(&summary.subject, 64)),
        ])
    });

    let count = summaries.len();
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Percentage(30),
            Constraint::Percentage(45),
        ],
    )
    .header(
        Row::new(["id", "prob", "verdict", "from", "subject"])
            .style(Style::default().fg(Color::Cyan).bold()),
    )
    .block(
        Block::default()
            .title(format!(" {title} - {} - {count} ", mailbox.as_str()))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
    .highlight_symbol("> ");

    frame.render_stateful_widget(table, area, state);
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let selected = app.selected();
    let mut lines = Vec::new();
    if let Some(summary) = selected {
        lines.push(Line::from(vec![
            Span::styled("Subject ", Color::Cyan),
            Span::raw(compact(&summary.subject, 140)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("From    ", Color::Cyan),
            Span::raw(compact(&summary.from, 140)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Date    ", Color::Cyan),
            Span::raw(compact(&summary.date, 120)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("SLAC    ", Color::Cyan),
            Span::raw(slac_detail(summary)),
        ]));
        if let Some(correction) = correction_detail(app) {
            lines.push(Line::from(vec![
                Span::styled("Correct ", Color::Yellow),
                Span::raw(correction),
            ]));
        }
        lines.push(Line::raw(""));
        if app.mode == InputMode::Preview {
            lines.extend(app.preview.lines().take(200).map(Line::raw));
        } else {
            lines.push(Line::styled("Enter", Color::Yellow));
            lines.push(Line::raw("open message preview"));
        }
    } else {
        lines.push(Line::raw("No message selected."));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .scroll(if app.mode == InputMode::Preview {
                (app.preview_scroll, 0)
            } else {
                (0, 0)
            })
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Detail ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta)),
            ),
        area,
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let help = match app.mode {
        InputMode::Normal => "Tab switch  Up/Down j/k select  Enter preview  m move  r refresh  q quit",
        InputMode::Preview => "Up/Down j/k scroll  PgUp/PgDn page  q close preview",
        InputMode::Reason => "Enter confirm move  Esc cancel  type reason",
    };
    let text = vec![
        Line::from(vec![Span::styled(" keys ", Color::Cyan), Span::raw(help)]),
        Line::from(vec![Span::styled(" status ", Color::Green), Span::raw(&app.status)]),
    ];
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

fn draw_reason_popup(frame: &mut Frame<'_>, area: Rect, app: &App) {
    frame.render_widget(Clear, area);
    let destination = app.active.other();
    let lines = vec![
        Line::from(vec![
            Span::styled("Move to ", Color::Cyan),
            Span::styled(destination.as_str(), Color::Yellow).bold(),
        ]),
        Line::raw(""),
        Line::raw("Reason:"),
        Line::styled(format!("{}█", app.reason), Color::White),
        Line::raw(""),
        Line::styled("Enter confirms - Esc cancels", Color::DarkGray),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Correction ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn preview_text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .lines()
        .take(400)
        .map(|line| compact(line, 240))
        .collect::<Vec<_>>()
        .join("\n")
}

fn correction_detail(app: &App) -> Option<String> {
    let id = app.selected()?.id;
    let raw = review::read(&app.config, app.active, id).ok()?;
    let corrected = crate::mail_headers::header_value(&raw, "X-SLAC-User-Correction")?;
    let reason = crate::mail_headers::header_value(&raw, "X-SLAC-Correction-Reason")
        .unwrap_or_else(|| "no reason provided".to_string());
    Some(format!(
        "{} - {}",
        compact(&corrected, 24),
        compact(&reason, 120)
    ))
}

fn slac_detail(summary: &MessageSummary) -> String {
    if summary.slac_verdict.is_empty()
        && summary.slac_probability.is_empty()
        && summary.slac_action.is_empty()
    {
        return "not processed".to_string();
    }

    format!(
        "{} / {} / {}",
        fallback(&summary.slac_verdict, "unknown"),
        fallback(&summary.slac_probability, "unknown"),
        fallback(&summary.slac_action, "unknown")
    )
}

fn fallback<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn compact(value: &str, max_chars: usize) -> String {
    let mut compacted = String::new();
    let mut last_space = false;
    for ch in value.chars().take(max_chars) {
        if ch.is_whitespace() {
            if !last_space {
                compacted.push(' ');
            }
            last_space = true;
        } else {
            compacted.push(ch);
            last_space = false;
        }
    }
    compacted.trim().to_string()
}
