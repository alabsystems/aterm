// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `mac nux`'s tree rows (one-mesh design §11 A17c): `reveal-tab` runs `lab reveal-tab`'s world
//! ([`crate::reveal_tab`]: its synthetic import, zsh rc and windowed instance) in a fresh clone of
//! the person base, and `reveal-tab-control` runs it under the GUI's plain-publication negative
//! control, so the window's accessibility tree is read with no person's grant.
//!
//! The run is the guest's own. This lab's executable (a debug image, which `reveal-tab` needs) and
//! the aterm under test (a debug build, the only kind the lab's window guard takes) are shared into
//! the guest ([`stage`]) and run there as an agent of the console's session ([`run_script`]). Its
//! instance's window therefore opens on the guest's console, and its outside job is a launchd job
//! of that session. The tree is read by the guest helper: the run's `--ax-helper` starts
//! `aterm-lab-guest ax-texts PID` disclaimed, so tccd judges the helper's own grant, which the
//! person base writes for its path and identifier, and never the run's. What the helper read
//! stays in the guest. The run's report carries its evidence and no value
//! ([`crate::reveal_tab::evidence_json`]), and this Mac reads it back and judges it
//! ([`crate::reveal_tab::judge`]), so each check is the one `lab reveal-tab` makes on a Mac.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{GUEST_HELPER, GUEST_NUX, Guest, Row, Value, millis, squeeze};
use crate::mac::{GUEST_SHARE, PERSON_HELPER, STEP};
use crate::reveal_tab::{Evidence, TREE_CHECKS, evidence_from, judge, scrub, skipped};
use crate::{Fail, stop};

/// The share's folder of the run's two executables, and the guest's (in [`GUEST_NUX`]).
const SHARED: &str = "reveal";
/// The aterm under test, by the one file name the lab's window guard takes.
const ATERM: &str = "aterm";
/// This lab's executable, which runs `reveal-tab` in the guest.
const LAB: &str = "aterm-lab";

/// The run's launchd label: an agent of the console's session.
const AGENT: &str = "com.aterm.nux.reveal";
/// What the run leaves in [`GUEST_NUX`]: its report (stdout), its stderr, and its exit status,
/// written once it ended.
const REPORT: &str = "reveal.json";
const LOG: &str = "reveal.log";
const EXIT: &str = "reveal.exit";

/// The run's hang detector. Its own waits end it sooner: its window admission waits up to ten
/// minutes for a quiet guest, then each of its steps waits up to a minute.
const HANG: Duration = Duration::from_secs(15 * 60);
/// How often the guest is asked whether the run ended.
const LOOK: Duration = Duration::from_secs(1);

/// Put the aterm under test (`LAB_ATERM`, else `target/debug/aterm`) and this lab's own executable
/// into the share's [`SHARED`] folder, for each tree row to copy into its guest.
///
/// # Errors
/// No aterm to test (NOT RUN), or a copy failed.
pub(super) fn stage(share: &Path) -> Result<(), Fail> {
    let paths = crate::paths::paths();
    let aterm = paths
        .need_aterm()
        .map_err(|e| crate::mac::not_run(format!("the tree rows: {e}")))?;
    let dir = share.join(SHARED);
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (from, name) in [(aterm, ATERM), (paths.exe.as_path(), LAB)] {
        crate::util::copy_regular_file(from, dir.join(name))
            .map_err(|e| format!("{}: {e}", from.display()))?;
    }
    Ok(())
}

/// The run, as the agent's shell runs it in `dir`: `lab reveal-tab --json`, its tree read by the
/// helper at [`PERSON_HELPER`], `control` adding `--plain-publication`; its report, stderr and
/// exit status into [`GUEST_NUX`], the status last.
fn run_script(dir: &str, control: bool) -> String {
    format!(
        "'{dir}/{LAB}' reveal-tab --json --ax-helper {PERSON_HELPER}{plain} '{dir}/{ATERM}' \
         > {GUEST_NUX}/{REPORT} 2> {GUEST_NUX}/{LOG}; echo $? > {GUEST_NUX}/{EXIT}",
        plain = if control { " --plain-publication" } else { "" },
    )
}

/// The row: the run in `g`, its evidence judged here into `row`'s cells, and the row failed unless
/// its verdict holds ([`verdict`]).
///
/// # Errors
/// A guest step failed, the run did not end within [`HANG`], or it left no evidence.
pub(super) fn row(g: &Guest, row: &mut Row, control: bool) -> Result<(), Fail> {
    let dir = format!("{GUEST_NUX}/{SHARED}");
    g.sh(
        &format!(
            "rm -rf {dir} && rm -f {GUEST_NUX}/{REPORT} {GUEST_NUX}/{LOG} {GUEST_NUX}/{EXIT} && mkdir -p {dir} \
             && cp '{GUEST_SHARE}/{SHARED}/{ATERM}' '{GUEST_SHARE}/{SHARED}/{LAB}' {dir}/ && chmod 755 {dir}/{ATERM} {dir}/{LAB}"
        ),
        STEP,
    )?;
    // This run's helper at the path the grant names, signed with the identifier it requires.
    g.sh(
        &g.as_root(&crate::mac::person_install(&format!(
            "{GUEST_NUX}/{GUEST_HELPER}"
        ))),
        STEP,
    )?;
    let started = Instant::now();
    g.agent_start(AGENT, &["/bin/sh", "-c", &run_script(&dir, control)])?;
    let ended = await_end(g, started);
    g.agent_stop(AGENT);
    row.cell("guest run", Value::Ms(millis(started.elapsed())));
    ended?;
    let report = g.read(&format!("{GUEST_NUX}/{REPORT}"))?;
    let Some(ev) = report_evidence(&report, control) else {
        let exit = g.read(&format!("{GUEST_NUX}/{EXIT}"))?;
        return Err(Fail::die(format!(
            "the guest's lab reveal-tab left no evidence (exit {}): {}",
            exit.trim(),
            said(g)
        )));
    };
    let checks = judge(&ev);
    for (name, value) in cells(&ev, &checks) {
        row.cell(name, value);
    }
    if let Err(why) = verdict(&checks, control) {
        row.fail(why);
    }
    Ok(())
}

/// Wait for the run's exit status, within [`HANG`] of `started`.
///
/// # Errors
/// A stop, or the hang detector, with what the run said.
fn await_end(g: &Guest, started: Instant) -> Result<(), Fail> {
    loop {
        if g.vm
            .exec(&format!("test -e {GUEST_NUX}/{EXIT}"), STEP)
            .is_ok_and(|o| o.status.success())
        {
            return Ok(());
        }
        if started.elapsed() >= HANG {
            return Err(Fail::die(format!(
                "the guest's lab reveal-tab did not end within {} min: {}",
                HANG.as_secs() / 60,
                said(g)
            )));
        }
        stop::sleep(LOOK)?;
    }
}

/// What the run and its agent wrote to stderr, each line holding a value withheld.
fn said(g: &Guest) -> String {
    let text = [
        format!("{GUEST_NUX}/{LOG}"),
        format!("{GUEST_NUX}/{AGENT}.err"),
    ]
    .iter()
    .map(|file| g.read(file).unwrap_or_default())
    .collect::<Vec<_>>()
    .join("\n");
    squeeze(scrub(text.trim()).trim(), 400)
}

/// The evidence of the run's report: the last line that is `reveal-tab`'s report, made under the
/// publication the row asked for (`control`). `None` when there is none, or the evidence is not
/// whole.
fn report_evidence(stdout: &str, control: bool) -> Option<Evidence> {
    let report = stdout.lines().rev().find_map(|line| {
        aterm_json::from_str::<aterm_json::Value>(line.trim())
            .ok()
            .filter(|v| v.get("lane").and_then(aterm_json::Value::as_str) == Some("reveal-tab"))
    })?;
    if report
        .get("plain_publication")
        .and_then(aterm_json::Value::as_bool)?
        != control
    {
        return None;
    }
    evidence_from(report.get("evidence")?)
}

/// The row's cells: how many checks held, each of the tree's three (NOT MEASURED, with why, where
/// the tree was not read), and the names of the checks off the tree that failed.
fn cells(ev: &Evidence, checks: &[(String, bool)]) -> Vec<(String, Value)> {
    let held = checks.iter().filter(|(_, held)| *held).count();
    let mut cells = vec![(
        "checks".to_owned(),
        Value::Text(format!("{held} of {} held", checks.len())),
    )];
    let unread = skipped(ev)
        .first()
        .map_or_else(String::new, |why| (*why).to_owned());
    for name in TREE_CHECKS {
        let value = match checks.iter().find(|(check, _)| check == name) {
            Some((_, true)) => Value::Text("PASS".to_owned()),
            Some((_, false)) => Value::Text("FAIL".to_owned()),
            None => Value::Missing(unread.clone()),
        };
        cells.push((format!("tree: {name}"), value));
    }
    let failed = off_tree_failures(checks);
    if !failed.is_empty() {
        cells.push((
            "failed off the tree".to_owned(),
            Value::Text(failed.join("; ")),
        ));
    }
    cells
}

/// The checks off the tree that failed, by name.
fn off_tree_failures(checks: &[(String, bool)]) -> Vec<&str> {
    checks
        .iter()
        .filter(|(name, held)| !*held && !TREE_CHECKS.contains(&name.as_str()))
        .map(|(name, _)| name.as_str())
        .collect()
}

/// Whether the row holds: the tree was read and every check off it held; then, for `reveal-tab`,
/// the tree's three held, and for its control, "and no value" failed (the lane sees a leak).
///
/// # Errors
/// What fails the row.
fn verdict(checks: &[(String, bool)], control: bool) -> Result<(), String> {
    let tree = |name: &str| {
        checks
            .iter()
            .find(|(check, _)| check == name)
            .map(|(_, held)| *held)
    };
    let [_, _, clean] = TREE_CHECKS;
    let Some(no_value) = tree(clean) else {
        return Err(
            "the window's accessibility tree was not read: the guest helper holds no Accessibility grant"
                .to_owned(),
        );
    };
    let off = off_tree_failures(checks);
    if !off.is_empty() {
        return Err(format!(
            "{} of the run's checks off the tree failed: {}",
            off.len(),
            off.join("; ")
        ));
    }
    if control {
        return if no_value {
            Err("CONTROL: under the plain publication the tree held no value, so this lane's \"and no value\" is no reading".to_owned())
        } else {
            Ok(())
        };
    }
    let failed: Vec<&str> = TREE_CHECKS
        .into_iter()
        .filter(|name| tree(name) == Some(false))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the window's accessibility tree: {} failed",
            failed.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reveal_tab::{AxEvidence, evidence_json};

    /// A run's checks as `judge` names them: the ones off the tree `off` each, and the tree's
    /// three as `tree` says (none: not read).
    fn checks(off: bool, tree: Option<[bool; 3]>) -> Vec<(String, bool)> {
        let mut checks = vec![
            ("the import sealed the synthetic logins".to_owned(), true),
            ("`text` of the Editor tab refuses".to_owned(), off),
        ];
        if let Some(tree) = tree {
            checks.extend(
                TREE_CHECKS
                    .iter()
                    .zip(tree)
                    .map(|(n, h)| ((*n).to_owned(), h)),
            );
        }
        checks
    }

    /// The row holds when every check does; its control holds only when "and no value" fails,
    /// whatever the grid read. Neither holds with the tree unread or a check off it failed.
    #[test]
    fn a_row_holds_on_every_check_and_its_control_only_on_a_seen_value() {
        assert_eq!(verdict(&checks(true, Some([true; 3])), false), Ok(()));
        assert!(
            verdict(&checks(true, Some([true; 3])), true)
                .unwrap_err()
                .starts_with("CONTROL:")
        );
        for seen in [
            [false, true, false],
            [false, false, false],
            [true, true, false],
        ] {
            assert_eq!(verdict(&checks(true, Some(seen)), true), Ok(()));
            assert!(
                verdict(&checks(true, Some(seen)), false)
                    .unwrap_err()
                    .contains("and no value")
            );
        }
        for control in [false, true] {
            assert!(
                verdict(&checks(true, None), control)
                    .unwrap_err()
                    .contains("not read")
            );
            let off = verdict(&checks(false, Some([false, true, false])), control).unwrap_err();
            assert!(off.contains("`text` of the Editor tab refuses"), "{off}");
        }
    }

    /// The cells name every tree check, NOT MEASURED with why where the tree was not read, and
    /// the failures off it by name.
    #[test]
    fn the_cells_name_each_tree_check_and_each_failure() {
        let read = Evidence {
            ax: Some(AxEvidence::default()),
            ..Evidence::default()
        };
        let cells = cells(&read, &checks(false, Some([true, true, false])));
        let text: Vec<String> = cells.iter().map(|(n, v)| format!("{n}={v}")).collect();
        assert_eq!(
            text,
            [
                "checks=3 of 5 held".to_owned(),
                format!("tree: {}=PASS", TREE_CHECKS[0]),
                format!("tree: {}=PASS", TREE_CHECKS[1]),
                format!("tree: {}=FAIL", TREE_CHECKS[2]),
                "failed off the tree=`text` of the Editor tab refuses".to_owned(),
            ]
        );
        let unread = cells_unread();
        assert!(
            unread.iter().filter(|(n, _)| n.starts_with("tree: ")).all(
                |(_, v)| matches!(v, Value::Missing(why) if why.contains("Accessibility grant"))
            )
        );
    }

    fn cells_unread() -> Vec<(String, Value)> {
        cells(&Evidence::default(), &checks(true, None))
    }

    /// The report's evidence is read from its last report line, after anything else the run
    /// printed, and only when it was made under the publication the row asked for.
    #[test]
    fn the_evidence_is_the_report_lines_under_the_rows_publication() {
        let ev = Evidence {
            imported: true,
            ax: Some(AxEvidence {
                grid_read: true,
                logins_read: true,
                clean: false,
            }),
            ..Evidence::default()
        };
        let report = |plain: bool| {
            aterm_json::to_string(&aterm_json::json!({
                "lane": "reveal-tab",
                "pass": false,
                "plain_publication": plain,
                "evidence": evidence_json(&ev),
            }))
            .unwrap()
        };
        let stdout = format!("waiting for a quiet machine\n{}\n", report(true));
        assert_eq!(report_evidence(&stdout, true), Some(ev.clone()));
        assert_eq!(report_evidence(&stdout, false), None);
        assert_eq!(report_evidence(&report(false), false), Some(ev));
        assert_eq!(report_evidence("", false), None);
        assert_eq!(
            report_evidence(
                "{\"lane\":\"reveal-tab\",\"plain_publication\":false}",
                false
            ),
            None
        );
    }

    /// The run reads its tree through the granted helper, writes its status last, and names the
    /// negative control only for the control row; the aterm it runs is named `aterm`.
    #[test]
    fn the_run_reads_through_the_helper_and_names_the_control_alone() {
        let plain = run_script("/var/tmp/aterm-nux/reveal", false);
        let control = run_script("/var/tmp/aterm-nux/reveal", true);
        for script in [&plain, &control] {
            assert!(script.starts_with(
                "'/var/tmp/aterm-nux/reveal/aterm-lab' reveal-tab --json --ax-helper /usr/local/libexec/aterm-lab-guest"
            ));
            assert!(
                script.contains(
                    " '/var/tmp/aterm-nux/reveal/aterm' > /var/tmp/aterm-nux/reveal.json"
                )
            );
            assert!(script.ends_with("; echo $? > /var/tmp/aterm-nux/reveal.exit"));
            // The agent's plist carries it as text: nothing XML would read as markup.
            assert!(!script.contains('<') && !script.contains('&'));
        }
        assert!(!plain.contains("--plain-publication"));
        assert!(control.contains(" --plain-publication '"));
    }
}
