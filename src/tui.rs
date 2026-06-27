use crate::detect::{OffsetEstimate, ProfileDetection};
use crate::frequency::{format_frequency_hz, format_offset_hz};
use crate::peak::Peak;
use crate::persistence::{PersistenceSummary, StabilityState};
use crate::timeparse::format_duration;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Color, Modifier, Span, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};
use ratatui::Terminal;
use std::collections::{HashSet, VecDeque};
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

pub struct TuiScanState<'a> {
    pub pass: usize,
    pub elapsed: Duration,
    pub pass_duration: Duration,
    pub peaks: &'a [Peak],
    pub results: &'a [ProfileDetection],
    pub summaries: &'a [PersistenceSummary],
    pub hidden_suspects: &'a HashSet<String>,
    pub expanded_suspects: &'a HashSet<String>,
    pub hide_tracking: bool,
    pub selected_suspect_index: usize,
    pub focus: TuiFocus,
    pub peaks_scroll: usize,
    pub logs_scroll: usize,
    pub logs: &'a VecDeque<String>,
}

#[derive(Clone, Debug)]
pub struct TuiScanSnapshot {
    pub pass: usize,
    pub elapsed: Duration,
    pub pass_duration: Duration,
    pub peaks: Vec<Peak>,
    pub results: Vec<ProfileDetection>,
    pub summaries: Vec<PersistenceSummary>,
    pub logs: VecDeque<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiAction {
    Previous,
    Next,
    PagePrevious,
    PageNext,
    Home,
    End,
    FocusPrevious,
    FocusNext,
    ToggleSelectedDetails,
    HideSelected,
    ClearHidden,
    ToggleTracking,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiFocus {
    Suspects,
    Peaks,
    Logs,
}

impl TuiFocus {
    fn previous(self) -> Self {
        match self {
            TuiFocus::Suspects => TuiFocus::Logs,
            TuiFocus::Peaks => TuiFocus::Suspects,
            TuiFocus::Logs => TuiFocus::Peaks,
        }
    }

    fn next(self) -> Self {
        match self {
            TuiFocus::Suspects => TuiFocus::Peaks,
            TuiFocus::Peaks => TuiFocus::Logs,
            TuiFocus::Logs => TuiFocus::Suspects,
        }
    }

    fn label(self) -> &'static str {
        match self {
            TuiFocus::Suspects => "Suspects",
            TuiFocus::Peaks => "All Peaks",
            TuiFocus::Logs => "Logs",
        }
    }
}

pub struct TuiRenderer {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

pub fn run_tui_loop(receiver: Receiver<TuiScanSnapshot>, stop: Arc<AtomicBool>) -> Result<()> {
    let mut renderer = TuiRenderer::new()?;
    let mut latest = None;
    let mut hidden_suspects = HashSet::new();
    let mut expanded_suspects = HashSet::new();
    let mut hide_tracking = false;
    let mut selected_suspect_index = 0usize;
    let mut focus = TuiFocus::Suspects;
    let mut peaks_scroll = 0usize;
    let mut logs_scroll = 0usize;
    let mut disconnected = false;

    while !stop.load(Ordering::Relaxed) {
        let mut needs_redraw = false;

        loop {
            match receiver.try_recv() {
                Ok(snapshot) => {
                    latest = Some(snapshot);
                    needs_redraw = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        for action in renderer.drain_actions()? {
            match action {
                TuiAction::Previous => {
                    match focus {
                        TuiFocus::Suspects => {
                            selected_suspect_index = selected_suspect_index.saturating_sub(1);
                        }
                        TuiFocus::Peaks => {
                            peaks_scroll = peaks_scroll.saturating_sub(1);
                        }
                        TuiFocus::Logs => {
                            logs_scroll = logs_scroll.saturating_add(1);
                        }
                    }
                    needs_redraw = true;
                }
                TuiAction::Next => {
                    match focus {
                        TuiFocus::Suspects => {
                            if let Some(snapshot) = latest.as_ref() {
                                let visible = visible_summary_count(
                                    &snapshot.summaries,
                                    &hidden_suspects,
                                    hide_tracking,
                                );
                                if visible > 0 {
                                    selected_suspect_index =
                                        (selected_suspect_index + 1).min(visible - 1);
                                }
                            }
                        }
                        TuiFocus::Peaks => {
                            peaks_scroll = peaks_scroll.saturating_add(1);
                        }
                        TuiFocus::Logs => {
                            logs_scroll = logs_scroll.saturating_sub(1);
                        }
                    }
                    needs_redraw = true;
                }
                TuiAction::PagePrevious => {
                    match focus {
                        TuiFocus::Suspects => {
                            selected_suspect_index = selected_suspect_index.saturating_sub(5);
                        }
                        TuiFocus::Peaks => {
                            peaks_scroll = peaks_scroll.saturating_sub(10);
                        }
                        TuiFocus::Logs => {
                            logs_scroll = logs_scroll.saturating_add(5);
                        }
                    }
                    needs_redraw = true;
                }
                TuiAction::PageNext => {
                    match focus {
                        TuiFocus::Suspects => {
                            if let Some(snapshot) = latest.as_ref() {
                                let visible = visible_summary_count(
                                    &snapshot.summaries,
                                    &hidden_suspects,
                                    hide_tracking,
                                );
                                if visible > 0 {
                                    selected_suspect_index =
                                        (selected_suspect_index + 5).min(visible - 1);
                                }
                            }
                        }
                        TuiFocus::Peaks => {
                            peaks_scroll = peaks_scroll.saturating_add(10);
                        }
                        TuiFocus::Logs => {
                            logs_scroll = logs_scroll.saturating_sub(5);
                        }
                    }
                    needs_redraw = true;
                }
                TuiAction::Home => {
                    match focus {
                        TuiFocus::Suspects => selected_suspect_index = 0,
                        TuiFocus::Peaks => peaks_scroll = 0,
                        TuiFocus::Logs => {
                            if let Some(snapshot) = latest.as_ref() {
                                logs_scroll = snapshot.logs.len().saturating_sub(1);
                            }
                        }
                    }
                    needs_redraw = true;
                }
                TuiAction::End => {
                    match focus {
                        TuiFocus::Suspects => {
                            if let Some(snapshot) = latest.as_ref() {
                                let visible = visible_summary_count(
                                    &snapshot.summaries,
                                    &hidden_suspects,
                                    hide_tracking,
                                );
                                selected_suspect_index = visible.saturating_sub(1);
                            }
                        }
                        TuiFocus::Peaks => {
                            if let Some(snapshot) = latest.as_ref() {
                                peaks_scroll = snapshot.peaks.len().saturating_sub(1);
                            }
                        }
                        TuiFocus::Logs => logs_scroll = 0,
                    }
                    needs_redraw = true;
                }
                TuiAction::FocusPrevious => {
                    focus = focus.previous();
                    needs_redraw = true;
                }
                TuiAction::FocusNext => {
                    focus = focus.next();
                    needs_redraw = true;
                }
                TuiAction::ToggleSelectedDetails => {
                    if focus == TuiFocus::Suspects {
                        if let Some(snapshot) = latest.as_ref() {
                            if let Some(key) = selected_suspect_key(
                                selected_suspect_index,
                                &snapshot.summaries,
                                &hidden_suspects,
                                hide_tracking,
                            ) {
                                if expanded_suspects.contains(&key) {
                                    expanded_suspects.remove(&key);
                                } else {
                                    expanded_suspects.insert(key);
                                }
                                needs_redraw = true;
                            }
                        }
                    }
                }
                TuiAction::HideSelected => {
                    if focus == TuiFocus::Suspects {
                        if let Some(snapshot) = latest.as_ref() {
                            if let Some(key) = selected_suspect_key(
                                selected_suspect_index,
                                &snapshot.summaries,
                                &hidden_suspects,
                                hide_tracking,
                            ) {
                                hidden_suspects.insert(key.clone());
                                expanded_suspects.remove(&key);
                                selected_suspect_index = clamp_selected_suspect(
                                    selected_suspect_index,
                                    &snapshot.summaries,
                                    &hidden_suspects,
                                    hide_tracking,
                                );
                                needs_redraw = true;
                            }
                        }
                    }
                }
                TuiAction::ClearHidden => {
                    hidden_suspects.clear();
                    if let Some(snapshot) = latest.as_ref() {
                        selected_suspect_index = clamp_selected_suspect(
                            selected_suspect_index,
                            &snapshot.summaries,
                            &hidden_suspects,
                            hide_tracking,
                        );
                    }
                    needs_redraw = true;
                }
                TuiAction::ToggleTracking => {
                    hide_tracking = !hide_tracking;
                    if let Some(snapshot) = latest.as_ref() {
                        selected_suspect_index = clamp_selected_suspect(
                            selected_suspect_index,
                            &snapshot.summaries,
                            &hidden_suspects,
                            hide_tracking,
                        );
                    }
                    needs_redraw = true;
                }
                TuiAction::Quit => {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        if let Some(snapshot) = latest.as_ref() {
            selected_suspect_index = clamp_selected_suspect(
                selected_suspect_index,
                &snapshot.summaries,
                &hidden_suspects,
                hide_tracking,
            );
            peaks_scroll = peaks_scroll.min(snapshot.peaks.len().saturating_sub(1));
            logs_scroll = logs_scroll.min(snapshot.logs.len().saturating_sub(1));

            if needs_redraw {
                renderer.draw(&TuiScanState {
                    pass: snapshot.pass,
                    elapsed: snapshot.elapsed,
                    pass_duration: snapshot.pass_duration,
                    peaks: &snapshot.peaks,
                    results: &snapshot.results,
                    summaries: &snapshot.summaries,
                    hidden_suspects: &hidden_suspects,
                    expanded_suspects: &expanded_suspects,
                    hide_tracking,
                    selected_suspect_index,
                    focus,
                    peaks_scroll,
                    logs_scroll,
                    logs: &snapshot.logs,
                })?;
            }
        }

        if disconnected {
            break;
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

impl TuiRenderer {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }

    pub fn draw(&mut self, state: &TuiScanState<'_>) -> Result<()> {
        self.terminal.draw(|frame| {
            let area = frame.area();
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(9),
                ])
                .split(area);
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(vertical[1]);

            let header = Paragraph::new(format!(
                "camfinder  pass={}  elapsed={}  pass={}  peaks={}  hidden={}  tracking={}  focus={}  Tab focus  ↑/↓ move/scroll  e/Enter expand  h hide  t tracking  u restore  q stop",
                state.pass,
                format_duration(state.elapsed),
                format_duration(state.pass_duration),
                state.peaks.len(),
                state.hidden_suspects.len(),
                if state.hide_tracking { "hidden" } else { "shown" },
                state.focus.label()
            ))
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(header, vertical[0]);

            frame.render_widget(suspicious_panel(state, body[0]), body[0]);
            frame.render_widget(
                peaks_panel(
                    state.peaks,
                    state.peaks_scroll,
                    state.focus == TuiFocus::Peaks,
                    body[1],
                ),
                body[1],
            );
            frame.render_widget(
                logs_panel(
                    state.logs,
                    state.logs_scroll,
                    state.focus == TuiFocus::Logs,
                    vertical[2],
                ),
                vertical[2],
            );
        })?;
        Ok(())
    }

    pub fn drain_actions(&mut self) -> Result<Vec<TuiAction>> {
        let mut actions = Vec::new();
        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        actions.push(TuiAction::Previous);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        actions.push(TuiAction::Next);
                    }
                    KeyCode::PageUp => {
                        actions.push(TuiAction::PagePrevious);
                    }
                    KeyCode::PageDown => {
                        actions.push(TuiAction::PageNext);
                    }
                    KeyCode::Home => {
                        actions.push(TuiAction::Home);
                    }
                    KeyCode::End => {
                        actions.push(TuiAction::End);
                    }
                    KeyCode::Tab => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            actions.push(TuiAction::FocusPrevious);
                        } else {
                            actions.push(TuiAction::FocusNext);
                        }
                    }
                    KeyCode::BackTab => {
                        actions.push(TuiAction::FocusPrevious);
                    }
                    KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('e') => {
                        actions.push(TuiAction::ToggleSelectedDetails);
                    }
                    KeyCode::Char('h') | KeyCode::Delete | KeyCode::Backspace => {
                        actions.push(TuiAction::HideSelected);
                    }
                    KeyCode::Char('u') => {
                        actions.push(TuiAction::ClearHidden);
                    }
                    KeyCode::Char('t') => {
                        actions.push(TuiAction::ToggleTracking);
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        actions.push(TuiAction::Quit);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        actions.push(TuiAction::Quit);
                    }
                    _ => {}
                }
            }
        }
        Ok(actions)
    }
}

impl Drop for TuiRenderer {
    fn drop(&mut self) {
        let _ = self.terminal.backend_mut().execute(LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
        let _ = disable_raw_mode();
    }
}

fn panel_block(title: &'static str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title)
}

fn visible_line_count(area: Rect, reserved_lines: u16) -> usize {
    area.height.saturating_sub(reserved_lines).max(1) as usize
}

fn to_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

fn suspicious_panel(state: &TuiScanState<'_>, area: Rect) -> Paragraph<'static> {
    let mut lines = Vec::new();
    let mut selected_line = 0usize;

    for (visible_index, summary) in state
        .summaries
        .iter()
        .filter(|summary| is_summary_visible(summary, state.hidden_suspects, state.hide_tracking))
        .enumerate()
    {
        let selected = visible_index == state.selected_suspect_index;
        if selected {
            selected_line = lines.len();
        }
        let expanded = state.expanded_suspects.contains(&summary.suspect_key);
        let result = current_result_for(summary, state.results);
        let (hits, required_hits, score, min_score, snr_sum, snr_avg, spread, offset, group) =
            if let Some(result) = result {
                (
                    result.hits,
                    result.required_hits,
                    result.score,
                    result.min_score,
                    result.snr_sum_db,
                    result.snr_avg_db,
                    result.offset_spread_hz,
                    format_group_offset_value(result.offset_group_hz, result.offset),
                    harmonic_group_label(&result.profile).to_string(),
                )
            } else {
                (
                    summary.last_hits,
                    0,
                    summary.last_score,
                    summary.min_score,
                    summary.last_snr_sum_db,
                    summary.last_snr_avg_db,
                    summary.last_offset_spread_hz,
                    format_group_offset_value(summary.offset_group_hz, summary.last_offset),
                    harmonic_group_label(&summary.profile).to_string(),
                )
            };
        let required = if required_hits == 0 {
            "-".to_string()
        } else {
            required_hits.to_string()
        };
        let seen = format!(
            "{}/{} {:.0}%",
            summary.detected_passes,
            summary.total_passes,
            summary.detection_ratio * 100.0
        );
        let now = if let Some(result) = result {
            if result.detected {
                "hit".to_string()
            } else {
                "weak".to_string()
            }
        } else {
            format!("missed={}", summary.missed_passes)
        };

        let marker = if expanded { "- " } else { "+ " };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                marker,
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(
                format!(
                    "#{:02} {:<10}",
                    summary.display_order + 1,
                    stability_label(&summary.state)
                ),
                stability_style(&summary.state),
            ),
            Span::raw(format!(
                " group={:<6} offset={:<14} avg_snr={:>5.1}dB score={:>5.1} sig={}",
                group,
                offset,
                snr_avg,
                score,
                score_strength_bar(score, min_score)
            )),
        ]));

        if expanded {
            lines.push(Line::from(Span::styled(
                format!(
                    "  hits={}/{} score={:.1} snr_sum={:.1}dB spread={:.0}Hz seen={} consec={} {}",
                    hits, required, score, snr_sum, spread, seen, summary.consecutive_detected, now
                ),
                Style::default().fg(Color::Gray),
            )));

            if let Some(result) = result {
                for hit in &result.hit_details {
                    lines.push(Line::from(Span::raw(format!(
                        "  {:.3} MHz -> {}  snr={:.1}dB err={}",
                        hit.nominal_mhz,
                        format_frequency_hz(hit.observed_hz),
                        hit.snr_db,
                        format_hit_error(result, hit)
                    ))));
                }
            } else {
                lines.push(Line::from(Span::raw("  no current matching offset group")));
            }
            lines.push(Line::from(""));
        }
    }

    if lines.is_empty() {
        if state.hide_tracking
            && state.summaries.iter().any(|summary| {
                summary.state == StabilityState::Tracking
                    && !state.hidden_suspects.contains(&summary.suspect_key)
            })
        {
            lines.push(Line::from("Tracking suspects hidden. Press t to show."));
        } else if state.hidden_suspects.is_empty() {
            lines.push(Line::from("No suspects."));
        } else {
            lines.push(Line::from("All suspects hidden. Press u to restore."));
        }
    }

    let visible_lines = visible_line_count(area, 2);
    let scroll = selected_line.saturating_sub(visible_lines.saturating_sub(1));

    Paragraph::new(Text::from(lines))
        .block(panel_block("Suspects", state.focus == TuiFocus::Suspects))
        .scroll((to_u16(scroll), 0))
        .wrap(Wrap { trim: false })
}

fn current_result_for<'a>(
    summary: &PersistenceSummary,
    results: &'a [ProfileDetection],
) -> Option<&'a ProfileDetection> {
    results.iter().find(|result| {
        if result.profile != summary.profile {
            return false;
        }

        match (summary.offset_group_hz, result.offset_group_hz) {
            (Some(summary_hz), Some(result_hz)) => {
                let tolerance = summary
                    .offset_tolerance_hz
                    .max(result.offset_tolerance_hz)
                    .max(1.0);
                (summary_hz - result_hz).abs() <= tolerance
            }
            (None, None) => true,
            _ => false,
        }
    })
}

fn visible_summary_count(
    summaries: &[PersistenceSummary],
    hidden_suspects: &HashSet<String>,
    hide_tracking: bool,
) -> usize {
    summaries
        .iter()
        .filter(|summary| is_summary_visible(summary, hidden_suspects, hide_tracking))
        .count()
}

fn selected_suspect_key(
    selected_index: usize,
    summaries: &[PersistenceSummary],
    hidden_suspects: &HashSet<String>,
    hide_tracking: bool,
) -> Option<String> {
    summaries
        .iter()
        .filter(|summary| is_summary_visible(summary, hidden_suspects, hide_tracking))
        .nth(selected_index)
        .map(|summary| summary.suspect_key.clone())
}

fn clamp_selected_suspect(
    selected_index: usize,
    summaries: &[PersistenceSummary],
    hidden_suspects: &HashSet<String>,
    hide_tracking: bool,
) -> usize {
    let visible = visible_summary_count(summaries, hidden_suspects, hide_tracking);
    if visible == 0 {
        0
    } else {
        selected_index.min(visible - 1)
    }
}

fn is_summary_visible(
    summary: &PersistenceSummary,
    hidden_suspects: &HashSet<String>,
    hide_tracking: bool,
) -> bool {
    !(hidden_suspects.contains(&summary.suspect_key)
        || hide_tracking && summary.state == StabilityState::Tracking)
}

fn peaks_panel(peaks: &[Peak], scroll: usize, focused: bool, area: Rect) -> Table<'static> {
    let max_rows = visible_line_count(area, 3);
    let rows = peaks.iter().rev().skip(scroll).take(max_rows).map(|peak| {
        Row::new(vec![
            Cell::from(format!("#{}", peak.id)),
            Cell::from(format_frequency_hz(peak.freq_hz)),
            Cell::from(format!("{:.1}", peak.snr_db())),
            Cell::from(peak.source.clone().unwrap_or_else(|| "-".to_string())),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(18),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec!["id", "freq", "snr", "source"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(panel_block("All Peaks", focused))
}

fn logs_panel(
    logs: &VecDeque<String>,
    scroll: usize,
    focused: bool,
    area: Rect,
) -> Paragraph<'static> {
    let max_lines = visible_line_count(area, 2);
    let lines = logs
        .iter()
        .rev()
        .skip(scroll)
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();

    Paragraph::new(Text::from(lines))
        .block(panel_block("Logs", focused))
        .wrap(Wrap { trim: false })
}

fn stability_label(state: &StabilityState) -> &'static str {
    match state {
        StabilityState::Stable => "stable",
        StabilityState::Candidate => "candidate",
        StabilityState::Tracking => "tracking",
        StabilityState::Transient => "transient",
        StabilityState::Absent => "absent",
    }
}

fn stability_style(state: &StabilityState) -> Style {
    match state {
        StabilityState::Stable => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        StabilityState::Candidate => Style::default().fg(Color::Yellow),
        StabilityState::Tracking => Style::default().fg(Color::Cyan),
        StabilityState::Transient => Style::default().fg(Color::Magenta),
        StabilityState::Absent => Style::default().fg(Color::DarkGray),
    }
}

fn format_offset_value(offset: Option<OffsetEstimate>) -> String {
    match offset {
        Some(OffsetEstimate::FixedHz { hz }) => format_offset_hz(hz),
        Some(OffsetEstimate::Ppm { ppm }) => format!("{ppm:+.3} ppm"),
        None => "-".to_string(),
    }
}

fn format_group_offset_value(
    offset_group_hz: Option<f64>,
    fallback: Option<OffsetEstimate>,
) -> String {
    match fallback {
        Some(OffsetEstimate::Ppm { .. }) => format_offset_value(fallback),
        _ => offset_group_hz
            .map(format_offset_hz)
            .unwrap_or_else(|| format_offset_value(fallback)),
    }
}

fn harmonic_group_label(profile: &str) -> &str {
    profile
        .rsplit_once(':')
        .map(|(_, group)| group)
        .unwrap_or(profile)
}

fn score_strength_bar(score: f64, min_score: f64) -> String {
    const WIDTH: usize = 10;
    const FULL_SCALE_MIN_SCORE_FACTOR: f64 = 12.0;

    let upper = (min_score * FULL_SCALE_MIN_SCORE_FACTOR).max(1.0);
    let filled = ((score.max(0.0) / upper).clamp(0.0, 1.0) * WIDTH as f64).round() as usize;

    format!("[{}{}]", "#".repeat(filled), "-".repeat(WIDTH - filled))
}

fn format_hit_error(result: &ProfileDetection, hit: &crate::detect::DetectionHit) -> String {
    match result.offset {
        Some(OffsetEstimate::Ppm { .. }) => {
            format!(
                "{:+.3} ppm ({})",
                hit.error_hz / hit.nominal_mhz,
                format_offset_hz(hit.error_hz)
            )
        }
        _ => format_offset_hz(hit.error_hz),
    }
}
