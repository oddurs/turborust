//! Reading a `Procfile`, so `turborust init` can start from what foreman,
//! overmind, hivemind and honcho already run.
//!
//! The format is one `name: command` per line. Everything else (comments,
//! blank lines) is ignored by those tools too; a line that is neither is
//! reported rather than dropped, since a skipped process is one the user would
//! otherwise find missing only once `up` is running.

/// What came out of a Procfile.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Procfile {
    pub processes: Vec<Process>,
    /// Lines not turned into a service, each with the reason.
    pub skipped: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Process {
    pub name: String,
    pub command: String,
    /// The `PORT` foreman would have given it, when the command reads `$PORT`:
    /// 5000 for the first process, then 5100, 5200, …
    pub port: Option<u16>,
}

pub fn parse(text: &str) -> Procfile {
    let mut out = Procfile::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let n = i + 1;
        let Some((name, command)) = line.split_once(':') else {
            out.skipped.push(format!("line {n}: not `name: command`"));
            continue;
        };
        let (name, command) = (name.trim(), command.trim());
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            out.skipped
                .push(format!("line {n}: `{name}` is not a process name"));
            continue;
        }
        if command.is_empty() {
            out.skipped
                .push(format!("line {n}: `{name}` has no command"));
            continue;
        }
        // Heroku runs `release` once per deploy, not as a process to keep up.
        if name == "release" {
            out.skipped.push(format!(
                "line {n}: `release` runs once per deploy; make it a task if you need it"
            ));
            continue;
        }
        if out.processes.iter().any(|p| p.name == name) {
            out.skipped
                .push(format!("line {n}: `{name}` is declared twice"));
            continue;
        }
        // foreman numbers by position in the file, counting every process.
        let index = out.processes.len() as u16;
        let reads_port = command.contains("$PORT") || command.contains("${PORT}");
        out.processes.push(Process {
            name: name.to_string(),
            command: command.to_string(),
            port: reads_port.then(|| 5000 + 100 * index),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_processes_and_says_what_it_skipped() {
        let p = parse(
            "# the stack\n\
             web: bundle exec puma -p $PORT\n\
             worker:   bundle exec sidekiq   \n\
             \n\
             release: rails db:migrate\n\
             assets: bin/vite dev --port ${PORT}\n\
             what is this\n\
             web: again\n\
             bad name: x\n\
             empty:\n",
        );
        let got: Vec<(&str, &str, Option<u16>)> = p
            .processes
            .iter()
            .map(|p| (p.name.as_str(), p.command.as_str(), p.port))
            .collect();
        assert_eq!(
            got,
            [
                ("web", "bundle exec puma -p $PORT", Some(5000)),
                ("worker", "bundle exec sidekiq", None),
                ("assets", "bin/vite dev --port ${PORT}", Some(5200)),
            ]
        );
        assert_eq!(p.skipped.len(), 5, "{:?}", p.skipped);
        for (want, line) in [
            ("release", 5),
            ("not `name: command`", 7),
            ("declared twice", 8),
            ("not a process name", 9),
            ("no command", 10),
        ] {
            assert!(
                p.skipped
                    .iter()
                    .any(|s| s.starts_with(&format!("line {line}:")) && s.contains(want)),
                "{want} at line {line}: {:?}",
                p.skipped
            );
        }
    }

    #[test]
    fn a_colon_in_the_command_is_part_of_the_command() {
        let p = parse("web: python -m http.server --bind 127.0.0.1:8000\n");
        assert_eq!(
            p.processes[0].command,
            "python -m http.server --bind 127.0.0.1:8000"
        );
    }
}
