// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two smokes: a headless round trip and typing burst, and the pacing gate.
//!
//! 5) HEADLESS CONTROL-SOCKET SMOKE (the LAND tier's exclusive stage). Launch
//!    `aterm-gui --headless` sandboxed under a throwaway `$XDG_RUNTIME_DIR`,
//!    drive one `aterm-ctl cursor` round trip, then a plain typing burst through
//!    `aterm-ctl send`, and read one counter off `metrics`. Two checks, each the
//!    only automatic guard of its failure:
//!    - `cursor` must answer `OK <row> <col> <visible> <style>`: it is the
//!      preflight every `aterm drive` verb sends first, and no suite sends it;
//!    - `wake_heals` must stay 0 over a burst of at least half its keys
//!      delivered. A heal is a `Wake::Output` that went unhandled past the latch
//!      expiry — the 2026-07-05 incident class, which the self-expiring latch
//!      now turns from a permanent 5 fps into a 100 ms hiccup per lost wake. A
//!      burst whose every wake is lost and healed at the expiry can still clear
//!      a window's frames floor and its 250 ms input→present ceiling, so this
//!      counter is the one reading that sees the class.
//!
//!    What it no longer checks (2026-09-28) was never measured here:
//!    `sync_rel_timeout` and `perf_reduced` are booked only on a window's
//!    present path, so a headless instance read 0 whatever the tree did — the
//!    GUI smoke reads them where they are real. The socket's boot is also
//!    proven by the live `aterm --headless` suites
//!    (`crates/aterm/tests/*_live_headless.rs`, whose one boot panics on an
//!    instance that exits or never listens), the aterm-link e2e worlds and the
//!    foreground handback. The stage is also the LAND tier's barrier in every
//!    scope: the driver-lane rows behind it (the objc drives, the foreground
//!    handback) run with nothing else in flight.
//!
//! 5b) GUI TYPING-PACING SMOKE (macOS desktop only; the MEASURE tier's since
//!    2026-09-26 — `--measure` and `--full` run it, the merge contract does
//!    not, and a release cut requires it green). Headless never PRESENTS, so
//!    only a real window can measure pacing. The 2026-07-05 incident build
//!    presented at ~5/s with 190-530 ms input→present; a healthy build does 30+/s
//!    under 15 ms. Skips automatically without a WindowServer session (CI/SSH), or
//!    with `--skip-gui-smoke`.
//!
//!    ONE BURST PER LANE (2026-09-28): a hardware burst (`ctl hwkey`) that posts
//!    real NSEvents into the app's own queue, so the keys take winit's
//!    `KeyboardInput` arm like a physical press — OS queue residence included,
//!    which the controller seam (`ctl key`, born already dequeued) never saw.
//!    One verdict reads both replies after it ([`hardware_key_verdict`]): the
//!    `metrics` summary's frames floor, true input→present maximum, present
//!    retries and drops and sync timeout-releases, and `metrics percentiles`'
//!    per-slice ceilings, so a failure names who owes the time: aterm's
//!    `key->write`, the press's terminal-mutex wait, the drawable acquire, the
//!    compositor leg and the queue-inclusive input->present. The child's echo
//!    round trip is reported and never gated, because it is not aterm's cost.
//!
//!    The burst runs in THREE EFFECT LANES ([`effect_lanes`];
//!    docs/INCIDENT-typing-latency-2026-08-26.md items 1, 2 and item 4's
//!    late-deadline half) — the cursor effects OFF, and ON at the shipped default
//!    style with trail sounds off and on (the last is the shipped default
//!    config) — each on a fresh instance. An ON lane must show the effects
//!    engaged (it armed `cursor_effect` deadlines and composed the ribbon,
//!    `fx_under_frames`) and the OFF lane must compose no ribbon, so no lane can
//!    pass having measured nothing; a lane fails when the `cursor_effect` owner
//!    books late (past) arms faster than [`CURSOR_EFFECT_LATE_MAX`] allows. Each
//!    ON lane's input→present, present, key→write and frame-gap readings are
//!    reported beside the OFF lane's (reported, not gated: no relative bound has
//!    been calibrated).
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
//!  * These stages are EXCLUSIVE (see `crate::sched`), as are the deadline and
//!    measuring tests: they decide on frame counts and latencies, so they must
//!    own the machine.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::Ctx;
use crate::exec::{Capture, Cmd, capture_reply, run as exec_run};
use crate::glob::glob_match;
use crate::ladder::Report;
use crate::smoke::{
    is_socket_or_symlink, metric_ms_whole, metric_u64, retire_smoke_child, smoke_log_tail,
    socket_listening,
};
use crate::stages::{driver_bin, smoke_build_args};

/// The reply shape the smokes decide a verb answered with: `OK` and a field.
/// (Every counter is read by name, [`metric_u64`], so an absent one fails.)
const OK_REPLY: &str = "OK *";

/// One effect lane: its label, the `aterm.toml` lines that select it (top-level
/// keys, ahead of the sandbox's first `[table]`), and whether the cursor
/// effects are on in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectLane {
    pub label: &'static str,
    pub config: &'static str,
    pub effects_on: bool,
}

/// THE EFFECT LANES: the cursor effects OFF — `cursor_trail = false`, the
/// family's master, which also silences the trail sounds (their cues come from
/// the engine it gates) and, since 2026-09-27, the typing-momentum glow (before
/// that fix the glow sat outside the master, and a master-off lane that left it
/// on armed ~430 `cursor_effect` deadlines a burst, so this lane also switched
/// it off by hand) — then ON at the shipped default style with trail sounds off
/// and on. The OFF lane is what a user who turns the effects off writes: the
/// baseline the ON lanes are reported against, and the negative control for the
/// engagement witnesses — it must compose no ribbon.
#[must_use]
pub fn effect_lanes() -> [EffectLane; 3] {
    [
        EffectLane {
            label: "effects=off",
            config: "cursor_trail = false\ntrail_sounds = false\n",
            effects_on: false,
        },
        EffectLane {
            label: "effects=on sounds=off",
            config: "cursor_trail = true\ntrail_sounds = false\n",
            effects_on: true,
        },
        EffectLane {
            label: "effects=on sounds=on",
            config: "cursor_trail = true\ntrail_sounds = true\n",
            effects_on: true,
        },
    ]
}

/// Late (`past`) `cursor_effect` arms one lane's burst may book. CALIBRATED
/// 2026-09-27 on a WindowServer session (docs/INCIDENT-typing-latency-2026-08-26.md):
/// ten healthy ON-lane bursts armed 417-432 deadlines each and booked 0, 0, 0,
/// 0, 0, 1, 1, 1, 1 and 2 late — the lane services its deadline on every wake
/// (e578d3ff0, c795ca6ee). Four leaves two of headroom over that. What it can
/// catch is a SUSTAINED regression: the incident's own rate (97 late of 12,477
/// arms, 0.8 %) is about 3.3 a burst, which one burst cannot tell from healthy.
const CURSOR_EFFECT_LATE_MAX: u64 = 4;
/// …and as a share of the owner's arms, so a lane that armed little cannot pass
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

/// One effect lane's verdict on its post-burst `metrics` summary: the
/// engagement witnesses (an ON lane armed `cursor_effect` deadlines and
/// composed the ribbon, `fx_under_frames > 0`; the OFF lane composed none),
/// then its late `cursor_effect` arms against [`CURSOR_EFFECT_LATE_MAX`] and
/// the one-in-ten share.
///
/// # Errors
/// A summary missing either reading, an ON lane whose effects never engaged,
/// an OFF lane that composed ribbon, or too many late arms.
pub fn effect_lane_verdict(lane: &EffectLane, reply: &str) -> Result<String, String> {
    let name = lane.label;
    let Some((arms, late)) = cursor_effect_arms(reply) else {
        return Err(format!(
            "gui smoke [{name}]: metrics carry no deadline_arms_by_owner ledger -> {}",
            or_no_reply(reply)
        ));
    };
    let Some(under) = metric_u64(reply, "fx_under_frames") else {
        return Err(format!(
            "gui smoke [{name}]: metrics carry no fx_under_frames -> {}",
            or_no_reply(reply)
        ));
    };
    if lane.effects_on && (arms == 0 || under == 0) {
        return Err(format!(
            "gui smoke [{name}]: the cursor effects never engaged — cursor_effect \
             arms={arms}, fx_under_frames={under} — so this lane measured an \
             effects-off burst under an effects-on label"
        ));
    }
    if !lane.effects_on && under > 0 {
        return Err(format!(
            "gui smoke [{name}]: the effects-off lane composed ribbon on {under} \
             ticks — the lane is not off, and the baseline is not one"
        ));
    }
    let over_share = late.saturating_mul(CURSOR_EFFECT_LATE_SHARE_DEN) > arms && late > 1;
    if late > CURSOR_EFFECT_LATE_MAX || over_share {
        return Err(format!(
            "gui smoke [{name}]: cursor_effect booked {late} late arms of {arms} during a \
             paced burst (max {CURSOR_EFFECT_LATE_MAX}, and at most 1 in \
             {CURSOR_EFFECT_LATE_SHARE_DEN}) — the effect lane is arming deadlines it \
             already missed"
        ));
    }
    Ok(format!(
        "gui smoke [{name}]: cursor_effect arms={arms} late={late} fx_under_frames={under}"
    ))
}

/// The readings one lane contributes to the cross-lane comparison
/// (INCIDENT-typing-latency item 2), as published: `input_p95_ms`,
/// `present_p95_ms`, `max_frame_gap_ms` and the per-owner deadline ledger off
/// the `metrics` summary, `key_write_p99_ms` off `metrics percentiles`.
#[must_use]
pub fn lane_latency(summary: &str, percentiles: &str) -> LaneReadings {
    [
        ("input_p95", reply_field(summary, "input_p95_ms").to_owned()),
        (
            "present_p95",
            reply_field(summary, "present_p95_ms").to_owned(),
        ),
        (
            "key_write_p99",
            reply_field(percentiles, "key_write_p99_ms").to_owned(),
        ),
        (
            "max_frame_gap",
            reply_field(summary, "max_frame_gap_ms").to_owned(),
        ),
        (
            "deadline_arms_by_owner",
            reply_field(summary, "deadline_arms_by_owner").to_owned(),
        ),
    ]
}

/// One lane's comparison readings, `(name, value as published)`.
pub type LaneReadings = [(&'static str, String); 5];

/// The comparison line: every reading of every lane that measured, lane by
/// lane, in ms. Reported, not gated.
#[must_use]
pub fn lane_comparison(readings: &[(&str, LaneReadings)]) -> String {
    let mut line = String::from("gui smoke lanes (reported, not gated; times in ms):");
    for (label, fields) in readings {
        line.push_str(" [");
        line.push_str(label);
        line.push(']');
        for (name, value) in fields {
            line.push(' ');
            line.push_str(name);
            line.push('=');
            line.push_str(value);
        }
    }
    line
}

/// 30 driven keys at ~20/s, then a second to settle — a human-shaped light typing
/// burst, deliberately not a socket-flood test. Both smokes type it.
const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);
const SETTLE: Duration = Duration::from_secs(1);
/// Keys the headless burst must deliver (`send` answered `OK`) before its
/// `wake_heals=0` means anything: half the burst, the GUI floors' ratio. A heal
/// is booked only when a LATER output chunk finds a lost wake's latch older than
/// the 100 ms expiry (aterm-gui's `gated_output_wake`), and each delivered key is
/// one echo, so one key — or a few inside 100 ms — cannot register a lost wake
/// whatever the tree does.
const SEND_ACCEPTED_FLOOR: usize = BURST_KEYS / 2;

/// The input->present ceiling, on the true maximum (`>=` fails) and on the
/// bucketed p99 (`>` fails): ~10x the healthy margin and still far under the
/// 2026-07-05 incident's 300-530 ms worst case. Measured 2026-09-28 on the
/// hardware burst, OS queue residence included (Apple M5 Max, 18 cores, load
/// 19-38): true maximum 6, 9 and 8 ms in the effects-off, on/sounds-off and
/// on/sounds-on lanes, input p99 6.82, 9.44 and 9.44 ms.
const INPUT_PRESENT_CEILING_MS: u64 = 250;
/// Frames the window must present over the burst: a healthy build presents one
/// or more per key (30+), the 2026-07-05 incident build ~10. Measured
/// 2026-09-28 (same run as [`INPUT_PRESENT_CEILING_MS`]): 31 frames with the
/// effects off — one per key, so this floor is half the quietest healthy lane —
/// and 169 and 173 with them on.
const FRAMES_FLOOR: u64 = 15;
/// Hardware keys that must reach the `KeyboardInput` arm (`n_key_write`) before
/// any hardware slice verdict means anything: half the burst, the frames floor's
/// ratio. Below it the burst measured nothing, and that is a FAIL, not a pass.
const HW_KEY_WRITE_FLOOR: u64 = 15;
/// Input→present samples the burst must book (`n_input`, one per presented key)
/// before the input→present ceilings mean anything, at the same ratio: a burst
/// that booked none would pass both on a maximum of 0 ms. Measured 2026-09-28:
/// n_input=30 and n_key_write=30 of 30 posted in every lane.
const HW_INPUT_FLOOR: u64 = 15;
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
/// The socket-listen budget: how many times a smoke looks at its instance's
/// control socket, [`POLL_GAP`] apart, before `control socket never started
/// listening` ([`wait_to_listen`]) — ten seconds of sleeps, plus what each
/// look (a `try_wait`, a stat and a connect) costs on the machine of the day.
///
/// COUNTED IN LOOKS, NEVER TIMED BY THE CLOCK (the review of 2026-09-28).
/// f6921d3de timed it at "exactly 10 s", which can only be shorter than the
/// looks it replaced: the review measured 0.26-0.40 s less under load 4.5-7,
/// and an instance listening 10.15-10.3 s after its spawn, which passed the
/// smoke before, red. The gate's own fixtures wait the same: the stand-in
/// instance their build writes is run once by that build, so its first exec
/// is paid there.
pub const SOCKET_POLLS: usize = 100;
/// The pause between two looks of a smoke's waits.
pub const POLL_GAP: Duration = Duration::from_millis(100);

/// A running smoke's disposable state, torn down on every exit path.
struct Sandbox {
    tmp: PathBuf,
    rundir: PathBuf,
    cfgdir: PathBuf,
    gui_log: PathBuf,
    child: Option<Child>,
    /// The headless child's lifeline ([`arm_lifeline`]): dropped by `teardown`
    /// after the child is retired, and closed by the kernel if this gate dies first.
    lifeline: Option<std::fs::File>,
}

impl Sandbox {
    fn new(tag: &str) -> Option<Self> {
        Self::with_config(tag, "")
    }

    /// [`Self::new`] with extra `aterm.toml` lines ahead of the fixed tables
    /// (top-level keys must precede the first `[table]`).
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
            format!(
                "{extra}agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
                 [packages]\nenabled = false\n\
                 [machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n"
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
            lifeline: None,
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
                r.fail_and_continue("smoke: child cleanup/reap failed");
            }
        }
        self.lifeline.take();
        std::fs::remove_dir_all(&self.tmp).ok();
    }
}

/// Arm a HEADLESS smoke child with a lifeline — `--lifeline-fd 0` on a FIFO this
/// gate holds the one writer of — so a gate run that is killed outright (no
/// teardown, no `Drop`) does not leave its instance running (gap #36: a leaked
/// headless instance ran for eleven days). The protocol and its measurements are
/// `aterm_uds::lifeline`'s; this is the same launcher end in std alone, because
/// this crate has no dependencies by charter: `mkfifo(1)` makes the node, and the
/// held end is opened read-write (never blocks: it is its own reader), the far
/// end read-only (a writer exists), both close-on-exec from birth, the name
/// unlinked at once. Call it before any `-e` payload.
///
/// # Errors
/// The FIFO could not be made or opened; nothing is left behind.
fn arm_lifeline(cmd: &mut Command, dir: &Path) -> std::io::Result<std::fs::File> {
    let fifo = dir.join("lifeline");
    let mkfifo = ["/usr/bin/mkfifo", "/bin/mkfifo"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("mkfifo");
    let made = Command::new(mkfifo)
        .arg("-m")
        .arg("600")
        .arg(&fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !made.success() {
        return Err(std::io::Error::other(format!("mkfifo exited {made}")));
    }
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .and_then(|held| std::fs::File::open(&fifo).map(|far| (held, far)));
    std::fs::remove_file(&fifo).ok();
    let (held, far) = opened?;
    cmd.arg("--lifeline-fd").arg("0").stdin(far);
    Ok(held)
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
    let build = crate::stages::driver_build_cmd(ctx, smoke_build_args())
        .capture(Capture::Append(sb.gui_log.clone()));
    let built = exec_run(&build, ctx.exec_env());
    if !built.ok {
        r.fail_child(
            &built,
            format!("{tag}: targo build -p aterm-gui -p aterm-ctl failed"),
        );
        r.raw(smoke_log_tail(log_label, &sb.gui_log));
        // What the build kept from running is decided by nothing (2026-09-27,
        // fourth review): a smoke build red main had stood for the smoke.
        if built.environment_failure().is_none() {
            crate::stages::not_run_behind(
                r,
                &format!("{tag}: the smoke's checks"),
                "the build above failed, so there is no fresh aterm-gui to launch",
            );
        }
        return Ready::Stopped;
    }
    // The driver lane's dir: that is where the build above put them.
    let gui = driver_bin(ctx, "aterm-gui");
    let ctl = driver_bin(ctx, "aterm-ctl");
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
        .env("SHELL", "/bin/sh")
        .stdout(log)
        .stderr(log2);
    // The socket and headless are launch FLAGS — the one spelling; no environment
    // variable selects either (2026-09-24).
    cmd.arg("--control-sock").arg(sb.sock());
    if headless {
        cmd.arg("--headless");
        match arm_lifeline(&mut cmd, &sb.tmp) {
            Ok(held) => sb.lifeline = Some(held),
            Err(e) => {
                r.cannot_run(format!("{tag}: cannot arm the instance's lifeline ({e})"));
                return Ready::Stopped;
            }
        }
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
    let up = || is_socket_or_symlink(&sock) && socket_listening(&sock);
    match wait_to_listen(|| child_exited(sb), up) {
        Listen::Up => return Ready::Up { ctl },
        Listen::Exited => r.fail(format!("{tag}: aterm-gui exited early")),
        Listen::Never => r.fail(format!("{tag}: control socket never started listening")),
    }
    r.raw(smoke_log_tail(log_label, &sb.gui_log));
    Ready::Stopped
}

/// What a smoke's listen wait found ([`wait_to_listen`]).
#[derive(Debug, PartialEq, Eq)]
enum Listen {
    /// The instance listens.
    Up,
    /// It exited first.
    Exited,
    /// It never listened within the wait.
    Never,
}

/// THE LISTEN WAIT: [`SOCKET_POLLS`] looks at `exited` and then `up`,
/// [`POLL_GAP`] apart, and one last look at `up` after them. However long a
/// look takes, every one is taken: the wait is never shorter than its sleeps.
fn wait_to_listen(mut exited: impl FnMut() -> bool, mut up: impl FnMut() -> bool) -> Listen {
    for _ in 0..SOCKET_POLLS {
        if exited() {
            return Listen::Exited;
        }
        if up() {
            return Listen::Up;
        }
        std::thread::sleep(POLL_GAP);
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

fn ctl_quiet(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) {
    let cmd = Cmd::new(ctl_bin)
        .arg("--sock")
        .arg(sb.sock())
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
        cursor_round_trip(ctx, r, &sb, &ctl_bin);
        headless_typing_burst(ctx, r, &sb, &ctl_bin);
    }
    sb.teardown(r);
}

/// `aterm-ctl cursor`, the preflight every `aterm drive` verb on the `CtlClient`
/// path sends before driving (aterm-agent's `drive_cli`: "cannot reach an
/// aterm"), must answer its documented reply ([`is_cursor_reply`]). No suite
/// sends the plain verb, so this row is its one automatic guard. The burst
/// still runs after a bad reply: this row hides nothing, so it never ends the
/// smoke ([`Report::fail_and_continue`]).
fn cursor_round_trip(ctx: &Ctx, r: &mut Report, sb: &Sandbox, ctl_bin: &Path) {
    let got = ctl(ctx, sb, ctl_bin, &["cursor"]);
    if is_cursor_reply(&got) {
        r.pass(format!("smoke: aterm-ctl cursor -> {got}"));
    } else {
        r.fail_and_continue(format!("smoke: aterm-ctl cursor -> {}", or_no_reply(&got)));
    }
}

/// `cursor`'s documented reply (aterm-gui's `control_query::cmd_cursor`):
/// `OK <row> <col> <visible:0|1> <style>`, one line. The shape, not a bare
/// `OK *`, so a verb routed to another handler that also answers `OK …` does
/// not pass for it.
fn is_cursor_reply(reply: &str) -> bool {
    let fields: Vec<&str> = reply.split_whitespace().collect();
    matches!(
        fields.as_slice(),
        ["OK", row, col, "0" | "1", _style]
            if row.parse::<u32>().is_ok() && col.parse::<u32>().is_ok()
    )
}

/// A plain typing burst through `aterm-ctl send`, then `wake_heals` off `metrics`.
fn headless_typing_burst(ctx: &Ctx, r: &mut Report, sb: &Sandbox, ctl_bin: &Path) {
    ctl_quiet(ctx, sb, ctl_bin, &["metrics", "reset"]);
    // Let the reader/event-loop lane reach its ordinary idle wait before the
    // first byte, or the first startup-edge repair is miscounted as a
    // typing-burst lost-wake heal.
    std::thread::sleep(Duration::from_millis(250));
    // COUNT WHAT THE ENGINE ACCEPTED. `wake_heals=0` is satisfied A FORTIORI by
    // a burst that typed too little for a heal to register
    // ([`SEND_ACCEPTED_FLOOR`]), so a dead client, a rejected verb or a wedged
    // input seam would pass for a subject this stage never exercised. `send`
    // answers the bare `OK` on success and `ERR …` otherwise, so an OK-prefixed
    // reply is the evidence available at this seam, and the count rides on the
    // ladder line so the row states what it measured.
    let mut accepted = 0usize;
    for _ in 0..BURST_KEYS {
        if ctl(ctx, sb, ctl_bin, &["send", "x"]).starts_with("OK") {
            accepted += 1;
        }
        std::thread::sleep(BURST_GAP);
    }
    std::thread::sleep(SETTLE);
    if accepted < SEND_ACCEPTED_FLOOR {
        r.fail(format!(
            "smoke: only {accepted}/{BURST_KEYS} keys accepted (< {SEND_ACCEPTED_FLOOR}), \
             too few for a lost wake to register"
        ));
        return;
    }
    let got = ctl(ctx, sb, ctl_bin, &["metrics"]);
    // The smoke's last check: it hides none.
    match metric_u64(&got, "wake_heals") {
        Some(0) => r.pass(format!(
            "smoke: {accepted}/{BURST_KEYS} keys accepted over the control socket, no lost-wake heals"
        )),
        Some(n) => r.fail_and_continue(format!(
            "smoke: {n} lost output wake(s) healed during a plain typing burst -> {got}"
        )),
        None => r.fail_and_continue(format!(
            "smoke: metrics carry no wake_heals after the typing burst -> {}",
            or_no_reply(&got)
        )),
    }
}

// ---------------------------------------------------------------------------
// 5b) GUI TYPING-PACING SMOKE
// ---------------------------------------------------------------------------
pub fn gui_typing_smoke(ctx: &Ctx, r: &mut Report) {
    if let Some(reason) = gui_smoke_unavailable(ctx) {
        r.skip(reason);
        return;
    }
    let mut readings = Vec::new();
    for lane in effect_lanes() {
        let tag = format!("gui smoke [{}]", lane.label);
        let Some(mut sb) = Sandbox::with_config("atl", lane.config) else {
            r.cannot_run(format!("{tag}: mktemp"));
            continue;
        };
        if let Ready::Up { ctl: ctl_bin } = bring_up(ctx, r, &mut sb, &tag, "GUI smoke", false)
            && let Some(reading) = effect_lane_burst(ctx, r, &mut sb, &ctl_bin, &lane)
        {
            readings.push((lane.label, reading));
        }
        sb.teardown(r);
    }
    if !readings.is_empty() {
        r.raw(lane_comparison(&readings));
    }
}

/// One lane: frontmost, first present, reset, the paced HARDWARE burst, settle;
/// then the lane's engagement and late-arm verdict on its `metrics` summary, and
/// [`hardware_key_verdict`] on the summary and `metrics percentiles` together.
/// Returns the lane's comparison readings.
fn effect_lane_burst(
    ctx: &Ctx,
    r: &mut Report,
    sb: &mut Sandbox,
    ctl_bin: &Path,
    lane: &EffectLane,
) -> Option<LaneReadings> {
    let name = lane.label;
    let Some(pid) = sb.child.as_ref().map(Child::id) else {
        r.cannot_run(format!("gui smoke [{name}]: no child to measure"));
        return None;
    };
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip(format!(
            "gui smoke [{name}] (could not make the test window frontmost)"
        ));
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return None;
    }
    // Prove one real present happened while frontmost BEFORE resetting counters:
    // this distinguishes a product present failure from a measurement window that
    // opened while backend initialization was still in flight.
    let mut got = String::new();
    let mut presented = false;
    for _ in 0..150 {
        if child_exited(sb) {
            r.fail(format!(
                "gui smoke [{name}]: aterm-gui exited before its initial present"
            ));
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return None;
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
            "gui smoke [{name}]: the frontmost window never produced an initial present [{}]",
            or_no_reply(&got)
        ));
        r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
        return None;
    }
    // Posted keys go to the KEY window: re-assert frontmost after the wait.
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip(format!(
            "gui smoke [{name}] (could not keep the test window frontmost)"
        ));
        return None;
    }
    let got = ctl(ctx, sb, ctl_bin, &["metrics", "reset"]);
    if !glob_match(OK_REPLY, &got) {
        r.fail(format!(
            "gui smoke [{name}]: metrics reset -> {}",
            or_no_reply(&got)
        ));
        return None;
    }
    let args = hwkey_burst_args();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    // Blocks while it paces the burst (~1.5 s); `OK posted=<n>` says only that the
    // OS queue took them. What arrived is read from the replies below.
    let got = ctl(ctx, sb, ctl_bin, &argv);
    let Some(posted) = metric_u64(&got, "posted").filter(|_| glob_match(OK_REPLY, &got)) else {
        r.fail(format!(
            "gui smoke [{name}]: hardware key injection -> {}",
            or_no_reply(&got)
        ));
        return None;
    };
    std::thread::sleep(SETTLE);
    let summary = ctl(ctx, sb, ctl_bin, &["metrics"]);
    let percentiles = ctl(ctx, sb, ctl_bin, &["metrics", "percentiles"]);
    match effect_lane_verdict(lane, &summary) {
        Ok(line) => r.pass(line),
        Err(bad) => r.fail_and_continue(bad),
    }
    match hardware_key_verdict(posted, &summary, &percentiles) {
        Ok(line) => r.pass(format!("[{name}] {line}")),
        Err(bad) => r.fail_and_continue(format!("[{name}] {bad}")),
    }
    Some(lane_latency(&summary, &percentiles))
}

/// Every honest reason this stage cannot measure anything, in the script's order.
/// Each is a SKIP: the machine cannot present, which says nothing about the code.
fn gui_smoke_unavailable(ctx: &Ctx) -> Option<String> {
    if ctx.skip_gui_smoke {
        return Some("gui smoke (--skip-gui-smoke)".into());
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
/// Measured 2026-09-18 on a 2017 Intel MacBook Pro, in the merge gate: the
/// compositor leg reported `p99 50ms` beside `max 47.85ms` — a percentile above
/// the observed maximum, which only a bucket edge can be — and the gate refused
/// a run whose worst frame was 2 ms inside budget. Where the same slice also
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
    use crate::cli::Mode;
    use crate::scope::Scope;

    fn ctx() -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            Mode::Fast,
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    /// A REAL RUN'S LISTEN WAIT IS [`SOCKET_POLLS`] LOOKS, WHATEVER EACH COSTS
    /// (the review of 2026-09-28). The wait was a hundred polls, each a
    /// `try_wait`, a stat and a connect before its 100 ms sleep, so it stood
    /// for ten seconds plus what those cost on the machine of the day.
    /// f6921d3de timed it by the clock instead, "exactly 10 s", which can only
    /// be shorter: the review measured 0.26-0.40 s less under load 4.5-7, and
    /// an instance listening 10.15-10.3 s after its spawn passed the old smoke
    /// and was red in the new one — the slow-first-exec exposure the change
    /// was for, in a real run. A look here costs 15 ms: every poll is still
    /// taken, and the last look after them. Cheap controls: an instance that
    /// listens, or exits, ends the wait at that look.
    #[test]
    fn a_real_runs_listen_wait_takes_every_poll_whatever_each_costs() {
        let looks = std::cell::Cell::new(0usize);
        let costly = || {
            looks.set(looks.get() + 1);
            std::thread::sleep(Duration::from_millis(15));
            false
        };
        let started = std::time::Instant::now();
        assert_eq!(wait_to_listen(|| false, costly), Listen::Never);
        assert_eq!(
            looks.get(),
            SOCKET_POLLS + 1,
            "every poll, and the last look"
        );
        assert!(started.elapsed() >= Duration::from_secs(10));

        let third = std::cell::Cell::new(0usize);
        let up_at_third = || {
            third.set(third.get() + 1);
            third.get() == 3
        };
        assert_eq!(wait_to_listen(|| false, up_at_third), Listen::Up);
        assert_eq!(third.get(), 3);
        assert_eq!(wait_to_listen(|| true, || false), Listen::Exited);
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

    /// THE PACING BOUNDS, FOLDED INTO THE ONE VERDICT (2026-09-28). The
    /// controller burst's `pacing_verdict` enforced these four on its own
    /// `metrics` read; the hardware burst's verdict now reads the same summary
    /// after the same burst length and holds every one at the same boundary —
    /// plus the sync timeout-releases the stage checked beside it.
    #[test]
    fn the_summary_bounds_are_the_ported_pacing_boundaries() {
        let hw = percentiles(30, "6.00", "0.10", "1.00", "10.00");
        let with = |frames, max_input: &str, retries, drops, sync| {
            hardware_key_verdict(30, &summary(frames, max_input, retries, drops, sync), &hw)
        };
        assert!(
            with(41, "8.90", 0, 0, 0).is_ok(),
            "the healthy summary passes"
        );
        let starved = with(14, "8.90", 0, 0, 0).expect_err("14 frames");
        assert!(
            starved.contains("present starvation — frames=14 (< 15)"),
            "{starved}"
        );
        assert!(
            with(15, "8.90", 0, 0, 0).is_ok(),
            "15 frames is the floor, inclusive"
        );
        assert!(
            with(41, "249.90", 0, 0, 0).is_ok(),
            "249 ms is under the ceiling"
        );
        // A TRUE maximum, so `>=`: 250 ms is a sample at the bar.
        let slow = with(41, "250.00", 0, 0, 0).expect_err("250 ms");
        assert!(
            slow.contains("input→present latency — max 250ms (>= 250)"),
            "{slow}"
        );
        for (retries, drops) in [(4, 0), (0, 7)] {
            let bad = with(41, "8.90", retries, drops, 0).expect_err("retries or drops");
            assert!(
                bad.contains("present retries/drops during frontmost typing"),
                "{bad}"
            );
        }
        let sync = with(41, "8.90", 0, 0, 2).expect_err("sync timeouts");
        assert!(
            sync.contains("sync timeout-releases during plain typing — sync_rel_timeout=2"),
            "{sync}"
        );
        // The 2026-07-05 incident build: ~5 frames/s and 190-530 ms input→present.
        assert!(with(10, "530.10", 4, 7, 2).is_err());
        // A summary that lacks a gated reading is never a pass.
        let bare = hardware_key_verdict(30, "OK frames=41", &hw).expect_err("no summary");
        assert!(
            bare.starts_with("gui smoke: could not parse the post-burst metrics -> OK frames=41"),
            "{bare}"
        );
        assert!(hardware_key_verdict(30, "", &hw).is_err());
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

    /// THE EFFECT LANES cross the effects master with the trail sounds: one
    /// OFF lane (the master off, and the sounds with it), two ON lanes that
    /// differ only in the sounds — every lane a distinct config of top-level keys
    /// only (they precede the sandbox's first `[table]`). No two lanes may be the
    /// same configuration under two spellings (the first cut crossed
    /// `"rainbow kitty"` with the default, which IS `rainbow kitty pet`).
    #[test]
    fn the_effect_lanes_cross_the_effects_master_with_the_sounds() {
        let lanes = effect_lanes();
        let configs: std::collections::BTreeSet<&str> = lanes.iter().map(|l| l.config).collect();
        assert_eq!(
            configs.len(),
            lanes.len(),
            "every lane is a distinct config"
        );
        let off: Vec<_> = lanes.iter().filter(|l| !l.effects_on).collect();
        assert_eq!(off.len(), 1, "one effects-off baseline");
        assert!(off[0].config.contains("cursor_trail = false"));
        assert!(
            !off[0].config.contains("cursor_momentum_glow"),
            "the master alone is the off switch (the momentum glow obeys it since \
             2026-09-27): the OFF lane is what a user writes"
        );
        let on: Vec<_> = lanes.iter().filter(|l| l.effects_on).collect();
        assert!(on.iter().all(|l| l.config.contains("cursor_trail = true")));
        assert_eq!(
            on.iter()
                .filter(|l| l.config.contains("trail_sounds = true"))
                .count(),
            1,
            "the ON lanes differ in the sounds"
        );
        assert!(
            lanes
                .iter()
                .all(|l| !l.config.contains("cursor_trail_style")),
            "the ON lanes run the shipped default style, not a respelling of it"
        );
        assert!(lanes.iter().all(|l| !l.config.contains('[')));
    }

    /// The lane verdict reads the `cursor_effect` owner out of the ledger by
    /// NAME and the ribbon witness by name; passes a healthy lane; fails both an
    /// absolute and a share late-arm regression, an ON lane whose effects never
    /// engaged (the first cut read an absent owner as `(0, 0)` and PASSED it),
    /// and an OFF lane that composed ribbon — never a lane whose ledger or
    /// witness is simply missing.
    #[test]
    fn the_effect_lane_verdict_demands_engagement_and_bounds_late_arms() {
        let [off, on, _] = effect_lanes();
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
        assert!(effect_lane_verdict(&on, &reply("cursor_effect:430/0", 30)).is_ok());
        assert!(
            effect_lane_verdict(&on, &reply("cursor_effect:430/2", 30)).is_ok(),
            "the calibrated healthy worst"
        );
        assert!(effect_lane_verdict(&on, &reply("cursor_effect:430/4", 30)).is_ok());
        assert!(
            effect_lane_verdict(&on, &reply("cursor_effect:430/5", 30)).is_err(),
            "over the absolute bound"
        );
        assert!(
            effect_lane_verdict(&on, &reply("cursor_effect:80/2", 30)).is_err(),
            "over one in fifty"
        );
        assert!(effect_lane_verdict(&on, &reply("cursor_effect:40/1", 30)).is_ok());
        assert!(
            effect_lane_verdict(&on, &reply("session_status:12/0", 30)).is_err(),
            "an ON lane that armed no cursor_effect deadline measured nothing"
        );
        assert!(
            effect_lane_verdict(&on, &reply("cursor_effect:80/0", 0)).is_err(),
            "an ON lane that composed no ribbon measured nothing"
        );
        assert!(effect_lane_verdict(&off, &reply("session_status:12/0", 0)).is_ok());
        assert!(
            effect_lane_verdict(&off, &reply("session_status:12/0", 3)).is_err(),
            "an OFF lane that composed ribbon is not off"
        );
        assert!(
            effect_lane_verdict(&on, "OK frames=40 fx_under_frames=3").is_err(),
            "no ledger is not a pass"
        );
        assert!(
            effect_lane_verdict(&on, "OK frames=40 deadline_arms_by_owner=cursor_effect:9/0")
                .is_err(),
            "no ribbon witness is not a pass"
        );
    }

    /// The comparison reads each lane's five readings by name, from the reply
    /// that publishes it, and prints every lane that measured.
    #[test]
    fn the_lane_comparison_reports_every_lane_s_readings() {
        let summary = "OK frames=40 input_p95_ms=6.10 present_p95_ms=4.20 \
                       deadline_arms_by_owner=cursor_effect:80/0 max_frame_gap_ms=33.30";
        let percentiles = "OK n_key_write=30 key_write_p99_ms=2.50";
        let got = lane_latency(summary, percentiles);
        assert_eq!(
            got.iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>(),
            [
                "input_p95=6.10",
                "present_p95=4.20",
                "key_write_p99=2.50",
                "max_frame_gap=33.30",
                "deadline_arms_by_owner=cursor_effect:80/0"
            ]
        );
        assert_eq!(lane_latency("OK", "OK")[0].1, "?", "absent reads as `?`");
        let line = lane_comparison(&[("effects=off", got.clone()), ("effects=on", got)]);
        assert!(line.contains("[effects=off] input_p95=6.10"), "{line}");
        assert!(line.contains("[effects=on] input_p95=6.10"), "{line}");
    }

    #[test]
    fn a_40ms_press_path_regression_fails_the_hardware_gate() {
        // The finding's scenario: 40 ms of new UI-thread work on every keystroke.
        // The echo still presents in ~58 ms, under the 250 ms input→present
        // ceilings; the burst books every key's key→write, and that fails.
        let hw = percentiles(30, "46.13", "0.10", "5.24", "58.00");
        let bad = hv(&hw).expect_err("a finding");
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
        let ok = hv(&hw).expect("a pass");
        assert_eq!(
            ok,
            "gui smoke: frames=41 max_input_present=8ms, no present retries, drops or \
             sync timeouts; hardware keys n_key_write=30/30 key_write_p99=6.29ms \
             max_term_wait_press=0.10ms acquire_p99=5.24ms (n=31) input_p99=14.20ms \
             (n=30) present_glass_p99=8.90ms (n=31, 0 never shown); \
             echo_p99=150.04ms is the child's round trip (reported, not gated)"
        );
    }

    #[test]
    fn a_burst_that_never_reached_the_keyboard_arm_is_a_failure_not_a_pass() {
        // Exactly what a `ctl key`-driven burst reports: every slice healthy, and
        // `n_key_write=0` because nothing armed the key-arrival stamp.
        let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
        let bad = hv(&controller_shaped).expect_err("a finding");
        assert!(
            bad.contains("never reached the KeyboardInput arm — n_key_write=0 of posted=30 (< 15)"),
            "{bad}"
        );
        assert!(hv(&percentiles(14, "1.00", "0.0", "1.0", "9.0")).is_err());
        assert!(hv(&percentiles(15, "1.00", "0.0", "1.0", "9.0")).is_ok());
    }

    /// THE INPUT CEILINGS CANNOT PASS VACUOUSLY: a burst that booked no
    /// input→present sample reads `max 0 ms` and `p99 0 ms`, under both bars.
    #[test]
    fn a_burst_that_booked_no_input_samples_is_a_failure_not_a_pass() {
        let hw = |n: u64| {
            percentiles(30, "6.00", "0.10", "1.00", "0.00")
                .replace("n_input=30", &format!("n_input={n}"))
        };
        let bad = hardware_key_verdict(30, &summary(41, "0.00", 0, 0, 0), &hw(0))
            .expect_err("no samples");
        assert!(
            bad.contains("n_input=0 input→present samples of posted=30 (< 15)"),
            "{bad}"
        );
        assert!(hv(&hw(14)).is_err());
        assert!(
            hv(&hw(15)).is_ok(),
            "half the burst is the floor, inclusive"
        );
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
            let bad = hv(&without(&healthy, field)).expect_err(field);
            assert!(bad.contains(&format!("published no {field}")), "{bad}");
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
    fn the_hardware_slice_ceilings_are_exact_and_separately_attributed() {
        let v =
            |kw: &str, press: &str, acq: &str, inp: &str| hv(&percentiles(30, kw, press, acq, inp));
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
        let acq = hv(&with_max(
            &percentiles(30, "6.00", "0.10", "56.00", "10.00"),
            "max_acquire_wait_ms",
            "55.20",
        ))
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
        // This slice publishes no max of its own here (the summary's
        // `max_input_present_ms` is gated separately, and there `>=` is right because
        // that IS a true maximum), so the bar rests on the edge alone: one step above
        // it fails.
        let inp = v("6.00", "0.10", "1.00", "260.00").expect_err("input→present");
        assert!(
            inp.contains("hardware input→present — p99 260ms (> 250)"),
            "{inp}"
        );

        // A CPU backend books no acquires; an absent slice is not a slow one.
        let cpu =
            percentiles(30, "6.00", "0.10", "0.00", "10.00").replace("n_acquire=31", "n_acquire=0");
        assert!(hv(&cpu).is_ok());
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
        let bad = hv(&held).expect_err("a finding");
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
            hv(&with_glass(31, "49.90")).is_ok(),
            "49 ms present->glass passes"
        );
        // AN EDGE AT THE BAR IS NOT A CROSSING. The reported p99 is a bucket's
        // EXCLUSIVE upper edge, so 50.00 against a 50 ms bar says every sample was
        // under it — the run the 2026-09-18 gate refused reported exactly this beside
        // `max 47.85ms`, a percentile above its own maximum.
        assert!(
            hv(&with_glass(31, "50.00")).is_ok(),
            "an exclusive edge AT the bar means every sample was under it"
        );
        // Above the bar, with the max confirming: the finding this row exists for.
        assert!(
            hv(&with_max(
                &with_glass(31, "56.00"),
                "max_present_glass_ms",
                "55.10"
            ))
            .is_err(),
            "a p99 above the bar whose max confirms it fails"
        );
        // Above the bar with a max that does not reach it: the bucket-edge artifact,
        // which is a pass. A 12.40 ms worst frame is not a compositor holding frames.
        assert!(
            hv(&with_glass(31, "56.00")).is_ok(),
            "an edge above the bar that no sample reached is not a slow leg"
        );
        // No sink, no handler: a CPU backend and every non-macOS present book none,
        // and an absent slice must never be read as a slow one.
        assert!(
            hv(&with_glass(0, "900.00")).is_ok(),
            "an unsampled compositor leg is not a slow one"
        );
    }

    #[test]
    fn an_unparsable_hardware_reply_fails_closed() {
        let bad = hv("").expect_err("no reply");
        assert_eq!(
            bad,
            "gui smoke: could not parse hardware-key percentiles -> <no reply>"
        );
        // The summary line (what `metrics` answers) lacks the slices: never a pass.
        let summary =
            "OK frames=41 max_input_present_ms=8.100 last_key_write_ms=0.00 max_key_write_ms=0.00 ";
        assert!(hv(summary).is_err());
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
        // The summary the verdict (and the headless smoke's `wake_heals`) reads.
        for spelled in [
            "scrollback_truncated_lines={truncated} frames={}",
            "max_input_present_ms={:.2}",
            "sync_armed={} sync_rel_end={} sync_rel_timeout={} sync_holding={}",
            "perf_reduced={} shed_transitions={} wake_heals={}",
            "redraw_retry_gated={}",
            "present_drops={}",
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
        // The reply the headless smoke's `cursor` round trip parses.
        assert!(query.contains("format!(\"OK {} {} {} {}\\n\", c.row, c.col, vis, style)"));
    }

    #[test]
    fn only_the_documented_cursor_reply_passes_the_round_trip() {
        assert!(is_cursor_reply("OK 0 0 1 blinking_block"));
        assert!(is_cursor_reply("OK 12 40 0 steady_bar"));
        for bad in [
            "",
            "OK",
            "ERR unknown verb",
            // The stand-in's old answer, which the old `OK *` check passed.
            "OK row=0 col=0",
            "OK 0 0 2 blinking_block",
            "OK -1 0 1 blinking_block",
            "OK 0 0 1",
            "OK 0 0 1 blinking_block extra",
            // A banner ahead of the reply is not the reply.
            "aterm: note\nOK 0 0 1 blinking_block",
        ] {
            assert!(!is_cursor_reply(bad), "{bad:?} passed for a cursor reply");
        }
    }

    #[test]
    fn an_empty_reply_is_reported_as_no_reply_never_as_a_pass() {
        assert_eq!(or_no_reply(""), "<no reply>");
        assert_eq!(or_no_reply("OK x"), "OK x");
        assert!(!glob_match(OK_REPLY, ""));
    }

    #[test]
    fn the_gui_smoke_skips_honestly_when_the_machine_cannot_present() {
        let mut c = ctx();
        c.skip_gui_smoke = true;
        assert_eq!(
            gui_smoke_unavailable(&c).as_deref(),
            Some("gui smoke (--skip-gui-smoke)")
        );

        // …and without the flag it is not the opt-out.
        c.skip_gui_smoke = false;
        assert_ne!(
            gui_smoke_unavailable(&c).as_deref(),
            Some("gui smoke (--skip-gui-smoke)")
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
        // Its own exit, witnessed before teardown's TERM — a 50 ms nap let a
        // starved `sh` be killed first, a death teardown DID cause.
        let pid = sb.child.as_ref().expect("child").id();
        assert!(
            crate::smoke::exited_on_its_own(pid),
            "`exit 7` never exited within 30 s"
        );
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
}
