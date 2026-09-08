//! How a run reports itself.
//!
//! The same tool has to serve a person watching one screen and a build server
//! writing to a file nobody reads until something breaks. Streaming interleaved
//! output is right for the first and close to useless for the second.

use crate::proc::LogLine;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Everything, as it arrives.
    Full,
    /// Only nodes that failed.
    ErrorsOnly,
    /// Only nodes that actually ran — pairs with a warm cache to turn a
    /// hundred-line run into three.
    NewOnly,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Print as it arrives.
    Stream,
    /// Buffer each node and print it as one block when it finishes.
    Grouped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Auto,
    Always,
    Never,
}

pub fn parse_verbosity(s: &str) -> Result<Verbosity> {
    Ok(match s {
        "full" => Verbosity::Full,
        "errors-only" => Verbosity::ErrorsOnly,
        "new-only" => Verbosity::NewOnly,
        "none" => Verbosity::None,
        other => bail!("unknown --output-logs `{other}` (full, errors-only, new-only, none)"),
    })
}

pub fn parse_order(s: &str, is_terminal: bool) -> Result<Order> {
    Ok(match s {
        "stream" => Order::Stream,
        "grouped" => Order::Grouped,
        // Interleaving is noise on a build server and useful at a desk, so the
        // default follows where the output is going.
        "auto" => {
            if is_terminal {
                Order::Stream
            } else {
                Order::Grouped
            }
        }
        other => bail!("unknown --log-order `{other}` (stream, grouped, auto)"),
    })
}

/// Whether to emit colour.
///
/// Children are spawned on a pty and told `FORCE_COLOR`, so they will emit
/// escapes regardless of this setting. `Never` therefore has to strip on the way
/// out rather than ask them nicely.
pub fn use_colour(mode: Colour, is_terminal: bool) -> bool {
    match mode {
        Colour::Always => true,
        Colour::Never => false,
        Colour::Auto => is_terminal && std::env::var_os("NO_COLOR").is_none(),
    }
}

pub fn parse_colour(s: &str) -> Result<Colour> {
    Ok(match s {
        "auto" => Colour::Auto,
        "always" => Colour::Always,
        "never" => Colour::Never,
        other => bail!("unknown --color `{other}` (auto, always, never)"),
    })
}

/// Decides what reaches the terminal, and in what order.
pub struct Reporter {
    verbosity: Verbosity,
    order: Order,
    colour: bool,
    log_file: Option<std::fs::File>,
    /// Buffered lines per node, for `Grouped`.
    pending: BTreeMap<String, Vec<String>>,
    /// Nodes that actually executed rather than being served from cache.
    ran: std::collections::BTreeSet<String>,
}

impl Reporter {
    pub fn new(
        verbosity: Verbosity,
        order: Order,
        colour: bool,
        log_file: Option<PathBuf>,
    ) -> Result<Self> {
        let file =
            match log_file {
                Some(path) => Some(std::fs::File::create(&path).map_err(|e| {
                    anyhow::anyhow!("cannot write log file {}: {e}", path.display())
                })?),
                None => None,
            };
        Ok(Reporter {
            verbosity,
            order,
            colour,
            log_file: file,
            pending: BTreeMap::new(),
            ran: std::collections::BTreeSet::new(),
        })
    }

    /// Records that a node executed, which `new-only` needs to know.
    pub fn mark_ran(&mut self, node: &str) {
        self.ran.insert(node.to_string());
    }

    pub fn line(&mut self, line: &LogLine) {
        // The log file always gets everything, ANSI-stripped: it exists to be
        // read after the fact, when filtering would only have lost evidence.
        if let Some(f) = self.log_file.as_mut() {
            let _ = writeln!(
                f,
                "{:>10} | {}",
                line.proc,
                crate::proc::strip_ansi(&line.text)
            );
        }
        if self.verbosity == Verbosity::None {
            return;
        }
        let rendered = self.render(line);
        match self.order {
            Order::Stream => {
                if self.verbosity == Verbosity::Full {
                    println!("{rendered}");
                }
                // errors-only and new-only cannot be decided until the node
                // finishes, so they buffer even when streaming.
                else {
                    self.pending
                        .entry(line.proc.clone())
                        .or_default()
                        .push(rendered);
                }
            }
            Order::Grouped => {
                self.pending
                    .entry(line.proc.clone())
                    .or_default()
                    .push(rendered);
            }
        }
    }

    fn render(&self, line: &LogLine) -> String {
        let text = if self.colour {
            line.text.clone()
        } else {
            crate::proc::strip_ansi(&line.text)
        };
        if self.colour {
            format!("\u{1b}[1m{:>10}\u{1b}[0m | {text}", line.proc)
        } else {
            format!("{:>10} | {text}", line.proc)
        }
    }

    /// Flushes a node's buffered output now that its outcome is known.
    pub fn finish(&mut self, node: &str, failed: bool) {
        let Some(lines) = self.pending.remove(node) else {
            return;
        };
        let show = match self.verbosity {
            Verbosity::None => false,
            Verbosity::Full => true,
            Verbosity::ErrorsOnly => failed,
            Verbosity::NewOnly => failed || self.ran.contains(node),
        };
        if show {
            for line in lines {
                println!("{line}");
            }
        }
    }

    /// Flushes anything still buffered, at the end of a run.
    pub fn flush(&mut self) {
        let nodes: Vec<String> = self.pending.keys().cloned().collect();
        for node in nodes {
            // Unknown outcome at this point, so treat it as worth showing rather
            // than swallowing it.
            self.finish(&node, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_order_auto_follows_the_destination() {
        assert_eq!(parse_order("auto", true).unwrap(), Order::Stream);
        // Interleaved parallel output in a CI log is close to unreadable.
        assert_eq!(parse_order("auto", false).unwrap(), Order::Grouped);
    }

    #[test]
    fn unknown_modes_list_the_real_ones() {
        let err = parse_verbosity("quiet").unwrap_err().to_string();
        assert!(err.contains("errors-only"), "{err}");
        assert!(parse_order("weird", true).is_err());
        assert!(parse_colour("maybe").is_err());
    }

    #[test]
    fn no_color_is_honoured_in_auto() {
        unsafe { std::env::set_var("NO_COLOR", "1") };
        assert!(!use_colour(Colour::Auto, true));
        // An explicit --color always still wins; the user asked directly.
        assert!(use_colour(Colour::Always, true));
        unsafe { std::env::remove_var("NO_COLOR") };
        assert!(use_colour(Colour::Auto, true));
    }

    #[test]
    fn colour_never_strips_rather_than_trusting_children() {
        // Children run on a pty with FORCE_COLOR and will emit escapes anyway.
        let r = Reporter::new(Verbosity::Full, Order::Stream, false, None).unwrap();
        let line = LogLine {
            seq: 1,
            proc: "api".into(),
            text: "\u{1b}[32mok\u{1b}[0m".into(),
            transient: false,
        };
        let out = r.render(&line);
        assert!(!out.contains('\u{1b}'), "escapes survived: {out:?}");
        assert!(out.contains("ok"));
    }

    #[test]
    fn errors_only_keeps_a_failing_node_and_drops_a_passing_one() {
        let mut r = Reporter::new(Verbosity::ErrorsOnly, Order::Grouped, false, None).unwrap();
        for proc in ["good", "bad"] {
            r.line(&LogLine {
                seq: 1,
                proc: proc.into(),
                text: "x".into(),
                transient: false,
            });
        }
        r.finish("good", false);
        assert!(
            !r.pending.contains_key("good"),
            "a passing node should be consumed"
        );
        r.finish("bad", true);
        assert!(r.pending.is_empty());
    }

    #[test]
    fn the_log_file_gets_everything_even_when_the_terminal_does_not() {
        let path = std::env::temp_dir().join(format!("tr-log-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let mut r =
                Reporter::new(Verbosity::None, Order::Stream, false, Some(path.clone())).unwrap();
            r.line(&LogLine {
                seq: 1,
                proc: "api".into(),
                text: "\u{1b}[31mboom\u{1b}[0m".into(),
                transient: false,
            });
        }
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("boom"),
            "the file is for reading later: {written:?}"
        );
        assert!(!written.contains('\u{1b}'), "the file should be ANSI-free");
        let _ = std::fs::remove_file(&path);
    }
}
