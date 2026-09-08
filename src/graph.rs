//! Rendering the node graph.
//!
//! `plan` prints a list, which stops conveying shape past a handful of nodes:
//! what runs in parallel, and what everything is waiting on, are properties of
//! the edges rather than the entries.

use crate::plan::{Kind, Plan};
use crate::summary::Prediction;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Pastes into a README or a pull request and renders there.
    Mermaid,
    /// Pipes into `dot -Tsvg`.
    Dot,
}

pub fn parse_format(s: &str) -> Result<Format> {
    Ok(match s {
        "mermaid" => Format::Mermaid,
        "dot" => Format::Dot,
        other => bail!("unknown --format `{other}` (mermaid, dot)"),
    })
}

/// Cache outcome per task, when a prediction is available.
fn outcomes(prediction: Option<&Prediction>) -> BTreeMap<&str, bool> {
    prediction
        .map(|p| {
            p.tasks
                .iter()
                .map(|t| (t.task.as_str(), t.cached))
                .collect()
        })
        .unwrap_or_default()
}

/// Renders the graph, optionally coloured by what a run would do.
///
/// With a prediction attached the picture stops being a diagram of the config and
/// becomes a diagram of the next run, which is the version worth having.
pub fn render(plan: &Plan, format: Format, prediction: Option<&Prediction>) -> Result<String> {
    let cached = outcomes(prediction);
    match format {
        Format::Mermaid => mermaid(plan, &cached),
        Format::Dot => dot(plan, &cached),
    }
}

/// Mermaid rejects several characters in node ids, so ids are sanitised and the
/// real name goes in the label.
fn id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn mermaid(plan: &Plan, cached: &BTreeMap<&str, bool>) -> Result<String> {
    let mut out = String::from("graph LR\n");
    for name in &plan.order {
        let node = plan.get(name)?;
        let derived = node.derived_from_crate.is_some();
        let label = if derived {
            format!("{name}<br/><small>from cargo</small>")
        } else {
            name.clone()
        };
        // Services get rounded ends, tasks get square: shape carries the kind so
        // the legend is the picture rather than a key beside it.
        let shape = if node.kind == Kind::Task {
            format!("{}[\"{}\"]", id(name), label)
        } else {
            format!("{}([\"{}\"])", id(name), label)
        };
        out.push_str(&format!("  {shape}\n"));
        match cached.get(name.as_str()) {
            Some(true) => out.push_str(&format!("  class {} cached\n", id(name))),
            Some(false) => out.push_str(&format!("  class {} runs\n", id(name))),
            None => {}
        }
    }
    for name in &plan.order {
        for dep in &plan.get(name)?.depends_on {
            out.push_str(&format!("  {} --> {}\n", id(dep), id(name)));
        }
    }
    if !cached.is_empty() {
        out.push_str("  classDef cached fill:#e8f5ec,stroke:#2f9e5c,color:#14532d\n");
        out.push_str("  classDef runs fill:#fdf3e0,stroke:#b07d09,color:#5c3d00\n");
    }
    Ok(out)
}

fn dot(plan: &Plan, cached: &BTreeMap<&str, bool>) -> Result<String> {
    let mut out = String::from("digraph turborust {\n  rankdir=LR;\n  node [fontname=\"sans\"];\n");
    for name in &plan.order {
        let node = plan.get(name)?;
        let shape = if node.kind == Kind::Task {
            "box"
        } else {
            "ellipse"
        };
        let fill = match cached.get(name.as_str()) {
            Some(true) => ", style=filled, fillcolor=\"#e8f5ec\"",
            Some(false) => ", style=filled, fillcolor=\"#fdf3e0\"",
            None => "",
        };
        out.push_str(&format!("  {:?} [shape={shape}{fill}];\n", name));
    }
    for name in &plan.order {
        for dep in &plan.get(name)?.depends_on {
            out.push_str(&format!("  {dep:?} -> {name:?};\n"));
        }
    }
    out.push_str("}\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Workspace};
    use std::path::PathBuf;

    fn plan() -> Plan {
        let src = r#"
            [tasks.check]
            cmd = "true"
            [services.api]
            cmd = "true"
            depends_on = ["check"]
            [services."web-ui"]
            cmd = "true"
            depends_on = ["api"]
        "#;
        let ws = Workspace {
            root: PathBuf::from("/w"),
            config: toml::from_str::<Config>(src).unwrap(),
            source: None,
        };
        crate::plan::resolve(&ws, &["web-ui".into()]).unwrap()
    }

    #[test]
    fn mermaid_declares_every_node_and_edge() {
        let out = render(&plan(), Format::Mermaid, None).unwrap();
        assert!(out.starts_with("graph LR"));
        assert!(out.contains("check[\"check\"]"), "{out}");
        assert!(
            out.contains("api([\"api\"])"),
            "services are rounded: {out}"
        );
        assert!(out.contains("check --> api"));
        assert!(out.contains("api --> web_ui"));
    }

    #[test]
    fn ids_are_sanitised_but_labels_keep_the_real_name() {
        let out = render(&plan(), Format::Mermaid, None).unwrap();
        // A hyphen in a mermaid id breaks the parse; the label must still read.
        assert!(out.contains("web_ui([\"web-ui\"])"), "{out}");
    }

    #[test]
    fn dot_is_well_formed_and_quotes_names() {
        let out = render(&plan(), Format::Dot, None).unwrap();
        assert!(out.starts_with("digraph turborust {"));
        assert!(out.trim_end().ends_with('}'));
        assert!(
            out.contains("\"web-ui\""),
            "dot quotes handle hyphens: {out}"
        );
        assert!(out.contains("\"api\" -> \"web-ui\";"));
    }

    #[test]
    fn a_prediction_colours_the_nodes() {
        let prediction = Prediction {
            schema: crate::summary::SCHEMA,
            tasks: vec![crate::summary::TaskPrediction {
                task: "check".into(),
                command: "true".into(),
                cached: true,
                key: "abc".into(),
                changed: vec![],
                uncacheable: false,
            }],
        };
        let out = render(&plan(), Format::Mermaid, Some(&prediction)).unwrap();
        assert!(out.contains("class check cached"), "{out}");
        assert!(
            out.contains("classDef cached"),
            "the class must be defined: {out}"
        );
    }

    #[test]
    fn without_a_prediction_no_styling_is_emitted() {
        let out = render(&plan(), Format::Mermaid, None).unwrap();
        assert!(!out.contains("classDef"), "unused styling is noise: {out}");
    }

    #[test]
    fn unknown_formats_list_the_real_ones() {
        let err = parse_format("svg").unwrap_err().to_string();
        assert!(err.contains("mermaid"), "{err}");
    }
}
