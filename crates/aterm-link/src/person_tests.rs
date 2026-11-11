// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_cap_stdout_did_not_take_whole_is_refused() {
    assert_eq!(cap_on_stdout(Ok(())), Ok(()));
    for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::StorageFull] {
        let refused = cap_on_stdout(Err(kind.into())).expect_err("no cap was handed over");
        assert!(
            refused.starts_with("aterm fabric mint-for: the cap was not written whole"),
            "{kind:?}: {refused}"
        );
    }
}

#[test]
fn a_lifetime_is_days_or_hours() {
    assert_eq!(parse_lifetime("30d"), Ok(30 * 24 * 3_600_000));
    assert_eq!(parse_lifetime("12h"), Ok(12 * 3_600_000));
    for bad in [
        "0d", "0h", "d", "30", "30m", "-1d", "+3h", "1.5d", "99999d", "",
    ] {
        assert!(parse_lifetime(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_person_ring_stamped_with_an_expiry_reads_its_expiry() {
    let exp = 1_800_000_000_000u64;
    let stamped: Vec<String> = crate::enable::person_rules("lab", "h-andrew")
        .iter()
        .map(|g| with_expiry(g, exp))
        .collect();
    assert!(
        stamped
            .iter()
            .any(|g| g.starts_with(&format!("rw,p=h-andrew,exp={exp}:"))),
        "{stamped:?}"
    );
    for g in &stamped {
        assert_eq!(astream_cap::expires_at(g), Some(exp), "{g}");
    }
}

#[test]
fn a_person_is_h_or_a_dash_and_a_principal() {
    assert!(is_person_id("h-andrew"));
    assert!(is_person_id("a-review"));
    for bad in ["n-0123", "s-0123", "h-", "h-UPPER", "h-a b", "andrew", ""] {
        assert!(!is_person_id(bad), "{bad}");
    }
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

#[test]
fn each_verb_parses_its_words_and_refuses_the_rest() {
    let halt = parse("halt", &words("--cap-file c main is broken")).unwrap();
    assert_eq!(
        halt,
        Some(PersonCmd {
            act: Act::Halt(Some("main is broken".to_string())),
            cap_files: vec!["c".to_string()],
        })
    );
    let off = parse("halt", &words("off --cap-file c")).unwrap().unwrap();
    assert_eq!(off.act, Act::Halt(None));
    let id = "0123456789abcdef0123456789abcdef";
    for (tail, here) in [("", false), (" --here", true)] {
        let lift = parse("halt", &words(&format!("off {id}{tail} --cap-file c")))
            .unwrap()
            .unwrap();
        let want = Act::Lift {
            id: id.to_string(),
            here,
        };
        assert_eq!(lift.act, want, "off <id>{tail}");
    }
    assert!(
        parse("halt", &words("off 0123 --cap-file c")).is_err(),
        "not a halt id"
    );
    let flag_first = parse("halt", &words(&format!("--here --cap-file c off {id}")));
    assert!(
        matches!(
            flag_first,
            Ok(Some(PersonCmd {
                act: Act::Lift { here: true, .. },
                ..
            }))
        ),
        "{flag_first:?}"
    );
    assert!(
        parse("halt", &words(&format!("off {id} extra --cap-file c"))).is_err(),
        "a mistyped halt-off never halts"
    );
    assert!(
        parse("halt", &words("--here --cap-file c main is broken")).is_err(),
        "a halt is never for one Mac"
    );
    assert!(
        parse("halt", &words("--here off --cap-file c")).is_err(),
        "--here names an id"
    );
    assert!(parse("halt", &words("--cap-file c")).is_err());
    assert!(parse("halt", &words("main is broken")).is_err(), "no cap");

    let post = parse(
        "post",
        &words("--cap-file c to=@s-a@n-b kind=answer re=@12 -- yes, ship it"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        post.act,
        Act::Post {
            sid: "s-a".to_string(),
            node: "n-b".to_string(),
            kind: "answer".to_string(),
            re: Some(12),
            text: "yes, ship it".to_string(),
        }
    );
    for re in ["5", "@5"] {
        let post = parse(
            "post",
            &words(&format!("--cap-file c to=@s-a@n-b re={re} hi")),
        )
        .unwrap()
        .unwrap();
        assert!(
            matches!(post.act, Act::Post { re: Some(5), .. }),
            "re={re}: {:?}",
            post.act
        );
    }
    for re in ["+5", "@", "", "@-1"] {
        let refused = parse(
            "post",
            &words(&format!("--cap-file c to=@s-a@n-b re={re} hi")),
        );
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains(&format!("re={re}"))),
            "re={re}: refused, naming the key and the value: {refused:?}"
        );
    }
    for bad in [
        "--cap-file c to=@s-a kind=note hi",
        "--cap-file c to=s-a@n-b kind=note hi",
        "--cap-file c to=@s-a@n-b kind=ack hi",
        "--cap-file c to=@s-a@n-b kind=note",
        "--cap-file c kind=note hi",
        "--cap-file c to=@s-a@n-b re=x note hi",
    ] {
        assert!(parse("post", &words(bad)).is_err(), "{bad}");
    }

    let inbox = parse("inbox", &words("--peek --cap-file c"))
        .unwrap()
        .unwrap();
    assert_eq!(inbox.act, Act::Inbox { peek: true });
    assert!(parse("inbox", &words("--cap-file c extra")).is_err());
    assert_eq!(parse("inbox", &words("--help")).unwrap(), None);
}

#[test]
fn a_post_delimiter_makes_key_shaped_words_literal_message_text() {
    for text in [
        "kind=task",
        "re=17 --cap-file literal.cap",
        "--help kind=answer",
    ] {
        let parsed = parse(
            "post",
            &words(&format!("--cap-file c to=@s-a@n-b -- {text}")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            parsed.act,
            Act::Post {
                sid: "s-a".to_string(),
                node: "n-b".to_string(),
                kind: "note".to_string(),
                re: None,
                text: text.to_string(),
            }
        );
        assert_eq!(parsed.cap_files, ["c"]);
    }
}

#[cfg(unix)]
fn guarded_person(
    person: &str,
    legacy_halt: bool,
) -> (
    aterm_tempfile::TempDir,
    astream_broker::BrokerHandle,
    Config,
    PersonCmd,
) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-p")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let secret = vec![0x51; 32];
    let broker =
        astream_broker::Broker::open_guarded(dir.path().join("bus.log"), secret.clone()).unwrap();
    let socket = dir.path().join("bus.sock");
    let server = broker.serve(&socket).unwrap();
    let cap_path = dir.path().join("person.cap");
    let mut grants = crate::enable::person_rules("f", person);
    if legacy_halt {
        grants.push(format!("rw,p={person}:/f/f/fleet/{person}/>"));
        grants.sort();
        grants.dedup();
    }
    let caps: String = grants
        .iter()
        .map(|grant| {
            let cap = astream_cap::mint(&secret, grant).unwrap();
            format!(
                "{}\n",
                astream_cap::capfile::format_line(&cap.filter, &cap.tag)
            )
        })
        .collect();
    crate::bridge::write_owner_only(&cap_path, caps);
    let cfg = unix_cfg(dir.path(), &socket);
    let cmd = PersonCmd {
        act: Act::Halt(Some("stop".to_string())),
        cap_files: vec![cap_path.to_string_lossy().into_owned()],
    };
    (dir, server, cfg, cmd)
}

/// Fleet `f` on the broker socket `socket`.
#[cfg(unix)]
fn unix_cfg(dir: &std::path::Path, socket: &std::path::Path) -> Config {
    Config {
        fleet: "f".to_string(),
        broker: socket.to_string_lossy().into_owned(),
        cap_files: Vec::new(),
        state_dir: dir.join("state").to_string_lossy().into_owned(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::default(),
        receipts: true,
        log: crate::mirror::LogMode::Off,
        mesh: None,
    }
}

/// The commits a stand-in broker acked, `(group, upto)`.
#[cfg(unix)]
type Commits = std::sync::Arc<std::sync::Mutex<Vec<(String, u64)>>>;

#[cfg(unix)]
#[derive(Default)]
struct StandInState {
    stopped: bool,
    active: Option<std::sync::Weak<std::os::unix::net::UnixStream>>,
    /// When the group subscription arrived.
    subscribed: Option<std::time::Instant>,
}

#[cfg(unix)]
struct StandIn {
    _dir: aterm_tempfile::TempDir,
    state: std::sync::Arc<(std::sync::Mutex<StandInState>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl StandIn {
    fn subscribed(&self) -> Option<std::time::Instant> {
        let (state, _) = &*self.state;
        state.lock().unwrap_or_else(|e| e.into_inner()).subscribed
    }
}

#[cfg(unix)]
impl Drop for StandIn {
    fn drop(&mut self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        if let Some(stream) = state.active.take().and_then(|s| s.upgrade()) {
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.shutdown(std::net::Shutdown::Read);
        }
        changed.notify_one();
        drop(state);
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !std::thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

#[cfg(unix)]
fn log_identity(id: u8, generation: u64) -> astream_broker::LogIdentity {
    astream_broker::LogIdentity {
        id: [id; 16],
        generation,
    }
}

const STAND_IN: Bounds = Bounds {
    request: aterm_spec::wait::MUST_HAPPEN,
    subscription: aterm_spec::wait::MUST_HAPPEN,
};

/// A stand-in broker for ONE `inbox` over `conns` connections, at astream's
/// wire: the head query answers `end`, the group subscription is sent
/// `tail` (nothing, for an empty one), and every `Commit` is recorded before
/// its ack, so once `inbox` returns the record is final.
#[cfg(unix)]
fn stand_in(
    end: u64,
    tail: Vec<astream_broker::Response>,
    conns: usize,
) -> (StandIn, Config, Commits) {
    scripted_inbox(end, tail, vec![Some(log_identity(1, 0)); conns], false)
}

#[cfg(unix)]
fn scripted_inbox(
    end: u64,
    tail: Vec<astream_broker::Response>,
    logs: Vec<Option<astream_broker::LogIdentity>>,
    close_after_tail: bool,
) -> (StandIn, Config, Commits) {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response,
    };
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-inbox")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("the fixture dir");
    let socket = dir.path().join("b.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let commits = Commits::default();
    let seen = std::sync::Arc::clone(&commits);
    let state = std::sync::Arc::new((
        std::sync::Mutex::new(StandInState::default()),
        std::sync::Condvar::new(),
    ));
    let accepting = state.clone();
    let worker = std::thread::spawn(move || {
        let mark = |next, head| Response::Mark {
            next,
            head,
            resume: String::new(),
            durable: None,
            oldest: None,
        };
        for log in logs {
            let (state, changed) = &*accepting;
            let stream = loop {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    return;
                }
                match aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                ) {
                    Ok((stream, _)) => {
                        let stream = std::sync::Arc::new(stream);
                        state.active = Some(std::sync::Arc::downgrade(&stream));
                        break stream;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => drop(
                        changed
                            .wait_timeout(state, std::time::Duration::from_millis(20))
                            .unwrap_or_else(|e| e.into_inner()),
                    ),
                    Err(e) => panic!("accept the inbox fixture: {e}"),
                }
            };
            let mut c = stream.as_ref();
            while let Ok(Some(frame)) = read_frame(&mut c) {
                let mut end_stream = false;
                let replies = match decode_request(&frame) {
                    Some(Request::Hello) => vec![Response::Nonce {
                        nonce: vec![0; 32],
                        log,
                        ext: astream_broker::NonceExt::default(),
                    }],
                    Some(Request::IdleMarks | Request::GapNotices) => vec![mark(end, end)],
                    Some(Request::Fetch { max: 0, .. }) => vec![mark(0, end)],
                    Some(Request::SubscribeGroup { .. }) => {
                        accepting
                            .0
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .subscribed = Some(std::time::Instant::now());
                        end_stream = close_after_tail;
                        tail.clone()
                    }
                    Some(Request::Commit { group, upto }) => {
                        seen.lock().unwrap().push((group, upto));
                        vec![Response::PublishAck {
                            offset: end,
                            deduped: false,
                        }]
                    }
                    other => panic!("an inbox sends no {other:?}"),
                };
                for r in &replies {
                    if write_frame(&mut c, &encode_response(r)).is_err() {
                        break;
                    }
                }
                if end_stream {
                    break;
                }
            }
        }
    });
    let cfg = unix_cfg(dir.path(), &socket);
    (
        StandIn {
            _dir: dir,
            state,
            worker: Some(worker),
        },
        cfg,
        commits,
    )
}

#[cfg(unix)]
#[test]
fn an_inbox_stops_at_its_captured_head_without_a_tail_mark() {
    use astream_broker::Response;
    let mail = |offset| Response::Delivery {
        offset,
        subject: "/f/f/in/p/h-x/n-peer/note".into(),
        body: b"v=1 t=1 text=mail".to_vec(),
    };
    let (_server, cfg, commits) = scripted_inbox(
        10,
        vec![mail(7), mail(10), mail(11)],
        vec![Some(log_identity(1, 0)); 2],
        true,
    );
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let mut out = Vec::new();
    inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN)
        .expect("a delivery at the captured head completes the snapshot without another mark");
    assert_eq!(
        *commits.lock().unwrap(),
        [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
    );
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.starts_with("@7  ") && out.ends_with("  text=mail\n"),
        "{out}"
    );
    assert_eq!(
        out.lines().count(),
        1,
        "only the mail below the head: {out}"
    );
}

/// Mail stdout refused was not read: a reader that went away (`aterm fabric inbox |
/// head -1`) ends the inbox with [`Unprinted`] and nothing committed. std's `println!`
/// panicked there; a printer that dropped the line would have marked it read.
#[cfg(unix)]
#[test]
fn an_inbox_whose_reader_went_away_marks_nothing_read() {
    struct Gone;
    impl io::Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (_server, cfg, commits) = scripted_inbox(
        10,
        vec![astream_broker::Response::Delivery {
            offset: 7,
            subject: "/f/f/in/p/h-x/n-peer/note".into(),
            body: b"v=1 t=1 text=mail".to_vec(),
        }],
        vec![Some(log_identity(1, 0)); 2],
        true,
    );
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let e = inbox(&cfg, &cmd, "h-x", false, &mut Gone, STAND_IN)
        .expect_err("mail nobody read is not an inbox read");
    assert!(
        e.get_ref().is_some_and(|inner| inner.is::<Unprinted>()),
        "{e}"
    );
    assert!(e.to_string().contains("nothing was marked read"), "{e}");
    assert!(
        commits.lock().unwrap().is_empty(),
        "mail the reader never had was marked read"
    );
}

#[cfg(unix)]
#[test]
fn an_inbox_commits_only_through_its_captured_head() {
    let (_server, cfg, commits) = stand_in(
        10,
        vec![astream_broker::Response::Mark {
            next: 14,
            head: 14,
            resume: String::new(),
            durable: None,
            oldest: None,
        }],
        2,
    );
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).unwrap();
    assert_eq!(
        *commits.lock().unwrap(),
        [("/f/f/cur/p/h-x/inbox".to_string(), 9)],
        "later mail was never part of this snapshot"
    );
}

#[cfg(unix)]
#[test]
fn an_inbox_never_commits_its_old_offsets_on_a_replacement_log() {
    for replacement in [log_identity(2, 0), log_identity(1, 1)] {
        let (_server, cfg, commits) = scripted_inbox(
            10,
            vec![astream_broker::Response::Mark {
                next: 10,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            }],
            vec![Some(log_identity(1, 0)), Some(replacement)],
            false,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        assert!(
            inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).is_err(),
            "a replacement log accepted the old snapshot cursor"
        );
        assert!(
            commits.lock().unwrap().is_empty(),
            "unread replacement mail was marked read"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_inbox_with_an_unidentified_connection_never_marks_mail_read() {
    for logs in [
        vec![None, Some(log_identity(1, 0))],
        vec![Some(log_identity(1, 0)), None],
    ] {
        let (_server, cfg, commits) = scripted_inbox(
            10,
            vec![astream_broker::Response::Mark {
                next: 10,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            }],
            logs,
            false,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        assert!(
            inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).is_err(),
            "unknown log identity was treated as equal"
        );
        assert!(commits.lock().unwrap().is_empty());
    }
}

/// **A CATCH-UP IS READ TO ITS END, NOT TO THE FIRST MARK.** The broker
/// marks part-way (`next=4`, below the head of 10 the inbox started at),
/// then delivers the mail at @7 and marks the end.
#[cfg(unix)]
#[test]
fn an_inbox_reads_past_its_progress_marks() {
    use astream_broker::Response;
    let mut body = Body::new(1);
    body.text = "is main green?".to_string();
    let (_dir, cfg, commits) = stand_in(
        10,
        vec![
            Response::Mark {
                next: 4,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            },
            Response::Delivery {
                offset: 7,
                subject: "/f/f/in/p/h-x/n-00000000000000aa/ask".to_string(),
                body: body.encode(),
            },
            Response::Mark {
                next: 10,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            },
        ],
        2,
    );
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let mut out = Vec::new();
    inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN).expect("the inbox reads to its end");
    assert!(
        String::from_utf8(out).unwrap().starts_with("@7  "),
        "the mail past the progress mark is printed"
    );
    assert_eq!(
        *commits.lock().unwrap(),
        [("/f/f/cur/p/h-x/inbox".to_string(), 9)],
        "committed through the end mark"
    );
}

#[cfg(unix)]
#[test]
fn an_inbox_says_what_retention_gave_up_and_reads_on() {
    use astream_broker::Response;
    let (_dir, cfg, commits) = stand_in(
        10,
        vec![
            Response::Gap(astream_broker::Gap {
                from: 0,
                to: 6,
                records: Some(2),
            }),
            Response::Delivery {
                offset: 7,
                subject: "/f/f/in/p/h-x/n-peer/note".into(),
                body: b"v=1 t=1 text=mail".to_vec(),
            },
            Response::Mark {
                next: 10,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            },
        ],
        2,
    );
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let mut out = Vec::new();
    inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN).expect("the inbox reads past the gap");
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("given up: ") && lines[0].contains("2 record(s) in @0..@6"),
        "{out}"
    );
    assert!(lines[1].starts_with("@7  "), "{out}");
    assert_eq!(
        *commits.lock().unwrap(),
        [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
    );
}

/// **THE SUBSCRIPTION IS HELD TO ITS OWN BOUND, NOT A REQUEST'S.** A broker
/// that takes the subscription and then says nothing: the read ends at the
/// subscription's bound, in words, with nothing marked read.
#[cfg(unix)]
#[test]
fn an_inbox_subscription_is_held_to_its_own_bound() {
    let (server, cfg, commits) = stand_in(10, Vec::new(), 1);
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let bounds = Bounds {
        request: aterm_spec::wait::MUST_HAPPEN * 2,
        subscription: Duration::from_millis(50),
    };
    let e = inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), bounds)
        .expect_err("a broker that never marks ends no inbox well");
    let held = server.subscribed().expect("the inbox subscribed").elapsed();
    assert!(
        held < aterm_spec::wait::MUST_HAPPEN,
        "the subscription waited out the request's bound ({held:?}): {e}"
    );
    assert!(e.to_string().starts_with("went silent"), "{e}");
    assert!(commits.lock().unwrap().is_empty(), "nothing marked read");
}

/// **AN INBOX WITH NO NEW MAIL MARKS READ WHAT IT SCANNED; `--peek` MARKS
/// NOTHING.** Ten records, none the person's: the cursor moves past them
/// (through @9), so the next inbox does not scan them again — committed
/// only after mail, an inbox re-scanned the whole bus since the person's
/// last message on every run.
#[cfg(unix)]
#[test]
fn an_inbox_with_no_new_mail_marks_what_it_scanned_and_peek_marks_nothing() {
    let end = || {
        vec![astream_broker::Response::Mark {
            next: 10,
            head: 10,
            resume: String::new(),
            durable: None,
            oldest: None,
        }]
    };
    let cmd = PersonCmd {
        act: Act::Inbox { peek: false },
        cap_files: Vec::new(),
    };
    let (_dir, cfg, commits) = stand_in(10, end(), 2);
    inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).expect("an inbox with no mail");
    assert_eq!(
        *commits.lock().unwrap(),
        [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
    );
    let (_dir, cfg, commits) = stand_in(10, end(), 1);
    inbox(&cfg, &cmd, "h-x", true, &mut Vec::new(), STAND_IN).expect("a peek with no mail");
    assert!(commits.lock().unwrap().is_empty(), "--peek committed");
}

/// The CLI must reject an agent halt before publishing an ignored fleet
/// row, even if an older minted ring still grants that write.
#[cfg(unix)]
#[test]
fn an_agent_halt_is_refused_before_it_can_append_a_fleet_row() {
    let (_dir, _server, cfg, cmd) = guarded_person("a-review", true);
    let (mut conn, _closer) = connect_within(&cfg, &cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let (_, (_, before)) = conn.fetch(0, "/f/f/pub/>", 0).unwrap();
    assert_eq!(main("halt", &cmd, &cfg), ExitCode::from(2));
    let (_, (_, after)) = conn.fetch(0, "/f/f/pub/>", 0).unwrap();
    assert_eq!(after, before, "a refused agent halt appended a bus record");
}

/// The minted agent ring keeps its mail and cursor authority, while the
/// guarded broker refuses a direct attempt to bypass the CLI's halt gate.
#[cfg(unix)]
#[test]
fn an_agents_ring_reads_and_sends_mail_but_cannot_write_a_halt() {
    let (_dir, _server, cfg, cmd) = guarded_person("a-review", false);
    let (mut conn, _closer) = connect_within(&cfg, &cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let producer = astream_cap::producer_id_of("a-review");
    let (off, _) = conn
        .publish(
            producer,
            1,
            "/f/f/in/p/a-review/a-review/note",
            b"v=1 text=mail",
        )
        .unwrap();
    let (rows, _) = conn.fetch(0, "/f/f/in/p/a-review/>", 8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, off);
    conn.commit("/f/f/cur/p/a-review/inbox", off).unwrap();
    conn.publish(
        producer,
        2,
        "/f/f/in/n-target/s-target/a-review/note",
        b"v=1 text=reply",
    )
    .unwrap();
    let denied = conn
        .publish(producer, 3, "/f/f/fleet/a-review/halt", b"v=1 state=on")
        .unwrap_err();
    assert!(astream_broker::broker_refusal(&denied)
        .is_some_and(astream_broker::BrokerRefusal::is_permanent));
}

#[test]
fn only_a_humans_ring_can_publish_a_halt() {
    let secret = [7u8; 32];
    let can = |person: &str, subject: &str| {
        crate::enable::person_rules("f1", person).iter().any(|g| {
            let cap = astream_cap::mint(&secret, g).expect("mint");
            astream_cap::grants_publish(
                &secret,
                &cap,
                subject,
                astream_cap::producer_id_of(person),
                0,
            )
        })
    };
    assert!(can("h-andrew", "/f/f1/fleet/h-andrew/halt"));
    assert!(!can("a-svc", "/f/f1/fleet/a-svc/halt"));
    for p in ["h-andrew", "a-svc"] {
        assert!(can(p, &format!("/f/f1/in/n-a/s-b/{p}/answer")), "{p} posts");
    }
}

/// **WITH THE MESH ON, A PERSON'S POST TO ANOTHER MAC IS SEALED, OR REFUSED BEFORE ANYTHING
/// IS WRITTEN** (§11 C3c): to a member the node service's hand-off names, once the node
/// service vouched for this Mac's person key, the post is written on the person's lane to that
/// member, sealed so it verifies under this Mac's node key, and to the member a retired node
/// became; while there is no hand-off, before the person key is vouched for, to a node that is
/// no member, or while this Mac has no node key, it is refused (exit 2) and nothing is
/// written. One to this Mac's own session is written unsealed, as before.
/// NEGATIVE CONTROL: with the mesh off, a post to another Mac is written, unsealed.
#[cfg(unix)]
#[test]
fn with_the_mesh_on_a_persons_post_to_another_mac_is_sealed_or_refused_before_anything_is_written()
{
    use crate::federate::{node_of, Handoff, MeshPaths};
    use crate::person_key::{self, CERT_TOKEN};
    let (dir, _server, mut cfg, mut cmd) = guarded_person("h-andrew", false);
    let keys = dir.path().join("keys");
    aterm_uds::private_file::ensure_private_dir(&keys).unwrap();
    let seed = [5u8; 32];
    let text = format!("{}\n", aterm_codec::hex::encode_lower(&seed));
    aterm_uds::private_file::write_private(&keys, crate::federate::NODE_KEY_FILE, text.as_bytes())
        .unwrap();
    let node_key = astream_aead::signature::public_key(&seed);
    let here = node_of(&node_key);
    let there = node_of(&astream_aead::signature::public_key(&[6u8; 32]));
    let retired = "n-00000000000000ee";
    let stranger = "n-00000000000000ff";
    // A reader of every lane, minted under the broker's secret: a person's ring reads its own.
    let mut reader = astream_broker::Client::connect(dir.path().join("bus.sock")).unwrap();
    let cap = astream_cap::mint(&[0x51; 32], "ro:/f/f/in/>").unwrap();
    reader.attach(&cap.filter, &cap.tag).unwrap();
    let mut held = || reader.fetch(0, "/f/f/in/>", 64).unwrap().0;
    let to = |node: &str| Act::Post {
        sid: "s-a".to_string(),
        node: node.to_string(),
        kind: "note".to_string(),
        re: None,
        text: "hi".to_string(),
    };
    let mut posted = |cfg: &Config, act: Act| {
        cmd.act = act;
        main("post", &cmd, cfg)
    };
    let mesh = |keys: &std::path::Path| {
        Some(MeshPaths {
            keys: keys.to_path_buf(),
            root: dir.path().join("keys"),
        })
    };
    cfg.mesh = mesh(&keys);
    assert_eq!(posted(&cfg, to(&there)), ExitCode::from(2), "no hand-off");
    Handoff {
        me: here.clone(),
        members: [here.clone(), there.clone()].into(),
        moved: [(retired.to_owned(), there.clone())].into(),
        rank: std::collections::BTreeMap::new(),
        gone: std::collections::BTreeSet::new(),
    }
    .write(&keys)
    .unwrap();
    assert_eq!(
        posted(&cfg, to(&there)),
        ExitCode::from(2),
        "no person key yet"
    );
    assert!(held().is_empty(), "a refused post writes nothing");

    assert!(person_key::vouch(&keys, &seed, crate::now_ms()).is_ok());
    let person = person_key::PersonKey::read(&keys)
        .unwrap()
        .unwrap()
        .public();
    for node in [there.as_str(), retired] {
        assert_eq!(posted(&cfg, to(node)), ExitCode::SUCCESS, "to {node}");
    }
    let rows = held();
    let lane = format!("/f/f/in/{there}/s-a/h-andrew/note");
    assert_eq!(rows.len(), 2);
    for (_, subject, raw) in &rows {
        assert_eq!(subject, &lane, "on the person's lane to the member");
        let (body, _) = Body::decode(raw);
        assert_eq!(body.text, "hi");
        let address = person_key::post_address("f", "s-a", "h-andrew", "note");
        assert_eq!(
            person_key::verify(
                person_key::Signer::Key(&node_key),
                &address,
                "note",
                &body,
                crate::now_ms()
            ),
            Ok(person),
            "sealed under this Mac's node key"
        );
    }
    assert_eq!(posted(&cfg, to(stranger)), ExitCode::from(2), "no member");
    assert_eq!(held().len(), 2);
    assert_eq!(posted(&cfg, to(&here)), ExitCode::SUCCESS, "this Mac");
    let rows = held();
    assert_eq!(rows.len(), 3);
    assert!(
        !Body::decode(&rows[2].2).0.unknown.contains_key(CERT_TOKEN),
        "a post to this Mac's own session is not sealed"
    );
    cfg.mesh = mesh(&dir.path().join("no-key"));
    assert_eq!(posted(&cfg, to(&here)), ExitCode::from(2), "no node key");
    assert_eq!(held().len(), 3);
    cfg.mesh = None;
    assert_eq!(posted(&cfg, to(stranger)), ExitCode::SUCCESS, "the control");
    let rows = held();
    assert_eq!(rows.len(), 4, "with the mesh off the post is written");
    assert!(!Body::decode(&rows[3].2).0.unknown.contains_key(CERT_TOKEN));
}

/// **A PERSON'S ANSWER TO ANOTHER MAC NAMES THE POST IT ANSWERS AS THAT MAC NUMBERS IT, OR IS
/// REFUSED** (§11 C3d): `re=` names a post this Mac's bridge handed the person, on the
/// person's lane of this Mac's node or of a node its seat held before, from the session the
/// answer goes to (`<sid>@<n>`, `n` that session's member or a node that member's seat held),
/// brought by that member's producer or one of a node its seat held, and the answer carries
/// the offset that post has on the asker's Mac, which the bridge stamped on it
/// ([`WIRE_RE_TOKEN`]). An `re=` that names another session's post, a post of that session
/// id on another Mac, one with no stamp, a stamp out of range, of another format or under
/// another Mac's producer, a post on another node's lane, or no post at all, is refused
/// (exit 2) and nothing is written: an offset on this Mac names nothing on another.
/// NEGATIVE CONTROLS: to this Mac's own session, named by its node or by a node its seat held
/// before, and with the mesh off, `re=` is kept as given.
#[cfg(unix)]
#[test]
fn a_persons_answer_to_another_mac_names_the_post_it_answers_as_that_mac_numbers_it_or_is_refused()
{
    use crate::federate::{node_of, Handoff, MeshPaths};
    let (dir, _server, mut cfg, mut cmd) = guarded_person("h-andrew", false);
    let keys = dir.path().join("keys");
    aterm_uds::private_file::ensure_private_dir(&keys).unwrap();
    let seed = [5u8; 32];
    let text = format!("{}\n", aterm_codec::hex::encode_lower(&seed));
    aterm_uds::private_file::write_private(&keys, crate::federate::NODE_KEY_FILE, text.as_bytes())
        .unwrap();
    let here = node_of(&astream_aead::signature::public_key(&seed));
    let there = node_of(&astream_aead::signature::public_key(&[6u8; 32]));
    let (retired, old_here) = ("n-00000000000000ee", "n-00000000000000dd");
    let stranger = "n-00000000000000ff";
    Handoff {
        me: here.clone(),
        members: [here.clone(), there.clone()].into(),
        moved: [
            (retired.to_owned(), there.clone()),
            (old_here.to_owned(), here.clone()),
        ]
        .into(),
        rank: std::collections::BTreeMap::new(),
        gone: std::collections::BTreeSet::new(),
    }
    .write(&keys)
    .unwrap();
    assert!(crate::person_key::vouch(&keys, &seed, crate::now_ms()).is_ok());
    let socket = dir.path().join("bus.sock");
    // What this Mac's bridge hands the person: a copy on the person's lane of `node`.
    let mut seq = 0;
    let mut handed = |node: &str, from: &str, stamp: Option<String>| {
        let mut writer = astream_broker::Client::connect(&socket).unwrap();
        let grant = format!("rw,p={node}:/f/f/in/p/h-andrew/{node}/ask");
        let cap = astream_cap::mint(&[0x51; 32], &grant).unwrap();
        writer.attach(&cap.filter, &cap.tag).unwrap();
        let mut body = Body::new(crate::now_ms());
        body.from = Some(from.to_owned());
        body.text = "ship it?".to_owned();
        if let Some(stamp) = stamp {
            body.unknown.insert(WIRE_RE_TOKEN.to_owned(), stamp);
        }
        seq += 1;
        let subject = format!("/f/f/in/p/h-andrew/{node}/ask");
        let producer = astream_cap::producer_id_of(node);
        writer
            .publish(producer, seq, &subject, &body.encode())
            .unwrap()
            .0
    };
    let by = |seq: u64, node: &str| Some(wire_stamp(seq, astream_cap::producer_id_of(node)));
    let from_a = format!("s-a@{there}");
    let current = handed(&here, &from_a, by(7, &there));
    let via_retired = handed(&here, &format!("s-a@{retired}"), by(9, retired));
    let pushed_after_rotation = handed(&here, &format!("s-a@{retired}"), by(10, &there));
    let before_rotation = handed(old_here, &from_a, by(11, &there));
    let another_session = handed(&here, &format!("s-other@{there}"), by(13, &there));
    let same_sid_here = handed(&here, &format!("s-a@{here}"), by(17, &there));
    let same_sid_stranger = handed(&here, &format!("s-a@{stranger}"), by(19, &there));
    let same_sid_old_here = handed(&here, &format!("s-a@{old_here}"), by(21, &there));
    let unstamped = handed(&here, &from_a, None);
    let out_of_range = handed(&here, &from_a, by(astream_broker::ACK_SEQ_BASE, &there));
    let bare_seq = handed(&here, &from_a, Some("23".to_owned()));
    let another_producer = handed(&here, &from_a, by(25, stranger));
    let another_lane = handed(stranger, &from_a, by(15, &there));
    // A session of this Mac asked before its key rotated: the bridge wrote it unstamped, from
    // the bare session, on the person's lane of the node it was then.
    let local_before_rotation = handed(old_here, "s-a", None);

    let mut reader = astream_broker::Client::connect(&socket).unwrap();
    let cap = astream_cap::mint(&[0x51; 32], "ro:/f/f/in/>").unwrap();
    reader.attach(&cap.filter, &cap.tag).unwrap();
    let mut answers = |node: &str| -> Vec<Option<u64>> {
        let lane = format!("/f/f/in/{node}/s-a/h-andrew/answer");
        let (rows, _) = reader.fetch(0, &lane, 64).unwrap();
        rows.iter()
            .map(|(_, _, raw)| Body::decode(raw).0.re)
            .collect()
    };
    let answer = |node: &str, re: u64| Act::Post {
        sid: "s-a".to_string(),
        node: node.to_string(),
        kind: "answer".to_string(),
        re: Some(re),
        text: "yes".to_string(),
    };
    let mut posted = |cfg: &Config, act: Act| {
        cmd.act = act;
        main("post", &cmd, cfg)
    };
    cfg.mesh = Some(MeshPaths {
        keys: keys.clone(),
        root: keys.clone(),
    });
    for (node, re) in [
        (there.as_str(), current),
        (there.as_str(), via_retired),
        (retired, via_retired),
        (there.as_str(), pushed_after_rotation),
        (there.as_str(), before_rotation),
    ] {
        assert_eq!(
            posted(&cfg, answer(node, re)),
            ExitCode::SUCCESS,
            "re={re} to s-a@{node}"
        );
    }
    assert_eq!(
        answers(&there),
        [Some(7), Some(9), Some(9), Some(10), Some(11)],
        "each names the post as the asker's Mac numbers it"
    );
    for (why, re) in [
        ("another session's post", another_session),
        ("that session id's post on this Mac", same_sid_here),
        (
            "that session id's post on a node that is no member",
            same_sid_stranger,
        ),
        (
            "that session id's post on a node this Mac held",
            same_sid_old_here,
        ),
        ("an unstamped post", unstamped),
        ("a stamp out of range", out_of_range),
        ("a stamp of another format", bare_seq),
        ("a stamp under another Mac's producer", another_producer),
        ("a post on another node's lane", another_lane),
        ("no post", local_before_rotation + 1000),
    ] {
        assert_eq!(
            posted(&cfg, answer(&there, re)),
            ExitCode::from(2),
            "{why} is refused"
        );
    }
    assert_eq!(answers(&there).len(), 5, "a refused answer writes nothing");
    assert_eq!(
        posted(&cfg, answer(&here, unstamped)),
        ExitCode::SUCCESS,
        "this Mac's own session"
    );
    assert_eq!(
        posted(&cfg, answer(old_here, local_before_rotation)),
        ExitCode::SUCCESS,
        "this Mac's own session, named by a node its seat held before"
    );
    assert_eq!(
        answers(&here),
        [Some(unstamped), Some(local_before_rotation)],
        "keeps re= as given"
    );
    cfg.mesh = None;
    assert_eq!(
        posted(&cfg, answer(&there, another_session)),
        ExitCode::SUCCESS,
        "the mesh off"
    );
    assert_eq!(
        answers(&there).last(),
        Some(&Some(another_session)),
        "with the mesh off, re= is kept as given"
    );
}

#[test]
fn the_person_is_read_off_the_cap_files() {
    let dir = crate::test_scratch_dir("person");
    let write = |name: &str, grants: &[String]| {
        let path = dir.path().join(name);
        let lines: String = grants
            .iter()
            .map(|g| format!("{g} {}\n", "ab".repeat(32)))
            .collect();
        crate::bridge::write_owner_only(&path, lines);
        path.to_string_lossy().into_owned()
    };
    let person = write("h.cap", &crate::enable::person_rules("f1", "h-andrew"));
    let node = write(
        "n.cap",
        &crate::enable::node_rules("f1", "n-00000000000000aa"),
    );
    let other = write("o.cap", &crate::enable::person_rules("f1", "h-other"));
    assert_eq!(
        person_of(&[node.clone(), person.clone()], "f1"),
        Ok("h-andrew".to_string())
    );
    assert!(
        person_of(&[node], "f1").is_err(),
        "a node's ring is no person"
    );
    assert!(person_of(&[person.clone(), other], "f1").is_err());
    assert!(person_of(&[person], "f2").is_err(), "another fleet");
}
