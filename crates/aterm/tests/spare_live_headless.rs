// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: the warm spare shell a plain new tab adopts
//! (`docs/DESIGN-warm-spare-2026-10-16.md`, tests L1–L11).
//!
//! Driven through the real binary, headless, with a scratch home whose rc each
//! test writes, `SHELL=/bin/zsh`, and `spare_shell_needs_person = false` (a
//! wire-driven instance has no person; L10 is the default's own test):
//!
//! * L1 a parked spare is nobody's session (`sessions` lists only the boot
//!   one), and a `spawn` adopts it: `born=spare-parked`, its prompt drawn;
//! * L2/L11 with learning skipped (`ATERM_DEBUG_SPARE_ELIGIBLE=1`) the spare
//!   runs its rc unseen and a `spawn` adopts it at its hold (`born=spare-ready`)
//!   or while its rc still runs (`born=spare-warming`, the buffered `go` kept
//!   until its first prompt);
//! * L3 three clean visible runs teach the gate, and the next spare is warm;
//! * L4 an rc that reads input, sleeps, `exec`s another program, rings the bell
//!   or asks the control socket never makes a generation eligible;
//! * L6 an rc edited while a spare is ready sends the next tab cold
//!   (`spare_miss=stale`);
//! * L7 asks the spare cannot serve go cold, with their reason, and the spare
//!   stays;
//! * L13 a tab from a held pane adopts the spare booked in its folder, and
//!   the next one too while the pane is held;
//! * L14 a parked spare serves a tab asked for in another folder, `cd`ing
//!   there at its release;
//! * L8 `spare_shell = "off"` retires it; `parked` retires a warm one;
//! * L9 quitting with a spare leaves no shell behind and no history written;
//! * L10 with the default (a person must be present) a wire-driven instance
//!   never spawns one;
//! * L12 a SIGKILLed instance's warm spare whose rc ignores the hangup is
//!   released by the next launch on its control dir, and its FIFO removed.
//!
//! Every wait for something that must happen is a one-minute hang detector,
//! never a latency budget; no check retries.
//!
//! Linux runs the suite too (2026-10-19, R6), on the lab's lane. Where the
//! host's zsh reads a system file before aterm's parked hold (Ubuntu's
//! `/etc/zsh/zshenv`: [`zsh_runs_a_system_file_first`]), a parked spare has run
//! it before anyone asked, so the product parks only for an eligible gate and
//! never moves such a spare to another folder: the visible runs that teach the
//! gate are cold tabs there, a `spare_shell = "parked"` instance is given the
//! eligible gate, and L14 reads the refusal.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::headless_boot;

use headless_boot::{Instance, boot_session, boot_with, ctl_ok, field, status};

/// Boot an instance whose home holds `zshrc`, with `config` prepended to its
/// aterm.toml and `seams` in its environment.
fn boot_spare(prefix: &str, zshrc: &str, config: &str, seams: &[(&str, &str)]) -> Instance {
    let zshrc = zshrc.to_owned();
    let config = config.to_owned();
    let seams: Vec<(String, String)> = seams
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    boot_with(
        prefix,
        move |tmp: &Path, cmd| {
            std::fs::write(tmp.join("home/.zshrc"), &zshrc).expect("zshrc");
            let toml = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&toml).expect("aterm.toml");
            std::fs::write(&toml, format!("{config}\n{base}")).expect("aterm.toml");
            cmd.env("SHELL", "/bin/zsh");
            // The gate's own account of every run it judged, for a failure
            // message ([`gate_log`]).
            cmd.env("ATERM_LOG", "info");
            for (k, v) in &seams {
                cmd.env(k, v);
            }
        },
        |_| true,
    )
}

/// `key=value` of the `metrics` line.
fn metric(inst: &Instance, key: &str) -> String {
    let m = ctl_ok(inst, &["metrics"]);
    field(&m, key).unwrap_or("-").to_owned()
}

/// Where the instance keeps its state and its log: its scratch home, and the
/// `XDG_STATE_HOME` the boot gave it (macOS reads only the home).
fn instance_dirs(inst: &Instance) -> aterm_types::dirs::StatePlatform {
    aterm_types::dirs::StatePlatform {
        home: Some(inst.tmp.join("home")),
        xdg_state_home: Some(inst.tmp.join("state")),
        ..Default::default()
    }
}

/// Whether this host's zsh reads a system file before aterm's parked hold
/// (`pre_hold_files`): then its parked spare waits for the gate, and serves
/// only the folder it was spawned in.
fn zsh_runs_a_system_file_first() -> bool {
    aterm_shell_integration::pre_hold_files(aterm_shell_integration::ShellType::Zsh)
        .iter()
        .any(|f| Path::new(f).exists())
}

/// A `spare_shell = "parked"` instance's seams: where zsh runs a system file
/// before its hold, the eligible gate parking needs there (parked mode
/// teaches no gate, and never promotes).
fn parked_seams() -> &'static [(&'static str, &'static str)] {
    if zsh_runs_a_system_file_first() {
        &[("ATERM_DEBUG_SPARE_ELIGIBLE", "1")]
    } else {
        &[]
    }
}

/// The instance log's spare lines (`boot_spare` logs at `info`): the gate's
/// verdict on each visible run, and why a run was unclean, for a failure
/// message.
fn gate_log(inst: &Instance) -> String {
    let log = aterm_types::dirs::resolve_logs_dir(None, instance_dirs(inst))
        .and_then(|dir| std::fs::read_to_string(dir.join("aterm.log")).ok())
        .unwrap_or_default();
    log.lines()
        .filter(|l| l.contains("spare"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wait until `metrics spare=` reads `state`; a hang detector.
fn await_spare(inst: &Instance, state: &str) {
    await_metric(inst, "spare", state);
}

/// The sids `sessions` lists.
fn listed(inst: &Instance) -> Vec<String> {
    ctl_ok(inst, &["sessions"])
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned))
        .collect()
}

/// `spawn` (a plain new tab), returning its sid.
fn spawn(inst: &Instance, args: &[&str]) -> String {
    let mut all = vec!["spawn"];
    all.extend_from_slice(args);
    let reply = ctl_ok(inst, &all);
    reply
        .split_whitespace()
        .find(|t| t.starts_with("s-"))
        .unwrap_or_else(|| panic!("spawn names its sid: {reply}"))
        .to_owned()
}

/// Wait until `sid`'s screen shows `text`; a hang detector.
fn await_text(inst: &Instance, sid: &str, text: &str) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
        if screen.contains(text) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "{sid} never showed {text:?}: {screen}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn born(inst: &Instance, sid: &str) -> String {
    field(&status(inst, sid), "born").unwrap_or("-").to_owned()
}

/// The `spare_adopted=` count for `tier`.
fn adopted(inst: &Instance, tier: &str) -> u32 {
    metric(inst, "spare_adopted")
        .split(',')
        .find_map(|kv| kv.strip_prefix(&format!("{tier}:")))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

const PROMPT_RC: &str = "PS1='SPARE%# '\n";

/// The current generation's one owned persisted snapshot. The fixture starts
/// from an exclusive empty state root and never changes its rc generation.
/// Open nonblocking/no-follow and read at most the production snapshot ceiling.
struct GateSnapshot {
    // Keep this inode alive until the next snapshot is observed.
    _file: std::fs::File,
    identity: (u64, u64),
    generation: String,
    streak: u32,
}

fn gate_snapshot(inst: &Instance) -> Option<GateSnapshot> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let state = aterm_types::dirs::resolve_state_dir(None, instance_dirs(inst))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(state.join("spare-gate-v1.tsv"))
        .ok()?;
    let meta = file.metadata().ok()?;
    const MAX: u64 = 128 * 1024;
    if !meta.is_file() || meta.len() > MAX {
        return None;
    }
    let mut text = String::new();
    (&file).take(MAX + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > MAX {
        return None;
    }
    let lines: Vec<_> = text.lines().collect();
    let [line] = lines.as_slice() else {
        return None;
    };
    let fields: Vec<_> = line.split('\t').collect();
    let [generation, streak, _, _, _, _, _] = fields.as_slice() else {
        return None;
    };
    Some(GateSnapshot {
        _file: file,
        identity: (meta.dev(), meta.ino()),
        generation: (*generation).to_owned(),
        streak: streak.parse().ok()?,
    })
}

fn await_sample_after(inst: &Instance, before: &GateSnapshot) -> GateSnapshot {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(after) = gate_snapshot(inst)
            && after.identity != before.identity
        {
            assert_eq!(
                after.generation, before.generation,
                "the fixture's generation stayed fixed"
            );
            return after;
        }
        assert!(
            Instant::now() < give_up,
            "the visible run never published its gate snapshot"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn await_gate(inst: &Instance, word: &str) {
    await_metric(inst, "spare_gate", word);
}

/// Wait until the `metrics` line's `key` reads `word`; a hang detector.
fn await_metric(inst: &Instance, key: &str, word: &str) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if metric(inst, key) == word {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "{key} never reached {word}: {}\n{}",
            ctl_ok(inst, &["metrics"]),
            gate_log(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The boot session is itself the first visible run. Settle it before making
/// the next two runs, and observe each fold before asking for another spare.
/// Each is a parked spare's adoption, or, where zsh runs a system file before
/// its hold (no parking before the gate opens), a cold tab.
fn learn_three_visible_runs(inst: &Instance) {
    let (_, boot) = boot_session(inst);
    await_text(inst, &boot, "SPARE%");
    await_gate(inst, "learning_1/3");
    for expected in [2, 3] {
        let tier = await_visible_run_tier(inst);
        let before = gate_snapshot(inst).expect("one settled generation in the fresh fixture");
        assert_eq!(before.streak, expected - 1);
        let sid = spawn(inst, &[]);
        assert_eq!(born(inst, &sid), tier);
        await_text(inst, &sid, "SPARE%");
        let after = await_sample_after(inst, &before);
        assert_eq!(
            after.streak, expected,
            "the actual visible run taught the gate"
        );
        await_gate(
            inst,
            if expected == 2 {
                "learning_2/3"
            } else {
                "eligible"
            },
        );
    }
}

/// Before a visible run that teaches the gate: wait for the parked spare it
/// adopts, and answer the tier it is born from; where zsh runs a system file
/// before its hold nothing parks before the gate opens, and the run is a cold
/// tab.
fn await_visible_run_tier(inst: &Instance) -> &'static str {
    if zsh_runs_a_system_file_first() {
        "cold"
    } else {
        await_spare(inst, "parked");
        "spare-parked"
    }
}

fn await_private_file(path: &Path) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "the owned fixture never wrote {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Release an owned rc barrier on assertion unwind too. Exclusive creation
/// never truncates a standing path; InstanceDrop still owns the App/lifeline.
struct RcRelease(PathBuf);

impl RcRelease {
    fn release(&self) -> std::io::Result<()> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.0)
            .map(drop)
    }
}

impl Drop for RcRelease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// L1.
#[test]
fn a_parked_spare_is_nobodys_session_until_a_spawn_adopts_it() {
    let inst = boot_spare(
        "aterm-spare-l1",
        PROMPT_RC,
        "spare_shell = \"parked\"\nspare_shell_needs_person = false",
        parked_seams(),
    );
    let (_, boot) = boot_session(&inst);
    await_spare(&inst, "parked");
    assert_eq!(listed(&inst), vec![boot.clone()], "the spare is not listed");
    assert_eq!(metric(&inst, "spare_ptys"), "1");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "spare-parked");
    await_text(&inst, &sid, "SPARE%");
    assert!(listed(&inst).contains(&sid), "listed once adopted");
    assert_eq!(adopted(&inst, "parked"), 1);
}

/// L2: a warm spare (learning skipped) is adopted at its hold.
#[test]
fn a_ready_warm_spare_is_adopted_at_its_hold() {
    let inst = boot_spare(
        "aterm-spare-l2",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    let (_, boot) = boot_session(&inst);
    await_spare(&inst, "ready");
    assert_eq!(listed(&inst), vec![boot], "a ready spare is not listed");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "spare-ready");
    await_text(&inst, &sid, "SPARE%");
    assert_eq!(adopted(&inst, "ready"), 1);
}

/// L3: three observed clean visible runs, including boot, teach the gate.
#[test]
fn three_clean_visible_runs_make_the_next_spare_warm() {
    let inst = boot_spare(
        "aterm-spare-l3",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[("ATERM_DEBUG_SPARE_IDLE_MS", "100")],
    );
    learn_three_visible_runs(&inst);
    await_spare(&inst, "ready");
    let parked_runs = if zsh_runs_a_system_file_first() { 0 } else { 2 };
    assert_eq!(
        adopted(&inst, "parked"),
        parked_runs,
        "boot was the first clean run"
    );
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "spare-ready");
}

/// L4: the boot session is the first clean visible run (`learning_1/3`); the
/// spare adopted once the rc misbehaves must fold as unclean and reset the
/// streak to 0, so no number of such runs reaches the three clean ones the gate
/// needs. Each negative run finishes its gate fold before the test closes it.
/// The daemon also records its own finite completion before this fixture
/// proceeds; no discovered PID is signalled. Each misbehaviour boots its own
/// instance and scratch world, so they run side by side.
#[test]
fn an_unclean_rc_never_opens_the_gate() {
    let aterm = env!("CARGO_BIN_EXE_aterm");
    let cases = [
        ("read", "read -r x".to_owned()),
        ("slow", "sleep 3".to_owned()),
        ("exec", "exec /bin/cat".to_owned()),
        ("bell", "print -n '\\a'".to_owned()),
        ("ctl", format!("'{aterm}' ctl @self status >/dev/null 2>&1")),
        (
            "daemon",
            "( { /bin/sleep 8; : > \"$HOME/daemon-finished\"; } & )".to_owned(),
        ),
    ];
    std::thread::scope(|scope| {
        let runs: Vec<_> = cases
            .iter()
            .map(|(name, misbehave)| scope.spawn(move || an_unclean_run(name, misbehave)))
            .collect();
        for run in runs {
            run.join()
                .unwrap_or_else(|why| std::panic::resume_unwind(why));
        }
    });
}

/// One L4 case: `misbehave` armed after the boot session's clean run.
fn an_unclean_run(name: &str, misbehave: &str) {
    let rc = format!("[[ -e $HOME/armed ]] && {{ {misbehave}; }}\nPS1='SPARE%# '\n");
    let inst = boot_spare(
        &format!("aterm-spare-l4{name}"),
        &rc,
        "spare_shell_needs_person = false",
        &[("ATERM_DEBUG_SPARE_IDLE_MS", "100")],
    );
    let (_, boot) = boot_session(&inst);
    await_text(&inst, &boot, "SPARE%");
    await_gate(&inst, "learning_1/3");
    std::fs::write(inst.tmp.join("home/armed"), b"").expect("arm");
    let tier = await_visible_run_tier(&inst);
    let before = gate_snapshot(&inst).expect("one settled generation");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), tier, "{name}");
    let after = await_sample_after(&inst, &before);
    assert_eq!(
        after.streak, 0,
        "{name}: the actual negative fold reset the streak"
    );
    await_gate(&inst, "learning_0/3");
    if name == "daemon" {
        await_private_file(&inst.tmp.join("home/daemon-finished"));
    }
    ctl_ok(&inst, &[&format!("@{sid}"), "close"]);
    assert_eq!(adopted(&inst, "ready"), 0, "{name}");
    assert_eq!(adopted(&inst, "warming"), 0, "{name}");
}

/// L6: an rc edited while a warm spare waits sends the next tab cold, and the
/// spawner says why.
#[test]
fn an_rc_edited_under_a_ready_spare_sends_the_next_tab_cold() {
    let inst = boot_spare(
        "aterm-spare-l6",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    await_spare(&inst, "ready");
    std::fs::write(inst.tmp.join("home/.zshrc"), "PS1='EDITED%# '\n").expect("edit");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "cold");
    await_text(&inst, &sid, "EDITED%");
    assert!(
        metric(&inst, "spare_miss").contains("stale:1"),
        "{}",
        ctl_ok(&inst, &["metrics"])
    );
}

/// L7: an identity, and another folder for a warm spare (its rc ran in its
/// own): each spawn goes cold with its reason, and the spare stays.
///
/// Both asks land behind the boot tab (`raise=f`), so the focused pane, whose
/// folder the spare follows, never moves: a tab raised in `/usr` would move
/// it, and the follow rule retires a misplaced warm spare 10 s later
/// (`REPLACE_WARM_AFTER`; a parked one 1 s later), a race with this test's own
/// pace on a loaded host. NEGATIVE CONTROL: raised asks and a pause past that
/// hold before the last check read `spare=absent`.
#[test]
fn asks_a_spare_cannot_serve_go_cold_and_the_spare_stays() {
    let inst = boot_spare(
        "aterm-spare-l7",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    let _ = boot_session(&inst);
    await_spare(&inst, "ready");
    let sid = spawn(&inst, &["cwd=/usr", "raise=f"]);
    assert_eq!(born(&inst, &sid), "cold");
    assert!(metric(&inst, "spare_miss").contains("folder:1"));
    assert_eq!(
        metric(&inst, "spare"),
        "ready",
        "the spare stays: {}\n{}",
        ctl_ok(&inst, &["metrics"]),
        gate_log(&inst)
    );
    // An identity is never a spare's (it may not even exist: the ask is
    // judged before the spawn).
    let _ = headless_boot::ctl(&inst, &["spawn", "identity=nobody-here", "raise=f"]);
    assert!(metric(&inst, "spare_miss").contains("ask:1"));
    assert_eq!(
        metric(&inst, "spare"),
        "ready",
        "the spare stays: {}\n{}",
        ctl_ok(&inst, &["metrics"]),
        gate_log(&inst)
    );
}

/// Wait until session `sid`'s foreground program is `program`: the pane is
/// held; a hang detector.
fn await_program(inst: &Instance, sid: &str, program: &str) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while field(&status(inst, sid), "program") != Some(program) {
        assert!(Instant::now() < give_up, "{sid}: {}", status(inst, sid));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// L13 (2026-10-18): Cmd-T while a program holds the focused pane (an agent,
/// here `sleep`) adopts the warm spare booked in that pane's folder, and the
/// next spare is booked, warmed and adopted while the pane is still held.
/// NEGATIVE CONTROL (the code before): a held pane's tab went cold, `held`.
#[test]
fn a_tab_from_a_held_pane_adopts_the_spare_booked_in_its_folder() {
    let inst = boot_spare(
        "aterm-spare-l13",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    let (_, boot) = boot_session(&inst);
    await_spare(&inst, "ready");
    headless_boot::type_line(&inst, &boot, "sleep 600");
    await_program(&inst, &boot, "sleep");
    for _ in 0..2 {
        await_spare(&inst, "ready");
        let sid = spawn(&inst, &[]);
        assert_eq!(
            born(&inst, &sid),
            "spare-ready",
            "{}",
            ctl_ok(&inst, &["metrics"])
        );
        await_text(&inst, &sid, "SPARE%");
        ctl_ok(&inst, &[&format!("@{sid}"), "close"]);
    }
    assert_eq!(adopted(&inst, "ready"), 2);
    assert_eq!(
        field(&status(&inst, &boot), "program"),
        Some("sleep"),
        "still held"
    );
}

/// L14 (2026-10-18): a parked spare serves a tab asked for in another folder:
/// it `cd`s there at its release, before its startup files, and the shell
/// reads as one started there (zsh: `$PWD`, and `$OLDPWD` the same). A warm
/// spare does not (L7). NEGATIVE CONTROL (the code before): `cwd=` went cold,
/// `folder`.
#[test]
fn a_parked_spare_serves_a_tab_asked_for_in_another_folder() {
    let inst = boot_spare(
        "aterm-spare-l14",
        PROMPT_RC,
        "spare_shell = \"parked\"\nspare_shell_needs_person = false",
        parked_seams(),
    );
    let _ = boot_session(&inst);
    await_spare(&inst, "parked");
    let sid = spawn(&inst, &["cwd=/usr"]);
    if zsh_runs_a_system_file_first() {
        // The system zshenv ran in the spare's own folder before its hold:
        // the tab goes cold where it was asked for, and the spare stays.
        assert_eq!(born(&inst, &sid), "cold", "{}", ctl_ok(&inst, &["metrics"]));
        assert!(metric(&inst, "spare_miss").contains("folder:1"));
        await_text(&inst, &sid, "SPARE%");
        headless_boot::type_line(&inst, &sid, "echo \"W$((40+2))[$PWD]\"");
        await_text(&inst, &sid, "W42[/usr]");
        assert_eq!(metric(&inst, "spare"), "parked");
        return;
    }
    assert_eq!(
        born(&inst, &sid),
        "spare-parked",
        "{}",
        ctl_ok(&inst, &["metrics"])
    );
    await_text(&inst, &sid, "SPARE%");
    headless_boot::type_line(&inst, &sid, "echo \"W$((40+2))[$PWD|$OLDPWD]\"");
    await_text(&inst, &sid, "W42[/usr|/usr]");
    assert_eq!(adopted(&inst, "parked"), 1);
}

/// L8: `parked`, hot-reloaded, retires a warm spare and keeps a parked one.
#[test]
fn spare_shell_parked_retires_a_warm_spare_on_reload() {
    let inst = boot_spare(
        "aterm-spare-l8p",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    await_spare(&inst, "ready");
    let toml = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&toml).expect("aterm.toml");
    std::fs::write(&toml, format!("spare_shell = \"parked\"\n{text}")).expect("aterm.toml");
    await_spare(&inst, "parked");
    assert!(
        metric(&inst, "spare_discard").contains("parked:1"),
        "{}",
        ctl_ok(&inst, &["metrics"])
    );
}

/// L8: the kill switch, hot-reloaded.
#[test]
fn spare_shell_off_retires_the_spare_on_reload() {
    let inst = boot_spare(
        "aterm-spare-l8",
        PROMPT_RC,
        "spare_shell = \"parked\"\nspare_shell_needs_person = false",
        parked_seams(),
    );
    await_spare(&inst, "parked");
    let toml = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&toml).expect("aterm.toml");
    std::fs::write(
        &toml,
        text.replace("spare_shell = \"parked\"", "spare_shell = \"off\""),
    )
    .expect("aterm.toml");
    await_spare(&inst, "off");
    // The retire hands the session to the spare thread, which drops its PTY
    // after the state already reads off.
    await_metric(&inst, "spare_ptys", "0");
}

/// L10: by default a spare exists only after a person's gesture: a wire-driven
/// instance never spawns one.
#[test]
fn without_a_person_no_spare_is_spawned() {
    let inst = boot_spare("aterm-spare-l10", PROMPT_RC, "", &[]);
    let (_, boot) = boot_session(&inst);
    // Past the boot grace and a landing's settle.
    let sid = spawn(&inst, &[]);
    await_text(&inst, &sid, "SPARE%");
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert_eq!(metric(&inst, "spare"), "absent");
    assert_eq!(metric(&inst, "spare_ptys"), "0");
    assert_eq!(born(&inst, &sid), "cold");
    assert!(listed(&inst).contains(&boot));
}

/// L11: an actual spare-only rc barrier holds Warming until the test has
/// adopted that spare and then explicitly releases it. The host's buffered
/// go still waits for the first prompt hook.
#[test]
fn a_warming_spare_is_adopted_and_its_buffered_release_survives() {
    let inst = boot_spare(
        "aterm-spare-l11",
        r#"if [[ ${__aterm_spare_mode-} == warm ]]; then
  : > "$HOME/warm-rc-entered"
  while [[ ! -e $HOME/warm-rc-release ]]; do /bin/sleep 0.01; done
fi
PS1='SPARE%# '
"#,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
            ("ATERM_DEBUG_SPARE_TREADY_MS", "600000"),
        ],
    );
    let release = RcRelease(inst.tmp.join("home/warm-rc-release"));
    await_private_file(&inst.tmp.join("home/warm-rc-entered"));
    assert_eq!(
        metric(&inst, "spare"),
        "warming",
        "the owned barrier holds the state"
    );
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "spare-warming");
    release.release().expect("release the owned rc barrier");
    await_text(&inst, &sid, "SPARE%");
    let left = spare_entries(&inst.tmp.join("run/aterm/spare"));
    assert!(
        !left.iter().any(|n| n.ends_with(&sid)),
        "the adopted spare's FIFO and its link are gone after its first prompt: {left:?}"
    );
}

/// The names in a control dir's spare folder.
fn spare_entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A shell this test left running, SIGKILLed on every exit path while it is
/// still the process born at `birth`, so a failing run leaks no blocked zsh.
struct Leftover {
    pid: i32,
    birth: u64,
}

impl Leftover {
    fn alive(&self) -> bool {
        aterm_pty::process_birth(self.pid) == Some(self.birth)
    }
}

impl Drop for Leftover {
    fn drop(&mut self) {
        if self.alive() {
            // SAFETY: a signal to the shell this test's instance spawned, its
            // birth checked just above.
            let _ = unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}

/// L12: an instance SIGKILLed while it holds a warm spare whose rc ignores the
/// hangup leaves that zsh blocked opening its FIFO. The next launch on the same
/// control dir finds the FIFO's owner dead, releases the zsh (it exits) and
/// removes the FIFO and its link. The new instance books no spare of its own
/// (no person), so the folder ends empty.
#[test]
fn a_crashed_instances_spare_is_released_by_the_next_launch() {
    let mut crashed = boot_spare(
        "aterm-spare-l12",
        "if [[ ${__aterm_spare_mode-} == warm ]]; then\n  trap '' HUP\n  print $$ > \"$HOME/spare-pid\"\nfi\nPS1='SPARE%# '\n",
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    await_spare(&crashed, "ready");
    let pid: i32 = std::fs::read_to_string(crashed.tmp.join("home/spare-pid"))
        .expect("the warm rc wrote its pid")
        .trim()
        .parse()
        .expect("a pid");
    let spare = Leftover {
        pid,
        birth: aterm_pty::process_birth(pid).expect("the ready spare runs"),
    };
    let spare_dir = crashed.tmp.join("run/aterm/spare");
    crashed.child.kill().expect("SIGKILL the instance");
    crashed.child.wait().expect("reap the instance");
    assert!(
        spare.alive(),
        "the HUP-ignoring spare outlived its instance"
    );
    assert!(
        !spare_entries(&spare_dir).is_empty(),
        "and its FIFO with it"
    );

    let run = crashed.tmp.join("run");
    let _next = headless_boot::boot_with(
        "aterm-spare-l12b",
        move |_, cmd| {
            cmd.env("XDG_RUNTIME_DIR", &run).env("SHELL", "/bin/zsh");
        },
        |_| true,
    );
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while spare.alive() || !spare_entries(&spare_dir).is_empty() {
        assert!(
            Instant::now() < give_up,
            "the next launch never released the crashed instance's spare (alive: {}; left: {:?})",
            spare.alive(),
            spare_entries(&spare_dir)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// L5: learn from observed folds, then hold only the refill's warm rc until
/// the explicitly selected product timeout retires it. The test waits for the
/// timeout verdict; it asserts no wall-clock latency.
#[test]
fn a_warm_spare_past_t_ready_is_killed_and_its_generation_backs_off() {
    let inst = boot_spare(
        "aterm-spare-l5",
        r#"if [[ ${__aterm_spare_mode-} == warm && -e $HOME/slow ]]; then
  : > "$HOME/warm-rc-entered"
  while [[ ! -e $HOME/warm-rc-release ]]; do /bin/sleep 0.01; done
fi
PS1='SPARE%# '
"#,
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
            ("ATERM_DEBUG_SPARE_TREADY_MS", "1000"),
        ],
    );
    let _release = RcRelease(inst.tmp.join("home/warm-rc-release"));
    learn_three_visible_runs(&inst);
    await_spare(&inst, "ready");
    assert_eq!(metric(&inst, "spare_gate"), "eligible");
    std::fs::write(inst.tmp.join("home/slow"), b"").expect("slow");
    let sid = spawn(&inst, &[]);
    assert_eq!(
        born(&inst, &sid),
        "spare-ready",
        "the ready one was not held by the barrier"
    );
    await_private_file(&inst.tmp.join("home/warm-rc-entered"));
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !metric(&inst, "spare_discard").contains("rc_timeout:1") {
        assert!(Instant::now() < give_up, "{}", ctl_ok(&inst, &["metrics"]));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    await_gate(&inst, "backoff");
    assert_eq!(adopted(&inst, "warming"), 0);
}

/// L9: quitting with a warm spare held leaves no shell behind and writes no
/// history: the spare is retired and its thread joined before the final reap.
#[test]
fn quitting_with_a_spare_leaves_no_shell_and_no_history() {
    let mut inst = boot_spare(
        "aterm-spare-l9",
        "HISTFILE=$HOME/hist\nSAVEHIST=50\nHISTSIZE=50\nsetopt inc_append_history\nPS1='SPARE%# '\n",
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    let hist = inst.tmp.join("home/hist");
    std::fs::write(&hist, "echo kept\n").expect("hist");
    await_spare(&inst, "ready");
    let pid = inst.child.id();
    let children = aterm_uds::spawnfd::command("pgrep")
        .arg("-P")
        .arg(pid.to_string())
        .output()
        .expect("pgrep");
    let shells: Vec<i32> = String::from_utf8_lossy(&children.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    assert!(
        shells.len() >= 2,
        "the boot shell and the spare: {shells:?}"
    );
    // SAFETY: a signal to the instance this test launched.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while inst.child.try_wait().expect("wait").is_none() {
        assert!(Instant::now() < give_up, "the instance never exited");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for shell in shells {
        let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        // SAFETY: a liveness probe (signal 0) of a pid this instance spawned.
        while unsafe { libc::kill(shell, 0) } == 0 {
            assert!(Instant::now() < give_up, "shell {shell} outlived the quit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    assert_eq!(
        std::fs::read_to_string(&hist).expect("hist"),
        "echo kept\n",
        "the spare wrote no history"
    );
}
