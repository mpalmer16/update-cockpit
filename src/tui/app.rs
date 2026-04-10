use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{Local, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{Frame, Terminal};

use crate::catalog::{Catalog, ExecutionPlan};
use crate::persistence::{HistoryEntry, PersistedState, PersistenceStore};
use crate::runner::{OutcomeStatus, RunOptions, Runner, RunnerEvent};
use crate::tui::state::{AppState, CompletedRun, Screen, TaskItem, TaskState};

enum AppEvent {
    Runner(RunnerEvent),
    RunFinished(CompletedRun),
}

pub fn run(root: PathBuf, catalog: Catalog, options: RunOptions) -> Result<()> {
    let store = store_for_root(&root);
    let (persisted, initial_load_error) = match store.load() {
        Ok(state) => (state, None),
        Err(error) => (PersistedState::default(), Some(error.to_string())),
    };

    let mut terminal = setup_terminal()?;
    let result = run_loop(
        &mut terminal,
        root,
        catalog,
        options,
        store,
        persisted,
        initial_load_error,
    );
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    root: PathBuf,
    catalog: Catalog,
    options: RunOptions,
    store: PersistenceStore,
    persisted: PersistedState,
    initial_load_error: Option<String>,
) -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let mut state = AppState::new(catalog, options, persisted);
    if let Some(error) = initial_load_error {
        state.set_status_message(format!("State load failed: {error}"));
    }

    loop {
        let events_changed_state = drain_app_events(&event_rx, &mut state);
        if events_changed_state {
            persist_state(&store, &mut state);
        }

        terminal.draw(|frame| render(frame, &state))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(key, &root, &event_tx, &mut state)?
                {
                    break;
                }
                persist_state(&store, &mut state);
            }
        }
    }

    Ok(())
}

fn handle_key(
    key: KeyEvent,
    root: &PathBuf,
    event_tx: &Sender<AppEvent>,
    state: &mut AppState,
) -> Result<bool> {
    match state.screen() {
        Screen::Select => match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => state.move_next(),
            KeyCode::Up | KeyCode::Char('k') => state.move_previous(),
            KeyCode::Char(' ') => state.toggle_current(),
            KeyCode::Char('a') => state.select_all(),
            KeyCode::Char('x') => state.clear_selection(),
            KeyCode::Char('d') => state.toggle_dry_run(),
            KeyCode::Char('v') => state.toggle_verbose(),
            KeyCode::Char('c') => state.toggle_brew_cleanup(),
            KeyCode::Char('u') => state.toggle_npm_audit(),
            KeyCode::Char('p') | KeyCode::Tab => state.cycle_profile_next(),
            KeyCode::BackTab => state.cycle_profile_previous(),
            KeyCode::Char('P') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                state.cycle_profile_previous()
            }
            KeyCode::Enter => match state.prepare_run() {
                Ok(Some(plan)) => spawn_run(root.clone(), plan, state, event_tx.clone()),
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            _ => {}
        },
        Screen::ConfirmDangerous => match key.code {
            KeyCode::Char('y') => {
                if let Some(plan) = state.confirm_run() {
                    spawn_run(root.clone(), plan, state, event_tx.clone());
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => state.cancel_confirmation(),
            _ => {}
        },
        Screen::Running => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                state.set_status_message("Wait for the current run to finish.");
            }
            _ => {}
        },
        Screen::Summary => match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Enter => state.reset_after_summary(),
            KeyCode::Char('r') => match state.rerun_failed() {
                Ok(Some(plan)) => spawn_run(root.clone(), plan, state, event_tx.clone()),
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            KeyCode::Char('l') => match state.rerun_last_profile() {
                Ok(Some(plan)) => spawn_run(root.clone(), plan, state, event_tx.clone()),
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            _ => {}
        },
    }

    Ok(false)
}

fn spawn_run(root: PathBuf, plan: ExecutionPlan, state: &AppState, tx: Sender<AppEvent>) {
    let selected_tasks = plan
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let profile_id = state.active_profile_id().to_string();
    let options = state.options();
    let started_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    std::thread::spawn(move || {
        let started = Instant::now();
        let runner = Runner::new(root);
        let mut sink = |event| {
            let _ = tx.send(AppEvent::Runner(event));
        };
        let result = runner
            .run_with_events(&plan, &options, &mut sink)
            .map_err(|error| error.to_string());
        let _ = tx.send(AppEvent::RunFinished(CompletedRun {
            started_at_unix_secs,
            duration_ms: started.elapsed().as_millis() as u64,
            profile_id,
            selected_tasks,
            result,
        }));
    });
}

fn drain_app_events(rx: &Receiver<AppEvent>, state: &mut AppState) -> bool {
    let mut changed = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::Runner(event) => state.handle_runner_event(event),
            AppEvent::RunFinished(result) => {
                state.finish_run(result);
                changed = true;
            }
        }
    }
    changed
}

fn persist_state(store: &PersistenceStore, state: &mut AppState) {
    if !state.is_dirty() {
        return;
    }

    match store.save(&state.snapshot()) {
        Ok(()) => state.mark_clean(),
        Err(error) => state.set_status_message(format!("State save failed: {error}")),
    }
}

fn render(frame: &mut Frame, state: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(14),
            Constraint::Length(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_title(frame, layout[0], state);
    render_main(frame, layout[1], state);
    render_logs(frame, layout[2], state);
    render_footer(frame, layout[3], state);

    if state.screen() == Screen::ConfirmDangerous {
        render_confirmation(frame, centered_rect(60, 30, frame.area()));
    }
}

fn render_title(frame: &mut Frame, area: Rect, state: &AppState) {
    let titles = ["Select", "Confirm", "Running", "Summary"]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let active = match state.screen() {
        Screen::Select => 0,
        Screen::ConfirmDangerous => 1,
        Screen::Running => 2,
        Screen::Summary => 3,
    };
    let title = format!(" Upgrade Cockpit [{}] ", state.active_profile().label);
    let tabs = Tabs::new(titles)
        .select(active)
        .block(
            Block::default()
                .title(title)
                .title_alignment(ratatui::layout::Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::Gray))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(tabs, area);
}

fn render_main(frame: &mut Frame, area: Rect, state: &AppState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_tasks(frame, columns[0], state);
    render_detail(frame, columns[1], state);
}

fn render_tasks(frame: &mut Frame, area: Rect, state: &AppState) {
    let items = state
        .tasks()
        .iter()
        .map(|task| ListItem::new(task_line(task)))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Tasks ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_symbol(">> ")
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(28, 36, 49))
                .add_modifier(Modifier::BOLD),
        );
    let mut list_state = ListState::default().with_selected(Some(state.selected_index()));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn task_line(task: &TaskItem) -> Line<'static> {
    let checkbox = if task.selected { "[x]" } else { "[ ]" };
    let danger = if task.dangerous { " !" } else { "" };
    Line::from(vec![
        Span::styled(
            format!("{checkbox} "),
            Style::default().fg(if task.selected {
                Color::LightGreen
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw(task.label.clone()),
        Span::styled(
            format!("  {}{danger}", task.state.label()),
            task_state_style(task.state),
        ),
    ])
}

fn render_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    match state.screen() {
        Screen::Summary => render_summary(frame, area, state),
        _ => render_select_detail(frame, area, state),
    }
}

fn render_select_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Min(6),
        ])
        .split(area);

    render_task_detail(frame, sections[0], state);
    render_profile_panel(frame, sections[1], state);
    render_history_panel(frame, sections[2], state.history());
}

fn render_task_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let task = state.selected_task();
    let dependencies = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies.join(", ")
    };
    let flags = vec![
        flag_span("dry-run", state.options().dry_run),
        flag_span("verbose", state.options().verbose),
        flag_span("brew-cleanup", state.options().brew_cleanup),
        flag_span("npm-audit", state.options().npm_audit),
    ];

    let lines = vec![
        Line::from(vec![
            Span::styled(task.label.clone(), Style::default().fg(Color::Cyan).bold()),
            Span::raw(" "),
            Span::styled(
                if task.dangerous { "dangerous" } else { "safe" },
                if task.dangerous {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]),
        Line::raw(""),
        Line::raw(task.description.clone()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Dependencies: ", Style::default().fg(Color::Gray)),
            Span::raw(dependencies),
        ]),
        Line::raw(""),
        Line::from(flags),
    ];

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Detail ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_profile_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let profile = state.active_profile();
    let selections = if profile.selected_tasks.is_empty() {
        "none".to_string()
    } else {
        profile.selected_tasks.join(", ")
    };

    let paragraph = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                profile.label.clone(),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw(" "),
            Span::styled(profile.id.clone(), Style::default().fg(Color::Gray)),
        ]),
        Line::raw(""),
        Line::raw(profile.description.clone()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Selection: ", Style::default().fg(Color::Gray)),
            Span::raw(selections),
        ]),
    ])
    .block(
        Block::default()
            .title(" Profile ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_history_panel(frame: &mut Frame, area: Rect, history: &[HistoryEntry]) {
    let mut lines = Vec::new();
    if history.is_empty() {
        lines.push(Line::styled(
            "No runs recorded yet.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for entry in history.iter().rev().take(5) {
            lines.push(Line::from(vec![
                Span::styled(format_history_time(entry), Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(entry.profile_id.clone(), Style::default().fg(Color::White)),
                Span::raw("  "),
                Span::styled(
                    format!(
                        "{} / {} / {}",
                        entry.summary.ok_count, entry.summary.warn_count, entry.summary.fail_count
                    ),
                    outcome_style(entry.summary.overall_status()),
                ),
                Span::raw(format!("  {} ms", entry.duration_ms)),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Recent Runs ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_summary(frame: &mut Frame, area: Rect, state: &AppState) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(6)])
        .split(area);

    let mut lines = Vec::new();
    if let Some(summary) = state.summary() {
        lines.push(Line::from(vec![
            Span::styled("OK ", Style::default().fg(Color::Green).bold()),
            Span::raw(summary.ok_count.to_string()),
            Span::raw("   "),
            Span::styled("WARN ", Style::default().fg(Color::Yellow).bold()),
            Span::raw(summary.warn_count.to_string()),
            Span::raw("   "),
            Span::styled("FAIL ", Style::default().fg(Color::Red).bold()),
            Span::raw(summary.fail_count.to_string()),
        ]));
        lines.push(Line::raw(""));
        for outcome in &summary.outcomes {
            lines.push(Line::from(vec![
                Span::raw(format!("{} ", outcome.label)),
                Span::styled(outcome.status.label(), outcome_style(outcome.status)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "Enter return to selection   r rerun failed   l rerun last profile   q quit",
        ));
    } else if let Some(message) = state.status_message() {
        lines.push(Line::raw(message.to_string()));
        lines.push(Line::raw(""));
        lines.push(Line::raw("Enter return to selection   q quit"));
    }

    let summary = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Summary ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(summary, sections[0]);
    render_history_panel(frame, sections[1], state.history());
}

fn render_logs(frame: &mut Frame, area: Rect, state: &AppState) {
    let lines = if state.logs().is_empty() {
        vec![Line::styled(
            "No output yet.",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        state
            .logs()
            .iter()
            .rev()
            .take((area.height.saturating_sub(2)) as usize)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(Line::raw)
            .collect::<Vec<_>>()
    };

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Live Log ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let message = state.status_message().unwrap_or(match state.screen() {
        Screen::Select => {
            "j/k move   space toggle   p next profile   shift+p prev   d/v/c/u flags   enter run   q quit"
        }
        Screen::ConfirmDangerous => "Dangerous tasks need confirmation.",
        Screen::Running => "Task output streams in real time. Quit is disabled while running.",
        Screen::Summary => "Review the summary, rerun what you need, or return to selection.",
    });

    let paragraph = Paragraph::new(message)
        .style(Style::default().fg(Color::White).bg(Color::Rgb(28, 36, 49)))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_confirmation(frame: &mut Frame, area: Rect) {
    let paragraph = Paragraph::new(vec![
        Line::styled(
            "Dangerous tasks selected",
            Style::default().fg(Color::Yellow).bold(),
        ),
        Line::raw(""),
        Line::raw("This run includes at least one task marked dangerous."),
        Line::raw("Flutter currently resets local SDK changes before upgrading."),
        Line::raw(""),
        Line::raw("Press y to continue or n to cancel."),
    ])
    .block(
        Block::default()
            .title(" Confirm Run ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn task_state_style(state: TaskState) -> Style {
    match state {
        TaskState::Pending => Style::default().fg(Color::Gray),
        TaskState::Running => Style::default().fg(Color::Cyan).bold(),
        TaskState::Ok => Style::default().fg(Color::Green).bold(),
        TaskState::Warn => Style::default().fg(Color::Yellow).bold(),
        TaskState::Fail => Style::default().fg(Color::Red).bold(),
    }
}

fn outcome_style(status: OutcomeStatus) -> Style {
    match status {
        OutcomeStatus::Ok => Style::default().fg(Color::Green).bold(),
        OutcomeStatus::Warn => Style::default().fg(Color::Yellow).bold(),
        OutcomeStatus::Fail => Style::default().fg(Color::Red).bold(),
    }
}

fn flag_span(label: &'static str, enabled: bool) -> Span<'static> {
    if enabled {
        Span::styled(
            format!("{label}:on  "),
            Style::default().fg(Color::Black).bg(Color::LightGreen),
        )
    } else {
        Span::styled(
            format!("{label}:off  "),
            Style::default().fg(Color::Gray).bg(Color::Rgb(28, 36, 49)),
        )
    }
}

fn format_history_time(entry: &HistoryEntry) -> String {
    Local
        .timestamp_opt(entry.started_at_unix_secs as i64, 0)
        .single()
        .map(|time| time.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| entry.started_at_unix_secs.to_string())
}

fn store_for_root(root: &std::path::Path) -> PersistenceStore {
    PersistenceStore::new(root.join(".upgrade-cockpit").join("state.toml"))
}
