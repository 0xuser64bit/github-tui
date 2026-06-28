//! The event backbone that keeps the UI responsive during network I/O.
//!
//! A single [`mpsc`] channel multiplexes three sources:
//!   * an input thread forwarding terminal events,
//!   * a tick thread driving spinner animation,
//!   * on-demand worker threads posting [`Message`]s with fetch results.
//!
//! The UI thread simply blocks on [`Events::next`] and never performs network
//! work itself, so a slow request can never freeze rendering or input.

use std::sync::mpsc::{self, Receiver, RecvError, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self as terminal, Event as TerminalEvent};

use crate::models::{Issue, PullRequest, Repo, User};

/// How often the spinner advances while something is loading.
const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// A result delivered from a worker thread back to the UI.
///
/// Repo-scoped variants carry the repository `full_name` they were issued for,
/// so the app can discard results the user has already navigated away from.
pub enum Message {
    User(Result<User, String>),
    Repos(Result<Vec<Repo>, String>),
    Issues {
        repo: String,
        result: Result<Vec<Issue>, String>,
    },
    Pulls {
        repo: String,
        result: Result<Vec<PullRequest>, String>,
    },
}

/// Anything the main loop reacts to.
pub enum Event {
    /// A key, resize, mouse, or paste event from the terminal.
    Terminal(TerminalEvent),
    /// A fetch completed on a worker thread.
    Message(Message),
    /// A periodic tick for animation.
    Tick,
}

/// Owns the channel and the background input/tick threads.
pub struct Events {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
}

impl Events {
    /// Create the channel and start the input and tick threads.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        spawn_input_thread(sender.clone());
        spawn_tick_thread(sender.clone());
        Self { sender, receiver }
    }

    /// A sender clone for worker threads to post [`Message`]s back.
    pub fn sender(&self) -> Sender<Event> {
        self.sender.clone()
    }

    /// Block until the next event arrives.
    pub fn next(&self) -> Result<Event, RecvError> {
        self.receiver.recv()
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

/// Forward terminal events until the channel closes (UI gone) or stdin errors.
fn spawn_input_thread(sender: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(event) = terminal::read() {
            if sender.send(Event::Terminal(event)).is_err() {
                break;
            }
        }
    });
}

/// Emit a tick on a fixed interval; stop once the receiver is dropped.
fn spawn_tick_thread(sender: Sender<Event>) {
    thread::spawn(move || {
        loop {
            thread::sleep(TICK_INTERVAL);
            if sender.send(Event::Tick).is_err() {
                break;
            }
        }
    });
}
