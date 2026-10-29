//! Completing node names in the shell (cairn 0061).
//!
//! The static scripts know the subcommands and flags. The part a person has to
//! remember is the nodes in the nearest config, so the scripts also ask the
//! binary, through the hidden `turborust __nodes`, whenever a word is a node.
//!
//! A completer runs on every tab, so this path reads `turborust.toml` as plain
//! TOML and nothing else: no `cargo metadata`, no validation, no plan. It
//! prints nothing when anything is wrong, because whatever it prints lands in
//! the command line being typed.
//!
//! clap_complete's own dynamic completion would do this too, but only behind
//! its `unstable-dynamic` feature, which may change in any release; a broken
//! completer breaks the shell, so the glue here is small and ours.

use std::path::{Path, PathBuf};

/// Subcommands whose positional arguments are node names.
const TAKES_NODES: &str = "up run why connect verify";

/// Every service and task in the nearest config, sorted; empty on any error.
pub fn node_names(explicit: Option<&Path>) -> Vec<String> {
    let Some(path) = explicit.map(Path::to_path_buf).or_else(nearest) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut names: Vec<String> = ["services", "tasks"]
        .iter()
        .filter_map(|k| table.get(*k)?.as_table())
        .flat_map(|t| t.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn nearest() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        for name in crate::config::CONFIG_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The generated script, with node names wired in where the shell allows it.
pub fn script(shell: clap_complete::Shell, generated: String) -> String {
    use clap_complete::Shell;
    match shell {
        Shell::Bash => generated + &BASH.replace("{TAKES}", &TAKES_NODES.replace(' ', "|")),
        Shell::Zsh => zsh(generated),
        Shell::Fish => generated + &FISH.replace("{TAKES}", TAKES_NODES),
        // PowerShell and elvish keep the static script.
        _ => generated,
    }
}

const BASH: &str = r#"
# Node names from the nearest turborust.toml, for the subcommands that take
# them; everything else as generated above.
_turborust_nodes() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}
    if [[ $COMP_CWORD -ge 2 && $cur != -* && ( $prev != -* || $prev == -F || $prev == --filter ) ]]; then
        case ${COMP_WORDS[1]} in
            {TAKES})
                local names
                names=$(turborust __nodes 2>/dev/null)
                if [[ -n $names ]]; then
                    COMPREPLY=($(compgen -W "$names" -- "$cur"))
                    return 0
                fi
                ;;
        esac
    fi
    _turborust "$@"
}
complete -F _turborust_nodes -o bashdefault -o default turborust
"#;

const ZSH_FN: &str = r#"
# Node names from the nearest turborust.toml, for the subcommands that take
# them; everything else as generated above.
_turborust_nodes() {
    if (( CURRENT > 2 )) && [[ ${words[2]} == ({TAKES}) && ${words[CURRENT]} != -* ]] \
        && [[ ${words[CURRENT-1]} != -* || ${words[CURRENT-1]} == (-F|--filter) ]]; then
        local -a nodes
        nodes=(${(f)"$(turborust __nodes 2>/dev/null)"})
        if (( ${#nodes} )); then
            compadd -a nodes
            return
        fi
    fi
    _turborust "$@"
}

"#;

const FISH: &str = r#"
# Node names from the nearest turborust.toml.
complete -c turborust -n "__fish_seen_subcommand_from {TAKES}" -f -a "(turborust __nodes 2>/dev/null)"
"#;

/// zsh's script both defines the function and registers it, at its end; the
/// node-aware function goes in before that and is what gets registered.
fn zsh(generated: String) -> String {
    const TAIL: &str = "if [ \"$funcstack[1]\" = \"_turborust\" ]; then\n    _turborust \"$@\"\nelse\n    compdef _turborust turborust\nfi";
    let Some(at) = generated.rfind(TAIL) else {
        // Not the shape this was written against: the static script, unchanged,
        // beats a half-edited one.
        return generated;
    };
    let tail = TAIL
        .replace("    _turborust \"$@\"", "    _turborust_nodes \"$@\"")
        .replace(
            "compdef _turborust turborust",
            "compdef _turborust_nodes turborust",
        );
    format!(
        "{}{}{}{}",
        &generated[..at],
        ZSH_FN.replace("{TAKES}", &TAKES_NODES.replace(' ', "|")),
        tail,
        &generated[at + TAIL.len()..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_file_alone_and_errors_are_silent() {
        let dir = std::env::temp_dir().join(format!("turborust-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("turborust.toml");
        // Not a valid config (an unknown key, a dependency on nothing), which
        // completion has no business refusing.
        std::fs::write(
            &cfg,
            "[project]\nmade_up = 1\n[services.web]\ncmd = \"x\"\ndepends_on = [\"nope\"]\n[tasks.build]\ncmd = \"y\"\n[tasks.api-test]\n",
        )
        .unwrap();
        assert_eq!(node_names(Some(&cfg)), ["api-test", "build", "web"]);
        std::fs::write(&cfg, "[tasks.build\n").unwrap();
        assert!(node_names(Some(&cfg)).is_empty());
        assert!(node_names(Some(&dir.join("missing.toml"))).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
