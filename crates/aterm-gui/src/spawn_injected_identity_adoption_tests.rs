// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::adopt_injected_identity_in;
use crate::session_log::{Epochs, SessionLog};
use aterm_session::{LaunchNonce, SessionId};
use std::sync::Arc;

#[test]
fn two_launches_from_one_shell_do_not_share_an_identity() {
    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let premint = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let (sid, hex) = (premint.as_str().to_string(), nonce.to_hex());

    let first =
        adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
            .expect("the first launch adopts the premint");
    assert_eq!(first.0, premint);
    assert!(first.1.ct_eq(&nonce), "the nonce rides with the id");

    assert!(
        adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
            .is_none(),
        "a second launch from the same shell must NOT answer to a live id"
    );

    crate::identity_claim::release_claims_for_test();
    let relaunch =
        adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &Epochs::new())
            .expect("a relaunch after the first launch exits adopts the premint");
    assert_eq!(relaunch.0, premint);
    crate::identity_claim::release_claims_for_test();
}

#[test]
fn adoption_is_refused_without_a_whole_identity_or_a_place_to_claim_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let sid = SessionId::generate().as_str().to_string();
    let hex = LaunchNonce::generate().to_hex();
    assert!(
        adopt_injected_identity_in(Some(dir.path()), Some(&sid), None, &Epochs::new()).is_none()
    );
    assert!(
        adopt_injected_identity_in(Some(dir.path()), None, Some(&hex), &Epochs::new()).is_none()
    );
    assert!(
        adopt_injected_identity_in(Some(dir.path()), Some("bad"), Some(&hex), &Epochs::new())
            .is_none()
    );
    assert!(
        adopt_injected_identity_in(None, Some(&sid), Some(&hex), &Epochs::new()).is_none(),
        "no control directory, no claim, no adoption"
    );
}

#[test]
fn a_relaunch_under_the_premint_takes_an_epoch_above_its_predecessors() {
    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    let premint = SessionId::generate();
    let (sid, hex) = (
        premint.as_str().to_string(),
        LaunchNonce::generate().to_hex(),
    );
    let hand = aterm_slog::Stamp::new(1_792_000_000_400, 400);
    let launch = || {
        let epochs = Arc::new(Epochs::new());
        let (id, _) = adopt_injected_identity_in(Some(dir.path()), Some(&sid), Some(&hex), &epochs)
            .expect("the launch adopts the premint");
        let ep = SessionLog::starting(Arc::clone(&epochs), hand).log().ep();
        crate::identity_claim::record_epoch_in(dir.path(), &id, ep, true);
        crate::identity_claim::release_claims_for_test();
        ep
    };
    let first = launch();
    let second = launch();
    assert!(second > first, "the same second: {first} then {second}");
    assert_eq!(
        SessionLog::starting(Arc::new(Epochs::new()), hand)
            .log()
            .ep(),
        first,
        "NEGATIVE CONTROL: told nothing, the same second, the same epoch"
    );
    let recorded = || crate::identity_claim::recorded_epoch_in(dir.path(), &premint);
    assert_eq!(recorded(), Some(second));
    crate::identity_claim::record_epoch_in(dir.path(), &premint, second + 1, false);
    crate::identity_claim::record_epoch_in(dir.path(), &premint, first, false);
    assert_eq!(recorded(), Some(second + 1));
    let third = launch();
    assert!(third > second + 1, "{second} + 1 then {third}");
    let minted = SessionId::generate();
    crate::identity_claim::record_epoch_in(dir.path(), &minted, 7, false);
    assert_eq!(
        crate::identity_claim::recorded_epoch_in(dir.path(), &minted),
        None
    );
    let path = dir.path().join("claims").join(format!("{sid}.ep"));
    for foreign in [format!("{}\n", u32::MAX), "zz\n".to_string()] {
        std::fs::write(&path, foreign).expect("plant");
        assert_eq!(recorded(), None);
    }
    let fourth = launch();
    assert_eq!(recorded(), Some(fourth));

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::symlink;
        let claims = dir.path().join("claims");
        let sentinel = dir.path().join("sentinel");
        std::fs::write(&sentinel, "71\n").expect("owned sentinel");
        let old_epoch_stage = claims.join(format!(".{sid}.ep.{}.tmp", std::process::id()));
        let old_marker_stage = claims.join(format!(".{sid}.successor.{}.tmp", std::process::id()));
        for stage in [&old_epoch_stage, &old_marker_stage] {
            symlink(&sentinel, stage).expect("owned stage alias");
        }
        crate::identity_claim::record_epoch_in(dir.path(), &premint, fourth + 1, true);
        crate::identity_claim::mark_successor_in(dir.path(), std::slice::from_ref(&premint), 123);
        assert_eq!(recorded(), Some(fourth + 1), "ordinary publication");
        assert_eq!(
            std::fs::read_to_string(claims.join(format!("{sid}.successor"))).unwrap(),
            "pid 123\n",
            "the marker publishes without consulting process liveness"
        );
        for stage in [&old_epoch_stage, &old_marker_stage] {
            assert!(std::fs::symlink_metadata(stage).unwrap().is_symlink());
            assert_eq!(std::fs::read_link(stage).unwrap(), sentinel);
        }
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"71\n");

        std::fs::remove_file(&path).expect("remove our record");
        symlink(&sentinel, &path).expect("owned final alias");
        assert_eq!(recorded(), None, "a symlink is not a record");
        std::fs::remove_file(&path).expect("remove our alias");
        std::fs::hard_link(&sentinel, &path).expect("owned hardlink");
        assert_eq!(recorded(), None, "a multiply named inode is not a record");
        std::fs::remove_file(&path).expect("remove our hardlink");
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a live NUL-terminated owned path; mkfifo retains nothing.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(recorded(), None, "a FIFO with no writer is refused");
        std::fs::remove_file(&path).expect("remove our FIFO");
        std::fs::write(&path, "7               x").expect("over-bound record");
        assert_eq!(recorded(), None, "a capped prefix is not a whole record");
        crate::identity_claim::record_epoch_in(dir.path(), &premint, fourth + 2, true);
        assert_eq!(recorded(), Some(fourth + 2), "ordinary record reads again");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"71\n");
    }
}
