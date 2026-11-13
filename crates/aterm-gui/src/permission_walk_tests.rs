// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn facts() -> StartFacts {
    StartFacts {
        cold: true,
        typing: false,
        in_front: true,
    }
}

/// Automatic acquisition starts without opening System State or a recent-input requirement,
/// once the keyboard rests in an aterm window the person is looking at.
#[test]
fn the_walk_starts_automatically_after_the_keyboard_rests_with_aterm_in_front() {
    assert!(starts(facts()));
    assert!(
        !starts(StartFacts {
            typing: true,
            ..facts()
        }),
        "a key typed 1 s ago waits for the keyboard to rest 2 s"
    );
    assert!(
        !starts(StartFacts {
            cold: false,
            ..facts()
        }),
        "an update's successor, which the owner did not start, never (owner ruling 2026-11-09)"
    );
    assert!(
        !starts(StartFacts {
            in_front: false,
            ..facts()
        }),
        "behind another app System Settings would come up over it: the walk waits"
    );
}

/// An opening found for a key read before the bundle id or the Mac holds the walk for good
/// only when it names neither beyond what the key carries: one that names a Mac the key
/// lacks may be another Mac's.
#[test]
fn only_an_opening_that_names_no_more_than_the_key_holds_the_walk_for_good() {
    for (word, key, want) in [
        ("r1@com.aterm.aterm@A", "r1@com.aterm.aterm@A", true),
        ("r1", "r1@com.aterm.aterm@A", true),
        ("0.146.0", "r1@com.aterm.aterm", true),
        ("r1@com.aterm.aterm", "r1@com.aterm.aterm", true),
        ("r1@com.aterm.aterm@A", "r1@com.aterm.aterm", false),
        ("r1@com.aterm.aterm", "r1", false),
    ] {
        assert_eq!(found_for_good(word, key), want, "{word} for {key}");
    }
}

/// The named subjects lead in their order, the rest follow in the walk's own, each once and
/// only while missing.
#[test]
fn named_subjects_lead_and_the_walk_continues_with_the_rest() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    assert_eq!(
        subjects(&[], |_| true),
        vec![FullDiskAccess, DeveloperTools, Accessibility]
    );
    assert_eq!(
        subjects(&[DeveloperTools], |subject| subject != Accessibility),
        vec![DeveloperTools, FullDiskAccess],
        "doctor's Developer Tools hand-over: Full Disk Access right after it"
    );
    assert_eq!(
        subjects(&[Accessibility, Accessibility], |subject| subject
            == Accessibility),
        vec![Accessibility]
    );
    assert!(subjects(&[FullDiskAccess], |_| false).is_empty());
}

/// The walk counts the pages the person sees: a page that came up counts, and so does each
/// later one whose grant does not read held; a page skipped without coming up does not.
#[test]
fn a_walk_counts_its_pages_and_ends_after_the_last() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    let now = Instant::now();
    let none_held = |_| false;
    let mut walk = Walk::new(vec![FullDiskAccess, Accessibility], now).expect("two pages");
    assert_eq!(walk.position(none_held), (1, 2));
    assert_eq!(walk.next(), Some(Accessibility));
    walk.opened();
    assert_eq!(walk.step(), Step::Up);
    assert_eq!(walk.advance(now), Some(Accessibility));
    assert_eq!((walk.position(none_held), walk.step()), ((2, 2), Step::Due));
    assert_eq!(walk.next(), None);
    assert_eq!(walk.advance(now), None);
    assert_eq!(walk.advance(now), None, "stays over");
    assert!(Walk::new(Vec::new(), now).is_none());

    let mut walk =
        Walk::new(vec![FullDiskAccess, DeveloperTools, Accessibility], now).expect("three pages");
    assert_eq!(
        walk.position(|subject| subject == DeveloperTools),
        (1, 2),
        "a later page whose grant reads held is not counted"
    );
    assert_eq!(walk.advance(now), Some(DeveloperTools), "skipped, never up");
    assert_eq!(walk.position(none_held), (1, 2), "nor is a page skipped");
}

#[test]
fn each_subject_has_its_word_and_the_rows_name() {
    for subject in Subject::ORDER {
        assert_eq!(Subject::parse(subject.word()), Some(subject));
    }
    assert_eq!(
        Subject::parse("full-disk-access"),
        Some(Subject::FullDiskAccess)
    );
    assert_eq!(
        Subject::parse("developer-tools"),
        Some(Subject::DeveloperTools)
    );
    assert_eq!(Subject::parse("camera"), None);
}

/// A day, in seconds: [`WALK_AGAIN`]'s first pause.
const DAY: u64 = 86_400;

/// The unix second the tests' clock starts at.
const T0: u64 = 1_800_000_000;

thread_local! {
    /// What the walk record holds as the lane's reach keeps it: `<state>`, which outlives
    /// each lane.
    static RECORD: std::cell::RefCell<Option<(u64, Vec<KeptPage>)>> =
        const { std::cell::RefCell::new(None) };
    static KEEPS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// The clock the lane's reach reads, in unix seconds; `None` unread.
    static CLOCK: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(Some(T0)) };
}

/// A lane whose walk record is [`RECORD`], each write counted, and whose clock is [`CLOCK`]:
/// a later process when `fresh` is false, else one on a Mac whose record is empty and whose
/// clock reads [`T0`].
fn lane(fresh: bool) -> WalkLane {
    if fresh {
        RECORD.with(|record| *record.borrow_mut() = None);
        CLOCK.with(|clock| clock.set(Some(T0)));
    }
    WalkLane::with_reach(WalkReach {
        keep_pages: |revision, pages| {
            KEEPS.with(|keeps| keeps.set(keeps.get() + 1));
            RECORD.with(|record| {
                *record.borrow_mut() = (!pages.is_empty()).then(|| (revision, pages.to_vec()));
            });
        },
        left_pages: |revision| {
            RECORD.with(|record| {
                record
                    .borrow()
                    .clone()
                    .filter(|(kept, _)| *kept == revision)
                    .map(|(_, pages)| pages)
                    .unwrap_or_default()
            })
        },
        unix_now: || CLOCK.with(std::cell::Cell::get),
        ..WalkReach::INERT
    })
}

fn recording() -> WalkLane {
    lane(true)
}

/// What [`RECORD`] holds: the revision and each page with its tries.
fn held() -> Option<(u64, Vec<(Subject, u32)>)> {
    RECORD.with(|record| {
        record.borrow().clone().map(|(revision, pages)| {
            (
                revision,
                pages
                    .iter()
                    .map(|page| (page.subject, page.tries))
                    .collect(),
            )
        })
    })
}

fn keeps() -> u32 {
    KEEPS.with(std::cell::Cell::get)
}

fn at(now: u64) {
    CLOCK.with(|clock| clock.set(Some(now)));
}

/// `subjects` as pages no walk left unanswered.
fn fresh(subjects: &[Subject]) -> Vec<KeptPage> {
    subjects.iter().copied().map(KeptPage::new).collect()
}

/// `pages` with no walk left unanswered.
fn none_left(pages: &[Subject]) -> Vec<(Subject, u32)> {
    pages.iter().map(|subject| (*subject, 0)).collect()
}

/// The walk record keeps what the walk under way has still to show, at its start and at each
/// turn, and lets it go only with the person's answer or the walk run out: the close box, a
/// Not now that ends it, the last page passed ([`WalkLane::end`]). A walk that stops
/// unanswered keeps its pages, each counting a walk that left it so, free in this process too
/// ([`WalkLane::stop`]); one that gives way keeps and frees them with none counted
/// ([`WalkLane::give_way`]); a lapse counts only the page that lapsed ([`WalkLane::lapse`]).
/// A walk under no revision keeps nothing, and a walk whose pages did not change writes
/// nothing again.
#[test]
fn the_walk_record_goes_only_with_the_persons_answer() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    let now = Instant::now();
    let every = [FullDiskAccess, DeveloperTools, Accessibility];
    let walk = || Walk::new(every.to_vec(), now).expect("three pages");
    let started = |revision| {
        let mut lane = recording();
        lane.start(walk(), "a test", revision);
        lane
    };

    let mut lane = started(Some(4));
    assert_eq!(held(), Some((4, none_left(&every))), "kept as it starts");
    let written = keeps();
    lane.keep_ahead();
    assert_eq!(keeps(), written, "unchanged: nothing written");
    assert_eq!(lane.advance(now), Some(DeveloperTools));
    assert_eq!(
        held(),
        Some((4, none_left(&[DeveloperTools, Accessibility])))
    );
    assert!(lane.join(&[FullDiskAccess], &[], "a test"));
    assert_eq!(
        held(),
        Some((
            4,
            none_left(&[DeveloperTools, Accessibility, FullDiskAccess])
        ))
    );
    lane.end("the close box");
    assert_eq!(held(), None, "the person's answer");

    let mut lane = started(Some(4));
    for _ in every {
        let _ = lane.advance(now);
    }
    assert_eq!(held(), None, "the last page passed");

    let mut lane = started(Some(4));
    lane.stop("its panel is gone");
    assert_eq!(
        held(),
        Some((
            4,
            every
                .iter()
                .map(|subject| (*subject, 1))
                .collect::<Vec<_>>()
        )),
        "not shown: kept, each counting the walk that left it so"
    );
    assert!(
        every.iter().all(|subject| !lane.walked(*subject)),
        "free here: the record's time decides"
    );
    lane.take_left(4);
    assert_eq!(lane.kept(4).len(), 3, "as this process knows it");
    assert!(lane.kept(5).is_empty(), "under no other revision");

    let mut lane = started(Some(4));
    let _ = lane.advance(now);
    lane.give_way("a press elsewhere");
    assert_eq!(
        held(),
        Some((4, none_left(&[DeveloperTools, Accessibility]))),
        "a press is no answer: nothing counted"
    );
    assert!(lane.walked(FullDiskAccess));
    assert!(
        !lane.walked(DeveloperTools) && !lane.walked(Accessibility),
        "freed here"
    );

    let mut lane = started(Some(4));
    let _ = lane.advance(now);
    lane.lapse();
    assert_eq!(
        held(),
        Some((4, vec![(Accessibility, 0)])),
        "the page the person left goes; the page after it stays, counting nothing"
    );
    assert!(
        lane.walked(DeveloperTools) && !lane.walked(Accessibility),
        "the page that lapsed stays walked here; the page after it is free"
    );

    let written = keeps();
    let mut lane = started(None);
    let _ = lane.advance(now);
    lane.stop("its panel is gone");
    assert_eq!(keeps(), written, "no revision: nothing kept");
    assert!(lane.walked(DeveloperTools), "and nothing freed");
}

/// A page no walk left unanswered is due at once; one a walk left so waits [`WALK_AGAIN`]'s
/// pause from that walk's end, a day, doubling with each walk in a row that left it so, up to
/// the longest; an unread clock holds a page that waits. NEGATIVE CONTROL: a page whose pause
/// passed is due, and its next try waits twice as long.
#[test]
fn a_page_left_unanswered_waits_for_its_pause_and_each_one_in_a_row_doubles_it() {
    let page = KeptPage::new(Subject::FullDiskAccess);
    assert!(
        page.due(Some(T0)) && page.due(None),
        "never left: due at once"
    );
    assert_eq!(page.due_at(), None);
    let once = page.left_unanswered(T0);
    assert_eq!(once.due_at(), Some(T0 + DAY));
    assert!(!once.due(Some(T0 + DAY - 1)), "a day after");
    assert!(once.due(Some(T0 + DAY)));
    assert!(!once.due(None), "an unread clock holds it");
    let twice = once.left_unanswered(T0 + DAY);
    assert_eq!(
        twice.due_at(),
        Some(T0 + 3 * DAY),
        "two days after the second"
    );
    let mut many = page;
    for _ in 0..12 {
        many = many.left_unanswered(T0);
    }
    assert_eq!(
        many.due_at(),
        Some(T0 + WALK_AGAIN.longest().as_secs()),
        "never longer than the longest"
    );
}

/// The tries follow each page from process to process: a walk taken up from the record that
/// stops again counts one more, a page the walk answered leaves with its count, and the next
/// page keeps its own. A start keeps the record's pages outside the walk, due or not (a
/// person's press walks one page while the record holds others), and the person's answer
/// leaves them. The park's deadline is the soonest page that waits. NEGATIVE CONTROL: the
/// walk's own pages go with the answer.
#[test]
fn a_pages_tries_follow_it_from_walk_to_walk() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    let now = Instant::now();
    let both = [FullDiskAccess, DeveloperTools];
    let mut first = recording();
    first.start(
        Walk::new(both.to_vec(), now).expect("pages"),
        "first launch",
        Some(1),
    );
    first.stop("its panel is gone");
    assert_eq!(
        held(),
        Some((1, vec![(FullDiskAccess, 1), (DeveloperTools, 1)]))
    );
    assert_eq!(
        first.next_due(now),
        Some(now + Duration::from_secs(DAY)),
        "the park wakes a day on"
    );

    at(T0 + DAY);
    let mut later = lane(false);
    later.take_left(1);
    assert!(later.kept(1).iter().all(|page| page.due(Some(T0 + DAY))));
    assert_eq!(later.next_due(now), None, "nothing waits");
    later.start(
        Walk::new(both.to_vec(), now).expect("pages"),
        "walks on",
        Some(1),
    );
    assert_eq!(
        held(),
        Some((1, vec![(FullDiskAccess, 1), (DeveloperTools, 1)])),
        "taken up with its tries"
    );
    later.stop("its panel is gone");
    assert_eq!(
        held(),
        Some((1, vec![(FullDiskAccess, 2), (DeveloperTools, 2)]))
    );
    let kept = later.kept(1);
    assert_eq!(kept[0].at, T0 + DAY);
    assert_eq!(kept[0].due_at(), Some(T0 + 3 * DAY));

    at(T0 + 3 * DAY);
    let mut third = lane(false);
    third.start(
        Walk::new(both.to_vec(), now).expect("pages"),
        "walks on",
        Some(1),
    );
    let _ = third.advance(now);
    assert_eq!(
        held(),
        Some((1, vec![(DeveloperTools, 2)])),
        "the answered page leaves; the next keeps its own count"
    );
    third.end("the close box");
    assert_eq!(held(), None);

    let mut waits = recording();
    RECORD.with(|record| {
        *record.borrow_mut() = Some((
            1,
            vec![
                KeptPage::new(FullDiskAccess).left_unanswered(T0),
                KeptPage::new(Accessibility).left_unanswered(T0 - 2 * DAY),
            ],
        ));
    });
    waits.start(
        Walk::new(vec![DeveloperTools], now).expect("a page"),
        "a person's press",
        Some(1),
    );
    assert_eq!(
        held(),
        Some((
            1,
            vec![(DeveloperTools, 0), (FullDiskAccess, 1), (Accessibility, 1)]
        )),
        "the pages outside the walk stay beside it, the one that waits and the due one"
    );
    waits.end("the close box");
    assert_eq!(
        held(),
        Some((1, vec![(FullDiskAccess, 1), (Accessibility, 1)])),
        "the answer leaves the pages outside the walk"
    );
    assert_eq!(waits.next_due(now), Some(now + Duration::from_secs(DAY)));
    CLOCK.with(|clock| clock.set(None));
    assert_eq!(waits.next_due(now), None, "an unread clock");
}

/// A page whose last unanswered walk reads after the clock's now (a clock set back, a walk that
/// stopped while the clock read nothing) is taken as left unanswered now, and that is kept, so
/// its pause counts from a time the clock has seen. NEGATIVE CONTROL: a page whose time reads
/// before now keeps it.
#[test]
fn a_page_left_after_the_clocks_now_counts_from_now() {
    let now = Instant::now();
    let mut unread = recording();
    CLOCK.with(|clock| clock.set(None));
    unread.start(
        Walk::new(vec![Subject::FullDiskAccess], now).expect("a page"),
        "first launch",
        Some(1),
    );
    unread.stop("its panel is gone");
    let page = |lane: &WalkLane| lane.kept(1)[0];
    assert_eq!(page(&unread).at, u64::MAX, "no time read");
    assert!(!page(&unread).due(Some(T0 + 365 * DAY)), "it waits");

    at(T0);
    let mut read = lane(false);
    read.take_left(1);
    assert_eq!(page(&read).at, T0, "now, as the clock reads it");
    assert_eq!(
        RECORD.with(|record| record.borrow().clone().map(|(_, pages)| pages[0].at)),
        Some(T0),
        "and kept"
    );

    at(T0 + 10);
    let mut again = lane(false);
    again.take_left(1);
    assert_eq!(page(&again).at, T0, "a time before now stands");
}

/// An absolute path to a `.app`, spaces and all, on the host the test runs on.
fn host_bundle(name: &str) -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"C:\Applications"
    } else {
        "/Applications"
    })
    .join(name)
}

/// A hardware UUID, one word.
const MAC: &str = "1D683F62-5C4B-4E3A-9F21-7B8C6D5E4A30";

/// A walk record line reads back the Mac, the revision, the pages in their order with their
/// tries, and the bundle; a page's word this aterm does not know, or tries that do not read
/// whole, is left out, and each page comes once; anything else is junk.
#[test]
fn a_walk_record_line_reads_back_what_was_kept_and_nothing_else() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    let bundle = host_bundle("aterm (dev).app");
    let path = bundle.to_str().expect("a UTF-8 path");
    let line = Line {
        mac: MAC.to_owned(),
        revision: 7,
        pages: vec![
            KeptPage::new(DeveloperTools).left_unanswered(T0),
            KeptPage::new(Accessibility),
        ],
        automation: None,
        bundle: bundle.clone(),
    };
    let text = record_line(&line).expect("a UTF-8 bundle path");
    assert_eq!(
        text,
        format!("{MAC}\t7\tdevtools:1:{T0} accessibility\t{path}")
    );
    assert_eq!(read_line(&text), Some(line.clone()));
    // The window's Automation ask rides the line, after its pages, and alone holds a line.
    let asked = AutomationAsked { tries: 2, at: T0 };
    for pages in [line.pages.clone(), Vec::new()] {
        let with = Line {
            pages,
            automation: Some(asked),
            ..line.clone()
        };
        let text = record_line(&with).expect("a UTF-8 bundle path");
        assert!(text.contains(&format!("automation:2:{T0}")), "{text}");
        assert_eq!(read_line(&text), Some(with));
    }
    assert_eq!(
        read_line(&format!(
            "{MAC}\t7\tcamera devtools devtools:3:9 fda fda:2 fda:x:1 fda:1:2:3\t{path}"
        ))
        .map(|line| line.pages),
        Some(vec![
            KeptPage::new(DeveloperTools),
            KeptPage::new(FullDiskAccess)
        ])
    );
    for junk in [
        String::new(),
        "\t\t\t".to_owned(),
        format!("{MAC}\t7\tdevtools"),
        format!("{MAC}\t7\tdevtools\t{path}\tmore"),
        format!("MAC A\t7\tdevtools\t{path}"),
        format!("\t7\tdevtools\t{path}"),
        format!("{MAC}\t-7\tdevtools\t{path}"),
        format!("{MAC}\t7x\tdevtools\t{path}"),
        format!("{MAC}\t\tdevtools\t{path}"),
        format!("{MAC}\t7\t\t{path}"),
        format!("{MAC}\t7\tcamera\t{path}"),
        format!("{MAC}\t7\tfda:1\t{path}"),
        format!("{MAC}\t7\tautomation\t{path}"),
        format!("{MAC}\t7\tautomation:1\t{path}"),
        format!("{MAC}\t7\tautomation:x:1\t{path}"),
        format!("{MAC}\t7\tdevtools\tApplications/aterm.app"),
    ] {
        assert_eq!(read_line(&junk), None, "{junk:?}");
    }
}

/// The window's Automation ask is kept beside the pages under its revision, so a relaunch
/// asks no sooner: keeping pages keeps it, keeping it keeps the pages, it alone holds the
/// line, and a new revision's read drops it with the line. Before, the mark lived in the
/// process alone, so every relaunch asked again. NEGATIVE CONTROL: another revision, bundle
/// or Mac reads no ask.
#[test]
fn the_walk_record_keeps_the_automation_ask_beside_the_pages() {
    use Subject::{Accessibility, DeveloperTools};
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    let (bundle, other) = (host_bundle("aterm.app"), host_bundle("aterm (dev).app"));
    let asked = AutomationAsked::after(None, T0);
    assert_eq!(asked, AutomationAsked { tries: 1, at: T0 });
    keep_automation_ask_in(state, &bundle, MAC, 3, asked).expect("the walk record");
    assert_eq!(automation_asked_in(state, &bundle, MAC, 3), Some(asked));
    assert!(
        state.join(WALK_RECORD).exists(),
        "the ask alone holds the line"
    );
    for (bundle, mac, revision) in [(&other, MAC, 3), (&bundle, "OTHER", 3), (&bundle, MAC, 4)] {
        assert_eq!(automation_asked_in(state, bundle, mac, revision), None);
    }
    let pages = vec![KeptPage::new(DeveloperTools), KeptPage::new(Accessibility)];
    keep_pages_in(state, &bundle, MAC, 3, &pages).expect("the walk record");
    assert_eq!(
        automation_asked_in(state, &bundle, MAC, 3),
        Some(asked),
        "pages keep it"
    );
    let again = AutomationAsked::after(Some(asked), T0 + 5);
    assert_eq!(
        again,
        AutomationAsked {
            tries: 2,
            at: T0 + 5
        }
    );
    keep_automation_ask_in(state, &bundle, MAC, 3, again).expect("the walk record");
    assert_eq!(
        left_pages_in(state, &bundle, MAC, 3),
        pages,
        "it keeps the pages"
    );
    keep_pages_in(state, &bundle, MAC, 3, &[]).expect("the walk record");
    assert_eq!(
        automation_asked_in(state, &bundle, MAC, 3),
        Some(again),
        "nor do no pages drop it"
    );
    assert!(left_pages_in(state, &bundle, MAC, 4).is_empty());
    assert_eq!(
        automation_asked_in(state, &bundle, MAC, 3),
        None,
        "a new revision drops it"
    );
    assert!(!state.join(WALK_RECORD).exists());
    // Its pause is the walk's, from the last ask; a clock set back asks at once.
    assert_eq!(again.due_by(T0 + 6), WALK_AGAIN.due(T0 + 5, 2));
    assert_eq!(again.due_by(T0), T0);
}

/// The walk record keeps one line for each bundle on each Mac: the pages that bundle's walk
/// has still to show there under a revision, with their tries, read as often as asked and for
/// as long as it stands (no clock bounds it), replaced by the next pages kept, gone with no
/// page. A line read under another revision goes and gives nothing; another bundle's or Mac's
/// stays for the aterm it names. Junk is passed over and goes at the next write, the newest
/// [`MAX_RECORD_LINES`] lines stay, and a bundle path no line can hold or a Mac that is not
/// one word writes nothing.
#[test]
fn the_walk_record_keeps_each_bundles_pages_on_each_mac_under_its_revision() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    const OTHER_MAC: &str = "8F2E6A41-0B3C-4D5E-A697-C8D9E0F1A2B3";
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    let record = state.join(WALK_RECORD);
    let (bundle, other) = (host_bundle("aterm.app"), host_bundle("aterm (dev).app"));
    let pages = vec![
        KeptPage::new(FullDiskAccess).left_unanswered(T0),
        KeptPage::new(DeveloperTools),
        KeptPage::new(Accessibility),
    ];
    let keep = |bundle: &Path, mac: &str, revision: u64, pages: &[KeptPage]| {
        keep_pages_in(state, bundle, mac, revision, pages).expect("the walk record");
    };
    let left =
        |bundle: &Path, mac: &str, revision: u64| left_pages_in(state, bundle, mac, revision);

    assert!(left(&bundle, MAC, 3).is_empty(), "no record");
    keep(&bundle, MAC, 3, &pages);
    assert_eq!(left(&bundle, MAC, 3), pages, "read, with the tries");
    assert_eq!(
        left(&bundle, MAC, 3),
        pages,
        "and read again: a read takes nothing"
    );
    keep(&other, MAC, 3, &fresh(&[Accessibility]));
    keep(&bundle, OTHER_MAC, 3, &fresh(&[DeveloperTools]));
    keep(&bundle, MAC, 3, &fresh(&[DeveloperTools, Accessibility]));
    assert_eq!(
        left(&bundle, MAC, 3),
        fresh(&[DeveloperTools, Accessibility]),
        "replaced"
    );
    assert_eq!(
        left(&other, MAC, 3),
        fresh(&[Accessibility]),
        "another bundle's stays"
    );
    assert_eq!(
        left(&bundle, OTHER_MAC, 3),
        fresh(&[DeveloperTools]),
        "another Mac's stays"
    );

    assert!(
        left(&bundle, MAC, 4).is_empty(),
        "a new revision asks again by itself"
    );
    assert!(
        left(&bundle, MAC, 3).is_empty(),
        "the old revision's line went"
    );
    assert_eq!(
        left(&other, MAC, 3),
        fresh(&[Accessibility]),
        "the others stay"
    );
    keep(&other, MAC, 3, &[]);
    assert!(left(&other, MAC, 3).is_empty(), "no page removes its line");
    keep(&bundle, OTHER_MAC, 3, &[]);
    assert!(!record.exists(), "the record goes with its last line");

    let line = |bundle: &Path, pages: &[KeptPage]| {
        record_line(&Line {
            mac: MAC.to_owned(),
            revision: 3,
            pages: pages.to_vec(),
            automation: None,
            bundle: bundle.to_path_buf(),
        })
        .expect("a line")
    };
    std::fs::write(
        &record,
        format!("junk\n{}\n", line(&other, &fresh(&[DeveloperTools]))),
    )
    .expect("junk");
    assert_eq!(
        left(&other, MAC, 3),
        fresh(&[DeveloperTools]),
        "junk is passed over"
    );
    keep(&bundle, MAC, 3, &pages);
    let text = std::fs::read_to_string(&record).expect("the record");
    assert_eq!(
        text,
        format!(
            "{}\n{}\n",
            line(&other, &fresh(&[DeveloperTools])),
            line(&bundle, &pages)
        ),
        "junk goes at the next write"
    );

    let many: Vec<PathBuf> = (0..=MAX_RECORD_LINES)
        .map(|n| host_bundle(&format!("aterm {n}.app")))
        .collect();
    for each in &many {
        keep(each, MAC, 3, &fresh(&[Accessibility]));
    }
    assert!(left(&many[0], MAC, 3).is_empty(), "the oldest line went");
    assert_eq!(
        left(&many[MAX_RECORD_LINES], MAC, 3),
        fresh(&[Accessibility])
    );

    let before = std::fs::read_to_string(&record).expect("the record");
    let split = PathBuf::from(format!("{}\nx.app", bundle.display()));
    let tabbed = PathBuf::from(format!("{}\tx.app", bundle.display()));
    let long = host_bundle(&"a".repeat(MAX_RECORD_BYTES));
    for (bundle, mac) in [
        (&split, MAC),
        (&tabbed, MAC),
        (&long, MAC),
        (&bundle, "MAC A"),
        (&bundle, ""),
    ] {
        assert!(
            keep_pages_in(state, bundle, mac, 3, &pages).is_err(),
            "{bundle:?} on {mac:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&record).expect("the record"),
        before,
        "nothing written"
    );
}

/// An older aterm's Quit & Reopen marker for this bundle on this Mac is taken into the walk
/// record at its first read, its pages due at once and ahead of the line's own, and goes, so
/// a Quit & Reopen that opened into this aterm (a staged update) walks on. NEGATIVE CONTROLS:
/// another bundle's or Mac's marker stays for its aterm and gives nothing; one that does not
/// read goes and gives nothing.
#[test]
fn an_older_aterms_quit_and_reopen_marker_joins_the_walk_record_once() {
    use Subject::{Accessibility, DeveloperTools, FullDiskAccess};
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    let marker = state.join(OLD_REOPEN_MARKER);
    let bundle = host_bundle("aterm.app");
    let path = bundle.to_str().expect("a UTF-8 path");
    let write = |text: String| std::fs::write(&marker, text).expect("the marker");

    write(format!(
        "1700000000 devtools accessibility\n{path}\n{MAC}\n"
    ));
    let waits = KeptPage::new(FullDiskAccess).left_unanswered(T0);
    keep_pages_in(state, &bundle, MAC, 2, &[waits]).expect("a line");
    assert_eq!(
        left_pages_in(state, &bundle, MAC, 2),
        vec![
            KeptPage::new(DeveloperTools),
            KeptPage::new(Accessibility),
            waits
        ]
    );
    assert!(!marker.exists(), "the marker went");
    assert_eq!(
        left_pages_in(state, &bundle, MAC, 2).len(),
        3,
        "the record keeps them"
    );

    write(format!("1700000000 devtools\n{path}\n{MAC}\n"));
    keep_pages_in(state, &bundle, MAC, 1, &fresh(&[FullDiskAccess])).expect("a line");
    assert_eq!(
        left_pages_in(state, &bundle, MAC, 2),
        fresh(&[DeveloperTools]),
        "a line under another revision gives way to the marker's pages"
    );

    let other = host_bundle("aterm (dev).app");
    let other = other.to_str().expect("a UTF-8 path");
    for (theirs, why) in [
        (format!("1 fda\n{other}\n{MAC}\n"), "another bundle's"),
        (
            format!("1 fda\n{path}\n8F2E6A41-0B3C-4D5E-A697-C8D9E0F1A2B3\n"),
            "another Mac's",
        ),
    ] {
        write(theirs.clone());
        assert_eq!(
            left_pages_in(state, &bundle, MAC, 2),
            fresh(&[DeveloperTools]),
            "{why}"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).expect("left in place"),
            theirs,
            "{why}"
        );
    }
    for junk in [
        format!("fda\n{path}\n{MAC}\n"),
        format!("1 camera\n{path}\n{MAC}\n"),
        format!("1 fda\n{path}\n"),
    ] {
        write(junk.clone());
        assert_eq!(
            left_pages_in(state, &bundle, MAC, 2),
            fresh(&[DeveloperTools]),
            "{junk:?}"
        );
        assert!(!marker.exists(), "{junk:?} went");
    }
}
