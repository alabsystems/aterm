// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::fd::{AsFd as _, FromRawFd as _};

const TEST_NONCE: &str = "0123456789abcdef0123456789abcdef";

fn own_pid() -> i32 {
    i32::try_from(std::process::id()).expect("a pid fits pid_t")
}

fn own_parent() -> Option<crate::seamless::AttestedParent> {
    let parent = crate::seamless::AttestedParent::attest_live_for_test(own_pid());
    assert!(
        parent.is_some(),
        "this live process attests by its birth record"
    );
    parent
}

#[test]
fn a_claim_frame_matches_only_its_own_secret() {
    let secret = "a".repeat(CLAIM_HEX_LEN);
    let frame = claim_frame(&secret);
    assert_eq!(frame.len(), CLAIM_FRAME_LEN);
    assert!(claim_frame_matches(&frame, &secret));

    let mut wrong = frame;
    let last = wrong.len() - 1;
    wrong[last] = b'b';
    assert!(
        !claim_frame_matches(&wrong, &secret),
        "one differing byte refuses"
    );

    let mut mismagicked = frame;
    mismagicked[0] = b'X';
    assert!(
        !claim_frame_matches(&mismagicked, &secret),
        "the frame magic is part of the claim"
    );

    assert!(
        !claim_frame_matches(&frame, &"a".repeat(CLAIM_HEX_LEN - 1)),
        "a shorter secret is not a prefix match"
    );
    assert!(!claim_frame_matches(&claim_frame("abc"), "abc"));
}

#[test]
fn a_grant_body_round_trips_in_transfer_order() {
    let held = std::io::stdin();
    let borrowed = held.as_fd();
    let sessions = vec![(7u64, 4242i32, borrowed), (3u64, 99i32, borrowed)];
    let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("encodes");
    assert_eq!(body, format!("{TEST_NONCE}\n7:4242,3:99\n"));
    assert_eq!(
        parse_grant_body(&body, TEST_NONCE, false).expect("parses"),
        GrantBody {
            sessions: vec![(7, 4242), (3, 99)],
            parked_for: None,
        },
        "order is preserved, because order IS the addressing"
    );
}

#[test]
fn a_grant_carries_how_long_before_it_the_parent_parked() {
    let held = std::io::stdin();
    let sessions = vec![(7u64, 4242i32, held.as_fd())];
    let parked = Duration::from_micros(1_500);
    let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked)).expect("encodes");
    assert_eq!(body, format!("{TEST_NONCE}\n7:4242\npark_us=1500\n"));
    assert_eq!(
        parse_grant_body(&body, TEST_NONCE, false),
        Ok(GrantBody {
            sessions: vec![(7, 4242)],
            parked_for: Some(parked),
        })
    );
    let older = format!("{TEST_NONCE}\n7:4242\n");
    assert_eq!(
        parse_grant_body(&older, TEST_NONCE, false).map(|body| body.parked_for),
        Ok(None),
        "an older parent's grant has no stamp"
    );
    for malformed in [
        "park_us=\n",
        "park_us=-1\n",
        "park_us=1.5\n",
        "park=1500\n",
        "\n",
    ] {
        let body = format!("{TEST_NONCE}\n7:4242\n{malformed}");
        assert!(
            parse_grant_body(&body, TEST_NONCE, false).is_err(),
            "{body:?}"
        );
    }
    for parked in [
        Duration::ZERO,
        Duration::from_nanos(999),
        Duration::from_micros(u64::MAX),
        Duration::from_micros(u64::MAX) + Duration::from_nanos(999),
    ] {
        let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked))
            .expect("a stamp in the reader's domain encodes");
        let parsed = parse_grant_body(&body, TEST_NONCE, false).expect("the stamp parses");
        assert_eq!(parsed.sessions, vec![(7, 4242)]);
        assert_eq!(
            parsed.parked_for,
            Some(Duration::from_micros(
                u64::try_from(parked.as_micros()).expect("the ordinary boundary fits"),
            )),
        );
    }
    let refused = [
        Duration::from_micros(u64::MAX) + Duration::from_micros(1),
        Duration::MAX,
    ]
    .map(|parked| encode_grant_body(TEST_NONCE, &sessions, Some(parked)).is_none());
    assert!(
        refused.into_iter().all(|refused| refused),
        "GRANT_PARK_STAMP_ENCODER_EXCEEDS_READER_DOMAIN",
    );
}

#[test]
fn a_sessionless_grant_round_trips_only_to_a_successor_offered_it() {
    let body = encode_grant_body(TEST_NONCE, &[], Some(Duration::from_micros(7))).expect("encodes");
    assert_eq!(body, format!("{TEST_NONCE}\n\npark_us=7\n"));
    assert_eq!(
        parse_grant_body(&body, TEST_NONCE, true),
        Ok(GrantBody {
            sessions: Vec::new(),
            parked_for: Some(Duration::from_micros(7)),
        })
    );
    let unstamped = encode_grant_body(TEST_NONCE, &[], None).expect("encodes");
    assert_eq!(
        parse_grant_body(&unstamped, TEST_NONCE, true).map(|body| body.sessions),
        Ok(Vec::new())
    );
    for body in [&body, &unstamped] {
        assert_eq!(
            parse_grant_body(body, TEST_NONCE, false),
            Err(RendezvousError::Grant("no sessions".to_string())),
            "a successor not offered empty1 refuses the empty list, as every build \
             before it does"
        );
    }
    assert_eq!(
        grant_shape(false, true, 0),
        Ok(GrantShape::OneMessage),
        "the empty grant is one message carrying the two channel descriptors"
    );
    assert!(matches!(
        grant_shape(true, false, 0),
        Err(RendezvousError::TooManySessions { sessions: 0, .. })
    ));
    assert_eq!(grant_caps(false, false), None);
    assert_eq!(grant_caps(true, false).as_deref(), Some("chunks1"));
    assert_eq!(grant_caps(false, true).as_deref(), Some("empty1"));
    assert_eq!(grant_caps(true, true).as_deref(), Some("chunks1,empty1"));
    let offered = |caps: &str| offers_empty_grant(Some(std::ffi::OsStr::new(caps)));
    assert!(offered("empty1"));
    assert!(offered("chunks1,empty1"));
    assert!(!offered("chunks1"));
    assert!(!offered("empty10"));
    assert!(!offers_empty_grant(None));
}

#[test]
fn a_malformed_grant_body_is_refused_rather_than_partially_believed() {
    assert_eq!(
        parse_grant_body(&format!("{TEST_NONCE}\n1:2\n"), &"f".repeat(32), false),
        Err(RendezvousError::NonceMismatch),
        "a grant for another attempt"
    );
    for body in [
        String::new(),
        TEST_NONCE.to_string(),
        format!("{TEST_NONCE}\n"),
        format!("{TEST_NONCE}\n1:2"),
        format!("{TEST_NONCE}\n1:2,\n"),
        format!("{TEST_NONCE}\nx:2\n"),
        format!("{TEST_NONCE}\n1:y\n"),
        format!("{TEST_NONCE}\n1:0\n"),
        format!("{TEST_NONCE}\n1:-4\n"),
        format!("{TEST_NONCE}\n+7:42\n"),
        format!("{TEST_NONCE}\n07:42\n"),
        format!("{TEST_NONCE}\n00:42\n"),
        format!("{TEST_NONCE}\n7:+42\n"),
        format!("{TEST_NONCE}\n7:042\n"),
        format!("{TEST_NONCE}\n1:2\npark_us=00\n"),
        format!("{TEST_NONCE}\n1:2\npark_us=01500\n"),
        format!("{TEST_NONCE}\n1:2\npark_us=+1500\n"),
        format!("{TEST_NONCE}\n1:2,1:3\n"),
        format!("{TEST_NONCE}\n\n"),
    ] {
        assert!(
            parse_grant_body(&body, TEST_NONCE, false).is_err(),
            "{body:?} must not parse"
        );
    }
}

#[test]
fn a_grant_header_frames_its_body_exactly() {
    let header = grant_header(11).expect("fits");
    assert_eq!(&header[..GRANT_MAGIC.len()], GRANT_MAGIC);
    assert_eq!(
        u16::from_be_bytes([header[GRANT_MAGIC.len()], header[GRANT_MAGIC.len() + 1]]),
        11
    );
    assert!(
        grant_header(MAX_GRANT_BODY_BYTES + 1).is_none(),
        "an oversized body is refused, never truncated"
    );
}

#[test]
fn only_a_well_formed_nonce_can_be_granted() {
    let held = std::io::stdin();
    let sessions = vec![(1u64, 2i32, held.as_fd())];
    assert!(encode_grant_body("short", &sessions, None).is_none());
    assert!(encode_grant_body(&"z".repeat(32), &sessions, None).is_none());
    assert!(encode_grant_body(TEST_NONCE, &sessions, None).is_some());
}

#[test]
fn the_device_term_distinguishes_masters() {
    let ptys = (0..3).map(|_| open_pty()).collect::<Vec<_>>();
    let terms = ptys
        .iter()
        .map(|(master, _)| pty_device_term(*master).expect("a master answers fstat"))
        .collect::<Vec<_>>();
    let mut distinct = terms.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        terms.len(),
        "three simultaneously-open masters must have three different device numbers, or \
         the term could not exclude a substitution"
    );
    for (master, slave) in ptys {
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }
}

#[test]
fn an_expired_deadline_is_a_deadline_and_not_a_zero_timeout() {
    let expired = Instant::now() - Duration::from_secs(1);
    assert_eq!(remaining_io_budget(expired), Err(RendezvousError::Deadline));
    let live = Instant::now() + Duration::from_secs(5);
    assert!(remaining_io_budget(live).expect("live budget") >= MIN_IO_TIMEOUT);
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_code_check_judges_the_token_read_at_the_claim_and_only_for_its_pid() {
    let (a, _b) = CtlStream::pair().expect("pair");
    let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
    let code = DialerCode::this_process();
    let judged = |attested: i32, token: aterm_uds::peer::AuditToken| match admit_code(
        &a,
        attested,
        Some(token),
        &code,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        &|| false,
        &|_| {},
    ) {
        Ok(()) => Ok(()),
        Err(Gated::Refused(RendezvousError::PeerCode { refusal, .. })) => Err(refusal),
        Err(Gated::Refused(other)) => panic!("refused for no code check: {other}"),
        Err(Gated::WaitEnded { held, .. }) => trustd_silent(held),
    };
    if let Err(refusal) = judged(own_pid(), token) {
        panic!("the token names the attested dialer, which runs this code: {refusal:?}")
    }
    let elsewhere = judged(own_pid() + 1, token);
    assert!(
        matches!(&elsewhere, Err(CodeRefusal::Unchecked(why)) if why.contains("not the attested dialer")),
        "a token naming another pid was checked for the attested dialer: {elsewhere:?}"
    );
    let mut later = token;
    later.0[7] = later.0[7].wrapping_add(1);
    let stale = judged(own_pid(), later);
    assert!(
        matches!(stale, Err(CodeRefusal::Unchecked(_))),
        "a token for another image at the pid was judged as the live one: {stale:?}"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_check_answered_after_its_in_line_wait_is_waited_for_and_never_asked_again() {
    use aterm_keeper::identity::{CheckError, PeerCheck};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError, mpsc};

    const IN_LINE: Duration = Duration::from_millis(50);

    if !crate::test_alone::alone() {
        return;
    }
    let answer_at =
        2 * usize::try_from(CODE_RECHECK_INTERVAL.as_nanos() / ACCEPT_POLL_SLICE.as_nanos())
            .expect("a slice count");
    let through_the_hold = |cut: bool| {
        let (a, _b) = CtlStream::pair().expect("pair");
        let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
        let (trustd_answers, unanswered) = mpsc::channel::<()>();
        let unanswered = Arc::new(Mutex::new(unanswered));
        let asked = Arc::new(AtomicUsize::new(0));
        let answered = Arc::new(AtomicUsize::new(0));
        let code = DialerCode::scripted(IN_LINE, {
            let (asked, answered) = (Arc::clone(&asked), Arc::clone(&answered));
            move || {
                let again = asked.fetch_add(1, Ordering::SeqCst) > 0;
                assert!(
                    cut || !again,
                    "asked again while the first check was still waited for"
                );
                let (unanswered, answered) = (Arc::clone(&unanswered), Arc::clone(&answered));
                let check = PeerCheck::start(IN_LINE, move || {
                    let heard = unanswered
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .recv();
                    if heard.is_ok() {
                        answered.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                });
                if !cut {
                    return check;
                }
                let answer = check.wait(IN_LINE).unwrap_or_else(|abandoned| {
                    Err(CheckError::Unanswered(abandoned.unanswered()))
                });
                PeerCheck::start(IN_LINE, move || answer)
            }
        });
        let slices = std::cell::Cell::new(0usize);
        let asked_when_heard = std::cell::Cell::new(None::<usize>);
        let abort = || {
            let slice = slices.get() + 1;
            slices.set(slice);
            if slice == answer_at {
                trustd_answers
                    .send(())
                    .expect("the first check waits for trustd");
            }
            if asked_when_heard.get().is_none() && answered.load(Ordering::SeqCst) > 0 {
                asked_when_heard.set(Some(asked.load(Ordering::SeqCst)));
            }
            cut && asked_when_heard
                .get()
                .is_some_and(|then| asked.load(Ordering::SeqCst) > then)
        };
        let turned_away = std::cell::RefCell::new(Vec::new());
        let end = admit_code(
            &a,
            own_pid(),
            Some(token),
            &code,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &abort,
            &|dialer| turned_away.borrow_mut().push(dialer.clone()),
        );
        (
            end,
            asked.load(Ordering::SeqCst),
            answered.load(Ordering::SeqCst),
            slices.get(),
            turned_away.into_inner(),
        )
    };

    let (end, asked, answered, slices, turned_away) = through_the_hold(false);
    assert!(
        matches!(end, Ok(())),
        "a check trustd answered within the wait did not admit its dialer: {end:?}"
    );
    assert_eq!(asked, 1, "the check the gate let through was asked again");
    assert_eq!(answered, 1, "admitted on trustd's answer to that check");
    assert!(
        slices >= answer_at,
        "admitted only once trustd answered, {answer_at} held slices in: {slices}"
    );
    assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

    let (end, asked, answered, _, turned_away) = through_the_hold(true);
    assert!(
        matches!(
            end,
            Err(Gated::WaitEnded {
                end: RendezvousError::Cancelled,
                ..
            })
        ),
        "the shape that asks again admitted its dialer: {end:?}"
    );
    assert!(asked > 1, "the old shape asks again: {asked}");
    assert_eq!(
        answered, 1,
        "trustd answered the first check, and nobody heard it"
    );
    assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the trustd gate's helpers return",
        || (aterm_uds::deadline::TRUST.running() == 0).then_some(()),
    );
}

fn dial_and_count(path: PathBuf, claim: String, chunks: bool) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let stream = CtlStream::connect(&path).expect("dial the rendezvous");
        (&stream)
            .write_all(&claim_frame_with(&claim, chunks))
            .expect("present the claim");
        let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
        let mut received = 0;
        let mut buf = [0u8; 4096];
        while let Ok(got) = aterm_uds::fdpass::recv_with_fds(&stream, &mut buf, GRANT_MESSAGE_FDS) {
            if got.bytes == 0 && got.fds.is_empty() {
                break;
            }
            received += got.fds.len();
        }
        received
    })
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_grant_is_not_sent_when_the_dialers_token_is_not_the_one_checked() {
    let sessions = 100;
    let rendezvous = bind_for_test();
    let (reads, writes) = pipes_for_test(sessions);
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let dialer = dial_and_count(
        rendezvous.path().to_path_buf(),
        rendezvous.claim().to_string(),
        true,
    );
    let mut peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        ),
        "the dialer presents the claim and is this very process",
    );
    assert!(peer.chunks, "the dialer claimed the chunked grant");
    let mut later = peer.token.expect("the gate read the dialer's token");
    assert_eq!(
        later.pid(),
        std::process::id(),
        "the token the gate kept names the attested dialer"
    );
    later.0[7] = later.0[7].wrapping_add(1);
    peer.token = Some(later);
    let granted = writes
        .iter()
        .enumerate()
        .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
        .collect::<Vec<_>>();
    assert_eq!(
        grant_shape(true, false, sessions),
        Ok(GrantShape::Chunked),
        "PRECONDITION: {sessions} sessions take the chunked grant"
    );
    let refused = peer.transfer(
        TEST_NONCE,
        &granted,
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        None,
    );
    assert!(
        matches!(&refused, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
        "a chunked grant left for a token other than the one checked: {refused:?}"
    );
    drop((peer, rendezvous));
    assert_eq!(
        dialer.join().expect("dialer thread"),
        0,
        "the dialer received a descriptor the gate refused to send"
    );
    drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
}

#[cfg(target_vendor = "apple")]
const EXEC_DIALER_PATH: &str = "ATERM_TEST_EXEC_DIALER_RENDEZVOUS";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_CLAIM: &str = "ATERM_TEST_EXEC_DIALER_CLAIM";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_GO: &str = "ATERM_TEST_EXEC_DIALER_GO";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_EXECD: &str = "ATERM_TEST_EXEC_DIALER_EXECD";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_STAYS: &str = "ATERM_TEST_EXEC_DIALER_STAYS";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_FD: &str = "ATERM_TEST_EXEC_DIALER_FD";
#[cfg(target_vendor = "apple")]
const EXEC_DIALER_RECEIVED: &str = "EXEC DIALER RECEIVED";

#[cfg(target_vendor = "apple")]
fn count_until_closed(stream: &CtlStream) -> usize {
    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
    let mut received = 0;
    let mut buf = [0u8; 4096];
    while let Ok(got) = aterm_uds::fdpass::recv_with_fds(stream, &mut buf, GRANT_MESSAGE_FDS) {
        if got.bytes == 0 && got.fds.is_empty() {
            break;
        }
        received += got.fds.len();
    }
    received
}

#[cfg(target_vendor = "apple")]
fn exec_dialer_stand_in_if_asked() -> bool {
    use std::os::unix::process::CommandExt as _;
    if std::env::var_os(EXEC_DIALER_FD).is_some() {
        let stream = CtlStream::from(crate::test_alone::handed(EXEC_DIALER_FD));
        let execd = std::env::var_os(EXEC_DIALER_EXECD).expect("the marker's path");
        std::fs::write(execd, b"").expect("say the new image runs");
        println!(
            "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
            count_until_closed(&stream)
        );
        return true;
    }
    let (Some(path), Ok(claim), Some(go)) = (
        std::env::var_os(EXEC_DIALER_PATH),
        std::env::var(EXEC_DIALER_CLAIM),
        std::env::var_os(EXEC_DIALER_GO),
    ) else {
        return false;
    };
    let stream = dial_for_test(Path::new(&path), &claim).expect("dial and claim");
    let go = PathBuf::from(go);
    let told = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || go.exists().then_some(()),
    );
    assert!(told.is_some(), "the parent never said go");
    if std::env::var_os(EXEC_DIALER_STAYS).is_some() {
        println!(
            "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
            count_until_closed(&stream)
        );
        return true;
    }
    let fd = stream.into_raw_fd();
    let mut image = std::process::Command::new(std::env::current_exe().expect("this binary"));
    image.args(std::env::args_os().skip(1));
    crate::test_alone::hand(&mut image, EXEC_DIALER_FD, fd);
    let error = image.exec();
    panic!("exec the new image: {error}");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_grant_is_not_sent_once_the_dialers_image_changed() {
    if exec_dialer_stand_in_if_asked() {
        return;
    }
    for stays in [true, false] {
        let rendezvous = bind_for_test();
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let (go, execd) = (scratch.path().join("go"), scratch.path().join("execd"));
        let mut stand_in = crate::test_alone::StandIn::helper(
            module_path!(),
            "a_grant_is_not_sent_once_the_dialers_image_changed",
        );
        stand_in
            .command()
            .env(EXEC_DIALER_PATH, rendezvous.path())
            .env(EXEC_DIALER_CLAIM, rendezvous.claim())
            .env(EXEC_DIALER_GO, &go)
            .env(EXEC_DIALER_EXECD, &execd);
        if stays {
            stand_in.command().env(EXEC_DIALER_STAYS, "1");
        }
        let mut stand_ins = [stand_in.spawn()];
        let pid = i32::try_from(stand_ins[0].id()).expect("a pid fits pid_t");
        let peer = admitted(
            rendezvous.accept_claim(
                Some(pid),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the stand-in runs this binary's code and is the attested dialer",
        );
        assert_eq!(peer.pid(), pid, "LOCAL_PEERPID names the stand-in");
        std::fs::write(&go, b"").expect("say go");
        if !stays {
            crate::test_alone::until_or_ended(
                "the stand-in's new image runs",
                &mut stand_ins,
                || execd.exists(),
            );
        }
        let (reads, writes) = pipes_for_test(1);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let granted = peer.transfer(
            TEST_NONCE,
            &[(11, 4242, writes[0].as_fd())],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        );
        if stays {
            granted.expect("the dialer the gate checked, unchanged, is granted");
        } else {
            assert!(
                matches!(&granted, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
                "a grant left for an image the gate never checked: {granted:?}"
            );
        }
        drop((peer, rendezvous));
        let [stand_in] = stand_ins;
        let ran = stand_in.passed();
        let received = ran
            .stdout
            .lines()
            .find_map(|line| line.split(EXEC_DIALER_RECEIVED).nth(1))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| panic!("the stand-in never said what it received:\n{}", ran.stdout));
        assert_eq!(
            received,
            if stays { 1 + RENDEZVOUS_CHANNEL_FDS } else { 0 },
            "stays={stays}: what reached the dialer"
        );
        drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
    }
}

/// The parent hangs up right after the grant, 20 times (the window is a race), and the claim
/// still lands: a socket shut both ways answers `setsockopt(SO_RCVTIMEO)` with EINVAL on
/// Darwin, which the successor once took for a broken grant. The dialer's code is admitted at
/// the same-uid floor: its signature is not what this law is about, and checking a test
/// binary's code lists the whole directory it sits in (`SecCodeCopySelf` and the peer check
/// each walk the ~300,000 entries of `target/debug/deps`), some 0.9 s an iteration on an
/// idle machine and past a minute for the 20 under a full run.
#[test]
fn a_grant_whose_connection_closes_at_once_is_still_claimed() {
    let code = crate::handoff_rendezvous::DialerCode::same_uid_floor_for_test();
    for _ in 0..20 {
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &code,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the dialer presents the claim",
        );
        // SAFETY: `master` remains open for the duration of `transfer`.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect("transfer");
        drop(peer);
        drop(rendezvous);
        let claimed = dialer
            .join()
            .expect("dialer thread")
            .expect("a grant already queued is claimed though the parent hung up");
        assert_eq!(claimed.session_count(), 1);
        let received = claimed.masters[0].as_raw_fd();
        assert_eq!(
            claimed.fds_wire,
            format!("11={received}:4242"),
            "the wire names the descriptor at the number THIS process received it at"
        );
        assert_ne!(received, master, "SCM_RIGHTS installs a different number");
        assert_eq!(
            pty_device_term(received),
            pty_device_term(master),
            "the device term survives the transfer"
        );
        drop(claimed);
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }
}

#[test]
fn a_successor_process_claims_from_the_real_outgoing_listener() {
    const CHILD: &str = "RZ_CROSS_PROCESS_SUCCESSOR";
    if let Some(spec) = std::env::var_os(CHILD) {
        let spec = spec.into_string().expect("the child spec is UTF-8");
        let mut fields = spec.split('\n');
        let mut field = || fields.next().expect("a complete child spec");
        let (path, claim) = (PathBuf::from(field()), field().to_string());
        let parent_pid: libc::pid_t = field().parse().expect("the parent pid");
        let term: i32 = field().parse().expect("the master's device term");
        let parent = crate::seamless::AttestedParent::attest_live_for_test(parent_pid);
        assert!(
            parent.is_some(),
            "the outgoing process attests by its birth record"
        );
        let claimed = dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(120), parent, false)
            .expect("the successor's identity gate admits the real outgoing listener");
        assert_eq!(claimed.session_count(), 1);
        assert_eq!(
            pty_device_term(claimed.masters[0].as_raw_fd()),
            Some(term),
            "the granted master is the outgoing process's PTY"
        );
        return;
    }

    let rendezvous = bind_for_test();
    let (master, slave) = open_pty();
    let term = pty_device_term(master).expect("a master answers fstat");
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let spec = format!(
        "{}\n{}\n{}\n{term}",
        rendezvous.path().display(),
        rendezvous.claim(),
        own_pid()
    );
    let mut child = crate::test_alone::StandIn::this_test();
    child.command().env(CHILD, spec);
    let child = std::cell::RefCell::new(child.spawn());
    let successor = i32::try_from(child.borrow().id()).expect("a pid fits pid_t");

    let turned_away = std::cell::RefCell::new(Vec::new());
    let claimed = rendezvous.accept_claim_observed(
        Some(successor),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        &|| child.borrow_mut().exited(),
        &|dialer| turned_away.borrow_mut().push(dialer.clone()),
    );
    let peer = match claimed {
        Ok(peer) => peer,
        Err(error) => {
            let turned_away = turned_away.into_inner();
            if held_for_trustd(&error)
                || turned_away
                    .iter()
                    .any(|dialer| matches!(dialer.refusal, CodeRefusal::TrustdSilent(_)))
            {
                trustd_silent(format_args!("{error}: {turned_away:?}"));
            }
            child.into_inner().passed();
            panic!("the successor process passed without presenting its claim: {error}");
        }
    };
    assert_eq!(
        peer.pid(),
        successor,
        "LOCAL_PEERPID on accept names the successor"
    );
    // SAFETY: `master` remains open for the duration of `transfer`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
    peer.transfer(
        TEST_NONCE,
        &[(11, 4242, borrowed)],
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        None,
    )
    .expect("transfer");

    child.into_inner().passed();
    drop((ready_rd, ready_wr, commit_rd, commit_wr));
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

#[test]
fn a_transfer_failure_says_whether_the_descriptors_left() {
    let rendezvous = bind_for_test();
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let (fake_master_rd, _fake_master_wr) = pipe_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();

    let (close_tx, close_rx) = std::sync::mpsc::channel::<()>();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
    let dialer = std::thread::spawn(move || {
        let stream = CtlStream::connect(&path).expect("dial");
        (&stream)
            .write_all(&claim_frame(&claim))
            .and_then(|()| (&stream).flush())
            .expect("claim");
        close_rx.recv().expect("told to close");
        drop(stream);
        closed_tx.send(()).expect("signal");
    });
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        ),
        "the claim was presented",
    );
    close_tx.send(()).expect("tell the dialer to close");
    closed_rx.recv().expect("the dialer closed");
    dialer.join().expect("dialer thread");
    wait_for_hangup(|| peer.poll_hangup());
    let borrowed = fake_master_rd.as_fd();
    let error = peer
        .transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect_err("a closed peer cannot take the descriptors");
    assert!(
        matches!(error, RendezvousError::TransferNotSent(_)),
        "the sendmsg itself failed, so no descriptor left: {error}"
    );
    drop(peer);
    drop(rendezvous);

    let rendezvous = bind_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let dialer = std::thread::spawn(move || {
        let stream = CtlStream::connect(&path).expect("dial");
        (&stream)
            .write_all(&claim_frame(&claim))
            .and_then(|()| (&stream).flush())
            .expect("claim");
        let mut header = [0u8; GRANT_HEADER_LEN];
        let received = fdpass::recv_with_fds(&stream, &mut header, GRANT_MESSAGE_FDS)
            .expect("the header and its descriptors");
        let count = received.fds.len();
        drop(received);
        drop(stream);
        count
    });
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        ),
        "claimed",
    );
    let outcome = peer.transfer(
        TEST_NONCE,
        &[(11, 4242, fake_master_rd.as_fd())],
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        None,
    );
    let received = dialer.join().expect("dialer thread");
    assert_eq!(received, 3, "the master and both pipes reached the peer");
    match outcome {
        Ok(()) => {}
        Err(RendezvousError::TransferPartial(_)) => {}
        Err(other) => panic!(
            "after a delivered header the failure can never claim the descriptors \
             stayed home: {other}"
        ),
    }
    drop((ready_rd, ready_wr, commit_rd, commit_wr));
}

#[test]
fn a_dialer_without_the_secret_is_refused() {
    let rendezvous = bind_for_test();
    let stream = CtlStream::connect(rendezvous.path()).expect("connect");
    (&stream)
        .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
        .and_then(|()| (&stream).flush())
        .expect("the dialer presents a wrong secret");
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let dialer = stream.as_raw_fd();
    // The attempt's clock reaches its deadline once the gate has refused and hung up on
    // the dialer, so the wait ends after the refusal however slowly this thread runs.
    set_io_clock_for_test(Some(Box::new(move || {
        if hung_up_on(dialer) {
            deadline
        } else {
            Instant::now()
        }
    })));
    let refused = rendezvous.accept_claim(
        Some(own_pid()),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        deadline,
        &|| false,
    );
    set_io_clock_for_test(None);
    assert_eq!(
        refused.err(),
        Some(RendezvousError::Claim),
        "the wait that outlives a refusal ends naming it"
    );
    drop(stream);
}

/// Whether the rendezvous closed its end of `dialer`'s connection: a peek reads end of file.
fn hung_up_on(dialer: std::os::fd::RawFd) -> bool {
    let mut byte = 0u8;
    // SAFETY: `dialer` is an open socket the caller owns, the peek writes at most the one
    // byte `byte` holds, and `MSG_DONTWAIT` keeps it from blocking.
    let peeked = unsafe {
        libc::recv(
            dialer,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    peeked == 0
}

#[test]
fn a_wrong_dialer_does_not_end_the_attempt_and_the_successor_still_claims() {
    let rendezvous = bind_for_test();
    let (master, slave) = open_pty();
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let path = rendezvous.path().to_path_buf();
    let claim = rendezvous.claim().to_string();

    let bogus = CtlStream::connect(&path).expect("the wrong dialer connects");
    (&bogus)
        .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
        .and_then(|()| (&bogus).flush())
        .expect("the wrong dialer presents a wrong secret");
    bogus
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("bound the wrong dialer's own read");

    let successor = std::thread::spawn(move || {
        dial_and_claim(
            &path,
            &claim,
            TEST_NONCE,
            deadlines_in(120),
            own_parent(),
            false,
        )
    });
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        ),
        "the wrong dialer is refused and the wait resumes for the real successor",
    );
    // SAFETY: `master` remains open for the duration of `transfer`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
    let parked_at = Instant::now().checked_sub(Duration::from_millis(5));
    peer.transfer(
        TEST_NONCE,
        &[(11, 4242, borrowed)],
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        parked_at,
    )
    .expect("transfer");
    let claimed = successor
        .join()
        .expect("dialer thread")
        .expect("the successor behind a wrong dialer still claims its handoff");
    assert_eq!(claimed.session_count(), 1);
    assert!(
        parked_at.is_none()
            || claimed
                .parked_before_grant()
                .is_some_and(|parked| parked >= Duration::from_millis(5)),
        "the park stamp crossed the real socket: {:?}",
        claimed.parked_before_grant()
    );

    let mut sink = [0u8; 1];
    assert_eq!(
        (&bogus).read(&mut sink).ok(),
        Some(0),
        "and the refused connection was CLOSED, which is how the wrong dialer learns it lost"
    );

    drop(claimed);
    drop((bogus, ready_rd, ready_wr, commit_rd, commit_wr));
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

#[test]
fn a_flood_of_wrong_dialers_is_bounded_rather_than_served_forever() {
    let rendezvous = bind_for_test();
    let path = rendezvous.path().to_path_buf();
    let wrong = claim_frame(&"f".repeat(CLAIM_HEX_LEN));
    let flood = (0..MAX_REFUSED_DIALS)
        .map(|_| {
            let stream = CtlStream::connect(&path).expect("connect");
            (&stream)
                .write_all(&wrong)
                .and_then(|()| (&stream).flush())
                .expect("present a wrong claim");
            stream
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    let refused = rendezvous.accept_claim(
        Some(own_pid()),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        Instant::now() + Duration::from_secs(120),
        &|| false,
    );
    assert_eq!(refused.err(), Some(RendezvousError::Claim));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "the cap ended the attempt; nothing here waited out the deadline: {elapsed:?}"
    );
    drop(flood);
}

#[test]
fn a_dribbling_dialer_is_refused_at_the_deadline_rather_than_stretching_it() {
    let rendezvous = bind_for_test();
    let path = rendezvous.path().to_path_buf();
    let claim = rendezvous.claim().to_string();
    let code = crate::handoff_rendezvous::DialerCode::this_process();
    let stream = CtlStream::connect(&path).expect("connect");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dribbler = {
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut wrote = 0usize;
            for byte in claim_frame(&claim) {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if (&stream).write_all(&[byte]).is_err() {
                    break;
                }
                wrote += 1;
                let _ = (&stream).flush();
                std::thread::sleep(Duration::from_millis(10));
            }
            wrote
        })
    };

    let budget = Duration::from_millis(300);
    let refused =
        rendezvous.accept_claim(Some(own_pid()), &code, Instant::now() + budget, &|| false);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    assert!(
        refused.is_err(),
        "a frame that does not arrive inside the deadline is refused, not awaited"
    );
    let wrote = dribbler.join().expect("dribbler thread");
    assert!(
        wrote < CLAIM_FRAME_LEN,
        "the frame read waited past the attempt's deadline for the whole \
         {CLAIM_FRAME_LEN}-byte frame"
    );
}

#[test]
fn half_a_rendezvous_environment_says_so_instead_of_claiming_to_be_neither() {
    let some = |value: &str| Some(std::ffi::OsString::from(value));
    let secret = "a".repeat(CLAIM_HEX_LEN);

    assert_eq!(
        rendezvous_env(some("/tmp/aterm/seamless.sock"), None).err(),
        Some(RendezvousError::HalfRendezvous {
            present: ENV_RENDEZVOUS,
            missing: ENV_CLAIM,
        })
    );
    assert_eq!(
        rendezvous_env(None, some(&secret)).err(),
        Some(RendezvousError::HalfRendezvous {
            present: ENV_CLAIM,
            missing: ENV_RENDEZVOUS,
        })
    );
    assert_eq!(
        rendezvous_env(None, None).err(),
        Some(RendezvousError::NoRendezvous),
        "and an empty environment is still plain absence"
    );
    assert_eq!(
        rendezvous_env(some("/tmp/aterm/seamless.sock"), some(&secret)).expect("both halves"),
        (PathBuf::from("/tmp/aterm/seamless.sock"), secret),
    );
}

#[test]
fn a_rendezvous_nobody_dials_expires_and_unlinks() {
    let rendezvous = bind_for_test();
    let path = rendezvous.path().to_path_buf();
    assert!(path.exists(), "the listener is on disk while it is bound");
    let expired = rendezvous.accept_claim(
        Some(own_pid()),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        Instant::now() + Duration::from_millis(50),
        &|| false,
    );
    assert_eq!(expired.err(), Some(RendezvousError::Deadline));
    drop(rendezvous);
    assert!(
        !path.exists(),
        "a dropped rendezvous unlinks, so a late dial fails ENOENT rather than hanging"
    );
    assert!(
        CtlStream::connect(&path).is_err(),
        "and a late dialer cannot connect to what is no longer there"
    );
}

#[test]
fn an_aborted_wait_returns_promptly() {
    let rendezvous = bind_for_test();
    let started = Instant::now();
    let cancelled = rendezvous.accept_claim(
        Some(own_pid()),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        Instant::now() + Duration::from_secs(120),
        &|| true,
    );
    assert_eq!(cancelled.err(), Some(RendezvousError::Cancelled));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "the abort predicate is checked between poll slices, not after the deadline: \
         {elapsed:?}"
    );
}

fn deadlines_in(secs: u64) -> ClaimDeadlines {
    let at = Instant::now() + Duration::from_secs(secs);
    ClaimDeadlines {
        dial: at,
        grant: at,
    }
}

#[test]
fn a_held_claim_is_granted_after_the_hold_under_the_grant_budget() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let rendezvous = bind_for_test();
    let (master, slave) = open_pty();
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let dial = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let deadlines = ClaimDeadlines {
        dial,
        grant: dial + 2 * aterm_spec::wait::MUST_HAPPEN,
    };
    let reads = Arc::new(AtomicUsize::new(0));
    let ended = Arc::new(AtomicBool::new(false));
    let dialer = {
        let (reads, ended) = (Arc::clone(&reads), Arc::clone(&ended));
        std::thread::spawn(move || {
            set_io_clock_for_test(Some(Box::new(move || {
                if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    Instant::now()
                } else {
                    dial + MIN_IO_TIMEOUT
                }
            })));
            let claimed = dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false);
            ended.store(true, Ordering::SeqCst);
            claimed
        })
    };
    let accepted = rendezvous.accept_claim(
        Some(own_pid()),
        &crate::handoff_rendezvous::DialerCode::this_process(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        &|| ended.load(Ordering::SeqCst),
    );
    if accepted.is_err() && ended.load(Ordering::SeqCst) {
        let gave_up = dialer.join().expect("dialer thread").err();
        panic!("the dialer ended before its claim was accepted: {gave_up:?}");
    }
    let peer = admitted(accepted, "claimed");
    assert!(!peer.poll_hangup(), "a held, idle dialer is not a hangup");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the dialer's grant wait judged its budget past the dial",
        || (reads.load(Ordering::SeqCst) >= 2).then_some(()),
    );
    assert!(
        !peer.poll_hangup(),
        "…nor once the hold outlasted its dial budget"
    );
    // SAFETY: `master` remains open for the duration of `transfer`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
    peer.transfer(
        TEST_NONCE,
        &[(11, 4242, borrowed)],
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        None,
    )
    .expect("transfer after the hold");
    let claimed = dialer
        .join()
        .expect("dialer thread")
        .expect("claimed after the hold");
    assert_eq!(claimed.session_count(), 1);
    drop(claimed);
    drop((ready_rd, ready_wr, commit_rd, commit_wr));
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

#[test]
fn an_accept_short_of_a_descriptor_ends_the_wait_because_the_kernel_dropped_the_dial() {
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let rendezvous = bind_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let parent = own_parent();
    let code = crate::handoff_rendezvous::DialerCode::this_process();
    let mut filler = Vec::new();
    loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => filler.push(file),
            Err(error) if error.raw_os_error() == Some(libc::EMFILE) => break,
            Err(error) => panic!("filling the table: {error}"),
        }
    }
    drop(filler.pop());
    let dialer = std::thread::spawn(move || {
        let started = Instant::now();
        let outcome = dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(180), parent, false);
        (outcome, started.elapsed())
    });
    let queued_by = Instant::now() + Duration::from_secs(60);
    loop {
        let mut pollfd = libc::pollfd {
            fd: rendezvous.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single initialized `pollfd` for the open listener.
        if unsafe { libc::poll(&mut pollfd, 1, 50) } > 0 {
            break;
        }
        assert!(Instant::now() < queued_by, "the dial never queued");
    }

    let started = Instant::now();
    let starved = rendezvous.accept_claim(
        Some(own_pid()),
        &code,
        Instant::now() + Duration::from_secs(180),
        &|| false,
    );
    match starved {
        Err(RendezvousError::Accept(detail)) => assert!(
            detail.contains(&std::io::Error::from_raw_os_error(libc::EMFILE).to_string()),
            "the attempt says what ended it: {detail}"
        ),
        Err(other) => panic!("expected the accept's own error, got {other}"),
        Ok(_) => panic!("PRECONDITION: the table was full"),
    }
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "ended at once, not at the deadline: {:?}",
        started.elapsed()
    );
    let (outcome, elapsed) = dialer.join().expect("dialer thread");
    match outcome {
        Err(RendezvousError::Grant(reason)) => {
            assert_eq!(reason, "the parent closed the rendezvous");
        }
        Err(RendezvousError::Dial(_)) => {}
        Err(other) => panic!("expected the dropped dial's end, got {other}"),
        Ok(_) => panic!("expected the dropped dial's end, got a grant"),
    }
    assert!(
        elapsed < aterm_spec::wait::MUST_HAPPEN,
        "the dialer is told at once, not at its grant budget: {elapsed:?}"
    );
    drop(filler);
    drop(rendezvous);
}

#[test]
fn a_grant_that_never_arrives_ends_as_the_claim_deadline() {
    let rendezvous = bind_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let deadlines = ClaimDeadlines {
        dial: Instant::now() + Duration::from_secs(60),
        grant: Instant::now() + Duration::from_millis(300),
    };
    let dialer = std::thread::spawn(move || {
        dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false)
    });
    let held = rendezvous
        .accept_one(Instant::now() + Duration::from_secs(60), &|| false)
        .expect("accepted");
    let outcome = dialer.join().expect("dialer thread");
    assert!(
        Instant::now() >= deadlines.grant,
        "the wait lasted the grant budget"
    );
    match outcome {
        Err(RendezvousError::Deadline) => {}
        Err(other) => panic!("expected the grant budget's deadline, got {other}"),
        Ok(_) => panic!("expected the grant budget's deadline, got a grant"),
    }
    assert_eq!(
        claim_side(RendezvousError::Deadline),
        RendezvousError::ClaimDeadline,
        "which the claim says as this successor's own deadline"
    );
    drop(held);
}

fn wait_for_hangup(hung_up: impl Fn() -> bool) {
    let seen = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || hung_up().then_some(()),
    );
    assert!(seen.is_some(), "the closed dialer never read as a hangup");
}

#[cfg(unix)]
#[test]
fn the_rendezvous_base_is_admitted_only_private_absolute_and_off_tmp() {
    use super::{BaseFacts, admits_temp_base};
    use std::path::Path;
    let uid = 501;
    let private = BaseFacts {
        is_dir: true,
        uid,
        mode: 0o40700,
    };
    let base = Path::new("/var/folders/xy/T");
    assert!(admits_temp_base(base, Some(private), uid), "a private base");
    let readable = BaseFacts {
        mode: 0o40755,
        ..private
    };
    assert!(
        admits_temp_base(base, Some(readable), uid),
        "others may read it, as long as they cannot write it"
    );
    for (why, base, facts) in [
        ("relative", Path::new("tmp"), Some(private)),
        ("/tmp itself", Path::new("/tmp"), Some(private)),
        ("under /tmp", Path::new("/tmp/x"), Some(private)),
        (
            "under /private/tmp",
            Path::new("/private/tmp/x"),
            Some(private),
        ),
        ("unreadable", base, None),
        (
            "a symlink or a file",
            base,
            Some(BaseFacts {
                is_dir: false,
                ..private
            }),
        ),
        (
            "another uid's",
            base,
            Some(BaseFacts {
                uid: uid + 1,
                ..private
            }),
        ),
        (
            "group-writable",
            base,
            Some(BaseFacts {
                mode: 0o40770,
                ..private
            }),
        ),
        (
            "other-writable",
            base,
            Some(BaseFacts {
                mode: 0o40707,
                ..private
            }),
        ),
    ] {
        assert!(!admits_temp_base(base, facts, uid), "{why} is admitted");
    }
}

fn bind_for_test() -> Rendezvous {
    let nonce = aterm_uds::rand::hex_token::<16>()
        .unwrap_or_else(|error| panic!("a rendezvous test cannot mint its nonce: {error}"));
    Rendezvous::bind(&nonce).unwrap_or_else(|error| panic!("bind a rendezvous: {error}"))
}

fn open_pty() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

fn pipe_for_test() -> (OwnedFd, OwnedFd) {
    let mut raw = [0i32; 2];
    // SAFETY: `pipe` initializes two file descriptors in `raw`.
    assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: `raw` holds two freshly created pipe descriptors owned exclusively here.
    unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
}

fn pipes_for_test(count: usize) -> (Vec<OwnedFd>, Vec<OwnedFd>) {
    (0..count).map(|_| pipe_for_test()).unzip()
}

fn inode(fd: i32) -> (libc::dev_t, libc::ino_t) {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes one `struct stat` into `info`.
    assert_eq!(unsafe { libc::fstat(fd, info.as_mut_ptr()) }, 0, "fstat");
    // SAFETY: `fstat` returned 0, so `info` is initialized.
    let info = unsafe { info.assume_init() };
    (info.st_dev, info.st_ino)
}

fn assert_every_write_end_closed(reads: &[OwnedFd], what: &str) {
    for read in reads {
        // SAFETY: `F_SETFL` on a valid descriptor owned by this test.
        unsafe {
            libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    for (index, read) in reads.iter().enumerate() {
        loop {
            let mut byte = [0u8; 1];
            // SAFETY: 1-byte read into a stack buffer from a valid descriptor.
            let got = unsafe {
                libc::read(
                    read.as_raw_fd(),
                    byte.as_mut_ptr().cast::<libc::c_void>(),
                    1,
                )
            };
            if got == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: descriptor {index} is still open somewhere in this process"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn body_for(sessions: usize) -> String {
    let entries = (0..sessions)
        .map(|id| format!("{id}:{}", 5000 + id))
        .collect::<Vec<_>>()
        .join(",");
    format!("{TEST_NONCE}\n{entries}\n")
}

#[test]
fn the_chunk_layout_places_every_descriptor_and_the_pipes_last() {
    for sessions in 1..=MAX_CHUNKED_SESSIONS {
        let sizes = grant_chunk_sizes(sessions);
        assert_eq!(
            sizes.iter().sum::<usize>(),
            sessions + RENDEZVOUS_CHANNEL_FDS,
            "{sessions}: every descriptor, once"
        );
        assert!(
            sizes
                .iter()
                .all(|size| (1..=GRANT_MESSAGE_FDS).contains(size)),
            "{sessions}: {sizes:?} fits the message ceiling"
        );
        assert!(
            *sizes.last().expect("a message") >= RENDEZVOUS_CHANNEL_FDS,
            "{sessions}: the readiness and Commit ends travel last"
        );
        assert!(sizes.len() <= MAX_GRANT_CHUNKS, "{sessions}: {sizes:?}");
        assert_eq!(
            sizes.len() == 1,
            sessions <= MAX_RENDEZVOUS_SESSIONS,
            "{sessions}: one message exactly when one message carries it"
        );
    }
    assert_eq!(
        grant_chunk_sizes(MAX_CHUNKED_SESSIONS).len(),
        MAX_GRANT_CHUNKS,
        "the protocol ceiling spans the most messages the header allows"
    );
}

#[test]
fn both_claim_magics_carry_the_secret_and_say_which_grant() {
    let secret = "a".repeat(CLAIM_HEX_LEN);
    assert_eq!(
        claim_frame_kind(&claim_frame_with(&secret, false), &secret),
        Some(false)
    );
    assert_eq!(
        claim_frame_kind(&claim_frame_with(&secret, true), &secret),
        Some(true)
    );
    assert_eq!(
        claim_frame_kind(&claim_frame_with(&secret, true), &"b".repeat(CLAIM_HEX_LEN)),
        None,
        "the chunked claim still needs this attempt's secret"
    );
    let mut unknown = claim_frame_with(&secret, true);
    unknown[4] = b'9';
    assert_eq!(claim_frame_kind(&unknown, &secret), None);
    assert_eq!(&claim_frame(&secret)[..6], b"ATRZ1C", "the default claim");
}

#[test]
fn only_the_parents_offer_makes_a_chunked_claim() {
    let offered = |caps: &str| offers_chunked_grant(Some(std::ffi::OsStr::new(caps)));
    assert!(offered("chunks1"));
    assert!(offered("later2, chunks1"));
    assert!(!offered(""));
    assert!(!offered("chunks2"));
    assert!(!offered("chunks10"));
    assert!(!offers_chunked_grant(None));
}

#[test]
fn a_successor_raises_its_descriptor_limit_before_claiming() {
    use crate::descriptor_limit::{raise_descriptor_limit_for_claim, raised_soft_limit};
    assert_eq!(raised_soft_limit(256, u64::MAX), Some(4096));
    assert_eq!(raised_soft_limit(256, 1024), Some(1024));
    assert_eq!(raised_soft_limit(10_240, u64::MAX), None, "never lowered");
    assert_eq!(raised_soft_limit(1024, 1024), None, "at the hard limit");
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let raised = raise_descriptor_limit_for_claim().expect("the raise is permitted");
    let (from, to) = raised.expect("a 64-spare limit is below the floor");
    assert!(from < to, "{from} -> {to}");
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid resource constant and writable `rlimit` out-pointer.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
        0
    );
    assert_eq!(limit.rlim_cur, to);
    a_grant_of_100_sessions_crosses_in_chunks();
    crate::descriptor_limit::tests::a_shell_spawned_after_the_raise_gets_the_launchers_limit(
        from, to,
    );
}

#[test]
fn a_grant_past_its_deadline_says_the_successor_dialed_and_sends_nothing() {
    for (sessions, chunks) in [(3_usize, false), (100, true)] {
        let rendezvous = bind_for_test();
        let (_reads, writes) = pipes_for_test(sessions);
        let (_ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, _commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                chunks,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the claim is this attempt's",
        );
        let handed = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("an instant in the past");
        let error = peer
            .transfer(
                TEST_NONCE,
                &handed,
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                expired,
                None,
            )
            .expect_err("a grant past its deadline is not sent");
        assert_eq!(error, RendezvousError::GrantDeadline, "{sessions} sessions");
        drop((peer, rendezvous));
        let received = dialer.join().expect("dialer thread");
        assert!(
            received.is_err(),
            "{sessions} sessions: the dialer received no grant"
        );
    }
}

#[test]
fn a_grant_of_100_sessions_crosses_in_chunks() {
    let rendezvous = bind_for_test();
    let (reads, writes) = pipes_for_test(100);
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let dialer = std::thread::spawn(move || {
        dial_and_claim(
            &path,
            &claim,
            TEST_NONCE,
            deadlines_in(120),
            own_parent(),
            true,
        )
    });
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(60),
            &|| false,
        ),
        "the chunked claim is this attempt's",
    );
    assert!(peer.chunks, "the dialer claimed the chunked grant");
    let sessions = writes
        .iter()
        .enumerate()
        .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
        .collect::<Vec<_>>();
    peer.transfer(
        TEST_NONCE,
        &sessions,
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + Duration::from_secs(60),
        None,
    )
    .expect("a chunked grant of 100 sessions is sent");
    let claimed = dialer
        .join()
        .expect("dialer thread")
        .expect("the chunked grant is taken whole");
    assert_eq!(claimed.session_count(), 100);
    for (id, (master, sent)) in claimed.masters.iter().zip(&writes).enumerate() {
        assert_eq!(
            inode(master.as_raw_fd()),
            inode(sent.as_raw_fd()),
            "session {id} is the descriptor sent for it"
        );
    }
    assert_eq!(
        inode(claimed.ready.as_raw_fd()),
        inode(ready_wr.as_raw_fd())
    );
    assert_eq!(
        inode(claimed.commit.as_raw_fd()),
        inode(commit_rd.as_raw_fd())
    );
    let wire = claimed.fds_wire.split(',').collect::<Vec<_>>();
    assert_eq!(wire.len(), 100);
    for (id, entry) in wire.iter().enumerate() {
        assert!(
            entry.starts_with(&format!("{id}=")) && entry.ends_with(&format!(":{}", 5000 + id)),
            "the wire pairs session {id} with its shell: {entry}"
        );
    }
    drop((claimed, peer, rendezvous));
    drop((writes, ready_wr, commit_wr, ready_rd, commit_rd));
    drop(reads);
}

#[test]
fn an_atrz1c_claim_never_receives_a_chunked_grant() {
    use std::io::Read as _;

    let rendezvous = bind_for_test();
    let (reads, writes) = pipes_for_test(100);
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let claim = rendezvous.claim().to_string();
    let path = rendezvous.path().to_path_buf();
    let dialer = std::thread::spawn(move || {
        dial_and_claim(
            &path,
            &claim,
            TEST_NONCE,
            deadlines_in(120),
            own_parent(),
            false,
        )
    });
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(60),
            &|| false,
        ),
        "the one-message claim is this attempt's",
    );
    assert!(!peer.chunks);
    let sessions = writes
        .iter()
        .enumerate()
        .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
        .collect::<Vec<_>>();
    assert_eq!(
        peer.transfer(
            TEST_NONCE,
            &sessions,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(60),
            None,
        ),
        Err(RendezvousError::TooManySessions {
            sessions: 100,
            limit: MAX_RENDEZVOUS_SESSIONS,
        }),
        "refused before any descriptor left"
    );
    drop((peer, rendezvous));
    assert!(
        matches!(
            dialer.join().expect("dialer thread"),
            Err(RendezvousError::Grant(_))
        ),
        "the ATRZ1C successor reads the close and takes nothing"
    );
    drop((writes, ready_wr, commit_wr, ready_rd, commit_rd, reads));

    let rendezvous = bind_for_test();
    let (master, slave) = open_pty();
    let (ready_rd, ready_wr) = pipe_for_test();
    let (commit_rd, commit_wr) = pipe_for_test();
    let dialer = CtlStream::connect(rendezvous.path()).expect("connect");
    (&dialer)
        .write_all(&claim_frame_with(rendezvous.claim(), true))
        .expect("claim");
    let peer = admitted(
        rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(60),
            &|| false,
        ),
        "the chunked claim is this attempt's",
    );
    // SAFETY: `master` remains open for the duration of `transfer`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
    let sessions = [
        (0u64, 4000, borrowed),
        (1, 4001, borrowed),
        (7, 4002, borrowed),
    ];
    peer.transfer(
        TEST_NONCE,
        &sessions,
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + Duration::from_secs(60),
        None,
    )
    .expect("three sessions are granted");
    dialer
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("a read bound");
    let mut header = [0u8; GRANT_HEADER_LEN];
    let received =
        fdpass::recv_with_fds(&dialer, &mut header, GRANT_MESSAGE_FDS).expect("the header");
    assert_eq!(received.bytes, GRANT_HEADER_LEN);
    assert_eq!(received.fds.len(), 5, "every descriptor in the one message");
    let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("a body");
    assert_eq!(
        header,
        grant_header(body.len()).expect("a header"),
        "ATRZ1G, byte for byte"
    );
    let mut sent_body = vec![0u8; body.len()];
    (&dialer).read_exact(&mut sent_body).expect("the body");
    assert_eq!(sent_body, body.as_bytes());
    drop((received, dialer, peer, rendezvous));
    drop((ready_rd, ready_wr, commit_rd, commit_wr));
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);

    let (reads, writes) = pipes_for_test(3);
    let outcome = stub_grant(
        &writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>(),
        &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
            let body = body_for(1);
            let header = grant_chunks_header(body.len(), 1).expect("a header");
            fdpass::send_with_fds(stream, &header, fds)?;
            (&*stream).write_all(body.as_bytes())
        },
        false,
    );
    assert_eq!(
        outcome,
        Err(RendezvousError::Grant("wrong magic".to_string())),
        "a successor that never claimed the chunked grant refuses it"
    );
    drop(writes);
    assert_every_write_end_closed(&reads, "a refused ATRZ2G");
}

fn stub_grant(
    fds: &[i32],
    send: &mut (dyn FnMut(&CtlStream, &[BorrowedFd<'_>]) -> std::io::Result<()> + Send),
    chunks: bool,
) -> Result<(), RendezvousError> {
    use std::io::Read as _;

    let dir = crate::test_scratch::short_dir("a stub parent's socket");
    let path = dir.path().join("p.sock");
    let listener = aterm_uds::CtlListener::bind(&path).expect("a stub listener");
    let secret = "c".repeat(CLAIM_HEX_LEN);
    let outcome = std::thread::scope(|scope| {
        let parent = scope.spawn(|| {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("the dial");
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a read bound");
            let mut claim = [0u8; CLAIM_FRAME_LEN];
            (&stream).read_exact(&mut claim).expect("the claim");
            let borrowed = fds
                .iter()
                // SAFETY: caller keeps every descriptor in `fds` open until `transfer` returns.
                .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) })
                .collect::<Vec<_>>();
            let _ = send(&stream, &borrowed);
            stream
        });
        let outcome = dial_and_claim(
            &path,
            &secret,
            TEST_NONCE,
            deadlines_in(60),
            own_parent(),
            chunks,
        )
        .map(drop);
        drop(parent.join().expect("the stub parent"));
        outcome
    });
    drop((listener, dir));
    outcome
}

#[test]
fn a_short_chunk_is_refused_and_closes_every_fd() {
    for (what, second) in [
        ("a short second chunk", Some(30usize)),
        ("no second chunk", None),
    ] {
        let (reads, writes) = pipes_for_test(100 + RENDEZVOUS_CHANNEL_FDS);
        let raw = writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>();
        let sizes = grant_chunk_sizes(100);
        assert_eq!(sizes, [64, 38], "the layout this test shortens");
        let outcome = stub_grant(
            &raw,
            &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
                let body = body_for(100);
                let header = grant_chunks_header(body.len(), sizes.len()).expect("a header");
                fdpass::send_with_fds(stream, &header, &fds[..sizes[0]])?;
                (&*stream).write_all(body.as_bytes())?;
                match second {
                    Some(short) => {
                        let frame = grant_continuation(1).expect("a continuation");
                        fdpass::send_with_fds(stream, &frame, &fds[sizes[0]..sizes[0] + short])?;
                    }
                    None => stream.shutdown(std::net::Shutdown::Write)?,
                }
                Ok(())
            },
            true,
        );
        match second {
            Some(_) => assert_eq!(
                outcome,
                Err(RendezvousError::DescriptorCount {
                    expected: 102,
                    received: 94,
                }),
                "{what}: refused"
            ),
            None => assert!(
                matches!(outcome, Err(RendezvousError::Grant(_))),
                "{what}: refused, not {outcome:?}"
            ),
        }
        drop(writes);
        assert_every_write_end_closed(&reads, what);
    }
}

fn step_grant(
    model: &aterm_spec::derive::Model,
    state: &mut std::collections::BTreeMap<&'static str, i64>,
    action: &'static str,
) {
    let successors = model.successors(action, state);
    assert_eq!(successors.len(), 1, "{action} is not admitted at {state:?}");
    *state = successors[0].clone();
}

/// The code [`grant_disagreement`] reads against the model.
#[derive(Clone, Copy)]
struct GrantCode<'a> {
    decide: &'a dyn Fn(bool, bool, usize) -> Result<GrantShape, RendezvousError>,
    read: &'a dyn Fn(&str, &str, bool) -> Result<GrantBody, RendezvousError>,
    /// What a park captures of a desk with a tab or none, with a lone Workers list or none,
    /// for a target whose read declared `desk1` or did not.
    capture: &'a dyn Fn(bool, bool, bool) -> crate::restore::RestoreManifest,
    /// The parent has no layout to send (`outgoing_layout_wire`).
    nothing: &'a dyn Fn(&crate::restore::RestoreManifest) -> bool,
    /// A successor with `desk1`'s code, or a build before it, places what it was handed.
    take: &'a dyn Fn(&crate::restore::RestoreManifest, bool) -> bool,
}

/// A window for the sweep: one with a tab, or one whose list stands alone.
fn swept_window(tab: bool) -> crate::restore::WindowLayout {
    let mut window = crate::restore::WindowLayout::lone_workers_list(24, 80);
    if tab {
        window.workers_in_front = false;
        window.restored_tabs = vec![crate::restore::RestoredTab {
            root: crate::restore::RestoredSplitTree::leaf(crate::restore::RestoredView::Native(
                crate::restore::NativeLeafRestore::settings("/".to_string()),
            )),
            focused_path: Vec::new(),
            zoomed: false,
            parked: false,
            ephemeral: false,
            fleet: false,
        }];
    }
    window
}

fn swept_desk(tab: bool, lone_list: bool) -> Vec<crate::restore::WindowLayout> {
    [
        tab.then(|| swept_window(true)),
        lone_list.then(|| swept_window(false)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn captured(tab: bool, lone_list: bool, read: bool) -> crate::restore::RestoreManifest {
    let mut windows = swept_desk(tab, lone_list);
    windows.retain(|window| window.kept_in_layout(read));
    crate::restore::RestoreManifest::new(windows)
}

/// A build with `desk1`'s code places a lone list. One before it reads only tabs, which is
/// why a parent never sends it one.
fn placed(layout: &crate::restore::RestoreManifest, desk_capable: bool) -> bool {
    if desk_capable {
        !layout.is_empty()
    } else {
        layout.windows.iter().any(|window| !window.holds_no_tab())
    }
}

fn shipped_code() -> GrantCode<'static> {
    GrantCode {
        decide: &grant_shape,
        read: &parse_grant_body,
        capture: &captured,
        nothing: &crate::restore::RestoreManifest::is_empty,
        take: &placed,
    }
}

fn grant_disagreement(code: GrantCode<'_>) -> Option<String> {
    let GrantCode {
        decide,
        read,
        capture,
        nothing,
        take,
    } = code;
    let model = aterm_models_update::native_update_rendezvous_grant_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let bools = [false, true];
    for (capable, declared, offered, large) in bools.into_iter().flat_map(|a| {
        bools.into_iter().flat_map(move |b| {
            bools
                .into_iter()
                .flat_map(move |c| bools.into_iter().map(move |d| (a, b, c, d)))
        })
    }) {
        for (empty_capable, empty_declared, empty_offered, sessionless) in
            bools.into_iter().flat_map(|a| {
                bools.into_iter().flat_map(move |b| {
                    bools
                        .into_iter()
                        .flat_map(move |c| bools.into_iter().map(move |d| (a, b, c, d)))
                })
            })
        {
            if (declared && !capable)
                || (offered && !declared)
                || (empty_declared && !empty_capable)
                || (empty_offered && !empty_declared)
                || (large && sessionless)
            {
                continue;
            }
            for (desk_capable, desk_declared, desk_read, workers_window, workers_only) in
                bools.into_iter().flat_map(|a| {
                    bools.into_iter().flat_map(move |b| {
                        bools.into_iter().flat_map(move |c| {
                            bools
                                .into_iter()
                                .flat_map(move |d| bools.into_iter().map(move |e| (a, b, c, d, e)))
                        })
                    })
                })
            {
                if (desk_declared && !desk_capable)
                    || (desk_read && !desk_declared)
                    || (workers_only && !(sessionless && workers_window))
                {
                    continue;
                }
                let mut state = model.init_state();
                for (taken, action) in [
                    (capable, "Upgrade"),
                    (declared, "Declare"),
                    (empty_capable, "UpgradeEmpty"),
                    (empty_declared, "DeclareEmpty"),
                    (desk_capable, "UpgradeDesk"),
                    (desk_declared, "DeclareDesk"),
                    (large, "Grow"),
                    (sessionless, "Empty"),
                    (workers_window, "WorkersWindow"),
                    (workers_only, "WorkersOnly"),
                    (offered, "Offer"),
                    (empty_offered, "OfferEmpty"),
                    (desk_read, "ReadDesk"),
                    (true, "Launch"),
                ] {
                    if taken {
                        step_grant(&model, &mut state, action);
                    }
                }
                let layout = capture(!workers_only, workers_window, desk_read);
                if state["carried"] != i64::from(layout.carries_lone_workers_list()) {
                    return Some(format!(
                        "a lone list with desk read={desk_read}: the capture carries {}",
                        layout.carries_lone_workers_list()
                    ));
                }
                let nothing_to_send = nothing(&layout);
                let caps = grant_caps(offered, empty_offered).map(std::ffi::OsString::from);
                let claims_chunks = capable && offers_chunked_grant(caps.as_deref());
                if model.action_enabled("ClaimChunks", &state) != claims_chunks
                    || model.action_enabled("ClaimOne", &state) == claims_chunks
                {
                    return Some(format!("the claim of capable={capable} offered={offered}"));
                }
                let secret = "d".repeat(CLAIM_HEX_LEN);
                let claimed = claim_frame_kind(&claim_frame_with(&secret, claims_chunks), &secret)
                    .expect("the claim carries the secret");
                step_grant(
                    &model,
                    &mut state,
                    if claimed { "ClaimChunks" } else { "ClaimOne" },
                );
                let pools: &[usize] = if sessionless {
                    &[0]
                } else if large {
                    &[MAX_RENDEZVOUS_SESSIONS + 1, 100, MAX_CHUNKED_SESSIONS]
                } else {
                    &[1, 3, MAX_RENDEZVOUS_SESSIONS]
                };
                for &sessions in pools {
                    let decided = decide(claimed, empty_offered, sessions);
                    let expected = (
                        matches!(decided, Ok(GrantShape::OneMessage)) && !nothing_to_send,
                        matches!(decided, Ok(GrantShape::Chunked)) && !nothing_to_send,
                        matches!(decided, Err(RendezvousError::TooManySessions { .. }))
                            || nothing_to_send,
                    );
                    let modelled = (
                        model.action_enabled("GrantOne", &state),
                        model.action_enabled("GrantChunks", &state),
                        model.action_enabled("RefuseTooMany", &state)
                            || model.action_enabled("RefuseEmpty", &state)
                            || model.action_enabled("RefuseDesk", &state),
                    );
                    if expected != modelled {
                        return Some(format!(
                            "{sessions} sessions for claim chunks={claimed} empty \
                             offered={empty_offered} desk read={desk_read} alone \
                             list={workers_only}: the parent decides {decided:?} with \
                             nothing={nothing_to_send}, the model enables {modelled:?}"
                        ));
                    }
                }
                if sessionless {
                    // Where the sound parent refuses, the Buggy model's parent sends the
                    // empty grant it never offered and the lone list it never read a
                    // declaration of, so the successor's refusal of that grant is read
                    // against the model's Refuse as its adoption of an offered one is against
                    // Adopt.
                    let parent = if model.action_enabled("GrantOne", &state) {
                        &model
                    } else {
                        &buggy
                    };
                    for action in ["GrantOne", "Send", "Take"] {
                        step_grant(parent, &mut state, action);
                    }
                    let reads = empty_capable && offers_empty_grant(caps.as_deref());
                    let sent = capture(
                        !workers_only,
                        workers_window,
                        desk_read || std::ptr::eq(parent, &buggy),
                    );
                    let adopted = read(&format!("{TEST_NONCE}\n\n"), TEST_NONCE, reads).is_ok()
                        && take(&sent, desk_capable);
                    if parent.action_enabled("Adopt", &state) != adopted
                        || parent.action_enabled("Refuse", &state) == adopted
                    {
                        return Some(format!(
                            "a sessionless grant to empty capable={empty_capable} \
                             offered={empty_offered} and desk capable={desk_capable} alone \
                             list={workers_only}: the successor adopts={adopted}"
                        ));
                    }
                }
            }
        }
    }
    None
}

#[test]
fn rendezvous_grant_conformance() {
    let shipped = shipped_code();
    assert_eq!(grant_disagreement(shipped), None);
    assert!(
        grant_disagreement(GrantCode {
            decide: &|_, empty, sessions| grant_shape(true, empty, sessions),
            ..shipped
        })
        .is_some(),
        "a parent that ignores the claim must disagree with the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            decide: &|chunks, _, sessions| grant_shape(chunks, true, sessions),
            ..shipped
        })
        .is_some(),
        "a parent that sends an empty grant it never offered must disagree with the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            read: &|body, nonce, _| parse_grant_body(body, nonce, true),
            ..shipped
        })
        .is_some(),
        "a successor that adopts an empty grant it was never offered must disagree with the \
         model"
    );
    assert!(
        grant_disagreement(GrantCode {
            capture: &|tab, lone_list, _| captured(tab, lone_list, true),
            ..shipped
        })
        .is_some(),
        "a capture that carries a lone list to a target with no `desk1` must disagree with \
         the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            capture: &|tab, lone_list, _| captured(tab, lone_list, false),
            ..shipped
        })
        .is_some(),
        "a capture that never carries a lone list must disagree with the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            nothing: &|layout| layout.windows.iter().all(|w| w.holds_no_tab()),
            ..shipped
        })
        .is_some(),
        "a parent that reads a lone list as nothing to send must disagree with the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            take: &|layout, _| placed(layout, true),
            ..shipped
        })
        .is_some(),
        "a build before `desk1` that places a lone list must disagree with the model"
    );
    assert!(
        grant_disagreement(GrantCode {
            take: &|layout, _| placed(layout, false),
            ..shipped
        })
        .is_some(),
        "a successor that refuses the lone list it declared must disagree with the model"
    );
}
