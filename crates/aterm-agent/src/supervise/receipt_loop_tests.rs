// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE LANDING'S RECEIPT IN THE LOOP (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9,
// slice 9b): told to the host before the landed face, journaled right after the landing's
// `DONE` note, read back by the next loop with that note's seq, dropped once the face moves.

use crate::harness::acts::LandReceipt;
use crate::harness::land_receipt::Statement;
use crate::harness::repo_work::{CleanAt, RepoRead};

const RECEIPT_DOOR: &str = "host.land_receipt";
const HEAD: &str = "3f9a1c2e4b7d0123456789abcdef0123456789ab";
const OTHER_HEAD: &str = "b0b0b0b0b0b01111111111111111111111111111";
const WAKE: &str = "The monitor fired: the deploy finished.";
const LANDING_SAID: &str = "Nothing left: all of it is merged into the remote main branch.";

/// What the host was told, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Told {
    Face(TaskFace),
    Receipt(Option<LandReceipt>),
}

/// A host whose probe reads `read` at every ask, recording each face and each receipt it is told.
#[derive(Debug)]
struct Receipts {
    told: Mutex<Vec<Told>>,
    read: RepoRead,
    finished: Option<bool>,
}

impl Receipts {
    fn over(read: RepoRead, finished: Option<bool>) -> Arc<Self> {
        Arc::new(Self {
            told: Mutex::new(Vec::new()),
            read,
            finished,
        })
    }

    fn told(&self) -> Vec<Told> {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn receipts(&self) -> Vec<Option<LandReceipt>> {
        self.told()
            .into_iter()
            .filter_map(|t| match t {
                Told::Receipt(r) => Some(r),
                Told::Face(_) => None,
            })
            .collect()
    }

    fn push(&self, told: Told) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(told);
    }
}

impl IdleHost for Receipts {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn finished(&self) -> Option<bool> {
        self.finished
    }
    fn repo_work(&self) -> Option<RepoRead> {
        Some(self.read.clone())
    }
    fn task_face(&self, face: TaskFace) {
        self.push(Told::Face(face));
    }
    fn task_face_noted(&self, face: TaskFace, _noted_ms: i64) {
        self.push(Told::Face(face));
    }
    fn land_receipt(&self, receipt: Option<&LandReceipt>) {
        self.push(Told::Receipt(receipt.cloned()));
    }
}

/// A clean repository at `head`, on `origin/main`.
fn on_main(head: &str) -> RepoRead {
    on_main_stating(head, Statement::Unread)
}

/// [`on_main`], its head stating `statement` of how it was judged (design B10).
fn on_main_stating(head: &str, statement: Statement) -> RepoRead {
    RepoRead::Checked(CleanAt {
        repo: "/Users//a/Developer/parser".into(),
        head: head.into(),
        main_ref: "origin/main".into(),
        remote: true,
        branch: "main".into(),
        statement,
    })
}

fn opts_for(host: &Arc<Receipts>, ledger: Option<&Path>) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        journal: ledger.map(crate::supervise::approvals::journal_beside),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..auto(30)
    }
}

fn mock(screens: Vec<Vec<String>>, gates: Vec<usize>) -> Mock {
    let mut m = Mock::new(screens);
    m.autopilot = Some("land");
    m.turn_gates = gates;
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.history_served();
    m
}

fn run_receipts(
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    host: &Arc<Receipts>,
    ledger: Option<&Path>,
) -> (Vec<String>, usize) {
    run_receipts_ticking(screens, gates, host, ledger, 0)
}

/// [`run_receipts`] on a wall clock that each read moves on by `tick_ms`
/// (`ScriptedClock::set_tick_ms`), so two reads of it never agree where the tick is not 0.
fn run_receipts_ticking(
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    host: &Arc<Receipts>,
    ledger: Option<&Path>,
    tick_ms: i64,
) -> (Vec<String>, usize) {
    let mut m = mock(screens, gates);
    let (lines, _) = watch_lines_with(&mut m, &opts_for(host, ledger), |s| {
        s.test_clock().set_tick_ms(tick_ms);
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        s.set_approval_ledger(ledger.map(Path::to_path_buf));
    });
    let turns = count(&m, "turn");
    (lines, turns)
}

/// Progress, the land instruction, its defense (where the land check is typed), the check's
/// turn and its `LANDED` ([`landing`]): run with the gates [`LANDING_GATES`].
fn a_landing() -> Vec<Vec<String>> {
    let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
    screens.extend(landing());
    screens
}

/// [`a_landing`]'s turns: the instruction's, then the land check's.
const LANDING_GATES: [usize; 2] = [1, 3];

/// The journal's `DONE` notes of the land rule and the `RECEIPT` lines, in order, each with its
/// record's time and first time.
fn ends(ledger: &Path) -> Vec<(String, i64, Option<i64>)> {
    let (records, _) = crate::supervise::journal::read_journal(
        &crate::supervise::approvals::journal_beside(ledger),
    )
    .expect("journal");
    records
        .into_iter()
        .filter(|r| landed_note(&r.line) || r.line.starts_with("RECEIPT "))
        .map(|r| (r.line, r.t, r.ended))
        .collect()
}

/// The seq a `DONE` note names.
fn seq_of(line: &str) -> u64 {
    line.split(' ')
        .find_map(|w| w.strip_prefix("seq="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a seq: {line}"))
}

/// THE RECEIPT IS TOLD BEFORE THE FACE (§14.9, slice 9b): a fresh landing tells the host what
/// aterm checked (the probe's reading at the landing, the agent's defense) and then the landed
/// face; someone else's turn tells `None` and then the open face. The journal says the receipt
/// right after the landing's `DONE` note, with its seq. A plain folder's landing is a no-repo
/// receipt. NEGATIVE CONTROL (run by hand): the face told before the receipt in `tell_face_over`
/// fails the order.
#[test]
fn the_receipt_is_told_before_the_face() {
    let (dir, ledger) = files("receipt-order", &[], &[]);
    let host = Receipts::over(on_main(HEAD), None);
    let mut screens = a_landing();
    screens.push(busy_screen());
    let (lines, turns) = run_receipts(screens, LANDING_GATES.to_vec(), &host, Some(&ledger));
    assert_eq!(turns, 2, "the instruction, then the land check: {lines:#?}");
    let told = host.told();
    let receipt = match told.get(1) {
        Some(Told::Receipt(Some(r))) => r.clone(),
        _ => panic!("the receipt second, before the landed face: {told:#?}"),
    };
    assert_eq!(
        told,
        [
            Told::Face(TaskFace::Open),
            Told::Receipt(Some(receipt.clone())),
            Told::Face(TaskFace::Landed),
            Told::Receipt(None),
            Told::Face(TaskFace::Open),
        ],
        "{lines:#?}"
    );
    assert_eq!(
        (
            receipt.head.as_str(),
            receipt.main_ref.as_str(),
            receipt.leg,
            receipt.repo.as_str(),
            receipt.check,
            receipt.said.as_str(),
        ),
        (
            &HEAD[..12],
            "origin/main",
            crate::harness::acts::ReceiptLeg::OnMain,
            "/Users//a/Developer/parser",
            true,
            LANDING_SAID,
        ),
        "the land check answered, its audited defense quoted, never the word"
    );
    assert!(receipt.at_ms > 0, "{receipt:?}");
    let ends = ends(&ledger);
    let [(done, _, _), (line, t, first)] = &ends[..] else {
        panic!("one DONE note, then its receipt: {ends:#?}");
    };
    assert!(landed_note(done), "{done}");
    assert_eq!(*line, receipt.line(seq_of(done)), "{ends:#?}");
    assert_eq!(*first, Some(receipt.at_ms), "{ends:#?}");
    assert!(*t >= receipt.at_ms, "{ends:#?}");
    let _ = std::fs::remove_dir_all(&dir);

    let host = Receipts::over(RepoRead::Clean, None);
    let (lines, _) = run_receipts(a_landing(), LANDING_GATES.to_vec(), &host, None);
    let r = LandReceipt {
        check: true,
        said: LANDING_SAID.into(),
        at_ms: host.receipts()[0].as_ref().map_or(0, |r| r.at_ms),
        ..LandReceipt::default()
    };
    assert_eq!(
        host.told(),
        [
            Told::Face(TaskFace::Open),
            Told::Receipt(Some(r)),
            Told::Face(TaskFace::Landed),
        ],
        "a plain folder's landing: {lines:#?}"
    );
}

/// A HEAD THAT STATES NO RECEIPT NEVER HOLDS A LANDING (design B10): a landing whose head states
/// how it was judged and one whose head states nothing land alike, the same faces in the same
/// order; each receipt records what its head states, and its journal line says so (`stated=`).
/// NEGATIVE CONTROL: the two receipts differ in their statement alone.
#[test]
fn a_landing_lands_alike_whether_its_head_states_a_receipt_or_none() {
    let judged = Statement::Stated(crate::harness::land_receipt::Judgment {
        tree: "0a1b2c3d4e5f".into(),
        rejudges: 0,
        onto: String::new(),
        inherited: 0,
        secs: 2712,
    });
    let mut receipts = Vec::new();
    for (name, statement) in [
        ("receipt-stated", judged),
        ("receipt-missing", Statement::Missing),
    ] {
        let (dir, ledger) = files(name, &[], &[]);
        let host = Receipts::over(on_main_stating(HEAD, statement.clone()), None);
        let (lines, turns) =
            run_receipts(a_landing(), LANDING_GATES.to_vec(), &host, Some(&ledger));
        assert_eq!(turns, 2, "the instruction, then the land check: {lines:#?}");
        let told = host.told();
        let Some(Told::Receipt(Some(receipt))) = told.get(1).cloned() else {
            panic!("{name}: {told:#?}");
        };
        assert_eq!(
            told,
            [
                Told::Face(TaskFace::Open),
                Told::Receipt(Some(receipt.clone())),
                Told::Face(TaskFace::Landed),
            ],
            "{name}: {lines:#?}"
        );
        assert_eq!(receipt.statement, statement, "{name}");
        let ends = ends(&ledger);
        let [(done, _, _), (line, _, _)] = &ends[..] else {
            panic!("{name}: one DONE note, then its receipt: {ends:#?}");
        };
        assert!(landed_note(done), "{done}");
        assert_eq!(*line, receipt.line(seq_of(done)), "{ends:#?}");
        let word = statement.word().expect("a read statement");
        assert!(line.contains(&format!(" stated={word} ")), "{line}");
        receipts.push(receipt);
        let _ = std::fs::remove_dir_all(&dir);
    }
    let [stated, missing] = &receipts[..] else {
        panic!("{receipts:#?}");
    };
    assert_ne!(stated.statement, missing.statement);
    assert_eq!(
        LandReceipt {
            statement: Statement::Missing,
            at_ms: missing.at_ms,
            ..stated.clone()
        },
        *missing,
        "only the statement differs"
    );
}

/// A WAKE-KEPT LANDING TELLS NO NEW RECEIPT: the task's own wake keeps the landing, and the host
/// hears nothing more of it; the wake's restated `DONE` note is followed by the same receipt under
/// the new seq (its head, its words, its time), so a later loop reading that note finds it. The
/// restated note's first time is the receipt's: the landing's one wall read made both
/// (§14.9.3 departure 3), on a clock each read moves on, so a second read would differ.
/// NEGATIVE CONTROLS (run by hand): the door told at each `DONE` note fails the tells; the note
/// reading the clock again for its own first time (`task_edge_with`) fails the shared time.
#[test]
fn a_wake_kept_landing_tells_no_new_receipt() {
    let (dir, ledger) = files("receipt-wake", &[], &[]);
    let host = Receipts::over(on_main(HEAD), Some(true));
    let mut screens = a_landing();
    screens.extend([busy_screen(), ended(WAKE)]);
    let (lines, turns) =
        run_receipts_ticking(screens, LANDING_GATES.to_vec(), &host, Some(&ledger), 1);
    assert_eq!(turns, 2, "the instruction, then the land check: {lines:#?}");
    let told = host.told();
    let Some(Told::Receipt(Some(receipt))) = told.get(1).cloned() else {
        panic!("{told:#?}");
    };
    assert_eq!(
        told,
        [
            Told::Face(TaskFace::Open),
            Told::Receipt(Some(receipt.clone())),
            Told::Face(TaskFace::Landed),
        ],
        "{lines:#?}"
    );
    let ends = ends(&ledger);
    let [
        (landed, _, landed_first),
        (first, _, _),
        (kept, _, kept_first),
        (again, _, _),
    ] = &ends[..]
    else {
        panic!("the landing and its receipt, the wake's and its: {ends:#?}");
    };
    assert!(
        kept.contains(crate::supervise::policy::turn_end::WAKE_KEPT),
        "{ends:#?}"
    );
    assert_eq!(*landed_first, None, "a fresh note: {ends:#?}");
    assert_eq!(
        *kept_first,
        Some(receipt.at_ms),
        "the wake's note restates the landing's one read: {ends:#?}"
    );
    assert!(seq_of(kept) > seq_of(landed), "{ends:#?}");
    assert_eq!(*first, receipt.line(seq_of(landed)), "{ends:#?}");
    assert_eq!(*again, receipt.line(seq_of(kept)), "{ends:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RESTARTED LOOP RESTATES THE RECEIPT: a loop started over the books of a landing reads the
/// `RECEIPT` journaled with the landing's `DONE` note and tells it before the landed face, as it
/// was (the first loop's head and time, though its own probe now reads another head), and
/// journals it again under its restated note. NEGATIVE CONTROL: a `RECEIPT` with another seq than
/// the note's is not read, and the host hears of no receipt.
#[test]
fn a_restarted_loop_restates_the_receipt() {
    let (dir, ledger) = files("receipt-restart", &[], &[]);
    let host = Receipts::over(on_main(HEAD), None);
    run_receipts(a_landing(), LANDING_GATES.to_vec(), &host, Some(&ledger));
    let first = host.receipts()[0].clone().expect("the landing's receipt");
    for again in 0..2 {
        let host = Receipts::over(on_main(OTHER_HEAD), None);
        let (lines, turns) = run_receipts(vec![ended(LANDED)], Vec::new(), &host, Some(&ledger));
        assert_eq!(turns, 0, "{again}: {lines:#?}");
        assert_eq!(
            host.told(),
            [
                Told::Receipt(Some(first.clone())),
                Told::Face(TaskFace::Landed)
            ],
            "{again}: {lines:#?}"
        );
        let ends = ends(&ledger);
        let [.., (done, _, _), (line, _, at)] = &ends[..] else {
            panic!("{again}: {ends:#?}");
        };
        assert!(landed_note(done), "{again}: {ends:#?}");
        assert_eq!(*line, first.line(seq_of(done)), "{again}: {ends:#?}");
        assert_eq!(*at, Some(first.at_ms), "{again}: {ends:#?}");
    }
    let _ = std::fs::remove_dir_all(&dir);

    let min = |n: u64| Duration::from_secs(60 * n);
    let restart = |tag: &str, receipt_seq: u64| {
        let receipt = first.line(receipt_seq);
        let (dir, path) = files(
            tag,
            &[ledger_row(RULE_LAND, "land", min(10))],
            &[
                journal_row(LANDED_NOTE, min(9), None),
                journal_row(&receipt, min(9), None),
            ],
        );
        let host = Receipts::over(on_main(OTHER_HEAD), None);
        let (lines, turns) =
            run_receipts(vec![ended(LANDING_REPLY)], Vec::new(), &host, Some(&path));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(turns, 0, "{tag}: {lines:#?}");
        host.receipts()
    };
    let read = restart("receipt-seq", seq_of(LANDED_NOTE));
    assert_eq!(
        read.iter()
            .map(|r| r.as_ref().map(|r| r.head.as_str()))
            .collect::<Vec<_>>(),
        [Some(&HEAD[..12])],
        "the note's own receipt: {read:#?}"
    );
    assert_eq!(
        restart("receipt-other-seq", seq_of(LANDED_NOTE) - 1),
        [],
        "another seq's receipt is not read"
    );
}

/// A RESTART OVER A LANDING NOTED WITH NO RECEIPT (a journal from before 9b) tells no receipt
/// and writes none: the landing restates as it did, its face told at the loop's start, and
/// nothing is made of the restart's own reading, which the probe leaves unread for the seed's
/// words, so no `no-repo` receipt is stamped with the old landing's time. The restated note
/// stands alone. NEGATIVE CONTROL (run by hand): `tell_start_face` telling the receipt it holds
/// whatever it is (`None` included) fails the tells.
#[test]
fn a_restart_over_a_landing_with_no_receipt_tells_none_and_writes_none() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let (dir, path) = files(
        "receipt-none-before",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[journal_row(LANDED_NOTE, min(9), None)],
    );
    let host = Receipts::over(on_main(OTHER_HEAD), None);
    let (lines, turns) = run_receipts(vec![ended(LANDING_REPLY)], Vec::new(), &host, Some(&path));
    let ends = ends(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.told(), [Told::Face(TaskFace::Landed)], "{lines:#?}");
    assert!(
        ends.len() == 2 && ends.iter().all(|(line, _, _)| landed_note(line)),
        "the old note and its restatement, no RECEIPT: {ends:#?}"
    );
}

/// THE RECEIPT IS A TAPED NOTIFICATION AT THE LOOP: a landing's tape replays strictly, and the
/// same tape with its receipt tells taken off replays in acts mode with every decision the same,
/// the tells the outputs no act matches, while its strict replay differs first at the call taken
/// off. That tape is not one main recorded: main's also reads the landing's wall clock after the
/// face, where this build reads it before (§14.9.3 departure 3), so a strict replay of a real
/// tape of main's differs first at that read (the corpus's `c5-land`, §14.9.3).
#[test]
fn a_landing_tape_without_its_receipt_tells_replays_in_acts_mode_and_strictly_differs_there() {
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;

    let host = Receipts::over(on_main(HEAD), None);
    let mut screens = a_landing();
    screens.push(busy_screen());
    let mut m = mock(screens, LANDING_GATES.to_vec());
    m.vanish_after = Some(8);
    m.stall_sleep = Some(Duration::from_secs(20));
    let opts = opts_for(&host, None);
    let run = record_watch(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    crate::supervise::tape::testing::assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    let tells = host.receipts().len();
    assert_eq!(
        tells,
        2,
        "the landing's and the move off it: {:#?}",
        host.told()
    );
    let tape = crate::supervise::tape::show::open(&run.tape.reframed(&run.header.encode()), false)
        .expect("the tape reads");
    let kept: Vec<_> = tape
        .items
        .iter()
        .filter(|i| {
            !matches!(i.record(),
                Some(astream_effects::EffectRecord::Emit { door, .. }) if door == RECEIPT_DOOR)
        })
        .cloned()
        .collect();
    assert_eq!(tape.items.len() - kept.len(), tells, "every tell taken off");
    let old = crate::supervise::tape::text::render(tape.header.as_deref(), &kept).into_bytes();
    let strict = replay(&old, &Options::default()).expect("a strict replay");
    match &strict.verdict {
        Verdict::Diverged(d) => {
            assert_eq!(d.door.as_deref(), Some(RECEIPT_DOOR), "{}", strict.render());
        }
        other => panic!("the strict replay differs at the call taken off: {other:?}"),
    }
    let acts = replay(
        &old,
        &Options {
            candidate: Some(crate::supervise::tape::acts::Candidate::default()),
            ..Options::default()
        },
    )
    .expect("an acts replay");
    assert_eq!(acts.verdict, Verdict::Reproduced, "{}", acts.render());
    let account = &acts.acts.as_ref().expect("an acts report").account;
    assert_eq!(account.unmatched_outputs, tells, "{account:?}");
    assert_eq!(account.taken, account.acts, "{account:?}");
    assert_eq!(
        (
            account.dropped,
            account.repeated,
            account.keyed,
            account.tainted
        ),
        (0, 0, 0, 0),
        "{account:?}"
    );
}

/// A LANDING MARKS WHAT ATERM CHECKED (§14.9, slice 9c): the task record's `done` says the
/// receipt's `checked=` word, and the result's sidecar keeps it for `result`'s header:
/// `<head>@origin/main` on main, `no-repo` in a plain folder. A landing with no receipt marks
/// none: a loop restarted over a landing a build before the receipt noted (its journal holds no
/// `RECEIPT`) marks the record still working with no `checked=`, though its own probe now reads a
/// head on main. NEGATIVE CONTROLS (run by hand): `checked` taken as `None` in `publish_done`
/// fails the first two; read off the probe's reading instead of the receipt, the third.
#[test]
fn a_landing_marks_what_aterm_checked() {
    let root = crate::supervise::ScratchDir::new("receipt", "checked");
    let mark = |read: RepoRead,
                screens: Vec<Vec<String>>,
                gates: Vec<usize>,
                ledger: Option<&Path>,
                tag: &str| {
        let host = Receipts::over(read, None);
        let mut m = mock(screens, gates);
        m.task = Some((
            1,
            "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
        ));
        let opts = SuperviseOpts {
            tasks: Some(root.join(tag)),
            ..opts_for(&host, ledger)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(ledger.map(Path::to_path_buf));
        });
        let marks: Vec<String> = m
            .task_requests
            .iter()
            .filter(|r| r.starts_with("task mark done "))
            .cloned()
            .collect();
        let [mark] = marks.as_slice() else {
            panic!("{tag}: one done: {marks:?}: {lines:#?}");
        };
        let field = |key: &str| {
            mark.split(' ')
                .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        let path = PathBuf::from(aterm_codec::pct::decode(
            &field("result_path").unwrap_or_else(|| panic!("{tag}: a result: {mark}")),
        ));
        (field("checked"), crate::harness::result::checked_of(&path))
    };
    let on = format!("{}@origin/main", &HEAD[..12]);
    assert_eq!(
        mark(on_main(HEAD), a_landing(), vec![1], None, "on-main"),
        (Some(on.clone()), Some(on))
    );
    let plain = Some("no-repo".to_string());
    assert_eq!(
        mark(RepoRead::Clean, a_landing(), vec![1], None, "plain"),
        (plain.clone(), plain)
    );
    let min = |n: u64| Duration::from_secs(60 * n);
    let (dir, ledger) = files(
        "receipt-unchecked",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[journal_row(LANDED_NOTE, min(9), None)],
    );
    let restated = mark(
        on_main(OTHER_HEAD),
        vec![ended(LANDING_REPLY)],
        Vec::new(),
        Some(&ledger),
        "restated",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(restated, (None, None), "no receipt, no checked=");
}

const COULD_NOT_FINISH_REPLY: &str = "Could not finish: the grammar the parser needs was deleted.";

fn working_task() -> Option<(u64, String)> {
    Some((
        1,
        "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
    ))
}

/// The marks the loop asked for and how many turns it typed, on these `screens` and `task`.
fn run_with_task(
    tag: &str,
    screens: Vec<Vec<String>>,
    gates: Vec<usize>,
    task: Option<(u64, String)>,
) -> (Vec<String>, usize, Vec<String>) {
    let root = crate::supervise::ScratchDir::new("conclusion", tag);
    let host = Receipts::over(on_main(HEAD), None);
    let mut m = mock(screens, gates);
    m.task = task;
    let opts = SuperviseOpts {
        tasks: Some(root.join(tag)),
        ..opts_for(&host, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let marks = m
        .task_requests
        .iter()
        .filter(|r| r.starts_with("task mark "))
        .cloned()
        .collect();
    let turns = count(&m, "turn");
    (marks, turns, lines)
}

/// A MANAGED WORKER'S `Could not finish:` MARKS ITS RECORD AND ENDS THE TYPING: Land's instruction
/// went in, the reply came, and nothing follows it, however long the screen stands. The mark names
/// no `checked=`, since nothing was. NEGATIVE CONTROL: the same reply in a session with no task
/// record is a report like any other, and Land nudges on.
#[test]
fn a_could_not_finish_reply_marks_failed_and_types_nothing_more() {
    let reply = || {
        vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(COULD_NOT_FINISH_REPLY),
        ]
    };
    let (marks, turns, lines) = run_with_task("managed", reply(), vec![1], working_task());
    assert_eq!(turns, 1, "the instruction alone: {lines:#?}");
    let [mark] = marks.as_slice() else {
        panic!("one mark: {marks:?}: {lines:#?}");
    };
    assert!(
        mark.starts_with("task mark failed why=could-not-finish"),
        "{mark}"
    );
    assert!(!mark.contains("checked="), "{mark}");

    let (marks, turns, lines) = run_with_task("unmanaged", reply(), vec![1, 3], None);
    assert!(marks.is_empty(), "{marks:?}");
    assert!(turns >= 2, "Land goes on: {lines:#?}");
}

/// A CONCLUSION THE LOOP BEFORE THIS ONE MADE STANDS IN THIS ONE (a harness restart): nothing is
/// typed at the reply it read and nothing is marked. NEGATIVE CONTROL: a record no `GaveUp` word
/// names (`why=off`, the Autopilot word's to hold) seeds no rest, so the same screen under Land
/// types the instruction.
#[test]
fn a_restarted_loop_takes_a_failed_record_and_types_nothing() {
    let failed = |why: &str| {
        Some((
            3,
            format!("state=failed from=s-0a1b since_ms=0 result_bytes=9 why={why}"),
        ))
    };
    for why in ["could-not-finish", "repeated"] {
        let (marks, turns, lines) =
            run_with_task(why, vec![ended(PROGRESS)], Vec::new(), failed(why));
        assert_eq!(turns, 0, "{why}: {lines:#?}");
        assert!(marks.is_empty(), "{why}: {marks:?}");
    }
    let (_, turns, lines) = run_with_task(
        "off",
        vec![ended(PROGRESS), busy_screen()],
        vec![0],
        failed("off"),
    );
    assert!(turns >= 1, "the instruction: {lines:#?}");
}

/// THE SAME REPLY ENDS A MANAGED WORKER'S WORK: Land's instruction and its answer went in, the
/// question came back word for word after no work, and the stop that takes the point marks `failed
/// why=repeated` and types nothing new. NEGATIVE CONTROL: with no task record the same stop rests
/// and marks nothing.
#[test]
fn the_same_reply_again_marks_failed_and_types_nothing_new() {
    let asked = || {
        let mut r = rows(&[
            "⏺ Does the parser have to keep the old syntax?",
            "",
            "✻ Worked for 5s · done 4:24 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let reply = || {
        vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            asked(),
            busy_screen(),
            asked(),
        ]
    };
    let (marks, turns, lines) = run_with_task("repeated", reply(), vec![1, 3], working_task());
    assert_eq!(turns, 2, "the instruction and the answer: {lines:#?}");
    let [mark] = marks.as_slice() else {
        panic!("one mark: {marks:?}: {lines:#?}");
    };
    assert!(mark.starts_with("task mark failed why=repeated"), "{mark}");

    let (marks, turns, lines) = run_with_task("rests", reply(), vec![1, 3], None);
    assert!(marks.is_empty(), "{marks:?}");
    assert_eq!(turns, 2, "{lines:#?}");
}

/// What the loop asked for at the weekly wall two days off ([`weekly_limited`], the test clock's
/// Sep 17 against its Sep 19 reset), with the record `task` under `policy`: its marks, how many
/// turns it typed, and the attention it set.
fn run_at_far_wall(
    tag: &str,
    task: Option<(u64, String)>,
    policy: impl FnOnce(SupervisorConfig) -> SupervisorConfig,
) -> (Vec<String>, usize, Vec<String>, Vec<String>) {
    let root = crate::supervise::ScratchDir::new("usage-wall", tag);
    let host = Receipts::over(on_main(HEAD), None);
    let mut m = mock(vec![busy_screen(), weekly_limited()], Vec::new());
    m.task = task;
    let base = opts_for(&host, None);
    let opts = SuperviseOpts {
        tasks: Some(root.join(tag)),
        policy: policy(base.policy.clone()),
        ..base
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |_| {});
    let marks = m
        .task_requests
        .iter()
        .filter(|r| r.starts_with("task mark "))
        .cloned()
        .collect();
    let told = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention"))
        .cloned()
        .collect();
    (marks, count(&m, "turn"), told, lines)
}

/// Managed workers retain their open task while waiting for quota, just as a person's session does.
#[test]
fn a_managed_workers_far_usage_wall_keeps_its_task_open() {
    let limited = |told: &[String]| told.iter().any(|t| t.contains(ATTENTION_PREFIX));
    for resumes in [true, false] {
        let policy = |p: SupervisorConfig| SupervisorConfig {
            resume_limits: resumes,
            ..p
        };
        let tag = if resumes { "managed" } else { "managed-kept" };
        let (marks, turns, told, lines) = run_at_far_wall(tag, working_task(), policy);
        assert!(
            marks.is_empty(),
            "resume_limits={resumes}: {marks:?}: {lines:#?}"
        );
        assert_eq!(turns, 0, "resume_limits={resumes}: {lines:#?}");
        assert_eq!(
            limited(&told),
            !resumes,
            "resume_limits={resumes}: {told:?}"
        );

        let tag = if resumes { "person" } else { "person-kept" };
        let (marks, turns, told, lines) = run_at_far_wall(tag, None, policy);
        assert!(marks.is_empty(), "CONTROL {marks:?}");
        assert_eq!(turns, 0, "CONTROL: {lines:#?}");
        assert_eq!(
            limited(&told),
            !resumes,
            "CONTROL resume_limits={resumes}: {told:?}"
        );
    }
}
