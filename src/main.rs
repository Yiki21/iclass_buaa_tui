mod api;
mod app;
mod bykc;
mod cli;
mod constants;
mod model;
mod ui;

use std::{env, io, time::Duration};

use anyhow::Result;
use app::{App, AsyncEvent};
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, prelude::CrosstermBackend};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let run_result = if cli::should_run_cli(env::args_os()) {
        cli::run_cli().await
    } else {
        run_app().await
    };
    if let Err(error) = run_result {
        eprintln!("{error:?}");
        return Err(error);
    }
    Ok(())
}

async fn run_app() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<AsyncEvent>();
    let mut app = App::default();

    let loop_result = event_loop(&mut terminal, &mut app, &tx, &mut rx).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    loop_result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    tx: &mpsc::UnboundedSender<AsyncEvent>,
    rx: &mut mpsc::UnboundedReceiver<AsyncEvent>,
) -> Result<()> {
    loop {
        while let Ok(message) = rx.try_recv() {
            app.handle_async(message);
        }

        app.handle_tick();

        terminal.draw(|frame| ui::render(frame, app))?;

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.handle_key(key, tx);
            }
        }
    }

    Ok(())
}
