//! `.env` files: the `KEY=value` format foreman, honcho, overmind, Docker
//! Compose and dotenv all read.
//!
//! Read when a node starts rather than when the plan is built, so an edit to
//! `.env` takes effect on the next restart without restarting turborust. The
//! plan still loads each file once up front, so a missing or malformed file is
//! an error before anything runs instead of an empty environment later.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// One file a node's environment is loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFile {
    pub path: PathBuf,
    /// A missing required file is an error; a missing optional one is nothing.
    pub required: bool,
}

/// Loads the files in order, later files overriding earlier ones.
pub fn load(files: &[EnvFile]) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for f in files {
        let text = match std::fs::read_to_string(&f.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !f.required => continue,
            Err(e) => return Err(format!("env_file {}: {e}", f.path.display())),
        };
        for (k, v) in parse(&text).map_err(|e| format!("env_file {}: {e}", f.path.display()))? {
            env.insert(k, v);
        }
    }
    Ok(env)
}

/// Parses dotenv syntax: blank lines and `#` comments, an optional `export `
/// prefix, unquoted values (trimmed, with a ` #` comment cut off), single quotes
/// taken literally, and double quotes with `\n`, `\t`, `\"` and `\\` escapes,
/// which may span lines. No `${VAR}` expansion: a value is what the file says.
pub fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((i, raw)) = lines.next() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let n = i + 1;
        let (key, rest) = line
            .split_once('=')
            .ok_or_else(|| format!("line {n}: expected KEY=value"))?;
        let key = key.trim_end();
        let valid = key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if !valid {
            return Err(format!("line {n}: `{key}` is not a variable name"));
        }
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some('\'') => {
                let body = &rest[1..];
                let end = body
                    .find('\'')
                    .ok_or_else(|| format!("line {n}: unterminated single quote"))?;
                body[..end].to_string()
            }
            Some('"') => {
                let mut value = String::new();
                let mut chunk = rest[1..].to_string();
                loop {
                    let mut chars = chunk.chars();
                    let mut closed = false;
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => {
                                closed = true;
                                break;
                            }
                            '\\' => match chars.next() {
                                Some('n') => value.push('\n'),
                                Some('t') => value.push('\t'),
                                Some('r') => value.push('\r'),
                                Some(c @ ('"' | '\\' | '$')) => value.push(c),
                                Some(c) => {
                                    value.push('\\');
                                    value.push(c);
                                }
                                None => value.push('\\'),
                            },
                            c => value.push(c),
                        }
                    }
                    if closed {
                        break;
                    }
                    // An open quote carries on to the next line, newline kept.
                    let (_, next) = lines
                        .next()
                        .ok_or_else(|| format!("line {n}: unterminated double quote"))?;
                    value.push('\n');
                    chunk = next.to_string();
                }
                value
            }
            _ => {
                let cut = rest.find(" #").map(|i| &rest[..i]).unwrap_or(rest);
                cut.trim_end().to_string()
            }
        };
        out.push((key.to_string(), value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(text: &str) -> BTreeMap<String, String> {
        parse(text).unwrap().into_iter().collect()
    }

    #[test]
    fn reads_what_dotenv_tools_write() {
        let env = map(r#"
# a comment
PLAIN=value
SPACED = padded
export EXPORTED=yes
COMMENTED=kept # dropped
HASH=a#b
SINGLE='literal $HOME \n # kept'
DOUBLE="line\nbreak \"quoted\""
EMPTY=
MULTI="one
two"
"#);
        assert_eq!(env["PLAIN"], "value");
        assert_eq!(env["SPACED"], "padded");
        assert_eq!(env["EXPORTED"], "yes");
        assert_eq!(env["COMMENTED"], "kept");
        assert_eq!(env["HASH"], "a#b");
        assert_eq!(env["SINGLE"], "literal $HOME \\n # kept");
        assert_eq!(env["DOUBLE"], "line\nbreak \"quoted\"");
        assert_eq!(env["EMPTY"], "");
        assert_eq!(env["MULTI"], "one\ntwo");
        assert_eq!(env.len(), 9);
    }

    #[test]
    fn a_line_that_is_not_an_assignment_names_its_line() {
        assert_eq!(
            parse("A=1\nnonsense\n").unwrap_err(),
            "line 2: expected KEY=value"
        );
        assert!(parse("1A=x").unwrap_err().contains("not a variable name"));
        assert!(parse("A=\"open").unwrap_err().contains("unterminated"));
        assert!(parse("A='open").unwrap_err().contains("unterminated"));
    }

    #[test]
    fn later_files_win_and_only_optional_files_may_be_missing() {
        let dir = std::env::temp_dir().join(format!("tr-envfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "X=1\nY=1\n").unwrap();
        std::fs::write(dir.join("b"), "Y=2\n").unwrap();
        let f = |name: &str, required| EnvFile {
            path: dir.join(name),
            required,
        };
        let env = load(&[f("a", true), f("b", true), f("absent", false)]).unwrap();
        assert_eq!((env["X"].as_str(), env["Y"].as_str()), ("1", "2"));
        let err = load(&[f("absent", true)]).unwrap_err();
        assert!(err.contains("absent"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
