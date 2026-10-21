//! The event backbone that keeps the UI responsive during network I/O.
//!
//! A single [`mpsc`] channel multiplexes three sources:
//!   * an input thread forwarding terminal events,
//!   * a tick thread driving spinner animation,
//!   * on-demand worker threads posting [`Message`]s with fetch results.
//!
//! The UI thread simply blocks on [`Events::next`] and never performs network
//! work itself, so a slow request can never freeze rendering or input.

use std::io;
use std::sync::mpsc::{self, Receiver, RecvError, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self as terminal, Event as TerminalEvent};

use crate::github::Page;
use crate::models::{Issue, PullRequest, Repo, SearchedPullRequest, User};

/// How often the spinner advances while something is loading.
const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// A result delivered from a worker thread back to the UI.
///
/// Repo-scoped variants carry the repository `full_name` they were issued for,
/// so the app can discard results the user has already navigated away from.
pub enum Message {
    User {
        request: u64,
        result: Result<User, String>,
    },
    Repos {
        request: u64,
        result: Result<Page<Repo>, String>,
    },
    Issues {
        request: u64,
        repo: String,
        result: Result<Page<Issue>, String>,
    },
    Pulls {
        request: u64,
        repo: String,
        result: Result<Page<PullRequest>, String>,
    },
    MyPulls {
        request: u64,
        result: Result<Page<SearchedPullRequest>, String>,
    },
}

/// Anything the main loop reacts to.
pub enum Event {
    /// A key, resize, mouse, or paste event from the terminal.
    Terminal(TerminalEvent),
    /// Terminal input failed, so the app must exit and restore the terminal.
    InputFailed(io::Error),
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
    thread::spawn(move || forward_input(sender, terminal::read));
}

fn forward_input(sender: Sender<Event>, mut read: impl FnMut() -> io::Result<TerminalEvent>) {
    loop {
        match read() {
            Ok(event) => {
                if sender.send(Event::Terminal(event)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(Event::InputFailed(error));
                return;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_failure_reaches_the_ui_after_prior_events() {
        let (sender, receiver) = mpsc::channel();
        let mut reads = [
            Ok(TerminalEvent::Resize(80, 24)),
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "input closed")),
        ]
        .into_iter();

        forward_input(sender, || {
            reads.next().expect("reading stops after failure")
        });

        assert!(matches!(
            receiver.recv().unwrap(),
            Event::Terminal(TerminalEvent::Resize(80, 24))
        ));
        let Event::InputFailed(error) = receiver.recv().unwrap() else {
            panic!("terminal error must reach the UI");
        };
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "input closed");
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn input_forwarding_stops_when_the_ui_is_gone() {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        let mut reads = 0;

        forward_input(sender, || {
            reads += 1;
            Ok(TerminalEvent::FocusGained)
        });

        assert_eq!(reads, 1);
    }
}
