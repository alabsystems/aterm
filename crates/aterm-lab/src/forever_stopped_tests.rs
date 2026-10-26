// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stopped-first want (§14.7) over `chrome` as a 7a build and as main before it print the
//! menu and the presence line with `cruise-n`'s report and the stand-in ask standing.

use super::super::Window;
use super::*;

const ASK_SID: &str = "s-ask";
const REPORT_SID: &str = "s-cn";

/// `chrome`'s Needs you and presence lines with the ask at `ask_at` and the report at the other
/// row, the report row's `tip`, and `pressing`.
fn chrome(ask_first: bool, tip: Option<&str>, pressing: &str) -> String {
    let ask = format!(
        "statusmenu \"◇ {ASK_TAB} — {} · 1s\" enabled=yes checked=no at=1",
        ASK_WORDS.join(" ")
    );
    let report = format!(
        "statusmenu \"◇ cruise-n — grant Screen Recording to com.aterm.aterm in System Settings · 3s\" enabled=yes checked=no{} at=2\n\
         statusmenu \"a second reviewer for the parser's error paths\" enabled=yes checked=no tip=\"Claude Code in cruise-n needs this too\" indent=1 at=3",
        tip.map_or_else(String::new, |t| format!(" tip={t:?}"))
    );
    let (one, two) = if ask_first {
        (ask, report)
    } else {
        (report, ask)
    };
    format!(
        "statusitem title=❯2 needs=2 stuck=0 exceptions=0 done=0 fleet=off\n\
         statusmenu \"Needs you (2)\" enabled=no at=0\n{one}\n{two}\n\
         statusmenu --- at=4\n\
         statusmenu \"This Mac — 2 need you · 3 working\" enabled=no at=5\n\
         presence rim=decide pressing=@{pressing} pill=\"◇ {ASK_TAB} (tab 4) · approve the stand-in request · 1s\""
    )
}

fn judged(chrome: &str, program: &'static str) -> Verdict {
    let read = Read::of(chrome, "cruise-n");
    let seen = Seen {
        window: Window {
            read: true,
            label: "cruise-n".into(),
            stopped: Some((
                5_000,
                format!("{} cleared=yes", read.line(ASK_SID, REPORT_SID)),
            )),
            ..Window::default()
        },
        ..Seen::default()
    };
    first(
        &seen,
        &Ctx {
            chain: Chain::CruiseN,
            program,
            t0: 1_000,
        },
    )
}

/// A 7a build's read passes on every harness, each in its own program's name; main's before 7a
/// (the older report first, the front tab's report pressed, no tip) fails on each count.
#[test]
fn the_ask_lists_first_presses_and_the_report_carries_on() {
    for (program, product) in [
        ("claude", "Claude Code"),
        ("agy", "Antigravity CLI"),
        ("muse", "Muse Code"),
    ] {
        let tip = report_words(product, false);
        let ours = chrome(true, Some(&tip), ASK_SID);
        let read = Read::of(&ours, "cruise-n");
        assert_eq!((read.ask, read.report), (Some(0), Some(1)), "{read:?}");
        assert_eq!(read.tip.as_deref(), Some(tip.as_str()));
        assert_eq!(read.pressing, format!("@{ASK_SID}"));
        let v = judged(&ours, program);
        assert!(matches!(v, Verdict::Pass(_)), "{program}: {v:?}");

        let main = chrome(false, None, REPORT_SID);
        let Verdict::Fail(why) = judged(&main, program) else {
            panic!("CONTROL: main's read passed on {program}");
        };
        assert!(why.contains("after its older report"), "{why}");
        assert!(why.contains("pressing=report"), "{why}");
        assert!(why.contains("tip is none"), "{why}");
        for (one, broken) in [
            ("order", chrome(false, Some(&tip), ASK_SID)),
            ("pressing", chrome(true, Some(&tip), REPORT_SID)),
            ("tip", chrome(true, None, ASK_SID)),
        ] {
            assert!(
                matches!(judged(&broken, program), Verdict::Fail(_)),
                "CONTROL: {one} alone wrong passed on {program}"
            );
        }
        if program != "claude" {
            let claude = chrome(true, Some(&report_words("Claude Code", false)), ASK_SID);
            assert!(
                matches!(judged(&claude, program), Verdict::Fail(_)),
                "CONTROL: {program}'s row said Claude Code"
            );
        }
    }
}

/// No step, no verdict but a named skip; a step whose rows never both showed, or whose ask stood
/// on once taken down, fails.
#[test]
fn an_ask_never_read_is_unobserved_and_one_never_listed_fails() {
    let cx = Ctx {
        chain: Chain::CruiseN,
        program: "claude",
        t0: 1_000,
    };
    let none = Seen {
        window: Window {
            read: true,
            ..Window::default()
        },
        ..Seen::default()
    };
    assert!(matches!(first(&none, &cx), Verdict::Unobserved(_)));
    let tip = report_words("Claude Code", false);
    let mut alone = chrome(true, Some(&tip), ASK_SID);
    alone = alone
        .lines()
        .filter(|l| !l.contains(ASK_TAB) || l.starts_with("presence "))
        .collect::<Vec<_>>()
        .join("\n");
    let read = Read::of(&alone, "cruise-n");
    assert!(!read.listed(), "{read:?}");
    let seen = |line: String| Seen {
        window: Window {
            read: true,
            stopped: Some((5_000, line)),
            ..Window::default()
        },
        ..Seen::default()
    };
    let Verdict::Fail(why) = first(
        &seen(format!("{} cleared=yes", read.line(ASK_SID, REPORT_SID))),
        &cx,
    ) else {
        panic!("unlisted passed");
    };
    assert!(why.contains("did not list"), "{why}");
    let ours = Read::of(&chrome(true, Some(&tip), ASK_SID), "cruise-n");
    assert!(matches!(
        first(
            &seen(format!("{} cleared=no", ours.line(ASK_SID, REPORT_SID))),
            &cx
        ),
        Verdict::Fail(_)
    ));
}
