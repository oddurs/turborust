//! Selecting which nodes a command acts on.
//!
//! The `...` idiom is turborepo's, deliberately: it is already learned, and the
//! rule is unambiguous once you know that the dots point at the part of the graph
//! being pulled in. Inventing a different syntax for the same idea would be a
//! worse kind of originality.
//!
//! Works from the config's `depends_on` edges rather than a resolved `Plan`,
//! which keeps it cheap — expanding a filter must not require asking cargo about
//! the workspace first.

use crate::config::Workspace;
use anyhow::{Result, bail};
use globset::Glob;
use std::collections::{BTreeMap, BTreeSet};

/// What a single pattern pulls in, alongside the nodes it names.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Reach {
    dependencies: bool,
    dependents: bool,
}

/// Splits the `...` markers off a pattern.
fn parse(pattern: &str) -> (&str, Reach) {
    let (rest, dependents) = match pattern.strip_prefix("...") {
        Some(r) => (r, true),
        None => (pattern, false),
    };
    let (name, dependencies) = match rest.strip_suffix("...") {
        Some(r) => (r, true),
        None => (rest, false),
    };
    (
        name,
        Reach {
            dependencies,
            dependents,
        },
    )
}

/// Every node name in the workspace, with its direct dependencies.
fn edges(ws: &Workspace) -> BTreeMap<String, Vec<String>> {
    ws.config
        .services
        .keys()
        .chain(ws.config.tasks.keys())
        .map(|n| (n.clone(), ws.deps_of(n).to_vec()))
        .collect()
}

/// Expands filter patterns into the set of nodes they select.
///
/// An empty pattern list selects nothing and says so; the caller decides what
/// "no filter" means, because it differs between `up` (everything) and `run`
/// (an error).
pub fn expand(ws: &Workspace, patterns: &[String]) -> Result<Vec<String>> {
    let forward = edges(ws);
    let mut reverse: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, deps) in &forward {
        for d in deps {
            reverse.entry(d.as_str()).or_default().push(name.as_str());
        }
    }

    let mut selected: BTreeSet<String> = BTreeSet::new();
    for pattern in patterns {
        let (name, reach) = parse(pattern);
        if name.is_empty() {
            bail!("`{pattern}` names nothing; write a node name between the dots");
        }
        let matcher = Glob::new(name)
            .map_err(|e| anyhow::anyhow!("bad filter `{pattern}`: {e}"))?
            .compile_matcher();

        let matched: Vec<&str> = forward
            .keys()
            .map(|k| k.as_str())
            .filter(|k| matcher.is_match(k))
            .collect();
        if matched.is_empty() {
            let known: Vec<&str> = forward.keys().map(|k| k.as_str()).collect();
            bail!(
                "`{pattern}` matched no nodes (available: {})",
                known.join(", ")
            );
        }

        for node in matched {
            selected.insert(node.to_string());
            if reach.dependencies {
                walk(node, &mut selected, |n| {
                    forward.get(n).cloned().unwrap_or_default()
                });
            }
            if reach.dependents {
                walk(node, &mut selected, |n| {
                    reverse
                        .get(n)
                        .map(|v| v.iter().map(|s| s.to_string()).collect())
                        .unwrap_or_default()
                });
            }
        }
    }
    Ok(selected.into_iter().collect())
}

fn walk(from: &str, into: &mut BTreeSet<String>, edges: impl Fn(&str) -> Vec<String>) {
    let mut stack = edges(from);
    while let Some(n) = stack.pop() {
        if into.insert(n.clone()) {
            stack.extend(edges(&n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;

    /// check <- api <- web   (arrows point from dependency to dependent)
    fn ws() -> Workspace {
        let src = r#"
            [tasks.check]
            cmd = "true"
            [services.api]
            cmd = "true"
            depends_on = ["check"]
            [services.web]
            cmd = "true"
            depends_on = ["api"]
            [services.docs]
            cmd = "true"
        "#;
        Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        }
    }

    fn sel(patterns: &[&str]) -> Vec<String> {
        expand(
            &ws(),
            &patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn the_dots_parse_in_all_four_forms() {
        assert_eq!(
            parse("web"),
            (
                "web",
                Reach {
                    dependencies: false,
                    dependents: false
                }
            )
        );
        assert_eq!(
            parse("web..."),
            (
                "web",
                Reach {
                    dependencies: true,
                    dependents: false
                }
            )
        );
        assert_eq!(
            parse("...web"),
            (
                "web",
                Reach {
                    dependencies: false,
                    dependents: true
                }
            )
        );
        assert_eq!(
            parse("...web..."),
            (
                "web",
                Reach {
                    dependencies: true,
                    dependents: true
                }
            )
        );
    }

    #[test]
    fn a_bare_name_selects_only_itself() {
        assert_eq!(sel(&["api"]), vec!["api"]);
    }

    #[test]
    fn trailing_dots_pull_in_dependencies() {
        assert_eq!(sel(&["web..."]), vec!["api", "check", "web"]);
    }

    #[test]
    fn leading_dots_pull_in_dependents() {
        assert_eq!(sel(&["...check"]), vec!["api", "check", "web"]);
    }

    #[test]
    fn both_ends_reach_both_ways() {
        assert_eq!(sel(&["...api..."]), vec!["api", "check", "web"]);
    }

    #[test]
    fn patterns_are_unioned_not_intersected() {
        assert_eq!(sel(&["docs", "check"]), vec!["check", "docs"]);
    }

    #[test]
    fn globs_match_names() {
        let mut got = sel(&["*e*"]);
        got.sort();
        assert_eq!(got, vec!["check", "web"]);
    }

    #[test]
    fn an_unmatched_filter_names_what_exists() {
        let err = expand(&ws(), &["frontend".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("matched no nodes"), "{err}");
        // Silently selecting nothing would look like a successful empty run.
        assert!(
            err.contains("api"),
            "the error should list real nodes: {err}"
        );
    }

    #[test]
    fn dots_with_no_name_are_rejected() {
        assert!(expand(&ws(), &["...".to_string()]).is_err());
    }
}
