// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local brief (`survey --brief`) over two fake instances, one refusing.

use super::*;

const NOW: u64 = 1_790_000_000_000;

fn newer() -> NodeRead {
    NodeRead {
        pid: 7,
        sock: "/private/tmp/x/aterm-7.sock".to_string(),
        dial: true,
        node: Ok(vec![
            format!(
                "1 open n start prev_end=clean by=aterm src=aterm conf=exact t={}",
                NOW - 3_600_000
            ),
            "2 mark n link state=stalled gen=1 by=aterm src=fabric conf=strong t=1".to_string(),
            "3 mark n link state=connected gen=1 by=bridge src=fabric conf=exact t=2".to_string(),
            "4 mark n config key=privacy.enabled by=aterm src=aterm conf=exact t=3".to_string(),
        ]),
    }
}

fn older() -> NodeRead {
    NodeRead {
        pid: 9,
        sock: "/private/tmp/x/aterm-9.sock".to_string(),
        dial: true,
        node: Ok(vec![
            "gap why=evicted lost=1030 resync_to=1030".to_string(),
            "1031 mark n link state=disconnected gen=4 by=aterm src=fabric conf=strong t=9"
                .to_string(),
        ]),
    }
}

fn ask(
    asked: &mut Vec<(u32, usize)>,
) -> impl FnMut(&NodeRead, usize) -> Result<Vec<String>, String> {
    move |r, tokens| {
        asked.push((r.pid, tokens));
        match r.pid {
            7 => Ok(vec![
                "brief sid=s-1 v=2 as_of=40 ep=1 budget=300 used=80".to_string(),
                "agent claude busy 2m · t3 open 40s by=ctl ledger=3".to_string(),
                "brief sid=s-2 v=2 as_of=9 ep=1 budget=300 used=40".to_string(),
            ]),
            _ => Err("server replied: ERR denied: brief all is the owner's".to_string()),
        }
    }
}

fn keeps_every_gap(out: &str, reads: &[NodeRead]) -> bool {
    reads
        .iter()
        .filter_map(|r| r.node.as_ref().ok())
        .flatten()
        .filter(|l| l.starts_with("gap "))
        .all(|gap| out.lines().any(|l| l == gap))
}

#[test]
fn the_local_brief_heads_each_instance_and_keeps_its_gap_lines() {
    let reads = vec![older(), newer()];
    let mut asked = Vec::new();
    let out = render_brief(reads.clone(), BRIEF_BUDGET, NOW, ask(&mut asked));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], BRIEF_DATA_LINE);
    assert_eq!(
        lines[1],
        "== pid 7 up 1h · node log 4 of 4 · 2 brief(s) trust=screen"
    );
    assert_eq!(&lines[2..6], &newer().node.unwrap()[..]);
    assert_eq!(
        lines[6],
        "brief sid=s-1 v=2 as_of=40 ep=1 budget=300 used=80"
    );
    assert_eq!(
        lines[9],
        "== pid 9 · node log 1 of 1 · brief all refused: server replied: ERR denied: brief \
         all is the owner's trust=screen"
    );
    assert_eq!(&lines[10..], &older().node.unwrap()[..]);
    assert_eq!(
        asked.iter().map(|a| a.0).collect::<Vec<_>>(),
        [7, 9],
        "the newest asked first"
    );
    assert!(keeps_every_gap(&out, &reads));
    let dropped: String = out
        .lines()
        .filter(|l| !l.starts_with("gap "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!keeps_every_gap(&dropped, &reads));
}

#[test]
fn an_instance_that_refuses_is_said_in_its_header() {
    let refusing = NodeRead {
        pid: 11,
        sock: "/private/tmp/x/aterm-11.sock".to_string(),
        dial: true,
        node: Err("server replied: ERR denied".to_string()),
    };
    let foreign = NodeRead {
        pid: 12,
        sock: "/private/tmp/y/private.sock".to_string(),
        dial: false,
        node: Err("pid 12's token is not readable here".to_string()),
    };
    let mut asked = Vec::new();
    let out = render_brief(vec![refusing, foreign], BRIEF_BUDGET, NOW, ask(&mut asked));
    let heads: Vec<&str> = out.lines().filter(|l| l.starts_with("== ")).collect();
    assert_eq!(
        heads,
        [
            "== pid 11 · log n refused: server replied: ERR denied · brief all refused: server \
             replied: ERR denied: brief all is the owner's trust=screen",
            "== pid 12 · log n refused: pid 12's token is not readable here · brief all \
             refused: not dialled: no token of its to read trust=screen",
        ]
    );
    let room = BRIEF_BUDGET - (BRIEF_DATA_LINE.len() + 1) - 120;
    assert_eq!(
        asked,
        [(11, (room - HEADER_CHARS - 1) / 4)],
        "the foreign one never asked"
    );
    assert_eq!(out.lines().count(), 3, "{out}");
}

#[test]
fn the_budget_is_shared_newest_first_and_never_passed() {
    for budget in [BRIEF_BUDGET, 1_200, 700, 500, 200, 10] {
        let mut asked = Vec::new();
        let out = render_brief(vec![older(), newer()], budget, NOW, ask(&mut asked));
        assert!(
            out.chars().count() <= budget,
            "{budget}: {} chars\n{out}",
            out.chars().count()
        );
        if asked.len() == 2 {
            assert_eq!((asked[0].0, asked[1].0), (7, 9), "{budget}");
            assert!(asked[0].1 > asked[1].1, "{budget}: {asked:?}");
        }
        let shown = out.lines().filter(|l| l.starts_with("== ")).count();
        if shown < 2 {
            let last = out.lines().last().unwrap_or_default();
            assert!(
                last.starts_with(&format!("… the {budget}-char budget left out"))
                    || (budget < 50 && last.starts_with('…')),
                "{budget}: {out}"
            );
        }
    }
    let mut asked = Vec::new();
    let out = render_brief(vec![older(), newer()], BRIEF_BUDGET, NOW, ask(&mut asked));
    assert!(!out.contains("left out"), "{out}");
}

#[test]
fn a_long_node_log_prints_its_newest_records() {
    let mut lines = vec!["gap why=evicted lost=5 resync_to=5".to_string()];
    for seq in 6..106u64 {
        lines.push(format!(
            "{seq} mark n link state=connected gen={seq} by=bridge src=fabric conf=exact t={seq}"
        ));
    }
    let read = NodeRead {
        pid: 3,
        sock: "/private/tmp/x/aterm-3.sock".to_string(),
        dial: true,
        node: Ok(lines.clone()),
    };
    let mut asked = Vec::new();
    let out = render_brief(vec![read], BRIEF_BUDGET, NOW, ask(&mut asked));
    let body: Vec<&str> = out.lines().skip(1).collect();
    assert!(
        body[0].starts_with(&format!("== pid 3 · node log {NODE_LINES_MAX} of 100 ")),
        "{out}"
    );
    assert_eq!(body[1], lines[0], "the gap first");
    assert_eq!(body[2], lines[lines.len() - NODE_LINES_MAX]);
    assert_eq!(body[1 + NODE_LINES_MAX], lines[lines.len() - 1]);
    assert!(!out.contains("budget left out"), "{out}");
    let mut asked = Vec::new();
    let out = render_brief(
        vec![NodeRead {
            pid: 3,
            sock: "/private/tmp/x/aterm-3.sock".to_string(),
            dial: true,
            node: Ok(lines),
        }],
        1_500,
        NOW,
        ask(&mut asked),
    );
    let head = out.lines().nth(1).unwrap_or_default();
    let kept: usize = head
        .strip_prefix("== pid 3 · node log ")
        .and_then(|r| r.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    assert!(kept > 0 && kept < NODE_LINES_MAX, "{out}");
    assert!(
        out.lines().last().is_some_and(|l| l.starts_with(&format!(
            "… the 1500-char budget left out {} line(s) and 0 instance(s)",
            NODE_LINES_MAX - kept
        ))),
        "{out}"
    );
}

#[test]
fn an_instance_that_refuses_log_n_still_gets_its_brief() {
    let refused = "ERR usage: log [<node>] [since=<seq> ep=<e>] [--json]".to_string();
    let probes = vec![(
        41,
        "/private/tmp/x/aterm-41.sock".to_string(),
        crate::Probe::Replied(refused),
    )];
    let mut asked = Vec::new();
    let out = brief_of(
        crate::DirOutcome::Scoped { live_elsewhere: 0 },
        &probes,
        BRIEF_BUDGET,
        NOW,
        |r, tokens| {
            asked.push((r.pid, tokens));
            Ok(vec![
                "brief sid=s-1 v=2 as_of=40 ep=1 budget=300 used=80".to_string(),
                "agent claude idle 2m".to_string(),
            ])
        },
    )
    .unwrap_or_else(|(report, code)| panic!("exit {code}: {report}"));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], BRIEF_DATA_LINE);
    assert!(
        lines[1].starts_with("== pid 41 · log n refused: server replied: ERR usage: log ")
            && lines[1].ends_with(" · 1 brief(s) trust=screen"),
        "{out}"
    );
    assert_eq!(
        &lines[2..],
        [
            "brief sid=s-1 v=2 as_of=40 ep=1 budget=300 used=80",
            "agent claude idle 2m"
        ]
    );
    assert_eq!(asked.iter().map(|a| a.0).collect::<Vec<_>>(), [41]);
    let mut asked = 0;
    let timed_out = vec![(
        41,
        "/private/tmp/x/aterm-41.sock".to_string(),
        crate::Probe::Timeout,
    )];
    let refused = brief_of(
        crate::DirOutcome::Scoped { live_elsewhere: 0 },
        &timed_out,
        BRIEF_BUDGET,
        NOW,
        |_, _| {
            asked += 1;
            Ok(Vec::new())
        },
    );
    assert!(
        matches!(&refused, Err((report, 124)) if report.contains("timed out after 2s")),
        "{refused:?}"
    );
    assert_eq!(asked, 0);
}
