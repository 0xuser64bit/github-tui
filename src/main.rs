use color_eyre::Result;
use github_tui::app::App;
use github_tui::auth;
use github_tui::event::Events;
use github_tui::github::Github;

fn main() -> Result<()> {
    color_eyre::install()?;

    // Resolve a token before taking over the terminal, so any auth side effects
    // (e.g. `gh`) print normally. A missing token is not fatal: the app shows an
    // auth-required screen instead.
    let client = auth::resolve_token().map(Github::new);

    let events = Events::new();
    let app = App::new(client, events.sender());

    let terminal = ratatui::init();
    let result = app.run(terminal, events);
    ratatui::restore();
    result
}
