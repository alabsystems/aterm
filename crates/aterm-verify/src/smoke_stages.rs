// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GUI typing-pacing smoke: the `measure` check's live window.

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

/// Does a reply say `OK` and a field (`OK <…>`)?
fn ok_reply(reply: &str) -> bool {
    reply.starts_with("OK ")
}

/// The `aterm.toml` lines the smoke's instance runs with (top-level keys, ahead of the sandbox's first `[table]`).
pub const SMOKE_CONFIG: &str = "cursor_trail_style = \"rainbow kitty pet\"\ncursor_momentum_glow = true\ntrail_sounds = true\n";

/// Late (`past`) `cursor_effect` arms the burst may book: healthy effects-on
/// bursts arm ~430 deadlines and book 0-2 late, so four leaves two of headroom.
const CURSOR_EFFECT_LATE_MAX: u64 = 4;
/// …and as a share of the owner's arms, so a burst that armed little cannot pass
/// on the absolute bound alone: more than one late arm in fifty fails once more
/// than one arm was late.
const CURSOR_EFFECT_LATE_SHARE_DEN: u64 = 50;

/// The `cursor_effect` owner's `arms/past` out of a `metrics` reply's `deadline_arms_by_owner=owner:arms/past,...` ledger.
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

/// The burst's verdict on its post-burst `metrics` summary.
///
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
const FRAMES_FLOOR: u64 = 15;
/// Hardware keys that must reach the `KeyboardInput` arm (`n_key_write`) before
/// any hardware slice verdict means anything: half the burst, the frames
/// floor's ratio.
const HW_KEY_WRITE_FLOOR: u64 = 15;
/// Input→present samples the burst must book (`n_input`, one per presented key)
/// before the input→present ceilings mean anything, at the same ratio: a burst
/// that booked none would pass both on a maximum of 0 ms.
const HW_INPUT_FLOOR: u64 = 15;
/// `key->write` p99 ceiling, OS queue residence included.
const KEY_WRITE_P99_CEILING_MS: u64 = 40;
/// Worst wait for the terminal mutex at the key-press site (presses and
/// releases, which `key->write` does not sample).
const TERM_WAIT_PRESS_MAX_CEILING_MS: u64 = 25;
/// Drawable-acquire p99 ceiling: three 60 Hz frames.
const ACQUIRE_P99_CEILING_MS: u64 = 50;
/// Present->glass p99 ceiling: the compositor leg, `presentDrawable:`
/// registration to the drawable's `presentedTime`.
const PRESENT_GLASS_P99_CEILING_MS: u64 = 50;
/// The socket-listen budget.
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
        let rundir = tmp.join("run");
        std::fs::create_dir_all(rundir.join("aterm")).ok()?;
        chmod_700(&rundir)?;
        chmod_700(&rundir.join("aterm"))?;
        let cfgdir = tmp.join("cfg");
        std::fs::create_dir_all(cfgdir.join("aterm")).ok()?;
        chmod_700(&cfgdir)?;
        std::fs::write(
            cfgdir.join("aterm/aterm.toml"),
            format!(
                "{extra}agents_auto_prime = false\nbell_sound = false\n\
                 [update]\nenabled = false\nauto_apply = false\n\
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

    /// Reap the child and remove the sandbox.
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
/// socket; the bundle's `aterm-ctl` drives it.
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
    let mut cmd = Command::new(&gui);
    gui_env(ctx, &mut cmd);
    cmd.current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .env("HOME", sb.tmp.join("home"))
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir)
        .env("SHELL", "/bin/sh")
        .stdout(log)
        .stderr(log2);
    cmd.arg("--control-sock").arg(sb.sock());
    match cmd.spawn() {
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

/// The smoke instance's inherited environment: no `ATERM_*` session, capability, handoff,
/// fabric or update authority (its own bindings are set after this removal), and the run's
/// mark (`ATERM_VERIFY_RUN`, [`crate::runs::VERIFY_RUN_ENV`]) every child of a run carries,
/// so a run killed while the instance is up leaves it to the next run's start pass to judge.
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

/// [`SOCKET_POLLS`] looks at `exited` and then `up`, `gap` apart ([`POLL_GAP`]
/// in a real run), and one last look at `up` after them.
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

/// One `aterm-ctl` round trip inside the sandbox, stdout and stderr merged.
fn ctl(ctx: &Ctx, sb: &Sandbox, ctl_bin: &Path, args: &[&str]) -> String {
    let cmd = Cmd::new(ctl_bin)
        .arg("--sock")
        .arg(sb.sock())
        .args(args.iter().copied())
        .env("XDG_RUNTIME_DIR", &sb.rundir)
        .env("XDG_CONFIG_HOME", &sb.cfgdir);
    capture_reply(&cmd, ctx.exec_env())
}

/// `got`, or `<no reply>` when it is empty.
fn or_no_reply(got: &str) -> &str {
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
        typing_burst(ctx, r, &mut sb, &ctl_bin);
        settings_hardware_key_step(ctx, r, &mut sb, &ctl_bin);
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

/// THE REAL KEY PATH REACHES A NATIVE VIEW (xa-20): with Settings in front of a
/// real window, one `hwkey tab` — a real NSEvent through the OS queue, winit's
/// `KeyboardInput` arm, `App::on_key` and the native-view key path — must move
/// Settings' keyboard focus.
fn settings_hardware_key_step(ctx: &Ctx, r: &mut Report, sb: &mut Sandbox, ctl_bin: &Path) {
    let tag = "gui smoke: settings key path";
    let Some(pid) = sb.child.as_ref().map(Child::id) else {
        r.cannot_run(format!("{tag}: no child to drive"));
        return;
    };
    let got = ctl(
        ctx,
        sb,
        ctl_bin,
        &["open", "app", "settings", "/appearance"],
    );
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
        .and_then(|line| reply_field(line, "view").parse::<u64>().ok())
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

/// Every reason this stage cannot measure anything.
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

/// The text of `<name>=<value>` in a metrics reply (the last occurrence, as the
/// numeric helpers read it), for a verdict line; `?` when absent.
fn reply_field<'a>(reply: &'a str, name: &str) -> &'a str {
    let needle = format!(" {name}=");
    reply
        .rfind(needle.as_str())
        .and_then(|at| reply[at + needle.len()..].split_whitespace().next())
        .unwrap_or("?")
}

/// WHAT A REPORTED p99 IS, and why these four bars read `>` and not `>=`.
///
/// # Errors
/// A reply that cannot be parsed, a sampled slice without its maximum, a burst under a floor, or any gated reading at or over its ceiling.
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

    /// The same reply with one `max_*_ms` field replaced: the bucketed p99 is an
    /// exclusive bucket edge, so a case meaning "a sample really crossed the bar"
    /// has to move the max too, as the verdict requires.
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
    fn a_burst_that_never_reached_the_keyboard_arm_is_a_failure_not_a_pass() {
        let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
        assert!(hv(&controller_shaped).is_err());
    }

    /// The input ceilings cannot pass vacuously: a burst that booked no
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

    /// A sampled acquire or compositor slice must publish its true maximum, or a
    /// p99 far over its ceiling would pass once the max field was renamed or
    /// dropped.
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

        let mut r = Report::new("t");
        sb.teardown(&mut r);
        assert!(!sb.tmp.exists(), "the sandbox removes itself");
        assert_eq!(r.outcomes().count(), 0, "a clean teardown says nothing");
    }

    /// The smoke instance carries the run's mark and no other `ATERM_*` it would inherit.
    /// NEGATIVE CONTROL: strip without setting the mark, and the instance carries no run id.
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
            Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn"),
        );
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
