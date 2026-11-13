// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn native_report_reply_waits_for_an_unchanged_local_instance() {
    native_instance_reply(super::io_timeout_at(
        std::time::Instant::now() - std::time::Duration::from_secs(9),
    ))
    .unwrap();
    assert!(native_instance_reply(super::io_timeout_at(
        std::time::Instant::now() - std::time::Duration::from_secs(61)
    ))
    .is_err());
}

#[test]
fn native_enable_reply_waits_for_an_unchanged_local_instance() {
    native_instance_reply(crate::enable::io_timeout_at(
        std::time::Instant::now() - std::time::Duration::from_secs(9),
    ))
    .unwrap();
    assert!(native_instance_reply(crate::enable::io_timeout_at(
        std::time::Instant::now() - std::time::Duration::from_secs(61)
    ))
    .is_err());
}

fn native_instance_reply(timeout: std::time::Duration) -> Result<(), String> {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("fabric-io-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let endpoint = scratch.path().join("ctl.sock");
    let listener =
        aterm_uds::kept_out(|| std::os::unix::net::UnixListener::bind(&endpoint)).unwrap();
    let token = "ab".repeat(32);
    let name = aterm_types::control_socket::token_names_for_sock("ctl.sock").remove(0);
    let token_path = scratch.path().join(name);
    std::fs::write(&token_path, &token).unwrap();
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            ),
            aterm_uds::AcceptedMode::Blocking,
        )?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(60)))?;
        let mut input = std::io::BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        assert_eq!(line, format!("AUTH {token}\n"));
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        assert_eq!(line, "version\n");
        stream.write_all(b"OK unchanged-native-instance\n")
    });
    let result = super::open_instance(std::process::id(), endpoint.to_str().unwrap(), timeout)
        .and_then(|ctl| {
            let mut ctl = ctl.ok_or("native instance absent")?;
            let reply = ctl.request("version").map_err(|e| e.to_string())?;
            assert_eq!(reply.header(), "OK unchanged-native-instance");
            Ok(())
        });
    if result.is_err() {
        let _ = aterm_uds::connect::until(
            &endpoint,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
    }
    let served = server.join().unwrap();
    result?;
    served.map_err(|e| e.to_string())
}
use super::*;

/// A linked `aterm.toml` says what its file says, and `doctor`'s fabric row reads it.
#[cfg(unix)]
#[test]
fn the_fabric_command_reads_through_a_linked_aterm_toml() {
    let dir = crate::test_scratch_dir("fab");
    let real = dir.path().join("dotfiles.toml");
    aterm_uds::atomic_file::write(&real, b"[fabric]\ncommand = \"astream serve\"\n", 0o600)
        .expect("write");
    let link = dir.path().join("aterm.toml");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let (_, command) = resolve_command(None, Some(&link))
        .expect("read")
        .expect("a command");
    assert_eq!(command, words("astream serve"));
}

#[cfg(unix)]
#[test]
fn refused_config_does_not_enable_receipts_or_presence_metadata() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("fab");
    let path = dir.path().join("aterm.toml");
    aterm_uds::atomic_file::write(
        &path,
        b"[fabric]\nreceipts = false\npresence = \"minimal\"\ncommand = \"astream serve\"\n",
        0o600,
    )
    .unwrap();
    let receipts = || receipts_from_config_at(Some(&path));
    let presence = || presence_from_config_at(Some(&path));
    assert!(!receipts());
    assert_eq!(presence(), crate::presence::Mode::Minimal);
    assert!(resolve_command(None, Some(&path)).unwrap().is_some());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(!receipts());
    assert_eq!(presence(), crate::presence::Mode::Minimal);
    assert!(resolve_command(None, Some(&path)).is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(receipts());
    assert_eq!(presence(), crate::presence::Mode::Meta);
}

fn words(line: &str) -> Vec<String> {
    aterm_types::fabric_command::split_line(line)
}

#[test]
fn a_retire_is_refused_for_a_sibling_hosted_sid_or_an_unaskable_node() {
    let hosted: Result<BTreeMap<String, u32>, String> =
        Ok([("s-aaaaaaaaaaaaaaaaaaaa".to_string(), 4242)]
            .into_iter()
            .collect());
    assert_eq!(
        retire_refusal("s-aaaaaaaaaaaaaaaaaaaa", &hosted),
        Some(RetireRefusal::Hosted(4242))
    );
    assert_eq!(retire_refusal("s-bbbbbbbbbbbbbbbbbbbb", &hosted), None);
    let unaskable: Result<BTreeMap<String, u32>, String> =
        Err("instance 77: connect: Operation timed out".to_string());
    assert_eq!(
        retire_refusal("s-bbbbbbbbbbbbbbbbbbbb", &unaskable),
        Some(RetireRefusal::Unaskable(
            "instance 77: connect: Operation timed out".to_string()
        ))
    );
}

#[test]
fn ghost_rows_are_live_unhosted_and_ours() {
    let row = |sid: &str, pid: Option<u32>, node: &str, bus: &str| SessionRow {
        sid: sid.to_string(),
        pid,
        node: node.to_string(),
        bus: bus.to_string(),
        ..SessionRow::default()
    };
    let sessions = vec![
        row("s-ghost", None, "n-ours", "live"),
        row("s-hosted", Some(42), "n-ours", "live"),
        row("s-gone", None, "n-ours", "exited"),
        row("s-elsewhere", None, "n-theirs", "live"),
    ];
    let local = local_nodes(&[], Some("n-ours"));
    let got: Vec<&str> = ghost_rows(&sessions, &local)
        .iter()
        .map(|s| s.sid.as_str())
        .collect();
    assert_eq!(got, ["s-ghost"]);
}

#[test]
fn incomplete_instance_discovery_never_admits_a_retirement_candidate() {
    let mut report = healthy();
    report.sessions = vec![SessionRow {
        sid: "s-ghost".into(),
        node: "n-a".into(),
        bus: "live".into(),
        ..SessionRow::default()
    }];
    assert_eq!(
        retirement_candidates(&report).unwrap(),
        [("s-ghost".into(), "n-a".into())]
    );
    report.instances[0].error = Some("native reply unavailable".into());
    assert!(retirement_candidates(&report).is_err());
    report.instances[0].error = None;
    report.discovery_error = Some("native discovery incomplete".into());
    assert!(retirement_candidates(&report).is_err());
}

#[test]
fn a_retired_body_moves_only_state_and_the_stamp() {
    let out = retired_body("v=1 t=1 inc=7 epoch=e1 gen=g1 state=live hold=0 role=worker");
    assert!(out.contains("state=exited"), "{out}");
    assert!(!out.contains("state=live"), "{out}");
    assert!(!out.contains(" t=1 "), "the stamp is refreshed: {out}");
    for kept in [
        "v=1",
        "inc=7",
        "epoch=e1",
        "gen=g1",
        "hold=0",
        "role=worker",
    ] {
        assert!(out.contains(kept), "{kept} must survive: {out}");
    }
    let bare = retired_body("v=1 inc=2");
    assert!(
        bare.contains("state=exited") && bare.contains("t="),
        "{bare}"
    );
}

fn argv(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn the_grammar_is_status_by_default_and_refuses_what_it_does_not_know() {
    assert_eq!(parse_args(&[]), Ok(Cmd::Status { json: false }));
    assert_eq!(parse_args(&argv("--json")), Ok(Cmd::Status { json: true }));
    for help in ["brief --help", "brief -h", "brief --json -h", "tail --help"] {
        assert_eq!(parse_args(&argv(help)), Ok(Cmd::Help), "{help}");
    }
    for bad in [
        "pub/>",
        "/f//pub/>",
        "/f/lab/pub/ /say",
        "/f/lab/\u{1b}[2J",
        "/f/lab/a;rm -rf /",
        "/f/lab/>/x",
    ] {
        let got = parse_args(&[
            "tail".to_string(),
            "--filter".to_string(),
            (*bad).to_string(),
        ]);
        assert!(got.is_err(), "{bad}: {got:?}");
    }
    assert_eq!(
        parse_args(&argv("on --durability relaxed")),
        Ok(Cmd::On(crate::enable::OnOpts {
            durability: Some(crate::durability::Durability::Relaxed),
            ..crate::enable::OnOpts::default()
        }))
    );
    assert_eq!(
        parse_args(&argv("tail --filter /f/lab/pub/*/*/say/>")),
        Ok(Cmd::Tail {
            bodies: false,
            json: false,
            from: None,
            filter: Some("/f/lab/pub/*/*/say/>".to_string())
        })
    );
    assert_eq!(
        parse_args(&argv("doctor --retire-ghosts")),
        Ok(Cmd::Doctor {
            retire_ghosts: true,
            yes: false
        }),
        "dry by default"
    );
    for bad in [
        "--aterm-front-door-routing-probe",
        "doctor --yes",
        "status --retire-ghosts",
        "tail --yes",
        "status --filter /f/lab/>",
        "doctor --filter /f/lab/>",
        "status --bodies",
        "doctor --json",
        "on --json",
        "tail --from",
        "tail --from x",
        "stats",
        "on --service cron",
        "on --fleet LAB",
        "off --fleet lab",
        "on --durability fast",
        "off --durability relaxed",
        "doctor --dry-run",
        "mint-for",
        "mint-for n-a",
        "mint-for h-a h-b",
        "mint-for h-a --expires soon",
        "join --broker x:1",
        "on --broker x:1",
        "on --cap-file /c",
        "on --tcp 127.0.0.1:7000",
        "status --accept-from n-a",
    ] {
        assert!(parse_args(&argv(bad)).is_err(), "{bad} must be refused");
    }
}

#[test]
fn tail_takes_json_beside_its_other_flags_and_the_others_name_where_it_is_valid() {
    let tail = |bodies: bool, json: bool, from: Option<u64>| {
        Ok(Cmd::Tail {
            bodies,
            json,
            from,
            filter: None,
        })
    };
    assert_eq!(parse_args(&argv("tail --json")), tail(false, true, None));
    assert_eq!(
        parse_args(&argv("tail --bodies --json --from @5")),
        tail(true, true, Some(5))
    );
    assert_eq!(parse_args(&argv("tail --bodies")), tail(true, false, None));
    for verb in ["doctor", "on", "off", "mint-for h-jsmith"] {
        let e = parse_args(&argv(&format!("{verb} --json"))).expect_err(verb);
        assert!(
            e.contains("`aterm fabric tail --json`") && e.contains("`aterm fabric --json`"),
            "{verb}: {e}"
        );
    }
    let help = usage();
    assert!(
        help.contains("aterm fabric tail [--bodies] [--json]"),
        "{help}"
    );
    assert_lists(&help, &TAIL_KEYS, "the keys tail_json writes");
}

/// `help` prints `keys` as one whole list, and [`key_list`] names exactly them, in order.
fn assert_lists(help: &str, keys: &[&str], what: &str) {
    let list = key_list(keys);
    let named: Vec<&str> = list
        .split_whitespace()
        .map(|w| w.trim_matches('`'))
        .collect();
    assert_eq!(named, keys, "{what}: the list names them, in order");
    assert!(
        help.contains(&format!("\n{list}")),
        "{what}: the help prints that list whole: {help}"
    );
}

#[test]
fn the_help_lists_the_keys_the_brief_writes_for_each_session() {
    let help = usage();
    assert_lists(
        &help,
        &crate::fabric_brief::SESSION_KEYS,
        "the keys session_json writes",
    );
    assert!(
        !help.contains("@SESSION_KEYS@") && !help.contains("@TAIL_KEYS@"),
        "every key list is filled: {help}"
    );
}

#[test]
fn tail_from_reads_a_bus_offset_by_the_shared_grammar() {
    for from in ["5", "@5"] {
        let args = ["tail", "--from", from].map(str::to_string);
        assert_eq!(
            parse_args(&args),
            Ok(Cmd::Tail {
                bodies: false,
                json: false,
                from: Some(5),
                filter: None,
            }),
            "{from}"
        );
    }
    for from in ["+5", "@", "", "@-1"] {
        let args = ["tail", "--from", from].map(str::to_string);
        let refused = parse_args(&args);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("--from") && e.contains(from)),
            "{from:?}: refused, naming the flag and the value: {refused:?}"
        );
    }
}

/// A writer that keeps what it is given and refuses a write once it holds `lines` lines,
/// as a closed pipe does (the tail then ends, as it does when its reader goes).
struct Lines {
    got: Vec<u8>,
    lines: usize,
}

impl Write for Lines {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.got.iter().filter(|c| **c == b'\n').count() >= self.lines {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.got.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn tail_json_prints_one_object_per_record_and_a_body_only_under_bodies() {
    let scratch = crate::test_scratch_dir("tail");
    let dir = scratch.path();
    let broker = crate::test_broker(dir.join("t.log")).expect("a broker");
    let sock = dir.join("t.sock");
    let mut server = broker.serve(&sock).expect("serves");
    let (mut conn, _) =
        transport::connect(&sock.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN).expect("dial");
    let records = [
        (
            "/f/f1/in/n-a/s-x/h-jsmith/note",
            "v=1 t=1700000000000 text=QZP8NOTE",
        ),
        ("/f/f1/pub/n-a/node/presence", "v=1 t=1700000000001 inc=3"),
        (
            "/f/f1/pub/n-a/s-x/presence",
            "v=1 t=1700000000002 state=live",
        ),
    ];
    let mut offs = Vec::new();
    for (seq, (subject, body)) in (1..).zip(records) {
        offs.push(
            conn.publish(7, seq, subject, body.as_bytes())
                .expect("lands")
                .0,
        );
    }
    let sock = sock.to_string_lossy().into_owned();
    let cfg = crate::cli::parse(&[
        "--fleet".to_string(),
        "f1".to_string(),
        "--broker".to_string(),
        sock,
    ])
    .expect("a config")
    .cfg;
    for bodies in [false, true] {
        let mut out = Lines {
            got: Vec::new(),
            lines: 2,
        };
        tail(&cfg, TailFmt::new(bodies, true), Some(0), None, &mut out)
            .expect("the tail ends when its reader does");
        let text = String::from_utf8(out.got).expect("utf-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "{bodies}: no header, one line per record: {text}"
        );
        for (i, line) in lines.iter().enumerate() {
            let v: aterm_json::Value = aterm_json::from_str(line)
                .unwrap_or_else(|e| panic!("{bodies}: one JSON object: {e}: {line}"));
            let aterm_json::Value::Object(map) = &v else {
                panic!("{bodies}: an object: {line}");
            };
            let keys: BTreeSet<&str> = map.keys().map(String::as_str).collect();
            let want: BTreeSet<&str> = TAIL_KEYS
                .iter()
                .copied()
                .filter(|k| bodies || *k != "body")
                .collect();
            assert_eq!(keys, want, "{bodies}: {line}");
            let (subject, body) = records[i];
            let field = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
            assert_eq!(
                v.get("off").and_then(aterm_json::Value::as_u64),
                Some(offs[i])
            );
            assert_eq!(field("subject"), Some(subject));
            assert_eq!(field("node"), Some("n-a"));
            assert_eq!(field("sid"), [Some("s-x"), None][i], "{line}");
            assert_eq!(field("kind"), Some(["note", "presence"][i]));
            let t = traffic_of("f1", offs[i], subject, body.as_bytes(), 0);
            let table = tail_line(&t, 0, 0, true);
            let printed = table.split_once("  text=").map_or("", |(_, text)| text);
            assert_eq!(
                field("body"),
                bodies.then_some(printed),
                "{bodies}: the body is the text --bodies prints, and only under it"
            );
            assert_eq!(
                line.contains("QZP8NOTE"),
                bodies && i == 0,
                "{bodies}: {line}"
            );
        }
    }
    server.shutdown();
}

/// A fetch of face `face` of `log` from `from` on a broker whose head is `head`: the face's
/// records in `from..head`, then `head` as the offset after the last one scanned.
fn face_page(
    log: &[Vec<u64>],
    head: u64,
    face: usize,
    from: u64,
) -> (Vec<astream_broker::Record>, u64) {
    let page = log[face]
        .iter()
        .filter(|off| (from..head).contains(*off))
        .map(|off| (*off, format!("/f/f1/s{off}"), Vec::new()))
        .collect();
    (page, head)
}

/// [`tail_faces`]'s loop over `merge`, as it runs: [`drain_merge`] (a face read on by
/// `fetch(face, below)`), then the next of `events`. What it printed. LAWS: the merge waits
/// only for a face that has not said it is caught up, and holds fewer than its hold while it
/// does.
fn drive_merge(
    merge: &mut FaceMerge,
    events: Vec<(usize, FaceRead)>,
    fetch: impl Fn(usize, u64) -> (Vec<astream_broker::Record>, u64),
) -> Vec<u64> {
    let faces: Vec<String> = (0..merge.below.len()).map(|f| format!("face{f}")).collect();
    let mut printed = Vec::new();
    let mut events = events.into_iter();
    loop {
        let there = drain_merge(
            merge,
            &faces,
            "/b",
            astream_broker::IDLE_MARK_DEADLINE,
            |face, _, from| Ok(fetch(face, from)),
            |off, _, _| {
                printed.push(off);
                true
            },
        )
        .expect("every fetch moves its face on");
        assert!(there, "the reader is there");
        if let MergeStep::Behind { face, fetch } = merge.next() {
            assert!(!fetch, "drain_merge returned with face {face} to read on");
            assert!(
                !merge.caught[face],
                "the merge waits for face {face}, which said it is caught up"
            );
            assert!(
                merge.held.len() < merge.hold,
                "the merge waits for face {face} holding {} records, its hold {}",
                merge.held.len(),
                merge.hold
            );
        }
        let Some((face, read)) = events.next() else {
            return printed;
        };
        match read {
            FaceRead::Record(off, subject, raw) => merge.record(face, off, subject, raw),
            FaceRead::Caught(next) => merge.caught(face, next),
            FaceRead::Gap(_) => {}
        }
    }
}

/// THE FACES ARE MERGED BY OFFSET (audit findings BUS2-5 and LOOP2-3; [`FaceMerge`]): four
/// faces of one log, the fourth the same as the first (as `pub/>` and `pub/<node>/>` are),
/// each delivering its own records in offset order while the faces interleave as their
/// threads race, a sparse one caught up early, and offsets 5 and 8 on none. The merge
/// prints every record once, offsets strictly increasing, waiting only for a face still
/// delivering its backlog. A caught-up face is read on by a fetch: a live record on one face
/// prints while the others are idle, and one a face's stream has not delivered yet is
/// fetched and printed first, its delivery then taken as read. Past its hold, a face still
/// delivering its backlog is read on by a fetch too, though its stream says nothing. Every
/// face of a broker that marks no tail is caught from its start ([`tail_faces`]), so every
/// record prints though no stream ever marks. A fetch that fails ends the merge naming the
/// `--from` a resume starts at.
/// NEGATIVE CONTROLS (run): printing the oldest held at once (arrival order) prints 1 first,
/// and 0 never; waiting for a caught-up face's stream breaks the law above; waiting past the
/// hold for a face not caught up prints nothing past the hold; a fetch's error passed on
/// as it came names no `--from`.
#[test]
fn faces_merge_by_offset_and_a_caught_up_face_is_read_on_never_waited_for() {
    let rec = |off: u64| FaceRead::Record(off, format!("/f/f1/s{off}"), Vec::new());
    let mut log = vec![
        vec![0, 3, 6, 9],
        vec![1, 4, 7, 10],
        vec![2, 11],
        vec![0, 3, 6, 9],
    ];
    let mut merge = FaceMerge::new(4, 0);
    let backlog = vec![
        (1, rec(1)),
        (0, rec(0)),
        (2, rec(2)),
        (3, rec(0)),
        (0, rec(3)),
        (1, rec(4)),
        (3, rec(3)),
        (0, rec(6)),
        (2, rec(11)),
        (2, FaceRead::Caught(12)),
        (1, rec(7)),
        (3, rec(6)),
        (0, rec(9)),
        (1, rec(10)),
        (3, rec(9)),
        (0, FaceRead::Caught(12)),
        (1, FaceRead::Caught(12)),
        (3, FaceRead::Caught(12)),
    ];
    let printed = drive_merge(&mut merge, backlog, |f, from| face_page(&log, 12, f, from));
    assert_eq!(printed, vec![0, 1, 2, 3, 4, 6, 7, 9, 10, 11], "the backlog");

    log[1].push(12);
    let printed = drive_merge(&mut merge, vec![(1, rec(12))], |f, from| {
        face_page(&log, 13, f, from)
    });
    assert_eq!(printed, vec![12], "a live record, the other faces idle");

    log[2].push(13);
    log[0].push(14);
    log[3].push(14);
    let live = vec![(0, rec(14)), (2, rec(13)), (3, rec(14))];
    let printed = drive_merge(&mut merge, live, |f, from| face_page(&log, 15, f, from));
    assert_eq!(
        printed,
        vec![13, 14],
        "a record a face's stream has not delivered yet comes first, and once"
    );
    assert!(merge.held.is_empty(), "{merge:?}");

    let log = vec![vec![0, 1, 2, 3], vec![5]];
    let mut merge = FaceMerge::new(2, 0);
    merge.hold = 2;
    let events = vec![
        (0, rec(0)),
        (0, rec(1)),
        (0, rec(2)),
        (0, rec(3)),
        (0, FaceRead::Caught(6)),
    ];
    let printed = drive_merge(&mut merge, events, |f, from| face_page(&log, 6, f, from));
    assert_eq!(
        printed,
        vec![0, 1, 2, 3, 5],
        "past the hold, the silent face is fetched"
    );

    let log = vec![vec![10, 12], vec![11, 13]];
    let mut merge = FaceMerge::new(2, 10);
    for face in 0..2 {
        merge.caught(face, 10);
    }
    let events = vec![(1, rec(11)), (0, rec(10)), (1, rec(13)), (0, rec(12))];
    let printed = drive_merge(&mut merge, events, |f, from| face_page(&log, 14, f, from));
    assert_eq!(printed, vec![10, 11, 12, 13], "a broker that marks no tail");

    let faces = ["/f/f1/pub/>".to_string(), "/f/f1/in/n-a/>".to_string()];
    let mut merge = FaceMerge::new(2, 0);
    merge.caught(0, 0);
    merge.record(1, 0, "/f/f1/s0".to_string(), Vec::new());
    merge.record(1, 3, "/f/f1/s3".to_string(), Vec::new());
    let mut printed = Vec::new();
    let ended = drain_merge(
        &mut merge,
        &faces,
        "/b",
        astream_broker::IDLE_MARK_DEADLINE,
        |face, _, from| match from {
            0 => Ok(face_page(&[vec![1, 2], vec![]], 3, face, from)),
            _ => Err(io::Error::from(io::ErrorKind::TimedOut)),
        },
        |off, _, _| {
            printed.push(off);
            true
        },
    );
    assert_eq!(printed, vec![0, 1, 2], "printed before the fetch failed");
    assert!(
        ended
            .as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::TimedOut
                && e.to_string().contains(&resume_from(3))
                && e.to_string().contains(&faces[0])),
        "{ended:?}"
    );
}

/// A writer that hands each line it is given to `tx` and refuses a write once it has sent
/// `lines` lines, as a closed pipe does (the tail then ends).
struct Sent {
    tx: std::sync::mpsc::Sender<String>,
    buf: Vec<u8>,
    lines: usize,
}

impl Write for Sent {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.lines == 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.buf.extend_from_slice(b);
        while let Some(at) = self.buf.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=at).collect();
            let _ = self
                .tx
                .send(String::from_utf8_lossy(&line).trim_end().to_string());
            self.lines = self.lines.saturating_sub(1);
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The `off` of a `tail --json` record line.
fn tail_off(line: &str) -> Option<u64> {
    aterm_json::from_str::<aterm_json::Value>(line)
        .ok()?
        .get("off")?
        .as_u64()
}

/// `tail --json` UNDER A NODE CAP PRINTS OFFSETS STRICTLY INCREASING, SO A RESUME FROM THE
/// LAST ONE PLUS ONE MISSES NOTHING (audit findings BUS2-5 and LOOP2-3): on a guarded broker
/// a node's cap (its ceiling's rules) does not grant the fleet's root, so the tail reads
/// each of its five faces apart. 250 records cycle over subjects on four faces (one on two,
/// `pub/>` and `pub/n-a/>`) and one the node cannot read. A tail from 0 stopped after 100
/// lines and one resumed from its last offset plus one print, between them, every record the
/// node reads, once, in offset order, and the resumed tail then prints each record published
/// live on one face while the others are idle. The broker keeps its default idle mark, so
/// no phase races astream's link deadline; that a caught-up face is read on, never waited
/// for, is [`faces_merge_by_offset_and_a_caught_up_face_is_read_on_never_waited_for`]'s law.
/// NEGATIVE CONTROLS (run): printing each record as it arrives, deduplicated (the code before
/// this fix), prints `[1, 0, 2, 3, 6, 11, 7, …]`; printing in arrival order fails it too.
#[cfg(unix)]
#[test]
fn tail_json_under_a_node_cap_prints_offsets_in_order_so_a_resume_misses_none() {
    use aterm_spec::wait::MUST_HAPPEN;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-tail-faces")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let secret = vec![0x5a_u8; 32];
    let broker = astream_broker::Broker::open_guarded_with(
        dir.path().join("b.log"),
        secret.clone(),
        astream_broker::Durability::Relaxed,
    )
    .expect("a guarded broker");
    let socket = dir.path().join("b.sock");
    let _server = broker.serve(&socket).expect("serve the broker");
    let sock = socket.to_string_lossy().into_owned();
    let (mut god, _) = transport::connect(&sock, MUST_HAPPEN).expect("dial");
    let all = astream_cap::mint(&secret, "/f/f1/>").expect("mint the fleet's cap");
    god.attach(&all.filter, &all.tag).expect("attach it");
    let cap = dir.path().join("node.cap");
    let lines: String = crate::enable::node_rules("f1", "n-a")
        .iter()
        .map(|g| {
            let c = astream_cap::mint(&secret, g).expect("mint");
            format!("{}\n", astream_cap::capfile::format_line(&c.filter, &c.tag))
        })
        .collect();
    crate::bridge::write_owner_only(&cap, lines);
    let subjects = [
        ("/f/f1/pub/n-b/s-x/presence", true),
        ("/f/f1/in/n-a/s-y/n-b/note", true),
        ("/f/f1/pub/n-a/s-y/presence", true),
        ("/f/f1/fleet/h-andrew/halt", true),
        ("/f/f1/in/n-b/s-z/n-c/note", false),
    ];
    let mut seq = 0u64;
    let mut publish = |i: usize| {
        seq += 1;
        let (subject, read) = subjects[i % subjects.len()];
        let body = format!("v=1 t={} state=live", 1_700_000_000_000 + seq);
        let off = god
            .publish(7, seq, subject, body.as_bytes())
            .expect("lands")
            .0;
        (off, read)
    };
    let readable: Vec<u64> = (0..250)
        .map(&mut publish)
        .filter_map(|(off, read)| read.then_some(off))
        .collect();
    let cfg = crate::cli::parse(&[
        "--fleet".to_string(),
        "f1".to_string(),
        "--broker".to_string(),
        sock,
        "--cap-file".to_string(),
        cap.to_string_lossy().into_owned(),
    ])
    .expect("a config")
    .cfg;
    let json = TailFmt::new(false, true);

    let stop = 100;
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut out = Sent {
        tx,
        buf: Vec::new(),
        lines: stop,
    };
    let from_zero = cfg.clone();
    std::thread::spawn(move || {
        let ended = tail(&from_zero, json, Some(0), None, &mut out);
        let _ = done_tx.send(ended.map_err(|e| e.to_string()));
    });
    let mut first = Vec::new();
    for _ in 0..stop {
        let line = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the first tail's lines ({first:?}): {e}"));
        first.extend(tail_off(&line));
    }
    assert_eq!(
        done_rx
            .recv_timeout(MUST_HAPPEN)
            .expect("the first tail ends"),
        Ok(()),
        "it ends when its reader does"
    );
    assert!(
        first.windows(2).all(|w| w[0] < w[1]),
        "offsets strictly increasing: {first:?}"
    );
    assert_eq!(first.as_slice(), &readable[..stop], "the first {stop}");

    let live = 4;
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let resume = first.last().map_or(0, |off| off + 1);
    let mut out = Sent {
        tx,
        buf: Vec::new(),
        lines: readable.len() - stop + live,
    };
    let resumed = cfg.clone();
    std::thread::spawn(move || {
        let ended = tail(&resumed, json, Some(resume), None, &mut out);
        let _ = done_tx.send(ended.map_err(|e| e.to_string()));
    });
    let mut rest = Vec::new();
    while rest.len() < readable.len() - stop {
        let line = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the resumed tail's backlog ({rest:?}): {e}"));
        rest.extend(tail_off(&line));
    }
    assert!(
        rest.windows(2).all(|w| w[0] < w[1]),
        "offsets strictly increasing: {rest:?}"
    );
    assert_eq!(
        [first.as_slice(), rest.as_slice()].concat(),
        readable,
        "the two runs read every record the node reads, once: no hole"
    );
    for i in 0..live {
        let (off, _) = publish(i);
        let line = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the live record @{off} on face {i}: {e}"));
        assert_eq!(tail_off(&line), Some(off), "{line}");
    }
    publish(0);
    assert_eq!(
        done_rx.recv_timeout(MUST_HAPPEN).expect("the tail ends"),
        Ok(()),
        "it ends when its reader does"
    );
}

/// A GUARDED BROKER AND A CAP THAT GRANTS TWO OF ITS FACES APART: the broker serves at
/// `real`; `god` holds the fleet's root, so it publishes anywhere on f1; `cap` grants
/// [`TwoFaces::FACES`] and not the root, so a tail under it reads the two faces apart
/// ([`TailLink::open`]).
#[cfg(unix)]
struct TwoFaces {
    _server: astream_broker::BrokerHandle,
    god: Conn,
    real: std::path::PathBuf,
    cap: std::path::PathBuf,
    seq: u64,
}

#[cfg(unix)]
impl TwoFaces {
    /// The two faces, in the order [`readable_faces`] reads them.
    const FACES: [&'static str; 2] = ["/f/f1/in/n-a/>", "/f/f1/pub/>"];
    /// A subject on each face.
    const SUBJECTS: [&'static str; 2] = ["/f/f1/in/n-a/s-y/n-b/note", "/f/f1/pub/n-b/s-x/presence"];

    fn new(dir: &std::path::Path) -> Self {
        let secret = vec![0x5a_u8; 32];
        let broker = astream_broker::Broker::open_guarded_with(
            dir.join("b.log"),
            secret.clone(),
            astream_broker::Durability::Relaxed,
        )
        .expect("a guarded broker");
        let real = dir.join("b.sock");
        let server = broker.serve(&real).expect("serve the broker");
        let (mut god, _) =
            transport::connect(&real.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN)
                .expect("dial");
        let all = astream_cap::mint(&secret, "/f/f1/>").expect("mint the fleet's cap");
        god.attach(&all.filter, &all.tag).expect("attach it");
        let cap = dir.join("two.cap");
        let lines: String = Self::FACES
            .iter()
            .map(|face| {
                let c = astream_cap::mint(&secret, &format!("ro:{face}")).expect("mint");
                format!("{}\n", astream_cap::capfile::format_line(&c.filter, &c.tag))
            })
            .collect();
        crate::bridge::write_owner_only(&cap, lines);
        Self {
            _server: server,
            god,
            real,
            cap,
            seq: 0,
        }
    }

    /// A tail's config of f1 on the broker at `sock` (the broker's own socket, or one in
    /// front of it), under the two faces' cap.
    fn cfg(&self, sock: &std::path::Path) -> Config {
        crate::cli::parse(&[
            "--fleet".to_string(),
            "f1".to_string(),
            "--broker".to_string(),
            sock.to_string_lossy().into_owned(),
            "--cap-file".to_string(),
            self.cap.to_string_lossy().into_owned(),
        ])
        .expect("a config")
        .cfg
    }

    /// Publish a record on face `face`: its offset.
    fn publish(&mut self, face: usize) -> u64 {
        self.seq += 1;
        let body = format!("v=1 t={} state=live", 1_700_000_000_000 + self.seq);
        self.god
            .publish(7, self.seq, Self::SUBJECTS[face], body.as_bytes())
            .expect("lands")
            .0
    }
}

/// A BROKER IN FRONT OF THE ONE AT `real`, listening at `at`, that relays every frame
/// either way, but: where `older`, it answers `IdleMarks`, and `GapNotices`, which a broker
/// that predates idle marks predates too (astream's `Client::ask_for_tail`), as such a
/// broker does, with what astream's broker answers a request it cannot decode, `Error 5
/// "malformed request"` ([`astream_broker::proto::ERR_REFUSED`]), keeping the connection;
/// and once the flag it hands back is set, it takes every request it is sent and answers
/// none, as a stalled broker does, while the streams it serves go on.
#[cfg(unix)]
fn broker_in_front(
    real: &std::path::Path,
    at: &std::path::Path,
    older: bool,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, ERR_REFUSED,
    };
    use astream_broker::{Request, Response};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{atomic::Ordering, Arc, Mutex, PoisonError};
    let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = UnixListener::bind(at).expect("bind the broker in front");
    let real = real.to_path_buf();
    let stall = Arc::clone(&stalled);
    std::thread::spawn(move || loop {
        let (Ok((peer, _)), Ok(broker)) = (
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ),
            UnixStream::connect(&real),
        ) else {
            return;
        };
        let to_peer = Arc::new(Mutex::new(peer.try_clone().expect("the peer's writer")));
        let back = Arc::clone(&to_peer);
        let mut from_broker = broker.try_clone().expect("the broker's reader");
        let peer_end = peer.try_clone().expect("the peer's end");
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut from_broker) {
                let mut to = back.lock().unwrap_or_else(PoisonError::into_inner);
                if write_frame(&mut *to, &frame).is_err() {
                    break;
                }
            }
            let _ = peer_end.shutdown(std::net::Shutdown::Both);
        });
        let stall = Arc::clone(&stall);
        let (mut from_peer, mut to_broker) = (peer, broker);
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut from_peer) {
                let sent = match decode_request(&frame) {
                    _ if stall.load(Ordering::SeqCst) => Ok(()),
                    Some(Request::IdleMarks | Request::GapNotices) if older => {
                        let refusal = Response::Error {
                            code: ERR_REFUSED,
                            msg: "malformed request".to_string(),
                        };
                        let mut to = to_peer.lock().unwrap_or_else(PoisonError::into_inner);
                        write_frame(&mut *to, &encode_response(&refusal))
                    }
                    _ => write_frame(&mut to_broker, &frame),
                };
                if sent.is_err() {
                    break;
                }
            }
            let _ = to_broker.shutdown(std::net::Shutdown::Both);
        });
    });
    stalled
}

/// A BROKER THAT REFUSES `IdleMarks`, AS ONE THAT PREDATES THEM DOES, MARKS NO TAIL, so
/// [`tail_faces`] takes each of its faces as caught at the tail's start (`merge.caught(i,
/// start)`) and reads every face on by a told fetch, never waiting for a stream that says
/// nothing until its face's next record. Two faces, their records interleaved by offset,
/// tailed from the fourth record: every record at or above it prints once, offsets strictly
/// increasing, then a live one, and the tail returns when its reader goes. The fetch link
/// waits for the one-minute hang detector, so no fetch the broker answers is held to a
/// budget.
/// NEGATIVE CONTROL (run): without the `caught` call the merge holds the last record for
/// the other face's stream, which never says it is past it: the hang detector fires.
#[cfg(unix)]
#[test]
fn a_tail_reads_each_face_of_a_broker_that_refuses_idle_marks_on_by_fetches_in_offset_order() {
    use aterm_spec::wait::MUST_HAPPEN;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-tail-older")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let mut world = TwoFaces::new(dir.path());
    let older = dir.path().join("older.sock");
    let _stall = broker_in_front(&world.real, &older, true);
    let cfg = world.cfg(&older);
    let offs: Vec<u64> = (0..12).map(|i| world.publish(i % 2)).collect();
    let start = offs[3];
    let want: Vec<u64> = offs.iter().copied().filter(|off| *off >= start).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut out = Sent {
        tx,
        buf: Vec::new(),
        lines: want.len() + 1,
    };
    std::thread::spawn(move || {
        let ended = TailLink::open(&cfg, None, MUST_HAPPEN)
            .and_then(|link| tail_on(&cfg, link, TailFmt::new(false, true), Some(start), &mut out));
        let _ = done_tx.send(ended.map_err(|e| e.to_string()));
    });
    let mut printed = Vec::new();
    while printed.len() < want.len() {
        let line = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the records from @{start} ({printed:?}): {e}"));
        printed.extend(tail_off(&line));
    }
    assert!(
        printed.windows(2).all(|w| w[0] < w[1]),
        "offsets strictly increasing: {printed:?}"
    );
    assert_eq!(printed, want, "every record at or above the start, once");
    let live = world.publish(0);
    let line = rx
        .recv_timeout(MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("the live record @{live}: {e}"));
    assert_eq!(tail_off(&line), Some(live), "{line}");
    world.publish(1);
    assert_eq!(
        done_rx.recv_timeout(MUST_HAPPEN).expect("the tail returns"),
        Ok(()),
        "it ends when its reader does"
    );
}

/// THE TAIL'S FETCH LINK WAITS AS ASTREAM LETS A STREAM WAIT: where the caps do not grant
/// the fleet's root, the link [`tail`] opens ([`TailLink::for_tail`]) reads the faces apart
/// and is opened with [`astream_broker::IDLE_MARK_DEADLINE`], never the 3 s of a one-shot
/// request ([`IO_TIMEOUT`]), as a live tail fetches for as long as it runs. It reads the
/// deadline [`TailLink::open`] recorded (astream's closer has no getter for a socket's
/// timeout); that `open` sets it on the socket is shown by
/// `a_fetch_the_broker_never_answers_ends_the_tail_naming_the_face_and_the_from_that_resumes_it`.
/// NEGATIVE CONTROL (run): [`IO_TIMEOUT`] in `for_tail` fails it.
#[cfg(unix)]
#[test]
fn the_tails_fetch_link_waits_the_idle_mark_deadline_never_a_requests_timeout() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-tail-link")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let world = TwoFaces::new(dir.path());
    let link = TailLink::for_tail(&world.cfg(&world.real), None).expect("the tail's link");
    let faces: Vec<String> = TwoFaces::FACES.into_iter().map(String::from).collect();
    assert_eq!(link.faces, Some(faces), "the faces are read apart");
    assert_eq!(link.read_timeout, astream_broker::IDLE_MARK_DEADLINE);
}

/// A FETCH THE BROKER NEVER ANSWERS ENDS THE TAIL AT ITS LINK'S DEADLINE, NAMING THE FACE
/// AND THE `--from` THAT RESUMES IT: the broker answers both faces' subscriptions (each
/// backlog, then each stream's catch-up mark, so the backlog prints with no fetch; the last
/// record is on the second face, so the first face's mark is read before it prints), then
/// takes the told fetch a live record on the second face needs of the first and never
/// replies. Opened with a 1 s deadline, the value under test (that fetch is the first
/// request the link sends past its opening, so no reply that comes is held to it), the
/// tail ends with an error of a timeout's kind that names the first face and, by the
/// producer's own words ([`resume_from`]), the merge's floor, every record below it
/// printed. The one-second limit is this silent-fetch adversary's explicit contract;
/// a minute hang detector bounds waiting for its result.
#[cfg(unix)]
#[test]
fn a_fetch_the_broker_never_answers_ends_the_tail_naming_the_face_and_the_from_that_resumes_it() {
    use aterm_spec::wait::MUST_HAPPEN;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-tail-stall")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let mut world = TwoFaces::new(dir.path());
    let front = dir.path().join("front.sock");
    let stall = broker_in_front(&world.real, &front, false);
    let cfg = world.cfg(&front);
    let offs: Vec<u64> = [0, 1, 0, 1, 0, 1]
        .into_iter()
        .map(|face| world.publish(face))
        .collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut out = Sent {
        tx,
        buf: Vec::new(),
        lines: usize::MAX,
    };
    std::thread::spawn(move || {
        let ended = TailLink::open(&cfg, None, Duration::from_secs(1))
            .and_then(|link| tail_on(&cfg, link, TailFmt::new(false, true), Some(0), &mut out));
        let _ = done_tx.send(ended.map_err(|e| (e.kind(), e.to_string())));
    });
    let mut printed = Vec::new();
    while printed.len() < offs.len() {
        let line = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the backlog ({printed:?}): {e}"));
        printed.extend(tail_off(&line));
    }
    assert_eq!(printed, offs, "the backlog, in offset order");
    let detector = Instant::now() + MUST_HAPPEN;
    stall.store(true, std::sync::atomic::Ordering::SeqCst);
    world.publish(1);
    let ended = done_rx
        .recv_timeout(detector.saturating_duration_since(Instant::now()))
        .expect("the tail ends at its link's deadline");
    let (kind, said) = ended.expect_err("a fetch the broker never answers ends the tail");
    assert!(
        matches!(kind, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
        "{kind:?}: {said}"
    );
    assert!(
        said.contains(&safe(TwoFaces::FACES[0], 128)),
        "it names the face: {said}"
    );
    let floor = printed.last().map_or(0, |off| off + 1);
    assert!(
        said.contains(&resume_from(floor)),
        "a resume from the floor, @{floor}: {said}"
    );
    assert!(rx.try_recv().is_err(), "nothing past the floor printed");
}

#[test]
fn a_tail_filter_passes_astreams_grammar_then_aterms_narrower_one() {
    use astream_wire::subject::validate_filter_str as astream;
    for f in ["/f/lab/>/x", "/f/>/pub/>", "/f/lab/>/>"] {
        assert!(astream(f).is_err(), "astream must refuse {f}");
        assert!(!is_subject_filter(f), "{f} passed astream's half");
    }
    let long = format!("/f/lab/{}", "a".repeat(250));
    assert_eq!(long.len(), 257, "one byte past the cap");
    for f in [
        "/x/lab/>",
        long.as_str(),
        "/f/lab/pub/ /say",
        "/f/lab/a;b",
        "/f/lab/caf\u{e9}",
    ] {
        assert!(astream(f).is_ok(), "astream must accept {f}");
        assert!(!is_subject_filter(f), "{f} passed aterm's half");
    }
    let at_cap = format!("/f/lab/{}", "a".repeat(249));
    for f in [
        "/f/lab/>",
        "/f/lab/fleet/>",
        "/f/lab/fleet/*/halt",
        "/f/lab/pub/*/*/say/>",
        "/f/lab/pub/*/*/presence",
        "/f/lab/pub/n-00000000000000aa/*/presence",
        "/f/lab/pub/*/s-1/presence",
        "/f/lab/pub/*/*/ev",
        "/f/lab/pub/n-a/>",
        "/f/lab/in/n-a/>",
        "/f/lab/in/n-a/s-1/>",
        "/f/lab/in/*/*/n-a/*",
        "/f/lab/in/p/h-x/>",
        "/f/lab/in/p/h-x/*/ask",
        at_cap.as_str(),
    ] {
        assert!(astream(f).is_ok(), "astream must accept {f}");
        assert!(is_subject_filter(f), "{f} must pass");
    }
}

#[test]
fn the_command_is_read_from_every_toml_spelling_and_blank_is_off() {
    let cmd = "aterm link serve --fleet f --broker /tmp/b.sock";
    let want = words(cmd);
    for text in [
        format!("[fabric]\ncommand = \"{cmd}\"\n"),
        format!("fabric.command = \"{cmd}\"\n"),
        format!("fabric = {{ command = \"{cmd}\" }}\n"),
        format!("font_px = 12.0\n\n[fabric]\n# a comment\ncommand = '{cmd}'\n\n[keys]\n"),
        format!("[fabric]\ncommand = \"\"\"{cmd}\"\"\"\n"),
        "[fabric]\ncommand = [\"aterm\", \"link\", \"serve\", \"--fleet\", \"f\", \"--broker\", \"/tmp/b.sock\"]\n"
            .to_string(),
        "[fabric]\ncommand = [\n  \"aterm\", 'link', \"serve\", # the verb\n  \"--fleet\", \"f\",\n  \"--broker\", \"/tmp/b.sock\",\n]\n"
            .to_string(),
    ] {
        assert_eq!(command_in_toml(&text), Ok(Some(want.clone())), "{text}");
    }
    let spaced = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
    assert_eq!(
        command_in_toml(&format!("[fabric]\ncommand = [\"{spaced}\", \"link\"]\n")),
        Ok(Some(vec![spaced.to_string(), "link".to_string()]))
    );
    assert_eq!(
        command_in_toml(&format!("[fabric]\ncommand = \"{spaced} link\"\n")),
        Ok(Some(words(&format!("{spaced} link")))),
    );
    assert_eq!(words(&format!("{spaced} link")).len(), 3);
    for off in [
        "",
        "font_px = 1.0\n",
        "[fabric]\n",
        "[fabric]\ncommand = \"   \"\n",
        "[fabric]\ncommand = []\n",
    ] {
        assert_eq!(command_in_toml(off), Ok(None), "{off:?}");
    }
    for (bad, why) in [
        ("[fabric]\ncommand = 3\n", "is a TOML integer"),
        ("fabric = 1\n", "not a table"),
        ("[fabric\n", "not valid TOML"),
        (
            "[fabric]\ncommand = [\"aterm\", \"\"]\n",
            "element 2 is empty",
        ),
        (
            "[fabric]\ncommand = [\"a\\u0000b\"]\n",
            "element 1 holds a NUL",
        ),
        (
            "[fabric]\ncommand = [\"aterm\", 7]\n",
            "element 2 is not a string",
        ),
        ("[fabric]\ncommand = { a = 1 }\n", "is a TOML table"),
    ] {
        let err = command_in_toml(bad).expect_err(bad);
        assert!(err.contains(why), "{bad:?}: {err}");
    }
}

#[test]
fn the_status_header_names_the_argv_whole() {
    let argv = vec![
        "/a b/aterm".to_string(),
        "link".to_string(),
        "serve".to_string(),
        "--state".to_string(),
        "/s/100%".to_string(),
    ];
    let header = format!(
        "OK state=connected supervised=1 command={} argv={} reason=-",
        crate::pct::encode(&argv.join(" ")),
        crate::pct::encode(&aterm_codec::pct::join_words(&argv))
    );
    assert_eq!(status_argv(&header), Some(argv.clone()));
    let without = header.replace(" argv=", " was=");
    assert_ne!(
        status_argv(&without),
        Some(argv.clone()),
        "command= alone splits"
    );
    let old = words("/usr/bin/aterm link serve --state /s/100%41");
    let old_header = format!(
        "OK state=connected supervised=1 command={} reason=-",
        crate::pct::encode(&old.join(" "))
    );
    assert_eq!(
        status_argv(&old_header),
        Some(old.clone()),
        "an older instance's"
    );
    assert_ne!(
        aterm_codec::pct::split_words(&old.join(" ")),
        old,
        "decoding its words again would change them"
    );
    for none in [
        "OK state=absent supervised=0 command=- argv=- reason=-",
        "OK state=absent supervised=0 command=- reason=-",
        "OK state=absent",
    ] {
        assert_eq!(status_argv(none), None, "{none}");
    }
    assert_eq!(shown_argv(&argv), "'/a b/aterm' link serve --state /s/100%");
    assert_eq!(shell_word("it's"), "'it'\\''s'");
    assert_eq!(bridge_program(&argv).as_deref(), Some("'/a b/aterm'"));
}

#[test]
fn the_presence_mode_is_read_from_the_toml_and_absent_is_meta() {
    use crate::presence::Mode;
    assert_eq!(
        presence_in_toml("[fabric]\ncommand = \"x\"\npresence = \"minimal\"\n"),
        Ok(Some(Mode::Minimal))
    );
    assert_eq!(
        presence_in_toml("fabric = { presence = \"meta\" }\n"),
        Ok(Some(Mode::Meta))
    );
    for absent in ["", "[fabric]\ncommand = \"x\"\n", "font_px = 1.0\n"] {
        assert_eq!(presence_in_toml(absent), Ok(None), "{absent:?}");
    }
    for bad in [
        "[fabric]\npresence = \"full\"\n",
        "[fabric]\npresence = 1\n",
        "fabric = 1\n",
    ] {
        assert!(presence_in_toml(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn sessions_show_the_broadcast_topics_and_a_dash_for_none() {
    let mut r = healthy();
    r.sessions.clear();
    let mut listening = r.instances[0].sessions[0].clone();
    listening.sid = "s-ears".to_string();
    listening.topics = vec!["build.failed".to_string(), "sat-comp".to_string()];
    r.instances[0].sessions.push(listening);
    join_sessions(&mut r, None);
    let json = render_json(&r);
    assert!(
        json.contains("\"topics\":[\"build.failed\",\"sat-comp\"]"),
        "{json}"
    );
    assert!(json.contains("\"topics\":[]"), "{json}");
    r.sessions.push(SessionRow {
        sid: "s-gone".to_string(),
        pid: None,
        node: "n-a".to_string(),
        bus: "gone".to_string(),
        local: None,
        bus_hold: None,
        bus_role: None,
        bus_title: None,
        detail: None,
        phase: None,
        context: None,
    });
    let json = render_json(&r);
    let gone = json
        .split("\"sid\":\"s-gone\"")
        .nth(1)
        .expect("the gone row");
    assert!(gone.contains("\"topics\":null"), "{gone}");
}

#[test]
fn sessions_show_the_presence_rows_meaning_fields() {
    let mut r = healthy();
    let (_, body) = (
        "",
        b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=- role=bus-role \
        detail=claude phase=busy context=12%25 title=bus%20title",
    );
    let local_row =
        crate::glance::Row::parse("lab", 5, "/f/lab/pub/n-a/s-one/presence", body).expect("a row");
    let remote_row = crate::glance::Row::parse("lab", 6, "/f/lab/pub/n-far/s-far/presence", body)
        .expect("a row");
    let old_row = crate::glance::Row::parse(
        "lab",
        7,
        "/f/lab/pub/n-far/s-old/presence",
        b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=-",
    )
    .expect("a row");
    let roster = crate::glance::Glance {
        fleet: "lab".to_string(),
        rows: vec![local_row, remote_row, old_row],
        truncated: false,
    };
    join_sessions(&mut r, Some(&roster));
    assert_eq!(r.sessions.len(), 3, "{:?}", r.sessions);
    let _ = render_text(&r);
    let json = render_json(&r);
    assert!(json.contains("\"phase\":\"busy\""), "{json}");
    assert!(json.contains("\"context\":12"), "{json}");
    assert!(json.contains("\"detail\":\"claude\""), "{json}");
    assert!(json.contains("\"presence\":\"meta\""), "{json}");
    assert!(json.contains("\"title\":\"bus title\""), "{json}");
}

#[test]
fn the_program_row_names_a_development_build_by_cargos_tag() {
    let scratch = crate::test_scratch_dir("prog");
    let dir = scratch.path();
    let debug = dir.join("target").join("debug");
    std::fs::create_dir_all(&debug).expect("scratch");
    let aterm = debug.join("aterm");
    assert!(!in_cargo_target_dir(&aterm), "untagged");
    std::fs::write(dir.join("target").join("CACHEDIR.TAG"), "Signature: x\n").expect("tag");
    assert!(in_cargo_target_dir(&aterm), "tagged");
    assert!(!in_cargo_target_dir(Path::new("aterm")), "a bare name");
    assert!(
        !in_cargo_target_dir(Path::new("target/debug/aterm")),
        "relative"
    );

    let mut r = healthy();
    r.program_in_target_dir = true;
    r.warnings = warned(&r);
    assert_eq!(exit_of(&r), 0, "informational, never a warning");
}

#[test]
fn the_env_overrides_the_file_and_a_missing_file_is_off() {
    let scratch = crate::test_scratch_dir("cfg");
    let dir = scratch.path();
    let path = dir.join("aterm.toml");
    aterm_uds::atomic_file::write(
        &path,
        b"[fabric]\ncommand = \"aterm-link serve --fleet file\"\n",
        0o600,
    )
    .expect("write");
    let (src, c) = resolve_command(None, Some(&path))
        .expect("ok")
        .expect("some");
    assert_eq!(src, Source::File(path.clone()));
    assert!(c.ends_with(&["--fleet".to_string(), "file".to_string()]));
    let (src, c) = resolve_command(Some("aterm-link serve --fleet env"), Some(&path))
        .expect("ok")
        .expect("some");
    assert_eq!(src, Source::Env);
    assert!(c.ends_with(&["--fleet".to_string(), "env".to_string()]));
    let (src, _) = resolve_command(Some("  "), Some(&path))
        .expect("ok")
        .expect("some");
    assert_eq!(src, Source::File(path.clone()), "a blank env is absent");
    assert_eq!(
        resolve_command(None, Some(&dir.join("absent.toml"))),
        Ok(None)
    );
    assert_eq!(resolve_command(None, None), Ok(None));
    aterm_uds::atomic_file::write(&path, b"[fabric\n", 0o600).expect("write");
    assert!(resolve_command(None, Some(&path)).is_err());
}

#[test]
fn both_spellings_of_the_bridge_command_parse_with_the_bridges_own_parser() {
    let tail = "--fleet lab --broker /tmp/b.sock --cap-file /c --state /s --accept-from n-1";
    for cmd in [
        format!("aterm link serve {tail}"),
        format!("/Users//u/.local/bin/aterm link serve {tail}"),
        format!("/Applications/aterm.app/Contents/MacOS/aterm link serve {tail}"),
        format!("aterm-link serve {tail}"),
        format!("/opt/x/aterm-link   serve  {tail}"),
    ] {
        let cfg = bridge_config(&words(&cmd)).expect(&cmd);
        assert_eq!(cfg.fleet, "lab", "{cmd}");
        assert_eq!(cfg.broker, "/tmp/b.sock");
        assert_eq!(cfg.cap_files, vec!["/c".to_string()]);
        assert_eq!(cfg.state_dir, "/s");
        assert_eq!(cfg.accept_from, vec!["n-1".to_string()]);
    }
    for not_a_bridge in [
        "aterm link broker /tmp/b.sock",
        "serve --fleet x --broker y",
        "/usr/bin/false",
        "aterm-link ls --fleet x --broker y",
    ] {
        assert!(
            serve_flags(&words(not_a_bridge)).is_none(),
            "{not_a_bridge}"
        );
        assert!(bridge_config(&words(not_a_bridge)).is_err());
    }
    assert!(bridge_config(&words("aterm link serve --fleet lab")).is_err());
}

#[cfg(unix)]
#[test]
fn a_bridges_program_is_the_file_it_runs_under_any_spelling() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-prog")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
    std::fs::write(at("aterm"), b"#!/bin/sh\n").expect("program");
    std::fs::write(at("other"), b"#!/bin/sh\n").expect("another program");
    std::os::unix::fs::symlink(at("aterm"), at("aterm-link")).expect("alias");
    std::os::unix::fs::symlink(at("aterm"), at("on-path")).expect("link");
    let flags = "--fleet lab --broker /tmp/b.sock --state /s";
    let program = |head: String| bridge_program(&words(&format!("{head} {flags}")));
    let ours = program(format!("{} link serve", at("aterm")));
    assert!(ours.is_some());
    assert_eq!(program(format!("{} serve", at("aterm-link"))), ours);
    assert_eq!(program(format!("{} link serve", at("on-path"))), ours);
    assert_ne!(program(format!("{} link serve", at("other"))), ours);
    let shell = program(format!("/bin/sh -c {} link serve", at("aterm")));
    assert_ne!(shell, ours);
    assert!(
        shell
            .as_deref()
            .is_some_and(|p| p.ends_with(&format!(" -c {}", at("aterm")))),
        "{shell:?}"
    );
    assert_eq!(bridge_program(&words("aterm link ls --fleet lab")), None);
}

/// An instance's bridges are its aterm children whose words serve a fleet, by either
/// spelling: a broker child and a child whose words were not read are none. NEGATIVE
/// CONTROL: count a child with unread words as a bridge, and it joins.
#[test]
fn an_instance_s_bridges_are_its_aterm_children_that_serve() {
    use aterm_ctl::census::Child;
    let child = |pid: u32, words: Option<&[&str]>| Child {
        pid,
        words: words.map(|w| w.iter().map(ToString::to_string).collect()),
    };
    let children = [
        child(71, Some(&["/x/aterm", "link", "serve", "--fleet", "f"])),
        child(72, Some(&["aterm-link", "serve", "--fleet", "f"])),
        child(73, Some(&["aterm", "link", "broker", "/s", "/l"])),
        child(74, None),
    ];
    assert_eq!(bridges_among(&children), [71, 72]);
}

#[test]
fn the_launchd_label_is_read_from_launchctl() {
    let launchctl =
        "PID\tStatus\tLabel\n-\t0\tcom.apple.x\n48078\t0\tsystems.alab.astream-broker\n";
    assert_eq!(
        parse_launchctl(launchctl, 48078).as_deref(),
        Some("systems.alab.astream-broker")
    );
    assert_eq!(parse_launchctl(launchctl, 1), None);
}

#[test]
fn unread_is_listed_minus_handled_and_posts_are_the_unlanded_rows() {
    let mut s = LocalSession::default();
    let header0 = "OK 2 hold=1 holder=h-andrew seen=3 bus_head=40 dropped=2 pending=1";
    let posts = vec![
        "post 7 to=@s-a kind=task off=- len=5".to_string(),
        "post 8 to=@s-b kind=note off=- len=9".to_string(),
    ];
    let full = vec![
        "msg 2 off=10 t=5 from=h-andrew kind=task trust=human len=3".to_string(),
        "msg 3 off=11 t=6 from=s-x@n-y kind=note trust=agent len=3".to_string(),
        "msg 4 off=12 t=7 from=s-x@n-y kind=ask trust=agent len=3".to_string(),
        "msg 5 off=13 t=8 from=h-andrew kind=task trust=human len=3".to_string(),
        "msg 6 off=14 t=9 from=s-x@n-y kind=note trust=agent re=3 len=3".to_string(),
        "post 7 to=@s-a kind=task off=- len=5".to_string(),
    ];
    fill_inbox(&mut s, header0, &posts, &full);
    assert_eq!(s.hold, Some(1));
    assert_eq!(s.holder, "h-andrew");
    assert_eq!(s.seen, Some(3));
    assert_eq!(s.dropped, Some(2));
    assert_eq!(s.pending, Some(1));
    assert_eq!(s.posts, Some(2));
    assert_eq!(
        s.unhandled.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![4, 5, 6],
        "rows at or below seen= are handled"
    );
    assert_eq!(
        s.unread,
        Some(2),
        "3 above the watermark, 1 of them never listed"
    );
    assert_eq!(s.unhandled[0].kind, "ask");
    assert_eq!(s.unhandled[0].off, 12);
}

#[test]
fn the_hold_reason_is_the_last_hold_transition_in_the_timeline() {
    let tl = vec![
        "event 1 t=1 kind=spawned".to_string(),
        "event 2 t=2 kind=hold 1 reason=main%20broken origin=local".to_string(),
        "event 3 t=3 kind=inbox 1 from=h-a kind=note off=4".to_string(),
        "event 4 t=4 kind=hold 1 reason=fabric-lost origin=fleet".to_string(),
    ];
    assert_eq!(
        last_hold(&tl),
        Some(("fabric-lost".to_string(), "fleet".to_string()))
    );
    let lifted = [
        tl.clone(),
        vec!["event 5 t=5 kind=hold 0 reason=- origin=local".to_string()],
    ]
    .concat();
    assert_eq!(last_hold(&lifted), None);
    assert_eq!(
        last_hold(&tl[..2]),
        Some(("main broken".to_string(), "local".to_string()))
    );
    assert_eq!(last_hold(&tl[..1]), None);
    let both = [
        tl.clone(),
        vec![
            "event 6 t=6 kind=hold 1 reason=stop origin=fleet hold_origin=fleet,local".to_string(),
        ],
    ]
    .concat();
    assert_eq!(
        last_hold(&both),
        Some(("stop".to_string(), "fleet,local".to_string()))
    );
    let left = [
        both,
        vec!["event 7 t=7 kind=hold 1 reason=mine origin=local hold_origin=local".to_string()],
    ]
    .concat();
    assert_eq!(
        last_hold(&left),
        Some(("mine".to_string(), "local".to_string()))
    );
}

#[test]
fn traffic_rows_classify_every_face_and_carry_no_text_in_their_columns() {
    let t = traffic_of(
        "lab",
        4,
        "/f/lab/in/n-a/s-w/n-a/task",
        b"v=1 t=1757000000000 from=s-me text=secret%20plan",
        0,
    );
    assert_eq!(t.from, "s-me@n-a", "the node attests which session spoke");
    assert_eq!(t.to, "s-w@n-a");
    assert_eq!(t.kind, "task");
    assert_eq!(t.trust, "agent");
    assert_eq!(t.len, "secret plan".len());
    assert_eq!(t.t_ms, 1_757_000_000_000);
    let cells = traffic_cells(&t, t.t_ms, 0).join(" ");
    assert!(!cells.contains("secret"), "{cells}");
    assert!(!tail_line(&t, t.t_ms, 0, false).contains("secret"));
    assert!(tail_line(&t, t.t_ms, 0, true).ends_with("text=secret plan"));

    let mirrored = b"v=2 t=1757000000001 ep=1 seq=4 ms=3 op=close node=r2 k=cmd out=fail code=1 src=shell conf=exact";
    let t = traffic_of("lab", 9, "/f/lab/pub/n-a/s-w/ev", mirrored, 0);
    assert_eq!((t.kind.as_str(), t.trust), ("ev", "shell"));
    assert_eq!(t.len, mirrored.len());
    assert_eq!(t.t_ms, 1_757_000_000_001);
    let line = tail_line(&t, t.t_ms, 0, true);
    assert!(line.contains(" op=close node=r2 k=cmd out=fail "), "{line}");
    assert!(!tail_line(&t, t.t_ms, 0, false).contains("op=close"));
    let other = traffic_of("lab", 9, "/f/lab/pub/n-a/s-w/presence", mirrored, 0);
    assert_eq!(other.kind, "presence");

    let h = traffic_of("lab", 5, "/f/lab/in/n-a/s-w/h-andrew/ask", b"v=1 t=1", 0);
    assert_eq!((h.from.as_str(), h.trust), ("h-andrew", "human"));
    let r = traffic_of(
        "lab",
        6,
        "/f/lab/in/n-a/s-w/h-andrew/ask",
        b"v=1 t=1 via=n-b",
        0,
    );
    assert_eq!(r.trust, "relayed");
    let forged = traffic_of(
        "lab",
        7,
        "/f/lab/in/n-a/s-w/s-evil/note",
        b"v=1 from=h-andrew",
        0,
    );
    assert_eq!(forged.from, "s-evil");

    // A record whose meaning is in its fields, not a `text=` (fix unit E5, E2E-10): its
    // body is its whole first line and `len` that line's bytes, shown under `--bodies`
    // alone. NEGATIVE CONTROL (scratch): today's decoded-text rule gives `len` 0 and an
    // empty body.
    let row: &[u8] = b"v=1 t=2 state=live attention=QZP7WORD1";
    let p = traffic_of("lab", 1, "/f/lab/pub/n-a/s-w/presence", row, 0);
    assert_eq!(
        (p.from.as_str(), p.to.as_str(), p.kind.as_str(), p.len),
        ("s-w@n-a", "-", "presence", row.len())
    );
    assert_eq!(p.text.as_bytes(), row);
    let subject = "/f/lab/pub/n-a/s-w/presence";
    assert!(!traffic_cells(&p, 0, 0).join(" ").contains("QZP7WORD1"));
    assert!(!tail_line(&p, 0, 0, false).contains("QZP7WORD1"));
    assert!(!tail_json("lab", subject, &p, false).contains("QZP7WORD1"));
    assert!(tail_line(&p, 0, 0, true).ends_with("text=v=1 t=2 state=live attention=QZP7WORD1"));
    assert!(tail_json("lab", subject, &p, true).contains("attention=QZP7WORD1"));
    // Never a byte past the first newline: a drive record's payload (one a log from before
    // the face's cut may hold) is keystrokes.
    // NEGATIVE CONTROL (scratch): taking the whole raw body prints them.
    let keys: &[u8] = b"v=1 t=1 gen=1:ab len=9\nQZP7KEYS1";
    let k = traffic_of("lab", 13, "/f/lab/term/n-a/s-w/in/h-andrew", keys, 0);
    assert_eq!((k.text.as_str(), k.len), ("v=1 t=1 gen=1:ab len=9", 22));
    for shown in [
        tail_line(&k, 0, 0, true),
        tail_json("lab", "/f/lab/term/n-a/s-w/in/h-andrew", &k, true),
    ] {
        assert!(!shown.contains("QZP7KEYS1"), "{shown}");
    }
    let n = traffic_of("lab", 2, "/f/lab/pub/n-a/node/presence", b"v=1 t=2", 0);
    assert_eq!(n.from, "n-a");
    let s = traffic_of(
        "lab",
        150,
        "/f/lab/pub/n-a1b2/s-c3d4/say/build.failed",
        b"v=1 t=1 kind=task from=s-c3d4 text=x",
        0,
    );
    assert_eq!(
        (s.from.as_str(), s.to.as_str(), s.kind.as_str(), s.trust),
        ("s-c3d4@n-a1b2", "say:build.failed", "task", "agent")
    );
    let bare = traffic_of("lab", 151, "/f/lab/pub/n-a/s-w/say/say", b"v=1 t=1", 0);
    assert_eq!((bare.to.as_str(), bare.kind.as_str()), ("say:say", "note"));
    assert_eq!(
        (bare.text.as_str(), bare.len),
        ("v=1 t=1", 7),
        "a bare body's own line"
    );
    let ev = traffic_of("lab", 3, "/f/lab/pub/n-a/s-w/ev", b"v=1 t=2", 0);
    assert_eq!(ev.trust, SCREEN);
    let halt = traffic_of(
        "lab",
        8,
        "/f/lab/fleet/h-andrew/halt",
        b"v=1 t=2 state=on",
        0,
    );
    assert_eq!(
        (
            halt.from.as_str(),
            halt.to.as_str(),
            halt.kind.as_str(),
            halt.trust
        ),
        ("h-andrew", "fleet", "halt", "human")
    );
    let odd = traffic_of("lab", 9, "/f/lab/pub/h-andrew/say/note", b"v=1 t=2", 0);
    assert_eq!((odd.to.as_str(), odd.kind.as_str()), ("-", "note"));
    // The drive face is cut: its record reads as any face nothing serves, by its subject.
    let key = traffic_of("lab", 10, "/f/lab/term/n-a/s-w/in/h-andrew", b"v=1", 0);
    assert_eq!(
        (
            key.from.as_str(),
            key.to.as_str(),
            key.kind.as_str(),
            key.trust
        ),
        ("-", "/f/lab/term/n-a/s-w/in/h-andrew", "term", SCREEN)
    );
    let odd = traffic_of("lab", 11, "/f/lab/zzz/q", b"", 0);
    assert_eq!((odd.kind.as_str(), odd.trust), ("zzz", SCREEN));
    let alien = traffic_of("lab", 11, "/f/other/in/n-a/s-w/n-a/note", b"", 0);
    assert_eq!((alien.kind.as_str(), alien.trust), ("unknown", SCREEN));

    let hostile = traffic_of("lab", 12, "/f/lab/in/n-a/s-w/n-\u{1b}[2J/note", b"v=1", 0);
    let line = tail_line(&hostile, 0, 0, true);
    assert!(!line.contains('\u{1b}'), "{line:?}");
}

/// When the carried-post tests' certificate was signed, and the post: a reader's clock.
const CARRIED_AT: u64 = 1_800_000_000_000;

/// A post of h-andrew's to session `s-q`, of `kind`, sealed on the Mac whose node seed is
/// `[51; 32]` and carried by that Mac's mesh (§11 C3c, `federate::Routes::carry_one`): the
/// Mac's node, and the body its lane holds, `by=h-andrew` beside the seal.
fn carried_post(kind: &str) -> (String, Body) {
    use crate::person_key::{post_address, read_cert, vouch, PersonKey, BY_TOKEN};
    let dir = crate::test_scratch_dir("fab");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private dir");
    let seed = [51u8; 32];
    assert!(vouch(dir.path(), &seed, CARRIED_AT).is_ok());
    let key = PersonKey::read(dir.path()).expect("reads").expect("made");
    let cert = read_cert(dir.path()).expect("reads").expect("vouched");
    let mut body = Body::new(CARRIED_AT);
    body.dl = Some(60_000);
    body.text = "QZP9PLAN".to_owned();
    key.seal(
        &cert,
        &post_address("lab", "s-q", "h-andrew", kind),
        &mut body,
    );
    body.unknown
        .insert(BY_TOKEN.to_owned(), "h-andrew".to_owned());
    let node = crate::federate::node_of(&astream_aead::signature::public_key(&seed));
    (node, body)
}

/// **A CARRIED PERSON'S POST READS AS THE PERSON ONLY WHILE ITS SEAL VERIFIES** (§11 C3d): a
/// post of h-andrew's that the Mac P carried onto its own lane to session s-q on Q,
/// `by=h-andrew` beside the seal, is the person's in every observer view (`traffic_of`, the
/// TRAFFIC cells, `tail`, `tail --json`, the deadline scan), `from=h-andrew@<P>
/// trust=human`, once its seal verifies, at the reader's clock, under the certificate of the
/// node key that names P, as Q's bridge checks it (`person_key::carried_by`).
/// NEGATIVE CONTROLS, each the same record less one premise, each refused for its reason and
/// shown as the lane's writer and `agent`, never under the name it claims: the same bytes on
/// another Mac's lane (not vouched), `by=` naming another person and its text changed (each
/// a bad seal), its seal taken off (unsealed), a body `from=s-x` beside a failed claim (the
/// Mac still, not `s-x@<P>`), `by=` naming no person (malformed), and a reader whose clock
/// is past the certificate's life (lapsed, though nothing in the post changed); one relayed
/// (`via=`) reads `relayed`.
#[test]
fn a_carried_persons_post_reads_as_the_person_only_while_its_seal_verifies() {
    use crate::person_key::{carried_by, Refusal, BY_TOKEN, CERT_LIFE_MS, CERT_TOKEN, SEAL_TOKEN};
    let (p, post) = carried_post("ask");
    let q = crate::federate::node_of(&[52; 32]);
    let other = crate::federate::node_of(&[53; 32]);
    let lane = |writer: &str| format!("/f/lab/in/{q}/s-q/{writer}/ask");
    // Every view of the record: its classification, its TRAFFIC cells' trust and sender,
    // its `tail` line's words and its `tail --json` object.
    let views = |subject: &str, body: &Body, now: u64| {
        let t = traffic_of("lab", 40, subject, &body.encode(), now);
        let cells = traffic_cells(&t, now, 0);
        let line = tail_line(&t, now, 0, false);
        let json = tail_json("lab", subject, &t, false);
        (t, (cells[4].clone(), cells[5].clone()), line, json)
    };
    let shows = |what: &str, subject: &str, body: &Body, now: u64, (from, trust): (&str, &str)| {
        let (t, cells, line, json) = views(subject, body, now);
        assert_eq!((t.from.as_str(), t.trust), (from, trust), "{what}");
        assert_eq!(t.to, format!("s-q@{q}"), "{what}");
        assert_eq!(cells, (trust.to_owned(), from.to_owned()), "{what}");
        let words: Vec<&str> = line.split_whitespace().collect();
        assert!(
            words.contains(&from) && words.contains(&trust),
            "{what}: {line}"
        );
        for (key, want) in [("from", from), ("trust", trust)] {
            assert!(
                json.contains(&format!("\"{key}\":\"{want}\"")),
                "{what}: {json}"
            );
        }
        assert!(
            !line.contains("QZP9PLAN") && !json.contains("QZP9PLAN"),
            "{what}"
        );
    };
    let person = format!("h-andrew@{p}");
    shows("verified", &lane(&p), &post, CARRIED_AT, (&person, "human"));
    assert_eq!(
        carried_by("lab", "s-q", &p, "ask", &post, CARRIED_AT),
        Ok(Some(person.clone()))
    );
    let late = CARRIED_AT + 60_001;
    let overdue = overdue_of("lab", &[(40, lane(&p), post.encode())], late);
    assert_eq!(
        overdue.iter().map(|o| o.from.as_str()).collect::<Vec<_>>(),
        vec![person.as_str()],
        "the deadline scan names the person"
    );

    let with = |change: &dyn Fn(&mut Body)| {
        let mut body = post.clone();
        change(&mut body);
        body
    };
    let lapsed_at = CARRIED_AT + CERT_LIFE_MS + 1;
    let controls: [(&str, &str, Body, u64, Refusal); 7] = [
        (
            "another Mac's lane",
            &other,
            post.clone(),
            CARRIED_AT,
            Refusal::NotVouched,
        ),
        (
            "by= another person",
            &p,
            with(&|b| {
                b.unknown.insert(BY_TOKEN.to_owned(), "h-bob".to_owned());
            }),
            CARRIED_AT,
            Refusal::BadSeal,
        ),
        (
            "its text changed",
            &p,
            with(&|b| b.text.push('!')),
            CARRIED_AT,
            Refusal::BadSeal,
        ),
        (
            "its seal taken off",
            &p,
            with(&|b| {
                b.unknown.remove(CERT_TOKEN);
                b.unknown.remove(SEAL_TOKEN);
            }),
            CARRIED_AT,
            Refusal::Unsealed,
        ),
        (
            "from=s-x beside a failed claim",
            &p,
            with(&|b| {
                b.from = Some("s-x".to_owned());
                b.text.push('!');
            }),
            CARRIED_AT,
            Refusal::BadSeal,
        ),
        (
            "by= no person",
            &p,
            with(&|b| {
                b.unknown.insert(BY_TOKEN.to_owned(), "s-evil".to_owned());
            }),
            CARRIED_AT,
            Refusal::Malformed,
        ),
        (
            "a reader past its certificate's life",
            &p,
            post.clone(),
            lapsed_at,
            Refusal::Lapsed,
        ),
    ];
    for (what, writer, body, now, refusal) in &controls {
        assert_eq!(
            carried_by("lab", "s-q", writer, "ask", body, *now),
            Err(*refusal),
            "{what}"
        );
        shows(what, &lane(writer), body, *now, (writer, "agent"));
    }
    let overdue = overdue_of("lab", &[(40, lane(&p), post.encode())], lapsed_at);
    assert_eq!(
        overdue.iter().map(|o| o.from.as_str()).collect::<Vec<_>>(),
        vec![p.as_str()],
        "the deadline scan judges the certificate at the reader's clock"
    );
    let relayed = with(&|b| b.via = Some(other.clone()));
    assert_eq!(
        carried_by("lab", "s-q", &p, "ask", &relayed, CARRIED_AT),
        Err(Refusal::Relayed)
    );
    shows("relayed", &lane(&p), &relayed, CARRIED_AT, (&p, "relayed"));
}

/// **A PERSON'S LANE NAMES A SESSION ON ANOTHER MAC AS ITS BRIDGE WROTE IT** (§11 C3d): only
/// this Mac's own bridge writes a person's lane (`/f/<F>/in/p/<person>/<this node>/<kind>`;
/// no peer's ring reaches one), and it names a session on another Mac whose answer or
/// receipt it hands the person `from=<sid>@<node>`, which TRAFFIC, `tail` and the person's
/// inbox line show as it is, the writer's trust beside it.
/// NEGATIVE CONTROLS: the same `from` on a session's lane shows the writer node, as before (a
/// node attests only a bare session of its own there); on a person's lane whose writer is no
/// node it shows the writer; and a `from` that is not a session `@` a node shows the writer.
#[test]
fn a_person_lane_names_a_session_on_another_mac_as_its_bridge_wrote_it() {
    let (p, q) = (
        crate::federate::node_of(&[51; 32]),
        crate::federate::node_of(&[52; 32]),
    );
    let named = format!("s-q@{q}");
    let raw = |from: &str| format!("v=1 t=1757000000000 from={from} re=3 text=done");
    let on_person_lane = format!("/f/lab/in/p/h-andrew/{p}/answer");
    let t = traffic_of("lab", 41, &on_person_lane, raw(&named).as_bytes(), 0);
    assert_eq!((t.from.as_str(), t.trust), (named.as_str(), "agent"));
    let line = tail_line(&t, t.t_ms, 0, false);
    assert!(line.split_whitespace().any(|w| w == named), "{line}");
    let inbox = crate::person::inbox_delivery_line(
        ("lab", "h-andrew.cap"),
        41,
        &on_person_lane,
        raw(&named).as_bytes(),
        0,
        Some(0),
        &|_| Some(0),
    );
    assert!(
        inbox.contains(&format!("  answer  from={named} trust=agent re=3")),
        "{inbox}"
    );
    let bare = traffic_of("lab", 42, &on_person_lane, raw("s-x").as_bytes(), 0);
    assert_eq!(
        bare.from,
        format!("s-x@{p}"),
        "a bare session is still the writer's"
    );

    let on_session_lane = format!("/f/lab/in/{p}/s-x/{q}/note");
    let t = traffic_of("lab", 43, &on_session_lane, raw(&named).as_bytes(), 0);
    assert_eq!((t.from.as_str(), t.trust), (q.as_str(), "agent"));
    let by_person = "/f/lab/in/p/h-andrew/h-bob/answer";
    let t = traffic_of("lab", 44, by_person, raw(&named).as_bytes(), 0);
    assert_eq!((t.from.as_str(), t.trust), ("h-bob", "human"));
    for odd in [
        format!("h-x@{q}"),
        "s-q@h-x".to_owned(),
        format!("s-q@{q}@{q}"),
        format!("s-Q@{q}"),
        "s-q@".to_owned(),
    ] {
        let t = traffic_of("lab", 45, &on_person_lane, raw(&odd).as_bytes(), 0);
        assert_eq!(t.from, p, "{odd}");
    }
}

const CMD: &str = "aterm link serve --fleet lab --broker /tmp/b.sock --state /s";

fn healthy() -> Report {
    let session = LocalSession {
        sid: "s-one".to_string(),
        title: "work".to_string(),
        role: "-".to_string(),
        hold: Some(0),
        holder: "-".to_string(),
        seen: Some(3),
        dropped: Some(0),
        pending: Some(0),
        unread: Some(0),
        posts: Some(0),
        ..LocalSession::default()
    };
    Report {
        source: Source::File(PathBuf::from("/c/aterm.toml")),
        command: words(CMD),
        program_in_target_dir: false,
        program: bridge_program(&words(CMD)),
        config_path: Some(PathBuf::from("/c/aterm.toml")),
        cfg: bridge_config(&words(CMD)).expect("a bridge command"),
        presence: crate::presence::Mode::Meta,
        receipts: false,
        node: Some("n-a".to_string()),
        caps: vec![("/c/node.cap".to_string(), Ok(6))],
        wide_caps: Vec::new(),
        broker: BrokerView {
            endpoint: "/tmp/b.sock".to_string(),
            reachable: true,
            attached: true,
            head: Some(10),
            ..BrokerView::default()
        },
        instances: vec![InstanceView {
            pid: 100,
            sock: "/run/aterm-100.sock".to_string(),
            fabric: Some("connected".to_string()),
            rtt_ms: Some(2),
            link_age_ms: Some(3_000),
            supervised: Some(true),
            command: Some(words(CMD)),
            program: bridge_program(&words(CMD)),
            bridge_pids: Some(vec![101]),
            node: Some("n-a".to_string()),
            state_dir: Some("/s".to_string()),
            sessions: vec![session],
            ..InstanceView::default()
        }],
        discovery_error: None,
        sessions: Vec::new(),
        nodes: Vec::new(),
        roster_rows: Some(0),
        roster_truncated: false,
        subjects: Some(Ok(3)),
        exited: 0,
        traffic: Vec::new(),
        overdue: Vec::new(),
        overdue_unjudged: 0,
        warning_items: Vec::new(),
        warnings: Vec::new(),
        now_ms: 100 * 60 * 1000,
    }
}

fn warned_items(r: &Report) -> Vec<Warning> {
    warning_items(r, &BTreeMap::new(), &[])
}

fn warned(r: &Report) -> Vec<String> {
    warnings(r, &BTreeMap::new(), &[])
}

/// A RING WIDER THAN THE NODE'S IS NAMED ON DEMAND (DESIGN-aterm-fabric §14 Q5): `doctor` and
/// `status` warn of each cap file `gather` found wider than the node's ceiling, naming the
/// ceiling check's first violation, with `on`'s keep-aside as the fix; a healthy report,
/// whose rings are within it, warns of none.
#[test]
fn a_ring_wider_than_the_nodes_is_a_warning_whose_fix_is_on() {
    let violation = format!(
        "{}: ring[6] \"rw:/f/lab/>\" is an unbound read-write god capability outside \
         ceiling (x)",
        crate::enable::WIDER_RING
    );
    assert!(
        !warned(&healthy())
            .iter()
            .any(|w| w.contains(crate::enable::WIDER_RING)),
        "CONTROL: a healthy report"
    );
    let mut r = healthy();
    r.wide_caps = vec![("/c/node.cap".to_string(), violation.clone())];
    let w = warned_items(&r);
    let wide: Vec<&Warning> = w
        .iter()
        .filter(|l| l.text.contains(crate::enable::WIDER_RING))
        .collect();
    assert_eq!(
        wide.iter().map(|l| &l.text).collect::<Vec<_>>(),
        [&format!("cap file /c/node.cap: {violation}")],
        "{w:#?}"
    );
    let fix = crate::enable::fix_for(&wide[0].kind, &crate::enable::Role::default());
    assert!(
        fix.contains("<root>/node.cap")
            && fix.contains("keeps aside")
            && fix.contains("no longer attached"),
        "{fix}"
    );
}

/// A ring this report may not read is one warning, whose fix is the file's and not a re-mint
/// `on` will not make, since nothing was asked of the bus. Every line that says why prints the
/// refusal whole, where a long path's `chmod` is its last words. Any other failed read keeps
/// its own warning, the control.
#[test]
fn a_refused_ring_is_one_warning_and_every_report_prints_it_whole() {
    let long = format!("/{}node.cap", "a-folder-with-a-long-name/".repeat(6));
    let chmod = format!("chmod 600 {long}");
    let said = format!(
        "{long} is mode 0644: a secret file must be readable by its owner alone (0600 or \
         tighter); run `{chmod}` and treat its contents as exposed"
    );
    assert!(said.len() > 256 && said.len() < CAP_REFUSAL_WORDS, "{said}");
    let mut r = healthy();
    r.caps = vec![(long.clone(), Err(said.clone()))];
    r.broker.attached = false;
    r.broker.head = None;
    r.broker.cap_unread = true;
    r.broker.error = Some(format!("cap file {long}: {said}"));

    let w = warned_items(&r);
    let about: Vec<&Warning> = w.iter().filter(|l| l.text.contains(&long)).collect();
    assert_eq!(about.len(), 1, "{w:#?}");
    assert!(about[0].text.contains(&chmod), "{w:#?}");
    let fix = crate::enable::fix_for(&about[0].kind, &crate::enable::Role::default());
    assert!(fix.contains("chmod 600 <cap>"), "{fix}");
    assert!(!fix.contains("re-mints"), "{fix}");

    let text = render_text_local(&r, &|_| Some(0));
    assert_eq!(
        text.matches(&chmod).count(),
        2,
        "its row and BROKER's:\n{text}"
    );
    assert!(summary_of(&r.broker).text.contains(&chmod));

    r.broker.cap_unread = false;
    r.broker.error = Some("head query: refused".to_string());
    assert!(
        warned(&r).iter().any(|l| l.contains("could not be read")),
        "{:#?}",
        warned(&r)
    );
}

#[cfg(unix)]
fn unix_config(broker: &std::path::Path) -> Config {
    Config {
        fleet: "f".to_string(),
        broker: broker.to_string_lossy().into_owned(),
        cap_files: Vec::new(),
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        receipts: false,
        log: crate::mirror::LogMode::Off,
        mesh: None,
    }
}

#[cfg(unix)]
/// `broker.err` is a broker job's word about its own root's socket, so it is read only for
/// that socket and only where a broker job keeps the bus: never where the node service
/// serves it, or, with the mesh on, where nothing names who does. NEGATIVE CONTROL: the same
/// root and file read under a broker's keeping, or with the mesh off, give the word.
#[test]
fn only_a_locally_served_broker_is_diagnosed_from_local_supervisor_stderr() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("broker-role")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path();
    std::fs::write(root.join("broker.err"), "old-local-broker-refusal\n").unwrap();
    let unix = unix_config(&root.join("bus.sock"));
    let view = |mesh, host| BrokerView {
        mesh,
        host,
        ..BrokerView::default()
    };
    for (mesh, host) in [
        (false, None),
        (false, Some(Host::LinkBroker)),
        (true, Some(Host::LinkBroker)),
    ] {
        assert!(
            supervisor_last_word(&unix, &view(mesh, host)).is_some(),
            "the local socket keeps its supervisor's word (mesh {mesh}, {host:?})"
        );
    }
    for (mesh, host) in [
        (true, None),
        (true, Some(Host::NodeService)),
        (false, Some(Host::NodeService)),
    ] {
        assert_eq!(
            supervisor_last_word(&unix, &view(mesh, host)),
            None,
            "the node service keeps the bus (mesh {mesh}, {host:?}): broker.err is a retired job's"
        );
    }
    let mut relative = unix.clone();
    relative.broker = "bus.sock".to_string();
    relative.cap_files = vec![root.join("node.cap").to_string_lossy().into_owned()];
    assert_eq!(
        supervisor_last_word(&relative, &view(false, None)),
        None,
        "a cap directory alone does not prove where the broker runs"
    );
}

/// The renewal's health read: a broker answers when its `Hello` comes back, in this protocol
/// or another; a socket nobody holds, or one that accepts and says nothing, does not.
/// NEGATIVE CONTROLS: an `answers` that only connects passes the silent socket; one with no
/// protocol arm fails the broker of another protocol.
#[cfg(unix)]
#[test]
fn a_broker_answers_when_its_hello_comes_back() {
    use astream_broker::proto::PROTO_VERSION;
    use astream_broker::proto::{encode_response, read_frame, write_frame, Response};
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-answers")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let endpoint = |name: &str| dir.path().join(name).to_string_lossy().into_owned();

    let why = answers(&endpoint("none.sock")).expect_err("nobody holds it");
    assert!(why.starts_with("connect: "), "{why}");

    let silent =
        std::os::unix::net::UnixListener::bind(dir.path().join("silent.sock")).expect("bind");
    let closer = std::thread::spawn(move || {
        drop(aterm_uds::accept(
            &silent,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ));
    });
    let why = answers(&endpoint("silent.sock")).expect_err("accepted, said nothing");
    assert!(
        why.starts_with("the socket accepted but no broker answered"),
        "{why}"
    );
    closer.join().expect("the listener");

    let other =
        std::os::unix::net::UnixListener::bind(dir.path().join("other.sock")).expect("bind");
    let replier = std::thread::spawn(move || {
        let (mut c, _) = aterm_uds::accept(
            &other,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        if let Ok(Some(_hello)) = read_frame(&mut c) {
            let mut reply = encode_response(&Response::Error {
                code: astream_broker::proto::ERR_REFUSED,
                msg: "malformed request".to_string(),
            });
            reply[0] = PROTO_VERSION + 1;
            let _ = write_frame(&mut c, &reply);
        }
    });
    assert_eq!(answers(&endpoint("other.sock")), Ok(()));
    replier.join().expect("the replier");

    let broker = crate::test_broker(dir.path().join("b.log")).expect("broker");
    let mut server = broker.serve(dir.path().join("b.sock")).expect("serve");
    assert_eq!(answers(&endpoint("b.sock")), Ok(()));
    server.shutdown();
}

#[cfg(unix)]
#[test]
fn a_broker_in_another_protocol_is_named_with_both_versions() {
    use astream_broker::proto::PROTO_VERSION;
    use astream_broker::proto::{encode_response, read_frame, write_frame, Response};
    let probe_against = |version: u8| {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-proto")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let sock = dir.path().join("bus.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        std::thread::spawn(move || {
            let Ok((mut c, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) else {
                return;
            };
            if let Ok(Some(_hello)) = read_frame(&mut c) {
                let mut reply = encode_response(&Response::Error {
                    code: astream_broker::proto::ERR_REFUSED,
                    msg: "malformed request".to_string(),
                });
                reply[0] = version;
                let _ = write_frame(&mut c, &reply);
            }
        });
        let (view, _) = reach_broker(&unix_config(&sock));
        (view, dir)
    };
    let (view, _dir) = probe_against(PROTO_VERSION + 1);
    let skew = view.protocol.expect("the mismatch is named");
    assert_eq!(
        (skew.broker, skew.client),
        (PROTO_VERSION + 1, PROTO_VERSION)
    );
    assert!(!view.reachable, "{view:?}");
    assert!(
        view.error
            .as_deref()
            .is_some_and(|e| e.starts_with("a broker answered in another astream protocol")),
        "{view:?}"
    );
    let mut r = healthy();
    r.broker = view;
    let w = warned_items(&r);
    let warning = w
        .iter()
        .find(|l| l.text.contains("speaks astream protocol"))
        .unwrap_or_else(|| panic!("no protocol warning: {w:#?}"));
    assert!(
        warning.text.contains(&format!(
            "protocol {} and this aterm speaks {PROTO_VERSION}",
            PROTO_VERSION + 1
        )) && !warning.text.contains("does not answer"),
        "{}",
        warning.text
    );
    assert!(
        crate::enable::fix_for(&warning.kind, &crate::enable::Role::default())
            .contains("different releases"),
        "{}",
        crate::enable::fix_for(&warning.kind, &crate::enable::Role::default())
    );
    let summary = summary_of(&r.broker);
    assert_eq!(summary.level, SummaryLevel::Warn);
    assert!(!summary.unmeasured, "a protocol mismatch was measured");
    assert!(
        summary.text.contains(&format!(
            "speaks astream protocol {} and this aterm speaks {PROTO_VERSION}",
            PROTO_VERSION + 1
        )) && !summary.text.contains("does not answer"),
        "{}",
        summary.text
    );
    let (view, _dir) = probe_against(PROTO_VERSION);
    assert!(view.protocol.is_none(), "{view:?}");
    assert!(
        view.error
            .as_deref()
            .is_some_and(|e| e.contains("no broker answered: malformed request")),
        "{view:?}"
    );
    assert!(
        summary_of(&view).text.contains("does not answer"),
        "{}",
        summary_of(&view).text
    );
}

/// Who serves the fabric's socket is said only of the one process its probe found holding
/// the listener: a probe that found no one holder (none, two, or a scan that did not read)
/// says nothing, so doctor judges no broker unsupervised by it. NEGATIVE CONTROL: take a
/// probe that found no holder for one that found nobody serving, and the live broker whose
/// listener the scan could not read is judged unsupervised.
#[test]
fn only_the_one_holder_found_says_who_serves_the_fabric() {
    let found = BrokerView {
        endpoint: "/r/bus.sock".into(),
        pid: Some(4242),
        ..BrokerView::default()
    };
    assert_eq!(summary_of(&found).serving, Some(Some(4242)));
    let unfound = BrokerView {
        pid: None,
        ..found.clone()
    };
    assert_eq!(summary_of(&unfound).serving, None);
    let reachable = BrokerView {
        reachable: true,
        attached: true,
        head: Some(7),
        ..unfound
    };
    assert_eq!(summary_of(&reachable).serving, None, "an answering broker");
}

#[test]
fn a_withheld_fabric_row_says_why_and_probes_nothing() {
    let scratch = crate::test_scratch_dir("withheld");
    assert!(
        scratch.path().to_string_lossy().ends_with(".noindex"),
        "scratch dir must exclude Spotlight: {}",
        scratch.path().display()
    );
    assert!(
        crate::test_scratch_dir("éééééé")
            .path()
            .to_string_lossy()
            .ends_with(".noindex"),
        "UTF-8 tag truncation must split on a char boundary"
    );
    let absent = scratch.path().join("absent");
    let bridge = words(&format!(
        "aterm link serve --fleet lab --broker {b} --cap-file /c --state {s}",
        b = absent.join("b.sock").display(),
        s = absent.join("state").display()
    ));
    let unread = |_: &Config| -> BrokerView { panic!("the broker was probed while withheld") };
    let why = Some("the pool is full");
    let summary = summary_for(Ok(Some(bridge.clone())), why, unread);
    assert_eq!(summary.level, SummaryLevel::Note);
    assert!(summary.unmeasured);
    assert_eq!(summary.text, "not read (the pool is full)");
    assert!(summary_for(Ok(None), why, unread).text.starts_with("off: "));
    assert_eq!(
        summary_for(Err("bad toml".into()), why, unread).level,
        SummaryLevel::Warn
    );
    assert_eq!(
        summary_for(Ok(Some(words("/usr/bin/false"))), why, unread).level,
        SummaryLevel::Warn
    );

    let read = std::cell::Cell::new(false);
    let probed = summary_for(Ok(Some(bridge)), None, |cfg| {
        read.set(true);
        reach_broker(cfg).0
    });
    assert!(read.get(), "CONTROL: nothing withheld probes the broker");
    assert!(probed.text.contains("does not answer"), "{}", probed.text);
}

#[cfg(unix)]
#[test]
fn a_broker_that_does_not_answer_is_shown_with_its_supervisors_last_line() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-err")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let cfg = unix_config(&dir.path().join("bus.sock"));
    let (view, _) = reach_broker(&cfg);
    assert_eq!(view.last_word, None, "nothing was written");
    std::fs::write(
        dir.path().join("broker.err"),
        "aterm link broker: open bus.log: earlier\n\
         aterm link broker: open bus.log: record format v9: refused\n",
    )
    .expect("broker.err");
    let (view, _) = reach_broker(&cfg);
    assert!(!view.reachable);
    let mut r = healthy();
    r.broker = view;
    let w = warned_items(&r);
    let warning = w
        .iter()
        .find(|l| l.text.contains("does not answer (connect:"))
        .unwrap_or_else(|| panic!("no unreachable warning: {w:#?}"));
    assert!(
        warning
            .text
            .contains("; its supervisor's broker.err, written ")
            && warning
                .text
                .ends_with("ago: aterm link broker: open bus.log: record format v9: refused"),
        "{}",
        warning.text
    );
    assert!(
        crate::enable::fix_for(&warning.kind, &crate::enable::Role::default())
            .contains("fix that first"),
        "{}",
        crate::enable::fix_for(&warning.kind, &crate::enable::Role::default())
    );
}

#[cfg(unix)]
#[test]
fn a_poisoned_brokers_exit_line_gets_its_remedy_at_real_lengths() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-poison")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let role = crate::enable::Role::default();
    let fix = |w: &Warning| crate::enable::fix_for(&w.kind, &role);
    let root = crate::enable::default_root(Path::new("/Users//someone"));
    let log = root.join("bus.log").to_string_lossy().into_owned();
    let argv = format!(
        "{0}/bin/aterm link broker {1}/bus.sock {log} --secret-file {1}/mint.secret \
         --durability relaxed",
        "/Applications/aterm.app/Contents/MacOS",
        root.display()
    );
    let real = crate::broker_health::astream_poisoned_reason();
    let long = format!("{real}; {}", "and more of astream's words ".repeat(40));
    let warnings = |last: &str| {
        std::fs::write(dir.path().join("broker.err"), format!("{last}\n")).expect("err");
        let mut r = healthy();
        r.broker.reachable = false;
        r.broker.error = Some("connect: No such file or directory (os error 2)".into());
        r.broker.last_word = crate::enable::supervisor_last_word(dir.path());
        let inst = &mut r.instances[0];
        inst.fabric = Some("stalled".to_string());
        inst.reason = Some("no-socket".to_string());
        let w = warned_items(&r);
        let pick = |needle: &str| {
            w.iter()
                .find(|l| l.text.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?} warning: {w:#?}"))
                .clone()
        };
        [
            pick("does not answer (connect:"),
            pick("broker link is down"),
        ]
    };
    for reason in [real.as_str(), long.as_str()] {
        let supervised = crate::broker_health::poisoned_line(&log, reason, None);
        assert!(
            supervised.chars().count() > 256,
            "PRECONDITION: {supervised}"
        );
        for w in warnings(&supervised) {
            assert!(w.text.contains("supervisor's broker.err"), "{}", w.text);
            let f = fix(&w);
            assert!(
                f.starts_with("the broker exited over a poisoned log for its supervisor")
                    && !f.contains("fix that first")
                    && !f.contains("--repair"),
                "{f}\nfor: {}",
                w.text
            );
        }
        let alone = crate::broker_health::poisoned_line(&log, reason, Some(&argv));
        for w in warnings(&alone) {
            let f = fix(&w);
            assert!(
                f.starts_with("the broker exited over a poisoned log, and no job `on`")
                    && f.contains("broker.err")
                    && !f.contains("fix that first"),
                "{f}\nfor: {}",
                w.text
            );
        }
        let other = format!("aterm link broker: open {log}: {reason}");
        for w in warnings(&other) {
            assert!(fix(&w).contains("fix that first"), "{}", fix(&w));
        }
    }
}

/// A sealed log's refusal is the last line of broker.err at its real length, a home folder's
/// path in it twice: the quote cuts at 256 characters, and the fix still reads it, whichever of
/// the two refusals it is, and says every way out itself, since the quote may have cut them.
#[test]
fn a_sealed_logs_refusal_gets_its_remedy_at_real_lengths() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-sealed")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path().join("Users/someone/.local/share/aterm-fabric");
    std::fs::create_dir_all(&root).expect("root");
    let log = root.join("bus.log");
    std::fs::write(&log, b"sealed").expect("log");
    std::fs::write(root.join("bus.log.seal"), b"binding").expect("seal");
    assert!(crate::at_rest::is_sealed(&log));
    let role = crate::enable::Role::default();
    let fix = |w: &Warning| crate::enable::fix_for(&w.kind, &role);
    let warnings = |last: &str| {
        std::fs::write(dir.path().join("broker.err"), format!("{last}\n")).expect("err");
        let mut r = healthy();
        r.broker.reachable = false;
        r.broker.error = Some("connect: No such file or directory (os error 2)".into());
        r.broker.last_word = crate::enable::supervisor_last_word(dir.path());
        let inst = &mut r.instances[0];
        inst.fabric = Some("stalled".to_string());
        inst.reason = Some("no-socket".to_string());
        let w = warned_items(&r);
        let pick = |needle: &str| {
            w.iter()
                .find(|l| l.text.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?} warning: {w:#?}"))
                .clone()
        };
        [
            pick("does not answer (connect:"),
            pick("broker link is down"),
        ]
    };
    let line = |remedy: String| format!("aterm link broker: {remedy}");
    let refused = crate::at_rest::open_remedy(&log, std::io::ErrorKind::InvalidData)
        .expect("the open's remedy");
    let missing = crate::at_rest::refusal(&log, "no such file").remedy;
    for remedy in [refused, missing] {
        let whole = line(remedy);
        assert!(whole.chars().count() > 256, "PRECONDITION: {whole}");
        for w in warnings(&whole) {
            assert!(w.text.contains("supervisor's broker.err"), "{}", w.text);
            let f = fix(&w);
            assert!(
                f.starts_with("the broker refuses its log, sealed at rest")
                    && !f.contains("fix that first")
                    && f.contains(crate::at_rest::REST_KEY)
                    && f.contains("--repair <log>")
                    && f.contains("dropping the records from there on")
                    && f.contains(crate::at_rest::RETIRE_VERB),
                "{f}\nfor: {}",
                w.text
            );
        }
    }
    std::fs::write(root.join("bus.log.retiring"), b"retired-1").expect("marker");
    let stopped = crate::at_rest::unfinished_retire(&log).expect("stopped part way");
    for w in warnings(&line(stopped.remedy)) {
        let f = fix(&w);
        assert!(
            f.starts_with("a `--retire-log` of the broker's log stopped part way")
                && f.contains(crate::at_rest::RETIRE_VERB),
            "{f}\nfor: {}",
            w.text
        );
    }
}

/// `aterm fabric` has an `at-rest` row and `--json` an `at_rest` value: of the log the serving
/// process names, and with none serving of the root's own, since a refused broker is when a
/// person asks about the seal. With neither there is no row, rather than a guess.
#[test]
fn the_at_rest_row_reads_the_served_log_or_with_none_serving_the_roots() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-rest")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let log = dir.path().join("bus.log");
    let seal = dir.path().join("bus.log.seal");
    let cfg = unix_config(&dir.path().join("bus.sock"));
    let row = |r: &Report| {
        render_text(r)
            .lines()
            .find(|l| l.trim_start().starts_with("at-rest"))
            .map(|l| l.split_whitespace().last().unwrap_or("").to_string())
    };
    let down = || {
        let (view, conn) = probe_broker(&cfg);
        assert!(conn.is_none() && !view.reachable, "nothing serves here");
        let mut r = healthy();
        r.broker = view;
        r
    };

    let none = down();
    assert_eq!(row(&none), None, "no log: no row");
    assert_eq!(none.broker.at_rest(), None);

    std::fs::write(&log, b"records").expect("log");
    let plain = down();
    assert_eq!(row(&plain).as_deref(), Some("plaintext"));
    assert!(
        render_json(&plain).contains("\"at_rest\":\"plaintext\""),
        "{}",
        render_json(&plain)
    );

    std::fs::write(&seal, b"binding").expect("seal");
    let sealed = down();
    assert_eq!(row(&sealed).as_deref(), Some("sealed"));
    assert!(render_json(&sealed).contains("\"at_rest\":\"sealed\""));

    let mut served = healthy();
    served.broker.root_log = Some(log.clone());
    served.broker.log = Some(dir.path().join("elsewhere.log"));
    assert_eq!(
        row(&served).as_deref(),
        Some("plaintext"),
        "a serving process's log is the one that counts"
    );
}

#[test]
fn the_report_says_what_the_broker_says_of_its_log() {
    use astream_broker::{FlushCounts, HelloHealth};
    let role = crate::enable::Role::default();
    let fix = |w: &Warning| crate::enable::fix_for(&w.kind, &role);
    let ours = |w: &[Warning]| -> Vec<Warning> {
        w.iter()
            .filter(|l| l.text.contains(POISONED_WARNING) || l.text.contains(FLUSH_FAILED_WARNING))
            .cloned()
            .collect()
    };
    let row = |r: &Report, key: &str| -> Option<String> {
        render_text(r)
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{key} ")))
            .map(|l| l.trim().to_string())
    };
    let mut r = healthy();
    r.broker.health = Some(HelloHealth {
        poisoned: None,
        flushes: FlushCounts {
            full: 3,
            barrier: 9,
            ..FlushCounts::default()
        },
    });
    assert!(ours(&warned_items(&r)).is_empty(), "{:?}", warned_items(&r));
    let health = row(&r, "health").expect("a health row");
    assert!(
        health.ends_with(
            "ok, not poisoned — flushes since it opened: 3 full, 9 barrier, 0 skipped, 0 \
             failed"
        ),
        "{health}"
    );
    assert_eq!(summary_of(&r.broker).level, SummaryLevel::Ok);
    assert!(
        render_json(&r).contains(
            "\"in_band\":{\"poisoned\":null,\"flushes\":{\"full\":3,\"barrier\":9,\
             \"skipped\":0,\"failed\":0}}"
        ),
        "{}",
        render_json(&r)
    );

    if let Some(h) = r.broker.health.as_mut() {
        h.flushes.failed = 2;
    }
    let w = ours(&warned_items(&r));
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].text
            .contains(&format!("{FLUSH_FAILED_WARNING} 2 time(s)")),
        "{}",
        w[0].text
    );
    assert!(fix(&w[0]).starts_with("check the disk"), "{}", fix(&w[0]));
    for said in [w[0].text.as_str(), fix(&w[0]).as_str()] {
        assert!(
            !said.contains("lost nothing") && !said.contains("nothing acked was lost"),
            "{said}"
        );
    }
    assert_eq!(summary_of(&r.broker).level, SummaryLevel::Ok);

    if let Some(h) = r.broker.health.as_mut() {
        h.poisoned = Some("a flush failed (EIO)\u{1b}[2J".to_string());
    }
    let w = ours(&warned_items(&r));
    assert_eq!(w.len(), 1, "the poison, not the disk too: {w:?}");
    assert!(
        w[0].text
            .contains(&format!("{POISONED_WARNING} (a flush failed (EIO)")),
        "{}",
        w[0].text
    );
    assert!(
        !w[0].text.contains('\u{1b}'),
        "astream's words are made safe: {}",
        w[0].text
    );
    let f = fix(&w[0]);
    assert!(
        f.starts_with("the broker's log is poisoned")
            && !f.contains("node.cap")
            && !f.contains("--repair"),
        "{f}"
    );
    let health = row(&r, "health").expect("a health row");
    assert!(
        health.contains("POISONED (a flush failed (EIO)"),
        "{health}"
    );
    let summary = summary_of(&r.broker);
    assert_eq!(summary.level, SummaryLevel::Warn);
    assert!(summary.text.contains(POISONED_WARNING), "{}", summary.text);
    assert!(!summary.unmeasured, "a poisoned log is a measured failure");
    assert!(
        render_json(&r).contains("\"in_band\":{\"poisoned\":\"a flush failed (EIO)"),
        "{}",
        render_json(&r)
    );

    r.broker.health = None;
    assert!(ours(&warned_items(&r)).is_empty());
    let health = row(&r, "health").expect("a health row");
    assert!(health.contains("carries no health entry"), "{health}");
    assert_eq!(summary_of(&r.broker).level, SummaryLevel::Note);
    assert!(summary_of(&r.broker).unmeasured);

    r.broker.reachable = false;
    r.broker.error = Some("connect: refused".to_string());
    assert_eq!(row(&r, "health"), None);
    let summary = summary_of(&r.broker);
    assert_eq!(summary.level, SummaryLevel::Warn);
    assert!(
        summary.text.contains("does not answer (connect: refused)"),
        "{}",
        summary.text
    );
}

/// The report says a holder the scan could not name is unknown, and why, where one nobody
/// holds reads as no broker process; the JSON's `pid` is null either way.
#[test]
fn the_report_says_an_unknown_holder_is_unknown() {
    let mut r = healthy();
    r.broker.pid = None;
    r.broker.launchd = None;
    let pid_row = |r: &Report| {
        render_text(r)
            .lines()
            .find(|line| line.trim_start().starts_with("pid"))
            .map(str::to_string)
            .expect("a pid row")
    };
    let none = pid_row(&r);
    assert!(
        none.contains("- (no `link broker` process serves this socket)"),
        "{none}"
    );
    r.broker.pid_unknown = Some("2 processes hold its listener (71, 72)".to_string());
    let unknown = pid_row(&r);
    assert!(
        unknown.contains("- (unknown: 2 processes hold its listener (71, 72))"),
        "{unknown}"
    );
    assert!(
        render_json(&r).contains("\"pid\":null,\"launchd\":null"),
        "{}",
        render_json(&r)
    );
}

/// Where the node service serves the socket, the report names it: the text's `host` row (its
/// job and pid, the log it hosts, the tier its log was opened at, and its build where it is
/// this aterm's, else its program, REPLACED where it is), doctor's summary, `--json`'s
/// `broker.host`, `program` and `build`, and a poisoned log's warning, which says the node
/// service reopens it. With the mesh on and nothing serving the socket, the pid row and the
/// summary say the node service hosts bus.log there. NEGATIVE CONTROL: a broker's report has
/// no host row, says "the broker" and its supervisor, and its JSON host is `link-broker` with
/// a null build; with the mesh off a socket nobody holds reads as it did.
#[test]
fn the_report_names_the_node_service_where_it_hosts_bus_log() {
    use astream_broker::{FlushCounts, HelloHealth, ServedInfo, ServedReason, Tier};
    let row = |r: &Report, key: &str| -> Option<String> {
        render_text(r)
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{key} ")))
            .map(|l| l.trim().to_string())
    };
    let program = "/Applications/aterm.app/Contents/MacOS/aterm";
    let mut r = healthy();
    r.broker.endpoint = "/r/bus.sock".to_string();
    r.broker.pid = Some(71);
    r.broker.launchd = Some(crate::enable::NODE_JOB.to_string());
    r.broker.mesh = true;
    r.broker.host = Some(Host::NodeService);
    r.broker.log = Some(PathBuf::from("/r/bus.log"));
    r.broker.program = Some(program.to_string());
    r.broker.own_build = true;
    r.broker.served = Some(ServedInfo {
        requested: Tier::Ordered,
        served: Tier::Ordered,
        reason: ServedReason::None,
        horizon_ms: None,
        rules: 0,
    });
    r.broker.health = Some(HelloHealth {
        poisoned: None,
        flushes: FlushCounts::default(),
    });
    let id = aterm_stamp::build_id();
    assert_eq!(
        row(&r, "host")
            .as_deref()
            .map(|l| l["host".len()..].trim_start()),
        Some(
            format!(
                "the node service (systems.alab.aterm-node, pid 71) hosts /r/bus.log at \
                 ordered, build {id} (this aterm's)"
            )
            .as_str()
        )
    );
    assert_eq!(
        host_row(&r.broker),
        row(&r, "host").map(|l| l["host".len()..].trim_start().to_string())
    );
    let json = render_json(&r);
    assert!(
        json.contains(&format!(
            "\"host\":\"node-service\",\"program\":\"{program}\",\"build\":\"{id}\""
        )),
        "{json}"
    );
    let summary = summary_of(&r.broker);
    assert!(
        summary
            .text
            .starts_with("the node service at /r/bus.sock answers, serves ordered"),
        "{}",
        summary.text
    );
    assert!(
        row(&r, "holds").is_some_and(|h| h.contains("the node service keeps one there")),
        "{:?}",
        row(&r, "holds")
    );
    r.broker.own_build = false;
    r.broker.replaced = Some(program.to_string());
    assert!(
        row(&r, "host").is_some_and(|h| h.ends_with(&format!(
            "at ordered, program {program} — A REPLACED BUILD: that path is another file now"
        ))),
        "{:?}",
        row(&r, "host")
    );
    assert!(render_json(&r).contains("\"build\":null"));
    let replaced: Vec<Warning> = warned_items(&r)
        .into_iter()
        .filter(|w| w.kind == WarningKind::BrokerReplacedBuild)
        .collect();
    assert!(
        replaced.len() == 1
            && replaced[0]
                .text
                .starts_with("the node service hosting bus.log (pid 71)"),
        "{replaced:?}"
    );
    r.broker.replaced = None;
    if let Some(h) = r.broker.health.as_mut() {
        h.poisoned = Some("a flush failed".to_string());
    }
    let poisoned = |r: &Report| {
        warned_items(r)
            .into_iter()
            .find(|w| w.kind == WarningKind::BrokerLogPoisoned)
            .map(|w| w.text)
            .expect("a poisoned log's warning")
    };
    let node = poisoned(&r);
    assert!(
        node.ends_with("the node service reopens it at its next pass, within seconds")
            && !node.contains("supervisor"),
        "{node}"
    );

    let mut broker = r.clone();
    broker.broker.host = Some(Host::LinkBroker);
    broker.broker.launchd = Some(crate::enable::LABEL.to_string());
    broker.broker.durability = Some(crate::durability::Durability::Ordered);
    assert!(
        poisoned(&broker).contains("for its supervisor to reopen it"),
        "NEGATIVE CONTROL: a broker job's poison exit"
    );
    broker.broker.health = None;
    assert_eq!(row(&broker, "host"), None, "a broker has no host row");
    assert!(render_json(&broker).contains("\"host\":\"link-broker\""));
    assert!(summary_of(&broker.broker)
        .text
        .starts_with("the broker at /r/bus.sock answers"));

    let pid_row = |r: &Report| row(r, "pid").expect("a pid row");
    r.broker = BrokerView {
        endpoint: "/r/bus.sock".to_string(),
        mesh: true,
        error: Some("connect: refused".to_string()),
        ..BrokerView::default()
    };
    assert!(
        pid_row(&r).ends_with(
            "- (no process serves this socket: the node service (systems.alab.aterm-node) \
             hosts bus.log here, and `aterm node status` says whether it runs)"
        ),
        "{}",
        pid_row(&r)
    );
    assert!(
        summary_of(&r.broker)
            .text
            .starts_with("the node service's socket /r/bus.sock does not answer"),
        "{}",
        summary_of(&r.broker).text
    );
    r.broker.mesh = false;
    assert!(
        pid_row(&r).ends_with("- (no `link broker` process serves this socket)"),
        "NEGATIVE CONTROL: {}",
        pid_row(&r)
    );
    assert!(summary_of(&r.broker)
        .text
        .starts_with("the broker at /r/bus.sock does not answer"));
}

#[test]
fn the_holds_row_is_current_only_for_the_serving_pid() {
    use astream_broker::{BrokerHealth, HealthRead, HealthWriter};
    let mut r = healthy();
    let row = |r: &Report| holds_row(&r.broker, 61_000);
    assert_eq!(row(&r), None, "no log named, no row");
    r.broker.log = Some(PathBuf::from("/r/bus.log"));
    r.broker.pid = Some(42);
    assert_eq!(
        row(&r).as_deref(),
        Some(
            "- (no health snapshot beside /r/bus.log: a broker older than this aterm keeps \
             none, and one of this build that could not write it says why on its stderr, the \
             supervisor's broker.err)"
        )
    );
    r.broker.snapshot = Some(Err("/r/bus.log.health: not a snapshot".to_string()));
    assert!(row(&r)
        .expect("a row")
        .contains("could not be read: /r/bus.log.health"));
    let mut h = BrokerHealth::default();
    h.pid = 42;
    h.written_at_ms = 1_000;
    h.retained_records = 5;
    h.retained_bytes = 2048;
    h.distinct_subjects = 3;
    h.active_conns = 4;
    h.process_flushes = Some(astream_sys::FlushTally {
        full: 7,
        barrier: 2,
        plain: 1,
    });
    h.resident = Some(astream_broker::Residency {
        records: 2,
        bytes: 400,
    });
    h.process_flush_times = Some(astream_sys::FlushTimes {
        clock: astream_sys::FlushClock::Machine,
        full_ns: 9_000_000,
        barrier_ns: 300_000,
        plain_ns: 0,
        slow_count: 1,
        slow: vec![astream_sys::SlowFlush {
            index: 0,
            kind: astream_sys::FlushKind::Full,
            start_ns: 70,
            end_ns: 4_000_070,
        }],
        running: 0,
    });
    let read = |writer, h: &BrokerHealth| HealthRead {
        health: h.clone(),
        writer,
    };
    r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &h)));
    assert_eq!(
        row(&r).as_deref(),
        Some(
            "5 record(s) in 2.0 KiB (resident=2/400 B of 5), 3 subject(s), 0 pending \
             will(s), 0 group commit(s); 4 connection(s), 0 parked tail(s); 7 full, 2 \
             barrier and 1 plain flush(es) by its process since it started; its flushes \
             took 9.0 ms full, 0.3 ms barrier and 0.0 ms plain; 1 slower than 1 ms, 1 \
             kept, the slowest a 4.0 ms full (astream's <log>.health, written 1m ago)"
        )
    );
    // `--json` carries every fact the row shows, the process's tally, its flush times and
    // the residency among them; an older broker's snapshot, which carries none of them,
    // reads null there and says so in the row.
    assert!(
        render_json(&r).contains(r#""process_flushes":{"full":7,"barrier":2,"plain":1}"#),
        "{}",
        render_json(&r)
    );
    assert!(
        render_json(&r).contains(
            r#""process_flush_times":{"clock":"machine","full_ns":9000000,"barrier_ns":300000,"plain_ns":0,"slow_count":1,"slow":[{"index":0,"kind":"full","start_ns":70,"end_ns":4000070}],"running":0}"#
        ),
        "{}",
        render_json(&r)
    );
    assert!(
        render_json(&r).contains(r#""resident":{"records":2,"bytes":400}"#),
        "{}",
        render_json(&r)
    );
    let mut older = h.clone();
    older.process_flushes = None;
    older.resident = None;
    older.process_flush_times = None;
    r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &older)));
    assert!(
        render_json(&r).contains(r#""process_flushes":null"#),
        "{}",
        render_json(&r)
    );
    assert!(
        render_json(&r).contains(r#""process_flush_times":null"#),
        "{}",
        render_json(&r)
    );
    assert!(
        render_json(&r).contains(r#""resident":null"#),
        "{}",
        render_json(&r)
    );
    assert!(
        row(&r)
            .expect("a row")
            .contains(crate::broker_health::NO_RESIDENCY),
        "{:?}",
        row(&r)
    );
    assert!(
        row(&r)
            .expect("a row")
            .contains(crate::broker_health::NO_PROCESS_FLUSHES),
        "{:?}",
        row(&r)
    );
    r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &h)));
    assert!(r.broker.snapshot_now().is_some());
    r.broker.pid = Some(43);
    assert!(row(&r)
        .expect("a row")
        .starts_with("- (astream's <log>.health is pid 42's"));
    assert!(r.broker.snapshot_now().is_none());
    r.broker.pid = Some(42);
    h.poisoned = Some("a flush failed".to_string());
    r.broker.snapshot = Some(Ok(read(HealthWriter::Gone, &h)));
    let gone = row(&r).expect("a row");
    assert!(
        gone.starts_with("- (astream's <log>.health is the last word of pid 42")
            && gone.contains("that broker's log was POISONED (a flush failed)"),
        "{gone}"
    );
    assert!(r.broker.snapshot_now().is_none());
    assert_eq!(
        r.broker.poisoned(),
        None,
        "a gone broker's poison is not this one's"
    );
    assert!(
        !warned(&r).iter().any(|w| w.contains(POISONED_WARNING)),
        "{:?}",
        warned(&r)
    );
    r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &h)));
    assert_eq!(r.broker.poisoned(), Some("a flush failed"));
    assert!(render_json(&r).contains("\"writer\":\"serving\",\"current\":true"));
}

#[cfg(unix)]
#[test]
fn a_bridge_dial_nothing_answers_quotes_the_brokers_last_word() {
    use std::io::ErrorKind as K;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dial")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let cfg = unix_config(&dir.path().join("bus.sock"));
    let role = crate::enable::Role::default();
    let report = |kind: K, reason: &str, command: Option<String>| {
        let line = crate::cli::log_refusal_remedy(kind, "/r/bus.log").expect("a remedy");
        std::fs::write(
            dir.path().join("broker.err"),
            format!("aterm link broker: open /r/bus.log: x — {line}\n"),
        )
        .expect("broker.err");
        let (view, _) = reach_broker(&cfg);
        assert!(!view.reachable && view.last_word.is_some(), "{view:?}");
        let mut r = healthy();
        r.broker = view;
        let inst = &mut r.instances[0];
        inst.fabric = Some("stalled".to_string());
        inst.reason = Some(reason.to_string());
        if let Some(c) = command {
            inst.command = Some(words(&c));
        }
        let w = warned_items(&r);
        let bridge = w
            .iter()
            .find(|l| l.text.contains("broker link is down"))
            .unwrap_or_else(|| panic!("no bridge warning: {w:#?}"))
            .clone();
        let broker = w
            .iter()
            .find(|l| l.text.contains("does not answer (connect:"))
            .unwrap_or_else(|| panic!("no broker warning: {w:#?}"))
            .clone();
        (bridge, broker)
    };
    let fix = |w: &Warning| crate::enable::fix_for(&w.kind, &role);
    for reason in ["no-socket", "refused"] {
        let (bridge, broker) = report(K::InvalidData, reason, None);
        assert!(
            bridge
                .text
                .contains("; its supervisor's broker.err, written ")
                && bridge.text.contains(crate::cli::CORRUPT_LOG_REMEDY),
            "{}",
            bridge.text
        );
        assert!(fix(&bridge).starts_with(&fix(&broker)), "{}", fix(&bridge));
        assert!(fix(&bridge).contains("--repair"), "{}", fix(&bridge));
        let (bridge, broker) = report(K::Unsupported, reason, None);
        assert!(
            bridge.text.contains(crate::cli::NEWER_LOG_REMEDY),
            "{}",
            bridge.text
        );
        assert!(
            fix(&bridge).starts_with(&fix(&broker)) && !fix(&bridge).contains("--repair"),
            "{}",
            fix(&bridge)
        );
    }
    let other = CMD.replace("/tmp/b.sock", "/elsewhere/b.sock");
    assert_ne!(other, CMD, "PRECONDITION: the command names /tmp/b.sock");
    let (bridge, _) = report(K::InvalidData, "refused", Some(other));
    assert!(!bridge.text.contains("broker.err"), "{}", bridge.text);
    assert!(fix(&bridge).contains("starts it"), "{}", fix(&bridge));
    let (bridge, _) = report(K::InvalidData, "no-ack", None);
    assert!(!bridge.text.contains("broker.err"), "{}", bridge.text);
}

#[cfg(unix)]
#[test]
fn a_lapsed_ring_is_named_and_its_fix_is_a_renewal() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-lapsed")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let secret = vec![0x5a_u8; 32];
    let broker = astream_broker::Broker::open_guarded(dir.path().join("b.log"), secret.clone())
        .expect("a guarded broker");
    let socket = dir.path().join("b.sock");
    let _server = broker.serve(&socket).expect("serve the broker");
    let cap = dir.path().join("node.cap");
    let mut cfg = unix_config(&socket);
    cfg.cap_files = vec![cap.to_string_lossy().into_owned()];
    let probe = |key: &[u8], exp: &str| {
        let lines: String = crate::enable::node_rules("f", "n-a")
            .iter()
            .map(|g| {
                let c = astream_cap::mint(key, &g.replacen(':', exp, 1)).expect("mint");
                format!("{}\n", astream_cap::capfile::format_line(&c.filter, &c.tag))
            })
            .collect();
        crate::bridge::write_owner_only(&cap, lines);
        let (view, _) = reach_broker(&cfg);
        let mut r = healthy();
        r.broker = view;
        r
    };
    let r = probe(&secret, ",exp=1:");
    assert!(
        r.broker.reachable && r.broker.cap_expired && !r.broker.read_ok(),
        "{:?}",
        r.broker
    );
    let w = warned_items(&r);
    let warning = w
        .iter()
        .find(|l| l.text.contains(CAP_LAPSED))
        .unwrap_or_else(|| panic!("no lapsed warning: {w:#?}"));
    assert!(
        !warning.text.contains("could not be read"),
        "{}",
        warning.text
    );
    let here = crate::enable::fix_for(&warning.kind, &crate::enable::Role::default());
    assert!(
        here.contains("lapsed") && here.contains("move <root>/node.cap aside"),
        "{here}"
    );

    let r = probe(&[0xa5_u8; 32], ":");
    assert!(
        r.broker.reachable && !r.broker.cap_expired,
        "{:?}",
        r.broker
    );
    let w = warned(&r);
    assert!(
        w.iter().any(|l| l.contains("could not be read"))
            && !w.iter().any(|l| l.contains(CAP_LAPSED)),
        "{w:#?}"
    );
    let r = probe(&secret, ":");
    assert!(
        r.broker.read_ok() && !r.broker.cap_expired,
        "{:?}",
        r.broker
    );
}

#[test]
fn nodes_lists_every_node_and_marks_this_one() {
    let mut r = healthy();
    let body = b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=-";
    let roster = crate::glance::Glance {
        fleet: "lab".to_string(),
        rows: vec![
            crate::glance::Row::parse(
                "lab",
                1,
                "/f/lab/pub/n-z/node/presence",
                b"v=1 t=1 inc=1 state=live fabric=connected host=m7",
            )
            .expect("row"),
            crate::glance::Row::parse(
                "lab",
                2,
                "/f/lab/pub/n-a/node/presence",
                b"v=1 t=1 inc=1 state=live fabric=connected host=m100",
            )
            .expect("row"),
            crate::glance::Row::parse("lab", 3, "/f/lab/pub/n-z/s-far/presence", body)
                .expect("row"),
            crate::glance::Row::parse("lab", 4, "/f/lab/pub/n-a/s-one/presence", body)
                .expect("row"),
        ],
        truncated: false,
    };
    join_sessions(&mut r, Some(&roster));
    let json = render_json(&r);
    assert!(
        json.contains("\"this\":true") && json.contains("\"where\":\"remote\""),
        "{json}"
    );
    let mut lonely = healthy();
    lonely.nodes = vec![NodeRow {
        node: "n-z".to_string(),
        host: "m7".to_string(),
        state: "live".to_string(),
        fabric: "connected".to_string(),
        ..NodeRow::default()
    }];
    assert_eq!(node_where(&lonely, "n-a"), "this");
    assert_eq!(node_where(&lonely, "n-z"), "remote");
    let text = render_text(&lonely);
    assert_eq!(
        text.matches(REMOTE_NODE_NOTE).count(),
        1,
        "said once, under NODES:\n{text}"
    );
    lonely.instances[0].node = Some("n-z".to_string());
    assert_eq!(node_where(&lonely, "n-z"), "local");
    assert_eq!(node_where(&lonely, "n-q"), "remote");
    let text = render_text(&lonely);
    assert!(!text.contains(REMOTE_NODE_NOTE), "no remote row:\n{text}");
    assert!(text.contains("n-z"), "{text}");
}

#[test]
fn the_readable_faces_are_the_caps_filters_under_the_fleet() {
    let scratch = crate::test_scratch_dir("faces");
    let dir = scratch.path();
    let cap = dir.join("node.cap");
    let lines: String = crate::enable::node_rules("lab", "n-a")
        .iter()
        .chain(std::iter::once(&"ro:/f/other/pub/>".to_string()))
        .map(|g| format!("{g} {}\n", "00".repeat(32)))
        .collect();
    crate::bridge::write_owner_only(&cap, lines);
    let faces = readable_faces("lab", &[cap.to_string_lossy().into_owned()]);
    assert_eq!(
        faces,
        [
            "/f/lab/fleet/>",
            "/f/lab/halt/>",
            "/f/lab/halt/n-a",
            "/f/lab/in/*/*/n-a/*",
            "/f/lab/in/n-a/>",
            "/f/lab/pub/>",
            "/f/lab/pub/n-a/>",
        ],
        "no cur/ group names and no other fleet's face"
    );
    assert_eq!(
        in_filters("lab", Some(&faces)),
        ["/f/lab/in/*/*/n-a/*", "/f/lab/in/n-a/>"]
    );
    assert_eq!(in_filters("lab", None), ["/f/lab/in/>"]);
}

#[test]
fn a_process_whose_executable_was_replaced_is_named() {
    let mut kept = aterm_uds::deadline::KeptUntilRan::new(crate::test_scratch_dir("exe").keep());
    let root = kept.path().to_path_buf();
    let dir = root.join("a b");
    std::fs::create_dir_all(&dir).expect("scratch");
    let exe = dir.join("slp");
    let copy = |to: &Path| aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), to);
    copy(&exe);
    let run = |path: &Path| {
        aterm_uds::spawnfd::command(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("run {}: {e}", path.display()))
    };
    let mut child = run(&exe);
    let link = root.join("slp-link");
    std::os::unix::fs::symlink(&exe, &link).expect("link it");
    let mut linked = run(&link);
    let fed = [
        aterm_uds::deadline::cat_echoes(&mut child),
        aterm_uds::deadline::cat_echoes(&mut linked),
    ];
    kept.ran();
    let shown = exe.to_string_lossy().into_owned();
    let via = link.to_string_lossy().into_owned();
    let before = replaced_build(child.id(), &shown);
    let before_via = replaced_build(linked.id(), &via);
    let own = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(&link).expect("the link").ino()
    };
    let running_via = running_image_inode(linked.id());
    copy(&dir.join("slp.new"));
    std::fs::rename(dir.join("slp.new"), &exe).expect("rename a new build over it");
    let after = replaced_build(child.id(), &shown);
    let after_via = replaced_build(linked.id(), &via);
    let relative = replaced_build(child.id(), "slp");
    #[cfg(target_os = "macos")]
    let read =
        [child.id(), linked.id()].map(|pid| (running_image_inode(pid), lsof_image_inode(pid)));
    for mut c in [child, linked] {
        let _ = c.kill();
        let _ = c.wait();
    }
    drop(fed);
    assert_eq!(before, None, "it runs the file at its path");
    assert_eq!(
        after.as_deref(),
        Some(shown.as_str()),
        "a new file is at its path"
    );
    assert_eq!(before_via, None, "it runs the file its link names");
    assert_eq!(
        after_via.as_deref(),
        Some(via.as_str()),
        "a new file is behind its link"
    );
    assert!(
        running_via.is_some_and(|i| i != own),
        "{running_via:?} vs the link's {own}"
    );
    assert_eq!(relative, None, "a relative argv names no file");
    #[cfg(target_os = "macos")]
    for (ours, lsof) in read {
        assert!(ours.is_some(), "an image read");
        assert_eq!(ours, lsof, "the image lsof names");
    }
    let lsof = "p57558\nftxt\ni366658160\nn/x/slp\nftxt\ni1152921500312609216\nn/usr/lib/dyld\n";
    assert_eq!(lsof_first_inode(lsof), Some(366_658_160));
    assert_eq!(lsof_first_inode(""), None);
}

#[test]
fn a_head_query_that_failed_is_not_a_broker_that_answered() {
    let mut r = healthy();
    r.broker.head = None;
    r.broker.rtt_ms = None;
    r.broker.error =
        Some("head query: unauthorized: capability does not grant this subject/filter".to_string());
    let w = warned(&r);
    assert!(
        !w.is_empty(),
        "the bus could not be read and NOTHING warns: {w:#?}"
    );
    r.warnings = w;
    assert_eq!(exit_of(&r), 1, "a bus that cannot be read is not healthy");
}

#[test]
fn a_node_the_bus_calls_away_is_a_warning_while_its_instance_says_connected() {
    let mut r = healthy();
    r.nodes = vec![NodeRow {
        node: "n-a".to_string(),
        host: "-".to_string(),
        state: "gone".to_string(),
        fabric: "disconnected".to_string(),
        ..NodeRow::default()
    }];
    one(&r, "state=away");

    let mut ok = healthy();
    ok.nodes = vec![
        NodeRow {
            node: "n-a".to_string(),
            host: "-".to_string(),
            state: "live".to_string(),
            fabric: "connected".to_string(),
            ..NodeRow::default()
        },
        NodeRow {
            node: "n-far".to_string(),
            host: "box".to_string(),
            state: "gone".to_string(),
            fabric: "disconnected".to_string(),
            ..NodeRow::default()
        },
    ];
    assert_eq!(warned(&ok), Vec::<String>::new());
}

fn one(r: &Report, needle: &str) {
    let w = warned(r);
    assert!(
        w.iter().any(|l| l.contains(needle)),
        "no warning says {needle:?}: {w:#?}"
    );
}

#[test]
fn every_condition_that_makes_connected_a_lie_or_loses_mail_is_a_warning() {
    let mut r = healthy();
    assert_eq!(warned(&r), Vec::<String>::new());
    r.warnings = warned(&r);
    assert_eq!(exit_of(&r), 0);

    let mut r = healthy();
    r.broker.reachable = false;
    r.broker.attached = false;
    r.broker.error = Some("connect: Connection refused".to_string());
    one(&r, "connect: Connection refused");
    one(&r, "instance 100 says fabric=connected");

    let mut r = healthy();
    r.instances[0].fabric = Some("stalled".to_string());
    r.instances[0].reason = Some("refused".to_string());
    one(&r, "fabric=stalled reason=refused");
    r.instances[0].link_age_ms = None;
    r.instances[0].reason = Some("no-socket".to_string());
    one(&r, "fabric=stalled reason=no-socket");

    let mut r = healthy();
    r.instances[0].build = Some("0.104.0+g0123456789ab".to_string());
    r.instances[0].bridge_build = Some("0.104.0+g0123456789ab".to_string());
    assert_eq!(warned(&r), Vec::<String>::new());
    r.instances[0].bridge_build = None;
    assert_eq!(warned(&r), Vec::<String>::new());
    r.instances[0].bridge_build = Some("0.98.0+gfedcba987654".to_string());
    one(
        &r,
        "instance 100's bridge is another build than the instance: bridge \
         0.98.0+gfedcba987654, instance 0.104.0+g0123456789ab",
    );
    let mut r = healthy();
    r.instances[0].lane_faults = Some(0);
    assert_eq!(warned(&r), Vec::<String>::new());
    r.instances[0].lane_faults = Some(3);
    one(&r, "instance 100 could not parse 3 of its bridge's lines");

    let mut r = healthy();
    r.instances[0].sessions[0].dropped = Some(2);
    one(&r, "@s-one (instance 100)");

    let mut r = healthy();
    r.instances[0].sessions[0].hold = Some(1);
    r.instances[0].sessions[0].hold_why = Some(("fabric-lost".to_string(), "fleet".to_string()));
    one(&r, "reason=fabric-lost origin=fleet");
    r.instances[0].sessions[0].hold_why = None;
    one(&r, "@s-one");

    let mut r = healthy();
    r.instances[0].sessions[0].unhandled = vec![
        MsgMeta {
            id: 4,
            off: 40,
            from: "h-andrew".to_string(),
            kind: "task".to_string(),
        },
        MsgMeta {
            id: 5,
            off: 41,
            from: "s-x@n-b".to_string(),
            kind: "note".to_string(),
        },
    ];
    let mut ages = BTreeMap::new();
    ages.insert(("s-one".to_string(), 40), r.now_ms - 11 * 60 * 1000);
    ages.insert(("s-one".to_string(), 41), r.now_ms - 60 * 60 * 1000);
    let w = warnings(&r, &ages, &[]);
    assert!(w.len() == 1 && w[0].contains("h-andrew"), "{w:#?}");
    ages.insert(("s-one".to_string(), 40), r.now_ms - 9 * 60 * 1000);
    assert!(warnings(&r, &ages, &[]).is_empty(), "a note is not work");
}

/// R5: WHAT THE BROKER'S RETENTION GAVE UP IS A WARNING, ONCE WHERE IT WAS TOLD. The fields
/// parse as aterm writes them (`inbox`'s `given_up=`, `given_up_count=`, `lane_given_up=`; a
/// `topic ls` row's `given_up=`; `fabric status`'s `mail_gaps=`, `mail_gap=`,
/// `mail_gap_count=`). The node's unread mail is one line per instance, from its counter
/// alone (a range no session was there to be told of, or one on a log since replaced), from
/// its sessions alone (after a seamless update, which carries theirs and not the counter), or
/// both, each range once and newest first, the fourth past counted; a session's own lane
/// and topics are one line for that session, never its copy of the node's range. Each gets
/// `doctor`'s retention fix. NC (run): a `node_mail_given_up` that returns `None` fails the
/// counter's line; one that takes no session's range fails the update's; a
/// `session_given_up` that returns `None` fails the session's line; one that keeps each
/// session's copy of a range apart fails "once for both".
#[test]
fn records_the_brokers_retention_gave_up_are_listed_once_where_they_were_told() {
    let mut s = LocalSession::default();
    fill_inbox(
        &mut s,
        "OK 0 hold=0 holder=- seen=3 bus_head=90 oldest_on_bus=@44 dropped=0 pending=0 \
         given_up=@10..@42 given_up_count=3 lane_given_up=@50..@60",
        &[],
        &[],
    );
    let range = |from, to, records| GivenUpRange { from, to, records };
    assert_eq!(s.given_up, Some(range(10, 42, Some(3))));
    assert_eq!(s.lane_given_up, Some(range(50, 60, None)));
    fill_topics(
        &mut s,
        &[
            "topic deploy since=@0 given_up=@70..@80".to_string(),
            "topic news since=head".to_string(),
        ],
    );
    assert_eq!(s.topics, ["deploy", "news"]);
    assert_eq!(
        s.topic_given_up,
        [("deploy".to_string(), range(70, 80, None))]
    );
    for bad in ["@5..@3", "10..42", "@a..@b", "@1..", ""] {
        assert_eq!(GivenUpRange::parse(bad, Some("2")), None, "{bad}");
    }
    assert_eq!(
        GivenUpRange::parse("@1..@2", Some("x")),
        Some(range(1, 2, None))
    );

    let node_lines = |r: &Report| -> Vec<String> {
        warned(r)
            .into_iter()
            .filter(|l| l.contains("this node's unread mail"))
            .collect()
    };
    let fix = |k: &WarningKind| crate::enable::fix_for(k, &crate::enable::Role::default());

    let mut r = healthy();
    r.instances[0].mail_gaps = Some(2);
    r.instances[0].mail_gap = Some(range(70, 75, Some(1)));
    assert_eq!(
        warned(&r),
        [
            "instance 100: the broker's retention gave up this node's unread mail before its \
          bridge read it (2 range(s) since the instance started): @70..@75 (1 record); the \
          sessions it served then were told (`inbox` given_up=, a `mail-gap` mark in each \
          log)"
        ],
        "the counter alone, no session told"
    );
    assert!(fix(&warned_items(&r)[0].kind).starts_with("nothing brings those records back"));
    r.instances[0].mail_gap = None;
    assert_eq!(
        node_lines(&r),
        [
            "instance 100: the broker's retention gave up this node's unread mail before its \
          bridge read it (2 range(s) since the instance started), its offsets on a bus log \
          since replaced; the sessions it served then were told (`inbox` given_up=, a \
          `mail-gap` mark in each log)"
        ]
    );
    r.instances[0].mail_gaps = Some(0);
    assert_eq!(warned(&r), Vec::<String>::new(), "none told");

    let mut r = healthy();
    let mut two = r.instances[0].sessions[0].clone();
    two.sid = "s-two".to_string();
    r.instances[0].sessions.push(two);
    for s in &mut r.instances[0].sessions {
        s.given_up = Some(range(10, 42, Some(3)));
    }
    assert_eq!(
        warned(&r),
        [
            "instance 100: the broker's retention gave up this node's unread mail before its \
          bridge read it: @10..@42 (3 records); the sessions it served then were told \
          (`inbox` given_up=, a `mail-gap` mark in each log)"
        ],
        "the sessions' range after an update, once for both"
    );
    r.instances[0].mail_gaps = Some(2);
    r.instances[0].mail_gap = Some(range(100, 120, None));
    r.instances[0].sessions[1].given_up = Some(range(100, 120, Some(2)));
    assert_eq!(
        node_lines(&r),
        [
            "instance 100: the broker's retention gave up this node's unread mail before its \
          bridge read it (2 range(s) since the instance started): @100..@120 (2 records), \
          @10..@42 (3 records); the sessions it served then were told (`inbox` given_up=, a \
          `mail-gap` mark in each log)"
        ],
        "the counter's and the sessions', each range once, newest first"
    );
    for (i, from) in [130, 140, 150, 160].into_iter().enumerate() {
        let mut s = r.instances[0].sessions[0].clone();
        s.sid = format!("s-more{i}");
        s.given_up = Some(range(from, from + 5, None));
        r.instances[0].sessions.push(s);
    }
    let lines = node_lines(&r);
    assert!(
        lines.len() == 1
            && lines[0].contains(": @160..@165, @150..@155, @140..@145, @130..@135 and 2 more;"),
        "{lines:#?}"
    );

    let mut r = healthy();
    let s = &mut r.instances[0].sessions[0];
    s.given_up = Some(range(10, 42, Some(3)));
    s.lane_given_up = Some(range(50, 60, None));
    s.topic_given_up = vec![
        ("deploy".to_string(), range(70, 80, None)),
        ("news".to_string(), range(81, 90, Some(4))),
    ];
    let w = warned_items(&r);
    let own: Vec<&Warning> = w.iter().filter(|l| l.text.starts_with("@s-one ")).collect();
    assert_eq!(
        own.iter().map(|l| &l.text).collect::<Vec<_>>(),
        [
            "@s-one (instance 100): the broker's retention gave up records of its own before it \
          handled them: its mail lane @50..@60 (`inbox` lane_given_up=); topic deploy \
          @70..@80, topic news @81..@90 (4 records) (`topic ls` given_up=)"
        ],
        "{w:#?}"
    );
    assert!(fix(&own[0].kind).starts_with("nothing brings those records back"));
    assert_eq!(w.len(), 2, "the node's range is its own line: {w:#?}");
    r.instances[0].sessions[0].lane_given_up = None;
    one(
        &r,
        "@s-one (instance 100): the broker's retention gave up records of its own before it handled them: topic deploy",
    );
    r.instances[0].sessions[0].topic_given_up.clear();
    assert_eq!(warned(&r).len(), 1, "only the node's: {:#?}", warned(&r));
}

/// R5 review: OVERLAPPING RANGES OF THE NODE'S GIVEN-UP MAIL ARE ONE LOSS. A later notice on
/// one subscription can overlap an earlier one (astream's `Gap::and`, whose counts sum): the
/// instance's counter widens its newest over both (`fabric status` `mail_gap=@4..@12`, no
/// count) while each session keeps the later (`given_up=@6..@12 given_up_count=1`). The
/// warning merges the counter's and the sessions' ranges that overlap, keeps a count only
/// where the range it counted is the whole merged one, keeps ranges that only touch apart,
/// and lists no empty range. NC (run): a `node_mail_given_up` keyed by the exact range says
/// `@6..@12 (1 record), @4..@12` for one range.
#[test]
fn overlapping_ranges_of_the_nodes_given_up_mail_are_listed_as_one() {
    let range = |from, to, records| GivenUpRange { from, to, records };
    let node_lines = |r: &Report| -> Vec<String> {
        warned(r)
            .into_iter()
            .filter(|l| l.contains("this node's unread mail"))
            .collect()
    };
    let said = |since: &str, ranges: &str| {
        vec![format!(
            "instance 100: the broker's retention gave up this node's unread mail before its \
             bridge read it{since}: {ranges}; the sessions it served then were told (`inbox` \
             given_up=, a `mail-gap` mark in each log)"
        )]
    };
    let one_told = " (1 range(s) since the instance started)";

    let mut r = healthy();
    r.instances[0].mail_gaps = Some(1);
    r.instances[0].mail_gap = Some(range(4, 12, None));
    r.instances[0].sessions[0].given_up = Some(range(6, 12, Some(1)));
    assert_eq!(
        node_lines(&r),
        said(one_told, "@4..@12"),
        "one range; the later notice's count is of part of it"
    );
    r.instances[0].mail_gap = Some(range(4, 12, Some(3)));
    assert_eq!(
        node_lines(&r),
        said(one_told, "@4..@12 (3 records)"),
        "a count of the whole merged range stays"
    );

    let mut r = healthy();
    let mut two = r.instances[0].sessions[0].clone();
    two.sid = "s-two".to_string();
    r.instances[0].sessions.push(two);
    r.instances[0].sessions[0].given_up = Some(range(10, 42, Some(3)));
    r.instances[0].sessions[1].given_up = Some(range(30, 50, Some(2)));
    assert_eq!(
        node_lines(&r),
        said("", "@10..@50"),
        "the sessions' alone, after an update"
    );
    let mut three = r.instances[0].sessions[0].clone();
    three.sid = "s-three".to_string();
    three.given_up = Some(range(50, 60, Some(1)));
    r.instances[0].sessions.push(three);
    let mut four = r.instances[0].sessions[0].clone();
    four.sid = "s-four".to_string();
    four.given_up = Some(range(60, 60, None));
    r.instances[0].sessions.push(four);
    assert_eq!(
        node_lines(&r),
        said("", "@50..@60 (1 record), @10..@50"),
        "ranges that only touch stay apart, and an empty one is no loss"
    );
}

/// A bus where retention holds overdue asks below the `in` lanes' oldest and gave up the
/// verdicts of some ([`held_ask_bus`]).
#[cfg(unix)]
struct HeldAsk {
    _server: astream_broker::BrokerHandle,
    _broker: astream_broker::Broker,
    _dir: aterm_tempfile::TempDir,
    path: String,
    /// The ask nothing answered or expired.
    ask: u64,
    /// The ask its asker's bridge expired, the `expired` given up.
    lapsed: u64,
    /// The ask the person answered, the answer given up.
    answered: u64,
    /// Each ask's `t=`.
    t: u64,
    /// When their deadlines are ten minutes past.
    now: u64,
    /// The bus head after the cut.
    head: u64,
    /// The node cap's faces (`enable::node_rules`), its group grant left out as
    /// `readable_faces` leaves it.
    node_faces: Vec<String>,
}

#[cfg(unix)]
impl HeldAsk {
    fn dial(&self) -> Conn {
        transport::connect(&self.path, aterm_spec::wait::MUST_HAPPEN)
            .expect("connect")
            .0
    }
}

/// RETENTION HOLDS OVERDUE ASKS BELOW THE `in` LANES' OLDEST (audit finding BUS-1, on one
/// node): `s-one@n-a` asks the person `h-jsmith` three times with a deadline of 10 min, 20
/// min before `now`; the person lanes' reader group (`/f/lab/cur/p/node/inbox`) is one
/// nothing commits, so retention holds the asks. On `s-one`'s own lane, the person answers
/// the first (`answered`) and `s-one`'s bridge records the second expired (`lapsed`); the
/// third (`ask`) gets neither. Last, a mail between two of the node's sessions. The node's
/// inbox group reads all three, and the bulk stream's cut (aterm's rules, its bytes
/// scaled) gives them up past the asks, which raises their subjects' floors, and so the
/// oldest of the whole fleet's `in` lanes (`/f/lab/in/>`) and of the node cap's
/// `in/*/*/n-a/*`, above every ask. CONTROL (each filter): read on at its floor
/// (`transport::fetch_kept`, every read status made before audit fix unit F7), it misses
/// the asks.
#[cfg(unix)]
fn held_ask_bus(tag: &str) -> HeldAsk {
    const BODY: u64 = 4096;
    let dir = crate::test_scratch_dir(tag);
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    broker.set_retention_interval(Duration::from_secs(3600));
    broker
        .set_retention(crate::broker_rules::rules_scaled(
            3 * BODY / 2,
            1 << 30,
            1 << 30,
        ))
        .expect("aterm's rules, scaled");
    let server = broker.serve(&socket).expect("serve the broker");
    let path = socket.to_string_lossy().into_owned();
    let (mut writer, _) =
        transport::connect(&path, aterm_spec::wait::MUST_HAPPEN).expect("connect");
    let t = 1_000_000_u64;
    let now = t + 20 * 60 * 1000;
    let mut seq = 0u64;
    let mut publish = |subject: &str, body: &str| {
        seq += 1;
        writer
            .publish(91, seq, subject, body.as_bytes())
            .unwrap_or_else(|e| panic!("publish on {subject}: {e}"))
            .0
    };
    let to_person = format!(
        "{}/ask",
        crate::subject::person_lane("lab", "h-jsmith", "n-a")
    );
    let asked = format!("v=1 t={t} from=s-one dl=600000 text=which");
    let answered = publish(&to_person, &asked);
    publish(
        &crate::subject::post_lane("lab", "n-a", "s-one", "h-jsmith", "answer"),
        &format!("v=1 t=1 re={answered} text=yes"),
    );
    let lapsed = publish(&to_person, &asked);
    publish(
        &crate::subject::post_lane("lab", "n-a", "s-one", "n-a", "expired"),
        &format!("v=1 t=1 re={lapsed} dl=600000 text=expired"),
    );
    let ask = publish(&to_person, &asked);
    let read = publish(
        &crate::subject::post_lane("lab", "n-a", "s-two", "n-a", "note"),
        "v=1 t=1 from=s-one text=fyi",
    );
    let filler = "e".repeat(BODY as usize);
    for _ in 0..3 {
        publish("/f/lab/pub/n-a/s-one/ev", &filler);
    }
    writer
        .commit(&crate::subject::inbox_group("lab", "n-a"), read)
        .expect("the node's inbox group reads the answer, the verdict and the mail");
    broker
        .retention_tick_at(broker.now_ms())
        .expect("the retention tick");
    let (_, (_, head)) = writer.fetch(0, "/f/lab/>", 0).expect("the head");
    let node_faces: Vec<String> = crate::enable::node_rules("lab", "n-a")
        .iter()
        .filter_map(|g| astream_cap::Grant::parse(g).ok())
        .map(|g| g.filter)
        .filter(|f| !f.starts_with("/f/lab/cur/"))
        .collect();
    for filter in ["/f/lab/in/>", "/f/lab/in/*/*/n-a/*"] {
        let of = astream_wire::Filter::new(filter).expect("a filter");
        assert!(
            broker.oldest(&of) > ask,
            "PRECONDITION: {filter}'s oldest lies above the asks retention holds"
        );
        let ((rows, _), floor) =
            transport::fetch_kept(&mut writer, 0, filter, transport::POST_PAGE)
                .expect("a read on at the floor");
        assert!(
            floor.is_some() && rows.iter().all(|(off, _, _)| *off > ask),
            "CONTROL: {filter} read on at its floor misses the held asks: {floor:?} {rows:?}"
        );
    }
    HeldAsk {
        _server: server,
        _broker: broker,
        _dir: dir,
        path,
        ask,
        lapsed,
        answered,
        t,
        now,
        head,
        node_faces,
    }
}

/// `aterm fabric status` LISTS AN ASK RETENTION HOLDS BELOW THE `in` LANES' OLDEST, JUDGED ON
/// THE VERDICTS THE BUS KEEPS (audit fix unit F7, C12, finding BUS-1; [`held_ask_bus`]):
/// read told, the deadline scan over the whole fleet's `in` lanes and over the node cap's
/// `in` faces both list the ask nothing answered or expired (`expired` `Some(false)`), and
/// the one whose `expired` retention gave up with `expired` `None` (`null` in `--json`),
/// and leave out the one whose answer retention gave up: status cannot say it had none, and
/// counts it unjudged (audit finding BUS2-3).
/// NEGATIVE CONTROLS (run): `overdue_work` reading through `transport::fetch_kept` lists
/// nothing; skipping its check of the asker's answers lists the answered ask; skipping its
/// check of the asker's `expired` verdicts says `Some(false)` for the lapsed one.
#[cfg(unix)]
#[test]
fn an_ask_held_below_the_in_lanes_oldest_is_listed_overdue() {
    let bus = held_ask_bus("al-held-ask");
    assert!(bus.answered < bus.lapsed && bus.lapsed < bus.ask);
    for faces in [None, Some(bus.node_faces.as_slice())] {
        let found =
            overdue_work(&mut bus.dial(), "lab", faces, bus.head, bus.now).expect("the deadlines");
        assert_eq!(
            found
                .overdue
                .iter()
                .map(|o| (o.off, o.expired))
                .collect::<Vec<_>>(),
            vec![(bus.lapsed, None), (bus.ask, Some(false))],
            "{faces:?}: {found:#?}"
        );
        assert_eq!(found.unjudged, 1, "{faces:?}: the answered ask");
        let mut r = healthy();
        r.overdue = found.overdue;
        let json = render_json(&r);
        assert!(
            json.contains(&format!("{{\"off\":{},", bus.lapsed))
                && json.contains("\"expired\":null"),
            "{json}"
        );
    }
}

/// AN ASK STATUS CANNOT JUDGE IS COUNTED, NEVER LISTED (audit finding BUS2-3): `s-one@n-a`
/// asks the person `h-jsmith` with a deadline that has passed (`ask`), then again with one
/// that has not (`later`), and the person answers `later` on `s-one`'s lane. The node's
/// inbox group reads the answer and a mail, and the bulk stream's cut (aterm's rules, its
/// bytes scaled) gives them up, which raises the floor of `s-one`'s answers above `ask`: the
/// bus no longer says whether `ask` had one. Over the whole fleet's `in` lanes and over the
/// node cap's faces, the deadline scan lists nothing and counts `ask` unjudged, and status
/// says so in one warning (exit 1), which `doctor` gives its own fix, and as
/// `overdue_unjudged` in `--json`.
/// NEGATIVE CONTROLS (run): `overdue_work`'s `continue` without the count gives 0 and no
/// warning; `fix_for` without the warning's branch gives it the catch-all.
#[cfg(unix)]
#[test]
fn an_ask_whose_askers_later_answer_was_given_up_is_counted_unjudged() {
    const BODY: u64 = 4096;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-unjudged")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    broker.set_retention_interval(Duration::from_secs(3600));
    broker
        .set_retention(crate::broker_rules::rules_scaled(
            3 * BODY / 2,
            1 << 30,
            1 << 30,
        ))
        .expect("aterm's rules, scaled");
    let _server = broker.serve(&socket).expect("serve the broker");
    let path = socket.to_string_lossy().into_owned();
    let dial = || {
        transport::connect(&path, aterm_spec::wait::MUST_HAPPEN)
            .expect("connect")
            .0
    };
    let mut writer = dial();
    let t = 1_000_000_u64;
    let now = t + 20 * 60 * 1000;
    let mut seq = 0u64;
    let mut publish = |subject: &str, body: &str| {
        seq += 1;
        writer
            .publish(91, seq, subject, body.as_bytes())
            .unwrap_or_else(|e| panic!("publish on {subject}: {e}"))
            .0
    };
    let to_person = format!(
        "{}/ask",
        crate::subject::person_lane("lab", "h-jsmith", "n-a")
    );
    let ask = publish(
        &to_person,
        &format!("v=1 t={t} from=s-one dl=600000 text=which"),
    );
    let later = publish(
        &to_person,
        &format!("v=1 t={now} from=s-one dl=600000 text=which"),
    );
    publish(
        &crate::subject::post_lane("lab", "n-a", "s-one", "h-jsmith", "answer"),
        &format!("v=1 t=1 re={later} text=yes"),
    );
    let read = publish(
        &crate::subject::post_lane("lab", "n-a", "s-two", "n-a", "note"),
        "v=1 t=1 from=s-one text=fyi",
    );
    let filler = "e".repeat(BODY as usize);
    for _ in 0..3 {
        publish("/f/lab/pub/n-a/s-one/ev", &filler);
    }
    writer
        .commit(&crate::subject::inbox_group("lab", "n-a"), read)
        .expect("the node's inbox group reads the answer and the mail");
    broker
        .retention_tick_at(broker.now_ms())
        .expect("the retention tick");
    let (_, (_, head)) = writer.fetch(0, "/f/lab/>", 0).expect("the head");
    let lane = verdict_lane("lab", &to_person, Some("s-one"));
    let (floor, _) = writer
        .oldest(&format!("{lane}/answer"))
        .expect("the answers' oldest");
    assert!(
        floor.is_some_and(|floor| floor > ask + 1),
        "PRECONDITION: the cut gave up the answer above the ask: {floor:?}"
    );
    let node_faces: Vec<String> = crate::enable::node_rules("lab", "n-a")
        .iter()
        .filter_map(|g| astream_cap::Grant::parse(g).ok())
        .map(|g| g.filter)
        .filter(|f| !f.starts_with("/f/lab/cur/"))
        .collect();
    for faces in [None, Some(node_faces.as_slice())] {
        let found = overdue_work(&mut dial(), "lab", faces, head, now).expect("the deadlines");
        assert_eq!(
            (found.overdue.len(), found.unjudged),
            (0, 1),
            "{faces:?}: {found:#?}"
        );
        let mut r = healthy();
        r.overdue = found.overdue;
        r.overdue_unjudged = found.unjudged;
        r.warning_items = warned_items(&r);
        r.warnings = r.warning_items.iter().map(|w| w.text.clone()).collect();
        assert_eq!(r.warnings.len(), 1, "{faces:?}: {:?}", r.warnings);
        assert_eq!(r.warning_items[0].kind, WarningKind::OverdueUnjudged);
        let role = crate::enable::Role::default();
        let fix = crate::enable::fix_for(&r.warning_items[0].kind, &role);
        assert!(
            fix.contains("nothing on the bus is broken")
                && fix.contains(&OVERDUE_SCAN_SPAN.to_string()),
            "doctor gives the warning its own fix: {fix}"
        );
        assert_eq!(exit_of(&r), 1, "{faces:?}");
        let json: aterm_json::Value = aterm_json::from_str(&render_json(&r)).expect("JSON");
        assert_eq!(json["overdue_unjudged"].as_u64(), Some(1), "{faces:?}");
        assert_eq!(json["overdue"].as_array().map(Vec::len), Some(0));
    }
}

/// THE LANE AN ASK'S VERDICTS LAND ON, AND THE FACES THAT READ IT (`verdict_lane`,
/// `filter_meet`): a session's ask (to a person or a session) is answered on its own lane,
/// a person's on the person's lane, an asker the record does not name on every lane; the
/// meet of two filters matches exactly what both match. NEGATIVE CONTROL (run): a meet
/// that takes the first filter's `*` over the second's literal fails it.
#[test]
fn an_asks_verdicts_are_read_on_its_askers_lane() {
    let lane = |subject: &str, from: Option<&str>| verdict_lane("lab", subject, from);
    assert_eq!(
        lane("/f/lab/in/p/h-jsmith/n-a/ask", Some("s-one")),
        "/f/lab/in/n-a/s-one/*"
    );
    assert_eq!(
        lane("/f/lab/in/n-b/s-two/n-a/ask", Some("s-one")),
        "/f/lab/in/n-a/s-one/*"
    );
    assert_eq!(
        lane("/f/lab/in/n-a/s-one/h-jsmith/ask", None),
        "/f/lab/in/p/h-jsmith/*"
    );
    for (subject, from) in [
        ("/f/lab/in/n-b/s-two/n-a/ask", None),
        ("/f/lab/in/n-b/s-two/n-a/ask", Some("s-Bad")),
        ("/f/lab/in/n-b/s-two/s-one/ask", Some("s-one")),
    ] {
        assert_eq!(lane(subject, from), "/f/lab/in/*/*/*", "{subject} {from:?}");
    }
    let verdicts = "/f/lab/in/n-b/s-two/*/answer";
    assert_eq!(
        filter_meet(verdicts, "/f/lab/in/>").as_deref(),
        Some(verdicts)
    );
    assert_eq!(
        filter_meet(verdicts, "/f/lab/in/*/*/n-a/*").as_deref(),
        Some("/f/lab/in/n-b/s-two/n-a/answer")
    );
    assert_eq!(
        filter_meet("/f/lab/in/*/*/n-a/*", verdicts).as_deref(),
        Some("/f/lab/in/n-b/s-two/n-a/answer")
    );
    assert_eq!(filter_meet(verdicts, "/f/lab/in/n-a/>"), None);
    assert_eq!(filter_meet(verdicts, "/f/lab/in/*/*/*"), None);
    assert_eq!(
        filter_meet("/f/lab/in/>", "/f/lab/>").as_deref(),
        Some("/f/lab/in/>")
    );
}

/// A HELD RECORD BELOW ITS FILTER'S OLDEST HAS ITS AGE (`record_t`, [`held_ask_bus`]): read
/// told on a connection that asked for nothing before, the record at the ask's offset is
/// found on the whole fleet's `in` lanes and on the node cap's first `in` face. NEGATIVE
/// CONTROL (run): `record_t` reading with a plain `fetch` gets no age.
#[cfg(unix)]
#[test]
fn a_record_held_below_the_in_lanes_oldest_has_its_age() {
    let bus = held_ask_bus("al-held-age");
    for faces in [None, Some(bus.node_faces.as_slice())] {
        let mut fresh = bus.dial();
        assert_eq!(
            in_filters("lab", faces)
                .iter()
                .find_map(|f| record_t(&mut fresh, f, bus.ask)),
            Some(bus.t),
            "{faces:?}"
        );
    }
}

/// TRAFFIC SHOWS A RECORD HELD BELOW THE FLEET'S OLDEST (`last_records`,
/// [`held_ask_bus`]): the fleet's root reaches the bulk stream's class, so its oldest is the
/// cut, and read told the newest records include the ask held below it. NEGATIVE CONTROL
/// (run): `last_records` reading through `transport::fetch_kept` leaves it out.
#[cfg(unix)]
#[test]
fn traffic_shows_a_record_held_below_the_fleets_oldest() {
    let bus = held_ask_bus("al-held-traffic");
    let traffic =
        last_records(&mut bus.dial(), "lab", None, bus.head, TRAFFIC_ROWS, 0).expect("the traffic");
    assert!(
        traffic.iter().any(|t| t.off == bus.ask && t.kind == "ask"),
        "{traffic:#?}"
    );
}

#[cfg(unix)]
#[test]
fn the_status_reports_only_a_halt_a_bridge_applies() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-halt")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("the broker dir");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _server = broker.serve(&socket).expect("serve the broker");
    let (mut conn, _closer) =
        transport::connect(&socket.to_string_lossy(), Duration::from_secs(60)).expect("connect");
    for (seq, subject, body) in [
        (
            1,
            "/f/f1/fleet/h-andrew/halt",
            "v=1 t=1 state=on reason=main%20broken",
        ),
        (2, "/f/f1/fleet/a-svc/halt", "v=1 t=1 state=on reason=svc"),
        (3, "/f/f1/fleet/n-node/halt", "v=1 t=1 state=on reason=node"),
        (4, "/f/f1/fleet/h-bea/halt", "v=1 t=1\tstate=on reason=tab"),
        (
            5,
            "/f/f1/fleet/h-cy/halt",
            "v=1 t=1 state=off reason=lifted",
        ),
    ] {
        conn.publish(91, seq, subject, body.as_bytes())
            .expect("publish a halt row");
    }
    assert_eq!(
        fleet_halts(&mut conn, "f1"),
        vec![("h-andrew".to_string(), "main broken".to_string())]
    );
}

#[test]
fn an_ask_past_its_deadline_with_no_reply_is_a_warning() {
    let now = 100 * 60 * 1000u64;
    let lane = |src: &str, kind: &str| format!("/f/lab/in/n-a/s-one/{src}/{kind}");
    let rec = |off: u64, subject: String, body: &str| (off, subject, body.as_bytes().to_vec());
    let records = vec![
        rec(
            40,
            lane("h-andrew", "ask"),
            &format!("v=1 t={} dl=600000 text=which", now - 20 * 60 * 1000),
        ),
        rec(
            41,
            lane("h-andrew", "task"),
            &format!("v=1 t={} dl=600000 text=do", now - 20 * 60 * 1000),
        ),
        rec(
            45,
            lane("n-b", "answer"),
            &format!("v=1 t={} from=s-two re=41 text=done", now - 5 * 60 * 1000),
        ),
        rec(
            42,
            lane("n-b", "ask"),
            &format!(
                "v=1 t={} from=s-two dl=600000 text=why",
                now - 20 * 60 * 1000
            ),
        ),
        rec(
            46,
            lane("n-a", "expired"),
            &format!("v=1 t={} re=42 dl=600000 text=expired", now - 9 * 60 * 1000),
        ),
        rec(
            43,
            lane("h-andrew", "ask"),
            &format!("v=1 t={} dl=600000 text=soon", now - 5 * 60 * 1000),
        ),
        rec(
            44,
            lane("h-andrew", "note"),
            &format!("v=1 t={} dl=1 text=fyi", now - 20 * 60 * 1000),
        ),
        rec(
            47,
            lane("h-andrew", "ask"),
            &format!("v=1 t={} text=nodl", now - 20 * 60 * 1000),
        ),
        rec(
            48,
            lane("h-andrew", "ask"),
            &format!("v=1 t={} dl=1 text=a", now - 20 * 60 * 1000),
        ),
        rec(
            49,
            lane("n-b", "ack"),
            "v=1 t=1 re=48 text=verdict%3Dhandled",
        ),
        rec(
            50,
            lane("h-andrew", "task"),
            &format!("v=1 t={} dl=1 text=b", now - 20 * 60 * 1000),
        ),
        rec(51, lane("n-b", "report"), "v=1 t=1 re=50 text=done"),
    ];
    let overdue = overdue_of("lab", &records, now);
    let offs: Vec<(u64, Option<bool>)> = overdue.iter().map(|o| (o.off, o.expired)).collect();
    assert_eq!(
        offs,
        vec![(40, Some(false)), (42, Some(true))],
        "{overdue:#?}"
    );
    assert_eq!(overdue[0].late_ms, 10 * 60 * 1000);
    assert_eq!(overdue[0].from, "h-andrew");
    assert_eq!(overdue[0].to, "s-one@n-a");
    assert_eq!(overdue[1].from, "s-two@n-b");

    let mut r = healthy();
    r.overdue = overdue;
    let w = warned(&r);
    assert_eq!(w.len(), 2, "{w:#?}");
    assert!(w[0].contains("off=40"), "{}", w[0]);
    assert!(w[1].contains("off=42"), "{}", w[1]);
    r.warnings = w;
    assert_eq!(exit_of(&r), 1);
    let json = render_json(&r);
    assert!(json.contains("\"overdue\":[{\"off\":40,"), "{json}");
    assert!(json.contains("\"expired\":true"), "{json}");

    let mut r = healthy();
    r.instances[0].supervised = Some(false);
    r.instances[0].fabric = Some("absent".to_string());
    one(&r, "fabric=absent");
    let mut r = healthy();
    r.instances[0].fabric = Some("disconnected".to_string());
    one(&r, "fabric=disconnected");

    r.instances[0].sessions[0].posts = Some(3);
    one(&r, "@s-one");

    let mut r = healthy();
    r.instances[0].command = Some(words(
        "aterm link serve --fleet lab --broker /tmp/old.sock --state /s",
    ));
    one(&r, "--broker /tmp/old.sock");
    r.broker.reachable = false;
    assert!(
        !warned(&r)
            .iter()
            .any(|l| l.contains("says fabric=connected")),
        "{:#?}",
        warned(&r)
    );
    let mut r = healthy();
    let substitute = CMD.replacen("aterm link", "/bin/sh -c /tmp/x link", 1);
    r.instances[0].command = Some(words(&substitute));
    r.instances[0].program = bridge_program(&words(&substitute));
    one(
        &r,
        &format!(
            "instance 100's bridge was armed with a different command than [fabric] command \
             in /c/aterm.toml: it runs program {} (not {})",
            r.instances[0]
                .program
                .as_deref()
                .expect("a bridge's program"),
            r.program.as_deref().expect("the report's program")
        ),
    );
    assert!(
        r.instances[0]
            .program
            .as_deref()
            .is_some_and(|p| p.ends_with(" -c /tmp/x")),
        "the words before `link serve` are the program's: {:?}",
        r.instances[0].program
    );

    let mut r = healthy();
    let mut twin = r.instances[0].clone();
    twin.pid = 200;
    twin.sessions.clear();
    twin.fabric = Some("stalled".to_string());
    twin.reason = Some("state-locked".to_string());
    r.instances.push(twin);
    one(&r, "reason=state-locked");

    let mut r = healthy();
    r.instances[0].error = Some("connect: timed out".to_string());
    one(&r, "connect: timed out");

    let mut r = healthy();
    r.source = Source::Instance(100);
    one(&r, "/c/aterm.toml");

    let mut r = healthy();
    r.node = None;
    r.caps = vec![(
        "/c/node.cap".to_string(),
        Err("the tag is not hex".to_string()),
    )];
    one(&r, "/s/node");
    one(&r, "the tag is not hex");

    let r = healthy();
    let w = warnings(
        &r,
        &BTreeMap::new(),
        &[("h-andrew".to_string(), "stop".to_string())],
    );
    assert!(w.iter().any(|l| l.contains("reason=stop")), "{w:#?}");

    let mut r = healthy();
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-kept")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a state dir");
    r.cfg.state_dir = dir.path().to_string_lossy().into_owned();
    let st = crate::state::StateDir::open(dir.path()).expect("open");
    let halt = |reason: &str, kept| crate::state::StandingHalt {
        reason: reason.to_string(),
        kept,
    };
    st.set_standing_halts(
        "lab",
        &BTreeMap::from([
            ("h-andrew".to_string(), halt("main%20broken", true)),
            ("h-bea".to_string(), halt("stop", false)),
        ]),
    )
    .expect("write");
    let w = warned(&r);
    assert!(w.iter().any(|l| l.contains("h-andrew")), "{w:#?}");
    assert!(!w.iter().any(|l| l.contains("h-bea's")), "{w:#?}");

    let mut r = healthy();
    let fresh_gone = format!("v=1 t={} state=exited hold=0", crate::now_ms());
    let rows: [(&str, &[u8]); 5] = [
        (
            "/f/lab/pub/n-a/node/presence",
            b"v=1 t=1 state=live fabric=connected",
        ),
        (
            "/f/lab/pub/n-a/s-one/presence",
            b"v=1 t=1 state=live hold=0",
        ),
        (
            "/f/lab/pub/n-a/s-ghost/presence",
            b"v=1 t=1 state=live hold=0",
        ),
        ("/f/lab/pub/n-a/s-gone/presence", fresh_gone.as_bytes()),
        (
            "/f/lab/pub/n-far/s-far/presence",
            b"v=1 t=1 state=live hold=0",
        ),
    ];
    let glance = crate::glance::Glance {
        fleet: "lab".to_string(),
        rows: rows
            .iter()
            .enumerate()
            .filter_map(|(i, (subj, body))| crate::glance::Row::parse("lab", i as u64, subj, body))
            .collect(),
        truncated: false,
    };
    join_sessions(&mut r, Some(&glance));
    let joined: Vec<(&str, Option<u32>, &str)> = r
        .sessions
        .iter()
        .map(|s| (s.sid.as_str(), s.pid, s.bus.as_str()))
        .collect();
    assert_eq!(
        joined,
        vec![
            ("s-one", Some(100), "live"),
            ("s-far", None, "live"),
            ("s-ghost", None, "live"),
            ("s-gone", None, "gone"),
        ],
        "local rows first, then the bus-only ones by sid; a recently retired \
         local row is LISTED as `gone` rather than counted"
    );
    assert_eq!(r.exited, 0, "nothing here is old enough to be a bare count");
    let w = warned(&r);
    assert!(w.iter().any(|l| l.contains("@s-ghost")), "{w:#?}");
    assert!(
        !w.iter().any(|l| l.contains("@s-gone")),
        "a retired row is not advertised live and must not be warned about: {w:#?}"
    );
    assert!(
        !w.iter().any(|l| l.contains("@s-far")),
        "a REMOTE node's session is not this machine's to host: {w:#?}"
    );
}

#[test]
fn a_will_reads_away_and_so_do_the_rows_it_left_live() {
    let mut r = healthy();
    let fresh_exited = format!("v=1 t={} state=exited hold=0", crate::now_ms());
    let rows: [(&str, &[u8]); 7] = [
        (
            "/f/lab/pub/n-a/node/presence",
            b"v=1 t=1 state=live fabric=connected",
        ),
        (
            "/f/lab/pub/n-a/s-one/presence",
            b"v=1 t=1 state=live hold=0",
        ),
        (
            "/f/lab/pub/n-far/node/presence",
            b"v=1 t=1 state=away inc=4 fabric=disconnected",
        ),
        (
            "/f/lab/pub/n-old/node/presence",
            b"v=1 t=1 state=gone inc=2 fabric=disconnected",
        ),
        (
            "/f/lab/pub/n-far/s-far/presence",
            b"v=1 t=1 state=live hold=0",
        ),
        (
            "/f/lab/pub/n-old/s-old/presence",
            b"v=1 t=1 state=live hold=0",
        ),
        ("/f/lab/pub/n-a/s-ended/presence", fresh_exited.as_bytes()),
    ];
    let glance = crate::glance::Glance {
        fleet: "lab".to_string(),
        rows: rows
            .iter()
            .enumerate()
            .filter_map(|(i, (subj, body))| crate::glance::Row::parse("lab", i as u64, subj, body))
            .collect(),
        truncated: false,
    };
    join_sessions(&mut r, Some(&glance));
    let joined: Vec<(&str, &str)> = r
        .sessions
        .iter()
        .map(|s| (s.sid.as_str(), s.bus.as_str()))
        .collect();
    assert_eq!(
        joined,
        vec![
            ("s-one", "live"),
            ("s-ended", "gone"),
            ("s-far", "away"),
            ("s-old", "away"),
        ]
    );
    assert_eq!(
        live_sessions_on(&r, "n-far"),
        1,
        "an away node keeps its sessions"
    );
    let text = render_text(&r);
    for node in ["n-far", "n-old"] {
        let line = text
            .lines()
            .find(|l| l.contains(node) && l.contains("host="))
            .unwrap_or_else(|| panic!("the {node} row:\n{text}"));
        assert!(line.contains("state=away"), "{line}");
    }
    assert!(
        render_json(&r).contains("\"state\":\"gone\",\"reads\":\"away\""),
        "the JSON keeps the word on the bus beside the reading"
    );
}

#[test]
fn a_cut_roster_and_a_spent_subject_budget_are_said() {
    let mut quiet = healthy();
    let text = render_text(&quiet);
    assert!(text.contains(" 3 of 4096 spent by this node"), "{text}");
    assert!(!text.contains("truncated"), "{text}");
    assert_eq!(warned(&quiet), Vec::<String>::new());
    quiet.subjects = Some(Err("unauthorized".to_string()));
    assert!(render_text(&quiet).contains("could not be counted: unauthorized"));

    let mut r = healthy();
    r.roster_rows = Some(crate::glance::MAX_ROWS);
    r.roster_truncated = true;
    r.subjects = Some(Ok(SUBJECT_BUDGET_WARN));
    assert!(
        render_text(&r).contains("rows=4096+ truncated"),
        "the SESSIONS section says the read was cut"
    );
    one(&r, "rows=4096+ truncated");
    one(
        &r,
        &format!("holds {SUBJECT_BUDGET_WARN} of the 4096 distinct subjects"),
    );
    let json = render_json(&r);
    assert!(
        json.contains("\"roster\":{\"rows\":4096,\"truncated\":true}"),
        "{json}"
    );
    assert!(
        json.contains(&format!(
            "\"subjects\":{{\"used\":{SUBJECT_BUDGET_WARN},\"budget\":4096"
        )),
        "{json}"
    );
}

#[cfg(unix)]
#[test]
fn the_subject_count_is_read_off_a_guarded_broker_through_the_nodes_ring() {
    let secret = [5u8; 32];
    let dir = aterm_tempfile::Builder::new()
        .prefix("atfab-subj")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let socket = dir.path().join("b.sock");
    let broker = astream_broker::Broker::open_guarded(dir.path().join("b.log"), secret.to_vec())
        .expect("a guarded broker");
    let _server = broker.serve(&socket).expect("serve");
    let ring = |node: &str| -> Conn {
        let (mut conn, _) = transport::connect(&socket.to_string_lossy(), Duration::from_secs(60))
            .expect("connect");
        for grant in crate::enable::node_rules("lab", node) {
            let cap = astream_cap::mint(&secret, &grant).expect("mint");
            conn.attach(&cap.filter, &cap.tag).expect("attach");
        }
        conn
    };
    let publish = |conn: &mut Conn, node: &str, subjects: &[&str]| {
        let producer = astream_cap::producer_id_of(node);
        for (seq, subject) in subjects.iter().enumerate() {
            conn.publish(producer, seq as u64 + 1, subject, b"v=1 t=1")
                .unwrap_or_else(|e| panic!("{node} publishes {subject}: {e}"));
        }
    };
    let mut a = ring("n-a");
    publish(
        &mut a,
        "n-a",
        &[
            "/f/lab/pub/n-a/node/presence",
            "/f/lab/pub/n-a/node/ev",
            "/f/lab/pub/n-a/s-1/presence",
            "/f/lab/pub/n-a/s-1/ev",
            "/f/lab/pub/n-a/s-2/presence",
            "/f/lab/pub/n-a/s-1/say/lab",
            "/f/lab/in/n-b/s-9/n-a/ask",
            "/f/lab/in/n-b/s-9/n-a/ask",
            "/f/lab/in/n-b/s-8/n-a/note",
            "/f/lab/pub/n-a/node/presence",
        ],
    );
    let mut b = ring("n-b");
    publish(
        &mut b,
        "n-b",
        &[
            "/f/lab/pub/n-b/node/presence",
            "/f/lab/pub/n-b/s-9/presence",
            "/f/lab/in/n-a/s-1/n-b/ask",
        ],
    );
    assert_eq!(subjects_of(&mut a, "lab", "n-a").expect("counted"), 8);
    assert_eq!(subjects_of(&mut b, "lab", "n-b").expect("counted"), 3);
}

#[test]
fn the_report_prints_no_body() {
    let mut r = healthy();
    r.traffic = vec![traffic_of(
        "lab",
        7,
        "/f/lab/in/n-a/s-one/h-andrew/task",
        b"v=1 t=6000000 text=the%20secret%20plan",
        0,
    )];
    r.warnings = warned(&r);
    let text = render_text(&r);
    assert!(
        !text.contains("secret"),
        "a body reached the report: {text}"
    );
    let json = render_json(&r);
    assert!(!json.contains("secret"), "{json}");
    assert!(json.contains("\"exit\":0"), "{json}");
}

#[test]
fn a_retire_refusal_event_names_its_sid() {
    for (ev, sid) in [
        (
            "presence-retire-refused sid=s-x reason=hosted pid=42",
            Some("s-x"),
        ),
        (
            "presence-retire-refused sid=s-y reason=unaskable",
            Some("s-y"),
        ),
        ("presence-retire-refused sid=s-z reason=new", Some("s-z")),
        ("presence-retired sid=s-x reason=operator", None),
        ("presence-retire-refused reason=hosted", None),
    ] {
        assert_eq!(
            refused_retire_event(ev).map(|(s, _)| s).as_deref(),
            sid,
            "{ev}"
        );
    }
}

#[test]
fn a_json_value_cannot_break_out_of_its_string() {
    let mut out = String::new();
    J::Obj(vec![
        ("a", J::s("q\"uo\\te\n\u{1b}")),
        ("b", J::Arr(vec![J::Num(1), J::Null, J::Bool(true)])),
    ])
    .render(&mut out);
    assert_eq!(out, r#"{"a":"q\"uo\\te\u000a\u001b","b":[1,null,true]}"#);
}

#[test]
fn reports_tails_and_inboxes_label_offsets_across_daylight_saving() {
    const PDT: i64 = -7 * 3600;
    const PST: i64 = -8 * 3600;
    const CHANGE: i64 = 1_793_523_600; // 2026-11-01T09:00:00Z
    let la_at = |unix: i64| Some(if unix < CHANGE { PDT } else { PST });
    let pre_ms = u64::try_from(CHANGE - 1800).unwrap() * 1000; // 01:30:00 PDT
    let post_ms = u64::try_from(CHANGE + 1800).unwrap() * 1000; // 01:30:00 PST
    let now_ms = u64::try_from(CHANGE + 2700).unwrap() * 1000; // 01:45:00 PST

    assert_eq!(clock_at(0, now_ms, Some(PST), &la_at), "-");
    assert_eq!(
        clock_at(pre_ms, now_ms, Some(PST), &la_at),
        "01:30:00 UTC-07:00"
    );
    assert_eq!(clock_at(post_ms, now_ms, Some(PST), &la_at), "01:30:00");
    assert_eq!(
        clock_at(pre_ms, now_ms, Some(PST), &|_| None),
        "08:30:00 UTC"
    );
    assert_eq!(
        clock(pre_ms, now_ms, PST),
        "00:30:00",
        "CONTROL: flat PST shifts pre-change 01:30 PDT to 00:30"
    );

    let mut r = healthy();
    r.now_ms = now_ms;
    r.traffic = vec![
        traffic_of(
            "f",
            1,
            "/f/f/in/n-1/s-1/h-a/note",
            format!("v=1 t={pre_ms} text=pre").as_bytes(),
            now_ms,
        ),
        traffic_of(
            "f",
            2,
            "/f/f/in/n-1/s-1/h-a/note",
            format!("v=1 t={post_ms} text=post").as_bytes(),
            now_ms,
        ),
    ];
    let report = render_text_local(&r, &la_at);
    assert!(report.contains("01:45:00 · times UTC-08:00"), "{report}");
    assert!(
        report.contains("  @1      01:30:00 UTC-07:00  note")
            && report.contains("  @2      01:30:00            note"),
        "{report}"
    );

    let tail = TailFmt {
        bodies: false,
        json: false,
        offset: Some(PST),
    };
    let tail_pre = tail.record_at(
        "f",
        1,
        "/f/f/in/n-1/s-1/h-a/note",
        format!("v=1 t={pre_ms} text=pre").as_bytes(),
        now_ms,
        &la_at,
    );
    let tail_post = tail.record_at(
        "f",
        2,
        "/f/f/in/n-1/s-1/h-a/note",
        format!("v=1 t={post_ms} text=post").as_bytes(),
        now_ms,
        &la_at,
    );
    assert!(tail_pre.contains("01:30:00 UTC-07:00  note"), "{tail_pre}");
    assert!(
        tail_post.contains("01:30:00  note") && !tail_post.contains("UTC-"),
        "{tail_post}"
    );

    let inbox_pre = crate::person::inbox_delivery_line(
        ("f", "h-a.cap"),
        7,
        "/f/f/in/p/h-a/n-1/note",
        format!("v=1 t={pre_ms} text=hello").as_bytes(),
        now_ms,
        Some(PST),
        &la_at,
    );
    let inbox_post = crate::person::inbox_delivery_line(
        ("f", "h-a.cap"),
        8,
        "/f/f/in/p/h-a/n-1/note",
        format!("v=1 t={post_ms} text=later").as_bytes(),
        now_ms,
        Some(PST),
        &la_at,
    );
    assert_eq!(
        inbox_pre,
        "@7  01:30:00 UTC-07:00  note  from=n-1 trust=agent  text=hello\n"
    );
    assert_eq!(
        inbox_post,
        "@8  01:30:00  note  from=n-1 trust=agent  text=later\n"
    );
}

/// An instance whose children could not be read has no bridge list, not an empty one: the JSON
/// says `null` and the BRIDGES table `?`, while one read with no bridge says `[]` and `-`.
/// NEGATIVE CONTROL: render an unread list as empty, and the unread instance reads bridgeless.
#[test]
fn an_unread_bridge_list_is_unknown_not_empty() {
    let mut r = healthy();
    r.instances[0].bridge_pids = None;
    assert!(render_json(&r).contains("\"bridge_pids\":null"));
    // The BRIDGE cell of instance 100's row; the table's cells are two spaces apart.
    let row = |text: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with("100 "))
            .and_then(|l| {
                l.split("  ")
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .nth(3)
                    .map(str::to_string)
            })
    };
    assert_eq!(
        row(&render_text_local(&r, &|_| Some(0))).as_deref(),
        Some("?")
    );
    r.instances[0].bridge_pids = Some(Vec::new());
    assert!(render_json(&r).contains("\"bridge_pids\":[]"));
    assert_eq!(
        row(&render_text_local(&r, &|_| Some(0))).as_deref(),
        Some("-")
    );
}
