// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn time_advances_monotonically_and_only_a_judged_reading_gets_a_receipt() {
    let root = crate::test_scratch_dir("clock");
    let clock = Clock::new(Some(root.path().as_os_str().to_owned()), "operation");
    let path = root.path().join("operation");
    let before = clock.read().now;
    std::fs::write(&path, "32000").unwrap();
    let after = clock.read();
    assert!(after.now.duration_since(before) >= Duration::from_secs(32));
    assert!(!path.with_extension("seen").exists());
    after.observed();
    assert_eq!(
        std::fs::read_to_string(path.with_extension("seen")).unwrap(),
        "32000"
    );
    for value in ["0", "partial"] {
        std::fs::write(&path, value).unwrap();
        assert!(clock.read().now >= before + Duration::from_secs(32));
    }
    let native = Clock::new(None, "unused");
    let reading = native.read();
    let until = reading.now + Duration::from_secs(60);
    assert_eq!(reading.wait_until(until), until);
}

#[test]
fn a_held_read_takes_the_time_written_before_its_release() {
    let root = crate::test_scratch_dir("clock");
    let path = root.path().join("operation");
    std::fs::write(path.with_extension("hold"), "").unwrap();
    let clock = Clock::new(Some(root.path().as_os_str().to_owned()), "operation");
    let (sent, heard) = std::sync::mpsc::channel();
    let before = Instant::now();
    let reader = std::thread::spawn(move || {
        let reading = clock.read();
        sent.send(reading.now).unwrap();
        reading.observed();
    });
    aterm_spec::wait::until_within(
        Duration::from_secs(60),
        "the read reached its barrier",
        || path.with_extension("ready").exists().then_some(()),
    );
    assert_eq!(heard.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    std::fs::write(&path, "25000").unwrap();
    std::fs::remove_file(path.with_extension("hold")).unwrap();
    assert!(
        heard.recv_timeout(Duration::from_secs(60)).unwrap() >= before + Duration::from_secs(25)
    );
    reader.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(path.with_extension("seen")).unwrap(),
        "25000"
    );
}
