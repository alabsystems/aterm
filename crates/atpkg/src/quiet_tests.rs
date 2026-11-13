// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-quiet-{label}-"))
}

const T0: i64 = 1_790_000_000;

#[test]
fn a_person_a_revocation_or_quiet_flips_at_once() {
    for (policy, forced, quiet) in [
        (FlipPolicy::Now, false, false),
        (FlipPolicy::WhenQuiet, true, false),
        (FlipPolicy::WhenQuiet, false, true),
    ] {
        assert_eq!(
            decide(policy, forced, quiet, Some(T0 - 60), T0),
            Verdict::Flip,
            "{policy:?} forced={forced} quiet={quiet}"
        );
    }
}

#[test]
fn a_busy_toolchain_waits_up_to_the_ceiling_counted_from_the_first_deferral() {
    let wait = |since, now| decide(FlipPolicy::WhenQuiet, false, false, since, now);
    assert_eq!(wait(None, T0), Verdict::Defer { since: T0 });
    assert_eq!(
        wait(Some(T0), T0 + CEILING_SECS - 1),
        Verdict::Defer { since: T0 }
    );
    assert_eq!(
        wait(Some(T0), T0 + CEILING_SECS),
        Verdict::FlipAtCeiling { since: T0 }
    );
    assert_eq!(wait(Some(T0 + 3600), T0), Verdict::Defer { since: T0 });
}

#[test]
fn the_record_round_trips_and_keeps_its_first_since() {
    let l = layout("record");
    assert_eq!(read(&l), None);
    let from = BTreeMap::from([("trust".to_string(), 9192u64)]);
    let to = BTreeMap::from([("trust".to_string(), 9200u64)]);
    let busy = Busy::Running {
        exe: PathBuf::from("/x/targo"),
    };
    assert!(note_deferred(
        &l,
        "rustc",
        T0,
        T0,
        &busy,
        &Moves {
            from: from.clone(),
            to: to.clone(),
            assets: BTreeMap::new(),
        },
    ));
    let d = read(&l).unwrap();
    assert_eq!((d.since, d.checked, d.group.as_str()), (T0, T0, "rustc"));
    assert_eq!(d.from, from);
    assert_eq!(since_of(&l, "rustc"), Some(T0));
    assert_eq!(since_of(&l, "other"), None);
    clear(&l);
    assert_eq!(read(&l), None);
    std::fs::write(record_path(&l), b"schema = \"x\"\n").unwrap();
    assert_eq!(read(&l), None);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// Lay a complete trust build and return its `bin/targo`.
fn seed_build(l: &Layout, program: &str, build: u64) -> PathBuf {
    let bin = l.build_dir(program, build).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("targo");
    std::fs::write(&exe, b"#!/bin/true\n").unwrap();
    exe
}

/// The probe, every arm: a lease on a member's live build or the laid view, a process in
/// the build, its exec root or the view, an unreadable table, and quiet.
#[test]
fn the_probe_reads_leases_then_the_process_table() {
    let l = layout("probe");
    let exe = seed_build(&l, "trust", 9192);
    seed_build(&l, "trust-ir", 40);
    let live = BTreeMap::from([("trust".to_string(), 9192u64), ("trust-ir".to_string(), 40)]);
    let empty = || Some(Vec::new());
    assert_eq!(probe(&l, &live, &empty), Busy::Quiet);
    assert!(matches!(probe(&l, &live, &|| None), Busy::Unknown { .. }));
    assert_eq!(
        probe(&l, &live, &|| Some(vec![exe.clone()])),
        Busy::Running { exe: exe.clone() }
    );
    let root_exe = crate::compat::root_dir(&l, 9192).join("bin/trustc");
    std::fs::create_dir_all(root_exe.parent().unwrap()).unwrap();
    assert!(matches!(
        probe(&l, &live, &|| Some(vec![root_exe.clone()])),
        Busy::Running { .. }
    ));
    let other = seed_build(&l, "trust", 9100);
    assert_eq!(probe(&l, &live, &|| Some(vec![other.clone()])), Busy::Quiet);
    let lease = crate::lease::take(
        &l.prefix,
        &crate::lease::Subject::build("trust-ir", 40).unwrap(),
        "aterm-release cut",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let busy = probe(&l, &live, &|| -> Option<Vec<PathBuf>> {
        panic!("a lease answers before the table is read")
    });
    assert!(matches!(busy, Busy::Leased { .. }), "{busy:?}");
    lease.release();
    let view = crate::seam::view_dir(&l, "trust");
    std::fs::create_dir_all(view.join("bin")).unwrap();
    let in_view = view.join("bin/targo");
    assert!(matches!(
        probe(&l, &live, &|| Some(vec![in_view.clone()])),
        Busy::Running { .. }
    ));
    let lease = crate::lease::take(
        &l.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 7) \u{2014} held by `aterm pkg lease` in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    assert!(matches!(probe(&l, &live, &empty), Busy::Leased { .. }));
    lease.release();
    assert_eq!(probe(&l, &live, &empty), Busy::Quiet);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// [`decide`] and [`Busy::is_quiet`] against the derived model `AtpkgFlipQuiet` at every
/// reachable look: the model's `busy` maps onto a [`Busy`] (0 quiet, 1 a process running
/// from it, 2 an unreadable table), `forced` onto revocation, and `age` onto the clock in
/// periods of `CEILING_SECS / Ceiling`.
#[test]
fn the_flip_decision_refines_the_derived_model() {
    use aterm_models_atpkg::atpkg_flip_quiet_model;
    let model = atpkg_flip_quiet_model();
    let ceiling = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Ceiling")
        .map(|(_, v)| *v)
        .expect("the model names its ceiling");
    let period = CEILING_SECS / ceiling;
    assert_eq!(
        period * ceiling,
        CEILING_SECS,
        "the ceiling maps onto whole periods"
    );
    let mut seen = std::collections::BTreeSet::new();
    let mut frontier = vec![model.init_state()];
    while let Some(state) = frontier.pop() {
        if !seen.insert(state.clone()) {
            continue;
        }
        for action in &model.actions {
            let mut next = state.clone();
            if model.fire(action.name, &mut next) {
                frontier.push(next);
            }
        }
    }
    let looks: Vec<_> = seen
        .iter()
        .filter(|s| s["pending"] == 1 && s["looked"] == 0)
        .collect();
    assert!(
        looks.len() >= 16,
        "the explorer reached the looks: {looks:?}"
    );
    let busy_of = |b: i64| match b {
        0 => Busy::Quiet,
        1 => Busy::Running {
            exe: PathBuf::from("/x/targo"),
        },
        _ => Busy::Unknown {
            why: "the process table could not be read".to_string(),
        },
    };
    let modeled = |s: &std::collections::BTreeMap<&'static str, i64>| match ["Flip", "Hold"]
        .into_iter()
        .filter(|a| model.action_enabled(a, s))
        .collect::<Vec<_>>()[..]
    {
        ["Flip"] => true,
        ["Hold"] => false,
        ref other => panic!("exactly one of Flip/Hold is enabled at {s:?}: {other:?}"),
    };
    let flips = |v: Verdict| !matches!(v, Verdict::Defer { .. });
    let at = |s: &std::collections::BTreeMap<&'static str, i64>| T0 + s["age"] * period;
    for s in &looks {
        let real = decide(
            FlipPolicy::WhenQuiet,
            s["forced"] == 1,
            busy_of(s["busy"]).is_quiet(),
            Some(T0),
            at(s),
        );
        assert_eq!(
            flips(real),
            modeled(s),
            "the real decision at {s:?}: {real:?}"
        );
        if let Verdict::Defer { since } = real {
            assert_eq!(since, T0, "the wait counts from its first deferral");
        }
    }
    assert!(
        !Busy::Leased {
            what: "trust build 1".into(),
            holders: crate::lease::Holders::Held(vec!["x".into()]),
        }
        .is_quiet()
    );
    assert!(
        looks.iter().any(|s| {
            let mutant = decide(
                FlipPolicy::WhenQuiet,
                s["forced"] == 1,
                s["busy"] != 1,
                Some(T0),
                at(s),
            );
            flips(mutant) != modeled(s)
        }),
        "the comparison must be able to fail"
    );
}

/// A recorded deferral is due when the toolchain is quiet, or at the ceiling however
/// busy, keyed by its `checked` stamp; one whose builds are all gone is not.
#[test]
fn a_recorded_deferral_is_due_when_quiet_or_at_the_ceiling() {
    let l = layout("due");
    let exe = seed_build(&l, "trust", 9192);
    let busy = || Some(vec![exe.clone()]);
    assert_eq!(due_with(&l, T0, &busy), None, "nothing recorded");
    let from = BTreeMap::from([("trust".to_string(), 9192u64)]);
    let to = BTreeMap::from([("trust".to_string(), 9200u64)]);
    assert!(note_deferred(
        &l,
        "rustc",
        T0,
        T0 + 5,
        &Busy::Quiet,
        &Moves {
            from,
            to,
            assets: BTreeMap::new(),
        },
    ));
    assert_eq!(due_with(&l, T0 + 60, &busy), None);
    assert_eq!(due_with(&l, T0 + 60, &|| None), None, "unknown is busy");
    assert_eq!(
        due_with(&l, T0 + 60, &|| Some(Vec::new())),
        Some(DueFlip {
            why: Due::Quiet,
            checked: T0 + 5
        })
    );
    assert_eq!(
        due_with(&l, T0 + CEILING_SECS, &busy).map(|d| d.why),
        Some(Due::Ceiling)
    );
    std::fs::remove_dir_all(l.build_dir("trust", 9192)).unwrap();
    assert_eq!(
        due_with(&l, T0 + 60, &|| Some(Vec::new())),
        None,
        "a record that outlived the builds it names wakes nothing"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// [`Recheck`]: looked at once a minute; quiet wakes a pass only on two looks in a row
/// (a busy look between starts over), the ceiling at once, one wake per record look.
#[test]
fn the_recheck_wakes_on_two_quiet_looks_or_the_ceiling_once_per_look() {
    let look = |why, checked| move || Some(DueFlip { why, checked });
    let mut r = Recheck::default();
    let t0 = std::time::Instant::now();
    assert!(!r.wake(t0, look(Due::Quiet, 1)), "one quiet look");
    assert!(
        !r.wake(t0 + std::time::Duration::from_secs(5), || {
            panic!("no second look inside the minute")
        }),
        "not looked at again inside the minute"
    );
    assert!(
        r.wake(t0 + RECHECK, look(Due::Quiet, 1)),
        "the second quiet look in a row wakes the pass"
    );
    for n in 2..5 {
        assert!(!r.wake(t0 + n * RECHECK, look(Due::Quiet, 1)), "look {n}");
    }
    assert!(
        !r.wake(t0 + 5 * RECHECK, look(Due::Ceiling, 1)),
        "one wake per look of a record, even at the ceiling"
    );
    let t1 = t0 + 6 * RECHECK;
    assert!(!r.wake(t1, look(Due::Quiet, 2)));
    assert!(!r.wake(t1 + RECHECK, || None));
    assert!(
        !r.wake(t1 + 2 * RECHECK, look(Due::Quiet, 2)),
        "a busy look between two quiet ones starts the count over"
    );
    assert!(r.wake(t1 + 3 * RECHECK, look(Due::Quiet, 2)));
    assert!(!r.wake(t1 + 4 * RECHECK, look(Due::View, 2)));
    assert!(
        r.wake(t1 + 5 * RECHECK, look(Due::View, 2)),
        "a view's look is its own"
    );
    let mut fresh = Recheck::default();
    assert!(
        fresh.wake(t0, look(Due::Ceiling, 3)),
        "the ceiling wakes at once"
    );
}

/// A quiet look counts only for the move it looked at (flip or view) at that stamp: a
/// quiet view is not the first of the flip's two looks, nor the reverse, and a record
/// written again starts the count over.
#[test]
fn a_quiet_look_at_one_move_is_not_the_first_of_two_at_another() {
    let look = |why, checked| move || Some(DueFlip { why, checked });
    let t0 = std::time::Instant::now();
    let mut r = Recheck::default();
    assert!(!r.wake(t0, look(Due::View, 1)));
    assert!(
        !r.wake(t0 + RECHECK, look(Due::Quiet, 2)),
        "the flip's first quiet look"
    );
    assert!(
        r.wake(t0 + 2 * RECHECK, look(Due::Quiet, 2)),
        "its second wakes the pass"
    );
    let mut r = Recheck::default();
    assert!(!r.wake(t0, look(Due::Quiet, 1)));
    assert!(
        !r.wake(t0 + RECHECK, look(Due::View, 2)),
        "the view's first quiet look"
    );
    assert!(r.wake(t0 + 2 * RECHECK, look(Due::View, 2)));
    let mut r = Recheck::default();
    assert!(!r.wake(t0, look(Due::Quiet, 1)));
    assert!(
        !r.wake(t0 + RECHECK, look(Due::Quiet, 5)),
        "the record was written again"
    );
    assert!(r.wake(t0 + 2 * RECHECK, look(Due::Quiet, 5)));
}

/// The views-left record round-trips, keeps its first `since`, and goes when its last
/// view is cleared.
#[test]
fn the_views_left_record_round_trips_and_goes_with_its_last_view() {
    let l = layout("views-left");
    assert!(views_left(&l).views.is_empty());
    note_view_left(
        &l,
        "trust",
        std::path::Path::new("/p/store/trust/9200"),
        "it is leased",
        T0,
    );
    note_view_left(
        &l,
        "trust",
        std::path::Path::new("/p/store/trust/9201"),
        "it is leased",
        T0 + 60,
    );
    let v = views_left(&l);
    let trust = &v.views["trust"];
    assert_eq!(
        (trust.build.as_str(), trust.since, trust.checked),
        ("/p/store/trust/9201", T0, T0 + 60)
    );
    clear_view_left(&l, "trust");
    assert!(views_left(&l).views.is_empty());
    assert!(!view_record_path(&l).exists(), "no view, no file");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_woken_move_is_retired_when_another_pass_clears_or_renews_its_record() {
    let l = layout("woken-move-renewal");
    std::fs::create_dir_all(l.build_dir("trust", 1)).unwrap();
    let record = Deferral {
        schema: SCHEMA,
        group: "rustc".into(),
        checked: T0,
        from: [("trust".into(), 1)].into(),
        ..Deferral::default()
    };
    write(&l, &record).unwrap();
    let mut flip = Recheck::default();
    assert!(flip.wake(std::time::Instant::now(), || Some(DueFlip {
        why: Due::Ceiling,
        checked: T0,
    })));
    assert!(woken_move_still_stands(&l, &flip), "unchanged flip");
    write(
        &l,
        &Deferral {
            checked: T0 + 1,
            ..record.clone()
        },
    )
    .unwrap();
    assert!(!woken_move_still_stands(&l, &flip), "renewed flip");
    clear(&l);
    assert!(!woken_move_still_stands(&l, &flip), "completed flip");

    note_view_left(&l, "trust", std::path::Path::new("/tmp/trust"), "busy", T0);
    let mut view = Recheck::default();
    let t0 = std::time::Instant::now();
    assert!(!view.wake(t0, || Some(DueFlip {
        why: Due::View,
        checked: T0,
    })));
    assert!(view.wake(t0 + RECHECK, || Some(DueFlip {
        why: Due::View,
        checked: T0,
    })));
    assert!(woken_move_still_stands(&l, &view), "unchanged view");
    note_view_left(
        &l,
        "trust",
        std::path::Path::new("/tmp/trust"),
        "busy",
        T0 + 1,
    );
    assert!(!woken_move_still_stands(&l, &view), "renewed view");
    clear_view_left(&l, "trust");
    assert!(!woken_move_still_stands(&l, &view), "completed view");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// NEGATIVE CONTROL: `read` and `views_left` drop records with traversal/unsafe program or
/// seam names or control characters, while valid `rustc` / `trust` records round-trip cleanly.
#[test]
fn deferral_and_views_left_reject_traversal_keys_and_control_bytes() {
    let l = layout("quiet-validate");
    std::fs::write(
        record_path(&l),
        format!(
            "schema = {SCHEMA}\ngroup = \"rustc\"\nsince = 1\nchecked = 2\nwhy = \"busy\"\n\
             [from]\n\"../escape\" = 1\n"
        ),
    )
    .unwrap();
    assert_eq!(read(&l), None);

    std::fs::write(
        record_path(&l),
        format!(
            "schema = {SCHEMA}\ngroup = \"bad\\u0007\"\nsince = 1\nchecked = 2\nwhy = \"busy\"\n"
        ),
    )
    .unwrap();
    assert_eq!(read(&l), None);

    std::fs::write(
        view_record_path(&l),
        format!(
            "schema = {SCHEMA}\n\
             [views.\"../escape\"]\nbuild = \"/p/store/trust/1\"\nsince = 1\nchecked = 2\nwhy = \"busy\"\n\
             [views.trust]\nbuild = \"/p/store/trust/1\"\nsince = 1\nchecked = 2\nwhy = \"busy\"\n"
        ),
    )
    .unwrap();
    let views = views_left(&l);
    assert_eq!(views.views.keys().collect::<Vec<_>>(), ["trust"]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
