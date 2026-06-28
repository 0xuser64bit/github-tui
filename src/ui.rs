//! All rendering. Pure presentation: reads [`App`] state and draws it, with the
//! sole exception of mutating `ListState`s, which stateful widgets require.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

use crate::app::{App, Load, Screen, Tab};
use crate::models::{Issue, PullRequest, Repo};

const ACCENT: Color = Color::Cyan;
const OPEN: Color = Color::Green;
const CLOSED: Color = Color::Magenta;

/// Entry point: lay out header / body / footer and draw the active screen.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(area);

    render_header(frame, app, chunks[0]);

    if app.needs_auth() {
        render_auth_required(frame, chunks[1]);
    } else {
        match app.screen {
            Screen::Repos => render_repos(frame, app, chunks[1]),
            Screen::RepoDetail => render_detail(frame, app, chunks[1]),
        }
    }

    render_footer(frame, app, chunks[2]);

    if app.show_help {
        render_help(frame, area);
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" github-tui ", Style::new().bold().fg(ACCENT))];

    if let Load::Loaded(user) = &app.user {
        spans.push(Span::styled(
            format!("@{} ", user.login),
            Style::new().dim(),
        ));
    }

    spans.push(Span::raw("· "));
    match app.screen {
        Screen::Repos => spans.push(Span::raw("Repositories")),
        Screen::RepoDetail => {
            let name = app
                .active_repo
                .as_ref()
                .map(|r| r.full_name.as_str())
                .unwrap_or("repository");
            spans.push(Span::styled(name.to_string(), Style::new().bold()));
            spans.push(Span::raw(" › "));
            spans.push(Span::raw(match app.tab {
                Tab::Issues => "Issues",
                Tab::Pulls => "Pull Requests",
            }));
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(Color::Reset)),
        area,
    );
}

fn render_repos(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered()
        .border_style(Style::new().dim())
        .title(Line::from(" Repositories "));

    match &app.repos {
        Load::Idle | Load::Loading => {
            render_status_box(frame, area, block, &loading(app, "repositories"))
        }
        Load::Failed(message) => render_status_box(frame, area, block, &failure(message)),
        Load::Loaded(repos) if repos.is_empty() => {
            render_status_box(frame, area, block, &Text::from("No repositories found."))
        }
        Load::Loaded(repos) => {
            let items: Vec<ListItem> = repos.iter().map(repo_item).collect();
            let list = list_widget(items, block);
            frame.render_stateful_widget(list, area, &mut app.repos_state);
        }
    }
}

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_tabs(frame, app, rows[0]);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().dim());

    match app.tab {
        Tab::Issues => match &app.issues {
            Load::Idle | Load::Loading => {
                render_status_box(frame, rows[1], block, &loading(app, "issues"))
            }
            Load::Failed(message) => render_status_box(frame, rows[1], block, &failure(message)),
            Load::Loaded(items) if items.is_empty() => {
                render_status_box(frame, rows[1], block, &Text::from("No issues."))
            }
            Load::Loaded(items) => {
                let list = list_widget(items.iter().map(issue_item).collect(), block);
                frame.render_stateful_widget(list, rows[1], &mut app.issues_state);
            }
        },
        Tab::Pulls => match &app.pulls {
            Load::Idle | Load::Loading => {
                render_status_box(frame, rows[1], block, &loading(app, "pull requests"))
            }
            Load::Failed(message) => render_status_box(frame, rows[1], block, &failure(message)),
            Load::Loaded(items) if items.is_empty() => {
                render_status_box(frame, rows[1], block, &Text::from("No pull requests."))
            }
            Load::Loaded(items) => {
                let list = list_widget(items.iter().map(pull_item).collect(), block);
                frame.render_stateful_widget(list, rows[1], &mut app.pulls_state);
            }
        },
    }
}

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles = [
        format!(" Issues {} ", count_badge(&app.issues)),
        format!(" Pull Requests {} ", count_badge(&app.pulls)),
    ];
    let selected = match app.tab {
        Tab::Issues => 0,
        Tab::Pulls => 1,
    };
    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::new().dim())
        .highlight_style(
            Style::new()
                .bold()
                .fg(ACCENT)
                .remove_modifier(Modifier::DIM),
        )
        .divider("|");
    frame.render_widget(tabs, area);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    // A transient status message (e.g. "Opening …") takes precedence.
    if let Some(status) = &app.status {
        frame.render_widget(
            Paragraph::new(Line::from(format!(" {status}"))).style(Style::new().fg(ACCENT)),
            area,
        );
        return;
    }

    let hints = if app.needs_auth() {
        " q quit  ·  ? help"
    } else {
        match app.screen {
            Screen::Repos => " ↑↓ move · enter open · o browser · r refresh · ? help · q quit",
            Screen::RepoDetail => {
                " ↑↓ move · tab switch · enter/o browser · esc back · r refresh · q quit"
            }
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(hints)).style(Style::new().dim()),
        area,
    );
}

fn render_auth_required(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "No GitHub token found",
            Style::new().bold().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from("github-tui needs a token to read your repositories."),
        Line::from(""),
        Line::from("Provide one of the following, then restart:"),
        Line::from("  • export GITHUB_TOKEN=…   (or GH_TOKEN)"),
        Line::from("  • run `gh auth login` with the GitHub CLI"),
        Line::from(""),
        Line::from(Span::styled(
            "A read-only (repo) scope is sufficient.",
            Style::new().dim(),
        )),
    ]);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Yellow))
        .title(" Authentication required ");
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_help(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(area, 56, 16);
    frame.render_widget(Clear, popup);

    let rows = [
        ("↑ / k, ↓ / j", "Move selection"),
        ("g / G, Home / End", "Jump to first / last"),
        ("Enter / l / →", "Open repo (or issue/PR in browser)"),
        ("Esc / h / ←", "Back to repository list"),
        ("Tab", "Switch Issues / Pull Requests"),
        ("o", "Open current selection in browser"),
        ("r", "Refresh the current view"),
        ("?", "Toggle this help"),
        ("q / Ctrl-C", "Quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(keys, desc)| {
            Line::from(vec![
                Span::styled(format!("  {keys:<20}"), Style::new().fg(ACCENT)),
                Span::raw((*desc).to_string()),
            ])
        })
        .collect();

    let block = Block::bordered()
        .border_style(Style::new().fg(ACCENT))
        .title(" Keybindings (any key to close) ");
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}

// --- item builders ---------------------------------------------------------

fn repo_item(repo: &Repo) -> ListItem<'static> {
    let mut spans = vec![Span::styled(
        repo.name.clone(),
        Style::new().bold().fg(ACCENT),
    )];
    for (flag, label, color) in [
        (repo.private, " [private]", Color::Yellow),
        (repo.archived, " [archived]", Color::DarkGray),
        (repo.fork, " [fork]", Color::DarkGray),
    ] {
        if flag {
            spans.push(Span::styled(label, Style::new().fg(color)));
        }
    }
    if let Some(language) = &repo.language {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(language.clone(), Style::new().dim()));
    }
    spans.push(Span::styled(
        format!("  ★ {}", repo.stargazers_count),
        Style::new().dim(),
    ));
    spans.push(Span::styled(
        format!("  {} open", repo.open_issues_count),
        Style::new().dim(),
    ));

    let mut lines = vec![Line::from(spans)];
    if let Some(description) = repo.description.as_ref().filter(|d| !d.is_empty()) {
        lines.push(Line::from(Span::styled(
            format!("  {description}"),
            Style::new().dim(),
        )));
    }
    ListItem::new(Text::from(lines))
}

fn issue_item(issue: &Issue) -> ListItem<'static> {
    let open = issue.state == "open";
    ListItem::new(Line::from(vec![
        state_dot(open),
        Span::styled(format!("#{:<5} ", issue.number), Style::new().dim()),
        Span::raw(issue.title.clone()),
        Span::styled(
            format!("  by {} · {} comments", issue.author(), issue.comments),
            Style::new().dim(),
        ),
    ]))
}

fn pull_item(pr: &PullRequest) -> ListItem<'static> {
    let open = pr.state == "open";
    let mut spans = vec![
        state_dot(open),
        Span::styled(format!("#{:<5} ", pr.number), Style::new().dim()),
        Span::raw(pr.title.clone()),
    ];
    if pr.draft {
        spans.push(Span::styled(" [draft]", Style::new().fg(Color::DarkGray)));
    }
    spans.push(Span::styled(
        format!("  by {}", pr.author()),
        Style::new().dim(),
    ));
    ListItem::new(Line::from(spans))
}

fn state_dot(open: bool) -> Span<'static> {
    let color = if open { OPEN } else { CLOSED };
    Span::styled("● ", Style::new().fg(color))
}

// --- small helpers ---------------------------------------------------------

fn list_widget<'a>(items: Vec<ListItem<'a>>, block: Block<'a>) -> List<'a> {
    List::new(items)
        .block(block)
        .highlight_symbol("▶ ")
        .highlight_style(Style::new().bold().bg(Color::Indexed(236)))
}

fn render_status_box(frame: &mut Frame, area: Rect, block: Block, text: &Text) {
    frame.render_widget(
        Paragraph::new(text.clone())
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn loading(app: &App, what: &str) -> Text<'static> {
    Text::from(Line::from(vec![
        Span::styled(format!("{} ", app.spinner()), Style::new().fg(ACCENT)),
        Span::raw(format!("Loading {what}…")),
    ]))
}

fn failure(message: &str) -> Text<'static> {
    Text::from(vec![
        Line::from(Span::styled(
            message.to_string(),
            Style::new().fg(Color::Red),
        )),
        Line::from(""),
        Line::from(Span::styled("Press r to retry.", Style::new().dim())),
    ])
}

fn count_badge<T>(load: &Load<Vec<T>>) -> String {
    match load {
        Load::Loaded(items) => format!("({})", items.len()),
        Load::Loading => "(…)".to_string(),
        _ => String::new(),
    }
}

/// A centered rectangle `width` x `height` cells, clamped to `area`.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let [horizontal] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [vertical] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(horizontal);
    vertical
}
