// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// EVERY REQUEST READS BACK AS IT WAS WRITTEN: the opener's line is the host's request.
#[test]
fn a_request_reads_back_as_it_was_written() {
    for request in [
        Request::Load {
            id: 1,
            url: "https://claude.ai/login?a=b&c=\"d\"".into(),
            aside: false,
        },
        Request::Eval {
            id: 2,
            script: "JSON.stringify({a: 'b\\n'})".into(),
            aside: false,
        },
        Request::Load {
            id: 4,
            url: "https://claude.ai/magic-link".into(),
            aside: true,
        },
        Request::Eval {
            id: 5,
            script: "document.title".into(),
            aside: true,
        },
        Request::Forget { id: 6 },
        Request::Close { id: 3 },
    ] {
        let line = request_line(&request);
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(parse_request(&line), Ok(request));
    }
    assert_eq!(
        request_line(&Request::Close { id: 7 }),
        r#"{"id":7,"op":"close"}"#
    );
}

/// A LINE THAT IS NO REQUEST IS REFUSED, and says which part is missing.
#[test]
fn a_line_that_is_no_request_is_refused() {
    assert_eq!(parse_request("load"), Err("the line is not JSON"));
    assert_eq!(
        parse_request(r#"{"op":"close"}"#),
        Err("the request has no id")
    );
    assert_eq!(
        parse_request(r#"{"id":-1,"op":"close"}"#),
        Err("the request has no id")
    );
    assert_eq!(
        parse_request(r#"{"id":1,"op":"load"}"#),
        Err("the request lacks a field of its operation")
    );
    assert_eq!(
        parse_request(r#"{"id":1,"op":"click"}"#),
        Err("the request names no operation the window has")
    );
    // A request that does not say which page is the window's own; one that says it in
    // anything but yes or no is refused.
    assert_eq!(
        parse_request(r#"{"id":1,"op":"load","url":"https://claude.ai/"}"#),
        Ok(Request::Load {
            id: 1,
            url: "https://claude.ai/".into(),
            aside: false,
        })
    );
    assert_eq!(
        parse_request(r#"{"id":1,"op":"eval","script":"1","aside":"yes"}"#),
        Err("the request's aside is not yes or no")
    );
}

/// THE HOST'S LINES READ BACK AS REPLIES AND EVENTS: a reply keeps its id, an evaluation's
/// value and address, and a failure's reason; an event keeps its name.
#[test]
fn the_hosts_lines_read_back_as_replies_and_events() {
    assert_eq!(
        parse_line(&reply_done(4)),
        Ok(Line::Reply {
            id: 4,
            answer: Ok((None, None))
        })
    );
    assert_eq!(
        parse_line(&reply_value(
            5,
            Some("{\"a\":1}"),
            "https://claude.ai/login"
        )),
        Ok(Line::Reply {
            id: 5,
            answer: Ok((
                Some("{\"a\":1}".into()),
                Some("https://claude.ai/login".into())
            ))
        })
    );
    assert_eq!(
        parse_line(&reply_value(6, None, "")),
        Ok(Line::Reply {
            id: 6,
            answer: Ok((None, Some(String::new())))
        })
    );
    assert_eq!(
        parse_line(&reply_error(7, "the window is closed")),
        Ok(Line::Reply {
            id: 7,
            answer: Err("the window is closed".into())
        })
    );
    assert_eq!(
        parse_line(&ready(true)),
        Ok(Line::Event {
            name: READY.into(),
            code: None,
            persistent: true
        })
    );
    assert_eq!(
        parse_line(&ready(false)),
        Ok(Line::Event {
            name: READY.into(),
            code: None,
            persistent: false
        })
    );
    assert_eq!(
        parse_line(&load_failed(-1022)),
        Ok(Line::Event {
            name: LOAD_FAILED.into(),
            code: Some(-1022),
            persistent: false
        })
    );
    assert_eq!(
        parse_line(&event(CLOSED)),
        Ok(Line::Event {
            name: CLOSED.into(),
            code: None,
            persistent: false
        })
    );
    assert_eq!(
        parse_line(r#"{"ok":true}"#),
        Err("the line is neither a reply nor an event")
    );
    assert_eq!(
        parse_line(r#"{"id":1}"#),
        Err("the reply does not say whether it was carried out")
    );
}

/// THE FIRST LINE SAYS WHAT TO OPEN and reads back as it was written: the title, the store,
/// the hosts and the loopback port. A line that is no such line is refused, so a host given
/// one opens nothing.
#[test]
fn the_first_line_says_what_to_open() {
    let open = Open {
        title: "aterm \u{b7} signing in Claude \"one\"".into(),
        store: Some("0F9E2C1A-7B3D-8A4E-9C5F-0123456789AB".into()),
        hosts: vec!["claude.ai".into(), "claude.com".into()],
        loopback: Some(4321),
    };
    let line = open_line(&open);
    assert!(!line.contains('\n'), "{line}");
    assert_eq!(parse_open(&line), Ok(open));
    let bare = Open {
        title: "aterm".into(),
        store: None,
        hosts: Vec::new(),
        loopback: None,
    };
    assert_eq!(parse_open(&open_line(&bare)), Ok(bare));
    assert_eq!(
        parse_open(r#"{"id":1,"op":"load","url":"https://claude.ai/"}"#),
        Err("the first line does not say what to open")
    );
    assert_eq!(
        parse_open(r#"{"op":"open","hosts":[]}"#),
        Err("the window has no title")
    );
    assert_eq!(
        parse_open(r#"{"op":"open","title":"t"}"#),
        Err("the window names no hosts its page may go to")
    );
    assert_eq!(
        parse_open(r#"{"op":"open","title":"t","hosts":[7]}"#),
        Err("a host is not a name")
    );
    for port in ["0", "70000", "\"4321\""] {
        assert_eq!(
            parse_open(&format!(
                r#"{{"op":"open","title":"t","hosts":[],"loopback":{port}}}"#
            )),
            Err("the loopback port is not a port"),
            "{port}"
        );
    }
}
