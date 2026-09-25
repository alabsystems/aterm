// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two smokes: the AI-first spine, and the pacing gate.
//!
//! 5) HEADLESS CONTROL-SOCKET SMOKE. Launch `aterm-gui --headless`
//!    (binds the socket, no window), then drive one round trip with `aterm-ctl`:
//!    `cursor` must answer `OK …`. Both binaries come from the just-built
//!    workspace; the whole thing is sandboxed under a throwaway `$XDG_RUNTIME_DIR`
//!    so it cannot touch a real instance, and it tears itself down on every exit
//!    path. Every gate run proves the socket still answers.
//!
//! 5b) GUI TYPING-PACING SMOKE (macOS desktop only). Headless never PRESENTS, so
//!    only a real window can measure pacing. The 2026-07-05 incident build
//!    presented at ~5/s with 190-530 ms input→present; a healthy build does 30+/s
//!    under 15 ms. Skips automatically without a WindowServer session (CI/SSH), or
//!    with `ATERM_SKIP_GUI_SMOKE=1`.
//!
//!    It drives TWO bursts. The controller burst (`ctl key`) is born already
//!    dequeued: it never arms the key-arrival stamp, so it cannot see OS event-queue
//!    residence, never books `key->write`, and a press-path stall that runs before
//!    its mid-handler `note_input()` is invisible to it. The hardware burst
//!    (`ctl hwkey`) posts real NSEvents into the app's own queue, so the keys take
//!    winit's `KeyboardInput` arm like a physical press. It is gated per slice from
//!    `metrics percentiles`, so a failure names who owes the time: aterm's
//!    `key->write`, the press's terminal-mutex wait, the drawable acquire, and the
//!    queue-inclusive input->present. The child's echo round trip is reported and
//!    never gated, because it is not aterm's cost.
//!
//! TWO RULES THAT LOOK LIKE DETAILS AND ARE NOT:
//!  * BUILD BOTH BINARIES SYNCHRONOUSLY, THEN DRIVE THE BINARIES — never `targo
//!    run`. (1) Timing: the test stage links `aterm-gui`'s dev-deps with
//!    `spec-anchors` ON, which invalidates its non-test build, so a `run` here
//!    would rebuild and the bounded socket-poll budget would expire MID-BUILD,
//!    reporting a false "socket never appeared" on healthy code. (Since
//!    2026-09-13 both binaries build into `target-drivers/`, which the test
//!    stage never writes, and the `driver builds` row has usually compiled
//!    them already — the build here is then a fingerprint no-op.) (2) Output
//!    purity: the driver writes lane diagnostics to stderr, and these round trips
//!    capture stderr to catch real errors — through `run` that banner lands in the
//!    reply and every `OK`-prefix match fails.
//!  * These stages are the run's only EXCLUSIVE ones (see `crate::sched`): they
//!    decide on frame counts and latencies, so they must own the machine.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::Ctx;
use crate::exec::{Capture, Cmd, capture_reply, run as exec_run};
use crate::glob::glob_match;
use crate::ladder::Report;
use crate::smoke::{
    debug_bin, is_socket_or_symlink, metric_ms_whole, metric_u64, retire_smoke_child,
    smoke_helpers_selftest, smoke_log_tail, socket_listening,
};

/// `targo --unverified build -q -p aterm-gui -p aterm-ctl`
#[must_use]
pub fn smoke_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "-p",
        "aterm-ctl",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The reply shapes the smokes decide on, as the shell globs they were.
pub mod pattern {
    pub const OK: &str = "OK *";
    pub const SYNC_CLEAN: &str = "OK *sync_rel_timeout=0 *";
    pub const NOT_SHEDDING: &str = "*perf_reduced=0 *";
    pub const NO_WAKE_HEALS: &str = "*wake_heals=0 *";
    pub const NO_RETRIES_OR_DROPS: &str = "*redraw_retry_gated=0 *present_drops=0 *";
    pub const SYNC_CLEAN_ANYWHERE: &str = "*sync_rel_timeout=0 *";
}

/// 30 driven keys at ~20/s, then a second to settle — a human-shaped light typing
/// burst, deliberately not a socket-flood test.
const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);
const SETTLE: Duration = Duration::from_secs(1);

/// The input->present ceiling both bursts share: ~10x the healthy margin and still
/// far under the 2026-07-05 incident's 300-530 ms worst case.
const INPUT_PRESENT_CEILING_MS: u64 = 250;
/// Hardware keys that must reach the `KeyboardInput` arm (`n_key_write`) before
/// any hardware slice verdict means anything: half the burst, the frames floor's
/// ratio. Below it the burst measured nothing, and that is a FAIL, not a pass.
const HW_KEY_WRITE_FLOOR: u64 = 15;
/// `key->write` p99 ceiling, OS queue residence included. The release build wrote
/// each key at p99 6.29 ms while a whole gate compiled beside it; 40 ms is more than
/// two 60 Hz frames, so a change that adds 40 ms of UI-thread work to every key
/// fails here whatever the healthy baseline, where the 250 ms input->present
/// ceiling let it through.
const KEY_WRITE_P99_CEILING_MS: u64 = 40;
/// Worst wait for the terminal mutex at the key-press site (presses AND releases,
/// which `key->write` does not sample). The P63 handoff gives a waiting press the
/// lock at the reader's next slice boundary, and the smoke's shell echoes one byte
/// per key; a 25 ms wait means some holder stopped honouring the handoff.
const TERM_WAIT_PRESS_MAX_CEILING_MS: u64 = 25;
/// Drawable-acquire p99 ceiling: three 60 Hz frames. Live windows read 3.4-5.24 ms
/// p99 (max 15.32 ms on a loaded machine). Gated only when acquires happened (a
/// CPU backend books none).
const ACQUIRE_P99_CEILING_MS: u64 = 50;
/// Present->glass p99 ceiling: the COMPOSITOR leg, `presentDrawable:` registration
/// -> the drawable's `presentedTime`. EVERY other ceiling on this list stops at
/// application present-return, so until this row a change that made the window
/// server hold frames -- re-enabling `displaySyncEnabled`, a deeper compositor
/// queue -- moved no gated number at all: the present call still returned at once
/// and `key->write`, the press lock, acquire and `input->present` all stayed green
/// while the owner waited longer for every keystroke to appear.
///
/// Three 60 Hz refreshes, the budget `ACQUIRE_P99_CEILING_MS` already spends. The
/// shipped macOS present is `Immediate` and WindowServer still composites at the
/// display refresh, so ONE refresh of wait is healthy here; this is therefore a bar
/// on a compositor holding frames for 3+ refreshes, not on a single added frame.
/// Pinning it tighter needs a measured per-refresh-rate baseline the idle-only
/// smoke cannot supply.
///
/// AND ON ONE CLASS OF HOST THE BAR IS ALREADY AT THE BASELINE, which the row's
/// author could not know without such a measurement. Measured 2026-09-18 by
/// reading `metrics percentiles` off the RUNNING app on a 2017 15-inch MacBook Pro
/// (macOS 13.7.8, Intel HD Graphics 630, 60 Hz panel) after two days of ordinary
/// use — 71,884 samples, not a smoke's 88:
///
/// ```text
/// present_glass_p50_ms=27.26  p95=37.75  p99=50.33  max=147.52
/// ```
///
/// So this machine's HEALTHY p99 is 50.33 ms: the three-refresh bar sits on top of
/// it, and the gate's own smoke read 50 ms (inside the bar, an exclusive bucket
/// edge) on one run and 54 ms (over it, max 51.19 ms) on the next. Nothing about
/// either run was wrong. What the number says is that a 27 ms MEDIAN — 1.6
/// refreshes after present-return — is what this iGPU does, so three refreshes is
/// not headroom here, and the row cannot separate a compositor regression from this
/// host's floor until the ceiling is decided against a measured baseline per
/// refresh rate and GPU class. That decision is the owner's: widening a latency bar
/// is a product statement, and this comment is the measurement it needs, not a
/// licence to move the constant.
///
/// Gated only when the leg was SAMPLED (`n_present_glass > 0`):
/// a CPU backend, a non-macOS present and a process that installs no sink register
/// no presented handler at all, and an absent slice is not a slow one.
const PRESENT_GLASS_P99_CEILING_MS: u64 = 50;
/// The socket-bind budget: 100 polls at 100 ms.
const SOCKET_POLLS: usize = 100;
const POLL_GAP: Duration = Duration::from_millis(100);

/// A running smoke's disposable state, torn down on every exit path.
struct Sandbox {
    tmp: PathBuf,
    rundir: PathBuf,
    cfgdir: PathBuf,
    gui_log: PathBuf,
    child: Option<Child>,
}

impl Sandbox {
    fn new(tag: &str) -> Option<Self> {
        let tmp = crate::mktemp_dir(tag).ok()?;
        std::fs::create_dir(tmp.join("home")).ok()?;
        // The per-user runtime dir the server and client both resolve to
        // ($XDG_RUNTIME_DIR/aterm); 0700 so the same-uid check holds.
        let rundir = tmp.join("run");
        std::fs::create_dir_all(rundir.join("aterm")).ok()?;
        chmod_700(&rundir)?;
        chmod_700(&rundir.join("aterm"))?;
        // The settings dir the engine resolves ($XDG_CONFIG_HOME/aterm/aterm.toml).
        // Same reason SHELL is forced to /bin/sh below — a gate must not read the
        // developer's machine — but sharper: this gate DECIDES ON FRAME COUNTS AND
        // LATENCIES, and the owner's live config sets `cursor_trail_style`, so
        // without this the pacing smoke measures whatever effect that machine
        // happens to have enabled. Config resolution has no probe marker, so the
        // launch env is the only lever. Keep rendering at its shipped defaults,
        // but explicitly opt out of unrelated update, package and host maintenance —
        // through the config, the way a person does: the environment vetoes are gone
        // (2026-09-23), so `[update] enabled = false` keeps the app updater's background
        // checks off (nothing here asks it to check).
        // `[packages].enabled = false` alone still runs `machine apply`, whose
        // defaults change per-user settings even with a scratch config directory.
        // It closes a write path too: `aterm-ctl` auto-presents the token and
        // owner scope satisfies `ConfigWrite`, which is how a probe rewrote the
        // owner's font on 2026-08-10.
        let cfgdir = tmp.join("cfg");
        std::fs::create_dir_all(cfgdir.join("aterm")).ok()?;
        chmod_700(&cfgdir)?;
        std::fs::write(
            cfgdir.join("aterm/aterm.toml"),
            "agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
             [packages]\nenabled = false\n\
             [machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n",
        )
        .ok()?;
        let gui_log = tmp.join("gui.log");
        Some(Self {
            tmp,
            rundir,
            cfgdir,
            gui_log,
            child: None,
        })
    }

    fn sock(&self) -> PathBuf {
        self.rundir.join("aterm/aterm.sock")
    }

    /// Reap the child and remove the sandbox. A teardown that could not retire
    /// exactly the process it launched is itself a FAIL: the smoke's conclusions
    /// are about that process.
    fn teardown(&mut self, r: &mut Report) {
        if let Some(mut child) = self.child.take() {
            let (ok, _) = retire_smoke_child(&mut child);
            if !ok {
                r.fail("smoke: child cleanup/reap failed");
            }
        }
        std::fs::remove_dir_all(&self.tmp).ok();
    }
}

#[cfg(unix)]
fn chmod_700(path: &Path) -> Option<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).ok()
}

/// No POSIX permission bits on Windows (privacy there is the per-user ACL a
/// `%LOCALAPPDATA%`-rooted directory already inherits), so this is a no-op that
/// reports success — the same contract `atpkg::platform::windows::set_mode` uses.
#[cfg(not(unix))]
fn chmod_700(_path: &Path) -> Option<()> {
    Some(())
}

/// Outcome of the shared "build, launch, wait for the socket" preamble.
enum Ready {
    /// Both binaries exist, the process is up and the socket is bound.
    Up { ctl: PathBuf },
    /// Already reported; the stage is over.
    Stopped,
}

/// The preamble both smokes share, with the label prefix each one uses.
fn bring_up(
    ctx: &Ctx,
    r: &mut Report,
    sb: &mut Sandbox,
    tag: &str,
    log_label: &str,
    headless: bool,
) -> Ready {
    let build = crate::stages::driver_build_cmd(ctx, smoke_build_args())
        .capture(Capture::Append(sb.gui_log.clone()));
    let built = exec_run(&build, ctx.exec_env());
    if !built.ok {
        r.fail_child(
            &built,
            format!("{tag}: targo build -p aterm-gui -p aterm-ctl failed"),
        );
        r.raw(smoke_log_tail(log_label, &sb.gui_log));
        return Ready::Stopped;
    }
    // The driver lane's dir, never the caller's `CARGO_TARGET_DIR`: that is where
    // the build above put them.
    let drivers = crate::stages::drivers_dir(ctx);
    let gui = debug_bin(&ctx.root, Some(drivers.as_os_str()), "aterm-gui");
    let ctl = debug_bin(&ctx.root, Some(drivers.as_os_str()), "aterm-ctl");
    if !crate::is_executable_file(&gui) || !crate::is_executable_file(&ctl) {
        r.cannot_run(format!(
            "{tag}: just-built binaries missing ({}, {})",
            gui.display(),
            ctl.display()
        ));
        return Ready::Stopped;
    }

    let log = match std::fs::File::create(&sb.gui_log) {
        Ok(f) => f,
        Err(e) => {
            r.cannot_run(format!("{tag}: cannot open child log ({e})"));
            return Ready::Stopped;
        }
    };
    let log2 = match log.try_clone() {
        Ok(f) => f,
        Err(e) => {
            r.cannot_run(format!("{tag}: cannot open child log ({e})"));
            return Ready::Stopped;
        }
    };
    // SHELL forced to a quiet, always-present /bin/sh so the engine's PTY child
    // cannot drag a developer's rc files into a gate.
    let mut cmd = Command::new(&gui);
    // A fixture has no inherited session, capability, handoff, fabric or
    // update authority. Apply its explicit private bindings only after this
    // removal, just as the shared Fabric harness does.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(name);
        }
    }
    cmd.current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .env("HOME", sb.tmp.join("home"))
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir)
        .env("ATERM_CONTROL_SOCK", sb.sock())
        .env("SHELL", "/bin/sh")
        .stdout(log)
        .stderr(log2);
    // Headless via the FLAG — the canonical arming ($ATERM_HEADLESS is an exact
    // equivalent, but a flag is visible in the spawn line and cannot be lost to
    // an env-inheritance rule between here and exec).
    if headless {
        cmd.arg("--headless");
    }
    match cmd.spawn() {
        Ok(c) => sb.child = Some(c),
        Err(e) => {
            r.cannot_run(format!("{tag}: cannot launch aterm-gui ({e})"));
            return Ready::Stopped;
        }
    }

    let sock = sb.sock();
    // Up means LISTENING, not bound: the file appears at bind(2), before
    // listen(2), and a first ctl call in that gap is refused.
    let up = |sock: &Path| is_socket_or_symlink(sock) && socket_listening(sock);
    for _ in 0..SOCKET_POLLS {
        if child_exited(sb) {
            r.fail(format!("{tag}: aterm-gui exited early"));
            r.raw(smoke_log_tail(log_label, &sb.gui_log));
            return Ready::Stopped;
        }
        if up(&sock) {
            return Ready::Up { ctl };
        }
        std::thread::sleep(POLL_GAP);
    }
    if up(&sock) {
        return Ready::Up { ctl };
    }
    r.fail(format!("{tag}: control socket never started listening"));
    r.raw(smoke_log_tail(log_label, &sb.gui_log));
    Ready::Stopped
}

fn child_exited(sb: &mut Sandbox) -> bool {
    sb.child
        .as_mut()
        .is_none_or(|c| matches!(c.try_wait(), Ok(Some(_)) | Err(_)))
}

/// One `aterm-ctl` round trip inside the sandbox, stdout and stderr merged the
/// way `$(… 2>&1)` merged them.
fn ctl(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) -> String {
    let cmd = Cmd::new(ctl_bin)
        .args(args.iter().copied())
        // Current ctl reads its token beside this exact socket; there is no
        // token/token-file/cap environment override. Defeat the independent
        // ambient disable selector too, without changing product auth tests.
        .env("ATERM_CONTROL_SOCK", sb.sock())
        .env("ATERM_NO_CONTROL_SOCK", "0")
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir);
    capture_reply(&cmd, ctx.exec_env())
}

fn ctl_quiet(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) {
    let cmd = Cmd::new(ctl_bin)
        .args(args.iter().copied())
        .env("ATERM_CONTROL_SOCK", sb.sock())
        .env("ATERM_NO_CONTROL_SOCK", "0")
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir)
        .capture(Capture::Silent);
    // `>/dev/null 2>&1` with the status ignored, exactly as the script drove the
    // burst: a dropped keystroke shows up in the pacing counters this stage
    // reads next, which is a better witness than the client's exit code.
    let _ = exec_run(&cmd, ctx.exec_env());
}

/// `${got:-<no reply>}`
fn or_no_reply(got: &str) -> &str {
    if got.is_empty() { "<no reply>" } else { got }
}

// ---------------------------------------------------------------------------
// 5) HEADLESS CONTROL-SOCKET SMOKE
// ---------------------------------------------------------------------------
pub fn control_socket_smoke(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        if smoke_helpers_selftest(&ctx.root) {
            r.pass("smoke helper invariants (short socket, target path, metrics, bounded reap)");
        } else {
            r.fail("smoke helper invariants");
        }
        r.skip("control-socket smoke (selftest)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("smoke (no targo)");
        return;
    }
    let Some(mut sb) = Sandbox::new("ats") else {
        r.cannot_run("smoke: mktemp");
        return;
    };
    if let Ready::Up { ctl: ctl_bin } =
        bring_up(ctx, r, &mut sb, "smoke", "control-socket smoke", true)
    {
        headless_round_trips(ctx, r, &sb, &ctl_bin);
    }
    sb.teardown(r);
}

fn headless_round_trips(ctx: &Ctx, r: &mut Report, sb: &Sandbox, ctl_bin: &Path) {
    let got = ctl(ctx, sb, ctl_bin, &["cursor"]);
    if glob_match(pattern::OK, &got) {
        r.pass(format!("smoke: aterm-ctl cursor -> {got}"));
    } else {
        r.fail(format!("smoke: aterm-ctl cursor -> {}", or_no_reply(&got)));
    }

    // Driven-typing pacing counters (the 2026-07-05 incident class). Headless
    // never PRESENTS, so frame-rate floors live in the GUI smoke; what headless
    // CAN honestly gate is that a plain typing burst arms no pathological state.
    ctl_quiet(ctx, sb, ctl_bin, &["metrics", "reset"]);
    // Let the reader/event-loop lane reach its ordinary idle wait before the
    // first byte, or the first startup-edge repair is miscounted as a
    // typing-burst lost-wake heal.
    std::thread::sleep(Duration::from_millis(250));
    // COUNT WHAT THE ENGINE ACCEPTED. The burst was driven through `ctl_quiet`,
    // which discards the client's status on the grounds that "a dropped
    // keystroke shows up in the pacing counters this stage reads next" — true of
    // a keystroke that ARRIVED, false of one that never did. Every counter
    // decided on below (`sync_rel_timeout=0`, `perf_reduced=0`, `wake_heals=0`)
    // is satisfied A FORTIORI by a burst that typed NOTHING, so a dead client, a
    // rejected verb or a wedged input seam printed `ok  smoke: typing burst
    // pacing counters clean` — a pass for a subject this stage never exercised.
    // `send` answers the bare `OK` on success (no fields, which is why
    // `pattern::OK`'s protocol space deliberately does not match it) and `ERR …`
    // otherwise, so an OK-prefixed reply is the evidence available at this seam.
    // At least one is now required, and the count rides on the ladder line so
    // the row states what it measured.
    let mut accepted = 0usize;
    for _ in 0..BURST_KEYS {
        if ctl(ctx, sb, ctl_bin, &["send", "x"]).starts_with("OK") {
            accepted += 1;
        }
        std::thread::sleep(BURST_GAP);
    }
    std::thread::sleep(SETTLE);
    if accepted == 0 {
        r.fail(format!(
            "smoke: none of the {BURST_KEYS} driven keys were accepted, so the pacing \
             counters below describe a burst that never happened"
        ));
        return;
    }

    let got = ctl(ctx, sb, ctl_bin, &["metrics"]);
    if !glob_match(pattern::SYNC_CLEAN, &got) {
        r.fail(format!(
            "smoke: pacing counters after typing burst -> {}",
            or_no_reply(&got)
        ));
        return;
    }
    if !glob_match(pattern::NOT_SHEDDING, &got) {
        r.fail(format!(
            "smoke: perf_reduced engaged during a light typing burst -> {got}"
        ));
        return;
    }
    if glob_match(pattern::NO_WAKE_HEALS, &got) {
        r.pass(format!(
            "smoke: typing burst pacing counters clean ({accepted}/{BURST_KEYS} keys accepted)"
        ));
    } else {
        r.fail(format!(
            "smoke: wake heals during a plain typing burst -> {got}"
        ));
    }
}

// ---------------------------------------------------------------------------
// 5b) GUI TYPING-PACING SMOKE
// ---------------------------------------------------------------------------
pub fn gui_typing_smoke(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("gui typing-pacing smoke (selftest)");
        return;
    }
    if let Some(reason) = gui_smoke_unavailable(ctx) {
        r.skip(reason);
        return;
    }
    let Some(mut sb) = Sandbox::new("atg") else {
        r.cannot_run("gui smoke: mktemp");
        return;
    };
    if let Ready::Up { ctl: ctl_bin } = bring_up(ctx, r, &mut sb, "gui smoke", "GUI smoke", false) {
        gui_measurements(ctx, r, &mut sb, &ctl_bin);
    }
    sb.teardown(r);
}

/// Every honest reason this stage cannot measure anything, in the script's order.
/// Each is a SKIP: the machine cannot present, which says nothing about the code.
fn gui_smoke_unavailable(ctx: &Ctx) -> Option<String> {
    if ctx.env.skip_gui_smoke.as_deref() == Some("1") {
        return Some("gui smoke (ATERM_SKIP_GUI_SMOKE)".into());
    }
    if std::env::consts::OS != "macos" {
        return Some("gui smoke (macOS only)".into());
    }
    // A WindowServer session is required to present; SSH/CI sessions have none.
    let has_hid = Command::new("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !has_hid
        || ctx
            .env
            .ssh_connection
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    {
        return Some("gui smoke (no WindowServer session)".into());
    }
    if !ctx.tools.have_targo() {
        return Some("gui smoke (no targo)".into());
    }
    if !Path::new("/usr/bin/swift").exists() {
        return Some(
            "gui smoke (/usr/bin/swift unavailable; cannot prove a frontmost drawable)".into(),
        );
    }
    None
}

fn gui_measurements(ctx: &Ctx, r: &mut Report, sb: &mut Sandbox, ctl_bin: &Path) {
    let Some(pid) = sb.child.as_ref().map(Child::id) else {
        r.cannot_run("gui smoke: no child to measure");
        return;
    };
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip("gui smoke (could not make the test window frontmost)");
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return;
    }

    // Prove one real present happened while frontmost BEFORE resetting counters:
    // this distinguishes a product present failure from a measurement window that
    // opened while backend initialization was still in flight.
    let mut got = String::new();
    let mut ready = false;
    for _ in 0..150 {
        if child_exited(sb) {
            r.fail("gui smoke: aterm-gui exited before its initial present");
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
        got = ctl(ctx, sb, ctl_bin, &["metrics"]);
        if metric_u64(&got, "frames").is_some_and(|f| f > 0) {
            ready = true;
            break;
        }
        std::thread::sleep(POLL_GAP);
    }
    if !ready {
        r.fail(format!(
            "gui smoke: frontmost window never produced an initial present [{}]",
            if got.is_empty() {
                "no metrics reply"
            } else {
                got.as_str()
            }
        ));
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return;
    }

    let got = ctl(ctx, sb, ctl_bin, &["metrics", "reset"]);
    if !glob_match(pattern::OK, &got) {
        r.fail(format!("gui smoke: metrics reset -> {}", or_no_reply(&got)));
        return;
    }

    for _ in 0..BURST_KEYS {
        // The real controller-input seam, not raw PTY `send`, paced over ~1.5 s
        // so the gate measures presentation rather than burst coalescing.
        let got = ctl(ctx, sb, ctl_bin, &["key", "x"]);
        if !glob_match(pattern::OK, &got) {
            r.fail(format!("gui smoke: key injection -> {}", or_no_reply(&got)));
            return;
        }
        std::thread::sleep(BURST_GAP);
    }
    std::thread::sleep(SETTLE);

    let got = ctl(ctx, sb, ctl_bin, &["metrics"]);
    let (Some(frames), Some(maxin)) = (
        metric_u64(&got, "frames"),
        metric_ms_whole(&got, "max_input_present_ms"),
    ) else {
        r.fail(format!(
            "gui smoke: could not parse metrics -> {}",
            or_no_reply(&got)
        ));
        return;
    };
    if let Some(bad) = pacing_verdict(frames, maxin, &got) {
        r.fail(bad);
        return;
    }
    if glob_match(pattern::SYNC_CLEAN_ANYWHERE, &got) {
        r.pass(format!(
            "gui smoke: frames={frames} max_input_present={maxin}ms, no sync timeouts"
        ));
    } else {
        r.fail(format!(
            "gui smoke: sync timeout-releases during plain typing [{got}]"
        ));
    }

    hardware_key_slices(ctx, r, sb, ctl_bin, pid);
}

/// `hwkey x count=30 interval=50`: the controller burst's keys and pacing, posted
/// as real NSEvents so they are dequeued, routed and translated by the code a
/// physical keypress runs, including the queue-age backdate `ctl key` never arms.
#[must_use]
pub fn hwkey_burst_args() -> Vec<String> {
    vec![
        "hwkey".into(),
        "x".into(),
        format!("count={BURST_KEYS}"),
        format!("interval={}", BURST_GAP.as_millis()),
    ]
}

/// The hardware-key burst and its per-slice verdict.
fn hardware_key_slices(ctx: &Ctx, r: &mut Report, sb: &Sandbox, ctl_bin: &Path, pid: u32) {
    // Posted keys go to the KEY window, so the test window must still be frontmost;
    // re-assert it rather than trust the activation from before the first burst.
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip("gui smoke: hardware keys (could not keep the test window frontmost)");
        return;
    }
    let got = ctl(ctx, sb, ctl_bin, &["metrics", "reset"]);
    if !glob_match(pattern::OK, &got) {
        r.fail(format!(
            "gui smoke: metrics reset before hardware keys -> {}",
            or_no_reply(&got)
        ));
        return;
    }
    let args = hwkey_burst_args();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    // Blocks while it paces the burst (~1.5 s); `OK posted=<n>` says only that the
    // OS queue took them. What arrived is read from the percentiles below.
    let got = ctl(ctx, sb, ctl_bin, &argv);
    let Some(posted) = metric_u64(&got, "posted").filter(|_| glob_match(pattern::OK, &got)) else {
        r.fail(format!(
            "gui smoke: hardware key injection -> {}",
            or_no_reply(&got)
        ));
        return;
    };
    std::thread::sleep(SETTLE);
    let got = ctl(ctx, sb, ctl_bin, &["metrics", "percentiles"]);
    match hardware_key_verdict(posted, &got) {
        Ok(line) => r.pass(line),
        Err(bad) => r.fail(bad),
    }
}

/// The text of `<name>=<value>` in a metrics reply (the LAST occurrence, as the
/// numeric helpers read it), for a verdict line; `?` when absent.
fn reply_field<'a>(reply: &'a str, name: &str) -> &'a str {
    let needle = format!(" {name}=");
    reply
        .rfind(needle.as_str())
        .and_then(|at| reply[at + needle.len()..].split_whitespace().next())
        .unwrap_or("?")
}

/// WHAT A REPORTED p99 IS, and why these four bars read `>` and not `>=`.
/// `aterm-gui`'s percentiles come from a histogram and report the containing
/// bucket's EXCLUSIVE upper edge (`metrics.rs`'s `Histogram::percentile`:
/// "every value in the bucket is strictly below it, so reporting it keeps
/// percentiles conservative"). So a reported `p99 = 50.00` against a 50 ms bar
/// says every sample was UNDER the bar, not at it — and `>=` failed such a run.
/// Measured 2026-09-18 on a 2017 Intel MacBook Pro, in the merge gate: the
/// compositor leg reported `p99 50ms` beside `max 47.85ms` — a percentile above
/// the observed maximum, which only a bucket edge can be — and the gate refused
/// a run whose worst frame was 2 ms inside budget. Where the same slice also
/// publishes a true maximum (acquire, present→glass), the max must confirm the
/// bar too, so a ceiling that does not fall exactly on a bucket edge cannot fail
/// a run on the edge above it either.
/// The hardware burst's `metrics percentiles` reply, slice by slice: `Ok` is the
/// pass line, `Err` the failure. Extracted so the thresholds are testable.
///
/// # Errors
/// A reply that cannot be parsed, a burst that never reached the `KeyboardInput`
/// arm, or any gated slice at or over its ceiling.
pub fn hardware_key_verdict(posted: u64, reply: &str) -> Result<String, String> {
    let whole = |name: &str| metric_ms_whole(reply, name);
    let (
        Some(n_key_write),
        Some(key_write),
        Some(press_max),
        Some(n_acquire),
        Some(acquire),
        Some(n_input),
        Some(input),
        Some(n_present_glass),
        Some(glass),
    ) = (
        metric_u64(reply, "n_key_write"),
        whole("key_write_p99_ms"),
        whole("max_term_wait_press_ms"),
        metric_u64(reply, "n_acquire"),
        whole("acquire_p99_ms"),
        metric_u64(reply, "n_input"),
        whole("input_p99_ms"),
        metric_u64(reply, "n_present_glass"),
        whole("present_glass_p99_ms"),
    )
    else {
        return Err(format!(
            "gui smoke: could not parse hardware-key percentiles -> {}",
            or_no_reply(reply)
        ));
    };
    let f = |name: &str| reply_field(reply, name);
    // A slice's TRUE maximum, as a number: the bucketed p99 above is an exclusive
    // bucket edge, so the max is what confirms a bar the edge only brushes. An
    // unparsable or absent field reads 0, which confirms nothing — the same
    // fail-open direction `n_* > 0` already takes for an unsampled slice.
    let max_ms = |name: &str| reply_field(reply, name).parse::<f64>().unwrap_or(0.0);
    if n_key_write < HW_KEY_WRITE_FLOOR {
        return Err(format!(
            "gui smoke: hardware keys never reached the KeyboardInput arm — \
             n_key_write={n_key_write} of posted={posted} (< {HW_KEY_WRITE_FLOOR}), so \
             key→write, the queue-age backdate and the press lock wait measured nothing \
             [{reply}]"
        ));
    }
    if key_write > KEY_WRITE_P99_CEILING_MS {
        return Err(format!(
            "gui smoke: hardware key→write — p99 {key_write}ms (> {KEY_WRITE_P99_CEILING_MS}), \
             aterm's own dispatch with OS queue residence; press lock wait max {}ms, \
             acquire p99 {}ms, child echo p99 {}ms [{reply}]",
            f("max_term_wait_press_ms"),
            f("acquire_p99_ms"),
            f("echo_p99_ms"),
        ));
    }
    if press_max >= TERM_WAIT_PRESS_MAX_CEILING_MS {
        return Err(format!(
            "gui smoke: key-press terminal-mutex wait — max {press_max}ms \
             (>= {TERM_WAIT_PRESS_MAX_CEILING_MS}) over n={} [{reply}]",
            f("n_term_wait_press"),
        ));
    }
    if n_acquire > 0
        && acquire > ACQUIRE_P99_CEILING_MS
        && max_ms("max_acquire_wait_ms")
            >= f64::from(u32::try_from(ACQUIRE_P99_CEILING_MS).unwrap_or(u32::MAX))
    {
        return Err(format!(
            "gui smoke: drawable acquire — p99 {acquire}ms (> {ACQUIRE_P99_CEILING_MS}, and its max confirms it) \
             over n={n_acquire}, max {}ms [{reply}]",
            f("max_acquire_wait_ms"),
        ));
    }
    if n_present_glass > 0
        && glass > PRESENT_GLASS_P99_CEILING_MS
        && max_ms("max_present_glass_ms")
            >= f64::from(u32::try_from(PRESENT_GLASS_P99_CEILING_MS).unwrap_or(u32::MAX))
    {
        return Err(format!(
            "gui smoke: present→glass — p99 {glass}ms \
             (> {PRESENT_GLASS_P99_CEILING_MS}, and its max confirms it) over n={n_present_glass}, the compositor \
             leg AFTER present-return that every slice above stops short of; max {}ms, \
             {} drawable(s) never shown [{reply}]",
            f("max_present_glass_ms"),
            f("present_glass_skipped"),
        ));
    }
    if n_input > 0 && input > INPUT_PRESENT_CEILING_MS {
        return Err(format!(
            "gui smoke: hardware input→present — p99 {input}ms (> {INPUT_PRESENT_CEILING_MS}), \
             OS queue residence included [{reply}]"
        ));
    }
    Ok(format!(
        "gui smoke: hardware keys n_key_write={n_key_write}/{posted} key_write_p99={}ms \
         max_term_wait_press={}ms acquire_p99={}ms (n={n_acquire}) input_p99={}ms \
         present_glass_p99={}ms (n={n_present_glass}, {} never shown); \
         echo_p99={}ms is the child's round trip (reported, not gated)",
        f("key_write_p99_ms"),
        f("max_term_wait_press_ms"),
        f("acquire_p99_ms"),
        f("input_p99_ms"),
        f("present_glass_p99_ms"),
        f("present_glass_skipped"),
        f("echo_p99_ms"),
    ))
}

/// The pacing thresholds, extracted so they are readable and testable.
///
/// 30 driven keys over ~2 s: a healthy build presents 30+ frames, the incident
/// build managed ~10. The input→present ceiling of 250 ms is ~10x the healthy
/// margin and still far under the incident's 300-530 ms worst case.
#[must_use]
pub fn pacing_verdict(frames: u64, max_input_present_ms: u64, reply: &str) -> Option<String> {
    if frames < 15 {
        return Some(format!(
            "gui smoke: present starvation — frames={frames} (< 15) [{reply}]"
        ));
    }
    if max_input_present_ms >= INPUT_PRESENT_CEILING_MS {
        return Some(format!(
            "gui smoke: input→present latency — max {max_input_present_ms}ms \
             (>= {INPUT_PRESENT_CEILING_MS}) [{reply}]"
        ));
    }
    if !glob_match(pattern::NO_RETRIES_OR_DROPS, reply) {
        return Some(format!(
            "gui smoke: present retries/drops during frontmost typing [{reply}]"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::cli::Mode;
    use crate::scope::Scope;

    fn ctx() -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            Mode::Fast,
            Scope::workspace(),
            false,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn the_smokes_build_both_binaries_quietly_and_drive_neither_through_the_driver() {
        assert_eq!(
            smoke_build_args(),
            [
                "--unverified",
                "build",
                "-q",
                "-p",
                "aterm-gui",
                "-p",
                "aterm-ctl"
            ]
        );
    }

    #[test]
    fn the_smoke_build_is_a_driver_lane_build() {
        let c = ctx();
        let cmd = crate::stages::driver_build_cmd(&c, smoke_build_args());
        let env: Vec<(String, String)> = cmd
            .envs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            env,
            [
                (
                    "CARGO_TARGET_DIR".to_string(),
                    "/repo/target-drivers".to_string()
                ),
                ("CARGO_BUILD_JOBS".to_string(), "8".to_string()),
            ]
        );
        assert_eq!(cmd.argv()[1..], smoke_build_args()[..]);
        assert_eq!(
            crate::stages::drivers_dir(&c),
            PathBuf::from("/repo/target-drivers")
        );
    }

    #[test]
    fn a_healthy_pacing_reply_passes_every_threshold() {
        let good = "OK frames=41 max_input_present_ms=8.900 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 ";
        assert_eq!(metric_u64(good, "frames"), Some(41));
        assert_eq!(metric_ms_whole(good, "max_input_present_ms"), Some(8));
        assert_eq!(pacing_verdict(41, 8, good), None);
        assert!(glob_match(pattern::SYNC_CLEAN_ANYWHERE, good));
    }

    #[test]
    fn the_incident_build_still_fails_every_threshold_it_failed() {
        // ~5 frames/s and 190-530 ms input→present: the build that shipped as a
        // daily driver before this stage existed.
        let incident = "OK frames=10 max_input_present_ms=530.100 redraw_retry_gated=4 present_drops=7 sync_rel_timeout=2 ";
        let starved = pacing_verdict(10, 530, incident).expect("a finding");
        assert!(
            starved.contains("present starvation — frames=10 (< 15)"),
            "{starved}"
        );

        let slow = pacing_verdict(40, 530, incident).expect("a finding");
        assert!(
            slow.contains("input→present latency — max 530ms (>= 250)"),
            "{slow}"
        );

        let dropping = pacing_verdict(40, 8, incident).expect("a finding");
        assert!(
            dropping.contains("present retries/drops during frontmost typing"),
            "{dropping}"
        );
    }

    #[test]
    fn the_thresholds_are_exactly_the_ported_boundaries() {
        let clean = "OK redraw_retry_gated=0 present_drops=0 ";
        assert!(
            pacing_verdict(14, 0, clean).is_some(),
            "14 frames is starvation"
        );
        assert!(
            pacing_verdict(15, 0, clean).is_none(),
            "15 frames is the floor, inclusive"
        );
        assert!(
            pacing_verdict(15, 249, clean).is_none(),
            "249 ms is under the ceiling"
        );
        assert!(
            pacing_verdict(15, 250, clean).is_some(),
            "250 ms is the ceiling, exclusive"
        );
    }

    /// A `metrics percentiles` reply in the verb's own field order, with the slices
    /// the hardware verdict reads set per case.
    fn percentiles(
        n_key_write: u64,
        key_write: &str,
        press_max: &str,
        acquire: &str,
        input: &str,
    ) -> String {
        format!(
            "OK n_input=30 input_p50_ms=9.11 input_p95_ms=12.30 input_p99_ms={input} \
             n_present=31 present_p50_ms=8.20 present_p95_ms=10.10 present_p99_ms=11.40 \
             n_key_write={n_key_write} key_write_p50_ms=1.10 key_write_p95_ms=2.30 \
             key_write_p99_ms={key_write} n_pre_present=31 pre_present_p50_ms=0.90 \
             n_acquire=31 acquire_p50_ms=0.02 acquire_p95_ms=1.10 acquire_p99_ms={acquire} \
             last_acquire_wait_ms=0.02 max_acquire_wait_ms=4.80 \
             n_term_wait_redraw_a=40 term_wait_redraw_a_p99_ms=0.02 max_term_wait_redraw_a_ms=0.10 \
             n_term_wait_press=60 term_wait_press_p50_ms=0.00 term_wait_press_p95_ms=0.01 \
             term_wait_press_p99_ms=0.02 max_term_wait_press_ms={press_max} \
             n_echo=30 echo_p50_ms=3.10 echo_p95_ms=40.20 echo_p99_ms=150.04 echo_max_ms=160.00 \
             n_present_glass=31 present_glass_p50_ms=6.10 present_glass_p95_ms=8.20 \
             present_glass_p99_ms=8.90 last_present_glass_ms=6.00 max_present_glass_ms=12.40 \
             present_glass_skipped=0\n"
        )
    }

    /// The same reply with one `max_*_ms` field replaced: the bucketed p99 above is
    /// an exclusive bucket edge, so a case that means "a sample really did cross the
    /// bar" has to move the MAX too, which is what the verdict now requires.
    fn with_max(reply: &str, field: &str, value: &str) -> String {
        let at = reply
            .find(&format!("{field}="))
            .expect("the field is in the reply");
        let from = at + field.len() + 1;
        let end = from + reply[from..].find(' ').expect("a field ends in a space");
        format!("{}{value}{}", &reply[..from], &reply[end..])
    }

    /// A healthy reply with the compositor leg's sample count and p99 replaced.
    fn with_glass(n: u64, p99: &str) -> String {
        percentiles(30, "6.00", "0.10", "1.00", "10.00")
            .replace("n_present_glass=31", &format!("n_present_glass={n}"))
            .replace(
                "present_glass_p99_ms=8.90",
                &format!("present_glass_p99_ms={p99}"),
            )
    }

    #[test]
    fn the_hardware_burst_is_the_controller_burst_through_the_os_queue() {
        assert_eq!(
            hwkey_burst_args(),
            ["hwkey", "x", "count=30", "interval=50"],
            "same keys, same count, same pacing as the `ctl key` burst"
        );
    }

    #[test]
    fn a_40ms_press_path_regression_passes_the_controller_gate_and_fails_the_hardware_gate() {
        // The finding's scenario: 40 ms of new UI-thread work on every keystroke.
        // Through `ctl key` the echo still presents in ~50 ms and the only latency
        // ceiling (250 ms input→present) passes it.
        let controller = "OK frames=41 max_input_present_ms=52.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 ";
        assert_eq!(pacing_verdict(41, 52, controller), None, "the blind spot");

        // The hardware burst books every key's key→write, and it fails.
        let hw = percentiles(30, "46.13", "0.10", "5.24", "58.00");
        let bad = hardware_key_verdict(30, &hw).expect_err("a finding");
        assert!(
            bad.contains("hardware key→write — p99 46ms (> 40)"),
            "{bad}"
        );
        assert!(
            bad.contains("press lock wait max 0.10ms, acquire p99 5.24ms, child echo p99 150.04ms"),
            "the failure names who owes the time: {bad}"
        );
    }

    #[test]
    fn a_healthy_hardware_burst_passes_and_the_childs_echo_is_never_gated() {
        // echo p99 150.04 ms is the gate-load measurement of a child waiting behind
        // compiles: reported, and not aterm's to fail on.
        let hw = percentiles(30, "6.29", "0.10", "5.24", "14.20");
        let ok = hardware_key_verdict(30, &hw).expect("a pass");
        assert_eq!(
            ok,
            "gui smoke: hardware keys n_key_write=30/30 key_write_p99=6.29ms \
             max_term_wait_press=0.10ms acquire_p99=5.24ms (n=31) input_p99=14.20ms \
             present_glass_p99=8.90ms (n=31, 0 never shown); \
             echo_p99=150.04ms is the child's round trip (reported, not gated)"
        );
    }

    #[test]
    fn a_burst_that_never_reached_the_keyboard_arm_is_a_failure_not_a_pass() {
        // Exactly what a `ctl key`-driven burst reports: every slice healthy, and
        // `n_key_write=0` because nothing armed the key-arrival stamp.
        let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
        let bad = hardware_key_verdict(30, &controller_shaped).expect_err("a finding");
        assert!(
            bad.contains("never reached the KeyboardInput arm — n_key_write=0 of posted=30 (< 15)"),
            "{bad}"
        );
        assert!(hardware_key_verdict(30, &percentiles(14, "1.00", "0.0", "1.0", "9.0")).is_err());
        assert!(hardware_key_verdict(30, &percentiles(15, "1.00", "0.0", "1.0", "9.0")).is_ok());
    }

    #[test]
    fn the_hardware_slice_ceilings_are_exact_and_separately_attributed() {
        let v = |kw: &str, press: &str, acq: &str, inp: &str| {
            hardware_key_verdict(30, &percentiles(30, kw, press, acq, inp))
        };
        assert!(
            v("39.99", "0.10", "1.00", "10.00").is_ok(),
            "39 ms key→write passes"
        );
        assert!(
            v("40.00", "0.10", "1.00", "10.00").is_ok(),
            "an exclusive edge AT the 40 ms bar means every sample was under it"
        );
        assert!(
            v("41.00", "0.10", "1.00", "10.00").is_err(),
            "41 ms key→write fails"
        );

        assert!(
            v("6.00", "24.90", "1.00", "10.00").is_ok(),
            "24 ms press wait passes"
        );
        let press = v("6.00", "25.00", "1.00", "10.00").expect_err("press wait");
        assert!(
            press.contains("key-press terminal-mutex wait — max 25ms (>= 25) over n=60"),
            "{press}"
        );

        assert!(
            v("6.00", "0.10", "49.90", "10.00").is_ok(),
            "49 ms acquire passes"
        );
        assert!(
            v("6.00", "0.10", "50.00", "10.00").is_ok(),
            "an exclusive edge AT the 50 ms bar is not a crossing"
        );
        // Above the bar AND confirmed by the max: 4.80 ms is the fixture's max, so the
        // case has to move it or it is the bucket-edge artifact, not a slow acquire.
        let acq = hardware_key_verdict(
            30,
            &with_max(
                &percentiles(30, "6.00", "0.10", "56.00", "10.00"),
                "max_acquire_wait_ms",
                "55.20",
            ),
        )
        .expect_err("acquire");
        assert!(
            acq.contains(
                "drawable acquire — p99 56ms (> 50, and its max confirms it) over n=31, max 55.20ms"
            ),
            "{acq}"
        );
        assert!(
            v("6.00", "0.10", "56.00", "10.00").is_ok(),
            "an acquire edge above the bar with a 4.80 ms max is the artifact, not a finding"
        );

        assert!(
            v("6.00", "0.10", "1.00", "249.90").is_ok(),
            "249 ms input→present passes"
        );
        assert!(
            v("6.00", "0.10", "1.00", "250.00").is_ok(),
            "an exclusive edge AT the 250 ms bar is not a crossing"
        );
        // This slice publishes no max of its own here (the pacing verdict gates
        // `max_input_present_ms` separately, and there `>=` is right because that IS a
        // true maximum), so the bar rests on the edge alone: one step above it fails.
        let inp = v("6.00", "0.10", "1.00", "260.00").expect_err("input→present");
        assert!(
            inp.contains("hardware input→present — p99 260ms (> 250)"),
            "{inp}"
        );

        // A CPU backend books no acquires; an absent slice is not a slow one.
        let cpu =
            percentiles(30, "6.00", "0.10", "0.00", "10.00").replace("n_acquire=31", "n_acquire=0");
        assert!(hardware_key_verdict(30, &cpu).is_ok());
    }

    #[test]
    fn a_compositor_that_holds_frames_fails_while_every_present_return_slice_stays_green() {
        // THE FINDING'S SCENARIO: a change adds a frame of compositor queue --
        // `displaySyncEnabled` re-enabled, a deeper queue. `presentDrawable:` still
        // RETURNS at once, so key->write, the press lock, acquire and input->present
        // are all healthy, and until the present->glass row EVERY published number
        // stayed green while the owner waited an extra frame for each keystroke.
        // 92 ms is four refreshes of queue, and the max confirms it — without that
        // the number could be a bucket edge brushed by a sample well under the bar.
        let held = with_max(&with_glass(31, "92.00"), "max_present_glass_ms", "91.50");
        assert!(
            hardware_key_verdict(30, &with_glass(31, "8.90")).is_ok(),
            "the same reply with a healthy compositor leg passes"
        );
        for (slice, healthy) in [
            ("key_write_p99_ms", 6u64),
            ("acquire_p99_ms", 1),
            ("input_p99_ms", 10),
        ] {
            assert_eq!(
                metric_ms_whole(&held, slice),
                Some(healthy),
                "{slice} is untouched by a compositor regression"
            );
        }
        let bad = hardware_key_verdict(30, &held).expect_err("a finding");
        assert!(
            bad.contains("present→glass — p99 92ms (> 50, and its max confirms it) over n=31"),
            "{bad}"
        );
        assert!(
            bad.contains("AFTER present-return") && bad.contains("0 drawable(s) never shown"),
            "the failure names the leg that owes the time: {bad}"
        );
    }

    #[test]
    fn the_compositor_leg_ceiling_is_exact_and_an_unsampled_leg_is_not_a_slow_one() {
        assert!(
            hardware_key_verdict(30, &with_glass(31, "49.90")).is_ok(),
            "49 ms present->glass passes"
        );
        // AN EDGE AT THE BAR IS NOT A CROSSING. The reported p99 is a bucket's
        // EXCLUSIVE upper edge, so 50.00 against a 50 ms bar says every sample was
        // under it — the run the 2026-09-18 gate refused reported exactly this beside
        // `max 47.85ms`, a percentile above its own maximum.
        assert!(
            hardware_key_verdict(30, &with_glass(31, "50.00")).is_ok(),
            "an exclusive edge AT the bar means every sample was under it"
        );
        // Above the bar, with the max confirming: the finding this row exists for.
        assert!(
            hardware_key_verdict(
                30,
                &with_max(&with_glass(31, "56.00"), "max_present_glass_ms", "55.10")
            )
            .is_err(),
            "a p99 above the bar whose max confirms it fails"
        );
        // Above the bar with a max that does not reach it: the bucket-edge artifact,
        // which is a pass. A 12.40 ms worst frame is not a compositor holding frames.
        assert!(
            hardware_key_verdict(30, &with_glass(31, "56.00")).is_ok(),
            "an edge above the bar that no sample reached is not a slow leg"
        );
        // No sink, no handler: a CPU backend and every non-macOS present book none,
        // and an absent slice must never be read as a slow one.
        assert!(
            hardware_key_verdict(30, &with_glass(0, "900.00")).is_ok(),
            "an unsampled compositor leg is not a slow one"
        );
    }

    #[test]
    fn an_unparsable_hardware_reply_fails_closed() {
        let bad = hardware_key_verdict(30, "").expect_err("no reply");
        assert_eq!(
            bad,
            "gui smoke: could not parse hardware-key percentiles -> <no reply>"
        );
        // The summary line (what `metrics` answers) lacks the slices: never a pass.
        let summary =
            "OK frames=41 max_input_present_ms=8.100 last_key_write_ms=0.00 max_key_write_ms=0.00 ";
        assert!(hardware_key_verdict(30, summary).is_err());
    }

    #[test]
    fn the_fields_the_hardware_verdict_reads_are_the_ones_aterm_gui_publishes() {
        // The verdict fails closed on a renamed field; this catches the rename at
        // test time instead of on the next gate run.
        let gui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-gui/src");
        let read = |f: &str| std::fs::read_to_string(gui.join(f)).expect("aterm-gui source");
        let query = read("control_query.rs");
        for spelled in [
            "n_input={} input_p50_ms={:.2} input_p95_ms={:.2} input_p99_ms={:.2}",
            "n_key_write={} key_write_p50_ms={:.2} key_write_p95_ms={:.2}",
            "key_write_p99_ms={:.2}",
            "n_acquire={} acquire_p50_ms={:.2} acquire_p95_ms={:.2} acquire_p99_ms={:.2}",
            "max_acquire_wait_ms={:.2}",
            " n_term_wait_{label}={}",
            "max_term_wait_{label}_ms={:.2}",
            "text_term_wait_fields(),",
            "crate::echo_rtt::percentile_fields_text(),",
        ] {
            assert!(
                query.contains(spelled),
                "control_query.rs no longer spells `{spelled}`"
            );
        }
        assert!(read("metrics.rs").contains("Self::Press => \"press\","));
        assert!(read("echo_rtt.rs").contains("echo_p99_ms={:.2}"));
        // The compositor leg: published by `metrics.rs`, appended to the reply by
        // `control_query.rs`. The verdict fails closed on a rename of either half.
        let metrics_rs = read("metrics.rs");
        for spelled in [
            "n_present_glass={} present_glass_p50_ms={:.2} present_glass_p95_ms={:.2}",
            "present_glass_p99_ms={:.2} last_present_glass_ms={:.2}",
            "max_present_glass_ms={:.2} present_glass_skipped={}",
        ] {
            assert!(
                metrics_rs.contains(spelled),
                "metrics.rs no longer spells `{spelled}`"
            );
        }
        assert!(query.contains("crate::metrics::present_glass_fields_text(),"));
        assert!(read("control.rs").contains("\"hwkey\" => control_input::cmd_hwkey(proxy, rest),"));
        let hwkey = read("hwkey.rs");
        assert!(
            hwkey.contains("strip_prefix(\"count=\")")
                && hwkey.contains("strip_prefix(\"interval=\")")
        );
        assert!(read("control_input.rs").contains("format!(\"OK posted={n}\\n\")"));
    }

    #[test]
    fn an_empty_reply_is_reported_as_no_reply_never_as_a_pass() {
        assert_eq!(or_no_reply(""), "<no reply>");
        assert_eq!(or_no_reply("OK x"), "OK x");
        assert!(!glob_match(pattern::OK, ""));
        assert!(!glob_match(pattern::SYNC_CLEAN, ""));
    }

    #[test]
    fn the_gui_smoke_skips_honestly_when_the_machine_cannot_present() {
        let mut c = ctx();
        c.env.skip_gui_smoke = Some("1".into());
        assert_eq!(
            gui_smoke_unavailable(&c).as_deref(),
            Some("gui smoke (ATERM_SKIP_GUI_SMOKE)")
        );

        // …and the opt-out is exact: any other value is not the opt-out.
        c.env.skip_gui_smoke = Some("0".into());
        assert_ne!(
            gui_smoke_unavailable(&c).as_deref(),
            Some("gui smoke (ATERM_SKIP_GUI_SMOKE)")
        );
    }

    #[test]
    fn an_ssh_session_cannot_present_and_says_so() {
        let mut c = ctx();
        c.env.ssh_connection = Some("10.0.0.1 22 10.0.0.2 22".into());
        if std::env::consts::OS == "macos" {
            assert_eq!(
                gui_smoke_unavailable(&c).as_deref(),
                Some("gui smoke (no WindowServer session)")
            );
        } else {
            assert_eq!(
                gui_smoke_unavailable(&c).as_deref(),
                Some("gui smoke (macOS only)")
            );
        }
    }

    #[test]
    fn the_sandbox_is_private_and_short_enough_for_a_unix_socket() {
        let mut sb = Sandbox::new("ats").expect("sandbox");
        // THE MODE ASSERTION IS THE ONLY UNIX PART, so it is the only part
        // gated: the socket-name length, the layout and the teardown below are
        // the same law everywhere, and gating the whole test would have made
        // them unpinned off unix rather than merely unmeasured.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&sb.rundir)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o700,
                "the same-uid control-socket check depends on 0700"
            );
        }
        assert!(sb.sock().as_os_str().len() < crate::smoke::SUN_LEN);
        assert!(sb.rundir.join("aterm").is_dir());

        let mut r = Report::new("t");
        sb.teardown(&mut r);
        assert!(!sb.tmp.exists(), "the sandbox removes itself");
        assert_eq!(r.outcomes().count(), 0, "a clean teardown says nothing");
    }

    #[test]
    fn a_teardown_that_loses_its_child_is_a_failure() {
        let mut sb = Sandbox::new("ats").expect("sandbox");
        // A child that exits on its own is not one this smoke retired, and the
        // measurements were about THAT process.
        sb.child = Some(
            Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn"),
        );
        std::thread::sleep(Duration::from_millis(50));
        let mut r = Report::new("t");
        sb.teardown(&mut r);
        assert_eq!(
            r.outcomes().collect::<Vec<_>>(),
            [(
                crate::Outcome::Fail(crate::Severity::GateFailed),
                "smoke: child cleanup/reap failed"
            )]
        );
    }

    #[test]
    fn selftest_runs_the_harness_invariants_and_nothing_else() {
        let mut c = ctx();
        c.selftest = true;
        c.root = std::env::current_dir().expect("cwd");
        let mut r = Report::new("control-socket smoke");
        control_socket_smoke(&c, &mut r);
        let got: Vec<_> = r.outcomes().map(|(o, l)| (o, l.to_string())).collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, crate::Outcome::Ok);
        assert_eq!(
            got[0].1,
            "smoke helper invariants (short socket, target path, metrics, bounded reap)"
        );
        assert_eq!(
            got[1],
            (
                crate::Outcome::Skip,
                "control-socket smoke (selftest)".to_string()
            )
        );

        let mut r = Report::new("gui typing-pacing smoke");
        gui_typing_smoke(&c, &mut r);
        assert_eq!(
            r.outcomes().collect::<Vec<_>>(),
            [(crate::Outcome::Skip, "gui typing-pacing smoke (selftest)")]
        );
    }
}
