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
pub const SMOKE_CONFIG: &str = "motion = \"full\"\ncursor_trail_style = \"rainbow\"\ncursor_pet = \"cat\"\ncursor_momentum_glow = true\ntrail_sounds = true\n";

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

/// WHY A BURST VERDICT DID NOT PASS ([`effects_verdict`], [`hardware_key_verdict`]): what the
/// machine's load can explain, and what it cannot.
#[derive(Debug, PartialEq, Eq)]
pub enum Miss {
    /// A reading past its ceiling, or a count that booked samples but fewer than its floor: the
    /// burst measured, and a burst that ran loaded (admitted loaded at its patience bound, or
    /// entered on a busy machine) is no evidence either way, so there the breach is UNDECIDED,
    /// stated beside the pass and never a finding (a busy machine never blocks a release: the row
    /// is named, not counted red). On a quiet admission it fails.
    Breach(String),
    /// What no load explains: a reply that cannot be read, native clocks that did not report,
    /// effects that never engaged, a count that booked nothing at all, or a present retry, drop
    /// or sync timeout-release. A finding however busy the machine was.
    Fail(String),
}

impl Miss {
    /// The line that says what missed.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Breach(why) | Self::Fail(why) => why,
        }
    }
}

/// The burst's verdict on its post-burst `metrics` summary.
/// # Errors
/// [`Miss::Fail`] for a summary missing either reading or effects that never engaged;
/// [`Miss::Breach`] for too many late arms.
pub fn effects_verdict(reply: &str) -> Result<String, Miss> {
    let Some((arms, late)) = cursor_effect_arms(reply) else {
        return Err(Miss::Fail(format!(
            "gui smoke: metrics carry no deadline_arms_by_owner ledger -> {}",
            or_no_reply(reply)
        )));
    };
    let Some(under) = metric_u64(reply, "fx_under_frames") else {
        return Err(Miss::Fail(format!(
            "gui smoke: metrics carry no fx_under_frames -> {}",
            or_no_reply(reply)
        )));
    };
    if arms == 0 || under == 0 {
        return Err(Miss::Fail(format!(
            "gui smoke: the cursor effects never engaged — cursor_effect arms={arms}, \
             fx_under_frames={under} — so the burst measured an effects-off instance"
        )));
    }
    let over_share = late.saturating_mul(CURSOR_EFFECT_LATE_SHARE_DEN) > arms && late > 1;
    if late > CURSOR_EFFECT_LATE_MAX || over_share {
        return Err(Miss::Breach(format!(
            "gui smoke: cursor_effect booked {late} late arms of {arms} during a paced \
             burst (max {CURSOR_EFFECT_LATE_MAX}, and at most 1 in \
             {CURSOR_EFFECT_LATE_SHARE_DEN}) — the effects are arming deadlines they \
             already missed"
        )));
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
    /// browse the person's real LAN through the LAN broker, submit that bundle's keeper
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
    let ready = crate::stages::native_row(ctx, r, "launch and listen", |r| {
        bring_up(ctx, r, &mut sb, bundle)
    });
    if let Ready::Up { ctl: ctl_bin } = ready {
        let subject = crate::stages::Subject::of(ctx, bundle);
        crate::stages::native_row(ctx, r, "typing burst", |r| {
            typing_burst(ctx, r, &mut sb, &ctl_bin);
        });
        if !ctx.cursor_pacing {
            crate::stages::native_row(ctx, r, "release AOS", |r| {
                aos_release_step(ctx, r, &mut sb, &ctl_bin, &subject);
            });
            crate::stages::native_row(ctx, r, "Settings hardware keys", |r| {
                settings_hardware_key_step(ctx, r, &mut sb, &ctl_bin, &subject);
            });
        }
    }
    crate::stages::native_row(ctx, r, "teardown", |r| {
        sb.teardown(r, ctx.exec_env().stage_groups);
    });
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
    let started = std::time::Instant::now();
    let presented = wait_for_initial_present(
        || {
            if child_exited(sb) {
                Err(InitialPresent::Exited)
            } else {
                Ok(ctl(ctx, sb, ctl_bin, &["metrics"]))
            }
        },
        || started.elapsed(),
        || std::thread::sleep(POLL_GAP),
    );
    match presented {
        Ok(_) => {}
        Err(InitialPresent::Exited) => {
            r.fail("gui smoke: aterm-gui exited before its initial present");
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
        Err(InitialPresent::Unpresented(got)) => {
            r.fail(format!(
                "gui smoke: the frontmost window never produced an initial present [{}]",
                or_no_reply(&got)
            ));
            r.raw(smoke_log_tail("GUI smoke", &sb.gui_log));
            return;
        }
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

#[derive(Debug)]
enum InitialPresent {
    Exited,
    Unpresented(String),
}

const INITIAL_PRESENT_HANG: Duration = Duration::from_secs(60);

fn wait_for_initial_present(
    mut observe: impl FnMut() -> Result<String, InitialPresent>,
    elapsed: impl Fn() -> Duration,
    mut pause: impl FnMut(),
) -> Result<String, InitialPresent> {
    loop {
        let got = observe()?;
        if metric_u64(&got, "frames").is_some_and(|frames| frames > 0) {
            return Ok(got);
        }
        if elapsed() >= INITIAL_PRESENT_HANG {
            return Err(InitialPresent::Unpresented(got));
        }
        pause();
    }
}

struct BurstTiming {
    outer: Arc<aterm_capacity::Claim>,
    entry: aterm_capacity::Claim,
    /// Why the burst runs without a qualified quiet admission: stated, never a finding.
    unqualified: Vec<aterm_capacity::Unqualified>,
}

/// The burst's measured entry, under the stage's claim: on a machine that stayed busy through
/// its patience it runs loaded and says so.
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
fn check_burst_timing(timing: &BurstTiming) -> Result<Vec<aterm_capacity::Unqualified>, String> {
    let mut unqualified = timing.unqualified.clone();
    lapsed_or(timing.outer.timing_protection(), &mut unqualified)
        .map_err(|why| format!("outer timing hold: {why}"))?;
    lapsed_or(timing.entry.timing_protection(), &mut unqualified)
        .map_err(|why| format!("burst timing hold: {why}"))?;
    Ok(unqualified)
}

/// Whether the burst ran loaded: its claim was admitted loaded at its patience bound, or its
/// fresh entry read a busy machine. A hold that only lapsed is not load: the burst entered quiet,
/// and its readings stand.
fn ran_loaded(unqualified: &[aterm_capacity::Unqualified]) -> bool {
    unqualified.iter().any(|u| {
        matches!(
            u,
            aterm_capacity::Unqualified::Admission(_) | aterm_capacity::Unqualified::Entry(_)
        )
    })
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
    protected: impl FnOnce(&G) -> Result<Vec<aterm_capacity::Unqualified>, String>,
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
    // Stated beside each pass, never a finding: a busy machine never blocks a release.
    let stated = if unqualified.is_empty() {
        String::new()
    } else {
        let why: Vec<String> = unqualified.iter().map(ToString::to_string).collect();
        format!(
            "; ran without a qualified quiet admission (stated, not a finding): {}",
            why.join("; ")
        )
    };
    let loaded = ran_loaded(&unqualified);
    if let Err(why) = bounded {
        r.fail(why);
        return;
    }
    let effects = effects_verdict(&readings.summary);
    let hardware = hardware_key_verdict(readings.posted, &readings.summary, &readings.percentiles);
    retain_burst_readings(r, &readings);
    for verdict in [effects, hardware] {
        match verdict {
            Ok(line) => r.pass(format!("{line}{stated}")),
            // A ceiling or floor the loaded burst missed is UNDECIDED: its takes are no evidence
            // either way on that load, so it is stated beside the pass, as a measure row the
            // machine's load leaves undecided is (stages::stated_note).
            Err(Miss::Breach(why)) if loaded => r.pass(format!(
                "{why}; undecided, stated and not a finding: the burst ran loaded, so its takes \
                 are no evidence either way{stated}"
            )),
            Err(Miss::Breach(why) | Miss::Fail(why)) => r.fail(why),
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
/// [`Miss::Fail`] for a reply that cannot be parsed, unreported or unavailable native clocks, a
/// floored count that booked nothing, a sampled slice without its maximum, or any present retry,
/// drop or sync timeout-release, each judged before any breach; [`Miss::Breach`] for a floored
/// count that booked some samples but too few, or any gated reading at or over its ceiling.
pub fn hardware_key_verdict(posted: u64, summary: &str, reply: &str) -> Result<String, Miss> {
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
        reply_field(reply, "key_write_p99_ms")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0),
        whole("max_term_wait_press_ms"),
        metric_u64(reply, "n_acquire"),
        whole("acquire_p99_ms"),
        metric_u64(reply, "n_input"),
        whole("input_p99_ms"),
        metric_u64(reply, "n_present_glass"),
        whole("present_glass_p99_ms"),
    )
    else {
        return Err(Miss::Fail(format!(
            "gui smoke: could not parse hardware-key percentiles -> {}",
            or_no_reply(reply)
        )));
    };
    let (Some(frames), Some(max_input), Some(retries), Some(drops), Some(sync_timeouts)) = (
        metric_u64(summary, "frames"),
        metric_ms_whole(summary, "max_input_present_ms"),
        metric_u64(summary, "redraw_retry_gated"),
        metric_u64(summary, "present_drops"),
        metric_u64(summary, "sync_rel_timeout"),
    ) else {
        return Err(Miss::Fail(format!(
            "gui smoke: could not parse the post-burst metrics -> {}",
            or_no_reply(summary)
        )));
    };
    let f = |name: &str| reply_field(reply, name);
    let max_ms = |source: &str, name: &str| {
        reply_field(source, name)
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| {
                Miss::Fail(format!(
                    "gui smoke: a sampled slice published no {name}, so nothing can confirm or \
                     refute its p99 [{source}]"
                ))
            })
    };
    let bar = |ms: u64| f64::from(u32::try_from(ms).unwrap_or(u32::MAX));
    let (Some(queue_unavailable), Some(write_unavailable)) = (
        metric_u64(reply, "key_queue_unavailable"),
        metric_u64(reply, "key_write_unavailable"),
    ) else {
        return Err(Miss::Fail(format!(
            "gui smoke: hardware clock availability is unreported or invalid; the subject \
             must publish key_queue_unavailable and key_write_unavailable to validate \
             hardware latency [{reply}]"
        )));
    };
    if queue_unavailable != 0 || write_unavailable != 0 {
        return Err(Miss::Fail(format!(
            "gui smoke: hardware latency is unconfirmed because native clock samples are \
             unavailable; input_clock=unconfirmed key_queue_unavailable={queue_unavailable} \
             key_write_unavailable={write_unavailable} n_key_write={n_key_write} \
             posted={posted} n_input={n_input} [{reply}]"
        )));
    }
    let floors = [
        (
            n_key_write,
            HW_KEY_WRITE_FLOOR,
            format!(
                "gui smoke: insufficient measured hardware key→write samples — \
                 n_key_write={n_key_write} of posted={posted} (< {HW_KEY_WRITE_FLOOR}); \
                 native queue age unavailable for {queue_unavailable} presses and \
                 {write_unavailable} write completions; n_input={n_input} [{reply}]"
            ),
        ),
        (
            n_input,
            HW_INPUT_FLOOR,
            format!(
                "gui smoke: the burst booked n_input={n_input} input→present samples of \
                 posted={posted} (< {HW_INPUT_FLOOR}), so the input→present ceilings measured \
                 nothing [{reply}]"
            ),
        ),
        (
            frames,
            FRAMES_FLOOR,
            format!(
                "gui smoke: present starvation — frames={frames} (< {FRAMES_FLOOR}) [{summary}]"
            ),
        ),
    ];
    // What no load explains is judged before any ceiling or floor, so a breach a loaded burst
    // states as undecided never hides it: a floored count that booked nothing, a sampled slice
    // without its maximum, and any present retry, drop or sync timeout-release.
    if let Some((_, _, why)) = floors.iter().find(|(count, _, _)| *count == 0) {
        return Err(Miss::Fail(why.clone()));
    }
    let acquire_max = if n_acquire > 0 {
        Some(max_ms(reply, "max_acquire_wait_ms")?)
    } else {
        None
    };
    let glass_max = if n_present_glass > 0 {
        Some(max_ms(reply, "max_present_glass_ms")?)
    } else {
        None
    };
    let key_write_max = max_ms(summary, "max_key_write_ms")?;
    if retries != 0 || drops != 0 {
        return Err(Miss::Fail(format!(
            "gui smoke: present retries/drops during frontmost typing — \
             redraw_retry_gated={retries} present_drops={drops} [{summary}]"
        )));
    }
    if sync_timeouts != 0 {
        return Err(Miss::Fail(format!(
            "gui smoke: sync timeout-releases during plain typing — \
             sync_rel_timeout={sync_timeouts} [{summary}]"
        )));
    }
    if let Some((_, _, why)) = floors.into_iter().find(|(count, floor, _)| count < floor) {
        return Err(Miss::Breach(why));
    }
    if max_input >= INPUT_PRESENT_CEILING_MS {
        return Err(Miss::Breach(format!(
            "gui smoke: input→present latency — max {max_input}ms \
             (>= {INPUT_PRESENT_CEILING_MS}) [{summary}]"
        )));
    }
    // Histogram quantiles are bucket upper edges. A measured maximum below the ceiling
    // disproves a breach even when that bucket straddles it.
    if key_write_max > bar(KEY_WRITE_P99_CEILING_MS) && key_write > bar(KEY_WRITE_P99_CEILING_MS) {
        return Err(Miss::Breach(format!(
            "gui smoke: hardware key→write — p99 {key_write}ms (> {KEY_WRITE_P99_CEILING_MS}), \
             aterm's own dispatch with OS queue residence; press lock wait max {}ms, \
             acquire p99 {}ms, child echo p99 {}ms [{reply}]",
            f("max_term_wait_press_ms"),
            f("acquire_p99_ms"),
            f("echo_p99_ms"),
        )));
    }
    if press_max >= TERM_WAIT_PRESS_MAX_CEILING_MS {
        return Err(Miss::Breach(format!(
            "gui smoke: key-press terminal-mutex wait — max {press_max}ms \
             (>= {TERM_WAIT_PRESS_MAX_CEILING_MS}) over n={} [{reply}]",
            f("n_term_wait_press"),
        )));
    }
    if acquire_max.is_some_and(|max| max >= bar(ACQUIRE_P99_CEILING_MS))
        && acquire > ACQUIRE_P99_CEILING_MS
    {
        return Err(Miss::Breach(format!(
            "gui smoke: drawable acquire — p99 {acquire}ms (> {ACQUIRE_P99_CEILING_MS}, and its max confirms it) \
             over n={n_acquire}, max {}ms [{reply}]",
            f("max_acquire_wait_ms"),
        )));
    }
    if glass_max.is_some_and(|max| max >= bar(PRESENT_GLASS_P99_CEILING_MS))
        && glass > PRESENT_GLASS_P99_CEILING_MS
    {
        return Err(Miss::Breach(format!(
            "gui smoke: present→glass — p99 {glass}ms \
             (> {PRESENT_GLASS_P99_CEILING_MS}, and its max confirms it) over n={n_present_glass}, the compositor \
             leg AFTER present-return that every slice above stops short of; max {}ms, \
             {} drawable(s) never shown [{reply}]",
            f("max_present_glass_ms"),
            f("present_glass_skipped"),
        )));
    }
    if input > INPUT_PRESENT_CEILING_MS {
        return Err(Miss::Breach(format!(
            "gui smoke: hardware input→present — p99 {input}ms (> {INPUT_PRESENT_CEILING_MS}), \
             input_clock=native [{reply}]"
        )));
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
#[path = "smoke_stages_tests.rs"]
pub(crate) mod tests;
