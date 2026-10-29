//! The supervisor's decisions, driven exactly.
//!
//! Cover for cairn 0105. Every test here is a sequence of events and the actions
//! they produce — no processes, no sleeps, no tokio — so each runs in
//! microseconds and none can be flaky. The integration suite in
//! `supervisor.rs` still proves the executor does what these say; this proves
//! what they say is right, including the orderings that are only ever hit by
//! luck with real processes.

use std::time::{Duration, Instant};
use turborust::config::{OnChange, Reload, RestartPolicy};
use turborust::lifecycle::{
    Action, Deps, Event, Purpose, ReloadCounters, Service, ServiceConfig, Task, Timer,
};
use turborust::state::Status;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cfg() -> ServiceConfig {
    ServiceConfig {
        has_cmd: true,
        port: None,
        probed: false,
        probe_interval: ms(100),
        ready_timeout: ms(1000),
        stop_timeout: ms(500),
        restart: RestartPolicy::Always,
        on_change: OnChange::Restart,
        signal: None,
        reload: Reload::Restart,
        follows_reloads: false,
        liveness: None,
        backoff_min: ms(100),
        backoff_max: ms(1000),
        max_restarts: None,
    }
}

/// One dependency, `db`, ready at generation `g`.
fn ready(g: u64) -> Deps {
    Deps {
        unready: None,
        generations: vec![g],
        names: vec!["db".into()],
    }
}

fn unready(g: u64) -> Deps {
    Deps {
        unready: Some("db".into()),
        generations: vec![g],
        names: vec!["db".into()],
    }
}

fn none() -> Deps {
    Deps::default()
}

/// A clock that only moves when told.
struct Clock(Instant);

impl Clock {
    fn new() -> Clock {
        Clock(Instant::now())
    }
    fn now(&self) -> Instant {
        self.0
    }
    fn advance(&mut self, d: Duration) -> Instant {
        self.0 += d;
        self.0
    }
}

fn has(acts: &[Action], a: &Action) -> bool {
    acts.contains(a)
}

fn status(acts: &[Action]) -> Option<Status> {
    acts.iter().rev().find_map(|a| match a {
        Action::SetStatus(s) => Some(s.clone()),
        _ => None,
    })
}

fn logs(acts: &[Action]) -> Vec<String> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Log(l) => Some(l.clone()),
            _ => None,
        })
        .collect()
}

/// Drives a service from `Begin` to a spawned process.
fn running(m: &mut Service, deps: &Deps, t: &Clock) -> Vec<Action> {
    assert_eq!(
        m.on(Event::Begin, deps, t.now()),
        vec![Action::DrainControl]
    );
    respawn(m, deps, t)
}

/// The rest of a cycle, from a machine that has just asked for a drain.
fn respawn(m: &mut Service, deps: &Deps, t: &Clock) -> Vec<Action> {
    assert_eq!(
        m.on(Event::Drained { stop: false }, deps, t.now()),
        vec![Action::AcquireSlot]
    );
    assert_eq!(
        m.on(Event::SlotAcquired, deps, t.now()),
        vec![Action::SetStatus(Status::Starting), Action::Spawn]
    );
    m.on(Event::Spawned, deps, t.now())
}

#[test]
fn a_service_waits_for_its_dependency_then_starts() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    assert_eq!(
        m.on(Event::Begin, &unready(0), t.now()),
        vec![Action::SetStatus(Status::Waiting("db".into()))]
    );
    // A restart while waiting is noise: starting is what it is waiting to do.
    assert_eq!(
        m.on(Event::Restart("a.rs".into()), &unready(0), t.now()),
        vec![Action::SetStatus(Status::Waiting("db".into()))]
    );
    assert_eq!(
        m.on(Event::Board, &ready(1), t.now()),
        vec![Action::DrainControl]
    );
    assert_eq!(
        m.on(Event::Drained { stop: false }, &ready(1), t.now()),
        vec![Action::AcquireSlot]
    );
}

#[test]
fn a_stop_while_waiting_ends_the_supervisor_stopped() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    m.on(Event::Begin, &unready(0), t.now());
    assert_eq!(
        m.on(Event::Stop, &unready(0), t.now()),
        vec![Action::SetStatus(Status::Stopped), Action::Exit]
    );
    assert!(m.is_done());
    assert!(
        m.on(Event::Board, &ready(1), t.now()).is_empty(),
        "a finished machine is inert"
    );
}

#[test]
fn a_stop_found_while_draining_wins_over_starting() {
    // Shutdown began while blocked on dependencies; spawning on the way out is
    // the bug this exists for.
    let t = Clock::new();
    let mut m = Service::new(cfg());
    m.on(Event::Begin, &none(), t.now());
    assert_eq!(
        m.on(Event::Drained { stop: true }, &none(), t.now()),
        vec![Action::SetStatus(Status::Stopped), Action::Exit]
    );
}

#[test]
fn without_a_probe_a_spawned_service_is_healthy_at_once() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    let acts = running(&mut m, &none(), &t);
    assert_eq!(
        acts,
        vec![
            Action::SetStatus(Status::Healthy),
            Action::ReleaseSlot,
            Action::ReloadBrowser
        ]
    );
    assert!(m.has_child());
}

#[test]
fn a_dependency_going_down_stops_a_running_service_once() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    running(&mut m, &ready(1), &t);
    let acts = m.on(Event::Board, &unready(1), t.now());
    assert_eq!(status(&acts), Some(Status::Stopping));
    assert!(has(
        &acts,
        &Action::CountRestart("dependency `db` is rebuilding".into())
    ));
    assert!(has(&acts, &Action::StopChild { grace: ms(500) }));
    // While it is stopping, more news changes nothing: one cascade, one restart.
    assert!(m.on(Event::Board, &unready(1), t.now()).is_empty());
    assert!(
        m.on(Event::Restart("a.rs".into()), &unready(1), t.now())
            .is_empty()
    );
    // Stopped, it waits for the dependency like any new cycle.
    assert_eq!(
        m.on(Event::ChildStopped, &unready(1), t.now()),
        vec![Action::SetStatus(Status::Waiting("db".into()))]
    );
    assert_eq!(
        m.on(Event::Board, &ready(2), t.now()),
        vec![Action::DrainControl]
    );
}

#[test]
fn a_new_result_with_no_visible_down_edge_still_restarts_a_service() {
    // Cover for cairn 0101, at the level it belongs: the dependency reran and
    // came back before anything saw it leave ready. All that is left of it is
    // a generation that moved, and that is enough.
    let t = Clock::new();
    let mut m = Service::new(cfg());
    running(&mut m, &ready(1), &t);
    assert!(
        m.on(Event::Board, &ready(1), t.now()).is_empty(),
        "nothing moved"
    );
    let acts = m.on(Event::Board, &ready(2), t.now());
    assert!(has(
        &acts,
        &Action::CountRestart("dependency `db` produced a new result".into())
    ));
}

#[test]
fn a_change_restarts_a_running_service_and_resets_its_backoff() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    running(&mut m, &none(), &t);
    let acts = m.on(Event::Restart("src/a.rs".into()), &none(), t.now());
    assert_eq!(
        acts,
        vec![
            Action::SetStatus(Status::Stopping),
            Action::CountRestart("src/a.rs".into()),
            Action::Log("restarting: src/a.rs".into()),
            Action::StopChild { grace: ms(500) },
        ]
    );
    // The next cycle drains: a second save during the stop is not a second restart.
    assert_eq!(
        m.on(Event::ChildStopped, &none(), t.now()),
        vec![Action::DrainControl]
    );
}

#[test]
fn on_change_ignore_signal_and_self_reload_leave_the_process_running() {
    let t = Clock::new();
    for (on_change, signal, reload, expect) in [
        (OnChange::Ignore, None, Reload::Restart, vec![]),
        (
            OnChange::Signal,
            Some(1),
            Reload::Restart,
            vec![Action::Signal {
                sig: 1,
                reason: "a.rs".into(),
            }],
        ),
        (
            OnChange::Signal,
            None,
            Reload::Restart,
            vec![Action::Log(
                "on_change = \"signal\" needs a `signal` value".into(),
            )],
        ),
        (OnChange::Restart, None, Reload::SelfManaged, vec![]),
    ] {
        let mut m = Service::new(ServiceConfig {
            on_change,
            signal,
            reload,
            ..cfg()
        });
        running(&mut m, &none(), &t);
        assert_eq!(
            m.on(Event::Restart("a.rs".into()), &none(), t.now()),
            expect
        );
        assert!(
            m.has_child(),
            "{on_change:?}/{reload:?} must not stop the process"
        );
    }
}

#[test]
fn readiness_is_probed_at_the_interval_until_it_passes() {
    let mut t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        probed: true,
        ..cfg()
    });
    assert_eq!(
        running(&mut m, &none(), &t),
        vec![Action::Probe(Purpose::Ready)]
    );
    let acts = m.on(
        Event::Probed {
            purpose: Purpose::Ready,
            result: Err("nothing answering on port 1".into()),
        },
        &none(),
        t.now(),
    );
    assert_eq!(
        acts,
        vec![Action::SetTimer(Timer::Probe, t.now() + ms(100))]
    );
    t.advance(ms(100));
    assert_eq!(
        m.on(Event::Timer(Timer::Probe), &none(), t.now()),
        vec![Action::Probe(Purpose::Ready)]
    );
    let acts = m.on(
        Event::Probed {
            purpose: Purpose::Ready,
            result: Ok(()),
        },
        &none(),
        t.now(),
    );
    assert_eq!(status(&acts), Some(Status::Healthy));
    assert!(has(&acts, &Action::ReleaseSlot) && has(&acts, &Action::ReloadBrowser));
}

#[test]
fn readiness_that_never_comes_leaves_the_process_running_unhealthy() {
    let mut t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        probed: true,
        ..cfg()
    });
    running(&mut m, &none(), &t);
    t.advance(ms(1000));
    let acts = m.on(
        Event::Probed {
            purpose: Purpose::Ready,
            result: Err("\"ready\" not printed yet".into()),
        },
        &none(),
        t.now(),
    );
    assert_eq!(
        logs(&acts),
        [
            "last readiness attempt: \"ready\" not printed yet",
            "readiness probe never passed"
        ]
    );
    assert_eq!(status(&acts), Some(Status::Unhealthy));
    assert!(has(
        &acts,
        &Action::SetReason("not ready within 1.0s".into())
    ));
    assert!(!has(&acts, &Action::ReloadBrowser));
    assert!(m.has_child(), "unhealthy, but still running");
}

#[test]
fn a_child_that_dies_during_startup_fails_readiness_then_backs_off() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        probed: true,
        ..cfg()
    });
    running(&mut m, &none(), &t);
    let acts = m.on(Event::Exited(3), &none(), t.now());
    let order: Vec<Status> = acts
        .iter()
        .filter_map(|a| match a {
            Action::SetStatus(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        [
            Status::Unhealthy,
            Status::Failed(3),
            Status::Backoff(ms(100))
        ]
    );
    assert!(!m.has_child());
}

#[test]
fn a_dependency_or_a_change_interrupts_startup_except_under_queue() {
    let t = Clock::new();
    for on_change in [OnChange::Restart, OnChange::Queue] {
        let mut m = Service::new(ServiceConfig {
            probed: true,
            on_change,
            ..cfg()
        });
        running(&mut m, &ready(1), &t);
        let acts = m.on(Event::Restart("a.rs".into()), &ready(1), t.now());
        if on_change == OnChange::Queue {
            assert!(acts.is_empty(), "queue protects a cold build from a change");
        } else {
            assert!(has(
                &acts,
                &Action::Log("restarting mid-startup: a.rs".into())
            ));
            continue;
        }
        // A dependency outranks even `queue`.
        let acts = m.on(Event::Board, &unready(1), t.now());
        assert!(has(
            &acts,
            &Action::Log("restarting mid-startup: dependency `db` is rebuilding".into())
        ));
        assert!(
            has(&acts, &Action::ReleaseSlot),
            "the build slot must come back"
        );
    }
}

#[test]
fn crash_backoff_doubles_and_is_capped() {
    let mut t = Clock::new();
    let mut m = Service::new(cfg());
    running(&mut m, &none(), &t);
    let mut delays = Vec::new();
    for _ in 0..6 {
        let acts = m.on(Event::Exited(1), &none(), t.now());
        let Some(Status::Backoff(d)) = status(&acts) else {
            panic!("expected a backoff, got {acts:?}");
        };
        assert!(has(&acts, &Action::SetTimer(Timer::Backoff, t.now() + d)));
        delays.push(d);
        t.advance(d);
        assert_eq!(
            m.on(Event::Timer(Timer::Backoff), &none(), t.now()),
            vec![Action::DrainControl]
        );
        respawn(&mut m, &none(), &t);
    }
    assert_eq!(
        delays,
        [ms(100), ms(200), ms(400), ms(800), ms(1000), ms(1000)]
    );
}

#[test]
fn a_stop_or_a_change_cuts_a_backoff_short() {
    let t = Clock::new();
    let mut m = Service::new(cfg());
    running(&mut m, &none(), &t);
    m.on(Event::Exited(1), &none(), t.now());
    let acts = m.on(Event::Restart("fix.rs".into()), &none(), t.now());
    assert_eq!(
        acts,
        vec![
            Action::CancelTimer(Timer::Backoff),
            Action::Log("restarting: fix.rs".into()),
            Action::DrainControl
        ]
    );
    m.on(Event::Drained { stop: false }, &none(), t.now());
    m.on(Event::SlotAcquired, &none(), t.now());
    m.on(Event::Spawned, &none(), t.now());
    // The change also reset the backoff: the next crash waits the minimum.
    assert_eq!(
        status(&m.on(Event::Exited(1), &none(), t.now())),
        Some(Status::Backoff(ms(100)))
    );
    assert_eq!(
        m.on(Event::Stop, &none(), t.now()),
        vec![
            Action::CancelTimer(Timer::Backoff),
            Action::SetStatus(Status::Stopped),
            Action::Exit
        ]
    );
}

#[test]
fn max_restarts_gives_up_and_a_change_starts_a_fresh_count() {
    // Cover for cairn 0096.
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        max_restarts: Some(2),
        ..cfg()
    });
    let crash = |m: &mut Service, first: bool| {
        if first {
            running(m, &none(), &t);
        } else {
            respawn(m, &none(), &t);
        }
        let acts = m.on(Event::Exited(3), &none(), t.now());
        if matches!(status(&acts), Some(Status::Backoff(_))) {
            m.on(Event::Timer(Timer::Backoff), &none(), t.now());
        }
        acts
    };
    crash(&mut m, true);
    crash(&mut m, false);
    let acts = crash(&mut m, false);
    let why = "gave up after 2 restart(s) in a row; waiting for a change or a manual restart";
    assert_eq!(logs(&acts), [why]);
    assert_eq!(status(&acts), Some(Status::Failed(3)));
    // Parked: nothing restarts it but a change or a request.
    assert!(m.on(Event::Board, &none(), t.now()).is_empty());
    let acts = m.on(Event::Restart("fixed.rs".into()), &none(), t.now());
    assert_eq!(logs(&acts), ["restarting: fixed.rs"]);
    m.on(Event::Drained { stop: false }, &none(), t.now());
    m.on(Event::SlotAcquired, &none(), t.now());
    m.on(Event::Spawned, &none(), t.now());
    m.on(Event::Exited(3), &none(), t.now());
    m.on(Event::Timer(Timer::Backoff), &none(), t.now());
    assert!(
        !logs(&crash(&mut m, false))
            .iter()
            .any(|l| l.starts_with("gave up")),
        "the change's attempt has the whole allowance again"
    );
}

#[test]
fn a_run_longer_than_backoff_max_is_not_part_of_a_crash_loop() {
    let mut t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        max_restarts: Some(1),
        ..cfg()
    });
    running(&mut m, &none(), &t);
    m.on(Event::Exited(1), &none(), t.now());
    m.on(Event::Timer(Timer::Backoff), &none(), t.now());
    respawn(&mut m, &none(), &t);
    // This one ran for longer than backoff_max: the count starts again.
    t.advance(ms(1500));
    let acts = m.on(Event::Exited(1), &none(), t.now());
    assert!(
        matches!(status(&acts), Some(Status::Backoff(_))),
        "{acts:?}"
    );
}

#[test]
fn under_restart_never_an_exit_parks_until_asked_again() {
    let t = Clock::new();
    for (code, expect) in [(0, Status::Exited), (2, Status::Failed(2))] {
        let mut m = Service::new(ServiceConfig {
            restart: RestartPolicy::Never,
            ..cfg()
        });
        running(&mut m, &ready(1), &t);
        let acts = m.on(Event::Exited(code), &ready(1), t.now());
        assert_eq!(acts, vec![Action::SetStatus(expect)]);
        assert!(m.on(Event::Board, &ready(1), t.now()).is_empty());
        let acts = m.on(Event::Board, &ready(2), t.now());
        assert_eq!(
            acts,
            vec![
                Action::Log("restarting: dependency `db` changed".into()),
                Action::DrainControl
            ]
        );
    }
}

#[test]
fn a_spawn_that_fails_backs_off_unless_restart_is_never() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        restart: RestartPolicy::Never,
        ..cfg()
    });
    m.on(Event::Begin, &none(), t.now());
    m.on(Event::Drained { stop: false }, &none(), t.now());
    m.on(Event::SlotAcquired, &none(), t.now());
    let fail = Action::Fail {
        error: "spawn failed: no such file".into(),
        code: 127,
    };
    assert_eq!(
        m.on(Event::SpawnFailed("no such file".into()), &none(), t.now()),
        vec![fail.clone(), Action::ReleaseSlot, Action::Exit]
    );

    let mut m = Service::new(cfg());
    m.on(Event::Begin, &none(), t.now());
    m.on(Event::Drained { stop: false }, &none(), t.now());
    m.on(Event::SlotAcquired, &none(), t.now());
    let acts = m.on(Event::SpawnFailed("no such file".into()), &none(), t.now());
    assert_eq!(
        acts,
        vec![
            fail,
            Action::ReleaseSlot,
            Action::SetTimer(Timer::Backoff, t.now() + ms(100))
        ],
        "a backoff that leaves the failure showing"
    );
    // That backoff used to be a sleep a shutdown had to wait out.
    assert_eq!(
        m.on(Event::Stop, &none(), t.now()),
        vec![
            Action::CancelTimer(Timer::Backoff),
            Action::SetStatus(Status::Stopped),
            Action::Exit
        ]
    );
}

#[test]
fn a_stop_while_running_stops_the_child_and_ends() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        liveness: Some((ms(100), 3)),
        ..cfg()
    });
    running(&mut m, &none(), &t);
    assert_eq!(
        m.on(Event::Stop, &none(), t.now()),
        vec![
            Action::CancelTimer(Timer::Liveness),
            Action::SetStatus(Status::Stopped),
            Action::StopChild { grace: ms(500) }
        ]
    );
    assert_eq!(
        m.on(Event::ChildStopped, &none(), t.now()),
        vec![Action::Exit]
    );
}

fn liveness_result(m: &mut Service, ok: bool, t: &Clock) -> Vec<Action> {
    assert_eq!(
        m.on(Event::Timer(Timer::Liveness), &none(), t.now()),
        vec![Action::Probe(Purpose::Liveness)]
    );
    m.on(
        Event::Probed {
            purpose: Purpose::Liveness,
            result: if ok { Ok(()) } else { Err("refused".into()) },
        },
        &none(),
        t.now(),
    )
}

#[test]
fn liveness_acts_on_the_nth_miss_in_a_row_and_restores_what_it_took() {
    // Cover for cairn 0092.
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        liveness: Some((ms(100), 3)),
        restart: RestartPolicy::OnChange,
        ..cfg()
    });
    let acts = running(&mut m, &none(), &t);
    assert!(has(
        &acts,
        &Action::SetTimer(Timer::Liveness, t.now() + ms(100))
    ));
    liveness_result(&mut m, false, &t);
    liveness_result(&mut m, false, &t);
    liveness_result(&mut m, true, &t); // one answer resets the count
    liveness_result(&mut m, false, &t);
    liveness_result(&mut m, false, &t);
    let acts = liveness_result(&mut m, false, &t);
    let why = "not answering (3 checks in a row): refused";
    assert_eq!(logs(&acts), [why]);
    assert_eq!(status(&acts), Some(Status::Unhealthy));
    assert!(
        m.has_child(),
        "without restart = always it is not restarted"
    );
    // A fourth miss does not act again.
    assert!(logs(&liveness_result(&mut m, false, &t)).is_empty());
    let acts = liveness_result(&mut m, true, &t);
    assert_eq!(logs(&acts), ["answering again"]);
    assert_eq!(status(&acts), Some(Status::Healthy));
}

#[test]
fn under_restart_always_liveness_restarts_instead() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        liveness: Some((ms(100), 2)),
        ..cfg()
    });
    running(&mut m, &none(), &t);
    liveness_result(&mut m, false, &t);
    let acts = liveness_result(&mut m, false, &t);
    assert!(has(
        &acts,
        &Action::CountRestart("not answering (2 checks in a row): refused".into())
    ));
    assert!(has(&acts, &Action::StopChild { grace: ms(500) }));
}

fn counters(reloads: u64, ready: u64, at_reload: u64, errors: usize) -> Event {
    Event::ReloadCounters(ReloadCounters {
        reload_matches: reloads,
        ready_matches: ready,
        ready_matches_at_reload: at_reload,
        errors,
    })
}

fn self_reloading(t: &Clock) -> Service {
    let mut m = Service::new(ServiceConfig {
        probed: true,
        reload: Reload::SelfManaged,
        follows_reloads: true,
        liveness: Some((ms(100), 2)),
        ..cfg()
    });
    running(&mut m, &none(), t);
    let acts = m.on(
        Event::Probed {
            purpose: Purpose::Ready,
            result: Ok(()),
        },
        &none(),
        t.now(),
    );
    assert!(has(&acts, &Action::ReadReloadCounters));
    // The first look: one ready line, no rebuild yet.
    m.on(counters(0, 1, 0, 0), &none(), t.now());
    m
}

fn probe_reload(m: &mut Service, ok: bool, t: &Clock) -> Vec<Action> {
    m.on(
        Event::Probed {
            purpose: Purpose::Reload,
            result: if ok { Ok(()) } else { Err("no".into()) },
        },
        &none(),
        t.now(),
    )
}

#[test]
fn a_self_reload_leaves_ready_and_comes_back_without_a_restart() {
    // Cover for cairn 0073.
    let t = Clock::new();
    let mut m = self_reloading(&t);
    let acts = m.on(counters(1, 1, 1, 0), &none(), t.now());
    assert_eq!(status(&acts), Some(Status::Reloading));
    assert!(has(&acts, &Action::Probe(Purpose::Reload)));
    assert!(
        probe_reload(&mut m, true, &t).is_empty(),
        "not ready again yet"
    );
    // Liveness does not ask a rebuilding process to answer.
    assert_eq!(
        m.on(Event::Timer(Timer::Liveness), &none(), t.now()),
        vec![Action::SetTimer(Timer::Liveness, t.now() + ms(100))]
    );
    m.on(counters(1, 2, 1, 0), &none(), t.now());
    let acts = probe_reload(&mut m, true, &t);
    assert_eq!(
        acts,
        vec![
            Action::SetStatus(Status::Healthy),
            Action::SetReason("reloaded itself".into())
        ],
        "and no browser reload: the process kept its state"
    );
    assert!(m.has_child());
}

#[test]
fn a_failing_self_reload_is_unhealthy_at_once_or_after_its_deadline() {
    let mut t = Clock::new();
    let mut m = self_reloading(&t);
    m.on(counters(1, 1, 1, 0), &none(), t.now());
    probe_reload(&mut m, true, &t);
    m.on(counters(1, 1, 1, 2), &none(), t.now());
    let acts = probe_reload(&mut m, true, &t);
    assert_eq!(logs(&acts), ["reload failed: rebuild failed with 2 errors"]);
    assert_eq!(status(&acts), Some(Status::Unhealthy));

    let mut m = self_reloading(&t);
    m.on(counters(1, 1, 1, 0), &none(), t.now());
    t.advance(ms(1000));
    m.on(counters(1, 1, 1, 0), &none(), t.now());
    let acts = probe_reload(&mut m, true, &t);
    assert_eq!(
        logs(&acts),
        ["reload failed: rebuild did not finish within 1.0s"]
    );
}

#[test]
fn a_rebuild_already_under_way_at_startup_is_not_missed() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        probed: true,
        reload: Reload::SelfManaged,
        follows_reloads: true,
        ..cfg()
    });
    running(&mut m, &none(), &t);
    m.on(
        Event::Probed {
            purpose: Purpose::Ready,
            result: Ok(()),
        },
        &none(),
        t.now(),
    );
    // A rebuild was announced after the last ready line.
    let acts = m.on(counters(2, 1, 1, 0), &none(), t.now());
    assert_eq!(status(&acts), Some(Status::Reloading));
}

#[test]
fn a_served_node_is_ready_while_parked_and_comes_back_for_its_dependencies() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        has_cmd: false,
        ..cfg()
    });
    m.on(Event::Begin, &ready(1), t.now());
    assert_eq!(
        m.on(Event::Drained { stop: false }, &ready(1), t.now()),
        vec![Action::SetStatus(Status::Healthy)]
    );
    assert!(m.on(Event::Board, &ready(1), t.now()).is_empty());
    assert_eq!(
        m.on(Event::Board, &ready(2), t.now()),
        vec![Action::DrainControl]
    );
}

#[test]
fn a_port_is_reclaimed_before_a_slot_is_taken() {
    let t = Clock::new();
    let mut m = Service::new(ServiceConfig {
        port: Some(8080),
        ..cfg()
    });
    m.on(Event::Begin, &none(), t.now());
    assert_eq!(
        m.on(Event::Drained { stop: false }, &none(), t.now()),
        vec![Action::ReclaimPort(8080)]
    );
    assert_eq!(
        m.on(Event::PortReclaimed, &none(), t.now()),
        vec![Action::AcquireSlot]
    );
}

#[test]
fn a_task_runs_parks_and_reruns_for_a_change_or_a_new_dependency_result() {
    let mut m = Task::new();
    assert_eq!(
        m.on(Event::Begin, &unready(0)),
        vec![Action::SetStatus(Status::Waiting("db".into()))]
    );
    assert_eq!(m.on(Event::Board, &ready(1)), vec![Action::DrainControl]);
    assert_eq!(
        m.on(Event::Drained { stop: false }, &ready(1)),
        vec![Action::SetStatus(Status::Running), Action::RunJob]
    );
    assert_eq!(
        m.on(
            Event::JobDone {
                code: 0,
                cached: false
            },
            &ready(1)
        ),
        vec![Action::ReloadBrowser]
    );
    assert!(
        m.on(Event::Board, &ready(1)).is_empty(),
        "same result, no rerun"
    );
    assert_eq!(
        m.on(Event::Board, &ready(2)),
        vec![
            Action::SetReason("dependency `db` changed".into()),
            Action::DrainControl
        ]
    );
    m.on(Event::Drained { stop: false }, &ready(2));
    // A cache hit tells no browser to reload: nothing it serves changed.
    assert!(
        m.on(
            Event::JobDone {
                code: 0,
                cached: true
            },
            &ready(2)
        )
        .is_empty()
    );
    assert_eq!(
        m.on(Event::Restart("src/a.rs".into()), &ready(2)),
        vec![Action::SetReason("src/a.rs".into()), Action::DrainControl]
    );
    m.on(Event::Drained { stop: false }, &ready(2));
    m.on(
        Event::JobDone {
            code: 1,
            cached: false,
        },
        &ready(2),
    );
    assert_eq!(m.on(Event::Stop, &ready(2)), vec![Action::Exit]);
    assert!(m.is_done());
}

#[test]
fn a_task_ignores_a_change_while_its_dependency_is_unready() {
    let mut m = Task::new();
    m.on(Event::Begin, &unready(0));
    assert_eq!(
        m.on(Event::Restart("a.rs".into()), &unready(0)),
        vec![Action::SetStatus(Status::Waiting("db".into()))]
    );
}

#[test]
fn thousands_of_random_events_never_leave_a_service_in_an_impossible_place() {
    // Not a property framework, a cheap stand-in: a fixed-seed walk through
    // every event in every phase, checking only what must always hold.
    let mut t = Clock::new();
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rand = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for _ in 0..200 {
        let mut m = Service::new(ServiceConfig {
            probed: rand(2) == 0,
            follows_reloads: rand(2) == 0,
            reload: if rand(2) == 0 {
                Reload::SelfManaged
            } else {
                Reload::Restart
            },
            liveness: (rand(2) == 0).then_some((ms(100), 2)),
            max_restarts: (rand(2) == 0).then_some(2),
            restart: [
                RestartPolicy::Always,
                RestartPolicy::OnChange,
                RestartPolicy::Never,
            ][rand(3) as usize],
            port: (rand(2) == 0).then_some(9),
            ..cfg()
        });
        let mut stopping = false;
        for step in 0..300 {
            let deps = if rand(4) == 0 {
                unready(rand(3))
            } else {
                ready(rand(3))
            };
            let ev = match rand(16) {
                0 => Event::Begin,
                1 | 2 => Event::Board,
                3 => Event::Restart("r".into()),
                4 => Event::Drained { stop: rand(8) == 0 },
                5 => Event::PortReclaimed,
                6 => Event::SlotAcquired,
                7 => Event::Spawned,
                8 => Event::SpawnFailed("x".into()),
                9 => Event::Exited(rand(3) as i32),
                10 => Event::ChildStopped,
                11 => Event::Timer(
                    [
                        Timer::Probe,
                        Timer::Backoff,
                        Timer::Liveness,
                        Timer::ReloadPoll,
                    ][rand(4) as usize],
                ),
                12 => Event::Probed {
                    purpose: [Purpose::Ready, Purpose::Reload, Purpose::Liveness][rand(3) as usize],
                    result: if rand(2) == 0 {
                        Ok(())
                    } else {
                        Err("e".into())
                    },
                },
                13 => counters(rand(3), rand(3), rand(2), rand(2) as usize),
                14 if step > 250 => {
                    stopping = true;
                    Event::Stop
                }
                _ => Event::Board,
            };
            let now = t.advance(ms(rand(300)));
            let acts = m.on(ev, &deps, now);
            let exits = acts.iter().filter(|a| **a == Action::Exit).count();
            assert!(exits <= 1, "two exits in one step: {acts:?}");
            assert_eq!(m.is_done(), exits == 1 || (m.is_done() && acts.is_empty()));
            if m.is_done() {
                assert!(acts.is_empty() || exits == 1);
                break;
            }
            // A child is only ever stopped while there is one to stop.
            if acts.iter().any(|a| matches!(a, Action::StopChild { .. })) {
                assert!(!m.has_child());
            }
        }
        let _ = stopping;
    }
}
