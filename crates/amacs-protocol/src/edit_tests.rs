// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn for_word_byte_is_the_set_it_replaced() {
    for byte in u8::MIN..=u8::MAX {
        let before = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        assert_eq!(for_word_byte(byte), before, "byte {byte:#04x}");
    }
}

const URI: &str = "file:///private/tmp/x%20y/COMMIT_EDITMSG";

#[test]
fn edit_requests_round_trip() {
    for request in [
        EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
            read_only: false,
            secret: None,
        },
        EditRequest {
            uri: URI.to_string(),
            line: Some((3, None)),
            create: false,
            for_word: Some("git".to_string()),
            wait: true,
            read_only: false,
            secret: None,
        },
        EditRequest {
            uri: URI.to_string(),
            line: Some((12, Some(7))),
            create: true,
            for_word: Some("claude-code_2.1".to_string()),
            wait: true,
            read_only: false,
            secret: None,
        },
        EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: Some("git".to_string()),
            wait: false,
            read_only: false,
            secret: None,
        },
        EditRequest {
            uri: URI.to_string(),
            line: Some((1, None)),
            create: false,
            for_word: Some("aterm".to_string()),
            wait: true,
            read_only: true,
            secret: None,
        },
        EditRequest {
            uri: URI.to_string(),
            line: Some((1, None)),
            create: false,
            for_word: Some("aterm-db".to_string()),
            wait: true,
            read_only: true,
            secret: Some(vec!["password".to_string(), "=totp".to_string()]),
        },
        EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: Some("aterm-db".to_string()),
            wait: true,
            read_only: false,
            secret: Some(Vec::new()),
        },
    ] {
        let args = request.format().unwrap();
        assert_eq!(EditRequest::parse(&args).unwrap(), request, "{args}");
        assert_eq!(
            args.contains("wait="),
            !request.wait,
            "only `wait=0` is written: {args}"
        );
        assert_eq!(
            args.contains("ro="),
            request.read_only,
            "only `ro=1` is written: {args}"
        );
        assert_eq!(
            args.contains("secret="),
            request.secret.is_some(),
            "`secret=` is written only when given: {args}"
        );
    }
    assert_eq!(
        EditRequest::parse(&format!("{URI} secret="))
            .unwrap()
            .secret,
        Some(Vec::new()),
        "`secret=` names no column"
    );
    assert!(
        !EditRequest::parse(&format!("{URI} ro=0"))
            .unwrap()
            .read_only,
        "`ro=0` is the default spelled out"
    );
    assert!(
        EditRequest::parse(&format!("{URI} wait=1")).unwrap().wait,
        "`wait=1` is the default spelled out"
    );
    assert_eq!(
        EditRequest::parse(&format!("{URI}   for=git line=2:1 ")).unwrap(),
        EditRequest {
            uri: URI.to_string(),
            line: Some((2, Some(1))),
            create: false,
            for_word: Some("git".to_string()),
            wait: true,
            read_only: false,
            secret: None,
        },
        "options in any order, any spacing"
    );
}

#[test]
fn edit_requests_refuse_a_bad_line() {
    for line in [
        "0",
        "3:0",
        "abc",
        "3:",
        ":4",
        "-1",
        "+3",
        "3:4:5",
        "99999999999",
        "",
    ] {
        assert_eq!(
            EditRequest::parse(&format!("{URI} line={line}")),
            Err(EditError::BadLine(line.to_string())),
            "line={line}"
        );
    }
    let zero = EditRequest {
        uri: URI.to_string(),
        line: Some((0, None)),
        create: false,
        for_word: None,
        wait: true,
        read_only: false,
        secret: None,
    };
    assert!(matches!(zero.format(), Err(EditError::BadLine(_))));
}

#[test]
fn edit_requests_refuse_what_the_form_does_not_have() {
    assert_eq!(EditRequest::parse("  "), Err(EditError::Usage(EDIT_USAGE)));
    assert!(matches!(
        EditRequest::parse("/tmp/x"),
        Err(EditError::BadField("uri", _))
    ));
    for tail in [
        "place=split",
        "line=1 line=2",
        "create=yes",
        "wait=no",
        "wait=0 wait=0",
        "ro=yes",
        "ro=1 ro=1",
        "secret=a,a",
        "secret=a,,b",
        "secret=,",
        "secret=a secret=b",
        "for=a/b",
        "bare",
    ] {
        assert!(
            EditRequest::parse(&format!("{URI} {tail}")).is_err(),
            "{tail}"
        );
    }
    let long = "x".repeat(33);
    assert!(EditRequest::parse(&format!("{URI} for={long}")).is_err());
}

#[test]
fn no_field_is_written_with_whitespace() {
    let spaced = EditRequest {
        uri: "file:///tmp/a b".to_string(),
        line: None,
        create: false,
        for_word: None,
        wait: true,
        read_only: false,
        secret: None,
    };
    assert_eq!(spaced.format(), Err(EditError::Whitespace("uri")));
    let word = EditRequest {
        uri: URI.to_string(),
        line: None,
        create: false,
        for_word: Some("git commit".to_string()),
        wait: true,
        read_only: false,
        secret: None,
    };
    assert_eq!(word.format(), Err(EditError::Whitespace("for")));
    let ok = EditOk {
        ticket: Ticket(1),
        view: 2,
        doc: 3,
        uri: "file:///tmp/a\tb".to_string(),
    };
    assert_eq!(ok.format(), Err(EditError::Whitespace("uri")));
    let wait = AwaitEdit {
        key: AwaitKey::Uri("file:///tmp/a b".to_string()),
        timeout_ms: None,
    };
    assert_eq!(wait.format(), Err(EditError::Whitespace("uri")));
    let empty = EditOk {
        uri: String::new(),
        ..ok
    };
    assert_eq!(empty.format(), Err(EditError::Whitespace("uri")));
}

#[test]
fn edit_replies_round_trip() {
    let ok = EditOk {
        ticket: Ticket(18_446_744_073_709_551_615),
        view: 41,
        doc: 7,
        uri: URI.to_string(),
    };
    let line = ok.format().unwrap();
    assert_eq!(
        line,
        format!("OK edit ticket=18446744073709551615 view=41 doc=7 uri={URI}")
    );
    assert_eq!(EditOk::parse(&format!("{line}\n")).unwrap(), ok);
    assert!(EditOk::parse("OK edit ticket=1 view=2 doc=3").is_err());
    assert!(EditOk::parse("OK edit view=2 ticket=1 doc=3 uri=file:///x").is_err());
    assert!(EditOk::parse("OK edit ticket=1 view=2 doc=3 uri=/tmp/x").is_err());
    assert!(
        EditOk {
            uri: "/tmp/x".to_string(),
            ..ok.clone()
        }
        .format()
        .is_err()
    );
    assert!(EditOk::parse("ERR refused missing").is_err());

    for (result, saved, seq) in [
        (EditResult::Closed, true, 9),
        (EditResult::Finished, true, 0),
        (EditResult::Aborted, false, 3),
    ] {
        let outcome = EditOutcome { result, saved, seq };
        let line = outcome.format();
        assert_eq!(EditOutcome::parse(&line).unwrap(), outcome, "{line}");
    }
    assert_eq!(
        EditOutcome {
            result: EditResult::Closed,
            saved: true,
            seq: 5
        }
        .format(),
        "OK edit done result=closed saved=1 seq=5"
    );
    assert!(EditOutcome::parse("OK edit done result=closed saved=2 seq=5").is_err());
    assert!(EditOutcome::parse("OK edit done result=kept saved=1 seq=5").is_err());
    assert!(EditOutcome::parse("OK timeout").is_err());
}

#[test]
fn await_edit_round_trips() {
    for wait in [
        AwaitEdit {
            key: AwaitKey::Ticket(Ticket(77)),
            timeout_ms: None,
        },
        AwaitEdit {
            key: AwaitKey::Uri(URI.to_string()),
            timeout_ms: Some(600_000),
        },
    ] {
        let args = wait.format().unwrap();
        assert_eq!(AwaitEdit::parse(&args).unwrap(), wait, "{args}");
    }
    for bad in [
        "edit",
        "inbox since=1",
        "edit ticket=x",
        "edit ticket=1 uri=file:///x",
        "edit uri=/tmp/x",
        "edit ticket=1 timeout=2s",
        "edit ticket=1 timeout=+1000",
        "edit ticket=1 timeout=-0",
        "edit ticket=1 later",
    ] {
        assert!(AwaitEdit::parse(bad).is_err(), "{bad}");
    }
}
