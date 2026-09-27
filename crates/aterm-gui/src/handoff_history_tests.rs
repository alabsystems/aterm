// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The history carry, end to end on real engines: the export with the
//! session live, the park's checkpoint, the join, the sidecar crossing, and
//! the import after Commit — plus every way it falls back, each of which must
//! COUNT what it leaves behind.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::{Terminal, TerminalCheckpoint};

use super::*;
use crate::session_store::{SessionHandoff, SessionRecord};

const NONCE: &str = "00112233445566778899aabbccddeeff";

fn write_lines(t: &mut Terminal, prefix: &str, from: usize, count: usize) {
    for i in from..from + count {
        t.process(format!("{prefix}{i}\r\n").as_bytes());
    }
}

fn history(t: &Terminal) -> Vec<String> {
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
        control: None,
        frozen_path: false,
        identity: None,
        topics: Vec::new(),
        fg_holder: None,
        rekey: false,
        loader: false,
        history: None,
        history_dropped: 0,
        history_lost: 0,
    }
}

fn manifest(ids: &[u64]) -> SessionHandoff {
    SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        sessions: ids.iter().map(|id| record(*id)).collect(),
        window: None,
        connections: Vec::new(),
        next_turn_id: None,
        outgoing_build: None,
    }
}

/// A live session as the outgoing process holds it.
fn live(rows: u16, cols: u16) -> Arc<Mutex<Terminal>> {
    Arc::new(Mutex::new(Terminal::new(rows, cols)))
}

/// The export, run to completion (it has nothing to wait on here).
fn export(dir: &Path, sessions: &[(u64, &Arc<Mutex<Terminal>>)]) -> ExportResults {
    let sessions = sessions
        .iter()
        .map(|(id, term)| (*id, Arc::clone(term)))
        .collect();
    let results = HistoryExporter::start(dir.to_path_buf(), sessions, false)
        .expect("the export worker starts")
        .finish(Duration::from_secs(30));
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

/// The successor's whole half for session 1: open before the proof, adopt,
/// import after Commit.
fn receive(dir: &Path, crossed: &Crossed) -> (Arc<Mutex<Terminal>>, ImportReport) {
    let mut remaining = MAX_AGGREGATE_BYTES;
    let adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir, 1),
        dir,
        &mut remaining,
    );
    assert!(
        !named(dir, 1).exists(),
        "the successor unlinks the sidecar as it opens it"
    );
    let mut adopted = adopted;
    let term = adopt(&crossed.checkpoint, &mut adopted);
    let report = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&term),
        history: adopted,
    }])
    .remove(0);
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
fn output_after_the_export_that_the_checkpoint_carries_still_meets_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "old", 0, 1000);
    let crossed = cross(dir.path(), &term, |t| write_lines(t, "late", 0, 100));
    assert_eq!(
        crossed.joined.fallback, None,
        "100 late lines fit the 256 carried"
    );
    let (successor, report) = receive(dir.path(), &crossed);
    assert_eq!(report.lost(), 0);
    assert_eq!(history(&successor.lock().unwrap()), crossed.parent_history);
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
    // The rows still ON SCREEN at the clear (the last seven) scroll into the
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
fn more_output_after_the_export_than_the_checkpoint_carries_is_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "old", 0, 1000);
    let crossed = cross(dir.path(), &term, |t| write_lines(t, "flood", 0, 400));
    assert_eq!(crossed.joined.fallback, Some(Fallback::Outrun));
    assert_eq!(
        crossed.joined.dropped,
        crossed.parent_history.len() as u64 - 256
    );
    assert_eq!(crossed.manifest.sessions[0].history, None);
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
    assert!(!exports_for_target(101, 100), "a rollback has no importer");
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
    drop(exporter);
    // The worker notices the stop at its next chunk and removes what it
    // wrote; wait for THAT, bounded only against a real leak.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !dir_entries(dir.path()).is_empty() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
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

/// A SCROLLBACK THE ADOPTED SHELL CLEARS BEFORE THE IMPORT LANDS STAYS CLEARED,
/// and is no loss (2026-09-26 review): the import runs on a worker after
/// Commit, while the shell and what it replays already run, and before the
/// claim an `ESC [3J` in that window was undone by the import. No count, no
/// band row. NEGATIVE CONTROL: the same crossing with no clear imports it all
/// (`a_whole_history_crosses_the_handoff_in_order_and_nothing_is_counted_lost`).
#[test]
fn a_scrollback_cleared_before_the_import_stays_cleared_and_is_not_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "secret", 0, 2000);
    let crossed = cross(dir.path(), &term, |_| {});
    assert!(crossed.joined.take > 0);
    let mut remaining = MAX_AGGREGATE_BYTES;
    let mut adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir.path(), 1),
        dir.path(),
        &mut remaining,
    );
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    successor.lock().unwrap().process(b"\x1b[3J");
    let report = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&successor),
        history: adopted,
    }])
    .remove(0);
    assert!(report.cleared, "{report:?}");
    assert_eq!((report.imported, report.lost()), (0, 0));
    assert_eq!(report.failed, None);
    assert!(
        history(&successor.lock().unwrap()).is_empty(),
        "nothing came back"
    );
    assert_eq!(loss_summary(&[report]), (0, 0), "no band row for a clear");
}

/// THE EXPORT FOLLOWS (2026-09-26 review). On the launched lane the first pass
/// runs beside the launch, and the park comes seconds later — the successor's
/// whole boot, then the gate's wait for a quiet moment, up to the automatic
/// lane's two-minute hold — so a tab that printed more than the screen carry's
/// 256 lines in between fell back to them: a build still running at the
/// update, the very log the carry exists for. Following, the export takes
/// what lands after its first pass, and the park still meets it.
/// NEGATIVE CONTROL: the same output after an export that does NOT follow
/// outruns the join, and is counted.
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
            exporter.await_caught_up(Duration::from_secs(30)),
            "the first pass finished"
        );
        // The build keeps printing while the successor boots: four times
        // what the checkpoint carries.
        write_lines(&mut term.lock().unwrap(), "late", 0, 1000);
        assert!(exporter.halt(Duration::from_secs(30)), "the export stopped");
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
        }
    }
}

/// THE PARK'S STOP ([`HistoryExporter::halt`]), which the park runs on the main
/// thread before it freezes anything, whether the export is following or
/// still in its first pass: the worker answers that it stopped, whatever it
/// finished is handed over whole, and what nobody takes is removed.
#[test]
fn the_parks_stop_is_answered_and_hands_over_only_whole_exports() {
    // Following, caught up: stopped, its one export covers its whole history.
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2500);
    let mut exporter =
        HistoryExporter::start(dir.path().to_path_buf(), vec![(1, Arc::clone(&term))], true)
            .unwrap();
    assert!(exporter.await_caught_up(Duration::from_secs(30)));
    assert!(
        exporter.halt(Duration::from_secs(30)),
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
        exporter.halt(Duration::from_secs(30)),
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
