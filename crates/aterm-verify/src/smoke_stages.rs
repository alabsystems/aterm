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
//! TWO RULES THAT LOOK LIKE DETAILS AND ARE NOT:
//!  * BUILD BOTH BINARIES SYNCHRONOUSLY, THEN DRIVE THE BINARIES — never `targo
//!    run`. (1) Timing: the test stage links `aterm-gui`'s dev-deps with
//!    `spec-anchors` ON, which invalidates its non-test build, so a `run` here
//!    would rebuild and the bounded socket-poll budget would expire MID-BUILD,
//!    reporting a false "socket never appeared" on healthy code. (2) Output
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
    smoke_helpers_selftest, smoke_log_tail,
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
        // launch env is the only lever; empty (NOT a copy of the caller's) is the
        // right seed here, because the gate's verdict must be machine-independent.
        // It closes a write path too: `aterm-ctl` auto-presents the token and
        // owner scope satisfies `ConfigWrite`, which is how a probe rewrote the
        // owner's font on 2026-08-10.
        let cfgdir = tmp.join("cfg");
        std::fs::create_dir_all(cfgdir.join("aterm")).ok()?;
        chmod_700(&cfgdir)?;
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

fn chmod_700(path: &Path) -> Option<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).ok()
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
    let build = Cmd::new(&ctx.tools.targo)
        .args(smoke_build_args())
        .capture(Capture::Append(sb.gui_log.clone()));
    if !exec_run(&build, ctx.exec_env()).ok {
        r.fail(format!(
            "{tag}: targo build -p aterm-gui -p aterm-ctl failed"
        ));
        r.raw(smoke_log_tail(log_label, &sb.gui_log));
        return Ready::Stopped;
    }
    let target_dir = ctx.env.cargo_target_dir.as_deref();
    let gui = debug_bin(&ctx.root, target_dir, "aterm-gui");
    let ctl = debug_bin(&ctx.root, target_dir, "aterm-ctl");
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
    cmd.current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir)
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
    for _ in 0..SOCKET_POLLS {
        if child_exited(sb) {
            r.fail(format!("{tag}: aterm-gui exited early"));
            r.raw(smoke_log_tail(log_label, &sb.gui_log));
            return Ready::Stopped;
        }
        if is_socket_or_symlink(&sock) {
            return Ready::Up { ctl };
        }
        std::thread::sleep(POLL_GAP);
    }
    if is_socket_or_symlink(&sock) {
        return Ready::Up { ctl };
    }
    r.fail(format!("{tag}: control socket never appeared"));
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
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir);
    capture_reply(&cmd, ctx.exec_env())
}

fn ctl_quiet(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) {
    let cmd = Cmd::new(ctl_bin)
        .args(args.iter().copied())
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
    for _ in 0..BURST_KEYS {
        ctl_quiet(ctx, sb, ctl_bin, &["send", "x"]);
        std::thread::sleep(BURST_GAP);
    }
    std::thread::sleep(SETTLE);

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
        r.pass("smoke: typing burst pacing counters clean");
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
    if max_input_present_ms >= 250 {
        return Some(format!(
            "gui smoke: input→present latency — max {max_input_present_ms}ms (>= 250) [{reply}]"
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
