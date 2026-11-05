// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GUI typing-pacing smoke: the `measure` check's live window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::Ctx;
use crate::exec::{Cmd, StageGroupEnd, capture_reply};
use crate::ladder::Report;
use crate::smoke::{
    SmokeChild, is_socket_or_symlink, metric_ms_whole, metric_u64, smoke_log_tail, socket_listening,
};

fn ok_reply(reply: &str) -> bool {
    reply.starts_with("OK ")
}

/// The `aterm.toml` lines the smoke's instance runs with (top-level keys, ahead of the sandbox's
/// first `[table]`).
pub const SMOKE_CONFIG: &str = "cursor_trail_style = \"rainbow kitty pet\"\ncursor_momentum_glow = true\ntrail_sounds = true\n";

const CURSOR_EFFECT_LATE_MAX: u64 = 4;
const CURSOR_EFFECT_LATE_SHARE_DEN: u64 = 50;

/// The `cursor_effect` owner's `arms/past` out of a `metrics` reply's
/// `deadline_arms_by_owner=owner:arms/past,...` ledger.
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
        if !arms.bytes().all(|b| b.is_ascii_digit()) || !past.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some((arms.parse().ok()?, past.parse().ok()?));
    }
    Some((0, 0))
}

/// The burst's verdict on its post-burst `metrics` summary.
/// # Errors
/// A summary missing either reading, effects that never engaged, or too many late arms.
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

const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);
const SETTLE: Duration = Duration::from_secs(1);

const INPUT_PRESENT_CEILING_MS: u64 = 250;
const FRAMES_FLOOR: u64 = 15;
const HW_KEY_WRITE_FLOOR: u64 = 15;
const HW_INPUT_FLOOR: u64 = 15;
const KEY_WRITE_P99_CEILING_MS: u64 = 40;
const TERM_WAIT_PRESS_MAX_CEILING_MS: u64 = 25;
const ACQUIRE_P99_CEILING_MS: u64 = 50;
const PRESENT_GLASS_P99_CEILING_MS: u64 = 50;
/// The socket-listen budget.
pub const SOCKET_POLLS: usize = 600;
/// The stage the instance's process group is recorded and swept under in the run's record.
const GROUP_LABEL: &str = "gui smoke";
/// The pause between two looks of the smoke's waits.
pub const POLL_GAP: Duration = Duration::from_millis(100);

struct Sandbox {
    tmp: PathBuf,
    rundir: PathBuf,
    cfgdir: PathBuf,
    /// The scratch world's marker value ([`aterm_types::world::state_home`] of its HOME).
    state: PathBuf,
    gui_log: PathBuf,
    child: Option<SmokeChild>,
}

impl Sandbox {
    /// A scratch world whose `aterm.toml` is `extra`, the smoke's root keys and a scratch world's
    /// lanes off ([`aterm_types::world::LANES_OFF`]): the handed bundle's window would otherwise
    /// browse the local network (macOS's consent prompt on the desk), submit that bundle's keeper
    /// job into the login's launchd domain, and run its update and package lanes.
    fn with_config(tag: &str, extra: &str) -> Option<Self> {
        let tmp = crate::mktemp_dir(tag).ok()?;
        std::fs::create_dir(tmp.join("home")).ok()?;
        let state = aterm_types::world::state_home(&tmp.join("home"))?;
        let rundir = tmp.join("run");
        std::fs::create_dir_all(rundir.join("aterm")).ok()?;
        chmod_700(&rundir)?;
        chmod_700(&rundir.join("aterm"))?;
        let cfgdir = tmp.join("cfg");
        std::fs::create_dir_all(cfgdir.join("aterm")).ok()?;
        chmod_700(&cfgdir)?;
        crate::write_fresh(
            cfgdir.join("aterm/aterm.toml"),
            format!(
                "{extra}agents_auto_prime = false\nbell_sound = false\n{}",
                aterm_types::world::LANES_OFF
            ),
        )
        .ok()?;
        let gui_log = tmp.join("gui.log");
        Some(Self {
            tmp,
            rundir,
            cfgdir,
            state,
            gui_log,
            child: None,
        })
    }

    fn sock(&self) -> PathBuf {
        self.rundir.join("aterm/aterm.sock")
    }

    /// The command that starts the bundle's window `gui` in this world: the run's own
    /// environment without its `ATERM_*` variables ([`gui_env`]), then this world's HOME, folders
    /// and marker ([`aterm_types::world::MARKER`], set after the strip so it is the world's), and
    /// its control socket.
    fn gui_command(&self, ctx: &Ctx, gui: &Path) -> Command {
        let mut cmd = Command::new(gui);
        gui_env(ctx, &mut cmd);
        cmd.current_dir(&ctx.root)
            .env("PATH", &ctx.path_env)
            .env("HOME", self.tmp.join("home"))
            .env("XDG_RUNTIME_DIR", &self.rundir)
            .env("XDG_CONFIG_HOME", &self.cfgdir)
            .env("SHELL", "/bin/sh")
            .env(aterm_types::world::MARKER, &self.state);
        cmd.arg("--control-sock").arg(self.sock());
        cmd
    }

    /// End the instance with its group ([`SmokeChild::retire`]); `groups` sweeps the group
    /// before its leader is reaped and records it once it is.
    fn teardown(&mut self, r: &mut Report, groups: Option<&dyn StageGroupEnd>) {
        if let Some(child) = self.child.take() {
            let ok = child.retire(
                |pgid| {
                    if let Some(groups) = groups {
                        groups.group_ended(pgid, GROUP_LABEL);
                    }
                },
                |pgid| {
                    if let Some(groups) = groups {
                        groups.group_reaped(pgid, GROUP_LABEL);
                    }
                },
            );
            if !ok {
                r.fail("smoke: child cleanup/reap failed");
            }
        }
        std::fs::remove_dir_all(&self.tmp).ok();
    }
}

fn chmod_700(path: &Path) -> Option<()> {
    crate::set_dir_mode_nofollow(path, 0o700).ok()
}

enum Ready {
    /// Both binaries exist, the process is up and the socket is bound.
    Up { ctl: PathBuf },
    /// Already reported; the stage is over.
    Stopped,
}

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
    let log = match crate::create_stdio_file(&sb.gui_log) {
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
    let mut cmd = sb.gui_command(ctx, &gui);
    cmd.stdout(log).stderr(log2);
    let groups = ctx.exec_env().stage_groups;
    let made = |pgid| {
        if let Some(groups) = groups {
            groups.group_made(pgid, GROUP_LABEL);
        }
    };
    match SmokeChild::spawn(&mut cmd, made) {
        Ok(c) => sb.child = Some(c),
        Err(e) => {
            r.cannot_run(format!("{tag}: cannot launch aterm-gui ({e})"));
            return Ready::Stopped;
        }
    }

    let sock = sb.sock();
    let up = || is_socket_or_symlink(&sock) && socket_listening(&sock);
    match wait_to_listen(POLL_GAP, || child_exited(sb), up) {
        Listen::Up => return Ready::Up { ctl },
        Listen::Exited => r.fail(format!("{tag}: aterm-gui exited early")),
        Listen::Never => r.fail(format!("{tag}: control socket never started listening")),
    }
    r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
    Ready::Stopped
}

fn gui_env(ctx: &Ctx, cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(name);
        }
    }
    for (name, value) in &ctx.child_env_add {
        if name == crate::runs::VERIFY_RUN_ENV {
            cmd.env(name, value);
        }
    }
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
    sb.child.as_mut().is_none_or(SmokeChild::exited)
}

fn ctl(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) -> String {
    let cmd = Cmd::new(ctl_bin)
        .arg("--sock")
        .arg(sb.sock())
        .args(args.iter().copied())
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir);
    capture_reply(&cmd, ctx.exec_env())
}

const fn or_no_reply(got: &str) -> &str {
    if got.is_empty() { "<no reply>" } else { got }
}

pub fn gui_typing_smoke(ctx: &Ctx, r: &mut Report) {
    if let Some(reason) = gui_smoke_unavailable(ctx) {
        r.skip(reason);
        return;
    }
    let Some(bundle) = &ctx.measure_artifact else {
        r.skip(format!("gui smoke {}", crate::stages::NO_APP));
        return;
    };
    let Some(mut sb) = Sandbox::with_config("atl", SMOKE_CONFIG) else {
        r.cannot_run("gui smoke: mktemp");
        return;
    };
    if let Ready::Up { ctl: ctl_bin } = bring_up(ctx, r, &mut sb, bundle) {
        let subject = crate::stages::Subject::of(ctx, bundle);
        typing_burst(ctx, r, &mut sb, &ctl_bin);
        if !ctx.cursor_pacing {
            aos_release_step(ctx, r, &mut sb, &ctl_bin, &subject);
            settings_hardware_key_step(ctx, r, &mut sb, &ctl_bin, &subject);
        }
    }
    sb.teardown(r, ctx.exec_env().stage_groups);
}

/// The server's answer in what the bundle's `aterm-ctl` printed: a refusal goes to stderr
/// behind [`CTL_STDERR`].
fn server_reply(printed: &str) -> &str {
    let printed = printed.trim_end();
    printed.strip_prefix(CTL_STDERR).unwrap_or(printed)
}

/// Whether `reply` refuses `verb` itself: the subject does not know it, or predates its release
/// form and answers it only in a debug build.
fn refuses_verb(reply: &str, verb: &str) -> bool {
    let reply = server_reply(reply);
    reply.starts_with("ERR unknown verb") || reply == format!("ERR {verb} needs a debug build")
}

/// What a release build answers `aos bundle` with.
pub const AOS_BUNDLE_REFUSAL: &str = "ERR aos bundle needs a debug build";

/// What `aterm ctl` puts before a reply it writes to stderr.
const CTL_STDERR: &str = "aterm-ctl: ";

/// The bundle's `aos` verb attaches and detaches the front window's glow, and refuses only the debug forms (design §14.5 R0 exit item 8).
fn aos_release_step(
    ctx: &Ctx,
    r: &mut Report,
    sb: &mut Sandbox,
    ctl_bin: &Path,
    subject: &crate::stages::Subject,
) {
    let tag = "gui smoke: release aos";
    let Some(pid) = sb.child.as_ref().map(SmokeChild::id) else {
        r.cannot_run(format!("{tag}: no child to drive"));
        return;
    };
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip(format!("{tag} (could not keep the test window frontmost)"));
        return;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let status = ctl(ctx, sb, ctl_bin, &["aos", "status"]);
        if ok_reply(&status) {
            break;
        }
        if refuses_verb(&status, "aos") {
            subject.unanswered(
                r,
                None,
                tag,
                &[format!("aos status ({})", status.trim_end())],
            );
            return;
        }
        if std::time::Instant::now() >= deadline || child_exited(sb) {
            r.fail(format!(
                "{tag}: aos status never answered OK -> {}",
                or_no_reply(&status)
            ));
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
        std::thread::sleep(POLL_GAP);
    }
    let attach = ctl(ctx, sb, ctl_bin, &["aos", "attach"]);
    let detach = ctl(ctx, sb, ctl_bin, &["aos", "detach"]);
    let bundle = ctl(ctx, sb, ctl_bin, &["aos", "bundle"]);
    match aos_release_verdict(&attach, &detach, &bundle) {
        Ok(line) => r.pass(line),
        Err(bad) => r.fail(bad),
    }
}

/// The release `aos` step's verdict on its `attach`, `detach` and `bundle` replies.
/// # Errors
/// An attach that is not `OK` hosting the shipped code, a detach that is not `OK`, or a bundle answer other than [`AOS_BUNDLE_REFUSAL`] (a debug build answers the form).
/// `aterm ctl` writes an `ERR` reply to stderr after [`CTL_STDERR`], and the step reads both streams.
pub fn aos_release_verdict(attach: &str, detach: &str, bundle: &str) -> Result<String, String> {
    if !(ok_reply(attach) && attach.contains(" attached=true code=shipped ")) {
        return Err(format!("gui smoke: aos attach -> {}", or_no_reply(attach)));
    }
    if !ok_reply(detach) {
        return Err(format!("gui smoke: aos detach -> {}", or_no_reply(detach)));
    }
    if server_reply(bundle) != AOS_BUNDLE_REFUSAL {
        return Err(format!(
            "gui smoke: aos bundle -> {} (a release build answers {AOS_BUNDLE_REFUSAL:?})",
            or_no_reply(bundle)
        ));
    }
    Ok("the release aos verb attached and detached the glow and refused bundle by name".into())
}

fn typing_burst(ctx: &Ctx, r: &mut Report, sb: &mut Sandbox, ctl_bin: &Path) {
    let Some(pid) = sb.child.as_ref().map(SmokeChild::id) else {
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
    guarded_burst(
        r,
        burst_timing_entry,
        |timing| {
            burst_target_ready(
                || !child_exited(sb),
                || crate::smoke::activate_macos_gui_pid(pid),
                || check_burst_timing(timing).map(drop),
            )?;
            let got = ctl(ctx, sb, ctl_bin, &["metrics", "reset"]);
            if !ok_reply(&got) {
                return Err(BurstError::Failed(format!(
                    "gui smoke: metrics reset -> {}",
                    or_no_reply(&got)
                )));
            }
            let args = hwkey_burst_args();
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let got = ctl(ctx, sb, ctl_bin, &argv);
            let Some(posted) = metric_u64(&got, "posted").filter(|_| ok_reply(&got)) else {
                return Err(BurstError::Failed(format!(
                    "gui smoke: hardware key injection -> {}",
                    or_no_reply(&got)
                )));
            };
            std::thread::sleep(SETTLE);
            Ok(BurstReadings {
                posted,
                summary: ctl(ctx, sb, ctl_bin, &["metrics"]),
                percentiles: ctl(ctx, sb, ctl_bin, &["metrics", "percentiles"]),
            })
        },
        check_burst_timing,
    );
}

struct BurstTiming {
    outer: Arc<aterm_capacity::Claim>,
    entry: aterm_capacity::Claim,
    /// Why the burst runs without a qualified quiet admission: stated, never a finding.
    unqualified: Vec<aterm_capacity::Unqualified>,
}

/// The burst's measured entry, under the stage's claim: on a machine that stayed busy through
/// its patience it runs loaded and says so (owner ruling 2026-11-05).
fn burst_timing_entry() -> Result<(BurstTiming, String), String> {
    let outer = crate::current_quiet_claim().ok_or("gui burst has no stage timing reservation")?;
    let mut unqualified = Vec::new();
    lapsed_or(outer.timing_protection(), &mut unqualified)
        .map_err(|why| format!("outer timing hold: {why}"))?;
    let mut entry = crate::admit_live(
        aterm_capacity::Need::timing("gui cursor-pacing burst entry").runs_loaded(),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|why| format!("burst timing admission: {why}"))?;
    let stated = entry
        .stated_entry()
        .map_err(|why| format!("burst measurement entry: {why}"))?;
    unqualified.extend(stated.unqualified);
    lapsed_or(outer.timing_protection(), &mut unqualified)
        .map_err(|why| format!("outer timing hold after entry: {why}"))?;
    let window = stated.window;
    let bounds = |claim: &aterm_capacity::Claim| {
        let admitted = claim.record().timing.admitted_ms;
        let expires = admitted
            .zip(claim.record().need.patience)
            .and_then(|(at, p)| at.checked_add(u64::try_from(p.hold.as_millis()).ok()?));
        (admitted, expires)
    };
    let (admitted, expires) = bounds(&entry);
    let (outer_admitted, outer_expires) = bounds(&outer);
    let note = format!(
        "timing phase=gui_burst_entry provider=sysprobe_core_ticks scope=sampled_global_cpu_window;not_continuous_quiet observed_ms={} interval_ns={} cores={} busy_ticks={} total_ticks={} entry_load1={:?} actual_admitted_ms={admitted:?} expires_ms={expires:?} outer_actual_admitted_ms={outer_admitted:?} outer_expires_ms={outer_expires:?} qualification={}",
        window.observed_ms,
        window.interval_ns,
        window.cores,
        window.busy_ticks,
        window.total_ticks,
        entry.record().timing.entry_load1,
        stated_qualification(&unqualified),
    );
    Ok((
        BurstTiming {
            outer,
            entry,
            unqualified,
        },
        note,
    ))
}

/// `qualified`, or why it ran unqualified.
fn stated_qualification(unqualified: &[aterm_capacity::Unqualified]) -> String {
    if unqualified.is_empty() {
        return "qualified".to_owned();
    }
    let why: Vec<String> = unqualified.iter().map(ToString::to_string).collect();
    format!("unqualified ({})", why.join("; "))
}

/// A timing hold that ended is stated ([`aterm_capacity::Unqualified::Lapsed`]); any other
/// failure is not.
fn lapsed_or(
    protection: Result<aterm_capacity::Protection, aterm_capacity::TimingIssue>,
    unqualified: &mut Vec<aterm_capacity::Unqualified>,
) -> Result<(), aterm_capacity::TimingIssue> {
    match protection {
        Ok(_) => Ok(()),
        Err(aterm_capacity::TimingIssue::Expired) => {
            if !unqualified.contains(&aterm_capacity::Unqualified::Lapsed) {
                unqualified.push(aterm_capacity::Unqualified::Lapsed);
            }
            Ok(())
        }
        Err(why) => Err(why),
    }
}

/// Why the collected burst is unqualified, or why its protection failed.
fn check_burst_timing(timing: &BurstTiming) -> Result<Vec<String>, String> {
    let mut unqualified = timing.unqualified.clone();
    lapsed_or(timing.outer.timing_protection(), &mut unqualified)
        .map_err(|why| format!("outer timing hold: {why}"))?;
    lapsed_or(timing.entry.timing_protection(), &mut unqualified)
        .map_err(|why| format!("burst timing hold: {why}"))?;
    Ok(unqualified.iter().map(ToString::to_string).collect())
}

struct BurstReadings {
    posted: u64,
    summary: String,
    percentiles: String,
}

enum BurstError {
    Failed(String),
    Skipped(&'static str),
    Unavailable(String),
}

fn burst_target_ready(
    mut alive: impl FnMut() -> bool,
    activate: impl FnOnce() -> bool,
    protected: impl FnOnce() -> Result<(), String>,
) -> Result<(), BurstError> {
    if !alive() {
        return Err(BurstError::Failed(
            "gui smoke: aterm-gui exited before the burst".into(),
        ));
    }
    if !activate() {
        return Err(BurstError::Skipped(
            "gui smoke (could not keep the test window frontmost)",
        ));
    }
    if !alive() {
        return Err(BurstError::Failed(
            "gui smoke: aterm-gui exited during activation".into(),
        ));
    }
    protected().map_err(BurstError::Unavailable)
}

const METRICS_RECEIPT_MAX: usize = 64 * 1024;

fn bounded_burst_readings(readings: &BurstReadings) -> Result<(), String> {
    for (name, text) in [
        ("summary", &readings.summary),
        ("percentiles", &readings.percentiles),
    ] {
        if text.len() > METRICS_RECEIPT_MAX || !ok_reply(text) || text.lines().count() != 1 {
            return Err(format!(
                "gui smoke: invalid {name} receipt ({} bytes; maximum {METRICS_RECEIPT_MAX})",
                text.len()
            ));
        }
    }
    Ok(())
}

fn retain_burst_readings(r: &mut Report, readings: &BurstReadings) {
    r.raw(format!("gui smoke metrics summary: {}", readings.summary));
    r.raw(format!(
        "gui smoke metrics percentiles: {}",
        readings.percentiles
    ));
}

fn guarded_burst<G>(
    r: &mut Report,
    enter: impl FnOnce() -> Result<(G, String), String>,
    collect: impl FnOnce(&G) -> Result<BurstReadings, BurstError>,
    protected: impl FnOnce(&G) -> Result<Vec<String>, String>,
) {
    let (guard, note) = match enter() {
        Ok(entered) => entered,
        Err(why) => {
            r.cannot_run(format!(
                "gui smoke: timing entry unavailable ({why}); no reset or keys"
            ));
            return;
        }
    };
    r.raw(note);
    let readings = match collect(&guard) {
        Ok(readings) => readings,
        Err(BurstError::Failed(why)) => {
            r.fail(why);
            return;
        }
        Err(BurstError::Skipped(why)) => {
            r.skip(why);
            return;
        }
        Err(BurstError::Unavailable(why)) => {
            r.cannot_run(why);
            return;
        }
    };
    let bounded = bounded_burst_readings(&readings);
    let unqualified = match protected(&guard) {
        Ok(unqualified) => unqualified,
        Err(why) => {
            match bounded {
                Ok(()) => retain_burst_readings(r, &readings),
                Err(why) => r.raw(why),
            }
            r.cannot_run(format!(
                "gui smoke: timing protection unavailable after collection ({why}); result unqualified"
            ));
            return;
        }
    };
    // Stated beside each pass, never a finding: a busy machine never blocks a release (owner
    // ruling 2026-11-05).
    let stated = if unqualified.is_empty() {
        String::new()
    } else {
        format!(
            "; ran without a qualified quiet admission (stated, not a finding): {}",
            unqualified.join("; ")
        )
    };
    if let Err(why) = bounded {
        r.fail(why);
        return;
    }
    let effects = effects_verdict(&readings.summary);
    let hardware = hardware_key_verdict(readings.posted, &readings.summary, &readings.percentiles);
    if effects.is_ok() && hardware.is_ok() {
        retain_burst_readings(r, &readings);
    }
    for verdict in [effects, hardware] {
        match verdict {
            Ok(line) => r.pass(format!("{line}{stated}")),
            Err(why) => r.fail(why),
        }
    }
}

fn settings_hardware_key_step(
    ctx: &Ctx,
    r: &mut Report,
    sb: &mut Sandbox,
    ctl_bin: &Path,
    subject: &crate::stages::Subject,
) {
    let tag = "gui smoke: settings key path";
    let Some(pid) = sb.child.as_ref().map(SmokeChild::id) else {
        r.cannot_run(format!("{tag}: no child to drive"));
        return;
    };
    let got = ctl(
        ctx,
        sb,
        ctl_bin,
        &["open", "app", "settings", "/appearance"],
    );
    if refuses_verb(&got, "open") {
        subject.unanswered(
            r,
            None,
            tag,
            &[format!("open app settings ({})", got.trim_end())],
        );
        return;
    }
    if !ok_reply(&got) {
        r.fail(format!("{tag}: open app settings -> {}", or_no_reply(&got)));
        return;
    }
    if !crate::smoke::activate_macos_gui_pid(pid) {
        r.skip(format!("{tag} (could not keep the test window frontmost)"));
        return;
    }
    let tabs = ctl(ctx, sb, ctl_bin, &["inspect", "app/v1", "tabs"]);
    let Some(view) = tabs
        .lines()
        .find(|line| line.contains("app=settings"))
        .and_then(|line| {
            let v = reply_field(line, "view");
            v.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| v.parse::<u64>().ok())
                .flatten()
        })
    else {
        r.fail(format!("{tag}: no Settings view in {}", or_no_reply(&tabs)));
        return;
    };
    let view = view.to_string();
    let focused = |sb: &Sandbox| -> Option<String> {
        ctl(
            ctx,
            sb,
            ctl_bin,
            &["inspect", "app/v1", "view", &view, "controls"],
        )
        .lines()
        .find(|line| line.contains("focused:true"))
        .map(|line| reply_field(line, "key").trim_matches('"').to_string())
    };
    let before = focused(sb);
    let got = ctl(ctx, sb, ctl_bin, &["hwkey", "tab"]);
    if refuses_verb(&got, "hwkey") {
        subject.unanswered(r, None, tag, &[format!("hwkey tab ({})", got.trim_end())]);
        return;
    }
    if !ok_reply(&got) || metric_u64(&got, "posted") != Some(1) {
        r.fail(format!("{tag}: hwkey tab -> {}", or_no_reply(&got)));
        return;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let now = focused(sb);
        if now.is_some() && now != before {
            r.pass(format!(
                "a real Tab moved Settings' focus ({} -> {})",
                before.as_deref().unwrap_or("-"),
                now.as_deref().unwrap_or("-")
            ));
            return;
        }
        if std::time::Instant::now() >= deadline || child_exited(sb) {
            r.fail(format!(
                "{tag}: a real Tab never moved Settings' focus (still {})",
                now.as_deref().unwrap_or("-")
            ));
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
        std::thread::sleep(POLL_GAP);
    }
}

fn gui_smoke_unavailable(ctx: &Ctx) -> Option<String> {
    if std::env::consts::OS != "macos" {
        return Some("gui smoke (macOS only)".into());
    }
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

/// `hwkey x count=30 interval=50`.
#[must_use]
pub fn hwkey_burst_args() -> Vec<String> {
    vec![
        "hwkey".into(),
        "x".into(),
        format!("count={BURST_KEYS}"),
        format!("interval={}", BURST_GAP.as_millis()),
    ]
}

fn reply_field<'a>(reply: &'a str, name: &str) -> &'a str {
    let needle = format!(" {name}=");
    reply
        .rfind(needle.as_str())
        .and_then(|at| reply[at + needle.len()..].split_whitespace().next())
        .unwrap_or("?")
}

/// WHAT A REPORTED p99 IS, and why these four bars read `>` and not `>=`.
/// Native-clock availability counters are required, including from older subjects. Both must
/// be zero: dispatch-time fallback samples cannot establish latency including OS queue residence.
/// The measured hardware-write floor is unchanged.
/// # Errors
/// A reply that cannot be parsed, unavailable native clocks, a sampled slice without its maximum,
/// a burst under a floor, or any gated reading at or over its ceiling.
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
    let max_ms = |name: &str| {
        reply_field(reply, name).parse::<f64>().map_err(|_| {
            format!(
                "gui smoke: a sampled slice published no {name}, so nothing can confirm or \
                 refute its p99 [{reply}]"
            )
        })
    };
    let bar = |ms: u64| f64::from(u32::try_from(ms).unwrap_or(u32::MAX));
    let (Some(queue_unavailable), Some(write_unavailable)) = (
        metric_u64(reply, "key_queue_unavailable"),
        metric_u64(reply, "key_write_unavailable"),
    ) else {
        return Err(format!(
            "gui smoke: hardware clock availability is unreported or invalid; the subject \
             must publish key_queue_unavailable and key_write_unavailable to validate \
             hardware latency [{reply}]"
        ));
    };
    if queue_unavailable != 0 || write_unavailable != 0 {
        return Err(format!(
            "gui smoke: hardware latency is unconfirmed because native clock samples are \
             unavailable; input_clock=unconfirmed key_queue_unavailable={queue_unavailable} \
             key_write_unavailable={write_unavailable} n_key_write={n_key_write} \
             posted={posted} n_input={n_input} [{reply}]"
        ));
    }
    if n_key_write < HW_KEY_WRITE_FLOOR {
        return Err(format!(
            "gui smoke: insufficient measured hardware key→write samples — \
             n_key_write={n_key_write} of posted={posted} (< {HW_KEY_WRITE_FLOOR}); \
             native queue age unavailable for {queue_unavailable} presses and \
             {write_unavailable} write completions; n_input={n_input} [{reply}]"
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
             input_clock=native [{reply}]"
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
         input_p99={}ms (n={n_input}) input_clock=native key_queue_unavailable={queue_unavailable} \
         key_write_unavailable={write_unavailable} present_glass_p99={}ms (n={n_present_glass}, {} \
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
pub(crate) mod tests {
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

    fn summary(frames: u64, max_input: &str, retries: u64, drops: u64, sync: u64) -> String {
        format!(
            "OK backend=gpu rows=24 cols=80 scrollback_truncated_lines=0 frames={frames} \
             last_input_present_ms=6.10 max_input_present_ms={max_input} n_input=30 \
             sync_armed=0 sync_rel_end=0 sync_rel_timeout={sync} sync_holding=0 \
             perf_reduced=0 shed_transitions=0 wake_heals=0 redraw_retry_gated={retries} \
             fx_under_frames=0 present_drops={drops} deadline_arms_by_owner=cursor_effect:0/0 "
        )
    }

    fn hv(reply: &str) -> Result<String, String> {
        hardware_key_verdict(30, &summary(41, "8.90", 0, 0, 0), reply)
    }

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
             key_queue_unavailable=0 key_write_unavailable=0 \
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

    fn with_max(reply: &str, field: &str, value: &str) -> String {
        let at = reply
            .find(&format!("{field}="))
            .expect("the field is in the reply");
        let from = at + field.len() + 1;
        let end = from + reply[from..].find(' ').expect("a field ends in a space");
        format!("{}{value}{}", &reply[..from], &reply[end..])
    }

    fn with_glass(n: u64, p99: &str) -> String {
        percentiles(30, "6.00", "0.10", "1.00", "10.00")
            .replace("n_present_glass=31", &format!("n_present_glass={n}"))
            .replace(
                "present_glass_p99_ms=8.90",
                &format!("present_glass_p99_ms={p99}"),
            )
    }

    fn burst_readings() -> BurstReadings {
        BurstReadings {
            posted: 30,
            summary: summary(40, "10.00", 0, 0, 0)
                .replace("fx_under_frames=0", "fx_under_frames=20")
                .replace("cursor_effect:0/0", "cursor_effect:100/0"),
            percentiles: percentiles(30, "6.00", "0.10", "1.00", "10.00"),
        }
    }

    struct BurstDrop<'a>(&'a std::cell::Cell<bool>);
    impl Drop for BurstDrop<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn burst_entry_refusal_starts_no_reset_or_keys() {
        let mut r = Report::new("burst");
        guarded_burst::<()>(
            &mut r,
            || Err("entry unavailable".into()),
            |_| panic!("collection before entry"),
            |_| panic!("postcheck without entry"),
        );
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::ladder::Outcome::Fail(
                crate::ladder::Severity::CouldNotRun
            )]
        );
    }

    #[test]
    fn burst_foreground_and_post_activation_guards_suppress_input() {
        for scenario in 0..4 {
            let mut r = Report::new("burst");
            let dropped = std::cell::Cell::new(false);
            let alive_calls = std::cell::Cell::new(0);
            guarded_burst(
                &mut r,
                || Ok((BurstDrop(&dropped), String::new())),
                |_| {
                    burst_target_ready(
                        || {
                            let n = alive_calls.get();
                            alive_calls.set(n + 1);
                            scenario != 0 && !(scenario == 2 && n == 1)
                        },
                        || scenario != 1,
                        || Err("outer hold expired during activation".into()),
                    )?;
                    panic!("reset/keys after unavailable target");
                },
                |_| panic!("no readings to qualify"),
            );
            assert!(dropped.get());
            assert!(!r.outcomes().any(|(o, _)| o == crate::ladder::Outcome::Ok));
            assert_eq!(alive_calls.get(), if scenario < 2 { 1 } else { 2 });
        }
    }

    #[test]
    fn burst_retains_full_success_metrics_and_guard_until_postcheck() {
        let dropped = std::cell::Cell::new(false);
        let phase = std::cell::Cell::new(0);
        let mut r = Report::new("burst");
        let expected = burst_readings();
        guarded_burst(
            &mut r,
            || {
                phase.set(1);
                Ok((BurstDrop(&dropped), String::new()))
            },
            |_| {
                assert_eq!(phase.replace(2), 1);
                assert!(!dropped.get());
                Ok(burst_readings())
            },
            |_| {
                assert_eq!(phase.replace(3), 2);
                assert!(!dropped.get());
                Ok(Vec::new())
            },
        );
        assert!(dropped.get());
        assert_eq!(phase.get(), 3);
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::ladder::Outcome::Ok, crate::ladder::Outcome::Ok]
        );
        for raw in [expected.summary, expected.percentiles] {
            assert_eq!(
                r.entries
                    .iter()
                    .filter(
                        |e| matches!(e, crate::ladder::Entry::Raw(text) if text.ends_with(raw.trim_end_matches('\n')))
                    )
                    .count(),
                1
            );
        }
    }

    #[test]
    fn burst_post_protection_failure_keeps_readings_but_never_passes() {
        let dropped = std::cell::Cell::new(false);
        let mut r = Report::new("burst");
        guarded_burst(
            &mut r,
            || Ok((BurstDrop(&dropped), String::new())),
            |_| Ok(burst_readings()),
            |_| Err("protection expired".into()),
        );
        assert!(dropped.get());
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::ladder::Outcome::Fail(
                crate::ladder::Severity::CouldNotRun
            )]
        );
        assert_eq!(r.entries.iter().filter(|e| matches!(e, crate::ladder::Entry::Raw(text) if text.starts_with("gui smoke metrics"))).count(), 2);
    }

    /// A BURST THAT RAN WITHOUT A QUALIFIED QUIET ADMISSION PASSES AND SAYS WHY ON EACH PASS: a
    /// busy machine never blocks a release (owner ruling 2026-11-05). NEGATIVE CONTROL: a
    /// qualified burst's passes say nothing of it
    /// ([`burst_retains_full_success_metrics_and_guard_until_postcheck`]).
    #[test]
    fn a_loaded_burst_passes_and_states_why() {
        let mut r = Report::new("burst");
        guarded_burst(
            &mut r,
            || Ok(((), String::new())),
            |_| Ok(burst_readings()),
            |_| {
                Ok(vec![
                    "admitted loaded at its patience bound: CPU 63.0% busy".to_owned(),
                ])
            },
        );
        let passes: Vec<&str> = r
            .outcomes()
            .filter(|(o, _)| *o == crate::ladder::Outcome::Ok)
            .map(|(_, line)| line)
            .collect();
        assert_eq!(passes.len(), 2);
        for line in passes {
            assert!(
                line.contains("ran without a qualified quiet admission (stated, not a finding): admitted loaded"),
                "{line}"
            );
        }
        let mut quiet = Report::new("burst");
        guarded_burst(
            &mut quiet,
            || Ok(((), String::new())),
            |_| Ok(burst_readings()),
            |_| Ok(Vec::new()),
        );
        assert!(
            !quiet
                .outcomes()
                .any(|(_, line)| line.contains("qualified quiet"))
        );
    }

    #[test]
    fn burst_oversized_receipt_cannot_pass_or_be_retained() {
        for percentiles in [false, true] {
            let mut readings = burst_readings();
            let oversized = "x".repeat(METRICS_RECEIPT_MAX + 1);
            if percentiles {
                readings.percentiles = oversized.clone();
            } else {
                readings.summary = oversized.clone();
            }
            let mut r = Report::new("burst");
            guarded_burst(
                &mut r,
                || Ok(((), String::new())),
                |_| Ok(readings),
                |_| Ok(Vec::new()),
            );
            assert_eq!(
                r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
                [crate::ladder::Outcome::Fail(
                    crate::ladder::Severity::GateFailed
                )]
            );
            assert!(!r.entries.iter().any(
                |e| matches!(e, crate::ladder::Entry::Raw(text) if text.contains(&oversized))
            ));
        }
    }

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
        let hw = percentiles(30, "46.13", "0.10", "5.24", "58.00");
        assert!(hv(&hw).is_err());
    }

    #[test]
    fn a_healthy_hardware_burst_passes_and_the_childs_echo_is_never_gated() {
        let hw = percentiles(30, "6.29", "0.10", "5.24", "14.20");
        hv(&hw).expect("a pass");
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
    fn a_burst_without_measured_hardware_writes_is_a_failure_not_a_pass() {
        let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
        assert!(hv(&controller_shaped).is_err());
        let unavailable = controller_shaped
            .replace("key_queue_unavailable=0", "key_queue_unavailable=30")
            .replace("key_write_unavailable=0", "key_write_unavailable=30");
        assert!(hv(&unavailable).is_err());
    }

    #[test]
    fn native_clock_quality_requires_every_observed_press_and_write() {
        let measured = percentiles(30, "6.00", "0.10", "1.00", "10.00");
        let known = hv(&measured).expect("all measured clocks pass");
        assert_eq!(reply_field(&known, "input_clock"), "native");
        let floor = measured.replace("n_key_write=30", "n_key_write=15");
        hv(&floor).expect("the measured-write floor remains sufficient for known clocks");
        for (queue, write) in [(1, 0), (0, 1), (1, 1), (15, 15)] {
            let mixed = floor
                .replace(
                    "key_queue_unavailable=0",
                    &format!("key_queue_unavailable={queue}"),
                )
                .replace(
                    "key_write_unavailable=0",
                    &format!("key_write_unavailable={write}"),
                );
            let fallback =
                hv(&mixed).expect_err("partial clocks cannot confirm whole-burst latency");
            assert_eq!(reply_field(&fallback, "input_clock"), "unconfirmed");
            assert_eq!(
                reply_field(&fallback, "key_queue_unavailable"),
                queue.to_string()
            );
            assert_eq!(
                reply_field(&fallback, "key_write_unavailable"),
                write.to_string()
            );
        }
        let legacy = measured
            .replace("key_queue_unavailable=0 ", "")
            .replace("key_write_unavailable=0 ", "");
        assert!(hv(&legacy).is_err());
        for field in ["key_queue_unavailable", "key_write_unavailable"] {
            for replacement in [String::new(), format!("{field}=unknown ")] {
                let unsupported = measured.replace(&format!("{field}=0 "), &replacement);
                assert!(hv(&unsupported).is_err(), "{unsupported}");
            }
        }
    }

    #[test]
    fn a_burst_that_booked_no_input_samples_is_a_failure_not_a_pass() {
        let hw = |n: u64| {
            percentiles(30, "6.00", "0.10", "1.00", "0.00")
                .replace("n_input=30", &format!("n_input={n}"))
        };
        assert!(hardware_key_verdict(30, &summary(41, "0.00", 0, 0, 0), &hw(0)).is_err());
        assert!(hv(&hw(30)).is_ok(), "the control");
    }

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
        let slow = percentiles(30, "6.00", "0.10", "56.00", "10.00");
        assert!(hv(&without(&slow, "max_acquire_wait_ms")).is_err());
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
        assert!(hv(&with_glass(31, "56.00")).is_ok());
        assert!(hv(&with_glass(0, "900.00")).is_ok());
    }

    #[test]
    fn a_verb_the_subject_lacks_is_told_from_its_answer() {
        assert!(refuses_verb("ERR unknown verb (try: help)\n", "aos"));
        assert!(refuses_verb(
            "aterm-ctl: ERR unknown verb (try: help)\n",
            "hwkey"
        ));
        assert!(refuses_verb("ERR aos needs a debug build\n", "aos"));
        assert!(!refuses_verb(AOS_BUNDLE_REFUSAL, "aos"));
        assert!(!refuses_verb(
            "aterm-ctl: ERR aos bundle needs a debug build",
            "aos"
        ));
        assert!(!refuses_verb("ERR aos attach refused: no window", "aos"));
        assert!(!refuses_verb("OK window=0 attached=false", "aos"));
        assert!(!refuses_verb("", "aos"));
    }

    #[test]
    fn the_release_aos_step_passes_only_a_release_builds_answers() {
        let attach = "OK window=0 attached=true code=shipped hot=false active=false settling=false \
                      build=0.140.0 paused=false";
        let detach =
            "OK window=0 attached=false code=shipped hot=false active=false settling=false";
        assert!(aos_release_verdict(attach, detach, AOS_BUNDLE_REFUSAL).is_ok());
        assert!(
            aos_release_verdict(
                attach,
                detach,
                "aterm-ctl: ERR aos bundle needs a debug build\n"
            )
            .is_ok(),
            "the reply as `aterm ctl` prints it"
        );
        let old_release = "ERR aos needs a debug build";
        for (a, d, b) in [
            (old_release, detach, AOS_BUNDLE_REFUSAL),
            (attach, old_release, AOS_BUNDLE_REFUSAL),
            (attach, detach, old_release),
            (
                attach,
                detach,
                "ERR aos bundle: no plan was published over the glow",
            ),
            (
                "OK window=0 attached=false code=shipped hot=false",
                detach,
                AOS_BUNDLE_REFUSAL,
            ),
            (
                "ERR aos attach refused: no window",
                detach,
                AOS_BUNDLE_REFUSAL,
            ),
            (attach, detach, "aterm-ctl: ERR aos needs a debug build"),
            ("", "", ""),
        ] {
            assert!(aos_release_verdict(a, d, b).is_err(), "{a:?} {d:?} {b:?}");
        }
    }

    #[test]
    fn an_unparsable_hardware_reply_fails_closed() {
        assert!(hv("").is_err());
        let summary =
            "OK frames=41 max_input_present_ms=8.100 last_key_write_ms=0.00 max_key_write_ms=0.00 ";
        assert!(hv(summary).is_err());
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
        let mut sb = Sandbox::with_config("atl", SMOKE_CONFIG).expect("sandbox");
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
        let toml = std::fs::read_to_string(sb.cfgdir.join("aterm/aterm.toml")).expect("written");
        let doc = aterm_toml::from_str::<aterm_toml::Value>(&toml).expect("one document");
        assert_eq!(
            doc.get("bell_sound").and_then(aterm_toml::Value::as_bool),
            Some(false),
            "{toml}"
        );
        assert_eq!(
            lanes_left_on(&doc),
            Vec::<String>::new(),
            "the bundle's window starts no lane of the login's account: {toml}"
        );

        let mut r = Report::new("t");
        sb.teardown(&mut r, None);
        assert!(!sb.tmp.exists(), "the sandbox removes itself");
        assert_eq!(r.outcomes().count(), 0, "a clean teardown says nothing");
    }

    /// Each entry of [`aterm_types::world::LANES_OFF`] that `doc` does not hold with the same
    /// value, as `table.key`.
    pub(crate) fn lanes_left_on(doc: &aterm_toml::Value) -> Vec<String> {
        let off = aterm_toml::from_str::<aterm_toml::Value>(aterm_types::world::LANES_OFF)
            .expect("the lanes-off tables are one document");
        let mut on = Vec::new();
        for (table, keys) in off.as_table().expect("a document is a table") {
            for (key, value) in keys.as_table().expect("each entry is a table") {
                if doc.get(table).and_then(|held| held.get(key)) != Some(value) {
                    on.push(format!("{table}.{key}"));
                }
            }
        }
        on
    }

    /// The handed bundle is a release build: its window reads no development seam, so the
    /// converge runner's window lane stays off only for the scratch world's marker, and the
    /// `_alab._tcp` browse for the marker and the world's `[swarm]` table both. The strip of the
    /// run's own `ATERM_*` variables would otherwise leave the marker out. The window's command
    /// carries it at the state folder under the world's HOME.
    #[test]
    fn the_smoke_window_carries_the_scratch_worlds_marker() {
        let ctx = ctx();
        let mut sb = Sandbox::with_config("atl", SMOKE_CONFIG).expect("sandbox");
        let cmd = sb.gui_command(&ctx, Path::new("/bin/true"));
        let marker = cmd
            .get_envs()
            .find(|(name, _)| name.to_str() == Some(aterm_types::world::MARKER))
            .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned));
        let state = aterm_types::world::state_home(&sb.tmp.join("home")).expect("a state folder");
        assert_eq!(marker, Some(state.into_os_string()), "{cmd:?}");
        let mut r = Report::new("t");
        sb.teardown(&mut r, None);
    }

    #[test]
    fn the_smoke_instance_carries_the_runs_mark() {
        let mut ctx = ctx();
        ctx.child_env_add
            .push((crate::runs::VERIFY_RUN_ENV.into(), "71-1".into()));
        let mut cmd = Command::new("/bin/true");
        gui_env(&ctx, &mut cmd);
        let set: Vec<_> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
            .collect();
        assert_eq!(
            set,
            [(
                std::ffi::OsString::from(crate::runs::VERIFY_RUN_ENV),
                std::ffi::OsString::from("71-1")
            )],
            "{cmd:?}"
        );
    }

    #[test]
    fn a_teardown_that_loses_its_child_is_a_failure() {
        let mut sb = Sandbox::with_config("atl", "").expect("sandbox");
        sb.child = Some(
            SmokeChild::spawn(
                Command::new("/bin/sh")
                    .args(["-c", "exit 7"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
                |_| {},
            )
            .expect("spawn"),
        );
        let pid = sb.child.as_ref().expect("child").id();
        assert!(
            crate::smoke::exited_on_its_own(pid),
            "`exit 7` never exited within a minute"
        );
        let mut r = Report::new("t");
        sb.teardown(&mut r, None);
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::Outcome::Fail(crate::Severity::GateFailed)]
        );
    }
}
