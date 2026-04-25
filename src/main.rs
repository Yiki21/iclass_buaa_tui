//! Program entry point for switching between the interactive TUI and the automation CLI.

mod app;
mod bykc;
mod cli;
mod constants;
mod iclass;
mod model;
mod ui;

use std::{env, io, time::Duration};

#[cfg(target_os = "macos")]
use std::{
    ffi::{OsStr, OsString},
    io::IsTerminal,
    path::Path,
    process::Command,
};

use anyhow::Result;
use app::{App, AsyncEvent, spawn_version_check};
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, prelude::CrosstermBackend};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = env::args_os().collect::<Vec<_>>();
    let run_cli = cli::should_run_cli(args.iter().cloned());

    #[cfg(target_os = "macos")]
    if !run_cli && relaunch_bundled_app_in_terminal_if_needed(&args)? {
        return Ok(());
    }

    let run_result = if run_cli {
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

/// macOS app bundles launched from Finder have no attached TTY, so the TUI needs
/// to relaunch itself inside Terminal before entering raw mode.
#[cfg(target_os = "macos")]
fn relaunch_bundled_app_in_terminal_if_needed(args: &[OsString]) -> Result<bool> {
    if args.len() > 1
        || io::stdin().is_terminal()
        || io::stdout().is_terminal()
        || io::stderr().is_terminal()
    {
        return Ok(false);
    }

    let executable = env::current_exe()?;
    if !is_macos_app_bundle_executable(&executable) {
        return Ok(false);
    }

    let command = shell_quote(executable.to_string_lossy().as_ref());
    let status = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"Terminal\" to activate")
        .arg("-e")
        .arg(format!(
            "tell application \"Terminal\" to do script \"{}\"",
            escape_applescript_string(&command)
        ))
        .status()?;

    anyhow::ensure!(
        status.success(),
        "failed to relaunch the app inside Terminal.app"
    );
    Ok(true)
}

#[cfg(target_os = "macos")]
fn is_macos_app_bundle_executable(path: &Path) -> bool {
    let Some(macos_dir) = path.parent() else {
        return false;
    };
    let Some(contents_dir) = macos_dir.parent() else {
        return false;
    };
    let Some(app_dir) = contents_dir.parent() else {
        return false;
    };

    macos_dir.file_name() == Some(OsStr::new("MacOS"))
        && contents_dir.file_name() == Some(OsStr::new("Contents"))
        && app_dir.extension() == Some(OsStr::new("app"))
}

#[cfg(target_os = "macos")]
fn shell_quote(input: &str) -> String {
    let mut quoted = String::from("'");
    for ch in input.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(target_os = "macos")]
fn escape_applescript_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Runs the full interactive terminal lifecycle from raw-mode setup to teardown.
///
/// Why:
/// Terminal state is easy to leave broken on early returns. Keeping setup, loop
/// execution, and teardown in one place makes that lifecycle easier to audit.
async fn run_app() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<AsyncEvent>();
    let mut app = App::default();
    spawn_version_check(tx.clone());

    let loop_result = event_loop(&mut terminal, &mut app, &tx, &mut rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    loop_result
}

/// Drives one TUI frame loop by applying async results, redrawing, and reading input.
///
/// How:
/// Each iteration first drains finished background jobs, then updates timer-based
/// state, renders the latest frame, and only then consumes one key event. That
/// ordering keeps the UI responsive without a second render thread.
fn event_loop(
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

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key, tx);
        }
    }

    Ok(())
}
