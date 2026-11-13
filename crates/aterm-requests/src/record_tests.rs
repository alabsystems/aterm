// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::why::{NONE_GIVEN, why_of};

const ID: &str = "r-20261109-1411-87be5228-01";
const SID: &str = "s-0123456789abcdef0123";
const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";

fn rec(body: Body) -> Rec {
    Rec {
        t_ms: 1_794_233_460_000,
        id: ID.to_owned(),
        body,
    }
}

fn open(host: &str, account: Option<&str>, program: Option<&str>, model: Option<&str>) -> Open {
    Open {
        via: Via::FleetWorker,
        form: Form::Agent("claude".to_owned()),
        node: "87be5228c0ffee0087be5228c0ffee00".to_owned(),
        host: host.to_owned(),
        by: BY.to_owned(),
        program: program.map(str::to_owned),
        model: model.map(str::to_owned),
        account: account.map(str::to_owned),
        why_src: WhySrc::Given,
    }
}

#[test]
fn every_record_round_trips() {
    let recs = [
        rec(Body::Open(open(
            "m114 (Andrew's Mac)",
            Some("a…@example.com"),
            Some("claude"),
            Some("opus-5.5"),
        ))),
        rec(Body::Open(Open {
            via: Via::CtlSpawn,
            form: Form::Shell,
            node: String::new(),
            host: String::new(),
            by: SID.to_owned(),
            program: None,
            model: None,
            account: None,
            why_src: WhySrc::None,
        })),
        rec(Body::Why {
            text: "Run trust-ir qualification: m114 lacks memory, 100% sure".to_owned(),
        }),
        rec(Body::Brief(Brief::of(b"do the thing\nthen stop"))),
        rec(Body::Tab {
            tab: SID.to_owned(),
            by: BY.to_owned(),
        }),
        rec(Body::Attach {
            tab: SID.to_owned(),
            verb: Verb::Turn,
            n: 2,
            by: BY.to_owned(),
        }),
        rec(Body::Left {
            tab: SID.to_owned(),
            reason: "shell-exit".to_owned(),
            code: Some(-1),
            task: Some(TaskEnd::Working),
            out: Out::Abandoned,
        }),
        rec(Body::Left {
            tab: SID.to_owned(),
            reason: "ui-close".to_owned(),
            code: None,
            task: None,
            out: Out::Abandoned,
        }),
        rec(Body::End {
            out: Out::Failed,
            why: EndWhy::SpawnRefused,
        }),
    ];
    for rec in recs {
        let line = rec.line();
        assert!(line.len() <= MAX_RECORD_BYTES, "{line}");
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(Rec::parse(&line), Some(rec), "{line}");
    }
    let saids = [
        SaidBody::Why {
            text: "Qualify AY entry replay".to_owned(),
            src: WhySrc::Goal,
        },
        SaidBody::Goal {
            text: "Qualify AY entry replay on m72".to_owned(),
        },
        SaidBody::Asked {
            text: "run the \"native\" build".to_owned(),
        },
    ];
    for body in saids {
        let said = Said {
            t_ms: 7,
            id: ID.to_owned(),
            body,
        };
        let line = said.line();
        assert!(line.len() <= MAX_RECORD_BYTES, "{line}");
        assert_eq!(Said::parse(&line), Some(said), "{line}");
    }
    // A record's line is one tier's: no content op parses as a journal record, or the reverse.
    assert!(Rec::parse(&format!("t=1 op=goal req={ID} text=x")).is_none());
    assert!(Said::parse(&format!("t=1 op=tab req={ID} tab={SID} by={BY}")).is_none());
}

#[test]
fn a_maximal_open_record_fits_the_log_line_cap() {
    use aterm_types::control_verbs::{REQUESTER_ACCOUNT_BYTES, REQUESTER_TOKEN_MAX};
    // Every byte of the host and account takes three encoded; each is cut to its cap first.
    let host = aterm_slog::field::clamp_encoded(&"é".repeat(100), HOST_BYTES).to_owned();
    let account =
        aterm_slog::field::clamp_encoded(&"é".repeat(100), REQUESTER_ACCOUNT_BYTES).to_owned();
    let word = "w".repeat(REQUESTER_TOKEN_MAX);
    let mut longest = open(&host, Some(&account), Some(&word), Some(&word));
    longest.via = Via::FleetWorker;
    longest.form = Form::Agent("a".repeat(16));
    longest.by = "fleet:87be5228:s-0123456789abcdef0123".to_owned();
    longest.node = "f".repeat(NODE_BYTES);
    assert!(longest.by.len() <= TOKEN_MAX);
    longest.why_src = WhySrc::Asked;
    let rec = Rec {
        t_ms: u64::MAX,
        id: format!("r-20261109-1411-87be5228-{}", u32::MAX),
        body: Body::Open(longest),
    };
    let line = rec.line();
    assert!(
        line.len() <= MAX_RECORD_BYTES,
        "{} bytes: {line}",
        line.len()
    );
    assert_eq!(Rec::parse(&line), Some(rec));
    // The longest why, and the longest content line, fit too.
    let why = rec_why("%".repeat(aterm_types::control_verbs::REQUEST_WHY_BYTES / 3));
    assert!(why.line().len() <= MAX_RECORD_BYTES);
    assert_eq!(Rec::parse(&why.line()), Some(why));
}

fn rec_why(text: String) -> Rec {
    Rec {
        t_ms: u64::MAX,
        id: format!("r-20261109-1411-87be5228-{}", u32::MAX),
        body: Body::Why { text },
    }
}

#[test]
fn a_torn_foreign_or_overlong_line_is_skipped_and_counted() {
    let whole = rec(Body::Tab {
        tab: SID.to_owned(),
        by: BY.to_owned(),
    })
    .line();
    // Control: the whole line parses.
    assert!(Rec::parse(&whole).is_some());
    let torn = &whole[..whole.len() - 3];
    let overlong = format!("t=1 op=why req={ID} text={}", "x".repeat(MAX_RECORD_BYTES));
    for bad in [
        torn.to_owned(),
        String::new(),
        "seq=1 op=open node=x kind=tab".to_owned(),
        format!("t=1 op=nope req={ID}"),
        format!("t=1 op=tab req=r-1 tab={SID} by={BY}"),
        format!("t=01 op=tab req={ID} tab={SID} by={BY}"),
        format!("t=1 op=tab req={ID} by={BY} tab={SID}"),
        format!("t=1 op=tab req={ID} tab={SID} by={BY} extra=1"),
        format!("t=1 op=tab req={ID} tab={SID}  by={BY}"),
        format!("t=1 op=why req={ID} text=a b"),
        format!("t=1 op=why req={ID} text=%41"),
        format!("t=1 op=attach req={ID} tab={SID} verb=turn n=0 by={BY}"),
        format!("t=1 op=attach req={ID} tab={SID} verb=paste n=1 by={BY}"),
        format!("t=1 op=brief req={ID} sha256=ABC bytes=1"),
        overlong,
    ] {
        assert!(Rec::parse(&bad).is_none(), "{bad}");
    }
    // A load skips and counts each, and folds the rest.
    let dir = aterm_tempfile::tempdir().unwrap();
    let opened = rec(Body::Open(open("m114", None, None, None))).line();
    std::fs::write(
        dir.path().join(crate::JOURNAL),
        format!("{opened}\nnot a record\n{whole}\nt=1 op=end req={ID} out=done\n{torn}"),
    )
    .unwrap();
    let store = crate::store::Store::open(dir.path()).unwrap();
    assert_eq!(store.skipped(), 2, "the foreign line and the short end");
    let request = store.get(ID).expect("the request folded");
    assert_eq!(request.tabs.len(), 1);
    assert_eq!(request.state(), crate::State::Running);
}

#[test]
fn the_why_falls_back_in_the_documented_order_and_says_so() {
    let given = Some("rebuild the cache");
    let brief = "\n   \nQualify \"AY\" entry\u{200b} replay\nsecond line".as_bytes();
    let (goal, asked) = (
        Some("ship the requests store"),
        Some("what are these tabs?"),
    );
    // `why=` first, whatever else there is.
    for via in [Via::FleetShell, Via::FleetWorker, Via::CtlSpawn] {
        let why = why_of(via, given, Some(brief), goal, asked);
        assert_eq!(
            (why.text.as_str(), why.src),
            ("rebuild the cache", WhySrc::Given)
        );
    }
    // A fleet worker: its brief's first line that is not blank, its quote and hidden character
    // made safe; never the goal or asked of a tab on this Mac.
    let why = why_of(Via::FleetWorker, None, Some(brief), goal, asked);
    assert_eq!(
        (why.text.as_str(), why.src),
        ("Qualify 'AY' entry replay", WhySrc::Brief)
    );
    let why = why_of(Via::FleetWorker, None, None, goal, asked);
    assert_eq!((why.text.as_str(), why.src), (NONE_GIVEN, WhySrc::None));
    // A local spawn: the goal, then the asked line; never a brief.
    let why = why_of(Via::CtlSpawn, None, Some(brief), goal, asked);
    assert_eq!(
        (why.text.as_str(), why.src),
        ("ship the requests store", WhySrc::Goal)
    );
    let why = why_of(Via::CtlSpawn, None, Some(brief), Some(" \n "), asked);
    assert_eq!(
        (why.text.as_str(), why.src),
        ("what are these tabs?", WhySrc::Asked)
    );
    let why = why_of(Via::CtlSpawn, None, Some(brief), None, None);
    assert_eq!((why.text.as_str(), why.src), (NONE_GIVEN, WhySrc::None));
    // A long fallback is cut to 200 characters and to 256 encoded bytes.
    let long = "word ".repeat(100);
    let why = why_of(Via::CtlSpawn, None, None, Some(&long), None);
    assert!(why.text.chars().count() <= aterm_types::control_verbs::REQUEST_WHY_MAX);
    assert!(
        aterm_codec::pct::encoded_len(&why.text) <= aterm_types::control_verbs::REQUEST_WHY_BYTES
    );
    assert!(long.starts_with(&why.text));
}

#[test]
fn keep_and_the_new_ends_round_trip_and_unknown_words_are_refused() {
    let mut recs = vec![
        rec(Body::Keep {
            tab: SID.to_owned(),
            by: "human".to_owned(),
        }),
        rec(Body::Left {
            tab: SID.to_owned(),
            reason: "relaunch".to_owned(),
            code: None,
            task: None,
            out: Out::Abandoned,
        }),
    ];
    for why in [
        EndWhy::Idle,
        EndWhy::RequesterLeft,
        EndWhy::Relaunch,
        EndWhy::Kept,
    ] {
        for out in [Out::Released, Out::Abandoned] {
            recs.push(rec(Body::End { out, why }));
        }
    }
    for rec in recs {
        let line = rec.line();
        assert_eq!(Rec::parse(&line), Some(rec), "{line}");
    }
    assert!(
        Rec::parse(&format!("t=1 op=end req={ID} out=released why=kept")).is_some(),
        "CONTROL"
    );
    for bad in [
        format!("t=1 op=end req={ID} out=released why=idled"),
        format!("t=1 op=end req={ID} out=let-go why=kept"),
        format!("t=1 op=keep req={ID} tab={SID}"),
        format!("t=1 op=keep req={ID} tab={SID} by=human extra=1"),
        format!("t=1 op=keep req={ID} tab=s-1 by=human"),
    ] {
        assert!(Rec::parse(&bad).is_none(), "{bad}");
    }
}

#[cfg(unix)]
#[test]
fn root_claim_touched_spent_lines_round_trip() {
    use crate::claim::{Claim, ClaimSrc, TouchWhy};
    let recs = [
        rec(Body::Root {
            path: "/Users//a/trust ir".to_owned(),
            tab: SID.to_owned(),
        }),
        rec(Body::Claim(Claim {
            path: "/Users//a/trust-ir-m72".to_owned(),
            src: ClaimSrc::Born,
            by: BY.to_owned(),
            tab: Some(SID.to_owned()),
            born_ms: Some(1_794_233_461_000),
            alone: Some(true),
        })),
        rec(Body::Claim(Claim {
            path: "/tmp/x".to_owned(),
            src: ClaimSrc::Named,
            by: BY.to_owned(),
            tab: None,
            born_ms: None,
            alone: None,
        })),
        rec(Body::Claim(Claim {
            path: "/Users//a/w".to_owned(),
            src: ClaimSrc::Cwd,
            by: SID.to_owned(),
            tab: Some(SID.to_owned()),
            born_ms: Some(0),
            alone: Some(false),
        })),
        rec(Body::Touched {
            path: "/Users//a/trust-ir-m72".to_owned(),
            tab: SID.to_owned(),
            blk: 7,
            why: TouchWhy::PersonTab,
        }),
        rec(Body::Spent {
            tab: SID.to_owned(),
            spent: Spent {
                cpu_s: Some(812),
                peak_mb: Some(4096),
                written_mb: None,
            },
        }),
    ];
    for rec in recs {
        let line = rec.line();
        assert_eq!(Rec::parse(&line), Some(rec), "{line}");
    }
    let good = format!(
        "t=1 op=claim req={ID} path=/Users//a/x src=born by={BY} tab={SID} born_ms=5 alone=1"
    );
    assert!(Rec::parse(&good).is_some(), "CONTROL");
    // NEGATIVE: a line cut inside `path=`, a non-canonical encoding, an unknown `src=`,
    // `alone=2`, a non-minted `tab=`, `born_ms=01`, a relative or `..` path, a trailing slash.
    for bad in [
        format!("t=1 op=claim req={ID} path=/Users//a/x src=born by={BY} tab={SID} born_ms=5"),
        format!(
            "t=1 op=claim req={ID} path=/Users//a/%78 src=born by={BY} tab={SID} born_ms=5 alone=1"
        ),
        format!(
            "t=1 op=claim req={ID} path=/Users//a/x src=made by={BY} tab={SID} born_ms=5 alone=1"
        ),
        format!(
            "t=1 op=claim req={ID} path=/Users//a/x src=born by={BY} tab={SID} born_ms=5 alone=2"
        ),
        format!("t=1 op=claim req={ID} path=/Users//a/x src=born by={BY} tab=s-1 born_ms=5 alone=1"),
        format!(
            "t=1 op=claim req={ID} path=/Users//a/x src=born by={BY} tab={SID} born_ms=01 alone=1"
        ),
        format!("t=1 op=claim req={ID} path=Users/a src=born by={BY} tab=- born_ms=- alone=-"),
        format!("t=1 op=claim req={ID} path=/Users/../a src=born by={BY} tab=- born_ms=- alone=-"),
        format!("t=1 op=root req={ID} path=/Users//a/ tab={SID}"),
        format!("t=1 op=touched req={ID} path=/a/b tab={SID} blk=1 why=looked"),
        format!("t=1 op=spent req={ID} cpu_s=1 peak_mb=- tab={SID}"),
    ] {
        assert!(Rec::parse(&bad).is_none(), "{bad}");
    }
    // NEGATIVE: a `root`, `spent`, `touched` or `claim` row cut anywhere short of its end, as an
    // interrupted append leaves it before the next one completes its newline, is torn: every
    // prefix of a path or a number is a path or a number too, so each of these ends in a word that
    // must be whole.
    let whole = [
        rec(Body::Root {
            path: "/Users//a/projects".to_owned(),
            tab: SID.to_owned(),
        }),
        rec(Body::Spent {
            tab: SID.to_owned(),
            spent: Spent {
                cpu_s: Some(812),
                peak_mb: Some(4096),
                written_mb: Some(1234),
            },
        }),
        rec(Body::Touched {
            path: "/Users//a/projects".to_owned(),
            tab: SID.to_owned(),
            blk: 70,
            why: TouchWhy::PersonTab,
        }),
    ];
    for rec in whole {
        let line = rec.line();
        assert_eq!(Rec::parse(&line).as_ref(), Some(&rec), "CONTROL: {line}");
        for cut in 1..line.len() {
            let torn = &line[..cut];
            assert!(Rec::parse(torn).is_none(), "{torn}");
        }
    }
}

#[cfg(unix)]
#[test]
fn the_longest_claim_line_fits_the_record_cap() {
    use crate::claim::{CLAIM_PATH_BYTES, Claim, ClaimSrc};
    let longest_id = format!("r-20261109-1411-{}-{}", "a".repeat(8), u32::MAX);
    assert!(request_id(&longest_id).is_ok());
    let by = format!("fleet:{}:{SID}", "b".repeat(8));
    assert!(by.len() <= TOKEN_MAX);
    // `/` and 255 characters, each written as three bytes.
    let path = format!("/{}", "\u{7f}".repeat((CLAIM_PATH_BYTES - 1) / 3));
    let claim = |path: String| Rec {
        t_ms: u64::MAX,
        id: longest_id.clone(),
        body: Body::Claim(Claim {
            path,
            src: ClaimSrc::Named,
            by: by.clone(),
            tab: Some(SID.to_owned()),
            born_ms: Some(u64::MAX),
            alone: Some(true),
        }),
    };
    let line = claim(path.clone()).line();
    assert!(line.len() <= MAX_RECORD_BYTES, "{} bytes", line.len());
    assert_eq!(Rec::parse(&line), Some(claim(path)));
    // NEGATIVE: a path one encoded byte over the cap is refused.
    let over = format!("/{}", "a".repeat(CLAIM_PATH_BYTES));
    assert!(Rec::parse(&claim(over).line()).is_none());
}

#[test]
fn review_kept_reply_lines_round_trip() {
    let recs = [
        rec(Body::Review {
            note: Some("looked: the qualification failed on memory, 100% expected".to_owned()),
            by: "human".to_owned(),
        }),
        rec(Body::Review {
            note: None,
            by: SID.to_owned(),
        }),
        rec(Body::Kept {
            by: "ctl".to_owned(),
        }),
        rec(Body::Reply {
            key: "rq.r-20261109-1411-87be5228-01.000000000000002a".to_owned(),
            sent: true,
            by: "human".to_owned(),
        }),
        rec(Body::Reply {
            key: "k".to_owned(),
            sent: false,
            by: SID.to_owned(),
        }),
    ];
    for rec in recs {
        let line = rec.line();
        assert_eq!(Rec::parse(&line), Some(rec), "{line}");
    }
    let said = Said {
        t_ms: 1_794_233_460_000,
        id: ID.to_owned(),
        body: SaidBody::Reply {
            text: "thanks, cleaning it up now".to_owned(),
            key: "rq.r-1.000000000000002a".to_owned(),
        },
    };
    assert_eq!(Said::parse(&said.line()), Some(said.clone()));
    // NEGATIVE: a row cut inside its note, its text or its key lacks the keys after it.
    let review = rec(Body::Review {
        note: Some("a note".to_owned()),
        by: "human".to_owned(),
    })
    .line();
    let cut = &review[..review.find(" by=").unwrap() - 2];
    assert_eq!(Rec::parse(cut), None, "{cut}");
    let said_line = said.line();
    let cut = &said_line[..said_line.find(" key=").unwrap() - 3];
    assert_eq!(Said::parse(cut), None, "{cut}");
    // NEGATIVE: `sent=` is 0 or 1, a note in its canonical encoding and one visible line.
    let reply = rec(Body::Reply {
        key: "k".to_owned(),
        sent: true,
        by: "human".to_owned(),
    })
    .line();
    assert_eq!(Rec::parse(&reply.replace("sent=1", "sent=2")), None);
    assert_eq!(
        Rec::parse(&review.replace("note=a%20note", "note=a note")),
        None
    );
    assert_eq!(
        Rec::parse(&review.replace("note=a%20note", "note=a%22note")),
        None
    );
    assert_eq!(
        Rec::parse(&review.replace("note=a%20note", "note=a%0Anote")),
        None
    );
    assert_eq!(Rec::parse(&review.replace("by=human", "by=hum")), None);
}

#[test]
fn the_longest_review_and_reply_lines_fit_the_cap() {
    use aterm_types::control_verbs::REQUEST_WHY_BYTES;
    // A note of 200 characters that encode three bytes each, clamped to the record's 256.
    let long = "é".repeat(200);
    let note = crate::why::line_of(&long).unwrap();
    assert!(aterm_codec::pct::encoded_len(&note) <= REQUEST_WHY_BYTES);
    assert!(
        note.chars().count() < 200,
        "clamped, never written past the cap"
    );
    let token = "fleet:87be5228:s-0123456789abcdef0123";
    let review = Rec {
        t_ms: u64::MAX,
        id: "r-20261109-1411-87be5228-4294967295".to_owned(),
        body: Body::Review {
            note: Some(note.clone()),
            by: token.to_owned(),
        },
    }
    .line();
    assert!(review.len() <= MAX_RECORD_BYTES, "{}", review.len());
    assert!(Rec::parse(&review).is_some());
    let said = Said {
        t_ms: u64::MAX,
        id: "r-20261109-1411-87be5228-4294967295".to_owned(),
        body: SaidBody::Reply {
            text: note,
            key: "k".repeat(KEY_MAX),
        },
    }
    .line();
    assert!(said.len() <= MAX_RECORD_BYTES, "{}", said.len());
    assert!(Said::parse(&said).is_some());
    let reply = Rec {
        t_ms: u64::MAX,
        id: "r-20261109-1411-87be5228-4294967295".to_owned(),
        body: Body::Reply {
            key: "k".repeat(KEY_MAX),
            sent: false,
            by: token.to_owned(),
        },
    }
    .line();
    assert!(reply.len() <= MAX_RECORD_BYTES, "{}", reply.len());
    // NEGATIVE: a key past the cap is no record's.
    let over = Rec {
        t_ms: 1,
        id: ID.to_owned(),
        body: Body::Reply {
            key: "k".repeat(KEY_MAX + 1),
            sent: false,
            by: token.to_owned(),
        },
    }
    .line();
    assert_eq!(Rec::parse(&over), None);
}
