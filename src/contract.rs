//! Machine-readable output as an interface (cairn 0078).
//!
//! Every JSON payload turborust prints or serves is listed in [`Output`], has a
//! JSON Schema (`turborust schema --json <output>`), and carries
//! `schemaVersion`. The schemas are committed under `schemas/`, and CI fails a
//! change that removes or retypes a field without raising that version. What
//! may change without one is written down in `docs/json.md`.

use crate::cache::{Divergence, Sweep};
use crate::state::HistoryRecord;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

/// Every machine-readable payload, by the name `schema --json` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Output {
    /// `run --dry-run=json`
    DryRun,
    /// `run --summarize`, written under `.turborust/summaries/`
    Summary,
    /// `verify --json`
    Verify,
    /// `cache --json`
    Cache,
    /// Each frame of the overlay's state stream, `/__turborust/state`
    State,
    /// `/__turborust/history`
    History,
    /// stderr, under `--error-format json`
    Error,
}

impl Output {
    pub const ALL: [Output; 7] = [
        Output::DryRun,
        Output::Summary,
        Output::Verify,
        Output::Cache,
        Output::State,
        Output::History,
        Output::Error,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Output::DryRun => "dry-run",
            Output::Summary => "summary",
            Output::Verify => "verify",
            Output::Cache => "cache",
            Output::State => "state",
            Output::History => "history",
            Output::Error => "error",
        }
    }

    /// Raised when a field is removed, renamed or retyped; never for an added
    /// one. Each payload has its own, so one breaking change does not ask every
    /// consumer to re-check.
    pub fn version(self) -> u32 {
        match self {
            Output::DryRun => 1,
            Output::Summary => 1,
            Output::Verify => 1,
            Output::Cache => 1,
            Output::State => 1,
            Output::History => 1,
            Output::Error => 1,
        }
    }

    pub fn schema(self) -> serde_json::Value {
        let schema = match self {
            Output::DryRun => schemars::schema_for!(crate::summary::Prediction),
            Output::Summary => schemars::schema_for!(crate::summary::RunSummary),
            Output::Verify => schemars::schema_for!(VerifyReport),
            Output::Cache => schemars::schema_for!(CacheReport),
            Output::State => schemars::schema_for!(crate::state::Snapshot),
            Output::History => schemars::schema_for!(HistoryReport),
            Output::Error => schemars::schema_for!(ErrorReport),
        };
        let mut v = serde_json::to_value(schema).expect("a schema serialises");
        // The version is part of the contract, so the schema states it: a
        // consumer validating against version 1 rejects a version 2 payload.
        v["properties"]["schemaVersion"] = serde_json::json!({
            "type": "integer",
            "const": self.version(),
        });
        v["title"] = serde_json::json!(format!("turborust {} output", self.name()));
        v
    }
}

/// `cache --json`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheReport {
    pub schema_version: u32,
    pub bytes: u64,
    pub records: usize,
    /// The local store's byte budget; absent when there is none.
    pub budget_bytes: Option<u64>,
    /// Task name -> [bytes, record count].
    pub by_task: BTreeMap<String, (u64, usize)>,
    pub shared_bytes: Option<u64>,
    pub last_sweep: Option<Sweep>,
}

/// `verify --json`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub schema_version: u32,
    /// True when every task checked produced identical outputs twice.
    pub deterministic: bool,
    pub tasks: Vec<VerifyTask>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VerifyTask {
    pub task: String,
    pub deterministic: bool,
    /// How many output files the first run produced.
    pub outputs: usize,
    pub differing: Vec<DifferingOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DifferingOutput {
    pub path: String,
    /// Content hash from the first run; absent if only the second produced it.
    pub first: Option<String>,
    pub second: Option<String>,
    /// Where the two first differ, when both runs produced the file.
    pub divergence: Option<Divergence>,
}

/// `/__turborust/history`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryReport {
    pub schema_version: u32,
    /// Oldest first.
    pub history: Vec<HistoryRecord>,
}

/// What a failed command prints on stderr under `--error-format json`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorReport {
    pub schema_version: u32,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorBody {
    pub code: Code,
    /// The outermost message, as the text form prints it first.
    pub message: String,
    /// What it was caused by, outermost first.
    pub causes: Vec<String>,
}

/// What kind of failure this was, so a consumer can branch without reading
/// English. A new code may be added in any release; one is never renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Code {
    /// No `turborust.toml` here or in any parent.
    ConfigNotFound,
    /// The config could not be read, parsed or validated.
    ConfigInvalid,
    /// The config is valid, but no plan could be built from it: an unknown
    /// target, a cycle, a setting that contradicts another.
    PlanInvalid,
    /// The command needs a running `turborust up`, and there is none.
    NotRunning,
    /// Anything not yet given a code of its own.
    Error,
}

/// An error that knows its [`Code`]. Reads exactly as the error it wraps, so
/// the text form is unchanged; only `--error-format json` looks at the code.
#[derive(Debug)]
pub struct Failure {
    code: Code,
    inner: anyhow::Error,
}

impl Failure {
    pub fn coded(code: Code, inner: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(Failure { code, inner })
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

/// The payload for `err`: the innermost code it was given, or `error`.
pub fn error_report(err: &anyhow::Error) -> ErrorReport {
    let code = err
        .chain()
        .filter_map(|e| e.downcast_ref::<Failure>())
        .last()
        .map_or(Code::Error, |f| f.code);
    let mut chain = err.chain().map(|e| e.to_string());
    ErrorReport {
        schema_version: Output::Error.version(),
        error: ErrorBody {
            code,
            message: chain.next().unwrap_or_default(),
            causes: chain.collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn every_schema_states_its_version() {
        for o in Output::ALL {
            let s = o.schema();
            assert_eq!(s["properties"]["schemaVersion"]["const"], o.version());
            assert!(
                s["required"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|f| f == "schemaVersion")),
                "{}: schemaVersion must be required",
                o.name()
            );
        }
    }

    #[test]
    fn a_coded_error_reads_as_before_and_keeps_its_code_under_context() {
        let err = Failure::coded(Code::ConfigInvalid, anyhow::anyhow!("bad key `x`"));
        let err = Err::<(), _>(err)
            .context("loading the workspace")
            .unwrap_err();
        assert_eq!(format!("{err:#}"), "loading the workspace: bad key `x`");
        let r = error_report(&err);
        assert_eq!(r.error.code, Code::ConfigInvalid);
        assert_eq!(r.error.message, "loading the workspace");
        assert_eq!(r.error.causes, ["bad key `x`"]);
        assert_eq!(error_report(&anyhow::anyhow!("x")).error.code, Code::Error);
    }
}
