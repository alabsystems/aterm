// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Step 1's host fitness ([`crate::release`]'s `Run::host_fits`) against the fake world: a Mac
//! that cannot hold the release takes no line by any way a line is taken, and a line it already
//! holds still resumes.

use super::{BUMP, FIX, NEXT, TIP, World, bump, fix, mac, red_line};
use crate::line::Line;
use crate::release::{Refusal, ReleaseOptions};

fn unfit() -> Refusal {
    Refusal::new(
        "this Mac cannot hold the release whose line it would take — the heaviest compiler unit \
         recorded here (aterm_gui lib): needs 32.8 GiB and 8 GiB spare, this Mac can never have \
         more than 29.4 GiB of its 32 GiB of memory available (2.6 GiB of it is wired). The \
         host: 32 GiB of memory (29.4 GiB of it can ever be available), 12 cores",
        "start or adopt the line on a Mac that fits, or leave it to the pump on the cut host",
    )
}

/// A MAC THAT CANNOT HOLD THE RELEASE TAKES NO LINE. Every way a run takes a line (a bump, a
/// commit, main's tip with no journal) refuses before its first write: nothing is bumped, no
/// candidate or journal is written, the fleet claim is not asked for, and the final line names
/// the claim, the shortfall and the host.
/// NEGATIVE CONTROL: asked at qualify, the same shortfall refuses hours in with the version's
/// one line held by this Mac.
#[test]
fn an_unfit_host_takes_no_line_by_any_start() {
    for start in [
        bump(),
        ReleaseOptions {
            commit: Some(TIP.into()),
            ..mac()
        },
        ReleaseOptions {
            prepare: true,
            ..bump()
        },
    ] {
        let mut w = World::new();
        if start.commit.is_some() {
            w.versions.insert(TIP.into(), "0.6.0".into());
        }
        w.host_unfit = Some(unfit());
        let (code, last) = w.run(start.clone());
        assert_eq!(code, 1, "{start:?}: {last}\n{:#?}", w.out);
        assert!(last.contains("needs 32.8 GiB and 8 GiB spare"), "{last}");
        assert!(
            last.contains("32 GiB of memory") && last.contains("12 cores"),
            "{last}"
        );
        assert!(last.contains("the pump on the cut host"), "{last}");
        assert_eq!(w.host_asked, 1);
        assert!(
            w.calls.is_empty(),
            "nothing ran before the refusal: {:?}",
            w.calls
        );
        assert_eq!(w.tip(), TIP, "nothing bumped");
        assert!(w.journal().is_none());
        assert!(!w.release_dir().join(super::SELECTION_FILE).exists());
        assert!(
            w.repairs.asked.is_empty(),
            "the host is no red of the source"
        );
    }
}

/// A LINE THIS CLONE HOLDS RESUMES WHATEVER THE HOST. A release prepared here is finished here:
/// the fitness question is asked only of a run that would take a line.
/// NEGATIVE CONTROL: asked of every run, a newer cutter strands the line its Mac already holds.
#[test]
fn a_line_this_clone_holds_resumes_on_an_unfit_host() {
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let asked = w.host_asked;
    assert!(asked >= 1);
    w.host_unfit = Some(unfit());
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert_eq!(w.host_asked, asked, "a resume asks nothing of the host");
    assert_eq!(w.journal().unwrap().commit, BUMP);

    // The next version's line is a line to take: it is refused here.
    w.origin_tags.insert("v0.6.0".into(), BUMP.into());
    w.ledger.push("0.6.0".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.contains("cannot hold the release"), "{last}");
    assert_eq!(
        w.count("bump"),
        1,
        "only the first release's bump: {:?}",
        w.calls
    );
}

/// `--fix` ON A CLONE WITH NO JOURNAL TAKES THE FLEET'S LINE, AND IS JUDGED. Selection adopts
/// the fleet line into a journal in memory before the fitness question; the question is asked
/// of the journal this clone had on disk, which is none.
/// NEGATIVE CONTROL: asked of the journal selection left in memory, the adopted line reads as
/// one this clone holds and the unfit Mac carries the fix onto it (the same run on a fit host,
/// below, does exactly that).
#[test]
fn a_fix_that_adopts_the_fleet_line_is_judged() {
    for dry_run in [true, false] {
        let forgotten = || {
            let mut w = red_line();
            let line = w.journal().unwrap().line();
            w.existing_fleet_line = Some(line.clone());
            let (code, last) = w.run(ReleaseOptions {
                forget: true,
                ..mac()
            });
            assert_eq!(code, 0, "{last}");
            assert!(w.journal().is_none());
            (w, line)
        };
        let (mut w, line) = forgotten();
        w.host_unfit = Some(unfit());
        let asked = w.host_asked;
        let carried = w.count("carry");
        let (code, last) = w.run(ReleaseOptions {
            dry_run,
            ..fix(&[FIX])
        });
        assert_eq!(code, 1, "dry {dry_run}: {last}\n{:#?}", w.out);
        if dry_run {
            assert!(
                w.out
                    .iter()
                    .any(|l| l.contains("WOULD REFUSE") && l.contains("cannot hold the release")),
                "{:#?}",
                w.out
            );
        } else {
            assert!(last.contains("cannot hold the release"), "{last}");
        }
        assert_eq!(w.host_asked, asked + 1);
        assert_eq!(w.count("carry"), carried, "nothing carried: {:?}", w.calls);
        assert!(
            w.journal().is_none(),
            "no journal of the line it did not take"
        );
        assert_eq!(
            w.existing_fleet_line,
            Some(line),
            "the fleet line is as it was"
        );

        let (mut fit, line) = forgotten();
        let (code, last) = fit.run(ReleaseOptions {
            dry_run,
            ..fix(&[FIX])
        });
        assert_eq!(code, 0, "dry {dry_run}: {last}\n{:#?}", fit.out);
        if !dry_run {
            assert_eq!(fit.journal().unwrap().line().base, line.base);
        }
    }
}

/// A LINE ANOTHER MAC TOOK IS A LINE TO TAKE. This clone's unfinished journal of the version is
/// no claim once the fleet's claim names another Mac: resuming would contend for it, so the
/// host is judged first and the unfit Mac leaves the line where it is.
/// NEGATIVE CONTROL: the same journal with the claim still this Mac's resumes on the same unfit
/// host, asking nothing (the journal alone decided before, whoever held the claim).
#[test]
fn a_journaled_line_another_mac_holds_is_judged_again() {
    let prepared = || {
        let mut w = World::new();
        let (code, last) = w.run(ReleaseOptions {
            prepare: true,
            ..bump()
        });
        assert_eq!(code, 0, "{last}\n{:#?}", w.out);
        // The fleet's claim of the line this clone prepared.
        w.existing_fleet_line = Some(w.journal().unwrap().line());
        w.host_unfit = Some(unfit());
        w
    };
    let mut w = prepared();
    w.fleet_elsewhere = true;
    let (asked, published) = (w.host_asked, w.count("publish"));
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.contains("cannot hold the release"), "{last}");
    assert_eq!(w.host_asked, asked + 1);
    assert_eq!(w.count("publish"), published, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, BUMP, "the journal is kept");

    let mut own = prepared();
    let asked = own.host_asked;
    let (code, last) = own.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", own.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert_eq!(own.host_asked, asked);
}

/// `--supersede` TAKES THE VERSION'S CLAIM, WHOEVER HOLDS IT, AND IS JUDGED: with no journal
/// here, and with this clone's own unfinished journal of the version it replaces.
/// NEGATIVE CONTROL: the same requests on a fit host reach the supersession's preflight; an
/// unfit one never asks the fleet.
#[test]
fn a_supersession_is_judged_whatever_this_clone_holds() {
    let supersede = || ReleaseOptions {
        commit: Some(TIP.into()),
        supersede: true,
        ..mac()
    };
    // No journal here: another holder's line.
    let other = || {
        let mut w = World::new();
        w.versions.insert(TIP.into(), "0.6.0".into());
        w.history.insert(1, NEXT.into());
        w.versions.insert(NEXT.into(), "0.6.0".into());
        w.existing_fleet_line = Some(Line::start("0.6.0", NEXT, 100));
        w
    };
    // This clone's own prepared line at NEXT, which main has moved past.
    let own = || {
        let mut w = other();
        w.existing_fleet_line = None;
        let (code, last) = w.run(ReleaseOptions {
            commit: Some(NEXT.into()),
            prepare: true,
            ..mac()
        });
        assert_eq!(code, 0, "{last}\n{:#?}", w.out);
        assert_eq!(w.journal().unwrap().commit, NEXT);
        w
    };
    for (name, world) in [
        ("another's", &other as &dyn Fn() -> World),
        ("its own", &own),
    ] {
        let mut w = world();
        w.host_unfit = Some(unfit());
        let (asked, before) = (w.host_asked, w.existing_fleet_line.clone());
        let (code, last) = w.run(supersede());
        assert_eq!(code, 1, "{name}: {last}\n{:#?}", w.out);
        assert!(last.contains("cannot hold the release"), "{name}: {last}");
        assert_eq!(w.host_asked, asked + 1, "{name}");
        assert_eq!(w.count("supersede-preflight"), 0, "{name}: {:?}", w.calls);
        assert_eq!(w.count("supersede"), 0, "{name}: {:?}", w.calls);
        assert_eq!(w.existing_fleet_line, before, "{name}");

        let mut fit = world();
        let _ = fit.run(supersede());
        assert!(
            fit.count("supersede-preflight") >= 1,
            "{name}: a fit host reaches the preflight: {:?}\n{:#?}",
            fit.calls,
            fit.out
        );
    }
}

/// What takes no line is not asked: a private qualification and `--open-next`. A dry run of a
/// start says what the real run would refuse.
#[test]
fn only_a_run_that_would_take_a_line_is_judged() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.host_unfit = Some(unfit());
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(TIP.into()),
        qualify: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("QUALIFIED aterm 0.6.0"), "{last}");
    assert_eq!(w.host_asked, 0);

    let mut w = World::new();
    w.host_unfit = Some(unfit());
    let (code, last) = w.run(ReleaseOptions {
        open_next: true,
        ..ReleaseOptions::default()
    });
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.host_asked, 0, "opening a version takes no line");

    let mut w = World::new();
    w.host_unfit = Some(unfit());
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.contains("a real run would refuse"), "{last}");
    assert!(
        w.out
            .iter()
            .any(|line| line.contains("WOULD REFUSE") && line.contains("cannot hold the release")),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("preview-bump"), 0);
}

/// A fit host says what it is, once, before step 1.
#[test]
fn a_fit_host_is_named_before_the_line_is_taken() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    let said = w
        .out
        .iter()
        .position(|line| line == "release: host: 256 GiB of memory, 36 cores")
        .expect("the host is said");
    assert!(
        w.said_at_bump.is_some_and(|at| said < at),
        "before the bump: {:#?}",
        w.out
    );
}
