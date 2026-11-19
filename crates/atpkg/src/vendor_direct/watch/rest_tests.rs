// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The hang detector for a ring that must come.
const HANG: Duration = Duration::from_secs(60);

/// A RING BEFORE THE WAIT IS HEARD AT ONCE, AND EACH SOURCE SAYS WHO RANG: an hour's wait after
/// a work ring returns it unparked, the next take finds nothing, and the wall and stop rings
/// each come back as their own field. NEGATIVE CONTROL: a wait that reads the state without
/// taking it hears the work ring again on the take.
#[test]
fn a_ring_before_the_wait_is_heard_and_names_its_source() {
    let rest = Rest::inert();
    rest.ringer().ring();
    assert_eq!(
        rest.wait(Some(Duration::from_secs(3600))),
        Some(Told {
            rung: true,
            ..Told::default()
        })
    );
    assert_eq!(rest.take(), None, "taken");
    rest.wall_ringer().ring();
    rest.stopper().ring();
    assert_eq!(
        rest.wait(None),
        Some(Told {
            woke: true,
            stop: true,
            ..Told::default()
        })
    );
    assert_eq!(rest.parks(), 0, "no wait parked");
}

/// A WAIT WITH NO DEADLINE ENDS ON A FILE IT WATCHES: a real file watch over a scratch file,
/// and a wait for events alone (no deadline, the hang detector on a helper thread) ends with
/// `inputs` once the file is written. NEGATIVE CONTROL: a rest that does not watch the file
/// never hears the write, and the hang detector fails the test.
#[test]
fn a_change_of_a_watched_file_ends_a_wait_with_no_deadline() {
    let dir = crate::testdir::dir("atpkg-rest-file-");
    let path = dir.join("aterm.toml");
    std::fs::write(&path, "[packages]\nenabled = true\n").unwrap();
    let rest = Arc::new(Rest::start("atpkg-rest-test", [path.clone()]));
    assert!(rest.watching(), "the platform's file watch started");
    // The watch's first look tells once; take it so the write below is what ends the wait.
    assert!(rest.heard(HANG, |told| told.inputs), "the first look rings");
    let _ = rest.take();
    let (said, heard) = std::sync::mpsc::channel();
    let waiting = Arc::clone(&rest);
    let waiter = std::thread::spawn(move || said.send(waiting.wait(None)));
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the waiter parks", || {
        (rest.parks() > 0).then_some(())
    });
    std::fs::write(&path, "[packages]\nenabled = false\n").unwrap();
    let told = heard
        .recv_timeout(HANG)
        .expect("the write ends the wait (a hang detector)");
    assert!(told.is_some_and(|told| told.inputs), "{told:?}");
    waiter.join().unwrap().unwrap();
}
