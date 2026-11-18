// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A trace against scratch files, scripted clocks and test statics: what it records, when it
//! stands, and what keeps it from being kept.

use super::*;

/// One second, in nanoseconds.
const SEC_NS: i128 = 1_000_000_000;

/// The value the exit test's run reads.
static GUARD: Watched<u32> = Watched::new("trace-test-guard", 0);

/// The run the exit test records: it reads `file`, compares a [`Now`] of 1 000 against 900
/// (passed) and 1 500 (still to come), and reads [`GUARD`], with its reads judged a minute
/// later than the clock so the fixture's file has settled.
fn guarded_run(file: &Path) -> Trace {
    read_as_if_later(Duration::from_secs(60), || {
        scope(|| {
            let text = fs::read_to_string(file).expect("the fixture's roster");
            assert!(text.starts_with("label"), "{text:?}");
            let now = Now::at(1_000);
            assert!(now.passed(900));
            assert!(!now.passed(1_500));
            let guard = GUARD.lock().expect("the guard");
            assert!(*guard < 10, "{}", *guard);
        })
        .1
    })
}

/// A trace stands while nothing it recorded has moved, and each kind of move ends it: a read
/// file rewritten (on Unix at the same length with its modification time put back, so only its
/// inode-change time moves; elsewhere, where std holds no such time, at another length), the
/// wall clock past a bound the run compared against or back before the other, and a watched
/// value changed on another thread.
///
/// NEGATIVE CONTROLS (2026-11-14), each run alone and then restored:
/// - `Trace::stands` without its loop over the files read failed with "a rewrite of a read
///   file, at the same length with its modification time put back on Unix, left the trace
///   standing", left `Ok(())`, right `Err(File(".../accounts.toml"))` (macOS).
/// - `Now::at_least` narrowing nothing failed with "the trace stood at 1 500, the bound its run
///   compared against", left `Ok(())`, right `Err(Wall)`.
/// - `Trace::stands` without its check of the generations read failed with "a watched value
///   changed on another thread left the trace standing", left `Ok(())`, right
///   `Err(Memory("trace-test-guard"))`.
#[test]
fn a_trace_stands_until_a_read_file_a_crossed_bound_or_a_watched_generation_moves() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("accounts.toml");
    std::fs::write(&file, "label = \"a\"\n").expect("the fixture's roster");
    let mono = Instant::now();

    let trace = guarded_run(&file);
    assert_eq!(trace.unkept(), None, "{trace:?}");
    for secs in [900, 1_000, 1_499] {
        assert_eq!(
            trace.stands(Now::at(secs), mono),
            Ok(()),
            "nothing moved, yet the trace did not stand at {secs}"
        );
    }

    assert_eq!(
        trace.stands(Now::at(1_500), mono),
        Err(Moved::Wall),
        "the trace stood at 1 500, the bound its run compared against"
    );
    assert_eq!(
        trace.stands(Now::at(899), mono),
        Err(Moved::Wall),
        "the trace stood at 899, before the 900 its run found passed"
    );
    assert_eq!(trace.horizon().wall_ns, Some(1_500 * SEC_NS));

    std::thread::spawn(|| *GUARD.lock().expect("the guard") += 1)
        .join()
        .expect("the other thread");
    assert_eq!(
        trace.stands(Now::at(1_000), mono),
        Err(Moved::Memory("trace-test-guard")),
        "a watched value changed on another thread left the trace standing"
    );

    let trace = guarded_run(&file);
    assert_eq!(trace.stands(Now::at(1_000), mono), Ok(()));
    #[cfg(unix)]
    {
        let modified = std::fs::metadata(&file)
            .and_then(|meta| meta.modified())
            .expect("the roster's modification time");
        std::fs::write(&file, "label = \"b\"\n").expect("the rewrite");
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|opened| opened.set_modified(modified))
            .expect("the modification time put back");
    }
    #[cfg(not(unix))]
    std::fs::write(&file, "label = \"bb\"\n").expect("the rewrite");
    assert_eq!(
        trace.stands(Now::at(1_000), mono),
        Err(Moved::File(file.clone())),
        "a rewrite of a read file, at the same length with its modification time put back on \
         Unix, left the trace standing"
    );
}

/// The value the generation test changes.
static TOGGLED: Watched<Vec<u8>> = Watched::new("trace-test-toggled", Vec::new());

/// A watched value's generation moves only when a guard leaves the value otherwise: a change
/// undone before its guard drops moves nothing, records no effect and rings nothing, while a
/// change on another thread moves it by one, records the effect in that thread's scope and
/// rings there, and ends every trace that read the old generation. A [`Generation`] moves by
/// its owner's bump, with the same effect and ring.
///
/// NEGATIVE CONTROL (2026-11-14): `WatchedGuard::drop` moving the generation on every drop
/// failed with "a change undone before its guard dropped moved the generation", left `1`,
/// right `0`. Restored.
#[test]
fn a_watched_value_moves_its_generation_only_when_a_guard_leaves_it_otherwise() {
    let rung = rung_here();
    let ((), toggled) = scope(|| {
        let mut guard = TOGGLED.lock().expect("the value");
        guard.push(1);
        guard.pop();
    });
    assert_eq!(
        TOGGLED.generation(),
        0,
        "a change undone before its guard dropped moved the generation"
    );
    assert_eq!(toggled.unkept(), None, "{toggled:?}");
    assert_eq!(rung_here(), rung, "a change undone rang the bell");
    assert_eq!(
        toggled.memory().collect::<Vec<_>>(),
        [("trace-test-toggled", 0)]
    );
    assert_eq!(toggled.stands(Now::at(0), Instant::now()), Ok(()));

    let (rang, effects) = std::thread::spawn(|| {
        let before = rung_here();
        let ((), changed) = scope(|| TOGGLED.lock().expect("the value").push(7));
        (rung_here() - before, changed.effects().collect::<Vec<_>>())
    })
    .join()
    .expect("the other thread");
    assert_eq!(TOGGLED.generation(), 1);
    assert_eq!(
        rang, 1,
        "a change on another thread rang {rang} times there"
    );
    assert_eq!(effects, [Effect::Memory]);
    assert_eq!(
        toggled.stands(Now::at(0), Instant::now()),
        Err(Moved::Memory("trace-test-toggled"))
    );

    let latches = Generation::new("trace-test-latches");
    let ((), read) = scope(|| latches.record());
    assert_eq!(read.stands(Now::at(0), Instant::now()), Ok(()));
    let before = rung_here();
    let ((), bumped) = scope(|| latches.clone().bump());
    assert_eq!(latches.get(), 1);
    assert_eq!(rung_here() - before, 1);
    assert_eq!(bumped.unkept(), Some(Unkept::Effect(Effect::Memory)));
    assert_eq!(
        read.stands(Now::at(0), Instant::now()),
        Err(Moved::Memory("trace-test-latches"))
    );
}

/// A watched value that is no static, shared by an `Arc`, is watched as a static is: a change
/// made through a guard dropped on another thread moves its generation, rings there and ends a
/// trace that read it, and that trace keeps judging by the value's counter after the value is
/// dropped. A poisoned lock answers `Err` as a `Mutex` does, and the guard it hands out watches
/// the value too. With no scope armed and nothing changed, a lock makes no counter at all.
///
/// NEGATIVE CONTROL (2026-11-15): `Watched::lock` handing the poisoned guard out as `Ok` failed
/// with "a poisoned lock answered Ok". Restored.
#[test]
fn a_watched_value_shared_by_an_arc_is_watched_across_threads_and_through_a_poisoned_lock() {
    let shared = Arc::new(Watched::new("trace-test-shared", 0u32));
    *shared.lock().expect("the value") += 0;
    assert!(
        shared.count.get().is_none(),
        "a lock with no scope armed that changed nothing made a counter"
    );
    let ((), read) = scope(|| assert_eq!(*shared.lock().expect("the value"), 0));
    assert_eq!(
        read.memory().collect::<Vec<_>>(),
        [("trace-test-shared", 0)]
    );
    assert_eq!(read.stands(Now::at(0), Instant::now()), Ok(()));

    let other = Arc::clone(&shared);
    let rang = std::thread::spawn(move || {
        let before = rung_here();
        *other.lock().expect("the value") = 1;
        rung_here() - before
    })
    .join()
    .expect("the other thread");
    assert_eq!(
        rang, 1,
        "a change on another thread rang {rang} times there"
    );
    assert_eq!(shared.generation(), 1);
    assert_eq!(
        read.stands(Now::at(0), Instant::now()),
        Err(Moved::Memory("trace-test-shared"))
    );

    let ((), reread) = scope(|| assert_eq!(*shared.lock().expect("the value"), 1));
    let poisoning = Arc::clone(&shared);
    let panicked = std::thread::spawn(move || {
        let mut guard = poisoning.lock().expect("the value");
        *guard = 2;
        panic!("the holder panics with the value changed");
    })
    .join();
    assert!(panicked.is_err(), "the holder did not panic");
    assert_eq!(
        shared.generation(),
        2,
        "a change a panic dropped moved nothing"
    );
    assert_eq!(
        reread.stands(Now::at(0), Instant::now()),
        Err(Moved::Memory("trace-test-shared"))
    );
    let Err(poisoned) = shared.lock() else {
        panic!("a poisoned lock answered Ok");
    };
    let mut guard = poisoned.into_inner();
    assert_eq!(*guard, 2);
    *guard = 3;
    drop(guard);
    assert_eq!(
        shared.generation(),
        3,
        "a poisoned lock's guard watched nothing"
    );

    let ((), last) = scope(|| {
        let _ = shared.lock().map_err(PoisonError::into_inner);
    });
    drop(shared);
    assert_eq!(
        last.stands(Now::at(0), Instant::now()),
        Ok(()),
        "a trace stopped standing once the value it read was dropped"
    );
}

/// A raw read of the wall clock, anywhere in a scope's extent, poisons that scope and each one
/// armed around it: an `aterm_time` accessor in a nested scope, one within [`unrecorded`], and
/// [`Now::raw`]. A run that compares through a [`Now`], read by [`Now::at_now`], is not.
///
/// NEGATIVE CONTROL (2026-11-14): `close_frame` without its count of raw reads failed with "a
/// unix_secs read inside the scope left it unpoisoned: Trace { .. }". Restored.
#[test]
fn a_raw_read_of_the_wall_clock_poisons_every_scope_armed_around_it() {
    let (inner, outer) = scope(|| {
        scope(|| {
            let _ = aterm_time::unix_secs();
        })
        .1
    });
    assert!(
        matches!(inner.unkept(), Some(Unkept::Poison(_))),
        "a unix_secs read inside the scope left it unpoisoned: {inner:?}"
    );
    assert!(
        matches!(outer.unkept(), Some(Unkept::Poison(_))),
        "a unix_secs read inside a nested scope left the outer one unpoisoned: {outer:?}"
    );
    assert_eq!(outer.horizon().rerun, Some(Rerun::Cap));

    let ((), around) = scope(|| {
        unrecorded(|| {
            let _ = aterm_time::unix_millis();
        });
    });
    assert!(
        matches!(around.unkept(), Some(Unkept::Poison(_))),
        "a unix_millis read within unrecorded left the scope around it unpoisoned: {around:?}"
    );

    let ((), raw) = scope(|| {
        let _ = Now::at(5).raw();
    });
    assert_eq!(
        raw.unkept(),
        Some(Unkept::Poison(Cow::Borrowed("a raw read of now")))
    );

    let ((), compared) = scope(|| {
        let _ = Now::at_now().passed(0);
    });
    assert_eq!(compared.unkept(), None, "{compared:?}");
}

/// A RAW READ OF THE WALL CLOCK SET ASIDE ([`clock_set_aside`]) POISONS NO SCOPE ARMED AROUND IT,
/// at any depth, while what the set-aside run compares is still recorded; a scope armed within
/// the run still counts the raw reads made in it, and its poison reaches the scopes around it.
/// CONTROL: the same read, not set aside, poisons the scope.
///
/// NEGATIVE CONTROL (2026-11-16): `clock_set_aside` without its move of each armed scope's count
/// failed `a read set aside poisons no scope around it`, left `Some(Poison("the wall clock was
/// read raw 1 times"))`, right `None`. Restored, the file touched.
#[test]
fn a_raw_wall_read_set_aside_poisons_no_scope_around_it_and_one_armed_within_still_counts() {
    let (inner, outer) = scope(|| {
        let (read, inner) = scope(|| {
            clock_set_aside(|| {
                let _ = Now::at(10).passed(5);
                aterm_time::unix_secs()
            })
        });
        assert!(read.is_some());
        inner
    });
    assert_eq!(
        inner.unkept(),
        None,
        "a read set aside poisons no scope around it"
    );
    assert_eq!(outer.unkept(), None, "nor the one around that");
    assert!(
        inner.stands(Now::at(5), Instant::now()).is_ok()
            && inner.stands(Now::at(4), Instant::now()).is_err(),
        "what the run set aside compared is recorded: {inner:?}"
    );

    let ((), plain) = scope(|| {
        let _ = aterm_time::unix_secs();
    });
    assert!(
        matches!(plain.unkept(), Some(Unkept::Poison(_))),
        "CONTROL: a read not set aside poisons the scope: {plain:?}"
    );

    let ((), around) = scope(|| {
        clock_set_aside(|| {
            let ((), within) = scope(|| {
                let _ = aterm_time::unix_millis();
            });
            assert!(
                matches!(within.unkept(), Some(Unkept::Poison(_))),
                "a scope armed within counts its own reads: {within:?}"
            );
        });
    });
    assert!(
        matches!(around.unkept(), Some(Unkept::Poison(_))),
        "its poison reaches the scope around it: {around:?}"
    );
}

/// The settle rule holds at its edge: a file that changed [`SETTLED`] before the read has
/// settled, and one nanosecond later it has not. A read of a file judged within the window is
/// not kept, and owes the moment its file settles; judged at that moment, it is kept.
///
/// NEGATIVE CONTROL (2026-11-14): `settled` answering `true` always failed with "a file that
/// changed one nanosecond inside the window had settled". Restored.
#[test]
fn a_file_read_within_the_settle_window_is_not_kept_until_it_settles() {
    let at = UNIX_EPOCH + Duration::from_secs(100);
    assert!(
        settled(98 * SEC_NS, at),
        "a file two seconds old had not settled"
    );
    assert!(
        !settled(98 * SEC_NS + 1, at),
        "a file that changed one nanosecond inside the window had settled"
    );
    assert!(!settled(0, UNIX_EPOCH - Duration::from_secs(1)));

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("state.json");
    std::fs::write(&file, "{}").expect("the fixture");
    let changed = Look::of(&file, Reach::Followed)
        .stamp()
        .expect("the fixture's look")
        .expect("the fixture is there")
        .changed_ns();
    let instant = |ns: i128| {
        UNIX_EPOCH + Duration::from_nanos(u64::try_from(ns).expect("a time past the epoch"))
    };

    let ((), fresh) = read_as_if_at(instant(changed + SEC_NS), || {
        scope(|| {
            fs::read(&file).expect("the fixture");
        })
    });
    let until_ns = changed + 2 * SEC_NS;
    assert_eq!(
        fresh.unkept(),
        Some(Unkept::Unsettled { until_ns }),
        "a file read a second after it changed was kept"
    );
    assert_eq!(fresh.horizon().wall_ns, Some(until_ns));
    assert_eq!(fresh.horizon().rerun, None);
    assert!(matches!(
        fresh.stands(Now::at(0), Instant::now()),
        Err(Moved::Unkept(Unkept::Unsettled { .. }))
    ));

    let ((), settled_read) = read_as_if_at(instant(until_ns), || {
        scope(|| {
            fs::read(&file).expect("the fixture");
        })
    });
    assert_eq!(settled_read.unkept(), None, "{settled_read:?}");
}

/// A run that unwinds.
fn unwinds() {
    panic!("the inner run unwinds");
}

/// The paths a trace read, in order.
fn paths(trace: &Trace) -> Vec<PathBuf> {
    trace.files().map(|(path, _)| path.to_path_buf()).collect()
}

/// A scope armed within another merges what it recorded into it as it closes; a read within
/// [`unrecorded`] reaches neither, and a scope armed there records apart. A scope that unwinds
/// poisons the scope around it and leaves no scope armed; [`absorb`] adds a kept trace to the
/// armed scope; [`in_keeper`] holds only within [`keeper`].
///
/// NEGATIVE CONTROL (2026-11-14): `close_frame` merging nothing into the scope around it failed
/// with "the outer trace lacks the inner scope's read", left `[".../b"]`, right
/// `[".../a", ".../b"]`. Restored.
#[test]
fn a_nested_scope_merges_into_its_parent_and_an_unrecorded_read_reaches_neither() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let [a, b, c, d] = ["a", "b", "c", "d"].map(|name| {
        let path = dir.path().join(name);
        std::fs::write(&path, name).expect("a fixture");
        path
    });

    let (inner, outer) = read_as_if_later(Duration::from_secs(60), || {
        scope(|| {
            let ((), inner) = scope(|| assert!(fs::is_file(&a)));
            assert!(fs::is_file(&b));
            unrecorded(|| {
                assert!(!armed());
                assert!(fs::is_file(&c));
                let ((), apart) = scope(|| assert!(fs::is_file(&d)));
                assert_eq!(paths(&apart), std::slice::from_ref(&d));
            });
            assert!(armed());
            inner
        })
    });
    assert_eq!(paths(&inner), std::slice::from_ref(&a));
    assert_eq!(
        paths(&outer),
        [a.clone(), b.clone()],
        "the outer trace lacks the inner scope's read"
    );
    assert!(!armed());

    let ((), adopted) = scope(|| absorb(&inner));
    assert_eq!(paths(&adopted), std::slice::from_ref(&a));

    let ((), around) = scope(|| {
        let unwound = std::panic::catch_unwind(|| scope(unwinds));
        assert!(unwound.is_err());
        assert!(armed());
    });
    assert_eq!(
        around.unkept(),
        Some(Unkept::Poison(Cow::Borrowed("the scope unwound")))
    );
    assert!(!armed());

    assert!(!in_keeper());
    keeper(|| assert!(in_keeper()));
    assert!(!in_keeper());
}

/// A listing stands while its directory holds the same names, each of the same kind: a file
/// renamed over one of the same name moves nothing, while a name that comes, or names a folder
/// where it named a file, ends it, and the listing stands again once the names are back. A
/// path found missing stands until something is there.
///
/// NEGATIVE CONTROL (2026-11-14): `names_digest` without the kinds failed with "a file replaced
/// by a folder of the same name left the listing standing", left `Ok(())`, right
/// `Err(Listing(".../logins"))`. Restored.
#[test]
fn a_listing_stands_until_a_name_or_its_kind_moves_and_a_miss_until_its_path_appears() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let logins = dir.path().join("logins");
    std::fs::create_dir(&logins).expect("the logins folder");
    std::fs::write(logins.join("a"), "a").expect("a login");
    std::fs::create_dir(logins.join("b")).expect("a login folder");
    let absent = dir.path().join("absent.toml");

    let ((), trace) = scope(|| {
        let entries = fs::read_dir(&logins).expect("the logins folder");
        assert_eq!(entries.len(), 2);
        assert!(!fs::exists(&absent));
    });
    let stands = || trace.stands(Now::at(0), Instant::now());
    assert_eq!(trace.unkept(), None, "{trace:?}");
    assert_eq!(stands(), Ok(()));

    std::fs::write(logins.join("a.new"), "a2").expect("a new login");
    std::fs::rename(logins.join("a.new"), logins.join("a")).expect("renamed over");
    assert_eq!(
        stands(),
        Ok(()),
        "a file renamed over one of its name moved the listing"
    );

    std::fs::write(logins.join("c"), "c").expect("a sibling");
    assert_eq!(stands(), Err(Moved::Listing(logins.clone())));
    std::fs::remove_file(logins.join("c")).expect("the sibling gone");
    assert_eq!(stands(), Ok(()), "the names back did not stand");

    std::fs::remove_file(logins.join("a")).expect("the login gone");
    std::fs::create_dir(logins.join("a")).expect("a folder of its name");
    assert_eq!(
        stands(),
        Err(Moved::Listing(logins.clone())),
        "a file replaced by a folder of the same name left the listing standing"
    );
    std::fs::remove_dir(logins.join("a")).expect("the folder gone");
    std::fs::write(logins.join("a"), "a").expect("the login back");
    assert_eq!(stands(), Ok(()));

    std::fs::write(&absent, "").expect("the missing file made");
    assert_eq!(
        stands(),
        Err(Moved::File(absent.clone())),
        "a path found missing stood once something was there"
    );
}

/// The owner's digest of a fixture: its first line alone, as a reader that decides by a file's
/// revision and not by when it was last heard.
fn first_line(path: &Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(aterm_digest::Sha256::digest(
        text.lines().next().unwrap_or(""),
    ))
}

/// An owner's standing stands while its digest computes the same: a rewrite of what its readers
/// do not decide by moves nothing, while a change to what they do, or the file gone, ends it.
///
/// NEGATIVE CONTROL (2026-11-14): `Trace::stands` without its loop over the standings failed
/// with "a revision change left the standing standing", left `Ok(())`, right
/// `Err(Standing(".../state.toml"))`. Restored.
#[test]
fn a_standing_stands_while_its_owner_digest_does() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("state.toml");
    std::fs::write(&file, "rev = 1\nheard = 10\n").expect("the fixture");

    let ((), trace) = scope(|| {
        let value = unrecorded(|| first_line(&file));
        standing(&file, value, first_line);
    });
    assert_eq!(trace.standings().collect::<Vec<_>>(), [file.as_path()]);
    assert_eq!(paths(&trace), Vec::<PathBuf>::new());
    let stands = || trace.stands(Now::at(0), Instant::now());
    assert_eq!(stands(), Ok(()));

    std::fs::write(&file, "rev = 1\nheard = 11\n").expect("a heard-only save");
    assert_eq!(stands(), Ok(()), "a heard-only save moved the standing");

    std::fs::write(&file, "rev = 2\nheard = 11\n").expect("a revision");
    assert_eq!(
        stands(),
        Err(Moved::Standing(file.clone())),
        "a revision change left the standing standing"
    );
    std::fs::remove_file(&file).expect("the file gone");
    assert_eq!(stands(), Err(Moved::Standing(file.clone())));
}

/// A [`Since`] that has not elapsed records its deadline, and the trace stands short of it and
/// not at it; one that has elapsed records nothing, as a later instant answers the same; of
/// two deadlines the trace keeps the earlier.
///
/// NEGATIVE CONTROL (2026-11-14): `Since::elapsed_at_least` recording nothing failed at the
/// first deadline's record, left `None`, right `Some(Instant { .. })`. Restored.
#[test]
fn a_since_not_yet_elapsed_records_its_deadline_and_one_elapsed_records_nothing() {
    let start = Instant::now();
    let secs = Duration::from_secs;
    let since = Since::new(start);

    let (elapsed, trace) = scope(|| since.elapsed_at_least(secs(10), start + secs(3)));
    assert!(!elapsed);
    assert_eq!(trace.mono_deadline(), Some(start + secs(10)));
    assert_eq!(trace.horizon().mono, Some(start + secs(10)));
    assert_eq!(trace.stands(Now::at(0), start + secs(9)), Ok(()));
    assert_eq!(
        trace.stands(Now::at(0), start + secs(10)),
        Err(Moved::Mono),
        "the trace stood at its deadline"
    );

    let (elapsed, trace) = scope(|| since.elapsed_at_least(secs(10), start + secs(10)));
    assert!(elapsed);
    assert_eq!(trace.mono_deadline(), None);

    let ((), both) = scope(|| {
        assert!(!since.elapsed_at_least(secs(30), start));
        assert!(!since.elapsed_at_least(secs(20), start));
    });
    assert_eq!(both.mono_deadline(), Some(start + secs(20)));
}

/// A comparison of a [`Now`], with the `i64` arithmetic it stands for.
type Comparison = (&'static str, fn(Now) -> bool, fn(i64) -> bool);

/// Each comparison of a [`Now`] answers as its arithmetic does, and narrows the wall to exactly
/// the run of seconds around its reading that answer the same: recorded at any second of a
/// range around its bounds, the trace stands at a second of that range if and only if every
/// second from the reading to it answers the same (so never where the answer differs, and,
/// for a `within` found false, not on the far side of its window, which an interval cannot
/// hold beside the near one). The horizon is the first second past the interval; the extremes
/// of `i64` overflow nothing.
///
/// NEGATIVE CONTROL (2026-11-14): `Now::within` narrowing a true answer to `start..=end` failed
/// with "within 5 for 5: recorded at 5, replayed at 10". Restored.
#[test]
fn each_comparison_of_now_narrows_the_wall_to_exactly_the_seconds_that_answer_the_same() {
    let comparisons: [Comparison; 7] = [
        ("passed 10", |now| now.passed(10), |t| t >= 10),
        ("before 10", |now| now.before(10), |t| t < 10),
        ("aged 4 by 6", |now| now.aged(4, 6), |t| t - 4 >= 6),
        (
            "within 5 for 5",
            |now| now.within(5, 5),
            |t| (0..5).contains(&(t - 5)),
        ),
        ("within 5 for 1", |now| now.within(5, 1), |t| t == 5),
        ("within 5 for 0", |now| now.within(5, 0), |_| false),
        ("within 5 for -3", |now| now.within(5, -3), |_| false),
    ];
    for (name, compare, arithmetic) in comparisons {
        for at in -2..=22 {
            let (answer, trace) = scope(|| compare(Now::at(at)));
            assert_eq!(answer, arithmetic(at), "{name} at {at}");
            for t in -2..=22 {
                let same_run = (at.min(t)..=at.max(t)).all(|s| arithmetic(s) == answer);
                assert_eq!(
                    trace.stands(Now::at(t), Instant::now()).is_ok(),
                    same_run,
                    "{name}: recorded at {at}, replayed at {t}"
                );
            }
        }
    }

    let (_, trace) = scope(|| Now::at(3).before(10));
    assert_eq!(
        trace.wall(),
        Wall {
            lo: i64::MIN,
            hi: 9
        }
    );
    assert_eq!(trace.horizon().wall_ns, Some(10 * SEC_NS));

    let (passed, trace) = scope(|| Now::at(i64::MAX).passed(i64::MIN));
    assert!(passed);
    assert_eq!(trace.wall(), Wall::ALWAYS);
    assert_eq!(trace.horizon().wall_ns, None);
    let (aged, trace) = scope(|| Now::at(i64::MIN).aged(i64::MAX, i64::MAX));
    assert!(!aged);
    assert!(trace.stands(Now::at(i64::MAX), Instant::now()).is_ok());
}

/// An effect, a retry or a poison keeps a run from being kept (a poison first, then a retry,
/// then an effect), and such a trace stands for nothing. An effect or a retry owes its rerun
/// after the backoff, 10, 20 and 40 seconds and then 60 for good; a poison after the cap. With
/// no scope armed, nothing is recorded.
///
/// NEGATIVE CONTROL (2026-11-14): `backoff` without its cap failed with left `[10s, 10s, 20s,
/// 40s, 80s, 160s, 320s]`, right `[10s, 10s, 20s, 40s, 60s, 60s, 60s]`. Restored.
#[test]
fn an_effect_a_retry_or_a_poison_keeps_a_run_from_being_kept_and_owes_its_rerun() {
    assert_eq!(
        (0..=6).map(backoff).collect::<Vec<_>>(),
        [10, 10, 20, 40, 60, 60, 60].map(Duration::from_secs)
    );
    assert_eq!(Rerun::Backoff.after(3), Duration::from_secs(40));
    assert_eq!(Rerun::Cap.after(1), RERUN_CAP);

    let ((), wrote) = scope(|| effect(Effect::Write));
    assert_eq!(wrote.unkept(), Some(Unkept::Effect(Effect::Write)));
    assert_eq!(wrote.horizon().rerun, Some(Rerun::Backoff));
    assert_eq!(
        wrote.stands(Now::at(0), Instant::now()),
        Err(Moved::Unkept(Unkept::Effect(Effect::Write)))
    );

    let ((), busy) = scope(|| retry("a lock stayed busy"));
    assert_eq!(
        busy.unkept(),
        Some(Unkept::Retry(Cow::Borrowed("a lock stayed busy")))
    );
    assert_eq!(busy.horizon().rerun, Some(Rerun::Backoff));

    let ((), all) = scope(|| {
        effect(Effect::Spawn);
        retry("a lock stayed busy");
        poison("an unconverted comparison");
    });
    assert_eq!(
        all.unkept(),
        Some(Unkept::Poison(Cow::Borrowed("an unconverted comparison")))
    );
    assert_eq!(all.horizon().rerun, Some(Rerun::Cap));

    effect(Effect::Log);
    retry("with no scope armed");
    poison("with no scope armed");
    let ((), clean) = scope(|| ());
    assert_eq!(clean.unkept(), None);
    assert_eq!(
        clean.horizon(),
        Horizon {
            wall_ns: None,
            mono: None,
            rerun: None
        }
    );
}

/// Each [`fs`] reader answers as std's call does, for a file, a folder, a link to each, a
/// dangling link, a missing path and a path under a file, and records each with no retry; a
/// non-text file read as text fails as std's does. A link records its target too, so a linked
/// file renamed over moves the link's read though the link itself stands.
///
/// NEGATIVE CONTROL (2026-11-14): `Look::stamp` leaving the target out failed at the link's
/// recorded stamp, `[Some(Stamp { own: Meta { kind: Link, .. }, target: None })]`, before its
/// target was renamed over. Restored.
#[cfg(unix)]
#[test]
fn each_fs_reader_answers_as_std_does_and_a_link_records_its_target_too() {
    use std::os::unix::fs::symlink;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path();
    std::fs::write(root.join("file"), "contents").expect("a file");
    std::fs::create_dir(root.join("folder")).expect("a folder");
    symlink(root.join("file"), root.join("linked")).expect("a link to the file");
    symlink(root.join("folder"), root.join("linked-folder")).expect("a link to the folder");
    symlink(root.join("gone"), root.join("dangling")).expect("a dangling link");
    std::fs::write(root.join("binary"), [0xff, 0xfe]).expect("a file that is no text");

    type Answers = (
        bool,
        bool,
        bool,
        Option<u64>,
        Option<bool>,
        Option<Vec<u8>>,
        Option<usize>,
    );
    let ask = |path: &Path| -> Answers {
        (
            fs::is_file(path),
            fs::is_dir(path),
            fs::exists(path),
            fs::metadata(path).map(|meta| meta.len()).ok(),
            fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_symlink())
                .ok(),
            fs::read(path).ok(),
            fs::read_to_string(path).ok().map(|text| text.len()),
        )
    };
    let std_answers = |path: &Path| -> Answers {
        (
            path.is_file(),
            path.is_dir(),
            path.exists(),
            std::fs::metadata(path).map(|meta| meta.len()).ok(),
            std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_symlink())
                .ok(),
            std::fs::read(path).ok(),
            std::fs::read_to_string(path).ok().map(|text| text.len()),
        )
    };
    for name in [
        "file",
        "folder",
        "linked",
        "linked-folder",
        "dangling",
        "missing",
        "file/under",
        "binary",
    ] {
        let path = root.join(name);
        let (answers, trace) = read_as_if_later(Duration::from_secs(60), || scope(|| ask(&path)));
        assert_eq!(answers, std_answers(&path), "{name}");
        assert_eq!(ask(&path), answers, "{name}, with no scope armed");
        assert_eq!(trace.unkept(), None, "{name}: {trace:?}");
        assert_eq!(paths(&trace), std::slice::from_ref(&path), "{name}");
        assert_eq!(trace.stands(Now::at(0), Instant::now()), Ok(()), "{name}");
    }
    assert_eq!(
        read_as_if_later(Duration::from_secs(60), || {
            scope(|| fs::read_to_string(root.join("binary")).map_err(|error| error.kind()))
        })
        .0,
        std::fs::read_to_string(root.join("binary")).map_err(|error| error.kind())
    );
    let opened = scope(|| fs::open(root.join("file")).is_ok());
    assert!(opened.0);
    assert_eq!(paths(&opened.1), [root.join("file")]);

    let linked = root.join("linked");
    let ((), trace) = read_as_if_later(Duration::from_secs(60), || {
        scope(|| {
            fs::read(&linked).expect("the linked file");
        })
    });
    let stamps = trace
        .files()
        .map(|(_, stamp)| stamp.copied())
        .collect::<Vec<_>>();
    assert!(
        matches!(stamps.as_slice(), [Some(Stamp { own, target: Some(target) })]
            if own.kind == Kind::Link && target.kind == Kind::File),
        "{stamps:?}"
    );
    std::fs::write(root.join("file.new"), "contents").expect("the new file");
    std::fs::rename(root.join("file.new"), root.join("file")).expect("renamed over");
    assert_eq!(
        trace.stands(Now::at(0), Instant::now()),
        Err(Moved::File(linked.clone())),
        "a linked file renamed over left the link's read standing"
    );
}

/// A name read as itself ([`fs::opening_itself`], for a reader that opens the name and refuses a
/// link there) records the name's own stamp alone, and its replay looks the same way: a link
/// there whose target is rewritten moves nothing, while the link replaced by a file moves it, as
/// a read that follows the link ([`fs::opening`]) is moved by the rewrite. `private_file`'s reads
/// record so: one of a link in a private folder is refused, and its trace never looked at the
/// link's target.
///
/// NEGATIVE CONTROLS (2026-11-16), each run alone and then restored:
/// - `fs::opening_itself` looking as `Reach::Followed` failed with "itself recorded the link's
///   target, or no stamp of the link", the stamp `Stamp { own: Meta { kind: Link, .. }, target:
///   Some(Meta { kind: File, .. }) }`.
/// - `private_file::open_in` recording through `trace::fs::opening` failed with "read_private
///   recorded the link's target, or no stamp of the link", the same stamp of `.../state/key`.
#[cfg(unix)]
#[test]
fn a_name_read_as_itself_records_its_own_stamp_and_never_a_links_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path();
    let target = root.join("target");
    std::fs::write(&target, "one").expect("the target");
    let link = root.join("link");
    symlink(&target, &link).expect("a link to the target");
    let state = root.join("state");
    std::fs::create_dir(&state).expect("the state folder");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("a private folder");
    let key = state.join("key");
    symlink(&target, &key).expect("a link in the private folder");
    let later = Duration::from_secs(60);
    let stands = |trace: &Trace| trace.stands(Now::at(0), Instant::now());

    let ((), itself) = read_as_if_later(later, || scope(|| fs::opening_itself(&link)));
    let ((), followed) = read_as_if_later(later, || scope(|| fs::opening(&link)));
    let (answer, refused) = read_as_if_later(later, || {
        scope(|| crate::private_file::read_private(&state, "key", PRIMITIVE_MAX))
    });
    assert!(
        matches!(answer, Err(crate::private_file::PrivateFileError::Symlink)),
        "{answer:?}"
    );
    for (what, trace, path) in [("itself", &itself, &link), ("read_private", &refused, &key)] {
        let stamps = trace
            .files()
            .map(|(at, stamp)| (at.to_path_buf(), stamp.copied()))
            .collect::<Vec<_>>();
        assert!(
            matches!(stamps.as_slice(), [(at, Some(Stamp { own, target: None }))]
                if at == path && own.kind == Kind::Link),
            "{what} recorded the link's target, or no stamp of the link: {stamps:?}"
        );
        assert_eq!(trace.unkept(), None, "{what}: {trace:?}");
        assert_eq!(stands(trace), Ok(()), "{what}");
    }

    std::fs::write(&target, "a rewrite at another length").expect("the target rewritten");
    assert_eq!(
        stands(&followed),
        Err(Moved::File(link.clone())),
        "CONTROL: a read that follows the link is moved by its target's rewrite"
    );
    assert_eq!(
        stands(&itself),
        Ok(()),
        "a link's target rewritten moved a name read as itself"
    );
    assert_eq!(
        stands(&refused),
        Ok(()),
        "a link's target rewritten moved a private read that refused the link"
    );

    std::fs::remove_file(&link).expect("the link removed");
    std::fs::write(&link, "a file now").expect("a file in its place");
    assert_eq!(
        stands(&itself),
        Err(Moved::File(link.clone())),
        "the link replaced by a file left a name read as itself standing"
    );
}

/// The bell calls each listener, on the ringing thread, until its listening drops; this
/// thread's rings after the drop reach it no more. Rings from other tests' threads are left
/// out by the listener itself.
///
/// NEGATIVE CONTROL (2026-11-14): `Listening::drop` removing nothing failed with "a listener
/// heard this thread's ring after its listening dropped", left `2`, right `1`. Restored.
#[test]
fn the_bell_calls_each_listener_until_its_listening_drops() {
    let heard = Arc::new(AtomicU64::new(0));
    let me = std::thread::current().id();
    let listening = listen({
        let heard = Arc::clone(&heard);
        move || {
            if std::thread::current().id() == me {
                heard.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let rings_before = rings();
    bell();
    assert_eq!(
        heard.load(Ordering::SeqCst),
        1,
        "a listener missed this thread's ring"
    );
    assert!(rings().wrapping_sub(rings_before) >= 1);
    drop(listening);
    bell();
    assert_eq!(
        heard.load(Ordering::SeqCst),
        1,
        "a listener heard this thread's ring after its listening dropped"
    );
}

/// A whole-file write records a write effect in the scope armed on its thread, through
/// `atomic_file` and `private_file` alike; a scope that only read records none.
///
/// NEGATIVE CONTROL (2026-11-14): `atomic_file`'s `temp_component` without its effect failed
/// with "an atomic write recorded no write effect", left `[]`, right `[Write]`. Restored.
#[test]
fn a_whole_file_write_records_a_write_effect() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ((), wrote) = scope(|| {
        crate::atomic_file::write(&dir.path().join("written"), b"bytes", 0o600).expect("the write");
    });
    assert_eq!(
        wrote.effects().collect::<Vec<_>>(),
        [Effect::Write],
        "an atomic write recorded no write effect"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("a private folder");
        let ((), private) = scope(|| {
            crate::private_file::write_private(dir.path(), "private", b"secret")
                .expect("the private write");
        });
        assert_eq!(
            private.effects().collect::<Vec<_>>(),
            [Effect::Write],
            "a private write recorded no write effect"
        );
    }
    let ((), quiet) = scope(|| {
        let _ = std::fs::read(dir.path().join("written"));
    });
    assert_eq!(quiet.effects().count(), 0);
}

/// The most a read primitive in [`each_read_primitive_records_its_path_and_a_miss`] reads.
const PRIMITIVE_MAX: usize = 4096;

/// A read primitive's answer: the bytes it read, or its error as text.
type Answer = Result<Vec<u8>, String>;

/// A shared read primitive by name, whether it reads under `private_file`'s rules (so its trace
/// holds its folder's verdict too), and the primitive, run on the file `name` in a folder.
type Primitive = (&'static str, bool, fn(&Path, &str) -> Answer);

/// An I/O error as text, its kind first.
fn io_text(error: io::Error) -> String {
    format!("{:?}: {error}", error.kind())
}

/// Everything left to read in `file`.
fn rest_of(mut file: std::fs::File) -> Answer {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(io_text)?;
    Ok(bytes)
}

/// Every shared read primitive: `atomic_file`'s, `regular_file`'s and each of `private_file`'s,
/// whose reads all reach one opening (those on Windows too, and on Unix its owned reads, its
/// reads into a buffer and its kept parse).
fn read_primitives() -> Vec<Primitive> {
    let mut primitives: Vec<Primitive> = vec![
        ("atomic_file::read_regular", false, |dir, name| {
            crate::atomic_file::read_regular(&dir.join(name), PRIMITIVE_MAX as u64)
                .map(|(bytes, _)| bytes)
                .map_err(io_text)
        }),
        ("atomic_file::read_regular_text", false, |dir, name| {
            crate::atomic_file::read_regular_text(&dir.join(name), PRIMITIVE_MAX as u64)
                .map(String::into_bytes)
                .map_err(io_text)
        }),
        ("atomic_file::read_bounded_stream", false, |dir, name| {
            crate::atomic_file::read_bounded_stream(&dir.join(name), PRIMITIVE_MAX as u64)
                .map_err(io_text)
        }),
        ("regular_file::open", false, |dir, name| {
            let (file, _) = crate::regular_file::open(&dir.join(name)).map_err(io_text)?;
            rest_of(file)
        }),
    ];
    #[cfg(any(unix, windows))]
    {
        use crate::private_file::{self as private, JudgedDir};
        let private_primitives: [Primitive; 6] = [
            ("private_file::read_private", true, |dir, name| {
                private::read_private(dir, name, PRIMITIVE_MAX).map_err(|e| e.to_string())
            }),
            (
                "private_file::read_private_with_metadata",
                true,
                |dir, name| {
                    private::read_private_with_metadata(dir, name, PRIMITIVE_MAX)
                        .map(|(bytes, _)| bytes)
                        .map_err(|e| e.to_string())
                },
            ),
            ("private_file::open_private", true, |dir, name| {
                let (file, _) = private::open_private(dir, name).map_err(|e| e.to_string())?;
                rest_of(file)
            }),
            ("private_file::JudgedDir::read", true, |dir, name| {
                JudgedDir::judge(dir)
                    .and_then(|judged| judged.read(name, PRIMITIVE_MAX))
                    .map_err(|e| e.to_string())
            }),
            (
                "private_file::JudgedDir::read_with_metadata",
                true,
                |dir, name| {
                    JudgedDir::judge(dir)
                        .and_then(|judged| judged.read_with_metadata(name, PRIMITIVE_MAX))
                        .map(|(bytes, _)| bytes)
                        .map_err(|e| e.to_string())
                },
            ),
            ("private_file::JudgedDir::read_head", true, |dir, name| {
                JudgedDir::judge(dir)
                    .and_then(|judged| judged.read_head(name, PRIMITIVE_MAX))
                    .map_err(|e| e.to_string())
            }),
        ];
        primitives.extend(private_primitives);
    }
    #[cfg(unix)]
    {
        use crate::private_file::{self as private, KeptParse};
        let unix_primitives: [Primitive; 4] = [
            ("private_file::read_private_into", true, |dir, name| {
                let mut buf = [0; PRIMITIVE_MAX];
                private::read_private_into(dir, name, PRIMITIVE_MAX, &mut buf)
                    .map(|read| buf[..read.len].to_vec())
                    .map_err(|e| e.to_string())
            }),
            ("private_file::read_owned", true, |dir, name| {
                private::read_owned(dir, name, PRIMITIVE_MAX).map_err(|e| e.to_string())
            }),
            ("private_file::read_owned_into", true, |dir, name| {
                let mut buf = [0; PRIMITIVE_MAX];
                private::read_owned_into(dir, name, PRIMITIVE_MAX, &mut buf)
                    .map(|read| buf[..read.len].to_vec())
                    .map_err(|e| e.to_string())
            }),
            ("private_file::KeptParse::read", true, |dir, name| {
                KeptParse::new(1)
                    .read(dir, name, PRIMITIVE_MAX, <[u8]>::to_vec)
                    .map_err(|e| e.to_string())
            }),
        ];
        primitives.extend(unix_primitives);
    }
    primitives
}

/// Each file a trace read, and whether it found something there.
fn found(trace: &Trace) -> Vec<(PathBuf, bool)> {
    trace
        .files()
        .map(|(path, stamp)| (path.to_path_buf(), stamp.is_some()))
        .collect()
}

/// Make `name` in `dir` holding `bytes`, the owner's alone: on Unix of mode 0600, on Windows
/// under `private_file`'s owner-only access list, given at its creation.
fn private_fixture(dir: &Path, name: &str, bytes: &[u8]) {
    #[cfg(windows)]
    crate::private_file::write_private(dir, name, bytes).expect("a private fixture");
    #[cfg(not(windows))]
    {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("a fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("the fixture made private");
        }
    }
}

/// Each shared read primitive, with a scope armed, answers as it does with none and records the
/// file it read: by its stamp, with nothing else to keep the run from being kept, so the trace
/// stands; and as missing where there is nothing, which is no retry, so a file made there ends
/// the trace. Each of `private_file`'s records its folder's verdict as well, so the folder
/// opened to others ends the trace while the file stands, and a file written beside moves
/// nothing. A read of a stream poisons its scope.
///
/// NEGATIVE CONTROLS (2026-11-14), each run alone and then restored:
/// - `atomic_file::read_regular` without its `trace::fs::opening` call failed with
///   "atomic_file::read_regular recorded no stamp of its path", left `[]`, right
///   `[(".../state/file", true)]`.
/// - `regular_file::open` without its `trace::fs::opening` call failed with
///   "regular_file::open recorded no stamp of its path", left `[]`, right
///   `[(".../state/file", true)]`.
/// - `private_file::open_in` without its `trace::fs::opening_itself` call (2026-11-16; before it,
///   its `trace::fs::opening` call, 2026-11-14) failed with "private_file::read_private recorded
///   no stamp of its path", left `[]`, right `[(".../state/file", true)]`.
/// - `private_file::dir_is_read` recording no standing failed with
///   "private_file::read_private recorded no standing of its folder", left `[]`, right
///   `[".../state"]`.
/// - `DirIs::standing` giving an open folder a private one's standing failed with
///   "private_file::read_private: its folder opened to others left the trace standing", left
///   `Ok(())`, right `Err(Standing(".../state"))`.
/// - `atomic_file::read_bounded_stream` without its poison failed with "a read of /dev/null, a
///   stream, left its scope unpoisoned: Trace { .. }".
#[test]
fn each_read_primitive_records_its_path_and_a_miss() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dir = root.path().join("state");
    #[cfg(windows)]
    crate::private_file::create_private_dir(&dir).expect("the private state folder");
    #[cfg(not(windows))]
    std::fs::create_dir(&dir).expect("the state folder");
    #[cfg(unix)]
    let set_mode = |mode: u32| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))
            .expect("the state folder's mode");
    };
    #[cfg(unix)]
    set_mode(0o700);
    private_fixture(&dir, "file", b"contents");
    let file = dir.join("file");
    let gone = dir.join("gone");
    let stands = |trace: &Trace| trace.stands(Now::at(0), Instant::now());
    let later = Duration::from_secs(60);

    for (what, private, read) in read_primitives() {
        let (answer, trace) = read_as_if_later(later, || scope(|| read(&dir, "file")));
        assert_eq!(answer, Ok(b"contents".to_vec()), "{what}");
        assert_eq!(answer, read(&dir, "file"), "{what}, with no scope armed");
        assert_eq!(
            found(&trace),
            [(file.clone(), true)],
            "{what} recorded no stamp of its path"
        );
        assert_eq!(trace.unkept(), None, "{what}: {trace:?}");
        let folders: &[&Path] = if private { &[dir.as_path()] } else { &[] };
        assert_eq!(
            trace.standings().collect::<Vec<_>>(),
            folders,
            "{what} recorded no standing of its folder"
        );
        assert_eq!(stands(&trace), Ok(()), "{what}");
        private_fixture(&dir, "beside", b"a file written beside");
        assert_eq!(
            stands(&trace),
            Ok(()),
            "{what}: a file written beside moved the trace"
        );
        std::fs::remove_file(dir.join("beside")).expect("the file beside gone");
        #[cfg(unix)]
        if private {
            set_mode(0o755);
            assert_eq!(
                stands(&trace),
                Err(Moved::Standing(dir.clone())),
                "{what}: its folder opened to others left the trace standing"
            );
            set_mode(0o700);
            assert_eq!(stands(&trace), Ok(()), "{what}: the folder private again");
        }

        let (answer, missed) = scope(|| read(&dir, "gone"));
        assert!(answer.is_err(), "{what}: {answer:?}");
        assert_eq!(answer, read(&dir, "gone"), "{what}, with no scope armed");
        assert_eq!(
            found(&missed),
            [(gone.clone(), false)],
            "{what} recorded no miss of its path"
        );
        assert_eq!(
            missed.unkept(),
            None,
            "{what}: a miss was not kept: {missed:?}"
        );
        assert_eq!(stands(&missed), Ok(()), "{what}");
        private_fixture(&dir, "gone", b"made");
        assert_eq!(
            stands(&missed),
            Err(Moved::File(gone.clone())),
            "{what}: a file made where its read found none left the trace standing"
        );
        std::fs::remove_file(&gone).expect("the made file gone");
    }

    #[cfg(unix)]
    {
        let (answer, streamed) =
            scope(|| crate::atomic_file::read_bounded_stream(Path::new("/dev/null"), 16));
        assert_eq!(answer.expect("/dev/null"), Vec::<u8>::new());
        assert!(
            matches!(streamed.unkept(), Some(Unkept::Poison(_))),
            "a read of /dev/null, a stream, left its scope unpoisoned: {streamed:?}"
        );
    }
}

/// What [`a_read_made_as_its_thread_exits_answers_and_records_nothing`]'s thread-local tells as
/// its destructor runs: what a read answered, and whether a scope was armed.
struct Late {
    /// Where it tells.
    told: std::sync::mpsc::Sender<(Answer, bool)>,
    /// The file it reads.
    file: PathBuf,
}

impl Drop for Late {
    fn drop(&mut self) {
        let answer = crate::atomic_file::read_regular(&self.file, PRIMITIVE_MAX as u64)
            .map(|(bytes, _)| bytes)
            .map_err(io_text);
        effect(Effect::Write);
        let _ = self.told.send((answer, armed()));
    }
}

std::thread_local! {
    /// The thread-local whose destructor reads as its thread exits.
    static LATE: RefCell<Option<Late>> = const { RefCell::new(None) };
}

/// A read and an effect made from a thread-local's destructor, after its thread's scopes were
/// torn down as it exited, answer as they would and record nothing, and nothing panics: every
/// shared reader and writer asks whether a scope is armed, wherever it runs. The thread-local
/// is set before the thread's scopes are first touched, so its destructor runs after theirs.
///
/// NEGATIVE CONTROL (2026-11-14): `armed` and `record` reaching the scopes by `with_borrow`
/// and `with_borrow_mut` instead of `try_with`, as before, aborted the test binary with
/// "fatal runtime error: thread local panicked on drop, aborting" (SIGTRAP). Restored.
#[test]
fn a_read_made_as_its_thread_exits_answers_and_records_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("late");
    std::fs::write(&file, "late").expect("the fixture");
    let (told, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        LATE.set(Some(Late { told, file }));
        let ((), trace) = scope(|| ());
        assert_eq!(trace.unkept(), None);
    })
    .join()
    .expect("the thread exited");
    let (answer, armed) = heard
        .recv_timeout(Duration::from_secs(60))
        .expect("the destructor told");
    assert_eq!(answer, Ok(b"late".to_vec()));
    assert!(
        !armed,
        "a scope was armed in a destructor after the scopes were torn down"
    );
}

/// A resolved path stands while it resolves to the same place: what it names may be rewritten,
/// but a link along it pointed elsewhere ends it, and the trace stands again once the link is
/// back. A path that resolved to nothing stands until it resolves, and records no retry, which
/// only a failure of another kind does.
///
/// NEGATIVE CONTROL (2026-11-15): `fs::canonicalize` recording no standing failed at the
/// standings' record, left `[]`, right `[".../current/accounts.toml",
/// ".../absent/accounts.toml"]`, before any link moved. Restored.
#[cfg(unix)]
#[test]
fn a_resolved_path_stands_until_a_link_along_it_points_elsewhere() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    for side in ["a", "b"] {
        std::fs::create_dir(dir.path().join(side)).expect("a side");
        std::fs::write(dir.path().join(side).join("accounts.toml"), side).expect("its roster");
    }
    let current = dir.path().join("current");
    std::os::unix::fs::symlink("a", &current).expect("the link");
    let roster = current.join("accounts.toml");
    let absent = dir.path().join("absent").join("accounts.toml");

    let ((resolved, missing), trace) =
        scope(|| (fs::canonicalize(&roster), fs::canonicalize(&absent)));
    let real = std::fs::canonicalize(dir.path().join("a").join("accounts.toml")).expect("real");
    assert_eq!(resolved.expect("resolved"), real);
    assert_eq!(
        missing.map_err(|error| error.kind()),
        Err(io::ErrorKind::NotFound)
    );
    assert_eq!(trace.unkept(), None, "{trace:?}");
    assert_eq!(
        trace.standings().collect::<Vec<_>>(),
        [roster.as_path(), absent.as_path()]
    );
    let stands = || trace.stands(Now::at(0), Instant::now());
    assert_eq!(stands(), Ok(()));

    std::fs::write(&real, "a, rewritten").expect("a rewrite of what it names");
    assert_eq!(stands(), Ok(()), "a rewrite moved a resolved path");

    std::fs::remove_file(&current).expect("the link gone");
    std::os::unix::fs::symlink("b", &current).expect("the link elsewhere");
    assert_eq!(
        stands(),
        Err(Moved::Standing(roster.clone())),
        "a link along a resolved path pointed elsewhere left the trace standing"
    );
    std::fs::remove_file(&current).expect("the link gone");
    std::os::unix::fs::symlink("a", &current).expect("the link back");
    assert_eq!(stands(), Ok(()), "the link back did not stand");

    std::fs::create_dir(dir.path().join("absent")).expect("the missing folder");
    std::fs::write(&absent, "").expect("the missing roster");
    assert_eq!(stands(), Err(Moved::Standing(absent.clone())));
}

/// A trace not kept still says whether what it read and compared moved ([`Trace::moved`]): its
/// owner keeps a run that did something, and runs it again for an input that moved apart from the
/// rerun it owes. [`Trace::stands`] answers the keeping first, then the same.
///
/// NEGATIVE CONTROL (2026-11-16): `Trace::moved` answering `Err(Moved::Unkept(..))` first, as
/// `stands` does, failed at "a run not kept whose reads stand", left `Err(Unkept(Effect(Write)))`,
/// right `Ok(())`. Restored, the file touched.
#[test]
fn a_trace_not_kept_still_says_whether_what_it_read_moved() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("roster");
    std::fs::write(&file, "label = \"a\"\n").expect("the fixture's roster");
    let ((), wrote) = read_as_if_later(Duration::from_secs(60), || {
        scope(|| {
            fs::read_to_string(&file).expect("the roster");
            let now = Now::at(1_000);
            assert!(now.before(1_500));
            effect(Effect::Write);
        })
    });
    let mono = Instant::now();
    assert_eq!(
        wrote.stands(Now::at(1_000), mono),
        Err(Moved::Unkept(Unkept::Effect(Effect::Write)))
    );
    assert_eq!(
        wrote.moved(Now::at(1_000), mono),
        Ok(()),
        "a run not kept whose reads stand"
    );
    assert_eq!(wrote.moved(Now::at(1_500), mono), Err(Moved::Wall));
    std::fs::write(&file, "label = \"b\", moved\n").expect("a rewrite");
    assert_eq!(
        wrote.moved(Now::at(1_000), mono),
        Err(Moved::File(file.clone()))
    );
}

/// Each traced read that opens a file counts one open ([`fs::opened`]), whether it reads through
/// this module or says so through [`fs::opening`]; a stamp alone, a listing, a replay and a read
/// with no scope armed count none.
///
/// NEGATIVE CONTROL (2026-11-16): `fs::read_to_string` taking its stamp through `looked` instead
/// of `looked_to_open` failed with left `2`, right `3`. Restored, the file touched.
#[test]
fn each_traced_read_that_opens_a_file_counts_one_open() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("roster");
    std::fs::write(&file, "label = \"a\"\n").expect("the fixture's roster");
    let before = fs::opened_here();
    let _ = fs::read_to_string(&file).expect("an unarmed read");
    assert_eq!(fs::opened_here(), before, "a read with no scope armed");
    let ((), trace) = scope(|| {
        let _ = fs::read_to_string(&file).expect("the roster");
        let _ = fs::read(&file).expect("the roster's bytes");
        fs::opening(&file);
        let _ = fs::symlink_metadata(&file).expect("a stamp");
        let _ = fs::read_dir(dir.path()).expect("a listing");
    });
    assert_eq!(fs::opened_here() - before, 3);
    let _ = trace.stands(Now::at(0), Instant::now());
    assert_eq!(fs::opened_here() - before, 3, "a replay opens nothing");
    assert!(fs::opened() >= 3);
}
