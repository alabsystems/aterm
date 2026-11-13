// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn request_heads_are_bounded_before_their_newline() {
    let prefix = "GET /lane.mjs HTTP/1.1\r\nX: ";
    let head = format!(
        "{prefix}{}\r\n\r\n",
        "x".repeat(MAX_HEAD - prefix.len() - 4)
    );
    assert_eq!(head.len(), MAX_HEAD);
    let bounded = |input| BufReader::new(Read::take(input, MAX_HEAD as u64 + 1));
    assert_eq!(request_head(&mut bounded(head.as_bytes())).unwrap(), head);
    let oversized = "x".repeat(MAX_HEAD * 2);
    let mut input = std::io::Cursor::new(oversized);
    assert_eq!(
        request_head(&mut BufReader::new((&mut input).take(MAX_HEAD as u64 + 1)))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(input.position(), MAX_HEAD as u64 + 1);
}

#[test]
fn a_post_body_is_its_content_length_and_no_more() {
    let body = "x".repeat(20_000);
    let request = format!(
        "POST /answer/gpu HTTP/1.1\r\ncontent-length: {}\r\n\r\n{body}trailing",
        body.len()
    );
    let mut reader = BufReader::new(Read::take(request.as_bytes(), MAX_HEAD as u64 + 1));
    let head = request_head(&mut reader).unwrap();
    assert_eq!(content_length(&head), Some(body.len()));
    assert_eq!(request_body(&head, &mut reader), Ok(body.into_bytes()));
    let mut rest = String::new();
    reader.get_mut().set_limit(u64::MAX);
    reader.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "trailing");
    let refused = |head: &str| {
        let mut reader = BufReader::new(Read::take(&b""[..], 0));
        request_body(head, &mut reader)
    };
    assert_eq!(
        refused("POST /answer/gpu HTTP/1.1\r\n\r\n"),
        Err("411 Length Required")
    );
    assert_eq!(
        refused(&format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_ANSWER + 1
        )),
        Err("413 Content Too Large")
    );
    assert_eq!(
        refused("POST / HTTP/1.1\r\nContent-Length: 4\r\n\r\n"),
        Err("400 Bad Request")
    );
    assert_eq!(answer_kind("/answer/worker"), Some("worker"));
    for bad in [
        "/answer/",
        "/answer/../x",
        "/answer/GPU",
        "/answers/gpu",
        "/cpu.html",
    ] {
        assert_eq!(answer_kind(bad), None, "{bad}");
    }
}

#[test]
fn only_plain_paths_under_the_root_resolve() {
    let root = Path::new("/site");
    assert_eq!(
        resolve(root, "/gpu/aterm_gpu_web.js?v=1#x"),
        Some(PathBuf::from("/site/gpu/aterm_gpu_web.js"))
    );
    assert_eq!(
        resolve(root, "/q/quote%27%60%24%7Bx%7D%23%3F%25_bg.wasm?copy=1"),
        Some(PathBuf::from("/site/q/quote'`${x}#?%_bg.wasm"))
    );
    for bad in [
        "/",
        "",
        "gpu.html",
        "/../etc/passwd",
        "/a/../../b",
        "/%2e%2e/x",
        "/%2E/x",
        "/a%2Fb",
        "/a%5Cb",
        "/a%00b",
        "/a%zzb",
        "/a%2",
        "/a%ff",
        "/a//b",
        "/a\\b",
        "/./a",
    ] {
        assert_eq!(resolve(root, bad), None, "{bad}");
    }
}

#[test]
fn modules_and_wasm_get_the_types_a_page_needs() {
    assert_eq!(
        content_type(Path::new("a.mjs")),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        content_type(Path::new("a.js")),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(content_type(Path::new("a_bg.wasm")), "application/wasm");
    assert_eq!(
        content_type(Path::new("cpu.html")),
        "text/html; charset=utf-8"
    );
    assert_eq!(content_type(Path::new("x")), "application/octet-stream");
}

fn request(origin: &str, head: &str) -> String {
    let addr = origin.trim_start_matches("http://");
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
        .expect("timeout");
    s.set_write_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
        .expect("write timeout");
    s.write_all(head.as_bytes()).expect("send");
    s.shutdown(std::net::Shutdown::Write)
        .expect("finish request");
    let mut reader = BufReader::new(s);
    let mut out = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).expect("response header"), 0);
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().expect("content length"));
        }
        out.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let mut body = vec![0; length.expect("counted response")];
    reader.read_exact(&mut body).expect("response body");
    out.push_str(std::str::from_utf8(&body).expect("text fixture"));
    out
}

fn get(origin: &str, target: &str) -> String {
    request(
        origin,
        &format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
}

#[test]
fn the_server_answers_files_and_refuses_the_rest() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-serve-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    std::fs::write(dir.path().join("lane.mjs"), "export {}\n").expect("write");
    let server = Server::start(dir.path()).expect("start");
    let ok = get(&server.origin(), "/lane.mjs");
    assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
    assert!(
        ok.contains("Content-Type: text/javascript; charset=utf-8\r\n"),
        "{ok}"
    );
    assert!(ok.ends_with("\r\n\r\nexport {}\n"), "{ok}");
    assert!(
        !ok.contains("Cross-Origin-"),
        "the lane's pages stay as they were: {ok}"
    );
    let isolated = Server::start_isolated(dir.path()).expect("start");
    let iso = get(&isolated.origin(), "/lane.mjs");
    assert!(
        iso.contains(ISOLATED) && iso.ends_with("\r\n\r\nexport {}\n"),
        "{iso}"
    );
    assert!(get(&server.origin(), "/missing.js").starts_with("HTTP/1.1 404"));
    assert!(get(&server.origin(), "/../lane.mjs").starts_with("HTTP/1.1 404"));
    let oversized = |prefix: &str| format!("{prefix}{}", "x".repeat(MAX_HEAD + 1 - prefix.len()));
    for invalid in [
        oversized("GET /lane.mjs "),
        oversized("GET /lane.mjs HTTP/1.1\r\nX: "),
        "GET /lane.mjs HTTP/1.1\r\nHost: localhost\r\n".to_string(),
    ] {
        let refused = request(&server.origin(), &invalid);
        assert!(
            refused.starts_with("HTTP/1.1 400"),
            "bad request admitted: {refused}"
        );
    }
    assert!(get(&server.origin(), "/lane.mjs").ends_with("\r\n\r\nexport {}\n"));
    let post = |kind: &str, body: &str| {
        request(
            &server.origin(),
            &format!(
                "POST /answer/{kind} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    assert!(post("gpu", "{\"g\":1}").starts_with("HTTP/1.1 204"));
    assert!(post("cpu", "{\"c\":1}").starts_with("HTTP/1.1 204"));
    assert!(post("Cpu", "{}").starts_with("HTTP/1.1 405"));
    let mut server = server;
    assert_eq!(
        server.answer("cpu", || true).as_deref(),
        Ok(&b"{\"c\":1}"[..])
    );
    assert_eq!(
        server.answer("gpu", || true).as_deref(),
        Ok(&b"{\"g\":1}"[..])
    );
    assert_eq!(
        server.answer("worker", || false),
        Err("the browser ended before the worker page answered".to_string())
    );
    assert!(server.served() >= 1);
    drop(server);
}

/// A WAIT IS HUNG A MINUTE AFTER ITS START OR ITS PAGE'S LAST BEAT, WHICHEVER IS LATER: a beat
/// after the start moves the end out; one from before it (an earlier page's) does not, and
/// the words say which minute ran out.
#[test]
fn a_wait_ends_a_minute_after_its_start_or_its_last_beat() {
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);
    let what = "answer from the gpu page";
    assert_eq!(hang_at(t0, None), at(60));
    assert_eq!(hang_at(t0, Some(at(45))), at(105));

    assert_eq!(wait_step(t0, None, t0, what), Ok(BEAT_POLL));
    assert_eq!(
        wait_step(t0, None, at(60) - Duration::from_millis(50), what),
        Ok(Duration::from_millis(50)),
        "the last look waits only what is left"
    );
    let unbeaten = Err(hang_words(what, false));
    assert_eq!(wait_step(t0, None, at(60), what), unbeaten);

    let beat = Some(at(45));
    assert_eq!(wait_step(t0, beat, at(100), what), Ok(BEAT_POLL));
    let beaten = wait_step(t0, beat, at(105), what);
    assert_eq!(beaten, Err(hang_words(what, true)));
    assert_ne!(beaten, unbeaten, "the words say a beat came");

    let before = t0
        .checked_sub(Duration::from_secs(5))
        .expect("a clock five seconds old");
    assert_eq!(
        wait_step(t0, Some(before), at(60), what),
        unbeaten,
        "NEGATIVE CONTROL: an earlier page's beat moves nothing"
    );
}

/// A PAGE'S BEAT IS HEARD AND NOT SERVED: `POST /beat` answers 204 and moves the beat clock,
/// and is not counted among the requests served; a GET of it is no file, and a POST anywhere
/// else moves nothing.
#[test]
fn a_beat_moves_the_clock_and_is_not_a_request_served() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-beat-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let server = Server::start(dir.path()).expect("start");
    assert_eq!(server.beats().last(), None, "CONTROL: no beat yet");
    let post = |target: &str| {
        request(
            &server.origin(),
            &format!("POST {target} HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
        )
    };
    let before = Instant::now();
    assert!(post(BEAT).starts_with("HTTP/1.1 204"));
    let first = server.beats().last().expect("the beat");
    assert!(first >= before);
    assert!(post(BEAT).starts_with("HTTP/1.1 204"));
    assert!(server.beats().last().expect("the beat") > first);
    assert_eq!(server.served(), 0, "a beat is not a request served");
    let last = server.beats().last();
    assert!(
        post("/beats").starts_with("HTTP/1.1 405"),
        "NEGATIVE CONTROL"
    );
    assert!(get(&server.origin(), BEAT).starts_with("HTTP/1.1 404"));
    assert_eq!(server.beats().last(), last, "another request moves nothing");
    assert_eq!(percent_decoded("a%20b"), Some("a b".to_string()));
    assert_eq!(percent_decoded("a%+ab"), None);
}
