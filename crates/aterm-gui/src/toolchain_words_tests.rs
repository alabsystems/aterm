// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::upgrade::Agent;
use aterm_messages::{Hold, Instant, MessageCenter, MessageLog, TITLE_CAP, WallStamp};
use std::collections::BTreeMap;

fn file(running_pid: Option<u32>, pass: &str) -> atpkg::progress::ProgressFile {
    atpkg::progress::ProgressFile {
        v: PROGRESS_VERSION,
        pid: running_pid,
        pass: pass.to_string(),
        started_unix: 1_700_000_000,
        generation: None,
        heartbeat_unix: 1_700_000_000,
        overall: atpkg::progress::Overall {
            programs_done: 3,
            programs_total: 10,
            bytes_done: 512_000_000,
            bytes_total: 1_200_000_000,
        },
        queue: vec!["ty".into(), "ay".into()],
        programs: BTreeMap::from([
            (
                "trust".to_string(),
                atpkg::progress::ProgramProgress {
                    phase: Phase::Extract,
                    bytes_done: 120_000_000,
                    bytes_total: 900_000_000,
                    build: Some(5520),
                    bumped: false,
                    error: None,
                },
            ),
            (
                "ty".to_string(),
                atpkg::progress::ProgramProgress {
                    phase: Phase::Queued,
                    bytes_done: 0,
                    bytes_total: 0,
                    build: None,
                    bumped: false,
                    error: None,
                },
            ),
        ]),
        ended_unix: None,
    }
}

fn snap(running: bool) -> crate::PkgProgressSnapshot {
    crate::PkgProgressSnapshot {
        file: file(Some(7), "net"),
        running,
    }
}

fn live_of(words: SnapshotWords) -> (Message, PassId) {
    match words {
        SnapshotWords::Live { message, pass_id } => (*message, pass_id),
        other => panic!("expected a live reading, got {other:?}"),
    }
}

fn fill_of(msg: &Message) -> Option<u16> {
    msg.meter.as_ref().and_then(|m| m.fill_permille)
}

fn stats_of(msg: &Message) -> &str {
    msg.meter.as_ref().map_or("", |m| m.stats.as_str())
}

#[test]
fn every_live_toolchain_row_completes_in_its_finished_form() {
    use crate::messages_host::assert_records_its_finished_words as completes;
    completes(
        &announced("installing 2 program(s) (about 1 GB)"),
        "Installed ALab tools",
    );
    completes(
        &live_of(snapshot_words(Some(&snap(true)), None, false)).0,
        "Installed ALab tools",
    );
    completes(
        &live_of(snapshot_words(Some(&snap(true)), None, true)).0,
        "Updated ALab tools",
    );
    let mut other = snap(true);
    other.file.pass = "seed".into();
    completes(
        &live_of(snapshot_words(Some(&other), None, false)).0,
        "Installed ALab tools",
    );
    let mut newer = snap(true);
    newer.file.v = PROGRESS_VERSION + 1;
    completes(
        &live_of(snapshot_words(Some(&newer), None, false)).0,
        "Installed ALab tools",
    );
}

#[test]
fn every_toolchain_record_carries_the_packages_capsule_and_an_admitted_glyph() {
    let rows = vec![
        announced("installing 2 program(s) (about 1 GB)"),
        deferred("an earlier toolchain pass is still running"),
        not_installed("[packages].exclude narrows the toolset to nothing"),
        unusable("ALab publishes no build for this machine's architecture (x86_64-apple-darwin)"),
        installed("✓ ALab toolchain installed: ay, trust — ready in every aterm tab"),
        ended("the pass finished; 12 ALab program(s) are installed", false),
        failed("partly installed: ay"),
        install_failed("no reason was given"),
        managed_current(
            "claude 2.1.280 (Anthropic latest)",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap(),
        live_of(snapshot_words(Some(&snap(true)), None, false)).0,
    ];
    let machine = machine_settings("universal-control disabled; something-new tuned");
    for m in &machine {
        assert_eq!(m.actions, vec![security()], "{}", m.title);
    }
    let rows: Vec<Message> = rows.into_iter().chain(machine).collect();
    for m in &rows {
        assert_eq!(m.tag, tags::TOOLCHAIN, "{}", m.title);
        if matches!(m.hold, Hold::Live { .. }) {
            assert!(m.actions.is_empty(), "{}: a live row", m.title);
        } else if !m
            .key
            .as_deref()
            .is_some_and(|k| k.starts_with(KEY_MACHINE_PREFIX))
        {
            assert_eq!(m.actions, vec![packages()], "{}", m.title);
        }
        let class = crate::message_reporters::attention(m)
            .unwrap_or_else(|why| panic!("{why}: {}", m.title));
        if matches!(m.hold, Hold::Live { .. }) {
            assert!(m.meter.is_some(), "{}: in flight", m.title);
            assert_eq!(class, crate::message_reporters::Attention::Progress);
        }
        assert!(m.title.chars().count() <= TITLE_CAP, "{}", m.title);
        assert!(
            Glyph::ALLOWED.contains(&m.glyph.ch()) && m.glyph != Glyph::FALLBACK,
            "{}: {:?}",
            m.title,
            m.glyph
        );
    }
    assert_eq!(
        packages(),
        Intent::OpenSettings {
            route: "/packages".into()
        }
    );
    assert_eq!(
        security(),
        Intent::OpenSettings {
            route: crate::native_settings::SettingsRoute::Security
                .path()
                .into()
        }
    );
    for ch in [DOWN, NOTED] {
        assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
    }
}

#[test]
fn a_first_run_that_ended_short_is_a_failure_row() {
    use crate::message_reporters::{Attention, attention};
    let failed = first_run_short(FirstRunShort::Failed, "could not reach the index");
    assert_eq!(
        failed.title, ALAB_INSTALL_FAILED,
        "an install, in the lane's noun"
    );
    assert_eq!(failed.severity, Severity::Warn, "ruling 309");
    assert_eq!(
        failed.severity,
        first_run_short(FirstRunShort::Nothing, "x").severity,
        "one family, one severity"
    );
    assert_eq!(failed.hold, Hold::Default);
    assert_eq!(failed.key.as_deref(), Some(KEY_PASS));
    assert_eq!(
        failed.tag,
        tags::PACKAGES,
        "an outcome, not the lane's live work"
    );
    assert_eq!(failed.actions, vec![packages()]);
    assert_eq!(failed.detail, ["could not reach the index"]);
    assert_eq!(attention(&failed), Ok(Attention::Failure));
    let full = first_run_short(FirstRunShort::Failed, "trust: No space left on device");
    assert_eq!(
        full.detail,
        ["trust: No space left on device"],
        "the cause once, in its own words"
    );
    let refusal = atpkg::flow::FlowError::InsufficientDisk {
        required: 4 << 30,
        available: 3 << 30,
    }
    .to_string();
    let refused = first_run_short(FirstRunShort::Failed, &refusal);
    assert_eq!(refused.detail, [refusal.as_str()]);
    let long = "net pass could not finish: the index at \
                https://packages.example.invalid/alab/index.json answered 503 Service \
                Unavailable three times in a row, and the mirror list had no other \
                entry to try before the deadline ran out";
    assert!(long.chars().count() > 160, "PRECONDITION: past the old cut");
    let whole = first_run_short(FirstRunShort::Failed, long);
    let words = |lines: &[String]| {
        lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        words(&whole.detail),
        long.split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        "every word survives: {:?}",
        whole.detail
    );
    assert!(
        !whole.detail.iter().any(|l| l.contains('\u{2026}')),
        "no cut"
    );
    assert_eq!(
        whole.detail,
        super::failed(long).detail,
        "the row and its record say the same cause"
    );
    let nothing = first_run_short(FirstRunShort::Nothing, "");
    assert_eq!(nothing.title, PACKAGES_NOT_INSTALLED);
    assert_eq!(nothing.severity, Severity::Warn);
    assert!(nothing.detail.is_empty());
}

#[test]
fn an_announcement_opens_the_row_before_any_snapshot() {
    let m = announced("installing 2 program(s) (about 1 GB)");
    assert_eq!(m.title, INSTALLING_ALAB);
    assert_eq!(m.detail, vec!["about 1 GB"]);
    assert_eq!(m.glyph.ch(), DOWN);
    assert_eq!(m.severity, Severity::Info);
    assert_eq!(
        m.hold,
        Hold::Live {
            stale_after: STALE_ANNOUNCE
        }
    );
    assert_eq!(m.key.as_deref(), Some(KEY_PASS));
    let meter = m.meter.as_ref().expect("busy: no bytes yet, no fraction");
    assert_eq!(meter.fill_permille, None, "no bytes yet: no fill");
    assert!(meter.busy, "work in flight moves");
    assert!(m.actions.is_empty(), "work in flight carries no capsule");
    assert_eq!(
        meter.stats, "2 programs \u{00b7} ~1 GB",
        "how big the wait is"
    );
    assert_eq!(
        announced(
            "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)"
        )
        .meter
        .unwrap()
        .stats,
        "10 programs \u{00b7} ~3 GB",
        "terse: no clause on the glass"
    );
    assert_eq!(meter.load, Some(Load::Network));
    assert_eq!(meter.amount, None, "no ETA for a phase of a larger job");
    let unsized_pass = announced("installing 3 ALab program(s)");
    assert_eq!(
        unsized_pass.meter.as_ref().unwrap().stats,
        "3 programs",
        "the (s) of program(s) is not a size"
    );
    assert!(unsized_pass.detail.is_empty(), "no size: no line");
    let bare = announced("installing two programs");
    assert!(bare.detail.is_empty(), "no size in the sentence: no line");
    assert_eq!(bare.meter.map(|m| m.stats), Some(String::new()));
}

#[test]
fn the_first_run_is_one_row_and_every_outcome_is_a_record() {
    let now = Instant::now();
    let stamp = WallStamp { unix_ms: 1 };
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    let a = center.post(announced("installing (about 1 GB)"), stamp, now);
    let (live, _) = live_of(snapshot_words(Some(&snap(true)), None, false));
    let b = center.post(live, stamp, now);
    assert_eq!(
        b.outcome,
        aterm_messages::PostOutcome::Superseded(a.id),
        "the meter takes the announcement's slot"
    );
    let records = [
        installed("\u{2713} ALab toolchain installed: ay, trust"),
        ended("the pass finished", false),
        failed("install failed \u{2014} see Settings \u{25b8} Packages"),
        deferred("an earlier toolchain pass is still running"),
        not_installed("ay: the signature did not verify"),
        managed_current(
            "claude 2.1.280 (Anthropic latest)",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap(),
    ]
    .into_iter()
    .chain(machine_settings("universal-control disabled"));
    for m in records {
        assert_eq!(m.hold, Hold::LogOnly, "{}: a record, never a row", m.title);
        let posted = center.post(m, stamp, now);
        assert!(center.live(posted.id).is_none(), "never live");
    }
    assert_eq!(center.live_rows().count(), 1, "the meter alone is live");
    assert_eq!(center.live_rows().next().map(|l| l.id), Some(b.id));
    assert_eq!(center.log().len(), 9, "every report is on record");
}

#[test]
fn a_lock_wait_timeout_is_a_deferred_record_not_a_failure() {
    let m = deferred(
        "waiting for an earlier package update to finish \u{2014} trying again when it finishes or in 30 s",
    );
    assert_eq!(m.severity, Severity::Info);
    assert_eq!(m.glyph.ch(), NOTED, "a record's mark (ruling 302)");
    assert_eq!(m.hold, Hold::LogOnly);
    assert_eq!(m.key, None, "unkeyed");
    assert!(!m.detail[0].contains("failed"), "{}", m.detail[0]);
    assert!(m.detail[0].contains("trying again when it finishes or in 30 s"));
}

#[test]
fn a_pass_that_left_no_tools_is_never_titled_failed() {
    let m = not_installed("ay: the signature did not verify");
    assert_eq!(m.title, PACKAGES_NOT_INSTALLED);
    assert_eq!(m.severity, Severity::Warn);
    assert_eq!(m.hold, Hold::LogOnly);
    assert_eq!(m.key.as_deref(), Some(KEY_PASS));
    assert_eq!(m.actions, vec![packages()]);
    assert!(!m.title.contains("failed"), "{}", m.title);
    assert_eq!(m.detail, vec!["ay: the signature did not verify"]);
    assert!(not_installed("").detail.is_empty(), "no echo of the title");
    let excluded = unusable(
        "[packages].exclude narrows the toolset to nothing installable on this machine \
         (aarch64-apple-darwin; excluded: ay, clean) \u{2014} shorten it to receive the toolset",
    );
    assert_eq!(excluded.title, PACKAGES_NOT_INSTALLED, "a line to shorten");
    assert_eq!(excluded.severity, Severity::Warn);
}

#[test]
fn an_informational_unusable_is_an_info_record_not_a_failure() {
    for body in [
        "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
         \u{2014} it builds for aarch64-apple-darwin; nothing installs here until one is published",
        "every ALab program available here was removed on this machine (ay, trust) \u{2014} \
         nothing to install. `aterm pkg install <program>` brings one back",
    ] {
        let m = unusable(body);
        assert_eq!(m.severity, Severity::Info, "{body}");
        assert_eq!(m.glyph.ch(), NOTED, "a record's mark");
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        assert_eq!(m.detail.join(" "), body, "atpkg's sentence, whole");
        assert!(!m.title.contains("Couldn't"), "{}", m.title);
    }
}

#[test]
fn the_positive_terminals_carry_no_meter_and_keep_the_sentence() {
    let m = installed(
        "\u{2713} ALab toolchain installed: claude, codex \u{2014} ready in every aterm tab",
    );
    assert_eq!(
        m.detail,
        vec!["claude, codex \u{2014} ready in every aterm tab"],
        "the pill's own opening is dropped"
    );
    assert_eq!(m.severity, Severity::Success);
    assert_eq!(m.meter, None, "a terminal Success record carries no meter");
    assert_eq!(m.hold, Hold::LogOnly, "no success row (2026-09-22)");
    let e = ended("the pass finished; 12 ALab program(s) are installed", false);
    assert_ne!(e.title, m.title, "no claim the pass changed anything");
    assert_eq!(
        ended("the pass finished; 12 ALab program(s) are installed", true).title,
        m.title,
        "a first run was an install"
    );
    assert_eq!(
        e.detail,
        vec!["the pass finished; 12 ALab program(s) are installed"]
    );
    assert_eq!(e.meter, None, "a terminal Success record carries no meter");
    assert_eq!(e.hold, Hold::LogOnly);
    assert!(
        ended("", false).detail.is_empty(),
        "no words from atpkg: the title alone"
    );
    let long = format!("the pass finished; {} are installed", "program ".repeat(30));
    for m in [installed(&long), ended(&long, false), deferred(&long)] {
        let rejoined = m.detail.join(" ");
        let words = |t: &str| {
            t.split_whitespace()
                .map(|w| w.trim_end_matches(';').to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(words(&rejoined), words(&long), "{}", m.title);
        assert!(!rejoined.contains('\u{2026}'), "{rejoined}");
    }
}

#[test]
fn a_failure_is_a_warn_record_with_no_meter() {
    let m = failed("partly installed: ay, trust");
    assert_eq!(m.detail, vec!["partly installed: ay, trust"]);
    assert_eq!(m.meter, None);
    assert_eq!(m.severity, Severity::Warn);
    assert_eq!(m.hold, Hold::LogOnly);
    assert_eq!(m.key.as_deref(), Some(KEY_PASS));
    let hostile = failed("bad\u{1b}[31m thing\u{7}");
    let d = &hostile.detail[0];
    assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
    let long = format!("the child died after announcing: {}", "x".repeat(170));
    let m = failed(&long);
    assert!(m.detail[0].chars().count() > 160, "{}", m.detail[0]);
    assert_eq!(m.detail, vec![long]);
    let m = failed("error: tar exited 2\nno space left on device");
    assert_eq!(
        m.detail,
        vec!["error: tar exited 2", "no space left on device"]
    );
    let cause = format!(
        "net-failed: claude: download failed: {}\u{2014} HTTP 403 from the mirror",
        "the release mirror refused the request ".repeat(8)
    );
    assert!(cause.chars().count() > DETAIL_LINE_CAP);
    let m = failed(&cause);
    assert!(m.detail.len() > 1, "{:?}", m.detail);
    assert!(
        m.detail
            .iter()
            .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
    );
    assert!(
        m.detail
            .last()
            .is_some_and(|l| l.ends_with("HTTP 403 from the mirror")),
        "{:?}",
        m.detail
    );
    assert_eq!(m.detail.join(" "), cause, "every word, in order");
}

#[test]
fn bytes_read_like_a_download_dialog() {
    assert_eq!(fmt_bytes(0), "0 B");
    assert_eq!(fmt_bytes(812_000), "812 KB");
    assert_eq!(fmt_bytes(512_000_000), "512 MB");
    assert_eq!(fmt_bytes(1_200_000_000), "1.2 GB");
    assert_eq!(fmt_bytes(999_600_000), "1.0 GB", "never `1000 MB`");
    assert_eq!(fmt_bytes(999_700), "1 MB", "never `1000 KB`");
    assert_eq!(fmt_bytes(999_400_000), "999 MB");
    assert_eq!(fill_permille(0, 0), None);
    assert_eq!(fill_permille(5, 10), Some(500));
    assert_eq!(fill_permille(20, 10), Some(1000), "clamped to the total");
}

#[test]
fn a_running_snapshot_reports_the_current_program_and_the_rollup() {
    let (m, pass_id) = live_of(snapshot_words(Some(&snap(true)), None, false));
    assert_eq!(m.title, INSTALLING_ALAB);
    assert_eq!(
        m.detail,
        vec!["trust \u{00b7} extracting", "512 MB / 1.2 GB downloaded"],
        "program and phase behind Details, and the download rollup with them"
    );
    assert!(m.actions.is_empty(), "work in flight carries no capsule");
    assert!(
        !m.meter.as_ref().is_some_and(|x| x.busy),
        "a fill is not busy"
    );
    assert_eq!(
        stats_of(&m),
        "3 of 10 programs",
        "the one count that agrees with the bar"
    );
    assert_eq!(
        m.meter.as_ref().and_then(|x| x.load),
        Some(Load::Disk),
        "an extraction loads the disk"
    );
    assert_eq!(
        m.meter.as_ref().and_then(|x| x.amount),
        Some(Amount {
            series: Amount::series_of("toolchain net 1700000000"),
            done: 356,
            total: 1000,
            unit: Unit::Steps,
        }),
        "the fill is the amount: the ETA is the whole pass's own rate"
    );
    for phase in [Phase::Verify, Phase::Link] {
        let mut held = snap(true);
        held.file.programs.get_mut("trust").unwrap().phase = phase;
        let (h, _) = live_of(snapshot_words(Some(&held), None, false));
        let amount = h.meter.as_ref().and_then(|x| x.amount).unwrap();
        assert_eq!(amount.unit, Unit::HeldSteps, "{phase:?}");
        assert_eq!(
            amount.series,
            Amount::series_of("toolchain net 1700000000"),
            "the same series: the estimator keeps its readings"
        );
    }
    let (routine, _) = live_of(snapshot_words(Some(&snap(true)), None, true));
    assert_eq!(routine.title, UPDATING_ALAB);
    assert_eq!(
        fill_of(&m),
        Some(356),
        "three of ten done, trust half-way through its phases (download, then 120 of 900 MB \
         extracted): (3000 + 500 + 65) / 10 — not the download rollup's 427"
    );
    assert_eq!(
        m.hold,
        Hold::Live {
            stale_after: STALE_TAILED
        }
    );
    assert_eq!(m.key.as_deref(), Some(KEY_PASS));
    assert_eq!(pass_id, ("net".to_string(), 1_700_000_000));
}

#[test]
fn a_silent_rollup_changes_nothing_the_programs_measure() {
    let mut f = file(Some(7), "net");
    f.overall.bytes_total = 0;
    f.overall.bytes_done = 0;
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: f.clone(),
            running: true,
        }),
        None,
        false,
    ));
    assert_eq!(m.title, INSTALLING_ALAB);
    assert_eq!(
        fill_of(&m),
        Some(356),
        "programs by phase: a silent byte rollup changes nothing"
    );
    assert_eq!(stats_of(&m), "3 of 10 programs");
    f.pass = "seed".to_string();
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: f,
            running: true,
        }),
        None,
        false,
    ));
    assert_eq!(
        m.title, INSTALLING_ALAB,
        "one noun for every first-run pass"
    );
}

#[test]
fn a_live_meter_never_runs_backwards_within_a_pass() {
    let (first, pass_id) = live_of(snapshot_words(Some(&snap(true)), None, false));
    let peak = (pass_id, fill_of(&first).unwrap());
    let mut dip = file(Some(7), "net");
    dip.overall.bytes_done = 100_000_000;
    dip.programs.get_mut("trust").unwrap().bytes_done = 0;
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: dip.clone(),
            running: true,
        }),
        Some(&peak),
        false,
    ));
    assert_eq!(fill_of(&m), Some(356), "clamped to the pass's own peak");
    assert_eq!(stats_of(&m), "3 of 10 programs");
    assert_eq!(
        m.detail.get(1).map(String::as_str),
        Some("100 MB / 1.2 GB downloaded"),
        "the figures are live"
    );
    dip.started_unix += 1;
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: dip,
            running: true,
        }),
        Some(&peak),
        false,
    ));
    assert_eq!(fill_of(&m), Some(350), "a new pass starts fresh");
}

#[test]
fn a_pass_that_is_over_claims_no_outcome() {
    let mut f = file(None, "net");
    f.ended_unix = Some(1_700_000_100);
    let over = |f: &atpkg::progress::ProgressFile| {
        snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: false,
            }),
            Some(&(("net".to_string(), 1_700_000_000), 427)),
            false,
        )
    };
    assert_eq!(over(&f), SnapshotWords::Over, "ended cleanly");
    f.programs.get_mut("trust").unwrap().phase = Phase::Failed;
    assert_eq!(over(&f), SnapshotWords::Over, "ended with a failure");
    f.ended_unix = None;
    assert_eq!(over(&f), SnapshotWords::Over, "the writer is gone");
    let mut newer = f.clone();
    newer.v = PROGRESS_VERSION + 1;
    assert_eq!(over(&newer), SnapshotWords::Over, "any schema");
    assert_eq!(snapshot_words(None, None, false), SnapshotWords::Vanished);
    let mut nothing = file(Some(7), "net");
    nothing.overall.programs_total = 0;
    nothing.programs.clear();
    nothing.queue.clear();
    assert_eq!(
        snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: nothing.clone(),
                running: true
            }),
            None,
            false
        ),
        SnapshotWords::Quiet
    );
    assert_eq!(
        snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: nothing,
                running: false
            }),
            None,
            false
        ),
        SnapshotWords::Over
    );
    let mut newer = file(Some(7), "net");
    newer.v = PROGRESS_VERSION + 1;
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: newer,
            running: true,
        }),
        None,
        false,
    ));
    assert_eq!(m.title, INSTALLING_ALAB, "the title alone");
    assert!(m.detail.is_empty(), "{:?}", m.detail);
    assert!(m.meter.as_ref().is_some_and(|m| m.busy));
}

#[test]
fn phase_words_say_up_to_date_and_keep_scheduling_off_the_row() {
    let mut f = file(Some(7), "net");
    for row in f.programs.values_mut() {
        row.bumped = true;
    }
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: f.clone(),
            running: true,
        }),
        None,
        false,
    ));
    assert!(
        !m.detail
            .iter()
            .any(|d| d.contains("bumped") || d.contains("you asked")),
        "{:?}",
        m.detail
    );
    let mut skipped = file(Some(7), "net");
    let name = skipped.queue[0].clone();
    for (key, row) in skipped.programs.iter_mut() {
        row.phase = if *key == name {
            Phase::Skipped
        } else {
            Phase::Done
        };
    }
    let (line, _) = current_program_line(&skipped);
    assert_eq!(line, format!("{name} \u{00b7} up to date"));
}

#[test]
fn a_download_waiting_for_the_pass_yields_to_the_program_in_hand() {
    let program = |phase, bytes_done, bytes_total| atpkg::progress::ProgramProgress {
        phase,
        bytes_done,
        bytes_total,
        build: None,
        bumped: false,
        error: None,
    };
    let mut f = file(Some(7), "net");
    f.programs = BTreeMap::from([
        ("ay".to_string(), program(Phase::Download, 9, 9)),
        ("trust".to_string(), program(Phase::Extract, 1, 4)),
    ]);
    assert_eq!(current_program_line(&f).0, "trust \u{00b7} extracting");
    f.programs
        .insert("ty".to_string(), program(Phase::Download, 3, 9));
    assert_eq!(current_program_line(&f).0, "ty \u{00b7} downloading");
}

#[test]
fn hostile_strings_are_sanitized_before_they_become_words() {
    let mut f = file(Some(7), "net");
    f.programs.clear();
    f.queue = vec!["../evil".into()];
    f.programs.insert(
        "../evil".into(),
        atpkg::progress::ProgramProgress {
            phase: Phase::Download,
            bytes_done: 1,
            bytes_total: 2,
            build: None,
            bumped: false,
            error: None,
        },
    );
    let (m, _) = live_of(snapshot_words(
        Some(&crate::PkgProgressSnapshot {
            file: f,
            running: true,
        }),
        None,
        false,
    ));
    assert_eq!(
        m.detail,
        ["512 MB / 1.2 GB downloaded"],
        "no program line: only the byte rollup, which carries no name"
    );
    for m in [
        announced("installing (bad\u{1b}[31m size\u{7})"),
        failed("bad\u{1b}[31m thing\u{7}"),
    ] {
        let d = &m.detail[0];
        assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
    }
    let n = appnotice(tags::TOOLCHAIN, "hello\u{1b}[2J from the terminal");
    assert_eq!(n.title, "hello[2J from the terminal", "sanitized");
    assert_eq!(n.severity, Severity::Info);
    assert_eq!(n.hold, Hold::LogOnly, "free text is a record");
    assert_eq!(n.key, None);
    assert!(n.actions.is_empty(), "a wire notice authors no capsule");
    assert_eq!(appnotice(tags::UPDATE, "x").tag, tags::UPDATE);
}

#[test]
fn a_program_the_pass_could_not_check_is_named_and_no_success() {
    let words = |wire: &str| managed_current(wire, 0, true, HookDialect::Zsh).unwrap();
    let m = words("claude 2.1.287 (not checked); codex 0.160.0 (OpenAI latest)");
    assert!(
        m.title.contains("Claude Code 2.1.287 not checked") && m.title.contains("Codex CLI"),
        "both programs are named: {}",
        m.title
    );
    assert_eq!(m.severity, Severity::Info, "no success");
    assert!(!m.detail[0].contains("doctor"), "{:?}", m.detail);
    let m = words("claude 2.1.287 (failed); codex 0.160.0 (held)");
    assert!(
        m.title.contains("failed") && m.title.contains("held"),
        "{}",
        m.title
    );
    assert!(m.detail[0].contains("`aterm pkg doctor`"), "{:?}", m.detail);
    let m = words("claude 2.1.287 (not checked); codex 0.160.0 (held)");
    assert!(
        m.detail[0].contains("`aterm pkg doctor`"),
        "doctor is named for the held one: {:?}",
        m.detail
    );
    assert_eq!(
        managed_version("claude 2.1.287 (not checked)", "claude").as_deref(),
        Some("2.1.287"),
        "its build is still the one the rows are counted against"
    );
    let m = words("claude 2.1.287 (Anthropic latest); codex 0.160.0 (OpenAI latest)");
    assert_eq!(m.severity, Severity::Success);
}

#[test]
fn four_agents_fit_the_title_whole() {
    let title = |[a, b, c, d]: [&str; 4]| {
        let wire = format!(
            "claude 2.1.287 ({a}); codex 0.160.0 ({b}); agy 1.3.6 ({c}); muse 1.4.9-R6297.1 ({d})"
        );
        let title = managed_current(&wire, 0, true, HookDialect::Zsh)
            .unwrap()
            .title;
        assert!(
            title.chars().count() <= TITLE_CAP && !title.contains('\u{2026}'),
            "{title}"
        );
        title
    };
    title(["not checked"; 4]);
    title(["failed"; 4]);
    title(["Anthropic latest", "not checked", "held", "failed"]);
    title(["Vendor latest"; 4]);
}

#[test]
fn the_managed_and_machine_records_carry_their_words() {
    let m = managed_current(
        "claude 2.1.267 (build 2026091001)",
        0,
        true,
        HookDialect::Zsh,
    )
    .unwrap();
    assert!(m.title.contains("2.1.267"), "{}", m.title);
    assert!(
        !m.detail[0].contains("2026091001"),
        "an older atpkg's wire still parses, its build unrepeated: {:?}",
        m.detail
    );
    assert_eq!(m.hold, Hold::LogOnly, "a record on every pass, never a row");
    assert_eq!(m.meter, None, "a record carries no meter");
    assert_eq!(m.severity, Severity::Success);
    assert_eq!(m.key.as_deref(), Some(KEY_MANAGED));

    let three = managed_current(
        "claude 2.1.280 (Anthropic latest); codex 0.156.1 (OpenAI latest); agy 1.0",
        0,
        true,
        HookDialect::Zsh,
    )
    .unwrap();
    for name in ["2.1.280", "0.156.1", "Antigravity CLI 1.0"] {
        assert!(three.title.contains(name), "{name}: {}", three.title);
    }
    assert!(!three.title.contains("latest") && !three.detail[0].contains("latest"));
    let unknown = managed_current("nosuchagent 1.0", 0, true, HookDialect::Zsh).unwrap();
    assert!(
        unknown.title.contains("nosuchagent 1.0"),
        "a name no vendor spec knows is said as the wire spells it: {}",
        unknown.title
    );
    assert!(
        managed_current("  ;  ", 0, true, HookDialect::Zsh).is_none(),
        "an empty body records nothing"
    );

    let rows = machine_settings("universal-control disabled");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, Severity::Info);
    assert_eq!(
        rows[0].glyph.ch(),
        NOTED,
        "from the band's closed set — ⚙ rendered blank headless"
    );
    assert_eq!(rows[0].hold, Hold::LogOnly);
    assert_eq!(
        rows[0].key.as_deref(),
        Some("toolchain.machine.universal-control")
    );
}

#[test]
fn the_managed_record_says_installed_while_sessions_lag_and_spares_agent_tabs() {
    let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
    let words =
        |tabs: TabsBehind| managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap();
    let remedy = HookDialect::Zsh.remedy().expect("zsh has a hook");
    let (current, plain) = words(TabsBehind::default());
    let (moving, detail) = words(TabsBehind {
        agents_behind: PerAgent::of(&[(Agent::Claude, 2)]),
        ..TabsBehind::default()
    });
    assert_ne!(
        moving, current,
        "lagging sessions turn current into installed"
    );
    assert!(detail.contains('2'), "{detail}");
    let (_, detail) = words(TabsBehind {
        frozen_agents: 1,
        ..TabsBehind::default()
    });
    assert!(detail.contains(&remedy), "{detail}");
    assert!(
        !detail.ends_with(&remedy),
        "an agent tab is told to wait: {detail}"
    );
    assert!(!detail.contains("every tab"), "{detail}");
    let (title, detail) = words(TabsBehind {
        frozen_shells: 1,
        frozen_agents: 1,
        agents_behind: PerAgent::of(&[(Agent::Claude, 1)]),
        ..TabsBehind::default()
    });
    assert_eq!(title, moving);
    assert!(
        detail.ends_with(&remedy),
        "the shell remedy is said last: {detail}"
    );
    assert_eq!(plain, "ready in every aterm tab");
}

#[test]
fn each_agent_is_counted_on_its_own_product_in_alls_order() {
    let mut each = PerAgent::default();
    for (n, agent) in Agent::ALL.into_iter().enumerate() {
        for _ in 0..=n {
            each.count(agent);
        }
    }
    let said: Vec<(&str, usize)> = each.each().collect();
    let expected: Vec<(&str, usize)> = Agent::ALL
        .into_iter()
        .enumerate()
        .map(|(n, agent)| (agent.product(), n + 1))
        .collect();
    assert_eq!(said, expected);
    assert_eq!(said[0].0, "Claude Code");
    assert_eq!(each.total(), expected.iter().map(|(_, n)| n).sum::<usize>());
    assert_eq!(PerAgent::default().total(), 0, "the negative control");
}

#[test]
fn each_lagging_product_is_named_with_its_own_count() {
    let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
    let words =
        |tabs: TabsBehind| managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap();
    let (title, detail) = words(TabsBehind {
        agents_behind: PerAgent::of(&[(Agent::Claude, 1), (Agent::Codex, 2)]),
        agents_held: PerAgent::of(&[(Agent::Codex, 1)]),
        ..TabsBehind::default()
    });
    assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
    assert_eq!(
        detail,
        "ready in every aterm tab · 1 running Claude Code session moves onto it on its own · \
         2 running Codex sessions move onto it on their own · 1 running Codex session is \
         still on the old build (`aterm harness upgrade --status` says why)"
    );
    let (title, detail) = words(TabsBehind {
        agents_behind: PerAgent::of(&[(Agent::Codex, 1)]),
        ..TabsBehind::default()
    });
    assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
    assert_eq!(
        detail,
        "ready in every aterm tab · 1 running Codex session moves onto it on its own"
    );
    let (_, detail) = words(TabsBehind {
        agents_behind: PerAgent::of(&[(Agent::Claude, 1)]),
        ..TabsBehind::default()
    });
    assert!(!detail.contains("Codex"), "{detail}");
}

#[test]
fn a_held_session_is_not_said_to_move_and_a_shadowed_tab_is_not_from_before() {
    let wire = "claude 2.1.282 (Anthropic latest)";
    let words =
        |tabs: TabsBehind| managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap();
    let (current, _) = words(TabsBehind::default());
    let (title, detail) = words(TabsBehind {
        agents_held: PerAgent::of(&[(Agent::Claude, 1)]),
        ..TabsBehind::default()
    });
    assert_ne!(
        title, current,
        "a held session keeps the title at installed"
    );
    assert!(
        detail.contains("`aterm harness upgrade --status`"),
        "{detail}"
    );
    let (title, detail) = words(TabsBehind {
        shadowed: 2,
        ..TabsBehind::default()
    });
    assert_eq!(title, current);
    assert!(!detail.contains("every aterm tab"), "{detail}");
    assert!(!detail.contains("00-atpkg"), "no hook to type: {detail}");
    assert_eq!(
        managed_version("claude 2.1.282 (Anthropic latest); codex 0.157.0", "claude").as_deref(),
        Some("2.1.282")
    );
    assert_eq!(
        managed_version("codex 0.157.0 (OpenAI latest)", "claude"),
        None
    );
}

#[test]
fn the_frozen_tab_remedy_is_the_hook_in_the_spawn_shells_dialect() {
    use aterm_core::shell_integration::ShellType;
    let wire = "claude 2.1.273 (build 2026091601)";
    let note = |shell: HookDialect| {
        let (_, detail) = managed_current_words(wire, 1, true, shell).unwrap();
        detail.split(" \u{00b7} ").last().unwrap().to_string()
    };
    assert_eq!(
        note(HookDialect::Zsh),
        "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
    );
    assert_eq!(
        note(HookDialect::Bash),
        "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.bash`"
    );
    assert_eq!(
        note(HookDialect::Fish),
        "1 tab from before this update picks them up with `source ~/.aterm/shell.d/00-atpkg.fish`"
    );
    assert_eq!(
        note(HookDialect::PowerShell),
        "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.ps1`"
    );
    assert_eq!(
        note(HookDialect::None),
        "1 tab from before this update: a new tab picks them up"
    );
    assert_eq!(HookDialect::default(), HookDialect::Zsh);
    assert_eq!(HookDialect::from(ShellType::Bash), HookDialect::Bash);
    assert_eq!(HookDialect::from(ShellType::Fish), HookDialect::Fish);
    assert_eq!(
        HookDialect::from(ShellType::PowerShell),
        HookDialect::PowerShell
    );
    assert_eq!(HookDialect::from(ShellType::Unknown), HookDialect::None);
    assert_eq!(HookDialect::from(ShellType::Cmd), HookDialect::None);
    assert_eq!(HookDialect::from(ShellType::Wsl), HookDialect::None);
}

#[test]
fn the_managed_row_stops_claiming_this_tab_when_integration_failed() {
    let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
    let (_, detail) = managed_current_words(wire, 0, false, HookDialect::Zsh).unwrap();
    assert!(!detail.contains("every"), "{detail}");
    let (_, detail) = managed_current_words(wire, 1, false, HookDialect::Zsh).unwrap();
    assert!(!detail.contains("every"), "{detail}");
    assert!(
        detail.ends_with(&HookDialect::Zsh.remedy().expect("zsh has a hook")),
        "{detail}"
    );
    let (_, detail) = managed_current_words(wire, 0, true, HookDialect::Zsh).unwrap();
    assert_eq!(detail, "ready in every aterm tab");
}

#[test]
fn the_seed_pill_keeps_every_tab_promise_it_makes() {
    use crate::spawn::ShellIntegrationOutcome as Si;
    const PREFIX: &str = "\u{2713} ALab toolchain installed: ";
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
    let rosters = [
        names(&["ay"]),
        names(&["ay", "trust"]),
        names(&["ay", "clean", "ny", "trust", "ty"]),
        names(&["ay", "clean", "ny", "trust", "ty", "nn"]),
        names(&["claude"]),
        names(&["claude", "codex"]),
        names(&["ay", "claude", "trust"]),
    ];
    let outcomes = [
        None,
        Some(Si::Prepared),
        Some(Si::WriteFailed("read-only cache".into())),
        Some(Si::UnknownShell("nu".into())),
    ];
    let dialects = [
        HookDialect::Zsh,
        HookDialect::Bash,
        HookDialect::Fish,
        HookDialect::PowerShell,
        HookDialect::None,
    ];
    let mut checked = 0;
    for roster in &rosters {
        let shown = if roster.len() > 5 {
            format!("{}\u{2026}", roster[..5].join(", "))
        } else {
            roster.join(", ")
        };
        let agents_only = roster.iter().all(|n| atpkg::stub::is_agent_program(n));
        for outcome in &outcomes {
            let failed = matches!(outcome, Some(Si::WriteFailed(_) | Si::UnknownShell(_)));
            for frozen in [0usize, 1, 2] {
                for hook in dialects {
                    let pill = seed_pill_text(roster, outcome.as_ref(), frozen, hook);
                    let case = format!("{roster:?} {outcome:?} frozen={frozen} {hook:?}: {pill}");
                    checked += 1;
                    assert!(
                        pill.starts_with(&format!("{PREFIX}{shown} \u{2014} ")),
                        "{case}"
                    );
                    let lower = pill.to_lowercase();
                    assert!(
                        !lower.contains("exec") && !lower.contains("restart"),
                        "{case}"
                    );
                    if failed {
                        let clause = unhooked_clause(outcome.as_ref()).expect("a cause");
                        assert!(pill.ends_with(&format!(" \u{2014} {clause}")), "{case}");
                        assert!(!pill.contains("ready in"), "{case}");
                        continue;
                    }
                    if frozen == 0 {
                        assert!(
                            pill.ends_with(" \u{2014} ready in every aterm tab"),
                            "{case}"
                        );
                        continue;
                    }
                    assert!(
                        pill.contains(" \u{2014} ready in every tab opened since this update"),
                        "{case}"
                    );
                    if agents_only {
                        assert!(!pill.contains(" \u{b7} "), "no remedy clause: {case}");
                        assert!(!lower.contains("new tab"), "{case}");
                        continue;
                    }
                    let tabs = if frozen == 1 { "1 tab" } else { "2 tabs" };
                    assert!(
                        pill.contains(&format!(" \u{b7} {tabs} from before it")),
                        "{case}"
                    );
                    let record =
                        managed_current("claude 2.1.273 (build 2026091601)", frozen, true, hook)
                            .expect("the managed record posts");
                    let record_note = record.detail[0]
                        .rsplit(" \u{00b7} ")
                        .next()
                        .expect("the note is the last clause")
                        .to_string();
                    match hook.remedy() {
                        Some(command) => {
                            let verb = if frozen == 1 { "picks" } else { "pick" };
                            let tail = format!(" {verb} them up with {command}");
                            assert!(pill.ends_with(&tail), "{case}");
                            assert!(record_note.ends_with(&tail), "{hook:?}: {record_note}");
                            assert!(!lower.contains("new tab"), "{case}");
                        }
                        None => {
                            let tail = ": a new tab picks them up";
                            assert!(pill.ends_with(tail), "{case}");
                            assert!(record_note.ends_with(tail), "{hook:?}: {record_note}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, rosters.len() * outcomes.len() * 3 * dialects.len());
    let tools = names(&["ay", "trust"]);
    let pill = seed_pill_text(&tools, None, 1, HookDialect::Zsh);
    let record = installed(&pill);
    assert_eq!(record.hold, Hold::LogOnly, "no success row");
    assert_eq!(
        Some(record.detail[0].as_str()),
        pill.strip_prefix(PREFIX),
        "the record keeps the pill whole after its opening"
    );
    assert!(
        HookDialect::Zsh
            .remedy()
            .unwrap()
            .contains(atpkg::hooks::HOOK_BASENAME)
    );
}

#[test]
fn machine_settings_record_one_entry_per_item_in_wire_order() {
    let rows =
        machine_settings("older-thing done; something-new tuned; universal-control disabled");
    assert_eq!(
        rows[0].title, "older-thing done",
        "an unknown item, verbatim"
    );
    let keys: Vec<&str> = rows.iter().map(|m| m.key.as_deref().unwrap()).collect();
    assert_eq!(
        keys,
        [
            "toolchain.machine.older-thing",
            "toolchain.machine.something-new",
            "toolchain.machine.universal-control",
        ],
        "each item keyed on atpkg's own word for it"
    );
    for m in &rows {
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(m.glyph.ch(), NOTED, "a record's mark (ruling 302)");
    }
    assert!(rows[1].detail.is_empty());
    assert!(
        rows[2].detail[0].ends_with(atpkg::machine::UNIVERSAL_CONTROL_REVERT),
        "the command whole, last"
    );
    assert_eq!(rows[2].actions, vec![security()], "where the switch is");
    assert!(
        machine_settings(" ; ").is_empty(),
        "an empty body raises nothing"
    );
}

#[test]
fn byte_stats_keep_one_width_for_the_life_of_a_transfer() {
    for total in [
        500u64,
        1_000,
        50_000,
        99_500,
        999_500,
        74_000_000,
        200_000_000,
        1_200_000_000,
    ] {
        let width = byte_stats(total, total).chars().count();
        for done in [0u64, 999, 1_000, 99_499, 99_500, 999_000, 999_499, 999_500]
            .into_iter()
            .filter(|&d| d <= total)
            .chain((0..=total).step_by((total / 80).max(1) as usize))
        {
            assert_eq!(
                byte_stats(done, total).chars().count(),
                width,
                "{done} of {total}"
            );
        }
    }
    assert_eq!(
        byte_stats(900_000_000, 74_000_000),
        byte_stats(74_000_000, 74_000_000),
        "clamped"
    );
}

#[test]
fn pass_load_names_the_heaviest_active_phase() {
    let program = |phase| atpkg::progress::ProgramProgress {
        phase,
        bytes_done: 0,
        bytes_total: 0,
        build: None,
        bumped: false,
        error: None,
    };
    let with = |phases: &[Phase]| {
        let mut f = file(Some(7), "net");
        f.programs = phases
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("p{i}"), program(*p)))
            .collect();
        pass_load(&f)
    };
    assert_eq!(with(&[Phase::Download, Phase::Extract]), Some(Load::Disk));
    assert_eq!(with(&[Phase::Download, Phase::Link]), Some(Load::Disk));
    assert_eq!(with(&[Phase::Download, Phase::Verify]), Some(Load::Cpu));
    assert_eq!(with(&[Phase::Download, Phase::Queued]), Some(Load::Network));
    assert_eq!(
        with(&[Phase::Queued, Phase::Done]),
        None,
        "nothing in flight"
    );
}
