//! Aster's terminal console: chat and train live, answer Aster's questions,
//! decide its requests, watch levels and decision traces, and change config.
//! A pure client of `aster serve`; Aster keeps running when it closes.

mod app;
mod client;
mod ui;

use std::time::Duration;

use tokio::sync::mpsc;

use crate::{
    app::{App, Msg},
    client::Client,
};

pub struct Options {
    pub server_url: String,
    pub token: String,
    pub show_trace: bool,
    pub training_prompt_after_reply: bool,
    pub history_turns: u32,
}

pub async fn run(options: Options) -> anyhow::Result<()> {
    let client = Client::new(&options.server_url, options.token.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Terminal input on a blocking thread; exits when the app drops `rx`.
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if input_tx.send(Msg::Input(event)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {
                    if input_tx.is_closed() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    let mut app = App::new(client.clone(), options, tx.clone());
    app.resync();
    client.spawn_events(tx);

    let mut terminal = ratatui::init();
    // Mouse wheel scrolls panes. Undo it on panic too, or the shell is left
    // receiving mouse escape codes.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        previous_hook(info);
    }));
    let result = loop {
        if let Err(error) = terminal.draw(|frame| ui::draw(frame, &app)) {
            break Err(error.into());
        }
        let Some(msg) = rx.recv().await else {
            break Ok(());
        };
        app.handle(msg);
        while let Ok(msg) = rx.try_recv() {
            app.handle(msg);
        }
        if app.quit {
            break Ok(());
        }
    };
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}
