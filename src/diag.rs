//! Parses rustc/cargo diagnostics out of a child's output stream.
//!
//! The browser currently shows nothing when a Rust build fails — you find out by
//! looking at another window. Surfacing the error where you are already looking
//! is the single highest-value thing a dev overlay can do, and it requires
//! turning cargo's line-oriented output back into structured diagnostics.
//!
//! Both of cargo's human formats are handled: the default multi-line form with a
//! code frame, and `--message-format short`, which is one line per diagnostic.
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
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discards everything. Called at the start of each build, since a fixed
    /// error must disappear from the overlay rather than linger.
    pub fn reset(&mut self) {
        self.diagnostics.clear();
        self.open = None;
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .count()
    }

    pub fn push_line(&mut self, raw: &str) {
        let line = crate::proc::strip_ansi(raw);
        let line = line.trim_end();

        if let Some(d) = parse_short(line) {
            self.diagnostics.push(d);
            self.open = None;
            return;
        }

        if let Some((level, code, message)) = parse_header(line) {
            // Summary lines are noise: the real diagnostics precede them.
            if is_summary(&message) {
                self.open = None;
                return;
            }
            self.diagnostics.push(Diagnostic {
                level,
                code,
                message,
                file: None,
                line: None,
                column: None,
                frame: Vec::new(),
            });
            self.open = Some(self.diagnostics.len() - 1);
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
fn is_summary(message: &str) -> bool {
    message.starts_with("could not compile")
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
