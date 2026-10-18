// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GUI typing-pacing smoke: the `measure` check's live window. The
//! headless control-socket smoke is a live test
//! (`crates/aterm/tests/control_socket_smoke_live_headless.rs`), in every
//! `test` run.
//!
//! macOS desktop only, run with `--app <APP>`: a release signs only bytes it
//! measured green. It drives the release bundle the run was handed — APP's
//! `aterm-gui` and `aterm-ctl`, argv0 names of the one `aterm` — and builds
//! nothing, so the latencies it judges are the shipped bytes'. Headless never
//! PRESENTS, so only a real window can measure pacing: a lost-wake build
//! presents at ~5/s with 190-530 ms input→present, a healthy build 30+/s under
//! 15 ms. Skips without a WindowServer session (SSH), or under `--no-windows`
//! (`plan::WINDOWED`, decided before this stage runs).
//!
//! ONE HARDWARE BURST (`ctl hwkey`) that posts real NSEvents into the app's own
//! queue, so the keys take winit's `KeyboardInput` arm like a physical press —
//! OS queue residence included, which the controller seam (`ctl key`, born
//! already dequeued) never saw. One verdict reads both replies after it
//! ([`hardware_key_verdict`]): the `metrics` summary's frames floor, true
//! input→present maximum, present retries and drops and sync timeout-releases,
//! and `metrics percentiles`' per-slice ceilings, so a failure names who owes
//! the time: aterm's `key->write`, the press's terminal-mutex wait, the drawable
//! acquire, the compositor leg and the queue-inclusive input->present. The
//! child's echo round trip is reported and never gated, because it is not
//! aterm's cost.
//!
//! The burst runs with the cursor effects ON in the shipped default config
//! ([`SMOKE_CONFIG`]; docs/INCIDENT-typing-latency-2026-08-26.md item 4's
//! late-deadline half): the effects must show engaged (it armed
//! `cursor_effect` deadlines and composed the ribbon, `fx_under_frames`), so
//! the burst cannot pass having measured nothing, and it fails when the
//! `cursor_effect` owner books late (past) arms faster than
//! `CURSOR_EFFECT_LATE_MAX` allows ([`effects_verdict`]).
//!
//! DRIVE THE BINARIES, never `targo run`: the driver writes lane diagnostics
//! to stderr, and these round trips capture stderr to catch real errors —
//! through `run` that banner lands in the reply and every `OK`-prefix match
//! fails. The stage is EXCLUSIVE (see `crate::sched`), as are the measuring
//! tests: it decides on frame counts and latencies, so it must own the machine.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::Ctx;
use crate::exec::{Cmd, capture_reply};
use crate::ladder::Report;
use crate::smoke::{
    is_socket_or_symlink, metric_ms_whole, metric_u64, retire_smoke_child, smoke_log_tail,
    socket_listening,
};

/// Does a reply say `OK` and a field (`OK <…>`)? Every counter is then read by
/// name ([`metric_u64`]), so an absent one fails.
fn ok_reply(reply: &str) -> bool {
    reply.starts_with("OK ")
}

/// The `aterm.toml` lines the smoke's instance runs with (top-level keys, ahead
/// of the sandbox's first `[table]`): the cursor effects and their trail
/// sounds on, the shipped default config, written out so the burst measures
/// the effects whatever the defaults become.
pub const SMOKE_CONFIG: &str = "cursor_trail = true\ntrail_sounds = true\n";

/// Late (`past`) `cursor_effect` arms the burst may book: healthy effects-on
/// bursts arm ~430 deadlines and book 0-2 late, so four leaves two of headroom.
/// It catches a SUSTAINED regression; a 0.8 % late rate is about 3 a burst,
/// which one burst cannot tell from healthy.
const CURSOR_EFFECT_LATE_MAX: u64 = 4;
/// …and as a share of the owner's arms, so a burst that armed little cannot pass
/// on the absolute bound alone: more than one late arm in fifty (2 %) is a
/// regression once more than one arm was late.
const CURSOR_EFFECT_LATE_SHARE_DEN: u64 = 50;

/// The `cursor_effect` owner's `arms/past` out of a `metrics` reply's
/// `deadline_arms_by_owner=owner:arms/past,...` ledger. An owner absent from the
/// ledger armed nothing: `(0, 0)`. `None` when the ledger itself is missing.
#[must_use]
pub fn cursor_effect_arms(reply: &str) -> Option<(u64, u64)> {
    let ledger = reply
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("deadline_arms_by_owner="))?;
    for entry in ledger.split(',') {
        let Some((owner, counts)) = entry.split_once(':') else {
            continue;
        };
        if owner != "cursor_effect" {
            continue;
        }
        let (arms, past) = counts.split_once('/')?;
        return Some((arms.parse().ok()?, past.parse().ok()?));
    }
    Some((0, 0))
}

/// The burst's verdict on its post-burst `metrics` summary: the engagement
/// witnesses (it armed `cursor_effect` deadlines and composed the ribbon,
/// `fx_under_frames > 0`), then its late `cursor_effect` arms against
/// `CURSOR_EFFECT_LATE_MAX` and the one-in-fifty share.
///
/// # Errors
/// A summary missing either reading, effects that never engaged, or too many
/// late arms.
pub fn effects_verdict(reply: &str) -> Result<String, String> {
    let Some((arms, late)) = cursor_effect_arms(reply) else {
        return Err(format!(
            "gui smoke: metrics carry no deadline_arms_by_owner ledger -> {}",
            or_no_reply(reply)
        ));
    };
    let Some(under) = metric_u64(reply, "fx_under_frames") else {
        return Err(format!(
            "gui smoke: metrics carry no fx_under_frames -> {}",
            or_no_reply(reply)
        ));
    };
    if arms == 0 || under == 0 {
        return Err(format!(
            "gui smoke: the cursor effects never engaged — cursor_effect arms={arms}, \
             fx_under_frames={under} — so the burst measured an effects-off instance"
        ));
    }
    let over_share = late.saturating_mul(CURSOR_EFFECT_LATE_SHARE_DEN) > arms && late > 1;
    if late > CURSOR_EFFECT_LATE_MAX || over_share {
        return Err(format!(
            "gui smoke: cursor_effect booked {late} late arms of {arms} during a paced \
             burst (max {CURSOR_EFFECT_LATE_MAX}, and at most 1 in \
             {CURSOR_EFFECT_LATE_SHARE_DEN}) — the effects are arming deadlines they \
             already missed"
        ));
    }
    Ok(format!(
        "gui smoke: cursor_effect arms={arms} late={late} fx_under_frames={under}"
    ))
}

/// 30 hardware keys at ~20/s, then a second to settle — a human-shaped light
/// typing burst, deliberately not a socket-flood test.
const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);
const SETTLE: Duration = Duration::from_secs(1);

/// The input->present ceiling, on the true maximum (`>=` fails) and on the
/// bucketed p99 (`>` fails): far over a healthy burst's maximum (under 10 ms,
/// OS queue residence included, effects on or off) and far under a lost-wake
/// build's 300-530 ms worst case.
const INPUT_PRESENT_CEILING_MS: u64 = 250;
/// Frames the window must present over the burst: a healthy build presents one
/// or more per key (30+ even with the effects off), a lost-wake build ~10.
/// Half the quietest healthy burst.
const FRAMES_FLOOR: u64 = 15;
/// Hardware keys that must reach the `KeyboardInput` arm (`n_key_write`) before
/// any hardware slice verdict means anything: half the burst, the frames floor's
/// ratio. Below it the burst measured nothing, and that is a FAIL, not a pass.
const HW_KEY_WRITE_FLOOR: u64 = 15;
/// Input→present samples the burst must book (`n_input`, one per presented key)
/// before the input→present ceilings mean anything, at the same ratio: a burst
/// that booked none would pass both on a maximum of 0 ms. A healthy burst books
/// all 30.
const HW_INPUT_FLOOR: u64 = 15;
/// `key->write` p99 ceiling, OS queue residence included. The release build wrote
/// each key at p99 6.29 ms while a whole run compiled beside it; 40 ms is more than
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
/// ON AN INTEL iGPU AT 60 Hz THE BAR IS THE BASELINE: a running app there reads
/// a healthy p99 of ~50 ms (median 27 ms, 1.6 refreshes after present-return),
/// so the row cannot separate a compositor regression from that host's floor
/// until the ceiling is decided per refresh rate and GPU class. Widening a
/// latency bar is the owner's product statement, not a constant to move here.
///
/// Gated only when the leg was SAMPLED (`n_present_glass > 0`):
/// a CPU backend, a non-macOS present and a process that installs no sink register
/// no presented handler at all, and an absent slice is not a slow one.
const PRESENT_GLASS_P99_CEILING_MS: u64 = 50;
/// The socket-listen budget: how many times the smoke looks at its instance's
/// control socket, [`POLL_GAP`] apart, before `control socket never started
/// listening` ([`wait_to_listen`]) — a minute of sleeps, plus what each look (a
/// `try_wait`, a stat and a connect) costs on the machine of the day. A hang
/// detector, never a latency budget: an instance that listens ends the wait at
/// that look, and a bundle's first exec on a loaded machine is slow, which is
/// no failure to listen.
///
/// COUNTED IN LOOKS, NEVER TIMED BY THE CLOCK: a clock-timed wait can only be
/// shorter than the looks, and turns a slow first exec red.
pub const SOCKET_POLLS: usize = 600;
/// The pause between two looks of the smoke's waits.
pub const POLL_GAP: Duration = Duration::from_millis(100);

/// A running smoke's disposable state, torn down on every exit path.
struct Sandbox {
    tmp: PathBuf,
    rundir: PathBuf,
    cfgdir: PathBuf,
    gui_log: PathBuf,
    child: Option<Child>,
}

impl Sandbox {
    /// A fresh sandbox whose `aterm.toml` starts with `extra` lines, ahead of the
    /// fixed tables (top-level keys must precede the first `[table]`).
    fn with_config(tag: &str, extra: &str) -> Option<Self> {
        let tmp = crate::mktemp_dir(tag).ok()?;
        std::fs::create_dir(tmp.join("home")).ok()?;
        // The per-user runtime dir the server and client both resolve to
        // ($XDG_RUNTIME_DIR/aterm); 0700 so the same-uid check holds.
        let rundir = tmp.join("run");
        std::fs::create_dir_all(rundir.join("aterm")).ok()?;
        chmod_700(&rundir)?;
        chmod_700(&rundir.join("aterm"))?;
        // The settings dir the engine resolves ($XDG_CONFIG_HOME/aterm/aterm.toml).
        // Same reason SHELL is forced to /bin/sh below — a run must not read the
        // developer's machine — but sharper: this run DECIDES ON FRAME COUNTS AND
        // LATENCIES, and the owner's live config sets `cursor_trail_style`, so
        // without this the pacing smoke measures whatever effect that machine
        // happens to have enabled. Config resolution has no probe marker, so the
        // launch env is the only lever. Keep rendering at its shipped defaults,
        // but explicitly opt out of unrelated update, package and host maintenance —
        // through the config, the way a person does: `[update] enabled = false` keeps
        // the app updater's background checks off (nothing here asks it to check).
        // `[packages].enabled = false` alone still runs `machine apply`, whose
        // defaults change per-user settings even with a scratch config directory.
        // It closes a write path too: `aterm-ctl` auto-presents the token and
        // owner scope satisfies `ConfigWrite`, so a probe could rewrite the
        // owner's real config.
        let cfgdir = tmp.join("cfg");
        std::fs::create_dir_all(cfgdir.join("aterm")).ok()?;
        chmod_700(&cfgdir)?;
        std::fs::write(
            cfgdir.join("aterm/aterm.toml"),
            format!(
                "{extra}agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
                 [packages]\nenabled = false\n\
                 [machine]\nuniversal_control = \"leave\"\n"
            ),
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
                // After every check: it hides none.
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

/// Outcome of the "launch, wait for the socket" preamble.
enum Ready {
    /// Both binaries exist, the process is up and the socket is bound.
    Up { ctl: PathBuf },
    /// Already reported; the stage is over.
    Stopped,
}

/// Launch the handed bundle's `aterm-gui` in `sb` and wait for its control
/// socket; the bundle's `aterm-ctl` drives it. It builds nothing.
fn bring_up(ctx: &Ctx, r: &mut Report, sb: &mut Sandbox, bundle: &crate::MeasureArtifact) -> Ready {
    let tag = "gui smoke";
    let (gui, ctl) = (bundle.bin("aterm-gui"), bundle.bin("aterm-ctl"));
    if !crate::is_executable_file(&gui) || !crate::is_executable_file(&ctl) {
        r.cannot_run(format!(
            "{tag}: the bundle's binaries are missing ({}, {})",
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
    // cannot drag a developer's rc files into a run.
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
        .env("SHELL", "/bin/sh")
        .stdout(log)
        .stderr(log2);
    // The socket is a launch FLAG — the one spelling; no environment variable
    // selects it.
    cmd.arg("--control-sock").arg(sb.sock());
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
    let up = || is_socket_or_symlink(&sock) && socket_listening(&sock);
    match wait_to_listen(POLL_GAP, || child_exited(sb), up) {
        Listen::Up => return Ready::Up { ctl },
        Listen::Exited => r.fail(format!("{tag}: aterm-gui exited early")),
        Listen::Never => r.fail(format!("{tag}: control socket never started listening")),
    }
    r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
    Ready::Stopped
}

/// What the smoke's listen wait found ([`wait_to_listen`]).
#[derive(Debug, PartialEq, Eq)]
enum Listen {
    /// The instance listens.
    Up,
    /// It exited first.
    Exited,
    /// It never listened within the wait.
    Never,
}

/// THE LISTEN WAIT: [`SOCKET_POLLS`] looks at `exited` and then `up`, `gap`
/// apart ([`POLL_GAP`] in a real run), and one last look at `up` after them.
/// However long a look takes, every one is taken: the wait is never shorter
/// than its sleeps.
fn wait_to_listen(
    gap: Duration,
    mut exited: impl FnMut() -> bool,
    mut up: impl FnMut() -> bool,
) -> Listen {
    for _ in 0..SOCKET_POLLS {
        if exited() {
            return Listen::Exited;
        }
        if up() {
            return Listen::Up;
        }
        std::thread::sleep(gap);
    }
    if up() { Listen::Up } else { Listen::Never }
}

fn child_exited(sb: &mut Sandbox) -> bool {
    sb.child
        .as_mut()
        .is_none_or(|c| matches!(c.try_wait(), Ok(Some(_)) | Err(_)))
}

/// One `aterm-ctl` round trip inside the sandbox, stdout and stderr merged the
/// way `$(… 2>&1)` merged them.
fn ctl(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) -> String {
    // `--sock` pins this exact socket; ctl reads its token beside it (there is
    // no token/token-file/cap environment override).
    let cmd = Cmd::new(ctl_bin)
        .arg("--sock")
        .arg(sb.sock())
        .args(args.iter().copied())
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir);
    capture_reply(&cmd, ctx.exec_env())
}

/// `${got:-<no reply>}`
fn or_no_reply(got: &str) -> &str {
    if got.is_empty() { "<no reply>" } else { got }
}

// ---------------------------------------------------------------------------
// GUI TYPING-PACING SMOKE
// ---------------------------------------------------------------------------
pub fn gui_typing_smoke(ctx: &Ctx, r: &mut Report) {
    if let Some(reason) = gui_smoke_unavailable(ctx) {
        r.skip(reason);
        return;
    }
    // Never a binary of the run's own choosing: the bundle publication signs.
    let Some(bundle) = &ctx.measure_artifact else {
        r.skip(format!("gui smoke {}", crate::stages::NO_APP));
        return;
    };
    let Some(mut sb) = Sandbox::with_config("atl", SMOKE_CONFIG) else {
        r.cannot_run("gui smoke: mktemp");
        return;
    };
    if let Ready::Up { ctl: ctl_bin } = bring_up(ctx, r, &mut sb, bundle) {
        typing_burst(ctx, r, &mut sb, &ctl_bin);
    }
    sb.teardown(r);
}

/// Frontmost, first present, reset, the paced HARDWARE burst, settle; then the
/// effects verdict on the `metrics` summary ([`effects_verdict`]), and
/// [`hardware_key_verdict`] on the summary and `metrics percentiles` together.
fn typing_burst(ctx: &Ctx, r: &mut Report, sb: &mut Sandbox, ctl_bin: &Path) {
    let Some(pid) = sb.child.as_ref().map(Child::id) else {
        r.cannot_run("gui smoke: no child to measure");
        return;
    };
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip(if crate::smoke::screen_locked() {
            "gui smoke (the screen is locked: unlock it and run again)"
        } else {
            "gui smoke (could not make the test window frontmost)"
        });
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return;
    }
    // Prove one real present happened while frontmost BEFORE resetting counters:
    // this distinguishes a product present failure from a measurement window that
    // opened while backend initialization was still in flight.
    let mut got = String::new();
    let mut presented = false;
    for _ in 0..150 {
        if child_exited(sb) {
            r.fail("gui smoke: aterm-gui exited before its initial present");
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
        got = ctl(ctx, sb, ctl_bin, &["metrics"]);
        if metric_u64(&got, "frames").is_some_and(|f| f > 0) {
            presented = true;
            break;
        }
        std::thread::sleep(POLL_GAP);
    }
    if !presented {
        r.fail(format!(
            "gui smoke: the frontmost window never produced an initial present [{}]",
            or_no_reply(&got)
        ));
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return;
    }
    // Posted keys go to the KEY window: re-assert frontmost after the wait.
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip("gui smoke (could not keep the test window frontmost)");
        return;
    }
    let got = ctl(ctx, sb, ctl_bin, &["metrics", "reset"]);
    if !ok_reply(&got) {
        r.fail(format!("gui smoke: metrics reset -> {}", or_no_reply(&got)));
        return;
    }
    let args = hwkey_burst_args();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    // Blocks while it paces the burst (~1.5 s); `OK posted=<n>` says only that the
    // OS queue took them. What arrived is read from the replies below.
    let got = ctl(ctx, sb, ctl_bin, &argv);
    let Some(posted) = metric_u64(&got, "posted").filter(|_| ok_reply(&got)) else {
        r.fail(format!(
            "gui smoke: hardware key injection -> {}",
            or_no_reply(&got)
        ));
        return;
    };
    std::thread::sleep(SETTLE);
    let summary = ctl(ctx, sb, ctl_bin, &["metrics"]);
    let percentiles = ctl(ctx, sb, ctl_bin, &["metrics", "percentiles"]);
    match effects_verdict(&summary) {
        Ok(line) => r.pass(line),
        Err(bad) => r.fail(bad),
    }
    match hardware_key_verdict(posted, &summary, &percentiles) {
        Ok(line) => r.pass(line),
        Err(bad) => r.fail(bad),
    }
}

/// Every honest reason this stage cannot measure anything, in the script's order.
/// Each is a SKIP: the machine cannot present, which says nothing about the code.
fn gui_smoke_unavailable(ctx: &Ctx) -> Option<String> {
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
    if !Path::new("/usr/bin/swift").exists() {
        return Some(
            "gui smoke (/usr/bin/swift unavailable; cannot prove a frontmost drawable)".into(),
        );
    }
    None
}

/// `hwkey x count=30 interval=50`: 30 keys at ~20/s, posted as real NSEvents so
/// they are dequeued, routed and translated by the code a physical keypress runs,
/// including the queue-age backdate a controller-injected key never arms.
#[must_use]
pub fn hwkey_burst_args() -> Vec<String> {
    vec![
        "hwkey".into(),
        "x".into(),
        format!("count={BURST_KEYS}"),
        format!("interval={}", BURST_GAP.as_millis()),
    ]
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
/// A reported p99 can sit above the observed maximum (`p99 50ms` beside `max
/// 47.85ms`), which only a bucket edge can be. Where the same slice also
/// publishes a true maximum (acquire, present→glass), the max must confirm the
/// bar too, so a ceiling that does not fall exactly on a bucket edge cannot fail
/// a run on the edge above it either — and a sampled slice whose maximum is
/// MISSING fails: an absent max once read as 0 ms, which confirmed nothing, so a
/// renamed field let any p99 through.
///
/// The hardware burst's verdict, over both replies read after it: the `metrics`
/// `summary` (frames, the true input→present maximum, present retries and
/// drops, sync timeout-releases) and the `metrics percentiles` `reply`, slice by
/// slice. `Ok` is the pass line, `Err` the failure. Extracted so the thresholds
/// are testable.
///
/// THE FLOORS COME FIRST, so no ceiling can pass on a burst that measured
/// nothing: the keys must reach the `KeyboardInput` arm (`n_key_write`) and
/// book input→present samples (`n_input`), and the window must present
/// ([`FRAMES_FLOOR`]).
///
/// # Errors
/// A reply that cannot be parsed, a sampled slice without its maximum, a burst
/// under a floor, or any gated reading at or over its ceiling.
pub fn hardware_key_verdict(posted: u64, summary: &str, reply: &str) -> Result<String, String> {
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
    let (Some(frames), Some(max_input), Some(retries), Some(drops), Some(sync_timeouts)) = (
        metric_u64(summary, "frames"),
        metric_ms_whole(summary, "max_input_present_ms"),
        metric_u64(summary, "redraw_retry_gated"),
        metric_u64(summary, "present_drops"),
        metric_u64(summary, "sync_rel_timeout"),
    ) else {
        return Err(format!(
            "gui smoke: could not parse the post-burst metrics -> {}",
            or_no_reply(summary)
        ));
    };
    let f = |name: &str| reply_field(reply, name);
    // A sampled slice's TRUE maximum, as a number — present, or the verdict fails.
    let max_ms = |name: &str| {
        reply_field(reply, name).parse::<f64>().map_err(|_| {
            format!(
                "gui smoke: a sampled slice published no {name}, so nothing can confirm or \
                 refute its p99 [{reply}]"
            )
        })
    };
    let bar = |ms: u64| f64::from(u32::try_from(ms).unwrap_or(u32::MAX));
    if n_key_write < HW_KEY_WRITE_FLOOR {
        return Err(format!(
            "gui smoke: hardware keys never reached the KeyboardInput arm — \
             n_key_write={n_key_write} of posted={posted} (< {HW_KEY_WRITE_FLOOR}), so \
             key→write, the queue-age backdate and the press lock wait measured nothing \
             [{reply}]"
        ));
    }
    if n_input < HW_INPUT_FLOOR {
        return Err(format!(
            "gui smoke: the burst booked n_input={n_input} input→present samples of \
             posted={posted} (< {HW_INPUT_FLOOR}), so the input→present ceilings measured \
             nothing [{reply}]"
        ));
    }
    if frames < FRAMES_FLOOR {
        return Err(format!(
            "gui smoke: present starvation — frames={frames} (< {FRAMES_FLOOR}) [{summary}]"
        ));
    }
    if max_input >= INPUT_PRESENT_CEILING_MS {
        return Err(format!(
            "gui smoke: input→present latency — max {max_input}ms \
             (>= {INPUT_PRESENT_CEILING_MS}) [{summary}]"
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
        && max_ms("max_acquire_wait_ms")? >= bar(ACQUIRE_P99_CEILING_MS)
        && acquire > ACQUIRE_P99_CEILING_MS
    {
        return Err(format!(
            "gui smoke: drawable acquire — p99 {acquire}ms (> {ACQUIRE_P99_CEILING_MS}, and its max confirms it) \
             over n={n_acquire}, max {}ms [{reply}]",
            f("max_acquire_wait_ms"),
        ));
    }
    if n_present_glass > 0
        && max_ms("max_present_glass_ms")? >= bar(PRESENT_GLASS_P99_CEILING_MS)
        && glass > PRESENT_GLASS_P99_CEILING_MS
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
    if input > INPUT_PRESENT_CEILING_MS {
        return Err(format!(
            "gui smoke: hardware input→present — p99 {input}ms (> {INPUT_PRESENT_CEILING_MS}), \
             OS queue residence included [{reply}]"
        ));
    }
    if retries != 0 || drops != 0 {
        return Err(format!(
            "gui smoke: present retries/drops during frontmost typing — \
             redraw_retry_gated={retries} present_drops={drops} [{summary}]"
        ));
    }
    if sync_timeouts != 0 {
        return Err(format!(
            "gui smoke: sync timeout-releases during plain typing — \
             sync_rel_timeout={sync_timeouts} [{summary}]"
        ));
    }
    Ok(format!(
        "gui smoke: frames={frames} max_input_present={max_input}ms, no present retries, \
         drops or sync timeouts; hardware keys n_key_write={n_key_write}/{posted} \
         key_write_p99={}ms max_term_wait_press={}ms acquire_p99={}ms (n={n_acquire}) \
         input_p99={}ms (n={n_input}) present_glass_p99={}ms (n={n_present_glass}, {} \
         never shown); echo_p99={}ms is the child's round trip (reported, not gated)",
        f("key_write_p99_ms"),
        f("max_term_wait_press_ms"),
        f("acquire_p99_ms"),
        f("input_p99_ms"),
        f("present_glass_p99_ms"),
        f("present_glass_skipped"),
        f("echo_p99_ms"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::scope::Scope;

    fn ctx() -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    /// The post-burst `metrics` summary a healthy burst leaves, with the readings
    /// the verdict gates set per case.
    fn summary(frames: u64, max_input: &str, retries: u64, drops: u64, sync: u64) -> String {
        format!(
            "OK backend=gpu rows=24 cols=80 scrollback_truncated_lines=0 frames={frames} \
             last_input_present_ms=6.10 max_input_present_ms={max_input} n_input=30 \
             sync_armed=0 sync_rel_end=0 sync_rel_timeout={sync} sync_holding=0 \
             perf_reduced=0 shed_transitions=0 wake_heals=0 redraw_retry_gated={retries} \
             fx_under_frames=0 present_drops={drops} deadline_arms_by_owner=cursor_effect:0/0 "
        )
    }

    /// [`hardware_key_verdict`] on a healthy summary.
    fn hv(reply: &str) -> Result<String, String> {
        hardware_key_verdict(30, &summary(41, "8.90", 0, 0, 0), reply)
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
             n_term_wait_redraw=40 term_wait_redraw_p99_ms=0.02 max_term_wait_redraw_ms=0.10 \
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

    /// The effects verdict reads the `cursor_effect` owner out of the ledger by
    /// NAME and the ribbon witness by name; passes a healthy burst; fails both
    /// an absolute and a share late-arm regression and effects that never
    /// engaged (the first cut read an absent owner as `(0, 0)` and PASSED it) —
    /// never a burst whose ledger or witness is simply missing.
    #[test]
    fn the_effects_verdict_demands_engagement_and_bounds_late_arms() {
        let reply = |ledger: &str, under: u64| {
            format!("OK frames=40 fx_under_frames={under} deadline_arms_by_owner={ledger} turns=9")
        };
        assert_eq!(
            cursor_effect_arms(&reply("session_status:12/0,cursor_effect:80/0", 5)),
            Some((80, 0))
        );
        assert_eq!(
            cursor_effect_arms(&reply("session_status:12/0", 5)),
            Some((0, 0))
        );
        assert_eq!(cursor_effect_arms("OK frames=40"), None);
        assert!(effects_verdict(&reply("cursor_effect:430/0", 30)).is_ok());
        assert!(
            effects_verdict(&reply("cursor_effect:430/40", 30)).is_err(),
            "over the absolute bound"
        );
        assert!(
            effects_verdict(&reply("cursor_effect:80/3", 30)).is_err(),
            "over one in fifty"
        );
        assert!(
            effects_verdict(&reply("session_status:12/0", 30)).is_err(),
            "a burst that armed no cursor_effect deadline measured nothing"
        );
        assert!(
            effects_verdict(&reply("cursor_effect:80/0", 0)).is_err(),
            "a burst that composed no ribbon measured nothing"
        );
        assert!(
            effects_verdict("OK frames=40 fx_under_frames=3").is_err(),
            "no ledger is not a pass"
        );
        assert!(
            effects_verdict("OK frames=40 deadline_arms_by_owner=cursor_effect:9/0").is_err(),
            "no ribbon witness is not a pass"
        );
    }

    #[test]
    fn a_40ms_press_path_regression_fails_the_hardware_gate() {
        // The finding's scenario: 40 ms of new UI-thread work on every keystroke.
        // The echo still presents in ~58 ms, under the 250 ms input→present
        // ceilings; the burst books every key's key→write, and that fails.
        let hw = percentiles(30, "46.13", "0.10", "5.24", "58.00");
        assert!(hv(&hw).is_err());
    }

    #[test]
    fn a_healthy_hardware_burst_passes_and_the_childs_echo_is_never_gated() {
        // echo p99 150.04 ms is the run-load measurement of a child waiting behind
        // compiles: reported, and not aterm's to fail on.
        let hw = percentiles(30, "6.29", "0.10", "5.24", "14.20");
        hv(&hw).expect("a pass");
        // A lost-wake build: ~5 frames/s and 190-530 ms input→present, with
        // present retries, drops and sync timeouts; each alone fails too.
        let with = |frames, max_input: &str, retries, drops, sync| {
            hardware_key_verdict(30, &summary(frames, max_input, retries, drops, sync), &hw)
        };
        for (case, frames, max_input, retries, drops, sync) in [
            ("starved", 10, "8.90", 0, 0, 0),
            ("slow", 41, "530.10", 0, 0, 0),
            ("retries", 41, "8.90", 4, 0, 0),
            ("drops", 41, "8.90", 0, 7, 0),
            ("sync timeouts", 41, "8.90", 0, 0, 2),
        ] {
            assert!(
                with(frames, max_input, retries, drops, sync).is_err(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_burst_that_never_reached_the_keyboard_arm_is_a_failure_not_a_pass() {
        // Exactly what a `ctl key`-driven burst reports: every slice healthy, and
        // `n_key_write=0` because nothing armed the key-arrival stamp.
        let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
        assert!(hv(&controller_shaped).is_err());
    }

    /// THE INPUT CEILINGS CANNOT PASS VACUOUSLY: a burst that booked no
    /// input→present sample reads `max 0 ms` and `p99 0 ms`, under both bars.
    #[test]
    fn a_burst_that_booked_no_input_samples_is_a_failure_not_a_pass() {
        let hw = |n: u64| {
            percentiles(30, "6.00", "0.10", "1.00", "0.00")
                .replace("n_input=30", &format!("n_input={n}"))
        };
        assert!(hardware_key_verdict(30, &summary(41, "0.00", 0, 0, 0), &hw(0)).is_err());
        assert!(hv(&hw(30)).is_ok(), "the control");
    }

    /// AN ABSENT MAXIMUM FAILS CLOSED. A sampled acquire or compositor slice
    /// must publish its true maximum: it used to read as 0 ms, which confirmed
    /// no bar, so a p99 far over its ceiling PASSED when the max field was
    /// renamed or dropped. An unsampled slice still needs none.
    #[test]
    fn a_sampled_slice_without_its_maximum_fails_closed() {
        let without = |reply: &str, field: &str| {
            let at = reply
                .find(&format!(" {field}="))
                .expect("the field is in the reply");
            let end = at + 1 + reply[at + 1..].find(' ').expect("a field ends in a space");
            format!("{}{}", &reply[..at], &reply[end..])
        };
        let healthy = percentiles(30, "6.00", "0.10", "1.00", "10.00");
        for field in ["max_acquire_wait_ms", "max_present_glass_ms"] {
            assert!(hv(&without(&healthy, field)).is_err(), "{field}");
        }
        // The fail-open this closes: an acquire p99 over the bar, max missing.
        let slow = percentiles(30, "6.00", "0.10", "56.00", "10.00");
        assert!(hv(&without(&slow, "max_acquire_wait_ms")).is_err());
        // Unsampled slices publish no maximum worth reading, and pass.
        let unsampled = without(
            &without(&healthy, "max_acquire_wait_ms"),
            "max_present_glass_ms",
        )
        .replace("n_acquire=31", "n_acquire=0")
        .replace("n_present_glass=31", "n_present_glass=0");
        assert!(hv(&unsampled).is_ok(), "{unsampled}");
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
            hv(&with_glass(31, "8.90")).is_ok(),
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
        assert!(hv(&held).is_err());
        // The reported p99 is a bucket's EXCLUSIVE upper edge: an edge above the
        // bar that no sample reached (a 12.40 ms worst frame) is not a slow leg,
        // and an unsampled leg (a CPU backend, a non-macOS present) is not one
        // either.
        assert!(hv(&with_glass(31, "56.00")).is_ok());
        assert!(hv(&with_glass(0, "900.00")).is_ok());
    }

    #[test]
    fn an_unparsable_hardware_reply_fails_closed() {
        assert!(hv("").is_err());
        // The summary line (what `metrics` answers) lacks the slices: never a pass.
        let summary =
            "OK frames=41 max_input_present_ms=8.100 last_key_write_ms=0.00 max_key_write_ms=0.00 ";
        assert!(hv(summary).is_err());
        // …nor is a summary that lacks a gated reading.
        let hw = percentiles(30, "6.00", "0.10", "1.00", "10.00");
        assert!(hardware_key_verdict(30, "OK frames=41", &hw).is_err());
        assert!(hardware_key_verdict(30, "", &hw).is_err());
    }

    #[test]
    fn an_ssh_session_cannot_present_and_says_so() {
        let mut c = ctx();
        c.env.ssh_connection = Some("10.0.0.1 22 10.0.0.2 22".into());
        assert!(gui_smoke_unavailable(&c).is_some());
    }

    #[test]
    fn the_sandbox_is_private_and_short_enough_for_a_unix_socket() {
        let mut sb = Sandbox::with_config("atl", "").expect("sandbox");
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
        let mut sb = Sandbox::with_config("atl", "").expect("sandbox");
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
        // Its own exit, witnessed before teardown's TERM — a 50 ms nap let a
        // starved `sh` be killed first, a death teardown DID cause.
        let pid = sb.child.as_ref().expect("child").id();
        assert!(
            crate::smoke::exited_on_its_own(pid),
            "`exit 7` never exited within a minute"
        );
        let mut r = Report::new("t");
        sb.teardown(&mut r);
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::Outcome::Fail(crate::Severity::GateFailed)]
        );
    }
}
