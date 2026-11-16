// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A refused Approve says why whole, in a record and not on screen: the refusal of a key the
/// queue lists under two names ends with the command that approves it, past one detail
/// line's cap.
/// NEGATIVE CONTROL: the refusal as one detail line is clipped before its command (asserted
/// below).
#[test]
fn a_refused_approve_says_why_whole_in_a_record() {
    let key = "age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq";
    let request = |host: &str| aterm_secrets::client::Request {
        recipient: key.to_string(),
        host: host.to_string(),
        code: "ABCD-EFGH-JKMN-PQRS".to_string(),
        at: None,
        allowed: None,
        proof: None,
    };
    let pending = [request("mc"), request(&"a-name-a-lan-host-chose".repeat(2))];
    let why = aterm_secrets::client::one_name_asked(&pending, &pending[0])
        .expect_err("a key listed under two names")
        .to_string();
    let command = format!("aterm secrets approve {key} --name <host>");
    assert!(why.ends_with(&command), "{why}");
    let said = refused(Some("mc"), &why).normalized();
    assert_eq!(said.title, "Couldn't approve mc");
    assert!(
        matches!(said.need(), aterm_messages::need::Need::Record),
        "a record, not an alert: {:?}",
        said.need()
    );
    let said = said.detail.join(" ");
    assert!(
        said.contains(&format!("Approve did not let it in: {why}")),
        "{said}"
    );
    let clipped = Message::new(tags::PRIVACY, Severity::Warn, "x")
        .line(&why)
        .normalized()
        .detail
        .join(" ");
    assert!(!clipped.contains(&command), "{clipped}");
}

/// A vault folder under a scratch home, made private as the approver makes it.
fn scratch_paths(home: &std::path::Path) -> Paths {
    use std::os::unix::fs::PermissionsExt as _;
    let paths = Paths::locate_for_home(home);
    std::fs::create_dir_all(&paths.dir).expect("the scratch vault folder");
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700))
        .expect("the scratch vault folder made private");
    paths
}

/// Write the record under `paths` as the approver writes it: `text` whole, private, renamed in.
fn write_record(paths: &Paths, text: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let next = paths.dir.join("asking.next");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&next)
        .expect("the next record");
    file.write_all(text.as_bytes())
        .expect("the next record's text");
    drop(file);
    std::fs::rename(&next, record(paths)).expect("the next record renamed in");
}

/// One asking line of the record, for the Mac named `host`.
fn asking_line(host: &str) -> String {
    format!(
        "ABCD-EFGH-JKMN-PQRS\tage1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq\t{host}\n"
    )
}

/// The hosts of `asking`, as the window files them.
fn hosts(asking: &[Asking]) -> Vec<&str> {
    asking.iter().map(|asking| asking.host.as_str()).collect()
}

/// A FIFO at the record's path is refused, as `aterm_secrets::client::asking` refuses it, and
/// never opened to wait for a writer: the lane reads no Mac asking and goes on to its wait, and a
/// record the approver renames over the FIFO shows at the next round. The rounds run on a thread
/// of their own, each waited on by the one-minute hang detector, never a latency budget.
///
/// NEGATIVE CONTROL (2026-11-15): with `approver_named` opening the record with a plain
/// `std::fs::File::open` (the lane before), the first round waits in the open for a writer and
/// meets the hang detector (`a FIFO at the record is refused`).
#[test]
fn a_fifo_at_the_record_is_refused_never_waited_on() {
    use std::sync::mpsc;
    let home = aterm_tempfile::tempdir().expect("a scratch home");
    let paths = scratch_paths(home.path());
    let fifo = std::ffi::CString::new(record(&paths).display().to_string()).expect("a path");
    // SAFETY: a NUL-terminated path in this test's scratch folder.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "the FIFO");
    let (tell, published) = mpsc::channel::<Vec<Asking>>();
    let lane = std::thread::spawn(move || {
        let bell = aterm_uds::ready::Doorbell::new().expect("the lane's doorbell");
        let mut rounds = 0;
        let mut publish = |now: Vec<Asking>| {
            let _ = tell.send(now);
        };
        let mut wait = |_: &[BorrowedFd<'_>], _: Option<Instant>| {
            rounds += 1;
            if rounds == 1 {
                write_record(&paths, &asking_line("mac-a"));
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        };
        follow(&paths, true, &bell, &mut publish, &mut wait)
    });
    let first = published
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a FIFO at the record is refused");
    assert!(first.is_empty(), "no Mac asking: {:?}", hosts(&first));
    let next = published
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the next round");
    assert_eq!(hosts(&next), ["mac-a"], "a record renamed over it shows");
    assert_eq!(lane.join().expect("the lane"), ControlFlow::Break(()));
}

/// The lane reads the approver's record at each change of it and at the exit of the approver it
/// names, and on no clock: a record written after launch shows, one naming a running approver
/// shows, and that approver's exit ends it, while every wait of the lane carries no deadline. The
/// bounds kept: a request shows once the watch's burst settles (`file_watch::QUIET` to
/// `SETTLE_CAP`, inside the two seconds a read every [`READ_EVERY`] took), and an approver's exit
/// ends its record as the kernel tells it, inside the same two seconds. Each step waits on the
/// one-minute hang detector, never a latency budget.
///
/// NEGATIVE CONTROLS (2026-11-14): with `rest` answering [`READ_EVERY`] whatever it is told
/// (the lane before), every wait carries a deadline (`a wait with no event to wait for`); with
/// the approver's exit left out of what the lane waits on, the exit ends nothing and the last
/// step meets the hang detector (`the approver's exit ends its record`).
#[cfg(target_os = "macos")]
#[test]
fn the_record_and_its_approvers_exit_wake_the_lane_and_no_clock_does() {
    use std::sync::mpsc;
    let home = aterm_tempfile::tempdir().expect("a scratch home");
    let paths = scratch_paths(home.path());
    let bell = Arc::new(aterm_uds::ready::Doorbell::new().expect("the lane's doorbell"));
    let stop = Arc::new(aterm_uds::ready::Doorbell::new().expect("the test's stop"));
    let watching = watch(&paths, Arc::clone(&bell)).expect("the record's watch");
    let deadlines: Arc<Mutex<Vec<Option<Instant>>>> = Arc::default();
    let gate: Arc<Mutex<()>> = Arc::default();
    let parked = Arc::new(AtomicBool::new(false));
    let (tell, published) = mpsc::channel::<Vec<Asking>>();
    let lane = {
        let (paths, bell, stop) = (paths.clone(), Arc::clone(&bell), Arc::clone(&stop));
        let (deadlines, gate, parked) = (
            Arc::clone(&deadlines),
            Arc::clone(&gate),
            Arc::clone(&parked),
        );
        std::thread::spawn(move || {
            let mut publish = |now: Vec<Asking>| {
                let _ = tell.send(now);
            };
            let mut wait = |fds: &[BorrowedFd<'_>], deadline: Option<Instant>| {
                deadlines.lock().unwrap().push(deadline);
                let mut all = fds.to_vec();
                all.push(stop.fd());
                parked.store(true, Ordering::Release);
                aterm_uds::ready::any(&all, deadline).expect("the lane's wait");
                // Parked until it holds the gate: a lane woken while the test holds it reads
                // nothing before the test lets go.
                let _held = gate.lock().unwrap();
                parked.store(false, Ordering::Release);
                if stop.clear() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            follow(&paths, true, &bell, &mut publish, &mut wait)
        })
    };
    let until = |wanted: &[&str], what: &str| loop {
        let now = published
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("{what}"));
        if hosts(&now) == wanted {
            break;
        }
    };
    until(&[], "the first read finds no record");

    write_record(&paths, &asking_line("mac-a"));
    until(&["mac-a"], "a record written after launch shows");

    let sleep = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"));
    let mut approver = aterm_uds::spawnfd::command(&sleep)
        .arg(aterm_spec::wait::PARKED_SECS.to_string())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("a stand-in approver");
    let pid = approver.id();
    let birth = aterm_uds::process::read_birth(i32::try_from(pid).expect("a pid"))
        .expect("the stand-in's start");
    write_record(
        &paths,
        &format!("{APPROVER_HEAD}{pid}@{}\n{}", birth.0, asking_line("mac-b")),
    );
    until(&["mac-b"], "a record naming a running approver shows");

    // The lane re-reads only once the stand-in is reaped: a child killed but not yet waited for
    // still answers its start, where the approver, launchd's child, is gone at its exit. It is
    // waiting, or woken and held at the gate, when the kill comes, so no read of its runs between
    // the kill and the reap.
    {
        let _held = gate.lock().unwrap();
        let began = Instant::now();
        while !parked.load(Ordering::Acquire) {
            assert!(
                began.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "the lane never waited"
            );
            aterm_uds::deadline::poll_yield();
        }
        approver.kill().expect("the approver killed");
        approver.wait().expect("the approver reaped");
    }
    until(&[], "the approver's exit ends its record");

    stop.ring();
    assert_eq!(lane.join().expect("the lane"), ControlFlow::Break(()));
    drop(watching);
    let deadlines = deadlines.lock().unwrap();
    assert!(deadlines.len() >= 4, "a wait at each step: {deadlines:?}");
    assert!(
        deadlines.iter().all(Option::is_none),
        "a wait with no event to wait for: {deadlines:?}"
    );
}

/// Where no file watch started, or the approver a shown record names cannot be watched for its
/// exit, the lane reads the record every [`READ_EVERY`], as every read was before it waited on
/// events; with both watched it rests on no clock.
///
/// NEGATIVE CONTROL (2026-11-14): with `rest` answering `None` for an unwatched record, the lane
/// with no file watch waits on no clock (`a lane with no file watch reads on today's period`).
#[test]
fn an_input_with_no_event_keeps_todays_period() {
    assert_eq!(
        rest(true, false, false),
        None,
        "nothing shown, the file watched"
    );
    assert_eq!(
        rest(true, true, true),
        None,
        "a shown approver watched for its exit"
    );
    assert_eq!(
        rest(true, true, false),
        Some(READ_EVERY),
        "a shown approver unwatched"
    );
    let home = aterm_tempfile::tempdir().expect("a scratch home");
    let paths = scratch_paths(home.path());
    let bell = aterm_uds::ready::Doorbell::new().expect("the lane's doorbell");
    let mut rested = None;
    let flow = follow(&paths, false, &bell, &mut |_| {}, &mut |_, deadline| {
        rested = Some(deadline);
        ControlFlow::Break(())
    });
    assert_eq!(flow, ControlFlow::Break(()));
    let rested = rested
        .expect("the lane waited")
        .expect("a lane with no file watch reads on today's period");
    assert!(
        rested <= Instant::now() + READ_EVERY,
        "at most READ_EVERY away: {rested:?}"
    );
}
