// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn the_offline_flags_parse_and_refuse() {
    assert_eq!(parse_offline(&strings(&["max_runs=3"])).unwrap(), None);
    assert_eq!(
        parse_offline(&strings(&[
            "--file",
            "a.cast",
            "rows=4",
            "--screen=s.txt",
            "--until",
            "2650.5",
        ]))
        .unwrap(),
        Some(Offline {
            file: "a.cast".to_string(),
            screen: Some("s.txt".to_string()),
            until: Some(2650.5),
            args: strings(&["rows=4"]),
        })
    );
    for bad in [
        &["--screen", "s.txt"][..],
        &["--file"][..],
        &["--file", "a", "--until", "soon"][..],
        &["--file", "a", "--until", "+1.0"][..],
        &["--file", "a", "--until", "-1.0"][..],
        &["--file", "a", "--until", "NaN"][..],
        &["--file", "a", "--until", "inf"][..],
        &["--file", "a", "--json"][..],
    ] {
        assert!(parse_offline(&strings(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn the_drift_request_shapes_are_recognized() {
    assert!(is_cast_drift(&strings(&["cast", "drift"])));
    assert!(!is_cast_drift(&strings(&["cast", "frames"])));
    assert!(!is_cast_drift(&strings(&["cast"])));
    assert!(is_drift_request("cast", "@s-a cast drift rows=2\n"));
    assert!(is_drift_request("cast", "cast drift"));
    assert!(!is_drift_request("cast", "cast frames"));
    assert_eq!(
        stamp("OK schema=1 agent_gen=4.5 gen=4.9 seq=9 hash=ab"),
        Some(("9".to_string(), "ab".to_string()))
    );
    assert_eq!(stamp("OK nothing"), None);
    for unstamped in [
        "OK schema=1 gen=4.9 seq=- hash=-",
        "OK schema=1 gen=4.9 seq=9 hash=-",
        "OK schema=1 gen=4.9 seq=- hash=ab",
    ] {
        assert_eq!(stamp(unstamped), None, "{unstamped}");
    }
    assert_eq!(parse_cursor("OK 58 2 1 blinking_block"), Some((58, 2)));
    assert_eq!(parse_cursor("OK +58 2 1 blinking_block"), None);
    assert_eq!(parse_cursor("OK 58 +2 1 blinking_block"), None);
    assert_eq!(screen_rows("one\r\ntwo\n"), strings(&["one", "two"]));
    assert_eq!(
        parse_alt(&strings(&["cursor_visible=true", "alt_screen=true"])),
        Some(true)
    );
}

/// [`echo`], refusing a job that does not name the peer the live fallback
/// computes against (an aterm older than the verb and the resize undo).
fn echo_pre_undo(job: &CastDriftJob<'_>) -> Result<String, String> {
    if job.peer == DriftPeer::PreUndo {
        echo(job)
    } else {
        Err(format!("the live fallback said peer={:?}", job.peer))
    }
}

/// [`echo`], refusing a job that does not leave the peer unknown, as a
/// saved recording's is.
fn echo_unknown(job: &CastDriftJob<'_>) -> Result<String, String> {
    if job.peer == DriftPeer::Unknown {
        echo(job)
    } else {
        Err(format!("the offline form said peer={:?}", job.peer))
    }
}

/// A fake analyzer that echoes the material it was handed.
fn echo(job: &CastDriftJob<'_>) -> Result<String, String> {
    let screen = job.screen.map_or(0, <[String]>::len);
    Ok(format!(
        "OK 1 verdict=echo computed=client\ncast_bytes={} screen={screen} cursor={:?} alt={:?} args={} cut={:?} peer={:?}\n",
        job.cast.len(),
        job.cursor,
        job.alt,
        job.args,
        job.cut,
        job.peer
    ))
}

/// Against a mock older server that answers `cast drift` with the bare cast, the client
/// reads the material itself (unmoved stamp: quiescent) and hands it to the analyzer;
/// without an analyzer it refuses by name.
#[cfg(unix)]
#[test]
fn an_older_server_gets_the_report_computed_client_side() {
    use std::io::BufRead as _;
    use std::os::unix::net::UnixListener;
    const CAST: &str = "{\"version\": 2, \"width\": 20, \"height\": 8}\n[0.1, \"o\", \"hi\"]\n";
    let scratch = crate::test_scratch_dir("drift");
    let sock = scratch.path().join("old.sock");
    let listener = UnixListener::bind(&sock).expect("bind");
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..8 {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("request");
            let req = line.trim_end().to_string();
            let verb = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let reply = match verb.as_str() {
                "cast" => format!("OK {}\n{CAST}", CAST.len()),
                "status" => "OK schema=1 gen=1.7 seq=7 hash=abc\n".to_string(),
                "text" => "OK 2\nrow zero\nrow one\n".to_string(),
                "cursor" => "OK 1 3 1 blinking_block\n".to_string(),
                "modes" => "OK 2\nalt_screen=true\ncursor_visible=true\n".to_string(),
                _ => "ERR unknown verb (try: help)\n".to_string(),
            };
            let mut w = conn;
            w.write_all(reply.as_bytes()).expect("reply");
            w.flush().expect("flush");
            seen.push(req);
        }
        seen
    });
    let path = sock.to_str().expect("utf8").to_string();
    let local = LocalVerbs {
        cast_drift: Some(echo_pre_undo),
        ..LocalVerbs::NONE
    };
    let code = live_with(
        &path,
        TargetOrigin::Pinned,
        Some("@s-x"),
        &strings(&["rows=3"]),
        Some(Duration::from_secs(60)),
        &local,
        &|_| {},
    )
    .expect("computed client-side");
    assert_eq!(code, ExitCode::SUCCESS);
    live_with(
        &path,
        TargetOrigin::Pinned,
        Some("@s-x"),
        &[],
        Some(Duration::from_secs(60)),
        &LocalVerbs::NONE,
        &|_| {},
    )
    .expect_err("no analyzer: refused");
    let seen = server.join().expect("server");
    assert_eq!(
        seen,
        [
            "@s-x cast drift rows=3",
            "@s-x status",
            "@s-x text",
            "@s-x cursor",
            "@s-x modes",
            "@s-x cast",
            "@s-x status",
            "@s-x cast drift",
        ]
    );
}

/// The offline form with no analyzer refuses by name, and with a selector is
/// a usage error; a live-form request is not taken.
#[test]
fn the_offline_entry_refuses_what_it_cannot_do() {
    let parts = strings(&["cast", "drift", "--file", "/nonexistent.cast"]);
    offline_entry(&parts, &LocalVerbs::NONE).expect_err("no analyzer");
    let with_sel = strings(&["@s-a", "cast", "drift", "--file", "x"]);
    let local = LocalVerbs {
        cast_drift: Some(echo),
        ..LocalVerbs::NONE
    };
    assert!(offline_entry(&with_sel, &local).is_err());
    let live_form = strings(&["@s-a", "cast", "drift", "rows=2"]);
    assert!(offline_entry(&live_form, &local).unwrap().is_none());
    let missing = offline_entry(&parts, &local).expect_err("unreadable file");
    assert!(
        missing.to_string().contains("/nonexistent.cast"),
        "{missing}"
    );
    let scratch = crate::test_scratch_dir("offline");
    let file = scratch.path().join("saved.cast");
    std::fs::write(&file, "{\"version\": 2, \"width\": 20, \"height\": 8}\n").expect("write");
    let saved = strings(&["cast", "drift", "--file", file.to_str().expect("utf8")]);
    let unknown = LocalVerbs {
        cast_drift: Some(echo_unknown),
        ..LocalVerbs::NONE
    };
    assert_eq!(
        offline_entry(&saved, &unknown).expect("computed offline"),
        Some(ExitCode::SUCCESS)
    );
    let pre_undo = LocalVerbs {
        cast_drift: Some(echo_pre_undo),
        ..LocalVerbs::NONE
    };
    assert_eq!(
        offline_entry(&saved, &pre_undo).expect("refused by the analyzer"),
        Some(ExitCode::FAILURE)
    );
}
