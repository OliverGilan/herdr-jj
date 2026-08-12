use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::jj::{ChangeStatus, WorkspaceEntry, path_slug, valid_workspace_name};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateChoice {
    pub name: String,
    pub create_bookmark: bool,
}

pub struct CreateDialog<'a> {
    pub initial_name: String,
    pub create_bookmark: bool,
    pub repository_name: &'a str,
    pub workspace_root: &'a Path,
    pub parent_change: &'a str,
    pub has_post_create: bool,
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

pub struct RemoveDialog<'a> {
    pub workspace_name: &'a str,
    pub root: &'a Path,
    pub status: &'a ChangeStatus,
}

pub fn create_dialog(dialog: CreateDialog<'_>) -> io::Result<Option<CreateChoice>> {
    let mut name = dialog.initial_name.clone();
    let mut bookmark = dialog.create_bookmark;
    let mut replace_on_type = true;
    let mut error = None;

    with_terminal(|terminal| {
        loop {
            terminal
                .draw(|frame| draw_create(frame, &dialog, &name, bookmark, error.as_deref()))?;
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

pub fn remove_dialog(dialog: RemoveDialog<'_>) -> io::Result<bool> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_remove(frame, &dialog))?;
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

fn draw_create(
    frame: &mut Frame,
    dialog: &CreateDialog<'_>,
    name: &str,
    bookmark: bool,
    error: Option<&str>,
) {
    let inner = shell(frame, " New JJ workspace ");
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);
    field(frame, rows[0], "Workspace", &format!("{name}_"), true);
    let destination = checkout_preview(dialog.workspace_root, dialog.repository_name, name);
    field(
        frame,
        rows[1],
        "Checkout",
        &destination.display().to_string(),
        false,
    );
    field(
        frame,
        rows[2],
        "Parent",
        &format!("@{}", dialog.parent_change),
        false,
    );
    let mark = if bookmark { "[x]" } else { "[ ]" };
    let setup = if dialog.has_post_create {
        "  setup command enabled"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!("Tab toggles bookmark  {mark}{setup}"))
            .style(Style::default().fg(MUTED)),
        rows[3],
    );
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error).style(Style::default().fg(Color::Red)),
            rows[4],
        );
    }
    footer(frame, rows[5], "Enter create", "Esc cancel");
}

fn draw_open(frame: &mut Frame, query: &str, entries: &[&OpenEntry], selected: usize) {
    let inner = shell(frame, " Open JJ workspace ");
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

fn draw_remove(frame: &mut Frame, dialog: &RemoveDialog<'_>) {
    let inner = shell(frame, " Remove JJ workspace ");
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(inner);
    field(frame, rows[0], "Workspace", dialog.workspace_name, false);
    field(
        frame,
        rows[1],
        "Checkout",
        &dialog.root.display().to_string(),
        false,
    );
    let bookmarks = if dialog.status.bookmarks.is_empty() {
        "none".to_owned()
    } else {
        dialog.status.bookmarks.join(" ")
    };
    field(
        frame,
        rows[2],
        "Change",
        &format!(
            "@{}  files={}  bookmarks={bookmarks}",
            dialog.status.change_id, dialog.status.changed_files
        ),
        false,
    );
    frame.render_widget(
        Paragraph::new("The checkout directory will be deleted.")
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new("Ignored files are outside JJ history and will also be deleted.")
            .style(Style::default().fg(Color::Yellow)),
        rows[4],
    );
    footer(frame, rows[5], "Enter remove", "Esc cancel");
}

fn shell(frame: &mut Frame, title: &str) -> Rect {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area).inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(block, area);
    inner
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
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {primary} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
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
    format!("workspace/{adjective}-{noun}-{:04x}", seed & 0xffff)
}

pub fn checkout_preview(root: &Path, repository: &str, name: &str) -> PathBuf {
    root.join(repository).join(path_slug(name))
}

#[cfg(test)]
mod tests {
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
        assert!(valid_workspace_name(&generated_name(42)));
    }

    #[test]
    fn checkout_preview_uses_the_same_slug_as_creation() {
        assert_eq!(
            checkout_preview(Path::new("/workspaces"), "repo", "feature/api"),
            PathBuf::from("/workspaces/repo/feature-api")
        );
    }
}
