//! Parses rustc/cargo diagnostics out of a child's output stream.
//!
//! The browser currently shows nothing when a Rust build fails — you find out by
//! looking at another window. Surfacing the error where you are already looking
//! is the single highest-value thing a dev overlay can do, and it requires
//! turning cargo's line-oriented output back into structured diagnostics.
//!
//! Both of cargo's human formats are handled: the default multi-line form with a
//! code frame, and `--message-format short`, which is one line per diagnostic.
//!
//! So are failing tests (cairn 0074), from the line every Rust test failure
//! prints, under libtest and nextest alike: `thread '<test>' panicked at
//! <file>:<line>:<col>:` and the message after it. A failure becomes an error
//! with code `panic`, so it reaches everywhere a compile error does — the TUI,
//! the overlay's Issues panel with an editor link, and the run summary.
//! Machine formats were considered and not used: libtest's JSON is
//! nightly-only, and nextest's libtest-compatible JSON is still experimental.
//! Parsing is done by hand rather than with a regex crate — the grammar is small,
//! and the dependency would not pay for itself.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub level: Level,
    /// `E0425`, when rustc supplies one.
    pub code: Option<String>,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The code frame, verbatim and ANSI-free, for display under the message.
    pub frame: Vec<String>,
}

impl Diagnostic {
    pub fn location(&self) -> Option<String> {
        let file = self.file.as_ref()?;
        match (self.line, self.column) {
            (Some(l), Some(c)) => Some(format!("{file}:{l}:{c}")),
            (Some(l), None) => Some(format!("{file}:{l}")),
            _ => Some(file.clone()),
        }
    }
}

/// Accumulates output lines and yields the diagnostics found so far.
///
/// Stateful because a diagnostic spans many lines and arrives incrementally —
/// the overlay wants to show the first error before the compiler has finished
/// finding the rest.
#[derive(Debug, Default, Clone)]
pub struct Parser {
    diagnostics: Vec<Diagnostic>,
    /// Index of the diagnostic currently collecting frame lines.
    open: Option<usize>,
    /// Errors seen past the cap, still counted so the tally stays true.
    dropped_errors: usize,
    /// Index of a test failure still collecting its panic message.
    open_test: Option<usize>,
}

/// Diagnostics kept between resets. Only a new build resets, and a service
/// that logs `error: connection refused` every second never builds again — so
/// without a bound this grew for as long as `up` ran. The first ones are kept,
/// as a compiler's first errors are the ones that explain the rest.
const MAX_DIAGNOSTICS: usize = 256;

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discards everything. Called at the start of each build, since a fixed
    /// error must disappear from the overlay rather than linger.
    pub fn reset(&mut self) {
        self.diagnostics.clear();
        self.open = None;
        self.open_test = None;
        self.dropped_errors = 0;
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .count()
            + self.dropped_errors
    }

    /// Keeps `d` if there is room; `false` if it was only counted.
    fn keep(&mut self, d: Diagnostic) -> bool {
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            if d.level == Level::Error {
                self.dropped_errors += 1;
            }
            self.open = None;
            return false;
        }
        self.diagnostics.push(d);
        true
    }

    pub fn push_line(&mut self, raw: &str) {
        self.push_plain(&crate::proc::strip_ansi(raw));
    }

    /// `push_line` for text already stripped of escapes.
    pub fn push_plain(&mut self, line: &str) {
        let line = line.trim_end();

        // A panic message runs until a blank line or the next thing libtest
        // or nextest prints, and may itself look like anything — `error: …`
        // included — so it is collected before any other reading.
        if let Some(idx) = self.open_test {
            if ends_panic_message(line) {
                self.open_test = None;
            } else {
                let d = &mut self.diagnostics[idx];
                if d.message.ends_with("panicked") {
                    d.message = format!("{}: {}", d.message, line.trim());
                } else if d.frame.len() < 40 {
                    d.frame.push(line.to_string());
                }
                return;
            }
        }
        if let Some(d) = parse_panic(line) {
            // nextest can print a failure's output twice: once as it fails and
            // again in the final summary.
            let seen = self.diagnostics.iter().any(|e| {
                e.code == d.code
                    && e.file == d.file
                    && e.line == d.line
                    && e.message.starts_with(&d.message)
            });
            self.open = None;
            if !seen && self.keep(d) {
                self.open_test = Some(self.diagnostics.len() - 1);
            }
            return;
        }

        if let Some(d) = parse_short(line) {
            self.keep(d);
            self.open = None;
            return;
        }

        if let Some((level, code, message)) = parse_header(line) {
            // Summary lines are noise: the real diagnostics precede them.
            if is_summary(&message) {
                self.open = None;
                return;
            }
            let kept = self.keep(Diagnostic {
                level,
                code,
                message,
                file: None,
                line: None,
                column: None,
                frame: Vec::new(),
            });
            self.open = kept.then(|| self.diagnostics.len() - 1);
            return;
        }

        let Some(idx) = self.open else { return };

        if let Some((file, l, c)) = parse_arrow(line) {
            let d = &mut self.diagnostics[idx];
            // Only the first `-->` is the primary span; later ones are notes.
            if d.file.is_none() {
                d.file = Some(file);
                d.line = Some(l);
                d.column = Some(c);
            }
            return;
        }

        // A blank line after the frame has started ends the diagnostic; a blank
        // line before it does not (rustc emits one between header and span).
        if line.trim().is_empty() {
            if !self.diagnostics[idx].frame.is_empty() {
                self.open = None;
            }
            return;
        }
        if self.diagnostics[idx].frame.len() < 40 {
            self.diagnostics[idx].frame.push(line.to_string());
        }
    }
}

/// `error[E0425]: message` / `warning: message`
fn parse_header(line: &str) -> Option<(Level, Option<String>, String)> {
    // Headers start at column zero; indented text is frame content.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (level, rest) = match (line.strip_prefix("error"), line.strip_prefix("warning")) {
        (Some(r), _) => (Level::Error, r),
        (_, Some(r)) => (Level::Warning, r),
        _ => return None,
    };
    let (code, rest) = match rest.strip_prefix('[') {
        Some(r) => {
            let end = r.find(']')?;
            (Some(r[..end].to_string()), &r[end + 1..])
        }
        None => (None, rest),
    };
    let message = rest.strip_prefix(':')?.trim();
    if message.is_empty() {
        return None;
    }
    Some((level, code, message.to_string()))
}

/// `  --> path/to/file.rs:12:9`
fn parse_arrow(line: &str) -> Option<(String, u32, u32)> {
    let rest = line.trim_start().strip_prefix("-->")?.trim();
    split_location(rest)
}

/// `path/to/file.rs:12:9: error[E0425]: message` (short format)
fn parse_short(line: &str) -> Option<Diagnostic> {
    if line.starts_with(char::is_whitespace) || !line.contains(".rs:") {
        return None;
    }
    // Find the `: ` that separates the location from the level.
    let marker = [": error", ": warning"]
        .iter()
        .filter_map(|m| line.find(m).map(|i| (i, *m)))
        .min_by_key(|(i, _)| *i)?;
    let (idx, _) = marker;
    let (loc, rest) = line.split_at(idx);
    let (file, l, c) = split_location(loc)?;
    let (level, code, message) = parse_header(rest.trim_start_matches(": "))?;
    Some(Diagnostic {
        level,
        code,
        message,
        file: Some(file),
        line: Some(l),
        column: Some(c),
        frame: Vec::new(),
    })
}

/// Splits `file:line:col` from the right, so Windows drive letters survive.
fn split_location(s: &str) -> Option<(String, u32, u32)> {
    let (rest, col) = s.rsplit_once(':')?;
    let (file, line) = rest.rsplit_once(':')?;
    Some((file.to_string(), line.parse().ok()?, col.parse().ok()?))
}

/// Cargo's tallies and wrap-ups, which are not actionable diagnostics.
/// `thread '<name>' panicked at <file>:<line>:<col>:` — with the thread's id,
/// `'<name>' (<id>) panicked`, in recent releases — or, before Rust 1.73,
/// `thread '<name>' panicked at '<message>', <file>:<line>:<col>`.
fn parse_panic(line: &str) -> Option<Diagnostic> {
    let rest = line.trim_start().strip_prefix("thread '")?;
    let (name, rest) = rest.split_once('\'')?;
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('(') {
        Some(id) => id.split_once(')')?.1.trim_start(),
        None => rest,
    };
    let rest = rest.strip_prefix("panicked at ")?;
    let (message, location) = match rest.strip_prefix('\'') {
        // The old form carries the message inline, before the location.
        Some(inline) => {
            let (msg, loc) = inline.rsplit_once("', ")?;
            (Some(msg.to_string()), loc)
        }
        None => (None, rest.strip_suffix(':').unwrap_or(rest)),
    };
    let (file, l, c) = split_location(location)?;
    // A test runs on a thread named for it, so this names a failing test; a
    // service's panic, on `main` or a worker, reads as what it is.
    let head = format!("`{name}` panicked");
    Some(Diagnostic {
        level: Level::Error,
        code: Some("panic".into()),
        message: match message {
            Some(m) => format!("{head}: {m}"),
            None => head,
        },
        file: Some(file),
        line: Some(l),
        column: Some(c),
        frame: Vec::new(),
    })
}

/// Where a panic's message ends: a blank line, or what the test harness
/// prints next.
fn ends_panic_message(line: &str) -> bool {
    let t = line.trim_start();
    line.trim().is_empty()
        || t.starts_with("note: run with `RUST_BACKTRACE")
        || t.starts_with("stack backtrace:")
        || t.starts_with("thread '")
        || t.starts_with("---- ")
        || t.starts_with("────")
        || t.starts_with("test ")
        || t.starts_with("failures:")
        || t.starts_with("FAIL ")
        || t.starts_with("PASS ")
        || t.starts_with("error: test failed")
}

fn is_summary(message: &str) -> bool {
    message.starts_with("could not compile")
        // cargo's own line after failing tests; the failures themselves are
        // the diagnostics.
        || message.starts_with("test failed, to rerun pass")
        || message.starts_with("aborting due to")
        || message.starts_with("build failed")
        || message.starts_with("generated ")
        || message.starts_with("failed to")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Vec<Diagnostic> {
        let mut p = Parser::new();
        for line in input.lines() {
            p.push_line(line);
        }
        p.diagnostics().to_vec()
    }

    const REAL_RUSTC_OUTPUT: &str = r#"
   Compiling api v0.1.0 (/w/crates/api)
error[E0425]: cannot find value `greetng` in this scope
  --> crates/api/src/main.rs:18:20
   |
18 |         let body = greetng();
   |                    ^^^^^^^ help: a function with a similar name exists: `greeting`

error: aborting due to 1 previous error

For more information about this error, try `rustc --explain E0425`.
error: could not compile `api` (bin "api") due to 1 previous error
"#;

    #[test]
    fn parses_a_real_rustc_error_with_its_frame() {
        let d = parse(REAL_RUSTC_OUTPUT);
        assert_eq!(
            d.len(),
            1,
            "summary lines must not become diagnostics: {d:#?}"
        );
        assert_eq!(d[0].level, Level::Error);
        assert_eq!(d[0].code.as_deref(), Some("E0425"));
        assert_eq!(d[0].message, "cannot find value `greetng` in this scope");
        assert_eq!(
            d[0].location().as_deref(),
            Some("crates/api/src/main.rs:18:20")
        );
        assert!(
            d[0].frame.iter().any(|l| l.contains("greetng()")),
            "{:#?}",
            d[0].frame
        );
    }

    #[test]
    fn parses_short_format() {
        let d = parse("crates/api/src/main.rs:18:20: error[E0425]: cannot find value `greetng`");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].file.as_deref(), Some("crates/api/src/main.rs"));
        assert_eq!(d[0].line, Some(18));
        assert_eq!(d[0].column, Some(20));
        assert_eq!(d[0].message, "cannot find value `greetng`");
    }

    #[test]
    fn separates_consecutive_diagnostics() {
        let d = parse(
            "error: first problem\n  --> a.rs:1:1\n   |\n1 | bad\n\nwarning: second problem\n  --> b.rs:2:2\n",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].level, Level::Error);
        assert_eq!(d[1].level, Level::Warning);
        assert_eq!(d[1].file.as_deref(), Some("b.rs"));
        assert!(!d[0].frame.iter().any(|l| l.contains("second")));
    }

    /// Real `cargo test` output, captured from a crate with three failing
    /// tests: a custom panic that reads like a compiler error, an `unwrap`, and
    /// an `assert_eq!` with a message.
    #[test]
    fn failing_tests_become_errors_with_their_location() {
        for fixture in [
            include_str!("../tests/fixtures/libtest-failures.txt"),
            include_str!("../tests/fixtures/libtest-failures-backtrace.txt"),
        ] {
            let d = parse(fixture);
            assert_eq!(d.len(), 3, "{d:#?}");
            assert!(
                d.iter()
                    .all(|d| d.level == Level::Error && d.code.as_deref() == Some("panic"))
            );
            let by = |name: &str| d.iter().find(|d| d.message.contains(name)).unwrap();

            let custom = by("tests::says_error");
            assert_eq!(
                custom.message,
                "`tests::says_error` panicked: error: looks like a compiler line"
            );
            assert_eq!(custom.location().as_deref(), Some("src/lib.rs:13:23"));

            let eq = by("tests::wrong_sum");
            assert_eq!(
                eq.message,
                "`tests::wrong_sum` panicked: assertion `left == right` failed: two and two"
            );
            assert_eq!(eq.frame, vec!["  left: 4", " right: 5"]);
            assert_eq!(eq.location().as_deref(), Some("src/lib.rs:9:22"));

            assert!(
                by("tests::unwraps")
                    .message
                    .ends_with("on an `Err` value: \"boom\"")
            );
        }
    }

    #[test]
    fn nextest_failures_are_read_the_same_and_once() {
        // nextest's own lines around the same panic, printed as the test fails
        // and again in its final summary.
        let out = "        FAIL [   0.004s] failing tests::wrong_sum
──── STDOUT:             failing tests::wrong_sum
──── STDERR:             failing tests::wrong_sum
thread 'tests::wrong_sum' (31) panicked at src/lib.rs:9:22:
assertion `left == right` failed
  left: 4
 right: 5
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

     Summary [   0.010s] 2 tests run: 1 passed, 1 failed, 0 skipped
        FAIL [   0.004s] failing tests::wrong_sum
──── STDERR:             failing tests::wrong_sum
thread 'tests::wrong_sum' (31) panicked at src/lib.rs:9:22:
assertion `left == right` failed
  left: 4
 right: 5
";
        let d = parse(out);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].frame, vec!["  left: 4", " right: 5"]);
    }

    #[test]
    fn the_pre_1_73_panic_form_is_read_too() {
        let d = parse(
            "thread 'tests::old' panicked at 'it broke', src/lib.rs:3:5\nnote: run with `RUST_BACKTRACE=1`",
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "`tests::old` panicked: it broke");
        assert_eq!(d[0].location().as_deref(), Some("src/lib.rs:3:5"));
    }

    #[test]
    fn a_service_panic_is_named_for_its_thread_not_as_a_test() {
        let d = parse("thread 'main' (7) panicked at src/main.rs:4:5:\nport 8080 in use");
        assert_eq!(d[0].message, "`main` panicked: port 8080 in use");
    }

    #[test]
    fn cargo_s_test_failed_line_is_not_a_second_error() {
        let d = parse("error: test failed, to rerun pass `--lib`");
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn counts_only_errors() {
        let mut p = Parser::new();
        for l in "error: a\nwarning: b\nerror: c".lines() {
            p.push_line(l);
        }
        assert_eq!(p.errors(), 2);
        assert_eq!(p.diagnostics().len(), 3);
    }

    #[test]
    fn strips_ansi_before_parsing() {
        let d = parse("\u{1b}[1m\u{1b}[31merror[E0308]\u{1b}[0m: mismatched types");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code.as_deref(), Some("E0308"));
        assert_eq!(d[0].message, "mismatched types");
    }

    #[test]
    fn ignores_ordinary_build_chatter() {
        let d = parse(
            "   Compiling api v0.1.0\n    Finished dev profile in 0.21s\n     Running `target/debug/api`",
        );
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn a_service_that_logs_errors_forever_holds_a_bounded_list() {
        let mut p = Parser::new();
        for i in 0..10_000 {
            p.push_line(&format!("error: connection refused ({i})"));
            p.push_line("  --> src/db.rs:1:1");
            p.push_line("warning: retrying");
        }
        assert_eq!(p.diagnostics().len(), MAX_DIAGNOSTICS);
        assert_eq!(p.errors(), 10_000, "every error still counted");
        assert_eq!(
            p.diagnostics()[0].message,
            "connection refused (0)",
            "the first kept"
        );
        p.reset();
        assert_eq!(p.errors(), 0);
    }

    #[test]
    fn reset_clears_fixed_errors() {
        let mut p = Parser::new();
        p.push_line("error: broken");
        assert_eq!(p.errors(), 1);
        p.reset();
        assert_eq!(p.errors(), 0);
    }

    #[test]
    fn only_the_first_span_becomes_the_location() {
        let d = parse("error: bad\n  --> a.rs:1:1\n   |\n   = note: see\n  --> b.rs:9:9\n");
        assert_eq!(d[0].file.as_deref(), Some("a.rs"));
    }
}
