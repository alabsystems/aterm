// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The package lane's one wait ([`lane_wait`]): each look the lane owes sets a deadline, and the
//! wait is the earliest of them, capped at a blind look only where nothing rings the lane at a
//! sleep or no file watch stands.

use super::*;
use atpkg::vendor_direct::watch::{BLIND_LOOK, ConcurrentVendorGetFn, HeadWatch, Rest};
use std::time::SystemTime;

const HOUR: Duration = Duration::from_secs(3600);

fn store(label: &str) -> atpkg::store::Layout {
    let prefix =
        std::env::temp_dir().join(format!("aterm-lane-wait-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&prefix);
    std::fs::create_dir_all(&prefix).unwrap();
    atpkg::store::Layout { prefix }
}

/// The lane's wait from a one-hour park at `mono`.
fn wait(
    l: &atpkg::store::Layout,
    watch: &BumpWatch,
    probes: ParkProbes,
    heads: Option<&HeadWatch>,
    rest: &Rest,
    mono: Instant,
) -> Duration {
    lane_wait(
        HOUR,
        Some(l),
        watch,
        probes,
        heads,
        rest,
        (SystemTime::now(), mono),
    )
}

fn ago(mono: Instant, secs: u64) -> Instant {
    mono.checked_sub(Duration::from_secs(secs))
        .expect("the monotonic clock has run that long")
}

/// AN IDLE WINDOW THAT HEARS ITS SLEEPS WAITS UNTIL ITS NEXT LOOK, NOT A BLIND LOOK. With a rest
/// that watches its files and hears sleeps, a window whose index was probed ten seconds ago waits
/// the rest of the probe's interval; with no look owed it waits out its park; the first probe and a
/// due head end the wait at once. A rest that watches no file, or one that hears no sleep while
/// nothing else rings the lane at a wake, caps the wait at a blind look, as on main. MUTANTS: no
/// cap (the inert rest waits the interval); a cap only where both fail (`&&`: the rest that hears
/// no sleep waits the interval); no index row (the heard window waits out its park).
#[test]
fn an_idle_window_that_hears_its_sleeps_waits_until_its_next_look() {
    let l = store("idle");
    let mono = Instant::now();
    let heard = Rest::by_hand();
    let mut watch = BumpWatch::default();
    assert_eq!(
        wait(&l, &watch, ParkProbes::INDEX, None, &heard, mono),
        Duration::ZERO,
        "the first index probe is owed now"
    );
    watch.last_index_probe = Some(ago(mono, 10));
    assert_eq!(
        wait(&l, &watch, ParkProbes::INDEX, None, &heard, mono),
        atpkg::index_probe::INTERVAL - Duration::from_secs(10),
        "the next probe's deadline"
    );
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &heard, mono),
        HOUR,
        "no look owed: the park's own deadline"
    );
    let fresh = HeadWatch::new(&[]);
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, Some(&fresh), &heard, mono),
        Duration::ZERO,
        "heads never asked are due now"
    );

    assert!(
        !crate::watchdog::wall_heard(),
        "no sampler runs in a unit test"
    );
    assert_eq!(
        wait(&l, &watch, ParkProbes::INDEX, None, &Rest::inert(), mono),
        BLIND_LOOK,
        "no file watch: a blind look"
    );
    let deaf = Rest::start("lane-wait-deaf", [l.bump_file()]);
    assert!(deaf.watching(), "the scratch store's folder is watched");
    assert_eq!(
        wait(&l, &watch, ParkProbes::INDEX, None, &deaf, mono),
        BLIND_LOOK,
        "a rest no wake rings: a blind look"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A HELD MOVE IS LOOKED AT ON ITS RE-CHECK, AND A BLIND LOOK AFTER A REFUSED SIT. While a held
/// move's record stands, the wait ends when the quiet re-check next looks (at once before its first
/// look). A window whose last sit was refused, because a session's round held the rendezvous, looks
/// again a blind look later, as main's slice did, never only at the re-check's minute. With no record
/// the held move owes nothing. MUTANTS: a refused sit waiting the re-check (the window learns of a
/// due move up to a minute late); no record row (the window waits out its park).
#[test]
fn a_held_move_is_looked_at_on_its_recheck_and_a_blind_look_after_a_refused_sit() {
    let l = store("held");
    let mono = Instant::now();
    let rest = Rest::by_hand();
    let mut watch = BumpWatch::default();
    std::fs::write(atpkg::quiet::record_path(&l), b"").unwrap();
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        Duration::ZERO,
        "never looked at: due now"
    );
    assert!(
        !watch.flip_recheck.wake(mono, || None),
        "the look finds nothing due"
    );
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        atpkg::quiet::RECHECK,
        "the next look is the re-check's"
    );
    watch.flip_sit_refused = true;
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        BLIND_LOOK,
        "a refused sit is tried again a blind look later"
    );
    std::fs::remove_file(atpkg::quiet::record_path(&l)).unwrap();
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        HOUR,
        "no held move: nothing owed"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A BUMP THE FLOOR HOLDS BACK ENDS THE WAIT AT THE FLOOR. A bump written a minute after the last
/// bump-triggered pass is held to [`BUMP_FLOOR`] past that pass, and the wait ends there with no
/// look between; once acted on, the bump owes nothing. MUTANT: no bump row (the held bump runs only
/// at the park's deadline).
#[test]
fn a_bump_the_floor_holds_back_ends_the_wait_at_the_floor() {
    let l = store("bump");
    let mono = Instant::now();
    let rest = Rest::by_hand();
    std::fs::write(l.bump_file(), b"").unwrap();
    let mut watch = BumpWatch {
        last_trigger: Some(ago(mono, 60)),
        ..BumpWatch::default()
    };
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        BUMP_FLOOR - Duration::from_secs(60)
    );
    watch.acted_mtime = Some(
        std::fs::metadata(l.bump_file())
            .unwrap()
            .modified()
            .unwrap(),
    );
    assert_eq!(
        wait(&l, &watch, ParkProbes::NONE, None, &rest, mono),
        HOUR,
        "a bump acted on owes nothing"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A HEAD ROUND THAT ENDED IS LOOKED AT AT ONCE, WHOEVER TOOK ITS RING. The lane's stand-by while
/// Automatic updates are off, or a park that ended on a bump, a held move or an index, takes the
/// ring a head round's end made without reaching the round; the lane's next wait is then zero, and
/// its round harvests the moved head, rather than at the end of the park. MUTANT: a
/// HeadWatch::next_due that waits on every round in flight (the window waits out its hour).
#[test]
fn a_head_round_that_ended_is_looked_at_whoever_took_its_ring() {
    let l = store("round-ended");
    let build = l.build_dir("claude", 2_026_092_201);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    atpkg::atomic_symlink(&build, &l.program_current("claude")).unwrap();
    let rest = Rest::by_hand();
    let mut heads = HeadWatch::new(&[]);
    heads.bind_ring(rest.ringer());
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_: &str, url: &str, _: u64, _: Option<&str>| {
            Ok::<_, atpkg::VendorFetchError>(atpkg::VendorGet::Body {
                bytes: b"2.1.281".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
    assert_eq!(
        heads.round(&l, SystemTime::now(), &get, false),
        None,
        "the round starts"
    );
    assert!(
        rest.wait(Some(aterm_spec::wait::MUST_HAPPEN))
            .is_some_and(|told| told.rung),
        "the round rang its end (a hang detector), and a stand-by took the ring"
    );
    assert_eq!(
        wait(
            &l,
            &BumpWatch::default(),
            ParkProbes::NONE,
            Some(&heads),
            &rest,
            Instant::now()
        ),
        Duration::ZERO,
        "the ended round is the lane's next look"
    );
    assert_eq!(
        heads.round(&l, SystemTime::now(), &get, false),
        Some(vec!["claude"]),
        "and that look harvests it"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
