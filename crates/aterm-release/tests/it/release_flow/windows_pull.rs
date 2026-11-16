// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows pull lane in the release's step machine ([`crate::release`]'s `Run::windows_ask`
//! and `Run::windows_pull`) against the fake world: the request at reservation, step 7's wait
//! while a builder serves or origin cannot be read, the attested pair declared to the sign and
//! the claim with the day it was admitted, a pair whose folder was lost put back together, and
//! every other outcome a release without a Windows zip, never a refused one.

use std::path::Path;

use super::{World, bump, mac};
use crate::buildplan::windows_pull::{
    ALIVE_SECS, Config, Found, Heartbeat, Pulled, Refused, WAIT_SECS,
};
use crate::release::ReleaseOptions;
use aterm_winship::pull::Alive;

const ON: Config = Config {
    on: true,
    wait: WAIT_SECS,
    alive: ALIVE_SECS,
};

const BUILDER: &str = "win-builder-1";

/// Origin with no handoff and the builder's heartbeat pushed at `at`, a builder C pins.
fn serving(at: u64) -> Found {
    Found::Absent(Ok(Some(Heartbeat {
        alive: Alive {
            builder: BUILDER.to_string(),
            at: aterm_winship::pointer::rfc3339(at),
            request: None,
            passed_over: None,
        },
        committed: at,
        pinned: Ok(()),
    })))
}

/// The handoff admitted, put together in `dir`: the zip, its receipt and its attestation are
/// there (empty: the fake world admits nothing itself).
fn pulled(dir: &Path) -> Pulled {
    let asset = aterm_winship::handoff::TARGET.asset_name("0.6.0");
    std::fs::create_dir_all(dir).unwrap();
    for name in [
        asset.clone(),
        aterm_winship::handoff::receipt_name(&asset),
        aterm_winship::pull::attestation_name(&asset),
    ] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    Pulled {
        directory: dir.to_path_buf(),
        asset,
        sha256: "c".repeat(64),
        builder: BUILDER.to_string(),
        public_key: "ab".repeat(32),
    }
}

fn windows_flags(args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|arg| !arg.starts_with("--windows"))
        .cloned()
        .collect()
}

/// A PUMP RELEASE WAITS FOR A SERVING BUILDER, THEN SIGNS ITS ATTESTED PAIR: the request is
/// written once, when the build is reserved; step 7, finding no handoff and a fresh heartbeat,
/// ends the run COULD NOT RUN before anything signs; the next run on the same reservation asks
/// nothing again, admits the handoff on that run's UTC day, journals the day, and declares it
/// `--windows-attested` to the sign and to the claim, journaled with the reservation and never
/// as the line's declaration.
/// NEGATIVE CONTROL: a release that went on at once shipped Mac-only while the builder was
/// minutes from handing off; one that asked at each run made the builder start over.
#[test]
fn a_pump_release_waits_for_a_serving_builder_then_signs_its_attested_pair() {
    let mut w = World::new();
    w.windows_pull = ON;
    w.windows_found = Ok(serving(w.now));
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}\n{:#?}", w.out);
    assert!(
        last.contains("COULD NOT RUN") && last.contains("waiting for this release's Windows zip"),
        "{last}"
    );
    assert_eq!(w.count("windows-request"), 1, "{:?}", w.calls);
    assert_eq!(w.count("windows-look"), 1, "{:?}", w.calls);
    assert_eq!(w.count("cut --sign"), 0, "nothing signs while it waits");
    assert!(
        w.calls.iter().position(|c| c == "windows-request")
            > w.calls.iter().position(|c| c == "reserve"),
        "asked once the build is reserved: {:?}",
        w.calls
    );
    let j = w.journal().unwrap();
    let reserved = j.reserved.clone().unwrap();
    let pull = j.windows_pull.clone().unwrap();
    assert!(pull.of(reserved.build, &reserved.commit), "{pull:?}");
    assert!(pull.request.is_some() && !pull.decided(), "{pull:?}");
    assert!(pull.admitted_on.is_none(), "{pull:?}");
    assert_eq!(
        w.windows_requests,
        [(
            "0.6.0".to_string(),
            reserved.build,
            reserved.commit.clone(),
            pull.requested_at
        )]
    );

    let dir = w.dir.path().join("pulled");
    w.windows_found = Ok(Found::Handoff(Ok(pulled(&dir))));
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert_eq!(
        w.count("windows-request"),
        0,
        "the reservation was asked for once"
    );
    let declared = vec![
        "--windows-artifacts".to_string(),
        dir.display().to_string(),
        "--windows-attested".to_string(),
    ];
    assert_eq!(windows_flags(w.sign_args.last().unwrap()), declared);
    let claim = w
        .calls
        .iter()
        .find(|c| c.starts_with("cut --mac-only"))
        .unwrap();
    assert!(claim.ends_with(&declared.join(" ")), "{claim}");
    let j = w.journal().unwrap();
    let pull = j.windows_pull.clone().unwrap();
    assert_eq!(
        (pull.admitted.as_deref(), pull.directory.as_deref()),
        (Some(BUILDER), Some(dir.as_path()))
    );
    // Admitted on the day the run looked, which every later check of it is judged on.
    let day = w.windows_look_days.last().cloned().unwrap();
    assert!(aterm_winship::pull::date_days(&day).is_some(), "{day}");
    assert_eq!(pull.admitted_on.as_deref(), Some(day.as_str()));
    assert_eq!(j.pulled_windows(), Some(dir));
    assert!(j.windows.is_none() && j.windows_sha256.is_none(), "{j:?}");
}

/// ORIGIN THAT CANNOT BE READ IS WAITED OUT, NOT DECIDED: a run whose look at origin fails
/// (origin down, a fetch that failed) once the request is written ends COULD NOT RUN with
/// nothing journaled as decided, and a later run that reads origin admits the handoff.
/// NEGATIVE CONTROL: one failed read of origin made the reservation Mac-only for good, though
/// the builder handed off minutes later.
#[test]
fn origin_that_cannot_be_read_is_waited_out() {
    let fetch_failed = Found::Handoff(Err(Refused {
        why: "the fetch failed".into(),
        bad_signature: false,
        transient: true,
    }));
    for found in [Err("origin is down".to_string()), Ok(fetch_failed)] {
        let mut w = World::new();
        w.windows_pull = ON;
        w.windows_found = found;
        let (code, last) = w.run(bump());
        assert_eq!(code, 3, "{last}\n{:#?}", w.out);
        assert!(
            last.contains("COULD NOT RUN") && last.contains("a later run reads origin again"),
            "{last}"
        );
        assert_eq!(w.count("cut --sign"), 0, "nothing signs while it waits");
        let pull = w.journal().unwrap().windows_pull.unwrap();
        assert!(!pull.decided(), "{pull:?}");

        let dir = w.dir.path().join("pulled");
        w.windows_found = Ok(Found::Handoff(Ok(pulled(&dir))));
        let (code, last) = w.run(mac());
        assert_eq!(code, 0, "{last}\n{:#?}", w.out);
        assert_eq!(
            windows_flags(w.sign_args.last().unwrap()),
            [
                "--windows-artifacts".to_string(),
                dir.display().to_string(),
                "--windows-attested".to_string(),
            ]
        );
    }
}

/// NO BUILDER SERVING, A HANDOFF REFUSED, A BUILDER C DOES NOT PIN OR THAT PASSED THE REQUEST
/// OVER, OR A REQUEST NEVER WRITTEN: THE RELEASE GOES ON WITHOUT A WINDOWS ZIP IN THE SAME RUN,
/// the reason journaled and said, a signature that does not verify said as such; nothing
/// Windows reaches the sign.
/// NEGATIVE CONTROL: the pull lane stopped the pump's release whenever the Windows host was off.
#[test]
fn without_a_serving_builder_or_an_admitted_handoff_the_release_goes_on_mac_only() {
    let bad_signature = Found::Handoff(Err(Refused {
        why: "BAD SIGNATURE: the signature is not builder win-builder-1's".into(),
        bad_signature: true,
        transient: false,
    }));
    let missing = Found::Handoff(Err(Refused {
        why: "a part is missing".into(),
        bad_signature: false,
        transient: false,
    }));
    let unpinned = {
        let Found::Absent(Ok(Some(mut beat))) = serving(1_000_000) else {
            unreachable!()
        };
        beat.pinned = Err("unknown builder win-builder-1".into());
        Found::Absent(Ok(Some(beat)))
    };
    for (found, request_error, says) in [
        (Ok(Found::Absent(Ok(None))), None, "no heartbeat"),
        (Ok(serving(1)), None, "not within 1800 s"),
        (Ok(bad_signature), None, "REFUSED, BAD SIGNATURE"),
        (Ok(missing), None, "a part is missing"),
        (Ok(unpinned), None, "does not admit its attestation"),
        (
            Ok(serving(1_000_000)),
            Some("the native push refused"),
            "the native push refused",
        ),
        (
            Err("origin is down".to_string()),
            Some("the native push refused"),
            "origin is down",
        ),
    ] {
        let mut w = World::new();
        w.windows_pull = ON;
        w.windows_found = found;
        w.windows_request_error = request_error.map(str::to_string);
        let (code, last) = w.run(bump());
        assert_eq!(code, 0, "{says}: {last}\n{:#?}", w.out);
        assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
        assert!(
            w.out
                .iter()
                .any(|line| line.contains("windows:") && line.contains(says)),
            "{says}: {:#?}",
            w.out
        );
        assert!(
            windows_flags(w.sign_args.last().unwrap()).is_empty(),
            "{:?}",
            w.sign_args
        );
        let pull = w.journal().unwrap().windows_pull.unwrap();
        assert!(
            pull.skipped
                .as_deref()
                .is_some_and(|why| why.contains(says))
                || says.contains("BAD SIGNATURE"),
            "{says}: {pull:?}"
        );
        assert!(pull.admitted.is_none(), "{pull:?}");
        assert_eq!(pull.unrequested.is_some(), request_error.is_some());
    }
}

/// A PAIR ADMITTED AND THEN LOST IS PUT BACK TOGETHER ON ITS ADMISSION DAY: a run that finds the
/// folder of a pair an earlier run admitted no longer whole says what it lacks, looks at
/// origin's handoff again judged on the day the pair was first admitted (not the run's own),
/// and declares what that admits; the journal keeps the first day.
/// NEGATIVE CONTROL: a resume whose folder something emptied handed the sign a declared
/// directory with no pair in it, which refused the cut for a lane that must fail open.
#[test]
fn a_pair_admitted_and_then_lost_is_put_back_together_on_its_admission_day() {
    let mut w = World::new();
    w.windows_pull = ON;
    w.windows_found = Ok(serving(w.now));
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}\n{:#?}", w.out);
    let dir = w.dir.path().join("pulled");
    w.windows_found = Ok(Found::Handoff(Ok(pulled(&dir))));
    // The run admits, then a signal stops it before anything is signed.
    w.signal_at = Some("publish-begin");
    let (code, last) = w.run(mac());
    assert_eq!(code, 130, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("cut --sign"), 0, "{:?}", w.calls);
    let admitted = w.journal().unwrap().windows_pull.unwrap();
    let first_day = admitted.admitted_on.clone().unwrap();
    assert!(admitted.admitted.is_some(), "{admitted:?}");
    // Days later, something has emptied the folder; origin still holds the handoff.
    std::fs::remove_dir_all(&dir).unwrap();
    w.signal_at = None;
    w.signal = None;
    w.windows_look_days.clear();
    w.now += 86_400;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        w.out
            .iter()
            .any(|line| line.contains("no longer whole") && line.contains("putting it back")),
        "{:#?}",
        w.out
    );
    assert_eq!(
        w.windows_look_days,
        std::slice::from_ref(&first_day),
        "{:?}",
        w.calls
    );
    assert_ne!(
        aterm_winship::pull::date_of(w.now),
        first_day,
        "the run's own day is another"
    );
    let pull = w.journal().unwrap().windows_pull.unwrap();
    assert_eq!(pull.admitted_on.as_deref(), Some(first_day.as_str()));
    assert_eq!(pull.directory.as_deref(), Some(dir.as_path()));
    assert_eq!(
        windows_flags(w.sign_args.last().unwrap()),
        [
            "--windows-artifacts".to_string(),
            dir.display().to_string(),
            "--windows-attested".to_string(),
        ]
    );
}

/// A PAIR CARRIED BY HAND IS THE PERSON'S: with the lane on, a release that declares
/// `--windows-artifacts` with `--windows-sha256` asks no builder and looks at no handoff, and
/// the sign is handed the digest the person named, as before the lane.
/// NEGATIVE CONTROL: the lane's admission replaced the person's pair.
#[test]
fn a_pair_carried_by_hand_asks_no_builder() {
    let mut w = World::new();
    w.windows_pull = ON;
    let dir = w.dir.path().join("carried");
    let sha256 = "a".repeat(64);
    let (code, last) = w.run(ReleaseOptions {
        windows: Some(dir.clone()),
        windows_sha256: Some(sha256.clone()),
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("windows-request"), 0, "{:?}", w.calls);
    assert_eq!(w.count("windows-look"), 0, "{:?}", w.calls);
    assert_eq!(
        windows_flags(w.sign_args.last().unwrap()),
        [
            "--windows-artifacts".to_string(),
            dir.display().to_string(),
            "--windows-sha256".to_string(),
            sha256,
        ]
    );
    let j = w.journal().unwrap();
    assert!(j.windows_pull.is_none(), "{j:?}");
    assert_eq!(j.windows, Some(dir));
}

/// A LANE THAT IS OFF ASKS NOTHING: the release commit's config without the lane writes no
/// request and looks at nothing.
#[test]
fn a_lane_that_is_off_asks_nothing() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.count("windows-request") + w.count("windows-look"), 0);
    assert!(w.journal().unwrap().windows_pull.is_none());
}

/// A LINE WITH HOPS ASKS AS ANY LINE DOES: its head is recorded in main's history before the
/// build is reserved, so its release commit's parent is on origin/main, which is all a builder
/// holds a request to; the request is written, and the pair handed off is admitted and declared.
/// NEGATIVE CONTROL: a fixed line journaled "a builder builds only over origin/main" and went
/// Mac-only, though the builder would have built it.
#[test]
fn a_line_with_hops_asks_as_any_line_does() {
    let mut w = super::red_line();
    w.windows_pull = ON;
    let dir = w.dir.path().join("pulled");
    w.windows_found = Ok(Found::Handoff(Ok(pulled(&dir))));
    w.calls.clear();
    let (code, last) = w.run(super::fix(&[super::FIX]));
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("windows-request"), 1, "{:?}", w.calls);
    assert_eq!(w.count("windows-look"), 1, "{:?}", w.calls);
    let j = w.journal().unwrap();
    assert!(!j.line().hops.is_empty(), "{j:?}");
    let pull = j.reserved_pull().cloned().unwrap();
    assert!(pull.request.is_some() && pull.skipped.is_none(), "{pull:?}");
    assert_eq!(pull.admitted.as_deref(), Some(BUILDER), "{pull:?}");
    assert_eq!(
        windows_flags(w.sign_args.last().unwrap()),
        [
            "--windows-artifacts".to_string(),
            dir.display().to_string(),
            "--windows-attested".to_string(),
        ]
    );
}
