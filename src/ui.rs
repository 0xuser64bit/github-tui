//! All rendering. Pure presentation: reads [`App`] state and draws it, with the
//! sole exception of mutating `ListState`s, which stateful widgets require.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

use crate::app::{App, Load, Screen, Tab};
use crate::models::{Issue, PullRequest, Repo, SearchedPullRequest};

const ACCENT: Color = Color::Cyan;
const OPEN: Color = Color::Green;
const CLOSED: Color = Color::Magenta;

/// Entry point: lay out header / body / footer and draw the active screen.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let progress = app.progress();
    let mut notices = Vec::new();
    let actions = if app.search_active {
        "Esc then r refresh"
    } else {
        "r refresh"
    };
    if progress.error.is_some() {
        notices.push(if progress.next.is_some() {
            format!("{actions}; n load next page")
        } else {
            actions.to_string()
        });
    } else if progress.next.is_some() && !progress.loading() {
        notices.push(
            if app.search_active {
                "Esc then n: load next page"
            } else {
                "n: load next page"
            }
            .to_string(),
        );
    }
    if let Some(error) = &progress.error {
        notices.push(format!("{error} Loaded rows retained."));
    }
    if let Some(warning) = &progress.warning {
        notices.push(warning.clone());
    }
    if progress.loading() {
        notices.push(format!("{} Fetching…", app.spinner()));
    } else if progress.next.is_some() {
        notices.push("More results available. Search filters loaded rows.".to_string());
    }
    let notice = notices.join("\n");
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(app.search_active)),
        Constraint::Min(0),
        Constraint::Length(if notice.is_empty() {
            0
        } else {
            notices.len().min(4) as u16 + 1
        }),
        Constraint::Length(1),
    ])
    .split(area);
    let (header_area, body_area, footer_area) = (chunks[0], chunks[2], chunks[4]);

    render_header(frame, app, header_area);

    if app.search_active {
        render_search_bar(frame, app, chunks[1]);
    }

    frame.render_widget(
        Paragraph::new(notice)
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(Color::Yellow)),
        chunks[3],
    );

    if app.needs_auth() {
        render_auth_required(frame, body_area);
    } else {
        match app.screen {
            Screen::Repos => render_repos(frame, app, body_area),
            Screen::RepoDetail => render_detail(frame, app, body_area),
            Screen::MyPulls => render_my_pulls(frame, app, body_area),
        }
    }

    render_footer(frame, app, footer_area);

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
        Screen::MyPulls => spans.push(Span::raw("My Pull Requests")),
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(Color::Reset)),
        area,
    );
}

fn render_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled("/ ", Style::new().fg(ACCENT).bold()),
        Span::raw(app.search_query.clone()),
        Span::styled("█", Style::new().fg(ACCENT)),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::new().bg(Color::Indexed(235))),
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
        Load::Loaded(repos) if repos.is_empty() && app.repos_progress.next.is_some() => {
            render_status_box(
                frame,
                area,
                block,
                &Text::from("No repositories on this page. More results are available."),
            )
        }
        Load::Loaded(repos) if repos.is_empty() => {
            render_status_box(frame, area, block, &Text::from("No repositories found."))
        }
        Load::Loaded(repos) => {
            if app.filtered_repos.is_empty() {
                render_status_box(frame, area, block, &Text::from("No matching repositories."));
            } else {
                let items: Vec<ListItem> = app
                    .filtered_repos
                    .iter()
                    .filter_map(|&i| repos.get(i))
                    .map(repo_item)
                    .collect();
                let list = list_widget(items, block);
                frame.render_stateful_widget(list, area, &mut app.repos_state);
            }
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
            Load::Loaded(items) if items.is_empty() && app.issues_progress.next.is_some() => {
                render_status_box(
                    frame,
                    rows[1],
                    block,
                    &Text::from("No issues on this page. More results are available."),
                )
            }
            Load::Loaded(items) if items.is_empty() => {
                render_status_box(frame, rows[1], block, &Text::from("No issues."))
            }
            Load::Loaded(items) => {
                if app.filtered_issues.is_empty() {
                    render_status_box(frame, rows[1], block, &Text::from("No matching issues."));
                } else {
                    let filtered: Vec<ListItem> = app
                        .filtered_issues
                        .iter()
                        .filter_map(|&i| items.get(i))
                        .map(issue_item)
                        .collect();
                    let list = list_widget(filtered, block);
                    frame.render_stateful_widget(list, rows[1], &mut app.issues_state);
                }
            }
        },
        Tab::Pulls => match &app.pulls {
            Load::Idle | Load::Loading => {
                render_status_box(frame, rows[1], block, &loading(app, "pull requests"))
            }
            Load::Failed(message) => render_status_box(frame, rows[1], block, &failure(message)),
            Load::Loaded(items) if items.is_empty() && app.pulls_progress.next.is_some() => {
                render_status_box(
                    frame,
                    rows[1],
                    block,
                    &Text::from("No pull requests on this page. More results are available."),
                )
            }
            Load::Loaded(items) if items.is_empty() => {
                render_status_box(frame, rows[1], block, &Text::from("No pull requests."))
            }
            Load::Loaded(items) => {
                if app.filtered_pulls.is_empty() {
                    render_status_box(
                        frame,
                        rows[1],
                        block,
                        &Text::from("No matching pull requests."),
                    );
                } else {
                    let filtered: Vec<ListItem> = app
                        .filtered_pulls
                        .iter()
                        .filter_map(|&i| items.get(i))
                        .map(pull_item)
                        .collect();
                    let list = list_widget(filtered, block);
                    frame.render_stateful_widget(list, rows[1], &mut app.pulls_state);
                }
            }
        },
    }
}

fn render_my_pulls(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered()
        .border_style(Style::new().dim())
        .title(Line::from(" My Pull Requests "));

    match &app.my_pulls {
        Load::Idle | Load::Loading => {
            render_status_box(frame, area, block, &loading(app, "pull requests"))
        }
        Load::Failed(message) => render_status_box(frame, area, block, &failure(message)),
        Load::Loaded(items) if items.is_empty() && app.my_pulls_progress.next.is_some() => {
            render_status_box(
                frame,
                area,
                block,
                &Text::from("No pull requests on this page. More results are available."),
            )
        }
        Load::Loaded(items) if items.is_empty() => {
            render_status_box(frame, area, block, &Text::from("No pull requests found."))
        }
        Load::Loaded(items) => {
            if app.filtered_my_pulls.is_empty() {
                render_status_box(
                    frame,
                    area,
                    block,
                    &Text::from("No matching pull requests."),
                );
            } else {
                let filtered: Vec<ListItem> = app
                    .filtered_my_pulls
                    .iter()
                    .filter_map(|&i| items.get(i))
                    .map(searched_pull_item)
                    .collect();
                let list = list_widget(filtered, block);
                frame.render_stateful_widget(list, area, &mut app.my_pulls_state);
            }
        }
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
    } else if app.search_active {
        " type to filter · ↑↓ move · enter select · esc clear"
    } else {
        match app.screen {
            Screen::Repos => {
                " ↑↓ move · enter open · m my PRs · / search · o browser · r refresh · ? help · q quit"
            }
            Screen::RepoDetail => {
                " ↑↓ move · tab switch · / search · enter/o browser · esc back · r refresh · q quit"
            }
            Screen::MyPulls => {
                " ↑↓ move · / search · enter/o browser · esc back · r refresh · ? help · q quit"
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
            "Prefer a fine-grained token limited to the repositories you need,",
            Style::new().dim(),
        )),
        Line::from(Span::styled(
            "with read-only Issues and Pull requests permissions.",
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
    let popup = centered_rect(area, 64, 18);
    frame.render_widget(Clear, popup);

    let rows = [
        ("↑ / k, ↓ / j", "Move selection"),
        ("g / G, Home / End", "Jump to first / last"),
        ("Enter / l / →", "Open repo (or issue/PR in browser)"),
        ("Esc / h / ←", "Back to repository list"),
        ("Tab", "Switch Issues / Pull Requests"),
        ("m", "View your pull requests"),
        ("/", "Search / filter the current list"),
        ("o", "Open current selection in browser"),
        ("r", "Refresh the current view"),
        ("n", "Load the next page (keeps loaded rows)"),
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
        repo.full_name.clone(),
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
    let state = issue.state.as_str();
    ListItem::new(Line::from(vec![
        state_dot(state),
        Span::styled(format!("#{:<5} ", issue.number), Style::new().dim()),
        Span::raw(issue.title.clone()),
        Span::styled(
            format!("  by {} · {} comments", issue.author(), issue.comments),
            Style::new().dim(),
        ),
    ]))
}

fn pull_item(pr: &PullRequest) -> ListItem<'static> {
    let state = pr.effective_state();
    let mut spans = vec![
        state_dot(state),
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

fn searched_pull_item(pr: &SearchedPullRequest) -> ListItem<'static> {
    let state = pr.effective_state();
    let mut spans = vec![
        state_dot(state),
        Span::styled(format!("#{:<5} ", pr.number), Style::new().dim()),
        Span::raw(pr.title.clone()),
    ];
    if pr.draft {
        spans.push(Span::styled(" [draft]", Style::new().fg(Color::DarkGray)));
    }
    spans.push(Span::styled(
        format!("  {} · {}", pr.repo(), pr.author()),
        Style::new().dim(),
    ));
    ListItem::new(Line::from(spans))
}

fn state_dot(state: &str) -> Span<'static> {
    let color = match state {
        "open" => OPEN,
        "merged" => Color::Cyan,
        _ => CLOSED,
    };
    Span::styled(format!("● {state} "), Style::new().fg(color))
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
        Load::Loaded(items) => format!("({} loaded)", items.len()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Github;
    use crate::models::{Issue, PullRequest, User};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::mpsc;

    /// Render `app` into a test buffer and return its text content.
    /// Panics in `render` (layout/index bugs) fail the test here.
    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn authed_app() -> App {
        let (tx, _rx) = mpsc::channel();
        App::new(Some(Github::new("test-token".to_string())), tx)
    }

    fn sample_repo() -> Repo {
        Repo {
            name: "github-tui".to_string(),
            full_name: "user-64bit/github-tui".to_string(),
            description: Some("a terminal UI for github".to_string()),
            language: Some("Rust".to_string()),
            stargazers_count: 7,
            open_issues_count: 2,
            fork: false,
            private: false,
            archived: false,
        }
    }

    #[test]
    fn auth_screen_explains_how_to_authenticate() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(None, tx);
        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("No GitHub token found"));
        assert!(text.contains("GITHUB_TOKEN"));
        assert!(text.contains("fine-grained token limited to the repositories you need"));
        assert!(text.contains("read-only Issues and Pull requests permissions"));
    }

    #[test]
    fn loading_then_loaded_repos_render() {
        let mut app = authed_app();
        // Loading state shows the spinner copy.
        app.repos = Load::Loading;
        assert!(rendered(&mut app, 80, 24).contains("Loading repositories"));

        // Loaded state shows the repo and its metadata.
        app.repos = Load::Loaded(vec![sample_repo()]);
        app.refilter();
        app.repos_state.select(Some(0));
        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("user-64bit/github-tui"));
        assert!(text.contains("Rust"));
    }

    #[test]
    fn failed_repos_show_retry_hint() {
        let mut app = authed_app();
        app.repos = Load::Failed("Network error: offline".to_string());
        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("Network error"));
        assert!(text.contains("retry"));
    }

    #[test]
    fn detail_view_renders_issues_and_pulls() {
        let mut app = authed_app();
        app.screen = Screen::RepoDetail;
        app.active_repo = Some(sample_repo());
        app.tab = Tab::Issues;

        app.issues = Load::Loaded(vec![Issue {
            number: 42,
            title: "fix the crash".to_string(),
            state: "open".to_string(),
            comments: 3,
            user: Some(User {
                login: "octocat".to_string(),
            }),
            pull_request: None,
        }]);
        app.pulls = Load::Loaded(vec![PullRequest {
            number: 7,
            title: "add tests".to_string(),
            state: "open".to_string(),
            draft: true,
            user: Some(User {
                login: "hubber".to_string(),
            }),
            merged_at: None,
        }]);
        app.refilter();

        let issues_view = rendered(&mut app, 80, 24);
        assert!(issues_view.contains("● open"));
        assert!(issues_view.contains("#42"));
        assert!(issues_view.contains("fix the crash"));
        assert!(issues_view.contains("octocat"));

        app.tab = Tab::Pulls;
        let pulls_view = rendered(&mut app, 80, 24);
        assert!(pulls_view.contains("● open"));
        assert!(pulls_view.contains("#7"));
        assert!(pulls_view.contains("add tests"));
        assert!(pulls_view.contains("[draft]"));
    }

    #[test]
    fn help_overlay_renders() {
        let mut app = authed_app();
        app.show_help = true;
        assert!(rendered(&mut app, 80, 24).contains("Keybindings"));
    }

    #[test]
    fn repository_owners_disambiguate_matching_names() {
        let mut app = authed_app();
        let mut first = sample_repo();
        first.full_name = "alice/github-tui".to_string();
        let mut second = first.clone();
        second.full_name = "bob/github-tui".to_string();
        app.repos = Load::Loaded(vec![first, second]);
        app.refilter();

        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("alice/github-tui"));
        assert!(text.contains("bob/github-tui"));
    }

    #[test]
    fn search_bar_and_filtered_results_render() {
        let mut app = authed_app();
        let mut kept = sample_repo();
        kept.name = "kept".to_string();
        kept.full_name = "alice/kept".to_string();
        let mut hidden = sample_repo();
        hidden.full_name = "bob/hidden".to_string();
        app.repos = Load::Loaded(vec![kept, hidden]);
        app.search_active = true;
        app.search_query = "kept".to_string();
        app.refilter();

        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("/ kept█"));
        assert!(text.contains("alice/kept"));
        assert!(!text.contains("bob/hidden"));

        app.search_query = "missing".to_string();
        app.refilter();
        assert!(rendered(&mut app, 80, 24).contains("No matching repositories"));
    }

    #[test]
    fn my_pulls_render_text_states_and_load_states() {
        let mut app = authed_app();
        app.screen = Screen::MyPulls;
        app.my_pulls = Load::Loaded(
            serde_json::from_str(
                r#"[
                    {"number":7,"title":"first","state":"open","draft":true,
                     "repository_url":"https://api.github.com/repos/octocat/project",
                     "user":{"login":"octocat"}},
                    {"number":8,"title":"second","state":"closed"},
                    {"number":9,"title":"third","state":"closed",
                     "pull_request":{"merged_at":"2024-01-01T00:00:00Z"}}
                ]"#,
            )
            .unwrap(),
        );
        app.refilter();

        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("My Pull Requests"));
        assert!(text.contains("octocat/project"));
        assert!(text.contains("[draft]"));
        assert!(text.contains("● open"));
        assert!(text.contains("● closed"));
        assert!(text.contains("● merged"));

        app.search_active = true;
        app.search_query = "missing".to_string();
        app.refilter();
        assert!(rendered(&mut app, 80, 24).contains("No matching pull requests"));

        app.my_pulls = Load::Loaded(vec![]);
        assert!(rendered(&mut app, 80, 24).contains("No pull requests found"));
        app.my_pulls = Load::Failed("Network error: offline".to_string());
        let failed = rendered(&mut app, 80, 24);
        assert!(failed.contains("Network error: offline"));
        assert!(failed.contains("Press r to retry"));
        app.my_pulls = Load::Loading;
        assert!(rendered(&mut app, 80, 24).contains("Loading pull requests"));
    }

    #[test]
    fn narrow_terminals_clip_content_and_render_each_screen() {
        let mut app = authed_app();
        app.repos = Load::Loaded(vec![sample_repo()]);
        app.refilter();
        app.repos_state.select(Some(0));
        let narrow = rendered(&mut app, 24, 8);
        assert!(narrow.contains("user-64bit/github"));
        assert!(!narrow.contains("Rust"));

        app.search_active = true;
        app.search_query = "a query wider than this terminal".to_string();
        for screen in [Screen::Repos, Screen::RepoDetail, Screen::MyPulls] {
            app.screen = screen;
            for (width, height) in [(24, 8), (1, 1)] {
                assert!(!rendered(&mut app, width, height).is_empty());
                app.show_help = true;
                assert!(!rendered(&mut app, width, height).is_empty());
                app.show_help = false;
            }
        }
    }
    #[test]
    fn pagination_failure_keeps_rows_and_visible_recovery_actions() {
        let mut app = authed_app();
        app.repos = Load::Loaded(vec![sample_repo()]);
        app.refilter();
        app.repos_progress.error =
            Some("A long network error that should never hide the recovery controls".to_string());
        let text = rendered(&mut app, 24, 14);
        assert!(text.contains("r refresh"));
        assert!(!text.contains("n load next page"));
        assert!(text.contains("user-64bit/github"));
        app.repos_progress.next = Some("https://api.github.com/user/repos?page=2".to_string());
        assert!(rendered(&mut app, 80, 24).contains("n load next page"));
        app.search_active = true;
        assert!(rendered(&mut app, 80, 24).contains("Esc then r refresh"));
    }

    #[test]
    fn incomplete_search_and_empty_continuation_are_visible() {
        let mut app = authed_app();
        app.screen = Screen::MyPulls;
        app.my_pulls = Load::Loaded(vec![]);
        app.my_pulls_progress.next =
            Some("https://api.github.com/search/issues?page=2".to_string());
        app.my_pulls_progress.warning =
            Some("GitHub Search timed out; this list may be incomplete.".to_string());
        let text = rendered(&mut app, 80, 24);
        assert!(text.contains("No pull requests on this page"));
        assert!(text.contains("incomplete"));
        assert!(text.contains("n: load next page"));
    }
}
