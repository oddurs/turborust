//! The injected dev overlay: asset serving and the bootstrap tag.
//!
//! CSS and JS are embedded in the binary rather than read from disk, so a
//! released `turborust` is a single file with no asset directory to lose. They
//! are served as separate requests instead of inlined into every HTML response,
//! which keeps the injected markup to one line.

use crate::config::Overlay;
use serde::Serialize;

pub const JS_PATH: &str = "/__turborust/overlay.js";
pub const CSS_PATH: &str = "/__turborust/overlay.css";
pub const STATE_PATH: &str = "/__turborust/state";
pub const LOGS_PATH: &str = "/__turborust/logs";
pub const RESTART_PATH: &str = "/__turborust/restart/{node}";
pub const RELOAD_PATH: &str = "/__turborust/reload";

pub const JS: &str = include_str!("../assets/overlay.js");
pub const CSS: &str = include_str!("../assets/overlay.css");

/// Config handed to the browser. Field names match the JS defaults exactly.
#[derive(Debug, Serialize)]
struct BootConfig<'a> {
    emoji: &'a str,
    position: &'a str,
    theme: &'a str,
    shortcut: &'a str,
    errors: &'a str,
    /// Absolute workspace root, so the overlay can build `vscode://` jump links.
    root: String,
    endpoint: &'a str,
    logs: &'a str,
    reload: &'a str,
    #[serde(rename = "cssUrl")]
    css_url: &'a str,
    live: bool,
}

/// The single tag injected before `</body>`.
pub fn bootstrap_tag(cfg: &Overlay, root: &std::path::Path) -> String {
    let boot = BootConfig {
        emoji: &cfg.emoji,
        position: cfg.position.as_str(),
        theme: cfg.theme.as_str(),
        shortcut: &cfg.shortcut,
        errors: cfg.errors.as_str(),
        root: root.to_string_lossy().trim_start_matches('/').to_string(),
        endpoint: STATE_PATH,
        logs: LOGS_PATH,
        reload: RELOAD_PATH,
        css_url: CSS_PATH,
        live: true,
    };
    let json = serde_json::to_string(&boot).unwrap_or_else(|_| "{}".into());
    // Single-quoted attribute, so only `'` and `&` need escaping; the JSON itself
    // contains double quotes throughout.
    let attr = json.replace('&', "&amp;").replace('\'', "&#39;");
    format!("<script src=\"{JS_PATH}\" data-turborust='{attr}'></script>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn assets_are_embedded() {
        assert!(CSS.contains("--tr-crab"), "stylesheet did not embed");
        assert!(JS.contains("__turborustMount"), "client did not embed");
    }

    #[test]
    fn bootstrap_carries_the_configured_values() {
        let cfg = Overlay {
            emoji: "🦞".into(),
            position: crate::config::Position::TopLeft,
            theme: crate::config::Theme::Dark,
            errors: crate::config::ErrorMode::Badge,
            ..Default::default()
        };
        let tag = bootstrap_tag(&cfg, Path::new("/w/app"));
        assert!(tag.contains("\"position\":\"top-left\""), "{tag}");
        assert!(tag.contains("\"theme\":\"dark\""), "{tag}");
        assert!(tag.contains("\"errors\":\"badge\""), "{tag}");
        assert!(tag.contains("🦞"), "{tag}");
        assert!(tag.contains(JS_PATH));
    }

    #[test]
    fn single_quotes_cannot_break_out_of_the_attribute() {
        let cfg = Overlay {
            shortcut: "ctrl+'".into(),
            ..Default::default()
        };
        let tag = bootstrap_tag(&cfg, Path::new("/w"));
        let attr = tag.split("data-turborust='").nth(1).unwrap();
        let attr = attr.split('\'').next().unwrap();
        assert!(
            attr.contains("&#39;"),
            "quote must be entity-escaped: {attr}"
        );
        assert!(attr.contains("shortcut"));
    }
}
