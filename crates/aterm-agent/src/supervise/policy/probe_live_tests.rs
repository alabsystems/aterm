// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live probe's looks under a bound: a disk that does not answer refuses, never approves.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::Duration;

use super::*;
use crate::supervise::policy::rm_breaker::{RmScope, ScratchRoot, resolve_rm_line};

/// The bound these tests wait a look for: the look behind a shut gate never ends.
const SHORT: Duration = Duration::from_millis(50);

/// A door every look on the thread that holds it passes first: shut, a look waits until it opens.
#[derive(Default)]
pub(crate) struct LookGate {
    open: Mutex<bool>,
    opened: Condvar,
    /// The looks started behind it, counted on the asking thread as each starts, before its
    /// helper runs.
    looks: AtomicUsize,
}

impl LookGate {
    pub(crate) fn shut() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn open(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.opened.notify_all();
    }

    fn pass(&self) {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let _open = self
            .opened
            .wait_while(open, |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
    }

    pub(crate) fn looks(&self) -> usize {
        self.looks.load(Ordering::SeqCst)
    }
}

/// Opens the gate as the test ends, passed or not, so no look is left waiting on it.
pub(crate) struct OpensAtEnd(pub(crate) Arc<LookGate>);

impl Drop for OpensAtEnd {
    fn drop(&mut self) {
        self.0.open();
    }
}

thread_local! {
    static GATE: RefCell<Option<Arc<LookGate>>> = const { RefCell::new(None) };
    static BOUND: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// What a look started on this thread passes first: the gate this thread holds, or nothing.
pub(super) fn gate_of_this_thread() -> impl FnOnce() + Send + 'static {
    let gate = GATE.with(|gate| gate.borrow().clone());
    if let Some(gate) = &gate {
        gate.looks.fetch_add(1, Ordering::SeqCst);
    }
    move || {
        if let Some(gate) = gate {
            gate.pass();
        }
    }
}

/// The bound a look asked on this thread is waited for, where its test set one.
pub(super) fn bound_of_this_thread() -> Option<Duration> {
    BOUND.with(Cell::get)
}

/// Every look this thread starts from now on passes `gate` and is waited `bound`, until the
/// returned guard drops.
pub(crate) fn hold_looks(gate: Arc<LookGate>, bound: Duration) -> impl Drop {
    /// Lets this thread's looks go as before.
    struct Held;
    impl Drop for Held {
        fn drop(&mut self) {
            GATE.with(|held| *held.borrow_mut() = None);
            BOUND.with(|held| held.set(None));
        }
    }
    GATE.with(|held| *held.borrow_mut() = Some(gate));
    BOUND.with(|held| held.set(Some(bound)));
    Held
}

/// Whether a look this thread left out has ended, waited for within the hang detector's minute.
pub(crate) fn left_out_ends() -> bool {
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || (!out_on_this_thread()).then_some(()),
    )
    .is_some()
}

/// `probe` on a thread of its own whose looks pass `gate` and are waited `bound`: its answer
/// within the hang detector's minute.
fn on_own_thread<T: Send + 'static>(
    gate: Option<Arc<LookGate>>,
    bound: Duration,
    probe: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (answered, answer) = mpsc::channel();
    std::thread::spawn(move || {
        GATE.with(|held| *held.borrow_mut() = gate);
        BOUND.with(|held| held.set(Some(bound)));
        let _ = answered.send(probe());
    });
    answer
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the probe answered within the hang detector's minute")
}

#[cfg(unix)]
fn comps_of(path: &Path) -> Vec<String> {
    abs_components(&path.to_string_lossy()).expect("an absolute path")
}

#[cfg(unix)]
fn real_dir(tree: &aterm_tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(tree.path()).expect("the test's temp dir resolves")
}

/// Every look that walks the disk, asked while the disk does not answer (the gate stays shut,
/// as a dead network mount's `lstat` never returns): each answers within its bound with what
/// refuses (an error, no resolution, or a shell that may stand anywhere), never an approval.
#[test]
fn a_look_the_disk_does_not_answer_refuses_what_it_would_have_proved() {
    let gate = LookGate::shut();
    let _opens = OpensAtEnd(Arc::clone(&gate));
    let worker = WorkerEnv::from_env(vec![
        "HOME=/nonexistent/aterm-probe-home".to_string(),
        "PATH=/usr/bin:/bin".to_string(),
    ]);
    let comps = vec!["private".to_string(), "tmp".to_string()];
    let (physical, real, twin, view, env, startup, asks, cwds) =
        on_own_thread(Some(Arc::clone(&gate)), SHORT, move || {
            let probe = LiveProbe;
            let launch = Path::new("/private/tmp");
            let writes = WorkerWrites {
                cwd: launch,
                shell_cwds: &[],
                scratch_roots: &[],
            };
            (
                probe.rm_physical(&comps),
                probe.real_components(&comps),
                probe.hidden_twin_in(&comps, None),
                probe.git_view(&worker, launch, &writes),
                probe.settings_env(&worker, launch),
                probe.shell_startup(&worker, launch),
                probe.owner_asks(&worker, launch),
                probe.shell_cwds(&worker, launch),
            )
        });
    assert!(
        physical
            .as_ref()
            .is_err_and(|why| why.contains("did not answer")),
        "{physical:?}"
    );
    assert_eq!(real, None, "no resolution, so the Read is refused");
    assert!(twin.is_err(), "{twin:?}");
    assert!(view.is_err(), "{view:?}");
    assert!(env.is_err(), "{env:?}");
    assert!(startup.is_err(), "{startup:?}");
    assert!(asks.is_err(), "{asks:?}");
    assert_eq!(
        cwds,
        [PathBuf::from("/")],
        "a shell that may stand anywhere: every place counts as the worker's to write"
    );
}

/// One look left out past its bound refuses the loop's next look at once, starting none (one
/// volume that does not answer costs the loop one bound, not one per look), and once it ends
/// the loop looks again and uses the answer; the late answer itself lands nowhere.
#[test]
fn a_look_left_out_refuses_the_next_at_once_until_it_ends() {
    let gate = LookGate::shut();
    let _opens = OpensAtEnd(Arc::clone(&gate));
    #[cfg(unix)]
    let (_tree, comps) = {
        let tree = aterm_tempfile::tempdir().expect("the test's temp dir");
        let dir = real_dir(&tree).join("a");
        std::fs::create_dir(&dir).expect("a folder");
        (tree, comps_of(&dir))
    };
    #[cfg(not(unix))]
    let comps = vec!["aterm-nonexistent-probe-dir".to_string()];
    let expected = comps.clone();
    let held = Arc::clone(&gate);
    let (first, second, looks_while_out, third) = on_own_thread(Some(gate), SHORT, move || {
        let first = LiveProbe.rm_physical(&comps);
        let second = LiveProbe.rm_physical(&comps);
        let looks_while_out = held.looks();
        let (out, unanswered) = (LiveProbe.look_out(), LiveProbe.unanswered());
        held.open();
        assert!(
            left_out_ends(),
            "the look left out ended once its gate opened"
        );
        assert!(out && !LiveProbe.look_out(), "look_out says so");
        assert_eq!(unanswered, 2, "both refusals counted unanswered");
        // The look that answers is waited the shipped bound: no latency budget.
        BOUND.with(|bound| bound.set(None));
        let third = LiveProbe.rm_physical(&comps);
        assert_eq!(LiveProbe.unanswered(), 2, "an answered look counts none");
        (first, second, looks_while_out, third)
    });
    assert!(
        first
            .as_ref()
            .is_err_and(|why| why.contains("did not answer")),
        "{first:?}"
    );
    assert!(
        second
            .as_ref()
            .is_err_and(|why| why.contains("has not answered")),
        "{second:?}"
    );
    assert_eq!(looks_while_out, 1, "the refusal started no second look");
    assert_eq!(
        third.map(|on_disk| on_disk.comps),
        Ok(expected),
        "the loop looks again once the look left out ends"
    );
}

/// The rm circuit breaker's whole proof through the live probe: with the disk answering, an rm
/// inside a scratch root resolves where the disk leads it (the negative control); with the disk
/// not answering, the same line escalates within the bound instead of hanging the loop.
#[cfg(unix)]
#[test]
fn an_rm_escalates_when_the_disk_does_not_answer_and_resolves_when_it_does() {
    let tree = aterm_tempfile::tempdir().expect("the test's temp dir");
    let root = real_dir(&tree);
    std::fs::create_dir(root.join("out")).expect("the rm's folder");
    std::fs::create_dir(root.join("work")).expect("the session's folder");
    let line = format!("rm -rf {}/out", root.display());
    let resolve = move |root: PathBuf, line: String| {
        move || {
            let roots: Vec<ScratchRoot> = ScratchRoot::dir(&root).into_iter().collect();
            let cwd = root.join("work");
            resolve_rm_line(
                &line,
                &RmScope {
                    cwd: &cwd,
                    home: Some(Path::new("/Users/_owner")),
                    roots: &roots,
                    probe: &LiveProbe,
                },
            )
        }
    };
    let answered = on_own_thread(None, LOOK_BOUND, resolve(root.clone(), line.clone()));
    assert_eq!(
        answered,
        Ok(vec![format!("{}/out", root.display())]),
        "the negative control: a disk that answers proves the rm"
    );
    let gate = LookGate::shut();
    let _opens = OpensAtEnd(Arc::clone(&gate));
    let refused = on_own_thread(Some(gate), SHORT, resolve(root, line));
    assert!(
        refused.as_ref().is_err_and(|why| why.contains("answer")),
        "{refused:?}"
    );
}

#[cfg(not(unix))]
#[test]
fn an_rm_on_a_non_unix_temp_root_has_no_unix_scratch_root() {
    let tree = aterm_tempfile::tempdir().expect("the test's temp dir");
    assert!(
        ScratchRoot::dir(tree.path()).is_none(),
        "ScratchRoot::dir proves only /-rooted Unix paths"
    );
    let refused = resolve_rm_line(
        "rm -rf /tmp/out",
        &RmScope {
            cwd: tree.path(),
            home: None,
            roots: &[],
            probe: &LiveProbe,
        },
    );
    assert!(refused.is_err(), "{refused:?}");
}
