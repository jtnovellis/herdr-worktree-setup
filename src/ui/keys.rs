//! Key and mouse handling.

use super::state::App;
use crate::pipeline::Worker;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

pub fn handle(app: &mut App, key: KeyEvent, worker: &Worker) {
    app.cancel_countdown();
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => quit(app, worker),
        KeyCode::Char('q') | KeyCode::Esc => quit(app, worker),
        KeyCode::Char('j') | KeyCode::Down => app.select_delta(1),
        KeyCode::Char('k') | KeyCode::Up => app.select_delta(-1),
        KeyCode::Char('J') => app.scroll_by(1),
        KeyCode::Char('K') => app.scroll_by(-1),
        KeyCode::PageDown | KeyCode::Char('f') if !ctrl => app.scroll_by(app.log_viewport as isize),
        KeyCode::PageDown | KeyCode::Char('f') => app.scroll_by(app.log_viewport as isize),
        KeyCode::PageUp | KeyCode::Char('b') => app.scroll_by(-(app.log_viewport as isize)),
        KeyCode::Char('d') if ctrl => app.scroll_by((app.log_viewport / 2) as isize),
        KeyCode::Char('u') if ctrl => app.scroll_by(-((app.log_viewport / 2) as isize)),
        KeyCode::Char('g') | KeyCode::Home => app.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End => app.scroll_to_bottom(),
        KeyCode::Char('r') => {
            if app.has_failures() && !app.is_running() && app.fatal.is_none() {
                app.retrying = true;
                app.done = None;
                worker.retry_failed();
            }
        }
        KeyCode::Enter | KeyCode::Char('o') => app.scroll_to_bottom(),
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            app.select((c as u8 - b'1') as usize);
        }
        _ => {}
    }
}

pub fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollDown => app.scroll_by(3),
        MouseEventKind::ScrollUp => app.scroll_by(-3),
        _ => {}
    }
}

fn quit(app: &mut App, worker: &Worker) {
    if app.is_running() {
        worker.abort();
    }
    app.quit = true;
}
