// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn requests_parse_what_they_format() {
    let forms = [
        ("ls", DocRequest::Ls { uri: None }),
        (
            "ls uri=file:///tmp/a%20b.txt",
            DocRequest::Ls {
                uri: Some("file:///tmp/a%20b.txt".to_string()),
            },
        ),
        (
            "read 3",
            DocRequest::Read {
                doc: DocRef::Id(3),
                lines: LineRange::ALL,
            },
        ),
        (
            "read file:///x lines=2-9",
            DocRequest::Read {
                doc: DocRef::Uri("file:///x".to_string()),
                lines: LineRange {
                    first: 2,
                    last: Some(9),
                },
            },
        ),
        (
            "read 4 lines=7-",
            DocRequest::Read {
                doc: DocRef::Id(4),
                lines: LineRange {
                    first: 7,
                    last: None,
                },
            },
        ),
    ];
    for (line, request) in forms {
        assert_eq!(DocRequest::parse(line).as_ref(), Ok(&request), "{line}");
        assert_eq!(request.format().as_deref(), Ok(line), "{line}");
    }
    assert_eq!(
        DocRequest::parse("read doc=3 lines=5"),
        Ok(DocRequest::Read {
            doc: DocRef::Id(3),
            lines: LineRange {
                first: 5,
                last: Some(5)
            },
        })
    );
    assert_eq!(
        DocRequest::parse("read uri=file:///x"),
        DocRequest::parse("read file:///x")
    );
    assert_eq!(
        DocRequest::parse("apply 3 base=7 len=12"),
        Ok(DocRequest::Later(LaterForm::Apply))
    );
    assert_eq!(
        DocRequest::parse("save 3"),
        Ok(DocRequest::Later(LaterForm::Save))
    );
    assert_eq!(
        DocRequest::parse("log 3 since=2"),
        Ok(DocRequest::Later(LaterForm::Log))
    );
}

#[test]
fn malformed_requests_say_why() {
    let refused = [
        ("", "usage: doc ls"),
        ("frob", "usage: doc ls"),
        ("read", "usage: doc ls"),
        ("read 0", "bad doc=0"),
        ("read doc=file:///x", "bad doc=file:///x"),
        ("read uri=7", "bad uri=7"),
        ("read /tmp/x", "bad doc=/tmp/x"),
        ("read 3 lines=0-2", "bad lines=0-2"),
        ("read 3 lines=5-2", "bad lines=5-2"),
        ("read 3 lines=1 lines=2", "unknown option lines=2"),
        ("read 3 near=2", "unknown option near=2"),
        ("ls uri=/tmp/x", "bad uri=/tmp/x"),
        ("ls 3", "unknown option 3"),
        (
            "ls uri=file:///a uri=file:///b",
            "unknown option uri=file:///b",
        ),
    ];
    for (line, why) in refused {
        let error = DocRequest::parse(line).expect_err(line).to_string();
        assert!(error.starts_with(why), "{line}: {error}");
    }
}

#[test]
fn rows_parse_what_they_format() {
    let row = DocRow {
        doc: 3,
        uri: "file:///private/tmp/COMMIT_EDITMSG".to_string(),
        seq: 12,
        dirty: true,
        disk: DiskState::Changed,
        views: vec![7, 9],
        tickets: vec![Ticket(4_294_967_297)],
        cursor: Some(ViewPoint {
            view: 7,
            at: Point { line: 2, col: 11 },
        }),
        sel: Some(ViewSpan {
            view: 7,
            from: Point { line: 2, col: 6 },
            to: Point { line: 2, col: 11 },
        }),
    };
    let line = row.format().unwrap();
    assert_eq!(
        line,
        "doc=3 uri=file:///private/tmp/COMMIT_EDITMSG seq=12 dirty=1 disk=changed \
         views=7,9 ticket=4294967297 cursor=7:2:11 sel=7:2:6-2:11"
    );
    assert_eq!(DocRow::parse(&format!("{line}\n")), Ok(row.clone()));
    let bare = DocRow {
        dirty: false,
        disk: DiskState::Same,
        views: vec![7],
        tickets: Vec::new(),
        cursor: None,
        sel: None,
        ..row
    };
    let line = bare.format().unwrap();
    assert!(line.ends_with("views=7 ticket=- cursor=- sel=-"), "{line}");
    assert_eq!(DocRow::parse(&line), Ok(bare));
    for broken in [
        "doc=3",
        "uri=file:///x doc=3 seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=-",
        "doc=3 uri=file:///x seq=1 dirty=2 disk=same views=1 ticket=- cursor=- sel=-",
        "doc=3 uri=file:///x seq=1 dirty=0 disk=lost views=1 ticket=- cursor=- sel=-",
        "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1, ticket=- cursor=- sel=-",
        "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=1:0:1 sel=-",
        "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=1:1:1",
        "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=- x=1",
    ] {
        assert!(DocRow::parse(broken).is_err(), "{broken}");
    }
    let spaced = DocRow {
        uri: "file:///a b".to_string(),
        ..DocRow::parse(&line).unwrap()
    };
    assert_eq!(spaced.format(), Err(DocError::Whitespace("uri")));
    assert_eq!(
        DocRow::parse("doc=3 uri=/tmp/x seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=-"),
        Err(DocError::BadField("uri", "/tmp/x".to_string()))
    );
    assert_eq!(
        DocRow {
            cursor: Some(ViewPoint {
                view: 1,
                at: Point { line: 0, col: 1 },
            }),
            ..DocRow::parse(&line).unwrap()
        }
        .format(),
        Err(DocError::BadField("cursor", "1:0:1".to_string()))
    );
    assert_eq!(
        DocRow {
            sel: Some(ViewSpan {
                view: 1,
                from: Point { line: 1, col: 1 },
                to: Point { line: 1, col: 0 },
            }),
            ..DocRow::parse(&line).unwrap()
        }
        .format(),
        Err(DocError::BadField("sel", "1:1:1-1:0".to_string()))
    );
    assert_eq!(
        DocRequest::Read {
            doc: DocRef::Id(0),
            lines: LineRange::ALL,
        }
        .format(),
        Err(DocError::BadField("doc", "0".to_string()))
    );
    assert_eq!(
        DocRequest::Read {
            doc: DocRef::Id(1),
            lines: LineRange {
                first: 5,
                last: Some(2),
            },
        }
        .format(),
        Err(DocError::BadLines("5-2".to_string()))
    );
}

#[test]
fn disk_state_by_content_is_its_table() {
    for bits in 0..16_u8 {
        let [now, base, len, content] = [8, 4, 2, 1].map(|bit| bits & bit != 0);
        let expected = match (now, base && len && content) {
            (false, _) => DiskState::Missing,
            (true, true) => DiskState::Same,
            (true, false) => DiskState::Changed,
        };
        assert_eq!(
            DiskState::by_content(now, base, len, content),
            expected,
            "now={now} base={base} len={len} content={content}"
        );
    }
}

#[test]
fn points_follow_the_anchor_rules() {
    let text = "ab\r\nc\u{e9}d\n\ne";
    let at = |offset| Point::at(text, offset).to_string();
    assert_eq!([at(0), at(1), at(2), at(3)], ["1:1", "1:2", "1:3", "1:3"]);
    assert_eq!(
        [at(4), at(5), at(6), at(7), at(8)],
        ["2:1", "2:2", "2:2", "2:3", "2:4"]
    );
    assert_eq!([at(9), at(10), at(11)], ["3:1", "4:1", "4:2"]);
    assert_eq!(at(99), "4:2", "past the end is the end");
    assert_eq!(Point::at("a\nb\n", 4).to_string(), "3:1");
    assert_eq!(Point::at("", 0), Point::START);
    let marked = "\u{feff}x\n";
    assert_eq!(
        [0, 1, 3, 4, 5].map(|offset| Point::at(marked, offset).to_string()),
        ["1:1", "1:1", "1:1", "1:2", "2:1"]
    );
    let lines = Lines::of(text);
    for offset in 0..text.len() {
        let point = Point::at(text, offset);
        if point.line <= lines.count() {
            let span = lines.span(point.line).unwrap();
            assert!(span.start <= offset && offset < span.end.max(span.start + 1));
        }
    }
}
