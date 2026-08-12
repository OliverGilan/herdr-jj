use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::jj::{WorkspaceEntry, valid_workspace_name};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateChoice {
    pub name: String,
    pub create_bookmark: bool,
}

pub struct CreateDialog {
    pub initial_name: String,
    pub create_bookmark: bool,
}

#[derive(Clone, Debug)]
pub struct OpenChoice {
    pub workspace: WorkspaceEntry,
    pub open_workspace_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OpenEntry {
    pub workspace: WorkspaceEntry,
    pub open_workspace_id: Option<String>,
}

pub fn create_dialog(dialog: CreateDialog) -> io::Result<Option<CreateChoice>> {
    let mut name = dialog.initial_name.clone();
    let mut bookmark = dialog.create_bookmark;
    let mut replace_on_type = true;
    let mut error = None;

    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_create(frame, &name, bookmark, error.as_deref()))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match handle_create_key(&mut name, &mut bookmark, &mut replace_on_type, key) {
                CreateAction::Continue => error = None,
                CreateAction::Cancel => break Ok(None),
                CreateAction::Submit if valid_workspace_name(name.trim()) => {
                    break Ok(Some(CreateChoice {
                        name: name.trim().to_owned(),
                        create_bookmark: bookmark,
                    }));
                }
                CreateAction::Submit => {
                    error = Some("Use letters, numbers, '.', '_', '-', or '/'.".to_owned());
                }
            }
        }
    })
}

pub fn open_dialog(entries: Vec<OpenEntry>) -> io::Result<Option<OpenChoice>> {
    let mut query = String::new();
    let mut selected = 0usize;

    with_terminal(|terminal| {
        loop {
            let filtered = filtered_entries(&entries, &query);
            selected = selected.min(filtered.len().saturating_sub(1));
            terminal.draw(|frame| draw_open(frame, &query, &filtered, selected))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('c')
                    if key.code == KeyCode::Esc
                        || key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    break Ok(None);
                }
                KeyCode::Enter => {
                    let Some(entry) = filtered.get(selected) else {
                        continue;
                    };
                    if entry.workspace.available {
                        break Ok(Some(OpenChoice {
                            workspace: entry.workspace.clone(),
                            open_workspace_id: entry.open_workspace_id.clone(),
                        }));
                    }
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => selected = (selected + 1).min(filtered.len().saturating_sub(1)),
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(character);
                    selected = 0;
                }
                _ => {}
            }
        }
    })
}

pub fn remove_dialog() -> io::Result<bool> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(draw_remove)?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Enter => break Ok(true),
                KeyCode::Esc => break Ok(false),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Ok(false);
                }
                _ => {}
            }
        }
    })
}

fn with_terminal<T>(
    run: impl FnOnce(&mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<T>,
) -> io::Result<T> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error);
    }
    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(error);
        }
    };
    let result = run(&mut terminal);
    let restore = restore_terminal(&mut terminal);
    match (result, restore) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let raw_mode = disable_raw_mode();
    let screen = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    raw_mode.and(screen)
}

fn draw_create(frame: &mut Frame, name: &str, bookmark: bool, error: Option<&str>) {
    let inner = shell(frame);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);
    field(frame, rows[0], "Name", &format!("{name}_"), true);
    let mark = if bookmark { "[x]" } else { "[ ]" };
    frame.render_widget(
        Paragraph::new(format!("{mark} Create bookmark with the same name  (Tab)"))
            .style(Style::default().fg(MUTED)),
        rows[1],
    );
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error).style(Style::default().fg(Color::Red)),
            rows[2],
        );
    }
    frame.render_widget(
        Paragraph::new("Type to replace the suggestion with a custom name.")
            .style(Style::default().fg(MUTED))
            .alignment(Alignment::Center),
        rows[3],
    );
    footer(frame, rows[4], "Enter create", "Esc cancel");
}

fn draw_open(frame: &mut Frame, query: &str, entries: &[&OpenEntry], selected: usize) {
    let inner = shell(frame);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .split(inner);
    field(frame, rows[0], "Filter", &format!("{query}_"), true);

    let visible = rows[1].height as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let lines = entries
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, entry)| {
            let marker = if index == selected { ">" } else { " " };
            let state = if !entry.workspace.available {
                "missing"
            } else if entry.open_workspace_id.is_some() {
                "open"
            } else {
                ""
            };
            let description = truncate(&entry.workspace.description, 28);
            let text = format!(
                "{marker} {:<28} @{:12} {:<7} {}",
                truncate(&entry.workspace.name, 28),
                entry.workspace.change_id,
                state,
                description
            );
            let style = if index == selected {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else if !entry.workspace.available {
                Style::default().fg(MUTED)
            } else {
                Style::default()
            };
            Line::styled(text, style)
        })
        .collect::<Vec<_>>();
    let content = if lines.is_empty() {
        vec![Line::styled(
            "No matching JJ workspaces",
            Style::default().fg(MUTED),
        )]
    } else {
        lines
    };
    frame.render_widget(Paragraph::new(content), rows[1]);
    footer(frame, rows[2], "Enter open", "Esc cancel");
}

fn draw_remove(frame: &mut Frame) {
    let inner = shell(frame);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("Are you sure you want to remove this workspace?")
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new("Your branch will not be removed.").style(Style::default().fg(MUTED)),
        rows[1],
    );
    danger_footer(frame, rows[3], "Enter remove", "Esc cancel");
}

fn shell(frame: &mut Frame) -> Rect {
    let area = frame.area();
    frame.render_widget(Clear, area);
    area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    })
}

fn field(frame: &mut Frame, area: Rect, label: &str, value: &str, active: bool) {
    let style = if active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(label, Style::default().fg(MUTED)),
            Line::styled(truncate(value, area.width as usize), style),
        ]),
        area,
    );
}

fn footer(frame: &mut Frame, area: Rect, primary: &str, secondary: &str) {
    action_footer(frame, area, primary, secondary, ACCENT);
}

fn danger_footer(frame: &mut Frame, area: Rect, primary: &str, secondary: &str) {
    action_footer(frame, area, primary, secondary, Color::Red);
}

fn action_footer(frame: &mut Frame, area: Rect, primary: &str, secondary: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {primary} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!(" {secondary} "), Style::default().fg(MUTED)),
        ]))
        .alignment(Alignment::Center),
        area,
    );
}

fn filtered_entries<'a>(entries: &'a [OpenEntry], query: &str) -> Vec<&'a OpenEntry> {
    let query = query.to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.workspace.name.to_ascii_lowercase().contains(&query)
                || entry
                    .workspace
                    .description
                    .to_ascii_lowercase()
                    .contains(&query)
        })
        .collect()
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    value
        .chars()
        .take(width.saturating_sub(3))
        .collect::<String>()
        + "..."
}

enum CreateAction {
    Continue,
    Cancel,
    Submit,
}

fn handle_create_key(
    name: &mut String,
    bookmark: &mut bool,
    replace_on_type: &mut bool,
    key: KeyEvent,
) -> CreateAction {
    match key.code {
        KeyCode::Esc => CreateAction::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => CreateAction::Cancel,
        KeyCode::Enter => CreateAction::Submit,
        KeyCode::Tab => {
            *bookmark = !*bookmark;
            CreateAction::Continue
        }
        KeyCode::Backspace => {
            if *replace_on_type {
                name.clear();
                *replace_on_type = false;
            } else {
                name.pop();
            }
            CreateAction::Continue
        }
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            if *replace_on_type {
                name.clear();
                *replace_on_type = false;
            }
            name.push(character);
            CreateAction::Continue
        }
        _ => CreateAction::Continue,
    }
}

pub fn generated_name(seed: u64) -> String {
    const ADJECTIVES: [&str; 8] = [
        "brisk", "calm", "clear", "green", "quick", "quiet", "sharp", "silver",
    ];
    const NOUNS: [&str; 8] = [
        "brook", "cloud", "field", "grove", "harbor", "meadow", "stone", "valley",
    ];
    let adjective = ADJECTIVES[(seed as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((seed / ADJECTIVES.len() as u64) as usize) % NOUNS.len()];
    format!("{adjective}-{noun}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn filters_picker_entries_by_name_and_description() {
        let entries = vec![OpenEntry {
            workspace: WorkspaceEntry {
                name: "feature-api".to_owned(),
                root: PathBuf::from("/repo.feature-api"),
                change_id: "abc".to_owned(),
                description: "add login endpoint".to_owned(),
                available: true,
            },
            open_workspace_id: None,
        }];

        assert_eq!(filtered_entries(&entries, "API").len(), 1);
        assert_eq!(filtered_entries(&entries, "login").len(), 1);
        assert!(filtered_entries(&entries, "docs").is_empty());
    }

    #[test]
    fn generated_names_are_valid() {
        let name = generated_name(42);
        assert!(valid_workspace_name(&name));
        assert_eq!(name.matches('-').count(), 1);
    }
}
