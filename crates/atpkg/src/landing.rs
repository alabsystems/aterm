// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LANDING WAIT (2026-09-16): what an agent program's front-of-`PATH` twin does while
//! a NEWER pinned build of that program is being fetched, staged and activated.
//!
//! Owner, 2026-09-16, looking at a status row that read "aterm-managed, current — what
//! `claude` and `codex` run in new tabs": *"aterm atpkg DID install the latest but it
//! didn't make them available for me. instead, it is telling me to open a new tab. NO!
//! all the latest and best MUST WORK IN THE SAME TAB with live update!"* and *"when we
//! are waiting for latest updates, have an stderr warning about waiting for the update"*.
//!
//! # What was true before
//!
//! The update pass stages `store/<program>/<build>`, flips the channel, and re-lays
//! `bin/<tool>` and the `agents/<tool>` twin atomically, so the NEXT invocation in any
//! shell whose `PATH` has `agents/` first runs the new build — that half already worked
//! per invocation. But for the minutes the pass is moving bytes the twin silently `exec`s
//! the OLD store build, and nothing says a newer one is on its way. The pending-stub
//! path (`atpkg __pending`) covers only a program that is NOT installed yet; an installed
//! program with a newer pin landing had no voice at all.
//!
//! # The mechanism
//!
//! * **The marker.** When the pass starts fetching a build for an AGENT PROGRAM
//!   ([`crate::stub::AGENT_PROGRAMS`]) that differs from the active one, it writes
//!   `<prefix>/landing/<program>` ([`crate::store::Layout::landing_marker`]) — inside the
//!   store lock, through [`LandingGuard::begin`] — naming the incoming build and version,
//!   the build it replaces, and the pass's pid. The guard removes the marker when it
//!   drops: after activation (by which point `install_tools_env` → `reconcile_agents`
//!   has re-laid the twin onto the new build), on a failed download or stage, and on
//!   any other exit of the install. [`sweep_stale`] — run by `reconcile_agents` at the
//!   end of every pass — removes a marker whose writer is dead or whose build the twin
//!   already runs, so a killed pass never leaves a `claude` waiting.
//! * **The twin's prelude.** The `agents/` twin carries ONE cheap test ahead of its
//!   exports and `exec`: `[ -f '<prefix>/landing/<program>' ]` — a single `stat`. Only
//!   when the marker stands does it hand over to the hidden verb `atpkg __landing
//!   <program> -- <args…>`, through a VARIABLE naming the embedded co-located `atpkg`;
//!   when that binary is not executable it falls through to the store `exec` — never
//!   dangle, and never `command -v atpkg`: an OLDER `atpkg` on PATH answers `__landing`
//!   with exit 2 `unknown verb` and the tool would not run (review, 2026-09-16; the
//!   wait is a courtesy, the `exec` is the guarantee). No line of the prelude is a
//!   literal `exec '`, so
//!   `platform::parse_sh_shim_target` / `resolve_shim` / `sweep_agents_dir`'s keep-
//!   predicate still read the twin's target off its real `exec` line
//!   ([`crate::platform::sh_landing_prelude`]). The prelude also passes the PREFIX as an
//!   operand (`__landing 'claude' '<prefix>' -- "$@"`), so the verb needs no `HOME` to
//!   find the store — an `env -i` wrapper, a launchd job — and never exits without
//!   running the tool over a prefix it could not resolve (review, 2026-09-16).
//! * **The verb** ([`wait_for_landing`], the state machine; `cli::cmd_landing`, its
//!   process): reads the marker and `progress.json` under the untrusted-reader rules,
//!   prints on STDERR `atpkg: waiting for the claude update to land — 2.1.274 (build
//!   2026091702), downloading 42% (12.3 of 29.0 MB) — Ctrl-C runs 2.1.273 now` and
//!   refreshes it every [`REFRESH_SECS`] (a new line, or `\r` on a terminal), bounded by
//!   [`WAIT_SECS_ENV`] (default [`DEFAULT_WAIT_SECS`]; `0` = warn once and do not wait).
//!   LANDED means ONE thing: `bin/<program>` resolves into the incoming build
//!   ([`shim_runs_build`]) — never merely "the marker is gone". The guard's drop removes
//!   the marker on EVERY exit of the install, a failed download or a rolled-back
//!   activation included, and the pass records its `failed` progress row only after the
//!   install has returned — so a vanished marker over an unmoved shim is a landing that
//!   DID NOT HAPPEN, and the verb says so (`atpkg: the claude update did not land (<why>)
//!   — running 2.1.273 now; \`aterm pkg update\` retries it`, the reason from the pass's
//!   progress row when one names it) instead of announcing the new version over the old
//!   build (review finding, 2026-09-16: the first cut read "marker gone" as "landed").
//!   On a landing it `exec`s the CURRENT `bin/<program>` shim, re-resolved after the
//!   wait so the NEW build is what runs, with the arguments verbatim. When the bound
//!   expires or on Ctrl-C (SIGINT stops the wait; it does not kill the command), it says
//!   so and `exec`s the current shim — the build the user had keeps working, and the
//!   line promises only what is true: the new build runs once it lands (a `claude` typed
//!   while the marker still stands waits for it again, bounded the same way). A marker
//!   whose writer is DEAD is stale whatever other pass may be running — the marker names
//!   the pid that was landing this build, and no other process finishes its work —
//!   removed, and the shim runs at once with nothing printed. Each tick looks at the
//!   marker FIRST and the shim SECOND (activation moves the shim before the guard drops
//!   the marker, so that order never reads a landing as a failure), re-reads the marker
//!   on disk and FOLLOWS it when another pass has laid its own over ours (a later tick,
//!   a hand-typed update: its build, its pid), and removes a stale marker only while the
//!   file still names the dead pid — never a live pass's fresh one (review, 2026-09-16).
//! * **The marker's mode** follows the prefix shape ([`marker_mode`]): `0600` under a
//!   `$HOME` prefix, `0644` under a SYSTEM prefix, where root's pass writes it and every
//!   user's twin must read it or the wait is silently disabled there.
//!
//! The verb never `exec`s the `agents/` twin (that would re-enter the prelude); it
//! `exec`s `bin/<program>`, which carries no prelude. Every line it prints goes to
//! stderr and starts with `atpkg: ` — stdout is the tool's.
//!
//! **Windows has no landing wait (residual, documented 2026-09-16).** The `agents/` twin
//! there is the plain `.cmd` shim ([`crate::platform::windows::twin_executable_to_env`]
//! takes the prelude and lays none — `cmd.exe` has no `[ -f ]`/`exec` prelude to carry),
//! so a `claude` typed during a pass runs the current build at once with no line; the
//! per-invocation live update itself (the re-laid `bin/` and twin) holds there as
//! everywhere. Closing it means a `.cmd` prelude of its own.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::store::{Layout, ToolName};

/// The hidden verb the twin's prelude execs: `atpkg __landing <program> -- <args…>`.
/// Unlisted, dispatched before the verb match and before the store lock, like
/// `__pending` and `__reroute` — it must answer while the installer HOLDS the lock.
pub const HIDDEN_VERB: &str = "__landing";

/// How long the verb waits for the landing, in seconds. Unset or unparsable ⇒
/// [`DEFAULT_WAIT_SECS`]; `0` ⇒ warn once and run the current build without waiting.
pub const WAIT_SECS_ENV: &str = "ATPKG_LANDING_WAIT_SECS";

/// The default bound on the wait: long enough for a 30–60 MB agent bundle on a home
/// connection to finish downloading and staging, short enough that a stalled pass never
/// holds a typed command hostage.
pub const DEFAULT_WAIT_SECS: u64 = 45;

/// How often the waiting line is refreshed.
pub const REFRESH_SECS: u64 = 2;

/// The marker is one short line; anything larger is not ours.
const MAX_MARKER_BYTES: usize = 1024;

/// A version string longer than this on the marker is a corrupt or hostile file: elided.
const VERSION_CAP: usize = 64;

/// The marker's schema tag: the first token of its one line.
const MARKER_TAG: &str = "atpkg-landing-v1";

/// One landing marker, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// The build being landed.
    pub build: u64,
    /// The active build it replaces, when the pass knew one.
    pub from_build: Option<u64>,
    /// The pass's pid — dead ⇒ the marker is stale.
    pub pid: u32,
    /// The incoming build's version from its signed manifest (`2.1.274`), when known.
    pub version: Option<String>,
    /// The replaced build's version, when the pass could say it cheaply.
    pub from_version: Option<String>,
}

impl Marker {
    /// `2.1.274 (build 2026091702)`, or `build 2026091702` with no version.
    #[must_use]
    pub fn incoming_words(&self) -> String {
        build_words(self.version.as_deref(), self.build)
    }

    /// The build the user has NOW, as the lines name it: `2.1.273`, `build 2026091601`,
    /// or `the current build` when the pass recorded neither.
    #[must_use]
    pub fn current_words(&self) -> String {
        match (self.from_version.as_deref(), self.from_build) {
            (Some(v), _) => v.to_string(),
            (None, Some(b)) => {
                let mut s = String::from("build ");
                s.push_str(&crate::dec_u64(b));
                s
            }
            (None, None) => String::from("the current build"),
        }
    }
}

/// `<version> (build <n>)` or `build <n>`.
fn build_words(version: Option<&str>, build: u64) -> String {
    let mut s = String::new();
    match version {
        Some(v) => {
            s.push_str(v);
            s.push_str(" (build ");
            s.push_str(&crate::dec_u64(build));
            s.push(')');
        }
        None => {
            s.push_str("build ");
            s.push_str(&crate::dec_u64(build));
        }
    }
    s
}

/// The marker's one line. Hand-built (`push_str`, no `format!`) like every other file
/// body this crate lays (the strict Trust gate, `lib.rs`).
#[must_use]
pub fn render_marker(m: &Marker) -> String {
    let mut s = String::from(MARKER_TAG);
    s.push_str(" build=");
    s.push_str(&crate::dec_u64(m.build));
    s.push_str(" pid=");
    s.push_str(&crate::dec_u64(u64::from(m.pid)));
    s.push_str(" from=");
    match m.from_build {
        Some(b) => s.push_str(&crate::dec_u64(b)),
        None => s.push('-'),
    }
    s.push_str(" version=");
    s.push_str(
        m.version
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("-"),
    );
    s.push_str(" from_version=");
    s.push_str(
        m.from_version
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("-"),
    );
    s.push('\n');
    s
}

/// The inverse of [`render_marker`] over UNTRUSTED text: the tag must lead, `build` and
/// `pid` must parse, and a version is admitted only as version-shaped characters
/// (digits, letters, `.`, `-`, `_`, `+`) capped at [`VERSION_CAP`] — it reaches a TTY.
/// Unknown keys are ignored (a newer writer). `None` for anything else.
#[must_use]
pub fn parse_marker(text: &str) -> Option<Marker> {
    let line = text.lines().next()?;
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some(MARKER_TAG) {
        return None;
    }
    let mut build = None;
    let mut pid = None;
    let mut from_build = None;
    let mut version = None;
    let mut from_version = None;
    for tok in tokens {
        let Some((key, value)) = tok.split_once('=') else {
            continue;
        };
        match key {
            "build" => build = value.parse::<u64>().ok(),
            "pid" => pid = value.parse::<u32>().ok(),
            "from" => from_build = value.parse::<u64>().ok(),
            "version" => version = admit_version(value),
            "from_version" => from_version = admit_version(value),
            _ => {}
        }
    }
    Some(Marker {
        build: build?,
        from_build,
        pid: pid?,
        version,
        from_version,
    })
}

/// A version token for the marker: `-` is none; otherwise only version-shaped
/// characters, capped.
fn admit_version(value: &str) -> Option<String> {
    if value == "-" || value.is_empty() {
        return None;
    }
    let clean: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .take(VERSION_CAP)
        .collect();
    if clean.is_empty() { None } else { Some(clean) }
}

/// The marker path for `program`, or `None` for a name the [`ToolName`] gate refuses.
#[must_use]
pub fn marker_path(layout: &Layout, program: &str) -> Option<PathBuf> {
    ToolName::new(program).map(|t| layout.landing_marker(&t))
}

/// Read `program`'s marker under the untrusted-reader rules ([`crate::metadata_io`]:
/// symlink-refusing, bounded). `None` when absent, oversize or malformed.
#[must_use]
pub fn read_marker(layout: &Layout, program: &str) -> Option<Marker> {
    let path = marker_path(layout, program)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_MARKER_BYTES).ok()?;
    parse_marker(&text)
}

/// Write `program`'s marker atomically (temp + rename) with the mode the PREFIX SHAPE
/// calls for ([`marker_mode`]): `0600` in the `0700` landing dir of a `$HOME` prefix;
/// `0644` in the `0755` one of a SYSTEM prefix, where root's pass writes it and every
/// unprivileged user's twin must still READ it — the same argument `store.rs` settles for
/// `.ready`. The first cut chmod'ed `0600` unconditionally, so under a system prefix a
/// user's `[ -f marker ]` was true (a `stat` needs only the dir's search bit) while the
/// verb's bounded read got `EACCES` → "not landing" → the OLD build at once, with one
/// extra exec hop and no line (review finding, 2026-09-16). The content is a build number,
/// a version and a pid — nothing private.
///
/// # Errors
/// The landing dir cannot be created or the file cannot be written.
pub fn write_marker(layout: &Layout, program: &str, m: &Marker) -> io::Result<()> {
    let path = marker_path(layout, program)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a tool name"))?;
    layout.ensure_dir(&layout.landing_dir())?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(program);
    tmp_name.push_str(".tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let mode = marker_mode(layout);
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, mode)?;
        f.write_all(render_marker(m).as_bytes())?;
        // `open(2)`'s mode is masked by the umask; the marker's readability under a
        // system prefix is the point, so re-state it on the inode we hold.
        crate::platform::set_mode_on(&f, mode)
    })();
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The marker's file mode for `layout`'s prefix shape: `0644` under a system prefix
/// (root-owned chain, every user reads), else `0600`.
#[must_use]
pub fn marker_mode(layout: &Layout) -> u32 {
    if layout.is_system_prefix() {
        0o644
    } else {
        0o600
    }
}

/// Remove `program`'s marker ONLY while the file on disk still names `pid` as its
/// writer. The stale arms hold a marker they read a moment ago; a LIVE pass (a later
/// tick's, a hand-typed update's) may have renamed its own marker over that file since,
/// and deleting that one would silence every `claude` typed for the whole of ITS landing
/// (review finding, 2026-09-16). Silent when there is none, or when it is another's.
pub fn remove_marker_of(layout: &Layout, program: &str, pid: u32) {
    if read_marker(layout, program).is_some_and(|m| m.pid == pid) {
        remove_marker(layout, program);
    }
}

/// Remove `program`'s marker; silent when there is none.
pub fn remove_marker(layout: &Layout, program: &str) {
    if let Some(path) = marker_path(layout, program) {
        let _ = std::fs::remove_file(path);
    }
}

/// THE PASS'S SIDE: the marker's lifetime, tied to the install that lands the build.
/// Created by the install flow at the moment it starts fetching (`flow::install_inner`,
/// after the digest memo and the staging path are settled, inside the store lock);
/// dropped — and the marker removed — when that install returns, whichever way.
#[must_use = "the marker lives exactly as long as this guard"]
pub struct LandingGuard {
    layout: Layout,
    program: String,
}

impl LandingGuard {
    /// Write the marker and return its guard — ONLY for an agent program with an active
    /// build (`installed`, else the `bin/` view) that DIFFERS from `pinned`. Every other
    /// install returns `None` and writes nothing: a first install is the pending stub's
    /// to announce, a re-install of the active build changes what nobody is waiting for,
    /// and a non-agent program has no twin to wait in. A marker that cannot be written
    /// costs one stderr line and nothing else — the install goes on; the twin simply runs
    /// the old build meanwhile, as it did before this existed.
    pub fn begin(
        layout: &Layout,
        program: &str,
        installed: Option<u64>,
        pinned: u64,
        version: &str,
        from_version: Option<&str>,
    ) -> Option<Self> {
        if !crate::stub::is_agent_program(program) {
            return None;
        }
        let active =
            installed.or_else(|| crate::ops::active_builds(layout).get(program).copied())?;
        if active == pinned {
            return None;
        }
        let marker = Marker {
            build: pinned,
            from_build: Some(active),
            pid: std::process::id(),
            version: admit_version(version),
            from_version: from_version.and_then(admit_version),
        };
        if let Err(e) = write_marker(layout, program, &marker) {
            eprintln!(
                "atpkg: {program}: the landing marker was not written ({e}) — a `{program}` \
                 typed during this update runs the current build without a wait"
            );
            return None;
        }
        Some(Self {
            layout: layout.clone(),
            program: program.to_string(),
        })
    }
}

impl Drop for LandingGuard {
    fn drop(&mut self) {
        remove_marker(&self.layout, &self.program);
    }
}

/// Whether `bin/<program>` (the shim the verb will exec) already resolves into
/// `store/<program>/<build>` — the landing is complete for the copy that runs.
#[must_use]
pub fn shim_runs_build(layout: &Layout, program: &str, build: u64) -> bool {
    let Some(tool) = ToolName::new(program) else {
        return false;
    };
    crate::platform::resolve_shim(&layout.shim(&tool)).is_some_and(|t| {
        crate::ops::store_build_of(&layout.prefix, &t)
            .is_some_and(|(p, b)| p == program && b == build)
    })
}

/// Remove every agent program's marker that is STALE: its writer is not this process
/// and is dead, or the shim already runs the build it announces. Run by
/// `activate::reconcile_agents` at the end of every pass, so a pass killed mid-transfer
/// never leaves a `claude` waiting 45 s for nothing. Silent, best-effort.
pub fn sweep_stale(layout: &Layout) {
    let me = std::process::id();
    for program in crate::stub::AGENT_PROGRAMS {
        let Some(m) = read_marker(layout, program) else {
            // A present-but-unparsable file is not ours to keep either.
            if marker_path(layout, program).is_some_and(|p| p.exists()) {
                remove_marker(layout, program);
            }
            continue;
        };
        if shim_runs_build(layout, program, m.build) {
            remove_marker(layout, program);
        } else if m.pid != me && !crate::progress::pid_alive(m.pid) {
            remove_marker_of(layout, program, m.pid);
        }
    }
}

/// [`WAIT_SECS_ENV`]'s value as the bound: unset, empty or unparsable ⇒
/// [`DEFAULT_WAIT_SECS`]; a number is taken as is (`0` = warn once, no wait).
#[must_use]
pub fn wait_secs_from_env(value: Option<&std::ffi::OsStr>) -> u64 {
    value
        .and_then(|v| v.to_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_WAIT_SECS)
}

/// The verb's I/O seam, so the state machine runs in tests with no process, no TTY
/// and no real sleep: `say(line, refresh)` receives each stderr line — `refresh`
/// true for the periodic waiting line (a terminal overwrites it with `\r`), false for
/// a final line; `sleep(d)` waits and answers `false` when a SIGINT arrived meanwhile.
pub struct LandingIo<'a> {
    pub say: &'a mut dyn FnMut(&str, bool),
    pub sleep: &'a mut dyn FnMut(Duration) -> bool,
}

/// How [`wait_for_landing`] ended. Every arm but `NotLanding` and `Stale` printed at
/// least one line; every arm means "exec the current `bin/<program>` now".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No marker: nothing is landing — run at once, say nothing.
    NotLanding,
    /// The marker's writer is dead: the marker was removed (while it still named that
    /// pid) — run at once, say nothing.
    Stale,
    /// `bin/<program>` resolves into the incoming build (waited ⇒ a line said so).
    Landed { waited: bool },
    /// The bound was `0`: one line, no wait.
    NoWait,
    /// The bound expired with the landing still in flight.
    TimedOut,
    /// SIGINT during the wait.
    Interrupted,
    /// The landing ended without the shim moving — the marker went while `bin/` still
    /// runs the old build (a failed download or stage, a rolled-back activation), or the
    /// pass's progress row says FAILED; the line carries the reason when one is recorded.
    Failed,
}

/// The `__landing` state machine — see the module doc. `max_wait_secs` bounds the wait
/// ([`wait_secs_from_env`]).
pub fn wait_for_landing(
    layout: &Layout,
    program: &str,
    max_wait_secs: u64,
    io: &mut LandingIo<'_>,
) -> Outcome {
    let Some(mut marker) = read_marker(layout, program) else {
        return Outcome::NotLanding;
    };
    let me = std::process::id();
    let mut waited_secs: u64 = 0;
    loop {
        // ONE look per tick, in THIS order: the marker FIRST, the shim SECOND. Activation
        // re-lays `bin/` and the twin BEFORE the guard drops the marker, so a marker seen
        // gone over a shim then seen unmoved is a landing that did not happen — while the
        // other order (shim, then marker) left a window in which the pass re-laid `bin/`
        // and dropped the guard between the two calls, and the verb said "did not land —
        // running <old> now" over an exec of the NEW build (review finding, 2026-09-16).
        let on_disk = read_marker(layout, program);
        // LANDED means the shim moved: `bin/<program>` resolves into the incoming build.
        // The marker's presence says nothing about the outcome.
        if shim_runs_build(layout, program, marker.build) {
            if waited_secs > 0 {
                (io.say)(&landed_line(program, &marker), false);
            }
            return Outcome::Landed {
                waited: waited_secs > 0,
            };
        }
        match on_disk {
            // The marker went and the shim did not move: the install returned some other
            // way — a failed download or stage, a refused digest, a rolled-back activation
            // (`abort_activated_install` re-points `bin/` at the old build, then the guard
            // drops). The first cut read this as "landed" and announced the new version
            // over the old build (review finding, 2026-09-16). Say what happened, with the
            // reason the pass records a moment later when it does.
            None => {
                let why = failure_reason_after_marker(layout, program, &marker, io);
                say_did_not_land(program, &marker, why.as_deref(), io);
                return Outcome::Failed;
            }
            // Another marker stands where ours did — a later tick's pass or a hand-typed
            // update adopted the landing (its own pid, maybe a newer build). Follow the
            // file: the build to wait for and the writer to watch are ITS, and a stale
            // verdict on the marker we first read must never delete this live one
            // (review finding, 2026-09-16). Back to the top: the shim may already run it.
            Some(fresh) if fresh != marker => {
                marker = fresh;
                continue;
            }
            Some(_) => {}
        }
        // Stale: the writer is dead. The marker names the pid that was landing THIS
        // build; no other pass — a later tick's, a hand-typed update's — finishes its
        // work, so another live pass is no reason to wait (a `claude` used to wait the
        // full bound for a landing nobody was doing while an unrelated pass ran). Removed
        // only while the file still names that dead pid.
        let writer_alive = marker.pid == me || crate::progress::pid_alive(marker.pid);
        if !writer_alive {
            remove_marker_of(layout, program, marker.pid);
            return Outcome::Stale;
        }
        // The pass's own word on this program, when a LIVE snapshot carries one.
        let now = crate::flow::now_unix().max(0).unsigned_abs();
        let snapshot = crate::progress::read_progress(layout);
        let row = snapshot
            .as_ref()
            .filter(|f| {
                f.v == crate::progress::PROGRESS_VERSION
                    && crate::progress::snapshot_running(f, now)
            })
            .and_then(|f| f.programs.get(program).cloned());
        if let Some(r) = row
            .as_ref()
            .filter(|r| r.phase == crate::progress::Phase::Failed)
        {
            say_did_not_land(program, &marker, r.error.as_deref(), io);
            return Outcome::Failed;
        }
        let progress = progress_words(row.as_ref());
        if max_wait_secs == 0 {
            let mut s = String::from("atpkg: the ");
            s.push_str(program);
            s.push_str(" update is landing — ");
            s.push_str(&marker.incoming_words());
            s.push_str(", ");
            s.push_str(&progress);
            s.push_str("; running ");
            s.push_str(&marker.current_words());
            s.push_str(" now; ");
            s.push_str(ONCE_IT_LANDS);
            (io.say)(&s, false);
            return Outcome::NoWait;
        }
        if waited_secs >= max_wait_secs {
            let mut s = String::from("atpkg: the ");
            s.push_str(program);
            s.push_str(" update is still landing — running ");
            s.push_str(&marker.current_words());
            s.push_str(" now; ");
            s.push_str(ONCE_IT_LANDS);
            (io.say)(&s, false);
            return Outcome::TimedOut;
        }
        let mut line = String::from("atpkg: waiting for the ");
        line.push_str(program);
        line.push_str(" update to land — ");
        line.push_str(&marker.incoming_words());
        line.push_str(", ");
        line.push_str(&progress);
        line.push_str(" — Ctrl-C runs ");
        line.push_str(&marker.current_words());
        line.push_str(" now");
        // Every tick, same words or not: a terminal gets its `\r` refresh, and the
        // caller's `say` is what keeps a log free of duplicates.
        (io.say)(&line, true);
        let step = REFRESH_SECS
            .min(max_wait_secs.saturating_sub(waited_secs))
            .max(1);
        if !(io.sleep)(Duration::from_secs(step)) {
            let mut s = String::from("atpkg: stopped waiting — running ");
            s.push_str(program);
            s.push(' ');
            s.push_str(&marker.current_words());
            s.push_str(" now; ");
            s.push_str(ONCE_IT_LANDS);
            (io.say)(&s, false);
            return Outcome::Interrupted;
        }
        waited_secs = waited_secs.saturating_add(step);
    }
}

/// The tail every "running the current build" line ends on. It promises exactly what
/// the mechanism guarantees: the new build runs once it lands. NOT "the next `claude`
/// runs the new build" — while the marker stands the next `claude` waits for it again,
/// and a pass that fails lands nothing (review finding, 2026-09-16).
const ONCE_IT_LANDS: &str = "the new build runs once it lands";

/// `atpkg: the claude update landed — running 2.1.274 (build 2026091702)`.
fn landed_line(program: &str, marker: &Marker) -> String {
    let mut s = String::from("atpkg: the ");
    s.push_str(program);
    s.push_str(" update landed — running ");
    s.push_str(&marker.incoming_words());
    s
}

/// `atpkg: the claude update did not land (<why>) — running 2.1.273 now; \`aterm pkg
/// update\` retries it` — the parenthetical only when a reason is recorded.
fn say_did_not_land(program: &str, marker: &Marker, why: Option<&str>, io: &mut LandingIo<'_>) {
    let mut s = String::from("atpkg: the ");
    s.push_str(program);
    s.push_str(" update did not land");
    if let Some(why) = why.map(str::trim).filter(|w| !w.is_empty()) {
        s.push_str(" (");
        s.push_str(&crate::progress::sanitize_for_tty(why, 300));
        s.push(')');
    }
    s.push_str(" — running ");
    s.push_str(&marker.current_words());
    s.push_str(" now; `aterm pkg update` retries it");
    (io.say)(&s, false);
}

/// One short grace (through the seam, so a test injects it), then the pass's recorded
/// reason for THIS landing: a `failed` row for `program` in a snapshot the marker's
/// writer wrote (`pid`), or whose row names the marker's build. The row is written by
/// `note_finished` a moment AFTER the guard drops — hence the grace — and may be absent
/// altogether (a hand-typed `aterm pkg update` writes no progress file): `None` then.
fn failure_reason_after_marker(
    layout: &Layout,
    program: &str,
    marker: &Marker,
    io: &mut LandingIo<'_>,
) -> Option<String> {
    let _ = (io.sleep)(FAILURE_GRACE);
    let file = crate::progress::read_progress(layout)?;
    if file.v != crate::progress::PROGRESS_VERSION {
        return None;
    }
    let row = file.programs.get(program)?;
    let ours = file.pid == Some(marker.pid) || row.build == Some(marker.build);
    if !ours || row.phase != crate::progress::Phase::Failed {
        return None;
    }
    row.error.clone()
}

/// How long [`failure_reason_after_marker`] gives the pass to record its row.
const FAILURE_GRACE: Duration = Duration::from_millis(300);

/// `downloading 42% (12.3 of 29.0 MB)`, `verifying`, `extracting 61% (…)`,
/// `activating`, `queued`, or `installing` when no row says.
fn progress_words(row: Option<&crate::progress::ProgramProgress>) -> String {
    use crate::progress::Phase;
    let Some(r) = row else {
        return String::from("installing");
    };
    let metered = |verb: &str| {
        let mut s = String::from(verb);
        if r.bytes_total > 0 {
            s.push(' ');
            s.push_str(&crate::dec_u64(
                r.bytes_done.saturating_mul(100) / r.bytes_total.max(1),
            ));
            s.push_str("% (");
            s.push_str(&megabytes(r.bytes_done));
            s.push_str(" of ");
            s.push_str(&megabytes(r.bytes_total));
            s.push_str(" MB)");
        }
        s
    };
    match r.phase {
        Phase::Download => metered("downloading"),
        Phase::Extract => metered("extracting"),
        Phase::Verify => String::from("verifying"),
        Phase::Link | Phase::Done => String::from("activating"),
        Phase::Queued => String::from("queued"),
        Phase::Skipped | Phase::Failed => String::from("installing"),
    }
}

/// `bytes` as `12.3` (one decimal, megabytes), loop-free like [`crate::dec_u64`].
fn megabytes(bytes: u64) -> String {
    let tenths = bytes / 100_000;
    let mut s = crate::dec_u64(tenths / 10);
    s.push('.');
    s.push_str(&crate::dec_u64(tenths % 10));
    s
}

/// The pure half — parsing, rendering, the env bound, the meter words — runs on every
/// platform; the state machine needs a store it can lay `sh` shims into (below).
#[cfg(test)]
mod pure_tests {
    use super::*;

    fn marker() -> Marker {
        Marker {
            build: 2_026_091_702,
            from_build: Some(2_026_091_601),
            pid: 4242,
            version: Some("2.1.274".into()),
            from_version: Some("2.1.273".into()),
        }
    }

    #[test]
    fn the_marker_renders_parses_and_refuses_what_is_not_ours() {
        let m = marker();
        assert_eq!(
            render_marker(&m),
            "atpkg-landing-v1 build=2026091702 pid=4242 from=2026091601 version=2.1.274 \
             from_version=2.1.273\n"
        );
        assert_eq!(parse_marker(&render_marker(&m)), Some(m.clone()));
        // No versions, no from-build: dashes, and they read back as None.
        let bare = Marker {
            build: 7,
            from_build: None,
            pid: 1,
            version: None,
            from_version: None,
        };
        assert_eq!(
            render_marker(&bare),
            "atpkg-landing-v1 build=7 pid=1 from=- version=- from_version=-\n"
        );
        assert_eq!(parse_marker(&render_marker(&bare)), Some(bare.clone()));
        assert_eq!(bare.incoming_words(), "build 7");
        assert_eq!(bare.current_words(), "the current build");
        assert_eq!(m.incoming_words(), "2.1.274 (build 2026091702)");
        assert_eq!(m.current_words(), "2.1.273");
        // Hostile: a version carrying an escape is stripped to its safe characters; an
        // unknown key is ignored; a missing build or pid is no marker.
        let hostile = "atpkg-landing-v1 build=9 pid=2 from=- version=1.0\u{1b}[31m;x from_version=- extra=1\n";
        let p = parse_marker(hostile).unwrap();
        assert_eq!(p.version.as_deref(), Some("1.031mx"));
        assert_eq!(parse_marker("atpkg-landing-v1 pid=2\n"), None);
        assert_eq!(parse_marker("something else\n"), None);
        assert_eq!(parse_marker(""), None);
    }

    #[test]
    fn the_bound_reads_the_env_with_a_default_and_a_zero() {
        assert_eq!(wait_secs_from_env(None), DEFAULT_WAIT_SECS);
        assert_eq!(
            wait_secs_from_env(Some(std::ffi::OsStr::new(""))),
            DEFAULT_WAIT_SECS
        );
        assert_eq!(
            wait_secs_from_env(Some(std::ffi::OsStr::new("abc"))),
            DEFAULT_WAIT_SECS
        );
        assert_eq!(wait_secs_from_env(Some(std::ffi::OsStr::new(" 10 "))), 10);
        assert_eq!(wait_secs_from_env(Some(std::ffi::OsStr::new("0"))), 0);
        assert_eq!(DEFAULT_WAIT_SECS, 45);
        assert_eq!(WAIT_SECS_ENV, "ATPKG_LANDING_WAIT_SECS");
        assert_eq!(HIDDEN_VERB, "__landing");
    }

    #[test]
    fn megabytes_and_percent_render_one_decimal() {
        assert_eq!(megabytes(12_300_000), "12.3");
        assert_eq!(megabytes(29_000_000), "29.0");
        assert_eq!(megabytes(0), "0.0");
        assert_eq!(megabytes(950_000), "0.9");
        let row = crate::progress::ProgramProgress {
            phase: crate::progress::Phase::Extract,
            bytes_done: 61,
            bytes_total: 100,
            build: None,
            bumped: false,
            bumped_with: None,
            error: None,
        };
        assert_eq!(progress_words(Some(&row)), "extracting 61% (0.0 of 0.0 MB)");
        assert_eq!(progress_words(None), "installing");
    }
}

/// The state machine and the marker's file half: `sh` shims, `chmod`, a symlink — the
/// Unix twin is the only one that carries the prelude (`platform::windows` lays the
/// plain shim), so this half is Unix-only like `activate`'s twin tests.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn temp_layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-landing-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// A managed `claude` at `build`: the store tree, its `bin/` shim and `agents/` twin.
    fn install_claude(l: &Layout, build: u64) -> PathBuf {
        let dir = l.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            dir.join("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        crate::activate::install_shims(l, &dir, &["claude".to_string()], crate::Aliases::Off)
            .unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// A live snapshot (this pid, a fresh heartbeat) with one `claude` row.
    fn live_progress(l: &Layout, row: &str) {
        std::fs::write(
            l.progress_file(),
            format!(
                "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"heartbeat_unix\":{},\
                 \"overall\":{{\"programs_done\":0,\"programs_total\":1}},\
                 \"queue\":[],\"programs\":{{\"claude\":{row}}}}}",
                std::process::id(),
                now_secs()
            ),
        )
        .unwrap();
    }

    fn marker(pid: u32) -> Marker {
        Marker {
            build: 2_026_091_702,
            from_build: Some(2_026_091_601),
            pid,
            version: Some("2.1.274".into()),
            from_version: Some("2.1.273".into()),
        }
    }

    /// Run the state machine collecting `(line, refresh)`; `on_sleep(n)` runs before the
    /// n-th sleep (1-based) and answers whether the sleep completed (false = SIGINT).
    fn run(
        l: &Layout,
        max: u64,
        mut on_sleep: impl FnMut(u32) -> bool,
    ) -> (Outcome, Vec<(String, bool)>) {
        let mut lines: Vec<(String, bool)> = Vec::new();
        let mut say = |line: &str, refresh: bool| lines.push((line.to_string(), refresh));
        let mut n = 0u32;
        let mut sleep = |_d: Duration| {
            n += 1;
            on_sleep(n)
        };
        let mut io = LandingIo {
            say: &mut say,
            sleep: &mut sleep,
        };
        let out = wait_for_landing(l, "claude", max, &mut io);
        (out, lines)
    }

    #[test]
    fn the_marker_round_trips_through_the_file_and_refuses_a_symlink() {
        let l = temp_layout("marker");
        let m = marker(4242);
        write_marker(&l, "claude", &m).unwrap();
        assert_eq!(read_marker(&l, "claude"), Some(m.clone()));
        // Not a tool name: no path, no write.
        assert!(marker_path(&l, "../x").is_none());
        assert!(write_marker(&l, "../x", &m).is_err());
        // A symlink where the marker should be is refused by the bounded reader.
        remove_marker(&l, "claude");
        std::os::unix::fs::symlink(
            "/etc/hosts",
            l.landing_marker(&ToolName::new("claude").unwrap()),
        )
        .unwrap();
        assert_eq!(read_marker(&l, "claude"), None);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE GUARD: written only for an agent program whose active build differs from the
    /// pin; removed when it drops (the install returned, whichever way).
    #[test]
    fn the_guard_marks_only_an_agent_update_and_removes_on_drop() {
        let l = temp_layout("guard");
        install_claude(&l, 2_026_091_601);
        // Not an agent program: nothing.
        assert!(LandingGuard::begin(&l, "ay", Some(1), 2, "1.0", None).is_none());
        // The active build IS the pin: nothing (a re-install changes nothing).
        assert!(
            LandingGuard::begin(
                &l,
                "claude",
                Some(2_026_091_601),
                2_026_091_601,
                "2.1.273",
                None
            )
            .is_none()
        );
        // Not installed at all (the request says None and bin/ has no other program):
        // the pending stub's case, not the landing's.
        assert!(LandingGuard::begin(&l, "codex", None, 1, "0.1", None).is_none());
        // An update: the marker names both builds and the pass's pid; the request's
        // `installed` may be None — the bin/ view answers.
        let g = LandingGuard::begin(
            &l,
            "claude",
            None,
            2_026_091_702,
            "2.1.274",
            Some("2.1.273"),
        )
        .expect("an agent update writes the marker");
        let m = read_marker(&l, "claude").unwrap();
        assert_eq!(m.build, 2_026_091_702);
        assert_eq!(m.from_build, Some(2_026_091_601));
        assert_eq!(m.pid, std::process::id());
        assert_eq!(m.version.as_deref(), Some("2.1.274"));
        assert_eq!(m.from_version.as_deref(), Some("2.1.273"));
        drop(g);
        assert!(
            read_marker(&l, "claude").is_none(),
            "the drop removes the marker"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE STALE SWEEP: a dead writer's marker goes; a marker whose build the shim
    /// already runs goes; a live writer's marker for a build still landing stays.
    #[test]
    fn the_sweep_removes_dead_and_landed_markers_and_keeps_a_live_one() {
        let l = temp_layout("sweep");
        install_claude(&l, 2_026_091_601);
        // Dead writer (a pid no process can have).
        write_marker(&l, "claude", &marker(u32::MAX)).unwrap();
        sweep_stale(&l);
        assert!(read_marker(&l, "claude").is_none(), "dead writer: swept");
        // Live writer (this process), build still landing: kept.
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        sweep_stale(&l);
        assert!(read_marker(&l, "claude").is_some(), "live writer: kept");
        // The shim now runs the announced build: swept even with a live writer.
        install_claude(&l, 2_026_091_702);
        sweep_stale(&l);
        assert!(read_marker(&l, "claude").is_none(), "landed: swept");
        // Garbage under the marker's name is swept too.
        std::fs::write(
            l.landing_marker(&ToolName::new("codex").unwrap()),
            b"junk\n",
        )
        .unwrap();
        sweep_stale(&l);
        assert!(!l.landing_marker(&ToolName::new("codex").unwrap()).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE MODE FOLLOWS THE PREFIX SHAPE (review finding, 2026-09-16): a `$HOME` prefix
    /// keeps the marker `0600`; a system prefix (root-owned chain; only when this test
    /// runs as root, like `activate.rs`'s `system_prefix_fixture`) lays it `0644` so an
    /// unprivileged twin's verb can READ it — the first cut's unconditional `0600` made
    /// `[ -f marker ]` true and the bounded read `EACCES` there: "not landing", the old
    /// build, one extra exec hop, no line.
    #[test]
    fn the_marker_is_private_under_home_and_world_readable_under_a_system_prefix() {
        let l = temp_layout("mode");
        assert!(!l.is_system_prefix());
        assert_eq!(marker_mode(&l), 0o600);
        write_marker(&l, "claude", &marker(4242)).unwrap();
        let mode = std::fs::metadata(l.landing_marker(&ToolName::new("claude").unwrap()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a $HOME prefix's marker is private");
        let _ = std::fs::remove_dir_all(&l.prefix);
        // The system shape: root only. Mirrors `activate::tests::system_prefix_fixture`.
        let mut system = None;
        for parent in ["/opt", "/usr/local", "/var/lib", "/usr/lib"] {
            let prefix = std::path::Path::new(parent)
                .join(format!("atpkg-landing-mode-{}", std::process::id()));
            let layout = Layout { prefix };
            if !layout.is_system_prefix() {
                continue;
            }
            let _ = std::fs::remove_dir_all(&layout.prefix);
            if std::fs::create_dir(&layout.prefix).is_err() {
                continue;
            }
            let shaped =
                std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o755))
                    .is_ok()
                    && layout.is_system_prefix();
            if !shaped {
                let _ = std::fs::remove_dir_all(&layout.prefix);
                continue;
            }
            system = Some(layout);
            break;
        }
        let Some(l) = system else {
            eprintln!("not root: the system-prefix half of this test needs a root-owned chain");
            return;
        };
        assert_eq!(marker_mode(&l), 0o644);
        write_marker(&l, "claude", &marker(4242)).unwrap();
        let path = l.landing_marker(&ToolName::new("claude").unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "every user's twin reads a system prefix's marker"
        );
        let dir_mode = std::fs::metadata(l.landing_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o755, "and traverses its dir");
        assert_eq!(read_marker(&l, "claude"), Some(marker(4242)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A stale removal is keyed on the pid it judged: the file is left alone when a
    /// LIVE pass has laid its own marker over it since (review finding, 2026-09-16), and
    /// the sweep keys the same way.
    #[test]
    fn a_stale_removal_never_deletes_a_marker_another_writer_laid_since() {
        let l = temp_layout("keyed");
        install_claude(&l, 2_026_091_601);
        let live = marker(std::process::id());
        write_marker(&l, "claude", &live).unwrap();
        // Judged stale as pid u32::MAX — but the file names a live writer now: kept.
        remove_marker_of(&l, "claude", u32::MAX);
        assert_eq!(
            read_marker(&l, "claude"),
            Some(live.clone()),
            "not ours to delete"
        );
        // The file names the dead pid: removed.
        write_marker(&l, "claude", &marker(u32::MAX)).unwrap();
        remove_marker_of(&l, "claude", u32::MAX);
        assert!(read_marker(&l, "claude").is_none());
        // Nothing there: silent.
        remove_marker_of(&l, "claude", u32::MAX);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn no_marker_is_not_landing_and_says_nothing() {
        let l = temp_layout("none");
        install_claude(&l, 2_026_091_601);
        let (out, lines) = run(&l, 45, |_| true);
        assert_eq!(out, Outcome::NotLanding);
        assert!(lines.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A marker whose writer is dead is stale — with no pass running, AND with some
    /// OTHER pass live (the marker names the pid that was landing this build; a later
    /// tick's pass does not finish its work, review 2026-09-16): removed, silent.
    #[test]
    fn a_stale_marker_self_heals_silently_whatever_other_pass_runs() {
        let l = temp_layout("stale");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(u32::MAX)).unwrap();
        let (out, lines) = run(&l, 45, |_| true);
        assert_eq!(out, Outcome::Stale);
        assert!(lines.is_empty(), "{lines:?}");
        assert!(read_marker(&l, "claude").is_none(), "removed");
        // Another pass (this process, a fresh heartbeat) is live and carries no word on
        // claude — still stale: nobody is landing this build.
        write_marker(&l, "claude", &marker(u32::MAX)).unwrap();
        live_progress(&l, "{\"phase\":\"queued\"}");
        let mut file = crate::progress::read_progress(&l).unwrap();
        file.programs.clear();
        std::fs::write(l.progress_file(), aterm_json::to_string(&file).unwrap()).unwrap();
        let (out, lines) = run(&l, 45, |_| panic!("no wait for a dead writer"));
        assert_eq!(out, Outcome::Stale);
        assert!(lines.is_empty(), "{lines:?}");
        assert!(read_marker(&l, "claude").is_none(), "removed");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The waiting line, refreshed with live bytes, then the landing: the shim runs the
    /// new build (and the marker goes a moment later, as the guard drops).
    #[test]
    fn waits_with_live_progress_then_runs_the_landed_build() {
        let l = temp_layout("lands");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(
            &l,
            "{\"phase\":\"download\",\"bytes_done\":12300000,\"bytes_total\":29000000,\"build\":2026091702}",
        );
        let l2 = l.clone();
        let (out, lines) = run(&l, 45, move |n| {
            match n {
                1 => live_progress(
                    &l2,
                    "{\"phase\":\"extract\",\"bytes_done\":0,\"bytes_total\":0}",
                ),
                2 => live_progress(&l2, "{\"phase\":\"link\"}"),
                _ => {
                    // The pass activates the new build; its guard drops a moment later.
                    install_claude(&l2, 2_026_091_702);
                }
            }
            true
        });
        assert_eq!(out, Outcome::Landed { waited: true });
        assert_eq!(
            lines[0],
            (
                "atpkg: waiting for the claude update to land — 2.1.274 (build 2026091702), \
                 downloading 42% (12.3 of 29.0 MB) — Ctrl-C runs 2.1.273 now"
                    .to_string(),
                true
            )
        );
        assert_eq!(
            lines[1].0,
            "atpkg: waiting for the claude update to land — 2.1.274 (build 2026091702), \
             extracting — Ctrl-C runs 2.1.273 now"
        );
        assert_eq!(
            lines[2].0,
            "atpkg: waiting for the claude update to land — 2.1.274 (build 2026091702), \
             activating — Ctrl-C runs 2.1.273 now"
        );
        assert_eq!(
            lines.last().unwrap(),
            &(
                "atpkg: the claude update landed — running 2.1.274 (build 2026091702)".to_string(),
                false
            )
        );
        assert!(lines.iter().all(|(s, _)| s.starts_with("atpkg: ")));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE LIE THE FIRST CUT TOLD (review, 2026-09-16): the marker goes while `bin/`
    /// still runs the old build — a failed download, a refused digest, a rolled-back
    /// activation; the guard's drop removes the marker on every one of them, and the
    /// pass records its `failed` row only after the install returned. That is NOT a
    /// landing: the line says the update did not land, names the reason when the row
    /// carries one, and never announces the new version.
    #[test]
    fn a_marker_that_goes_without_the_shim_moving_is_a_failed_landing_not_a_landed_one() {
        let l = temp_layout("notlanded");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(
            &l,
            "{\"phase\":\"download\",\"bytes_done\":0,\"bytes_total\":0}",
        );
        // No row ever names a reason (a hand-typed `aterm pkg update`: no progress file).
        let l2 = l.clone();
        let (out, lines) = run(&l, 45, move |_| {
            remove_marker(&l2, "claude");
            let _ = std::fs::remove_file(l2.progress_file());
            true
        });
        assert_eq!(out, Outcome::Failed);
        assert_eq!(
            lines.last().unwrap(),
            &(
                "atpkg: the claude update did not land — running 2.1.273 now; `aterm pkg \
                 update` retries it"
                    .to_string(),
                false
            )
        );
        assert!(
            lines.iter().all(|(s, _)| !s.contains("landed —")),
            "never the new version over the old build: {lines:?}"
        );
        // The pass records the reason a moment after the guard dropped: the grace catches
        // it, and the line names it.
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        let l3 = l.clone();
        let (out, lines) = run(&l, 45, move |n| {
            if n == 1 {
                remove_marker(&l3, "claude");
            } else {
                live_progress(
                    &l3,
                    "{\"phase\":\"failed\",\"build\":2026091702,\"error\":\"the download of \
                     claude-2026091702.tar.zst failed: HTTP 503\"}",
                );
            }
            true
        });
        assert_eq!(out, Outcome::Failed);
        assert_eq!(
            lines.last().unwrap().0,
            "atpkg: the claude update did not land (the download of claude-2026091702.tar.zst \
             failed: HTTP 503) — running 2.1.273 now; `aterm pkg update` retries it"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE SAME-TICK LANDING (review finding, 2026-09-16): the pass re-lays `bin/` AND
    /// drops the guard between two of the verb's looks. Read in the fixed order — marker
    /// first, shim second — a vanished marker over a shim that now runs the incoming
    /// build is the landing it is, with the landed line; never "did not land — running
    /// <old> now" over an exec of the NEW build. (The seam interleaves only between
    /// ticks; the order inside a tick is what closes the window the other way round left,
    /// and this pins the end state that order must report.)
    #[test]
    fn a_landing_seen_in_the_same_tick_as_the_marker_going_is_landed_not_failed() {
        let l = temp_layout("sametick");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(&l, "{\"phase\":\"link\"}");
        let l2 = l.clone();
        let (out, lines) = run(&l, 45, move |_| {
            install_claude(&l2, 2_026_091_702);
            remove_marker(&l2, "claude");
            let _ = std::fs::remove_file(l2.progress_file());
            true
        });
        assert_eq!(out, Outcome::Landed { waited: true });
        assert_eq!(
            lines.last().unwrap(),
            &(
                "atpkg: the claude update landed — running 2.1.274 (build 2026091702)".to_string(),
                false
            )
        );
        assert!(
            lines.iter().all(|(s, _)| !s.contains("did not land")),
            "never a failure over the new build: {lines:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LIVE PASS LAYS ITS OWN MARKER over ours mid-wait (a later tick's, a hand-typed
    /// update's — its pid, a newer build): the verb follows the file — the waiting line
    /// names the adopted build, LANDED is judged against it, and nothing of the newcomer's
    /// is deleted (review finding, 2026-09-16: a stale verdict on the marker first read
    /// used to remove whatever stood there).
    #[test]
    fn a_fresh_marker_laid_mid_wait_is_adopted_and_landed_on_its_own_build() {
        let l = temp_layout("adopt");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        let newer = Marker {
            build: 2_026_091_803,
            from_build: Some(2_026_091_601),
            pid: std::process::id(),
            version: Some("2.1.275".into()),
            from_version: Some("2.1.273".into()),
        };
        let l2 = l.clone();
        let n2 = newer.clone();
        let (out, lines) = run(&l, 45, move |n| {
            match n {
                1 => write_marker(&l2, "claude", &n2).unwrap(),
                // One tick after the adoption: the newcomer's marker stands untouched —
                // nothing of the first marker's judgement deleted it.
                2 => assert_eq!(read_marker(&l2, "claude"), Some(n2.clone())),
                _ => {
                    // The newcomer activates (its `install_shims` sweeps the landed marker).
                    install_claude(&l2, 2_026_091_803);
                }
            }
            true
        });
        assert_eq!(out, Outcome::Landed { waited: true });
        assert!(
            lines[0].0.contains("2.1.274 (build 2026091702)"),
            "the first marker's build first: {lines:?}"
        );
        assert!(
            lines[1].0.contains("2.1.275 (build 2026091803)"),
            "then the adopted one: {lines:?}"
        );
        assert_eq!(
            lines.last().unwrap().0,
            "atpkg: the claude update landed — running 2.1.275 (build 2026091803)"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The shim already resolving into the incoming build is the landing, marker or not
    /// (the guard drops a moment after activation) — no wait, no line.
    #[test]
    fn a_shim_already_on_the_incoming_build_is_landed_at_once() {
        let l = temp_layout("already");
        install_claude(&l, 2_026_091_702);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        let (out, lines) = run(&l, 45, |_| panic!("no sleep"));
        assert_eq!(out, Outcome::Landed { waited: false });
        assert!(lines.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn the_bound_expires_into_the_current_build() {
        let l = temp_layout("timeout");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(
            &l,
            "{\"phase\":\"download\",\"bytes_done\":0,\"bytes_total\":0}",
        );
        let (out, lines) = run(&l, 5, |_| true);
        assert_eq!(out, Outcome::TimedOut);
        // 5 s at a 2 s refresh: three waiting lines (2 + 2 + 1), then the verdict.
        assert_eq!(lines.iter().filter(|(_, r)| *r).count(), 3, "{lines:?}");
        assert_eq!(
            lines.last().unwrap().0,
            "atpkg: the claude update is still landing — running 2.1.273 now; the new build \
             runs once it lands"
        );
        assert!(
            lines[0].0.contains(", downloading — Ctrl-C"),
            "unmetered: no percent: {lines:?}"
        );
        assert!(
            read_marker(&l, "claude").is_some(),
            "the pass's marker is not ours to remove"
        );
        // No line promises what the next `claude` runs: it waits again while the marker
        // stands, and a pass that fails lands nothing (review, 2026-09-16).
        assert!(
            lines
                .iter()
                .all(|(s, _)| !s.contains("the next `claude` runs")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_zero_bound_warns_once_and_does_not_wait() {
        let l = temp_layout("nowait");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(&l, "{\"phase\":\"verify\"}");
        let (out, lines) = run(&l, 0, |_| panic!("no sleep"));
        assert_eq!(out, Outcome::NoWait);
        assert_eq!(
            lines,
            vec![(
                "atpkg: the claude update is landing — 2.1.274 (build 2026091702), verifying; \
                 running 2.1.273 now; the new build runs once it lands"
                    .to_string(),
                false
            )]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_sigint_stops_the_wait_and_runs_the_current_build() {
        let l = temp_layout("sigint");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        let (out, lines) = run(&l, 45, |n| n != 2);
        assert_eq!(out, Outcome::Interrupted);
        assert_eq!(
            lines.last().unwrap().0,
            "atpkg: stopped waiting — running claude 2.1.273 now; the new build runs once it \
             lands"
        );
        // With no progress file at all the line still says what it can.
        assert!(
            lines[0]
                .0
                .contains(", installing — Ctrl-C runs 2.1.273 now"),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A live pass whose row already says FAILED while the marker still stands (the
    /// window between `note_finished` and the guard's drop is the other way round in the
    /// flow, so this is the rare ordering — a client that records first): the same line,
    /// with the reason, no wait.
    #[test]
    fn a_failed_row_ends_the_wait_with_the_reason() {
        let l = temp_layout("failed");
        install_claude(&l, 2_026_091_601);
        write_marker(&l, "claude", &marker(std::process::id())).unwrap();
        live_progress(
            &l,
            "{\"phase\":\"failed\",\"error\":\"the download of claude-2026091702.tar.zst failed: HTTP 503\"}",
        );
        let (out, lines) = run(&l, 45, |_| panic!("no sleep"));
        assert_eq!(out, Outcome::Failed);
        assert_eq!(
            lines,
            vec![(
                "atpkg: the claude update did not land (the download of \
                 claude-2026091702.tar.zst failed: HTTP 503) — running 2.1.273 now; \
                 `aterm pkg update` retries it"
                    .to_string(),
                false
            )]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
