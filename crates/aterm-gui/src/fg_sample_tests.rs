// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`FgSampler`] tests over a scripted foreground, a scripted leader reader, a scripted start
//! clock and a ran log whose parse stage the test drives, on hand-set instants.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{FgSampler, LOOK, STEP_MAX, Sight, SightSeam};
use crate::command_ran::{
    During, ExecNames, Places, Ran, RanFeed, Steering, Table, Window, test_id,
};

/// A flag and path from different births cannot form an affirmative sampled execution, even
/// if the old process had exec'd and its numerical PID was immediately reused by a fork.
#[test]
fn an_exec_flag_and_sampled_path_must_share_one_birth() {
    for births in [
        [Some(10), Some(20), Some(20)],
        [Some(10), Some(10), Some(20)],
        [Some(10), Some(10), None],
    ] {
        let mut births = births.into_iter();
        let sight = super::read_sight_using(
            42,
            |_| births.next().flatten(),
            |_| Some(true),
            |_| Some("/bin/new-fork-shell".into()),
        );
        assert!(
            sight.is_none(),
            "a reused or unread birth supplies no image"
        );
    }
    let sight = super::read_sight_using(
        42,
        |_| Some(10),
        |_| Some(true),
        |_| Some("/bin/real-command".into()),
    )
    .expect("one unchanged executed process");
    assert_eq!((sight.pgid, sight.start_us), (42, 10));
    assert_eq!(sight.exe.as_ref(), Path::new("/bin/real-command"));
    assert!(
        super::read_sight_using(
            42,
            |_| Some(10),
            |_| Some(false),
            |_| { panic!("a never-exec'd fork supplies no program path") }
        )
        .is_none()
    );
}

/// Each scripted group's leader, by group, and how often each was read.
static SIGHTS: Mutex<Option<HashMap<i32, (Sight, usize)>>> = Mutex::new(None);

fn set_sight(pgid: i32, start_us: u64, exe: &str) {
    SIGHTS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(
            pgid,
            (
                Sight {
                    pgid,
                    start_us,
                    exe: Arc::from(Path::new(exe)),
                },
                0,
            ),
        );
}

fn scripted_sight(pgid: i32) -> Option<Sight> {
    let mut sights = SIGHTS.lock().unwrap();
    let (sight, reads) = sights.as_mut()?.get_mut(&pgid)?;
    *reads += 1;
    Some(sight.clone())
}

thread_local! {
    /// The scripted start clock of this test's thread ([`scripted_clock`]).
    static CLOCK: Cell<Option<u64>> = const { Cell::new(None) };
    /// The scripted start record's tick on this test's thread ([`scripted_tick`]).
    static TICK: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Set this thread's scripted start clock to `us` microseconds.
fn set_clock(us: u64) {
    CLOCK.with(|c| c.set(Some(us)));
}

fn scripted_clock() -> Option<u64> {
    CLOCK.with(Cell::get)
}

/// Set how far this thread's scripted starts can read before the real ones, in microseconds.
fn set_tick(us: u64) {
    TICK.with(|c| c.set(Some(us)));
}

fn scripted_tick() -> Option<u64> {
    TICK.with(Cell::get)
}

/// The scripted seam: [`scripted_sight`], [`scripted_clock`] and [`scripted_tick`].
const SEAM: SightSeam = SightSeam {
    read: scripted_sight,
    now_us: scripted_clock,
    tick_us: scripted_tick,
};

fn sight_reads(pgid: i32) -> usize {
    SIGHTS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pgid))
        .map_or(0, |(_, reads)| *reads)
}

/// The names the feed reads of a process: its scripted leader's file name and that file's test
/// identity ([`test_id`]).
fn scripted_names(pid: i32, _steer: Steering<'_>) -> ExecNames {
    let name = SIGHTS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pid))
        .and_then(|(s, _)| s.exe.file_name().map(|n| n.to_string_lossy().into_owned()));
    ExecNames {
        file: name.as_deref().map(test_id),
        image: name.map(Into::into),
        ..ExecNames::default()
    }
}

/// The gather hands on `len` bytes, which the parse stage parses with `block` executing at
/// their end, at wall ms `now`.
fn parse(ran: &Ran, len: usize, block: Option<u64>, now: u64) {
    let end = ran.handed_on(len);
    parsed(ran, end, len, block, now);
}

/// The parse stage parses the batch of `len` bytes ending at `end`, `block` executing at its
/// end, at wall ms `now`.
fn parsed(ran: &Ran, end: u64, len: usize, block: Option<u64>, now: u64) {
    let mut places = Places::default();
    ran.places_in(end, len, &mut places);
    places.parsed(len, block);
    ran.parsed_at(end, len, &places, now);
}

fn placed(ran: &Ran, pgid: i32) -> Vec<During> {
    ran.within(Some(Window { from: 0, to: None }))
        .into_iter()
        .filter(|e| e.pgid == pgid)
        .map(|e| e.during)
        .collect()
}

/// A block whose job prints nothing: once the parse stage finds the block executing, the
/// sampler reads the group in front when it is due, and while the shell holds the terminal it
/// reads no leader; the job's group in front, its leader's image is heard once, placed in the
/// block as its job's, so it stands in for a typed word that is no program file (an alias, a
/// function) and confirms one whose file it is; later samples of the same image add nothing, and
/// the leader's re-exec (a stub that runs itself again as another file) is heard again.
/// NEGATIVE CONTROLS (scratch): the sample heard placed by its stamp (`RanFeed::image`'s spot)
/// fails "stands in"; hearing every sample fails "heard once".
#[test]
fn a_silent_jobs_image_is_heard_once_placed_in_its_block() {
    const SH: i32 = 71_000;
    const JOB: i32 = 71_001;
    set_sight(JOB, 5, "/bin/sleep");
    let ran = Arc::new(Ran::with_files(
        Box::new(Table::default().program("/bin/sleep", "sleep")),
        None,
    ));
    ran.spawned_with(Some("/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    parse(&ran, 40, Some(3), 1_000);
    s.handed(t0);
    let mut now = t0 + LOOK;
    assert!(!s.tick(now, &feed, || SH, || false), "the shell in front");
    assert_eq!(sight_reads(SH), 0, "the shell's leader is never read");
    for _ in 0..4 {
        now += STEP_MAX;
        let _ = s.tick(now, &feed, || JOB, || false);
    }
    assert_eq!(sight_reads(JOB), 4, "each sample reads the leader");
    let heard = ran.heard(3, Window::of(Some(900), None, None, None), None);
    let reading =
        |line: &str| crate::session_status::command_reading(line, &heard).map(|r| (r.text, r.own));
    assert_eq!(
        reading("qzwait 2"),
        Some(("sleep".to_owned(), false)),
        "stands in for a word that is no program file"
    );
    assert_eq!(
        reading("sleep 2"),
        Some(("sleep".to_owned(), true)),
        "confirms the word whose file it is"
    );
    assert_eq!(
        placed(&ran, JOB),
        [During::Job(Some(3))],
        "heard once, placed in its block"
    );
    set_sight(JOB, 5, "/opt/py/Python");
    now += STEP_MAX;
    assert!(
        s.tick(now, &feed, || JOB, || false),
        "a re-exec is a new image"
    );
    assert_eq!(placed(&ran, JOB).len(), 2);
}

/// A sample that finds a group in front after the block's D was handed on, before the parse
/// stage parsed it (a prompt hook's job, its D already written), is placed past that D: it
/// counts for no block, so it confirms nothing in the block it followed, not even the word whose
/// file it is. NEGATIVE CONTROL (scratch): the sample heard placed by its stamp falls in the
/// closed block's window (its D plus `LAG_MS`) and confirms `qzhook`.
#[test]
fn a_sample_taken_after_d_never_counts_for_the_closed_block() {
    const SH: i32 = 72_000;
    const HOOK: i32 = 72_001;
    set_sight(HOOK, 9, "/usr/bin/qzhook");
    let ran = Arc::new(Ran::with_files(
        Box::new(Table::default().program("/usr/bin/qzhook", "qzhook")),
        None,
    ));
    ran.spawned_with(Some("/usr/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    parse(&ran, 40, Some(6), 1_000);
    let end = ran.handed_on(12);
    s.handed(t0);
    assert!(
        s.tick(t0 + LOOK, &feed, || HOOK, || false),
        "behind the gather, the hook's group is sampled"
    );
    assert_eq!(ran.waiting(), 1, "held for the parse");
    parsed(&ran, end, 12, None, 1_550);
    let heard = ran.heard(6, Window::of(Some(1_000), Some(1_500), None, None), None);
    assert_eq!(
        crate::session_status::command_reading("qzhook", &heard).map(|r| r.text),
        None,
        "it confirms nothing in the closed block"
    );
    assert_eq!(
        placed(&ran, HOOK),
        [During::Job(None)],
        "past D: no block's"
    );
}

/// No leader is read while the master holds a byte the gather has not read (its place before or
/// after the foreground read is not known), nor while the shell's own group is in front, nor for
/// a group that cannot be read; the next due sample reads it. NEGATIVE CONTROLS (scratch): no
/// unread check fails "nothing heard while output waits"; no shell check reads the shell's
/// leader and fails the shell's tick.
#[test]
fn a_sample_waits_for_unread_output_and_never_reads_the_shells_leader() {
    const SH: i32 = 73_000;
    const JOB: i32 = 73_001;
    const GONE: i32 = 73_002;
    set_sight(SH, 1, "/bin/zsh");
    set_sight(JOB, 4, "/usr/bin/qzquiet");
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    parse(&ran, 10, Some(1), 1_000);
    s.handed(t0);
    let mut now = t0 + LOOK;
    assert!(!s.tick(now, &feed, || JOB, || true));
    assert_eq!(
        (sight_reads(JOB), placed(&ran, JOB).len()),
        (0, 0),
        "nothing heard while output waits"
    );
    for fg in [SH, GONE] {
        now += STEP_MAX;
        assert!(!s.tick(now, &feed, || fg, || false));
    }
    assert_eq!(sight_reads(SH), 0, "the shell's leader is never read");
    assert!(placed(&ran, SH).is_empty() && placed(&ran, GONE).is_empty());
    now += STEP_MAX;
    assert!(s.tick(now, &feed, || JOB, || false), "the next due sample");
    assert_eq!(placed(&ran, JOB), [During::Job(Some(1))]);
}

/// The schedule: nothing is due before a batch; a batch makes the sampler due [`LOOK`] later;
/// while a block executes it samples on a step that starts at [`LOOK`] and doubles to
/// [`STEP_MAX`], a tick before it is due reading nothing; a look after a batch in the same block
/// reads nothing until the step is due, and one that finds a new block samples at once and
/// starts its step again; once the parse stage has parsed every byte with no block executing,
/// nothing is due until the next batch. NEGATIVE CONTROLS (scratch): a step that never doubles
/// fails the steps; a sampler that never stands down fails "nothing due".
#[test]
fn the_sampler_samples_on_a_doubling_step_while_a_block_runs_and_stands_down_after() {
    const SH: i32 = 74_000;
    const JOB: i32 = 74_001;
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let mut samples = 0usize;
    let mut tick = |s: &mut FgSampler, at: Instant| {
        let _ = s.tick(
            at,
            &feed,
            || {
                samples += 1;
                JOB
            },
            || false,
        );
        samples
    };
    let t0 = Instant::now();
    assert_eq!(s.wait_ms(t0), -1, "nothing due before a batch");
    parse(&ran, 10, Some(1), 1_000);
    s.handed(t0);
    assert_eq!(s.wait_ms(t0), 2);
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut at = Vec::new();
    for n in 1..=3_000 {
        let before = tick(&mut s, ms(n));
        let after = tick(&mut s, ms(n));
        assert_eq!(before, after, "a second tick at once is not due");
        if at.len() < before {
            at.push(n);
        }
    }
    assert_eq!(
        at,
        [2, 4, 8, 16, 32, 64, 128, 256, 512, 1_024, 2_024],
        "a doubling step from {LOOK:?} to {STEP_MAX:?}"
    );
    assert_eq!(s.wait_ms(ms(3_000)), 24, "due one step after the last");
    s.handed(ms(3_000));
    assert_eq!(
        tick(&mut s, ms(3_002)),
        11,
        "a look in the same block reads nothing"
    );
    parse(&ran, 10, Some(2), 3_003);
    s.handed(ms(3_003));
    assert_eq!(tick(&mut s, ms(3_005)), 12, "a new block samples at once");
    assert_eq!(s.wait_ms(ms(3_005)), 2, "and starts its step again");
    parse(&ran, 10, None, 3_006);
    let _ = tick(&mut s, ms(3_007));
    assert_eq!(s.wait_ms(ms(3_007)), -1, "nothing due once no block runs");
    assert_eq!(tick(&mut s, ms(9_000)), 12);
}

/// H3's landing (a look the parse stage consumed): a batch holding one block's D and the next
/// block's C (typed ahead, pasted) is handed on while the sampler is far into a long block's
/// step, and the look after it finds the parse stage still behind, the shell in front; nothing
/// wakes the gather when the parse stage catches up, so the sampler looks again on its own, and
/// the new block's silent job is sampled within a few milliseconds instead of at the old block's
/// next step a second later. NEGATIVE CONTROL (scratch): no look again while behind leaves the
/// job unheard through its 500 ms.
#[test]
fn a_block_begun_while_the_parse_stage_lags_is_sampled_once_it_catches_up() {
    const SH: i32 = 76_000;
    const LONG: i32 = 76_001;
    const SHORT: i32 = 76_002;
    set_sight(LONG, 1, "/usr/bin/qzlong");
    set_sight(SHORT, 2, "/usr/bin/qzshort");
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    parse(&ran, 10, Some(1), 1_000);
    s.handed(t0);
    for n in 1..=2_027 {
        let _ = s.tick(ms(n), &feed, || LONG, || false);
    }
    assert_eq!(
        s.wait_ms(ms(2_027)),
        997,
        "PRECONDITION: block 1's next step is a second off"
    );
    let end = ran.handed_on(20);
    s.handed(ms(2_028));
    for n in 2_029..=2_030 {
        assert!(!s.tick(ms(n), &feed, || SH, || false), "the shell in front");
    }
    parsed(&ran, end, 20, Some(2), 3_031);
    let heard_at = (2_031..=2_531).find(|n| s.tick(ms(*n), &feed, || SHORT, || false));
    assert!(
        heard_at.is_some_and(|n| n <= 2_034),
        "block 2's job is sampled within a few ms of the parse catching up: {heard_at:?}"
    );
    assert_eq!(placed(&ran, SHORT), [During::Job(Some(2))]);
}

/// H3's landing: a look while the parse stage is behind, its step not due, samples a group that
/// is not the one it last read (a block may have begun in the bytes still to parse, and its job
/// may end before the parse stage gets there), placed where the gather has handed on to; a look
/// that finds the group it last read reads no leader. NEGATIVE CONTROL (scratch): a look that
/// samples only on a new block or a due step leaves the job unheard before the parse.
#[test]
fn a_look_while_the_parse_stage_is_behind_samples_a_new_group_at_once() {
    const SH: i32 = 78_000;
    const LONG: i32 = 78_001;
    const SHORT: i32 = 78_002;
    set_sight(LONG, 1, "/usr/bin/qzlong");
    set_sight(SHORT, 2, "/usr/bin/qzshort");
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    parse(&ran, 10, Some(1), 1_000);
    s.handed(t0);
    for n in 1..=100 {
        let _ = s.tick(ms(n), &feed, || LONG, || false);
    }
    let reads = sight_reads(LONG);
    let first = ran.handed_on(5);
    s.handed(ms(101));
    assert!(!s.tick(ms(103), &feed, || LONG, || false));
    assert_eq!(
        sight_reads(LONG),
        reads,
        "the group it last read: no leader read"
    );
    let end = ran.handed_on(20);
    s.handed(ms(104));
    assert!(
        s.tick(ms(106), &feed, || SHORT, || false),
        "a new group in front while the parse stage is behind"
    );
    parsed(&ran, first, 5, Some(1), 1_105);
    parsed(&ran, end, 20, Some(2), 1_106);
    assert_eq!(placed(&ran, SHORT), [During::Job(Some(2))]);
}

/// H3's landing (findings 6 and 7): a sample taken while the parse stage has not yet parsed the
/// batch that holds a block's C, its job already in front, is heard placed where the gather
/// handed on to, so in that block once the parse stage parses it; the next sample, which finds
/// that block, does not hear the same image again. NEGATIVE CONTROLS (scratch): a sampler that
/// ignores bytes still to parse stands down at the look and fails "sampled while the parse
/// stage is behind"; one that forgets what it heard at each new block hears it twice.
#[test]
fn a_sample_taken_before_the_parse_stage_finds_its_block_is_heard_once_in_that_block() {
    const SH: i32 = 77_000;
    const JOB: i32 = 77_001;
    set_sight(JOB, 4, "/usr/bin/qzsilent");
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    parse(&ran, 30, None, 1_000);
    s.handed(t0);
    assert!(!s.tick(ms(2), &feed, || SH, || false));
    assert_eq!(
        s.wait_ms(ms(2)),
        -1,
        "PRECONDITION: stood down at the prompt"
    );
    let end = ran.handed_on(12);
    s.handed(ms(10));
    assert!(
        s.tick(ms(12), &feed, || JOB, || false),
        "sampled while the parse stage is behind"
    );
    parsed(&ran, end, 12, Some(3), 1_013);
    for n in [14, 16, 20, 1_100] {
        let _ = s.tick(ms(n), &feed, || JOB, || false);
    }
    assert!(sight_reads(JOB) >= 3, "later samples read the leader");
    assert_eq!(
        placed(&ran, JOB),
        [During::Job(Some(3))],
        "heard once, in the block the parse found"
    );
}

/// H3's landing (finding 5, the law): a resumed job exec'd nothing in the block that resumed it,
/// so no sample of it counts there. A job sampled in its own block, stopped, and resumed by a job
/// word typed ahead (`%1`, `%vim`, no quiet prompt between) is not heard again; a background job
/// (`sleep 100 &`) that started before the last quiet prompt and is brought to the front (`%2`)
/// is not heard, though the sampler never saw it before; a block's own job, whose start record
/// may read up to a tick before that prompt, is. So `%1`, `%vim` and `%2` read nothing, `fg`
/// reads the builtin, and the own job stands in for its word. NEGATIVE CONTROLS (scratch): a
/// sampler that forgets what it heard at each new block hears vim in block 8; one with no quiet
/// bound hears sleep in block 9.
#[test]
fn a_job_resumed_or_brought_to_the_front_is_heard_for_no_later_block() {
    const SH: i32 = 75_000;
    const VIM: i32 = 75_001;
    const BG: i32 = 75_002;
    const OWN: i32 = 75_003;
    const QUIET: u64 = 9_000_000;
    const TICK: u64 = 10_000;
    set_tick(TICK);
    set_sight(VIM, 1_000_000, "/usr/bin/vim");
    set_sight(BG, 2_000_000, "/bin/sleep");
    set_sight(OWN, QUIET - TICK / 2, "/usr/bin/qzown");
    let ran = Arc::new(Ran::with_files(
        Box::new(
            Table::default()
                .program("/usr/bin/vim", "vim")
                .program("/bin/sleep", "sleep")
                .program("/usr/bin/qzown", "qzown"),
        ),
        None,
    ));
    ran.spawned_with(Some("/usr/bin:/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let run = |s: &mut FgSampler, from: u64, fg: i32| {
        for n in from..from + 40 {
            let _ = s.tick(ms(n), &feed, || fg, || false);
        }
    };
    parse(&ran, 10, Some(7), 1_000);
    s.handed(ms(0));
    run(&mut s, 1, VIM);
    assert_eq!(
        placed(&ran, VIM),
        [During::Job(Some(7))],
        "block 7's own job"
    );
    parse(&ran, 10, Some(8), 1_100);
    s.handed(ms(100));
    run(&mut s, 101, VIM);
    assert_eq!(
        placed(&ran, VIM),
        [During::Job(Some(7))],
        "nothing more in block 8"
    );
    set_clock(QUIET);
    parse(&ran, 10, None, 1_200);
    s.handed(ms(200));
    run(&mut s, 201, SH);
    parse(&ran, 10, Some(9), 1_300);
    s.handed(ms(300));
    run(&mut s, 301, BG);
    assert!(
        placed(&ran, BG).is_empty(),
        "a background job brought to the front"
    );
    parse(&ran, 10, None, 1_400);
    s.handed(ms(400));
    run(&mut s, 401, SH);
    parse(&ran, 10, Some(10), 1_500);
    s.handed(ms(500));
    run(&mut s, 501, OWN);
    assert_eq!(
        placed(&ran, OWN),
        [During::Job(Some(10))],
        "block 10's own job"
    );
    let reading = |block: u64, from: u64, line: &str| {
        let heard = ran.heard(block, Window::of(Some(from), None, None, None), None);
        crate::session_status::command_reading(line, &heard).map(|r| (r.text, r.own))
    };
    for line in ["%1", "%vim"] {
        assert_eq!(reading(8, 1_100, line), None, "{line} in block 8");
    }
    assert_eq!(reading(8, 1_100, "fg").map(|r| r.0), Some("fg".to_owned()));
    assert_eq!(reading(9, 1_300, "%2"), None, "%2 in block 9");
    assert_eq!(
        reading(10, 1_500, "qzalias"),
        Some(("qzown".to_owned(), false)),
        "the own job stands in"
    );
}

/// H3's landing, as a live no-watch session showed it: a job the last command left in the
/// background (`sleep 3 &`) forked a few milliseconds before the prompt after it, so the
/// stand-down after that prompt reads the start clock only just after the job's start; brought
/// to the front by `%1` in the next block, the job exec'd nothing there and is heard as nothing.
/// macOS reads a start to the microsecond, so that reading already follows it and nothing more is
/// due until the next batch; Linux may read a start up to a tick early, so that stand-down looks
/// once more a tick and a [`LOOK`] later, whose reading surely follows it. A job of the next
/// block's own, its C typed ahead before that look and its start read up to a tick early, is
/// heard. NEGATIVE CONTROLS (scratch): the former fixed 20 ms slack in place of the start
/// record's tick fails "macOS"; no settle look fails "Linux"; a settle look armed at every
/// stand-down fails "one settle look".
#[test]
fn a_job_left_in_the_background_just_before_the_prompt_is_heard_for_no_later_block() {
    const SH: i32 = 79_000;
    const BG: i32 = 79_001;
    const OWN: i32 = 79_002;
    // The background job's real start on the start clock.
    const FORK: u64 = 5_000_000;
    for (case, tick, early, bg, own) in [
        ("macOS", 1, 0, BG, OWN),
        ("Linux", 10_000, 4_000, BG + 10, OWN + 10),
    ] {
        set_tick(tick);
        set_sight(bg, FORK - early, "/bin/sleep");
        let ran = Arc::new(Ran::with_files(
            Box::new(
                Table::default()
                    .program("/bin/sleep", "sleep")
                    .program("/usr/bin/qzown", "qzown"),
            ),
            None,
        ));
        ran.spawned_with(Some("/usr/bin:/bin"));
        let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
        let mut s = FgSampler::new(SEAM);
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        parse(&ran, 10, Some(11), 1_000);
        s.handed(ms(0));
        set_clock(FORK - 1_000);
        let _ = s.tick(ms(2), &feed, || SH, || false);
        parse(&ran, 10, None, 1_003);
        s.handed(ms(3));
        set_clock(FORK + 3_000);
        let _ = s.tick(ms(5), &feed, || SH, || false);
        let after_prompt = s.wait_ms(ms(5));
        set_clock(FORK + 15_000);
        for n in 6..=40 {
            let _ = s.tick(ms(n), &feed, || SH, || false);
        }
        let after_settle = s.wait_ms(ms(40));
        parse(&ran, 10, Some(12), 1_100);
        s.handed(ms(100));
        for n in 101..140 {
            let _ = s.tick(ms(n), &feed, || bg, || false);
        }
        assert!(
            placed(&ran, bg).is_empty(),
            "{case}: the job left in the background, brought to the front"
        );
        let settle = if tick > 1 { 12 } else { -1 };
        assert_eq!(after_prompt, settle, "{case}: what is due after the prompt");
        assert_eq!(after_settle, -1, "{case}: one settle look");
        parse(&ran, 10, None, 1_200);
        s.handed(ms(200));
        set_clock(FORK + 300_000);
        let _ = s.tick(ms(202), &feed, || SH, || false);
        set_sight(own, FORK + 301_000 - early, "/usr/bin/qzown");
        parse(&ran, 10, Some(13), 1_203);
        s.handed(ms(203));
        for n in 204..240 {
            let _ = s.tick(ms(n), &feed, || own, || false);
        }
        assert_eq!(
            placed(&ran, own),
            [During::Job(Some(13))],
            "{case}: the next block's own job"
        );
    }
}

/// Run `f` with the pid of a child forked and parked before any exec (it still maps this test
/// binary's image, as a shell's child between its fork and its exec maps the shell's), then let
/// the child exit and reap it.
fn with_parked_fork<T>(f: impl FnOnce(i32) -> T) -> T {
    use std::os::fd::AsRawFd as _;
    let (go_r, go_w) = std::io::pipe().expect("pipe");
    let (go_fd, go_w_fd) = (go_r.as_raw_fd(), go_w.as_raw_fd());
    // Held across the fork alone, so the child copies no neighbour's descriptor between its
    // creation and its close-on-fork mark (`aterm_uds::kept_out`).
    let spawning = aterm_uds::spawn_lock::spawning();
    // SAFETY: the child calls only async-signal-safe functions (`close`, `read`, `_exit`) on
    // descriptors opened before `fork`, then exits without unwinding.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid == 0 {
        let mut b = 0u8;
        // SAFETY: as above.
        unsafe {
            libc::close(go_w_fd);
            loop {
                let n = libc::read(go_fd, (&raw mut b).cast(), 1);
                if n >= 0
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
            libc::_exit(0);
        }
    }
    drop(spawning);
    drop(go_r);
    let out = f(pid);
    drop(go_w);
    let mut status = 0;
    // SAFETY: `pid` is this test's own child, reaped once here.
    let _ = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    out
}

/// The shipping reader reads a leader only once it has exec'd since its fork: a forked child
/// parked before any exec still maps this test binary's image, as a shell's child between its
/// fork and its exec (or one whose command was not found) maps the shell's, and reads nothing;
/// a spawned child reads its program. GATE3-2's live no-watch zsh read `zsh` for `/usr/bin/true`,
/// `env X=1 ls` and a missing command before the gate. NEGATIVE CONTROL (scratch): no exec check
/// in `read_sight` reads the parked fork as this test binary.
#[test]
fn the_shipping_reader_reads_nothing_of_a_fork_that_has_not_execd() {
    let mut spawned = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let spawned_pid = i32::try_from(spawned.id()).unwrap();
    let spawned_sight = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || super::read_sight(spawned_pid),
    );
    let _ = spawned.kill();
    let _ = spawned.wait();
    let exe = spawned_sight.expect("a spawned child's program").exe;
    assert_eq!(exe.file_name().and_then(|n| n.to_str()), Some("sleep"));
    let forked = with_parked_fork(super::read_sight);
    assert_eq!(forked, None, "a fork that has not exec'd reads nothing");
}

/// H3's landing (the edge images' exec check), THE SHIPPING READERS: a sampled edge to a leader
/// parked between its fork and any exec reads its image, this test binary's, and the ran log
/// hears nothing of it, as `read_sight` reads nothing of it: `sampled_edge_image` asks
/// for birth-bracketed exec evidence. NEGATIVE CONTROL (scratch): the edge gated by a flag that is
/// always set hears the fork's image.
#[test]
fn a_sampled_edge_hears_nothing_of_a_leader_that_has_not_execd() {
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::new(Arc::clone(&ran), 0);
    let (image, heard) = with_parked_fork(|pid| {
        let image = crate::spawn::sampled_edge_image(
            crate::holder_image::read,
            super::execd_start,
            &feed,
            pid,
            0,
        );
        (image, ran.within(Some(Window { from: 0, to: None })).len())
    });
    assert!(
        image.is_some_and(|i| i.exe.file_name().is_some()),
        "the edge reads the fork's image"
    );
    assert_eq!(heard, 0, "a leader that has not exec'd is heard as nothing");
}

/// An `exec` the proc connector routed (unit L2): `pid` of group `pgid`, started at `start_us`
/// and exec'd at `at_us` on the start clock, mapping `path`, whose file is the test identity of
/// its file name ([`test_id`]).
fn routed(
    pid: i32,
    pgid: i32,
    start_us: u64,
    at_us: u64,
    path: &str,
) -> aterm_uds::proc_connector::Exec {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let id = test_id(name);
    aterm_uds::proc_connector::Exec {
        pid,
        pgid,
        tpgid: pgid,
        start_ns: start_us * 1_000,
        at_ns: at_us * 1_000,
        image: aterm_uds::proc_connector::Image {
            path: path.into(),
            file: Some((id.dev, id.ino)),
        },
        env_path: None,
    }
}

/// The reading of `line` in block `block` opened at wall ms `from`: its text and whether its
/// own program ran.
fn reading_of(ran: &Ran, block: u64, from: u64, line: &str) -> Option<(String, bool)> {
    let heard = ran.heard(block, Window::of(Some(from), None, None, None), None);
    crate::session_status::command_reading(line, &heard).map(|r| (r.text, r.own))
}

/// A program too short for any sample (`true`, a millisecond): the proc connector's `exec`,
/// handed over between batches while its group holds the front and the master is quiet, is
/// placed where a sample would be, in its block as its job's, so it stands in and confirms; the
/// same `exec` handed again, and a later sample of the same image, add nothing (heard once); a
/// child's `exec` in the job's group is placed too, and never pushes the leader out of what the
/// sampler remembers; the shell's own group, and pid 1, are never heard. NEGATIVE CONTROLS
/// (scratch): no remembered connector `exec` (`L2_NC=remember`) fails "a sample adds nothing";
/// no remembered-image check in `connected` (`L2_NC=dedupe`) fails "handed again"; every
/// connector `exec` held by its stamp (`L2_NC=stamp`) fails "in its block".
#[test]
fn a_connector_exec_too_short_for_a_sample_is_placed_in_its_block_once() {
    const SH: i32 = 81_000;
    const JOB: i32 = 81_001;
    const CHILD: i32 = 81_002;
    let ran = Arc::new(Ran::with_files(
        Box::new(
            Table::default()
                .program("/usr/bin/true", "true")
                .program("/usr/bin/cc", "cc"),
        ),
        None,
    ));
    ran.spawned_with(Some("/usr/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    parse(&ran, 40, Some(3), 1_000);
    s.handed(t0);
    let exec = routed(JOB, JOB, 500, 501, "/usr/bin/true");
    assert!(s.connected(&exec, &feed, || JOB, || false));
    assert_eq!(placed(&ran, JOB), [During::Job(Some(3))], "in its block");
    assert_eq!(
        reading_of(&ran, 3, 900, "qzalias"),
        Some(("true".to_owned(), false)),
        "stands in"
    );
    assert_eq!(
        reading_of(&ran, 3, 900, "true"),
        Some(("true".to_owned(), true)),
        "confirms"
    );
    assert!(
        !s.connected(&exec, &feed, || JOB, || false),
        "handed again: heard once"
    );
    let children = i32::try_from(super::SEEN_MAX).expect("a few") + 4;
    for n in 0..children {
        assert!(s.connected(
            &routed(CHILD + n, JOB, 600, 601, "/usr/bin/cc"),
            &feed,
            || JOB,
            || false
        ));
    }
    set_sight(JOB, 500, "/usr/bin/true");
    let mut now = t0 + LOOK;
    for _ in 0..3 {
        assert!(
            !s.tick(now, &feed, || JOB, || false),
            "a sample adds nothing"
        );
        now += STEP_MAX;
    }
    let of_pid = |pid: i32| {
        ran.within(Some(Window { from: 0, to: None }))
            .iter()
            .filter(|e| e.pid == pid)
            .count()
    };
    assert_eq!(of_pid(JOB), 1, "the leader's image, once");
    assert_eq!(of_pid(CHILD), 1, "a child's");
    assert_eq!(
        placed(&ran, JOB).len(),
        1 + usize::try_from(children).expect("a few")
    );
    assert!(!s.connected(&routed(SH, SH, 1, 2, "/bin/sh"), &feed, || SH, || false));
    assert!(!s.connected(&routed(1, 1, 1, 2, "/sbin/init"), &feed, || 1, || false));
    assert!(!s.connected(
        &routed(81_100, SH, 1, 2, "/usr/bin/cc"),
        &feed,
        || SH,
        || false
    ));
    assert!(
        placed(&ran, SH).is_empty(),
        "the shell's group is never heard"
    );
}

/// Where a sample would not place it, a connector `exec` only confirms: handed over once the
/// terminal is back with the shell, or with output still unread on the master, it is held by its
/// stamp, confirming the word whose file it is but standing in for none; a later sample that
/// finds the same image in front still places it, once. NEGATIVE CONTROLS (scratch): no
/// foreground check (`L2_NC=front`) fails "the terminal back with the shell"; no unread check
/// (`L2_NC=unread`) fails "output unread"; a sampler that counts a stamp-only `exec` as heard
/// (`L2_NC=exactonly`) fails "a later sample places it".
#[test]
fn a_connector_exec_a_sample_would_not_place_only_confirms() {
    const SH: i32 = 82_000;
    const GONE: i32 = 82_001;
    const LOUD: i32 = 82_002;
    let ran = Arc::new(Ran::with_files(
        Box::new(
            Table::default()
                .program("/usr/bin/true", "true")
                .program("/usr/bin/qzloud", "qzloud"),
        ),
        None,
    ));
    ran.spawned_with(Some("/usr/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    parse(&ran, 40, Some(4), 1_000);
    s.handed(t0);
    assert!(s.connected(
        &routed(GONE, GONE, 700, 701, "/usr/bin/true"),
        &feed,
        || SH,
        || false
    ));
    assert_eq!(
        placed(&ran, GONE),
        [During::Window],
        "the terminal back with the shell"
    );
    assert!(s.connected(
        &routed(LOUD, LOUD, 800, 801, "/usr/bin/qzloud"),
        &feed,
        || LOUD,
        || true
    ));
    assert_eq!(placed(&ran, LOUD), [During::Window], "output unread");
    assert_eq!(
        reading_of(&ran, 4, 900, "true"),
        Some(("true".to_owned(), true)),
        "confirms by its stamp"
    );
    assert_eq!(
        reading_of(&ran, 4, 900, "qzalias"),
        None,
        "stands in for nothing"
    );
    set_sight(LOUD, 800, "/usr/bin/qzloud");
    assert!(
        s.tick(t0 + LOOK, &feed, || LOUD, || false),
        "a later sample places it"
    );
    assert_eq!(placed(&ran, LOUD), [During::Window, During::Job(Some(4))]);
    assert!(
        !s.tick(t0 + LOOK + STEP_MAX, &feed, || LOUD, || false),
        "once"
    );
}

/// The connector stands in only for what a sample could: an `exec` from before the last quiet
/// prompt's reading (a job left in the background) is no later block's, and after the listener
/// lost messages the block then running is the sampler's alone, its later `exec`s only
/// confirming, until a new block runs. NEGATIVE CONTROLS (scratch): no quiet check
/// (`L2_NC=quiet`) fails "before the quiet prompt"; a lost flag never set (`L2_NC=lostset`)
/// fails "lost"; one never lifted (`L2_NC=lostlift`) fails "the next block".
#[test]
fn a_connector_exec_before_the_quiet_prompt_or_after_a_loss_is_held_by_its_stamp() {
    const SH: i32 = 83_000;
    const OLD: i32 = 83_001;
    const NEW: i32 = 83_002;
    const AFTER: i32 = 83_003;
    const NEXT: i32 = 83_004;
    const QUIET: u64 = 5_000_000;
    let ran = Arc::new(Ran::with_files(
        Box::new(Table::default().program("/usr/bin/true", "true")),
        None,
    ));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    set_clock(QUIET);
    parse(&ran, 10, None, 1_000);
    s.handed(ms(0));
    let _ = s.tick(ms(10), &feed, || SH, || false);
    parse(&ran, 10, Some(5), 1_100);
    s.handed(ms(100));
    let true_at = |pid: i32, at: u64| routed(pid, pid, at - 1, at, "/usr/bin/true");
    assert!(s.connected(&true_at(OLD, QUIET - 1), &feed, || OLD, || false));
    assert_eq!(
        placed(&ran, OLD),
        [During::Window],
        "before the quiet prompt"
    );
    assert!(s.connected(&true_at(NEW, QUIET + 1), &feed, || NEW, || false));
    assert_eq!(placed(&ran, NEW), [During::Job(Some(5))]);
    s.connector_lost(&feed, || false);
    assert!(s.connected(&true_at(AFTER, QUIET + 2), &feed, || AFTER, || false));
    assert_eq!(placed(&ran, AFTER), [During::Window], "lost");
    parse(&ran, 10, Some(6), 1_200);
    s.handed(ms(200));
    assert!(s.connected(&true_at(NEXT, QUIET + 3), &feed, || NEXT, || false));
    assert_eq!(placed(&ran, NEXT), [During::Job(Some(6))], "the next block");
}

/// A connector `exec` is heard only when its group held the terminal's foreground, when the
/// listener read it (`tpgid`, just after the `exec`) or when the gather does: a background job's,
/// read with another group in front and still behind it, confirms nothing, as macOS's watch
/// hears none; one the listener read before its group took the terminal, in front now, is
/// placed in its block. NEGATIVE CONTROL (scratch, `L2_NC=bg`): no foreground check fails "a
/// background job's".
#[test]
fn a_background_jobs_connector_exec_confirms_nothing() {
    const SH: i32 = 84_000;
    const FG: i32 = 84_001;
    const BG: i32 = 84_002;
    const RACED: i32 = 84_003;
    let ran = Arc::new(Ran::with_files(
        Box::new(
            Table::default()
                .program("/usr/bin/qzbg", "qzbg")
                .program("/usr/bin/qzraced", "qzraced"),
        ),
        None,
    ));
    ran.spawned_with(Some("/usr/bin"));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    parse(&ran, 40, Some(4), 1_000);
    s.handed(Instant::now());
    let background = aterm_uds::proc_connector::Exec {
        tpgid: FG,
        ..routed(BG, BG, 700, 701, "/usr/bin/qzbg")
    };
    assert!(
        !s.connected(&background, &feed, || FG, || false),
        "a background job's"
    );
    assert!(placed(&ran, BG).is_empty(), "nothing heard");
    assert_eq!(
        reading_of(&ran, 4, 900, "qzbg"),
        None,
        "it confirms nothing"
    );
    let raced = aterm_uds::proc_connector::Exec {
        tpgid: SH,
        ..routed(RACED, RACED, 800, 801, "/usr/bin/qzraced")
    };
    assert!(s.connected(&raced, &feed, || RACED, || false));
    assert_eq!(
        placed(&ran, RACED),
        [During::Job(Some(4))],
        "in front now: in its block"
    );
}

/// A loss the connector reports while bytes still wait to be parsed, or to be read off the
/// master, may fall in a block not known yet: every block runs lost, its `exec`s held by their
/// stamps, until a quiet stand-down, after which a block's stand in again; a stand-down with
/// output unread is not quiet and lifts nothing. NEGATIVE CONTROLS (scratch): the loss charged
/// to the block parsed so far (`L2_NC=lostbehind`) fails "behind at the loss"; the master's
/// unread bytes ignored (`L2_NC=lostunread`) fails "unread at the loss"; a quiet stand-down
/// that lifts nothing (`L2_NC=lostquiet`) fails "after a quiet stand-down".
#[test]
fn a_loss_while_bytes_wait_lasts_until_a_quiet_stand_down() {
    const SH: i32 = 85_000;
    const JOB: i32 = 85_001;
    const CHILD: i32 = 85_002;
    const NEXT: i32 = 85_003;
    const LATER: i32 = 85_004;
    const LAST: i32 = 85_005;
    const UNREAD: i32 = 85_006;
    const QUIET: u64 = 6_000_000;
    let ran = Arc::new(Ran::with_files(
        Box::new(Table::default().program("/usr/bin/true", "true")),
        None,
    ));
    let feed = RanFeed::reading(Arc::clone(&ran), SH, scripted_names);
    let mut s = FgSampler::new(SEAM);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let true_at = |pid: i32, pgid: i32, at: u64| routed(pid, pgid, at - 1, at, "/usr/bin/true");
    set_clock(QUIET);
    parse(&ran, 10, None, 1_000);
    s.handed(ms(0));
    let _ = s.tick(ms(10), &feed, || SH, || false);
    // Block 7's C is handed on, and not parsed yet, when the loss is drained.
    let end = ran.handed_on(10);
    s.connector_lost(&feed, || false);
    parsed(&ran, end, 10, Some(7), 1_100);
    s.handed(ms(100));
    assert!(s.connected(&true_at(CHILD, JOB, QUIET + 1), &feed, || JOB, || false));
    assert_eq!(
        placed(&ran, JOB),
        [During::Window],
        "behind at the loss: block 7 may hold the lost exec"
    );
    parse(&ran, 10, Some(8), 1_200);
    s.handed(ms(200));
    assert!(s.connected(&true_at(NEXT, NEXT, QUIET + 2), &feed, || NEXT, || false));
    assert_eq!(
        placed(&ran, NEXT),
        [During::Window],
        "no quiet stand-down yet"
    );
    parse(&ran, 10, None, 1_300);
    s.handed(ms(300));
    let _ = s.tick(ms(310), &feed, || SH, || true);
    parse(&ran, 10, Some(9), 1_400);
    s.handed(ms(400));
    assert!(s.connected(&true_at(LATER, LATER, QUIET + 3), &feed, || LATER, || false));
    assert_eq!(
        placed(&ran, LATER),
        [During::Window],
        "a stand-down with output unread lifts nothing"
    );
    set_clock(QUIET + 10);
    parse(&ran, 10, None, 1_500);
    s.handed(ms(500));
    let _ = s.tick(ms(510), &feed, || SH, || false);
    parse(&ran, 10, Some(10), 1_600);
    s.handed(ms(600));
    assert!(s.connected(&true_at(LAST, LAST, QUIET + 11), &feed, || LAST, || false));
    assert_eq!(
        placed(&ran, LAST),
        [During::Job(Some(10))],
        "after a quiet stand-down"
    );
    // Every byte parsed, but some unread on the master at the loss: a block not known yet.
    s.connector_lost(&feed, || true);
    parse(&ran, 10, Some(11), 1_700);
    s.handed(ms(700));
    assert!(s.connected(
        &true_at(UNREAD, UNREAD, QUIET + 12),
        &feed,
        || UNREAD,
        || false
    ));
    assert_eq!(
        placed(&ran, UNREAD),
        [During::Window],
        "unread at the loss: lost until a quiet stand-down"
    );
}
