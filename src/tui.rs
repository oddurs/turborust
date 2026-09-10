//! Terminal UI.
//!
//! Renders `AppState` and nothing else — it holds no supervisor state of its own,
//! so `--no-tui` and the TUI run byte-identical supervision logic.

use crate::engine::{Ctl, Engine, Wire};
use crate::proc::strip_ansi;
use crate::state::{Status, fmt_dur};
use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub enum UiEvent {
    Key(KeyEvent),
    Tick,
}

/// Runs the UI until the user quits. Returns when shutdown should begin.
/// True when there is a real terminal on both ends.
///
/// Both matter: the TUI draws to stdout and reads keys from stdin, so a pipe on
/// either side makes it unusable.
pub fn available() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

/// Runs the UI until the user quits.
///
/// Fails rather than panics when there is no terminal. `ratatui::init` panics on
/// a raw-mode failure, and the panic names ratatui's internals — from which a
/// reader cannot tell whether turborust is broken, their config is wrong, or
/// their environment simply has no tty.
pub async fn run(engine: Arc<Engine>, wires: Arc<BTreeMap<String, Wire>>) -> Result<()> {
    if !available() {
        anyhow::bail!(
            "`turborust up` needs a terminal for its dashboard.\n\
             \x20 For a script, CI, or an agent: `turborust up --no-tui` streams the \
             same output,\n\
             \x20 and `turborust run <task>` is the non-interactive way to build."
        );
    }

    // `try_init` rather than `init`: on failure this must return an error, and it
    // must not leave a half-entered alt screen behind.
    let mut terminal = ratatui::try_init()
        .context("could not put the terminal into raw mode for the dashboard")?;
    let result = event_loop(&mut terminal, engine, wires).await;
    // Only restore what was actually entered — an unconditional restore writes
    // the leave-alt-screen escape into whatever is capturing stdout.
    let _ = ratatui::try_restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    engine: Arc<Engine>,
    wires: Arc<BTreeMap<String, Wire>>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();

    // crossterm's read() is blocking; it gets its own thread rather than a
    // feature-gated async wrapper.
    let key_tx = tx.clone();
    std::thread::Builder::new()
        .name("tui-input".into())
        .spawn(move || {
            loop {
                match event::poll(Duration::from_millis(200)) {
                    Ok(true) => {
                        if let Ok(Event::Key(k)) = event::read()
                            && key_tx.send(UiEvent::Key(k)).is_err()
                        {
                            return;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        })?;

    let tick_tx = tx;
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_millis(80));
        while tick_tx.send(UiEvent::Tick).is_ok() {
            iv.tick().await;
        }
    });

    let mut follow = true;
    // When true, keystrokes go to the focused node instead of driving the UI.
    let mut input_mode = false;

    loop {
        terminal.draw(|f| draw(f, &engine, follow, input_mode))?;

        let Some(ev) = rx.recv().await else {
            return Ok(());
        };
        let UiEvent::Key(k) = ev else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }

        let names = engine.state.lock().unwrap().names();

        // Input mode forwards almost everything, so it is handled before the
        // navigation keys rather than as a special case inside them.
        if input_mode {
            if k.code == KeyCode::Esc {
                input_mode = false;
                continue;
            }
            let selected = engine.state.lock().unwrap().selected;
            if let Some(bytes) = key_to_bytes(&k)
                && let Some(handle) = names.get(selected).and_then(|n| engine.handle_for(n))
            {
                handle.write_stdin(&bytes);
            }
            continue;
        }

        let mut st = engine.state.lock().unwrap();

        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => return Ok(()),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                if !names.is_empty() {
                    st.selected = (st.selected + 1) % names.len();
                }
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if !names.is_empty() {
                    st.selected = (st.selected + names.len() - 1) % names.len();
                }
            }
            (KeyCode::Enter, _) => {
                let name = names.get(st.selected).cloned();
                st.focus = if st.focus.is_some() { None } else { name };
                st.scroll = 0;
                follow = true;
            }
            (KeyCode::Char('i'), _) => {
                // Only meaningful for a node that is actually running.
                if names
                    .get(st.selected)
                    .and_then(|n| engine.handle_for(n))
                    .is_some()
                {
                    input_mode = true;
                }
            }
            (KeyCode::Char('a'), _) => {
                st.focus = None;
                st.scroll = 0;
                follow = true;
            }
            (KeyCode::Char('r'), _) => {
                if let Some(w) = names.get(st.selected).and_then(|n| wires.get(n)) {
                    w.ctl_tx.send(Ctl::Restart("manual restart".into()));
                }
            }
            (KeyCode::Char('R'), _) => {
                for w in wires.values() {
                    w.ctl_tx.send(Ctl::Restart("manual restart (all)".into()));
                }
            }
            (KeyCode::PageUp, _) => {
                st.scroll = st.scroll.saturating_add(20);
                follow = false;
            }
            (KeyCode::PageDown, _) => {
                st.scroll = st.scroll.saturating_sub(20);
                follow = st.scroll == 0;
            }
            (KeyCode::End, _) => {
                st.scroll = 0;
                follow = true;
            }
            _ => {}
        }
    }
}

fn draw(f: &mut Frame, engine: &Engine, follow: bool, input_mode: bool) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(20)])
        .split(rows[0]);

    let st = engine.state.lock().unwrap();
    draw_sidebar(f, cols[0], &st, engine);
    draw_logs(f, cols[1], &st, follow);
    draw_footer(f, rows[1], &st, input_mode);
}

fn draw_sidebar(f: &mut Frame, area: Rect, st: &crate::state::AppState, engine: &Engine) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, (name, node)) in st.nodes.iter().enumerate() {
        let selected = i == st.selected;
        let (glyph, color) = match &node.status {
            Status::Healthy => ("●", Color::Green),
            Status::Done { .. } => ("✓", Color::Green),
            Status::Running | Status::Starting => ("◐", Color::Yellow),
            Status::Waiting(_) => ("○", Color::DarkGray),
            // Queued is amber, not grey: it is work about to happen, not work
            // stuck behind something else.
            Status::Queued => ("◔", Color::Yellow),
            Status::Backoff(_) => ("↻", Color::Yellow),
            Status::Failed(_) => ("✕", Color::Red),
            Status::Exited => ("·", Color::DarkGray),
            Status::Stopped => ("■", Color::DarkGray),
            Status::Pending => ("·", Color::DarkGray),
        };
        let kind = engine
            .plan
            .get(name)
            .map(|n| n.kind == crate::plan::Kind::Task)
            .unwrap_or(false);

        let mut name_style = Style::default();
        if selected {
            name_style = name_style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        }
        if kind {
            name_style = name_style.fg(Color::Cyan);
        }
        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph} "), Style::default().fg(color)),
            Span::styled(format!("{name:<14}"), name_style),
        ]));

        let mut detail = node.status.label();
        if node.blocked_on_cargo_lock {
            detail = "waiting: cargo lock".into();
        }
        if let (Some(u), Status::Healthy) = (node.uptime(), &node.status) {
            detail = format!("{detail}  up {}", fmt_dur(u));
        }
        if node.restarts > 0 {
            detail = format!("{detail}  ×{}", node.restarts);
        }
        lines.push(Line::from(Span::styled(
            format!("     {detail}"),
            Style::default().fg(if node.status.is_bad() {
                Color::Red
            } else {
                Color::DarkGray
            }),
        )));
    }

    let title = if st.shutting_down {
        " shutting down "
    } else {
        " turborust "
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_logs(f: &mut Frame, area: Rect, st: &crate::state::AppState, follow: bool) {
    let inner_h = area.height.saturating_sub(2) as usize;
    let all = st.logs.view(st.focus.as_deref());
    let total = all.len();
    let end = total.saturating_sub(st.scroll);
    let start = end.saturating_sub(inner_h);
    let visible = &all[start.min(total)..end.min(total)];

    let show_prefix = st.focus.is_none();
    let lines: Vec<Line> = visible
        .iter()
        .map(|l| {
            // Render the child's own ANSI faithfully; fall back to plain text if
            // the escape sequence is malformed (partial writes happen).
            let mut spans: Vec<Span> = Vec::new();
            if show_prefix {
                spans.push(Span::styled(
                    format!("{:>10} │ ", l.proc),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            match l.text.clone().into_text() {
                Ok(t) => {
                    if let Some(first) = t.lines.into_iter().next() {
                        spans.extend(first.spans);
                    }
                }
                Err(_) => spans.push(Span::raw(strip_ansi(&l.text))),
            }
            Line::from(spans)
        })
        .collect();

    let title = match &st.focus {
        Some(n) => format!(" {n} "),
        None => format!(" all ({total} lines) "),
    };
    let title = if follow {
        title
    } else {
        format!("{title}[paused] ")
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, st: &crate::state::AppState, input_mode: bool) {
    // A TUI where typing sometimes restarts your server and sometimes types into
    // it, with no indication which, is worse than having no input at all.
    if input_mode {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " INPUT ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  keys go to the selected process   Esc to stop",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            area,
        );
        return;
    }
    let hint = if st.shutting_down {
        "stopping children…"
    } else {
        "↑↓ select   ↵ focus   i input   a all   r restart   R restart all   q quit"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

/// Translates a key press into the bytes a terminal would have sent.
///
/// Returns `None` for keys with no byte representation, so they are swallowed
/// rather than sent as something surprising.
fn key_to_bytes(k: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl-A..Ctrl-Z map to 1..26; this is what makes Ctrl-C reach the
            // child rather than being eaten by the UI.
            let upper = c.to_ascii_uppercase();
            if upper.is_ascii_uppercase() {
                vec![upper as u8 - b'A' + 1]
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        _ => return None,
    })
}

#[cfg(test)]
mod key_tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn printable_characters_pass_through() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('y'), KeyModifiers::NONE)),
            Some(b"y".to_vec())
        );
    }

    #[test]
    fn enter_is_carriage_return_not_newline() {
        // A pty expects CR; sending LF leaves many prompts waiting.
        assert_eq!(
            key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn ctrl_c_reaches_the_child() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(vec![4])
        );
    }

    #[test]
    fn arrows_send_escape_sequences() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn unmapped_keys_are_swallowed_rather_than_guessed() {
        assert_eq!(key_to_bytes(&key(KeyCode::F(5), KeyModifiers::NONE)), None);
    }

    #[test]
    fn utf8_is_encoded_correctly() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('é'), KeyModifiers::NONE)),
            Some("é".as_bytes().to_vec())
        );
    }
}
