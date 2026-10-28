//! `scripts/audit`, run against small tracker fixtures in their own repositories.
//!
//! Regression cover for cairn 0084: four finished milestones left open, a done
//! item with every criterion unticked, a claim held seven weeks after its work
//! merged, and an empty stub that was really done. Each case here is one of
//! those, reduced to the smallest tracker that shows it.
//!
//! Unix only: the audit is a bash script, like the rest of `scripts/`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CONFIG: &str = r#"
format = 2

[project]
name = "fixture"
dir = "cairn/items"

[[status]]
name = "backlog"
category = "open"

[[status]]
name = "doing"
category = "active"

# Renamed on purpose: the audit must read categories, not guess status names.
[[status]]
name = "shipped"
category = "done"

[[status]]
name = "dropped"
category = "dropped"

[[type]]
name = "milestone"
"#;

struct Tracker {
    dir: PathBuf,
}

impl Tracker {
    fn new(tag: &str) -> Tracker {
        let dir =
            std::env::temp_dir().join(format!("turborust-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cairn/items")).unwrap();
        let t = Tracker {
            dir: dir.canonicalize().unwrap(),
        };
        t.git(&["init", "-q", "-b", "main", "."]);
        t.assert_owns_its_repo();
        t.write("cairn.toml", CONFIG);
        t
    }

    /// Git with the variables a hook exports stripped, as `affected::git_command`
    /// does. Run from the pre-push hook without this, `git init` here would
    /// re-initialise turborust's own repository — see cairn 0083.
    fn git_cmd(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.dir);
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn git(&self, args: &[&str]) {
        let out = self
            .git_cmd()
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The 0083 guard: nothing is written until this directory is proved to be
    /// its own repository rather than one it sits inside.
    fn assert_owns_its_repo(&self) {
        let out = self
            .git_cmd()
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            .expect("git runs");
        let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        assert_eq!(
            common.canonicalize().ok(),
            self.dir.join(".git").canonicalize().ok(),
            "fixture is not its own repository"
        );
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.dir.join(rel), body).unwrap();
    }

    fn item(&self, id: u32, front: &str, body: &str) {
        self.write(
            &format!("cairn/items/{id:04}-item.md"),
            &format!("---\nid: {id}\ntitle: item {id}\n{front}\n---\n\n{body}"),
        );
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
    }

    /// Runs the real script from inside the fixture. The script shells out to
    /// git, so it gets the same stripped environment, and a fixed `today` so
    /// staleness does not depend on when the suite runs.
    fn audit(&self, args: &[&str], env: &[(&str, &str)]) -> Audit {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/audit");
        let mut cmd = Command::new(script);
        cmd.current_dir(&self.dir);
        // `get_envs` lists removals too, so this copies exactly the stripping above.
        for (var, _) in self.git_cmd().get_envs() {
            cmd.env_remove(var);
        }
        let out: Output = cmd
            .env_remove("GITHUB_ACTIONS")
            .env_remove("AUDIT_STALE_DAYS")
            .env("AUDIT_TODAY", "2026-10-28")
            .envs(env.iter().copied())
            .args(args)
            .output()
            .expect("audit runs");
        Audit {
            code: out.status.code().unwrap_or(-1),
            text: String::from_utf8_lossy(&out.stdout).into_owned(),
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Audit {
    code: i32,
    text: String,
}

impl Audit {
    fn says(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }
}

const BODY: &str = "## Problem\n\nSomething.\n";

fn milestone(t: &Tracker, id: u32, key: &str, status: &str) {
    t.item(
        id,
        &format!("type: milestone\nkey: {key}\nstatus: {status}"),
        "What it ships.\n",
    );
}

#[test]
fn a_tracker_that_matches_the_work_passes() {
    let t = Tracker::new("clean");
    milestone(&t, 1, "v1", "shipped");
    t.item(
        2,
        "type: bug\nstatus: shipped\nmilestone: v1",
        "## Criteria\n\n- [x] done\n",
    );
    t.item(3, "type: bug\nstatus: backlog", BODY);
    t.commit("chore: fixture");

    let a = t.audit(&[], &[]);
    assert_eq!(a.code, 0, "{}", a.text);
    assert!(a.says("tracker matches the work"), "{}", a.text);
}

#[test]
fn a_finished_milestone_left_open_warns_and_strict_fails() {
    let t = Tracker::new("open-milestone");
    milestone(&t, 1, "v1", "backlog");
    t.item(2, "type: bug\nstatus: shipped\nmilestone: v1", BODY);
    t.item(3, "type: bug\nstatus: dropped\nmilestone: v1", BODY);
    t.commit("chore: fixture");

    let a = t.audit(&[], &[]);
    assert_eq!(a.code, 0, "a warning alone must not fail: {}", a.text);
    assert!(
        a.says("all 2 items are finished, but the milestone is open"),
        "{}",
        a.text
    );
    assert_eq!(t.audit(&["--strict"], &[]).code, 1);
}

#[test]
fn a_closed_milestone_with_open_work_is_an_error() {
    let t = Tracker::new("closed-milestone");
    milestone(&t, 1, "v1", "shipped");
    t.item(2, "type: bug\nstatus: shipped\nmilestone: v1", BODY);
    t.item(3, "type: bug\nstatus: doing\nmilestone: v1", BODY);
    t.commit("chore: fixture");

    let a = t.audit(&[], &[]);
    assert_eq!(a.code, 1, "{}", a.text);
    assert!(
        a.says("closed, but 1 of its 2 items are still open"),
        "{}",
        a.text
    );
}

#[test]
fn a_milestone_with_nothing_scheduled_is_not_reported() {
    let t = Tracker::new("empty-milestone");
    milestone(&t, 1, "later", "backlog");
    t.commit("chore: fixture");
    assert!(t.audit(&[], &[]).says("tracker matches the work"));
}

#[test]
fn a_done_item_with_unticked_criteria_warns() {
    let t = Tracker::new("unticked");
    t.item(
        1,
        "type: bug\nstatus: shipped",
        "## Criteria\n\n- [x] one\n- [ ] two\n  - [ ] three\n",
    );
    t.commit("chore: fixture");

    let a = t.audit(&[], &[]);
    assert_eq!(a.code, 0);
    assert!(
        a.says("done with 2 of 3 acceptance criteria unticked"),
        "{}",
        a.text
    );
}

#[test]
fn a_claim_is_stale_from_the_day_it_was_claimed() {
    let t = Tracker::new("stale");
    // A recent `updated` must not hide an old claim: cairn measures from `claimed`.
    t.item(
        1,
        "type: bug\nstatus: doing\nclaimed: 2026-09-28\nupdated: 2026-10-27",
        BODY,
    );
    t.commit("chore: fixture");

    let at = |today| t.audit(&[], &[("AUDIT_TODAY", today)]);
    assert!(
        !at("2026-10-27").says("days ago"),
        "29 days is not yet stale"
    );
    assert!(at("2026-10-28").says("claimed 2026-09-28, 30 days ago"));
}

#[test]
fn staleness_counts_days_across_a_leap_february() {
    let t = Tracker::new("leap");
    t.item(1, "type: bug\nstatus: doing\nclaimed: 2024-02-28", BODY);
    t.commit("chore: fixture");
    // 28 Feb to 29 Mar 2024 is 30 days only because the 29th of February exists.
    let a = t.audit(&[], &[("AUDIT_TODAY", "2024-03-29")]);
    assert!(a.says("30 days ago"), "{}", a.text);
}

#[test]
fn the_stale_threshold_is_read_from_cairn_toml() {
    let t = Tracker::new("threshold");
    t.write(
        "cairn.toml",
        &CONFIG.replace(
            "dir = \"cairn/items\"",
            "dir = \"cairn/items\"\nclaim_stale_after = 7",
        ),
    );
    t.item(1, "type: bug\nstatus: doing\nclaimed: 2026-10-20", BODY);
    t.commit("chore: fixture");
    assert!(t.audit(&[], &[]).says("8 days ago"));
}

#[test]
fn an_open_item_that_landed_work_refs_warns() {
    let t = Tracker::new("landed");
    t.item(7, "type: bug\nstatus: doing", BODY);
    t.commit("fix: the thing\n\nRefs: 0007");

    let a = t.audit(&[], &[]);
    assert!(
        a.says("0007 item 7 — still doing, but a commit on the default branch refs it"),
        "{}",
        a.text
    );
}

#[test]
fn a_branch_that_leaves_its_item_open_gets_a_notice() {
    let t = Tracker::new("branch");
    t.item(5, "type: bug\nstatus: backlog", BODY);
    t.item(6, "type: bug\nstatus: shipped", BODY);
    t.commit("chore: fixture");
    t.git(&["checkout", "-q", "-b", "fix/5"]);
    t.commit("fix: part of five\n\nRefs: 0005");
    t.commit("fix: all of six\n\nRefs: 0006");

    let a = t.audit(&["--branch", "main"], &[]);
    assert_eq!(a.code, 0, "a notice never fails: {}", a.text);
    assert!(a.says("0005 item 5 — this branch refs it"), "{}", a.text);
    assert!(
        !a.says("0006 item 6"),
        "a closed item is not reported: {}",
        a.text
    );
    assert!(!t.audit(&[], &[]).says("this branch"), "only with --branch");
}

#[test]
fn an_open_stub_with_empty_sections_warns_but_a_closed_one_does_not() {
    let t = Tracker::new("stub");
    let stub = "## Problem\n\n## Proposal\n\nA plan.\n\n## Acceptance criteria\n\n- [x]\n";
    t.item(1, "type: feature\nstatus: backlog", stub);
    t.item(2, "type: feature\nstatus: shipped", stub);
    t.commit("chore: fixture");

    let a = t.audit(&[], &[]);
    assert!(
        a.says("0001 item 1 — open with nothing under `Problem`, `Acceptance criteria`"),
        "{}",
        a.text
    );
    assert!(!a.says("0002"), "{}", a.text);
}

#[test]
fn under_github_actions_findings_are_annotations_on_the_item_file() {
    let t = Tracker::new("annotations");
    milestone(&t, 1, "v1", "shipped");
    t.item(2, "type: bug\nstatus: backlog\nmilestone: v1", BODY);
    t.commit("chore: fixture");

    let a = t.audit(&[], &[("GITHUB_ACTIONS", "true")]);
    assert!(
        a.says("::error file=cairn/items/0001-item.md,line=1,title=tracker 0001::closed, but 1 of its 1 items are still open"),
        "{}",
        a.text
    );
}

#[test]
fn a_quoted_title_is_reported_without_its_quotes() {
    // cairn quotes a title containing `: `, as in `0042`.
    let t = Tracker::new("quoted");
    t.write(
        "cairn/items/0009-item.md",
        "---\nid: 9\ntitle: 'Proxy: one origin'\ntype: bug\nstatus: doing\n---\n\n## Problem\n\n",
    );
    t.commit("chore: fixture");
    let a = t.audit(&[], &[]);
    assert!(
        a.says("0009 Proxy: one origin — open with nothing under `Problem`"),
        "{}",
        a.text
    );
}
