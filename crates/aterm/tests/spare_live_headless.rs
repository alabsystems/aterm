// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end tests for the warm spare shell adopted by a new tab.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::headless_boot;

use aterm_shell_integration::ShellType;
use headless_boot::{Instance, LoginShell, boot_session, boot_with, ctl_ok, field, status};

const SCAN_MS: &str = "100";

fn shell() -> LoginShell {
    headless_boot::login_shell()
}

fn boot_spare(prefix: &str, rc: &str, config: &str, seams: &[(&str, &str)]) -> Instance {
    let rc = rc.to_owned();
    let config = config.to_owned();
    let seams: Vec<(String, String)> = seams
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    boot_with(
        prefix,
        move |tmp: &Path, cmd| {
            std::fs::write(tmp.join("home").join(shell().rc), &rc).expect("rc");
            let toml = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&toml).expect("aterm.toml");
            std::fs::write(&toml, format!("{config}\n{base}")).expect("aterm.toml");
            cmd.env("SHELL", shell().path);
            cmd.env("ATERM_LOG", "info");
            cmd.env("ATERM_DEBUG_SPARE_SCAN_MS", SCAN_MS);
            cmd.env(
                "ATERM_DEBUG_SPARE_PARKED_MS",
                aterm_spec::wait::MUST_HAPPEN.as_millis().to_string(),
            );
            for (k, v) in &seams {
                cmd.env(k, v);
            }
        },
        |_| true,
    )
}

fn metric(inst: &Instance, key: &str) -> String {
    let m = ctl_ok(inst, &["metrics"]);
    field(&m, key).unwrap_or("-").to_owned()
}

fn instance_dirs(inst: &Instance) -> aterm_types::dirs::StatePlatform {
    aterm_types::dirs::StatePlatform {
        home: Some(inst.tmp.join("home")),
        xdg_state_home: Some(inst.tmp.join("state")),
        ..Default::default()
    }
}

fn runs_a_system_file_first() -> bool {
    aterm_shell_integration::pre_hold_files(shell().kind)
        .iter()
        .any(|f| Path::new(f).exists())
}

fn parked_seams() -> &'static [(&'static str, &'static str)] {
    if runs_a_system_file_first() {
        &[("ATERM_DEBUG_SPARE_ELIGIBLE", "1")]
    } else {
        &[]
    }
}

fn gate_log(inst: &Instance) -> String {
    let log = aterm_types::dirs::resolve_logs_dir(None, instance_dirs(inst))
        .and_then(|dir| std::fs::read_to_string(dir.join("aterm.log")).ok())
        .unwrap_or_default();
    log.lines()
        .filter(|l| l.contains("spare"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn hang_report(inst: &Instance) -> String {
    format!(
        "{}\nits aterm-spare thread now:\n{}",
        gate_log(inst),
        headless_boot::thread_stack(inst.child.id(), &inst.log, Some("aterm-spare"))
    )
}

fn await_spare(inst: &Instance, state: &str) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let m = ctl_ok(inst, &["metrics"]);
        if field(&m, "spare") == Some(state) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "spare never reached {state}: {m}\n{}",
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn a_budget_decided_it(inst: &Instance, missed: &str) -> ! {
    let log = gate_log(inst);
    if let Some(load) = aterm_spec::host::oversubscribed() {
        aterm_spec::host::could_not_run(format!(
            "{missed}, on an oversubscribed host ({load}), where one-line rcs \
             miss the product's time budgets on load alone:\n{log}"
        ));
    }
    panic!("{missed}, on a host with CPU to spare:\n{log}");
}

fn fold_failed(log: &str) -> Option<(Vec<String>, String)> {
    let lines: Vec<&str> = log.lines().collect();
    let verdict = lines
        .iter()
        .rposition(|l| VERDICTS.iter().any(|v| l.contains(v)))
        .filter(|&at| lines[at].contains("saw a visible run: Unclean"))?;
    let named = lines[..verdict]
        .iter()
        .rev()
        .take_while(|l| !VERDICTS.iter().any(|v| l.contains(v)))
        .find(|l| {
            l.contains("spare gate: unclean run: failed=") || l.contains("spare gate: escape:")
        })?;
    let rules = match named.split("failed=").nth(1) {
        Some(rest) => rest
            .split([':', ' '])
            .next()?
            .split(',')
            .map(str::to_owned)
            .collect(),
        None => vec!["escape".to_owned()],
    };
    Some((rules, (*named).to_owned()))
}

fn a_budget_fold(log: &str) -> Option<String> {
    let (rules, line) = fold_failed(log)?;
    rules
        .iter()
        .all(|rule| matches!(rule.as_str(), "start" | "slow"))
        .then_some(line)
}

fn log_through(inst: &Instance, verdict: &str, n: usize) -> String {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let log = gate_log(inst);
        let lines: Vec<&str> = log.lines().collect();
        if let Some((at, _)) = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(verdict))
            .nth(n.saturating_sub(1))
        {
            return lines[..=at].join("\n");
        }
        assert!(
            Instant::now() < give_up,
            "the instance never logged {verdict:?} {n} times:\n{}",
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn listed(inst: &Instance) -> Vec<String> {
    ctl_ok(inst, &["sessions"])
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned))
        .collect()
}

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

fn adopted(inst: &Instance, tier: &str) -> u32 {
    metric(inst, "spare_adopted")
        .split(',')
        .find_map(|kv| kv.strip_prefix(&format!("{tier}:")))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

const PROMPT_RC: &str = "PS1='SPARE%# '\n";

struct GateSnapshot {
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
            "the visible run never published its gate snapshot:\n{}",
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn await_gate(inst: &Instance, word: &str) {
    await_metric(inst, "spare_gate", word);
}

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
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn await_first_sample(inst: &Instance) -> GateSnapshot {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(first) = gate_snapshot(inst) {
            return first;
        }
        assert!(
            Instant::now() < give_up,
            "the boot run never published its gate snapshot:\n{}",
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

const VERDICTS: [&str; 2] = ["saw a visible run:", "verified at its hold:"];

fn a_strangers_orphan(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let verdict = lines.iter().rposition(|l| {
        l.contains("saw a visible run: Unclean")
            || l.contains("verified at its hold: Backoff(\"escaped\")")
    })?;
    let escape = lines[..verdict]
        .iter()
        .rev()
        .take_while(|l| !VERDICTS.iter().any(|v| l.contains(v)))
        .find(|l| l.contains("spare gate: escape:"))?;
    let word = |key: &str| {
        escape
            .split_whitespace()
            .find_map(|w| w.strip_prefix(key))
            .map(str::to_owned)
    };
    let flags = u32::from_str_radix(word("flags=0x")?.as_str(), 16).ok()?;
    let terminal_born = flags & 0x80 != 0;
    (word("in_session=")? == "false" && word("attached=")? == "false" && !terminal_born)
        .then(|| (*escape).to_owned())
}

fn an_unsettled_scan(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let verdict = lines
        .iter()
        .rposition(|l| VERDICTS.iter().any(|v| l.contains(v)))?;
    let mut scanned = lines[..verdict]
        .iter()
        .rev()
        .take_while(|l| !VERDICTS.iter().any(|v| l.contains(v)));
    if scanned.clone().any(|l| l.contains(SHELL_UNKNOWN)) {
        return None;
    }
    if lines[verdict].contains("verified at its hold: Backoff(\"scan-unknown\")") {
        return Some(lines[verdict].to_owned());
    }
    if !lines[verdict].contains("saw a visible run: Inconclusive") {
        return None;
    }
    scanned
        .find(|l| l.contains("spare gate: inconclusive run: scan Unknown"))
        .map(|l| (*l).to_owned())
}

const SHELL_UNKNOWN: &str = "spare gate: scan unknown: the run's shell";

fn an_unsettled_scan_decided_it(line: &str, log: &str) -> ! {
    aterm_spec::host::could_not_run(format!(
        "the escape scan could not settle this account's process listing, so the gate \
         took no sample of the run ({line}); the listing is every process of this user, \
         which no test holds still:\n{log}"
    ))
}

#[test]
fn only_a_scan_with_no_verdict_is_the_hosts() {
    let fold = |why: &str, verdict: &str| {
        format!(
            "spare gate: generation 00000000000000aa saw a visible run: Clean {{ rc_ms: 9 }}\n\
             spare gate: {why}\n\
             spare gate: generation 00000000000000aa saw a visible run: {verdict}"
        )
    };
    assert_eq!(
        an_unsettled_scan(&fold("inconclusive run: scan Unknown", "Inconclusive")).as_deref(),
        Some("spare gate: inconclusive run: scan Unknown")
    );
    assert_eq!(
        an_unsettled_scan("spare 3: verified at its hold: Backoff(\"scan-unknown\")").as_deref(),
        Some("spare 3: verified at its hold: Backoff(\"scan-unknown\")")
    );
    for log in [
        fold(
            "inconclusive run: input moved before its start record",
            "Inconclusive",
        ),
        fold("inconclusive run: scan RateLimited", "Inconclusive"),
        fold("unclean run: failed=flags", "Unclean"),
        "spare 3: verified at its hold: Backoff(\"stale\")".to_owned(),
        format!(
            "{}\nspare gate: generation 00000000000000aa saw a visible run: Clean {{ rc_ms: 9 }}",
            fold("inconclusive run: scan Unknown", "Inconclusive")
        ),
        fold(
            "scan unknown: the run's shell -1 unread\n\
             spare gate: inconclusive run: scan Unknown",
            "Inconclusive",
        ),
        "spare gate: scan unknown: the run's shell 4242 gone\n\
         spare 3: verified at its hold: Backoff(\"scan-unknown\")"
            .to_owned(),
    ] {
        assert_eq!(an_unsettled_scan(&log), None, "{log}");
    }
}

fn expect_streak(inst: &Instance, streak: u32, expected: u32) {
    if streak != expected {
        let log = log_through(inst, "saw a visible run:", expected as usize);
        if let Some(escape) = a_strangers_orphan(&log) {
            a_stranger_decided_it(&escape);
        }
        if let Some(line) = an_unsettled_scan(&log) {
            an_unsettled_scan_decided_it(&line, &log);
        }
        if let Some(fold) = a_budget_fold(&log) {
            a_budget_decided_it(
                inst,
                &format!("a one-line rc's run missed the gate's window ({fold})"),
            );
        }
    }
    assert_eq!(
        streak,
        expected,
        "the actual visible run taught the gate:\n{}",
        gate_log(inst)
    );
}

fn a_stranger_decided_it(escape: &str) -> ! {
    aterm_spec::host::could_not_run(format!(
        "another program's orphan was adopted in a clean run's window, which \
         the scan counts as it counts the run's own double-fork daemon: {escape}"
    ))
}

fn await_learned_spare_ready(inst: &Instance) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let m = ctl_ok(inst, &["metrics"]);
        if field(&m, "spare") == Some("ready") {
            return;
        }
        if field(&m, "spare_gate").is_some_and(|gate| gate.starts_with("ineligible")) {
            panic!(
                "the learned generation was found ineligible before its warm spare was ready: \
                 {m}\n{}",
                gate_log(inst)
            );
        }
        if field(&m, "spare_gate") == Some("backoff") {
            if field(&m, "spare_discard").is_some_and(|d| d.contains("rc_timeout:")) {
                a_budget_decided_it(
                    inst,
                    "the learned generation's warm rc ran past its T_READY deadline",
                );
            }
            if field(&m, "spare_discard").is_some_and(|d| d.contains("escaped:"))
                && let Some(escape) =
                    a_strangers_orphan(&log_through(inst, "verified at its hold: Backoff", 1))
            {
                a_stranger_decided_it(&escape);
            }
            if field(&m, "spare_discard").is_some_and(|d| d.contains("scan-unknown:")) {
                let log = log_through(inst, "verified at its hold: Backoff", 1);
                if let Some(line) = an_unsettled_scan(&log) {
                    an_unsettled_scan_decided_it(&line, &log);
                }
            }
            panic!(
                "the learned generation backed off before its warm spare was ready: {m}\n{}",
                gate_log(inst)
            );
        }
        assert!(
            Instant::now() < give_up,
            "spare never reached ready: {m}\n{}",
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn boot_run_is_clean(inst: &Instance) {
    let (_, boot) = boot_session(inst);
    await_text(inst, &boot, "SPARE%");
    expect_streak(inst, await_first_sample(inst).streak, 1);
    await_gate(inst, "learning_1/3");
}

fn learn_three_visible_runs(inst: &Instance) {
    boot_run_is_clean(inst);
    for expected in [2, 3] {
        let tier = await_visible_run_tier(inst);
        let before = gate_snapshot(inst).expect("one settled generation in the fresh fixture");
        assert_eq!(before.streak, expected - 1);
        let sid = spawn(inst, &[]);
        assert_eq!(born(inst, &sid), tier);
        await_text(inst, &sid, "SPARE%");
        let after = await_sample_after(inst, &before);
        expect_streak(inst, after.streak, expected);
        if expected == 2 {
            await_gate(inst, "learning_2/3");
        } else {
            await_learned(inst);
        }
    }
}

fn await_learned(inst: &Instance) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let gate = metric(inst, "spare_gate");
        if gate == "eligible" || gate == "backoff" || gate.starts_with("ineligible") {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "spare_gate never reached eligible: {}\n{}",
            ctl_ok(inst, &["metrics"]),
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn await_visible_run_tier(inst: &Instance) -> &'static str {
    if runs_a_system_file_first() {
        "cold"
    } else {
        await_spare(inst, "parked");
        "spare-parked"
    }
}

fn await_private_file(inst: &Instance, path: &Path) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "the owned fixture never wrote {}: {}\n{}",
            path.display(),
            ctl_ok(inst, &["metrics"]),
            gate_log(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn await_slow_part(inst: &Instance, entered: &Path) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let timed_out = metric(inst, "spare_discard").contains("rc_timeout:");
        if std::fs::symlink_metadata(entered).is_ok_and(|meta| meta.is_file()) {
            return;
        }
        if timed_out {
            a_budget_decided_it(
                inst,
                "the warm rc ran past its 1 s T_READY before it reached its slow part",
            );
        }
        assert!(
            Instant::now() < give_up,
            "the warm rc never reached its slow part: {}\n{}",
            ctl_ok(inst, &["metrics"]),
            hang_report(inst)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

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

#[test]
fn three_clean_visible_runs_make_the_next_spare_warm() {
    let inst = boot_spare(
        "aterm-spare-l3",
        PROMPT_RC,
        "spare_shell_needs_person = false",
        &[("ATERM_DEBUG_SPARE_IDLE_MS", "100")],
    );
    learn_three_visible_runs(&inst);
    await_learned_spare_ready(&inst);
    let parked_runs = if runs_a_system_file_first() { 0 } else { 2 };
    assert_eq!(
        adopted(&inst, "parked"),
        parked_runs,
        "boot was the first clean run"
    );
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "spare-ready");
}

#[test]
fn an_unclean_rc_never_opens_the_gate() {
    let aterm = crate::launch_isolation::aterm_executable();
    let aterm = aterm.display();
    let cases: [(&str, &[&str], String); 6] = [
        ("read", &["start"], "read -r x".to_owned()),
        ("slow", &["start", "slow"], "sleep 3".to_owned()),
        ("exec", &["start"], "exec /bin/cat".to_owned()),
        ("bell", &["host"], "printf '\\a'".to_owned()),
        (
            "ctl",
            &["control"],
            format!("'{aterm}' ctl @self status >/dev/null 2>&1"),
        ),
        (
            "daemon",
            &["escape"],
            "( { n=0; while [[ ! -e $HOME/daemon-release && $n -lt 1200 ]]; do /bin/sleep 0.05; \
             n=$((n + 1)); done; : > \"$HOME/daemon-finished\"; } & )"
                .to_owned(),
        ),
    ];
    std::thread::scope(|scope| {
        let runs: Vec<_> = cases
            .iter()
            .map(|(name, rules, misbehave)| {
                scope.spawn(move || an_unclean_run(name, rules, misbehave))
            })
            .collect();
        let ends: Vec<_> = runs
            .into_iter()
            .filter_map(|run| run.join().err())
            .collect();
        if let Some(end) = worst_end(ends) {
            std::panic::resume_unwind(end);
        }
    });
}

fn worst_end(
    mut ends: Vec<Box<dyn std::any::Any + Send>>,
) -> Option<Box<dyn std::any::Any + Send>> {
    let could_not_run = |end: &Box<dyn std::any::Any + Send>| {
        end.downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| end.downcast_ref::<&str>().copied())
            .is_some_and(|said| said.contains(aterm_spec::host::COULD_NOT_RUN))
    };
    let at = ends.iter().position(|end| !could_not_run(end)).unwrap_or(0);
    (at < ends.len()).then(|| ends.swap_remove(at))
}

#[test]
fn a_failed_case_is_reported_over_one_that_could_not_run() {
    let said = |end: Option<Box<dyn std::any::Any + Send>>| {
        end.and_then(|end| {
            end.downcast_ref::<String>()
                .cloned()
                .or_else(|| end.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        })
    };
    let cnr = || -> Box<dyn std::any::Any + Send> {
        Box::new(format!(
            "{} — ctl: the window",
            aterm_spec::host::COULD_NOT_RUN
        ))
    };
    assert!(
        said(worst_end(vec![cnr(), Box::new("daemon: an escape unseen")])).as_deref()
            == Some("daemon: an escape unseen"),
        "a literal failure joined after a COULD NOT RUN is the one reported"
    );
    assert!(
        said(worst_end(vec![cnr(), Box::new("bell: failed".to_owned())])).as_deref()
            == Some("bell: failed"),
        "a formatted failure joined after a COULD NOT RUN is the one reported"
    );
    assert!(
        said(worst_end(vec![cnr()])).is_some_and(|s| s.ends_with("ctl: the window")),
        "with no failure, the COULD NOT RUN is reported"
    );
    assert!(worst_end(Vec::new()).is_none(), "no end, nothing reported");
}

fn an_unclean_run(name: &str, rules: &[&str], misbehave: &str) {
    let rc = format!("[[ -e $HOME/armed ]] && {{ {misbehave}; }}\nPS1='SPARE%# '\n");
    let inst = boot_spare(
        &format!("aterm-spare-l4{name}"),
        &rc,
        "spare_shell_needs_person = false",
        &[("ATERM_DEBUG_SPARE_IDLE_MS", "100")],
    );
    let daemon = RcRelease(inst.tmp.join("home/daemon-release"));
    boot_run_is_clean(&inst);
    std::fs::write(inst.tmp.join("home/armed"), b"").expect("arm");
    let tier = await_visible_run_tier(&inst);
    let before = gate_snapshot(&inst).expect("one settled generation");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), tier, "{name}");
    let after = await_sample_after(&inst, &before);
    let log = log_through(&inst, "saw a visible run:", 2);
    if after.streak != 0
        && let Some(line) = an_unsettled_scan(&log)
    {
        an_unsettled_scan_decided_it(&format!("{name}: {line}"), &log);
    }
    assert_eq!(
        after.streak, 0,
        "{name}: the actual negative fold reset the streak"
    );
    let (failed, line) = fold_failed(&log)
        .unwrap_or_else(|| panic!("{name}: an unclean fold names what it failed:\n{log}"));
    if !failed.iter().any(|rule| rules.contains(&rule.as_str())) {
        if a_budget_fold(&log).is_some() {
            a_budget_decided_it(
                &inst,
                &format!(
                    "{name}: the run missed the gate's window before {rules:?} was asked ({line})"
                ),
            );
        }
        panic!("{name}: the fold failed {failed:?}, not {rules:?}:\n{log}");
    }
    await_gate(&inst, "learning_0/3");
    if name == "daemon" {
        daemon.release().expect("release the daemon");
        await_private_file(&inst, &inst.tmp.join("home/daemon-finished"));
    }
    ctl_ok(&inst, &[&format!("@{sid}"), "close"]);
    assert_eq!(adopted(&inst, "ready"), 0, "{name}");
    assert_eq!(adopted(&inst, "warming"), 0, "{name}");
}

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
    std::fs::write(inst.tmp.join("home").join(shell().rc), "PS1='EDITED%# '\n").expect("edit");
    let sid = spawn(&inst, &[]);
    assert_eq!(born(&inst, &sid), "cold");
    await_text(&inst, &sid, "EDITED%");
    assert!(
        metric(&inst, "spare_miss").contains("stale:1"),
        "{}",
        ctl_ok(&inst, &["metrics"])
    );
}

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
}

fn await_program(inst: &Instance, sid: &str, program: &str) {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while field(&status(inst, sid), "program") != Some(program) {
        assert!(Instant::now() < give_up, "{sid}: {}", status(inst, sid));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

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
    if runs_a_system_file_first() {
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
    let (line, want) = if shell().kind == ShellType::Zsh {
        ("echo \"W$((40+2))[$PWD|$OLDPWD]\"", "W42[/usr|/usr]")
    } else {
        ("echo \"W$((40+2))[$PWD]\"", "W42[/usr]")
    };
    headless_boot::type_line(&inst, &sid, line);
    await_text(&inst, &sid, want);
    assert_eq!(adopted(&inst, "parked"), 1);
}

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
    await_metric(&inst, "spare_ptys", "0");
}

#[test]
fn without_a_person_no_spare_is_spawned() {
    let inst = boot_spare("aterm-spare-l10", PROMPT_RC, "", &[]);
    let (_, boot) = boot_session(&inst);
    let sid = spawn(&inst, &[]);
    await_text(&inst, &sid, "SPARE%");
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert_eq!(metric(&inst, "spare"), "absent");
    assert_eq!(metric(&inst, "spare_ptys"), "0");
    assert_eq!(born(&inst, &sid), "cold");
    assert!(listed(&inst).contains(&boot));
}

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
    await_private_file(&inst, &inst.tmp.join("home/warm-rc-entered"));
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

fn spare_entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

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
            // SAFETY: sends SIGKILL to the shell pid spawned by this test's instance.
            let _ = unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}

#[test]
fn a_crashed_instances_spare_is_released_by_the_next_launch() {
    let mut crashed = boot_spare(
        "aterm-spare-l12",
        "if [[ ${__aterm_spare_mode-} == warm ]]; then\n  trap '' HUP\n  echo $$ > \"$HOME/spare-pid\"\nfi\nPS1='SPARE%# '\n",
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
            cmd.env("XDG_RUNTIME_DIR", &run).env("SHELL", shell().path);
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
    await_learned_spare_ready(&inst);
    assert_eq!(metric(&inst, "spare_gate"), "eligible");
    std::fs::write(inst.tmp.join("home/slow"), b"").expect("slow");
    let sid = spawn(&inst, &[]);
    assert_eq!(
        born(&inst, &sid),
        "spare-ready",
        "the ready one was not held by the barrier"
    );
    await_slow_part(&inst, &inst.tmp.join("home/warm-rc-entered"));
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !metric(&inst, "spare_discard").contains("rc_timeout:1") {
        assert!(Instant::now() < give_up, "{}", ctl_ok(&inst, &["metrics"]));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    await_gate(&inst, "backoff");
    assert_eq!(adopted(&inst, "warming"), 0);
}

#[test]
fn quitting_with_a_spare_retires_it_before_the_exit() {
    let mut inst = boot_spare(
        "aterm-spare-l9",
        "if [[ ${__aterm_spare_mode-} == warm ]]; then\n  trap '' HUP\n  echo $$ > \"$HOME/spare-pid\"\nfi\nPS1='SPARE%# '\n",
        "spare_shell_needs_person = false",
        &[
            ("ATERM_DEBUG_SPARE_ELIGIBLE", "1"),
            ("ATERM_DEBUG_SPARE_IDLE_MS", "100"),
        ],
    );
    await_spare(&inst, "ready");
    let pid: i32 = std::fs::read_to_string(inst.tmp.join("home/spare-pid"))
        .expect("the warm rc wrote its pid")
        .trim()
        .parse()
        .expect("a pid");
    let spare = Leftover {
        pid,
        birth: aterm_pty::process_birth(pid).expect("the ready spare runs"),
    };
    let spare_dir = inst.tmp.join("run/aterm/spare");
    assert!(
        !spare_entries(&spare_dir).is_empty(),
        "the ready spare holds its FIFO"
    );
    let aterm = inst.child.id();
    // SAFETY: sends SIGTERM to the instance pid launched by this test.
    assert_eq!(unsafe { libc::kill(aterm as i32, libc::SIGTERM) }, 0);
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while inst.child.try_wait().expect("wait").is_none() {
        assert!(Instant::now() < give_up, "the instance never exited");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        spare_entries(&spare_dir),
        Vec::<String>::new(),
        "the quit retired the spare: its FIFO and link are gone"
    );
    while spare.alive() {
        assert!(
            Instant::now() < give_up,
            "the spare's shell outlived the quit"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
