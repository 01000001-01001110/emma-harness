//! Line-oriented terminal output. No TUI framework, on purpose: the moment
//! output owns the screen it also owns scrollback, resize, and every program
//! the agent shells out to.
//!
//! Two things here are not cosmetic.
//!
//! **Text streams as it arrives.** Forty seconds of blank terminal is
//! indistinguishable from a hang, and a user who cannot tell those apart kills
//! the process partway through a write.
//!
//! **Tool output is summarised, not dumped.** The model gets `content`; the
//! human gets `display` when the tool offered one and a few lines otherwise.
//! A screen of JSON is a screen nobody reads, and the reason to show anything
//! at all is so the user can tell that the agent is doing what they meant.

use std::io::{IsTerminal, Write};

use tokio::sync::mpsc;

// region: The terminal
// ---------------------------------------------------------------------------
// The terminal
//
// Every write to the screen goes through one of these methods. The split that
// matters is `delta`/`text` on stdout against `side` on stderr under `-p`, so
// a script can pipe the answer without filtering the commentary out of it.
// ---------------------------------------------------------------------------

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// How many lines of a tool result reach the screen.
const RESULT_LINES: usize = 8;

pub struct Term {
    color: bool,
    /// `-p`: assistant prose still goes to stdout, but the running commentary
    /// goes to stderr so a script can pipe the answer without filtering it.
    quiet: bool,
    enabled: bool,
}

impl Term {
    pub fn interactive() -> Self {
        Self {
            color: std::io::stdout().is_terminal(),
            quiet: false,
            enabled: true,
        }
    }

    pub fn printing() -> Self {
        Self {
            color: std::io::stderr().is_terminal(),
            quiet: true,
            enabled: true,
        }
    }

    /// Writes nothing. For tests, which assert on the log and the transcript
    /// rather than on the screen.
    pub fn silent() -> Self {
        Self {
            color: false,
            quiet: true,
            enabled: false,
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// The side channel: tool lines, notes, warnings. stderr in `-p`.
    fn side(&self, line: &str) {
        if !self.enabled {
            return;
        }
        if self.quiet {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    /// Assistant text, as it arrives. No newline, flushed every time — a
    /// buffered stream is a blank terminal with the words already in it.
    pub fn delta(&self, text: &str) {
        if !self.enabled {
            return;
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    pub fn end_of_text(&self) {
        if self.enabled {
            println!();
        }
    }

    /// Whole assistant text at once, for `-p` where nothing streamed.
    pub fn text(&self, text: &str) {
        if self.enabled && !text.trim().is_empty() {
            println!("{}", text.trim_end());
        }
    }

    pub fn note(&self, text: &str) {
        self.side(&self.paint(DIM, &format!("  {text}")));
    }

    pub fn warn(&self, text: &str) {
        self.side(&self.paint(YELLOW, &format!("! {text}")));
    }

    pub fn banner(&self, text: &str) {
        self.side(&self.paint(RED, &format!("!! {text}")));
    }

    /// A tool is about to run. One line, the interesting argument inline.
    pub fn tool_started(&self, name: &str, args: &serde_json::Value) {
        let head = summarise_args(name, args);
        self.side(&format!("{} {head}", self.paint(BOLD, &format!("● {name}"))));
    }

    pub fn tool_result(&self, display: Option<&str>, content: &str, error: bool) {
        let body = display.unwrap_or(content);
        let mut lines = body.lines();
        let shown: Vec<&str> = lines.by_ref().take(RESULT_LINES).collect();
        let rest = lines.count();
        for line in shown {
            let text = format!("  {line}");
            self.side(&self.paint(if error { RED } else { DIM }, &text));
        }
        if rest > 0 {
            self.side(&self.paint(DIM, &format!("  … {rest} more lines")));
        }
    }

    pub fn goal_started(&self, goal: &str) {
        self.side(&self.paint(BOLD, &format!("▸ {}", goal.trim())));
    }

    pub fn prompt_header(&self, tool: &str, preview: &str) {
        self.side("");
        self.side(&self.paint(BOLD, &format!("{tool} wants to run:")));
        for line in preview.lines() {
            self.side(&format!("  {line}"));
        }
    }

    pub fn prompt_question(&self, tool: &str) {
        if !self.enabled {
            return;
        }
        let q = format!("  allow? [y]es  [n]o  [a]lways {tool} this session: ");
        // Whichever stream the commentary is on, so the question is never
        // separated from the thing it is asking about.
        if self.quiet {
            eprint!("{q}");
            let _ = std::io::stderr().flush();
        } else {
            print!("{q}");
            let _ = std::io::stdout().flush();
        }
    }

    pub fn goal_prompt(&self) {
        if !self.enabled {
            return;
        }
        print!("{} ", self.paint(BOLD, ">"));
        let _ = std::io::stdout().flush();
    }
}

// endregion: The terminal

// region: The one place stdin is read
// ---------------------------------------------------------------------------
// The one place stdin is read
//
// A single reader behind a channel. The goal prompt and the approval prompt
// both want lines, and two readers on one stdin race for them.
// ---------------------------------------------------------------------------

/// The one place stdin is read.
///
/// A single reader, fed by a blocking thread, because the goal prompt and the
/// approval prompt both need lines and two independent readers race for them:
/// the loser buffers the answer to a question the winner asked. One queue
/// means a line is consumed exactly once by whoever asked most recently.
pub struct LineSource {
    rx: mpsc::Receiver<String>,
}

impl LineSource {
    pub fn stdin() -> Self {
        let (tx, rx) = mpsc::channel(8);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut stdin.lock(), &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.blocking_send(line.trim_end_matches(['\r', '\n']).to_string()).is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        Self { rx }
    }

    pub async fn next(&mut self) -> Option<String> {
        self.rx.recv().await
    }
}

// endregion: The one place stdin is read

// region: The banner argument
// ---------------------------------------------------------------------------
// The banner argument
//
// Which single argument identifies a tool call on one line. Per tool rather
// than by rule, so a `Write` never puts its content on the screen.
// ---------------------------------------------------------------------------

/// The one argument worth putting on the tool's own line.
///
/// Chosen per tool rather than "the first string field": a `Write` whose banner
/// showed its content would push the next twenty tool calls off the screen.
fn summarise_args(name: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let one_line = |t: String| {
        let t = t.replace('\n', " ");
        if t.chars().count() > 88 {
            format!("{}…", t.chars().take(88).collect::<String>())
        } else {
            t
        }
    };
    match name {
        "Bash" => one_line(s("command")),
        "Read" | "Write" | "Edit" => one_line(s("file_path")),
        "Glob" => one_line(s("pattern")),
        "Grep" => one_line(s("pattern")),
        "Skill" => one_line(s("name")),
        _ => one_line(args.to_string()),
    }
}

// endregion: The banner argument

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Only `summarise_args`, because it is the only thing here that decides
// something. The rest writes to a stream, and asserting on that would be
// asserting on `println!`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_banner_argument_is_the_one_a_human_would_look_at() {
        assert_eq!(
            summarise_args("Bash", &json!({ "command": "cargo test", "timeout_ms": 1 })),
            "cargo test"
        );
        // The failure this prevents: a Write banner that prints the file.
        let big = json!({ "file_path": "src/a.rs", "content": "x".repeat(5_000) });
        assert_eq!(summarise_args("Write", &big), "src/a.rs");
    }

    #[test]
    fn a_long_command_is_cut_to_one_line() {
        let out = summarise_args("Bash", &json!({ "command": "a\nb\n".repeat(200) }));
        assert!(!out.contains('\n'));
        assert!(out.chars().count() <= 89, "{}", out.chars().count());
    }
}

// endregion: Tests
