use std::path::{Path, PathBuf};
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

use crate::catalog::{Catalog, ExecutionPlan, MissingRequirementPolicy};
use crate::persistence::{HistoryEntry, PersistedState, PersistenceStore};
use crate::runner::{OutcomeStatus, RunOptions, Runner, RunnerEvent};
use crate::tui::state::{
    AppState, AvailabilityState, CompletedRun, Screen, TaskItem, TaskListEntry, TaskState,
};

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
    deactivate_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    let stdout = std::io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    activate_terminal(&mut terminal)?;
    Ok(terminal)
}

fn activate_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

fn deactivate_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
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
    let mut state = AppState::new(catalog, options, persisted);
    if let Some(error) = initial_load_error {
        state.set_status_message(format!("State load failed: {error}"));
    }

    loop {
        terminal.draw(|frame| render(frame, &state))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind == KeyEventKind::Press
                && handle_key(key, terminal, &root, &store, &mut state)?
            {
                break;
            }
            persist_state(&store, &mut state);
        }
    }

    Ok(())
}

fn handle_key(
    key: KeyEvent,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    root: &Path,
    store: &PersistenceStore,
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
            KeyCode::Char('f') => state.cycle_scope_filter(),
            KeyCode::Char('g') => state.toggle_selected_category_filter(),
            KeyCode::Char('t') => {
                if let Err(error) = state.toggle_selected_tag_filter() {
                    state.set_status_message(error.to_string());
                }
            }
            KeyCode::Char('z') => state.clear_filters(),
            KeyCode::Enter => match state.prepare_run() {
                Ok(Some(plan)) => run_selected_tasks(terminal, root, store, plan, state)?,
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            _ => {}
        },
        Screen::ConfirmDangerous => match key.code {
            KeyCode::Char('y') => {
                if let Some(plan) = state.confirm_run() {
                    run_selected_tasks(terminal, root, store, plan, state)?;
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
                Ok(Some(plan)) => run_selected_tasks(terminal, root, store, plan, state)?,
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            KeyCode::Char('l') => match state.rerun_last_profile() {
                Ok(Some(plan)) => run_selected_tasks(terminal, root, store, plan, state)?,
                Ok(None) => {}
                Err(error) => state.set_status_message(error.to_string()),
            },
            _ => {}
        },
    }

    Ok(false)
}

fn run_selected_tasks(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    root: &Path,
    store: &PersistenceStore,
    plan: ExecutionPlan,
    state: &mut AppState,
) -> Result<()> {
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
    let started = Instant::now();

    persist_state(store, state);
    deactivate_terminal(terminal)?;

    println!("upgrade-cockpit released the terminal for task output.");
    println!("The summary screen will return when the run finishes.");
    println!();

    let runner = Runner::new(root.to_path_buf());
    let result = {
        let mut sink = |event| {
            print_runner_event(&event);
            state.handle_runner_event(event);
        };
        runner
            .run_interactive_with_events(&plan, &options, &mut sink)
            .map_err(|error| error.to_string())
    };

    activate_terminal(terminal)?;

    state.finish_run(CompletedRun {
        started_at_unix_secs,
        duration_ms: started.elapsed().as_millis() as u64,
        profile_id,
        selected_tasks,
        result,
    });
    persist_state(store, state);
    Ok(())
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

fn print_runner_event(event: &RunnerEvent) {
    match event {
        RunnerEvent::TaskStarted { label, .. } => {
            println!("==> {label}");
        }
        RunnerEvent::OutputLine { stream, line, .. } => match stream {
            crate::runner::StreamKind::Stdout => println!("{line}"),
            crate::runner::StreamKind::Stderr => eprintln!("{line}"),
        },
        RunnerEvent::TaskFinished { label, status, .. } => {
            println!("{label} finished: {}", status.label());
            println!();
        }
    }
}

fn render(frame: &mut Frame, state: &AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(16),
            Constraint::Length(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_title(frame, layout[0], state);
    render_main(frame, layout[1], state);
    render_logs(frame, layout[2], state);
    render_footer(frame, layout[3], state);

    if state.screen() == Screen::ConfirmDangerous {
        render_confirmation(frame, centered_rect(68, 45, frame.area()), state);
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
    let title = format!(
        " Upgrade Cockpit [{}] [{}] ",
        state.active_profile().label,
        state.filter_summary()
    );
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
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    render_tasks(frame, columns[0], state);
    render_detail(frame, columns[1], state);
}

fn render_tasks(frame: &mut Frame, area: Rect, state: &AppState) {
    let entries = state.task_list_entries();
    let items = if entries.is_empty() {
        vec![ListItem::new(Line::styled(
            " No tasks match the current filters. ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))]
    } else {
        entries
            .iter()
            .map(|entry| match entry {
                TaskListEntry::Header(category) => ListItem::new(Line::styled(
                    format!(" {} ", category),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(143, 185, 177))
                        .add_modifier(Modifier::BOLD),
                )),
                TaskListEntry::Task(index) => ListItem::new(task_line(&state.tasks()[*index])),
            })
            .collect::<Vec<_>>()
    };

    let title = format!(" Tasks [{}] ", state.filter_summary());
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_symbol(">> ")
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(28, 36, 49))
                .add_modifier(Modifier::BOLD),
        );
    let mut list_state = ListState::default().with_selected(state.selected_list_index());
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn task_line(task: &TaskItem) -> Line<'static> {
    let checkbox = if task.selected { "[x]" } else { "[ ]" };
    let danger = if task.dangerous { " !" } else { "" };
    let availability = match task.availability {
        AvailabilityState::Available => "",
        AvailabilityState::WarnUnavailable => " ~",
        AvailabilityState::FailUnavailable => " x",
    };

    Line::from(vec![
        Span::styled(
            format!("{checkbox} "),
            Style::default().fg(if task.selected {
                Color::LightGreen
            } else {
                Color::DarkGray
            }),
        ),
        Span::styled(
            task.label.clone(),
            match task.availability {
                AvailabilityState::Available => Style::default().fg(Color::White),
                AvailabilityState::WarnUnavailable => Style::default().fg(Color::Yellow),
                AvailabilityState::FailUnavailable => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            },
        ),
        Span::styled(
            format!("  {}{danger}{availability}", task.state.label()),
            task_state_style(task.state, task.availability),
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
            Constraint::Length(18),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(area);

    render_task_detail(frame, sections[0], state);
    render_profile_panel(frame, sections[1], state);
    render_history_panel(frame, sections[2], state.history());
}

fn render_task_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(task) = state.selected_visible_task() else {
        let paragraph = Paragraph::new(vec![
            Line::styled(
                "No tasks match the current filters.",
                Style::default().fg(Color::Yellow).bold(),
            ),
            Line::raw(""),
            Line::raw("Clear the active filters or change the scope to see tasks again."),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Active filter: ", Style::default().fg(Color::Gray)),
                Span::raw(state.filter_summary()),
            ]),
        ])
        .block(
            Block::default()
                .title(" Detail ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
        return;
    };

    let dependencies = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies.join(", ")
    };
    let tags = if task.tags.is_empty() {
        "none".to_string()
    } else {
        task.tags.join(", ")
    };
    let required_commands = if task.requires_commands.is_empty() {
        "none".to_string()
    } else {
        task.requires_commands.join(", ")
    };
    let required_paths = if task.requires_paths.is_empty() {
        "none".to_string()
    } else {
        task.requires_paths.join(", ")
    };
    let flags = vec![
        flag_span("dry-run", state.options().dry_run),
        flag_span("verbose", state.options().verbose),
        flag_span("brew-cleanup", state.options().brew_cleanup),
        flag_span("npm-audit", state.options().npm_audit),
    ];

    let mut lines = vec![
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
            Span::raw(" "),
            Span::styled(
                task.availability.label(),
                availability_style(task.availability),
            ),
        ]),
        Line::raw(""),
        Line::raw(task.description.clone()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Category: ", Style::default().fg(Color::Gray)),
            Span::raw(task.category.clone()),
        ]),
        Line::from(vec![
            Span::styled("Tags: ", Style::default().fg(Color::Gray)),
            Span::raw(tags),
        ]),
        Line::from(vec![
            Span::styled("Dependencies: ", Style::default().fg(Color::Gray)),
            Span::raw(dependencies),
        ]),
    ];

    if let Some(danger_message) = &task.danger_message {
        lines.push(Line::from(vec![
            Span::styled("Danger: ", Style::default().fg(Color::Yellow).bold()),
            Span::raw(danger_message.clone()),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Requires commands: ", Style::default().fg(Color::Gray)),
        Span::raw(required_commands),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Requires paths: ", Style::default().fg(Color::Gray)),
        Span::raw(required_paths),
    ]));
    lines.push(Line::from(vec![
        Span::styled("On missing: ", Style::default().fg(Color::Gray)),
        Span::raw(match task.on_missing {
            MissingRequirementPolicy::Warn => "warn and skip",
            MissingRequirementPolicy::Fail => "fail task",
        }),
    ]));

    if !task.notes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Notes", Style::default().fg(Color::Gray)));
        for note in &task.notes {
            lines.push(Line::raw(format!("- {note}")));
        }
    }

    if !task.preflight_messages.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Preflight", Style::default().fg(Color::Gray)));
        for message in &task.preflight_messages {
            lines.push(Line::raw(format!("- {message}")));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(flags));

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
        Line::from(vec![
            Span::styled("Filter: ", Style::default().fg(Color::Gray)),
            Span::raw(state.filter_summary()),
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
        if let Some(message) = state.status_message() {
            lines.push(Line::styled(
                message.to_string(),
                Style::default().fg(Color::Cyan).bold(),
            ));
            lines.push(Line::raw(""));
        }
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
    } else if let Some(message) = state.status_message() {
        lines.push(Line::raw(message.to_string()));
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
    let mut lines = Vec::new();
    if matches!(state.screen(), Screen::Summary) && state.summary().is_some() {
        lines.push(Line::styled(
            "Task output was shown directly in the terminal during the run.",
            Style::default().fg(Color::Cyan),
        ));
        lines.push(Line::styled(
            "This panel keeps cockpit-level activity such as task starts, finishes, and preflight warnings.",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::raw(""));
    }

    if state.logs().is_empty() {
        lines.push(Line::styled(
            "No activity yet.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        let available_lines = area.height.saturating_sub(2) as usize;
        let reserved = lines.len();
        let log_capacity = available_lines.saturating_sub(reserved);
        if log_capacity > 0 {
            lines.extend(
                state
                    .logs()
                    .iter()
                    .rev()
                    .take(log_capacity)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(Line::raw),
            );
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Activity ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let status = state.status_message().unwrap_or(match state.screen() {
        Screen::Select => "Choose tasks, adjust filters, and launch a run.",
        Screen::ConfirmDangerous => "Dangerous tasks need confirmation.",
        Screen::Running => "Wait for the current run to finish.",
        Screen::Summary => "Review the summary or rerun what you need.",
    });
    let keybindings = match state.screen() {
        Screen::Select => {
            "j/k move   space toggle   p profile   f scope   g category   t tag   z clear   enter run   q quit"
        }
        Screen::ConfirmDangerous => "y continue   n cancel",
        Screen::Running => "running   terminal control is with the updater",
        Screen::Summary => "enter selection   r rerun failed   l rerun last profile   q quit",
    };

    let paragraph = Paragraph::new(vec![
        Line::raw(status.to_string()),
        Line::styled(keybindings, Style::default().fg(Color::Gray)),
    ])
    .style(Style::default().fg(Color::White).bg(Color::Rgb(28, 36, 49)))
    .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_confirmation(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = vec![
        Line::styled(
            "Dangerous tasks selected",
            Style::default().fg(Color::Yellow).bold(),
        ),
        Line::raw(""),
        Line::raw("This run includes tasks with explicit danger notes:"),
        Line::raw(""),
    ];

    for (label, message) in state.pending_danger_messages() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label}: "),
                Style::default().fg(Color::Yellow).bold(),
            ),
            Span::raw(message),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::raw("Press y to continue or n to cancel."));

    let paragraph = Paragraph::new(lines)
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

fn task_state_style(state: TaskState, availability: AvailabilityState) -> Style {
    match state {
        TaskState::Pending => match availability {
            AvailabilityState::Available => Style::default().fg(Color::Gray),
            AvailabilityState::WarnUnavailable => Style::default().fg(Color::Yellow),
            AvailabilityState::FailUnavailable => Style::default().fg(Color::DarkGray),
        },
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

fn availability_style(state: AvailabilityState) -> Style {
    match state {
        AvailabilityState::Available => Style::default().fg(Color::Green),
        AvailabilityState::WarnUnavailable => Style::default().fg(Color::Yellow).bold(),
        AvailabilityState::FailUnavailable => Style::default().fg(Color::Red).bold(),
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
