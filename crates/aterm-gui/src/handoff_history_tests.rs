// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The history carry end to end on real engines: the export with the session
//! live, the park's checkpoint, the join, the sidecar crossing, and the import
//! after Commit, plus every fallback, each of which must count what it leaves
//! behind.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::{Terminal, TerminalCheckpoint};

use super::*;
use crate::seamless::ladder_tests::link_dense;
use crate::session_store::{SessionHandoff, SessionRecord};

const NONCE: &str = "00112233445566778899aabbccddeeff";

fn write_lines(t: &mut Terminal, prefix: &str, from: usize, count: usize) {
    for i in from..from + count {
        t.process(format!("{prefix}{i}\r\n").as_bytes());
    }
}

/// The main grid's scrollback as trimmed text, oldest first.
pub(crate) fn history(t: &Terminal) -> Vec<String> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn record(local_id: u64) -> SessionRecord {
    SessionRecord {
        local_id,
        sid: format!("s-{local_id:04x}"),
        parent: None,
        state: "alive".to_string(),
        title: String::new(),
        screen: None,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: None,
        land_at: None,
        control: None,
        frozen_path: false,
        topics: Vec::new(),
        fg_holder: None,
        rekey: false,
        loader: false,
        history: None,
        history_dropped: 0,
        history_withheld: false,
        history_lost: 0,
        hold: None,
        more_holds: Vec::new(),
        supervisor: None,
        claim_known: false,
        attention_owners: Vec::new(),
        viewport_from_bottom: None,
        render_debt: None,
        render_debt_cut: 0,
        fabric: Default::default(),
        timeline_id: 0,
        log_ids: None,
        log_seq: 0,
        journal_key: None,
        task: None,
        status_seen: None,
        autopilot_end: None,
    }
}

fn manifest(ids: &[u64]) -> SessionHandoff {
    SessionHandoff {
        resources: None,
        schema: SessionHandoff::SCHEMA,
        sessions: ids.iter().map(|id| record(*id)).collect(),
        window: None,
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
        held: Vec::new(),
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
    }
}

/// A live session as the outgoing process holds it.
fn live(rows: u16, cols: u16) -> Arc<Mutex<Terminal>> {
    Arc::new(Mutex::new(Terminal::new(rows, cols)))
}

/// The export, run to completion (it has nothing to wait on here).
fn export(dir: &Path, sessions: &[(u64, &Arc<Mutex<Terminal>>)]) -> ExportResults {
    let sessions: Vec<_> = sessions
        .iter()
        .map(|(id, term)| (*id, Arc::clone(term)))
        .collect();
    let results = HistoryExporter::start(dir.to_path_buf(), sessions, false)
        .expect("the export worker starts")
        .finish(aterm_spec::wait::MUST_HAPPEN);
    assert!(!results.unfinished, "the export finished");
    results
}

/// The park: the session's screen carry (at most 256 lines of history, as
/// the capture ladder takes it) and its history head, under one lock.
fn park(local_id: u64, term: &Arc<Mutex<Terminal>>) -> (TerminalCheckpoint, HistoryHead) {
    let t = term.lock().unwrap();
    let checkpoint = t.checkpoint_carry(256).expect("a Ground parser");
    (checkpoint, capture_head(local_id, &t))
}

/// The successor's adopt, through the real seam
/// (`spawn::hydrate_adopted_engine`): a fresh engine restored from the
/// checkpoint, the import's keys reserved under the same lock, the claim kept
/// on `history` for the import.
fn adopt(checkpoint: &TerminalCheckpoint, history: &mut AdoptedHistory) -> Arc<Mutex<Terminal>> {
    let term = Arc::new(Mutex::new(Terminal::new(checkpoint.rows, checkpoint.cols)));
    crate::spawn::hydrate_adopted_engine(&term, Some(checkpoint), None, None, None, 1, history);
    term
}

fn named(dir: &Path, local_id: u64) -> PathBuf {
    dir.join(format!(
        "seamless-{}-{NONCE}.s{local_id}.hist",
        std::process::id()
    ))
}

fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Run the outgoing half for one session and return what the successor sees.
struct Crossed {
    parent_history: Vec<String>,
    checkpoint: TerminalCheckpoint,
    manifest: SessionHandoff,
    joined: Joined,
}

fn cross(dir: &Path, term: &Arc<Mutex<Terminal>>, between: impl FnOnce(&mut Terminal)) -> Crossed {
    let results = export(dir, &[(1, term)]);
    between(&mut term.lock().unwrap());
    let (checkpoint, head) = park(1, term);
    let parent_history = history(&term.lock().unwrap());
    let mut manifest = manifest(&[1]);
    let verdicts = stamp_manifest(
        &mut manifest,
        &[(1, checkpoint.clone())],
        &[head],
        results,
        dir,
        NONCE,
    );
    assert_eq!(verdicts.len(), 1);
    Crossed {
        parent_history,
        checkpoint,
        manifest,
        joined: verdicts[0].1,
    }
}

/// A live session 3000 lines deep, its view scrolled 1000 lines back.
fn scrolled_back_session() -> Arc<Mutex<Terminal>> {
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
    term
}

/// The successor opens session 1's sidecar in `dir` for `record`, before the
/// proof, with the whole aggregate budget to spend.
fn open_sidecar(dir: &Path, record: &SessionRecord) -> AdoptedHistory {
    let mut remaining = MAX_AGGREGATE_BYTES;
    incoming(record, false, false, &named(dir, 1), dir, &mut remaining)
}

/// The import after Commit of `history` into `term`, as session 1.
fn import_one(term: &Arc<Mutex<Terminal>>, history: AdoptedHistory) -> ImportReport {
    run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(term),
        history,
    }])
    .remove(0)
}

/// An adopted session scrolled 1000 lines back at the park, adopted and ready
/// for its import: the successor and its history.
fn scrolled_back_adoption(dir: &Path) -> (Arc<Mutex<Terminal>>, AdoptedHistory) {
    let term = scrolled_back_session();
    let crossed = cross(dir, &term, |_| {});
    let mut adopted = open_sidecar(dir, &crossed.manifest.sessions[0]);
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    (successor, adopted)
}

/// The successor's whole half for session 1: open before the proof, adopt,
/// import after Commit.
fn receive(dir: &Path, crossed: &Crossed) -> (Arc<Mutex<Terminal>>, ImportReport) {
    let mut adopted = open_sidecar(dir, &crossed.manifest.sessions[0]);
    assert!(
        !named(dir, 1).exists(),
        "the successor unlinks the sidecar as it opens it"
    );
    let term = adopt(&crossed.checkpoint, &mut adopted);
    let report = import_one(&term, adopted);
    (term, report)
}

#[test]
fn stamps_round_trip_and_a_malformed_one_is_refused() {
    let sha = [0xabu8; 32];
    let s = stamp(4096, &sha, 777);
    assert_eq!(parse_stamp(&s), Some((4096, sha, 777)));
    for bad in [
        "",
        "4096",
        "4096 abab 7",
        &s.replace("ab", "AB"),
        &format!("{s} extra"),
        &s.replacen(' ', "  ", 1),
    ] {
        assert_eq!(parse_stamp(bad), None, "{bad:?}");
    }
}

#[test]
fn a_whole_history_crosses_the_handoff_in_order_and_nothing_is_counted_lost() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    let crossed = cross(dir.path(), &term, |_| {});
    assert!(
        crossed.parent_history.len() > 2900,
        "a deep history, far past the screen carry's 256 lines"
    );
    assert_eq!(crossed.joined.fallback, None);
    assert_eq!(crossed.joined.dropped, 0);
    assert_eq!(
        crossed.joined.take as usize,
        crossed.parent_history.len() - 256,
        "the sidecar carries exactly what the checkpoint does not"
    );
    let record = &crossed.manifest.sessions[0];
    assert!(record.history.is_some(), "the record names the sidecar");
    assert_eq!((record.history_dropped, record.history_lost), (0, 0));
    assert!(
        named(dir.path(), 1).exists(),
        "linked to its attempt-bound name"
    );
    assert_eq!(
        dir_entries(dir.path()),
        vec![
            named(dir.path(), 1)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ],
        "the export's own name is gone; only the attempt's remains"
    );

    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.failed, None);
    assert_eq!(report.lost(), 0);
    assert_eq!(report.imported, crossed.joined.take);
    assert_eq!(
        history(&successor.lock().unwrap()),
        crossed.parent_history,
        "the successor holds the parent's history, line for line"
    );
    assert!(
        dir_entries(dir.path()).is_empty(),
        "nothing is left on disk"
    );
}

#[test]
fn a_rewrap_after_the_export_breaks_the_fence_and_the_loss_is_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 1000);
    let crossed = cross(dir.path(), &term, |t| t.resize(8, 33));
    assert_eq!(crossed.joined.fallback, Some(Fallback::FenceMoved));
    assert_eq!(crossed.joined.take, 0);
    let carried = carried_history(&crossed.checkpoint);
    assert_eq!(
        crossed.joined.dropped,
        crossed.parent_history.len() as u64 - carried,
        "every line the checkpoint does not carry is counted"
    );
    let record = &crossed.manifest.sessions[0];
    assert_eq!(
        record.history, None,
        "no sidecar is named for a moved history"
    );
    assert_eq!(record.history_dropped, crossed.joined.dropped);
    assert_eq!(record.history_lost, crossed.joined.dropped);
    assert!(
        dir_entries(dir.path()).is_empty(),
        "the unused export is removed"
    );
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.dropped, crossed.joined.dropped);
    assert_eq!(report.failed_lines, 0);
    assert_eq!(
        history(&successor.lock().unwrap()).len() as u64,
        carried,
        "today's bounded carry, exactly"
    );
}

#[test]
fn a_scrollback_clear_after_the_export_breaks_the_fence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "secret", 0, 500);
    let crossed = cross(dir.path(), &term, |t| {
        t.process(b"\x1b[3J");
        write_lines(t, "after", 0, 20);
    });
    assert_eq!(crossed.joined.fallback, Some(Fallback::FenceMoved));
    let (successor, _) = receive(dir.path(), &crossed);
    // The rows still on screen at the clear (the last seven) scroll into the
    // new history like any other; every row that was scrollback is gone.
    let resurrected: Vec<String> = history(&successor.lock().unwrap())
        .into_iter()
        .filter(|line| {
            line.strip_prefix("secret")
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| n < 490)
        })
        .collect();
    assert!(
        resurrected.is_empty(),
        "a history the user cleared never comes back from a sidecar: {resurrected:?}"
    );
}

#[test]
fn a_sidecar_that_fails_its_digest_costs_its_lines_and_says_why() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2000);
    let crossed = cross(dir.path(), &term, |_| {});
    // Same length, one byte flipped past the header.
    let path = named(dir.path(), 1);
    let mut bytes = std::fs::read(&path).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x20;
    std::fs::write(&path, &bytes).unwrap();
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.imported, 0);
    assert_eq!(
        report.failed_lines, crossed.joined.take,
        "exactly the carry's lines"
    );
    assert!(
        report
            .failed
            .as_deref()
            .is_some_and(|why| why.contains("sha") || why.contains("decode")),
        "{:?}",
        report.failed
    );
    assert_eq!(
        history(&successor.lock().unwrap()).len(),
        256,
        "the checkpoint's own lines stay"
    );
}

#[test]
fn a_sidecar_of_another_length_is_refused_before_the_proof() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 1000);
    let crossed = cross(dir.path(), &term, |_| {});
    let path = named(dir.path(), 1);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.pop();
    std::fs::write(&path, &bytes).unwrap();
    let mut remaining = MAX_AGGREGATE_BYTES;
    let adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        false,
        &path,
        dir.path(),
        &mut remaining,
    );
    assert!(adopted.carry.is_none());
    assert_eq!(adopted.dropped, crossed.joined.take);
    assert_eq!(
        adopted.lost, crossed.joined.take,
        "counted onto the session"
    );
    assert!(!path.exists(), "removed on refusal too");
}

#[test]
fn a_screen_this_build_refused_drops_its_sidecar_and_counts_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 1000);
    let crossed = cross(dir.path(), &term, |_| {});
    let path = named(dir.path(), 1);
    let mut remaining = MAX_AGGREGATE_BYTES;
    let adopted = incoming(
        &crossed.manifest.sessions[0],
        true,
        false,
        &path,
        dir.path(),
        &mut remaining,
    );
    assert!(adopted.carry.is_none());
    assert_eq!(adopted.lost, crossed.joined.take);
    assert!(!path.exists());
}

#[test]
fn a_tab_on_the_alternate_screen_carries_its_shells_whole_history() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    {
        let mut t = term.lock().unwrap();
        write_lines(&mut t, "shell", 0, 800);
        t.process(b"\x1b[?1049h");
        write_lines(&mut t, "tui", 0, 30);
    }
    let crossed = cross(dir.path(), &term, |_| {});
    assert_eq!(
        carried_history(&crossed.checkpoint),
        0,
        "the screen carry holds none of the saved primary's history"
    );
    assert_eq!(crossed.joined.take as usize, crossed.parent_history.len());
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.lost(), 0);
    let mut successor = successor.lock().unwrap();
    successor.process(b"\x1b[?1049l");
    assert_eq!(
        history(&successor),
        crossed.parent_history,
        "the shell's history is there when the program exits"
    );
}

#[test]
fn a_history_deeper_than_one_export_keeps_the_newest_and_counts_the_rest() {
    // The bound is structural; exercise the join's arithmetic on it directly.
    let head = HistoryFence {
        renumber_epoch: 0,
        clear_gen: 0,
        reveal_gen: 0,
        cols: 80,
        oldest: 0,
        end: MAX_EXPORT_LINES + 500,
        detached: false,
    };
    let export = ExportFacts {
        fence: head,
        first: 500,
        lines: MAX_EXPORT_LINES,
    };
    let joined = join(Some(export), head, 256);
    assert_eq!(joined.fallback, None);
    assert_eq!(joined.dropped, 500, "the oldest 500 stay behind, counted");
    assert_eq!(joined.take, MAX_EXPORT_LINES - 256);
}

#[test]
fn an_older_successor_is_handed_no_export() {
    assert!(exports_for_target(100, 100));
    assert!(exports_for_target(100, 101));
    assert!(
        !exports_for_target(101, 100),
        "a rollback to a build before v0.94.0 has no importer"
    );
}

#[test]
fn the_park_waits_for_the_export_but_only_so_long() {
    assert!(park_wait(true, Duration::ZERO).is_some());
    assert!(park_wait(true, EXPORT_PATIENCE - Duration::from_millis(1)).is_some());
    assert_eq!(park_wait(true, EXPORT_PATIENCE), None);
    assert_eq!(park_wait(false, Duration::ZERO), None);
}

#[test]
fn an_export_nobody_takes_leaves_nothing_on_disk() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 500);
    let results = export(dir.path(), &[(1, &term)]);
    assert_eq!(results.exports.len(), 1);
    assert_eq!(dir_entries(dir.path()).len(), 1);
    drop(results);
    assert!(dir_entries(dir.path()).is_empty());
    // And an exporter dropped before it is asked removes its files too.
    let exporter = HistoryExporter::start(
        dir.path().to_path_buf(),
        vec![(1, Arc::clone(&term))],
        false,
    )
    .unwrap();
    let exited = exporter.exited();
    drop(exporter);
    // The worker notices the stop at its next chunk and removes what it
    // wrote. Wait for it to finish — not for the directory to look empty,
    // which it also does before the worker has written anything.
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !exited.load(std::sync::atomic::Ordering::Acquire) && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(
        exited.load(std::sync::atomic::Ordering::Acquire),
        "the dropped exporter's worker finished"
    );
    assert!(
        dir_entries(dir.path()).is_empty(),
        "a dropped exporter leaks nothing"
    );
}

#[test]
fn a_session_without_history_exports_nothing_and_loses_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3);
    let crossed = cross(dir.path(), &term, |_| {});
    assert_eq!(
        crossed.joined,
        Joined {
            take: 0,
            dropped: 0,
            fallback: None
        }
    );
    assert!(dir_entries(dir.path()).is_empty());
}

/// A scrollback the adopted shell clears (`ESC [3J`) before the post-Commit
/// import lands stays cleared, with no loss count and no band row. Negative
/// control: the same crossing with no clear imports everything
/// (`a_whole_history_crosses_the_handoff_in_order_and_nothing_is_counted_lost`).
#[test]
fn a_scrollback_cleared_before_the_import_stays_cleared_and_is_not_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "secret", 0, 2000);
    let crossed = cross(dir.path(), &term, |_| {});
    assert!(crossed.joined.take > 0);
    let mut adopted = open_sidecar(dir.path(), &crossed.manifest.sessions[0]);
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    successor.lock().unwrap().process(b"\x1b[3J");
    let report = import_one(&successor, adopted);
    assert!(report.cleared, "{report:?}");
    assert_eq!((report.imported, report.lost()), (0, 0));
    assert_eq!(report.failed, None);
    assert!(
        history(&successor.lock().unwrap()).is_empty(),
        "nothing came back"
    );
    assert_eq!(
        loss_summary(&[report]),
        LossSummary::default(),
        "no band row for a clear"
    );
}

/// The export follows: output printed between its first pass and the park
/// (the successor's boot plus the gate's wait, up to the automatic lane's
/// two-minute hold) is taken too, so the park still meets it. Negative control:
/// the same output after a non-following export outruns the join and is
/// counted.
#[test]
fn a_following_export_takes_what_lands_after_its_first_pass() {
    for follow in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "build", 0, 3000);
        let mut exporter = HistoryExporter::start(
            dir.path().to_path_buf(),
            vec![(1, Arc::clone(&term))],
            follow,
        )
        .unwrap();
        assert!(
            exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN),
            "the first pass finished"
        );
        // The build keeps printing while the successor boots: four times
        // what the checkpoint carries.
        write_lines(&mut term.lock().unwrap(), "late", 0, 1000);
        assert!(
            exporter.halt(aterm_spec::wait::MUST_HAPPEN),
            "the export stopped"
        );
        let results = exporter.finish(Duration::ZERO);
        let (checkpoint, head) = park(1, &term);
        let parent_history = history(&term.lock().unwrap());
        let mut manifest = manifest(&[1]);
        let verdicts = stamp_manifest(
            &mut manifest,
            &[(1, checkpoint.clone())],
            &[head],
            results,
            dir.path(),
            NONCE,
        );
        let crossed = Crossed {
            parent_history,
            checkpoint,
            manifest,
            joined: verdicts[0].1,
        };
        if follow {
            assert_eq!(crossed.joined.fallback, None, "{:?}", crossed.joined);
            assert_eq!(crossed.joined.dropped, 0);
            let (successor, report) = receive(dir.path(), &crossed);
            assert_eq!((report.lost(), report.failed.clone()), (0, None));
            assert_eq!(
                history(&successor.lock().unwrap()),
                crossed.parent_history,
                "the whole history crossed, the late lines included"
            );
        } else {
            assert_eq!(crossed.joined.fallback, Some(Fallback::Outrun));
            assert_eq!(
                crossed.joined.dropped as usize,
                crossed.parent_history.len() - 256,
                "everything but the screen carry's lines, counted"
            );
            assert_eq!(
                crossed.manifest.sessions[0].history, None,
                "no sidecar is named for an outrun export"
            );
        }
    }
}

/// The park's stop ([`HistoryExporter::halt`]), run on the main thread before
/// anything freezes, in either the following or first-pass phase: the worker
/// answers, finished work is handed over whole, and what nobody takes is
/// removed.
#[test]
fn the_parks_stop_is_answered_and_hands_over_only_whole_exports() {
    // Following, caught up: stopped, its one export covers its whole history.
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2500);
    let mut exporter =
        HistoryExporter::start(dir.path().to_path_buf(), vec![(1, Arc::clone(&term))], true)
            .unwrap();
    assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(!results.unfinished);
    assert_eq!(results.exports.len(), 1);
    assert_eq!(
        results.exports[0].facts.lines as usize,
        history(&term.lock().unwrap()).len(),
        "the whole history, and nothing past it"
    );
    drop(results);
    assert!(dir_entries(dir.path()).is_empty(), "nobody took it");

    // Stopped in its first pass: the sessions it finished come back whole,
    // the rest are simply absent (the join counts them), and nothing leaks.
    let dir = aterm_tempfile::tempdir().unwrap();
    let sessions: Vec<(u64, Arc<Mutex<Terminal>>)> = (1..=6)
        .map(|id| {
            let term = live(8, 40);
            write_lines(&mut term.lock().unwrap(), "deep", 0, 4000);
            (id, term)
        })
        .collect();
    let depth = history(&sessions[0].1.lock().unwrap()).len();
    let mut exporter = HistoryExporter::start(dir.path().to_path_buf(), sessions, true).unwrap();
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(!results.unfinished);
    for export in &results.exports {
        assert_eq!(export.facts.lines as usize, depth, "{export:?} is whole");
    }
    drop(results);
    assert!(dir_entries(dir.path()).is_empty());
}

/// The park's pause ([`HistoryExporter::pause`]) is answered like the stop and
/// reads nothing while held; a resume goes on following, so lines printed while
/// a missed park waited are in the sidecar, and `finish` stops a paused export
/// at once with its work whole.
#[test]
fn a_paused_export_resumes_following_and_finishes_whole() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2500);
    let mut exporter =
        HistoryExporter::start(dir.path().to_path_buf(), vec![(1, Arc::clone(&term))], true)
            .unwrap();
    assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    assert!(
        exporter.pause(aterm_spec::wait::MUST_HAPPEN),
        "the pause is answered"
    );
    // Paused: the history grows past it, and a repeated pause of the same
    // hold is answered again.
    write_lines(&mut term.lock().unwrap(), "late", 0, 600);
    exporter.resume();
    // Resumed: the next pause's last look reaches the history's end.
    assert!(
        exporter.pause(aterm_spec::wait::MUST_HAPPEN),
        "the re-pause is answered"
    );
    // Stopped under the hang detector, not finish's 100 ms HALT_WAIT:
    // the worker's answer to the stop must happen, however loaded the box.
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(!results.unfinished, "a paused export finishes at once");
    assert_eq!(results.exports.len(), 1);
    assert_eq!(
        results.exports[0].facts.lines as usize,
        history(&term.lock().unwrap()).len(),
        "the lines printed while it was paused followed after the resume"
    );
    drop(results);
    assert!(dir_entries(dir.path()).is_empty(), "nobody took it");
}

/// When a pause times out while the worker's last look waits
/// on the terminal lock, the park misses and resumes, and the worker reaches
/// its wait after the resume. It must go on following: the lines printed
/// before the next park are in the export, not lost to `Outrun`.
#[test]
fn a_resume_that_overtakes_a_timed_out_pause_keeps_the_export_following() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2500);
    let sessions = ExportSessions::from(vec![(1, Arc::clone(&term))]);
    let followed = Arc::clone(&sessions.followed);
    let mut exporter = HistoryExporter::start(dir.path().to_path_buf(), sessions, true).unwrap();
    assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    let guard = term.lock().unwrap();
    assert!(
        !exporter.pause(Duration::from_millis(20)),
        "the last look waits on the lock, so the pause times out"
    );
    exporter.resume();
    drop(guard);
    // More than the next pause's last look plus the screen carry can take.
    write_lines(&mut term.lock().unwrap(), "late", 0, 3000);
    await_followed(&followed, &term);
    assert!(
        exporter.pause(aterm_spec::wait::MUST_HAPPEN),
        "the re-pause is answered"
    );
    // Stopped under the hang detector, not finish's 100 ms HALT_WAIT:
    // the worker's answer to the stop must happen, however loaded the box.
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert_eq!(results.exports.len(), 1);
    let joined = join(
        Some(results.exports[0].facts),
        term.lock().unwrap().history_fence(),
        256,
    );
    assert_eq!(joined.fallback, None, "the export followed: {joined:?}");
    assert_eq!(joined.dropped, 0);
}

/// A session adopted with no screen to restore (its meta did not
/// parse) restored no history, so the park's offset has nothing it named here,
/// and the scrollback the program writes after the update is not it.
/// Negative control: the clamp still lets a view into history that crossed
/// (`a_view_into_withheld_history_stays_at_the_live_bottom`).
#[test]
fn a_view_over_a_screen_that_did_not_cross_stays_at_the_bottom() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut adopted = open_sidecar(
        dir.path(),
        &SessionRecord {
            viewport_from_bottom: Some(50),
            ..record(1)
        },
    );
    let term = live(8, 40);
    crate::spawn::hydrate_adopted_engine(&term, None, None, None, None, 1, &mut adopted);
    write_lines(&mut term.lock().unwrap(), "after", 0, 300);
    let _ = import_one(&term, adopted);
    assert_eq!(term.lock().unwrap().grid().display_offset(), 0);
}

/// The finished command whose rows `t` retains, found by the command it ran.
fn finished_output(t: &Terminal, command: &str) -> Option<String> {
    t.all_blocks()
        .filter(|b| b.is_complete())
        .find(|b| t.block_command(b).is_some_and(|c| c.ends_with(command)))
        .and_then(|b| t.block_output(b))
}

/// The shell carry meets the history carry: the adopt restores OSC 133 marks
/// and blocks at the source's absolute rows, reserves the sidecar's keys, then
/// imports after Commit. Through the real seam (`hydrate_adopted_engine`) and
/// import, finished and running blocks read exactly what the parent's do, not
/// rows shifted by the reserve.
#[test]
fn command_blocks_read_their_own_lines_after_the_history_import() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    {
        let mut t = term.lock().unwrap();
        write_lines(&mut t, "old", 0, 600);
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07pwd\r\n\x1b]133;C\x07FINISHED\r\n");
        t.process(b"\x1b]133;D;0\x07");
        write_lines(&mut t, "tail", 0, 20);
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07building\r\n");
    }
    let crossed = cross(dir.path(), &term, |_| {});
    assert_eq!(crossed.joined.fallback, None);
    assert!(crossed.joined.take > 0, "a sidecar crosses");
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!((report.imported, report.lost()), (crossed.joined.take, 0));
    let mut parent = term.lock().unwrap();
    let mut successor = successor.lock().unwrap();
    assert_eq!(history(&successor), crossed.parent_history);
    let finished = finished_output(&parent, "pwd");
    assert!(
        finished
            .as_deref()
            .is_some_and(|o| o.starts_with("FINISHED"))
    );
    assert_eq!(finished_output(&successor, "pwd"), finished);

    for t in [&mut *parent, &mut *successor] {
        t.process(b"done\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    }
    assert_eq!(
        finished_output(&parent, "make").as_deref(),
        Some("building\ndone")
    );
    assert_eq!(
        finished_output(&successor, "make"),
        finished_output(&parent, "make"),
        "the command that ran across the update completes with its own output"
    );
}

/// Under a successor policy of `carry = "visible"` or `"repaint"` the outgoing
/// process carries no scrollback and counts every line as left behind, booked
/// as withheld by the policy rather than as a failed fallback. Driven end to
/// end: the zero-history checkpoint, the join under the withheld plan, the
/// manifest over its TOML wire, the successor's `incoming` and its import.
/// Negative control: a real fallback (a rewrap after the export) books no
/// withheld lines.
#[cfg(unix)]
#[test]
fn a_withheld_carry_is_booked_as_the_policys() {
    for withheld in [true, false] {
        let at = if withheld { "withheld" } else { "rewrapped" };
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "line", 0, 1000);
        // The plan the park hands the worker, and the screen carry's depth
        // under the ceiling it read: none under a policy without scrollback.
        let (results, carried) = if withheld {
            (HistoryPlan::Withheld.results(dir.path()), 0)
        } else {
            let results = export(dir.path(), &[(1, &term)]);
            term.lock().unwrap().resize(8, 33);
            (results, 256)
        };
        let (checkpoint, head) = {
            let t = term.lock().unwrap();
            let checkpoint = t.checkpoint_carry(carried).expect("a Ground parser");
            (checkpoint, capture_head(1, &t))
        };
        let seen = head.fence.lines();
        let mut manifest = manifest(&[1]);
        stamp_manifest(
            &mut manifest,
            &[(1, checkpoint.clone())],
            &[head],
            results,
            dir.path(),
            NONCE,
        );
        let wire = manifest.to_toml().expect("the manifest serializes");
        assert_eq!(
            wire.contains("history_withheld = true"),
            withheld,
            "{at}: the record says why its lines stayed behind, only when the policy chose it"
        );
        let manifest = SessionHandoff::from_toml(&wire).expect("and parses back");

        let mut adopted = open_sidecar(dir.path(), &manifest.sessions[0]);
        let successor = adopt(&checkpoint, &mut adopted);
        let reports = run_imports(vec![ImportJob {
            session: 1,
            term: successor,
            history: adopted,
        }]);
        let lost = seen - carried_history(&checkpoint);
        assert!(lost > 0, "{at}: PRECONDITION — lines stay behind");
        assert_eq!(reports[0].lost(), lost, "{at}: every one counted");
        assert_eq!(
            loss_summary(&reports),
            LossSummary {
                lines: lost,
                tabs: 1,
                withheld: if withheld { lost } else { 0 },
                held: 0,
                held_failed: 0,
            },
            "{at}"
        );
    }
}

// ---------------------------------------------------------------------------
// A link-dense line in the carried history
// ---------------------------------------------------------------------------

/// A shell at 80 columns with `total` lines of output: a short OSC 8 link on
/// every 97th line, and at line `dense` a row of twelve 8 KiB links, whose
/// record (~96 KiB) is over the 57,344-byte cap of an 80-column frame.
fn linked_history(total: usize, dense: usize) -> Arc<Mutex<Terminal>> {
    let term = live(8, 80);
    {
        let mut t = term.lock().unwrap();
        for i in 0..total {
            if i == dense {
                t.process(&link_dense(12, 8192));
            } else if i % 97 == 0 {
                t.process(
                    format!("\x1b]8;;https://short.test/{i}\x1b\\link{i}\x1b]8;;\x1b\\").as_bytes(),
                );
            } else {
                t.process(format!("line{i}").as_bytes());
            }
            t.process(b"\r\n");
        }
    }
    term
}

/// Every line of `t`'s history, oldest first, as the lines themselves (so a
/// comparison sees every cell's style and link, not only the text).
fn history_wire(t: &Terminal) -> Vec<Line> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .expect("a history line")
                .into_owned()
        })
        .collect()
}

fn assert_same_wire(what: &str, got: &[Line], want: &[Line]) {
    assert_eq!(got.len(), want.len(), "{what}: how many lines");
    for (at, (got, want)) in got.iter().zip(want).enumerate() {
        assert!(
            got.serialize() == want.serialize(),
            "{what}: line {at} differs: {got:?} != {want:?}"
        );
    }
}

/// The export as v0.94.0–v0.97.0 write it, whose bytes are fixed: the fence and
/// the first chunk under one lock, then each chunk of [`CHUNK_LINES`] lines,
/// oldest first, as one frame of `serialize_lines` over the lines exactly as
/// the history holds them. That is this build's first pass without
/// [`strip_over_cap_links`];
/// [`the_shipped_producer_is_this_builds_export_but_the_strip`] pins it to the
/// live export byte for byte where the strip drops nothing.
fn shipped_export(dir: &Path, local_id: u64, term: &Arc<Mutex<Terminal>>) -> HistoryExport {
    let t = term.lock().unwrap();
    let fence = t.history_fence();
    assert!(!fence.detached && fence.lines() > 0, "a history to export");
    let first = fence.oldest.max(fence.end.saturating_sub(MAX_EXPORT_LINES));
    let mut bytes = header(fence.cols).to_vec();
    let mut cursor = first;
    while cursor < fence.end {
        let lines = t
            .history_lines_since_fence(&fence, cursor, CHUNK_LINES)
            .expect("the fence holds");
        assert!(!lines.is_empty(), "the history reaches its fence");
        let body = aterm_core::scrollback::serialize_lines(&lines);
        bytes.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&body);
        cursor += lines.len() as u64;
    }
    drop(t);
    let path = dir.join(format!("shipped-export.s{local_id}.hist"));
    std::fs::write(&path, &bytes).unwrap();
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(&bytes);
    HistoryExport {
        local_id,
        facts: ExportFacts {
            fence: HistoryFence {
                end: cursor,
                ..fence
            },
            first,
            lines: cursor - first,
        },
        file: OwnedFile(Some(path)),
        len: bytes.len() as u64,
        sha: hasher.finalize(),
        links_dropped: 0,
    }
}

/// The import's frame loop as v0.94.0–v0.97.0 run it (`read_sidecar`): every
/// frame the first `take` lines reach is decoded by `deserialize_lines_strict`
/// under [`line_caps`], and the first frame that fails refuses the whole
/// sidecar. `Ok` with the lines it keeps.
fn shipped_import_frames(bytes: &[u8], take: u64) -> Result<u64, &'static str> {
    let cols = u16::from_le_bytes([bytes[8], bytes[9]]);
    let (content_cap, record_cap) = line_caps(cols);
    let mut kept = 0;
    let mut frames = &bytes[HEADER_LEN..];
    while let Some((size, rest)) = frames.split_first_chunk::<4>() {
        let (frame, rest) = rest.split_at(u32::from_le_bytes(*size) as usize);
        if kept < take {
            let decoded = aterm_core::scrollback::deserialize_lines_strict(
                frame,
                CHUNK_LINES,
                usize::from(cols),
                content_cap,
                record_cap,
            )
            .ok_or("a frame that does not decode")?;
            kept += (decoded.len() as u64).min(take - kept);
        }
        frames = rest;
    }
    Ok(kept)
}

/// The shipped producer above is this build's export with the strip taken
/// out: on a history with links but none over the cap, the two write the same
/// sidecar, byte for byte.
#[test]
fn the_shipped_producer_is_this_builds_export_but_the_strip() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = linked_history(2500, usize::MAX);
    let shipped = shipped_export(dir.path(), 1, &term);
    let mut results = export(dir.path(), &[(1, &term)]);
    let this_build = results.exports.remove(0);
    assert_eq!(this_build.links_dropped, 0, "nothing over the cap");
    assert_eq!(
        (this_build.facts, this_build.len, this_build.sha),
        (shipped.facts, shipped.len, shipped.sha)
    );
    let read = |export: &HistoryExport| std::fs::read(export.file.0.as_ref().unwrap()).unwrap();
    assert!(
        read(&this_build) == read(&shipped),
        "the same bytes, frame for frame"
    );
}

/// A link-dense line in sidecars the shipped producers wrote costs only its
/// links: the dense line's text is exact with its links dropped, every other
/// line (links included) is exact, nothing is counted lost. Covers the line
/// among the sidecar's `take` lines (older than the checkpoint's 256) and among
/// the checkpoint's own lines in a frame the take reaches. Negative control:
/// the shipped import's frame loop refuses each of these sidecars.
#[test]
fn a_link_dense_line_a_shipped_producer_wrote_costs_only_its_links() {
    let (_, record_cap) = line_caps(80);
    for (dense, in_take) in [(400, true), (1000, false)] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = linked_history(1200, dense);
        let parent = history_wire(&term.lock().unwrap());
        let over: Vec<usize> = (0..parent.len())
            .filter(|&at| parent[at].serialize().len() > record_cap)
            .collect();
        assert_eq!(
            over,
            vec![dense],
            "PRECONDITION: the one record over the cap"
        );
        let other_links = parent.iter().filter(|line| line.has_hyperlinks()).count() - 1;
        assert!(other_links >= 10, "PRECONDITION: other lines carry links");

        let shipped = shipped_export(dir.path(), 1, &term);
        let (checkpoint, head) = park(1, &term);
        let mut manifest = manifest(&[1]);
        let verdicts = stamp_manifest(
            &mut manifest,
            &[(1, checkpoint.clone())],
            &[head],
            ExportResults {
                exports: vec![shipped],
                ..ExportResults::default()
            },
            dir.path(),
            NONCE,
        );
        let joined = verdicts[0].1;
        assert_eq!((joined.fallback, joined.dropped), (None, 0));
        let take = joined.take as usize;
        assert_eq!(
            take,
            parent.len() - 256,
            "the lines older than the checkpoint's"
        );
        assert_eq!(
            dense < take,
            in_take,
            "PRECONDITION: where the dense line sits"
        );
        assert!(dense < CHUNK_LINES, "PRECONDITION: in the first frame");

        let bytes = std::fs::read(named(dir.path(), 1)).unwrap();
        assert_eq!(
            shipped_import_frames(&bytes, joined.take),
            Err("a frame that does not decode"),
            "NEGATIVE CONTROL: the shipped import refused this sidecar whole"
        );

        let crossed = Crossed {
            parent_history: history(&term.lock().unwrap()),
            checkpoint,
            manifest,
            joined,
        };
        let (successor, report) = receive(dir.path(), &crossed);
        assert_eq!(
            (report.failed.clone(), report.lost()),
            (None, 0),
            "nothing lost"
        );
        assert_eq!(
            report.imported as usize, take,
            "every line the sidecar carries"
        );
        assert_eq!(report.links_dropped, u64::from(in_take));
        let successor = successor.lock().unwrap();
        assert_eq!(
            history(&successor),
            crossed.parent_history,
            "the text, line for line, the dense line's included"
        );
        let got = history_wire(&successor);
        let mut want = parent;
        if in_take {
            want[dense].clear_hyperlinks();
            assert!(
                !got[dense].has_hyperlinks(),
                "the dense line's links are gone"
            );
        }
        assert_same_wire("the successor's history", &got, &want);
        let kept_links = (0..got.len())
            .filter(|&at| at != dense && got[at].has_hyperlinks())
            .count();
        assert_eq!(kept_links, other_links, "every other line keeps its links");
    }
}

/// The export drops over-cap links itself on both paths (first pass and a
/// following export's ticks), so it writes no frame the strict decode refuses;
/// only the over-cap lines lose their links.
#[test]
fn the_export_writes_no_record_over_the_cap() {
    let (_, record_cap) = line_caps(80);
    for follow in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = linked_history(1200, 400);
        let mut exporter = HistoryExporter::start(
            dir.path().to_path_buf(),
            vec![(1, Arc::clone(&term))],
            follow,
        )
        .unwrap();
        assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
        if follow {
            // A second dense row lands after the first pass: a tick takes it.
            let mut t = term.lock().unwrap();
            t.process(&link_dense(12, 8192));
            t.process(b"\r\n");
            write_lines(&mut t, "late", 0, 20);
        }
        assert!(
            exporter.halt(aterm_spec::wait::MUST_HAPPEN),
            "the export stopped"
        );
        let mut results = exporter.finish(Duration::ZERO);
        let export = results.exports.remove(0);
        let history_now = history_wire(&term.lock().unwrap());
        assert_eq!(
            export.facts.lines as usize,
            history_now.len(),
            "the whole history"
        );
        assert_eq!(export.links_dropped, if follow { 2 } else { 1 });

        let bytes = std::fs::read(export.file.0.as_ref().unwrap()).unwrap();
        assert_eq!(
            shipped_import_frames(&bytes, export.facts.lines),
            Ok(export.facts.lines),
            "follow={follow}: every frame decodes strictly"
        );
        let mut written = Vec::new();
        let mut frames = &bytes[HEADER_LEN..];
        while let Some((size, rest)) = frames.split_first_chunk::<4>() {
            let (frame, rest) = rest.split_at(u32::from_le_bytes(*size) as usize);
            written.extend(aterm_core::scrollback::deserialize_lines(frame));
            frames = rest;
        }
        let want: Vec<Line> = history_now
            .into_iter()
            .map(|mut line| {
                if line.serialize().len() > record_cap {
                    line.clear_hyperlinks();
                }
                line
            })
            .collect();
        assert_same_wire(&format!("follow={follow}: the sidecar"), &written, &want);
    }
}

/// The sidecar bytes `frames` make at `cols`, and the import's read of them
/// under a stamp that matches them (its length, its sha, `take`) — so what
/// decides is the frame decode alone.
fn read_frames(cols: u16, frames: &[Vec<u8>], take: u64) -> Result<SidecarLines, &'static str> {
    let mut bytes = header(cols).to_vec();
    for frame in frames {
        bytes.extend_from_slice(&u32::try_from(frame.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(frame);
    }
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(&bytes);
    read_sidecar(
        &bytes[..],
        bytes.len() as u64,
        Some(&hasher.finalize()),
        0,
        take,
    )
}

/// The only leniency is a record over the cap because of its links, within the
/// hard ceiling. A frame corrupt any other way still refuses the whole sidecar,
/// and the digest still binds the repaired one.
#[test]
fn a_frame_corrupt_for_any_other_reason_still_refuses_the_sidecar() {
    use aterm_core::scrollback::{CellAttrs, HyperlinkSpan, Rle};
    let term = linked_history(40, 20);
    let lines = history_wire(&term.lock().unwrap());
    let dense_frame = aterm_core::scrollback::serialize_lines(&lines);
    let take = lines.len() as u64;

    // The control: the link-dense frame is read, the dense line's links gone.
    let read = read_frames(80, std::slice::from_ref(&dense_frame), take).expect("admitted");
    assert_eq!(
        (read.cols, read.lines.len(), read.links_dropped),
        (80, lines.len(), 1)
    );
    assert!(!read.lines[20].has_hyperlinks() && lines[20].has_hyperlinks());

    // Over the cap with no link at all (one attrs run per character), at a
    // width whose content cap admits its text.
    let styled = |chars: usize, links: Vec<HyperlinkSpan>| {
        let mut rle: Rle<CellAttrs> = Rle::new();
        for i in 0..chars {
            rle.push(CellAttrs::new(0x0100_0000 | (i as u32 & 1), 0xFF00_0000, 0));
        }
        Line::with_hyperlinks(&"a".repeat(chars), rle, links)
    };
    let refused = |what: &str, cols: u16, line: Line| {
        let frame = aterm_core::scrollback::serialize_lines(&[Line::from("before"), line]);
        let result = read_frames(cols, &[frame], 2);
        assert_eq!(
            result.map(|read| read.lines.len()).err(),
            Some("a frame that does not decode"),
            "{what}"
        );
    };
    refused(
        "a record over the cap without a link",
        8,
        styled(2048, Vec::new()),
    );
    refused(
        "a record over the cap even without its links",
        8,
        styled(
            2048,
            vec![HyperlinkSpan::new(0, 1, Arc::from("https://a.test"))],
        ),
    );
    // Past the hard ceiling (20,480 + 8 × 8,460 at eight columns): eleven
    // maximal links in one column, more than any ingested row holds.
    let url: Arc<str> = Arc::from("u".repeat(8192).as_str());
    refused(
        "a link-dense record past the hard ceiling",
        8,
        Line::with_hyperlinks(
            "L",
            Rle::new(),
            (0..11)
                .map(|_| HyperlinkSpan::new(0, 1, Arc::clone(&url)))
                .collect(),
        ),
    );
    // A link-dense frame with a byte after its last record.
    let mut trailing = dense_frame.clone();
    trailing.push(0);
    assert_eq!(
        read_frames(80, &[trailing], take)
            .map(|read| read.lines.len())
            .err(),
        Some("a frame that does not decode")
    );
    // The digest still binds the bytes: the repaired frame under a wrong sha.
    let mut bytes = header(80).to_vec();
    bytes.extend_from_slice(&u32::try_from(dense_frame.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&dense_frame);
    assert_eq!(
        read_sidecar(&bytes[..], bytes.len() as u64, Some(&[0u8; 32]), 0, take)
            .map(|read| read.lines.len())
            .err(),
        Some("a sha other than its stamp")
    );
}

/// An import rings its session's screen readers. The attach moves the screen
/// generation (it invalidates the scroll state) with no output, on the import
/// worker, so only the end of its own hold can push that to a `screen` or
/// `cells` subscriber. Negative control: a watcher that reads no screen is not
/// rung.
#[test]
fn an_import_rings_the_sessions_screen_readers() {
    use crate::session_watch::{Subscription, Watchers};
    use std::sync::atomic::Ordering;
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    let crossed = cross(dir.path(), &term, |_| {});
    let mut adopted = open_sidecar(dir.path(), &crossed.manifest.sessions[0]);
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    let watchers = Arc::<Watchers>::default();
    watchers.bind_frame(&successor);
    let other = Subscription::single(&watchers);
    let mut screen = Subscription::new();
    let flag = screen.watch_frame(&watchers);
    flag.store(false, Ordering::Release);
    let generation = || crate::frame_key::FrameKey::read(&successor.lock().unwrap()).generation();
    let before = generation();

    let report = import_one(&successor, adopted);
    assert_eq!(report.failed, None);
    assert!(report.imported > 1000, "REACH: the history came back");
    assert_ne!(
        generation(),
        before,
        "the premise: the attach moved the screen"
    );
    assert!(
        flag.load(Ordering::Acquire) && screen.wait(Duration::ZERO),
        "the import's hold rang the screen readers"
    );
    assert!(!other.wait(Duration::ZERO), "and only them");
}

/// A session scrolled back at the park crosses with its viewport (lines from
/// the bottom, `viewport_from_bottom`), and the successor restores it after the
/// import, clamped to the scrollback there. The offset is past the screen
/// carry's 256 lines, so only a post-import apply reaches it. Negative controls:
/// a view at the live bottom writes nothing (a record without the field leaves
/// the successor at the bottom); an over-long offset is clamped; a view the
/// person already moved is left alone.
#[test]
fn a_scrolled_viewport_is_restored_after_import() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = scrolled_back_session();
    assert_eq!(term.lock().unwrap().grid().display_offset(), 1000);
    let crossed = cross(dir.path(), &term, |_| {});
    let record = &crossed.manifest.sessions[0];
    assert_eq!(record.viewport_from_bottom, Some(1000), "the park wrote it");
    let wire = crossed.manifest.to_toml().unwrap();
    assert!(wire.contains("viewport_from_bottom = 1000"), "{wire}");
    assert_eq!(
        SessionHandoff::from_toml(&wire).as_ref(),
        Some(&crossed.manifest)
    );
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.failed, None);
    assert!(report.imported > 1000, "the history came back under it");
    let t = successor.lock().unwrap();
    assert_eq!(
        t.grid().display_offset(),
        1000,
        "the successor shows where the person was reading"
    );
    assert_eq!(history(&t), crossed.parent_history, "over the same history");
    drop(t);

    // At the live bottom: nothing is written, and the successor stays there.
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    let crossed = cross(dir.path(), &term, |_| {});
    assert_eq!(crossed.manifest.sessions[0].viewport_from_bottom, None);
    assert!(
        !crossed
            .manifest
            .to_toml()
            .unwrap()
            .contains("viewport_from_bottom"),
        "nothing on the wire for a view at the bottom"
    );
    let (successor, _) = receive(dir.path(), &crossed);
    assert_eq!(successor.lock().unwrap().grid().display_offset(), 0);

    // A record naming more than exists is clamped to the scrollback.
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 100);
    let mut crossed = cross(dir.path(), &term, |_| {});
    crossed.manifest.sessions[0].viewport_from_bottom = Some(u32::MAX);
    let (successor, _) = receive(dir.path(), &crossed);
    let t = successor.lock().unwrap();
    assert!(t.main_grid().scrollback_lines() > 0);
    assert_eq!(
        t.grid().display_offset(),
        t.main_grid().scrollback_lines(),
        "clamped to the scrollback that exists"
    );
    drop(t);

    // A view the person moved in the successor before the import is theirs.
    let dir = aterm_tempfile::tempdir().unwrap();
    let (successor, adopted) = scrolled_back_adoption(dir.path());
    successor.lock().unwrap().scroll_display(5);
    let _ = import_one(&successor, adopted);
    assert_eq!(
        successor.lock().unwrap().grid().display_offset(),
        5,
        "the person's own scroll wins"
    );
}

/// The history line at the top of `t`'s view, while it is scrolled back.
fn top_line(t: &Terminal) -> String {
    let grid = t.main_grid();
    let index = grid.scrollback_lines() - grid.display_offset();
    grid.get_history_line(index)
        .map(|l| l.to_string().trim_end().to_string())
        .unwrap_or_default()
}

/// A pipe whose read end never blocks: the test's stand-in for a PTY master.
/// Both ends are close-on-exec so a child another test spawns cannot inherit
/// the write end and turn an expected EOF into `EAGAIN`.
#[cfg(unix)]
fn nonblocking_pipe() -> [i32; 2] {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    for fd in pipe {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    pipe
}

/// What reaches a session after its adopt without anyone typing into it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterAdopt {
    /// The DEC 1004 focus-in the successor's window sends a focus-reporting
    /// program (Claude Code) when it takes focus at Commit.
    FocusReport,
    /// The engine's DA reply, written by the reply writer
    /// (`spawn::spawn_reply_writer_thread`) straight to the sink.
    EngineReply,
    /// A key the person types: the one of the three that takes the view back.
    Key,
}

/// Only input someone sent skips the restore: not the focus report sent when
/// the successor's window takes focus at Commit, nor the engine's DA/DSR
/// replies (the sink's input epoch counts both, asserted below). Driven through
/// the real App routes: `write_focus_report`, the reply writer's sink write,
/// and `App::input` for the key. Negative control: the key skips the restore.
#[cfg(unix)]
#[test]
fn a_report_or_an_engine_reply_after_the_adopt_leaves_the_viewport_restore() {
    for after in [
        AfterAdopt::FocusReport,
        AfterAdopt::EngineReply,
        AfterAdopt::Key,
    ] {
        let pipe = nonblocking_pipe();
        let sink = Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
        let mut app = crate::App::headless_for_test_with_sink(Arc::clone(&sink));
        let wid = crate::WindowId(0);
        let session = app.front_terminal_mirror(wid).expect("a session").session;
        let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
        app.pool
            .get(session)
            .expect("pooled")
            .term
            .lock()
            .unwrap()
            .process(b"\x1b[?1004h");

        let dir = aterm_tempfile::tempdir().unwrap();
        let (successor, mut adopted) = scrolled_back_adoption(dir.path());
        adopted.watch_input(&ctx.output_echo);
        let epoch = sink.input_epoch();
        match after {
            AfterAdopt::FocusReport => app.write_focus_report(session, true),
            AfterAdopt::EngineReply => {
                let _ = sink.write_frame_nonparking(b"\x1b[?62;22c");
            }
            AfterAdopt::Key => {
                let _ = app.input(
                    wid,
                    crate::InputEvent::Text("a".into()),
                    crate::Source::Human,
                );
            }
        }
        let mut wrote = [0u8; 64];
        let n = unsafe { libc::read(pipe[0], wrote.as_mut_ptr().cast(), wrote.len()) };
        let wrote = &wrote[..usize::try_from(n).unwrap_or(0)];
        match after {
            AfterAdopt::FocusReport => assert_eq!(wrote, b"\x1b[I", "the focus-in was sent"),
            AfterAdopt::EngineReply => assert_eq!(wrote, b"\x1b[?62;22c", "the reply was sent"),
            AfterAdopt::Key => assert_eq!(wrote, b"a", "the key was sent"),
        }
        assert_ne!(
            sink.input_epoch(),
            epoch,
            "{after:?}: the sink's epoch moved — the watch must not read it"
        );
        let _ = import_one(&successor, adopted);
        assert_eq!(
            successor.lock().unwrap().grid().display_offset(),
            if after == AfterAdopt::Key { 0 } else { 1000 },
            "{after:?}"
        );
        drop(ctx);
        drop(app);
        drop(sink);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

/// The restore's watch holds no descriptor: the adopted history keeps the
/// session's input count, never its sink, so dropping the sink closes the PTY
/// master at once (the read end sees EOF) and a tab closed during the import
/// hangs up its child; the watch still works.
#[cfg(unix)]
#[test]
fn a_pending_viewport_restore_keeps_no_pty_master_open() {
    use std::os::fd::{FromRawFd, OwnedFd};

    let pipe = nonblocking_pipe();
    let master = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(master));
    let input = Arc::new(crate::app_input::OutputEchoTracker::default());
    let dir = aterm_tempfile::tempdir().unwrap();
    let (successor, mut adopted) = scrolled_back_adoption(dir.path());
    adopted.watch_input(&input);
    // The tab closes: its session context — the sink with it — is dropped.
    drop(sink);
    // EOF within a bound, not at once: a child forked between `pipe` and the
    // close-on-exec above holds the write end until it execs.
    let mut hup = libc::pollfd {
        fd: pipe[0],
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut hup, 1, 5_000) },
        1,
        "no EOF in 5 s"
    );
    let mut byte = [0u8; 1];
    let n = unsafe { libc::read(pipe[0], byte.as_mut_ptr().cast(), 1) };
    assert_eq!(
        n, 0,
        "the master closed when its tab did (EOF), import pending"
    );
    let _ = import_one(&successor, adopted);
    assert_eq!(
        successor.lock().unwrap().grid().display_offset(),
        1000,
        "nothing was sent: the view is put back"
    );
    unsafe {
        libc::close(pipe[0]);
    }
}

/// The attach's own renumbering is not output. The restore adds every rise of
/// the absolute row counter since the adopt as pushed lines, but an attach
/// whose history is longer than the adopt reserved (a rewrap, or a short
/// reserve) raises the counter itself and pushes nothing. Here the parent sent
/// no counter and the reserve fell short, so the attach renumbers by the whole
/// import; the view must still land on the lines read at the park.
#[test]
fn an_attach_that_renumbers_does_not_move_the_restored_viewport() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = scrolled_back_session();
    let read_at_park = top_line(&term.lock().unwrap());
    let mut crossed = cross(dir.path(), &term, |_| {});
    crossed.checkpoint.absolute_row_counter = 0;
    let mut adopted = open_sidecar(dir.path(), &crossed.manifest.sessions[0]);
    let take = adopted.reserve();
    assert!(take > 0, "a carry to import");
    // The adopt with a reserve that falls short: the restore, a claim for no
    // keys, and the bottom noted under the same lock.
    let successor = live(8, 40);
    {
        let mut engine = successor.lock().unwrap();
        engine.restore_checkpoint(&crossed.checkpoint);
        adopted.claim = Some(engine.reserve_older_history_keys(0));
        adopted.anchor_in(&engine);
    }
    let bottom = adopted.bottom_at_adopt.expect("anchored");
    let report = import_one(&successor, adopted);
    assert_eq!(report.failed, None);
    assert!(report.imported > 0);
    let t = successor.lock().unwrap();
    assert!(
        t.main_grid().absolute_row_counter() > bottom,
        "the premise: the attach renumbered"
    );
    assert_eq!(t.grid().display_offset(), 1000, "nothing was pushed");
    assert_eq!(top_line(&t), read_at_park, "the lines read at the park");
}

/// The text of the top row the view shows.
fn top_row(t: &Terminal) -> String {
    t.display_row_text(0)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// Keys typed before Commit replay at Commit, and more follow it; paste and
/// End do the same at the bottom. None of them moves a view that is already
/// at the bottom, so "still at the bottom" cannot tell them from "nobody
/// touched it". Each gesture between the adopt and the import leaves the view
/// at the bottom. Negative control: with no gesture, and with only output, a
/// bare modifier or a key release in between (none of which says where the
/// person wants to be), the view is put back on the line read at the park:
/// output the adopted readers parse at Commit, before the import, does not
/// move the reader off the line they were reading.
#[test]
fn a_gesture_after_the_adopt_keeps_the_view_at_the_bottom() {
    use crate::app_input::{PressKind, apply_press_custody};
    type Gesture = fn(&mut Terminal);
    let cases: [(&str, Gesture, bool); 7] = [
        ("nothing", |_| {}, true),
        ("output", |t| write_lines(t, "after", 0, 5), true),
        (
            "a bare modifier",
            |t| {
                let _ = apply_press_custody(t, PressKind::Inert);
            },
            true,
        ),
        (
            "a key release",
            |t| {
                let _ = apply_press_custody(t, PressKind::Release);
            },
            true,
        ),
        (
            "typing",
            |t| {
                let _ = apply_press_custody(t, PressKind::Typing);
            },
            false,
        ),
        (
            "a held key's repeat",
            |t| {
                let _ = apply_press_custody(t, PressKind::Repeat);
            },
            false,
        ),
        ("a return to live", Terminal::return_to_live, false),
    ];
    for (what, gesture, restored) in cases {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = scrolled_back_session();
        let reading = top_row(&term.lock().unwrap());
        let crossed = cross(dir.path(), &term, |_| {});
        let mut adopted = open_sidecar(dir.path(), &crossed.manifest.sessions[0]);
        let successor = adopt(&crossed.checkpoint, &mut adopted);
        gesture(&mut successor.lock().unwrap());
        let report = import_one(&successor, adopted);
        assert_eq!(report.failed, None, "{what}");
        let t = successor.lock().unwrap();
        if restored {
            assert_eq!(top_row(&t), reading, "{what}: the view is put back");
        } else {
            assert_eq!(
                t.grid().display_offset(),
                0,
                "{what} after the adopt: the person stays at the bottom"
            );
        }
    }
}

/// A visible paste into the front session (⌘-V, menu Paste, a drop, `aterm ctl
/// paste`) is a gesture too, though it bypasses the press custody and at the
/// live bottom moves nothing. Driven through `App::input` with
/// `InputEvent::Paste`. Negative control: no paste, and the view is restored.
#[cfg(unix)]
#[test]
fn a_visible_paste_after_the_adopt_keeps_the_view_at_the_bottom() {
    use crate::input::{InputEvent, PasteFraming, Source};
    for paste in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = scrolled_back_session();
        let reading = top_row(&term.lock().unwrap());
        let crossed = cross(dir.path(), &term, |_| {});
        let mut adopted = open_sidecar(dir.path(), &crossed.manifest.sessions[0]);
        // The successor's front session is the adopted one — its engine
        // hydrated as `spawn_session` hydrates it — and its PTY a socket.
        let (_program, pty) = std::os::unix::net::UnixStream::pair().unwrap();
        let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(pty.into()));
        let mut app = crate::App::headless_for_test_with_sink(sink);
        let successor = Arc::clone(&app.pool.get(0).expect("session 0").term);
        crate::spawn::hydrate_adopted_engine(
            &successor,
            Some(&crossed.checkpoint),
            None,
            None,
            None,
            1,
            &mut adopted,
        );
        assert_eq!(successor.lock().unwrap().grid().display_offset(), 0);
        if paste {
            let ev = InputEvent::Paste("ls".into(), PasteFraming::Gesture { bracketed: false });
            assert_eq!(
                app.input(crate::WindowId(0), ev, Source::Human),
                crate::input::InputOutcome::Ok
            );
        }
        let report = import_one(&successor, adopted);
        assert_eq!(report.failed, None);
        let t = successor.lock().unwrap();
        if paste {
            assert_eq!(
                t.grid().display_offset(),
                0,
                "a paste at the prompt after the adopt: the person stays at the bottom"
            );
        } else {
            assert_eq!(top_row(&t), reading, "no paste: the view is put back");
        }
    }
}

/// A view scrolled into history that did not cross stays at the live bottom.
/// Under `carry = "visible"` or `"repaint"` no history crosses, yet the record
/// still names the reading position; scrolling into post-Commit output would
/// show lines printed after the update and stop the pane following. Negative
/// control: an offset inside the history the screen carry brought is restored,
/// riding the lines pushed since the adopt; one past it is clamped to the
/// oldest line that crossed.
#[test]
fn a_view_into_withheld_history_stays_at_the_live_bottom() {
    for (carried, scrolled, want) in [(0, 500, 0), (256, 100, 150), (256, 500, 306)] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "line", 0, 1000);
        term.lock().unwrap().scroll_display(scrolled);
        let results = HistoryPlan::Withheld.results(dir.path());
        let (checkpoint, head) = {
            let t = term.lock().unwrap();
            let checkpoint = t.checkpoint_carry(carried).expect("a Ground parser");
            (checkpoint, capture_head(1, &t))
        };
        let mut manifest = manifest(&[1]);
        stamp_manifest(
            &mut manifest,
            &[(1, checkpoint.clone())],
            &[head],
            results,
            dir.path(),
            NONCE,
        );
        let mut adopted = open_sidecar(dir.path(), &manifest.sessions[0]);
        let successor = adopt(&checkpoint, &mut adopted);
        // The adopted program, writing between the Commit and the import.
        write_lines(&mut successor.lock().unwrap(), "after", 0, 50);
        let _ = import_one(&successor, adopted);
        assert_eq!(
            successor.lock().unwrap().grid().display_offset(),
            want,
            "carried {carried}, scrolled {scrolled}"
        );
    }
}

/// A held pane's left-behind lines say why. The producer types the reason
/// ([`HeldLoss`]) from the attempt's plan ([`ExportResults::take_held`]) and
/// the join ([`name_held_sidecar`]); the successor reads it back off the two
/// wire keys ([`HeldLoss::from_wire`]) and books the lines by it
/// ([`incoming_held`]):
///
/// * a policy that carries no scrollback → the new version's request;
/// * a hop to an older build, which takes a held screen alone → a held
///   pane's, by design;
/// * an export offered and not reaching the pane, and an export whose fence
///   a resize moved → a failure, folded into [`LossSummary::held_failed`]
///   (never under "the newest lines came across"), and a record that says
///   both keys is read as failed.
#[test]
fn a_held_panes_left_lines_say_why() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "held", 0, 200);
    let head = || capture_head(7, &term.lock().unwrap()).fence;
    let lines = head().lines();
    assert!(lines > 0, "PRECONDITION: the pane has scrollback");
    let held_name = |tag: &str| dir.path().join(format!("held-{tag}.hist"));

    // The plan's reason, for a pane with no export.
    let plans = [
        (ExportResults::default(), false, HeldLoss::Failed),
        (ExportResults::default(), true, HeldLoss::ScreenOnly),
        (
            HistoryPlan::Withheld.results(dir.path()),
            false,
            HeldLoss::Withheld,
        ),
        (
            HistoryPlan::Withheld.results(dir.path()),
            true,
            HeldLoss::Withheld,
        ),
    ];
    for (mut results, older, why) in plans {
        let held = results.take_held(&[7], older);
        assert!(held.exports.is_empty());
        assert_eq!(held.unexported, why, "older successor: {older}");
        let carried = name_held_sidecar(None, head(), held_name("none"), held.unexported);
        assert_eq!(
            (carried.stamp, carried.dropped, carried.why),
            (None, lines, why),
            "no export: every line, with the plan's reason"
        );
    }

    // An export the pane's fence no longer meets: a failure, whatever the
    // plan would have said of a missing one.
    let mut results = export(dir.path(), &[(7, &term)]);
    let held = results.take_held(&[7], false);
    assert_eq!(held.exports.len(), 1, "the held pane's export is taken");
    assert!(results.exports.is_empty(), "and left out of the join");
    term.lock().unwrap().resize(8, 33);
    let moved = name_held_sidecar(
        held.exports.into_iter().next(),
        head(),
        held_name("moved"),
        HeldLoss::ScreenOnly,
    );
    assert_eq!(
        (moved.stamp.is_none(), moved.why),
        (true, HeldLoss::Failed),
        "a moved fence is a failed carry"
    );
    assert!(moved.dropped > 0);

    // The successor books each by its keys.
    for ((withheld, failed), why) in [
        ((false, false), HeldLoss::ScreenOnly),
        ((true, false), HeldLoss::Withheld),
        ((false, true), HeldLoss::Failed),
        ((true, true), HeldLoss::Failed),
    ] {
        assert_eq!(HeldLoss::from_wire(withheld, failed), why);
        let mut remaining = MAX_AGGREGATE_BYTES;
        let history = incoming_held(
            None,
            500,
            why,
            7,
            &held_name("in"),
            dir.path(),
            &mut remaining,
        );
        assert_eq!(
            carried_loss([&history]),
            LossSummary {
                lines: 500,
                tabs: 1,
                withheld: if why == HeldLoss::Withheld { 500 } else { 0 },
                held: if why == HeldLoss::ScreenOnly { 500 } else { 0 },
                held_failed: if why == HeldLoss::Failed { 500 } else { 0 },
            },
            "{why:?}"
        );
    }

    // A handed session's failure is never a held pane's.
    let handed = AdoptedHistory {
        dropped: 40,
        lost: 40,
        ..AdoptedHistory::default()
    };
    assert_eq!(carried_loss([&handed]).held_failed, 0);
}

/// A held pane never takes the room a live tab's follow needs. Held panes'
/// exports fill the aggregate after the handed session's first pass; the
/// handed session keeps printing, more than the screen carry holds, and its
/// follow evicts the largest held export for the room, so it follows and its
/// whole history crosses. The evicted pane is told as not exported (said as a
/// held pane's failed carry) and its sidecar is gone; the other held pane
/// keeps its export. A frame the handed session could not take even with
/// every held export gone evicts none. Negative control: the same desk with
/// those panes exported as handed sessions — whose room nothing takes back —
/// outruns the join.
#[test]
fn a_held_panes_export_gives_way_to_a_live_tabs_follow() {
    // Session 1 is the live tab; 2 (deep) and 3 (shallow) are held panes.
    let desk = || {
        let tab = live(8, 40);
        write_lines(&mut tab.lock().unwrap(), "tab", 0, 3000);
        let deep = live(8, 40);
        write_lines(&mut deep.lock().unwrap(), "deep", 0, 4000);
        let shallow = live(8, 40);
        write_lines(&mut shallow.lock().unwrap(), "shallow", 0, 100);
        [tab, deep, shallow]
    };
    let [tab, deep, shallow] = desk();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let measured = export(scratch.path(), &[(1, &tab), (2, &deep), (3, &shallow)]);
    let len = |id: u64| {
        measured
            .exports
            .iter()
            .find(|export| export.local_id == id)
            .map(|export| export.len)
            .expect("every session exported")
    };
    let (len1, len2, len3) = (len(1), len(2), len(3));
    drop(measured);
    // What the follow takes: 1000 lines, more than the screen carry's 256,
    // in one frame (written under one lock), smaller than the deep pane's
    // export and larger than the shallow one's.
    let late = 1000;

    // Run the launched lane's export over a fresh desk under `cap`, print
    // `late` lines into the tab once the first pass is done, park, and join
    // the tab.
    let run = |handed: &[u64], held: &[u64], cap: u64| {
        let dir = aterm_tempfile::tempdir().unwrap();
        let terms = desk();
        let pick = |ids: &[u64]| -> Vec<(u64, Arc<Mutex<Terminal>>)> {
            ids.iter()
                .map(|id| (*id, Arc::clone(&terms[usize::try_from(*id - 1).unwrap()])))
                .collect()
        };
        let sessions = ExportSessions {
            handed: pick(handed),
            held: pick(held),
            ..ExportSessions::default()
        };
        let mut exporter =
            HistoryExporter::start_within(dir.path().to_path_buf(), sessions, true, cap).unwrap();
        // Every first pass, the held panes' too: they run between the follows.
        assert!(
            exporter.await_passed(aterm_spec::wait::MUST_HAPPEN),
            "every first pass finished"
        );
        write_lines(&mut terms[0].lock().unwrap(), "late", 0, late);
        assert!(
            exporter.halt(aterm_spec::wait::MUST_HAPPEN),
            "the export stopped"
        );
        let results = exporter.finish(Duration::ZERO);
        assert!(!results.unfinished);
        assert!(
            results
                .exports
                .iter()
                .fold(0, |sum, export| sum + export.len)
                <= cap,
            "the sidecars stay within the aggregate"
        );
        (dir, terms, results)
    };
    let cross_tab = |dir: &Path, tab: &Arc<Mutex<Terminal>>, results: ExportResults| {
        let (checkpoint, head) = park(1, tab);
        let parent_history = history(&tab.lock().unwrap());
        let mut manifest = manifest(&[1]);
        let verdicts = stamp_manifest(
            &mut manifest,
            &[(1, checkpoint.clone())],
            &[head],
            results,
            dir,
            NONCE,
        );
        Crossed {
            parent_history,
            checkpoint,
            manifest,
            joined: verdicts[0].1,
        }
    };

    // The aggregate exactly full after the first pass: the follow evicts
    // the deep pane, and the tab's whole history crosses.
    let (dir, terms, mut results) = run(&[1], &[2, 3], len1 + len2 + len3);
    assert_eq!(
        results.refused,
        vec![(2, EVICTED.to_string())],
        "the deep pane, the largest, gave way"
    );
    let entries = dir_entries(dir.path());
    assert!(
        !entries.iter().any(|name| name.ends_with(".s2.hist")),
        "the evicted sidecar is gone: {entries:?}"
    );
    let held = results.take_held(&[2, 3], false);
    assert_eq!(
        held.exports
            .iter()
            .map(|export| export.local_id)
            .collect::<Vec<_>>(),
        vec![3],
        "the shallow pane keeps its export"
    );
    assert_eq!(held.unexported, HeldLoss::Failed, "said as a failed carry");
    let evicted = name_held_sidecar(
        None,
        capture_head(2, &terms[1].lock().unwrap()).fence,
        dir.path().join("held-2.hist"),
        held.unexported,
    );
    assert_eq!(
        (evicted.stamp, evicted.dropped, evicted.why),
        (
            None,
            capture_head(2, &terms[1].lock().unwrap()).fence.lines(),
            HeldLoss::Failed
        ),
        "every line of the evicted pane stays behind, counted"
    );
    assert!(results.refused.is_empty(), "the held refusal left the join");
    let crossed = cross_tab(dir.path(), &terms[0], results);
    assert_eq!(crossed.joined.fallback, None, "{:?}", crossed.joined);
    assert_eq!(crossed.joined.dropped, 0);
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!((report.lost(), report.failed.clone()), (0, None));
    assert_eq!(
        history(&successor.lock().unwrap()),
        crossed.parent_history,
        "the tab's whole history crossed, the late lines included"
    );
    drop(held);

    // A frame no held export could make room for evicts nothing: the
    // shallow pane alone is smaller than the tab's late frame.
    let (dir, terms, results) = run(&[1], &[3], len1 + len3);
    assert!(results.refused.is_empty(), "{:?}", results.refused);
    assert!(
        results.exports.iter().any(|export| export.local_id == 3),
        "the held pane keeps its export"
    );
    let crossed = cross_tab(dir.path(), &terms[0], results);
    assert_eq!(crossed.joined.fallback, Some(Fallback::Outrun));

    // NEGATIVE CONTROL: the same panes as handed sessions take room nothing
    // gives back, and the tab outruns the join.
    let (dir, terms, results) = run(&[1, 2, 3], &[], len1 + len2 + len3);
    assert!(results.refused.is_empty(), "{:?}", results.refused);
    let crossed = cross_tab(dir.path(), &terms[0], results);
    assert_eq!(
        crossed.joined.fallback,
        Some(Fallback::Outrun),
        "without the eviction the tab's follow stops"
    );
    assert!(crossed.joined.dropped > 0);
}

/// A live tab (session 1, 3000 lines deep) and
/// a deep held pane (session 2) whose first pass the test holds where it is,
/// as a pane of a million lines is held by its own depth until the park.
fn desk_with_a_deep_held_pane() -> (Arc<Mutex<Terminal>>, Arc<Mutex<Terminal>>, Arc<AtomicU64>) {
    let tab = live(8, 40);
    write_lines(&mut tab.lock().unwrap(), "tab", 0, 3000);
    let deep = live(8, 40);
    write_lines(&mut deep.lock().unwrap(), "deep", 0, 4000);
    (tab, deep, Arc::new(AtomicU64::new(2)))
}

/// The tab's join at the park, against what the export handed over.
fn join_the_tab(tab: &Arc<Mutex<Terminal>>, results: &ExportResults) -> Joined {
    let export = results
        .exports
        .iter()
        .find(|export| export.local_id == 1)
        .map(|export| export.facts);
    let (checkpoint, head) = park(1, tab);
    join(export, head.fence, carried_history(&checkpoint))
}

/// Wait (a hang detector) until every handed session's export has followed
/// `term`'s history to its end.
fn await_followed(followed: &AtomicU64, term: &Arc<Mutex<Terminal>>) {
    let end = term.lock().unwrap().history_fence().end;
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the export followed the history to its end",
        || (followed.load(Ordering::Acquire) >= end).then_some(()),
    );
}

/// A held pane's first pass never stops a live tab being followed. The held
/// pass here never finishes (the test holds it). The park gate is told the
/// export caught up once the tab's pass is done; the tab prints 3000 lines,
/// the export follows them, and the park pauses it in the middle of the held
/// pass: the tab's join is whole, and the held pane is told why it has no
/// export.
#[test]
fn a_held_pass_never_stops_a_live_tab_being_followed() {
    let late = 3000;
    assert!(
        late > 2 * CHUNK_LINES + 256,
        "PRECONDITION: more than the last look and the screen carry reach"
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    let (tab, deep, stalled) = desk_with_a_deep_held_pane();
    let sessions = ExportSessions {
        handed: vec![(1, Arc::clone(&tab))],
        held: vec![(2, Arc::clone(&deep))],
        stalled: Some(Arc::clone(&stalled)),
        followed: Arc::default(),
    };
    let followed = Arc::clone(&sessions.followed);
    let mut exporter = HistoryExporter::start(dir.path().to_path_buf(), sessions, true).unwrap();
    assert!(
        exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN),
        "the park need not wait for a held pane's pass"
    );
    write_lines(&mut tab.lock().unwrap(), "late", 0, late);
    await_followed(&followed, &tab);
    assert!(
        exporter.pause(aterm_spec::wait::MUST_HAPPEN),
        "the pause is answered in the middle of the held pass"
    );
    assert!(
        !exporter.passed,
        "PRECONDITION: the held pass is unfinished"
    );
    // Stopped under the hang detector, not finish's 100 ms HALT_WAIT:
    // the worker's answer to the stop must happen, however loaded the box.
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(!results.unfinished, "{results:?}");
    let joined = join_the_tab(&tab, &results);
    assert_eq!(joined.fallback, None, "the tab was followed: {joined:?}");
    assert_eq!(joined.dropped, 0);
    assert_eq!(
        results.refused,
        vec![(2, STOPPED_BEFORE_ITS_PASS.to_string())],
        "the held pane is told why it has no export"
    );
    drop(results);
    assert!(
        dir_entries(dir.path()).is_empty(),
        "nothing is left on disk"
    );
}

/// The park gate waits for every first pass (`passed`), within
/// [`EXPORT_PATIENCE`]; the held pass runs between the tab's follows, so the
/// tab, still printing, is never outrun meanwhile.
///
/// Negative control: keyed on `caught_up`, the gate would park while
/// the held pass is still running.
#[test]
fn the_park_gate_waits_for_a_held_pass_and_its_scrollback_crosses() {
    let late = 3000;
    assert!(
        late > 2 * CHUNK_LINES + 256,
        "PRECONDITION: more than the last look and the screen carry reach"
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    let (tab, deep, stalled) = desk_with_a_deep_held_pane();
    let sessions = ExportSessions {
        handed: vec![(1, Arc::clone(&tab))],
        held: vec![(2, Arc::clone(&deep))],
        stalled: Some(Arc::clone(&stalled)),
        followed: Arc::default(),
    };
    let followed = Arc::clone(&sessions.followed);
    let mut exporter = HistoryExporter::start(dir.path().to_path_buf(), sessions, true).unwrap();
    assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    assert!(
        !exporter.passed(),
        "PRECONDITION: the held pass is unfinished"
    );
    assert!(
        park_wait(!exporter.passed(), Duration::ZERO).is_some(),
        "the gate waits for the held pane's pass"
    );
    assert_eq!(
        park_wait(!exporter.caught_up(), Duration::ZERO),
        None,
        "NEGATIVE CONTROL: keyed on the handed passes, the gate would park now"
    );
    // The tab keeps printing while the held pass runs; then the pass is let go.
    write_lines(&mut tab.lock().unwrap(), "late", 0, late);
    stalled.store(u64::MAX, Ordering::Release);
    assert!(
        exporter.await_passed(aterm_spec::wait::MUST_HAPPEN),
        "the held pass finishes"
    );
    assert_eq!(
        park_wait(!exporter.passed(), Duration::ZERO),
        None,
        "the gate lets the park go once every pass is done"
    );
    await_followed(&followed, &tab);
    assert!(exporter.pause(aterm_spec::wait::MUST_HAPPEN));
    // Stopped under the hang detector, not finish's 100 ms HALT_WAIT:
    // the worker's answer to the stop must happen, however loaded the box.
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(
        results.refused.is_empty(),
        "nothing is told as not exported: {:?}",
        results.refused
    );
    assert!(
        results.exports.iter().any(|export| export.local_id == 2),
        "the held pane's scrollback crosses"
    );
    let joined = join_the_tab(&tab, &results);
    assert_eq!(joined.fallback, None, "the tab was followed: {joined:?}");
}

/// A park that misses resumes the export, and the held
/// pane's pass — kept across the pause, not dropped — finishes, while the tab
/// is followed; the next park hands both over whole.
#[test]
fn a_paused_held_pass_finishes_after_the_resume() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (tab, deep, stalled) = desk_with_a_deep_held_pane();
    let sessions = ExportSessions {
        handed: vec![(1, Arc::clone(&tab))],
        held: vec![(2, Arc::clone(&deep))],
        stalled: Some(Arc::clone(&stalled)),
        followed: Arc::default(),
    };
    let followed = Arc::clone(&sessions.followed);
    let mut exporter = HistoryExporter::start(dir.path().to_path_buf(), sessions, true).unwrap();
    assert!(exporter.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    assert!(
        exporter.pause(aterm_spec::wait::MUST_HAPPEN),
        "paused in the middle of the held pass"
    );
    // The park missed: the export resumes, and the held pass is let go.
    stalled.store(u64::MAX, Ordering::Release);
    exporter.resume();
    write_lines(&mut tab.lock().unwrap(), "late", 0, 3000);
    assert!(
        exporter.await_passed(aterm_spec::wait::MUST_HAPPEN),
        "the held pass finished after the resume"
    );
    await_followed(&followed, &tab);
    assert!(exporter.pause(aterm_spec::wait::MUST_HAPPEN));
    // Stopped under the hang detector, not finish's 100 ms HALT_WAIT:
    // the worker's answer to the stop must happen, however loaded the box.
    assert!(
        exporter.halt(aterm_spec::wait::MUST_HAPPEN),
        "the stop is answered"
    );
    let results = exporter.finish(Duration::ZERO);
    assert!(results.refused.is_empty(), "{:?}", results.refused);
    let held = results
        .exports
        .iter()
        .find(|export| export.local_id == 2)
        .expect("the held pane's export");
    assert_eq!(
        held.facts.lines as usize,
        history(&deep.lock().unwrap()).len(),
        "the held pane's whole history"
    );
    let joined = join_the_tab(&tab, &results);
    assert_eq!(joined.fallback, None, "{joined:?}");
    assert_eq!(joined.dropped, 0);
}

/// The fork lane exports with every reader parked; past the handed
/// sessions' passes the held ones get at most their own bound: here a held
/// pass that never finishes, a wait of a minute and a held bound of nothing,
/// and the export is handed over at once, the tab's whole, not "unfinished",
/// and the held pane told why it has none.
#[test]
fn the_fork_lanes_held_passes_wait_only_their_own_bound() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (tab, deep, stalled) = desk_with_a_deep_held_pane();
    let sessions = ExportSessions {
        handed: vec![(1, Arc::clone(&tab))],
        held: vec![(2, Arc::clone(&deep))],
        stalled: Some(stalled),
        followed: Arc::default(),
    };
    let results = HistoryExporter::start(dir.path().to_path_buf(), sessions, false)
        .unwrap()
        .finish_deferred(aterm_spec::wait::MUST_HAPPEN, Duration::ZERO);
    assert!(!results.unfinished, "every tab was reached: {results:?}");
    assert_eq!(
        results
            .exports
            .iter()
            .map(|export| export.local_id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    // Told before the worker's `Done` — which the halt waits for only
    // `HALT_WAIT` — so the held pane's reason may or may not be in by then;
    // no other reason ever is.
    assert!(
        results
            .refused
            .iter()
            .all(|refused| *refused == (2, STOPPED_BEFORE_ITS_PASS.to_string())),
        "{:?}",
        results.refused
    );
    let joined = join_the_tab(&tab, &results);
    assert_eq!((joined.fallback, joined.dropped), (None, 0));

    // CONTROL: a held pass that finishes within its bound is handed over.
    let dir = aterm_tempfile::tempdir().unwrap();
    let (tab, deep, _) = desk_with_a_deep_held_pane();
    let sessions = ExportSessions {
        handed: vec![(1, Arc::clone(&tab))],
        held: vec![(2, Arc::clone(&deep))],
        stalled: None,
        followed: Arc::default(),
    };
    let results = HistoryExporter::start(dir.path().to_path_buf(), sessions, false)
        .unwrap()
        .finish_deferred(aterm_spec::wait::MUST_HAPPEN, aterm_spec::wait::MUST_HAPPEN);
    assert!(results.refused.is_empty(), "{:?}", results.refused);
    assert_eq!(results.exports.len(), 2);
}
