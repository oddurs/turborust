//! Desktop notifications for failures a person is not looking at.
//!
//! The entire design problem here is restraint. A crash-looping service restarts
//! every few hundred milliseconds; a notification per restart is how a helpful
//! feature becomes the reason someone uninstalls the tool. So notifications
//! follow *transitions*, never states.

use std::collections::BTreeMap;

/// What a node's health looked like last time we considered notifying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Ok,
    Failing,
}

/// Decides when a notification is worth sending, and sends nothing itself when
/// the answer is no.
#[derive(Debug, Default)]
pub struct Notifier {
    enabled: bool,
    seen: BTreeMap<String, Seen>,
}

/// A notification worth showing.
#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub failure: bool,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Notifier {
            enabled,
            seen: BTreeMap::new(),
        }
    }

    /// Reports a node's current health and returns a notice only on a change.
    ///
    /// Returning `None` while a node stays broken is the point: a service that
    /// crashes fifty times in a minute is one piece of news, not fifty.
    pub fn observe(&mut self, node: &str, failing: bool) -> Option<Notice> {
        if !self.enabled {
            return None;
        }
        let now = if failing { Seen::Failing } else { Seen::Ok };
        let before = self.seen.insert(node.to_string(), now);

        match (before, now) {
            // First sighting of a healthy node is not news.
            (None, Seen::Ok) => None,
            (None, Seen::Failing) | (Some(Seen::Ok), Seen::Failing) => Some(Notice {
                title: format!("{node} failed"),
                body: "turborust: the process exited with an error.".into(),
                failure: true,
            }),
            (Some(Seen::Failing), Seen::Ok) => Some(Notice {
                title: format!("{node} recovered"),
                body: "turborust: back to healthy.".into(),
                failure: false,
            }),
            _ => None,
        }
    }
}

/// Shows a notice, if the platform can.
///
/// Failure is ignored on purpose: a headless machine, a missing notification
/// daemon or a denied permission are all reasons not to notify, and none of them
/// is a reason to interrupt a build.
pub fn show(notice: &Notice) {
    let _ = notify_rust::Notification::new()
        .summary(&notice.title)
        .body(&notice.body)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_loop_produces_one_notification() {
        let mut n = Notifier::new(true);
        assert!(
            n.observe("api", true).is_some(),
            "the first failure is news"
        );
        for _ in 0..50 {
            assert!(
                n.observe("api", true).is_none(),
                "a node that is still broken is not new news"
            );
        }
    }

    #[test]
    fn recovery_is_reported_once() {
        let mut n = Notifier::new(true);
        n.observe("api", true);
        let notice = n.observe("api", false).expect("recovery is news");
        assert!(!notice.failure);
        assert!(notice.title.contains("recovered"));
        assert!(
            n.observe("api", false).is_none(),
            "still healthy is not news"
        );
    }

    #[test]
    fn a_node_that_starts_healthy_says_nothing() {
        let mut n = Notifier::new(true);
        assert!(n.observe("api", false).is_none());
    }

    #[test]
    fn nodes_are_tracked_independently() {
        let mut n = Notifier::new(true);
        assert!(n.observe("api", true).is_some());
        // A different node failing is separate news.
        assert!(n.observe("web", true).is_some());
    }

    #[test]
    fn disabled_means_silent() {
        let mut n = Notifier::new(false);
        assert!(n.observe("api", true).is_none());
        assert!(n.observe("api", false).is_none());
    }
}
