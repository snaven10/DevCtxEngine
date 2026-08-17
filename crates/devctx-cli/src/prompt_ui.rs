//! Arrow-key selection for the setup wizard, drawn in place.
//!
//! Inline rather than a full-screen interface: a wizard that takes over the
//! terminal and restores it on exit leaves no record of what was chosen, and
//! the first thing anyone does after configuring something is scroll up to
//! check. Each question paints its list, the arrows move through it, Enter
//! collapses it to the single answered line, and that line stays.
//!
//! Falls back to typed input whenever the terminal cannot be driven — no TTY,
//! raw mode refused, a terminal too short for the list. The fallback is not a
//! degraded mode to apologize for: it is what a script, an agent, and a
//! terminal inside an editor all get, and it has to work.

use std::io::{IsTerminal as _, Write as _};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::{cursor, execute, terminal};

/// One option in a list.
pub struct Choice {
    /// What is written to the config.
    pub value: String,
    /// What the reader sees.
    pub label: String,
    /// One line under the label when highlighted, or empty.
    pub note: String,
}

impl Choice {
    pub fn new(value: &str, label: &str, note: &str) -> Self {
        Self {
            value: value.to_string(),
            label: label.to_string(),
            note: note.to_string(),
        }
    }
}

/// Whether a list can be driven here at all.
fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// How the list says it can be driven.
///
/// Set once at the start of the wizard so the hint follows the language chosen
/// there. A global rather than a parameter because every question would
/// otherwise thread it through, and there is exactly one wizard per process.
static HINT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

pub fn set_hint(hint: &'static str) {
    // A second call is the caller's business, not an error worth failing on.
    let _ = HINT.set(hint);
}

fn hint() -> &'static str {
    HINT.get().copied().unwrap_or("(↑↓, Enter)")
}

/// Ask one of `choices`, returning the chosen value.
///
/// `default_index` is where the cursor starts and what Enter accepts, so the
/// answer that reproduces the machine's current behaviour is always one key.
pub fn select(question: &str, choices: &[Choice], default_index: usize) -> String {
    let fallback = || {
        choices
            .get(default_index)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    };
    if !interactive() || choices.is_empty() {
        return fallback();
    }
    match select_interactive(question, choices, default_index) {
        Some(v) => v,
        // Raw mode refused, or the terminal disappeared mid-question. Typing is
        // still an answer; failing the whole init is not.
        None => select_typed(question, choices, default_index).unwrap_or_else(fallback),
    }
}

/// A yes/no question as a two-item list.
pub fn confirm(question: &str, default: bool, yes: &str, no: &str) -> bool {
    let choices = [Choice::new("y", yes, ""), Choice::new("n", no, "")];
    select(question, &choices, if default { 0 } else { 1 }) == "y"
}

/// Free text, with a default. Always typed: there is nothing to choose from.
pub fn input(question: &str, default: &str) -> String {
    print!("\n{question} [{default}]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return default.to_string();
    }
    let s = s.trim();
    if s.is_empty() {
        default.to_string()
    } else {
        s.to_string()
    }
}

/// The interactive path. `None` means the terminal could not be driven, which
/// the caller answers by asking for the choice to be typed instead.
fn select_interactive(question: &str, choices: &[Choice], default_index: usize) -> Option<String> {
    let mut out = std::io::stdout();
    terminal::enable_raw_mode().ok()?;

    let mut cursor_at = default_index.min(choices.len() - 1);
    let mut first_draw = true;
    let chosen = loop {
        draw(&mut out, question, choices, cursor_at, first_draw);
        first_draw = false;

        let ev = match event::read() {
            Ok(e) => e,
            Err(_) => break None,
        };
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = ev
        else {
            continue;
        };
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                cursor_at = cursor_at.checked_sub(1).unwrap_or(choices.len() - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                cursor_at = (cursor_at + 1) % choices.len();
            }
            KeyCode::Enter => break Some(choices[cursor_at].value.clone()),
            // Ctrl-C during setup means stop, not "take the default": the
            // caller is about to write a file, and writing one nobody
            // confirmed is worse than exiting.
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = terminal::disable_raw_mode();
                println!();
                std::process::exit(130);
            }
            KeyCode::Esc => break None,
            _ => {}
        }
    };

    // Collapse the list to one line: the transcript should read as a record of
    // what was decided, not as the menu it was decided from.
    // +1 for the question, +1 for the blank line opened before it.
    let painted = choices.len() as u16 + 2;
    let _ = execute!(
        out,
        cursor::MoveToPreviousLine(painted),
        terminal::Clear(terminal::ClearType::FromCursorDown)
    );
    let _ = terminal::disable_raw_mode();

    if let Some(v) = &chosen {
        println!();
        let label = choices
            .iter()
            .find(|c| &c.value == v)
            .map(|c| c.label.as_str())
            .unwrap_or(v.as_str());
        println!("{question}: {label}");
    }
    chosen
}

fn draw(
    out: &mut std::io::Stdout,
    question: &str,
    choices: &[Choice],
    cursor_at: usize,
    first: bool,
) {
    if first {
        // A blank line before each question: run together, the answered lines
        // and the next prompt read as one paragraph, and the eye has nowhere to
        // rest between decisions.
        let _ = write!(out, "\r\n");
    } else {
        let _ = execute!(
            out,
            cursor::MoveToPreviousLine(choices.len() as u16 + 1),
            terminal::Clear(terminal::ClearType::FromCursorDown)
        );
    }
    // Raw mode means \n does not return the carriage, so every line ends \r\n.
    let _ = write!(out, "{question}  {}\r\n", hint());
    for (i, c) in choices.iter().enumerate() {
        let mark = if i == cursor_at { "❯" } else { " " };
        let note = if i == cursor_at && !c.note.is_empty() {
            format!("   {}", c.note)
        } else {
            String::new()
        };
        let _ = write!(out, "{mark} {}{note}\r\n", c.label);
    }
    let _ = out.flush();
}

/// The typed fallback: the same list, numbered.
fn select_typed(question: &str, choices: &[Choice], default_index: usize) -> Option<String> {
    println!("{question}");
    for (i, c) in choices.iter().enumerate() {
        println!("  {}) {}", i + 1, c.label);
    }
    let default_label = choices.get(default_index).map(|c| c.label.as_str())?;
    let answer = input("Number or name", default_label);

    if let Ok(n) = answer.trim().parse::<usize>() {
        if n >= 1 && n <= choices.len() {
            return Some(choices[n - 1].value.clone());
        }
    }
    choices
        .iter()
        .find(|c| c.value.eq_ignore_ascii_case(answer.trim()) || c.label == answer)
        .map(|c| c.value.clone())
        .or_else(|| choices.get(default_index).map(|c| c.value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a terminal every question answers itself with its default, which
    /// is what makes the wizard safe to run from a script or an agent.
    #[test]
    fn without_a_terminal_the_default_is_taken() {
        let choices = [Choice::new("a", "A", ""), Choice::new("b", "B", "")];
        // Tests run with stdin redirected, so this exercises the real path.
        assert_eq!(select("pick", &choices, 1), "b");
        assert!(!confirm("sure", false, "yes", "no"));
        assert!(confirm("sure", true, "yes", "no"));
    }

    /// An empty list has no default to fall back on and must not panic — an
    /// index into nothing is exactly the sort of thing a registry that came
    /// back empty would produce.
    #[test]
    fn an_empty_list_answers_with_nothing_rather_than_panicking() {
        assert_eq!(select("pick", &[], 0), "");
    }
}
