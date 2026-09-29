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
        history_withheld: false,
        history_lost: 0,
        hold: None,
        supervisor: None,
        claim_known: false,
        attention_owners: Vec::new(),
        viewport_from_bottom: None,
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
        held: Vec::new(),
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
    assert_eq!(
        loss_summary(&[report]),
        LossSummary::default(),
        "no band row for a clear"
    );
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

/// THE PARK'S PAUSE ([`HistoryExporter::pause`], round six of the update
/// audit, finding 14): answered like the stop, and nothing is read while it
/// holds — but a RESUME goes on following, so the lines printed while a
/// missed park waited to re-park are in the sidecar the landed one hands
/// over, and `finish` stops a paused export at once with its work whole.
#[test]
fn a_paused_export_resumes_following_and_finishes_whole() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 2500);
    let mut exporter =
        HistoryExporter::start(dir.path().to_path_buf(), vec![(1, Arc::clone(&term))], true)
            .unwrap();
    assert!(exporter.await_caught_up(Duration::from_secs(30)));
    assert!(
        exporter.pause(Duration::from_secs(30)),
        "the pause is answered"
    );
    // Paused: the history grows past it, and a repeated pause of the same
    // hold is answered again.
    write_lines(&mut term.lock().unwrap(), "late", 0, 600);
    exporter.resume();
    // Resumed: the next pause's last look reaches the history's end.
    assert!(
        exporter.pause(Duration::from_secs(30)),
        "the re-pause is answered"
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

/// The finished command whose rows `t` retains, found by the command it ran.
fn finished_output(t: &Terminal, command: &str) -> Option<String> {
    t.all_blocks()
        .filter(|b| b.is_complete())
        .find(|b| t.block_command(b).is_some_and(|c| c.ends_with(command)))
        .and_then(|b| t.block_output(b))
}

/// THE SHELL CARRY MEETS THE HISTORY CARRY (2026-09-27 review). The adopt
/// restores the checkpoint — its OSC 133 marks and blocks at the source's
/// absolute rows — then reserves the sidecar's keys, then imports after
/// Commit. When the reserve raised the counter by the whole sidecar, every
/// restored row moved up by it and the marks did not: a finished block read
/// nothing, and a command running across the update completed with the old
/// history as its output. Through the real seam (`hydrate_adopted_engine`)
/// and the real import, both read exactly what the parent's do.
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

/// THE SUCCESSOR SAYS A POLICY'S WITHHOLDING AS THE POLICY'S (round 3 of the
/// update-robustness work, 2026-09-27). Under a successor handoff policy of
/// `carry = "visible"` or `"repaint"` the outgoing process carries NO
/// scrollback — no history lines in the screen carry, no sidecar — and counts
/// every line the park saw as left behind. The successor's band row read that
/// count as a failure and added "everything on screen and the newest lines
/// came across": no newest line crossed, and under `repaint` no screen did.
/// Driven the whole way: the park's zero-history checkpoint, the worker's join
/// under the withheld plan, the manifest over its TOML wire (which now says
/// why), the successor's `incoming`, its import, and the row
/// `settle_handoff_history` posts. NEGATIVE CONTROL: a real fallback (a rewrap
/// after the export) keeps the failure row, its comfort, and no policy words.
///
/// RED before the fix, run 2026-09-27 on fd30337e5 (this test less the
/// `history_withheld`/`withheld` checks, which name what the fix adds): the
/// withheld case posted "the newest lines came across" (`left: true, right:
/// false` at the comfort assertion).
#[cfg(unix)]
#[test]
fn a_withheld_carry_is_said_as_the_policys_on_the_successors_band() {
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

        let mut remaining = MAX_AGGREGATE_BYTES;
        let mut adopted = incoming(
            &manifest.sessions[0],
            false,
            &named(dir.path(), 1),
            dir.path(),
            &mut remaining,
        );
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
            },
            "{at}"
        );

        let mut app = crate::App::headless_for_test();
        app.settle_handoff_history(&reports);
        assert!(
            app.has_live_message(&format!("{lost} older lines stayed behind in 1 tab")),
            "{at}: the row counts the lines"
        );
        assert_eq!(
            app.has_live_message("the newest lines came across"),
            !withheld,
            "{at}: the failure's comfort is said only for a failure"
        );
        assert_eq!(
            app.has_live_message("the new version asked this update to carry no scrollback"),
            withheld,
            "{at}: a withheld carry is said as the new version's request"
        );
    }
}

// ---------------------------------------------------------------------------
// A link-dense line in the carried history (2026-09-28)
// ---------------------------------------------------------------------------

/// `n` one-cell OSC 8 links (`L`, then a space), each to a `url_len`-byte URL
/// — the row `seamless_ladder_tests::link_dense` builds for the screen carry.
fn link_dense(n: usize, url_len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for link in 0..n {
        let mut url = format!("https://example.test/{link}/");
        let pad = url_len.saturating_sub(url.len());
        url.push_str(&"u".repeat(pad));
        bytes.extend_from_slice(format!("\x1b]8;;{url}\x1b\\L\x1b]8;;\x1b\\ ").as_bytes());
    }
    bytes
}

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

/// THE EXPORT AS v0.94.0–v0.97.0 SHIP IT, the producer whose bytes are fixed
/// forever (law L3). Their `header`, `begin_export`, `OpenExport::append` and
/// `line_caps` are one text (checked against `git show
/// v0.97.0:crates/aterm-gui/src/handoff_history.rs` and its v0.94.0 and
/// v0.95.0 twins), and the line codec's bytes have not moved since v0.94.0:
/// the fence and the first chunk under one lock, then each chunk of
/// [`CHUNK_LINES`] lines, oldest first, as one frame of `serialize_lines` over
/// the lines EXACTLY as the history holds them. That is this build's first pass
/// without [`strip_over_cap_links`], and
/// [`the_shipped_producer_is_this_builds_export_but_the_strip`] pins it to the
/// live export byte for byte wherever the strip has nothing to drop.
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

/// THE IMPORT'S FRAME LOOP AS v0.94.0–v0.97.0 SHIP IT (`read_sidecar` at the
/// tag): every frame the first `take` lines reach is decoded by
/// `deserialize_lines_strict` under [`line_caps`], and the first frame that
/// does not decode refuses the whole sidecar. `Ok` with the lines it keeps.
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

/// THE 2026-09-28 DEFECT, on the bytes the shipped producers write: a tab
/// whose carried history holds one link-dense line lost ALL of it on every
/// v0.94.0–v0.97.0 → next hop, because the import refused the whole sidecar
/// for the one frame the line was in. Now every line crosses: the dense
/// line's text exact and its links dropped, every other line — its links
/// included — exact, nothing counted lost. Both places the line can sit: among
/// the sidecar's `take` lines (older than the checkpoint's 256), and among the
/// checkpoint's own lines but in a frame the take reaches, which is decoded
/// all the same. NEGATIVE CONTROL: the shipped import's frame loop refuses
/// each of these sidecars.
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

/// THE EXPORT DROPS THE LINKS ITSELF, on both of its paths — the first pass
/// and a following export's ticks — so this build writes no frame the strict
/// decode refuses: the shipped import's frame loop reads the whole sidecar.
/// What it writes is the history with only the over-cap lines' links dropped.
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
        assert!(exporter.await_caught_up(Duration::from_secs(30)));
        if follow {
            // A second dense row lands after the first pass: a tick takes it.
            let mut t = term.lock().unwrap();
            t.process(&link_dense(12, 8192));
            t.process(b"\r\n");
            write_lines(&mut t, "late", 0, 20);
        }
        assert!(exporter.halt(Duration::from_secs(30)), "the export stopped");
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
    read_sidecar(&bytes[..], bytes.len() as u64, &hasher.finalize(), take)
}

/// The leniency is exactly one: a record over the cap because of its links,
/// within the hard ceiling. A frame corrupt in any other way still refuses
/// the whole sidecar, as it did, and the digest still binds the repaired one.
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
        read_sidecar(&bytes[..], bytes.len() as u64, &[0u8; 32], take)
            .map(|read| read.lines.len())
            .err(),
        Some("a sha other than its stamp")
    );
}

/// WHERE THE PERSON WAS READING (round five, item 18): a session scrolled
/// back into its history at the park crosses with its viewport — lines from
/// the bottom, on the record as `viewport_from_bottom` — and the successor
/// puts it back once the import has put the history back under it, clamped to
/// the scrollback that exists there. Here the offset is far past the 256 lines
/// the screen carry holds, so only an apply AFTER the import can reach it.
/// NEGATIVE CONTROLS: a view at the live bottom writes nothing (an older
/// reader sees the wire it always saw, and a record without the field — every
/// older producer's — leaves the successor at the bottom); a record naming more
/// than exists is clamped; and a successor view the person already moved is
/// left where they put it.
#[test]
fn a_scrolled_viewport_is_restored_after_import() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
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
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
    let crossed = cross(dir.path(), &term, |_| {});
    let mut remaining = MAX_AGGREGATE_BYTES;
    let mut adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir.path(), 1),
        dir.path(),
        &mut remaining,
    );
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    successor.lock().unwrap().scroll_display(5);
    let _ = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&successor),
        history: adopted,
    }]);
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

/// THE VIEWPORT IS ANCHORED TO THE PARK'S BOTTOM, NOT TO WHEREVER OUTPUT HAS
/// PUSHED IT BY THE IMPORT. The restore runs on the import worker after
/// Commit, with the adopted reader already appending; the offset the park
/// measured, replayed from the bottom as it stands then, lands as many lines
/// below the reading position as have scrolled into history since. The adopt
/// notes the bottom it restored, and the restore adds what was pushed since.
///
/// RED before the fix: the view landed 50 lines low — `line2050`'s screen
/// instead of `line2000`'s.
#[test]
fn a_restored_viewport_holds_the_lines_read_at_the_park() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
    let read_at_park = top_line(&term.lock().unwrap());
    let crossed = cross(dir.path(), &term, |_| {});
    let mut remaining = MAX_AGGREGATE_BYTES;
    let mut adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir.path(), 1),
        dir.path(),
        &mut remaining,
    );
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    // The adopted reader, running between the Commit and the import.
    write_lines(&mut successor.lock().unwrap(), "after", 0, 50);
    let report = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&successor),
        history: adopted,
    }])
    .remove(0);
    assert_eq!(report.failed, None);
    let t = successor.lock().unwrap();
    assert_eq!(
        t.grid().display_offset(),
        1050,
        "the offset rides the lines pushed since the adopt"
    );
    assert_eq!(
        top_line(&t),
        read_at_park,
        "the person is shown the lines they were reading"
    );
}

/// INPUT SENT SINCE THE ADOPT TAKES THE VIEW BACK. A person typing at the
/// prompt the revealed window shows — before the import worker gets to the
/// restore — must not have the view yanked up into history under them. The
/// adopt notes the session's input count; any input attempt since skips the
/// restore. NEGATIVE CONTROL: the same session with no input is restored.
///
/// RED before the fix: the restore ran whatever had been typed.
#[test]
fn input_since_the_adopt_skips_the_viewport_restore() {
    for typed in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
        term.lock().unwrap().scroll_display(1000);
        let crossed = cross(dir.path(), &term, |_| {});
        let mut remaining = MAX_AGGREGATE_BYTES;
        let mut adopted = incoming(
            &crossed.manifest.sessions[0],
            false,
            &named(dir.path(), 1),
            dir.path(),
            &mut remaining,
        );
        let successor = adopt(&crossed.checkpoint, &mut adopted);
        // A sink over no descriptor: the write fails, but the ATTEMPT is what
        // the session's input count takes, exactly as for a keystroke — the
        // one egress law every key, paste and `send` writes through.
        let sink = aterm_session::sink::SinkWriter::new(-1);
        let input = Arc::new(crate::app_input::OutputEchoTracker::default());
        adopted.watch_input(&input);
        if typed {
            let _ = crate::app_input::tracked_egress(
                &successor,
                &crate::mode_mirror_of(&successor),
                &sink,
                &input,
                &crate::InputEvent::Text("ls\r".into()),
                crate::input::EgressMode::Interactive,
            );
        }
        let _ = run_imports(vec![ImportJob {
            session: 1,
            term: Arc::clone(&successor),
            history: adopted,
        }]);
        assert_eq!(
            successor.lock().unwrap().grid().display_offset(),
            if typed { 0 } else { 1000 },
            "typed={typed}"
        );
    }
}

/// A pipe whose read end never blocks: the test's stand-in for a PTY master,
/// read back to see what the session was sent. Both ends are close-on-exec: a
/// child another test of this binary spawns meanwhile must not inherit the
/// write end, or a read that expects EOF once the sink drops it sees `EAGAIN`
/// (measured: `a_pending_viewport_restore_keeps_no_pty_master_open` read -1
/// under the whole `--lib` suite, and passed alone).
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

/// An adopted session scrolled 1000 lines back at the park, adopted and ready
/// for its import: the successor and its history.
#[cfg(unix)]
fn scrolled_back_adoption(dir: &Path) -> (Arc<Mutex<Terminal>>, AdoptedHistory) {
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
    let crossed = cross(dir, &term, |_| {});
    let mut remaining = MAX_AGGREGATE_BYTES;
    let mut adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir, 1),
        dir,
        &mut remaining,
    );
    let successor = adopt(&crossed.checkpoint, &mut adopted);
    (successor, adopted)
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

/// ONLY INPUT SOMEONE SENT TAKES THE VIEW BACK — NOT A REPORT, NOT AN ENGINE
/// REPLY. The restore stands aside for a person typing at the prompt; it
/// must not stand aside for the focus report a focus-reporting program is
/// sent the moment the successor's window takes focus at Commit, nor for the
/// DA/DSR replies the engine writes — the sink's input epoch counts both, so
/// a watch on it skipped the restore in exactly the session it is for.
/// Driven through the real App routes: `write_focus_report` (the report the
/// focus delta writes), the reply writer's own sink write, and `App::input`
/// for the key. NEGATIVE CONTROL: the key skips the restore.
///
/// RED before the fix: the focus report and the reply each skipped it (the
/// sink's epoch moved for both, asserted below so the premise is measured).
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
        let _ = run_imports(vec![ImportJob {
            session: 1,
            term: Arc::clone(&successor),
            history: adopted,
        }]);
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

/// THE WATCH HOLDS NO DESCRIPTOR. An owned sink closes the PTY master when
/// its last reference goes; a tab closed while the imports run must hang up
/// its child then, not whenever the import worker reaches its job. The
/// adopted history waiting for its import keeps the session's input count,
/// never its sink: dropping the sink closes the master at once (the read end
/// sees EOF) with the watch still alive, and the watch still works.
///
/// RED before the fix: the watch held the sink, and the master stayed open
/// (the read end saw `EAGAIN`, not EOF) until the history was dropped.
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
    let mut byte = [0u8; 1];
    let n = unsafe { libc::read(pipe[0], byte.as_mut_ptr().cast(), 1) };
    assert_eq!(
        n, 0,
        "the master closed when its tab did (EOF), import pending"
    );
    let _ = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&successor),
        history: adopted,
    }]);
    assert_eq!(
        successor.lock().unwrap().grid().display_offset(),
        1000,
        "nothing was sent: the view is put back"
    );
    unsafe {
        libc::close(pipe[0]);
    }
}

/// THE ATTACH'S OWN RENUMBERING IS NOT OUTPUT. The restore adds every rise
/// of the history grid's absolute row counter since the adopt, as lines
/// output pushed into history; but an attach whose history is longer than
/// the adopt reserved (a rewrap, or a reserve that fell short) raises the
/// counter itself, moving every key — the adopt's bottom with them — and
/// pushing nothing. Here the parent sent no counter (an older producer) and
/// the reserve fell short, so the attach renumbers by the whole import; the
/// view must still land on the lines read at the park.
///
/// RED before the fix: the view landed as many lines too high as the attach
/// renumbered.
#[test]
fn an_attach_that_renumbers_does_not_move_the_restored_viewport() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let term = live(8, 40);
    write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
    term.lock().unwrap().scroll_display(1000);
    let read_at_park = top_line(&term.lock().unwrap());
    let mut crossed = cross(dir.path(), &term, |_| {});
    crossed.checkpoint.absolute_row_counter = 0;
    let mut remaining = MAX_AGGREGATE_BYTES;
    let mut adopted = incoming(
        &crossed.manifest.sessions[0],
        false,
        &named(dir.path(), 1),
        dir.path(),
        &mut remaining,
    );
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
    let report = run_imports(vec![ImportJob {
        session: 1,
        term: Arc::clone(&successor),
        history: adopted,
    }])
    .remove(0);
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

/// ROUND SIX, FINDING 24: OUTPUT AFTER THE PARK DOES NOT MOVE THE READER. At
/// Commit the adopted readers parse whatever the freeze queued BEFORE the
/// import puts the view back, so a session still printing had moved its
/// bottom by then — and a view put back as "N lines above the bottom" landed
/// on lines printed after the update. The view comes back on the very line
/// the person was reading. NEGATIVE CONTROL: with no output in between, the
/// same line (as round five's test, above, pins by offset).
#[test]
fn output_after_the_park_does_not_move_the_restored_view() {
    for post_park in [0usize, 30] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
        term.lock().unwrap().scroll_display(1000);
        let reading = top_row(&term.lock().unwrap());
        assert!(reading.starts_with("line"), "reading history: {reading:?}");
        let crossed = cross(dir.path(), &term, |_| {});
        let mut remaining = MAX_AGGREGATE_BYTES;
        let mut adopted = incoming(
            &crossed.manifest.sessions[0],
            false,
            &named(dir.path(), 1),
            dir.path(),
            &mut remaining,
        );
        let successor = adopt(&crossed.checkpoint, &mut adopted);
        // The post-Commit drain: the readers run before the import does.
        write_lines(&mut successor.lock().unwrap(), "after", 0, post_park);
        let report = run_imports(vec![ImportJob {
            session: 1,
            term: Arc::clone(&successor),
            history: adopted,
        }])
        .remove(0);
        assert_eq!(report.failed, None);
        assert_eq!(
            top_row(&successor.lock().unwrap()),
            reading,
            "{post_park} line(s) after the park: the person is back on the line they read"
        );
    }
}

/// ROUND SIX, FINDING 24 (its second half): A PERSON WORKING AT THE PROMPT IS
/// NOT PULLED UP. Keys typed before Commit replay at Commit, and more follow
/// it; paste and End do the same at the bottom. None of them moves a view that
/// is already at the bottom, so "still at the bottom" cannot tell them from
/// "nobody touched it" — and the import, which runs on a worker after Commit,
/// scrolled the prompt up the screen mid-command. Each gesture between the
/// adopt and the import now leaves the view at the bottom. NEGATIVE CONTROL:
/// with no gesture, and with only output, a bare modifier or a key release in
/// between (none of which says where the person wants to be), the view is put
/// back on the line read at the park.
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
        let term = live(8, 40);
        write_lines(&mut term.lock().unwrap(), "line", 0, 3000);
        term.lock().unwrap().scroll_display(1000);
        let reading = top_row(&term.lock().unwrap());
        let crossed = cross(dir.path(), &term, |_| {});
        let mut remaining = MAX_AGGREGATE_BYTES;
        let mut adopted = incoming(
            &crossed.manifest.sessions[0],
            false,
            &named(dir.path(), 1),
            dir.path(),
            &mut remaining,
        );
        let successor = adopt(&crossed.checkpoint, &mut adopted);
        gesture(&mut successor.lock().unwrap());
        let report = run_imports(vec![ImportJob {
            session: 1,
            term: Arc::clone(&successor),
            history: adopted,
        }])
        .remove(0);
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

/// A VIEW SCROLLED INTO HISTORY THAT DID NOT CROSS STAYS AT THE LIVE BOTTOM
/// (round seven, item 21). Under a successor policy of `carry = "visible"` or
/// `"repaint"` the park carries no history — a zero-history screen carry and
/// the withheld plan — yet the record still names where the person was
/// reading. The adopted program keeps writing after Commit, and those lines
/// are the only scrollback the successor holds when the import worker runs:
/// scrolling the view up into them shows output printed after the update, not
/// what the person was reading, and stops the pane following live output.
/// NEGATIVE CONTROL: an offset inside the history the screen carry brought is
/// still put back, riding the lines pushed since the adopt; one past it is
/// clamped to the oldest line that crossed.
///
/// RED before the fix: the withheld view was scrolled up 50 lines, into the
/// output written after the adopt.
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
        let mut remaining = MAX_AGGREGATE_BYTES;
        let mut adopted = incoming(
            &manifest.sessions[0],
            false,
            &named(dir.path(), 1),
            dir.path(),
            &mut remaining,
        );
        let successor = adopt(&checkpoint, &mut adopted);
        // The adopted program, writing between the Commit and the import.
        write_lines(&mut successor.lock().unwrap(), "after", 0, 50);
        let _ = run_imports(vec![ImportJob {
            session: 1,
            term: Arc::clone(&successor),
            history: adopted,
        }]);
        assert_eq!(
            successor.lock().unwrap().grid().display_offset(),
            want,
            "carried {carried}, scrolled {scrolled}"
        );
    }
}
