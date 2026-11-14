// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The transaction clock reads on every host aterm builds for and never runs back, so a
/// prepare's deadline on it can pass or hold (on Windows it read nothing, and every prepare
/// was abandoned).
#[test]
fn the_transaction_clock_reads_and_never_runs_back() {
    let first = monotonic_us().expect("the host's transaction clock");
    let second = monotonic_us().expect("the host's transaction clock");
    assert!(second >= first, "{second} < {first}");
}

/// The host observes this very process on every platform it reads: a birth and a parent, the
/// same identity on a second read, and the image [`Image::at`] names for its own executable.
#[test]
fn the_host_observes_this_process_and_the_image_it_runs() {
    let me = Process::observe(std::process::id()).expect("this process");
    assert_eq!(me.pid, std::process::id());
    assert_ne!(me.birth, 0);
    assert_ne!(me.parent, 0);
    if cfg!(windows) {
        assert_eq!(
            (me.group, me.session),
            (0, 0),
            "Windows keeps neither, so no leader check passes"
        );
    }
    assert_eq!(
        Process::observe(std::process::id()),
        Some(me.clone()),
        "a second read agrees"
    );
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        Image::at(&exe),
        Some(me.image),
        "the image is its own executable's file"
    );
    assert_eq!(
        Image::at(exe.parent().unwrap()),
        None,
        "a directory is no image"
    );
}

#[test]
fn durable_rejection_is_an_explicit_canonical_wire_requirement() {
    let legacy = fixture();
    let legacy_wire = legacy.encode().unwrap();
    assert!(!legacy_wire.contains("reject="));
    assert_eq!(Request::parse(&legacy_wire), Some(legacy.clone()));
    let Request::Prepare(mut marked) = legacy else {
        unreachable!()
    };
    let domain = "ab".repeat(32);
    marked.rejection_domain = Some(domain.clone());
    let marked = Request::Prepare(marked);
    let wire = marked.encode().unwrap();
    assert!(wire.contains(&format!(" reject={domain}")));
    assert_eq!(Request::parse(&wire), Some(marked));
    assert_eq!(
        Request::parse(&wire.replace(&format!("reject={domain}"), "reject=unknown")),
        None
    );
    assert_eq!(Request::parse(&(wire + &format!(" reject={domain}"))), None);
    assert_eq!(
        Request::parse("rejection-domain"),
        Some(Request::RejectionDomain)
    );
    assert_eq!(
        Reply::parse(&format!(
            "OK native-act state=rejection-domain domain={domain}\n"
        )),
        Some(Reply::RejectionDomain(domain))
    );
    assert_eq!(
        Reply::parse("OK native-act state=rejection-domain domain=bad\n"),
        None
    );
}

pub(super) fn fixture() -> Request {
    let image = Image {
        path: b"/private/image \xff".to_vec(),
        device: 7,
        inode: 19,
    };
    Request::prepare(Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: "12".repeat(16),
        host_epoch: "34".repeat(16),
        holder: "grok-alab@4242".into(),
        revision: 2,
        expected_human_seq: None,
        deadline_us: 91,
        duty: Scope::Upgrade,
        plan: NativePlan::OwnImage {
            child: Process {
                pid: 100,
                birth: 2,
                parent: 4242,
                group: 100,
                session: 1,
                image: image.clone(),
            },
            target: image,
            version: "0.1.0+g123".into(),
        },
        boot: None,
        custody: None,
    })
}

#[test]
fn unadmitted_lapsed_and_closed_replies_name_exactly_one_lower_hex_operation_id() {
    let id = "ab".repeat(16);
    for (state, expected) in [
        ("unadmitted", Reply::Unadmitted(id.clone())),
        ("lapsed", Reply::Lapsed(id.clone())),
        ("closed", Reply::Closed(id.clone())),
    ] {
        let reply = |id: &str| match state {
            "lapsed" => lapsed_reply(id),
            "closed" => closed_reply(id),
            _ => unadmitted_reply(id),
        };
        let line = reply(&id).expect("a canonical id");
        assert_eq!(line, format!("OK native-act state={state} id={id}\n"));
        assert_eq!(Reply::parse(&line), Some(expected));
        for bad in [
            format!("OK native-act state={state}\n"),
            format!("OK native-act state={state} id=\n"),
            format!("OK native-act state={state} id={}\n", "AB".repeat(16)),
            format!("OK native-act state={state} id={}\n", "ab".repeat(15)),
            format!(
                "OK native-act state={state} id={id} cap={}\n",
                "cd".repeat(32)
            ),
            format!("OK native-act state={state} {id}\n"),
        ] {
            assert_eq!(Reply::parse(&bad), None, "{bad}");
        }
        assert_eq!(reply(&"AB".repeat(16)), None);
        assert_eq!(reply("ab"), None);
    }
}

/// An operation closes [`COMPLETION_US`] after its deadline, which no wire field moves: longer
/// than a constructor waits for its new agent, so a grant delivered just before the deadline is
/// still observed and committed before the close.
#[test]
fn the_close_follows_the_deadline_by_more_than_the_wait_for_the_new_agent() {
    let held = super::super::Timings::PRODUCTION.held;
    assert!(
        held.as_micros() < u128::from(COMPLETION_US),
        "{held:?} reaches the close"
    );
    let Request::Prepare(mut prepare) = fixture() else {
        unreachable!("the fixture is a prepare")
    };
    assert_eq!(prepare.close_us(), prepare.deadline_us + COMPLETION_US);
    let wire = Request::Prepare(prepare.clone()).encode().unwrap();
    assert!(
        !wire.contains(&prepare.close_us().to_string()),
        "the close is derived, never sent"
    );
    prepare.deadline_us = u64::MAX - 1;
    assert_eq!(prepare.close_us(), u64::MAX, "the close saturates");
}

/// A Commit answered `closed` for its own operation is [`Decision::Closed`], which claims nothing
/// about execution; the same answer for another operation is Unknown.
#[test]
fn a_commit_answered_closed_for_its_own_operation_decides_closed() {
    struct Answer(std::io::Cursor<Vec<u8>>);
    impl std::io::Read for Answer {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(bytes)
        }
    }
    impl std::io::Write for Answer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let binding = Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    };
    for (answered, decision) in [
        (binding.id.clone(), Decision::Closed),
        ("cd".repeat(16), Decision::Unknown),
    ] {
        let line = closed_reply(&answered).unwrap();
        let mut client = crate::RelayClient::new(Answer(std::io::Cursor::new(line.into_bytes())));
        assert_eq!(
            commit(
                &mut client,
                "session",
                &binding,
                ImagePhase::Forward,
                "0.1.0"
            ),
            decision,
            "{answered}"
        );
    }
}

#[test]
fn stamps_are_optional_canonical_words_that_change_only_a_stamped_spelling() {
    let Request::Prepare(unstamped) = fixture() else {
        unreachable!()
    };
    let legacy = Request::Prepare(unstamped.clone()).encode().unwrap();
    assert!(!legacy.contains(" boot=") && !legacy.contains(" custody="));
    let boot = BootStamp {
        host: format!("darwin:{}", "1f".repeat(16)),
        boot: "2e".repeat(16),
    };
    let custody = "3d".repeat(32);
    let mut stamped = (*unstamped).clone();
    stamped.boot = Some(boot.clone());
    stamped.custody = Some(custody.clone());
    let wire = Request::prepare(stamped.clone()).encode().unwrap();
    assert_eq!(
        wire,
        format!(
            "{legacy} boot={}:{} custody={custody}",
            boot.host, boot.boot
        )
    );
    assert_eq!(
        Request::parse(&wire),
        Some(Request::prepare(stamped.clone()))
    );
    let mut boot_only = stamped.clone();
    boot_only.custody = None;
    assert_eq!(
        Request::parse(&wire.replace(&format!(" custody={custody}"), "")),
        Some(Request::prepare(boot_only))
    );
    for bad in [
        format!("{legacy} boot={}", boot.host),
        format!("{legacy} boot={}", boot.boot),
        format!("{legacy} boot=darwin:{}:{}", "1F".repeat(16), boot.boot),
        format!("{legacy} boot={}:{}", boot.host, "2e".repeat(15)),
        format!("{legacy} boot=:{}:{}", "1f".repeat(16), boot.boot),
        format!("{legacy} custody={}", "3d".repeat(31)),
        format!("{legacy} custody={}", "3D".repeat(32)),
        format!("{wire} custody={custody}"),
    ] {
        assert_eq!(Request::parse(&bad), None, "{bad}");
    }
}

#[test]
fn native_wire_preserves_unix_bytes_and_one_original_deadline() {
    let request = fixture();
    let wire = request.encode().expect("canonical plan");
    assert_eq!(Request::parse(&wire), Some(request));
    assert!(wire.contains("deadline-us=91"));
    assert!(wire.is_ascii());
    assert_eq!(
        Request::parse(&wire.replace("deadline-us=91", "deadline-us=091")),
        None
    );
    assert_eq!(Request::parse(&(wire.clone() + " deadline-us=92")), None);
    assert_eq!(Request::parse(&(wire + " unrecognized=1")), None);
}

#[test]
fn recipient_writer_discovery_is_explicit_and_strict() {
    assert_eq!(
        Request::parse("recipient-writer-support"),
        Some(Request::RecipientWriterSupport)
    );
    assert_eq!(
        Request::RecipientWriterSupport.encode().as_deref(),
        Some("recipient-writer-support")
    );
    for support in [
        RecipientWriterSupport::Unavailable,
        RecipientWriterSupport::BoundRecipientsV1,
    ] {
        let line = format!(
            "OK native-act state=recipient-writer-support writer={}\n",
            support.word()
        );
        assert_eq!(
            Reply::parse(&line),
            Some(Reply::RecipientWriterSupport(support))
        );
    }
    for malformed in [
        "recipient-writer-support writer=none",
        "recipient-writer-support writer=bound-recipients-v1",
        "recipient-writer-support extra=1",
    ] {
        assert_eq!(Request::parse(malformed), None);
    }
    for malformed in [
        "OK native-act state=recipient-writer-support",
        "OK native-act state=recipient-writer-support writer=recipients5",
        "OK native-act state=recipient-writer-support writer=bound-recipients5",
        "OK native-act state=recipient-writer-support writer=true",
        "OK native-act state=recipient-writer-support writer=unknown",
        "OK native-act state=recipient-writer-support writer=none writer=bound-recipients-v1",
        "OK native-act state=recipient-writer-support writer=bound-recipients-v1 native-act=1",
        "OK native-act state=recipient-writer-support ATermNativeRecipientRecovery=bound-recipients5",
        "OK native-act state=info writer=bound-recipients5",
    ] {
        assert_eq!(Reply::parse(malformed), None, "{malformed}");
    }
}

#[test]
fn initial_cancel_is_discovered_separately_and_echoes_the_exact_original_plan() {
    assert_eq!(
        Request::parse("cancel-support"),
        Some(Request::CancelSupport)
    );
    assert_eq!(
        Request::CancelSupport.encode().as_deref(),
        Some("cancel-support")
    );
    assert_eq!(
        Reply::parse("OK native-act state=cancel-support cancel-prepare=1\n"),
        Some(Reply::CancelSupport)
    );
    for malformed in [
        "OK native-act state=cancel-support",
        "OK native-act state=cancel-support cancel-prepare=2",
        "OK native-act state=cancel-support cancel-prepare=1 native-act=1",
    ] {
        assert_eq!(Reply::parse(malformed), None);
    }
    let Request::Prepare(original) = fixture() else {
        unreachable!()
    };
    let actor = Process {
        pid: 4242,
        birth: 17,
        parent: 1,
        group: 4242,
        session: 1,
        image: Image {
            path: b"/original-supervisor".to_vec(),
            device: 7,
            inode: 11,
        },
    };
    let cancel = Request::cancel_prepare((*original).clone(), actor);
    let Request::CancelPrepare(expected) = cancel.clone() else {
        unreachable!()
    };
    let wire = cancel.encode().unwrap();
    assert!(wire.starts_with("cancel-prepare "));
    assert_eq!(Request::parse(&wire), Some(cancel));
    assert_eq!(
        Request::parse(wire.split(" actor-pid=").next().unwrap()),
        None
    );
    let receipt = format!(
        "OK native-act state=cancelled request={}\n",
        aterm_codec::hex::encode_lower(wire.as_bytes())
    );
    assert_eq!(
        Reply::parse(&receipt),
        Some(Reply::Cancelled(expected.clone()))
    );
    assert_eq!(
        Reply::parse(&(receipt.trim_end().to_string() + " extra=1")),
        None
    );
    let changed = wire.replace("deadline-us=91", "deadline-us=092");
    assert_eq!(
        Reply::parse(&format!(
            "OK native-act state=cancelled request={}",
            aterm_codec::hex::encode_lower(changed.as_bytes())
        )),
        None
    );
    assert_eq!(Request::parse(&(wire + " epoch=00")), None);
    let mut different = expected.clone();
    different.prepare.deadline_us += 1;
    assert_ne!(Reply::Cancelled(different), Reply::Cancelled(expected));
}

#[test]
fn constructor_discovery_is_closed_canonical_and_separate_from_info() {
    assert_eq!(
        Request::Constructors.encode().as_deref(),
        Some("constructors")
    );
    assert_eq!(Request::parse("constructors"), Some(Request::Constructors));
    for (words, expected) in [
        ("-", Vec::new()),
        ("own-image", vec![Constructor::OwnImage]),
        ("private-restart", vec![Constructor::PrivateRestart]),
        ("cold-restart", vec![Constructor::ColdRestart]),
        ("asked-upgrade", vec![Constructor::AskedUpgrade]),
        ("asked-relaunch", vec![Constructor::AskedRelaunch]),
        (
            "own-image,private-restart",
            vec![Constructor::OwnImage, Constructor::PrivateRestart],
        ),
        (
            "own-image,private-restart,cold-restart",
            vec![
                Constructor::OwnImage,
                Constructor::PrivateRestart,
                Constructor::ColdRestart,
            ],
        ),
        (
            "own-image,private-restart,cold-restart,asked-upgrade,asked-relaunch",
            Constructor::ALL.to_vec(),
        ),
    ] {
        assert_eq!(
            Reply::parse(&format!("OK native-act state=constructors plans={words}")),
            Some(Reply::Constructors(expected))
        );
    }
    for words in [
        "",
        "own-image,own-image",
        "private-restart,own-image",
        "cold-restart,own-image",
        "own-image,cold-restart,private-restart",
        "cold-restart,cold-restart",
        "cold-restart,unknown",
        "unknown",
        "own-image,unknown",
        "- extra=1",
    ] {
        assert_eq!(
            Reply::parse(&format!("OK native-act state=constructors plans={words}")),
            None
        );
    }
    assert_eq!(Request::parse("constructors plans=own-image"), None);
    let info = format!(
        "OK native-act state=info epoch={} native-act=1 handoff=close clock=monotonic-us own-image=grok-alab:upgrade",
        "34".repeat(16)
    );
    assert!(matches!(Reply::parse(&info), Some(Reply::Info { .. })));
    // A host that still spells its handoff `blocked` held a granted operation's update hold with
    // no close: a constructor built on the close refuses it.
    assert_eq!(
        Reply::parse(&info.replace("handoff=close", "handoff=blocked")),
        None
    );
    assert_eq!(Reply::parse(&(info + " plans=own-image")), None);
}

#[test]
fn typing_duties_invalid_capabilities_and_noncanonical_paths_cannot_enter_native_wire() {
    let Request::Prepare(mut request) = fixture() else {
        unreachable!()
    };
    for duty in [
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
    ] {
        request.duty = duty;
        assert!(Request::Prepare(request.clone()).encode().is_none());
    }
    request.duty = Scope::Upgrade;
    let NativePlan::OwnImage { target, .. } = &mut request.plan else {
        unreachable!()
    };
    target.path = b"relative".to_vec();
    assert!(Request::Prepare(request).encode().is_none());
    let binding = Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    };
    for request in [
        Request::Query(binding.clone()),
        Request::Recover(binding.clone()),
        Request::Abort(binding.clone()),
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0".into()),
    ] {
        assert_eq!(Request::parse(&request.encode().unwrap()), Some(request));
    }
    assert!(
        Request::Query(Binding {
            capability: "C".repeat(64),
            ..binding
        })
        .encode()
        .is_none()
    );
}

#[test]
fn final_commit_transport_failure_is_unknown_and_never_rollback_safe() {
    struct Closed;
    impl std::io::Read for Closed {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }
    impl std::io::Write for Closed {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut client = crate::RelayClient::new(Closed);
    let binding = Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    };
    assert_eq!(
        commit(
            &mut client,
            "session",
            &binding,
            ImagePhase::Forward,
            "0.1.0"
        ),
        Decision::Unknown
    );
    assert_eq!(Reply::parse("OK native-act state=committed extra=1"), None);
    assert_eq!(
        Reply::parse("OK native-act state=rollback-safe"),
        Some(Reply::Decision(Decision::ProvenNotCommittedRollbackSafe))
    );
    assert!(!format!("{binding:?}").contains(&binding.capability));
}
