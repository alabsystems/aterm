// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::restore::{
    RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TerminalLeafRestore,
    WindowLayout,
};
#[cfg(unix)]
use crate::session_store::SessionRecord;
use std::sync::{Mutex, PoisonError};

/// The actual handoff JSON preserves saved empty title slots, while a carry
/// without the presence field retains its legacy interpretation.
#[test]
fn carried_empty_title_slots_survive_the_handoff_wire() {
    use aterm_core::terminal::{CheckpointMeta, Terminal, TitleRepr};
    let titles = |terminal: &Terminal| {
        let title = terminal.checkpoint_carry(0).expect("Ground").title.unwrap();
        (title.window, title.icon)
    };

    for (prefix, tail) in [
        (
            &b"\x1b]0;caller\x07\x1b[22t\x1b]0;program\x07"[..],
            &b"\x1b[23t"[..],
        ),
        (b"\x1b[22t\x1b]0;program\x07", b"\x1b[23t"),
        (
            b"\x1b]1;icon\x07\x1b[22;2t\x1b]0;program\x07",
            b"\x1b[23;2t",
        ),
        (
            b"\x1b]2;window\x07\x1b[22;1t\x1b]0;program\x07",
            b"\x1b[23;1t",
        ),
        (
            b"\x1b]1;icon\x07\x1b[22;2t\x1b[22;1t\x1b]0;program\x07",
            b"\x1b[23;2t",
        ),
    ] {
        let mut live = Terminal::new(4, 20);
        live.process(prefix);
        let carry = live.checkpoint_carry(0).expect("Ground");
        let meta = CheckpointMeta::from_checkpoint(&carry);
        let bytes = aterm_json::to_vec(&meta).unwrap();
        let back: CheckpointMeta = aterm_json::from_slice(&bytes).unwrap();
        assert_eq!(back, meta);
        let mut adopted = Terminal::from_checkpoint(
            &back.into_checkpoint(carry.grid.clone(), carry.alt_grid.clone()),
        );
        live.process(tail);
        adopted.process(tail);
        assert_eq!(
            titles(&adopted),
            titles(&live),
            "{}",
            String::from_utf8_lossy(prefix)
        );
    }

    let legacy: TitleRepr = aterm_json::from_str(
        r#"{"window":"program","icon":"current icon","stack":[["","caller"]]}"#,
    )
    .unwrap();
    assert!(legacy.stack_saved.is_empty());
    let mut terminal = Terminal::new(4, 20);
    terminal.restore_title(&legacy);
    terminal.process(b"\x1b[23t");
    assert_eq!(
        titles(&terminal),
        ("caller".to_string(), "current icon".to_string())
    );

    // Presence does not bypass title sanitation or the stack bound. An explicit
    // false slot stays absent, even when its untrusted string is nonempty.
    let hostile = TitleRepr {
        window: "program".into(),
        icon: "current icon".into(),
        stack: vec![("discard me".repeat(1024), "\x07\u{202e}".into()); 64],
        stack_saved: vec![(false, true); 65],
    };
    terminal.restore_title(&hostile);
    let carried = terminal.checkpoint_carry(0).unwrap().title.unwrap();
    assert!(carried.stack.len() < hostile.stack.len());
    assert_eq!(carried.stack_saved.len(), carried.stack.len());
    assert!(
        carried
            .stack
            .iter()
            .all(|(icon, window)| icon.is_empty() && window.is_empty())
    );
    terminal.process(b"\x1b[23t");
    assert_eq!(
        titles(&terminal),
        ("".to_string(), "current icon".to_string())
    );

    // A short mask supplies no new meaning for the remaining legacy entries.
    let partial: TitleRepr = aterm_json::from_str(
        r#"{"window":"program","icon":"current icon","stack":[["",""],["","caller"]],"stack_saved":[[true,false]]}"#,
    ).unwrap();
    terminal.restore_title(&partial);
    terminal.process(b"\x1b[23t");
    assert_eq!(titles(&terminal), ("caller".to_string(), "".to_string()));
}

/// Held by a test that writes the handoff variables, `HOME` or `XDG_RUNTIME_DIR`, in its own
/// child ([`env_alone`]): the lib-test process writes none of them.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// For a test that writes the process environment: runs it alone in a child
/// ([`crate::test_alone::alone`]), where it holds [`ENV_LOCK`], so the lib-test
/// process's environment is never written. `None` in the lib-test process.
pub(crate) fn env_alone() -> Option<std::sync::MutexGuard<'static, ()>> {
    crate::test_alone::alone().then(|| ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner))
}

/// RAII save/RESTORE of one env var: puts the prior value BACK on drop, unwind included.
/// Only in a test's own child ([`env_alone`]), which `new` asserts.
pub(super) struct RestoreVar {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}
impl RestoreVar {
    pub(super) fn new(key: impl Into<&'static str>) -> Self {
        let key = key.into();
        assert!(
            crate::test_alone::in_child(),
            "{key}: a test that writes the environment runs alone in a child (`env_alone`)"
        );
        Self {
            key,
            prior: std::env::var_os(key),
        }
    }
}
impl Drop for RestoreVar {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => aterm_log::env::set(self.key, v),
            None => aterm_log::env::unset(self.key),
        }
    }
}

/// Every variable a handoff test sets, restored when the test ends. In a test's own child, as
/// for [`RestoreVar`].
#[cfg(unix)]
pub(super) fn restore_env() -> [RestoreVar; 11] {
    [
        RestoreVar::new("XDG_RUNTIME_DIR"),
        RestoreVar::new("HOME"),
        RestoreVar::new(ENV_MANIFEST),
        RestoreVar::new(ENV_NONCE),
        RestoreVar::new(ENV_FDS),
        RestoreVar::new(ENV_LAYOUT),
        RestoreVar::new(ENV_TARGET),
        RestoreVar::new(ENV_READY_FD),
        RestoreVar::new(ENV_COMMIT_FD),
        RestoreVar::new(ENV_PARENT_PID),
        RestoreVar::new(ENV_PARENT_BIRTH),
    ]
}

/// A handoff test's private runtime dir — and, for [`ScratchEnv::home`],
/// its HOME: `<this process's handoff scratch root>/<label>`, named by the
/// process-wide variables until the guard drops. The drop puts the
/// variables BACK FIRST and only then removes the directory, which lives in
/// [`crate::test_scratch::process_root`] so a thread that re-creates under it
/// after the removal is taken back when the process exits.
///
/// In a test's own child, as for [`RestoreVar`], and one at a time: the guard
/// restores what it found.
#[cfg(unix)]
pub(crate) struct ScratchEnv {
    path: std::path::PathBuf,
    restore: Vec<RestoreVar>,
}

#[cfg(unix)]
impl ScratchEnv {
    /// `XDG_RUNTIME_DIR` alone: the control dir moves, HOME stays.
    pub(crate) fn runtime(label: &str) -> Self {
        Self::enter(label, &["XDG_RUNTIME_DIR"])
    }

    /// `XDG_RUNTIME_DIR` and `HOME`: for a test that builds anything that
    /// resolves the home (an App, the manifest writer's sidecars).
    pub(crate) fn home(label: &str) -> Self {
        Self::enter(label, &["XDG_RUNTIME_DIR", "HOME"])
    }

    fn enter(label: &str, vars: &[&'static str]) -> Self {
        let path = crate::test_scratch::process_root("aterm-hs-").join(label);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch");
        let restore = vars
            .iter()
            .map(|var| {
                let restore = RestoreVar::new(*var);
                aterm_log::env::set(var, &path);
                restore
            })
            .collect();
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        std::fs::create_dir_all(dir).expect("control dir");
        Self { path, restore }
    }
}

#[cfg(unix)]
impl Drop for ScratchEnv {
    fn drop(&mut self) {
        self.restore.clear();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Run the calling test alone in `children` side-by-side children ([`env_alone`]'s kind),
/// `marker` naming each child's index (0-based), and return each passed child's stdout.
#[cfg(unix)]
pub(crate) fn run_in_children(marker: &str, children: usize) -> Vec<String> {
    // A failed spawn panics here; the children already running are dropped with it, and so
    // killed and reaped.
    let spawned: Vec<crate::test_alone::Running> = (0..children)
        .map(|index| {
            let index = std::ffi::OsString::from(index.to_string());
            crate::test_alone::spawn_alone_with(&[(marker, &index)])
        })
        .collect();
    spawned
        .into_iter()
        .map(|child| child.passed().stdout)
        .collect()
}

/// Exit 75 needs all four facts [`passing_refusal_exit`] names, for each boot apply that
/// passed no verdict on the stage (a passing deferral, `NoUpdate`, `NotApplicable`,
/// another install's trial); a verdict on the stage, or `ReExecFailed`, keeps the
/// ordinary refusal's `0`.
#[cfg(unix)]
#[test]
fn a_target_refused_after_a_passing_deferral_exits_75() {
    use aterm_update::ApplyOutcome;
    let lock = ApplyOutcome::Deferred(format!(
        "lock: another process has held the update lock for more than 10s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    ));
    let trial = ApplyOutcome::Deferred(format!(
        "update trial for build 12 is still unconfirmed ({})",
        aterm_update::PASSING_REFUSAL_KEY
    ));
    // The status a parent of another build reads: a cross-version contract.
    assert_eq!(EXIT_SUCCESSOR_PASSING, 75);
    for outcome in [
        lock,
        ApplyOutcome::NoUpdate,
        ApplyOutcome::NotApplicable,
        trial,
    ] {
        assert_eq!(
            passing_refusal_exit(&outcome, false, false, true),
            Some(EXIT_SUCCESSOR_PASSING),
            "{outcome:?}"
        );
        // The same outcome, but the target was admitted or IS this build:
        // nothing was refused because of it. And a parent that never said
        // it reads 75 is owed the ending it has always filed.
        assert_eq!(passing_refusal_exit(&outcome, false, true, true), None);
        assert_eq!(passing_refusal_exit(&outcome, true, false, true), None);
        assert_eq!(passing_refusal_exit(&outcome, false, false, false), None);
    }
    // The boot apply's word on the stage keeps the ordinary refusal.
    for outcome in [
        ApplyOutcome::Deferred("install location not writable: /Applications".to_string()),
        ApplyOutcome::Deferred(
            "re-verify: bundle policy: codesign --verify (structural) failed".to_string(),
        ),
        ApplyOutcome::ReExecFailed(aterm_update::PASSING_REFUSAL_KEY.to_string()),
        ApplyOutcome::Reverted(aterm_update::PASSING_REFUSAL_KEY.to_string()),
    ] {
        assert_eq!(
            passing_refusal_exit(&outcome, false, false, true),
            None,
            "{outcome:?}"
        );
    }
}

/// A successor whose intake refused every session exits rather than booting fresh over
/// the parked parent's sessions. The claim was granted, so the descriptor list and both
/// overlap channels are in the environment; `take_incoming` then refuses the whole
/// handoff (here, a missing manifest), and the readiness channel is refused for want of
/// its nonce.
#[test]
#[cfg(unix)]
fn a_refused_intake_of_an_offered_handoff_exits_instead_of_booting_fresh() {
    let Some(_env) = env_alone() else { return };
    let _restore_fds = RestoreVar::new(ENV_FDS);
    let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
    let _restore_nonce = RestoreVar::new(ENV_NONCE);
    let _restore_layout = RestoreVar::new(ENV_LAYOUT);
    // The claimed shape: a descriptor list, a nonce and a manifest path
    // whose file is not there (any whole-handoff refusal will do).
    aterm_log::env::set(ENV_FDS, "0=-1:1");
    aterm_log::env::set(ENV_NONCE, "0123456789abcdef0123456789abcdef");
    aterm_log::env::set(ENV_MANIFEST, "/nonexistent/handoff.toml");
    let offered = incoming_offered();
    let incoming = take_incoming();
    assert!(offered, "the grant published the descriptor list");
    assert!(incoming.adopted.is_empty() && incoming.nonce.is_none());
    assert!(!incoming_offered(), "take_incoming consumed the list");
    // The readiness channel is refused without a nonce: degraded.
    assert!(
        overlap_intake_exits(true, incoming.adopted.len(), offered),
        "an offered handoff the intake refused must exit, not boot fresh"
    );
    // Adopted sessions under incomplete authority exit…
    assert!(overlap_intake_exits(true, 3, false));
    // …an admitted overlap boots its adoption…
    assert!(!overlap_intake_exits(false, 3, true));
    // …and a stray channel variable with nothing offered boots fresh.
    assert!(!overlap_intake_exits(true, 0, false));
    // An unreadable manifest is the PRODUCER's content (review round two of
    // item 18): `no_shipped_parent_is_sent_75_for_a_refused_intake` is how
    // each parent is left.
    assert_eq!(incoming.refusal, Some(WholeRefusal::Producer));
}

/// 0.98.0's reading of a candidate's death, transcribed from its
/// `PhysicalFailureShape::of_child_death`: it declares no exit capability and reads no
/// exit code. Its reject path keeps the counted trial launch only for a `Structural` death.
#[cfg(unix)]
fn shipped_parent_reads(
    death: crate::ChildDeathEvidence,
) -> crate::app_native::PhysicalFailureShape {
    use crate::ChildDeathEvidence as Death;
    use crate::app_native::PhysicalFailureShape as Shape;
    match death {
        Death::Unobserved => Shape::Unexplained,
        Death::Exited { .. } => Shape::Structural,
        Death::Signalled { signal } => {
            if matches!(
                signal,
                libc::SIGILL
                    | libc::SIGTRAP
                    | libc::SIGABRT
                    | libc::SIGFPE
                    | libc::SIGBUS
                    | libc::SIGSEGV
                    | libc::SIGSYS
            ) {
                Shape::Structural
            } else {
                Shape::Transient
            }
        }
    }
}

/// The death a parent observes of a candidate that leaves as `leave` says: its own exit
/// status, or, for a candidate that waits to be ended, the parent's own `SIGKILL` of a
/// live process, which no parent can tell from anyone else's and records as `Unobserved`
/// (`app_update_handoff::handoff_child_death`).
#[cfg(unix)]
fn death_a_parent_observes(leave: RefusedIntakeLeave) -> crate::ChildDeathEvidence {
    match leave {
        RefusedIntakeLeave::Passing => crate::ChildDeathEvidence::Exited {
            code: EXIT_SUCCESSOR_PASSING,
        },
        RefusedIntakeLeave::Ordinary => crate::ChildDeathEvidence::Exited { code: 0 },
        RefusedIntakeLeave::ParentEndsIt => crate::ChildDeathEvidence::Unobserved,
    }
}

/// No parent that did not declare the exit capability is sent 75, and none blames the build
/// for its own content: with nothing adopted and no declared capability the candidate
/// leaves the ending to the parent, which 0.98.0 files `Unexplained` and forgives. A
/// declaring parent gets 75 (transient) for the producer's content and `0` for this
/// build's own verdict. Negative control: 0.98.0 reads 75 as structural and keeps the
/// counted launch.
#[cfg(unix)]
#[test]
fn no_shipped_parent_is_sent_75_for_a_refused_intake() {
    use crate::app_native::PhysicalFailureShape as Shape;
    let refusals = [Some(WholeRefusal::Producer), Some(WholeRefusal::Own), None];
    for refusal in refusals {
        let leave = refused_intake_leave(0, refusal, false);
        assert_eq!(leave, RefusedIntakeLeave::ParentEndsIt, "{refusal:?}");
        assert!(leave.gives_back_trial_launch(), "{refusal:?}");
        let death = death_a_parent_observes(leave);
        assert_ne!(
            shipped_parent_reads(death),
            Shape::Structural,
            "{refusal:?}: 0.98.0 keeps the counted launch only for a structural death"
        );
        assert_eq!(
            crate::app_native::PhysicalFailureShape::of_child_death(death),
            shipped_parent_reads(death),
            "{refusal:?}: this build reads the same death the same way"
        );
    }
    // To a parent that declared it reads 75.
    assert_eq!(
        refused_intake_leave(0, Some(WholeRefusal::Producer), true),
        RefusedIntakeLeave::Passing
    );
    assert_eq!(
        crate::app_native::PhysicalFailureShape::of_child_death(death_a_parent_observes(
            RefusedIntakeLeave::Passing
        )),
        Shape::Transient,
        "the declaring parent reads 75 as a moment"
    );
    assert!(RefusedIntakeLeave::Passing.gives_back_trial_launch());
    for refusal in [Some(WholeRefusal::Own), None] {
        let leave = refused_intake_leave(0, refusal, true);
        assert_eq!(leave, RefusedIntakeLeave::Ordinary, "{refusal:?}");
        assert!(!leave.gives_back_trial_launch(), "{refusal:?}");
    }
    // Adopted sessions: the exit every build takes, whatever the parent.
    for refusal in refusals {
        for declared in [false, true] {
            assert_eq!(
                refused_intake_leave(2, refusal, declared),
                RefusedIntakeLeave::Ordinary,
                "{refusal:?} declared={declared}"
            );
        }
    }
    // Negative control: 75 to any parent.
    assert_eq!(
        shipped_parent_reads(crate::ChildDeathEvidence::Exited {
            code: EXIT_SUCCESSOR_PASSING
        }),
        Shape::Structural,
        "0.98.0 files 75 structural and keeps the launch"
    );
}

/// THE CAPABILITY IS DECLARED BESIDE THE READER THAT HONOURS IT, and reaches
/// the new image (round eight, finding 0). The parent publishes it in the one
/// list both handoff lanes publish ([`outgoing_parent_env`]); the snapshot
/// carries it across the boot apply's re-exec; the successor takes it out
/// before any shell; and this build's reader files 75 transient — so the
/// declaration can never ship without the reading it promises.
#[cfg(unix)]
#[test]
fn the_exit_capability_is_declared_on_both_lanes_and_taken_by_the_successor() {
    let published = outgoing_parent_env();
    assert!(
        published.contains(&ENV_EXIT_CAPS.pair(EXIT_CAPS_PASSING)),
        "the parent declares it: {published:?}"
    );
    assert_eq!(
        crate::app_native::PhysicalFailureShape::of_child_death(
            crate::ChildDeathEvidence::Exited {
                code: EXIT_SUCCESSOR_PASSING
            }
        ),
        crate::app_native::PhysicalFailureShape::Transient,
        "and reads what it declares"
    );
    // The successor: carried across the re-exec, then taken.
    let mut env = HandoffEnv::default();
    for (key, value) in &published {
        let name = crate::handoff_env::HANDOFF_ENV_KEYS
            .iter()
            .find(|name| name.name() == *key)
            .expect("a published name is a handoff name");
        env.set(*name, value.clone());
    }
    assert!(
        env.exec_env()
            .iter()
            .any(|(key, _)| key.as_os_str() == ENV_EXIT_CAPS.name()),
        "the boot apply's re-exec carries it onto the new image"
    );
    assert!(take_parent_reads_passing_exit_from(&mut env));
    assert!(!env.present(ENV_EXIT_CAPS), "taken, so no shell sees it");
    assert!(
        !take_parent_reads_passing_exit_from(&mut env),
        "a shipped parent never declared it"
    );
    for (caps, reads) in [
        ("passing75", true),
        ("chunks1, passing75", true),
        ("passing7", false),
        ("", false),
    ] {
        env.set(ENV_EXIT_CAPS, caps);
        assert_eq!(
            take_parent_reads_passing_exit_from(&mut env),
            reads,
            "{caps:?}"
        );
    }
}

/// The refusing candidate waits for its parent, bounded: a gone or unattested parent
/// ends the wait at once, a live one holds it to the bound and no longer. (macOS: the
/// attestations come from the birth record.)
#[cfg(target_os = "macos")]
#[test]
fn awaiting_the_parents_end_is_bounded_and_ends_with_the_parent() {
    // The product's bound is three minutes: a wait that sat it out instead of ending
    // at once fails the one-minute hang detector below.
    let started = std::time::Instant::now();
    assert_eq!(
        await_parents_end(None, PARENTS_END_BOUND),
        ParentsEnd::ParentGone
    );
    let gone = AttestedParent::recycled_for_test(
        libc::pid_t::try_from(std::process::id()).expect("a pid"),
    );
    assert_eq!(
        await_parents_end(Some(gone), PARENTS_END_BOUND),
        ParentsEnd::ParentGone
    );
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "a gone parent ends the wait at once, not at the bound"
    );
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let live = AttestedParent::attest_live_for_test(own).expect("this process attests");
    assert_eq!(
        await_parents_end(Some(live), std::time::Duration::from_millis(10)),
        ParentsEnd::BoundPassed
    );
}

/// A refused intake of the producer's content gives back its counted trial launch. The
/// manifest's nonce line names another attempt, so the real intake refuses it whole as
/// the producer's; on either leave that is no verdict on the bytes (`Passing`,
/// `ParentEndsIt`) the give-back restores the boot sentinel's pre-boot count.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_intake_of_the_producers_content_gives_back_its_counted_trial_launch() {
    // A build number no other test arms or runs as.
    const BUILD: u64 = 9_800_000_000_018;
    crate::prime_test_update_root();
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    // The intake: a manifest whose nonce line names another attempt.
    let staged = stage_outgoing_handoff("r8nonce", 2, None);
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (_, toml) = body.split_once('\n').expect("nonce header");
    std::fs::write(&staged.manifest_path, format!("{}\n{toml}", "f".repeat(32)))
        .expect("republish under another attempt's nonce");
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, None);
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(incoming.adopted.is_empty() && incoming.nonce.is_none());
    assert_eq!(incoming.refusal, Some(WholeRefusal::Producer));

    // The boot sentinel in the suite's scratch update root, armed for
    // BUILD, with this process's launch counted by name.
    let _ = aterm_update::trial_launch_count(BUILD); // lays the root out
    let root = aterm_update_core::seal_guard::updates_root().expect("the scratch update root");
    let sentinel = aterm_update_core::Sentinel::new(root.join("boot.sentinel"));
    let birth =
        aterm_uds::process::read_process_birth(i32::try_from(std::process::id()).expect("a pid"))
            .expect("this process's kernel birth record");
    let this_launch = aterm_update_core::LaunchName {
        pid: std::process::id(),
        started: (
            i64::try_from(birth.seconds).expect("seconds"),
            i64::try_from(birth.microseconds).expect("micros"),
        ),
    };
    for declared in [true, false] {
        sentinel.arm(BUILD).expect("arm the trial");
        let before_boot = aterm_update::trial_launch_count(BUILD);
        sentinel
            .observe_launch_by(BUILD, Some(this_launch))
            .expect("the boot counts this launch");
        assert_eq!(aterm_update::trial_launch_count(BUILD), before_boot + 1);

        let leave = refused_intake_leave(0, incoming.refusal, declared);
        assert_ne!(leave, RefusedIntakeLeave::Ordinary, "declared={declared}");
        assert!(
            give_back_refused_intake_launch(leave, BUILD),
            "declared={declared}: the launch is given back"
        );
        assert_eq!(
            aterm_update::trial_launch_count(BUILD),
            before_boot,
            "declared={declared}: back to its pre-boot value"
        );
        assert!(
            !give_back_refused_intake_launch(leave, BUILD),
            "declared={declared}: and only once"
        );
    }
    // This build's own verdict, to a parent that reads 75: the parent's
    // rule decides, so nothing is given back here.
    sentinel.arm(BUILD).expect("arm the trial");
    sentinel
        .observe_launch_by(BUILD, Some(this_launch))
        .expect("count");
    assert!(!give_back_refused_intake_launch(
        refused_intake_leave(0, Some(WholeRefusal::Own), true),
        BUILD
    ));
    assert_eq!(aterm_update::trial_launch_count(BUILD), 1);
    sentinel.confirm().expect("disarm the scratch trial");

    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// A pipe whose both ends are close-on-exec from birth, as `(read, write)`.
///
/// The flag must come from the creating syscall: with `pipe` + `F_SETFD`, a `posix_spawn`
/// on another test thread can inherit both ends, and a duplicate writer keeps the read end
/// from ever reaching EOF, the witness every handoff-close test here rests on. Where
/// `pipe2` is missing, [`assert_peer_reaches_eof`]'s bound outlives an inherited duplicate.
#[cfg(unix)]
fn cloexec_pipe() -> (i32, i32) {
    let mut fds = [-1i32; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        assert_eq!(
            // SAFETY: `pipe2` fills the two-element array this frame owns.
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "pipe2"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        assert_eq!(
            // SAFETY: `pipe` fills the two-element array this frame owns.
            unsafe { libc::pipe(fds.as_mut_ptr()) },
            0,
            "pipe"
        );
        for fd in fds {
            aterm_pty::set_cloexec(fd, true).expect("pipe end cloexec");
        }
    }
    (fds[0], fds[1])
}

/// Park a descriptor at a number no sibling test thread can be handed, and answer with
/// that number: the kernel hands out the lowest free descriptor, so a slot above the
/// suite's working set is the caller's while it holds it.
///
/// The floor is the live `RLIMIT_NOFILE` less headroom, capped at [`ISOLATED_FD_FLOOR`]. A
/// slot just under the ceiling is unreachable by construction: a sibling gets it only once
/// every lower number is taken, when the caller's own `pipe` would already fail.
/// The claim is `F_DUPFD_CLOEXEC`, never `dup2`, so it never replaces a neighbour's
/// descriptor. `None` leaves the caller on an unisolated number, which stays sound
/// because the assertions it guards are about identity.
#[cfg(unix)]
fn claim_isolated_fd_slot(fd: i32) -> Option<i32> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` only fills the `rlimit` this frame owns.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    // `RLIM_INFINITY` and any ceiling past the descriptor type both mean
    // "the cap below decides", never "no isolated region exists".
    let ceiling = i32::try_from(limit.rlim_cur).unwrap_or(i32::MAX);
    // Leave headroom above the floor: the caller reclaims the same number
    // after the close under test, and `F_DUPFD` refuses a floor the ceiling
    // does not admit.
    let floor = ceiling.checked_sub(8)?.min(ISOLATED_FD_FLOOR);
    // Below stdio there is no isolated region to claim, only stdin.
    if floor < 3 {
        return None;
    }
    // SAFETY: `fd` is a live descriptor the caller owns; F_DUPFD_CLOEXEC
    // only duplicates it.
    let claimed = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, floor) };
    (claimed >= floor).then_some(claimed)
}

/// The cap on [`claim_isolated_fd_slot`]'s floor: high enough that no concurrent test in
/// this binary reaches it (the full suite peaks under 600 open descriptors), low enough
/// that claiming it leaves the process fd table small.
#[cfg(unix)]
const ISOLATED_FD_FLOOR: i32 = 4096;

/// Arm a pipe read endpoint as the witness for its writer's close, and prove the witness
/// can still say no: while the named writer is live the empty peer is merely pending, so
/// an EOF observed later can only be the close under test.
#[cfg(unix)]
fn arm_peer_eof_witness(peer: i32) {
    // SAFETY: `peer` is a live descriptor; F_GETFL only reads status flags.
    let flags = unsafe { libc::fcntl(peer, libc::F_GETFL) };
    assert!(flags >= 0, "read peer flags");
    assert_eq!(
        // SAFETY: same descriptor, setting the status word we just read.
        unsafe { libc::fcntl(peer, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "read peer nonblocking"
    );
    let mut byte = [0u8; 1];
    assert_eq!(
        // SAFETY: a 1-byte read into a stack buffer of that length.
        unsafe { libc::read(peer, byte.as_mut_ptr().cast(), byte.len()) },
        -1,
        "negative control: a live named writer leaves the empty peer pending"
    );
    assert!(matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
    ));
}

/// The one sound witness that a handoff endpoint was closed: its pipe peer reaches EOF.
/// A numeric `F_GETFD` probe cannot say it, since a neighbour thread can take the number
/// the instant the close lands.
///
/// The 30 s wait covers platforms where [`cloexec_pipe`] is not atomic: a child forked
/// between the pipe and its `F_SETFD` holds a duplicate writer for its life, and this
/// binary's spawned copies of itself live for seconds. The bound only decides how long a
/// genuinely missing close takes to report.
#[cfg(unix)]
fn assert_peer_reaches_eof(peer: i32, subject: &str) {
    let mut pfd = libc::pollfd {
        fd: peer,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    let polled = loop {
        // SAFETY: one initialized `pollfd` describing a live descriptor.
        let result = unsafe { libc::poll(&raw mut pfd, 1, 30_000) };
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break result;
    };
    assert_eq!(
        polled, 1,
        "{subject}: the closed named writer must make its peer readable/hung up"
    );
    assert_ne!(
        pfd.revents & libc::POLLHUP,
        0,
        "{subject}: the peer wake must be the named writer's EOF"
    );
    let mut byte = [0u8; 1];
    assert_eq!(
        // SAFETY: a 1-byte read into a stack buffer of that length.
        unsafe { libc::read(peer, byte.as_mut_ptr().cast(), byte.len()) },
        0,
        "{subject}: the peer must reach EOF, not data"
    );
}

/// This build's number, as the handoff's target identity names it.
#[cfg(unix)]
fn own_build() -> u64 {
    crate::build_info::build_number()
        .parse::<u64>()
        .unwrap_or(0)
}

/// The target identity of this very build: what a parent that authorized it
/// publishes.
#[cfg(unix)]
fn own_target() -> String {
    encode_target_identity(own_build(), crate::build_info::git_commit())
}

/// A live `zsh` record for `local_id` under `sid`, every optional field empty.
#[cfg(unix)]
fn zsh_record(local_id: u64, sid: &str) -> SessionRecord {
    SessionRecord {
        local_id,
        sid: sid.to_string(),
        parent: None,
        state: "alive".to_string(),
        title: "zsh".to_string(),
        screen: None,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        control: None,
        frozen_path: false,
        topics: Vec::new(),
        fg_holder: None,
        rekey: false,
        loader: false,
        history: None,
        history_dropped: 0,
        history_withheld: false,
        history_lost: 0,
        hold: None,
        more_holds: Vec::new(),
        supervisor: None,
        claim_known: false,
        attention_owners: Vec::new(),
        viewport_from_bottom: None,
        render_debt: None,
        render_debt_cut: 0,
        questions: None,
        autopilot: None,
        land_at: None,
        fabric: Default::default(),
        timeline_id: 0,
        log_ids: None,
        log_seq: 0,
        journal_key: None,
        task: None,
        status_seen: None,
        autopilot_end: None,
    }
}

/// A manifest of `sessions` and nothing else.
#[cfg(unix)]
fn bare_manifest(sessions: Vec<SessionRecord>) -> SessionHandoff {
    SessionHandoff {
        resources: None,
        schema: SessionHandoff::SCHEMA,
        window: None,
        connections: Vec::new(),
        sessions,
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
    }
}

/// A `--hold` pane `local_id` under `sid`, showing `checkpoint`.
#[cfg(unix)]
fn held_screen(local_id: u64, sid: SessionId, checkpoint: TerminalCheckpoint) -> HeldScreen {
    HeldScreen {
        local_id,
        sid,
        checkpoint,
        exit: None,
        history_dropped: 0,
        history_head: None,
        failure_seen: false,
    }
}

/// `held` as the writer stamps it onto an empty manifest in `dir`, and then as
/// the successor takes it back beside the `handed` sessions: what was written
/// and what is shown.
#[cfg(unix)]
fn stamp_and_take_held(
    dir: &std::path::Path,
    held: &[HeldScreen],
    handed: &[(u64, &str)],
) -> (Vec<crate::session_store::HeldPaneCarry>, Vec<HeldPane>) {
    let mut manifest = bare_manifest(Vec::new());
    let nonce = "0123456789abcdef0123456789abcdef";
    stamp_held_panes(
        &mut manifest,
        held,
        &mut crate::handoff_history::HeldExports::default(),
        &[],
        &mut 0,
        dir,
        nonce,
        (0, 0),
        &mut Vec::new(),
    );
    let stem = format!("seamless-{}-{nonce}", std::process::id());
    let mut remaining = MAX_HANDOFF_AGGREGATE_GRID_BYTES;
    let mut history = crate::handoff_history::MAX_AGGREGATE_BYTES;
    let mut control = crate::handoff_carry::MAX_AGGREGATE_BYTES;
    let shown = take_held_panes(
        &manifest.held,
        handed,
        &dir.join(format!("{stem}.toml")),
        &stem,
        dir,
        &mut remaining,
        &mut 0,
        &mut history,
        &mut control,
    );
    (manifest.held, shown)
}

#[cfg(unix)]
fn exact_legacy_layout(ids: &[u64]) -> RestoreManifest {
    RestoreManifest::new(vec![WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        restored_tabs: ids
            .iter()
            .map(|id| RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    sid: None,
                    lineage: None,
                    log_ep: None,
                    cwd: None,
                    title: format!("session-{id}"),
                    profile: None,
                    local_id: Some(*id),
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                    questions: None,
                    autopilot: None,
                    land_at: None,
                    autopilot_by: None,
                    theme: None,
                    agent: None,
                    supervised: None,
                    lost: None,
                    held: false,
                    holds: Vec::new(),
                })),
                focused_path: Vec::new(),
                zoomed: false,
            })
            .collect(),
    }])
}

/// Exhaustive over all 256 env-presence combinations: a successor that accepted a mixed
/// shape would read an fd number from an environment that inherited no descriptors and
/// adopt whatever LaunchServices left at that index as a terminal master.
#[test]
fn protocol_shape_table_accepts_exactly_the_two_legal_forms() {
    let shape = |bits: u16| HandoffEnvPresence {
        manifest: bits & 1 != 0,
        fds: bits & 2 != 0,
        nonce: bits & 4 != 0,
        layout: bits & 8 != 0,
        ready: bits & 16 != 0,
        commit: bits & 32 != 0,
        rendezvous: bits & 64 != 0,
        claim: bits & 128 != 0,
    };
    for bits in 0u16..256 {
        let manifest = bits & 1 != 0;
        let fds = bits & 2 != 0;
        let nonce = bits & 4 != 0;
        let layout = bits & 8 != 0;
        let ready = bits & 16 != 0;
        let commit = bits & 32 != 0;
        let rendezvous = bits & 64 != 0;
        let claim = bits & 128 != 0;
        let got = handoff_is_modern_overlap(shape(bits));
        let base = manifest && nonce && layout;
        let inherited = base && fds && ready && commit && !rendezvous && !claim;
        let out_of_band =
            cfg!(target_os = "macos") && base && rendezvous && claim && !fds && !ready && !commit;
        assert_eq!(got, inherited || out_of_band, "shape bits {bits:08b}");
    }
    // Each form is reachable (out-of-band on macOS), so the disjunction is not vacuous.
    assert!(handoff_is_modern_overlap(shape(0b0011_1111)));
    assert_eq!(
        handoff_is_modern_overlap(shape(0b1100_1101)),
        cfg!(target_os = "macos")
    );
    assert!(
        !handoff_is_modern_overlap(shape(0b1111_1111)),
        "a MIXED shape is a forgery or a bug, never a handoff"
    );
}

#[test]
fn a_carried_checkpoint_round_trips_its_scrollback_and_bounds_it() {
    use aterm_core::terminal::Terminal;

    // A terminal with far more history than any bound would carry.
    let mut t = Terminal::new(4, 20);
    for i in 0..400 {
        t.process(format!("line{i}\r\n").as_bytes());
    }

    // A carried checkpoint declares its history and validates against it.
    let carried = t
        .checkpoint_carry(MAX_HANDOFF_HISTORY_LINES as usize)
        .expect("Ground");
    assert_eq!(
        carried.history_lines, MAX_HANDOFF_HISTORY_LINES,
        "a deep ring fills the bound exactly"
    );
    assert!(
        checkpoint_grid_is_canonical(
            &carried.grid,
            carried.rows,
            carried.cols,
            carried.history_lines
        ),
        "the consumer must accept the shape the producer emits"
    );
    // ...and rejects it under the wrong history count: the declared length is load-bearing.
    assert!(!checkpoint_grid_is_canonical(
        &carried.grid,
        carried.rows,
        carried.cols,
        0
    ));

    // The bound is a real ceiling: a hostile meta cannot exceed it.
    assert!(
        dimension_grid_cap(4, 20, MAX_HANDOFF_HISTORY_LINES + 1).is_none(),
        "history beyond the ceiling is refused before any decode"
    );

    // Restoring a carried checkpoint brings the history back.
    let restored = Terminal::from_checkpoint(&carried);
    assert_eq!(
        restored.grid().scrollback_lines(),
        MAX_HANDOFF_HISTORY_LINES as usize,
        "a seamless update must not truncate the tab to one screen"
    );
}

/// A bold row filling all 149 columns of a 55x149 pane with an NFD `e` + U+0301 (or a ZWJ
/// `👨‍💻`) is 149 cells but 150 characters. The strict line decoder must not compare the
/// per-character attrs RLE to the cell cap, or every in-session update is refused while
/// the row is on screen. Checked as the main grid and as the saved primary under 1049.
#[test]
fn a_full_styled_row_with_a_combining_mark_or_zwj_is_canonical_on_both_grids() {
    use aterm_core::terminal::Terminal;

    for cluster in ["e\u{301}", "\u{1F468}\u{200D}\u{1F4BB}"] {
        let mut t = Terminal::new(55, 149);
        let fill = 149 - if cluster.starts_with('e') { 1 } else { 2 };
        t.process(format!("\x1b[1m{}{cluster}\x1b[0m", "a".repeat(fill)).as_bytes());
        let row = t.row_text(0).expect("row 0");
        assert!(
            row.contains(cluster) && row.chars().count() > 149,
            "{cluster:?}: the row holds more characters than columns: {row:?}"
        );

        let main = t.checkpoint_carry(0).expect("Ground");
        assert!(
            checkpoint_grid_is_canonical(&main.grid, 55, 149, 0),
            "{cluster:?}: the main grid is canonical"
        );
        assert!(
            screen_digest(&[(0, main.clone())]).is_ok(),
            "{cluster:?}: the wire admits the main grid"
        );
        let restored = Terminal::from_checkpoint(&main);
        assert_eq!(restored.row_text(0), Some(row.clone()), "{cluster:?}");

        // The same row on the saved primary while an alt-screen app runs.
        t.process(b"\x1b[?1049h");
        let under_alt = t.checkpoint_carry(0).expect("Ground");
        let saved = under_alt.alt_grid.as_ref().expect("saved primary");
        assert!(
            checkpoint_grid_is_canonical(saved, 55, 149, 0),
            "{cluster:?}: the saved primary is canonical"
        );
        assert!(
            screen_digest(&[(0, under_alt)]).is_ok(),
            "{cluster:?}: the wire admits the saved primary"
        );
    }
}

/// A session parked mid-OSC (`printf '\e]0;x'`) is carried whole by the parser carry and
/// admitted: `ParserNotGround` refuses only a parser mid-sequence with nothing carried. A
/// hooked DCS query, which no carry holds, is admitted through the abandoning carry with
/// the cancelled (Ground) parser it restores into.
#[test]
fn a_session_parked_mid_osc_is_admitted_through_the_abandoning_carry() {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(b"prompt$ \x1b]0;x");
    let (carry, abandoned) = t.checkpoint_carry_abandoning_partial(0);
    assert_eq!(abandoned, None, "the partial OSC is carried");
    assert_eq!(Some(&carry), t.checkpoint_carry(0).as_ref());
    assert!(!carry.parser_ground && carry.parser.is_some());
    assert!(checkpoint_shape_refusal(0, &carry).is_none());
    assert!(screen_digest(&[(0, carry)]).is_ok());

    let mut dcs = aterm_core::terminal::Terminal::new(24, 80);
    dcs.process(b"prompt$ \x1bP$qm");
    assert!(
        dcs.checkpoint_carry(0).is_none(),
        "control: no exact carry holds a hooked DCS query"
    );
    let (carry, abandoned) = dcs.checkpoint_carry_abandoning_partial(0);
    assert_eq!(abandoned, Some("DcsPassthrough"));
    assert!(checkpoint_shape_refusal(0, &carry).is_none());
    assert!(screen_digest(&[(0, carry.clone())]).is_ok());

    // Negative control: a parser left mid-sequence with nothing carried is still
    // refused by name.
    let mut uncarried = carry;
    uncarried.parser_ground = false;
    assert_eq!(
        checkpoint_shape_refusal(0, &uncarried),
        Some(ScreenDigestRefusal::ParserNotGround { local_id: 0 })
    );
}

#[test]
fn modern_wide_narrow_checkpoint_preserves_full_tab_vector_and_rejects_bad_lengths() {
    let mut source = aterm_core::terminal::Terminal::new(6, 120);
    source.process(b"\x1b[3g\x1b[1;97H\x1bH");
    source.resize(6, 40);
    let checkpoint = source.checkpoint_visible().expect("Ground modern capture");
    let meta = CheckpointMeta::from_checkpoint(&checkpoint);
    assert_eq!(meta.cursor.tab_stops.len(), 120);
    assert!(meta.cursor.tab_stops[96]);
    assert!(checkpoint_meta_is_bounded(&meta));

    let carry = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: aterm_json::to_string(&meta).unwrap(),
        grid_file: "unused-in-parser-test".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let parsed = parse_checkpoint_meta(&carry).expect("modern strict meta");
    let rebuilt_checkpoint = parsed.into_checkpoint(checkpoint.grid.clone(), None);
    let mut restored = aterm_core::terminal::Terminal::from_checkpoint(&rebuilt_checkpoint);
    source.resize(6, 120);
    restored.resize(6, 120);
    for terminal in [&mut source, &mut restored] {
        terminal.process(b"\x1b[1;90H\t");
    }
    assert_eq!(source.cursor().col, 96);
    assert_eq!(restored.cursor(), source.cursor());
    assert_eq!(restored.grid().tab_stops(), source.grid().tab_stops());

    let mut short = meta.clone();
    short.cursor.tab_stops.truncate(39);
    assert!(!checkpoint_meta_is_bounded(&short));
    let mut oversized = meta;
    oversized.cursor.tab_stops = vec![false; usize::from(aterm_core::grid::MAX_GRID_COLS) + 1];
    assert!(
        !checkpoint_meta_is_bounded(&oversized),
        "oversize vector is rejected before restore/allocation growth"
    );
}

/// The holder fence (`aterm-core`'s `holder_fence`) crosses the meta wire
/// exactly; a value that does not read costs only the fence (the screen
/// adopts, every card bare until its drawer is seeded); and a session no
/// drawer ran in writes no key, so its meta bytes are what they were.
#[test]
fn the_holder_fence_crosses_the_meta_wire_and_a_bad_one_costs_only_the_fence() {
    use aterm_core::terminal::{CutKind, HolderCut};
    let mut t = aterm_core::terminal::Terminal::new(6, 40);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(HolderCut {
        to: Some(7),
        kind: CutKind::Group,
        from_alive: true,
    });
    t.process(b"\x1b[48;5;170mAAAA\x1b[0m\r\n");
    let checkpoint = t.checkpoint_visible().expect("Ground");
    let meta = CheckpointMeta::from_checkpoint(&checkpoint);
    assert!(!meta.holder.is_empty());
    let carry = |meta: String| ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta,
        grid_file: "unused-in-parser-test".to_string(),
        alt_grid_file: None,
        repaint: false,
    };
    let wire = aterm_json::to_string(&meta).unwrap();
    let parsed = parse_checkpoint_meta(&carry(wire.clone())).expect("modern strict meta");
    assert_eq!(parsed.holder, checkpoint.holder);
    let restored = aterm_core::terminal::Terminal::from_checkpoint(
        &parsed.into_checkpoint(checkpoint.grid.clone(), None),
    );
    assert_eq!(restored.holder_view(), t.holder_view());

    let mut value: aterm_json::Value = aterm_json::from_str(&wire).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("holder".to_string(), aterm_json::Value::from("garbage"));
    let parsed = parse_checkpoint_meta(&carry(aterm_json::to_string(&value).unwrap()))
        .expect("a bad holder never costs the screen");
    // It costs the fence, and is no seed authority either.
    assert_eq!(
        parsed.holder,
        aterm_core::terminal::HolderRepr {
            seed_untrusted: true,
            ..aterm_core::terminal::HolderRepr::default()
        }
    );
    assert_eq!(parsed.rows, 6);

    let quiet = aterm_core::terminal::Terminal::new(6, 40);
    let quiet = CheckpointMeta::from_checkpoint(&quiet.checkpoint_visible().expect("Ground"));
    assert!(
        !aterm_json::to_string(&quiet).unwrap().contains("holder"),
        "no drawer, no key"
    );
}

/// A DECSC slot on a row the shrink TRIMS (a blank row below the cursor)
/// stays past the grid in the engine (`aterm-core`'s
/// `handoff_saved_cursor_after_shrink`). That honest state is what refused
/// the 2026-09-22 updates (`saved_cursor_main.cursor_row == rows`, the
/// "update staged" status bar's one-row shrink): the wire bounds a DECSC slot
/// by the engine's ceiling rather than the grid, and the set commits.
#[test]
fn a_saved_cursor_on_a_row_the_shrink_trimmed_crosses_past_the_grid() {
    let mut t = aterm_core::terminal::Terminal::new(56, 149);
    for i in 0..60 {
        t.process(format!("$ command {i}\r\n").as_bytes());
    }
    // DECSC on the blank last row, then back up onto the last line of output.
    t.process(b"\x1b7\x1b[A");
    t.resize(55, 149);
    let checkpoint = t
        .checkpoint_carry(MAX_HANDOFF_HISTORY_LINES as usize)
        .expect("Ground");
    let meta = CheckpointMeta::from_checkpoint(&checkpoint);
    let saved = meta.saved_cursor_main.expect("DECSC saved the cursor");
    assert!(
        saved.cursor_row >= meta.rows,
        "PRECONDITION: the slot names a row the grid no longer has"
    );
    assert!(
        screen_digest(&[(0, checkpoint)]).is_ok(),
        "the visible checkpoint set commits canonically"
    );
}

/// Every conjunct of the bound predicate refuses its own field, the first
/// broken one in the consumer's order — and a DECSC slot past the CURRENT
/// grid (honest state after a shrink) is no refusal.
#[test]
fn a_meta_out_of_bounds_names_the_field_the_value_and_the_bound() {
    let mut t = aterm_core::terminal::Terminal::new(6, 40);
    t.process(b"\x1b7");
    let checkpoint = t.checkpoint_visible().expect("Ground");
    let meta = CheckpointMeta::from_checkpoint(&checkpoint);
    assert_eq!(checkpoint_meta_bound_violation(&meta), None);
    /// What is broken, how, and the `(slot, field)` the refusal names.
    type Case = (
        &'static str,
        fn(&mut CheckpointMeta),
        Option<(&'static str, &'static str)>,
    );
    let cases: [Case; 9] = [
        (
            "saved row past the grid",
            |m| m.saved_cursor_main.as_mut().unwrap().cursor_row = 6,
            None,
        ),
        (
            "saved col past the grid",
            |m| m.saved_cursor_main.as_mut().unwrap().cursor_col = 40,
            None,
        ),
        (
            "saved row at the engine ceiling",
            |m| m.saved_cursor_main.as_mut().unwrap().cursor_row = aterm_core::grid::MAX_GRID_ROWS,
            Some(("saved_cursor_main", "cursor_row")),
        ),
        (
            "saved col at the engine ceiling",
            |m| m.saved_cursor_main.as_mut().unwrap().cursor_col = aterm_core::grid::MAX_GRID_COLS,
            Some(("saved_cursor_main", "cursor_col")),
        ),
        ("no rows", |m| m.rows = 0, Some(("", "rows"))),
        (
            "cursor col",
            |m| m.cursor.cursor_col = 40,
            Some(("cursor", "cursor_col")),
        ),
        (
            "scroll bottom",
            |m| m.cursor.scroll_bottom = 6,
            Some(("cursor", "scroll_bottom")),
        ),
        (
            "scroll top",
            |m| m.cursor.scroll_top = 9,
            Some(("cursor", "scroll_top")),
        ),
        (
            "NUL cwd",
            |m| m.current_working_directory = Some("a\0b".into()),
            Some(("", "current_working_directory NUL bytes")),
        ),
    ];
    for (what, mutate, want) in cases {
        let mut forged = meta.clone();
        mutate(&mut forged);
        let got = checkpoint_meta_bound_violation(&forged);
        assert_eq!(got.map(|v| (v.slot, v.field)), want, "{what}");
    }
}

/// The per-grid ceiling is exactly half the aggregate, a refusal names the ceiling that
/// bound, and a refused admission leaves the aggregate untouched; the legacy ceiling
/// refuses a full-screen 5K window (99x338) that the current one admits.
#[test]
fn decoded_cell_budget_accepts_exact_max_and_rejects_max_plus_one() {
    let (rows, cols) = (512_u16, 4096_u16);
    assert_eq!(
        u64::from(rows) * u64::from(cols),
        MAX_HANDOFF_GRID_CELLS,
        "PRECONDITION: 512x4096 is exactly the per-grid ceiling"
    );
    let mut used = 0;
    assert!(
        admit_checkpoint_dimensions(&mut used, rows, cols, 0, false).is_ok(),
        "512x4096 is the exact per-grid cell ceiling"
    );
    assert_eq!(used, MAX_HANDOFF_GRID_CELLS);
    let before = used;
    assert_eq!(
        admit_checkpoint_dimensions(&mut used, rows + 1, cols, 0, false),
        Err(AdmitRefusal::PerGrid {
            cells: MAX_HANDOFF_GRID_CELLS + u64::from(cols),
            cap: MAX_HANDOFF_GRID_CELLS,
        }),
        "one row beyond the per-grid ceiling is rejected before capture, as the \
         PER-GRID cap"
    );
    assert_eq!(used, before, "failed admission is transactional");
    assert!(
        matches!(
            admit_checkpoint_dimensions(&mut used, 0, cols, 0, false),
            Err(AdmitRefusal::Dimension { .. })
        ),
        "a geometry outside the protocol names the protocol"
    );

    let mut aggregate = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
    assert_eq!(
        admit_checkpoint_dimensions(&mut aggregate, 1, 1, 0, false),
        Err(AdmitRefusal::Aggregate {
            cost: 1,
            used: MAX_HANDOFF_AGGREGATE_GRID_CELLS,
            cap: MAX_HANDOFF_AGGREGATE_GRID_CELLS,
        }),
        "aggregate max+1 cell is rejected, as the AGGREGATE"
    );
    assert_eq!(aggregate, MAX_HANDOFF_AGGREGATE_GRID_CELLS);

    // The rollback ceiling is the frozen 0.91 one, and nothing else moves.
    let legacy = WireCaps::legacy();
    assert_eq!(legacy.per_grid_cells(), 32 * 1024);
    assert!(legacy.admit(&mut 0, 256, 128, 0, true).is_ok());
    assert!(matches!(
        legacy.admit(&mut 0, 99, 338, 0, true),
        Err(AdmitRefusal::PerGrid { cells: 33_462, .. })
    ));
    assert!(WireCaps::current().admit(&mut 0, 99, 338, 0, true).is_ok());
}

/// A ROLLBACK is the one hop whose consumer is older than its producer, so a
/// rollback to a build before v0.93.0 is the one hop priced at the frozen
/// caps; every other hop — an upgrade, or a same-build relaunch — gets this
/// build's own (plan P0-4a).
#[test]
fn only_a_downgrade_target_gets_the_legacy_caps() {
    assert_eq!(WireCaps::for_hop(1_000, 999), WireCaps::legacy());
    assert_eq!(WireCaps::for_hop(1_000, 1_000), WireCaps::current());
    assert_eq!(WireCaps::for_hop(1_000, 1_001), WireCaps::current());
    assert_eq!(
        WireCaps::for_target(crate::running_build_number()),
        WireCaps::current(),
        "a same-image relaunch is not a downgrade"
    );
}

/// ROUND EIGHT'S LEAD U24: a rollback to v0.93.0 or later is priced at the
/// consumer it reaches, not at v0.91.0's. Every consumer from v0.93.0 on
/// admits this build's per-grid cap (`git show v0.93.0:crates/aterm-gui/src/
/// seamless.rs`: `MAX_HANDOFF_GRID_CELLS = MAX_HANDOFF_AGGREGATE_GRID_CELLS /
/// 2`, the aggregate 4 Mi cells) and bounds a line's attrs by its character
/// count (`decode_line_strict` there is this build's, comments aside), and
/// from v0.94.0 on imports the history sidecar (`handoff_history::incoming`;
/// v0.101.0 the held panes', `incoming_held`). A hop to such a build was
/// carried blank past 32 Ki cells and without its scrollback.
///
/// FAILS WITHOUT THE FIX: the 5K full-screen pane came back `Repaint` at
/// 24x80, the styled cluster row `Repaint`, and no sidecar was exported.
#[test]
fn a_rollback_to_a_relaxed_consumer_carries_the_exact_screen_and_history() {
    use aterm_core::terminal::Terminal;

    use crate::handoff_history::{exports_for_target, exports_held_for_target};

    // RELEASES.ledger.
    let (v0_92, v0_93, v0_94, v0_98, v0_100, v0_101) = (
        1_790_278_596,
        1_790_305_290,
        1_790_526_323,
        1_790_607_299,
        1_790_807_622,
        1_791_501_389,
    );
    let running = crate::running_build_number().max(v0_101 + 1);

    // A native full-screen window on a 5K display at the default font.
    let mut full_screen = Terminal::new(99, 338);
    full_screen.process(b"$ make\r\nbuilding...\r\n");
    // A full-width styled row holding a combining mark.
    let mut cluster = Terminal::new(55, 149);
    cluster.process(format!("\x1b[1m{}e\u{301}\x1b[0m", "a".repeat(148)).as_bytes());
    for target in [v0_93, v0_98, v0_101, running - 1] {
        let caps = WireCaps::for_hop(running, target);
        assert_eq!(caps.per_grid_cells(), WireCaps::current().per_grid_cells());
        let (carried, rung, cause) = carry_for_wire(&full_screen, 0, 0, &mut 0, caps);
        assert_eq!(rung, CarryRung::Full, "{target}: {cause:?}");
        assert_eq!((carried.rows, carried.cols), (99, 338), "{target}");
        let (_, rung, cause) = carry_for_wire(&cluster, 0, 0, &mut 0, caps);
        assert_eq!(rung, CarryRung::Full, "{target}: {cause:?}");
        // What it restores beyond the screen is not known from here (the
        // parser and output-state carries are the unix handoff's).
        #[cfg(unix)]
        assert!(
            !caps.carries_parser() && !caps.carries_output_state(),
            "{target}"
        );
    }
    for target in [v0_94, v0_98, v0_100, running - 1] {
        assert!(exports_for_target(running, target), "{target}");
    }
    for target in [v0_101, running - 1] {
        assert!(exports_held_for_target(running, target), "{target}");
    }
    // CONTROL: v0.92.0 and every build before it enforce the frozen caps, and
    // nothing before v0.94.0 (v0.101.0 for a held pane) imports a sidecar.
    for target in [0, v0_92, v0_93 - 1] {
        assert_eq!(WireCaps::for_hop(running, target), WireCaps::legacy());
        let (_, rung, _) = carry_for_wire(
            &full_screen,
            0,
            0,
            &mut 0,
            WireCaps::for_hop(running, target),
        );
        assert_eq!(rung, CarryRung::Repaint, "{target}");
    }
    assert!(!exports_for_target(running, v0_94 - 1));
    assert!(!exports_held_for_target(running, v0_100));
    // A successor at least as new as this build is this build.
    assert_eq!(WireCaps::for_hop(running, running), WireCaps::current());
    assert!(exports_for_target(running, running) && exports_held_for_target(running, running));
}

/// v0.91.0's strict line decoder (`decode_line_strict` in its
/// `aterm-scrollback/src/line_codec_block.rs`), transcribed: every record of a blob this
/// build decodes, its attrs run lengths summed and compared against the column count.
/// Written out rather than borrowed from `WireCaps` so the rollback test checks the
/// carry against the older consumer's arithmetic, not the function under test.
fn v0_91_line_decoder_admits(blob: &[u8], rows: u16, cols: u16, history: u32) -> bool {
    strict_grid_lines(blob, rows, cols, history).is_some_and(|lines| {
        lines.iter().all(|line| {
            line.attrs().is_none_or(|attrs| {
                let mut cells = 0usize;
                attrs.runs().iter().all(|run| {
                    cells = cells.saturating_add(run.length as usize);
                    cells <= usize::from(cols)
                })
            })
        })
    })
}

/// Whether a v0.91.0-vintage consumer's line decoder admits every grid of
/// `checkpoint` — the check its all-or-nothing adoption runs per session.
pub(super) fn v0_91_consumer_decodes(checkpoint: &TerminalCheckpoint) -> bool {
    v0_91_line_decoder_admits(
        &checkpoint.grid,
        checkpoint.rows,
        checkpoint.cols,
        checkpoint.history_lines,
    ) && checkpoint
        .alt_grid
        .as_ref()
        .is_none_or(|alt| v0_91_line_decoder_admits(alt, checkpoint.rows, checkpoint.cols, 0))
}

/// This build's strict line decoder bounds a line's attrs by its character count; a
/// v0.91.0 consumer bounds them by columns. So on a rollback hop a full-width styled row
/// holding `e` + U+0301 or a ZWJ cluster must not go at Full: that consumer would refuse
/// the whole adoption.
#[test]
fn a_rollback_never_sends_a_row_the_older_line_decoder_refuses() {
    use aterm_core::terminal::Terminal;

    for cluster in ["e\u{301}", "\u{1F468}\u{200D}\u{1F4BB}"] {
        let fill = 149 - if cluster.starts_with('e') { 1 } else { 2 };
        let row = format!("\x1b[1m{}{cluster}\x1b[0m", "a".repeat(fill));

        // On the visible grid: the forward hop carries it exactly; the rollback
        // hop cannot carry it exactly at all.
        let mut visible = Terminal::new(55, 149);
        visible.process(row.as_bytes());
        let (forward, rung, _) = carry_for_wire(&visible, 0, 0, &mut 0, WireCaps::current());
        assert_eq!(rung, CarryRung::Full, "{cluster:?}: forward hop");
        assert!(
            !v0_91_consumer_decodes(&forward),
            "{cluster:?}: CONTROL — this desk is one the older decoder refuses"
        );
        let (rollback, rung, _) = carry_for_wire(&visible, 0, 0, &mut 0, WireCaps::legacy());
        assert_eq!(rung, CarryRung::Repaint, "{cluster:?}: rollback hop");
        assert!(v0_91_consumer_decodes(&rollback), "{cluster:?}");
        assert!(screen_digest(&[(0, rollback)]).is_ok(), "{cluster:?}");

        // On the saved primary under an alt-screen app: the same.
        visible.process(b"\x1b[?1049h");
        let (under_alt, rung, _) = carry_for_wire(&visible, 0, 0, &mut 0, WireCaps::legacy());
        assert_eq!(rung, CarryRung::Repaint, "{cluster:?}: saved primary");
        assert!(v0_91_consumer_decodes(&under_alt), "{cluster:?}");

        // In scrollback only: the rollback hop drops the history and keeps the
        // screen exact, one rung down, not a blank tab.
        let mut scrolled = Terminal::new(4, 149);
        scrolled.process(row.as_bytes());
        scrolled.process(b"\r\n1\r\n2\r\n3\r\n4");
        let (_, rung, _) = carry_for_wire(&scrolled, 0, 16, &mut 0, WireCaps::current());
        assert_eq!(rung, CarryRung::Full, "{cluster:?}: forward, with history");
        let (visible_only, rung, _) = carry_for_wire(&scrolled, 0, 16, &mut 0, WireCaps::legacy());
        assert_eq!(rung, CarryRung::VisibleOnly, "{cluster:?}: history dropped");
        assert!(v0_91_consumer_decodes(&visible_only), "{cluster:?}");
    }
}

/// The exact rungs are priced as the consumer prices them: the inactive grid is charged
/// only when the engine has one, as the Repaint rung, `screen_digest` and the successor
/// charge it. Charging it always would invert the ladder with the aggregate nearly
/// spent: the exact visible screen refused, and the same session carried blank at the
/// same geometry and cost.
#[test]
fn an_exact_carry_is_priced_as_the_consumer_prices_it() {
    use aterm_core::terminal::Terminal;

    let mut plain = Terminal::new(100, 200);
    plain.process(b"$ make\r\nbuilding...\r\n");
    assert!(!plain.has_inactive_grid(), "PRECONDITION: no inactive grid");
    // 30,000 cells left: the screen's 20,000 fit, 40,000 would not.
    let spent = MAX_HANDOFF_AGGREGATE_GRID_CELLS - 30_000;
    let mut charged = spent;
    let (exact, rung, cause) = carry_for_wire(&plain, 0, 0, &mut charged, WireCaps::current());
    assert_eq!(rung, CarryRung::Full, "the exact screen fits: {cause:?}");
    assert_eq!(
        charged - spent,
        wire_cells(&exact),
        "charged exactly what the consumer charges"
    );
    let mut consumer = spent;
    assert!(
        admit_checkpoint_dimensions(
            &mut consumer,
            exact.rows,
            exact.cols,
            exact.history_lines,
            exact.alt_grid.is_some(),
        )
        .is_ok(),
        "and the consumer's admission agrees"
    );

    // With an inactive grid the exact carry costs two screens and does not fit; the
    // blank carry prices that grid too, so it shrinks.
    let mut on_alt = Terminal::new(100, 200);
    on_alt.process(b"\x1b[?1049h");
    let mut charged = spent;
    let (blank, rung, _) = carry_for_wire(&on_alt, 0, 0, &mut charged, WireCaps::current());
    assert_eq!(rung, CarryRung::Repaint);
    assert!(blank.alt_grid.is_some());
    assert!(
        (blank.rows, blank.cols) < (100, 200),
        "a smaller blank screen"
    );
    assert_eq!(charged - spent, wire_cells(&blank));
}

/// The pool `settle_wire_carries` is handed for `terminals`, as `Full` carries.
fn meta_pool(terminals: Vec<aterm_core::terminal::Terminal>) -> Vec<WireCarry> {
    terminals
        .into_iter()
        .zip(0..)
        .map(|(terminal, local_id)| WireCarry {
            local_id,
            checkpoint: terminal.checkpoint_carry(0).expect("carried"),
            rung: CarryRung::Full,
        })
        .collect()
}

fn metas_cost(pool: &[WireCarry]) -> u64 {
    pool.iter()
        .map(|carry| meta_manifest_cost(&carry.checkpoint))
        .sum::<u64>()
}

/// COLOUR STACKS, TAB STOPS PAST THE WIDTH AND OSC PAYLOADS ARE SHED, NOT
/// REFUSED (round eight of the update audit, finding 51). Tabs that pushed
/// the kitty colour stack and never popped it carry 256 palette entries per
/// push, with no OSC 133 mark to shed; a grid never shrinks its stop vector,
/// so a pane once 4,000 columns wide carries 4,000 stops per cursor at any
/// width; and a parser in the middle of an OSC carries up to 4 KiB of
/// payload, as hex. None had a rung, so a desk of such tabs refused the
/// whole update on content (`MetasOverBudget`). Now they give way, every
/// screen exact and every carry still one the consumer admits.
#[test]
fn colour_stacks_tab_stops_and_osc_payloads_are_shed_not_refused() {
    // A stack entry is the program's diff from the baseline, so each push
    // carries a whole recoloured palette.
    let recolour_all: Vec<u8> = (0..=u8::MAX)
        .flat_map(|i| format!("\x1b]4;{i};rgb:{i:02x}/{:02x}/01\x07", 255 - i).into_bytes())
        .collect();
    let colour_stacks = (0..28)
        .map(|_| {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
            terminal.apply_config(&aterm_core::config::TerminalConfig {
                allow_palette_reconfigure: true,
                ..aterm_core::config::TerminalConfig::default()
            });
            terminal.process(b"$ some output\r\n");
            terminal.process(&recolour_all);
            for _ in 0..16 {
                terminal.process(b"\x1b]30001\x07");
            }
            terminal
        })
        .collect::<Vec<_>>();
    let wide_once = (0..140)
        .map(|_| {
            let mut terminal = aterm_core::terminal::Terminal::new(1, 4000);
            terminal.resize(24, 80);
            terminal.process(b"$ some output\r\n");
            terminal
        })
        .collect::<Vec<_>>();
    let mut payload = b"$ \x1b]2;".to_vec();
    payload.extend(std::iter::repeat_n(b'x', 4000));
    // 400 columns: a stop per column the consumer needs (nothing to shed
    // there), and the payload on top puts the pool past the budget.
    let mid_osc = (0..250)
        .map(|_| {
            let mut terminal = aterm_core::terminal::Terminal::new(24, 400);
            terminal.process(&payload);
            terminal
        })
        .collect::<Vec<_>>();
    for (what, terminals) in [
        ("colour stacks", colour_stacks),
        ("tab stops", wide_once),
        ("OSC payloads", mid_osc),
    ] {
        let mut pool = meta_pool(terminals);
        assert!(
            metas_cost(&pool) > MAX_HANDOFF_AGGREGATE_META_BYTES,
            "PRECONDITION ({what}): past the budget ({})",
            metas_cost(&pool)
        );
        let grids = pool
            .iter()
            .map(|carry| carry.checkpoint.grid.clone())
            .collect::<Vec<_>>();
        settle_wire_carries(&mut pool, WireCaps::current())
            .unwrap_or_else(|refusal| panic!("{what}: the pool commits: {refusal}"));
        assert!(
            metas_cost(&pool) <= MAX_HANDOFF_AGGREGATE_META_BYTES,
            "{what}: {}",
            metas_cost(&pool)
        );
        assert!(
            screen_digest_refs(pool.iter().map(|c| (c.local_id, &c.checkpoint)).collect()).is_ok(),
            "{what}: every carry is one the consumer admits"
        );
        let mut shed = 0;
        for (carry, grid) in pool.iter().zip(&grids) {
            let id = carry.local_id;
            assert_eq!(
                &carry.checkpoint.grid, grid,
                "{what}: session {id}'s screen"
            );
            assert_eq!(carry.rung, CarryRung::Full, "{what}: session {id}");
            let cp = &carry.checkpoint;
            if cp.cursor.tab_stops.len() == usize::from(cp.cols)
                && cp
                    .parser
                    .as_ref()
                    .is_none_or(|parser| parser.osc.is_empty())
            {
                shed += 1;
            }
            if let Some(parser) = cp.parser.as_ref().filter(|parser| parser.osc.is_empty()) {
                assert!(
                    parser.osc_overflowed,
                    "{what}: session {id} discards its OSC"
                );
            }
        }
        assert!(shed > 0, "{what}: something was shed");
    }
}

/// With nothing left to shed, screens are lowered rather than the update refused: 62
/// one-row panes 4,096 columns wide on their alternate screens need a stop per column per
/// cursor and together are past the budget. The costliest are carried blank at a smaller
/// geometry; the rest keep their screens exactly.
#[test]
fn metas_past_every_shed_lower_screens_instead_of_refusing() {
    let terminals = (0..62)
        .map(|_| {
            let mut terminal = aterm_core::terminal::Terminal::new(1, 4096);
            terminal.process(b"\x1b[?1049hTUI");
            terminal
        })
        .collect::<Vec<_>>();
    let mut pool = meta_pool(terminals);
    assert!(
        metas_cost(&pool) > MAX_HANDOFF_AGGREGATE_META_BYTES,
        "PRECONDITION: past the budget ({})",
        metas_cost(&pool)
    );
    assert!(
        pool.iter()
            .all(|carry| !meta_has_sheddable(&carry.checkpoint)),
        "PRECONDITION: nothing to shed"
    );
    let grids = pool
        .iter()
        .map(|carry| carry.checkpoint.grid.clone())
        .collect::<Vec<_>>();
    settle_wire_carries(&mut pool, WireCaps::current())
        .unwrap_or_else(|refusal| panic!("the pool commits: {refusal}"));
    assert!(metas_cost(&pool) <= MAX_HANDOFF_AGGREGATE_META_BYTES);
    let repainted = pool
        .iter()
        .filter(|carry| carry.rung == CarryRung::Repaint)
        .count();
    assert!(
        (1..pool.len()).contains(&repainted),
        "some screens, not all, were lowered ({repainted})"
    );
    for (carry, grid) in pool.iter().zip(&grids) {
        if carry.rung == CarryRung::Full {
            assert_eq!(&carry.checkpoint.grid, grid, "session {}", carry.local_id);
        }
    }
}

/// A DESK WHOSE METAS OUTGROW THE MANIFEST DEGRADES, IT IS NOT REFUSED
/// (round six of the update audit, item 8). Forty shell tabs, each with
/// about three hundred OSC 133 commands across its carried history: every
/// carried command costs its meta a mark and an output block, and together
/// the metas are several MiB past the manifest's 4 MiB read cap.
///
/// * Unsettled, the writer must refuse typed (`ManifestOverCap`) and leave
///   no manifest — never write a file every successor refuses whole.
/// * Settled, the capture's self-check sheds the largest metas' marks until
///   they fit its budget, every grid byte-identical; the manifest is then
///   written under the cap and read back by `take_incoming`'s own capped
///   read.
///
/// RED before the fix: the self-check had no meta budget, and the writer,
/// with no held pane left to drop, wrote the oversized manifest anyway.
#[test]
#[cfg(unix)]
fn an_oversized_manifest_degrades_sessions_instead_of_being_written() {
    let Some(_env) = env_alone() else { return };
    let _scratch = ScratchEnv::home("metas");
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");

    const TABS: u64 = 40;
    let cwd = format!("/Users//someone/{}", "project-directory/".repeat(3));
    let mut pool = Vec::new();
    for local_id in 0..TABS {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(format!("\x1b]7;file://host{cwd}\x07").as_bytes());
        for n in 0..300 {
            terminal.process(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07make target-{n}\x1b]133;C\x07\r\n\
                     \x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let checkpoint = terminal.checkpoint_carry(256).expect("parser is Ground");
        assert!(
            checkpoint.shell.command_marks.len() > 200,
            "PRECONDITION — the carried history holds the marks ({})",
            checkpoint.shell.command_marks.len()
        );
        pool.push(WireCarry {
            local_id,
            checkpoint,
            rung: CarryRung::Full,
        });
    }
    let cost = |pool: &[WireCarry]| {
        pool.iter()
            .map(|carry| meta_manifest_cost(&carry.checkpoint))
            .sum::<u64>()
    };
    assert!(
        cost(&pool) > MAX_HANDOFF_MANIFEST_BYTES,
        "PRECONDITION — the metas alone are past the manifest cap ({})",
        cost(&pool)
    );
    let manifest = bare_manifest(
        (0..TABS)
            .map(|local_id| zsh_record(local_id, &format!("s-metas-{local_id}")))
            .collect(),
    );
    let fds = HandoffFds {
        entries: (0..TABS)
            .map(|local_id| {
                let n = i32::try_from(local_id).expect("small");
                (local_id, 100 + n, 4000 + n)
            })
            .collect(),
    };
    let screens_of = |pool: &[WireCarry]| {
        pool.iter()
            .map(|carry| (carry.local_id, carry.checkpoint.clone()))
            .collect::<Vec<_>>()
    };

    // Unsettled: refused, typed, and nothing published.
    let nonce = mint_outgoing_nonce();
    let written = write_outgoing(&manifest, &fds, &screens_of(&pool), &[], None, &[], &nonce);
    assert_eq!(
        written.err(),
        Some(WriteOutgoingFailure::ManifestOverCap),
        "a manifest no successor reads is never written"
    );
    assert!(
        !outgoing_manifest_path(&nonce)
            .expect("the control dir exists")
            .exists(),
        "a refused write publishes no manifest"
    );

    // Settled: the marks give way, the screens do not.
    let grids = pool
        .iter()
        .map(|carry| carry.checkpoint.grid.clone())
        .collect::<Vec<_>>();
    let digest = settle_wire_carries(&mut pool, WireCaps::current()).expect("the pool commits");
    assert!(
        cost(&pool) <= MAX_HANDOFF_AGGREGATE_META_BYTES,
        "the metas fit their budget ({})",
        cost(&pool)
    );
    assert!(
        pool.iter()
            .any(|carry| carry.checkpoint.shell.command_marks.is_empty()),
        "the largest metas shed their marks"
    );
    for (carry, grid) in pool.iter().zip(&grids) {
        assert_eq!(&carry.checkpoint.grid, grid, "session {}", carry.local_id);
        assert_eq!(carry.rung, CarryRung::Full, "session {}", carry.local_id);
    }
    let screens = screens_of(&pool);
    assert_eq!(screen_digest(&screens), Ok(digest));
    let nonce = mint_outgoing_nonce();
    let outgoing = write_outgoing(&manifest, &fds, &screens, &[], None, &[], &nonce)
        .expect("the settled manifest is written");
    assert_eq!(
        outgoing.screen_digest, digest,
        "the digest the park committed"
    );
    let size = std::fs::metadata(&outgoing.manifest_path)
        .expect("the manifest is on disk")
        .len();
    assert!(size <= MAX_HANDOFF_MANIFEST_BYTES, "{size}");
    assert!(
        take_regular_capped(
            std::path::Path::new(&outgoing.manifest_path),
            &dir,
            MAX_HANDOFF_MANIFEST_BYTES,
        )
        .is_some(),
        "the successor's capped read takes it"
    );
}

/// A refused write says why: with the grid sidecar's name occupied by a directory, the
/// writer hands the `io::ErrorKind` out as a typed [`WriteOutgoingFailure::Io`] (filed
/// `ProducerFailed`, transient) and leaves no manifest. An untyped failure would be filed
/// structural, so a full boot volume would latch the artifact manual-only.
#[test]
#[cfg(unix)]
fn a_refused_grid_write_carries_its_io_error_kind_out() {
    let Some(_env) = env_alone() else { return };
    let _scratch = ScratchEnv::home("kind");
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    let nonce = mint_outgoing_nonce();
    std::fs::create_dir_all(dir.join(format!("seamless-{}-{nonce}.s0.grid", std::process::id())))
        .expect("occupy the grid sidecar's name");

    let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
    terminal.process(b"$ ls");
    let screens = vec![(0, terminal.checkpoint_visible().expect("parser is Ground"))];
    let manifest = bare_manifest(vec![zsh_record(0, "s-write-kind")]);
    let fds = HandoffFds {
        entries: vec![(0, 100, 4000)],
    };
    let written = write_outgoing(&manifest, &fds, &screens, &[], None, &[], &nonce);
    assert_eq!(
        written.err(),
        Some(WriteOutgoingFailure::Io(std::io::ErrorKind::IsADirectory)),
        "the filesystem's own reason travels out of the writer"
    );
    assert!(
        !outgoing_manifest_path(&nonce)
            .expect("the control dir exists")
            .exists(),
        "a refused write publishes no manifest"
    );
}

/// RECOVERABLE AUTHORITY GIVES WAY BEFORE CONTENT THAT IS LOST FOR GOOD
/// (round eight of the update audit, U23's order, from the U2 group's
/// review). One tab with a waiting post, one `--hold` pane, and 50,000
/// carried connection rows from one source: the rows alone put the
/// manifest past its cap, and leaving them out alone brings it under. They
/// go, and the held pane's screen and the queued post cross. RED before
/// the fix: the held pane was dropped (its placeholder shown) and the post
/// retired dead `not-carried` first, both for good, and only then were the
/// connections — which a `connect` makes again — left out, which alone
/// would have fitted.
///
/// CONTROL: when the connections CANNOT cover what is over, the content
/// that has to go anyway goes first, and the few connections the smaller
/// excess leaves are kept: a post past the cap is shed, the peer's three
/// rows cross, and nothing is left out for the Commit to audit.
///
/// The rows left out are RETURNED, never audited by the writer (U23's
/// review): the Commit audits each as dropped
/// (`App::audit_connections_left_out`); a drop written with the manifest was
/// false for every attempt that then stood down, and repeated at each retry.
#[test]
#[cfg(unix)]
fn connections_give_way_before_held_panes_and_the_mailbox() {
    let Some(_env) = env_alone() else { return };
    let _scratch = ScratchEnv::home("conns-first");
    let sid = "s-00000000010000050505";
    let (controller, peer) = ("s-00000000010000030303", "s-00000000010000040404");
    let row = |src: &str, op: &str| ConnectionCarry {
        src: src.to_string(),
        dst: sid.to_string(),
        op: op.to_string(),
    };
    let peers = ["read-screen", "signal", "write-input"]
        .into_iter()
        .map(|op| row(peer, op))
        .collect::<Vec<_>>();
    let post = |body: String| crate::fabric::FabricCarry {
        last_post_id: 7,
        posts: vec![crate::fabric::PostCarry {
            id: 7,
            to: "@s-peer".to_string(),
            kind: "note".to_string(),
            re: None,
            dl: None,
            via: None,
            key: None,
            body,
        }],
        ..Default::default()
    };
    let manifest_with =
        |connections: Vec<ConnectionCarry>, fabric: crate::fabric::FabricCarry| SessionHandoff {
            connections,
            ..bare_manifest(vec![SessionRecord {
                land_at: None,
                fabric,
                ..zsh_record(0, sid)
            }])
        };
    let screens = || {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(b"$ ls");
        vec![(0, terminal.checkpoint_visible().expect("parser is Ground"))]
    };
    let held = || {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(b"build finished\r\n");
        vec![held_screen(
            1,
            SessionId::generate(),
            terminal.checkpoint_carry_abandoning_partial(0).0,
        )]
    };
    let fds = HandoffFds {
        entries: vec![(0, 100, 4000)],
    };
    let read_back = |outgoing: &OutgoingHandoff| {
        let body = std::fs::read_to_string(&outgoing.manifest_path).expect("written");
        assert!(
            body.len() as u64 <= MAX_HANDOFF_MANIFEST_BYTES,
            "{}",
            body.len()
        );
        SessionHandoff::from_toml(body.split_once('\n').expect("nonce line").1)
            .expect("the successor reads it")
    };

    // The connections alone are over, and alone they fit it.
    let mut connections = vec![row(controller, "read-screen"); 50_000];
    connections.extend(peers.clone());
    let manifest = manifest_with(connections, post("see the build log".to_string()));
    assert!(
        manifest.to_toml().expect("serializes").len() as u64 > MAX_HANDOFF_MANIFEST_BYTES,
        "PRECONDITION — the connection rows put the manifest past its cap"
    );
    let nonce = mint_outgoing_nonce();
    let outgoing = write_outgoing_with_held(
        &manifest,
        &fds,
        &screens(),
        &[],
        None,
        &[],
        &held(),
        crate::handoff_history::HeldExports::default(),
        &nonce,
    )
    .expect("the connections shed, the update is not refused");
    let read = read_back(&outgoing);
    assert_eq!(
        read.held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        vec![1],
        "the held pane's screen crosses"
    );
    assert_eq!(
        read.sessions[0].fabric.posts.len(),
        1,
        "the waiting post crosses"
    );
    assert!(
        read.sessions[0].fabric.uncarried.is_empty(),
        "no post is retired dead"
    );
    assert_eq!(read.connections, peers, "the costliest source went whole");
    assert_eq!(
        outgoing.connections_left_out,
        vec![row(controller, "read-screen"); 50_000],
        "what was left out goes to the Commit"
    );

    // CONTROL: a post past the cap, which the connections cannot cover. The
    // mailbox gives way before the update does (round seven's review, item 1:
    // `ManifestOverCap` on every retry before it), its id named, its counter
    // kept.
    let manifest = manifest_with(peers.clone(), post("\u{1b}[31m".repeat(500_000)));
    assert!(
        manifest.to_toml().expect("serializes").len() as u64 > MAX_HANDOFF_MANIFEST_BYTES,
        "PRECONDITION — the post puts the manifest past its cap"
    );
    let nonce = mint_outgoing_nonce();
    let outgoing = write_outgoing(&manifest, &fds, &screens(), &[], None, &[], &nonce)
        .expect("the mailbox sheds, the update is not refused");
    let read = read_back(&outgoing);
    assert!(read.sessions[0].fabric.posts.is_empty(), "the post is shed");
    assert_eq!(read.sessions[0].fabric.uncarried, [7], "and named");
    assert_eq!(read.sessions[0].fabric.last_post_id, 7, "the counter stays");
    assert_eq!(read.connections, peers, "the connections are kept");
    assert!(
        outgoing.connections_left_out.is_empty(),
        "nothing is left out that the shed post did not need"
    );
}

/// A held pane that covers the excess goes before connections that cannot. Whether they
/// can is measured by writing the manifest without them, since their ranking bound
/// overcounts a row's framing (64 bytes where the writer writes 43). Control: over by
/// exactly the rows' measured bytes, the rows go and the pane crosses.
#[test]
#[cfg(unix)]
fn connections_that_cannot_cover_the_excess_measured_are_kept() {
    let Some(_env) = env_alone() else { return };
    let _scratch = ScratchEnv::home("conns-measured");
    // Session ids as `SessionId::generate` spells them: the rows' real shape.
    let sid = "s-00000000010000050505";
    let controller = "s-00000000010000030303";
    let row = ConnectionCarry {
        src: controller.to_string(),
        dst: sid.to_string(),
        op: "read-screen".to_string(),
    };
    // `pad` bytes of description: the one knob that sets the manifest's size.
    let manifest_with = |connections: Vec<ConnectionCarry>, pad: u64| SessionHandoff {
        connections,
        ..bare_manifest(vec![SessionRecord {
            description: Some("x".repeat(usize::try_from(pad).expect("a pad in memory"))),
            ..zsh_record(0, sid)
        }])
    };
    let screens = {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(b"$ ls");
        vec![(0, terminal.checkpoint_visible().expect("parser is Ground"))]
    };
    // One pane, the same in every write, its title making its entry weigh.
    let held = {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(format!("\x1b]2;{}\x07", "build ".repeat(150)).as_bytes());
        terminal.process(b"build finished\r\n");
        vec![held_screen(
            1,
            SessionId::generate(),
            terminal.checkpoint_carry_abandoning_partial(0).0,
        )]
    };
    let fds = HandoffFds {
        entries: vec![(0, 100, 4000)],
    };
    let nonce_bytes = mint_outgoing_nonce().len() as u64;
    let bytes = |manifest: &SessionHandoff| {
        nonce_bytes + 1 + manifest.to_toml().expect("serializes").len() as u64
    };
    let write = |manifest: &SessionHandoff| {
        let nonce = mint_outgoing_nonce();
        assert_eq!(nonce.len() as u64, nonce_bytes, "every nonce is one length");
        let outgoing = write_outgoing_with_held(
            manifest,
            &fds,
            &screens,
            &[],
            None,
            &[],
            &held,
            crate::handoff_history::HeldExports::default(),
            &nonce,
        )
        .expect("the manifest is written");
        let body = std::fs::read_to_string(&outgoing.manifest_path).expect("written");
        let read = SessionHandoff::from_toml(body.split_once('\n').expect("nonce line").1)
            .expect("the successor reads it");
        assert_eq!(
            bytes(&read),
            body.len() as u64,
            "what is read writes the same bytes"
        );
        (outgoing, body.len() as u64, read)
    };

    // Measured on a desk that fits: what the pane's entry weighs, then what rows
    // whose bound is half of it weigh.
    let (_, whole, read) = write(&manifest_with(Vec::new(), 0));
    assert_eq!(read.held.len(), 1, "PRECONDITION — the pane is carried");
    let pane = whole
        - bytes(&SessionHandoff {
            held: Vec::new(),
            ..read
        });
    let rows = vec![row.clone(); usize::try_from(pane / 2 / connection_row_cost(&row)).unwrap()];
    assert!(
        rows.len() >= 4,
        "PRECONDITION — the pane outweighs rows ({pane} bytes)"
    );
    let (_, whole, read) = write(&manifest_with(rows.clone(), 0));
    assert_eq!(
        read.connections, rows,
        "PRECONDITION — the rows are carried"
    );
    let real = whole
        - bytes(&SessionHandoff {
            connections: Vec::new(),
            ..read
        });
    let bound: u64 = rows.iter().map(connection_row_cost).sum();
    // The band: past what the rows weigh, within their bound, and within what the
    // pane's entry frees.
    let excess = real + (bound - real) / 2;
    assert!(
        real < excess && excess <= bound && excess <= pane,
        "PRECONDITION — rows weigh {real}, bound {bound}, pane {pane}, excess {excess}"
    );

    let (outgoing, written, read) = write(&manifest_with(
        rows.clone(),
        MAX_HANDOFF_MANIFEST_BYTES + excess - whole,
    ));
    assert!(written <= MAX_HANDOFF_MANIFEST_BYTES, "{written}");
    assert!(
        read.held.is_empty(),
        "the pane goes: it alone covers the excess"
    );
    assert_eq!(read.connections, rows, "every connection crosses");
    assert!(
        outgoing.connections_left_out.is_empty(),
        "no connection is left out that the pane's going made unneeded"
    );

    // Control: over by exactly what the rows weigh.
    let (outgoing, written, read) = write(&manifest_with(
        rows.clone(),
        MAX_HANDOFF_MANIFEST_BYTES + real - whole,
    ));
    assert!(written <= MAX_HANDOFF_MANIFEST_BYTES, "{written}");
    assert_eq!(read.held.len(), 1, "the pane crosses");
    assert!(read.connections.is_empty(), "the connections give way");
    assert_eq!(
        outgoing.connections_left_out, rows,
        "for the Commit to audit"
    );
}

/// The `ATERM_SEAMLESS_FDS` wire another build reads: the encoder's exact
/// form round-trips through the receiver's parser, and one unparseable entry
/// refuses the whole wire, so a corrupt value fabricates no session.
#[test]
fn the_fds_wire_round_trips_and_one_bad_entry_refuses_it_whole() {
    let fds = HandoffFds {
        entries: vec![(0, 6, 12345), (1, 7, 12346), (2, 9, 12347)],
    };
    let wire = fds.encode();
    assert_eq!(wire, "0=6:12345,1=7:12346,2=9:12347");
    assert_eq!(decode_fds_bounded(&wire), Some(fds));
    assert_eq!(decode_fds_bounded(""), Some(HandoffFds::default()));
    for bad in ["0=6:100,junk", "1=7", "2=x:y", "0=6:100,", ",0=6:100"] {
        assert_eq!(decode_fds_bounded(bad), None, "{bad}");
    }
}

#[test]
#[cfg(unix)]
fn malformed_named_fd_is_closed_on_authority_rejection() {
    let Some(_env) = env_alone() else { return };
    let _restore_fds = RestoreVar::new(ENV_FDS);
    let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
    let _restore_nonce = RestoreVar::new(ENV_NONCE);
    // Keep the read endpoint and name the write endpoint: the peer's EOF proves that
    // open file description was closed, where a numeric check after the close races
    // with parallel tests reusing the number.
    let (peer, named) = cloexec_pipe();
    assert!(
        peer >= 3 && named >= 3,
        "handoff descriptors are never stdio"
    );
    arm_peer_eof_witness(peer);
    // The descriptor position is syntactically valid, while both identity
    // and pid invalidate the authority. Duplicate mentions must close once.
    aterm_log::env::unset(ENV_MANIFEST);
    aterm_log::env::unset(ENV_NONCE);
    aterm_log::env::set(ENV_FDS, format!("bad={named}:badpid,bad={named}:badpid"));
    assert!(take_incoming().adopted.is_empty());
    assert_peer_reaches_eof(peer, "every syntactically named invalid-authority endpoint");
    aterm_pty::close_fd(peer);
}

#[test]
#[cfg(unix)]
fn invalid_prearm_alias_closes_once_clears_authority_and_cannot_close_reused_fd() {
    let Some(_env) = env_alone() else { return };
    let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
    let _restore_fds = RestoreVar::new(ENV_FDS);
    let _restore_nonce = RestoreVar::new(ENV_NONCE);
    let _restore_layout = RestoreVar::new(ENV_LAYOUT);
    let _restore_ready = RestoreVar::new(ENV_READY_FD);
    let _restore_commit = RestoreVar::new(ENV_COMMIT_FD);

    // Keep the read endpoint and hand over the write endpoint: the peer's EOF is what
    // proves the aliased open file description was closed.
    let (peer, writer) = cloexec_pipe();
    // Everything after the prearm is about the number handed over (the close freed
    // it; a resource put back at it survives), so it must be one no sibling thread can
    // be handed. The claim inherits close-on-exec from `F_DUPFD_CLOEXEC`.
    let isolated = claim_isolated_fd_slot(writer);
    let aliased = isolated.map_or(writer, |slot| {
        aterm_pty::close_fd(writer);
        slot
    });
    assert!(
        peer >= 3 && aliased >= 3,
        "handoff descriptors are never stdio"
    );
    arm_peer_eof_witness(peer);

    // An otherwise recognizable overlap shape, but READY aliases the PTY authority:
    // a single fd cannot have two owners.
    aterm_log::env::set(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
    aterm_log::env::set(ENV_FDS, format!("7={aliased}:4242"));
    aterm_log::env::set(ENV_NONCE, "0123456789abcdef0123456789abcdef");
    aterm_log::env::unset(ENV_LAYOUT);
    aterm_log::env::set(ENV_READY_FD, aliased.to_string());
    aterm_log::env::unset(ENV_COMMIT_FD);

    let prearmed = prearm_incoming_fds();
    assert!(prearmed.rejects_boot());
    assert!(prearmed.final_exec_fds().is_empty());
    assert_peer_reaches_eof(peer, "the doubly named alias");
    for key in [
        ENV_MANIFEST,
        ENV_FDS,
        ENV_NONCE,
        ENV_LAYOUT,
        ENV_READY_FD,
        ENV_COMMIT_FD,
    ] {
        assert!(std::env::var_os(key).is_none(), "{key} cleared");
    }

    // Reuse the numeric slot for an unrelated resource: a later authority parse sees
    // no stale environment and so cannot close the replacement (the double-close /
    // fd-reuse case). A pipe, because its own peer turns "still open" into "still ours".
    let (replacement_peer, replacement_writer) = cloexec_pipe();
    // Reclaim with `F_DUPFD_CLOEXEC`, never `dup2` (which would replace a sibling's
    // descriptor). It lands only on a free number, so an isolated slot is always
    // reclaimed and a fallback slot that lost its number skips the identity arm below.
    let reclaimed = unsafe { libc::fcntl(replacement_writer, libc::F_DUPFD_CLOEXEC, aliased) };
    aterm_pty::close_fd(replacement_writer);
    assert!(
        reclaimed == aliased || isolated.is_none(),
        "an isolated slot is this test's own, so the prearm's close must leave it reclaimable"
    );

    assert!(take_incoming().adopted.is_empty());

    if reclaimed == aliased {
        // Identity, not liveness: the byte written through the reclaimed number
        // must arrive on the pipe kept here. A descriptor a stale handoff closed and
        // the kernel re-handed to a neighbour passes an `F_GETFD` probe and fails this.
        assert_eq!(
            unsafe { libc::write(aliased, b"R".as_ptr().cast(), 1) },
            1,
            "stale handoff must not close a reused descriptor"
        );
        let mut got = [0u8; 1];
        assert_eq!(
            unsafe { libc::read(replacement_peer, got.as_mut_ptr().cast(), got.len()) },
            1,
            "the reused descriptor must still carry bytes to its own peer"
        );
        assert_eq!(got, *b"R", "the reclaimed number must name the replacement");
    }
    aterm_pty::close_fd(reclaimed);
    aterm_pty::close_fd(replacement_peer);
    aterm_pty::close_fd(peer);
}

/// `final_exec_env` is authority-gated like `final_exec_fds`: only a valid prearm with a
/// validated parent yields the restoration pair, so a malformed handoff cannot smuggle a
/// parent-authority variable onto the exec image.
#[test]
#[cfg(unix)]
fn final_exec_env_requires_valid_parent_authority() {
    let prearm = |recognizable: bool, valid: bool, witness: Option<ParentWitness>| {
        PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable,
            valid,
            parent: witness.map(|witness| AttestedParent { pid: 4242, witness }),
        }
        .final_exec_env()
    };
    let pair = |key, value: &str| {
        (
            std::ffi::OsString::from(key),
            std::ffi::OsString::from(value),
        )
    };
    assert_eq!(
        prearm(true, true, Some(ParentWitness::ForkLink)),
        vec![pair(ENV_PARENT_PID, "4242")]
    );
    // A birth witness republishes the record too, so the re-exec'd image can
    // re-attest without a parent link — the property a LaunchServices
    // successor depends on.
    let birth = ParentWitness::Birth(ProcessBirth {
        seconds: 17,
        microseconds: 42,
    });
    assert_eq!(
        prearm(true, true, Some(birth)),
        vec![
            pair(ENV_PARENT_PID, "4242"),
            pair(ENV_PARENT_BIRTH, "17.42")
        ]
    );
    assert!(prearm(true, false, Some(ParentWitness::ForkLink)).is_empty());
    assert!(prearm(false, true, None).is_empty());
}

/// The birth record crosses a process boundary as text, so its encoding is protocol: a
/// value that did not round-trip would demote every modern handoff to the legacy witness,
/// and a malformed one is refused outright, never read as absent (`prearm_incoming_fds`'s
/// `birth_present` arm).
#[test]
#[cfg(unix)]
fn process_birth_wire_round_trips_and_rejects_malformed_values() {
    let birth = ProcessBirth {
        seconds: 1_784_100_000,
        microseconds: 7,
    };
    assert_eq!(birth.to_wire(), "1784100000.7");
    assert_eq!(ProcessBirth::from_wire(&birth.to_wire()), Some(birth));
    assert_eq!(ProcessBirth::from_wire("  1784100000.7  "), Some(birth));
    for bad in [
        "",
        "1784100000",
        "1784100000.",
        ".7",
        "a.b",
        "-1.7",
        "1.2.3",
    ] {
        assert_eq!(ProcessBirth::from_wire(bad), None, "malformed: {bad:?}");
    }
}

/// IDENTITY, the half of parental attestation that must NOT weaken when
/// `getppid()` stops being the liveness probe (blocker B1).
///
/// While this process has a live creator, the kernel parent link is the
/// authority and an environment-supplied claim may only AGREE with it. This
/// is the arm that refuses a stale `ATERM_HANDOFF_*` block that leaked into
/// a user shell which later runs `aterm`: such a process has a live creator
/// (the shell), so no published detail can make an unrelated pid pass.
#[test]
#[cfg(unix)]
fn attestation_refuses_any_pid_that_is_not_our_live_creator() {
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let creator = unsafe { libc::getppid() };
    assert!(creator > 1, "the test harness must have a real parent");
    let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");

    assert!(
        attest_handoff_parent_from(creator, None, creator, || creator).is_some(),
        "the live creator itself is always attestable"
    );
    assert_eq!(
        attest_handoff_parent_from(ours, None, creator, || creator),
        None,
        "a live creator that is not the claimed pid refuses the claim"
    );
    for claimed in [0, 1] {
        assert_eq!(
            attest_handoff_parent_from(claimed, None, claimed, || claimed),
            None,
            "pid {claimed} is what reparenting produces, never a parent"
        );
    }
}

/// BLOCKER B1, stated as a test: a successor with NO live creator — which is
/// what a LaunchServices-launched process is from birth, `getppid() == 1`
/// before it runs its first instruction — is admissible exactly when the
/// outgoing process published its own kernel birth record, and not
/// otherwise. The old `getppid()`-only predicate refused it unconditionally
/// and then fail-stopped it, which is why the launchd-job repair could not
/// start.
///
/// The stand-in parent is a real spawned process rather than this one, so
/// the "wrong record at a live pid" case below is the pid-reuse scenario
/// itself: a live process sitting at the attested pid that is NOT the
/// attested process.
#[test]
#[cfg(target_os = "macos")]
fn a_creatorless_successor_is_admitted_only_by_a_published_birth_record() {
    const NO_LIVE_CREATOR: libc::pid_t = 1;
    let mut stand_in = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the stand-in parent");
    let pid = libc::pid_t::try_from(stand_in.id()).expect("stand-in pid");
    let birth = read_process_birth(pid).expect("a live process has a birth record");
    let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");
    let not_its_birth = read_process_birth(ours).expect("our own birth record");
    assert_ne!(
        birth, not_its_birth,
        "two processes never share a birth instant"
    );

    assert_eq!(
        attest_handoff_parent_from(pid, Some(birth), NO_LIVE_CREATOR, || NO_LIVE_CREATOR),
        Some(AttestedParent {
            pid,
            witness: ParentWitness::Birth(birth),
        }),
        "a published record the kernel corroborates is a complete witness"
    );
    assert_eq!(
        attest_handoff_parent_from(pid, None, NO_LIVE_CREATOR, || NO_LIVE_CREATOR),
        None,
        "with no creator and nothing published there is no witness at all"
    );
    assert_eq!(
        attest_handoff_parent_from(pid, Some(not_its_birth), NO_LIVE_CREATOR, || {
            NO_LIVE_CREATOR
        }),
        None,
        "a live pid carrying the WRONG birth record is the pid-reuse case, \
         and it must refuse rather than fall back to something weaker"
    );

    stand_in.kill().expect("kill the stand-in parent");
    stand_in.wait().expect("reap the stand-in parent");
    assert_eq!(
        attest_handoff_parent_from(pid, Some(birth), NO_LIVE_CREATOR, || NO_LIVE_CREATOR),
        None,
        "a dead parent is not admissible however well it was described"
    );
}

/// The LEGACY-CAPTURE arm's race: a parent too old to publish its birth
/// record is attested by the parent link, and the birth record read next is
/// trusted as ITS record. If the parent exits between those two reads, the
/// record at that pid may belong to whatever recycled it — so the link is
/// re-read after the birth read, and a link that moved refuses. The control
/// (the link unchanged) admits with the record as the witness.
#[test]
#[cfg(target_os = "macos")]
fn legacy_capture_refuses_a_parent_that_exits_between_link_and_birth_reads() {
    let mut stand_in = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the stand-in parent");
    let pid = libc::pid_t::try_from(stand_in.id()).expect("stand-in pid");
    let birth = read_process_birth(pid).expect("a live process has a birth record");

    assert_eq!(
        attest_handoff_parent_from(pid, None, pid, || pid),
        Some(AttestedParent {
            pid,
            witness: ParentWitness::Birth(birth),
        }),
        "control: a parent link that still names pid after the read admits"
    );
    // The parent exited between the two reads: the link now says 1.
    assert_eq!(
        attest_handoff_parent_from(pid, None, pid, || 1),
        None,
        "a parent link that moved during the birth read must refuse"
    );

    stand_in.kill().expect("kill the stand-in parent");
    stand_in.wait().expect("reap the stand-in parent");
}

/// LIVENESS, the half that runs for the whole overlap and whose false
/// "alive" is the dangerous one: it would strand a readerless successor
/// holding duplicated masters of the user's live shells.
///
/// The witness discriminates by the kernel's birth record, so it survives
/// the successor not being a child of the process it watches, and a
/// different process at the same pid does not satisfy it.
#[test]
#[cfg(target_os = "macos")]
fn parent_liveness_tracks_the_process_not_the_pid() {
    let mut watched = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the watched process");
    let pid = libc::pid_t::try_from(watched.id()).expect("watched pid");
    let birth = read_process_birth(pid).expect("a live process has a birth record");
    let attested = AttestedParent {
        pid,
        witness: ParentWitness::Birth(birth),
    };
    assert!(
        attested.still_alive(),
        "the watched process is running and is not this process's parent, \
         which is exactly the situation a launchd-owned successor is in"
    );

    // The same pid, described by a record that belongs to someone else: what
    // a recycled pid looks like from here.
    let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");
    let our_birth = read_process_birth(ours).expect("our own birth record");
    let impostor = AttestedParent {
        pid,
        witness: ParentWitness::Birth(our_birth),
    };
    assert!(
        !impostor.still_alive(),
        "a live pid alone must never satisfy the liveness witness"
    );

    watched.kill().expect("kill the watched process");
    watched.wait().expect("reap the watched process");
    assert!(
        !attested.still_alive(),
        "the witness must go false when the attested process exits — this is \
         the fail-stop that stops a readerless candidate outliving its parent"
    );
}

/// The handoff bind's wait for its predecessor (`exited_by`): a running
/// process holds it to the deadline (the control), its exit ends it with the
/// deadline a minute out (a hang detector, not a budget) and before anyone
/// reaps it, and a pid whose birth no longer matches has exited already.
#[test]
#[cfg(target_os = "macos")]
fn the_exit_wait_ends_at_the_attested_exit_and_not_before() {
    use aterm_spec::wait::MUST_HAPPEN;
    use std::time::{Duration, Instant};
    let mut watched = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the watched process");
    let pid = libc::pid_t::try_from(watched.id()).expect("watched pid");
    let birth = read_process_birth(pid).expect("a live process has a birth record");
    let attested = AttestedParent {
        pid,
        witness: ParentWitness::Birth(birth),
    };
    assert!(
        !attested.exited_by(Instant::now() + Duration::from_millis(30)),
        "a running process has not exited at the deadline"
    );
    let waiter = std::thread::spawn(move || attested.exited_by(Instant::now() + MUST_HAPPEN));
    watched.kill().expect("kill the watched process");
    assert!(
        waiter.join().expect("the wait returns"),
        "the exit ends the wait"
    );
    watched.wait().expect("reap the watched process");
    assert!(
        AttestedParent::recycled_for_test(pid).exited_by(Instant::now() + MUST_HAPPEN),
        "a birth that no longer matches is an exit already"
    );
}

/// THE ChildDied regression (2026-07-22): a valid OverlapModern prearm
/// consumes `ATERM_HANDOFF_PARENT_PID` out of the ambient environment
/// (helpers spawned afterwards must never see it) — but the boot-apply
/// re-exec must be able to RESTORE that authority onto the successor
/// image, or the new build's own prearm rejects the inherited handoff and
/// the parked parent reads EOF. This proves the round trip: prearm is
/// valid, the ambient variables are gone, and `final_exec_env` carries the
/// exact pairs the successor's prearm requires.
#[test]
#[cfg(unix)]
fn valid_overlap_prearm_scrubs_ambient_parent_but_restores_it_for_exec() {
    let Some(_env) = env_alone() else { return };
    let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
    let _restore_fds = RestoreVar::new(ENV_FDS);
    let _restore_nonce = RestoreVar::new(ENV_NONCE);
    let _restore_layout = RestoreVar::new(ENV_LAYOUT);
    let _restore_ready = RestoreVar::new(ENV_READY_FD);
    let _restore_commit = RestoreVar::new(ENV_COMMIT_FD);
    let _restore_parent = RestoreVar::new(ENV_PARENT_PID);
    let _restore_birth = RestoreVar::new(ENV_PARENT_BIRTH);

    let mut master_pipe = [0i32; 2];
    let mut ready_pipe = [0i32; 2];
    let mut commit_pipe = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(master_pipe.as_mut_ptr()) }, 0, "pipe");
    assert_eq!(unsafe { libc::pipe(ready_pipe.as_mut_ptr()) }, 0, "pipe");
    assert_eq!(unsafe { libc::pipe(commit_pipe.as_mut_ptr()) }, 0, "pipe");
    let parent = unsafe { libc::getppid() };
    assert!(parent > 1, "test harness must have a real parent");
    aterm_log::env::set(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
    aterm_log::env::set(ENV_FDS, format!("7={}:4242", master_pipe[0]));
    aterm_log::env::set(ENV_NONCE, "0123456789abcdef0123456789abcdef");
    aterm_log::env::set(ENV_LAYOUT, "/tmp/not-consumed-by-prearm.layout.toml");
    aterm_log::env::set(ENV_READY_FD, ready_pipe[1].to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_pipe[0].to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent.to_string());
    aterm_log::env::unset(ENV_PARENT_BIRTH);

    let prearmed = prearm_incoming_fds();
    assert!(
        !prearmed.rejects_boot(),
        "the modern overlap shape is valid"
    );
    assert!(
        !prearmed.final_exec_fds().is_empty(),
        "descriptors survive for the final exec"
    );
    for key in [ENV_PARENT_PID, ENV_PARENT_BIRTH] {
        assert!(
            std::env::var_os(key).is_none(),
            "ambient parent authority ({key}) is scrubbed before any helper can spawn"
        );
    }
    // This handoff was published in the LEGACY shape — pid only, exactly what
    // a pre-0.13 outgoing build sends. Admission still succeeds, because the
    // parent link proves this pid is our creator; and where the kernel offers
    // a birth record the successor CAPTURES it under that proof and hands it
    // forward, so the re-exec'd image no longer needs a parent link either.
    let mut expected = vec![(
        std::ffi::OsString::from(ENV_PARENT_PID),
        std::ffi::OsString::from(parent.to_string()),
    )];
    if let Some(birth) = read_process_birth(parent) {
        expected.push((
            std::ffi::OsString::from(ENV_PARENT_BIRTH),
            std::ffi::OsString::from(birth.to_wire()),
        ));
    }
    assert_eq!(
        prearmed.final_exec_env(),
        expected,
        "the exec image gets the exact validated authority back"
    );

    for fd in [
        master_pipe[0],
        master_pipe[1],
        ready_pipe[0],
        ready_pipe[1],
        commit_pipe[0],
        commit_pipe[1],
    ] {
        aterm_pty::close_fd(fd);
    }
}

/// The readiness fd's fail-closed gauntlet: absent, garbage, stdio and a TTY all yield
/// `None` (the parked parent times out and resumes, so a spoof can only lose the
/// signal), and the variable is cleared on read so it never leaks into shell children.
///
/// `take_ready_fd` owns a live non-stdio descriptor before it judges it, so the refused
/// tty is closed by the refusal itself and this test never closes the number it hands
/// over: a second close would shut whatever descriptor a concurrent test was just given.
#[test]
#[cfg(unix)]
fn take_ready_fd_fails_closed() {
    let Some(_env) = env_alone() else { return };
    let _restore = RestoreVar::new("ATERM_HANDOFF_READY_FD");
    // All set/unset below: serialized by ENV_LOCK for the whole body.
    let take = || {
        take_ready_fd(
            Some("n".to_string()),
            Some([7; 32]),
            Some([8; 32]),
            HandoffTarget::own(),
            &[],
        )
    };
    for (what, value) in [
        ("absent", None),
        ("garbage", Some("not-a-number")),
        ("stdio", Some("1")),
    ] {
        match value {
            Some(value) => aterm_log::env::set("ATERM_HANDOFF_READY_FD", value),
            None => aterm_log::env::unset("ATERM_HANDOFF_READY_FD"),
        }
        assert!(take().is_none(), "{what} is refused");
        assert!(
            std::env::var_os("ATERM_HANDOFF_READY_FD").is_none(),
            "{what}: cleared on read regardless of outcome"
        );
    }
    // A real PTY master: the is_tty backstop must refuse it (a confused
    // wire must never let the readiness byte land in a shell's input).
    let (m, s) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // The refusal closes the number it is handed. Parked at an isolated slot, that
    // close is asserted below; without one the master's own number is handed over and
    // the close goes unasserted.
    let isolated = claim_isolated_fd_slot(m);
    let named = isolated.map_or(m, |slot| {
        aterm_pty::close_fd(m);
        slot
    });
    aterm_log::env::set("ATERM_HANDOFF_READY_FD", named.to_string());
    assert!(take().is_none(), "a tty is refused");
    if let Some(slot) = isolated {
        // SAFETY: `F_GETFD` only reads the flags a descriptor number carries.
        let flags = unsafe { libc::fcntl(slot, libc::F_GETFD) };
        assert_eq!(
            (flags, std::io::Error::last_os_error().raw_os_error()),
            (-1, Some(libc::EBADF)),
            "the refused tty is closed by the refusal, never left open"
        );
    }
    // `named` is `take_ready_fd`'s to close, and it has: only the slave
    // is still this test's.
    aterm_pty::close_fd(s);
}

/// A successor whose boot apply could not swap stays the old build, so
/// `take_target_identity` returns `None`. `take_ready_fd` already owns the readiness
/// write end when it refuses, so the refusal drops it, the pipe's last write end closes,
/// and the parent's `wait_handoff_ready` reads EOF and books `ChildDied` against a build
/// that never started (what that costs the build is `crate::ChildDeathEvidence`'s
/// business). The close is correct: a refused candidate must not sit on a live channel
/// it will never write. Pinned so a reader of a `ChildDied` verdict knows to look for a
/// wrong-build successor, not a crash.
#[test]
#[cfg(unix)]
fn a_refused_target_identity_closes_the_readiness_channel_as_child_death() {
    let Some(_env) = env_alone() else { return };
    let _restore = RestoreVar::new("ATERM_HANDOFF_READY_FD");
    let mut fds = [0i32; 2];
    // SAFETY: plain pipe(2) into a valid 2-slot out-array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (fds[0], fds[1]);
    // Serialized by ENV_LOCK.
    aterm_log::env::set("ATERM_HANDOFF_READY_FD", wr.to_string());
    // `target: None` is exactly what a wrong-build successor hands in: the
    // parent authorized build X, this image is build Y, so the identity was
    // refused a few instructions earlier.
    assert!(
        take_ready_fd(
            Some("n".to_string()),
            Some([7; 32]),
            Some([8; 32]),
            None,
            &[],
        )
        .is_none(),
        "a successor that is not the authorized build refuses the readiness channel"
    );
    // The parent's side: no write end survives anywhere, so the read end is at EOF
    // at once, a 0-byte read that `wait_handoff_ready` types as `ChildDied`.
    let mut byte = [0u8; 1];
    // SAFETY: bounded read into a stack local on an fd this test owns.
    let read = unsafe { libc::read(rd, byte.as_mut_ptr().cast(), 1) };
    assert_eq!(
        read, 0,
        "the refusal dropped the only write end, so the parked parent reads EOF (ChildDied)"
    );
    aterm_pty::close_fd(rd);
}

/// The happy path: a real pipe write-end is adopted and its CLOEXEC is
/// RE-ARMED (only the fork child's copy was cleared for its one exec; from
/// here no further child may inherit it). That the readiness byte then
/// arrives is `a_full_seamless_handoff_completes_and_commits`'s.
#[test]
#[cfg(unix)]
fn take_ready_fd_adopts_a_pipe_and_rearms_cloexec() {
    let Some(_env) = env_alone() else { return };
    let _restore = RestoreVar::new("ATERM_HANDOFF_READY_FD");
    let mut fds = [0i32; 2];
    // SAFETY: plain pipe(2) into a valid 2-slot out-array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (fds[0], fds[1]);
    // Simulate the fork child's inherited copy just before exec.
    let _ = aterm_pty::set_cloexec(wr, false);
    // Serialized by ENV_LOCK.
    aterm_log::env::set("ATERM_HANDOFF_READY_FD", wr.to_string());
    let owned = take_ready_fd(
        Some("n".to_string()),
        Some([7; 32]),
        Some([8; 32]),
        HandoffTarget::own(),
        &[],
    )
    .expect("a live pipe fd is adopted");
    assert!(
        std::env::var_os("ATERM_HANDOFF_READY_FD").is_none(),
        "cleared on read"
    );
    // SAFETY: F_GETFD on an fd we own.
    let flags = unsafe { libc::fcntl(owned.raw_fd(), libc::F_GETFD) };
    assert!(
        flags >= 0 && (flags & libc::FD_CLOEXEC) != 0,
        "CLOEXEC re-armed on adoption (EOF discipline)"
    );
    aterm_pty::close_fd(rd);
}

/// Rollback hygiene: `discard_outgoing` retires exactly this attempt's artifacts
/// (manifest and sidecars named by our pid and the nonce) and leaves every other file,
/// another attempt's manifest included, alone.
#[test]
fn discard_outgoing_removes_only_this_attempts_artifacts() {
    let Some(_env) = env_alone() else { return };
    // Bound before `_restore_xdg`, so the variable is restored before the
    // directory it names is removed.
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-discard-test-")
        .tempdir()
        .expect("scratch runtime dir");
    let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
    // Serialized by ENV_LOCK; restored by `_restore_xdg`.
    aterm_log::env::set("XDG_RUNTIME_DIR", tmp.path());
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    std::fs::create_dir_all(&dir).ok();
    let pid = std::process::id();
    let ours = [
        dir.join(format!("seamless-{pid}-aaaa.toml")),
        dir.join(format!("seamless-{pid}-aaaa.s0.grid")),
        dir.join(format!("seamless-{pid}-aaaa.s3.altgrid")),
        // The control carry's sidecar: a rollback retires it with the rest.
        dir.join(format!("seamless-{pid}-aaaa.s0.ctl")),
    ];
    let kept = [
        dir.join(format!("seamless-{pid}-bbbb.toml")), // a DIFFERENT attempt
        dir.join("unrelated.txt"),
    ];
    for p in ours.iter().chain(kept.iter()) {
        std::fs::write(p, b"x").expect("seed file");
    }
    discard_outgoing("aaaa");
    for p in &ours {
        assert!(!p.exists(), "retired: {}", p.display());
    }
    for p in &kept {
        assert!(p.exists(), "untouched: {}", p.display());
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(unix)]
const PARENT_DEATH_MODE: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_MODE";
#[cfg(unix)]
const PARENT_DEATH_MARKER: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_MARKER";
#[cfg(unix)]
const PARENT_DEATH_READY: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_READY";
#[cfg(unix)]
const PARENT_DEATH_READ_FD: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_READ_FD";
#[cfg(unix)]
const PARENT_DEATH_WRITE_FD: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_WRITE_FD";
#[cfg(unix)]
const PARENT_DEATH_EXPECTED_PID: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_EXPECTED_PID";
/// The file a watcher leaves when it outlives its watch: see the outer test.
#[cfg(unix)]
const PARENT_DEATH_OUTLIVED: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_OUTLIVED";
/// The file a watcher leaves once it has watched its live parent for
/// [`PARENT_DEATH_LIVE_LOOK`]: the word its parent goes on.
#[cfg(unix)]
const PARENT_DEATH_WATCHED: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_WATCHED";
/// The file the launcher makes its watcher's stderr, where the fail-stop's
/// line says which ending the watch took.
#[cfg(unix)]
const PARENT_DEATH_SAID: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_SAID";
/// How long a watcher goes on watching its LIVE parent, once armed, before it
/// says that it still does ([`PARENT_DEATH_WATCHED`]). NOT A DEADLINE: nothing
/// has to happen in it, and a busy machine makes the look weaker, never red.
/// It is twenty-five of the watch's 10 ms ticks, so that the watch reads its
/// parent's liveness, and reads it ALIVE, before the parent dies; and it is
/// the watcher's own word because only a process that still runs can give it.
/// MEASURED 2026-10-13, on what came before:
///
/// * no look — the launcher left within 5 ms of the ready marker, and the
///   watch's first read came after the death: neither a witness that calls a
///   live parent dead (10 runs of 10) nor a watcher that left right after its
///   marker (3 runs of 20) failed the test;
/// * the launcher looking at its watcher for 50 ms (`try_wait`) — a child
///   that has called `_exit` reads as running until the kernel has made its
///   exit waitable: with 18 copies at once on a machine at load average 50,
///   15 runs of 2,160 passed on a watcher that had called `_exit(74)` 42 to
///   82 ms before its launcher left, the launcher alive throughout. Hence
///   the word, and a look three times the longest of those lags.
#[cfg(unix)]
const PARENT_DEATH_LIVE_LOOK: std::time::Duration = std::time::Duration::from_millis(250);

/// Subprocess fixture for `leaked_commit_writer_cannot_hide_parent_death` and
/// `commit_eof_from_a_live_parent_stops_the_watcher_with_74`. Running normally
/// is a no-op; the outer test selects launcher, watcher or reader mode through
/// command-local environment, never process-global test mutation.
///
/// THE HAND-OVER IS EXPLICIT, AND THE WATCHER CHECKS IT. Until 2026-10-13 the
/// launcher cleared `FD_CLOEXEC` on its pipe and spawned the watcher with a
/// bare std `Command`, which hands the pipe over only on a std whose spawn
/// keeps every unflagged descriptor. Trust's std from seal `62f89f11` on
/// gives a `Command` child its stdio and nothing else (the SP-1 strip: on
/// Darwin `POSIX_SPAWN_CLOEXEC_DEFAULT` on its `posix_spawn` path, and every
/// descriptor above stdio marked close-on-exec on its fork path), so the
/// watcher wrapped two numbers that were not open, and the production watch
/// fail-stopped at its first poll (`POLLNVAL`) with the launcher alive. The
/// outer test then passed all the same, on the exit of a watcher that had
/// watched nothing — or failed, about once in 280 runs with 18 at once, when
/// that exit beat the ready marker. MEASURED on such a std (2026-10-13): a
/// watch whose liveness read cannot see the parent's death (`still_alive`
/// forced true) passed 5 runs of 5 on that fixture and fails 5 of 5 on this
/// one.
///
/// So the launcher keeps both ends close-on-exec and hands each over with
/// [`crate::test_alone::hand`]: a `pre_exec` hook that clears the flag on
/// exactly that number in the forked child, as the fork lane does for a real
/// successor (`app_update_handoff::fork_lane_child_holds`), which works under
/// every std. The watcher takes each with [`crate::test_alone::handed`],
/// which refuses to go on unless the number holds the very pipe end the
/// launcher named (by `fstat`), and the launcher leaves only on the watcher's
/// own word that it still watches, its parent alive
/// ([`PARENT_DEATH_LIVE_LOOK`]).
///
/// WHAT THIS FIXTURE'S WATCHER SHOWED ABOUT THE KERNEL (Darwin 25.6.0, arm64,
/// 2026-10-13; the test binary as it was before this change, under a `dyld`
/// interposer that handed the pipe over and logged each process's own
/// `_exit`): as the parent exits, [`read_process_birth`] stops answering for
/// it BEFORE its child's parent link moves. Of 5,768 watchers that left by
/// `still_alive() == false`, the 10 that left within 0.85 ms of the
/// launcher's `_exit` call all still had `getppid()` naming it, as had 6 of
/// the 22 that left 0.85 to 1.6 ms after it and none of the 5,736 that left
/// later; and for each of the 2,118 whose last birth read was logged, libproc
/// had answered `ESRCH`. So `getppid() == parent` beside a false
/// `still_alive()` is no evidence that the parent was alive: a second read of
/// its birth record is. `read_process_birth`'s docs stated the opposite order
/// until then; the protocol leans on neither.
#[test]
#[cfg(unix)]
fn commit_parent_death_helper() {
    let Ok(mode) = std::env::var(PARENT_DEATH_MODE) else {
        return;
    };
    if mode == "launcher" {
        let marker = std::path::PathBuf::from(
            std::env::var_os(PARENT_DEATH_MARKER).expect("launcher marker"),
        );
        let ready = std::path::PathBuf::from(
            std::env::var_os(PARENT_DEATH_READY).expect("launcher ready marker"),
        );
        let watched = std::path::PathBuf::from(
            std::env::var_os(PARENT_DEATH_WATCHED).expect("launcher watched marker"),
        );
        let mut raw = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "commit pipe");
        // Close-on-exec in this process, as the outgoing window keeps its own
        // ends: the hand-over below is then the one way they reach the
        // watcher, whichever std spawns it.
        assert!(aterm_pty::set_cloexec(raw[0], true).is_ok());
        assert!(aterm_pty::set_cloexec(raw[1], true).is_ok());

        let mut watcher =
            crate::test_alone::StandIn::helper(module_path!(), "commit_parent_death_helper");
        // The watcher's stderr goes where the outer test reads it, not into
        // this launcher's scratch, which goes when this launcher does: the
        // fail-stop's line there says which ending the watch took.
        let said = std::fs::File::create(
            std::env::var_os(PARENT_DEATH_SAID).expect("the watcher's stderr path"),
        )
        .expect("the watcher's stderr file");
        watcher
            .command()
            .env(PARENT_DEATH_MODE, "watcher")
            .env(PARENT_DEATH_READY, &ready)
            .env(PARENT_DEATH_EXPECTED_PID, std::process::id().to_string())
            .stderr(said);
        // THE HAND-OVER: both ends, each at its own number, each named with
        // what it is so that the watcher can check what arrived.
        crate::test_alone::hand(watcher.command(), PARENT_DEATH_READ_FD, raw[0]);
        crate::test_alone::hand(watcher.command(), PARENT_DEATH_WRITE_FD, raw[1]);
        let mut watcher = [watcher.spawn()];
        std::fs::write(&marker, watcher[0].id().to_string()).expect("publish watcher pid");

        // Waits on a re-exec of the whole debug TEST BINARY: a hang detector,
        // sleeping between looks so as not to compete with the very child it
        // waits for. A watcher that ends first never armed: it failed before its
        // marker, or ran no test of its name.
        crate::test_alone::until_or_ended(
            "the watcher arms parent-liveness detection",
            &mut watcher,
            || ready.exists(),
        );
        // ARMED IS NOT ENOUGH: IT MUST WATCH A LIVE PARENT WITHOUT STOPPING,
        // AND STILL BE WATCHING WHEN THAT PARENT GOES. A watcher that ended
        // while this launcher lived did not end of its parent's death, and
        // the outer test, which sees only that the watcher is gone, would
        // take that end for the fail-stop it is about. So this process lives
        // until the watcher says, some of its watch's ticks on
        // ([`PARENT_DEATH_LIVE_LOOK`]), that it still runs: ITS word, not a
        // look at it from here, which a watcher that has called `_exit` passes
        // for as long as the kernel takes to make that exit waitable. One
        // that ended instead is reported here, with its status. What happens
        // after that word is the subject.
        crate::test_alone::until_or_ended(
            "the watcher says it still watches its live parent, this launcher — whatever ended \
             it, it was not its parent's death",
            &mut watcher,
            || watched.exists(),
        );
        // Deliberately do not wait: exiting this launcher is the parent-death
        // cut. The watcher holds BOTH pipe ends, so EOF alone cannot help.
        let [watcher] = watcher;
        watcher.detach();
        aterm_pty::close_fd(raw[0]);
        aterm_pty::close_fd(raw[1]);
        return;
    }

    // A WATCHER holds both pipe ends and its parent dies. The other three hold
    // the read end alone, as a real successor does, and their parent lives: a
    // READER's closes the writer
    // (`commit_eof_from_a_live_parent_stops_the_watcher_with_74`), a DEAF one's
    // writes a Commit once the stand-in's event loop has dropped its receiver
    // (`a_commit_nobody_can_take_stops_the_watcher_with_74_its_pipe_closed_first`),
    // and a REFUSER's writes another attempt's Commit
    // (`a_mismatched_commit_stops_the_successor_with_74`).
    assert!(
        matches!(mode.as_str(), "watcher" | "reader" | "deaf" | "refuser"),
        "a parent-death stand-in is a launcher, a watcher, a reader, a deaf one or a refuser: \
         {mode}"
    );
    let expected_parent: libc::pid_t = std::env::var(PARENT_DEATH_EXPECTED_PID)
        .expect("expected parent")
        .parse()
        .expect("numeric parent pid");
    // THE HAND-OVER, CHECKED BEFORE ANYTHING IS ARMED ON IT. `CommitReceiver::raw`
    // takes any descriptor (production reaches it through
    // `take_commit_fd_from`, which refuses a number that is not open), and a
    // watch on a number that is not this pipe stops at once, its parent
    // alive, for a reason that is not parent death. `handed` refuses each
    // number unless it holds the very pipe end the launcher named: a watcher
    // that holds anything else says THE HAND-OVER FAILED and arms nothing.
    // Holding the writer is the Darwin CLOEXEC-leak regression, and by the
    // mode, never by what happens to be in the environment: a watcher that
    // was handed no writer must fail here, not turn into a reader whose
    // parent's exit is an end of file.
    let read_fd = crate::test_alone::handed(PARENT_DEATH_READ_FD);
    let _leaked_writer =
        (mode == "watcher").then(|| crate::test_alone::handed(PARENT_DEATH_WRITE_FD));
    // Attest through the real admission path: the launcher is still blocked
    // on our readiness marker, so it is a live creator and the birth record
    // captured here is provably its own. That makes this fixture exercise
    // the SAME witness production is admitted with.
    let parent = attest_handoff_parent(expected_parent, None)
        .expect("the launcher fixture is alive and is this process's parent");
    // Everything of the fixture's own that can fail is done before the ready
    // marker, except the word that follows it, and the parent waits for both:
    // a stand-in that ends before that word is its parent's to report, and
    // one that ends after it has only the watch, and the trace and the panic
    // under `receive_commit`, to end by.
    let outlived =
        std::path::PathBuf::from(std::env::var_os(PARENT_DEATH_OUTLIVED).expect("outlived trace"));
    let watched =
        std::path::PathBuf::from(std::env::var_os(PARENT_DEATH_WATCHED).expect("watched marker"));
    let receiver = CommitReceiver::raw(read_fd, true, parent)
        .start_watch()
        .expect("start commit parent watch");
    std::fs::write(
        std::env::var_os(PARENT_DEATH_READY).expect("ready marker"),
        b"armed",
    )
    .expect("publish watcher readiness");
    // STILL WATCHING, MANY TICKS ON, THE PARENT ALIVE. The watch has run on
    // its own thread since `start_watch`, so this one is free to let it have
    // those ticks ([`PARENT_DEATH_LIVE_LOOK`]) and then say that the process
    // still runs. The parent, which waits for this word, makes its cut on it
    // and on nothing else; a watch that fail-stopped in those ticks took this
    // thread with it, and the word is never said.
    std::thread::sleep(PARENT_DEATH_LIVE_LOOK);
    // A DEAF stand-in's event loop is gone before its parent writes: the
    // receiver is dropped, as an App torn down mid-overlap drops it.
    let receiver = if mode == "deaf" {
        drop(receiver);
        None
    } else {
        Some(receiver)
    };
    std::fs::write(&watched, b"watching").expect("say the watch still runs");
    // Reached past the answer only when the watch did not end the process
    // where it must. Nobody reads the exit of an orphan, so the panic below is
    // seen by no one: the trace is what the outer test reads.
    let outlived_by = if let Some(receiver) = receiver {
        let answer = receiver.receive_commit(stand_in_proof("parent-death-regression"));
        // Only a REFUSER leaves through the event loop's own fail-stop. In the
        // other modes the watch must end the process before it answers, and a
        // stand-in that left here would hide that it did not.
        if mode == "refuser"
            && let Err(end) = answer
        {
            receiver.fail_stop_if_required(end);
        }
        format!("receive_commit answered {answer:?}, and the process went on")
    } else {
        std::thread::sleep(aterm_spec::wait::MUST_HAPPEN);
        "the watch outlived its event loop's receiver by a minute".to_string()
    };
    std::fs::write(&outlived, outlived_by).expect("leave the outlived trace");
    panic!("production watcher must fail-stop when no Commit it can take can arrive");
}

/// The adoption proof a parent-death stand-in waits for under `nonce`:
/// `parent-death-regression` is its own attempt's, any other another's.
#[cfg(unix)]
fn stand_in_proof(nonce: &str) -> AdoptionProof {
    adoption_proof(
        nonce,
        1,
        crate::build_info::git_commit(),
        &[1; 32],
        &[2; 32],
        &[],
    )
    .expect("test proof")
}

/// A stand-in successor in `mode` (`reader`, `deaf` or `refuser`:
/// [`commit_parent_death_helper`]) holding the read end of a fresh Commit pipe
/// alone, its parent this test, and still watching when this returns; and the
/// write end, which nothing else holds. `stderr` replaces the stand-in's
/// stderr file. Only from a test that runs [`crate::test_alone::alone`]:
/// [`crate::test_alone::hand`]'s hook makes the spawn a fork.
#[cfg(unix)]
fn watching_stand_in(
    mode: &str,
    scratch: &std::path::Path,
    stderr: Option<std::process::Stdio>,
) -> (crate::test_alone::Running, std::io::PipeWriter) {
    use std::os::fd::AsRawFd as _;
    let ready = scratch.join("reader.ready");
    let watched = scratch.join("reader.watched");
    // Both ends close-on-exec from birth (`std::io::pipe`): the hand-over is
    // the one way the read end reaches the stand-in, and the writer reaches
    // nobody.
    let (read, write) = std::io::pipe().expect("commit pipe");
    let mut reader =
        crate::test_alone::StandIn::helper(module_path!(), "commit_parent_death_helper");
    reader
        .command()
        .env(PARENT_DEATH_MODE, mode)
        .env(PARENT_DEATH_READY, &ready)
        .env(PARENT_DEATH_WATCHED, &watched)
        .env(PARENT_DEATH_OUTLIVED, scratch.join("reader.outlived"))
        .env(PARENT_DEATH_EXPECTED_PID, std::process::id().to_string());
    if let Some(stderr) = stderr {
        reader.command().stderr(stderr);
    }
    crate::test_alone::hand(reader.command(), PARENT_DEATH_READ_FD, read.as_raw_fd());
    let mut reader = [reader.spawn()];
    drop(read);
    crate::test_alone::until_or_ended("the stand-in arms its watch", &mut reader, || {
        ready.exists()
    });
    // Not vacuous: a stand-in that stopped before the cut did not stop of it.
    // (One that stops in the few milliseconds between its word and the cut,
    // and with 74, is not told from the cut's effect — but by its line.)
    crate::test_alone::until_or_ended(
        "the stand-in says it still watches, the writer open and its parent alive",
        &mut reader,
        || watched.exists(),
    );
    let [reader] = reader;
    (reader, write)
}

/// Whether `stderr` names `end` as the ending the fail-stop took
/// ([`fail_stop_without_commit`]): the token, not the sentence around it.
#[cfg(unix)]
fn says_fail_stop(stderr: &str, end: NoCommit) -> bool {
    stderr.contains(&end.to_string())
}

#[test]
#[cfg(unix)]
fn leaked_commit_writer_cannot_hide_parent_death() {
    let scratch = aterm_tempfile::TempDir::new().expect("a parent-death scratch dir");
    let marker = scratch.path().join("watcher.pid");
    let ready = scratch.path().join("watcher.ready");
    let outlived = scratch.path().join("watcher.outlived");
    let said = scratch.path().join("watcher.stderr");
    // The launcher, as a stand-in that must run exactly its helper test: it
    // armed the watcher, left only on the watcher's word that it still
    // watched a live parent, and its exit is the parent's death. (The watcher
    // inherits the names of that word and of the trace from it.)
    let mut launcher =
        crate::test_alone::StandIn::helper(module_path!(), "commit_parent_death_helper");
    launcher
        .command()
        .env(PARENT_DEATH_MODE, "launcher")
        .env(PARENT_DEATH_MARKER, &marker)
        .env(PARENT_DEATH_READY, &ready)
        .env(PARENT_DEATH_WATCHED, scratch.path().join("watcher.watched"))
        .env(PARENT_DEATH_OUTLIVED, &outlived)
        .env(PARENT_DEATH_SAID, &said);
    launcher.spawn().passed();
    let watcher: libc::pid_t = std::fs::read_to_string(&marker)
        .expect("watcher pid marker")
        .trim()
        .parse()
        .expect("numeric watcher pid");

    // The watcher must fail-stop once its parent is gone: a hang detector,
    // never a latency budget — a watcher the leaked writer blinds never ends.
    let gone = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || {
            // SAFETY: signal 0 only asks whether `watcher` names a process.
            let gone = unsafe { libc::kill(watcher, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            gone.then_some(())
        },
    );
    if gone.is_none() {
        // SAFETY: the watcher this test's launcher started, still running.
        unsafe { libc::kill(watcher, libc::SIGKILL) };
        panic!("leaked commit writer hid direct-parent death");
    }
    // GONE, AND BY WHICH WAY OUT. The watcher is an orphan by now, so its exit
    // status is nobody's to read and "gone" is all this process can see of
    // it. What the fixture pins around that: the watcher held the very pipe
    // (it armed), still ran a quarter of a second later with its parent
    // alive (it said so, and the launcher left on nothing else), did not
    // leave through its own code, which writes this trace first, and left
    // through the fail-stop, which says why before its `_exit`: that its
    // parent was gone. A signal says nothing, and any other ending (a
    // hang-up, a failed read) says itself. What remains is a verdict of
    // "gone" taken between that word and the launcher's own exit (the
    // launcher reads the word within 5 ms and goes): nothing here tells it
    // from the true one.
    // AND A PASS DOES NOT SHOW THAT THE WATCH RAN IN THAT TIME. On a machine
    // too busy to run the watch's thread for a quarter of a second it read no
    // liveness before the word, or had its verdict and not yet its `_exit`,
    // and a witness that calls a live parent dead then passes: the look is
    // weaker there, never red. Which status the fail-stop leaves with, and
    // that an end of file stops the watch, is the next test's.
    assert!(
        !outlived.exists(),
        "the watcher outlived its watch: it ended, but not by the fail-stop ({})",
        std::fs::read_to_string(&outlived).unwrap_or_default()
    );
    let said = std::fs::read_to_string(&said).unwrap_or_default();
    assert!(
        says_fail_stop(&said, NoCommit::ParentGone),
        "the watcher is gone, but not by a fail-stop that says its parent was gone; its \
         stderr:\n{said}"
    );
}

/// THE SAME WATCH FROM ITS OTHER SIDE: END OF FILE FROM A LIVE PARENT, AND THE
/// STATUS THE FAIL-STOP LEAVES WITH. The test above can show neither: its
/// watcher holds the writer on purpose, so no end of file reaches it, and it
/// is an orphan when it stops, so its status is nobody's to read. MEASURED
/// 2026-10-13: under a watch whose fail-stop leaves with 75, that test passed
/// 10 runs of 10 and so did every other test of this module; this one fails
/// 10 of 10. Here the stand-in holds the read end alone, as a real successor
/// does, and its parent is this test: it stays alive, waits for the
/// stand-in's word that it still watches, closes the writer with no Commit
/// written, and reads how its child ended — by the watch's `_exit(74)`.
///
/// WHICH `poll` BIT CARRIES THE END OF FILE IS NOT PINNED, and on Darwin
/// cannot be by a pipe: a watch that ignores `POLLHUP` passes this test too
/// (measured, Darwin 25.6.0: a closed writer reads as `POLLIN | POLLHUP`, and
/// the read's end of file stops the watch).
///
/// Alone: [`crate::test_alone::hand`]'s hook makes the spawn a fork, and the
/// writer must have no holder but this test for its close to be an end of
/// file.
#[test]
#[cfg(unix)]
fn commit_eof_from_a_live_parent_stops_the_watcher_with_74() {
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::TempDir::new().expect("a commit-eof scratch dir");
    let (reader, write) = watching_stand_in("reader", scratch.path(), None);
    // THE CUT: the last writer goes, and no Commit was written.
    drop(write);
    let ran = stopped_with_74(
        reader,
        "end of file before a Commit did not stop the watcher",
    );
    // The line says the end of file (Darwin: `POLLIN | POLLHUP`, then a read
    // of nothing) or the hang-up alone, whichever bit carried it.
    assert!(
        says_fail_stop(&ran.stderr, NoCommit::EndOfFile)
            || says_fail_stop(
                &ran.stderr,
                NoCommit::PipeEnded {
                    revents: libc::POLLHUP
                }
            ),
        "the fail-stop says that the pipe ended with no Commit; its stderr:\n{}",
        ran.stderr
    );
}

/// `stand_in` stops — a hang detector, never a latency budget: a watch that
/// does not take its ending never ends, its parent alive — and by the
/// fail-stop's `_exit(74)`; what it ran and wrote, for the caller to read.
#[cfg(unix)]
#[track_caller]
fn stopped_with_74(mut stand_in: crate::test_alone::Running, what: &str) -> crate::test_alone::Ran {
    let stopped = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || stand_in.exited().then_some(()),
    );
    assert!(stopped.is_some(), "{what}");
    let ran = stand_in.selected();
    assert_eq!(
        ran.status.and_then(|status| status.code()),
        Some(74),
        "the fail-stop leaves with 74, and by `_exit`: no panic, no answer to its \
         receiver\nstdout:\n{}\nstderr:\n{}",
        ran.stdout,
        ran.stderr
    );
    ran
}

/// THE WATCH'S OTHER EXIT, AND THE READ END CLOSED BEFORE IT SAYS WHY. A whole
/// Commit reaches a stand-in whose event loop has dropped its receiver: the
/// watch has nowhere to hand it, and must fail-stop with 74 rather than go on
/// readerless, saying that the receiver was gone.
///
/// And by then the pipe's read end is closed. The stand-in's stderr is a pipe
/// this test fills before the Commit, so the fail-stop's line blocks until
/// this test drains it; while it blocks, a second Commit written here fails
/// with `EPIPE`, which is what sends a real parent down its
/// `commit_write_failed` rejection with its sessions kept. A watch that kept
/// the read end open until `_exit` (as it did until 2026-10-14) would take
/// that write instead, and its parent would `_exit(0)` beside a successor
/// that leaves too. The failed write also shows this process survives it:
/// the Rust runtime leaves `SIGPIPE` ignored, as in the window that writes a
/// real Commit.
///
/// Alone, as [`commit_eof_from_a_live_parent_stops_the_watcher_with_74`], and
/// because its stderr pipe must have no writer but the stand-in and this test.
#[test]
#[cfg(unix)]
fn a_commit_nobody_can_take_stops_the_watcher_with_74_its_pipe_closed_first() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::TempDir::new().expect("a receiver-gone scratch dir");
    let (said, said_by_stand_in) = std::io::pipe().expect("the stand-in's stderr pipe");
    let filler = said_by_stand_in
        .try_clone()
        .expect("a writer to fill it with");
    let (stand_in, mut write) =
        watching_stand_in("deaf", scratch.path(), Some(said_by_stand_in.into()));
    // FULL, so the fail-stop's line blocks. The stand-in shares this open
    // pipe, so it is nonblocking only while it is filled, and the stand-in
    // writes nothing meanwhile (its watch idles until the Commit).
    let set_nonblocking = |fd: i32, on: bool| {
        // SAFETY: flag reads and writes on a descriptor this test holds open.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
    };
    set_nonblocking(filler.as_raw_fd(), true);
    loop {
        match (&filler).write(&[0; 512]) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("fill the stand-in's stderr: {e}"),
        }
    }
    while (&filler).write(&[0]).is_ok() {}
    set_nonblocking(filler.as_raw_fd(), false);
    drop(filler);
    // This test's own writer, nonblocking (nobody else holds it), so a write
    // the pipe cannot take answers instead of waiting.
    set_nonblocking(write.as_raw_fd(), true);
    let commit = stand_in_proof("parent-death-regression").to_commit_wire();
    write
        .write_all(&commit)
        .expect("the Commit the stand-in cannot take");
    // THE SECOND COMMIT FAILS WHILE THE STAND-IN STILL RUNS. A hang
    // detector: until the watch has read the first and closed, this one
    // lands (the stand-in never reads it).
    let mut stand_in = stand_in;
    let refused = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || match write.write(&commit) {
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(e) => Some(e),
        },
    );
    let refused = refused.expect(
        "a Commit written after the watch ended was taken for a minute: the read end stayed \
         open while the fail-stop said why",
    );
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::EPIPE),
        "a Commit with no reader fails with EPIPE: {refused}"
    );
    assert!(
        !stand_in.exited(),
        "the stand-in is still saying why (its stderr is full) when its read end is already \
         closed"
    );
    // Drained, the line lands and the fail-stop leaves.
    let drain = std::thread::spawn(move || {
        let mut text = Vec::new();
        (&said).read_to_end(&mut text).map(|_| text)
    });
    let ran = stopped_with_74(
        stand_in,
        "a Commit nobody could take did not stop the watcher",
    );
    let text = drain
        .join()
        .expect("the drain thread")
        .expect("the stand-in's stderr");
    let text = String::from_utf8_lossy(&text).replace('\0', "");
    assert!(
        says_fail_stop(&text, NoCommit::ReceiverGone),
        "the fail-stop says that the event loop's receiver was gone; its stderr, less the \
         fill:\n{text}\nstdout:\n{}",
        ran.stdout
    );
}

/// THE EVENT LOOP'S EXIT ([`CommitReceiver::fail_stop_if_required`]): a whole
/// Commit for another attempt reaches the stand-in. The watch hands it over,
/// the event loop finds it is not this attempt's, and the successor
/// fail-stops with 74, saying so.
///
/// Alone, as [`commit_eof_from_a_live_parent_stops_the_watcher_with_74`].
#[test]
#[cfg(unix)]
fn a_mismatched_commit_stops_the_successor_with_74() {
    use std::io::Write as _;
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::TempDir::new().expect("a mismatched-commit scratch dir");
    let (refuser, mut write) = watching_stand_in("refuser", scratch.path(), None);
    write
        .write_all(&stand_in_proof("another-attempt").to_commit_wire())
        .expect("another attempt's Commit");
    let ran = stopped_with_74(refuser, "a mismatched Commit did not stop the successor");
    assert!(
        says_fail_stop(&ran.stderr, NoCommit::Mismatched),
        "the fail-stop says that the Commit was not this attempt's; its stderr:\n{}",
        ran.stderr
    );
}

// ======================================================================
// THE HAPPY PATH. Until 2026-07 there was no test anywhere that a full
// seamless handoff COMPLETES — every test proved a way for it to fail
// closed, and the RFC said in as many words that the models prove safety
// but "do not prove the happy path completes". In the field it never
// completed: not once, across three releases. These tests drive the real
// handshake end to end and assert SUCCESS, plus the three ways it must
// still fail closed.
// ======================================================================

/// One full outgoing-side preparation, ready to be consumed by the child
/// half of the same process. `layout_wire_override` writes a DIFFERENT byte
/// string to the layout sidecar than this binary's codec would produce —
/// standing in for "the outgoing process was a different build".
#[cfg(unix)]
pub(super) struct StagedHandoff {
    pub(super) manifest_path: std::path::PathBuf,
    pub(super) nonce: String,
    pub(super) fds_wire: String,
    pub(super) expected: AdoptionProof,
    /// The screen commitment the parent took over the bytes it wrote.
    pub(super) screen_digest: [u8; 32],
    slaves: Vec<i32>,
    /// The scratch HOME and runtime dir, put back and removed by `teardown`.
    scratch: ScratchEnv,
}

/// What a control-carry stage hands the outgoing process in place of the
/// default prompt screens: each session's checkpoint (by index), its
/// sidecar JSON, and the turn-id count the manifest carries.
#[cfg(unix)]
pub(super) struct StageCarry {
    pub(super) screens: Vec<aterm_core::terminal::TerminalCheckpoint>,
    pub(super) controls: Vec<(u64, Vec<u8>)>,
    pub(super) next_turn_id: Option<u64>,
    /// Each session's foreground holder, as the park capture stamps it on
    /// the manifest record (`SessionRecord::fg_holder`).
    pub(super) fg_holders: Vec<(u64, i32)>,
    /// Each session's render debt, as the park capture stamps it on the
    /// manifest record (`SessionRecord::render_debt`, R08).
    pub(super) render_debts: Vec<(u64, aterm_core::terminal::RenderDebt)>,
}

#[cfg(unix)]
pub(super) fn stage_outgoing_handoff(
    label: &str,
    sessions: usize,
    layout_wire_override: Option<&dyn Fn(&str) -> String>,
) -> StagedHandoff {
    stage_outgoing_handoff_with(label, sessions, layout_wire_override, None)
}

/// As [`stage_outgoing_handoff`], but the outgoing process carries the
/// given screens and CONTROL CARRY (one session per checkpoint).
#[cfg(unix)]
pub(super) fn stage_carry_handoff(label: &str, carry: &StageCarry) -> StagedHandoff {
    stage_outgoing_handoff_full(
        label,
        carry.screens.len(),
        None,
        None,
        Some(carry),
        &[],
        None,
    )
}

/// The HISTORY CARRY's stamp, as the handoff worker runs it just before
/// the manifest is written: the manifest, the control dir, the nonce.
#[cfg(unix)]
pub(super) type HistoryStamp<'a> = &'a dyn Fn(&mut SessionHandoff, &std::path::Path, &str);

/// As [`stage_carry_handoff`], with the HISTORY CARRY stamped onto the
/// manifest by `history` (`handoff_history::stamp_manifest`, run in the
/// worker's place with the control dir and the attempt nonce).
#[cfg(unix)]
pub(super) fn stage_history_handoff(
    label: &str,
    carry: &StageCarry,
    history: HistoryStamp<'_>,
) -> StagedHandoff {
    stage_outgoing_handoff_full(
        label,
        carry.screens.len(),
        None,
        None,
        Some(carry),
        &[],
        Some(history),
    )
}

/// As [`stage_carry_handoff`], with the producer's REPAINT set: the local
/// ids (session indices) its capture carried below the exact rungs, which
/// `write_outgoing` marks `ScreenCarry::repaint`.
#[cfg(unix)]
pub(super) fn stage_ladder_handoff(
    label: &str,
    carry: &StageCarry,
    repaint: &[u64],
) -> StagedHandoff {
    stage_outgoing_handoff_full(
        label,
        carry.screens.len(),
        None,
        None,
        Some(carry),
        repaint,
        None,
    )
}

/// As [`stage_outgoing_handoff`], but `meta_wire_override` also rewrites the
/// `CheckpointMeta` JSON the outgoing process publishes — standing in for
/// "the outgoing process's meta codec was a different version". The parent's
/// screen commitment is recomputed over the REWRITTEN bytes, because that is
/// what a differently-versioned parent would have hashed.
#[cfg(unix)]
fn stage_outgoing_handoff_with(
    label: &str,
    sessions: usize,
    layout_wire_override: Option<&dyn Fn(&str) -> String>,
    meta_wire_override: Option<&dyn Fn(&str) -> String>,
) -> StagedHandoff {
    stage_outgoing_handoff_full(
        label,
        sessions,
        layout_wire_override,
        meta_wire_override,
        None,
        &[],
        None,
    )
}

#[cfg(unix)]
fn stage_outgoing_handoff_full(
    label: &str,
    sessions: usize,
    layout_wire_override: Option<&dyn Fn(&str) -> String>,
    meta_wire_override: Option<&dyn Fn(&str) -> String>,
    carry: Option<&StageCarry>,
    repaint: &[u64],
    history: Option<HistoryStamp<'_>>,
) -> StagedHandoff {
    // Every caller holds ENV_LOCK for its whole body.
    let scratch = ScratchEnv::home(label);
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");

    let mut slaves = Vec::new();
    let mut records = Vec::new();
    let mut live = Vec::new();
    let mut screens = Vec::new();
    for index in 0..sessions {
        // Both ends CLOSE-ON-EXEC FROM BIRTH (`aterm_pty::open_pty_pair`).
        // The test binary runs hundreds of tests on parallel threads, and
        // any of them that spawns a process while this fixture's PTYs are
        // inheritable hands the child a copy: the master then never
        // closes, and `a_refused_handoff_closes_every_master_it_was_handed`
        // read its slave as `leaked` (the full-suite gate, 2026-09-26; 3/3
        // green alone). `openpty` + `F_SETFD` narrowed that to the few
        // instructions between the two, which a loaded machine still
        // stretched; the pair's own `O_CLOEXEC` opens leave none. The
        // handoff claims a descriptor by liveness (`F_GETFD`), never by
        // its CLOEXEC bit, so nothing here needs them inheritable.
        let (master, slave) = aterm_pty::open_pty_pair()
            .unwrap_or_else(|e| panic!("open a close-on-exec pty pair {index}: {e}"));
        slaves.push(slave);
        let local_id = index as u64;
        let mut record = SessionRecord {
            // What the park capture stamps (`stamp_fg_holders`).
            fg_holder: carry.and_then(|c| {
                c.fg_holders
                    .iter()
                    .find(|(id, _)| *id == local_id)
                    .map(|(_, holder)| *holder)
            }),
            ..zsh_record(local_id, &format!("s-e2e-{index}"))
        };
        // What the park capture stamps (`stamp_render_debts`).
        record.carry_render_debt(carry.and_then(|c| {
            c.render_debts
                .iter()
                .find(|(id, _)| *id == local_id)
                .map(|(_, debt)| *debt)
        }));
        records.push(record);
        live.push((local_id, master, 4000 + index as i32));
        if let Some(carry) = carry {
            screens.push((local_id, carry.screens[index].clone()));
            continue;
        }
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal
            .process(format!("\x1b[1;38;5;202muser@mac\x1b[0m ~ % session {index}").as_bytes());
        screens.push((
            local_id,
            terminal.checkpoint_visible().expect("parser is Ground"),
        ));
    }
    let mut manifest = SessionHandoff {
        window: Some(WindowCarry {
            rows: 24,
            cols: 80,
            outer_x: Some(120),
            outer_y: Some(64),
            inner_px: None,
            presence_row: false,
            status_bar_rows: 0,
            update_verified_unix_ms: None,
            messages: Vec::new(),
            next_message_id: 0,
            font_px_milli: None,
            font_reset_px_milli: None,
            font_size_milli: None,
            font_reset_size_milli: None,
            update_health_said: Vec::new(),
            messages_captured_unix_ms: None,
        }),
        // Tokenless connection carry (§1.4#6): a full `both` set 0 → 1
        // whenever the stage has two shells, `(src, dst, op)`-sorted like
        // the live projection. A single-session stage carries nothing.
        connections: if sessions >= 2 {
            ["read-screen", "signal", "write-input"]
                .into_iter()
                .map(|op| ConnectionCarry {
                    src: "s-e2e-0".to_string(),
                    dst: "s-e2e-1".to_string(),
                    op: op.to_string(),
                })
                .collect()
        } else {
            Vec::new()
        },
        next_turn_id: carry.and_then(|c| c.next_turn_id),
        outgoing_build: Some(own_build()),
        ..bare_manifest(records)
    };
    let fds = HandoffFds {
        entries: live.clone(),
    };
    // The nonce is the CALLER'S (2026-09-19): the writer names every file
    // from it and writes it as the manifest's first line, so a successor
    // launched with the path before the write can authenticate the file.
    let attempt_nonce = mint_outgoing_nonce();
    if let Some(history) = history {
        history(&mut manifest, &dir, &attempt_nonce);
    }
    let outgoing = write_outgoing(
        &manifest,
        &fds,
        &screens,
        repaint,
        manifest.window.clone(),
        carry.map_or(&[][..], |c| &c.controls[..]),
        &attempt_nonce,
    )
    .expect("outgoing handoff is written");
    assert_eq!(
        outgoing.nonce, attempt_nonce,
        "the writer echoes the supplied nonce"
    );
    assert_eq!(
        std::path::Path::new(&outgoing.manifest_path),
        outgoing_manifest_path(&attempt_nonce)
            .expect("the control dir exists")
            .as_path(),
        "the launch environment and the writer derive the same manifest path"
    );
    assert!(
        std::fs::read_to_string(&outgoing.manifest_path)
            .expect("read manifest")
            .starts_with(&format!("{attempt_nonce}\n")),
        "the supplied nonce is the manifest's first line"
    );

    // The PARENT's layout commitment is a pure function of the bytes it
    // writes to the sidecar — `restore::write_to` writes exactly
    // `to_toml()`, and `layout_digest` hashes exactly that.
    let ids = live.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    let layout = exact_legacy_layout(&ids);
    let layout_wire = layout.to_toml().expect("serialize layout");
    let layout_wire = layout_wire_override.map_or(layout_wire.clone(), |f| f(&layout_wire));
    let layout_path = std::path::Path::new(&outgoing.manifest_path).with_extension("layout.toml");
    std::fs::write(&layout_path, layout_wire.as_bytes()).expect("layout sidecar");
    let parent_layout_digest = layout_wire_digest(&layout_wire).expect("parent layout digest");

    // A differently-versioned outgoing process would have published — and
    // hashed — different meta bytes. Rewrite the manifest it just wrote, then
    // take the parent's commitment over exactly those bytes.
    let mutated_metas: Vec<(u64, String)> = meta_wire_override.map_or_else(Vec::new, |f| {
        // Round-trip through the manifest codec rather than patching TOML
        // text: the MANIFEST bytes are not hashed by anyone (only
        // `ScreenCarry::meta` is), so re-serializing it here is sound and
        // immune to how the codec escapes an embedded JSON string.
        // The published file is `<nonce>\n<manifest toml>`.
        let body = std::fs::read_to_string(&outgoing.manifest_path).expect("read manifest");
        let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
        let mut published = SessionHandoff::from_toml(toml).expect("parse the published manifest");
        let mut mutated = Vec::new();
        for rec in &mut published.sessions {
            let carry = rec.screen.as_mut().expect("every session carries a screen");
            let next = f(&carry.meta);
            assert_ne!(
                next, carry.meta,
                "the override must actually change the meta"
            );
            carry.meta = next.clone();
            mutated.push((rec.local_id, next));
        }
        let republished = format!(
            "{file_nonce}\n{}",
            published.to_toml().expect("reserialize manifest")
        );
        std::fs::write(&outgoing.manifest_path, republished.as_bytes())
            .expect("republish the rewritten manifest");
        mutated
    });
    let parent_screen_digest = if mutated_metas.is_empty() {
        outgoing.screen_digest
    } else {
        let mut entries = screens
            .iter()
            .map(|(local_id, cp)| ScreenWireEntry {
                local_id: *local_id,
                meta: mutated_metas
                    .iter()
                    .find(|(id, _)| id == local_id)
                    .map(|(_, meta)| meta.as_bytes())
                    .expect("every session has a rewritten meta"),
                grid: &cp.grid,
                alt_grid: cp.alt_grid.as_deref(),
            })
            .collect::<Vec<_>>();
        screen_wire_digest(&mut entries).expect("parent screen digest over the rewritten wire")
    };

    let expected = adoption_proof(
        &outgoing.nonce,
        own_build(),
        crate::build_info::git_commit(),
        &parent_layout_digest,
        &parent_screen_digest,
        &live,
    )
    .expect("parent expectation");

    StagedHandoff {
        manifest_path: std::path::PathBuf::from(&outgoing.manifest_path),
        nonce: outgoing.nonce,
        fds_wire: outgoing.fds_wire,
        expected,
        screen_digest: parent_screen_digest,
        slaves,
        scratch,
    }
}

#[cfg(unix)]
impl StagedHandoff {
    pub(super) fn publish_env(&self, ready_write: i32, commit_read: i32, target: Option<String>) {
        let layout_path = self.manifest_path.with_extension("layout.toml");
        // SAFETY: `getppid` is a side-effect-free libc getter.
        let parent_pid = unsafe { libc::getppid() };
        // Serialized by ENV_LOCK, held by the calling test.
        aterm_log::env::set(ENV_MANIFEST, &self.manifest_path);
        aterm_log::env::set(ENV_NONCE, &self.nonce);
        aterm_log::env::set(ENV_FDS, &self.fds_wire);
        aterm_log::env::set(ENV_LAYOUT, &layout_path);
        aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
        aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
        aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
        // The MODERN shape: publish the parent's birth record alongside its
        // pid, exactly as `outgoing_parent_env` does in production, so the
        // happy-path tests drive the witness a LaunchServices successor will
        // depend on rather than only the legacy pid-plus-parent-link one.
        match read_process_birth(parent_pid) {
            Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
            None => aterm_log::env::unset(ENV_PARENT_BIRTH),
        }
        match target {
            Some(value) => aterm_log::env::set(ENV_TARGET, value),
            None => aterm_log::env::unset(ENV_TARGET),
        }
    }

    /// Close the slaves, put HOME and the runtime dir back and remove the
    /// scratch ([`ScratchEnv`]). The masters are not the test's to close:
    /// once the wire names them they are the successor's, exactly as on the
    /// exec lane — the adoption closes each as it drops, and a refusal
    /// closes them at once.
    pub(super) fn teardown(self) {
        for fd in self.slaves {
            aterm_pty::close_fd(fd);
        }
        drop(self.scratch);
    }
}

#[cfg(unix)]
pub(super) fn pipe_pair(label: &str) -> (i32, i32) {
    let mut fds = [0i32; 2];
    // SAFETY: plain pipe(2) into a valid 2-slot out-array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "{label} pipe");
    (fds[0], fds[1])
}

/// Everything the child half holds at the instant it is ready to publish:
/// the proof it computed, the still-unfired ready channel it will publish
/// on, and the adopted session set that proof commits to. The three travel
/// together because a test that inspects one usually has to check it against
/// another (proof vs. adopted set, or signal-then-commit). The session set is
/// spelled with the crate-wide [`SessionIdentity`] alias, so the test oracle
/// and the production signatures it drives cannot drift on field ORDER.
#[cfg(unix)]
pub(super) type ReadyChild = (AdoptionProof, ReadySignal, Vec<SessionIdentity>);

/// Drive the CHILD half exactly as `run()` does: consume the manifest,
/// prove the target identity, adopt the ready channel, compute the proof.
#[cfg(unix)]
pub(super) fn child_proof() -> Option<ReadyChild> {
    child_proof_from(take_incoming()).map(|(ready, _)| ready)
}

/// [`child_proof`] from an already-consumed handoff, handing the adopted
/// sessions back too (their control carries are what a carry test reads).
#[cfg(unix)]
pub(super) fn child_proof_from(
    mut incoming: IncomingHandoff,
) -> Option<(ReadyChild, Vec<Adopted>)> {
    if incoming.adopted.is_empty() {
        return None;
    }
    let target = take_target_identity()?;
    let adopted_fds = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.master.raw())
        .collect::<Vec<_>>();
    let ready = take_ready_fd(
        incoming.nonce.clone(),
        incoming.layout_digest,
        incoming.screen_digest,
        Some(target),
        &adopted_fds,
    )?;
    let adopted = incoming
        .adopted
        .iter()
        .map(|item| (item.local_id, item.master.raw(), item.pid))
        .collect::<Vec<_>>();
    let proof = ready.proof(&adopted)?;
    Some((
        (proof, ready, adopted),
        std::mem::take(&mut incoming.adopted),
    ))
}

/// THE TEST THAT WAS MISSING. Parent prepares → child consumes, adopts and
/// proves → the two proofs MATCH → the parent's Commit wire is accepted by
/// the child's own `commit_wire_matches`. Multi-session so a subset or a
/// cross-wire cannot pass.
#[test]
#[cfg(unix)]
fn a_full_seamless_handoff_completes_and_commits() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("ok", 3, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));

    let (proof, ready, adopted) = child_proof().expect("the child adopts and proves");
    assert_eq!(adopted.len(), 3, "every session is adopted, none lost");
    assert_eq!(
        proof, staged.expected,
        "THE HANDOFF COMPLETES: the child's proof equals the parent's expectation"
    );
    assert!(ready.signal_proof(proof), "ProofReady is published");

    // The parent's side of the wire: read ProofReady, then Commit.
    let mut wire = [0u8; READY_WIRE_LEN];
    // SAFETY: bounded read into a live fixed buffer from our own pipe.
    let read = unsafe { libc::read(ready_read, wire.as_mut_ptr().cast(), wire.len()) };
    assert_eq!(
        read, READY_WIRE_LEN as isize,
        "the whole proof wire arrives"
    );
    assert_eq!(
        AdoptionProof::from_wire(&wire),
        Some(staged.expected),
        "the parent recognizes the proof it was waiting for"
    );
    assert!(
        staged
            .expected
            .commit_wire_matches(&staged.expected.to_commit_wire()),
        "the exact Commit the parent would send is the one the child accepts"
    );
    assert_ne!(
        staged.expected.to_commit_wire(),
        staged.expected.to_wire(),
        "Commit and ProofReady are distinct authorities on the wire"
    );

    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// Design §1.4#6: the tokenless connection triples ride the authenticated
/// manifest and come out of `take_incoming` VERBATIM alongside a real
/// adoption — and nothing token-shaped rides with them (the published wire
/// carries no bearer-token-width hex blob).
#[test]
#[cfg(unix)]
fn take_incoming_carries_the_tokenless_connection_triples() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("conncarry", 2, None);
    // §1.4#3 on the PUBLISHED wire: an `EdgeToken` is 64 hex chars; no run
    // of that width may appear (sids are 20 hex — no false positive).
    let published =
        std::fs::read_to_string(&staged.manifest_path).expect("read the published manifest");
    assert!(
        !published
            .as_bytes()
            .windows(64)
            .any(|w| w.iter().all(u8::is_ascii_hexdigit)),
        "no bearer-token-width hex blob may ride the manifest"
    );
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));

    let incoming = take_incoming();
    assert_eq!(incoming.adopted.len(), 2, "the carry rides a REAL adoption");
    assert_eq!(
        incoming.connections,
        ["read-screen", "signal", "write-input"]
            .into_iter()
            .map(|op| ConnectionCarry {
                src: "s-e2e-0".to_string(),
                dst: "s-e2e-1".to_string(),
                op: op.to_string(),
            })
            .collect::<Vec<_>>(),
        "the staged triples come through verbatim, still tokenless"
    );

    // `take_incoming` consumed neither pipe end (only `take_ready_fd`/
    // `take_commit_fd` do): close all four.
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE REGRESSION GUARD FOR FIX #1. The outgoing process is a DIFFERENT
/// BUILD: its layout codec emitted `schema = 1`, which this binary's
/// `RestoreManifest::from_toml` accepts and then rewrites to `schema = 2`.
///
/// Hashing the WIRE (what the child does now) matches the parent exactly.
/// Re-serializing the parse (what the child did until 2026-07) produces a
/// different digest and therefore `AdoptionMismatch` — which is what every
/// handoff in the field actually died of. The second assertion below IS the
/// revert check: it computes the old child's digest and shows it differs.
#[test]
#[cfg(unix)]
fn a_handoff_across_a_version_boundary_still_completes() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let older_codec = |wire: &str| {
        assert!(wire.contains("schema = 2"), "today's wire: {wire}");
        wire.replacen("schema = 2", "schema = 1", 1)
    };
    let staged = stage_outgoing_handoff("xver", 1, Some(&older_codec));
    // What the OLD child would have committed to: re-serialize its parse.
    let old_wire = std::fs::read_to_string(staged.manifest_path.with_extension("layout.toml"))
        .expect("sidecar");
    let reserialized = crate::restore::RestoreManifest::from_toml(&old_wire)
        .expect("the child still ACCEPTS the older wire")
        .to_toml()
        .expect("reserialize");
    assert_ne!(
        reserialized, old_wire,
        "precondition: this binary's codec is NOT a fixed point on the older wire"
    );
    assert_ne!(
        layout_wire_digest(&reserialized),
        layout_wire_digest(&old_wire),
        "REVERTING fix #1 (re-serializing the parse) provably breaks the proof"
    );

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let (proof, _ready, _adopted) = child_proof().expect("the child adopts and proves");
    assert_eq!(
        proof, staged.expected,
        "hashing the WIRE makes the two sides agree ACROSS the version boundary"
    );

    drop(_ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE REGRESSION GUARD FOR THE SCREEN HALF OF FIX #1 — and it is a
/// SEPARATE guard on purpose. The layout test above rewrites only the layout
/// sidecar, so reverting the SCREEN commitment to the old
/// re-derive-from-the-rebuilt-checkpoint behaviour left every test green: the
/// screen half was covered vacuously. It no longer is.
///
/// Here the outgoing process is a NEWER build whose `CheckpointMeta` carries
/// one extra key. `CheckpointMeta` has no `deny_unknown_fields`, so this
/// binary parses the carry happily and silently DROPS the key — and would
/// then hash 17 fields where the parent hashed 18. Hashing the carried bytes
/// makes the two sides agree anyway; re-deriving from the parse cannot.
#[test]
#[cfg(unix)]
fn a_handoff_across_a_screen_meta_version_boundary_still_completes() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    // One additive key, exactly as a newer build's meta codec would emit it.
    let newer_meta_codec = |meta: &str| {
        assert!(meta.starts_with('{'), "meta json: {meta}");
        meta.replacen('{', "{\"scene_overlay\":true,", 1)
    };
    let staged = stage_outgoing_handoff_with("xvermeta", 2, None, Some(&newer_meta_codec));

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let (proof, _ready, adopted) = child_proof().expect("the child adopts and proves");
    assert_eq!(
        adopted.len(),
        2,
        "both sessions adopt across the meta boundary"
    );
    assert_eq!(
        proof, staged.expected,
        "hashing the CARRIED META BYTES makes the two sides agree across a \
         CheckpointMeta version boundary; re-deriving from the parse does not"
    );

    drop(_ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// Rewrite ONE session of an already-staged handoff the way an older
/// producer whose predicate differs from this build's would have published
/// it: `meta` rewrites that record's `CheckpointMeta` JSON and `grid` its
/// main grid sidecar. The parent's screen commitment and adoption
/// expectation are recomputed over the REWRITTEN bytes, because that is
/// what such a parent hashed — `screen_wire_digest` commits to the wire as
/// written and never asks whether the consumer will like what it says.
#[cfg(unix)]
pub(super) fn restage_session(
    staged: &mut StagedHandoff,
    local_id: u64,
    meta: &dyn Fn(&str) -> String,
    grid: &dyn Fn(Vec<u8>) -> Vec<u8>,
) {
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    let mut published = SessionHandoff::from_toml(toml).expect("parse the published manifest");
    let mut wire = Vec::new();
    for rec in &mut published.sessions {
        let carry = rec.screen.as_mut().expect("every session carries a screen");
        let grid_path = std::path::PathBuf::from(&carry.grid_file);
        let mut grid_bytes = std::fs::read(&grid_path).expect("grid sidecar");
        if rec.local_id == local_id {
            carry.meta = meta(&carry.meta);
            grid_bytes = grid(grid_bytes);
            std::fs::write(&grid_path, &grid_bytes).expect("rewrite the grid sidecar");
        }
        let alt = carry
            .alt_grid_file
            .as_ref()
            .map(|path| std::fs::read(path).expect("alt sidecar"));
        wire.push((rec.local_id, carry.meta.clone(), grid_bytes, alt));
    }
    let republished = format!(
        "{file_nonce}\n{}",
        published.to_toml().expect("reserialize manifest")
    );
    std::fs::write(&staged.manifest_path, republished.as_bytes())
        .expect("republish the rewritten manifest");
    let mut entries = wire
        .iter()
        .map(|(id, meta, grid, alt)| ScreenWireEntry {
            local_id: *id,
            meta: meta.as_bytes(),
            grid,
            alt_grid: alt.as_deref(),
        })
        .collect::<Vec<_>>();
    staged.screen_digest =
        screen_wire_digest(&mut entries).expect("parent screen digest over the rewritten wire");
    let layout_wire = std::fs::read_to_string(staged.manifest_path.with_extension("layout.toml"))
        .expect("layout sidecar");
    let live = decode_fds_bounded(&staged.fds_wire)
        .expect("the staged fd wire decodes")
        .entries;
    staged.expected = adoption_proof(
        &staged.nonce,
        own_build(),
        crate::build_info::git_commit(),
        &layout_wire_digest(&layout_wire).expect("parent layout digest"),
        &staged.screen_digest,
        &live,
    )
    .expect("parent expectation");
}

/// THE HALT AND THE TITLES CROSS THE REAL WIRE (the round-four plan, items
/// 3 and 8). Three sessions are handed across `write_outgoing` and
/// `take_incoming` as a producer of this build writes them, then put back
/// through the spawn's own seams (`hydrate_adopted_engine`,
/// `seed_adopted_title`, `seed_adopted_fabric`):
///
/// * session 0 was halted by the FLEET over its owner's own hold (both
///   come back, each in its own slot) and runs a program that titled its
///   tab, named its icon and pushed its caller's title — all of it comes
///   back, the pop included;
/// * session 1 was halted by its OWNER and arrives as a producer from
///   before the title carry wrote it (no `title` key): the title its
///   layout leaf was captured with at the park labels the tab instead
///   (item 8a; round eight, U47, for why not the record's copy);
/// * session 2 carries a title value that does not read: its SCREEN still
///   adopts exactly (a bad title costs the title, never the screen), and
///   its layout leaf's title stands in.
///
/// The two halves sit on different sides of the proof, and the test says
/// which. A carried TITLE is part of the meta JSON, which the screen digest
/// hashes byte for byte: rewriting one moves the parent's screen digest and
/// so its adoption expectation (checked below), and the proof matches only
/// because the parent's commitment is recomputed over the retitled wire — a
/// title that changed between the park and the Commit would change the
/// digest the parent committed to. A HOLD rides on the session record,
/// outside both digests: stamping the holds after the expectation was fixed
/// leaves it, and the proof, unchanged.
///
/// FAILS WITHOUT THE FIX: every session comes back unheld and untitled.
#[test]
#[cfg(unix)]
fn a_hold_and_the_titles_cross_the_real_wire() {
    use aterm_core::terminal::TitleRepr;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let mut staged = stage_outgoing_handoff("haltwire", 3, None);
    // The producer's meta names an untitled engine as `"title":{}` — the
    // presence that tells the successor titles were carried.
    let titled = |meta: &str| {
        assert!(
            meta.contains("\"title\":{}"),
            "this build writes the key: {meta}"
        );
        meta.replace(
            "\"title\":{}",
            "\"title\":{\"window\":\"vim main.rs\",\"icon\":\"vim\",\
             \"stack\":[[\"\",\"the caller\"]]}",
        )
    };
    let before_the_carry = |meta: &str| meta.replace(",\"title\":{}", "");
    let unreadable = |meta: &str| meta.replace("\"title\":{}", "\"title\":[1,2,3]");
    let untitled_digest = staged.screen_digest;
    let untitled_expectation = staged.expected;
    restage_session(&mut staged, 0, &titled, &|grid| grid);
    // A title is INSIDE the screen digest: the meta bytes it lives in are
    // hashed as sent, so retitling moves the commitment and the expectation.
    assert_ne!(
        staged.screen_digest, untitled_digest,
        "a carried title is covered by the screen digest"
    );
    assert_ne!(staged.expected, untitled_expectation);
    restage_session(&mut staged, 1, &before_the_carry, &|grid| grid);
    restage_session(&mut staged, 2, &unreadable, &|grid| grid);
    // The park's capture stamps the holds onto the records — OUTSIDE both
    // digests, so the expectation fixed above is not recomputed, and the
    // proof below still matching it is what shows the holds are outside.
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    let mut published = SessionHandoff::from_toml(toml).expect("parse the manifest");
    published.sessions[0].hold = Some("fleet fabric-lost".to_string());
    // …beside the owner's own stop, which a fleet halt never replaces.
    published.sessions[0].more_holds = vec!["local mine".to_string()];
    published.sessions[1].hold = Some("local stop%20driving".to_string());
    std::fs::write(
        &staged.manifest_path,
        format!("{file_nonce}\n{}", published.to_toml().expect("toml")),
    )
    .expect("republish");

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(incoming.adopted.len(), 3, "every session adopts");
    assert!(
        incoming.adopted.iter().all(|a| !a.repaint),
        "a title, readable or not, never costs a screen"
    );
    let ((proof, ready, _), mut adopted) =
        child_proof_from(incoming).expect("the child adopts and proves");
    assert_eq!(
        proof, staged.expected,
        "the proof covers the retitled wire it was recomputed over, and no hold"
    );

    let hold = |reason: &str, origin: &str| crate::fabric::Hold {
        reason: reason.to_string(),
        origin: origin.to_string(),
    };
    let expected_hold = [
        vec![hold("fabric-lost", "fleet"), hold("mine", "local")],
        vec![hold("stop%20driving", "local")],
        vec![],
    ];
    for (a, want_hold) in adopted.iter_mut().zip(expected_hold) {
        let term = std::sync::Mutex::new(aterm_core::terminal::Terminal::new(24, 80));
        crate::spawn::hydrate_adopted_engine(
            &term,
            a.checkpoint.as_ref(),
            None,
            None,
            None,
            a.local_id,
            &mut a.history,
        );
        let seeded = crate::spawn::seed_adopted_title(&term, a.checkpoint.as_ref(), &a.title);
        let fabric = crate::fabric::SessionFabric::default();
        let timeline = std::sync::Mutex::new(crate::session_timeline::SessionTimeline::default());
        crate::spawn::seed_adopted_fabric(&fabric, &timeline, &a.topics, &a.holds, &a.fabric);
        assert_eq!(
            fabric.holds().standing().collect::<Vec<_>>(),
            want_hold,
            "session {}",
            a.local_id
        );
        let mut engine = term.lock().unwrap();
        match a.local_id {
            0 => {
                assert_eq!(
                    a.checkpoint.as_ref().and_then(|cp| cp.title.clone()),
                    Some(TitleRepr {
                        window: "vim main.rs".to_string(),
                        icon: "vim".to_string(),
                        stack: vec![(String::new(), "the caller".to_string())],
                        ..TitleRepr::default()
                    })
                );
                assert!(!seeded, "the carried title is the exact answer");
                assert_eq!(engine.title(), "vim main.rs");
                engine.process(b"\x1b[23;2t");
                assert_eq!(engine.title(), "the caller", "the pushed title pops back");
            }
            _ => {
                assert_eq!(
                    a.checkpoint.as_ref().map(|cp| cp.title.clone()),
                    Some(None),
                    "session {}: no title carried",
                    a.local_id
                );
                assert!(
                    seeded,
                    "session {}: the park's title labels the tab",
                    a.local_id
                );
                assert_eq!(
                    engine.title(),
                    format!("session-{}", a.local_id),
                    "session {}: the layout leaf's title, not the record's `zsh`",
                    a.local_id
                );
            }
        }
    }

    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// A WORKER'S TASK RECORD CROSSES THE REAL WIRE (the worker pillar,
/// 2026-10-15): the record's row rides the manifest beside the screen, outside
/// both digests; the successor's take puts it on the adopted session
/// (`Adopted::task`), and the seed `spawn_session` runs puts the same rev,
/// state and facts on that session's timeline. A session with no task
/// crosses with none.
///
/// FAILS WITHOUT THE FIX: the adopted worker reads `OK task -`, and a
/// manager re-armed past its rev on the successor takes it for never tasked.
#[test]
#[cfg(unix)]
fn a_workers_task_record_crosses_the_real_wire() {
    use crate::task_record::TaskState;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("taskwire", 2, None);
    // The park draws the record onto its session's manifest row
    // (`SessionHandoff::from_store`), outside both digests: the expectation
    // fixed above is not recomputed, and the proof below still matching it is
    // what shows the row is outside.
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    let mut published = SessionHandoff::from_toml(toml).expect("parse the manifest");
    let row = "rev=3 state=done from=ctl since=1000 result_bytes=12 attention=0 why=marker";
    published.sessions[0].task = Some(row.to_string());
    std::fs::write(
        &staged.manifest_path,
        format!("{file_nonce}\n{}", published.to_toml().expect("toml")),
    )
    .expect("republish");

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(
        ready_write,
        commit_read,
        Some(encode_target_identity(
            crate::build_info::build_number()
                .parse::<u64>()
                .unwrap_or(0),
            crate::build_info::git_commit(),
        )),
    );
    let incoming = take_incoming_as(ReceiverShape::Current);
    let ((proof, ready, _), adopted) =
        child_proof_from(incoming).expect("the child adopts and proves");
    assert_eq!(proof, staged.expected, "the task row is in no digest");
    let rows: Vec<Option<&str>> = adopted.iter().map(|a| a.task.as_deref()).collect();
    assert_eq!(rows, [Some(row), None], "the record rides its own session");
    let worker = crate::session_store::test_handle(adopted[0].local_id);
    crate::task_record::seed_carried(&worker.ctx, adopted[0].task.as_deref());
    let seeded = worker.ctx.timeline.lock().unwrap().task().cloned();
    let seeded = seeded.expect("the record crossed");
    assert_eq!(
        (seeded.rev, seeded.state, seeded.why, seeded.result_bytes),
        (3, TaskState::Done, Some("marker"), 12)
    );

    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// AN OLDER PRODUCER'S BACKGROUND TAB TAKES THE TITLE ITS ENGINE HAD AT THE
/// PARK, NOT THE RECORD'S STALE COPY (round eight of the update audit,
/// U47). A producer before the title carry (v0.98.0 and older) writes no
/// `title` key in the screen meta, and its record's title is the store's
/// copy, which it updates for the pane IN FRONT of a window only (its
/// active tab's focused terminal). The layout leaf of each tab carries the
/// engine's `(cwd, title)` as the park read them. RED before the fix: tab
/// 0, behind, was labelled with the record's `vim notes.md`, which its
/// program had since cleared.
///
/// The leaf's read can MISS: the capture tries the engine lock and writes
/// `(no cwd, "")` when it is busy (a control thread, a reflow), which is
/// also what a program that set neither reads as. For the pane in front
/// that leaf gives no answer and the record's copy, current there, stands
/// (the adversarial review of U47: before that, a busy read at the park
/// cost the front tab its title, which main had shown). The cases:
///
/// * tab 1 in front: tab 0 behind reads `(no cwd, "")` and stays untitled
///   (it falls to its folder); tab 1's read `~/proj` beats a record that
///   lags it (the shell retitled as the park began, before a redraw
///   published it);
/// * tab 0 in front reading `(no cwd, "")`: the record's `vim notes.md`
///   stands — the review's case;
/// * tab 0 in front reading `(a cwd, "")`: a read that happened, so the
///   cleared title is the answer, in front too;
/// * ONE split tab, its second pane focused, both reading `(no cwd, "")`:
///   the focused pane takes the record's copy, the other pane — in the
///   front tab, but not the pane the record kept current — stays untitled;
/// * NEGATIVE CONTROL: a layout that does not name every handed session is
///   not placed, and then the record's copy is all there is.
#[test]
#[cfg(unix)]
fn an_older_producers_tab_takes_its_park_title_not_the_records_stale_copy() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let before_the_carry = |meta: &str| meta.replace(",\"title\":{}", "");
    // The copies v0.98.0's store held for the two sessions.
    let records = ["vim notes.md", "cargo test"];
    let titles_after = |layout: &dyn Fn(&mut RestoreManifest)| {
        let mut staged = stage_outgoing_handoff("parktitle", 2, None);
        let layout_path = staged.manifest_path.with_extension("layout.toml");
        let mut placed = RestoreManifest::from_toml(
            &std::fs::read_to_string(&layout_path).expect("layout sidecar"),
        )
        .expect("the staged layout parses");
        layout(&mut placed);
        std::fs::write(&layout_path, placed.to_toml().expect("layout toml"))
            .expect("rewrite the layout sidecar");
        // Both screens as v0.98.0 writes them (the expectation is
        // recomputed over the layout and screens on disk), then the
        // records' titles, which ride outside both digests.
        restage_session(&mut staged, 0, &before_the_carry, &|grid| grid);
        restage_session(&mut staged, 1, &before_the_carry, &|grid| grid);
        let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
        let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
        let mut published = SessionHandoff::from_toml(toml).expect("parse the manifest");
        for rec in &mut published.sessions {
            rec.title = records[usize::try_from(rec.local_id).expect("small")].to_string();
        }
        std::fs::write(
            &staged.manifest_path,
            format!("{file_nonce}\n{}", published.to_toml().expect("toml")),
        )
        .expect("republish");

        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(ready_write, commit_read, Some(own_target()));
        let mut incoming = take_incoming_as(ReceiverShape::Current);
        assert_eq!(incoming.adopted.len(), 2, "every session adopts");
        let mut titles = Vec::new();
        for a in &mut incoming.adopted {
            assert_eq!(
                a.checkpoint.as_ref().map(|cp| cp.title.clone()),
                Some(None),
                "session {}: no title carried",
                a.local_id
            );
            let term = std::sync::Mutex::new(aterm_core::terminal::Terminal::new(24, 80));
            crate::spawn::hydrate_adopted_engine(
                &term,
                a.checkpoint.as_ref(),
                None,
                None,
                None,
                a.local_id,
                &mut a.history,
            );
            let seeded = crate::spawn::seed_adopted_title(&term, a.checkpoint.as_ref(), &a.title);
            titles.push((a.local_id, seeded, term.lock().unwrap().title().to_string()));
        }
        titles.sort_unstable();
        drop(incoming);
        for fd in [ready_read, ready_write, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
        titles
    };
    // One tab per session, its leaf read as `park[id]`, with tab `front`
    // active in the window.
    let one_tab_each = |front: usize, park: [(Option<&'static str>, &'static str); 2]| {
        move |layout: &mut RestoreManifest| {
            layout.windows[0].active_item = Some(front);
            for tab in &mut layout.windows[0].restored_tabs {
                let RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(leaf),
                } = &mut tab.root
                else {
                    panic!("one terminal leaf per tab");
                };
                let (cwd, title) =
                    park[usize::try_from(leaf.local_id.expect("handed")).expect("small")];
                leaf.cwd = cwd.map(str::to_string);
                leaf.title = title.to_string();
            }
        }
    };
    let untitled = |id: u64| (id, false, String::new());
    let titled = |id: u64, title: &str| (id, true, title.to_string());

    // Tab 1 in front; tab 0, behind, read untitled.
    assert_eq!(
        titles_after(&one_tab_each(1, [(None, ""), (None, "~/proj")])),
        vec![untitled(0), titled(1, "~/proj")],
        "a cleared title behind stays cleared (the tab falls to its folder), and a read \
         title beats the record's copy"
    );
    // Tab 0 in front, and its read missed: the record's copy, current for
    // the pane in front, stands.
    assert_eq!(
        titles_after(&one_tab_each(0, [(None, ""), (None, "~/proj")])),
        vec![titled(0, records[0]), titled(1, "~/proj")],
        "a busy read in front keeps the title the record kept current"
    );
    // Tab 0 in front with a read that happened (it has a cwd): the cleared
    // title is the answer, in front too.
    assert_eq!(
        titles_after(&one_tab_each(
            0,
            [(Some("/tmp/notes"), ""), (None, "~/proj")]
        )),
        vec![untitled(0), titled(1, "~/proj")],
        "only the missed read's shape defers to the record"
    );
    // ONE split tab, its SECOND pane focused, both reads missed: only the
    // focused pane is the one the record kept current.
    let unread_leaf = |id: u64| {
        Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
            TerminalLeafRestore {
                sid: None,
                lineage: None,
                log_ep: None,
                cwd: None,
                title: String::new(),
                profile: None,
                local_id: Some(id),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
                questions: None,
                autopilot: None,
                land_at: None,
                autopilot_by: None,
                theme: None,
                agent: None,
                supervised: None,
                lost: None,
                held: false,
                holds: Vec::new(),
            },
        )))
    };
    assert_eq!(
        titles_after(&|layout: &mut RestoreManifest| {
            layout.windows[0].active_item = Some(0);
            layout.windows[0].restored_tabs = vec![RestoredTab {
                root: RestoredSplitTree::Split {
                    axis: crate::restore::SplitKind::Vertical,
                    ratio: 0.5,
                    first: unread_leaf(0),
                    second: unread_leaf(1),
                },
                focused_path: vec![crate::restore::RestoreBranch::Second],
                zoomed: false,
            }];
        }),
        vec![untitled(0), titled(1, records[1])],
        "the focused pane of the front tab, and only it, keeps the record's copy"
    );

    // A FOLDED LAYOUT (round eight, the D5 review): the panes in front are
    // the ones the outgoing build had in front, recorded before the fold
    // moved a tab (`LayoutFold::front`), not what the folded window shows.
    // Both reads missed, and tab 0 is the window's active tab in each.
    let folded = |windows: u32, tabs: u32, front: &'static [u64]| {
        move |layout: &mut RestoreManifest| {
            one_tab_each(0, [(None, ""), (None, "")])(layout);
            layout.fold = crate::restore::LayoutFold {
                windows,
                tabs,
                front: front.to_vec(),
            };
        }
    };
    // Tab 1 was a window of its own past the cap, merged into tabs of
    // this one: its pane was in front there, so its record copy is current.
    assert_eq!(
        titles_after(&folded(1, 0, &[0, 1])),
        vec![titled(0, records[0]), titled(1, records[1])],
        "a merged window's front pane keeps the title the record kept current"
    );
    // A donor past the tab cap whose front tab moved: tab 0 stands for the
    // tab it shows since (behind before the fold, so its record copy is
    // the stale one) and tab 1 for the moved front tab, behind in the
    // window that took it (in front before, so its copy is current).
    assert_eq!(
        titles_after(&folded(0, 1, &[1])),
        vec![untitled(0), titled(1, records[1])],
        "the tab a donor shows since the fold was behind, and stays untitled"
    );

    // NEGATIVE CONTROL: the layout misses tab 1, so it is not placed and
    // names no title; the records' copies are the fallback, as before.
    let unplaced = titles_after(&|layout: &mut RestoreManifest| {
        layout.windows[0].restored_tabs.retain(|tab| {
            !matches!(
                &tab.root,
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(leaf),
                } if leaf.local_id == Some(1)
            )
        });
    });
    assert_eq!(unplaced, vec![titled(0, records[0]), titled(1, records[1])]);
}

/// A MANIFEST AN OLDER PRODUCER WROTE PAST THE 4 MiB WRITE CAP IS STILL READ
/// (round eight of the update audit, U8; L3). v0.98.0 and every build before
/// it put no budget on the screen metas (ab83bf3b0 budgets this build's own
/// writes only), so a desk of shell tabs whose carried history held many OSC
/// 133 commands wrote a manifest past 4 MiB, and this consumer, reading with
/// the same 4 MiB cap, refused the whole handoff on every retry: the next
/// release could never be installed in place from such a desk. The producer
/// cannot change, so the consumer reads a larger bounded manifest.
///
/// Staged as v0.98.0 writes it: well-formed, schema 1, `outgoing_build` of
/// v0.98.0, its metas padded with carried shell marks and output blocks
/// only, 4 MiB + 64 KiB and more. Every session is adopted, and the screen
/// commitment equals the parent's. The negative control: a manifest past the
/// consumer's own bound is still refused whole, before it is parsed.
#[test]
#[cfg(unix)]
fn a_manifest_an_older_producer_wrote_past_the_write_cap_is_still_adopted() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    /// v0.98.0's build number: the producer of the next release's handoff.
    const V0_98_0: u64 = 1_790_607_299;
    const SESSIONS: usize = 6;
    // One carried command's mark and output block, as a shell tab's meta
    // carries each of its commands.
    let (mark, block) = {
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        terminal.process(
            b"\x1b]133;A\x07$ \x1b]133;B\x07make target\x1b]133;C\x07\r\nbuilt\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07$ ",
        );
        let checkpoint = terminal.checkpoint_visible().expect("parser is Ground");
        (
            checkpoint.shell.command_marks[0].clone(),
            checkpoint.shell.output_blocks[0].clone(),
        )
    };
    let padded = |meta: &str| {
        let mut parsed: CheckpointMeta = aterm_json::from_str(meta).expect("staged meta");
        parsed.shell.command_marks = vec![mark.clone(); 1_000];
        parsed.shell.output_blocks = vec![block.clone(); 1_000];
        aterm_json::to_string(&parsed).expect("padded meta")
    };
    let staged = stage_outgoing_handoff_with("olderovercap", SESSIONS, None, Some(&padded));
    // As v0.98.0 names itself; the manifest's bytes are hashed by no one.
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    let mut published = SessionHandoff::from_toml(toml).expect("parse the manifest");
    published.outgoing_build = Some(V0_98_0);
    let republished = format!(
        "{file_nonce}\n{}",
        published.to_toml().expect("reserialize")
    );
    std::fs::write(&staged.manifest_path, republished.as_bytes()).expect("republish");
    let size = std::fs::metadata(&staged.manifest_path)
        .expect("manifest")
        .len();
    assert!(
        size > MAX_HANDOFF_MANIFEST_BYTES + 64 * 1024,
        "PRECONDITION — past the write cap by more than a rounding error ({size})"
    );

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    let publish = |staged: &StagedHandoff| {
        staged.publish_env(ready_write, commit_read, Some(own_target()));
    };
    publish(&staged);
    let incoming = take_incoming();
    assert_eq!(
        incoming.adopted.len(),
        SESSIONS,
        "every session of the older producer's over-cap manifest is adopted"
    );
    assert_eq!(
        incoming.screen_digest,
        Some(staged.screen_digest),
        "the screen commitment is the parent's"
    );
    for adopted in &incoming.adopted {
        assert!(!adopted.repaint, "session {}: exact", adopted.local_id);
    }
    drop(incoming);
    staged.teardown();

    // THE NEGATIVE CONTROL: past the consumer's own bound a manifest is
    // still refused whole, read no further than the bound.
    let staged = stage_outgoing_handoff("olderovercapbound", 1, None);
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, _) = body.split_once('\n').expect("nonce header");
    let oversized = format!(
        "{file_nonce}\n{}",
        " ".repeat(usize::try_from(MAX_INCOMING_MANIFEST_BYTES).expect("bound"))
    );
    std::fs::write(&staged.manifest_path, oversized.as_bytes()).expect("oversize");
    publish(&staged);
    let refused = take_incoming();
    assert!(
        refused.adopted.is_empty(),
        "a manifest past the consumer's bound is refused whole"
    );
    drop(refused);

    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE CONSUMER DEGRADES ONE SESSION, NEVER THE HANDOFF (the 2026-09-22/23
/// update audit, plan P0-2). Session 1 arrives as an older producer whose
/// predicate disagreed with this build's would have published it: a cursor
/// three rows below the grid, a working directory holding a NUL (OSC 7
/// `%00`), and a main grid that is not canonical. Before this, one such
/// session made `take_incoming` return an EMPTY handoff: the successor
/// adopted nothing, the parent read `ChildDied`, and every one of the three
/// shells stayed behind on the old build — for a screen the user could
/// have lost at the cost of one repaint.
///
/// Now all three adopt; session 1 comes back blank at a sanitized meta and
/// flagged for a repaint, the other two exactly; and the screen commitment
/// still equals the parent's, because it is taken over the raw bytes read,
/// not over what this build made of them.
#[test]
#[cfg(unix)]
fn a_semantically_refused_session_adopts_blank_and_the_proof_still_matches() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let mut staged = stage_outgoing_handoff("degrade", 3, None);
    let hostile_meta = |meta: &str| {
        let mut parsed: CheckpointMeta = aterm_json::from_str(meta).expect("staged meta");
        parsed.cursor.cursor_row = parsed.rows + 3;
        parsed.current_working_directory = Some("/tmp/a\0b".to_string());
        aterm_json::to_string(&parsed).expect("rewritten meta")
    };
    let non_canonical_grid = |mut grid: Vec<u8>| {
        grid.extend_from_slice(&[0xff, 0x00, 0x7f]);
        grid
    };
    restage_session(&mut staged, 1, &hostile_meta, &non_canonical_grid);

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(
        incoming.adopted.len(),
        3,
        "one session's refused screen must not cost the other two — or its own shell"
    );
    assert_eq!(
        incoming.screen_digest,
        Some(staged.screen_digest),
        "the commitment is over the RAW bytes read, degraded session included"
    );
    for adopted in &incoming.adopted {
        let checkpoint = adopted
            .checkpoint
            .as_ref()
            .expect("a meta that parses still yields a checkpoint to adopt onto");
        if adopted.local_id != 1 {
            assert!(!adopted.repaint, "session {}: exact", adopted.local_id);
            continue;
        }
        assert!(
            adopted.repaint,
            "the degraded session is flagged for a repaint"
        );
        assert!(
            adopted.control.is_none(),
            "and adopts without a control carry"
        );
        assert_eq!(
            (checkpoint.rows, checkpoint.cols),
            (24, 80),
            "at the carried geometry"
        );
        assert!(
            checkpoint.cursor.cursor_row < checkpoint.rows,
            "the out-of-bounds cursor is clamped onto the grid"
        );
        assert_eq!(
            checkpoint.current_working_directory, None,
            "the NUL directory is dropped"
        );
        let blank = aterm_core::terminal::Terminal::new(24, 80)
            .checkpoint_carry(0)
            .expect("a fresh engine is Ground");
        assert_eq!(checkpoint.grid, blank.grid, "the screen is blank");
        screen_digest(&[(1, checkpoint.clone())])
            .expect("the blank screen passes this build's own predicates");
    }
    let ((proof, ready, adopted), _) =
        child_proof_from(incoming).expect("the child adopts and proves");
    assert_eq!(adopted.len(), 3, "the proof covers every session");
    assert_eq!(
        proof, staged.expected,
        "THE HANDOFF COMPLETES: the child's proof equals the parent's expectation"
    );

    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE LAYOUT DEGRADES TOO (the 2026-09-22/23 update audit, plan P0-2e). An
/// outgoing build whose layout this build cannot parse — here a schema it
/// does not know — used to make `take_incoming` refuse everything. Now every
/// session adopts with no layout (the orphan net places them as tabs), the
/// layout digest is still over the bytes read, and the proof matches.
#[test]
#[cfg(unix)]
fn an_unparseable_layout_still_adopts_every_session_and_proves() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let future_schema = |wire: &str| {
        assert!(wire.contains("schema = 2"), "today's wire: {wire}");
        wire.replacen("schema = 2", "schema = 99", 1)
    };
    let staged = stage_outgoing_handoff("badlayout", 2, Some(&future_schema));
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(
        incoming.layout.is_none(),
        "the layout itself is not adopted"
    );
    assert_eq!(
        incoming.adopted.len(),
        2,
        "but every shell is, for the orphan net to place"
    );
    assert!(
        incoming.adopted.iter().all(|adopted| !adopted.repaint),
        "a layout this build cannot read costs no screen"
    );
    let ((proof, ready, _adopted), _) =
        child_proof_from(incoming).expect("the child adopts and proves");
    assert_eq!(
        proof, staged.expected,
        "the layout digest over the bytes read still matches the parent's"
    );

    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// A LAYOUT NAMING A PANE THE OUTGOING PROCESS DID NOT HAND IS PLACED
/// (round three of the 2026-09 update robustness work). Under `aterm
/// --hold` an exited pane stays open, the handoff hands only the live
/// sessions, but the layout capture names every terminal view — so the
/// layout named one id more than the manifest, and `take_incoming` dropped
/// the WHOLE layout: every window collapsed into tabs of the first, and the
/// unsaved Settings draft that rides only in the layout was gone with a log
/// line. Now the layout is adopted with that one pane a placeholder and the
/// draft still on its Settings leaf. (The proof is over the bytes read
/// whatever is placed: `a_handoff_across_a_version_boundary_still_completes`.)
#[test]
#[cfg(unix)]
fn a_layout_naming_an_exited_pane_is_still_placed() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    // The outgoing desk: the two handed sessions, plus a tab whose pane
    // exited (id 9, never handed) and a Settings tab holding a draft.
    let held_desk = |wire: &str| {
        let mut layout = RestoreManifest::from_toml(wire).expect("today's wire parses");
        let exited = exact_legacy_layout(&[9]).windows[0].restored_tabs[0].clone();
        let mut settings = crate::restore::NativeLeafRestore::settings("/text-fonts".to_string());
        settings.settings_drafts = vec![crate::restore::SettingsDraftRestore {
            key: "font_family".to_string(),
            text: "Carried Mono".to_string(),
        }];
        let window = &mut layout.windows[0];
        window.restored_tabs.push(exited);
        window.restored_tabs.push(RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Native(settings)),
            focused_path: Vec::new(),
            zoomed: false,
        });
        layout.to_toml().expect("the desk serializes")
    };
    let staged = stage_outgoing_handoff("heldpane", 2, Some(&held_desk));
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(incoming.adopted.len(), 2, "both live sessions adopt");
    let layout = incoming
        .layout
        .as_ref()
        .expect("the layout is PLACED, not dropped for the exited pane");
    assert_eq!(layout.seamless_terminal_ids(), Some(vec![0, 1]));
    let tabs = &layout.windows[0].restored_tabs;
    assert_eq!(tabs.len(), 4, "every tab keeps its place");
    assert!(
        matches!(
            &tabs[2].root,
            RestoredSplitTree::Leaf { view: RestoredView::Placeholder(retired) }
                if retired.reason == crate::update_words::PANE_NOT_CARRIED
        ),
        "the exited pane is a placeholder: {:?}",
        tabs[2].root
    );
    assert_eq!(
        layout.carried_settings_drafts().0.len(),
        1,
        "the Settings draft is placed with its tab"
    );
    assert_eq!(incoming.unplaced_settings_drafts, (Vec::new(), 0));

    drop(incoming);
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// …and a layout that CANNOT be placed — here one that misses a handed
/// session — still adopts every shell for the orphan net, but hands the
/// drafts it carried to the restore, to be said with their text
/// (`App::settle_carried_settings_drafts`) rather than dropped with it.
#[test]
#[cfg(unix)]
fn an_unplaced_layout_hands_over_the_drafts_it_carried() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let missing_one = |wire: &str| {
        let mut layout = RestoreManifest::from_toml(wire).expect("today's wire parses");
        let window = &mut layout.windows[0];
        window.restored_tabs.truncate(1);
        let mut settings = crate::restore::NativeLeafRestore::settings("/text-fonts".to_string());
        settings.settings_drafts = vec![crate::restore::SettingsDraftRestore {
            key: "font_family".to_string(),
            text: "Carried Mono".to_string(),
        }];
        window.restored_tabs.push(RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Native(settings)),
            focused_path: Vec::new(),
            zoomed: false,
        });
        layout.to_toml().expect("the desk serializes")
    };
    let staged = stage_outgoing_handoff("missingone", 2, Some(&missing_one));
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(
        incoming.layout.is_none(),
        "a missing handed session is not placed"
    );
    assert_eq!(
        incoming.adopted.len(),
        2,
        "every shell adopts for the orphan net"
    );
    assert_eq!(
        incoming.unplaced_settings_drafts,
        (
            vec![crate::restore::SettingsDraftRestore {
                key: "font_family".to_string(),
                text: "Carried Mono".to_string(),
            }],
            0
        ),
        "the draft it carried is handed to the restore to say"
    );

    drop(incoming);
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// A HELD SCREEN THE SUCCESSOR CANNOT SHOW IS NOT WRITTEN (round seven,
/// item 52). The successor strips every link from a held pane's grids and
/// refuses to decode one past [`MAX_LINK_STRIP_BYTES`], so a dense held
/// screen under its per-grid cap but over that bound was written, read,
/// charged to the held budget and then dropped in silence. The writer now
/// applies the reader's bound: the dense pane is left out (its placeholder
/// shows, as it would have), and the small pane beside it is written and
/// shown. RED before the fix: `manifest.held` named the dense pane.
#[test]
#[cfg(unix)]
fn a_held_screen_past_the_link_strip_bound_is_not_written() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let dir = scratch.path();
    let dense = {
        let mut t = aterm_core::terminal::Terminal::new(50, 200);
        let url_tail = "a".repeat(2090);
        for row in 0..50 {
            let mut line = Vec::new();
            for link in 0..40 {
                line.extend_from_slice(
                    format!("\x1b]8;;https://e/{row}-{link}-{url_tail}\x07X\x1b]8;;\x07")
                        .as_bytes(),
                );
            }
            if row + 1 < 50 {
                line.extend_from_slice(b"\r\n");
            }
            t.process(&line);
        }
        t.checkpoint_carry_abandoning_partial(0).0
    };
    let len = u64::try_from(dense.grid.len()).expect("a length");
    assert!(
        len > MAX_LINK_STRIP_BYTES
            && checkpoint_grid_cap(&CheckpointMeta::from_checkpoint(&dense))
                .is_some_and(|cap| len <= cap),
        "PRECONDITION: over the strip bound, under the grid cap ({len} bytes)"
    );
    let small = {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"build finished\r\n");
        t.checkpoint_carry_abandoning_partial(0).0
    };
    let held = [
        held_screen(5, SessionId::generate(), dense),
        held_screen(6, SessionId::generate(), small),
    ];
    let (written, shown) = stamp_and_take_held(dir, &held, &[]);
    assert_eq!(
        written.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        vec![6],
        "only the screen the successor can show is written"
    );
    assert_eq!(
        shown.iter().map(|pane| pane.local_id).collect::<Vec<_>>(),
        vec![6],
        "and the successor shows it"
    );
}

/// A HELD PANE'S SID CROSSES ONLY WHERE IT NAMES NO OTHER SESSION (round
/// eight, finding 21). The writer carries each held pane's sid; the reader
/// gives a pane back its sid when it has the shape this build mints and no
/// handed session, nor an earlier held pane, answers to it. Each refused
/// one still shows its screen, under a fresh sid ([`HeldPane::sid`] `None`).
/// RED before the fix: no sid crossed at all.
#[test]
#[cfg(unix)]
fn a_held_panes_sid_crosses_unless_it_names_another_session() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let dir = scratch.path();
    let screen = || {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"build finished\r\n");
        t.checkpoint_carry_abandoning_partial(0).0
    };
    let sids = [
        "s-0a0b0c0d0e0f10111213", // its own: crosses
        "s-feedfacefeedfacefeed", // a handed session's
        "../not-a-sid",           // not a shape this build mints
        "s-0a0b0c0d0e0f10111213", // the first pane's again
    ];
    let held = sids
        .iter()
        .zip(5_u64..)
        .map(|(sid, local_id)| held_screen(local_id, SessionId::new(*sid), screen()))
        .collect::<Vec<_>>();
    let (written, shown) = stamp_and_take_held(dir, &held, &[(99, "s-feedfacefeedfacefeed")]);
    assert_eq!(
        written
            .iter()
            .map(|pane| pane.sid.as_deref())
            .collect::<Vec<_>>(),
        sids.iter().copied().map(Some).collect::<Vec<_>>(),
        "the writer carries every held pane's sid"
    );
    assert_eq!(
        shown
            .iter()
            .map(|pane| (pane.local_id, pane.sid.as_ref().map(SessionId::as_str)))
            .collect::<Vec<_>>(),
        vec![(5, Some(sids[0])), (6, None), (7, None), (8, None)],
        "every screen shows; only a sid that names no other session crosses"
    );
}

/// A DESCRIPTOR LIST THAT DOES NOT MATCH THE MANIFEST IS THE OUTGOING
/// BUILD'S (round seven, item 105): the parent wrote both, so the refusal
/// exits as a moment, never the `0` that latches a healthy new build.
/// RED before the fix: `Some(Own)`.
#[test]
#[cfg(unix)]
fn a_descriptor_list_that_misses_a_session_is_the_producers_refusal() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("missingfd", 2, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, None);
    let first = staged
        .fds_wire
        .split(',')
        .next()
        .expect("a descriptor entry")
        .to_string();
    assert_ne!(first, staged.fds_wire, "PRECONDITION: two entries");
    aterm_log::env::set(ENV_FDS, &first);
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(incoming.adopted.is_empty() && incoming.screen_digest.is_none());
    assert_eq!(incoming.refusal, Some(WholeRefusal::Producer));
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// CONFORMANCE (Tier-1) of the incoming side's REFUSAL to
/// `aterm_models_session::fd_handoff_no_leak_model` (`FdHandoffNoLeak`): a
/// handoff [`take_incoming_from`] refuses after it began claiming masters closes
/// every master the wire named — session 0's, which its `Adopted` had
/// already claimed from the guard (the `HandedMaster` drops with the
/// half-built set), and session 1's, still owned by the `IncomingPtyGuard`
/// when session 1 is refused — so a refused handoff leaves no master open and
/// readerless in this process.
///
/// The staged PTYs' SLAVES are the witnesses: a slave reads EOF (macOS) or
/// EIO (Linux) once every copy of its master is closed, and would block
/// while one is still open. Per master that is the model's `CloseFallback`
/// after the `Prepare`s that handed them across, or — still open — its
/// `BuggyDrop`, the step only `Buggy = 1` admits.
///
/// NEGATIVE CONTROL: make `HandedMaster`'s `Drop` a no-op and session 0's
/// claimed master stays open; make `IncomingPtyGuard`'s `Drop` a no-op and
/// session 1's does. Either is the `BuggyDrop` step, and this test fails
/// once its 30 s wait for that slave's EOF runs out.
#[test]
#[cfg(unix)]
fn a_refused_handoff_closes_every_master_it_was_handed() {
    use aterm_spec::verify::validate_transition_tiered;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("refusedmasters", 2, None);
    // Session 1 names a sidecar that is not its own, so it is refused only
    // after session 0 passed every check and claimed its master.
    let body = std::fs::read_to_string(&staged.manifest_path).expect("read manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    let mut published = SessionHandoff::from_toml(toml).expect("parse the published manifest");
    let carry = published.sessions[1]
        .screen
        .as_mut()
        .expect("every session carries a screen");
    let own = std::path::PathBuf::from(&carry.grid_file);
    let foreign = own.with_file_name("seamless-1-someone-else.s1.grid");
    std::fs::copy(&own, &foreign).expect("a readable foreign sidecar");
    carry.grid_file = foreign.to_string_lossy().into_owned();
    std::fs::write(
        &staged.manifest_path,
        format!(
            "{file_nonce}\n{}",
            published.to_toml().expect("reserialize")
        ),
    )
    .expect("republish");
    let witnesses = staged.slaves.clone();
    for &slave in &witnesses {
        // SAFETY: fcntl on a slave descriptor this fixture opened and owns.
        let flags = unsafe { libc::fcntl(slave, libc::F_GETFL) };
        assert!(flags >= 0, "slave flags");
        // SAFETY: as above; only O_NONBLOCK is added.
        let rc = unsafe { libc::fcntl(slave, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(rc, 0, "non-blocking witness");
    }
    let master_closed = |slave: i32| {
        let mut byte = 0u8;
        // SAFETY: a one-byte read into a live local from an owned descriptor.
        let n = unsafe { libc::read(slave, std::ptr::from_mut(&mut byte).cast(), 1) };
        match n {
            0 => true,
            -1 => match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EIO) => true,
                Some(e) if e == libc::EAGAIN || e == libc::EWOULDBLOCK => false,
                other => panic!("unexpected slave read error {other:?}"),
            },
            _ => panic!("nothing writes to these PTYs, yet the slave read {n} byte(s)"),
        }
    };
    assert!(
        witnesses.iter().all(|&slave| !master_closed(slave)),
        "PRECONDITION: every handed master is open before the handoff is taken"
    );
    // After the refusal, WAIT FOR THE FACT — the slave's EOF — rather than
    // read the slave once. A child another test's thread has forked and not
    // yet exec'd holds a copy of every descriptor of this process, masters
    // included, until its exec closes them (they are close-on-exec), and
    // a loaded machine stretches that window to tens of milliseconds: a
    // master this process has closed then still reads open for a moment.
    // A master the refusal LEAKED never reads closed, however long the
    // wait; the bound is that verdict's hang detector, not a measurement.
    let master_closed_within = |slave: i32, hang: std::time::Duration| {
        let started = std::time::Instant::now();
        loop {
            if master_closed(slave) {
                return true;
            }
            let left = hang.saturating_sub(started.elapsed());
            if left.is_zero() {
                return false;
            }
            let mut ready = libc::pollfd {
                fd: slave,
                events: libc::POLLIN,
                revents: 0,
            };
            // Wakes on the hangup; sliced to 10 ms so a platform whose
            // slave never polls ready on it still reads the EOF promptly.
            let slice = i32::try_from(left.as_millis().min(10)).unwrap_or(10);
            // SAFETY: one live, owned pollfd and its exact count.
            unsafe { libc::poll(&mut ready, 1, slice) };
        }
    };

    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, None);
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert!(
        incoming.adopted.is_empty() && incoming.screen_digest.is_none(),
        "a sidecar that is not this record's own is refused outright"
    );
    // WHOSE REFUSAL (round seven, item 105): the sidecar name is what the
    // outgoing build wrote, so the successor leaves as for the producer's
    // content, never with the `0` that latches the new build. RED before
    // item 105: `Some(Own)`.
    assert_eq!(incoming.refusal, Some(WholeRefusal::Producer));

    let model = aterm_models_session::fd_handoff_no_leak_model();
    let label = "FdHandoffNoLeak(take_incoming refusal)";
    let mut state = model.init_state();
    let mut advance = |what: &str, action: &str, var: &'static str, value: i64| {
        let mut next = state.clone();
        next.insert(var, value);
        let (ok, why) = validate_transition_tiered(&model, &[], &state, &next, Some(action), label);
        assert!(
            ok,
            "{what}: the refused handoff took `{action}` to {next:?}, which the \
             committed model does not admit — {why}"
        );
        state = next;
    };
    for handed in 1..=2 {
        advance("the outgoing side", "Prepare", "prepared", handed);
    }
    let mut closed = 0;
    for (local_id, &slave) in witnesses.iter().enumerate() {
        let what = format!("session {local_id}'s master");
        if master_closed_within(slave, aterm_spec::wait::MUST_HAPPEN) {
            closed += 1;
            advance(&what, "CloseFallback", "closed", closed);
        } else {
            advance(&what, "BuggyDrop", "leaked", 1);
        }
    }
    assert!(model.check_invariant("NoLeak", &state), "{state:?}");
    assert_eq!(closed, 2, "both masters closed");
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// The degrade target is CANONICAL BY CONSTRUCTION: whatever a hostile or
/// older producer's meta says, [`repaint_checkpoint`] yields a screen this
/// build's own full predicate (`screen_digest`) admits — the property that
/// lets P0-1's producer and P0-2's consumer (the 2026-09-22/23 update audit)
/// share it without ever handing each other a carry the other refuses.
#[test]
fn a_repaint_checkpoint_is_admitted_by_this_builds_own_predicates() {
    let base = CheckpointMeta::from_checkpoint(&{
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/aterm % cargo test\r\nline 1");
        t.checkpoint_visible().expect("parser is Ground")
    });
    let mut cases: Vec<(&str, CheckpointMeta)> = Vec::new();
    let mut cursor_below = base.clone();
    cursor_below.cursor.cursor_row = base.rows + 3;
    cases.push(("cursor below the grid", cursor_below));
    let mut nul_cwd = base.clone();
    nul_cwd.current_working_directory = Some("/tmp/a\0b".to_string());
    cases.push(("NUL directory", nul_cwd));
    let mut saved_past_ceiling = base.clone();
    saved_past_ceiling.saved_cursor_main = CheckpointMeta::from_checkpoint(&{
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"\x1b7");
        t.checkpoint_carry(0).expect("Ground")
    })
    .saved_cursor_main
    .map(|mut saved| {
        saved.cursor_row = aterm_core::grid::MAX_GRID_ROWS;
        saved
    });
    cases.push(("saved slot past the engine ceiling", saved_past_ceiling));
    let mut alt_without_grid = base.clone();
    alt_without_grid.modes.alternate_screen = true;
    alt_without_grid.alt_cursor = None;
    cases.push(("alternate screen with no saved primary", alt_without_grid));
    let mut oversized = base.clone();
    oversized.rows = 99;
    oversized.cols = 338;
    oversized.cursor.tab_stops = vec![false; 338];
    oversized.cursor.scroll_bottom = 98;
    oversized.cursor.margin_right = 337;
    cases.push(("99x338, over the per-grid cell cap", oversized));
    let mut zero = base.clone();
    zero.rows = 0;
    zero.cols = 0;
    cases.push(("a zero geometry", zero));

    for (what, meta) in cases {
        let mut used_cells = 0;
        let checkpoint = repaint_checkpoint(Some(&meta), &mut used_cells)
            .unwrap_or_else(|| panic!("{what}: a repaint carry exists"));
        assert!(used_cells > 0, "{what}: the blank screen is charged");
        screen_digest(&[(0, checkpoint.clone())])
            .unwrap_or_else(|refusal| panic!("{what}: refused by this build: {refusal}"));
        assert_eq!(checkpoint.modes, meta.modes, "{what}: the modes survive");
        assert_eq!(
            checkpoint.kitty_keyboard, meta.kitty_keyboard,
            "{what}: the keyboard protocol survives"
        );
    }
    let mut spent = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
    assert!(
        repaint_checkpoint(Some(&base), &mut spent).is_none(),
        "with the aggregate spent there is no screen to carry — the pane builds its own"
    );
    assert!(repaint_checkpoint(None, &mut 0).is_none());
}

/// The repaint rung's screen is blank, so the rows a carried screen's resizes
/// owed back (`GridCursorRepr::handback_rows`, `handback_above`) are owed on
/// neither grid of its carry: a successor's grow must not append blanks or a
/// shrink demote rows for a screen that is not there.
#[test]
fn a_repaint_checkpoint_owes_no_rows_back() {
    let mut owing = CheckpointMeta::from_checkpoint(&{
        let mut t = aterm_core::terminal::Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n\x1b[?1049h");
        t.checkpoint_carry(0).expect("Ground")
    });
    owing.cursor.handback_rows = 6;
    owing.cursor.handback_above = 1;
    let alt = owing.alt_cursor.as_mut().expect("a saved primary");
    alt.handback_rows = -2;
    alt.handback_above = 3;
    let checkpoint = repaint_checkpoint(Some(&owing), &mut 0).expect("a repaint carry");
    let alt = checkpoint.alt_cursor.as_ref().expect("the saved primary");
    assert_eq!(
        (
            checkpoint.cursor.handback_rows,
            checkpoint.cursor.handback_above,
            alt.handback_rows,
            alt.handback_above,
        ),
        (0, 0, 0, 0)
    );
}

/// FAIL CLOSED #1 — a WRONG BUILD cannot claim the PTYs. The parent
/// authorized a different build; the child refuses before any proof exists,
/// so no ready signal is ever published.
#[test]
#[cfg(unix)]
fn a_wrong_build_child_refuses_the_handoff() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("wrongbuild", 1, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    let own = own_build();
    staged.publish_env(
        ready_write,
        commit_read,
        Some(encode_target_identity(
            own.wrapping_add(1),
            crate::build_info::git_commit(),
        )),
    );
    assert!(
        child_proof().is_none(),
        "a candidate that is not the authorized build must never publish a proof"
    );
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// FAIL CLOSED #2 — a WRONG SESSION SET cannot pass. The child's proof
/// commits to the `(local_id, fd, pid)` set it ACTUALLY adopted, so a
/// dropped or substituted session yields a digest the parent rejects.
#[test]
#[cfg(unix)]
fn a_wrong_session_set_cannot_match_the_parent_expectation() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("wrongset", 3, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let (_proof, ready, adopted) = child_proof().expect("the child adopts and proves");

    let mut subset = adopted.clone();
    subset.pop();
    assert_ne!(
        ready.proof(&subset),
        Some(staged.expected),
        "a SUBSET of the authorized PTY set must not prove complete adoption"
    );
    let mut swapped = adopted.clone();
    swapped[0].2 = swapped[0].2.wrapping_add(1);
    assert_ne!(
        ready.proof(&swapped),
        Some(staged.expected),
        "a substituted shell pid must not prove the authorized adoption"
    );
    assert_eq!(
        ready.proof(&adopted),
        Some(staged.expected),
        "…while the EXACT set still does"
    );

    drop(ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// FAIL CLOSED #3 — ProofReady is NOT Commit. A child holding a valid proof
/// must not accept the proof wire, a foreign build's Commit, or a Commit for
/// a different session set as release authority.
#[test]
#[cfg(unix)]
fn a_missing_or_foreign_commit_never_releases_the_readers() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let staged = stage_outgoing_handoff("nocommit", 2, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, Some(own_target()));
    let (proof, _ready, adopted) = child_proof().expect("the child adopts and proves");

    assert!(
        !proof.commit_wire_matches(&proof.to_wire()),
        "the ProofReady wire is NOT a Commit: the magic differs"
    );
    let mut subset = adopted;
    subset.pop();
    let other = adoption_proof(
        &staged.nonce,
        own_build(),
        crate::build_info::git_commit(),
        &[0x11; 32],
        &[0x22; 32],
        &subset,
    )
    .expect("a foreign proof");
    assert!(
        !proof.commit_wire_matches(&other.to_commit_wire()),
        "a Commit minted for a different attempt must not release these readers"
    );
    let mut corrupt = proof.to_commit_wire();
    corrupt[READY_WIRE_LEN - 1] ^= 0x01;
    assert!(
        !proof.commit_wire_matches(&corrupt),
        "a one-bit-corrupted Commit must not release these readers"
    );
    assert!(
        proof.commit_wire_matches(&proof.to_commit_wire()),
        "…only the exact Commit does"
    );

    drop(_ready);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// THE PACKED TAB STOPS (R17-HANDOFF-1): every vector a grid can hold packs and
/// unpacks to itself — the every-8 defaults as their length alone, a cleared
/// set as an empty bitmap, custom stops, a length that is no multiple of four
/// and a vector longer than its width — and a packing no window writes is
/// refused rather than guessed at.
#[test]
fn packed_tab_stops_round_trip_and_refuse_what_no_window_writes() {
    let defaults = default_tab_stops;
    let mut custom = defaults(203);
    custom[5] = true;
    custom[16] = false;
    custom[202] = true;
    let mut past_width = defaults(400);
    past_width[300] = true;
    for (what, stops) in [
        ("defaults", defaults(80)),
        ("wide defaults", defaults(4096)),
        ("one column", vec![false]),
        ("cleared", vec![false; 200]),
        ("custom", custom),
        ("past the width", past_width),
        ("every column", vec![true; 4096]),
    ] {
        let packed = PackedTabStops::pack(&stops).expect(what);
        assert_eq!(packed.unpack().as_ref(), Some(&stops), "{what}");
        let len = stops.len();
        assert!(
            packed
                .hex
                .as_ref()
                .is_none_or(|hex| hex.len() <= len.div_ceil(4)),
            "{what}: a quarter of a byte a column at most"
        );
    }
    assert_eq!(
        PackedTabStops::pack(&defaults(4096)),
        Some(PackedTabStops {
            len: 4096,
            hex: None
        }),
        "the defaults are their length alone"
    );
    assert_eq!(
        PackedTabStops::pack(&[false; 200]).and_then(|p| p.hex),
        Some(String::new()),
        "a cleared set is an empty bitmap"
    );
    let refused = |len: u16, hex: Option<&str>| {
        PackedTabStops {
            len,
            hex: hex.map(str::to_owned),
        }
        .unpack()
        .is_none()
    };
    assert!(refused(0, None), "no columns");
    assert!(refused(4097, None), "wider than a grid");
    assert!(refused(8, Some("000")), "a bitmap longer than its length");
    assert!(refused(8, Some("0g")), "not hex");
    assert!(refused(5, Some("02")), "a stop past the length");
    assert!(!refused(5, Some("01")), "the last column's own stop");
}
