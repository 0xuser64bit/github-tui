//! Application state and the main loop.
//!
//! The app is a state machine over repository, detail, and personal PR screens.
//! All network work is dispatched
//! to worker threads; results arrive as [`Message`]s and update the relevant
//! [`Load`] field. The UI thread only ever reads state and draws.

use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::widgets::ListState;

use crate::event::{Event, Events, Message};
use crate::github::{Github, Page};
use crate::models::{Issue, ItemDetail, PullRequest, Repo, SearchedPullRequest, User};
use crate::requests::{Cancellation, RequestPool};
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

#[derive(Default)]
struct Request {
    generation: u64,
    in_flight: bool,
    cancellation: Cancellation,
}

impl Request {
    fn start(&mut self) -> Option<u64> {
        if self.in_flight {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.in_flight = true;
        self.cancellation = Cancellation::default();
        Some(self.generation)
    }

    fn finish(&mut self, generation: u64) -> bool {
        if !self.in_flight || generation != self.generation {
            return false;
        }
        self.in_flight = false;
        true
    }

    fn invalidate(&mut self) {
        self.cancellation.cancel();
        self.generation = self.generation.wrapping_add(1);
        self.in_flight = false;
    }
}

#[derive(Default)]
pub(crate) struct ListProgress {
    request: Request,
    pub(crate) next: Option<String>,
    pub(crate) warning: Option<String>,
    pub(crate) error: Option<String>,
    cursor: Option<String>,
    completed: HashSet<String>,
}

impl ListProgress {
    fn reset(&mut self) {
        self.request.invalidate();
        *self = Self {
            request: std::mem::take(&mut self.request),
            ..Self::default()
        };
    }

    pub(crate) fn loading(&self) -> bool {
        self.request.in_flight
    }

    fn start(&mut self, more: bool) -> Option<(u64, Option<String>)> {
        let cursor = if more { Some(self.next.clone()?) } else { None };
        let generation = self.request.start()?;
        self.cursor = cursor.clone();
        self.error = None;
        Some((generation, cursor))
    }

    fn finish<T, K: Eq + std::hash::Hash>(
        &mut self,
        generation: u64,
        result: Result<Page<T>, String>,
        load: &mut Load<Vec<T>>,
        key: impl Fn(&T) -> K,
    ) -> bool {
        if !self.request.finish(generation) {
            return false;
        }
        match result {
            Ok(page) => {
                if let Some(cursor) = self.cursor.take() {
                    self.completed.insert(cursor);
                    if let Load::Loaded(items) = load {
                        let mut seen: HashSet<_> = items.iter().map(&key).collect();
                        items.extend(page.items.into_iter().filter(|item| seen.insert(key(item))));
                    } else {
                        *load = Load::Loaded(page.items);
                    }
                    if page.warning.is_some() {
                        self.warning = page.warning;
                    }
                } else {
                    self.completed.clear();
                    *load = Load::Loaded(page.items);
                    self.warning = page.warning;
                }
                self.next = page.next;
                if self
                    .next
                    .as_ref()
                    .is_some_and(|next| self.completed.contains(next))
                {
                    self.next = None;
                    self.error =
                        Some("GitHub repeated a pagination link. Refresh to retry.".to_string());
                }
            }
            Err(error) if matches!(load, Load::Loaded(_)) => self.error = Some(error),
            Err(error) => *load = Load::Failed(error),
        }
        true
    }
}

/// Which top-level screen is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Repos,
    RepoDetail,
    MyPulls,
}

/// Which tab of the repo-detail screen is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Issues,
    Pulls,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum StateFilter {
    #[default]
    All,
    Open,
    Closed,
    Merged,
}

impl StateFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }

    fn matches(self, state: &str) -> bool {
        self == Self::All || self.label() == state
    }
}

/// Braille spinner frames used while data loads.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The whole application state. Fields are `pub(crate)` so the `ui` module can
/// render them; all mutation happens through methods here.
pub struct App {
    pub(crate) running: bool,
    client: Option<Github>,
    sender: Sender<Event>,
    request_pool: RequestPool,

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
    pub(crate) my_pulls: Load<Vec<SearchedPullRequest>>,
    pub(crate) my_pulls_state: ListState,
    pub(crate) review_mode: bool,
    pub(crate) preview_open: bool,
    pub(crate) preview: Load<ItemDetail>,
    pub(crate) preview_scroll: u16,
    pub(crate) preview_repo: String,
    pub(crate) preview_state: String,
    preview_number: u64,
    preview_url: String,
    preview_request: Request,

    pub(crate) show_help: bool,
    pub(crate) status: Option<String>,
    spinner: usize,
    user_request: Request,
    pub(crate) repos_progress: ListProgress,
    pub(crate) issues_progress: ListProgress,
    pub(crate) pulls_progress: ListProgress,
    pub(crate) my_pulls_progress: ListProgress,

    /// Whether the search input bar is active.
    pub(crate) search_active: bool,
    /// Current search query text.
    pub(crate) search_query: String,
    pub(crate) state_filter: StateFilter,
    /// Indices into the loaded repos that match the current query.
    pub(crate) filtered_repos: Vec<usize>,
    /// Indices into the loaded issues that match the current query.
    pub(crate) filtered_issues: Vec<usize>,
    /// Indices into the loaded pulls that match the current query.
    pub(crate) filtered_pulls: Vec<usize>,
    /// Indices into the loaded global "my pulls" that match the current query.
    pub(crate) filtered_my_pulls: Vec<usize>,
}

impl App {
    /// Build the app. `client` is `None` when no token could be resolved, in
    /// which case the UI shows an auth-required screen instead of fetching.
    pub fn new(client: Option<Github>, sender: Sender<Event>) -> Self {
        Self {
            running: false,
            client,
            sender,
            request_pool: RequestPool::default(),
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
            my_pulls: Load::Idle,
            my_pulls_state: ListState::default(),
            review_mode: false,
            preview_open: false,
            preview: Load::Idle,
            preview_scroll: 0,
            preview_repo: String::new(),
            preview_state: String::new(),
            preview_number: 0,
            preview_url: String::new(),
            preview_request: Request::default(),
            show_help: false,
            status: None,
            spinner: 0,
            user_request: Request::default(),
            repos_progress: ListProgress::default(),
            issues_progress: ListProgress::default(),
            pulls_progress: ListProgress::default(),
            my_pulls_progress: ListProgress::default(),
            search_active: false,
            search_query: String::new(),
            state_filter: StateFilter::All,
            filtered_repos: Vec::new(),
            filtered_issues: Vec::new(),
            filtered_pulls: Vec::new(),
            filtered_my_pulls: Vec::new(),
        }
    }

    /// Run the main loop until the user quits.
    pub fn run(mut self, mut terminal: DefaultTerminal, events: Events) -> color_eyre::Result<()> {
        self.running = true;
        self.bootstrap();
        let mut redraw = true;
        while self.running {
            if redraw {
                terminal.draw(|frame| ui::render(frame, &mut self))?;
            }
            redraw = self.on_event(events.next()?)?;
        }
        Ok(())
    }

    fn on_event(&mut self, event: Event) -> color_eyre::Result<bool> {
        match event {
            Event::Terminal(event) => self.on_terminal(event),
            Event::Message(message) => self.on_message(message),
            Event::InputFailed(error) => return Err(error.into()),
            Event::Tick => {
                let loading = if self.preview_open {
                    self.preview_request.in_flight
                } else {
                    self.progress().loading()
                        || (self.screen == Screen::MyPulls && self.user_request.in_flight)
                };
                if !loading {
                    return Ok(false);
                }
                self.spinner = self.spinner.wrapping_add(1);
            }
        }
        Ok(true)
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
            self.fetch_repos(false);
        }
    }

    // --- event handling ----------------------------------------------------

    fn on_terminal(&mut self, event: TerminalEvent) {
        if let TerminalEvent::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            self.on_key(key);
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

        if self.preview_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('p') => {
                    self.preview_open = false;
                    self.preview_request.invalidate();
                }
                KeyCode::Char('q') => self.running = false,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.preview_scroll = self.preview_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1)
                }
                KeyCode::Home | KeyCode::Char('g') => self.preview_scroll = 0,
                KeyCode::Char('o') | KeyCode::Enter => {
                    let url = self.preview_url.clone();
                    self.launch_url(&url);
                }
                KeyCode::Char('r') => self.open_preview(),
                _ => {}
            }
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
                    self.activate_selection();
                    self.exit_search();
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
            KeyCode::Char('n') => self.load_more(),
            KeyCode::Char('s') => self.cycle_state_filter(),
            KeyCode::Char('p') => self.open_preview(),
            KeyCode::Char('o') => self.open_selected_in_browser(),
            KeyCode::Char('/') => self.enter_search(),
            KeyCode::Char('m') => self.open_my_pulls(),
            KeyCode::Char('v') => self.open_pull_queue(true),
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
        let updated = match message {
            Message::User { request, result } => {
                if !self.user_request.finish(request) {
                    return;
                }
                self.user = result.into();
                if self.screen == Screen::MyPulls && matches!(self.my_pulls, Load::Idle) {
                    if let Load::Failed(message) = &self.user {
                        self.my_pulls =
                            Load::Failed(format!("Could not load your account: {message}"));
                    } else {
                        self.fetch_my_pulls(false);
                    }
                }
                false
            }
            Message::Preview { request, result } => {
                if self.preview_request.finish(request) {
                    self.preview = result.into();
                }
                false
            }
            Message::Repos { request, result } => {
                self.repos_progress
                    .finish(request, result, &mut self.repos, |repo| {
                        repo.full_name.clone()
                    })
            }
            Message::Issues {
                request,
                repo,
                result,
            } if self.is_active_repo(&repo) => {
                self.issues_progress
                    .finish(request, result, &mut self.issues, |issue| issue.number)
            }
            Message::Pulls {
                request,
                repo,
                result,
            } if self.is_active_repo(&repo) => {
                self.pulls_progress
                    .finish(request, result, &mut self.pulls, |pr| pr.number)
            }
            Message::MyPulls { request, result } => {
                self.my_pulls_progress
                    .finish(request, result, &mut self.my_pulls, |pr| {
                        (pr.repo(), pr.number)
                    })
            }
            _ => false,
        };
        if updated {
            self.refilter();
        }
    }

    // --- actions -----------------------------------------------------------

    /// Enter: on the repo list open the detail view; in detail or the global
    /// pull-request list, open the selected item in a browser.
    fn activate_selection(&mut self) {
        match self.screen {
            Screen::Repos => self.open_selected_repo(),
            Screen::RepoDetail | Screen::MyPulls => self.open_selected_in_browser(),
        }
    }

    fn open_selected_repo(&mut self) {
        let Some(repo) = self.selected_repo() else {
            return;
        };
        self.active_repo = Some(repo.clone());
        self.screen = Screen::RepoDetail;
        self.tab = Tab::Issues;
        self.issues_progress.reset();
        self.pulls_progress.reset();
        self.issues = Load::Idle;
        self.pulls = Load::Idle;
        self.issues_state.select(None);
        self.pulls_state.select(None);
        self.clear_search();
        self.fetch_issues(&repo, false);
        self.fetch_pulls(&repo, false);
    }

    fn go_back(&mut self) {
        match self.screen {
            Screen::RepoDetail => {
                self.screen = Screen::Repos;
                self.active_repo = None;
                self.issues_progress.request.invalidate();
                self.pulls_progress.request.invalidate();
                self.clear_search();
            }
            Screen::MyPulls => {
                self.screen = Screen::Repos;
                self.clear_search();
            }
            Screen::Repos => {}
        }
    }

    fn toggle_tab(&mut self) {
        if self.screen == Screen::RepoDetail {
            self.tab = match self.tab {
                Tab::Issues => Tab::Pulls,
                Tab::Pulls => Tab::Issues,
            };
            self.state_filter = StateFilter::All;
            self.refilter();
        }
    }

    fn refresh(&mut self) {
        match self.screen {
            Screen::Repos => self.fetch_repos(false),
            Screen::RepoDetail => {
                if let Some(repo) = self.active_repo.clone() {
                    self.fetch_issues(&repo, false);
                    self.fetch_pulls(&repo, false);
                }
            }
            Screen::MyPulls => self.fetch_my_pulls(false),
        }
    }

    fn open_selected_in_browser(&mut self) {
        let Some(url) = self.selected_url() else {
            return;
        };
        self.launch_url(&url);
    }

    fn launch_url(&mut self, url: &str) {
        match open_url(url) {
            Ok(()) => self.status = Some(format!("Opening {url}")),
            Err(_) => self.status = Some("Could not launch a browser.".to_string()),
        }
    }

    fn open_my_pulls(&mut self) {
        self.open_pull_queue(false);
    }

    fn open_pull_queue(&mut self, review: bool) {
        if self.review_mode != review {
            self.review_mode = review;
            self.my_pulls_progress.reset();
            self.my_pulls = Load::Idle;
            self.my_pulls_state.select(None);
        }
        self.screen = Screen::MyPulls;
        self.active_repo = None;
        self.issues_progress.request.invalidate();
        self.pulls_progress.request.invalidate();
        self.clear_search();
        if !matches!(self.my_pulls, Load::Loaded(_) | Load::Loading) {
            self.fetch_my_pulls(false);
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
            Screen::MyPulls => (&mut self.my_pulls_state, self.filtered_my_pulls.len()),
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
            Screen::MyPulls => {
                let Load::Loaded(items) = &self.my_pulls else {
                    return None;
                };
                let visible_idx = self.my_pulls_state.selected()?;
                let real_idx = *self.filtered_my_pulls.get(visible_idx)?;
                let pr = items.get(real_idx)?;
                pr.html_url.clone().or_else(|| {
                    Some(format!(
                        "https://github.com/{}/pull/{}",
                        pr.repo(),
                        pr.number
                    ))
                })
            }
        }
    }

    fn is_active_repo(&self, full_name: &str) -> bool {
        self.active_repo
            .as_ref()
            .is_some_and(|repo| repo.full_name == full_name)
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
        self.state_filter = StateFilter::All;
        self.filtered_repos.clear();
        self.filtered_issues.clear();
        self.filtered_pulls.clear();
        self.filtered_my_pulls.clear();
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
                    self.state_filter.matches(&issue.state)
                        && matches_item(&query, &issue.title, issue.author(), issue.number)
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
                .filter(|(_, pr)| {
                    self.state_filter.matches(pr.effective_state())
                        && matches_item(&query, &pr.title, pr.author(), pr.number)
                })
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_pulls.clear();
        }

        // Global "my pulls"
        if let Load::Loaded(pulls) = &self.my_pulls {
            self.filtered_my_pulls = pulls
                .iter()
                .enumerate()
                .filter(|(_, pr)| {
                    self.state_filter.matches(pr.effective_state())
                        && (matches_item(&query, &pr.title, pr.author(), pr.number)
                            || pr.repo().to_lowercase().contains(&query))
                })
                .map(|(i, _)| i)
                .collect();
        } else {
            self.filtered_my_pulls.clear();
        }

        // Clamp cursor for the active list.
        let (state, len) = match self.screen {
            Screen::Repos => (&mut self.repos_state, self.filtered_repos.len()),
            Screen::RepoDetail => match self.tab {
                Tab::Issues => (&mut self.issues_state, self.filtered_issues.len()),
                Tab::Pulls => (&mut self.pulls_state, self.filtered_pulls.len()),
            },
            Screen::MyPulls => (&mut self.my_pulls_state, self.filtered_my_pulls.len()),
        };
        if len == 0 {
            state.select(None);
        } else if state.selected().is_none_or(|i| i >= len) {
            state.select(Some(0));
        }
    }

    // --- fetches (dispatched to bounded workers) ---------------------------

    fn dispatch<T: Send + 'static>(
        &mut self,
        cancellation: Cancellation,
        fetch: impl FnOnce() -> Result<T, String> + Send + 'static,
        message: impl FnOnce(Result<T, String>) -> Message + Send + Clone + 'static,
    ) {
        let sender = self.sender.clone();
        let rejected = message.clone();
        if let Err(error) = self.request_pool.execute(cancellation, move || {
            let _ = sender.send(Event::Message(message(fetch())));
        }) {
            self.on_message(rejected(Err(error)));
        }
    }

    fn fetch_user(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(request) = self.user_request.start() else {
            return;
        };
        self.user = Load::Loading;
        self.dispatch(
            self.user_request.cancellation.clone(),
            move || client.current_user(),
            move |result| Message::User { request, result },
        );
    }

    fn fetch_repos(&mut self, more: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some((request, next)) = self.repos_progress.start(more) else {
            return;
        };
        if !matches!(self.repos, Load::Loaded(_)) {
            self.repos = Load::Loading;
        }
        self.dispatch(
            self.repos_progress.request.cancellation.clone(),
            move || client.repos_page(next.as_deref()),
            move |result| Message::Repos { request, result },
        );
    }

    fn fetch_issues(&mut self, repo: &Repo, more: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some((request, next)) = self.issues_progress.start(more) else {
            return;
        };
        let (owner, name) = repo.owner_repo();
        let (owner, name, full) = (owner.to_string(), name.to_string(), repo.full_name.clone());
        if !matches!(self.issues, Load::Loaded(_)) {
            self.issues = Load::Loading;
        }
        self.dispatch(
            self.issues_progress.request.cancellation.clone(),
            move || client.issues_page(&owner, &name, next.as_deref()),
            move |result| Message::Issues {
                request,
                repo: full,
                result,
            },
        );
    }

    fn fetch_pulls(&mut self, repo: &Repo, more: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some((request, next)) = self.pulls_progress.start(more) else {
            return;
        };
        let (owner, name) = repo.owner_repo();
        let (owner, name, full) = (owner.to_string(), name.to_string(), repo.full_name.clone());
        if !matches!(self.pulls, Load::Loaded(_)) {
            self.pulls = Load::Loading;
        }
        self.dispatch(
            self.pulls_progress.request.cancellation.clone(),
            move || client.pulls_page(&owner, &name, next.as_deref()),
            move |result| Message::Pulls {
                request,
                repo: full,
                result,
            },
        );
    }

    fn fetch_my_pulls(&mut self, more: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let login = match &self.user {
            Load::Loaded(user) => user.login.clone(),
            Load::Loading => {
                self.my_pulls = Load::Idle;
                return;
            }
            Load::Idle | Load::Failed(_) => {
                self.my_pulls = Load::Idle;
                self.fetch_user();
                return;
            }
        };
        let Some((request, next)) = self.my_pulls_progress.start(more) else {
            return;
        };
        if !matches!(self.my_pulls, Load::Loaded(_)) {
            self.my_pulls = Load::Loading;
        }
        let review = self.review_mode;
        self.dispatch(
            self.my_pulls_progress.request.cancellation.clone(),
            move || {
                if review {
                    client.review_pulls_page(&login, next.as_deref())
                } else {
                    client.my_pulls_page(&login, next.as_deref())
                }
            },
            move |result| Message::MyPulls { request, result },
        );
    }

    fn load_more(&mut self) {
        match self.screen {
            Screen::Repos => self.fetch_repos(true),
            Screen::RepoDetail => {
                if let Some(repo) = self.active_repo.clone() {
                    match self.tab {
                        Tab::Issues => self.fetch_issues(&repo, true),
                        Tab::Pulls => self.fetch_pulls(&repo, true),
                    }
                }
            }
            Screen::MyPulls => self.fetch_my_pulls(true),
        }
    }

    pub(crate) fn progress(&self) -> &ListProgress {
        match self.screen {
            Screen::Repos => &self.repos_progress,
            Screen::RepoDetail => match self.tab {
                Tab::Issues => &self.issues_progress,
                Tab::Pulls => &self.pulls_progress,
            },
            Screen::MyPulls => &self.my_pulls_progress,
        }
    }

    fn cycle_state_filter(&mut self) {
        if self.screen == Screen::Repos {
            return;
        }
        self.state_filter = match self.state_filter {
            StateFilter::All => StateFilter::Open,
            StateFilter::Open => StateFilter::Closed,
            StateFilter::Closed if self.screen != Screen::RepoDetail || self.tab == Tab::Pulls => {
                StateFilter::Merged
            }
            StateFilter::Closed | StateFilter::Merged => StateFilter::All,
        };
        self.refilter();
    }

    fn open_preview(&mut self) {
        if self.preview_request.in_flight {
            return;
        }
        let target = if self.preview_open {
            Some((
                self.preview_repo.clone(),
                self.preview_number,
                self.preview_state.clone(),
            ))
        } else {
            self.selected_item()
        };
        let Some((repo, number, state)) = target else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some((owner, name)) = repo.split_once('/') else {
            return;
        };
        let (owner, name) = (owner.to_string(), name.to_string());
        if !self.preview_open {
            self.preview_url = self.selected_url().unwrap_or_default();
        }
        self.preview_repo = repo;
        self.preview_number = number;
        self.preview_state = state;
        self.preview_scroll = 0;
        self.preview_open = true;
        self.preview = Load::Loading;
        let Some(request) = self.preview_request.start() else {
            return;
        };
        self.dispatch(
            self.preview_request.cancellation.clone(),
            move || client.item_detail(&owner, &name, number),
            move |result| Message::Preview { request, result },
        );
    }

    fn selected_item(&self) -> Option<(String, u64, String)> {
        match self.screen {
            Screen::Repos => None,
            Screen::RepoDetail => {
                let repo = self.active_repo.as_ref()?.full_name.clone();
                match self.tab {
                    Tab::Issues => {
                        let Load::Loaded(items) = &self.issues else {
                            return None;
                        };
                        let item =
                            items.get(*self.filtered_issues.get(self.issues_state.selected()?)?)?;
                        Some((repo, item.number, item.state.clone()))
                    }
                    Tab::Pulls => {
                        let Load::Loaded(items) = &self.pulls else {
                            return None;
                        };
                        let item =
                            items.get(*self.filtered_pulls.get(self.pulls_state.selected()?)?)?;
                        Some((repo, item.number, item.effective_state().to_string()))
                    }
                }
            }
            Screen::MyPulls => {
                let Load::Loaded(items) = &self.my_pulls else {
                    return None;
                };
                let item = items.get(
                    *self
                        .filtered_my_pulls
                        .get(self.my_pulls_state.selected()?)?,
                )?;
                Some((item.repo(), item.number, item.effective_state().to_string()))
            }
        }
    }
}

fn matches_item(query: &str, title: &str, author: &str, number: u64) -> bool {
    let number_query = query.strip_prefix('#').unwrap_or(query);
    query.is_empty()
        || title.to_lowercase().contains(query)
        || author.to_lowercase().contains(query)
        || (!number_query.is_empty() && number.to_string().contains(number_query))
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
    validate_browser_url(url)?;
    launch_browser(browser_command(url)).map(|_| ())
}

fn validate_browser_url(url: &str) -> std::io::Result<()> {
    let valid = url.parse::<ureq::http::Uri>().is_ok_and(|uri| {
        uri.scheme_str() == Some("https")
            && uri
                .authority()
                .is_some_and(|authority| authority.as_str() == "github.com")
            && uri.query().is_none()
            && uri
                .path()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Expected a GitHub HTTPS resource URL",
        ))
    }
}

fn launch_browser(
    mut command: Command,
) -> std::io::Result<thread::JoinHandle<std::io::Result<std::process::ExitStatus>>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(thread::spawn(move || child.wait()))
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

    fn fixture_app(
        bodies: Vec<&'static str>,
    ) -> (App, mpsc::Receiver<Event>, thread::JoinHandle<()>, String) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let client = Github::test_client(&root);
        let server = thread::spawn(move || {
            for body in bodies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "fixture request never arrived");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let (tx, rx) = mpsc::channel();
        (App::new(Some(client), tx), rx, server, root)
    }

    fn receive_message(app: &mut App, receiver: &mpsc::Receiver<Event>) {
        let event = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let Event::Message(message) = event else {
            panic!("expected fetch result")
        };
        app.on_message(message);
    }

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
            request: 1,
            repo: "old/repo".to_string(),
            result: Ok(Page {
                items: vec![],
                next: None,
                warning: None,
            }),
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

    #[test]
    fn search_enter_opens_the_filtered_repository() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/alpha"), repo("user/beta")]);
        app.enter_search();
        app.search_query = "beta".to_string();
        app.refilter();

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.screen, Screen::RepoDetail);
        assert_eq!(app.active_repo.as_ref().unwrap().full_name, "user/beta");
        assert!(!app.search_active);
        assert!(app.search_query.is_empty());
    }

    #[test]
    fn search_enter_with_no_matches_does_not_open_a_repository() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/alpha")]);
        app.enter_search();
        app.search_query = "missing".to_string();
        app.refilter();

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.screen, Screen::Repos);
        assert!(app.active_repo.is_none());
        assert!(!app.search_active);
    }

    #[test]
    fn search_escape_clears_without_activating() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("user/alpha"), repo("user/beta")]);
        app.enter_search();
        app.search_query = "beta".to_string();
        app.refilter();

        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(app.screen, Screen::Repos);
        assert!(app.active_repo.is_none());
        assert_eq!(app.filtered_repos, vec![0, 1]);
    }

    #[test]
    fn filtered_issue_and_pull_urls_use_source_indices() {
        let mut app = test_app();
        app.screen = Screen::RepoDetail;
        app.active_repo = Some(repo("owner/project"));
        app.issues = Load::Loaded(
            serde_json::from_str(
                r#"[{"number":1,"title":"alpha","state":"open"},
                {"number":2,"title":"beta","state":"open"}]"#,
            )
            .unwrap(),
        );
        app.pulls = Load::Loaded(
            serde_json::from_str(
                r#"[{"number":3,"title":"alpha","state":"open"},
                {"number":4,"title":"beta","state":"open"}]"#,
            )
            .unwrap(),
        );
        app.my_pulls = Load::Loaded(
            serde_json::from_str(
                r#"[{"number":5,"title":"alpha","state":"open"},
                {"number":6,"title":"beta","state":"open",
                 "html_url":"https://github.com/other/project/pull/6"}]"#,
            )
            .unwrap(),
        );
        app.search_query = "beta".to_string();
        app.refilter();
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://github.com/owner/project/issues/2")
        );
        app.tab = Tab::Pulls;
        app.refilter();
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://github.com/owner/project/pull/4")
        );
        app.screen = Screen::MyPulls;
        app.refilter();
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://github.com/other/project/pull/6")
        );
    }

    #[test]
    fn my_pulls_retries_a_failed_account_lookup() {
        let (mut app, rx, server, _) = fixture_app(vec![
            r#"{"login":"octocat"}"#,
            r#"{"total_count":0,"incomplete_results":false,"items":[]}"#,
        ]);
        app.user = Load::Failed("offline".to_string());
        app.open_my_pulls();
        assert!(matches!(app.user, Load::Loading));
        assert!(matches!(app.my_pulls, Load::Idle));
        receive_message(&mut app, &rx);
        receive_message(&mut app, &rx);
        assert!(matches!(app.my_pulls, Load::Loaded(_)));
        server.join().unwrap();
    }

    #[test]
    fn my_pulls_refresh_waits_for_the_account_in_flight() {
        let (mut app, rx, server, _) = fixture_app(vec![
            r#"{"login":"octocat"}"#,
            r#"{"total_count":0,"incomplete_results":false,"items":[]}"#,
        ]);
        app.fetch_user();
        app.open_my_pulls();
        app.refresh();
        assert!(matches!(app.my_pulls, Load::Idle));
        receive_message(&mut app, &rx);
        receive_message(&mut app, &rx);
        assert!(matches!(app.my_pulls, Load::Loaded(_)));
        server.join().unwrap();
    }

    #[test]
    fn my_pulls_displays_the_account_failure() {
        let mut app = test_app();
        app.screen = Screen::MyPulls;
        let request = app.user_request.start().unwrap();
        app.on_message(Message::User {
            request,
            result: Err("offline".to_string()),
        });
        assert!(matches!(&app.my_pulls, Load::Failed(message) if message.contains("offline")));
    }

    fn page<T>(items: Vec<T>, next: Option<&str>) -> Page<T> {
        Page {
            items,
            next: next.map(str::to_string),
            warning: None,
        }
    }

    #[test]
    fn request_coalesces_until_completion_and_allows_retry() {
        let mut request = Request::default();
        let first = request.start().unwrap();
        assert!(request.start().is_none());
        assert!(!request.finish(first.wrapping_add(1)));
        assert!(request.in_flight);
        assert!(request.finish(first));
        assert!(!request.finish(first));
        let second = request.start().unwrap();
        assert_ne!(first, second);
        assert!(request.finish(second));
    }

    #[test]
    fn invalidation_cancels_queued_work_without_cancelling_the_next_generation() {
        let mut app = test_app();
        app.request_pool = RequestPool::with_limits(1, 1).unwrap();
        let (started, occupied) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        app.request_pool
            .execute(Cancellation::default(), move || {
                started.send(()).unwrap();
                let _ = blocked.recv();
            })
            .unwrap();
        occupied
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let old = app.repos_progress.request.start().unwrap();
        app.dispatch(
            app.repos_progress.request.cancellation.clone(),
            || panic!("abandoned repository fetch ran"),
            move |result| Message::Repos {
                request: old,
                result,
            },
        );

        app.repos_progress.reset();
        let current = app.repos_progress.request.start().unwrap();
        let (completed, result) = mpsc::channel();
        app.request_pool
            .execute(app.repos_progress.request.cancellation.clone(), move || {
                completed.send(()).unwrap();
            })
            .unwrap();
        release.send(()).unwrap();

        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_ne!(old, current);
        assert!(app.repos_progress.request.in_flight);
    }

    #[test]
    fn saturated_dispatch_finishes_every_resource_with_a_retryable_error() {
        let (sender, _receiver) = mpsc::channel();
        let mut app = App::new(Some(Github::new("test-token".to_string())), sender);
        app.request_pool = RequestPool::with_limits(1, 1).unwrap();
        let (started, occupied) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        app.request_pool
            .execute(Cancellation::default(), move || {
                started.send(()).unwrap();
                let _ = blocked.recv();
            })
            .unwrap();
        occupied
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        app.request_pool
            .execute(Cancellation::default(), || {})
            .unwrap();

        app.bootstrap();
        assert!(matches!(&app.user, Load::Failed(error) if error.contains("retry")));
        assert!(matches!(&app.repos, Load::Failed(error) if error.contains("retry")));
        assert!(!app.user_request.in_flight);
        assert!(!app.repos_progress.loading());

        let repository = repo("owner/project");
        app.screen = Screen::RepoDetail;
        app.active_repo = Some(repository.clone());
        app.fetch_issues(&repository, false);
        app.fetch_pulls(&repository, false);
        assert!(matches!(&app.issues, Load::Failed(error) if error.contains("retry")));
        assert!(matches!(&app.pulls, Load::Failed(error) if error.contains("retry")));
        assert!(!app.issues_progress.loading());
        assert!(!app.pulls_progress.loading());

        app.user = Load::Loaded(User {
            login: "octocat".to_string(),
        });
        app.open_my_pulls();
        assert!(matches!(&app.my_pulls, Load::Failed(error) if error.contains("retry")));
        assert!(!app.my_pulls_progress.loading());

        app.screen = Screen::RepoDetail;
        app.active_repo = Some(repository);
        app.issues = Load::Loaded(
            serde_json::from_str(r#"[{"number":42,"title":"work","state":"open"}]"#).unwrap(),
        );
        app.refilter();
        app.open_preview();
        assert!(app.preview_open);
        assert!(matches!(&app.preview, Load::Failed(error) if error.contains("retry")));
        assert!(!app.preview_request.in_flight);

        app.repos = Load::Loaded(vec![repo("me/retained")]);
        app.screen = Screen::Repos;
        app.refilter();
        app.refresh();
        assert_eq!(app.selected_repo().unwrap().full_name, "me/retained");
        assert!(
            app.repos_progress
                .error
                .as_deref()
                .unwrap()
                .contains("retry")
        );
        assert!(!app.repos_progress.loading());
        release.send(()).unwrap();
    }

    #[test]
    fn repeated_refresh_keeps_one_request_and_the_loaded_selection() {
        let (mut app, rx, server, _) =
            fixture_app(vec![r#"[{"name":"new","full_name":"me/new"}]"#]);
        app.repos = Load::Loaded(vec![repo("me/first"), repo("me/second")]);
        app.refilter();
        app.repos_state.select(Some(1));

        app.refresh();
        let request = app.repos_progress.request.generation;
        app.refresh();
        app.refresh();

        assert_eq!(app.repos_progress.request.generation, request);
        assert!(app.repos_progress.loading());
        assert_eq!(app.repos.len(), 2);
        assert_eq!(app.selected_repo().unwrap().full_name, "me/second");
        receive_message(&mut app, &rx);
        assert!(!app.repos_progress.loading());
        assert_eq!(app.selected_repo().unwrap().full_name, "me/new");
        server.join().unwrap();
    }

    #[test]
    fn superseded_repo_results_do_not_clear_the_current_request() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("me/current")]);
        app.refilter();
        let (old, _) = app.repos_progress.start(false).unwrap();
        app.repos_progress.request.invalidate();
        let (current, _) = app.repos_progress.start(false).unwrap();

        app.on_message(Message::Repos {
            request: old,
            result: Ok(page(vec![repo("me/stale")], None)),
        });
        app.on_message(Message::Repos {
            request: old,
            result: Err("old failure".to_string()),
        });

        assert!(app.repos_progress.loading());
        assert_eq!(app.repos_progress.request.generation, current);
        assert!(app.repos_progress.error.is_none());
        assert_eq!(app.selected_repo().unwrap().full_name, "me/current");
        app.on_message(Message::Repos {
            request: current,
            result: Ok(page(vec![repo("me/new")], None)),
        });
        assert!(!app.repos_progress.loading());
        assert_eq!(app.selected_repo().unwrap().full_name, "me/new");
    }

    #[test]
    fn superseded_user_results_leave_the_current_lookup_pending() {
        let mut app = test_app();
        app.screen = Screen::MyPulls;
        app.user = Load::Loading;
        let old = app.user_request.start().unwrap();
        app.user_request.invalidate();
        let current = app.user_request.start().unwrap();

        app.on_message(Message::User {
            request: old,
            result: Ok(User {
                login: "stale".to_string(),
            }),
        });
        app.on_message(Message::User {
            request: old,
            result: Err("old failure".to_string()),
        });

        assert!(matches!(app.user, Load::Loading));
        assert!(matches!(app.my_pulls, Load::Idle));
        assert!(app.user_request.in_flight);
        assert_eq!(app.user_request.generation, current);
        app.on_message(Message::User {
            request: current,
            result: Err("current failure".to_string()),
        });
        assert!(!app.user_request.in_flight);
        assert!(matches!(&app.my_pulls, Load::Failed(error) if error.contains("current failure")));
    }

    #[test]
    fn reentering_the_same_repo_rejects_both_old_detail_results() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("me/project")]);
        app.refilter();
        app.open_selected_repo();
        let (old_issues, _) = app.issues_progress.start(false).unwrap();
        let (old_pulls, _) = app.pulls_progress.start(false).unwrap();
        app.go_back();
        app.open_selected_repo();
        let (current_issues, _) = app.issues_progress.start(false).unwrap();
        let (current_pulls, _) = app.pulls_progress.start(false).unwrap();

        app.on_message(Message::Issues {
            request: old_issues,
            repo: "me/project".to_string(),
            result: Err("old issue failure".to_string()),
        });
        app.on_message(Message::Pulls {
            request: old_pulls,
            repo: "me/project".to_string(),
            result: Ok(page(vec![], None)),
        });

        assert!(matches!(app.issues, Load::Idle));
        assert!(matches!(app.pulls, Load::Idle));
        assert!(app.issues_progress.loading());
        assert!(app.pulls_progress.loading());
        app.on_message(Message::Issues {
            request: current_issues,
            repo: "me/project".to_string(),
            result: Ok(page(vec![], None)),
        });
        app.on_message(Message::Pulls {
            request: current_pulls,
            repo: "me/project".to_string(),
            result: Ok(page(vec![], None)),
        });
        assert!(matches!(app.issues, Load::Loaded(_)));
        assert!(matches!(app.pulls, Load::Loaded(_)));
        assert!(!app.issues_progress.loading());
        assert!(!app.pulls_progress.loading());
    }

    #[test]
    fn failed_continuation_preserves_rows_and_retries_the_same_cursor_once() {
        let mut app = test_app();
        app.repos = Load::Loaded(vec![repo("me/first"), repo("me/second")]);
        app.repos_progress.next = Some("page-two".to_string());
        app.refilter();
        app.repos_state.select(Some(1));
        let (failed, cursor) = app.repos_progress.start(true).unwrap();
        assert_eq!(cursor.as_deref(), Some("page-two"));

        app.on_message(Message::Repos {
            request: failed,
            result: Err("page failed".to_string()),
        });

        assert_eq!(app.repos.len(), 2);
        assert_eq!(app.repos_state.selected(), Some(1));
        assert_eq!(app.repos_progress.next.as_deref(), Some("page-two"));
        assert_eq!(app.repos_progress.error.as_deref(), Some("page failed"));
        let (retried, cursor) = app.repos_progress.start(true).unwrap();
        assert_eq!(cursor.as_deref(), Some("page-two"));
        assert!(app.repos_progress.error.is_none());
        app.on_message(Message::Repos {
            request: retried,
            result: Ok(page(vec![repo("me/second"), repo("me/third")], None)),
        });
        assert_eq!(app.repos.len(), 3);
        assert_eq!(app.selected_repo().unwrap().full_name, "me/second");
        assert!(app.repos_progress.next.is_none());
        assert!(app.repos_progress.start(true).is_none());
    }

    #[test]
    fn fetching_a_continuation_keeps_the_first_page_visible() {
        let (mut app, rx, server, root) =
            fixture_app(vec![r#"[{"name":"third","full_name":"me/third"}]"#]);
        app.repos = Load::Loaded(vec![repo("me/first"), repo("me/second")]);
        app.repos_progress.next = Some(format!("{root}/user/repos?page=2"));
        app.refilter();
        app.repos_state.select(Some(1));

        app.load_more();

        assert!(app.repos_progress.loading());
        assert_eq!(app.repos.len(), 2);
        assert_eq!(app.selected_repo().unwrap().full_name, "me/second");
        receive_message(&mut app, &rx);
        assert_eq!(app.repos.len(), 3);
        assert_eq!(app.selected_repo().unwrap().full_name, "me/second");
        assert!(!app.repos_progress.loading());
        server.join().unwrap();
    }

    #[test]
    fn failed_refresh_keeps_loaded_rows_and_a_visible_error() {
        let (mut app, rx, server, _) = fixture_app(vec!["invalid JSON"]);
        app.repos = Load::Loaded(vec![repo("me/first"), repo("me/second")]);
        app.repos_progress.next = Some("page-two".to_string());
        app.refilter();
        app.repos_state.select(Some(1));

        app.refresh();
        assert_eq!(app.repos.len(), 2);
        receive_message(&mut app, &rx);

        assert_eq!(app.repos.len(), 2);
        assert_eq!(app.selected_repo().unwrap().full_name, "me/second");
        assert_eq!(app.repos_progress.next.as_deref(), Some("page-two"));
        assert!(
            app.repos_progress
                .error
                .as_deref()
                .unwrap()
                .contains("Could not parse")
        );
        assert!(!app.repos_progress.loading());
        server.join().unwrap();
    }

    #[test]
    fn page_warnings_persist_until_a_successful_refresh_replaces_them() {
        let mut progress = ListProgress::default();
        let mut loaded = Load::Idle;
        let (first, _) = progress.start(false).unwrap();
        let mut partial = page(vec![1u8], Some("page-two"));
        partial.warning = Some("incomplete search".to_string());
        assert!(progress.finish(first, Ok(partial), &mut loaded, |item| *item));
        let (second, _) = progress.start(true).unwrap();
        assert!(progress.finish(second, Ok(page(vec![2], None)), &mut loaded, |item| *item));
        assert_eq!(progress.warning.as_deref(), Some("incomplete search"));

        let (failed, _) = progress.start(false).unwrap();
        progress.finish(failed, Err("offline".to_string()), &mut loaded, |item| {
            *item
        });
        assert_eq!(progress.warning.as_deref(), Some("incomplete search"));
        let (refreshed, _) = progress.start(false).unwrap();
        progress.finish(refreshed, Ok(page(vec![3], None)), &mut loaded, |item| {
            *item
        });
        assert!(progress.warning.is_none());
        assert!(progress.error.is_none());
        assert!(matches!(loaded, Load::Loaded(items) if items == [3]));
    }

    #[test]
    fn pagination_cycles_stop_after_each_cursor_is_consumed_once() {
        let mut progress = ListProgress::default();
        let mut loaded = Load::Idle;
        for (more, item, next) in [
            (false, 1u8, "page-two"),
            (true, 2, "page-three"),
            (true, 3, "page-two"),
        ] {
            let (request, _) = progress.start(more).unwrap();
            assert!(progress.finish(
                request,
                Ok(page(vec![item], Some(next))),
                &mut loaded,
                |item| *item
            ));
        }
        assert!(matches!(loaded, Load::Loaded(items) if items == [1, 2, 3]));
        assert!(progress.next.is_none());
        assert!(progress.error.as_deref().unwrap().contains("repeated"));
        assert!(progress.start(true).is_none());
    }

    #[test]
    fn my_pulls_reentry_preserves_loaded_selection_and_continuation() {
        let mut app = test_app();
        app.my_pulls = Load::Loaded(serde_json::from_str(
            r#"[{"number":1,"title":"first","state":"open","repository_url":"https://api.github.com/repos/me/first"},
                {"number":2,"title":"second","state":"open","repository_url":"https://api.github.com/repos/me/second"}]"#,
        ).unwrap());
        app.screen = Screen::MyPulls;
        app.my_pulls_progress.next = Some("page-two".to_string());
        app.refilter();
        app.my_pulls_state.select(Some(1));
        let selected = app.selected_url();

        app.open_my_pulls();
        app.go_back();
        app.open_my_pulls();

        assert_eq!(app.screen, Screen::MyPulls);
        assert_eq!(app.my_pulls.len(), 2);
        assert_eq!(app.my_pulls_state.selected(), Some(1));
        assert_eq!(app.selected_url(), selected);
        assert_eq!(app.my_pulls_progress.next.as_deref(), Some("page-two"));
        assert!(!app.my_pulls_progress.loading());
    }

    #[test]
    fn ticks_redraw_only_when_the_visible_screen_is_loading() {
        let mut app = test_app();
        let initial_spinner = app.spinner;
        assert!(!app.on_event(Event::Tick).unwrap());
        app.repos = Load::Loaded(vec![repo("me/project")]);
        app.refilter();
        assert!(!app.on_event(Event::Tick).unwrap());
        app.issues_progress.start(false).unwrap();
        assert!(!app.on_event(Event::Tick).unwrap());
        assert_eq!(app.spinner, initial_spinner);

        app.repos_progress.start(false).unwrap();
        assert!(app.on_event(Event::Tick).unwrap());
        assert_eq!(app.spinner, initial_spinner.wrapping_add(1));
        app.screen = Screen::MyPulls;
        assert!(!app.on_event(Event::Tick).unwrap());
        app.user = Load::Loading;
        app.user_request.start().unwrap();
        assert!(app.on_event(Event::Tick).unwrap());
        assert_eq!(app.spinner, initial_spinner.wrapping_add(2));
    }

    #[test]
    fn terminal_input_failure_exits_event_processing_with_an_error() {
        let mut app = test_app();
        let result = app.on_event(Event::InputFailed(std::io::Error::other(
            "fixture input failure",
        )));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("fixture input failure")
        );
    }
    #[test]
    fn browser_launch_accepts_only_canonical_github_resource_urls() {
        for url in [
            "https://github.com/owner/repo",
            "https://github.com/owner/repo/issues/12",
            "https://github.com/owner/repo/pull/7",
        ] {
            assert!(validate_browser_url(url).is_ok());
        }
        for url in [
            "http://github.com/owner/repo",
            "https://example.com/",
            "https://github.com.evil.example/",
            "https://github.com@evil.example/",
            "https://github.com:443/repo",
            "https://github.com/a/b?x=1&y=2",
            "https://github.com/a/b%20x",
            "https://github.com/a/b|other",
            "file:///tmp/anything",
        ] {
            assert!(open_url(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn browser_launcher_is_reaped_in_a_worker() {
        #[cfg(unix)]
        let mut command = Command::new("sh");
        #[cfg(windows)]
        let mut command = Command::new("cmd");
        #[cfg(unix)]
        command.args(["-c", "exit 0"]);
        #[cfg(windows)]
        command.args(["/C", "exit", "0"]);
        let worker = launch_browser(command).unwrap();
        assert!(worker.join().unwrap().unwrap().success());
    }
    #[test]
    fn state_and_text_filters_compose_without_losing_source_indices() {
        let mut app = test_app();
        app.screen = Screen::RepoDetail;
        app.active_repo = Some(repo("owner/project"));
        app.tab = Tab::Pulls;
        app.pulls = Load::Loaded(
            serde_json::from_str(
                r#"[
            {"number":1,"title":"old","state":"closed"},
            {"number":42,"title":"work","state":"open","user":{"login":"Octocat"}},
            {"number":43,"title":"work","state":"closed","merged_at":"2026-01-01"}
        ]"#,
            )
            .unwrap(),
        );
        app.refilter();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(app.filtered_pulls, vec![1]);
        app.search_query = "#42".to_string();
        app.refilter();
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://github.com/owner/project/pull/42")
        );
        app.search_query = "octocat".to_string();
        app.refilter();
        assert_eq!(app.filtered_pulls, vec![1]);
        app.cycle_state_filter();
        assert!(app.filtered_pulls.is_empty());
        assert!(app.pulls_state.selected().is_none());
        app.exit_search();
        assert_eq!(app.filtered_pulls, vec![0]);
        app.cycle_state_filter();
        assert_eq!(app.filtered_pulls, vec![2]);
        app.cycle_state_filter();
        assert_eq!(app.filtered_pulls, vec![0, 1, 2]);
    }

    #[test]
    fn issues_cycle_only_applicable_states_and_search_their_authors() {
        let mut app = test_app();
        app.screen = Screen::RepoDetail;
        app.issues = Load::Loaded(
            serde_json::from_str(
                r#"[
            {"number":7,"title":"first","state":"open","user":{"login":"Alice"}},
            {"number":8,"title":"second","state":"closed"}
        ]"#,
            )
            .unwrap(),
        );
        app.search_query = "alice".to_string();
        app.refilter();
        assert_eq!(app.filtered_issues, vec![0]);
        app.exit_search();
        for expected in ["open", "closed", "all"] {
            app.cycle_state_filter();
            assert_eq!(app.state_filter.label(), expected);
        }
        assert!(matches_item("#8", "second", "unknown", 8));
        assert!(!matches_item("#", "second", "unknown", 8));
    }

    #[test]
    fn global_pull_search_matches_number_author_and_repository() {
        let mut app = test_app();
        app.screen = Screen::MyPulls;
        app.my_pulls = Load::Loaded(serde_json::from_str(r#"[
            {"number":42,"title":"work","state":"open","user":{"login":"Alice"},"repository_url":"https://api.github.com/repos/owner/project"}
        ]"#).unwrap());
        for query in ["#42", "42", "ALICE", "owner/project"] {
            app.search_query = query.to_string();
            app.refilter();
            assert_eq!(app.filtered_my_pulls, vec![0]);
        }
    }
    fn issue_preview_app(
        bodies: Vec<&'static str>,
    ) -> (App, mpsc::Receiver<Event>, thread::JoinHandle<()>) {
        let (mut app, rx, server, _) = fixture_app(bodies);
        app.screen = Screen::RepoDetail;
        app.active_repo = Some(repo("owner/project"));
        app.issues = Load::Loaded(
            serde_json::from_str(r#"[{"number":42,"title":"work","state":"open"}]"#).unwrap(),
        );
        app.refilter();
        (app, rx, server)
    }

    #[test]
    fn preview_fetches_on_demand_and_coalesces_duplicate_fetches() {
        let (mut app, rx, server) = issue_preview_app(vec![
            r#"{"number":42,"title":"work","state":"open","body":"Details"}"#,
        ]);
        assert!(!app.preview_open);
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        let request = app.preview_request.generation;
        assert!(app.preview_open);
        assert!(app.on_event(Event::Tick).unwrap());
        app.open_preview();
        assert_eq!(app.preview_request.generation, request);
        receive_message(&mut app, &rx);
        assert!(
            matches!(&app.preview, Load::Loaded(detail) if detail.body.as_deref()==Some("Details"))
        );
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 1);
        app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, 0);
        assert!(!app.on_event(Event::Tick).unwrap());
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.preview_open);
        assert_eq!(app.screen, Screen::RepoDetail);
        server.join().unwrap();
    }

    #[test]
    fn closing_preview_rejects_its_pending_response() {
        let mut app = test_app();
        app.screen = Screen::RepoDetail;
        app.preview_open = true;
        app.preview = Load::Loading;
        let request = app.preview_request.start().unwrap();
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.on_message(Message::Preview {
            request,
            result: Ok(
                serde_json::from_str(r#"{"number":42,"title":"work","state":"open"}"#).unwrap(),
            ),
        });
        assert!(!app.preview_open);
        assert!(!app.preview_request.in_flight);
        assert!(matches!(app.preview, Load::Loading));
    }

    #[test]
    fn preview_retry_keeps_the_captured_item_when_the_list_changes() {
        let (mut app, rx, server) = issue_preview_app(vec![
            r#"{"number":42,"title":"work","state":"open"}"#,
            r#"{"number":42,"title":"work","state":"open","body":"Retried"}"#,
        ]);
        app.open_preview();
        receive_message(&mut app, &rx);
        app.issues = Load::Loaded(
            serde_json::from_str(r#"[{"number":99,"title":"other","state":"closed"}]"#).unwrap(),
        );
        app.refilter();
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.preview_number, 42);
        assert_eq!(
            app.preview_url,
            "https://github.com/owner/project/issues/42"
        );
        receive_message(&mut app, &rx);
        assert!(matches!(
            app.preview,
            Load::Loaded(ItemDetail { number: 42, .. })
        ));
        server.join().unwrap();
    }

    #[test]
    fn switching_personal_queues_discards_old_results_and_continuations() {
        let mut app = test_app();
        app.screen = Screen::MyPulls;
        app.my_pulls = Load::Loaded(vec![]);
        app.my_pulls_progress.next =
            Some("https://api.github.com/search/issues?page=2".to_string());
        let old = app.my_pulls_progress.request.start().unwrap();
        app.open_pull_queue(true);
        assert!(app.review_mode);
        assert!(app.my_pulls_progress.next.is_none());
        app.on_message(Message::MyPulls {
            request: old,
            result: Ok(Page {
                items: vec![],
                next: None,
                warning: None,
            }),
        });
        assert!(matches!(app.my_pulls, Load::Idle));
        app.open_my_pulls();
        assert!(!app.review_mode);
    }

    #[test]
    fn review_queue_waits_for_identity_and_loads_its_first_page() {
        let (mut app, rx, server, _) = fixture_app(vec![
            r#"{"login":"octocat"}"#,
            r#"{"total_count":1,"incomplete_results":false,"items":[{"number":7,"title":"Review me","state":"open"}]}"#,
        ]);
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        receive_message(&mut app, &rx);
        receive_message(&mut app, &rx);
        assert!(app.review_mode);
        assert!(matches!(&app.my_pulls, Load::Loaded(items) if items.len()==1));
        assert_eq!(app.filtered_my_pulls, vec![0]);
        server.join().unwrap();
    }
}
