// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread SCHEDULING ROLE, declared once per worker.
//!
//! macOS schedules by Quality of Service, and on Apple Silicon QoS also steers
//! P-core versus E-core placement. A thread that never declares one inherits
//! `QOS_CLASS_DEFAULT`, which sits just below `USER_INITIATED` and therefore
//! competes with the UI thread's work whenever the machine is saturated.
//!
//! That default is wrong for most of this app. aterm spawns dozens of workers —
//! font warmers, image/video encoders, update checkers, package probes, log
//! flushers, the smart-title LLM — and essentially all of them do work no human
//! is waiting on. Left at the default they are indistinguishable, to the
//! scheduler, from the thread drawing the next frame. Under load that is
//! precisely backwards: the cosmetic work keeps its share while keystroke
//! handling waits, which is felt as typing lag rather than as a slow font warm.
//!
//! Declaring the role makes the priority ORDER explicit and reviewable, instead
//! of an accident of which threads happened to get a `pthread_set_qos_class`
//! call. The scale is deliberately coarse — four roles, chosen by asking "who is
//! waiting for this?" — because a finer one invites per-thread tuning that no
//! one can reason about globally.
//!
//! Every function is a no-op off macOS, so call sites stay platform-neutral.
//!
//! PROVENANCE. This module is a HAND-PORT of commit `61a6c8b62`
//! (`fix/event-loop-wake-spin-v2`, 2026-08-11), which found thread QoS to be
//! the second cause of the measured keystroke-dequeue lag: 41 named threads,
//! 2 declaring a class. It is a port and not a merge because main moved ~2100
//! commits past that branch's base before it could land, so no hunk of it
//! applies textually. The enum, its class mapping and `set_self` are carried
//! over verbatim; the docs gain the port-time floor rule below and the tests
//! gain one extra rank assertion; each worker's role is re-derived against
//! today's spawn site. One rule was added at port time
//! that the branch only applied to `aterm-scrollback-compress`: a worker that
//! initialises or holds a lock the UI thread contends (a `Mutex`, or a
//! `OnceLock` the UI thread's own first use would block on) is NOT demoted
//! below [`Role::Responsive`], because a descheduled lock holder at a lower
//! class is a priority inversion — the one hazard a QoS port can introduce.

/// What a thread's work is worth relative to the human at the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The human is watching this land RIGHT NOW: input egress, and anything
    /// else on the keystroke→glass path. Ranks at or above the UI thread.
    Interactive,
    /// The human asked for it and is waiting, but not frame-by-frame: PTY drain
    /// and parse, reflow. Below the UI thread, above everything cosmetic.
    ///
    /// Also the FLOOR for any worker that holds or initialises a lock the UI
    /// thread contends. Matching the PTY drain here does not make such work
    /// urgent; it shortens the window in which a lock holder can sit
    /// descheduled with the UI thread queued behind it.
    Responsive,
    /// Useful work nobody is blocked on: font warming, encoding, log flushing,
    /// smart titles, package/update probes. Must never delay a keystroke.
    Background,
    /// Work with NO deadline at all, which may therefore run arbitrarily late:
    /// opportunistic disk sweeps and prefetch. Yields to literally everything.
    ///
    /// "Arbitrarily late" is the whole contract, and it is stricter than it
    /// sounds. A `QOS_CLASS_BACKGROUND` thread on a saturated machine can be
    /// starved for many seconds, so anything holding a lock, enforcing a
    /// timeout, or owing cleanup that something else waits on does NOT belong
    /// here. Process REAPERS in particular look like housekeeping and are not:
    /// they enforce a kill-and-reap contract on a real deadline, and a starved
    /// reaper leaks the very processes whose CPU use degrades the machine. They
    /// take [`Role::Background`]. (Pinned by experience — putting the managed
    /// runtime's reapers here made their 500 ms kill-and-reap assertions fail
    /// under load.)
    Housekeeping,
}

#[cfg(target_os = "macos")]
const fn qos_class(role: Role) -> libc::qos_class_t {
    match role {
        Role::Interactive => libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
        Role::Responsive => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
        Role::Background => libc::qos_class_t::QOS_CLASS_UTILITY,
        Role::Housekeeping => libc::qos_class_t::QOS_CLASS_BACKGROUND,
    }
}

/// Declare the CALLING thread's role. Call it as the first statement of a
/// worker's closure, so the whole body runs at the declared class.
///
/// Setting a thread's own QoS is the supported spelling (`_self_np`); it cannot
/// fail in a way worth branching on, so the result is discarded.
#[inline]
pub(crate) fn set_self(role: Role) {
    #[cfg(target_os = "macos")]
    // SAFETY: sets this thread's own QoS class. Takes no pointers and mutates
    // no shared state, so there is nothing for another thread to observe.
    unsafe {
        libc::pthread_set_qos_class_self_np(qos_class(role), 0);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = role;
}

// ---------------------------------------------------------------------------
// CHILD PROCESSES. A role is a THREAD attribute, and it stops at the spawn.
//
// Measured 2026-09-15 on macOS: a thread at UTILITY (`ps` pri 20) or BACKGROUND
// (pri 4) still starts its child at pri 31, the default band. That is the band
// the user's shell and the program being typed into run in. So the `Background`
// workers that launch `atpkg update`, a package verb, a login-shell PATH probe or
// the smart-title Ollama daemon handed that work straight back to the scheduler
// as an equal of the foreground TUI: the thread yielded, and the process doing
// the actual CPU work did not. Nothing in aterm attributes the lost time either,
// because the starved party is a sibling process aterm launched.
// ---------------------------------------------------------------------------

/// macOS's QoS launcher — the one `aterm-verify` wraps compile-only children in.
/// Absent elsewhere, where a [`command`] child runs as is.
const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// The `taskpolicy -c` clamp for a child spawned on behalf of `role`, or `None`
/// when the child keeps the tier it inherits.
///
/// Mirrors [`qos_class`]: a `Background` worker's child runs at UTILITY, the class
/// the worker's own thread runs at, and a `Housekeeping` worker's at BACKGROUND.
/// `Interactive` and `Responsive` rank ABOVE the inherited default, and a clamp can
/// only lower a child, so their children are spawned bare.
const fn child_clamp(role: Role) -> Option<&'static str> {
    match role {
        Role::Interactive | Role::Responsive => None,
        Role::Background => Some("utility"),
        Role::Housekeeping => Some("background"),
    }
}

/// A `Command` for `program`, run on behalf of `role`'s work: behind
/// `taskpolicy -c <clamp>` whenever that work ranks below the inherited default,
/// so the child — and everything it launches — yields to the program being typed
/// into. Measured: under `taskpolicy -c utility` a child and its own child both
/// read pri 20, against 31 spawned bare.
///
/// A SPAWN ATTRIBUTE, NOT A `pre_exec`. A `pre_exec` closure forces a real `fork()`
/// that copies aterm's page tables (see `configure_dedicated_process_group`); the
/// wrapper keeps the `posix_spawn` fast path. `taskpolicy` replaces itself with the
/// program (`POSIX_SPAWN_SETEXEC`), so `Child::id` is the program's own pid and its
/// parent is aterm: pid-keyed tailers, group kills and `SPAWNER_PID_ENV` checks see
/// exactly what they saw before.
///
/// Wrapped only when both ends are real: `taskpolicy` exists and `program` is an
/// absolute path to an executable file. A missing program under `taskpolicy` is an
/// exit 66 rather than a spawn error, so such a program is spawned bare and still
/// fails the way its caller expects.
///
/// NOT FOR a child whose pid is attested the moment `spawn` returns: until
/// `taskpolicy` execs the program, that pid is still running taskpolicy's image.
/// The managed Ollama daemon demotes itself with [`demote_forked_child`] instead.
/// NOT FOR a child that launches an aterm either: the clamp is inherited, and
/// docs/RELEASING.md measured an aterm under UTILITY starving its paint probe.
pub(crate) fn command(role: Role, program: impl AsRef<std::path::Path>) -> std::process::Command {
    command_with(role, program.as_ref(), std::path::Path::new(TASKPOLICY))
}

/// [`command`] over an explicit launcher, so the wrapping decision is testable on
/// every Unix against a stand-in.
fn command_with(
    role: Role,
    program: &std::path::Path,
    taskpolicy: &std::path::Path,
) -> std::process::Command {
    match child_clamp(role) {
        Some(clamp)
            if program.is_absolute()
                && is_executable_file(program)
                && is_executable_file(taskpolicy) =>
        {
            let mut wrapped = std::process::Command::new(taskpolicy);
            wrapped.arg("-c").arg(clamp).arg(program);
            wrapped
        }
        _ => std::process::Command::new(program),
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// `sys/resource.h`: `setpriority`'s "which" for the whole calling process, and the
/// value that puts it in the Darwin background band. Not in `aterm-libc`.
#[cfg(target_os = "macos")]
const PRIO_DARWIN_PROCESS: libc::c_int = 4;
#[cfg(target_os = "macos")]
const PRIO_DARWIN_BG: libc::c_int = 0x1000;

/// Put the CALLING PROCESS in the Darwin background band. For a `pre_exec`
/// closure whose child cannot take [`command`]'s wrapper.
///
/// This is what `taskpolicy -b` does: `setpriority(PRIO_DARWIN_PROCESS, 0,
/// PRIO_DARWIN_BG)`. It survives `execve` and reaches every descendant (measured:
/// the exec'd child and its own child read pri 4, against 31 bare). It is harsher
/// than the UTILITY clamp [`command`] gives a `Background` child — the lowest CPU
/// band, with disk I/O and newly opened sockets throttled — but it is the only
/// process-wide demotion that can run between fork and exec: the utility clamp
/// exists only as a spawn attribute, and `taskpolicy -p` cannot apply one to a
/// running pid.
///
/// Async-signal-safe: one syscall, no allocation. Its result is ignored, because a
/// scheduling hint must never be the reason a child fails to start. A no-op off
/// macOS.
#[inline]
pub(crate) fn demote_forked_child() {
    #[cfg(target_os = "macos")]
    // SAFETY: `setpriority` on the calling process takes no pointers and is a
    // plain syscall, safe between fork and exec.
    unsafe {
        libc::setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG);
    }
}

// ---------------------------------------------------------------------------
// READING A CLASS BACK.
//
// Until this, nothing in the tree ever observed the class a thread ACTUALLY
// runs at. `set_self` returns nothing worth branching on, and every QoS test
// below asserts either this enum's ORDER or the SOURCE TEXT of a spawn site —
// both of them claims about code. The class is a property of a RUNNING THREAD,
// and it is the only thing the scheduler reads.
//
// Neither symbol is in `libc`: that crate is GENERATED from a pinned reference
// and asserted item-for-item by its oracle ("Regenerate rather than edit"), so
// declaring them there would be a hand edit of generated code. Both are
// ordinary libSystem exports (`<sys/qos.h>`, `<pthread/qos.h>`), declared here
// the way this module already declares `PRIO_DARWIN_PROCESS`/`PRIO_DARWIN_BG`.
//
// Declared returning `c_uint`, NOT `libc::qos_class_t`: a `repr(u32)` enum
// return carrying a value outside its six discriminants would be undefined
// behaviour, so a class read back is compared as a number.
#[cfg(all(test, target_os = "macos"))]
unsafe extern "C" {
    fn qos_class_self() -> libc::c_uint;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        class: *mut libc::c_uint,
        relative_priority: *mut libc::c_int,
    ) -> libc::c_int;
}

/// The class the CALLING thread is actually running at.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of_self() -> u32 {
    // SAFETY: reads the calling thread's own class. Takes no pointers and
    // mutates no shared state.
    unsafe { qos_class_self() }
}

/// The class ANOTHER thread is actually running at, named by its pthread handle
/// (`std::os::unix::thread::JoinHandleExt::as_pthread_t`), or `None` if the
/// platform declined to report one.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of(thread: libc::pthread_t) -> Option<u32> {
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and the
    // caller's `JoinHandle` keeps `thread` alive across the call.
    let rc = unsafe { pthread_get_qos_class_np(thread, &mut class, &mut relative) };
    (rc == 0).then_some(class)
}

/// The class [`set_self`] puts a thread at, to compare against one read back off
/// the running thread.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_for(role: Role) -> u32 {
    qos_class(role) as u32
}

/// The class a worker that declares no role runs at — what every silent `spawn`
/// in this crate gets, and what a deleted `set_self` line leaves behind.
/// MEASURED, not assumed: see `tests::a_role_reaches_only_the_thread_that_declares_it`.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const UNDECLARED_CLASS: u32 = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // WHAT THE SCHEDULER ACTUALLY SEES
    // -----------------------------------------------------------------------

    /// THE CLASS, READ OFF A RUNNING THREAD. Every other QoS assertion in this
    /// crate is about text — this enum's order, or a `set_self` appearing in a
    /// spawn site's source. None of them can tell whether a worker HAS the class
    /// its role names. This one declares each role on a real thread and asks the
    /// kernel what that thread is running at.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_declared_role_really_lands_on_the_running_thread() {
        for role in [
            Role::Interactive,
            Role::Responsive,
            Role::Background,
            Role::Housekeeping,
        ] {
            let observed = std::thread::spawn(move || {
                set_self(role);
                class_of_self()
            })
            .join()
            .expect("the probe thread");
            assert_eq!(
                observed,
                class_for(role),
                "{role:?} was declared, but the thread runs at 0x{observed:x}"
            );
        }
    }

    /// A ROLE REACHES ONLY THE THREAD THAT DECLARES IT — the measurement the
    /// rest of this file rests on, and until now only a comment in it. It is why
    /// a declaration must be the first statement INSIDE a worker's closure, why
    /// `every_control_plane_thread_declares_its_role_inside_its_closure` insists
    /// the `set_self` sits after the `.spawn(`, and why a `Background` worker's
    /// CHILD PROCESS needs [`command`]'s clamp: neither a thread nor a process
    /// inherits the class of whatever created it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_role_reaches_only_the_thread_that_declares_it() {
        let inherited = std::thread::spawn(|| {
            set_self(Role::Interactive);
            std::thread::spawn(class_of_self)
                .join()
                .expect("the spawned thread")
        })
        .join()
        .expect("the declaring thread");
        assert_eq!(
            inherited, UNDECLARED_CLASS,
            "a spawned thread inherited its creator's class (0x{inherited:x}); every \
             `set_self` this crate places inside a closure on the opposite \
             assumption would need re-deriving"
        );
    }

    // -----------------------------------------------------------------------
    // THE CENSUS
    // -----------------------------------------------------------------------

    /// Workers that declare NO role, each with the reason. Two kinds only:
    ///
    /// * a worker on a platform where [`set_self`] is a no-op, where declaring
    ///   one would be a comment with a syscall's shape; and
    /// * a worker the 2026-08-29 audit's P2 role-by-role pass has not reached.
    ///
    /// The second kind is the OPEN half of that P2, countable here instead of
    /// guessed at: each of those runs in the band the compilers and the program
    /// you are typing into share. This is not a licence to add another — a new
    /// worker gets a role, and appending to this list instead is a decision to
    /// argue for in review, in the entry.
    const UNDECLARED_WORKERS: &[(&str, &str)] = &[
        (
            "aterm-admin-step-dismiss",
            "unclassified: posts an admin-step notice dismissal",
        ),
        (
            "aterm-agent-prime",
            "unclassified: primes the agent roster when due",
        ),
        (
            "aterm-artifact-cleanup",
            "unclassified: the artifact cleanup scheduler",
        ),
        (
            "aterm-config-runtime",
            "unclassified: the runtime config service's entry worker",
        ),
        (
            "aterm-claimant-census",
            "unclassified: lists the bundles claiming this app's identifier",
        ),
        (
            "aterm-claimant-retire",
            "unclassified: moves conflicting copies of the app to the Trash on the owner's press",
        ),
        (
            "aterm-consent-probe",
            "unclassified: probes macOS full-disk-access consent",
        ),
        (
            "aterm-defterm-broker",
            "Windows only (`defterm_broker_win`); `set_self` is a no-op there",
        ),
        (
            "aterm-document-admit",
            "unclassified: admits an opened document",
        ),
        (
            "aterm-fabric-launch",
            "unclassified: arms the fabric launcher",
        ),
        (
            "aterm-fleet-scan",
            "unclassified: scans the fleet on request",
        ),
        (
            "aterm-handoff-commit",
            "unclassified: signals update-handoff readiness",
        ),
        (
            "aterm-handoff-parent-watch",
            "unclassified: watches the seamless handoff's parent",
        ),
        (
            "aterm-jumplist",
            "Windows only (`jumplist_win::install`); `set_self` is a no-op there",
        ),
        (
            "aterm-macos-access-answer",
            "unclassified: records and clears the macOS access marker",
        ),
        (
            "aterm-macos-access-card",
            "unclassified: posts the macOS access card's facts",
        ),
        (
            "aterm-native-config",
            "unclassified: the native config queue's worker",
        ),
        (
            "aterm-native-document",
            "unclassified: the native document queue's worker",
        ),
        (
            "aterm-operator-observer",
            "unclassified: the operator host's observer",
        ),
        (
            "aterm-reroute-lay",
            "unclassified: lays the session reroute at startup",
        ),
        (
            "aterm-update-handoff",
            "unclassified: drives the Unix update handoff",
        ),
        (
            "aterm-update-preverify",
            "DELIBERATELY the inherited DEFAULT — the reason is written at its spawn \
             site in app_native.rs: it publishes into a mutex the UI thread takes, so \
             the floor rule forbids demoting it, and nothing waits on it, so promoting \
             it would be wrong too",
        ),
        (
            "aterm-watchdog",
            "unclassified: the freeze watchdog's beat monitor",
        ),
        (
            "aterm-x11-clipboard",
            "X11 only; `set_self` is a no-op off macOS",
        ),
        (
            "aterm-x11-paste",
            "X11 only; `set_self` is a no-op off macOS",
        ),
        (
            "aterm-x11-primary-paste",
            "X11 only; `set_self` is a no-op off macOS",
        ),
    ];

    /// EVERY NAMED WORKER IN THIS CRATE IS ROLE-DECLARED OR INVENTORIED — the
    /// standing thread-QoS census the 2026-08-29 audit's P2 asked for
    /// ("platform thread-QoS inspection"), as a test rather than a verb.
    ///
    /// The other QoS tests here each pin ONE lane by name — the package lane,
    /// the reflow pool, the control plane — so a worker nobody thought to name
    /// is invisible to all of them. That is the exact shape of the incident this
    /// module ports: 41 named threads, 2 declaring a class. This test enumerates
    /// instead, and has no list of lanes to keep up to date.
    ///
    /// A role counts only where it REACHES the worker, so the scan resolves the
    /// BODY rather than reading near the name:
    ///
    /// * an inline closure declares inside itself, before the next named spawn
    ///   (so one worker's declaration can never cover its neighbour's);
    /// * `.spawn(some_fn)` declares at that function's entry — resolved only
    ///   when the crate holds exactly one definition of the name, so an
    ///   ambiguous one is reported rather than guessed (the reflow pool's two
    ///   spawn sites are this shape);
    /// * a body handed to a spawn SEAM declares in every closure passed to that
    ///   seam. This is the reply writer, and it is the shape that motivated the
    ///   whole test: its `Role::Interactive` is one line inside the closure
    ///   `spawn_reply_writer` hands `spawn_reply_writer_thread`, and moving that
    ///   body to a bare `std::thread::spawn` would compile, pass, and drop the
    ///   class of the thread holding the mutex a keystroke write spins on.
    ///
    /// Line comments are blanked before the scan, so a comment that NAMES
    /// `set_self` — `aterm-update-preverify`'s spawn site says exactly why it
    /// declares none — can never read as a declaration.
    #[test]
    fn every_named_worker_declares_a_role_or_is_inventoried() {
        let sources = crate_sources();
        // Built at runtime so this file's own source never carries the token it
        // scans for.
        let marker = String::from(".name") + "(\"aterm-";
        let mut sites = 0usize;
        let mut undeclared: Vec<String> = Vec::new();
        let mut listed_seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (file, source) in &sources {
            let mut from = 0usize;
            while let Some(hit) = source[from..].find(&marker) {
                let site = from + hit;
                from = site + marker.len();
                let Some(quote) = source[from..].find('"') else {
                    continue;
                };
                let name = format!("aterm-{}", &source[from..from + quote]);
                sites += 1;
                if declares_a_role(&sources, source, site) {
                    continue;
                }
                match UNDECLARED_WORKERS
                    .iter()
                    .find(|(listed, _)| *listed == name)
                {
                    Some((listed, _)) => {
                        listed_seen.insert(*listed);
                    }
                    None => {
                        let line = source[..site].matches('\n').count() + 1;
                        undeclared.push(format!("{file}:{line} {name}"));
                    }
                }
            }
        }
        assert!(
            sites >= 50,
            "the scan found only {sites} named workers, so it has stopped seeing them"
        );
        assert!(
            undeclared.is_empty(),
            "these workers declare no scheduling role, so they run in the band the \
             compilers and the program being typed into share — give each one a \
             `crate::qos::set_self(…)` as the first statement INSIDE its closure, or \
             list it in UNDECLARED_WORKERS with the reason:\n  {}",
            undeclared.join("\n  ")
        );
        let stale: Vec<&str> = UNDECLARED_WORKERS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !listed_seen.contains(name))
            .collect();
        assert!(
            stale.is_empty(),
            "UNDECLARED_WORKERS lists workers that no longer exist, or that now declare \
             a role; drop them so the list stays the true inventory: {stale:?}"
        );
    }

    /// Every `.rs` file under this crate's `src/`, with line comments blanked to
    /// spaces: byte offsets — and so the line numbers reported — stay true.
    fn crate_sources() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src/") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    let text = std::fs::read_to_string(&path).expect("read source");
                    out.push((rel, blank_line_comments(&text)));
                }
            }
        }
        out.sort();
        out
    }

    /// `//` to end of line becomes spaces, outside a string literal.
    fn blank_line_comments(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        for line in source.split('\n') {
            let bytes = line.as_bytes();
            let mut quotes = 0usize;
            let mut cut = None;
            let mut i = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => quotes += 1,
                    b'/' if quotes.is_multiple_of(2) && bytes.get(i + 1) == Some(&b'/') => {
                        cut = Some(i);
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            match cut {
                Some(at) => {
                    out.push_str(&line[..at]);
                    for _ in at..line.len() {
                        out.push(' ');
                    }
                }
                None => out.push_str(line),
            }
            out.push('\n');
        }
        out
    }

    /// A byte window of `source` from `from`, clipped back to a char boundary.
    fn window(source: &str, from: usize, len: usize) -> &str {
        let mut end = from.saturating_add(len).min(source.len());
        while end > from && !source.is_char_boundary(end) {
            end -= 1;
        }
        source.get(from..end).unwrap_or("")
    }

    /// Does the worker named at `site` declare a role where that role REACHES
    /// its thread? The three body shapes are described on the census test.
    fn declares_a_role(sources: &[(String, String)], source: &str, site: usize) -> bool {
        const DECL: &str = "qos::set_self";
        let marker = String::from(".name") + "(\"aterm-";
        let Some(offset) = window(source, site, 800).find(".spawn(") else {
            return false;
        };
        let body = site + offset + ".spawn(".len();
        let trimmed = window(source, body, 200).trim_start();
        if trimmed.starts_with('|') || trimmed.starts_with("move |") {
            let mut inline = window(source, body, 2000);
            if let Some(next) = inline.find(&marker) {
                inline = &inline[..next];
            }
            return inline.contains(DECL);
        }
        let path: String = trimmed
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        if let Some(target) = path.rsplit("::").next().filter(|t| !t.is_empty()) {
            let needle = format!("fn {target}");
            let mut heads: Vec<&str> = Vec::new();
            for (_, other) in sources {
                for (at, _) in other.match_indices(&needle) {
                    let tail = window(other, at + needle.len(), 1);
                    if tail != "(" && tail != "<" {
                        continue;
                    }
                    // A DEFINITION, not a mention: `fn` opens its line, behind
                    // nothing but a visibility or a qualifier. The tests in this
                    // very file quote such a signature inside a string literal.
                    let line = other[..at].rfind('\n').map_or(0, |nl| nl + 1);
                    let opens = other[line..at].split_whitespace().all(|word| {
                        matches!(
                            word,
                            "pub" | "pub(crate)" | "pub(super)" | "async" | "const" | "unsafe"
                        )
                    });
                    if opens {
                        heads.push(window(other, at, 900));
                    }
                }
            }
            if heads.len() == 1 && heads[0].contains(DECL) {
                return true;
            }
        }
        // A body handed to a SEAM: every closure passed to the enclosing
        // function declares, and there is at least one caller to pass one.
        let Some(seam) = enclosing_fn(source, site) else {
            return false;
        };
        let call = format!("{seam}(");
        let (mut calls, mut declared) = (0usize, 0usize);
        for (_, other) in sources {
            for (at, _) in other.match_indices(&call) {
                let before = &other[..at];
                if before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    continue;
                }
                if before.trim_end().ends_with("fn") {
                    continue; // the seam's own definition
                }
                calls += 1;
                if window(other, at + call.len(), 800).contains(DECL) {
                    declared += 1;
                }
            }
        }
        calls > 0 && calls == declared
    }

    /// The name of the function enclosing `site`.
    fn enclosing_fn(source: &str, site: usize) -> Option<String> {
        let head = &source[..site];
        let mut name = None;
        for (at, _) in head.match_indices("fn ") {
            if head[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let found: String = head[at + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !found.is_empty() {
                name = Some(found);
            }
        }
        name
    }

    /// The ORDER is the contract: cosmetic work must never outrank the work a
    /// human is waiting on. Pinned as a test because the roles are assigned at
    /// ~20 scattered spawn sites, and a wrong one is invisible at the call site
    /// — it shows up only as latency on a loaded machine.
    #[cfg(target_os = "macos")]
    #[test]
    fn roles_rank_interactive_above_cosmetic_work() {
        let rank = |role| qos_class(role) as u32;
        assert!(
            rank(Role::Interactive) > rank(Role::Responsive),
            "input egress must outrank the PTY drain"
        );
        assert!(
            rank(Role::Responsive) > rank(Role::Background),
            "the PTY drain must outrank font warming and smart titles"
        );
        assert!(
            rank(Role::Background) > rank(Role::Housekeeping),
            "useful background work must outrank the artifact-quarantine sweep"
        );
    }

    /// The inherited class a silent `spawn` leaves a thread at sits BETWEEN the
    /// two roles the floor rule chooses between: demoting a lock holder to
    /// `Background` really is a demotion below what it had, and `Responsive`
    /// really is a promotion. If Apple ever renumbers the classes this is the
    /// assertion that notices.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_undeclared_default_sits_between_responsive_and_background() {
        let default = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;
        assert!(qos_class(Role::Responsive) as u32 > default);
        assert!(default > qos_class(Role::Background) as u32);
    }

    /// Off macOS the call must still compile and do nothing, so call sites never
    /// need a `cfg`.
    #[test]
    fn declaring_a_role_is_infallible_everywhere() {
        set_self(Role::Background);
        set_self(Role::Housekeeping);
    }

    fn argv(command: &std::process::Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The wrapping decision, against a stand-in launcher so it runs the same on
    /// every Unix: work below the inherited default runs behind its clamp when both
    /// ends are real; everything else is the program alone, so a missing tool still
    /// fails at `spawn`.
    #[cfg(unix)]
    #[test]
    fn only_below_default_work_with_both_ends_real_is_spawned_behind_a_clamp() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("aterm-qos-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let launcher = dir.join("taskpolicy");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tool = std::path::Path::new("/bin/sh");
        let l = launcher.to_string_lossy().into_owned();

        let run = |role, program: &std::path::Path, launcher: &std::path::Path| {
            argv(&command_with(role, program, launcher))
        };
        assert_eq!(
            run(Role::Background, tool, &launcher),
            [l.as_str(), "-c", "utility", "/bin/sh"]
        );
        assert_eq!(
            run(Role::Housekeeping, tool, &launcher),
            [l.as_str(), "-c", "background", "/bin/sh"]
        );
        for role in [Role::Interactive, Role::Responsive] {
            assert_eq!(
                run(role, tool, &launcher),
                ["/bin/sh"],
                "{role:?} is above the default"
            );
        }
        let missing = dir.join("no-such-tool");
        assert_eq!(
            run(Role::Background, &missing, &launcher),
            [missing.to_string_lossy().into_owned()],
            "a missing program must still fail at spawn"
        );
        assert_eq!(
            run(Role::Background, std::path::Path::new("sh"), &launcher),
            ["sh"]
        );
        assert_eq!(
            run(Role::Background, tool, &dir.join("no-launcher")),
            ["/bin/sh"],
            "no launcher, no wrapper"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The live contract (2026-09-15): a `Background` child really starts below the
    /// inherited band (pri 31). Before [`command`], every such child did not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_background_child_starts_below_the_inherited_band() {
        let output = command(Role::Background, "/bin/sh")
            .args(["-c", "/bin/ps -o pri= -p $$"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let said = String::from_utf8_lossy(&output.stdout);
        let pri: i32 = said
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("no priority in {said:?}"));
        assert!(
            pri <= 20,
            "a Background child ran at pri {pri}, not utility (20)"
        );
    }

    /// Every thread of the package lane DECLARES its role. A role does not reach
    /// a thread any more than it reaches a child: measured 2026-09-15, a thread
    /// created from a `QOS_CLASS_UTILITY` thread reads `QOS_CLASS_DEFAULT`, so a
    /// lane whose entry thread declares `Background` still hands every thread it
    /// spawns back to the band the program being typed into runs in. Undeclared
    /// costs nothing functional — it shows up only as echo lag while a six-hourly
    /// pass parses its child's stdout and tails its progress file.
    #[test]
    fn every_package_lane_thread_declares_its_role() {
        let lib = include_str!("lib.rs");
        let native = include_str!("app_native.rs");
        for (file, source, thread) in [
            ("lib.rs", lib, "atpkg-update"),
            ("lib.rs", lib, "atpkg-machine"),
            ("lib.rs", lib, "atpkg-progress-tail"),
            // The Settings ▸ Packages verb worker: same lane, same six-hourly
            // store, reached by a click instead of by the park. Its CHILD already
            // takes the clamp; the thread that waits on it and parses its output
            // was the half still in the typing band.
            ("app_native.rs", native, "aterm-packages-verb"),
        ] {
            let needle = format!(".name(\"{thread}\".into())");
            let at = source
                .find(&needle)
                .unwrap_or_else(|| panic!("no thread named {thread} in {file}"));
            let head = &source[at..source.len().min(at + 600)];
            assert!(
                head.contains("qos::set_self(crate::qos::Role::Background)"),
                "the {thread} thread declares no role at the top of its closure, \
                 so it runs at the inherited DEFAULT band"
            );
        }
    }

    /// The pooled scrollback rewrap declares [`Role::Responsive`] — the class this
    /// module's own doc names for reflow, and the FLOOR the port-time rule sets for
    /// a worker holding a lock the UI thread contends (`REFLOW_POOL`, which the
    /// main thread takes on every settle to submit). Undeclared it sat at the
    /// inherited DEFAULT: below its intended class, and low enough that a
    /// descheduled worker can hold the pool lock with the resizing UI thread queued
    /// behind it.
    ///
    /// Anchored on the FUNCTION, not on a `.name(…)` closure: the pool spawns
    /// `.spawn(reflow_worker_main)` by name, so the declaration belongs at the
    /// worker's entry rather than at either of the two spawn sites.
    #[test]
    fn the_reflow_pool_worker_declares_its_role_at_its_entry() {
        let source = include_str!("app_render.rs");
        let at = source
            .find("fn reflow_worker_main() {")
            .expect("no reflow_worker_main in app_render.rs");
        let head = &source[at..source.len().min(at + 600)];
        assert!(
            head.contains("qos::set_self(crate::qos::Role::Responsive)"),
            "the aterm-reflow worker declares no role at its entry, so it runs at \
             the inherited DEFAULT band — below the class reflow is meant to have"
        );
    }

    /// EVERY CONTROL-PLANE THREAD THAT SERVES A VERB declares [`Role::Responsive`],
    /// the floor this module's port-time rule sets for a worker holding a lock the
    /// UI thread contends. These lanes hold the TERMINAL MUTEX itself: a `text`
    /// verb formats every visible row under one `term_lock`, and a subscription's
    /// push loop takes it per target per tick. Undeclared, all thirteen of them ran
    /// at the inherited DEFAULT band — below the UI thread, which takes that same
    /// mutex on the key path (`term_lock_ui`) and in the redraw — so an agent
    /// driving a session with `ctl text`/`screen` could leave a descheduled holder
    /// in front of the next keystroke.
    ///
    /// What is asserted is that the declaration sits INSIDE the spawned closure. A
    /// role does not reach a thread from the one that creates it (measured
    /// 2026-09-15, the same finding that put every other entry in this file), so a
    /// `set_self` on the spawning side would read as a fix and change nothing.
    #[test]
    fn every_control_plane_thread_declares_its_role_inside_its_closure() {
        let source = include_str!("control.rs");
        for (thread, anchor) in [
            ("aterm-control-N (8 RPC lanes)", "fn spawn_control_workers("),
            (
                "aterm-subscribe-N (4 push lanes)",
                "fn spawn_subscription_workers(",
            ),
            ("aterm-fabric-bridge", "fn attach_fabric_bridge("),
            (
                "aterm-control-listener",
                ".name(\"aterm-control-listener\".into())",
            ),
        ] {
            let at = source
                .find(anchor)
                .unwrap_or_else(|| panic!("no `{anchor}` in control.rs"));
            let head = &source[at..source.len().min(at + 2000)];
            let spawn = head
                .find(".spawn(")
                .unwrap_or_else(|| panic!("no spawn under `{anchor}`"));
            let declared = head
                .find("qos::set_self(crate::qos::Role::Responsive)")
                .unwrap_or_else(|| {
                    panic!(
                        "the {thread} thread declares no role, so it serves control \
                         verbs at the inherited DEFAULT band while holding the \
                         terminal mutex the UI thread takes on the key path"
                    )
                });
            assert!(
                declared > spawn,
                "the {thread} thread's role is declared on the SPAWNING thread; a \
                 role does not reach the thread that thread creates"
            );
        }
    }

    /// Every `atpkg` child the GUI launches takes the clamp. A bare
    /// `Command::new(atpkg)` compiles, runs and passes every functional test; it
    /// shows up only as echo lag while an update pass verifies a snapshot.
    #[test]
    fn no_atpkg_child_is_spawned_at_the_inherited_band() {
        for (file, source) in [
            ("lib.rs", include_str!("lib.rs")),
            ("app_native.rs", include_str!("app_native.rs")),
        ] {
            for bare in ["Command::new(atpkg)", "Command::new(&atpkg)"] {
                assert!(
                    !source.contains(bare),
                    "{file} spawns `{bare}` at the inherited band; use \
                     `qos::command(Role::Background, …)`"
                );
            }
        }
    }
}
