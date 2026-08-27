//! Application state and the main loop.
//!
//! The app is a small state machine over two screens (repository list and a
//! per-repository detail with Issues/Pulls tabs). All network work is dispatched
//! to worker threads; results arrive as [`Message`]s and update the relevant
//! [`Load`] field. The UI thread only ever reads state and draws.

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::widgets::ListState;

use crate::event::{Event, Events, Message};
use crate::github::Github;
use crate::models::{Issue, PullRequest, Repo, User};
use crate::ui;

/// The async loading state of a piece of remote data.
pub enum Load<T> {
    /// Not requested yet.
    Idle,
    /// A request is in flight.
    Loading,
    /// Loaded successfully.
    Loaded(T),
    /// The request failed, with a user-facing message.
    Failed(String),
}

impl<T> From<Result<T, String>> for Load<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Load::Loaded(value),
            Err(message) => Load::Failed(message),
        }
    }
}

impl<U> Load<Vec<U>> {
    /// Number of loaded items (0 unless successfully loaded).
    pub fn len(&self) -> usize {
        match self {
            Load::Loaded(items) => items.len(),
            _ => 0,
        }
    }

    /// True when there are no loaded items (including while idle/loading/failed).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Which top-level screen is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Repos,
    RepoDetail,
}

/// Which tab of the repo-detail screen is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Issues,
    Pulls,
}

/// Braille spinner frames used while data loads.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The whole application state. Fields are `pub(crate)` so the `ui` module can
/// render them; all mutation happens through methods here.
pub struct App {
    pub(crate) running: bool,
    client: Option<Github>,
    sender: Sender<Event>,

    pub(crate) user: Load<User>,
    pub(crate) repos: Load<Vec<Repo>>,
    pub(crate) repos_state: ListState,

    pub(crate) screen: Screen,
    pub(crate) active_repo: Option<Repo>,
    pub(crate) tab: Tab,
    pub(crate) issues: Load<Vec<Issue>>,
    pub(crate) issues_state: ListState,
    pub(crate) pulls: Load<Vec<PullRequest>>,
    pub(crate) pulls_state: ListState,

    pub(crate) show_help: bool,
    pub(crate) status: Option<String>,
    spinner: usize,

    /// Whether the search input bar is active.
    pub(crate) search_active: bool,
    /// Current search query text.
    pub(crate) search_query: String,
    /// Indices into the loaded repos that match the current query.
    pub(crate) filtered_repos: Vec<usize>,
    /// Indices into the loaded issues that match the current query.
    pub(crate) filtered_issues: Vec<usize>,
    /// Indices into the loaded pulls that match the current query.
    pub(crate) filtered_pulls: Vec<usize>,
}

impl App {
    /// Build the app. `client` is `None` when no token could be resolved, in
    /// which case the UI shows an auth-required screen instead of fetching.
    pub fn new(client: Option<Github>, sender: Sender<Event>) -> Self {
        Self {
            running: false,
            client,
            sender,
            user: Load::Idle,
            repos: Load::Idle,
            repos_state: ListState::default(),
            screen: Screen::Repos,
            active_repo: None,
            tab: Tab::Issues,
            issues: Load::Idle,
            issues_state: ListState::default(),
            pulls: Load::Idle,
            pulls_state: ListState::default(),
            show_help: false,
            status: None,
            spinner: 0,
            search_active: false,
            search_query: String::new(),
            filtered_repos: Vec::new(),
            filtered_issues: Vec::new(),
            filtered_pulls: Vec::new(),
        }
    }

    /// Run the main loop until the user quits.
    pub fn run(mut self, mut terminal: DefaultTerminal, events: Events) -> color_eyre::Result<()> {
        self.running = true;
        self.bootstrap();
        while self.running {
            terminal.draw(|frame| ui::render(frame, &mut self))?;
            match events.next()? {
                Event::Terminal(event) => self.on_terminal(event),
                Event::Message(message) => self.on_message(message),
                Event::Tick => self.spinner = self.spinner.wrapping_add(1),
            }
        }
        Ok(())
    }

    /// True when no token was available, so nothing can be fetched.
    pub(crate) fn needs_auth(&self) -> bool {
        self.client.is_none()
    }

    /// Current spinner glyph.
    pub(crate) fn spinner(&self) -> &'static str {
        SPINNER[self.spinner % SPINNER.len()]
    }

    /// Kick off the initial fetches once, at startup.
    fn bootstrap(&mut self) {
        if self.client.is_some() {
            self.fetch_user();
            self.fetch_repos();
        }
    }

    // --- event handling ----------------------------------------------------

    fn on_terminal(&mut self, event: TerminalEvent) {
        if let TerminalEvent::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                self.on_key(key);
            }
        }
        // Resize/mouse need no handling: the loop redraws after every event.
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.running = false;
            return;
        }

        // While the help overlay is open it captures the next keypress.
        if self.show_help {
            self.show_help = false;
            return;
        }

        // While the search bar is active, most keys edit the query.
        if self.search_active {
            match key.code {
                KeyCode::Esc => self.exit_search(),
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.refilter();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.refilter();
                }
                KeyCode::Up | KeyCode::Down => {
                    let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                    self.move_cursor(delta);
                }
                KeyCode::Enter => {
                    self.exit_search();
                    self.activate_selection();
                }
                _ => {}
            }
            return;
        }

        self.status = None;
        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('o') => self.open_selected_in_browser(),
            KeyCode::Char('/') => self.enter_search(),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Home | KeyCode::Char('g') => self.move_to_edge(true),
            KeyCode::End | KeyCode::Char('G') => self.move_to_edge(false),
            KeyCode::Tab => self.toggle_tab(),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.activate_selection(),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.go_back(),
            _ => {}
        }
    }

    fn on_message(&mut self, message: Message) {
        match message {
            Message::User(result) => self.user = result.into(),
            Message::Repos(result) => {
                self.repos = result.into();
                self.refilter();
                Self::select_first(&mut self.repos_state, self.filtered_repos.len());
            }
            Message::Issues { repo, result } => {
                if self.is_active_repo(&repo) {
                    self.issues = result.into();
                    self.refilter();
                    Self::select_first(&mut self.issues_state, self.filtered_issues.len());
                }
            }
            Message::Pulls { repo, result } => {
                if self.is_active_repo(&repo) {
                    self.pulls = result.into();
                    self.refilter();
                    Self::select_first(&mut self.pulls_state, self.filtered_pulls.len());
                }
            }
        }
    }

    // --- actions -----------------------------------------------------------

    /// Enter: on the repo list open the detail view; in detail, open the
    /// selected issue/PR in a browser.
    fn activate_selection(&mut self) {
        match self.screen {
            Screen::Repos => self.open_selected_repo(),
            Screen::RepoDetail => self.open_selected_in_browser(),
        }
    }

    fn open_selected_repo(&mut self) {
        let Some(repo) = self.selected_repo() else {
            return;
        };
        self.active_repo = Some(repo.clone());
        self.screen = Screen::RepoDetail;
        self.tab = Tab::Issues;
        self.issues = Load::Idle;
        self.pulls = Load::Idle;
        self.issues_state.select(None);
        self.pulls_state.select(None);
        self.clear_search();
        self.fetch_issues(&repo);
        self.fetch_pulls(&repo);
    }

    fn go_back(&mut self) {
        if self.screen == Screen::RepoDetail {
            self.screen = Screen::Repos;
            self.active_repo = None;
            self.clear_search();
        }
    }

    fn toggle_tab(&mut self) {
        if self.screen == Screen::RepoDetail {
            self.tab = match self.tab {
                Tab::Issues => Tab::Pulls,
                Tab::Pulls => Tab::Issues,
            };
        }
    }

    fn refresh(&mut self) {
        match self.screen {
            Screen::Repos => self.fetch_repos(),
            Screen::RepoDetail => {
                if let Some(repo) = self.active_repo.clone() {
                    self.fetch_issues(&repo);
                    self.fetch_pulls(&repo);
                }
            }
        }
    }

    fn open_selected_in_browser(&mut self) {
        let Some(url) = self.selected_url() else {
            return;
        };
        match open_url(&url) {
            Ok(()) => self.status = Some(format!("Opening {url}")),
            Err(_) => self.status = Some("Could not launch a browser.".to_string()),
        }
    }

    // --- selection helpers -------------------------------------------------

    fn move_cursor(&mut self, delta: isize) {
        let (state, len) = self.active_list();
        move_selection(state, len, delta);
    }

    fn move_to_edge(&mut self, first: bool) {
        let (state, len) = self.active_list();
        if len == 0 {
            state.select(None);
        } else {
            state.select(Some(if first { 0 } else { len - 1 }));
        }
    }

    /// The list state and item count for whatever the user is currently
    /// navigating. Uses filtered counts when a search is active.
    fn active_list(&mut self) -> (&mut ListState, usize) {
        match self.screen {
            Screen::Repos => (&mut self.repos_state, self.filtered_repos.len()),
            Screen::RepoDetail => match self.tab {
                Tab::Issues => (&mut self.issues_state, self.filtered_issues.len()),
                Tab::Pulls => (&mut self.pulls_state, self.filtered_pulls.len()),
            },
        }
    }

    fn selected_repo(&self) -> Option<Repo> {
        match &self.repos {
            Load::Loaded(repos) => {
                let visible_idx = self.repos_state.selected()?;
                let real_idx = *self.filtered_repos.get(visible_idx)?;
                repos.get(real_idx).cloned()
            }
            _ => None,
        }
    }

    /// The GitHub web URL for the current selection, if any.
    fn selected_url(&self) -> Option<String> {
        match self.screen {
            Screen::Repos => self
                .selected_repo()
                .map(|repo| format!("https://github.com/{}", repo.full_name)),
            Screen::RepoDetail => {
                let repo = self.active_repo.as_ref()?;
                match self.tab {
                    Tab::Issues => {
                        let Load::Loaded(items) = &self.issues else {
                            return None;
                        };
                        let visible_idx = self.issues_state.selected()?;
                        let real_idx = *self.filtered_issues.get(visible_idx)?;
                        let issue = items.get(real_idx)?;
                        Some(format!(
                            "https://github.com/{}/issues/{}",
                            repo.full_name, issue.number
                        ))
                    }
                    Tab::Pulls => {
                        let Load::Loaded(items) = &self.pulls else {
                            return None;
                        };
                        let visible_idx = self.pulls_state.selected()?;
                        let real_idx = *self.filtered_pulls.get(visible_idx)?;
                        let pr = items.get(real_idx)?;
                        Some(format!(
                            "https://github.com/{}/pull/{}",
                            repo.full_name, pr.number
                        ))
                    }
                }
            }
        }
    }

    fn is_active_repo(&self, full_name: &str) -> bool {
        self.active_repo
            .as_ref()
            .is_some_and(|repo| repo.full_name == full_name)
    }

    fn select_first(state: &mut ListState, len: usize) {
        state.select((len > 0).then_some(0));
    }

    // --- search / filter ---------------------------------------------------

    fn enter_search(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        // Don't refilter yet — empty query shows everything.
    }

    fn exit_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.refilter();
    }

    /// Reset search state without refiltering (used on screen transitions).
    fn clear_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.filtered_repos.clear();
        self.filtered_issues.clear();
        self.filtered_pulls.clear();
        self.refilter();
    }

    /// Rebuild the filtered index vectors for the current screen from the
    /// loaded data and the search query. Case-insensitive substring match.
    pub(crate) fn refilter(&mut self) {
        let query = self.search_query.to_lowercase();

        // Repos — always refilter so the repo list stays consistent.
        if let Load::Loaded(repos) = &self.repos {
            self.filtered_repos = repos
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    query.is_empty()
                        || r.name.to_lowercase().contains(&query)
                        || r.full_name.to_lowercase().contains(&query)
                        || r.description
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&query)
                })
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_repos.clear();
        }

        // Issues
        if let Load::Loaded(issues) = &self.issues {
            self.filtered_issues = issues
                .iter()
                .enumerate()
                .filter(|(_, issue)| {
                    query.is_empty() || issue.title.to_lowercase().contains(&query)
                })
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_issues.clear();
        }

        // Pulls
        if let Load::Loaded(pulls) = &self.pulls {
            self.filtered_pulls = pulls
                .iter()
                .enumerate()
                .filter(|(_, pr)| query.is_empty() || pr.title.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_pulls.clear();
        }

        // Clamp cursor for the active list.
        let (state, len) = match self.screen {
            Screen::Repos => (&mut self.repos_state, self.filtered_repos.len()),
            Screen::RepoDetail => match self.tab {
                Tab::Issues => (&mut self.issues_state, self.filtered_issues.len()),
                Tab::Pulls => (&mut self.pulls_state, self.filtered_pulls.len()),
            },
        };
        if len == 0 {
            state.select(None);
        } else if state.selected().is_none_or(|i| i >= len) {
            state.select(Some(0));
        }
    }

    // --- fetches (each spawns a worker thread) -----------------------------

    fn fetch_user(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.user = Load::Loading;
        let sender = self.sender.clone();
        thread::spawn(move || {
            let _ = sender.send(Event::Message(Message::User(client.current_user())));
        });
    }

    fn fetch_repos(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.repos = Load::Loading;
        let sender = self.sender.clone();
        thread::spawn(move || {
            let _ = sender.send(Event::Message(Message::Repos(client.list_repos())));
        });
    }

    fn fetch_issues(&mut self, repo: &Repo) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (owner, name) = repo.owner_repo();
        let (owner, name, full) = (owner.to_string(), name.to_string(), repo.full_name.clone());
        self.issues = Load::Loading;
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client.list_issues(&owner, &name);
            let _ = sender.send(Event::Message(Message::Issues { repo: full, result }));
        });
    }

    fn fetch_pulls(&mut self, repo: &Repo) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (owner, name) = repo.owner_repo();
        let (owner, name, full) = (owner.to_string(), name.to_string(), repo.full_name.clone());
        self.pulls = Load::Loading;
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = client.list_pulls(&owner, &name);
            let _ = sender.send(Event::Message(Message::Pulls { repo: full, result }));
        });
    }
}

/// Move a list selection by `delta`, wrapping at both ends.
fn move_selection(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).rem_euclid(len as isize) as usize;
    state.select(Some(next));
}

/// Open `url` in the platform's default browser, detached and silent.
fn open_url(url: &str) -> std::io::Result<()> {
    browser_command(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/C", "start", "", url]);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_app() -> App {
        let (tx, _rx) = mpsc::channel();
        App::new(None, tx)
    }

    fn repo(full_name: &str) -> Repo {
        Repo {
            name: full_name
                .split('/')
                .next_back()
                .unwrap_or(full_name)
                .to_string(),
            full_name: full_name.to_string(),
            description: None,
            language: None,
            stargazers_count: 0,
            open_issues_count: 0,
            fork: false,
            private: false,
            archived: false,
        }
    }

    #[test]
    fn move_selection_wraps_both_ways() {
        let mut state = ListState::default();
        move_selection(&mut state, 3, 1);
        assert_eq!(state.selected(), Some(1));
        move_selection(&mut state, 3, -1);
        assert_eq!(state.selected(), Some(0));
        // wrap past the top
        move_selection(&mut state, 3, -1);
        assert_eq!(state.selected(), Some(2));
        // wrap past the bottom
        move_selection(&mut state, 3, 1);
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn move_selection_on_empty_list_clears() {
        let mut state = ListState::default();
        state.select(Some(0));
        move_selection(&mut state, 0, 1);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn load_from_result_and_len() {
        let ok: Load<Vec<u8>> = Ok(vec![1, 2, 3]).into();
        assert_eq!(ok.len(), 3);
        let err: Load<Vec<u8>> = Err("boom".to_string()).into();
        assert_eq!(err.len(), 0);
        assert!(matches!(err, Load::Failed(_)));
    }

    #[test]
    fn opening_a_repo_enters_detail_view() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user-64bit/github-tui")]);
        app.refilter();
        app.repos_state.select(Some(0));

        app.activate_selection();

        assert_eq!(app.screen, Screen::RepoDetail);
        assert_eq!(app.tab, Tab::Issues);
        assert_eq!(
            app.active_repo.as_ref().map(|r| r.full_name.as_str()),
            Some("user-64bit/github-tui")
        );
    }

    #[test]
    fn back_returns_to_repo_list() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("a/b")]);
        app.refilter();
        app.repos_state.select(Some(0));
        app.activate_selection();
        app.go_back();
        assert_eq!(app.screen, Screen::Repos);
        assert!(app.active_repo.is_none());
    }

    #[test]
    fn stale_repo_results_are_discarded() {
        let mut app = test_app();
        app.active_repo = Some(repo("current/repo"));
        // A result for a repo the user already left must be ignored.
        app.on_message(Message::Issues {
            repo: "old/repo".to_string(),
            result: Ok(vec![]),
        });
        assert!(matches!(app.issues, Load::Idle));
    }

    #[test]
    fn selected_repo_url_points_at_github() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user-64bit/github-tui")]);
        app.refilter();
        app.repos_state.select(Some(0));
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://github.com/user-64bit/github-tui")
        );
    }

    #[test]
    fn search_filters_repos_by_name() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![
            repo("user/alpha"),
            repo("user/beta"),
            repo("user/gamma"),
        ]);
        app.refilter();
        assert_eq!(app.filtered_repos.len(), 3);

        app.search_query = "bet".to_string();
        app.refilter();
        assert_eq!(app.filtered_repos.len(), 1);
        assert_eq!(app.filtered_repos[0], 1); // "beta" is at index 1
    }

    #[test]
    fn search_is_case_insensitive() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/MyRepo")]);
        app.search_query = "myrepo".to_string();
        app.refilter();
        assert_eq!(app.filtered_repos.len(), 1);
    }

    #[test]
    fn clearing_search_restores_all_items() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/a"), repo("user/b")]);
        app.search_query = "zzz".to_string();
        app.refilter();
        assert_eq!(app.filtered_repos.len(), 0);

        app.exit_search();
        assert_eq!(app.filtered_repos.len(), 2);
        assert!(!app.search_active);
    }

    #[test]
    fn search_clamps_cursor_when_results_shrink() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/aaa"), repo("user/bbb"), repo("user/ccc")]);
        app.refilter();
        app.repos_state.select(Some(2)); // cursor on "ccc"

        app.search_query = "aaa".to_string();
        app.refilter();
        // Only 1 result, cursor must have been clamped to 0.
        assert_eq!(app.repos_state.selected(), Some(0));
    }
}
