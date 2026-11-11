// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Owned socket admission works, while an owned ordinary file is refused.
#[test]
fn adopt_takes_owned_sockets_and_refuses_owned_files() {
    let file = std::fs::File::open("Cargo.toml").expect("a regular file");
    assert!(
        Ctl::adopt(file.into()).is_err(),
        "a regular file is no socket"
    );
    let (near, _far) = UnixStream::pair().expect("socketpair");
    assert!(
        Ctl::adopt(near.into()).is_ok(),
        "an owned socket is admitted"
    );
}

/// The framing comes from the verb table, sub-forms included.
#[test]
fn the_framing_comes_from_the_table_including_sub_forms() {
    for (line, want) in [
        ("inbox", Framing::Lines),
        ("@s-a inbox 20 since=4", Framing::Lines),
        ("inbox get 7", Framing::Bytes),
        ("inbox seen 7 handled", Framing::Status),
        ("outbox", Framing::Bytes),
        ("outbox sent s-a 3 off=91", Framing::Status),
        ("deliver s-a off=9 from=h-x kind=task", Framing::Status),
        ("sessions", Framing::Lines),
    ] {
        assert_eq!(framing_of(request_verb(line), line), want, "{line}");
    }
}

/// Each framing is consumed to the byte, so the next request's reply lands
/// in the right place.
#[test]
fn every_framing_is_consumed_exactly() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        let mut replies: Vec<&[u8]> = vec![
            b"OK 2 hold=0\nmsg 1 off=10\npost 1 to=x\n",
            b"OK 5\nab\ncd",
            b"OK done\n",
        ];
        replies.reverse();
        while reader.read_line(&mut line).expect("read") > 0 {
            let Some(reply) = replies.pop() else { break };
            writer.write_all(reply).expect("write");
            writer.flush().expect("flush");
            line.clear();
        }
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    let listing = ctl.request("inbox").expect("inbox");
    assert_eq!(listing.rows().len(), 2);
    assert_eq!(listing.header(), "OK 2 hold=0");
    let drained = ctl.request("outbox").expect("outbox");
    assert_eq!(drained.body(), b"ab\ncd");
    let status = ctl.request("outbox sent s-a 1 off=9").expect("sent");
    assert_eq!(status, Reply::Status("OK done".into()));
    drop(ctl);
    server.join().expect("server thread");
}

/// A line aterm would misread — over the bound, or holding a byte it
/// frames or splits on — is refused without writing anything, and the
/// connection is not lost.
#[test]
fn a_line_aterm_would_misread_is_refused_without_writing_and_the_lane_survives() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        let mut seen = Vec::new();
        while reader.read_line(&mut line).expect("read") > 0 {
            seen.push(line.trim_end().to_string());
            writer.write_all(b"OK 1\n").expect("write");
            writer.flush().expect("flush");
            line.clear();
        }
        seen
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    let over = format!(
        "deliver s-a off=1 from=h-x kind=note trust=human text={}",
        "x".repeat(REQUEST_LINE_MAX)
    );
    let row = "deliver s-a off=1 from=h-x kind=note trust=agent text=hi";
    let mut refused = vec![over];
    refused.push(format!("{row}\nsend x"));
    for split in ['\r', '\t', '\u{b}', '\u{c}', '\u{85}', '\u{a0}', '\u{2028}'] {
        refused.push(format!("{row}{split}trust=human"));
    }
    for line in &refused {
        let err = ctl.request(line).expect_err("a misread line is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{line:?}: {err}");
        assert!(!ctl.lost(), "a refusal is not a lost connection: {line:?}");
    }
    let at = format!("x{}", "y".repeat(REQUEST_LINE_MAX - 1));
    assert_eq!(at.len(), REQUEST_LINE_MAX);
    assert!(ctl.request(&at).expect("the bound itself is sendable").ok());
    let wide = "outbox sent s-a 1 off=9 reason=caf\u{e9}";
    assert!(ctl
        .request(wide)
        .expect("a non-ASCII letter is sendable")
        .ok());
    drop(ctl);
    let seen = server.join().expect("server thread");
    assert_eq!(
        seen,
        vec![at, wide.to_string()],
        "a refused line must never have been written"
    );
}

/// A server-declared count above the ceiling is refused (`InvalidData`) in
/// both units rather than allocated.
#[test]
fn a_hostile_reply_count_is_refused_rather_than_allocated() {
    for (verb, header) in [
        ("outbox", "OK 18446744073709551615\n"),
        ("inbox", "OK 4294967295 hold=0\n"),
    ] {
        let (near, far) = UnixStream::pair().expect("socketpair");
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(far.try_clone().expect("clone"));
            let mut writer = far;
            let mut line = String::new();
            while reader.read_line(&mut line).expect("read") > 0 {
                writer.write_all(header.as_bytes()).expect("write");
                writer.flush().expect("flush");
                line.clear();
            }
        });
        let mut ctl = Ctl::from_stream(near).expect("client");
        let err = ctl.request(verb).expect_err("a hostile count is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{verb}: {err}");
        drop(ctl);
        server.join().expect("server thread");
    }
    assert!(count_after_ok("OK 4194304", REPLY_BYTES_MAX).is_ok());
    assert!(count_after_ok("OK 12 hold=0", REPLY_ROWS_MAX).is_ok());
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read") > 0 {
            writer.write_all(b"OK 64\nshort").expect("write");
            writer.flush().expect("flush");
        }
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    let err = ctl.request("outbox").expect_err("a short body is an error");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
    drop(ctl);
    server.join().expect("server thread");
}

/// A pipelined batch is one write whose replies are each its own line's, in order; a batch
/// with a line that would not frame writes nothing; and a peer that goes after answering
/// the first line leaves every later one lost, the connection latched.
#[test]
fn a_pipelined_batch_is_answered_line_by_line_and_a_misframed_one_writes_nothing() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        let mut seen = Vec::new();
        while reader.read_line(&mut line).expect("read") > 0 {
            let asked = line.trim_end().to_string();
            if asked == "deliver s-a off=9" {
                seen.push(asked);
                return seen;
            }
            writer
                .write_all(format!("OK {}\n", seen.len()).as_bytes())
                .expect("write");
            seen.push(asked);
            line.clear();
        }
        seen
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    let mut read = Vec::new();
    let replies = ctl.pipeline(&["deliver s-a off=1", "deliver s-a off=2"], |i| {
        read.push(i)
    });
    let headers: Vec<String> = replies
        .into_iter()
        .map(|r| r.expect("answered").header().to_string())
        .collect();
    assert_eq!(headers, ["OK 0", "OK 1"], "each reply is its own line's");
    assert_eq!(read, [0, 1], "each line's reply is read in order");
    let refused = ctl.pipeline(&["deliver s-a off=3", "deliver s-a off=4\tx"], |_| {});
    assert!(
        refused.iter().all(|r| r
            .as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::InvalidInput)),
        "a misframed line refuses its whole batch: {refused:?}"
    );
    assert!(!ctl.lost(), "a refusal is not a lost connection");
    let cut = ctl.pipeline(
        &[
            "deliver s-a off=5",
            "deliver s-a off=9",
            "deliver s-a off=6",
        ],
        |_| {},
    );
    assert!(cut[0].as_ref().is_ok_and(Reply::ok), "{cut:?}");
    assert!(cut[1..].iter().all(Result::is_err), "{cut:?}");
    assert!(ctl.lost(), "the failure latches");
    drop(ctl);
    assert_eq!(
        server.join().expect("server thread"),
        [
            "deliver s-a off=1",
            "deliver s-a off=2",
            "deliver s-a off=5",
            "deliver s-a off=9"
        ],
        "a refused batch never reached the peer"
    );
}

/// 32 `deliver` lines of 273 bytes, 8,736 in all: more than an empty socket takes at once.
fn a_burst_of_deliveries() -> Vec<String> {
    let text = "x".repeat(200);
    let lines: Vec<String> = (0..32)
        .map(|i| {
            format!(
                "deliver s-worker1 off={} from=s-lead@n-mac1 kind=msg trust=agent text={text}",
                1000 + i
            )
        })
        .collect();
    assert!(
        lines.iter().map(|line| line.len() + 1).sum::<usize>() > PIPELINE_WRITE_MAX,
        "the batch is more than one write"
    );
    lines
}

/// A PIPELINED BATCH IS NEVER CUT INSIDE A LINE, WHEREVER ITS WRITER DIES (C08r review).
/// aterm serves the bytes a peer left before its end as a request even with no newline
/// after them, so a writer killed inside a line would have aterm take that line's prefix
/// as a whole `deliver`. Here a batch longer than an empty socket takes at once goes to a
/// peer that answers nothing, on a nonblocking socket: where a write would wait for the
/// peer it fails instead, and the writer is dropped there, as a kill there leaves it. What
/// the peer reads to its end is whole lines, the batch's first ones.
#[test]
fn a_pipelined_batch_leaves_whole_lines_wherever_its_writer_dies() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    near.set_nonblocking(true).expect("a nonblocking writer");
    let lines = a_burst_of_deliveries();
    let batch: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut ctl = Ctl::from_stream(near).expect("client");
    let replies = ctl.pipeline(&batch, |_| {});
    assert!(replies.iter().all(Result::is_err), "nobody answered");
    assert!(ctl.lost(), "the failure latches");
    drop(ctl);
    let mut left = Vec::new();
    BufReader::new(far)
        .read_to_end(&mut left)
        .expect("the peer reads to the writer's end");
    let text = String::from_utf8(left).expect("UTF-8");
    assert!(
        text.ends_with('\n'),
        "the peer was left a line cut at {} bytes: ...{}",
        text.rsplit('\n').next().map_or(0, str::len),
        &text[text.len().saturating_sub(60)..]
    );
    let served: Vec<&str> = text.lines().collect();
    assert!(!served.is_empty(), "the first write went whole");
    assert_eq!(served, batch[..served.len()], "the batch's first lines");
}

/// A batch of several writes is answered line by line, in order, by a peer that answers
/// each line as it reads it.
#[test]
fn a_pipelined_batch_of_several_writes_is_answered_in_order() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        let mut seen = 0usize;
        while reader.read_line(&mut line).expect("read") > 0 {
            writer
                .write_all(format!("OK {seen}\n").as_bytes())
                .expect("write");
            seen += 1;
            line.clear();
        }
        seen
    });
    let lines = a_burst_of_deliveries();
    let batch: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut ctl = Ctl::from_stream(near).expect("client");
    let mut read = Vec::new();
    let headers: Vec<String> = ctl
        .pipeline(&batch, |i| read.push(i))
        .into_iter()
        .map(|r| r.expect("answered").header().to_string())
        .collect();
    let want: Vec<String> = (0..batch.len()).map(|i| format!("OK {i}")).collect();
    assert_eq!(headers, want, "each reply is its own line's");
    assert_eq!(read, (0..batch.len()).collect::<Vec<_>>());
    drop(ctl);
    assert_eq!(server.join().expect("server thread"), batch.len());
}

/// Each write of a batch is the most whole lines an empty socket takes at once, and a line
/// longer than that is a write of its own.
#[test]
fn a_pipelined_write_is_the_most_whole_lines_an_empty_socket_takes() {
    let lines = a_burst_of_deliveries();
    let batch: Vec<&str> = lines.iter().map(String::as_str).collect();
    let per_write = PIPELINE_WRITE_MAX / (batch[0].len() + 1);
    assert_eq!(write_end(&batch, 0), per_write);
    assert_eq!(write_end(&batch, per_write), batch.len());
    let long = "x".repeat(PIPELINE_WRITE_MAX);
    assert_eq!(write_end(&[&long, "a", "b"], 0), 1, "alone");
    assert_eq!(write_end(&[&long, "a", "b"], 1), 3);
}

/// The first I/O failure latches the connection as lost.
#[test]
fn an_io_failure_latches_the_connection_as_lost() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    drop(far);
    let mut ctl = Ctl::from_stream(near).expect("client");
    assert!(!ctl.lost(), "a fresh connection is not lost");
    assert!(ctl.request("sessions").is_err(), "the peer is gone");
    assert!(ctl.lost(), "the failure latches");
    let again = ctl.request("sessions").expect_err("a lost lane refuses");
    assert_eq!(again.kind(), io::ErrorKind::BrokenPipe, "{again}");
}

/// A lane the bridge shut fails its next request as a broken pipe and latches as lost, on
/// a socket and on the simulation's lane alike (the lost-aterm fault shuts the lane so).
#[test]
fn the_simulated_lane_shuts_as_a_socket_does() {
    use crate::sim::endpoint::{Endpoint, EpRules};
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        while reader.read_line(&mut line).expect("read") > 0 {
            writer.write_all(b"OK\n").expect("write");
            line.clear();
        }
    });
    let ep = Endpoint::new(EpRules::default(), false);
    for mut ctl in [
        Ctl::from_stream(near).expect("client"),
        Ctl::simulated(ep.lane()),
    ] {
        assert!(ctl.request("hello build=x").expect("answered").ok());
        ctl.shutdown(std::net::Shutdown::Both).expect("shut");
        let shut = ctl.request("hello build=x").expect_err("a shut lane");
        assert_eq!(shut.kind(), io::ErrorKind::BrokenPipe, "{shut}");
        assert!(ctl.lost(), "the failure latches");
    }
    server.join().expect("server thread");
}

/// A read past aterm's reply on the simulation's lane panics, where aterm's socket would
/// block it for good: it never reads as the lane's end, which a framing bug would ride.
#[test]
#[should_panic(expected = "the bridge read past aterm's reply")]
fn a_read_past_the_reply_on_the_simulated_lane_panics() {
    use crate::sim::endpoint::{Endpoint, EpRules};
    let ep = Endpoint::new(EpRules::default(), false);
    let mut lane = ep.lane();
    lane.write_all(b"hello build=x\n").expect("asked");
    let mut reply = Vec::new();
    let _ = lane.read_to_end(&mut reply);
}

/// An `ERR` is one line whatever the verb's framing is.
#[test]
fn an_error_reply_is_one_line_even_for_a_framed_verb() {
    let (near, far) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        while reader.read_line(&mut line).expect("read") > 0 {
            writer.write_all(b"ERR quota\n").expect("write");
            writer.flush().expect("flush");
            line.clear();
        }
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    for line in ["inbox", "outbox", "deliver s-a off=1 from=h-a kind=task"] {
        let reply = ctl.request(line).expect("reply");
        assert!(!reply.ok(), "{line}");
        assert_eq!(reply.header(), "ERR quota");
    }
    drop(ctl);
    server.join().expect("server thread");
    assert!(Reply::Status("OK".into()).ok());
    assert!(Reply::Status("OK 0".into()).ok());
    assert!(!Reply::Status("OKAY".into()).ok());
    assert!(!Reply::Status("OK_NO".into()).ok());
    for bad_token in ["", "tok en", "tok\nen", "tok\r"] {
        let (near, _far) = UnixStream::pair().expect("socketpair");
        let err = Ctl::authenticate(near, bad_token)
            .err()
            .expect("invalid token rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad_token:?}");
    }
    let (near, far) = UnixStream::pair().expect("socketpair");
    let truncated = std::thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().expect("clone"));
        let mut writer = far;
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read") > 0 {
            writer.write_all(b"OK without newline").expect("write");
            writer.flush().expect("flush");
        }
    });
    let mut ctl = Ctl::from_stream(near).expect("client");
    assert_eq!(
        ctl.request("version")
            .expect_err("truncated status line")
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
    drop(ctl);
    truncated.join().expect("server thread");
}

#[test]
fn unsigned_parsers_reject_signed_and_padded_numbers() {
    for bad in ["OK +12", "OK  12", "OK \t12"] {
        assert!(count_after_ok(bad, REPLY_ROWS_MAX).is_err(), "{bad}");
    }
    assert!(crate::notify_need::Pair::parse("failure/+3/5").is_none());
    assert!(crate::notify_need::Pair::parse("failure/3/+5").is_none());
    assert!(crate::notify_need::Pair::parse("failure/3/~+5").is_none());
    assert!(crate::mirror::parse_take("OK 0 slots=+1", b"").is_err());
    assert!(
        crate::mirror::Pin::parse("logpin v=2 inc=+1 n=0 total=0\n").is_err(),
        "signed inc"
    );
    assert!(
        crate::mirror::Pin::parse("logpin v=2 inc=1 n=0 total=0 win=+1:10\n").is_err(),
        "signed win hi"
    );
    assert!(crate::state::Deadline::parse("off=+1 sid=s-a at=1 dl=1").is_none());
    assert!(crate::state::Asked::parse("+1 s-a").is_none());
    assert!(crate::fabric::GivenUpRange::parse("@+1..@2", None).is_none());
    assert!(crate::fabric::GivenUpRange::parse("@1..@+2", None).is_none());
    assert_eq!(
        crate::fabric::parse_launchctl("+42\t0\tcom.example\n", 42),
        None
    );
    assert!(crate::fabric_brief::BriefOpts::parse(&["--window".into(), "+10".into()]).is_err());
    assert!(crate::fabric_brief::BriefOpts::parse(&["--budget".into(), "+1000".into()]).is_err());
    let drain = crate::bridge::parse_drain(
        b"receipt sid=s-a rid=+1 off=1 verdict=handled kind=task from=s-b\n\
          fetch sid=s-a off=+2\n\
          sid=s-a id=+1 to=s-b kind=task len=1\nx",
    );
    assert!(drain.receipts.is_empty());
    assert!(drain.fetches.is_empty());
    assert!(drain.posts.is_empty());
}
