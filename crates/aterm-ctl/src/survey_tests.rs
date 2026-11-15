// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn quota_rows_survive_fleet_parsing_and_both_renderers() {
    let data = r#"{"as":"dev","caller":"cli:api","counted":12}"#;
    let survey = parse_reply(&[
        "instance host=peer pid=1 sessions=0 level=facts".into(),
        "github_rate hour=123 status=recorded attempts=12 groups=1 cut=0".into(),
        format!(
            "github_rate hour=123 data={}",
            aterm_codec::pct::encode(data)
        ),
    ])
    .unwrap();
    assert_eq!(survey.github_rate.len(), 2);
    let replies = vec![HostReply {
        label: "peer".into(),
        remote: true,
        unproven: false,
        node: None,
        answer: Ok(survey),
        reason: None,
    }];
    assert!(render_json(&replies).contains("\"github_rate\":["));
    assert!(render_full(&replies, None).contains(data));
    assert!(render_full(&replies, None).contains("status=recorded"));
}

fn render_text(replies: &[HostReply], budget: usize, own: Option<&str>) -> String {
    render_within(&blocks_of(replies, own, false), budget, budget).text
}

fn wire_row(fields: &[(&str, &str)]) -> String {
    let mut row = String::from("session");
    for (k, v) in fields {
        row.push(' ');
        row.push_str(k);
        row.push('=');
        row.push_str(&aterm_codec::pct::encode(v));
    }
    row
}

fn busy_agent(host: usize, n: usize) -> String {
    let sid = format!("s-{host:04x}{n:016x}");
    let words = "word ".repeat(80);
    wire_row(&[
        ("local", &n.to_string()),
        ("sid", &sid),
        ("program", "claude"),
        ("agent", "busy"),
        ("agent_ms", "125000"),
        ("doing", "Bash"),
        ("task", "working"),
        ("task_rev", "2"),
        ("model", "Opus 5.5"),
        ("effort", "xhigh"),
        ("title", "Fix the footer rendering on narrow panes"),
        ("cwd", "~/Developer/aterm-mesh-sv/crates/aterm-gui"),
        ("branch", "mesh/sv"),
        ("prompt", &excerpt400(&words)),
        ("said", &excerpt400(&words)),
    ])
}

fn excerpt400(text: &str) -> String {
    text.chars().take(400).collect()
}

fn shell(n: usize) -> String {
    wire_row(&[
        ("local", &n.to_string()),
        ("sid", &format!("s-ffff{n:016x}")),
        ("program", "zsh"),
        ("program_kind", "shell"),
        ("agent", "-"),
        ("cwd", "~/Developer/x"),
    ])
}

fn reply(host: usize, rows: Vec<String>) -> HostReply {
    let mut lines = vec![format!(
        "instance pid={} host=m{host}.local version=0.104.0 sessions={} level=content at_ms=1",
        1000 + host,
        rows.len()
    )];
    lines.extend(rows);
    HostReply {
        reason: None,
        label: format!("pid {}", 1000 + host),
        remote: false,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    }
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn says_it_left_out(text: &str) -> bool {
    text.lines().any(|l| l.starts_with(LEFT_OUT_LEAD))
}

fn assert_line_caps(text: &str) {
    for line in text.lines() {
        let cap = if line.starts_with("== ") && line.contains(" did not answer: ") {
            UNANSWERED_CHARS
        } else if line.starts_with("== ") || line.starts_with(LEFT_OUT_LEAD) {
            HEADER_CHARS
        } else if line.starts_with("    ") {
            EXCERPT_WIDE_CHARS
        } else {
            LINE_CHARS
        };
        assert!(chars(line) <= cap, "{} > {cap}: {line}", chars(line));
    }
}

#[test]
fn the_arguments_parse_and_a_stray_one_is_the_usage_line() {
    let parts = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    assert_eq!(
        parse_args(&parts(&["survey"])),
        Ok(Args {
            json: false,
            budget: DEFAULT_BUDGET,
            sid: None,
            fleet: false,
            host: None,
            within: FLEET_WITHIN,
            brief: false,
            holder: None,
        })
    );
    let brief = parse_args(&parts(&["survey", "--brief"])).unwrap();
    assert!(brief.brief && brief.budget == DEFAULT_BUDGET);
    let brief = parse_args(&parts(&["survey", "--brief", "--budget", "900"])).unwrap();
    assert!(brief.brief && brief.budget == 900);
    let fleet = parse_args(&parts(&["survey", "--fleet", "--timeout", "2.5"])).unwrap();
    assert!(fleet.fleet && fleet.host.is_none());
    assert_eq!(fleet.within, Duration::from_millis(2500));
    let host = parse_args(&parts(&["survey", "--host", "m7", "--sid", "s-2"])).unwrap();
    assert_eq!(host.host.as_deref(), Some("m7"));
    assert_eq!(host.within, FLEET_WITHIN);
    let all = parse_args(&parts(&["survey", "--json", "--sid", "@s-1"])).unwrap();
    assert!(all.json);
    assert_eq!(all.sid.as_deref(), Some("s-1"));
    let budget = parse_args(&parts(&["survey", "--fleet", "--budget", "500"])).unwrap();
    assert_eq!(budget.budget, 500);
    for bad in [
        &["survey", "--budget"][..],
        &["survey", "--budget", "0"],
        &["survey", "--sid"],
        &["survey", "--sid", ""],
        &["survey", "--sid", "@"],
        &["survey", "facts"],
        &["survey", "--host"],
        &["survey", "--fleet", "--host", "m7"],
        &["survey", "--fleet", "--timeout", "0"],
        &["survey", "--fleet", "--timeout", "1e-300"],
        &["survey", "--fleet", "--timeout", "+2.5"],
        &["survey", "--fleet", "--timeout", "11"],
        &["survey", "--fleet", "--timeout", "soon"],
        &["survey", "--timeout", "3"],
        &["survey", "--host", "m7", "--budget", "10"],
        &["survey", "--json", "--budget", "500"],
        &["survey", "--sid", "s-1", "--budget", "500"],
        &["survey", "--brief", "--json"],
        &["survey", "--brief", "--sid", "s-1"],
        &["survey", "--brief", "--fleet"],
        &["survey", "--brief", "--host", "m7"],
    ] {
        assert!(parse_args(&parts(bad)).is_err(), "{bad:?}");
    }
    assert!(is_survey(&parts(&["survey", "--json"])));
    assert!(!is_survey(&parts(&["sessions"])));
}

fn asked_rows(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("    asked: "))
        .count()
}

fn folded(text: &str) -> usize {
    text.lines()
        .filter_map(|l| l.strip_prefix("  +"))
        .filter_map(|l| l.split(' ').next()?.parse::<usize>().ok())
        .sum()
}

fn marked(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("s-") && l.contains(EXCERPTS_CUT))
        .count()
}

fn words_of<'a>(text: &'a str, sid: &str) -> Vec<&'a str> {
    text.lines()
        .skip_while(|l| !l.starts_with(sid))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .collect()
}

#[test]
fn one_hundred_eighty_sessions_render_within_the_budget() {
    let mut rows: Vec<String> = (0..170).map(shell).collect();
    rows.extend((0..10).map(|n| busy_agent(0, n)));
    let replies = [reply(0, rows)];
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("== m0.local pid 1000 0.104.0 · 180 sessions (10 busy)"),
        "{text}"
    );
    assert!(lines[0].ends_with(" trust=screen"), "{text}");
    assert!(lines[1].starts_with("s-0000"), "{text}");
    assert!(
        lines[1].contains(" claude busy 2m Bash task=working/2 "),
        "{text}"
    );
    assert!(lines[2].starts_with("    asked: word word"), "{text}");
    assert!(lines[3].starts_with("    said: word word"), "{text}");
    assert_eq!(lines.iter().filter(|l| l.starts_with("s-")).count(), 180);
    assert_eq!(folded(&text), 0, "{text}");
    assert!(!says_it_left_out(&text), "{text}");

    let tight = render_text(&replies, 12_000, None);
    assert!(chars(&tight) <= 12_000, "{}", chars(&tight));
    assert_eq!(
        asked_rows(&tight),
        10,
        "every agent keeps its words: {tight}"
    );
    let shown = tight.lines().filter(|l| l.starts_with("s-ffff")).count();
    let more = folded(&tight);
    assert!(more > 0 && shown + more == 170, "{shown} + {more}: {tight}");
    assert!(
        tight.ends_with(&format!(
            "  +{more} idle shells\n{}",
            left_out_line(more, 0, 0, false)
        )),
        "{tight}"
    );
}

fn fill_reads(replies: &[HostReply]) -> Vec<String> {
    let blocks = blocks_of(replies, None, false);
    let order = fill_order(&blocks);
    (0..=order.len())
        .map(|n| render(&blocks, &Shown::filled(&blocks, &order, n)))
        .collect()
}

/// Whether `text` is the fill of the most units whose read fits `room`: no read past it fits,
/// though a read can be shorter than the one before it (showing a kind's last line drops the
/// clause or line that counted it).
fn used_to_the_unit(reads: &[String], room: usize, text: &str) -> bool {
    reads
        .iter()
        .position(|read| read == text)
        .is_some_and(|n| n + 1 < reads.len() && reads[n + 1..].iter().all(|r| chars(r) > room))
}

#[test]
fn eighteen_instances_degrade_honestly_to_fit() {
    let replies: Vec<HostReply> = (0..18)
        .map(|h| reply(h, (0..10).map(|n| busy_agent(h, n)).collect()))
        .collect();
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    assert_eq!(text.lines().filter(|l| l.starts_with("== ")).count(), 18);
    assert_eq!(text.lines().filter(|l| l.starts_with("s-")).count(), 180);
    let rows = excerpt_rows(&text).len();
    assert!(rows > 0 && rows < 36, "{rows}: {text}");
    assert!(
        used_to_the_unit(&fill_reads(&replies), DEFAULT_BUDGET, &text),
        "{text}"
    );
    let firsts: Vec<Vec<&str>> = (0..18)
        .map(|h| words_of(&text, &format!("s-{h:04x}{:016x}", 0)))
        .collect();
    assert_eq!(
        firsts.iter().map(Vec::len).sum::<usize>(),
        rows,
        "only the firsts: {text}"
    );
    let said = firsts
        .iter()
        .filter(|w| w.iter().any(|l| l.starts_with("    said: ")))
        .count();
    assert_eq!(said, rows.min(18), "{text}");
    let whole = firsts.iter().filter(|w| w.len() == 2).count();
    assert_eq!(marked(&text), 180 - whole, "{text}");
    assert!(
        text.ends_with(&left_out_line(0, 180 - whole, 0, false)),
        "{text}"
    );

    let small = render_text(&replies, 8_000, None);
    assert!(chars(&small) <= 8_000, "{}", chars(&small));
    let shown = small.lines().filter(|l| l.starts_with("s-")).count();
    assert!(shown < 180, "{shown}");
    assert_eq!(small.lines().filter(|l| l.starts_with("== ")).count(), 18);
    let more = folded(&small);
    assert_eq!(shown + more, 180, "every session shown or counted");
    assert!(
        small.ends_with(&left_out_line(more, shown, 0, false)),
        "{small}"
    );

    let tiny = render_text(&replies, 600, None);
    assert!(chars(&tiny) <= 600, "{}", chars(&tiny));
    assert!(
        tiny.lines()
            .last()
            .unwrap()
            .contains("more lines past the 600-char budget")
    );
}

fn lab_agent(host: usize, n: usize, agent: &str, agent_ms: &str) -> String {
    let sid = format!("s-{host:04x}{n:016x}");
    wire_row(&[
        ("sid", &sid),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", agent),
        ("agent_ms", agent_ms),
        ("model", "Opus 5.5"),
        ("title", "Fix the reconnect backoff in the dial loop"),
        (
            "cwd",
            &format!("~/Developer/lab-{host:02}-worktrees/aterm-mesh-sv/crates/aterm-net"),
        ),
        ("branch", "mesh/sv"),
        ("prompt", &excerpt400(&"audit the dial loop ".repeat(30))),
        ("said", &excerpt400(&"the timers reset now ".repeat(30))),
    ])
}

fn lab_shell(host: usize, n: usize, running: bool) -> String {
    let sid = format!("s-{host:04x}{n:016x}");
    let mut fields = vec![
        ("sid", sid.as_str()),
        ("program", "zsh"),
        ("program_kind", "shell"),
        ("agent", "-"),
        ("cwd", "~/Developer/aterm-mesh-sv/crates/aterm-gui"),
    ];
    if running {
        fields.extend([
            ("detail", "targo"),
            ("cmd", "targo --unverified test -j 8 -p aterm-gui --lib"),
        ]);
    }
    wire_row(&fields)
}

#[test]
fn thirty_lab_macs_keep_each_machines_most_pressing_words() {
    let replies: Vec<HostReply> = (0..30)
        .map(|h| {
            let mut rows = vec![
                lab_agent(h, 1, "busy", "125000"),
                lab_agent(h, 2, "idle", "7200000"),
                lab_agent(h, 3, "idle", "18000000"),
                lab_shell(h, 4, true),
                lab_shell(h, 5, false),
            ];
            if h == 5 {
                rows[1] = lab_agent(h, 2, "prompt", "4000");
                rows[2] = lab_agent(h, 3, "prompt", "9000");
            }
            let mut lines = vec![format!(
                "instance pid={} host=Lab-Mac-{h:02}.local version=0.105.0 sessions=5 \
                 level=content cut=0 at_ms=1",
                2000 + h
            )];
            lines.extend(rows);
            HostReply {
                reason: None,
                label: format!("Lab-Mac-{h:02}.local"),
                remote: true,
                unproven: false,
                node: None,
                answer: parse_reply(&lines),
            }
        })
        .collect();
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    assert_eq!(
        text.lines().filter(|l| l.starts_with("== fleet ")).count(),
        30
    );
    let sessions: Vec<&str> = text.lines().filter(|l| l.starts_with("s-")).collect();
    let agents = sessions.iter().filter(|l| l.contains(" claude ")).count();
    assert_eq!(agents, 90, "every agent keeps its line: {text}");
    let more = folded(&text);
    assert_eq!(sessions.len() - agents + more, 60, "{text}");
    assert!(more > 0, "the shells fold first: {text}");
    for h in (0..30).filter(|&h| h != 5) {
        let busy = words_of(&text, &format!("s-{h:04x}{:016x}", 1));
        assert!(
            busy.iter().any(|l| l.starts_with("    said: ")),
            "m{h}'s busy agent keeps its last words: {text}"
        );
    }
    for n in [2, 3] {
        let asking = words_of(&text, &format!("s-{:04x}{n:016x}", 5));
        assert_eq!(
            asking.len(),
            2,
            "m5's asking agent {n} keeps every word: {text}"
        );
    }
    let reads = fill_reads(&replies);
    assert!(used_to_the_unit(&reads, DEFAULT_BUDGET, &text), "{text}");
    let n = reads.iter().position(|read| *read == text).unwrap();
    assert!(!used_to_the_unit(&reads, DEFAULT_BUDGET, &reads[n - 1]));
    let whole = (0..30)
        .flat_map(|h| (1..=3).map(move |n| format!("s-{h:04x}{n:016x}")))
        .filter(|sid| words_of(&text, sid).len() == 2)
        .count();
    assert_eq!(marked(&text), 90 - whole, "{text}");
    assert!(
        text.ends_with(&left_out_line(more, 90 - whole, 0, true)),
        "{text}"
    );
}

/// Thirty Macs that each say where their home is, twenty-four of them asking: the home lines fold,
/// counted on the closing line, after the shells and before any machine's most pressing agent
/// loses a word (sessions-as-subagents §3; one-mesh E2's `lab fleet-sim` found the last machine's
/// most pressing agent short once DX-2's home lines joined every header). NEGATIVE CONTROL: with
/// the home lines ranked with the header ([`HOME_WITH_HEADER`]), as before they could fold, every
/// home line shows and a machine's most pressing agent loses its words.
#[test]
fn thirty_macs_home_lines_fold_before_an_agent_loses_its_words() {
    let asking = |h: usize| h % 5 != 4;
    let replies: Vec<HostReply> = (0..30)
        .map(|h| {
            let rows = vec![
                if asking(h) {
                    lab_agent(h, 1, "prompt", "4000")
                } else {
                    lab_agent(h, 1, "busy", "125000")
                },
                lab_agent(h, 2, "idle", "7200000"),
                lab_agent(h, 3, "idle", "18000000"),
                lab_shell(h, 4, true),
                lab_shell(h, 5, false),
            ];
            let mut lines = vec![format!(
                "instance pid={} host=Lab-Mac-{h:02}.local version=0.155.0 sessions=5 \
                 level=content cut=0 at_ms=1793129880000 home=/Users//lab-{h:02} \
                 home_free_mb=111616 repos=0 repos_at=1793129874000 repos_cut=0",
                2000 + h
            )];
            lines.extend(rows);
            HostReply {
                reason: None,
                label: format!("Lab-Mac-{h:02}.local"),
                remote: true,
                unproven: false,
                node: None,
                answer: parse_reply(&lines),
            }
        })
        .collect();
    // The machines whose most pressing agent lost words: an asking one keeps both, another its
    // last words.
    let short = |text: &str| -> Vec<usize> {
        (0..30)
            .filter(|&h| {
                let words = words_of(text, &format!("s-{h:04x}{:016x}", 1));
                if asking(h) {
                    words.len() != 2
                } else {
                    !words.iter().any(|l| l.starts_with("    said: "))
                }
            })
            .collect()
    };
    let homes = |text: &str| text.lines().filter(|l| l.starts_with("  home ")).count();

    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    assert_eq!(short(&text), Vec::<usize>::new(), "{text}");
    assert_eq!(
        folded(&text) + text.lines().filter(|l| l.starts_with("s-")).count(),
        150
    );
    let shown = homes(&text);
    assert!(shown < 30, "the home lines fold: {text}");
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.contains(&format!(", {} homes; ", 30 - shown))),
        "the closing line counts them: {text}"
    );

    HOME_WITH_HEADER.with(|on| on.set(true));
    let before = render_text(&replies, DEFAULT_BUDGET, None);
    HOME_WITH_HEADER.with(|on| on.set(false));
    assert_eq!(homes(&before), 30, "the control shows every home: {before}");
    assert!(
        !short(&before).is_empty(),
        "the control: with every home line shown a most pressing agent loses words: {before}"
    );
}

/// Machine `h` of a `lab fleet-sim` fleet as its reply reads: a home with no checkouts, both
/// hourly quota rows a fresh instance sends (`github_rate … status=unrecorded`), its boot shell,
/// a `build-loop`, a `claude` busy, at a question, at a box or idle by turns, and two more
/// `claude` idle seconds ago (newly done), each asked and saying words near the 400-byte record
/// and reporting its model, effort, context and plan windows as the fixture's statusLine does.
fn sim_mac(h: usize) -> HostReply {
    let claude = |n: usize, agent: &str, ms: &str| {
        wire_row(&[
            ("sid", &format!("s-{h:04x}{n:016x}")),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", agent),
            ("agent_ms", ms),
            ("doing", "Bash"),
            ("model", "Opus 5.5"),
            ("effort", "xhigh"),
            ("ctx_left_pct", "62"),
            ("usage_5h_pct", "38"),
            ("usage_7d_pct", "21"),
            (
                "cwd",
                &format!("~/Developer/aterm-mesh-wt-{h:02}-{n}/crates/aterm-net"),
            ),
            ("prompt", &excerpt400(&"audit the dial loop ".repeat(30))),
            ("said", &excerpt400(&"the timers reset now ".repeat(30))),
        ])
    };
    let mode = ["busy", "question", "prompt", "idle"][h % 4];
    let rows = vec![
        lab_shell(h, 0, false),
        lab_shell(h, 1, true),
        claude(2, mode, if mode == "idle" { "6000" } else { "4000" }),
        claude(3, "idle", "5000"),
        claude(4, "idle", "4000"),
    ];
    let mut lines = vec![format!(
        "instance pid={} host=Lab-Mac-{h:02}.local version=0.160.0 sessions=5 level=content \
         cut=0 at_ms=1793129880000 home=/Users//lab-{h:02} home_free_mb=111616 repos=0 \
         repos_at=1793129874000 repos_cut=0",
        2000 + h
    )];
    lines.extend(["github_rate hour=498091 status=unrecorded".to_owned()]);
    lines.extend(["github_rate hour=498092 status=unrecorded".to_owned()]);
    lines.extend(rows);
    HostReply {
        reason: None,
        label: format!("127.0.0.1:{}", 7200 + h),
        remote: true,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    }
}

/// The machines of a [`sim_mac`] fleet whose most pressing agent (its `claude` 2, first in its
/// block at every mode) shows no excerpt line.
fn wordless_firsts(text: &str, n: usize) -> Vec<usize> {
    (0..n)
        .filter(|&h| words_of(text, &format!("s-{h:04x}{:016x}", 2)).is_empty())
        .collect()
}

/// ATERM-KI-1112-102: thirty `lab fleet-sim` machines under the default budget. Every machine's
/// most pressing agent keeps at least its leading excerpt line, whatever its class, and the
/// quota rows each machine sends fold, counted on a closing line of their own, rather than ride
/// with every header (at thirty machines their sixty lines, with the asking agents' second lines
/// ranked before them, had cost the most pressing idle agents their words).
#[test]
fn thirty_sim_macs_keep_every_most_pressing_agents_words() {
    let replies: Vec<HostReply> = (0..30).map(sim_mac).collect();
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    assert_eq!(wordless_firsts(&text, 30), Vec::<usize>::new(), "{text}");
    for h in (0..30).filter(|h| matches!(h % 4, 1 | 2)) {
        assert_eq!(
            words_of(&text, &format!("s-{h:04x}{:016x}", 2)).len(),
            2,
            "m{h}'s asking agent keeps both its lines: {text}"
        );
    }
    let rates = text
        .lines()
        .filter(|l| l.starts_with("  github_rate "))
        .count();
    assert!(rates < 60, "the quota rows fold: {text}");
    assert!(
        text.ends_with(&format!(
            "{LEFT_OUT_LEAD}{} quota rows; --fleet with --budget, --sid <s> or --json for more\n",
            60 - rates
        )),
        "a closing line of their own counts them: {text}"
    );
    let reads = fill_reads(&replies);
    assert!(used_to_the_unit(&reads, DEFAULT_BUDGET, &text), "{text}");
}

/// Every machine's most pressing agent's leading excerpt line comes before any machine's second
/// line of words, so a budget that holds every header, every agent's line and one line of words
/// for each machine's first agent shows that line for every machine, an idle one last in its
/// round included. NEGATIVE CONTROL: with the leading line ranked with the rest of its agent's
/// words ([`FIRST_WORDS_UNLED`]), as before, the asking machines' second lines take the room of
/// the idle machines' only line.
#[test]
fn every_machines_first_agent_speaks_before_any_second_line() {
    let replies: Vec<HostReply> = (0..30).map(sim_mac).collect();
    let blocks = blocks_of(&replies, None, false);
    let order = fill_order(&blocks);
    // The fill that holds the agent lines and every machine's first line of words, and no more.
    let agents = order
        .iter()
        .take_while(|unit| matches!(unit, Unit::Line(..)))
        .count();
    let led = render(&blocks, &Shown::filled(&blocks, &order, agents + 30));
    assert_eq!(wordless_firsts(&led, 30), Vec::<usize>::new(), "{led}");
    assert_eq!(excerpt_rows(&led).len(), 30, "{led}");
    let budget = chars(&led) + EXCERPT_CHARS;
    let text = render_text(&replies, budget, None);
    assert!(chars(&text) <= budget, "{}", chars(&text));
    assert_eq!(wordless_firsts(&text, 30), Vec::<usize>::new(), "{text}");

    FIRST_WORDS_UNLED.with(|on| on.set(true));
    let before = render_text(&replies, budget, None);
    FIRST_WORDS_UNLED.with(|on| on.set(false));
    assert!(
        !wordless_firsts(&before, 30).is_empty(),
        "the control: a machine's most pressing agent loses every word: {before}"
    );
}

/// A machine's second agent waiting on a person says its `attention:` line before any machine's
/// most pressing agent that waits on no one says a word: one machine with two asking agents,
/// each raising attention, beside twenty-nine whose first agent is idle, under a budget that
/// holds every header, every agent line and one line of words for each of those thirty-one
/// agents. Both asking agents show their `attention:` line, and every idle first its line.
#[test]
fn every_asking_agent_says_its_attention_before_an_idle_first() {
    let claude = |h: usize, n: usize, agent: &str, attention: Option<&str>| {
        let sid = format!("s-{h:04x}{n:016x}");
        let mut fields = vec![
            ("sid", sid.as_str()),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", agent),
            ("agent_ms", "4000"),
            ("cwd", "~/Developer/aterm"),
        ];
        fields.extend(attention.map(|words| ("attention", words)));
        let (prompt, said) = (
            excerpt400(&"audit the dial loop ".repeat(30)),
            excerpt400(&"the timers reset now ".repeat(30)),
        );
        fields.extend([("prompt", prompt.as_str()), ("said", said.as_str())]);
        wire_row(&fields)
    };
    let mut replies = vec![reply(
        0,
        vec![
            claude(0, 0, "question", Some("pick a port for the relay")),
            claude(0, 1, "question", Some("approve the schema migration")),
        ],
    )];
    replies.extend((1..30).map(|h| reply(h, vec![claude(h, 0, "idle", None)])));
    let blocks = blocks_of(&replies, None, false);
    let order = fill_order(&blocks);
    let agents = order
        .iter()
        .take_while(|unit| matches!(unit, Unit::Line(..)))
        .count();
    let led = render(&blocks, &Shown::filled(&blocks, &order, agents + 31));
    let budget = chars(&led) + EXCERPT_CHARS;
    let text = render_text(&replies, budget, None);
    assert!(chars(&text) <= budget, "{}", chars(&text));
    for n in 0..2 {
        let words = words_of(&text, &format!("s-{:04x}{n:016x}", 0));
        assert!(
            words
                .first()
                .is_some_and(|l| l.starts_with("    attention: ")),
            "asking agent {n} keeps its attention line: {text}"
        );
    }
    for h in 1..30 {
        assert!(
            !words_of(&text, &format!("s-{h:04x}{:016x}", 0)).is_empty(),
            "m{h}'s idle first keeps a line: {text}"
        );
    }
}

/// The fill is the most units whose read fits, though the read does not grow with every unit:
/// the unit that shows a fleet's last quota row drops the closing line that counted the quota
/// rows, a longer line than the row. Under every budget between the read with that last row and
/// the one before it, the fill reaches past that row, to the shells after it where they fit
/// (a bisection over the units, which assumes the read grows, can settle short of the row).
#[test]
fn the_fill_reaches_past_a_read_that_shrinks() {
    let replies: Vec<HostReply> = (0..3).map(sim_mac).collect();
    let reads = fill_reads(&replies);
    // Short of the whole read, which a budget that holds it shows wider than any fill.
    let whole = reads.len() - 1;
    let shrinks = (1..whole)
        .filter(|&n| chars(&reads[n]) < chars(&reads[n - 1]))
        .collect::<Vec<_>>();
    assert!(!shrinks.is_empty(), "some unit shortens the read");
    for &n in &shrinks {
        let below_whole = chars(&reads[n - 1]).min(chars(&reads[whole]));
        for budget in chars(&reads[n])..below_whole {
            let text = render_text(&replies, budget, None);
            assert!(chars(&text) <= budget, "{}", chars(&text));
            assert!(
                used_to_the_unit(&reads, budget, &text),
                "budget {budget}: {text}"
            );
        }
    }
}

/// Ten Macs whose homes each list sixteen checkouts, under a budget their home lines overrun:
/// each home line is a unit of its own, so the read leaves less than a line unused, every home
/// line shows before any checkout line, and the closing line counts the homes it cut (one-mesh
/// E2's review). NEGATIVE CONTROL: with each home one unit ([`HOME_WHOLE`]), the first home that
/// does not fit strands its room, more than a line of it.
#[test]
fn checkout_lines_fold_a_line_at_a_time() {
    let repo = |h: usize, n: usize| {
        wire_row(&[
            ("name", &format!("repo-{n:02}")),
            ("path", &format!("/Users//lab-{h:02}/repo-{n:02}")),
            ("head", "0123456789abcdef0123456789abcdef01234567"),
            ("branch", "main"),
            ("main", "origin/main"),
            ("behind", "0"),
            ("fetched_at", "1793129800000"),
            ("dirty", "0"),
        ])
        .replacen("session", "repo", 1)
    };
    let replies: Vec<HostReply> = (0..10)
        .map(|h| {
            let mut lines = vec![format!(
                "instance pid={} host=Lab-Mac-{h:02}.local version=0.155.0 sessions=1 \
                 level=content cut=0 at_ms=1793129880000 home=/Users//lab-{h:02} \
                 home_free_mb=111616 repos=16 repos_at=1793129874000 repos_cut=0",
                2000 + h
            )];
            lines.push(lab_agent(h, 1, "busy", "125000"));
            lines.extend((0..16).map(|n| repo(h, n)));
            HostReply {
                reason: None,
                label: format!("Lab-Mac-{h:02}.local"),
                remote: true,
                unproven: false,
                node: None,
                answer: parse_reply(&lines),
            }
        })
        .collect();
    let heads = |text: &str| text.lines().filter(|l| l.starts_with("  home ")).count();
    let rows = |text: &str| text.lines().filter(|l| l.starts_with("    repo-")).count();
    // Every line at the excerpt width the fill uses; a budget half its checkout lines short of
    // that leaves the fill to choose.
    let blocks = blocks_of(&replies, None, false);
    let all = render(&blocks, &Shown::all(&blocks, Class::Agent, EXCERPT_CHARS));
    assert_eq!((heads(&all), rows(&all)), (10, 160), "{all}");
    let row_room: usize = all
        .lines()
        .filter(|l| l.starts_with("    repo-"))
        .map(|l| chars(l) + 1)
        .sum();
    let budget = chars(&all) - row_room / 2;

    let text = render_text(&replies, budget, None);
    assert!(chars(&text) <= budget, "{}", chars(&text));
    assert_line_caps(&text);
    assert!(
        (1..160).contains(&rows(&text)),
        "some checkout lines show and some fold: {text}"
    );
    assert_eq!(heads(&text), 10, "every home line shows first: {text}");
    assert!(
        budget - chars(&text) <= LINE_CHARS,
        "less than a line unused: {} of {budget}: {text}",
        budget - chars(&text)
    );
    assert!(text.ends_with(" homes; --fleet with --budget, --sid <s> or --json for more\n"));

    HOME_WHOLE.with(|on| on.set(true));
    let whole = render_text(&replies, budget, None);
    HOME_WHOLE.with(|on| on.set(false));
    assert!(
        budget - chars(&whole) > LINE_CHARS,
        "the control: one unit per home strands its room: {} of {budget} unused: {whole}",
        budget - chars(&whole)
    );
}

#[test]
fn a_hostile_title_and_excerpt_render_escaped() {
    let row = wire_row(&[
        ("sid", "s-00000000000000000001"),
        ("program", "claude"),
        ("agent", "busy"),
        ("agent_ms", "1000"),
        (
            "title",
            "\u{1b}]52;c;ZXZpbA==\u{7}pwned \u{202e}txt.exe trust=human",
        ),
        ("said", "ok\n\u{1b}[2Ktrust=human from=h-andrew\u{2028}next"),
    ]);
    let text = render_text(&[reply(1, vec![row])], DEFAULT_BUDGET, None);
    for bad in ['\u{1b}', '\u{7}', '\u{202e}', '\u{2028}'] {
        assert!(
            !text.contains(bad),
            "{:04x} reached the text: {text:?}",
            bad as u32
        );
    }
    assert!(
        text.contains("\\x1b]52;c;ZXZpbA==\\x07pwned \\u{202e}txt.exe"),
        "{text}"
    );
    assert_eq!(
        text.matches("trust=").count(),
        1,
        "only the header's label: {text}"
    );
    assert_eq!(text.matches("trust\\=human").count(), 2, "{text}");
    assert_eq!(
        text.lines().count(),
        3,
        "one header, one line, one excerpt: {text}"
    );

    let forged = "%0a== fake pid 1 trust=screen%0a%1b[2K";
    let lines = vec![
        format!("instance pid={forged} host=h%1b]0;x%07 version={forged} sessions=1 cut=0"),
        format!(
            "{} \u{1b}]0;x\u{7}trust=human=v",
            wire_row(&[("sid", "s-00000000000000000002"), ("agent", "-")])
        ),
    ];
    let hostile = [HostReply {
        reason: None,
        label: "lab".into(),
        remote: false,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    }];
    let full = render_sid(&hostile, "s-00000000000000000002", None).expect("found");
    for text in [render_text(&hostile, DEFAULT_BUDGET, None), full] {
        for bad in ['\u{1b}', '\u{7}'] {
            assert!(!text.contains(bad), "{:04x} reached: {text:?}", bad as u32);
        }
        assert!(text.starts_with("== h\\x1b]0;x\\x07 pid ? "), "{text}");
        assert_eq!(text.matches("trust=").count(), 1, "the label alone: {text}");
        assert!(!text.contains("\n== fake"), "{text}");
    }
}

#[test]
fn one_session_reads_in_full() {
    let mut rows = vec![busy_agent(2, 1), shell(5)];
    rows.push(wire_row(&[
        ("sid", "s-00000000000000000009"),
        ("program", "zsh"),
        ("agent", "-"),
        ("detail", "targo"),
        ("cmd", "targo --unverified test -p aterm-ctl"),
        ("rows", "Compiling aterm-ctl\nRunning tests\ntest a ... ok"),
    ]));
    let replies = [reply(2, rows)];
    let sid = format!("s-{:04x}{:016x}", 2, 1);
    let full = render_sid(&replies, &sid, None).expect("found");
    let said = full
        .lines()
        .find_map(|l| l.strip_prefix("  said: "))
        .expect("said");
    assert_eq!(chars(said), 400, "the whole excerpt");
    let shell = render_sid(
        &replies,
        "s-00000000000000000009",
        Some("s-00000000000000000009"),
    )
    .expect("found");
    assert!(
        shell.contains(" (you) running targo | targo --unverified test -p aterm-ctl"),
        "{shell}"
    );
    assert_eq!(
        shell.lines().filter(|l| l.starts_with("  rows: ")).count(),
        3,
        "{shell}"
    );
    assert_eq!(render_sid(&replies, "s-nope", None), None);
}

#[test]
fn the_json_form_with_a_sid_is_that_session_alone() {
    let replies = vec![
        reply(3, vec![busy_agent(3, 1), shell(2)]),
        HostReply {
            reason: None,
            label: "pid 77".into(),
            remote: false,
            unproven: false,
            node: None,
            answer: Err("server replied: ERR unknown verb 'survey'".into()),
        },
    ];
    let sid = "s-ffff0000000000000002";
    let json = render_json(&only_sid(&replies, sid));
    assert!(json.contains(&format!("\"sid\":\"{sid}\"")), "{json}");
    assert!(!json.contains("\"program\":\"claude\""), "{json}");
    assert!(!json.contains("pid 77"), "{json}");
    assert!(only_sid(&replies, "s-nope").is_empty());
}

#[test]
fn the_json_form_is_every_record() {
    let replies = vec![
        reply(3, vec![busy_agent(3, 1), shell(2)]),
        HostReply {
            reason: None,
            label: "pid 77".into(),
            remote: false,
            unproven: false,
            node: None,
            answer: Err("server replied: ERR unknown verb 'survey'".into()),
        },
    ];
    let json = render_json(&replies);
    assert!(
        json.starts_with("[{\"label\":\"pid 1003\",\"instance\":{\"pid\":\"1003\""),
        "{json}"
    );
    assert!(json.contains("\"program\":\"claude\""), "{json}");
    assert!(json.contains("\"cwd\":\"~/Developer/x\""), "{json}");
    assert!(json.contains(
        "{\"label\":\"pid 77\",\"error\":\"server replied: ERR unknown verb 'survey'\"}"
    ));
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(
        text.contains(
            "== pid 77 did not answer: server replied: ERR unknown verb 'survey' trust=screen"
        ),
        "{text}"
    );
}

#[test]
fn sessions_are_ordered_by_what_needs_reading() {
    let row = |fields: &[(&str, &str)]| Fields::of(&wire_row(fields));
    assert_eq!(class_of(&row(&[("agent", "question")])), Class::Asking);
    assert_eq!(
        class_of(&row(&[("agent", "idle"), ("attention", "help")])),
        Class::Asking
    );
    assert_eq!(class_of(&row(&[("agent", "wall:usage")])), Class::Stalled);
    assert_eq!(
        class_of(&row(&[("agent", "idle"), ("task", "stalled")])),
        Class::Stalled
    );
    assert_eq!(class_of(&row(&[("agent", "busy")])), Class::Busy);
    assert_eq!(
        class_of(&row(&[
            ("agent", "idle"),
            ("task", "done"),
            ("task_ms", "5000"),
            ("agent_ms", "99999999")
        ])),
        Class::Done
    );
    assert_eq!(
        class_of(&row(&[
            ("agent", "idle"),
            ("task", "failed"),
            ("task_ms", "5000"),
            ("agent_ms", "99999999")
        ])),
        Class::Failed,
        "a failed task reads as failed, never as done or idle at a prompt"
    );
    for (conclusion, class) in [("failed", Class::Failed), ("finished", Class::Done)] {
        assert_eq!(
            class_of(&row(&[
                ("agent", "-"),
                ("task", "exited"),
                ("task_conclusion", conclusion),
                ("task_ms", "5000"),
            ])),
            class,
            "an exit reads as how the task ended"
        );
    }
    assert_eq!(
        class_of(&row(&[
            ("agent", "idle"),
            ("task", "failed"),
            ("task_ms", "99999999"),
            ("agent_ms", "99999999")
        ])),
        Class::Agent,
        "an old failure is no longer news"
    );
    assert_eq!(
        class_of(&row(&[("agent", "idle"), ("agent_ms", "60000")])),
        Class::Done
    );
    for task in ["working", "open"] {
        assert_eq!(
            class_of(&row(&[
                ("agent", "idle"),
                ("agent_ms", "60000"),
                ("task", task),
                ("task_ms", "60000"),
                ("bg", "1")
            ])),
            Class::Agent,
            "an idle agent whose task is {task} has not finished it"
        );
    }
    assert_eq!(
        class_of(&row(&[("agent", "idle"), ("agent_ms", "99999999")])),
        Class::Agent
    );
    assert_eq!(
        class_of(&row(&[("agent", "-"), ("detail", "targo")])),
        Class::Running
    );
    assert_eq!(
        class_of(&row(&[("agent", "-"), ("program", "sleep")])),
        Class::Running
    );
    assert_eq!(
        class_of(&row(&[
            ("agent", "-"),
            ("program", "-zsh"),
            ("program_kind", "shell")
        ])),
        Class::Shell
    );
    assert_eq!(
        class_of(&row(&[
            ("agent", "-"),
            ("program", "nu"),
            ("program_kind", "program")
        ])),
        Class::Running
    );
    assert_eq!(class_of(&row(&[("agent", "-")])), Class::Shell);
    let rank = |agent: &str, ms: &str| survey_rank([("agent", agent), ("agent_ms", ms)]);
    assert!(rank("prompt", "1") < rank("busy", "1"));
    assert!(rank("busy", "1") < rank("idle", "60000"));
    assert!(rank("idle", "60000") < rank("idle", "99999999"));
    assert_eq!(age(999), "0s");
    assert_eq!(age(125_000), "2m");
    assert_eq!(age(7_200_000), "2h");
    assert_eq!(age(200_000_000), "2d");
}

#[test]
fn a_record_naming_its_need_is_ranked_by_it() {
    use aterm_types::needs::{NeedWord, Severity};
    let row = |fields: &[(&str, &str)]| Fields::of(&wire_row(fields));
    for need in NeedWord::ALL {
        let want = match need.severity() {
            Severity::Stuck => Class::Stalled,
            Severity::Decide => Class::Asking,
        };
        assert_eq!(
            class_of(&row(&[("agent", "busy"), ("needs", need.word())])),
            want,
            "{need:?}"
        );
    }
    assert_eq!(
        class_of(&row(&[("agent", "idle"), ("needs", "lurking")])),
        Class::Asking
    );
    let quiet = |fields: &[(&str, &str)]| {
        let mut with: Vec<(&str, &str)> = fields.to_vec();
        with.push(("needs", "-"));
        (class_of(&row(&with)), class_of(&row(fields)))
    };
    assert_eq!(quiet(&[("agent", "prompt")]), (Class::Busy, Class::Asking));
    assert_eq!(
        quiet(&[("agent", "wall:usage-session")]),
        (Class::Agent, Class::Stalled)
    );
    assert_eq!(
        quiet(&[
            ("agent", "idle"),
            ("attention", "1"),
            ("agent_ms", "99999999")
        ]),
        (Class::Agent, Class::Asking)
    );
    assert_eq!(
        quiet(&[
            ("agent", "-"),
            ("task", "stalled"),
            ("program_kind", "shell")
        ]),
        (Class::Shell, Class::Stalled)
    );
}

fn peer(host: usize, level: &str, busy: usize, shells: usize) -> HostReply {
    HostReply {
        reason: None,
        label: format!("lab-{host:02}"),
        remote: true,
        unproven: false,
        node: None,
        answer: parse_reply(&peer_lines(host, level, busy, shells)),
    }
}

fn peer_lines(host: usize, level: &str, busy: usize, shells: usize) -> Vec<String> {
    let mut rows: Vec<String> = (0..busy).map(|n| busy_agent(host, n)).collect();
    rows.extend((0..shells).map(shell));
    let mut lines = vec![format!(
        "instance pid={} host=lab-{host:02} version=0.104.0 sessions={} level={level} at_ms=1",
        2000 + host,
        rows.len()
    )];
    lines.extend(rows);
    lines
}

fn unanswered(label: &str, why: &str) -> HostReply {
    HostReply {
        reason: None,
        label: label.to_owned(),
        remote: true,
        unproven: false,
        node: None,
        answer: Err(why.to_owned()),
    }
}

#[test]
fn thirty_hosts_render_within_the_budget_one_row_each() {
    let mut replies: Vec<HostReply> = (0..26)
        .map(|h| peer(h, if h % 2 == 0 { "content" } else { "facts" }, 6, 4))
        .collect();
    let reasons = [
        "nothing listens on 192.168.1.27:7100: no aterm running there, or no [fleet] listen",
        "connected, but no fleet handshake within 3.0s: an aterm stopped or overloaded, or no \
         aterm on that port",
        "another secret at 192.168.1.29:7100: the fleet rotated (join again with the new code) \
         or it left",
        "no route to 192.168.1.30:7100: off the LAN, asleep or off",
    ];
    for (n, why) in reasons.iter().enumerate() {
        replies.push(unanswered(
            &format!("lab-{:02}.example.lan:7101", 26 + n),
            why,
        ));
    }
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET, "{}", chars(&text));
    assert_line_caps(&text);
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("== ")).collect();
    assert_eq!(rows.len(), 30, "one row per machine:\n{text}");
    for n in 26..30 {
        let row = rows
            .iter()
            .find(|r| r.starts_with(&format!("== lab-{n:02}.example.lan:7101 did not answer: ")))
            .unwrap_or_else(|| panic!("lab-{n} has its row:\n{text}"));
        assert!(row.ends_with(" trust=screen"), "{row}");
    }
    assert!(
        rows.iter().any(|r| r.contains(
            " did not answer: nothing listens on 192.168.1.27:7100: no aterm \
                running there, or no [fleet] listen trust=screen"
        )),
        "a whole reason fits its line:\n{text}"
    );
    assert!(
        rows[1].starts_with("== fleet lab-01 pid 2001 0.104.0 facts · 10 sessions"),
        "{}",
        rows[1]
    );
    assert!(
        rows[0].starts_with("== fleet lab-00 pid 2000 0.104.0 · 10 sessions"),
        "{}",
        rows[0]
    );
}

#[test]
fn a_peer_header_keeps_its_counts_and_its_reply_cap() {
    let row = |sid: &str, agent: &str| {
        wire_row(&[
            ("sid", sid),
            ("program", "claude"),
            ("agent", agent),
            ("prompt", "p"),
            ("said", "s"),
        ])
    };
    let header = |label: &str, host: &str, version: &str, cut: &str| {
        let lines = vec![
            format!(
                "instance pid=77065 host={host} version={version} sessions=3 level=content \
                 at_ms=1{cut}"
            ),
            row("s-1", "busy"),
            row("s-2", "prompt"),
        ];
        let reply = HostReply {
            reason: None,
            label: label.to_owned(),
            remote: true,
            unproven: false,
            node: None,
            answer: parse_reply(&lines),
        };
        render_text(&[reply], DEFAULT_BUDGET, None)
            .lines()
            .next()
            .expect("a header")
            .to_owned()
    };
    let lan = header("192.168.1.23", "Andrews-MacBook-Pro.local", "0.105.0", "");
    assert_eq!(
        lan,
        "== fleet 192.168.1.23 (host Andrews-MacBook-Pro.local) pid 77065 0.105.0 · 2 sessions \
         (1 asking, 1 busy) · 1 not sent (reply cap) trust=screen"
    );
    let facts = " · 2 sessions (1 asking, 1 busy) · 1 not sent, 1 without excerpts (reply cap)";
    let (x, y) = (|n: usize| "x".repeat(n), |n: usize| "y".repeat(n));
    let no_version = header(&x(20), &y(20), &x(30), " cut=1");
    let cut_name = header(&x(60), &y(40), &x(40), " cut=1");
    for h in [&lan, &no_version, &cut_name] {
        assert!(
            chars(h) <= HEADER_CHARS,
            "{} > {HEADER_CHARS}: {h}",
            chars(h)
        );
        assert!(h.contains(" pid 77065"), "{h}");
    }
    for h in [&no_version, &cut_name] {
        assert!(h.ends_with(&format!("{facts} trust=screen")), "{h}");
    }
    assert_eq!(
        no_version,
        format!(
            "== fleet {} (host {}) pid 77065{facts} trust=screen",
            x(20),
            y(20)
        )
    );
    assert!(cut_name.contains("… pid 77065 ·"), "{cut_name}");
    let long = header(
        "Andrews-MacBook-Pro-long.local:57564 at 127.0.0.1",
        "Andrews-MacBook-Pro.local",
        "0.135.0",
        "",
    );
    assert!(
        long.starts_with("== fleet Andrews-MacBook-Pro-long.local:57564 at 127.0.0.1 pid 77065 ·"),
        "the label whole, its `(host …)` gone first: {long}"
    );
    let longer = header(
        &format!("{}:57564 at 127.0.0.1", x(150)),
        &y(20),
        "0.135.0",
        "",
    );
    assert!(chars(&longer) <= HEADER_CHARS, "{longer}");
    assert!(
        longer.contains("x…:57564 at 127.0.0.1 pid 77065 ·"),
        "the name gives way, never the address: {longer}"
    );
}

#[test]
fn a_peer_cannot_pass_for_this_machine() {
    let own = "s-00000000000000000001";
    let lines = vec![
        "instance pid=4190 host=my-mac version=0.104.0 sessions=1 level=content at_ms=1".to_owned(),
        wire_row(&[("sid", own), ("program", "zsh"), ("agent", "-")]),
    ];
    let from = |remote: bool| HostReply {
        reason: None,
        label: if remote {
            "lab-07 at 10.0.0.5"
        } else {
            "pid 4190"
        }
        .to_owned(),
        remote,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    };
    let peer = [from(true)];
    for text in [
        render_text(&peer, DEFAULT_BUDGET, Some(own)),
        render_full(&peer, Some(own)),
        render_sid(&peer, own, Some(own)).expect("found"),
    ] {
        assert!(
            text.starts_with("== fleet lab-07 at 10.0.0.5 (host my-mac) pid 4190 "),
            "{text}"
        );
        assert!(!text.contains("(you)"), "{text}");
    }
    let here = render_text(&[from(false)], DEFAULT_BUDGET, Some(own));
    assert!(here.starts_with("== my-mac pid 4190 "), "{here}");
    assert!(here.contains(&format!("{own} (you)")), "{here}");
    assert!(
        render_json(&peer).starts_with("[{\"label\":\"lab-07 at 10.0.0.5\",\"fleet\":true,"),
        "{}",
        render_json(&peer)
    );
    assert!(!render_json(&[from(false)]).contains("\"fleet\""));
}

#[test]
fn a_cut_reply_says_what_it_left_out() {
    let header = |sessions: usize, cut: usize| {
        let mut lines = vec![format!(
            "instance pid=7 host=lab version=0.104.0 sessions={sessions} level=content cut={cut} \
             at_ms=1"
        )];
        lines.extend((0..3).map(shell));
        let reply = HostReply {
            reason: None,
            label: "lab".into(),
            remote: false,
            unproven: false,
            node: None,
            answer: parse_reply(&lines),
        };
        render_text(&[reply], DEFAULT_BUDGET, None)
            .lines()
            .next()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        header(3, 0),
        "== lab pid 7 0.104.0 · 3 sessions trust=screen"
    );
    assert_eq!(
        header(3, 2),
        "== lab pid 7 0.104.0 · 3 sessions · 2 without excerpts (reply cap) trust=screen"
    );
    assert_eq!(
        header(5, 0),
        "== lab pid 7 0.104.0 · 3 sessions · 2 not sent (reply cap) trust=screen"
    );
    assert!(
        header(9, 1).contains(" · 6 not sent, 1 without excerpts (reply cap)"),
        "{}",
        header(9, 1)
    );
}

#[test]
fn a_sid_the_reply_cap_may_have_hidden_says_so() {
    let answered = |label: &str, header: &str, rows: Vec<String>| {
        let mut lines = vec![format!(
            "instance pid=7 host=x version=0.104.0 {header} at_ms=1"
        )];
        lines.extend(rows);
        HostReply {
            reason: None,
            label: label.into(),
            remote: true,
            unproven: false,
            node: None,
            answer: parse_reply(&lines),
        }
    };
    let whole = || {
        answered(
            "lab",
            "sessions=3 level=content cut=0",
            (0..3).map(shell).collect(),
        )
    };
    let capped = answered(
        "m7.local",
        "sessions=40 level=content cut=0",
        (3..6).map(shell).collect(),
    );
    let none = sid_not_found(&[whole()], "s-nope", true);
    let maybe = sid_not_found(&[whole(), capped], "s-nope", true);
    assert!(
        none.starts_with("ERR ") && maybe.starts_with("ERR "),
        "{none}\n{maybe}"
    );
    assert_ne!(none, maybe);
    assert!(!none.contains("lab"), "{none}");
    assert!(
        maybe.contains("37 on m7.local") && !maybe.contains("lab"),
        "{maybe}"
    );
    let frozen = || HostReply {
        reason: None,
        label: "127.0.0.1:47603".into(),
        remote: true,
        unproven: false,
        node: None,
        answer: Err("connected, but no fleet handshake within 2.0s: an aterm stopped".to_owned()),
    };
    let unserved = sid_not_found(&[frozen()], "s-nope", true);
    assert_eq!(
        unserved,
        "ERR 127.0.0.1:47603 did not answer: connected, but no fleet handshake within 2.0s: an \
         aterm stopped, so no session @s-nope was read"
    );
    let beside = sid_not_found(&[whole(), frozen()], "s-nope", true);
    assert!(
        beside.starts_with(
            "ERR 127.0.0.1:47603 did not answer: connected, but no fleet handshake within 2.0s: \
             an aterm stopped, so @s-nope was not read there; no such session @s-nope on any \
             instance that answered"
        ),
        "{beside}"
    );

    let row = wire_row(&[
        ("sid", "s-00000000000000000007"),
        ("program", "zsh"),
        ("agent", "-"),
    ]);
    let at_facts = [answered(
        "m7.local",
        "sessions=1 level=content cut=1",
        vec![row.clone()],
    )];
    let at_content = [answered(
        "m7.local",
        "sessions=1 level=content cut=0",
        vec![row],
    )];
    let cut = render_sid(&at_facts, "s-00000000000000000007", None).expect("found");
    let whole = render_sid(&at_content, "s-00000000000000000007", None).expect("found");
    assert_eq!(
        cut.lines().count(),
        whole.lines().count() + 1,
        "{cut}\n{whole}"
    );
}

#[test]
fn an_idle_shell_names_its_place_once() {
    let idle = |title: &str, cwd: &str| {
        let row = wire_row(&[
            ("sid", "s-00000000000000000003"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("title", title),
            ("cwd", cwd),
        ]);
        let survey = parse_reply(&["instance pid=1 sessions=1".to_owned(), row]).unwrap();
        session_line(&survey.sessions[0], None, false, false)
    };
    for (title, cwd) in [
        ("user@m7: ~/aterm", "~/aterm"),
        ("user@m7:~/aterm/", "~/aterm"),
        ("~/aterm", "~/aterm"),
        ("aterm", "~/aterm"),
        ("~/aterm", "/Users//example/aterm"),
    ] {
        let line = idle(title, cwd);
        assert_eq!(
            line.matches("aterm").count(),
            1,
            "{title:?} at {cwd}: {line}"
        );
    }
    let kept = idle("build", "~/aterm");
    assert!(kept.contains("| build | ~/aterm"), "{kept}");
    let other = idle("user@m7: ~/other", "~/aterm");
    assert!(
        other.contains("other") && other.contains("~/aterm"),
        "{other}"
    );
}

#[test]
fn a_session_that_rang_names_its_newest_bell() {
    let line = |bells: &str, last: &str, remote: bool| {
        let row = wire_row(&[
            ("sid", "s-00000000000000000004"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("bells", bells),
            ("bells_sounded", "1"),
            ("last_bell", last),
        ]);
        let survey = parse_reply(&["instance pid=1 sessions=1".to_owned(), row]).unwrap();
        session_line(&survey.sessions[0], None, remote, false)
    };
    let rang = line("2", "180000/sounded/zsh,190000/limited/zsh", false);
    assert!(rang.contains(" bells=2 (zsh sounded 3m ago)"), "{rang}");
    let unnamed = line("1", "4000/off/-", false);
    assert!(unnamed.contains(" bells=1 (off 4s ago)"), "{unnamed}");
    let quiet = line("0", "-", false);
    assert!(!quiet.contains("bell"), "{quiet}");
    let hostile = line("1", "4000/sounded/a b=c", true);
    assert!(hostile.contains(" bells=1 (? sounded 4s ago)"), "{hostile}");
}

/// Another Mac's session is shown by its tab ID in either form: a tab opened before the tab-ID
/// update keeps its `s-` ID until it closes. NEGATIVE CONTROL: a malformed ID of either form
/// from a peer is shown as `?`.
#[test]
fn a_peers_session_is_shown_by_its_tab_id_in_either_form() {
    let line = |sid: &str| {
        let row = wire_row(&[
            ("sid", sid),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
        ]);
        let survey = parse_reply(&["instance pid=1 sessions=1".to_owned(), row]).unwrap();
        session_line(&survey.sessions[0], None, true, false)
    };
    for sid in ["tqn5r6e0148", "s-0123456789abcdef0123"] {
        let shown = line(sid);
        assert!(shown.starts_with(sid), "{shown}");
    }
    for bad in ["tqn5r6e014", "tqn5r6e014i", "s-0123456789abcdeg0123"] {
        let shown = line(bad);
        assert!(
            shown.starts_with('?') && !shown.contains(bad),
            "{bad}: {shown}"
        );
    }
}

#[test]
fn an_old_bell_leaves_the_survey_line() {
    let line = |bells: Option<&str>| {
        let mut fields = vec![
            ("sid", "s-00000000000000000004"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "idle"),
            ("agent_ms", "86400000"),
            ("ctx_left_pct", "42"),
            ("title", "Land the survey bell fix"),
            ("cwd", "~/aterm"),
            ("branch", "main"),
            ("model", "opus-4.8"),
            ("effort", "high"),
        ];
        if let Some(last) = bells {
            fields.extend([
                ("bells", "37"),
                ("bells_sounded", "12"),
                ("last_bell", last),
            ]);
        }
        let row = wire_row(&fields);
        let survey = parse_reply(&["instance pid=1 sessions=1".to_owned(), row]).unwrap();
        session_line(&survey.sessions[0], None, false, false)
    };
    let quiet = line(None);
    assert!(
        quiet.ends_with(" ctx_left=42% | Land the survey bell fix | ~/aterm main | opus-4.8 high"),
        "{quiet}"
    );
    let hours_ago = line(Some("10800000/sounded/claude,10900000/limited/claude"));
    assert_eq!(hours_ago, quiet, "a bell 3h old is off the line");
    let at_the_edge = line(Some(&format!("{NEWLY_DONE_MS}/sounded/claude")));
    assert_eq!(at_the_edge, quiet, "a bell exactly 15 min old is off it");
    let unaged = line(Some("-"));
    assert_eq!(unaged, quiet, "a count with no aged bell says nothing");
    let new = line(Some("180000/sounded/claude"));
    assert!(new.contains(" bells=37 (claude sounded 3m ago)"), "{new}");
    assert!(!new.contains("opus-4.8"), "{new}");
    let just_new = line(Some(&format!("{}/off/claude", NEWLY_DONE_MS - 1000)));
    assert!(
        just_new.contains(" bells=37 (claude off 14m ago)"),
        "{just_new}"
    );
}

#[test]
fn an_unanswered_line_keeps_its_label() {
    let long = "x".repeat(400);
    let line = unanswered_line("lab-99.local", &format!("{long} trust=human"));
    assert_eq!(chars(&line), UNANSWERED_CHARS, "{line}");
    assert!(
        line.starts_with("== lab-99.local did not answer: xxx"),
        "{line}"
    );
    assert!(line.ends_with("… trust=screen"), "{line}");
    let hostile = unanswered_line("m7\u{1b}[2K trust=human", "no answer within 3.0s");
    assert_eq!(hostile.matches("trust=").count(), 1, "{hostile:?}");
    assert!(!hostile.contains('\u{1b}'), "{hostile:?}");
    let remedy = ": run `aterm fleet join` there";
    let why = format!(
        "{}{remedy}",
        "r".repeat(UNANSWERED_REASON_CHARS - chars(remedy))
    );
    assert_eq!(chars(&why), UNANSWERED_REASON_CHARS);
    let name = "Andrews-MacBook-Pro-of-the-third-floor-lab.example.lan";
    let line = unanswered_line(name, &why);
    assert_eq!(chars(&line), UNANSWERED_CHARS, "{line}");
    assert!(
        line.ends_with(&format!(" did not answer: {why} trust=screen")),
        "{line}"
    );
    assert!(
        line.starts_with("== Andrews-MacBook-Pro-of-… did not answer: "),
        "{line}"
    );
    let line = unanswered_line(&format!("{name}:57564"), &why);
    assert!(
        line.starts_with("== Andrews-MacBook-P…:57564 did not answer: "),
        "the port it dialled stays whole: {line}"
    );
    for ula in [
        "fd12:3456:789a:1:1c2b:3d4e:5f60:7182",
        "[fd12:3456:789a:1:1c2b:3d4e:5f60:7182]:7101",
        "lab-mini.local:7101 at fd12:3456:789a:1:1c2b:3d4e:5f60:7182",
    ] {
        let line = unanswered_line(ula, &why);
        assert!(
            chars(&line) <= UNANSWERED_CHARS,
            "{} > cap: {line}",
            chars(&line)
        );
        assert!(
            line.contains(":5f60:7182") && line.ends_with("… trust=screen"),
            "the address stays whole and the reason gives way: {line}"
        );
    }
}

#[test]
fn no_budget_is_ever_passed() {
    let replies: Vec<HostReply> = (0..18)
        .map(|h| reply(h, (0..10).map(|n| busy_agent(h, n)).collect()))
        .chain([unanswered("lab-x", "no answer within 3.0s")])
        .collect();
    for budget in [1, 2, 3, 10, 40, 99, 100, 101, 150, 161, 300, 600, 2_000] {
        let text = render_text(&replies, budget, None);
        assert!(
            chars(&text) <= budget,
            "{budget}: {} {text:?}",
            chars(&text)
        );
    }
}

#[test]
fn the_read_leads_with_its_data_line_within_the_budget() {
    let replies: Vec<HostReply> = (0..18)
        .map(|h| reply(h, (0..10).map(|n| busy_agent(h, n)).collect()))
        .collect();
    let head = chars(DATA_LINE) + 1;
    let blocks = blocks_of(&replies, None, false);
    let least = render(&blocks, &Shown::filled(&blocks, &[], 0))
        .lines()
        .count();
    let facts: Vec<HostReply> = (0..9).map(|h| peer(h, "facts", 1, 0)).collect();
    let note_sets = [
        Vec::new(),
        vec!["n".repeat(49)],
        vec![NO_ROUTE_NOTE.to_owned(), facts_note(&facts).expect("named")],
    ];
    for notes in &note_sets {
        let reserved: usize = notes.iter().map(|n| chars(n) + 1).sum();
        for budget in [
            1,
            2,
            40,
            100,
            200,
            300,
            head - 1,
            head,
            head + 1,
            head + 200,
            head + reserved + 200,
            8_000,
            DEFAULT_BUDGET,
        ] {
            let read = render_read(&replies, budget, None, notes, None);
            assert!(
                chars(&read) <= budget,
                "{budget}: {} {read:?}",
                chars(&read)
            );
            let fitted = budget
                .checked_sub(reserved + head)
                .map(|rest| render_within(&blocks, rest, budget))
                .filter(|fitted| fitted.quoted);
            match fitted {
                Some(fitted) => {
                    assert_eq!(read.lines().next(), Some(DATA_LINE), "{budget}");
                    let mut want = fitted.text.clone();
                    for note in notes {
                        want.push_str(note);
                        want.push('\n');
                    }
                    assert_eq!(read[DATA_LINE.len() + 1..], want, "{budget}");
                }
                None => {
                    assert!(
                        !read.contains(DATA_LINE) && !read.contains("== "),
                        "{read:?}"
                    );
                    let whole = closing_line(least, budget, false);
                    let mut lines = read.lines();
                    let first = lines.next().unwrap_or_default();
                    assert!(
                        whole.starts_with(first.trim_end_matches('…')),
                        "{budget}: {read:?}"
                    );
                    for (line, note) in lines.zip(notes) {
                        assert!(
                            note.starts_with(line.trim_end_matches('…')),
                            "{budget}: {read:?}"
                        );
                    }
                    if budget > chars(&whole) {
                        assert_eq!(first, whole, "{budget}: the closing line first, whole");
                    }
                }
            }
            if read.contains(" more lines past the ") && read.contains(" for all") {
                assert!(
                    read.contains(&format!(" more lines past the {budget}-char budget")),
                    "{budget}: {read:?}"
                );
            }
        }
    }
    assert_eq!(
        render_read(&replies, 100, None, &[], None),
        format!("{}\n", closing_line(least, 100, false))
    );
    let cut = render_read(&replies, head + 600, None, &[], None);
    assert!(cut.starts_with(DATA_LINE), "{cut}");
    assert!(
        cut.contains(&format!(" more lines past the {}-char budget", head + 600)),
        "{cut}"
    );
    assert!(with_data_line("== x\n").starts_with(&format!("{DATA_LINE}\n== x")));
    assert!(!DATA_LINE.starts_with("== ") && !DATA_LINE.starts_with("s-"));
}

#[test]
fn an_agent_idle_for_hours_keeps_its_excerpts_until_the_budget_is_tight() {
    let resting = wire_row(&[
        ("sid", "s-00000000000000000009"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "7200000"),
        ("prompt", "Summarise the flaky tests"),
        ("said", "Three are flaky; the list is in notes.md"),
    ]);
    let replies = [reply(8, vec![resting, busy_agent(8, 1)])];
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("s-00000000000000000009 claude idle 2h"))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(
        lines[at + 1..at + 3],
        [
            "    asked: Summarise the flaky tests",
            "    said: Three are flaky; the list is in notes.md"
        ],
        "{text}"
    );
    assert!(!says_it_left_out(&text), "{text}");

    let long = wire_row(&[
        ("sid", "s-00000000000000000009"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "7200000"),
        (
            "prompt",
            &excerpt400(&"Summarise the flaky tests ".repeat(20)),
        ),
        ("said", &excerpt400(&"Three are flaky. ".repeat(30))),
    ]);
    let replies = [reply(8, vec![long, busy_agent(8, 1)])];
    let tight = render_text(&replies, chars(&narrowest(&replies)) - 1, None);
    assert!(!tight.contains("    asked: Summarise"), "{tight}");
    assert!(tight.contains("    said: …ky. Three are flaky."), "{tight}");
    assert!(tight.contains("    asked: word word"), "{tight}");
    assert!(tight.contains("    said: word word"), "{tight}");
    assert!(
        tight.contains(&format!(
            "s-00000000000000000009 claude idle 2h {EXCERPTS_CUT}"
        )),
        "{tight}"
    );
    assert!(tight.ends_with(&left_out_line(0, 1, 0, false)), "{tight}");
}

fn narrowest(replies: &[HostReply]) -> String {
    let blocks = blocks_of(replies, None, false);
    render(&blocks, &Shown::all(&blocks, Class::Agent, EXCERPT_CHARS))
}

fn done_worker(n: usize, said: &str) -> String {
    wire_row(&[
        ("local", &n.to_string()),
        ("sid", &format!("s-dddd{n:016x}")),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "4000"),
        ("task", "done"),
        ("task_ms", "4000"),
        (
            "prompt",
            "Write notes.md on TCP slow start, about 120 words, then run wc -w",
        ),
        ("said", said),
    ])
}

fn excerpt_rows(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.starts_with("    ")).collect()
}

#[test]
fn a_read_that_fits_shows_every_excerpt_whole() {
    let report = format!(
        "{} `wc -w notes.md` printed **142**, above the ~120 you asked for.",
        "Created notes.md with an explanation of TCP slow start; it covers the window. ".repeat(3)
    );
    assert!(report.chars().count() > EXCERPT_CHARS && report.len() <= 400);
    let replies = [
        reply(0, vec![done_worker(1, &report), busy_agent(0, 2), shell(3)]),
        reply(1, vec![done_worker(4, &report), shell(5)]),
    ];
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    assert!(chars(&text) <= DEFAULT_BUDGET);
    assert_line_caps(&text);
    let rows = excerpt_rows(&text);
    assert_eq!(rows.len(), 6, "{text}");
    assert!(
        rows.iter().all(|l| !l.contains('…')),
        "none is cut:
{text}"
    );
    let saids: Vec<&&str> = rows
        .iter()
        .filter(|l| l.contains("printed **142**"))
        .collect();
    assert_eq!(
        saids.len(),
        2,
        "each worker's figure is shown:
{text}"
    );
    assert!(!says_it_left_out(&text), "{text}");
}

fn is_cut(row: &str) -> bool {
    row.ends_with('…')
        || row
            .split_once(": ")
            .is_some_and(|(_, words)| words.starts_with('…'))
}

#[test]
fn excerpts_narrow_alike_then_go_least_needed_first() {
    let resting = wire_row(&[
        ("sid", "s-00000000000000000009"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "7200000"),
        ("prompt", &excerpt400(&"rest ".repeat(80))),
        ("said", &excerpt400(&"rest ".repeat(80))),
    ]);
    let replies: Vec<HostReply> = (0..3)
        .map(|h| reply(h, vec![resting.clone(), busy_agent(h, 1), busy_agent(h, 2)]))
        .collect();
    let blocks = blocks_of(&replies, None, false);
    let whole = chars(&render_text(&replies, usize::MAX, None));
    let narrowest = chars(&narrowest(&replies));
    let all = excerpt_rows(&render_text(&replies, usize::MAX, None)).len();
    assert!(narrowest < whole);
    let mut last_width = usize::MAX;
    let mut narrowed = 0;
    let (mut end_kept, mut over_the_floor) = (false, false);
    for budget in (narrowest..=whole).rev().step_by(97) {
        let text = render_text(&replies, budget, None);
        assert!(chars(&text) <= budget, "{budget}: {}", chars(&text));
        let rows = excerpt_rows(&text);
        assert_eq!(rows.len(), all, "{budget}: every excerpt still shows");
        end_kept |= rows.iter().any(|l| l.starts_with("    said: …"));
        let cut: Vec<usize> = rows
            .iter()
            .filter(|l| is_cut(l))
            .map(|l| chars(l))
            .collect();
        if let Some(&width) = cut.first() {
            narrowed += 1;
            assert!(
                cut.iter().all(|w| *w == width),
                "{budget}: one width {cut:?}"
            );
            assert!(
                width >= EXCERPT_CHARS && width <= last_width,
                "{budget}: {width}"
            );
            let wider = Shown::all(&blocks, Class::Agent, width + 1);
            assert!(
                width == EXCERPT_WIDE_CHARS || chars(&render(&blocks, &wider)) > budget,
                "{budget}: {width} is not the widest that fits"
            );
            over_the_floor |= width > EXCERPT_CHARS;
            last_width = width;
        }
        assert!(!says_it_left_out(&text), "{budget}: {text}");
    }
    assert!(narrowed > 3, "the sweep crossed the narrowing: {narrowed}");
    assert!(over_the_floor, "some budget narrows above the floor");
    assert!(end_kept, "the resting agent's said kept its end");
    let tight = render_text(&replies, narrowest - 1, None);
    assert!(chars(&tight) < narrowest);
    let said = |text: &str, words: &str| {
        text.lines()
            .filter(|l| l.starts_with(&format!("    said: {words}")))
            .count()
    };
    let asked = |text: &str, words: &str| {
        text.lines()
            .filter(|l| l.starts_with(&format!("    asked: {words}")))
            .count()
    };
    assert_eq!(
        asked(&tight, "rest"),
        2,
        "the last resting agent's went first: {tight}"
    );
    assert_eq!(said(&tight, "word"), 6, "the busy agents' stay: {tight}");
    let last = tight.rsplit("\n== ").next().unwrap();
    assert!(
        last.lines()
            .any(|l| l.starts_with("s-00000000000000000009") && l.contains(EXCERPTS_CUT)),
        "{tight}"
    );
    assert!(
        excerpt_rows(&tight)
            .iter()
            .all(|l| chars(l) <= EXCERPT_CHARS)
    );
    assert!(tight.ends_with(&left_out_line(0, 1, 0, false)), "{tight}");
}

#[test]
fn a_finished_turns_said_keeps_its_end_when_cut() {
    let report = format!(
        "Created notes.md on TCP slow start. {}`wc -w notes.md` printed **142**.",
        "It covers the congestion window and ssthresh. ".repeat(6)
    );
    assert!(!report.ends_with('…') && report.len() <= 400);
    let server_cut = format!(
        "Created notes.md on TCP slow start. {}…",
        "Words. ".repeat(50)
    );
    let replies = [reply(
        0,
        vec![
            done_worker(1, &report),
            done_worker(2, &server_cut),
            busy_agent(0, 3),
        ],
    )];
    let text = render_text(&replies, chars(&narrowest(&replies)), None);
    let rows = excerpt_rows(&text);
    let said = |sid: &str| {
        let at = text.lines().position(|l| l.starts_with(sid)).unwrap();
        text.lines()
            .skip(at + 1)
            .take_while(|l| l.starts_with("    "))
            .find(|l| l.starts_with("    said: "))
            .unwrap_or_else(|| {
                panic!(
                    "{sid}:
{text}"
                )
            })
            .to_owned()
    };
    let kept_end = said("s-dddd0000000000000001");
    assert!(kept_end.starts_with("    said: …"), "{kept_end}");
    assert!(kept_end.ends_with("printed **142**."), "{kept_end}");
    assert_eq!(chars(&kept_end), EXCERPT_CHARS, "{kept_end}");
    let head = said("s-dddd0000000000000002");
    assert!(head.starts_with("    said: Created notes.md"), "{head}");
    let busy = said("s-0000");
    assert!(busy.starts_with("    said: word word"), "{busy}");
    assert!(
        rows.iter()
            .filter(|l| l.starts_with("    asked: "))
            .all(|l| !l.starts_with("    asked: …")),
        "{text}"
    );
}

#[test]
fn a_raised_attention_line_is_shown() {
    let row = |attention: &str| {
        wire_row(&[
            ("sid", "s-00000000000000000005"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("attention", attention),
        ])
    };
    let text = render_text(
        &[reply(6, vec![row("deploy waits on you \u{1b}[2K")])],
        DEFAULT_BUDGET,
        None,
    );
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains(" (1 asking)"), "{text}");
    assert!(lines[1].contains(" zsh idle attention"), "{text}");
    assert_eq!(
        lines[2], "    attention: deploy waits on you \\x1b[2K",
        "{text}"
    );

    let facts = parse_reply(&[
        "instance pid=7 host=m7 version=0.104.0 sessions=1 level=facts at_ms=1".to_owned(),
        row("1"),
    ]);
    let text = render_text(
        &[HostReply {
            reason: None,
            label: "m7".into(),
            remote: true,
            unproven: false,
            node: None,
            answer: facts,
        }],
        DEFAULT_BUDGET,
        None,
    );
    assert!(text.contains(" zsh idle attention"), "{text}");
    assert!(!text.contains("attention:"), "{text}");

    let quiet = render_text(&[reply(6, vec![row("-")])], DEFAULT_BUDGET, None);
    assert!(!quiet.contains("attention"), "{quiet}");
}

#[test]
fn an_agent_at_a_box_nobody_presses_reads_asking() {
    let row = |agent: &str, needs: Option<&str>| {
        let mut fields = vec![
            ("sid", "s-00000000000000000006"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", agent),
            ("agent_ms", "4000"),
            ("doing", "Bash"),
        ];
        fields.extend(needs.map(|needs| ("needs", needs)));
        wire_row(&fields)
    };
    for needs in [None, Some("approval")] {
        let text = render_text(
            &[reply(7, vec![row("prompt", needs)])],
            DEFAULT_BUDGET,
            None,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains(" (1 asking)"), "{needs:?}: {text}");
        assert!(
            lines[1].starts_with("s-00000000000000000006 claude asking 4s Bash"),
            "{needs:?}: {text}"
        );
    }
    let pressed = render_text(
        &[reply(7, vec![row("prompt", Some("-"))])],
        DEFAULT_BUDGET,
        None,
    );
    let lines: Vec<&str> = pressed.lines().collect();
    assert!(lines[0].contains(" (1 busy)"), "{pressed}");
    assert!(
        lines[1].starts_with("s-00000000000000000006 claude prompt 4s Bash"),
        "{pressed}"
    );
    let claimed = wire_row(&[
        ("sid", "s-00000000000000000006"),
        ("program", "codex"),
        ("program_kind", "agent"),
        ("agent", "prompt"),
        ("agent_ms", "4000"),
        ("needs", "approval"),
        ("needs_by", "program"),
    ]);
    let text = render_text(&[reply(7, vec![claimed])], DEFAULT_BUDGET, None);
    assert!(
        text.lines().nth(1).is_some_and(
            |line| line.starts_with("s-00000000000000000006 codex asking (its own report) 4s")
        ),
        "a need codex's own report raised says so: {text}"
    );
    for word in ["busy", "idle", "question", "wall:usage", "unknown"] {
        for rank in 0..=6 {
            assert_eq!(agent_word_shown(word, rank), word);
        }
        let text = render_text(&[reply(7, vec![row(word, None)])], DEFAULT_BUDGET, None);
        assert!(text.contains(&format!(" claude {word} 4s")), "{text}");
    }
}

#[test]
fn context_reads_as_what_is_left() {
    let row = wire_row(&[
        ("sid", "s-00000000000000000003"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "99999999"),
        ("ctx_left_pct", "5"),
    ]);
    let text = render_text(&[reply(4, vec![row])], DEFAULT_BUDGET, None);
    assert!(text.contains(" ctx_left=5%"), "{text}");
}

/// The plan's windows follow the context left; a window the row does not know (`-`) is not
/// shown.
#[test]
fn the_plans_windows_read_as_what_is_used() {
    let text = |five: &str, seven: &str| {
        let row = wire_row(&[
            ("sid", "s-00000000000000000003"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "busy"),
            ("agent_ms", "1000"),
            ("ctx_left_pct", "64"),
            ("usage_5h_pct", five),
            ("usage_7d_pct", seven),
        ]);
        render_text(&[reply(4, vec![row])], DEFAULT_BUDGET, None)
    };
    let both = text("7", "1");
    assert!(
        both.contains(" ctx_left=64% 5h_left=93% 7d_left=99%"),
        "{both}"
    );
    let weekly = text("-", "12");
    assert!(weekly.contains(" ctx_left=64% 7d_left=88%"), "{weekly}");
    assert!(!weekly.contains("5h="), "{weekly}");
}

#[test]
fn one_machine_reads_in_full() {
    let idle = wire_row(&[
        ("sid", "s-00000000000000000004"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "99999999"),
        ("said", "All green."),
    ]);
    let mut rows: Vec<String> = (0..12).map(shell).collect();
    rows.push(idle);
    let replies = [reply(5, rows)];
    let full = render_full(&replies, None);
    assert_eq!(
        full.lines().filter(|l| l.starts_with("s-")).count(),
        13,
        "{full}"
    );
    assert!(full.contains("\n    said: All green.\n"), "{full}");
    assert!(!full.contains("  +"), "{full}");
    assert_eq!(render_text(&replies, DEFAULT_BUDGET, None), full);
    let budgeted = render_text(&replies, chars(&full) - 1, None);
    assert!(budgeted.contains("\n    said: All green.\n"), "{budgeted}");
    let shells = budgeted.lines().filter(|l| l.starts_with("s-ffff")).count();
    let more = folded(&budgeted);
    assert!(more > 0 && shells + more == 12, "{budgeted}");
    assert!(
        budgeted.ends_with(&format!(
            "\n  +{more} idle shells\n{}",
            left_out_line(more, 0, 0, false)
        )),
        "{budgeted}"
    );
}

#[test]
fn one_cause_that_refused_every_peer_is_said_once() {
    let peer = |fault: PeerFault| PeerSurvey {
        authenticated: None,
        reason: None,
        label: "m7".to_owned(),
        answer: Err("no route to 10.0.0.7".to_owned()),
        fault,
        unproven: false,
        node: None,
        addr: None,
    };
    use PeerFault::{NoNetwork, NoRoute, NotDialled, NotPermitted, Other};
    let note =
        |faults: &[PeerFault]| fleet_note(&faults.iter().map(|f| peer(*f)).collect::<Vec<_>>());
    assert_eq!(note(&[]), None);
    assert_eq!(note(&[NoRoute, NoRoute]), Some(NO_ROUTE_NOTE));
    assert_eq!(note(&[NoRoute, NotDialled]), Some(NO_ROUTE_NOTE));
    assert_eq!(note(&[NoNetwork, NoNetwork]), Some(NO_NETWORK_NOTE));
    assert_eq!(
        note(&[NotPermitted, NotDialled, NotPermitted]),
        Some(SANDBOX_NOTE)
    );
    for none in [
        &[NoRoute, Other][..],
        &[NoRoute, NoNetwork],
        &[NotPermitted, Other],
        &[NotDialled, NotDialled],
    ] {
        assert_eq!(note(none), None, "{none:?}");
    }
    let answered = PeerSurvey {
        authenticated: None,
        answer: Ok(vec!["instance pid=1".to_owned()]),
        ..peer(Other)
    };
    assert_eq!(fleet_note(&[peer(NoRoute), answered]), None);
}

/// THE ON-LINK RULE IN A FLEET READ's NOTE: a no-route whose address the rule could place (on
/// this Mac's network, off the link now, or off it) is that member's own, which its row says, so
/// the read takes no every-peer note, at home and on a remembered network alike; a refusal with no
/// address keeps the every-peer note.
/// NEGATIVE CONTROL: dropping `fleet_note_at`'s placed return keeps the every-peer note for placed
/// refusals, and this test fails at "a member on the ALab LAN refused: its own".
#[test]
fn a_fleet_reads_no_route_is_judged_on_the_link() {
    let refused = |addr: &str| PeerSurvey {
        authenticated: None,
        reason: None,
        label: format!("m at {addr}"),
        answer: Err("no route".to_owned()),
        fault: PeerFault::NoRoute,
        unproven: false,
        node: None,
        addr: Some(addr.to_owned()),
    };
    fn utc(_: i64) -> Option<i64> {
        Some(0)
    }
    let words = aterm_netplace::words::Words {
        now_ms: 1_800_000_060_000,
        zone: &utc,
    };
    let home = aterm_netplace::sample::home(1_800_000_000_000, 0);
    assert_eq!(
        fleet_note_at(&[refused("192.168.5.20")], Some(&home), &words),
        None,
        "a member on the ALab LAN refused: its own"
    );
    assert_eq!(
        fleet_note_at(
            &[refused("10.9.9.9"), refused("10.9.9.8")],
            Some(&home),
            &words
        ),
        None,
        "refusals off this network: their own"
    );
    let once = aterm_netplace::Place {
        once_alab: true,
        at: aterm_netplace::Where::Remote(aterm_netplace::Kind::Lan),
        kind: Some(aterm_netplace::Kind::Lan),
        ..home.clone()
    };
    assert_eq!(
        fleet_note_at(&[refused("10.9.9.9")], Some(&once), &words),
        None,
        "a network members were once on: their own"
    );
    let unplaced = PeerSurvey {
        authenticated: None,
        addr: None,
        ..refused("0.0.0.0")
    };
    assert_eq!(
        fleet_note_at(&[unplaced], Some(&home), &words).as_deref(),
        Some(NO_ROUTE_NOTE),
        "no address to judge: every peer refused"
    );
}

/// Away from the ALab LAN (a phone hotspot), explicit route failures may say where this Mac was
/// observed. Unknown failures retain no inferred network cause; a sandbox that refused every
/// connect is still the sandbox, a read a peer answered takes no note, and on the ALab LAN the
/// no-route note stands.
/// NEGATIVE CONTROL: returning `fleet_note`'s note whatever the place prints the every-peer note
/// for the hotspot's no-route read, and this test fails at "no-route away".
#[test]
fn a_fleet_read_from_another_network_says_where_this_mac_is() {
    let peer = |fault: PeerFault| PeerSurvey {
        authenticated: None,
        reason: None,
        label: "m7".to_owned(),
        answer: Err("timed out".to_owned()),
        fault,
        unproven: false,
        node: None,
        addr: None,
    };
    fn utc(_: i64) -> Option<i64> {
        Some(0)
    }
    let words = aterm_netplace::words::Words {
        now_ms: 1_800_000_060_000,
        zone: &utc,
    };
    let away = aterm_netplace::sample::hotspot(1_800_000_000_000, 1_800_000_000_000);
    let said = aterm_netplace::words::survey_note(&away, &words);
    assert!(said.is_some());
    use PeerFault::{NoNetwork, NoRoute, NotPermitted, Other};
    let note = |faults: &[PeerFault], place: &aterm_netplace::Place| {
        fleet_note_at(
            &faults.iter().map(|f| peer(*f)).collect::<Vec<_>>(),
            Some(place),
            &words,
        )
    };
    assert_eq!(note(&[NoRoute, NoRoute], &away), said, "no-route away");
    assert_eq!(
        note(&[NoRoute, NoNetwork], &away),
        said,
        "known network failures away"
    );
    assert_eq!(note(&[Other, Other], &away), None, "unknown failures away");
    assert_eq!(
        note(&[NotPermitted, NotPermitted], &away).as_deref(),
        Some(SANDBOX_NOTE)
    );
    let answered = PeerSurvey {
        authenticated: None,
        answer: Ok(vec!["instance pid=1".to_owned()]),
        ..peer(Other)
    };
    assert_eq!(
        fleet_note_at(&[peer(Other), answered], Some(&away), &words),
        None
    );
    let home = aterm_netplace::sample::home(1_800_000_000_000, 0);
    assert_eq!(
        note(&[NoRoute, NoRoute], &home).as_deref(),
        Some(NO_ROUTE_NOTE)
    );
    assert_eq!(note(&[Other, Other], &home), None);
}

/// A remembered away network cannot override an actual refusal, protocol error or unknown
/// failure. Treating every unsuccessful dial as evidence of that network fails these controls.
#[test]
fn observed_fleet_failures_keep_their_cause_over_remembered_network_place() {
    fn utc(_: i64) -> Option<i64> {
        Some(0)
    }
    let words = aterm_netplace::words::Words {
        now_ms: 1_800_000_060_000,
        zone: &utc,
    };
    let away = aterm_netplace::sample::hotspot(1_800_000_000_000, 1_800_000_000_000);
    assert!(aterm_netplace::words::survey_note(&away, &words).is_some());
    for (fault, reason, error) in [
        (
            PeerFault::Other,
            "connect",
            "local transport refused its caller",
        ),
        (
            PeerFault::HostAnswered,
            "handshake",
            "peer refused authentication",
        ),
        (
            PeerFault::HostAnswered,
            "reply",
            "reply could not be decoded",
        ),
        (PeerFault::Unserved, "handshake", "handshake timed out"),
        (
            PeerFault::Other,
            "connect",
            "connection failed for an unknown cause",
        ),
    ] {
        let peer = PeerSurvey {
            authenticated: None,
            label: "m3".into(),
            answer: Err(error.into()),
            fault,
            unproven: false,
            node: None,
            addr: Some("192.168.1.121".into()),
            reason: Some(reason),
        };
        assert_eq!(
            fleet_note_at(std::slice::from_ref(&peer), Some(&away), &words),
            None
        );
        let reply = peer_reply(peer.clone());
        let json: aterm_json::Value =
            aterm_json::from_str(&render_json(std::slice::from_ref(&reply))).unwrap();
        assert_eq!(json[0]["error"].as_str(), Some(error));
        assert_eq!(json[0]["reason"].as_str(), Some(reason));
        assert!(render_full(&[reply], None).contains(error));
        let no_route = PeerSurvey {
            fault: PeerFault::NoRoute,
            ..peer.clone()
        };
        assert_eq!(fleet_note_at(&[no_route, peer], Some(&away), &words), None);
    }
}

#[test]
fn the_peers_that_allow_facts_alone_are_named() {
    let at = |host: usize, level: &str| peer(host, level, 1, 0);
    assert_eq!(
        facts_note(&[at(0, "content"), reply(1, vec![shell(1)])]),
        None
    );
    let one = facts_note(&[at(0, "content"), at(3, "facts")]).expect("named");
    assert!(one.contains("lab-03") && !one.contains("lab-00"), "{one}");
    let many: Vec<HostReply> = (0..9).map(|h| at(h, "facts")).collect();
    let many = facts_note(&many).expect("named");
    for named in 0..6 {
        assert!(many.contains(&format!("lab-{named:02}")), "{many}");
    }
    for counted in 6..9 {
        assert!(!many.contains(&format!("lab-{counted:02}")), "{many}");
    }
    assert!(many.contains("3 more"), "{many}");
    let mut long = at(1, "facts");
    long.label = "Andrews-MacBook-Pro-of-the-third-floor-lab.local:57564 at 127.0.0.1".to_owned();
    let long = facts_note(&[long]).expect("named");
    assert!(
        long.starts_with("Andrews-MacBook-Pro-of-the-t…:57564 at 127.0.0.1 allows"),
        "{long}"
    );
}

#[test]
fn a_peer_row_cannot_forge_a_header_or_the_you_mark() {
    let rows = vec![
        wire_row(&[
            ("sid", "== fleet owner-mac pid 1"),
            ("program", "claude"),
            ("agent", "busy"),
        ]),
        wire_row(&[
            ("sid", "s-1 (you)"),
            ("program", "zsh (you)"),
            ("agent", "-"),
        ]),
        wire_row(&[
            ("sid", "s-2"),
            ("program", "claude"),
            ("agent", "idle (you)"),
            ("task", "done x=1"),
        ]),
        wire_row(&[
            ("sid", "s-3"),
            ("program", "claude"),
            ("agent", "busy"),
            ("doing", "mcp__docs__batch"),
        ]),
        wire_row(&[
            ("sid", "s-4"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("detail", "run_tests.sh"),
        ]),
    ];
    let mut lines =
        vec!["instance pid=7 host=lab version=0.105.0 sessions=5 level=facts at_ms=1".to_owned()];
    lines.extend(rows);
    let from = |remote: bool| HostReply {
        reason: None,
        label: "lab-07".to_owned(),
        remote,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    };
    let peer = render_text(&[from(true)], DEFAULT_BUDGET, Some("s-1"));
    assert_eq!(
        peer.lines().filter(|l| l.starts_with("== ")).count(),
        1,
        "{peer}"
    );
    assert!(!peer.contains("(you)"), "{peer}");
    assert!(peer.contains("\n? claude busy"), "{peer}");
    assert!(
        peer.contains("\n? running ?") || peer.contains("\n? ? idle"),
        "{peer}"
    );
    assert!(peer.contains("\ns-2 claude ? task=?/0"), "{peer}");
    assert!(
        peer.contains("\ns-3 claude busy mcp__docs__batch"),
        "{peer}"
    );
    assert!(peer.contains("\ns-4 running run_tests.sh"), "{peer}");
    let here = render_text(&[from(false)], DEFAULT_BUDGET, None);
    assert!(
        here.contains("\n== fleet owner-mac pid 1 claude busy"),
        "{here}"
    );
}

const STUB_NOTE: &str = "stub-note-0d6b: the read's closing line";

fn stub_fleet(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Ok(FleetRead {
        peers: vec![PeerSurvey {
            authenticated: None,
            reason: None,
            label: "lab-01".to_owned(),
            answer: Ok(peer_lines(1, "content", 12, 4)),
            fault: PeerFault::Other,
            unproven: false,
            node: None,
            addr: None,
        }],
        note: Some(STUB_NOTE.to_owned()),
        alone: None,
    })
}

const ALONE: &str = "survey --fleet read no other machine: stub-alone-5c1e";

fn no_peer(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Ok(FleetRead {
        peers: Vec::new(),
        note: None,
        alone: Some(ALONE.to_owned()),
    })
}

#[test]
fn a_fleet_read_that_asked_no_other_machine_says_why_on_stderr() {
    let parts = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    let here = || Ok(vec![reply(0, vec![shell(2)])]);
    for form in [&[][..], &["--json"]] {
        let mut words = vec!["survey", "--fleet"];
        words.extend_from_slice(form);
        let printed = read_survey(
            &parse_args(&parts(&words)).unwrap(),
            Some(no_peer),
            here,
            None,
        );
        assert_eq!(
            (printed.code, &printed.err[..]),
            (0, &[ALONE.to_owned()][..]),
            "{form:?}"
        );
        let out = printed.out.expect("this machine's block");
        assert!(
            !out.contains(ALONE) && out.contains("1000"),
            "{form:?}: {out}"
        );
    }
}

#[test]
fn the_fleet_clients_closing_line_ends_the_read_within_its_budget() {
    let parts = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    let here = || Ok(vec![reply(0, vec![busy_agent(0, 1), shell(2)])]);
    for budget in (300..8_000).step_by(23).chain([DEFAULT_BUDGET]) {
        let args = parse_args(&parts(&[
            "survey",
            "--fleet",
            "--budget",
            &budget.to_string(),
        ]))
        .unwrap();
        let printed = read_survey(&args, Some(stub_fleet), here, None);
        assert_eq!((printed.code, printed.err.len()), (0, 0), "{printed:?}");
        let text = printed.out.expect("the text");
        assert_eq!(text.lines().last(), Some(STUB_NOTE), "{budget}: {text}");
        assert!(chars(&text) <= budget, "{} > {budget}", chars(&text));
    }

    let sid = format!("s-{:04x}{:016x}", 1, 3);
    for form in [&["--json"][..], &["--sid", &sid]] {
        let mut words = vec!["survey", "--fleet"];
        words.extend_from_slice(form);
        let printed = read_survey(
            &parse_args(&parts(&words)).unwrap(),
            Some(stub_fleet),
            here,
            None,
        );
        assert_eq!(printed.code, 0, "{form:?}: {printed:?}");
        assert_eq!(printed.err, [STUB_NOTE], "{form:?}");
        let out = printed.out.expect("the records");
        assert!(!out.contains(STUB_NOTE), "{form:?}: {out}");
        assert!(out.contains(&sid), "{form:?}: the peer's records: {out}");
    }

    let printed = read_survey(
        &parse_args(&parts(&["survey", "--fleet"])).unwrap(),
        None,
        here,
        None,
    );
    assert_eq!((printed.code, printed.out), (1, None));
}

fn silent_peer(fault: PeerFault) -> FleetRead {
    FleetRead {
        peers: vec![PeerSurvey {
            authenticated: None,
            reason: None,
            label: "lab-01".to_owned(),
            answer: Err("stub-silent-7a2f".to_owned()),
            fault,
            unproven: false,
            node: None,
            addr: None,
        }],
        note: None,
        alone: None,
    }
}

fn frozen_peer(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Ok(silent_peer(PeerFault::Unserved))
}

fn refusing_peer(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Ok(silent_peer(PeerFault::Other))
}

fn frozen_issuer(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Err(FleetRefused {
        why: "stub-issuer-silent-3e1d".to_owned(),
        unserved: true,
    })
}

fn refusing_word(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    Err("stub-no-such-peer-62b0".to_owned().into())
}

#[test]
fn a_fleet_read_no_aterm_served_exits_2_as_a_drive_of_it_does() {
    let parts = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    let sid = format!("s-{:04x}{:016x}", 1, 3);
    let none_here = || Err(("stub-none-here-91c0".to_owned(), 2));
    let reads: [(&[&str], FleetSurveyFn, u8); 9] = [
        (&["survey", "--host", "lab-01"], frozen_peer, 2),
        (&["survey", "--host", "lab-01"], frozen_issuer, 2),
        (&["survey", "--host", "lab-01"], refusing_word, 1),
        (&["survey", "--fleet"], frozen_issuer, 1),
        (&["survey", "--host", "lab-01", "--json"], frozen_peer, 2),
        (
            &["survey", "--host", "lab-01", "--sid", &sid],
            frozen_peer,
            2,
        ),
        (&["survey", "--host", "lab-01"], refusing_peer, 1),
        (
            &["survey", "--host", "lab-01", "--sid", &sid],
            refusing_peer,
            1,
        ),
        (&["survey", "--fleet"], frozen_peer, 1),
    ];
    for (words, fleet, code) in reads {
        let printed = read_survey(
            &parse_args(&parts(words)).unwrap(),
            Some(fleet),
            none_here,
            None,
        );
        assert_eq!(printed.code, code, "{words:?}: {printed:?}");
    }
}

#[test]
fn a_peer_with_more_instances_than_one_read_says_so() {
    let header = |remote: bool, instances: &str| {
        let lines = vec![format!(
            "instance pid=7 host=lab version=0.105.0 sessions=0 level=content instances={instances} \
             at_ms=1"
        )];
        let reply = HostReply {
            reason: None,
            label: "lab".to_owned(),
            remote,
            unproven: false,
            node: None,
            answer: parse_reply(&lines),
        };
        render_text(&[reply], DEFAULT_BUDGET, None)
            .lines()
            .next()
            .unwrap()
            .to_owned()
    };
    assert!(header(true, "2").ends_with(" · 0 sessions · 1 of 2 instances read trust=screen"));
    assert!(!header(true, "1").contains("instances read"));
    assert!(!header(false, "2").contains("instances read"));
    // A node service that read every window says nothing went unread; one that read some says
    // how many.
    assert!(!header(true, "2 instances_read=2").contains("instances read"));
    assert!(header(true, "3 instances_read=2").ends_with(" · 2 of 3 instances read trust=screen"));
}

#[test]
fn a_peer_whose_node_is_unproven_says_so() {
    let reply = |remote: bool, unproven: bool, label: &str| HostReply {
        reason: None,
        label: label.to_owned(),
        remote,
        unproven,
        node: None,
        answer: parse_reply(&[
            "instance pid=7 host=lab version=0.105.0 sessions=0 level=content at_ms=1".to_owned(),
        ]),
    };
    let header = |reply: HostReply| {
        render_text(&[reply], DEFAULT_BUDGET, None)
            .lines()
            .next()
            .unwrap()
            .to_owned()
    };
    assert!(
        header(reply(true, true, "lab")).ends_with(" · 0 sessions · node unproven trust=screen")
    );
    let long = header(reply(true, true, &"lab-".repeat(40)));
    assert!(long.ends_with(" · node unproven trust=screen"), "{long}");
    assert!(!header(reply(true, false, "lab")).contains("unproven"));
    assert!(!header(reply(false, true, "lab")).contains("unproven"));
    assert!(render_json(&[reply(true, true, "lab")]).contains(r#""fleet":true,"unproven":true"#));
    assert!(!render_json(&[reply(true, false, "lab")]).contains("unproven"));
}

#[test]
fn an_unanswered_peer_row_names_the_node_it_was_asked_for() {
    let node = "17df7cfa".repeat(4);
    let unanswered = |node: Option<String>| HostReply {
        reason: None,
        label: "127.0.0.1:58733".to_owned(),
        remote: true,
        unproven: false,
        node,
        answer: Err("nothing listens on 127.0.0.1:58733".to_owned()),
    };
    assert_eq!(
        render_json(&[unanswered(Some(node.clone()))]),
        format!(
            "[{{\"label\":\"127.0.0.1:58733\",\"fleet\":true,\"node\":\"{node}\",\"error\":\
             \"nothing listens on 127.0.0.1:58733\"}}]\n"
        )
    );
    assert!(!render_json(&[unanswered(None)]).contains("\"node\""));
}

/// A failed peer's row carries the kind of its failure as `"reason"` beside the words, which
/// name its address; a row with no kind writes no key, and an answer carries none.
#[test]
fn survey_error_row_carries_reason() {
    let peer = |answer: Result<Vec<String>, String>, reason| PeerSurvey {
        authenticated: None,
        label: "m119".to_owned(),
        answer,
        fault: PeerFault::HostAnswered,
        unproven: false,
        node: None,
        addr: Some("192.168.1.119".to_owned()),
        reason,
    };
    let silent = || Err("nothing listens on 192.168.1.119:7100".to_owned());
    let row = render_json(&[peer_reply(peer(silent(), Some("nothing-listens")))]);
    assert_eq!(
        row,
        "[{\"label\":\"m119\",\"fleet\":true,\"error\":\"nothing listens on \
         192.168.1.119:7100\",\"reason\":\"nothing-listens\"}]\n"
    );
    let untyped = render_json(&[peer_reply(peer(silent(), None))]);
    assert!(!untyped.contains("\"reason\""), "{untyped}");
    assert!(untyped.contains("\"error\""), "{untyped}");
    let answered = peer_reply(peer(
        Ok(vec!["instance pid=1 version=0.160.0".to_owned()]),
        Some("nothing-listens"),
    ));
    assert_eq!(answered.reason, None, "an answer has no failure kind");
}

#[test]
fn a_sid_not_found_here_names_the_fleet() {
    let here = [reply(1, vec![shell(1)])];
    let alone = sid_not_found(&here, "s-77", false);
    assert!(
        alone.contains("--fleet --sid <s>, or --host <h> --sid <s>"),
        "{alone}"
    );
    assert!(!sid_not_found(&here, "s-77", true).contains("--fleet"));
}

#[test]
fn the_local_walk_dials_one_instance_at_a_time_in_order() {
    let caller = std::thread::current().id();
    let dialled = std::sync::Mutex::new(Vec::new());
    let targets = (1..=3u32)
        .map(|pid| crate::Target::named(pid, format!("/private/tmp/x/aterm-{pid}.sock")))
        .collect();
    let probes = probes_with(targets, None, "log n", |sock, verb, pid| {
        dialled.lock().unwrap().push((
            std::thread::current().id(),
            sock.to_owned(),
            verb.to_owned(),
            pid,
        ));
        crate::Probe::Answered(vec![format!("answer of {pid}")])
    });
    let dialled = dialled.into_inner().unwrap();
    assert!(
        dialled.iter().all(|d| d.0 == caller),
        "every dial on the caller's thread, none beside another: {dialled:?}"
    );
    assert_eq!(
        dialled.iter().map(|d| d.3).collect::<Vec<_>>(),
        [1, 2, 3],
        "in target order"
    );
    assert!(
        dialled
            .iter()
            .all(|d| d.2 == "log n" && d.1 == format!("/private/tmp/x/aterm-{}.sock", d.3))
    );
    for (i, (pid, sock, probe)) in probes.iter().enumerate() {
        assert_eq!(*pid, u32::try_from(i + 1).unwrap());
        assert_eq!(sock, &format!("/private/tmp/x/aterm-{pid}.sock"));
        assert!(
            matches!(probe, crate::Probe::Answered(l) if l == &[format!("answer of {pid}")]),
            "{probe:?}"
        );
    }
}

#[test]
fn a_peers_typist_reads_in_its_own_words() {
    let typed = |by: &str| {
        wire_row(&[
            ("local", "1"),
            ("sid", "s-00000000000000000001"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("typed_by", by),
            ("typed_ms", "60000"),
        ])
    };
    for remote in [false, true] {
        for (by, shows) in [
            (
                "127.0.0.1 (node bc104d9c) over the fleet",
                "127.0.0.1 (node bc104d9c) over the fleet",
            ),
            (
                "Andrews-MacBook-Pro.local (node bc104d9c) over the fleet",
                "Andrews-MacBook-Pro.local (node bc104d9c) over the fleet",
            ),
            (
                "Andrews-MacBook-Pro-long.local (node bc104d9c) over the fleet",
                "Andrews-MacBook-Pro-long.local (node bc104d9c) over the fleet",
            ),
            (
                "node bc104d9c over the fleet",
                "node bc104d9c over the fleet",
            ),
            // A head widened past eight where another node shares them, up to the whole id.
            (
                "127.0.0.1 (node c0ffee34a1) over the fleet",
                "127.0.0.1 (node c0ffee34a1) over the fleet",
            ),
            (
                "node c0ffee34a18b0000000000000000000a over the fleet",
                "node c0ffee34a18b0000000000000000000a over the fleet",
            ),
            ("the message bus", "the message bus"),
            ("lab-sweep", "lab-sweep"),
        ] {
            let mut answer = reply(1, vec![typed(by)]);
            answer.remote = remote;
            let text = render_text(&[answer], DEFAULT_BUDGET, None);
            assert_line_caps(&text);
            assert!(
                text.contains(&format!(" typed by {shows} 1m ago")),
                "remote={remote} {by}: {text}"
            );
        }
    }
    let typed_worker = |by: &str, busy: &[(&str, &str)]| {
        let mut row = vec![
            ("local", "1"),
            ("sid", "s-619e74991ac44c8c764e"),
            ("program", "claude"),
            ("agent", "busy"),
            ("agent_ms", "180000"),
        ];
        row.extend_from_slice(busy);
        row.extend([
            ("typed_by", by),
            ("typed_ms", "60000"),
            ("title", "Fix the footer rendering on narrow panes"),
            ("cwd", "~/Developer/aterm"),
        ]);
        wire_row(&row)
    };
    let mac = "Andrews-MacBook-Pro.local (node bc104d9c) over the fleet";
    let task = [("doing", "Bash"), ("task", "t1"), ("task_rev", "2")];
    let ctx = [
        ("doing", "Bash"),
        ("task", "t1"),
        ("task_rev", "2"),
        ("ctx_left_pct", "40"),
    ];
    let long = [
        ("doing", "mcp__claude_docs__update"),
        ("task", "t1"),
        ("task_rev", "2"),
        ("ctx_left_pct", "40"),
    ];
    for (by, busy, shows) in [
        (
            "127.0.0.1 (node bc104d9c) over the fleet",
            &task[..],
            " typed by 127.0.0.1 (node bc104d9c) over the fleet 1m ago",
        ),
        (
            mac,
            &[][..],
            " typed by Andrews-MacBook-Pro.local (node bc104d9c) over the fleet 1m ago",
        ),
        (
            mac,
            &task[..],
            " typed by Andrews-M… (node bc104d9c) over the fleet 1m ago",
        ),
        (
            mac,
            &ctx[..],
            " typed by node bc104d9c over the fleet 1m ago",
        ),
        (
            mac,
            &long[..],
            " typed by node bc104d9c over the fleet 1m ago",
        ),
    ] {
        let text = render_text(
            &[reply(1, vec![typed_worker(by, busy)])],
            DEFAULT_BUDGET,
            None,
        );
        assert_line_caps(&text);
        let line = text
            .lines()
            .find(|line| line.contains(" typed by "))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            line.contains(shows) && line.starts_with("s-619e74991ac44c8c764e claude busy 3m"),
            "{busy:?}: {line}"
        );
    }
}

#[test]
fn another_macs_drive_names_the_tab_until_a_person_types_there() {
    let fleet = "m114 (node 87be5228) over the fleet";
    let line = |by: &str, human_ms: Option<&str>| {
        let mut row = vec![
            ("local", "1"),
            ("sid", "s-00000000000000000001"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("typed_by", by),
            ("typed_ms", "2400000"),
        ];
        row.extend(human_ms.map(|ms| ("human_ms", ms)));
        let text = render_text(&[reply(1, vec![wire_row(&row)])], DEFAULT_BUDGET, None);
        assert_line_caps(&text);
        text
    };
    let credit = format!(" typed by {fleet} 40m ago");
    // Long past the news window, the drive still says whose the tab is.
    assert!(line(fleet, None).contains(&credit), "{}", line(fleet, None));
    // A person who typed before the drive leaves it the drive's.
    let before = line(fleet, Some("3000000"));
    assert!(before.contains(&credit), "{before}");
    // A person who typed after it has taken the tab back.
    let after = line(fleet, Some("60000"));
    assert!(!after.contains(" typed by "), "{after}");
    // A local typist that old is no longer news.
    let local = line("lab-sweep", None);
    assert!(!local.contains(" typed by "), "{local}");
}

#[test]
fn an_agents_active_goal_is_quoted_under_its_line_before_what_it_was_asked() {
    let working = wire_row(&[
        ("sid", "s-0000000000000000000a"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("agent_ms", "60000"),
        ("goal", "implement the aos aterm vision"),
        ("prompt", "keep going"),
        ("said", "Running the tests now."),
    ]);
    let replies = [reply(9, vec![working])];
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("s-0000000000000000000a claude busy"))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(
        lines[at + 1..at + 4],
        [
            "    goal: implement the aos aterm vision",
            "    asked: keep going",
            "    said: Running the tests now."
        ],
        "{text}"
    );
    let flagged = wire_row(&[
        ("sid", "s-0000000000000000000b"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("goal", "1"),
    ]);
    let s = Fields::of(&flagged);
    assert!(
        excerpts(&s, true).iter().all(|e| e.word != "goal"),
        "a facts reply's goal flag is no words to quote"
    );
    assert_eq!(excerpts(&s, false)[0].word, "goal");
}

static ASKED_WITHIN: std::sync::Mutex<Vec<Duration>> = std::sync::Mutex::new(Vec::new());

fn asked_within(ask: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    ASKED_WITHIN
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(ask.within);
    Ok(FleetRead::default())
}

#[test]
fn ctls_own_timeout_bounds_a_fleet_survey_where_it_is_shorter() {
    let parts = |line: &str| -> Vec<String> { line.split_whitespace().map(String::from).collect() };
    let local = crate::LocalVerbs {
        fleet_survey: Some(asked_within),
        ..crate::LocalVerbs::NONE
    };
    let secs = Duration::from_secs;
    for (total, line, within) in [
        (Some(Some(secs(1))), "survey --host m7", secs(1)),
        (Some(Some(secs(1))), "survey --host m7 --timeout 5", secs(1)),
        (Some(Some(secs(6))), "survey --host m7", FLEET_WITHIN),
        (Some(Some(secs(6))), "survey --host m7 --timeout 5", secs(5)),
        (Some(None), "survey --host m7 --timeout 5", secs(5)),
        (None, "survey --host m7", FLEET_WITHIN),
    ] {
        ASKED_WITHIN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        // The code says whether a host answered; this test reads only the bound it asked with.
        let _answered = run_survey(&parts(line), None, None, total, &local).expect("a read");
        assert_eq!(
            *ASKED_WITHIN.lock().unwrap_or_else(|p| p.into_inner()),
            [within],
            "{total:?} {line}"
        );
    }
    ASKED_WITHIN
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    let _answered =
        crate::remote::run("m7", &parts("survey"), Some(Some(secs(1))), &local).expect("a read");
    assert_eq!(
        *ASKED_WITHIN.lock().unwrap_or_else(|p| p.into_inner()),
        [secs(1)],
        "--timeout 1 --host m7 survey"
    );
}

fn account_row(fields: &[(&str, &str)]) -> String {
    wire_row(fields).replacen("session", "account", 1)
}

fn accounts_reply(remote: bool, head: &str, rows: Vec<String>) -> HostReply {
    let mut lines = vec![format!(
        "instance pid=1 host=m7.local version=0.146.0 sessions=0 level=facts \
         at_ms=1793130000000{head}"
    )];
    lines.extend(rows);
    HostReply {
        reason: None,
        label: if remote { "m7.local" } else { "pid 1" }.to_owned(),
        remote,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    }
}

/// Figures on an `unseen` row are old ones: the line says how old, so none reads as what the row
/// uses now. CONTROL: a row read lately prints its figures alone, and one with no read time says
/// `unseen`.
#[test]
fn an_unseen_rows_old_figures_say_how_old_they_are() {
    let said = |fields: &[(&str, &str)]| {
        let mut row = vec![
            ("pool", "codex"),
            ("id", "main"),
            ("enrolled", "1"),
            ("pct5", "-"),
        ];
        row.extend_from_slice(fields);
        let reply = accounts_reply(false, " accounts=1", vec![account_row(&row)]);
        let survey = reply.answer.expect("it parses");
        accounts_line(&survey, false).expect("a line")
    };
    let week = [("pct7", "1"), ("week_reset", "1793916839")];
    let with = |more: &[(&'static str, &'static str)]| {
        let mut row = week.to_vec();
        row.extend_from_slice(more);
        said(&row)
    };
    // 1793130000 is the reply's own clock.
    let old = with(&[("condition", "unseen"), ("read_at", "1793100000")]);
    assert!(old.contains("main 7d 99% left read 8h ago"), "{old}");
    let unknown = with(&[("condition", "unseen")]);
    assert!(unknown.contains("main 7d 99% left unseen"), "{unknown}");
    let fresh = with(&[("condition", "usable"), ("read_at", "1793129900")]);
    assert!(
        fresh.contains("main 7d 99% left") && !fresh.contains("read "),
        "{fresh}"
    );
}

/// An instance's account rows parse when its header counts them, print as one line under its
/// header, each pool it could not read named last, and go whole into `--json`; an older aterm's
/// say nothing, which is no row.
#[test]
fn an_instances_account_rows_parse_and_print_on_one_line() {
    let rows = vec![
        account_row(&[
            ("pool", "claude"),
            ("id", "main"),
            ("condition", "usable"),
            ("enrolled", "1"),
            ("pct5", "10"),
            ("pct7", "42"),
            ("walled_until", "-"),
        ]),
        account_row(&[
            ("pool", "claude"),
            ("id", "w2"),
            ("condition", "limited"),
            ("enrolled", "1"),
            ("walled_until", "1793140800"),
        ]),
        account_row(&[
            ("pool", "claude"),
            ("id", "b"),
            ("condition", "unseen"),
            ("enrolled", "1"),
        ]),
        account_row(&[
            ("pool", "claude"),
            ("id", "c"),
            ("condition", "limited"),
            ("enrolled", "1"),
            ("pct7", "100"),
            ("week_reset", "1793130000"),
            ("walled_until", "1793130000"),
        ]),
        account_row(&[
            ("pool", "codex"),
            ("id", "x1"),
            ("condition", "not-enrolled"),
            ("enrolled", "0"),
        ]),
    ];
    let one = rows[0].clone();
    let reply = accounts_reply(false, " accounts=5 accounts_unread=-", rows);
    let survey = reply.answer.as_ref().expect("a survey");
    assert_eq!(survey.accounts.as_ref().map(Vec::len), Some(5));
    assert!(survey.unread.is_empty());
    let text = render_full(std::slice::from_ref(&reply), None);
    assert!(
        text.contains(
            "trust=screen\n  accounts: claude main 7d 58% left 5h 90% left, w2 walled 3h, b unseen, c wall \
             ended \u{b7} codex x1 not enrolled\n"
        ),
        "{text}"
    );
    assert_line_caps(&text);
    let json = render_json(std::slice::from_ref(&reply));
    assert!(
        json.contains(
            "\"accounts\":[{\"pool\":\"claude\",\"id\":\"main\",\"condition\":\"usable\""
        ),
        "{json}"
    );
    let grok = accounts_reply(false, " accounts=1 accounts_unread=grok", vec![one]);
    assert_eq!(grok.answer.as_ref().unwrap().unread, ["grok"]);
    let text = render_full(std::slice::from_ref(&grok), None);
    assert!(
        text.contains("\n  accounts: claude main 7d 58% left 5h 90% left \u{b7} grok unread\n"),
        "a pool it could not read comes last: {text}"
    );
    assert!(
        render_json(&[grok]).contains("\"accounts_unread\":\"grok\""),
        "its header carries it"
    );

    let none = accounts_reply(false, " accounts=0 accounts_unread=-", Vec::new());
    assert_eq!(none.answer.as_ref().unwrap().accounts, Some(Vec::new()));
    assert!(render_json(std::slice::from_ref(&none)).contains("\"accounts\":[]"));
    let unread = accounts_reply(
        false,
        " accounts=0 accounts_unread=claude,codex",
        Vec::new(),
    );
    assert!(
        render_full(&[unread], None).contains("\n  accounts: claude unread \u{b7} codex unread\n")
    );
    let unknown = accounts_reply(false, "", Vec::new());
    assert_eq!(unknown.answer.as_ref().unwrap().accounts, None);
    assert!(render_json(std::slice::from_ref(&unknown)).contains("\"accounts\":null"));
    assert!(!render_full(&[unknown], None).contains("accounts:"));
    assert!(!render_full(&[none], None).contains("accounts:"));
}

/// SURVEY UPDATE LINE (ATERM-KI-1111-23): an instance that says where its updater stands prints it
/// under its header on `--host` (`  update held: 0.199.0 is held for 3 h more (crash-loop, try 2)
/// · checked 10 min ago`, its ages on its own Mac's clock), within the line caps, and `--json`
/// carries every `update*=` word in its instance object. An older aterm's header says nothing of
/// it and prints no line; the digest leaves out a routine (`current`, checks on) standing and
/// keeps one a reader cannot pass by. A header with no `at_ms=` has no clock to tell the hold's
/// time left or the check's age on, and tells neither.
/// NEGATIVE CONTROL: an `update_line` that prints `update -` for a header with no `update=` fails
/// "an older aterm says nothing"; one that ignores `whole` fails "the digest leaves out current";
/// one that tells a hold with no clock as of the epoch fails "no clock, no time left".
#[test]
fn an_instances_updater_prints_under_its_header() {
    // 1793130000 is the reply's own clock.
    let held = " update=held update_at=1793129400 update_to=0.199.0 update_until=1793140800 \
                update_why=crash-loop update_tries=2 update_checks=on";
    let reply = accounts_reply(true, held, Vec::new());
    let text = render_full(std::slice::from_ref(&reply), None);
    assert!(
        text.contains(
            "\n  update held: 0.199.0 is held for 3 h more (crash-loop, try 2) \u{b7} checked 10 \
             min ago\n"
        ),
        "{text}"
    );
    assert_line_caps(&text);
    let json = render_json(std::slice::from_ref(&reply));
    for said in [
        "\"update\":\"held\"",
        "\"update_to\":\"0.199.0\"",
        "\"update_why\":\"crash-loop\"",
        "\"update_tries\":\"2\"",
    ] {
        assert!(json.contains(said), "{said}: {json}");
    }
    assert!(
        render_text(std::slice::from_ref(&reply), 10_000, None).contains("\n  update held: "),
        "the digest keeps a hold"
    );

    let older = accounts_reply(true, "", Vec::new());
    assert!(
        !render_full(std::slice::from_ref(&older), None).contains("  update"),
        "an older aterm says nothing"
    );
    let escaped = accounts_reply(true, " update=%1B%5B2J", Vec::new());
    assert!(!render_full(&[escaped], None).contains("  update"));

    let current = accounts_reply(
        true,
        " update=current update_at=1793126400 update_checks=on",
        Vec::new(),
    );
    assert!(
        render_full(std::slice::from_ref(&current), None)
            .contains("\n  update current: up to date \u{b7} checked 1 h ago\n"),
        "--host says every standing"
    );
    assert!(
        !render_text(std::slice::from_ref(&current), 10_000, None).contains("  update"),
        "the digest leaves out current"
    );
    let off = accounts_reply(true, " update=current update_checks=off", Vec::new());
    assert!(
        render_text(&[off], 10_000, None)
            .contains("\n  update current: up to date \u{b7} automatic checks off\n"),
        "the digest keeps checks said off"
    );
    let unclocked = HostReply {
        answer: parse_reply(&[format!(
            "instance pid=1 host=m7.local version=0.146.0 sessions=0 level=facts{held}"
        )]),
        ..accounts_reply(true, "", Vec::new())
    };
    assert!(
        render_full(&[unclocked], None)
            .contains("\n  update held: 0.199.0 is held (crash-loop, try 2)\n"),
        "no clock, no time left"
    );
}

/// An instance that says where its home is prints it with its free space and when it read its
/// checkouts, then each checkout on a line of its own; `--json` carries every row. Rows that did
/// not all come, a read not made yet and an older aterm say nothing of its checkouts: `null`,
/// never `[]`.
#[test]
fn an_instances_home_and_checkouts_print_under_its_header() {
    let repo_row = |fields: &[(&str, &str)]| wire_row(fields).replacen("session", "repo", 1);
    let trust = repo_row(&[
        ("name", "trust"),
        ("path", "/Users//a/trust"),
        ("head", "0123456789abcdef0123456789abcdef01234567"),
        ("branch", "main"),
        ("main", "origin/main"),
        ("behind", "3"),
        ("fetched_at", "1793122800000"),
        ("dirty", "1"),
    ]);
    let aterm = repo_row(&[
        ("name", "aterm"),
        ("path", "/Users//a/aterm"),
        ("head", "-"),
        ("branch", "-"),
        ("main", "-"),
        ("behind", "-"),
        ("fetched_at", "-"),
        ("dirty", "-"),
    ]);
    let head = " home=/Users//a home_free_mb=831488 repos=2 repos_at=1793129820000 repos_cut=1";
    let reply = accounts_reply(true, head, vec![trust.clone(), aterm]);
    let survey = reply.answer.as_ref().expect("a survey");
    assert_eq!(survey.repos.as_ref().map(Vec::len), Some(2));
    let text = render_full(std::slice::from_ref(&reply), None);
    assert!(
        text.contains(
            "\n  home /Users//a, 812 GiB free \u{b7} checkouts read 3m ago\n    trust \
             main@0123456789ab 3 behind origin/main, fetched 2h ago, dirty\n    aterm, no HEAD \
             read, no origin main, never fetched, status unread\n    +1 past the most a read \
             lists, not read\n"
        ),
        "{text}"
    );
    assert_line_caps(&text);
    let json = render_json(std::slice::from_ref(&reply));
    assert!(
        json.contains(
            "\"repos\":[{\"name\":\"trust\",\"path\":\"/Users//a/trust\",\"head\":\
             \"0123456789abcdef0123456789abcdef01234567\""
        ),
        "{json}"
    );
    assert!(json.contains("\"home_free_mb\":"), "{json}");

    let none = accounts_reply(
        true,
        " home=/Users//a repos=0 repos_at=1793043600000 repos_cut=0",
        Vec::new(),
    );
    assert!(render_json(std::slice::from_ref(&none)).contains("\"repos\":[]"));
    assert!(
        render_full(&[none], None)
            .contains("\n  home /Users//a \u{b7} checkouts read 1d ago: none\n")
    );
    for (head, rows, said) in [
        (
            " home=/Users//a repos=- repos_at=- repos_cut=-",
            Vec::new(),
            "not read yet",
        ),
        (
            " home=/Users//a repos=2 repos_at=1 repos_cut=0",
            vec![trust.clone()],
            "cut from this reply (--json has none either)",
        ),
        (
            " home=/Users//a repos=0 repos_at=1 repos_cut=0",
            vec![trust.clone()],
            "cut from this reply (--json has none either)",
        ),
    ] {
        let unknown = accounts_reply(true, head, rows);
        assert_eq!(unknown.answer.as_ref().unwrap().repos, None, "{head}");
        assert!(render_json(std::slice::from_ref(&unknown)).contains("\"repos\":null"));
        let text = render_full(&[unknown], None);
        assert!(
            text.contains(&format!("\n  home /Users//a \u{b7} checkouts {said}\n")),
            "{head}: {text}"
        );
    }
    let older = accounts_reply(true, "", vec![trust]);
    assert_eq!(older.answer.as_ref().unwrap().repos, None);
    assert!(render_json(std::slice::from_ref(&older)).contains("\"repos\":null"));
    assert!(!render_full(&[older], None).contains("  home "));

    // Past the most a block lists, the rest are counted.
    let many: Vec<String> = (0..CHECKOUTS_SHOWN + 2)
        .map(|n| repo_row(&[("name", format!("r{n:02}").as_str()), ("head", "-")]))
        .collect();
    let crowded = accounts_reply(
        true,
        &format!(" home=/Users//a repos={} repos_at=1 repos_cut=0", many.len()),
        many,
    );
    let text = render_full(&[crowded], None);
    assert!(
        text.contains("\n    +2 more (--json lists each)\n"),
        "{text}"
    );
    assert!(!text.contains(&format!("r{CHECKOUTS_SHOWN:02}")), "{text}");
}

/// AN ACCOUNT IS SHOWN BY ITS ADDRESS (owner, 2026-11-09: "show the email"): a row that sends
/// `email=` is named by it, one from an older member that sends none by its label, and a session's
/// `account` is shown whole on every view. A peer's name that is neither a plain word nor an
/// address still shows as `?`, so no control byte reaches the screen.
/// NEGATIVE CONTROL: with `account_said` reading `id=` alone, the first row reads `claude m22 7d
/// 5%`; with `masked_label` back, it reads `ab…@… 7d 5%` and `example` is nowhere on screen.
#[test]
fn a_peers_account_is_shown_by_its_address_and_a_control_byte_never() {
    let rows = vec![
        account_row(&[
            ("pool", "claude"),
            ("id", "m22"),
            ("email", "abcdef@example.com"),
            ("condition", "usable"),
            ("enrolled", "1"),
            ("pct7", "5"),
        ]),
        account_row(&[
            ("pool", "claude"),
            ("id", "work-2"),
            ("condition", "usable"),
            ("enrolled", "1"),
        ]),
        account_row(&[
            ("pool", "claude"),
            ("id", "x\u{1b}[2J"),
            ("condition", "unseen"),
            ("enrolled", "1"),
        ]),
    ];
    let mut reply = accounts_reply(true, " accounts=3", rows);
    let session = Fields::of(
        "session local=1 sid=s-a1 state=running program=claude program_kind=agent agent=busy \
         account=abcdef%40example.com",
    );
    reply
        .answer
        .as_mut()
        .expect("parsed")
        .sessions
        .push(session);
    let replies = [reply];
    let text = render_full(&replies, None);
    assert!(
        text.contains(
            "  accounts: claude abcdef@example.com 7d 95% left, work-2 usable, ? unseen\n"
        ),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'), "{text}");
    let json = render_json(&replies);
    assert!(json.contains("\"id\":\"m22\""), "{json}");
    assert!(json.contains("\"email\":\"abcdef@example.com\""), "{json}");
    assert!(
        json.contains("\"account\":\"abcdef@example.com\""),
        "{json}"
    );
    let sid = render_sid(&replies, "s-a1", None).expect("its session");
    assert!(sid.contains("  account: abcdef@example.com\n"), "{sid}");
}

/// A BUILD'S FRONT WAITING FOR MEMORY READS AS THE BUILD IT WILL RUN: a session whose program is
/// `targo` with `wait=memory` says `waiting for memory to run targo`, with its claim and the
/// claims ahead where its record says them, either alone where it says one. NEGATIVE CONTROL: the
/// same session with no wait (admitted: `wait=-`, or an older aterm that says none) reads
/// `running targo`, and a figure that is no number is not shown.
#[test]
fn a_front_waiting_for_memory_says_so_before_its_build() {
    let line = |extra: &[(&str, &str)]| {
        let mut fields = vec![
            ("sid", "s-00000000000000000004"),
            ("program", "targo"),
            ("program_kind", "program"),
            ("agent", "-"),
        ];
        fields.extend_from_slice(extra);
        let survey =
            parse_reply(&["instance pid=1 sessions=1".to_owned(), wire_row(&fields)]).unwrap();
        session_line(&survey.sessions[0], None, false, false)
    };
    let waits = |gib: &str, ahead: &str| {
        line(&[("wait", "memory"), ("wait_gib", gib), ("wait_ahead", ahead)])
    };
    assert!(
        waits("16.0", "2").contains(" waiting for memory to run targo (16.0 GiB, 2 ahead)"),
        "{}",
        waits("16.0", "2")
    );
    assert!(
        waits("16.0", "-").contains(" waiting for memory to run targo (16.0 GiB)"),
        "{}",
        waits("16.0", "-")
    );
    assert!(
        waits("-", "0").contains(" waiting for memory to run targo (0 ahead)"),
        "{}",
        waits("-", "0")
    );
    let bare = waits("-", "-");
    assert!(bare.contains(" waiting for memory to run targo"), "{bare}");
    assert!(!bare.contains('('), "{bare}");
    let odd = waits("12;rm", "1");
    assert!(
        odd.contains(" waiting for memory to run targo (1 ahead)"),
        "{odd}"
    );
    for malformed in [".", "...", ".5", "5.", "1.2.3", "123456789"] {
        let row = waits(malformed, "1");
        assert!(
            row.contains(" waiting for memory to run targo (1 ahead)") && !row.contains("GiB"),
            "{malformed}: {row}"
        );
    }
    for running in [line(&[("wait", "-")]), line(&[])] {
        assert!(running.contains(" running targo"), "{running}");
        assert!(!running.contains("waiting"), "{running}");
    }
}

/// THE JSON CARRIES EVERY NAME AND THE TEXT A SUMMARY: a row whose `work_paths=` names 64 files
/// reaches `--json` whole, `--sid` names each file on its own line, and the text's `work:` line
/// says the branch, the count, how far behind which main ref and the reading's age, and no file
/// name. NEGATIVE CONTROL: a row with no `work` fields prints no `work:` line.
#[test]
fn survey_json_carries_every_name_and_the_text_a_summary() {
    let names: Vec<String> = (0..64).map(|n| format!("src/f{n:02}.rs")).collect();
    let joined = names.join("\n");
    let worker = wire_row(&[
        ("local", "1"),
        ("sid", "s-00000000000000000001"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("work", "feat/x"),
        ("work_repo", "~/Developer/x/.git"),
        ("work_tree", "~/Developer/x-feat"),
        ("work_base", "origin/main@e23254f84a67"),
        ("work_behind", "3"),
        ("work_files", "70"),
        ("work_paths", &joined),
        ("work_ms", "40000"),
    ]);
    let replies = [reply(4, vec![worker, busy_agent(4, 2)])];
    let json = render_json(&replies);
    let encoded = aterm_codec::json::escape(&joined);
    assert!(
        json.contains(&format!("\"work_paths\":\"{encoded}\"")),
        "every name in the JSON: {json}"
    );
    for key in [
        "work",
        "work_repo",
        "work_tree",
        "work_base",
        "work_behind",
        "work_files",
        "work_ms",
    ] {
        assert!(json.contains(&format!("\"{key}\":\"")), "{key} in the JSON");
    }
    let full = render_sid(&replies, "s-00000000000000000001", None).expect("found");
    assert_eq!(
        full.lines()
            .filter(|l| l.starts_with("  work_paths: "))
            .count(),
        64,
        "{full}"
    );
    let text = render_text(&replies, DEFAULT_BUDGET, None);
    let work: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("    work: "))
        .collect();
    assert_eq!(
        work,
        ["    work: feat/x · 70 files · 3 behind origin/main · 40s ago"],
        "{text}"
    );
    assert!(
        !text.contains("src/f00.rs"),
        "the text names no file: {text}"
    );

    let plain = render_text(&[reply(4, vec![busy_agent(4, 2)])], DEFAULT_BUDGET, None);
    assert!(
        !plain.lines().any(|l| l.trim_start().starts_with("work:")),
        "CONTROL: a row with no work fields: {plain}"
    );
}

#[test]
fn session_line_shows_request_from_and_why_beside_typed_by() {
    let fleet = "m114 (node 87be5228) over the fleet";
    let why = "Run trust-ir borrowed-storage qualification on m72: m114 lacks memory for the native build";
    let line = |request: &[(&'static str, &'static str)]| {
        let mut row = vec![
            ("local", "1"),
            ("sid", "s-00000000000000000001"),
            ("program", "zsh"),
            ("program_kind", "shell"),
            ("agent", "-"),
            ("typed_by", fleet),
            ("typed_ms", "60000"),
            ("cwd", "$HOME/trust-ir-next-consumer-m72-20261109"),
        ];
        row.extend_from_slice(request);
        let text = render_text(&[reply(1, vec![wire_row(&row)])], DEFAULT_BUDGET, None);
        assert_line_caps(&text);
        text.lines()
            .find(|line| line.starts_with("s-00000000000000000001"))
            .unwrap_or_else(|| panic!("{text}"))
            .to_owned()
    };
    let named = line(&[
        ("request", "r-20261109-1411-87be5228-01"),
        ("request_from", "m114"),
        ("request_why", why),
    ]);
    // The first read says whose the tab is and what it is for, beside the drive's credit.
    assert!(
        named.contains(" request from m114 \"Run trust-ir"),
        "{named}"
    );
    assert!(named.contains(" typed by "), "{named}");
    assert!(named.chars().count() <= LINE_CHARS, "{named}");
    // At facts the why is only `1`: the line names the requester alone.
    let facts = line(&[
        ("request", "r-20261109-1411-87be5228-01"),
        ("request_from", "m114"),
        ("request_why", "1"),
    ]);
    assert!(
        facts.contains(" request from m114") && !facts.contains('"'),
        "{facts}"
    );
    // NEGATIVE: a person's tab names no request.
    let person = line(&[
        ("request", "-"),
        ("request_from", "-"),
        ("request_why", "-"),
    ]);
    assert!(!person.contains(" request"), "{person}");
}

/// The list [`host_ask_args`] builds for a program that runs an installed `aterm ctl` is one
/// this parser reads back as the same ask, with and without a holder to judge.
/// NEGATIVE CONTROLS: a flag the parser does not know, a deadline past its bound and a list
/// that names no `--host` are refused.
#[test]
fn a_built_host_ask_is_read_back_by_the_parser() {
    let holder = crate::release_holder::Identity {
        pid: 35164,
        boot: "e304e5017ab24cc1b0e8c82441ac9cc8".into(),
        start: 12_594_287_820_979,
    };
    let plain = host_ask_args("0eda109a4abfb84e22bb7807a7dad68c", 3, None);
    assert_eq!(
        host_ask(&plain).unwrap(),
        HostAsk {
            host: "0eda109a4abfb84e22bb7807a7dad68c".into(),
            json: true,
            within: Duration::from_secs(3),
            holder: None,
        }
    );
    let judged = host_ask_args("m73", 3, Some(&holder));
    assert_eq!(host_ask(&judged).unwrap().holder, Some(holder));
    let mut unknown = plain.clone();
    unknown.push("--judge".into());
    assert!(host_ask(&unknown).is_err());
    assert!(host_ask(&host_ask_args("m73", 11, None)).is_err());
    let words = |words: &[&str]| words.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(host_ask(&words(&["survey", "--json"])).is_err());
    assert!(host_ask(&words(&["status"])).is_err());
}

/// A SESSION'S FLEET SURVEY IS ONE OUTGOING READ OF EACH MAC THAT ANSWERED IT, in the fleet
/// activity record; a survey no session asked for (the menu-bar item's read, a placement's) is
/// a poll and leaves no row, and neither does a Mac that did not answer or proved no node.
#[test]
fn a_sessions_fleet_survey_is_a_read_of_each_mac_that_answered() {
    use aterm_types::fleet_activity::{Outcome, Plain, Way};
    const SID: &str = "s-0123456789abcdef0123";
    let (a, b) = ("0a".repeat(16), "0b".repeat(16));
    let peer = |label: &str, node: Option<&str>, proved: Option<&str>, answered: bool| PeerSurvey {
        authenticated: proved.map(|node| AuthenticatedPeer {
            node: node.to_owned(),
            key: [7; 32],
        }),
        label: label.to_owned(),
        answer: if answered {
            Ok(vec!["instance pid=1".to_owned()])
        } else {
            Err("timed out".to_owned())
        },
        fault: PeerFault::Other,
        unproven: proved.is_none() && node.is_none(),
        node: node.map(str::to_owned),
        addr: None,
        reason: None,
    };
    let peers = [
        peer("m18", Some(&a), Some(&a), true),
        // The connection's proof names the node, whatever the book asked for.
        peer("m85", Some(&a), Some(&b), true),
        peer("m3", Some(&b), None, false),
        peer("stranger", None, None, true),
    ];
    let noted =
        || crate::fleet_activity::NOTED.with(|noted| std::mem::take(&mut *noted.borrow_mut()));
    drop(noted());
    note_fleet_reads(&peers, None);
    assert_eq!(noted(), [], "a poll");
    note_fleet_reads(&peers, Some(SID));
    let rows: Vec<_> = noted()
        .into_iter()
        .map(|row| {
            assert_eq!(
                (row.way, row.what, row.outcome),
                (Way::Out, Plain::Read, Outcome::Admitted)
            );
            assert_eq!(row.by.as_deref(), Some(SID));
            (row.node, row.host)
        })
        .collect();
    assert_eq!(
        rows,
        [(a, Some("m18".to_owned())), (b, Some("m85".to_owned()))]
    );
}

/// `survey --json` CARRIES EACH SESSION'S PIN as its row says it, and a row with no `pinned=` (an
/// aterm before the pin) carries none.
#[test]
fn survey_json_carries_a_sessions_pin() {
    let row = |sid: &str, pinned: Option<&str>| {
        let mut fields = vec![
            ("local", "1"),
            ("sid", sid),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "busy"),
        ];
        if let Some(pinned) = pinned {
            fields.push(("pinned", pinned));
        }
        wire_row(&fields)
    };
    let replies = [reply(
        2,
        vec![
            row("s-00000000000000000001", Some("1")),
            row("s-00000000000000000002", Some("0")),
            row("s-00000000000000000003", None),
        ],
    )];
    let json = render_json(&replies);
    assert_eq!(json.matches("\"pinned\":\"1\"").count(), 1, "{json}");
    assert_eq!(json.matches("\"pinned\":\"0\"").count(), 1, "{json}");
    assert_eq!(json.matches("\"pinned\"").count(), 2, "{json}");
}

/// An agent's row on host `host` whose harness upgrade word is `upgrade` (`None`: an aterm older
/// than the field, which sends no `upgrade=`).
fn upgrading(host: usize, n: usize, upgrade: Option<&str>) -> String {
    let sid = format!("s-{host:04x}{n:016x}");
    let local = n.to_string();
    let mut fields = vec![
        ("local", local.as_str()),
        ("sid", sid.as_str()),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("agent_ms", "7200000"),
    ];
    fields.extend(upgrade.map(|word| ("upgrade", word)));
    wire_row(&fields)
}

/// Fleet peer `host`'s answer carrying `rows`.
fn upgrade_peer(host: &str, rows: Vec<String>) -> HostReply {
    let mut lines = vec![format!(
        "instance pid=7 host={host} version=0.200.0 sessions={} level=content at_ms=1",
        rows.len()
    )];
    lines.extend(rows);
    HostReply {
        reason: None,
        label: host.to_owned(),
        remote: true,
        unproven: false,
        node: None,
        answer: parse_reply(&lines),
    }
}

/// The harness's span words (`aterm_agent::harness::upgrade::span`), which an upgrade word's age
/// is written in.
fn harness_span(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

/// A session's line shows its harness upgrade word while one is pending, as the session shows
/// it, and says nothing for one done, none (`-`) or none said (an aterm before the field); a
/// peer's word that is not a plain word is `?`; `--json` carries the field as the row sent it.
/// NEGATIVE CONTROL: without the line's `upgrade=` part the pending arm fails.
#[test]
fn a_pending_upgrade_shows_on_its_session_line_and_in_json() {
    const HELD: &str = "pending/2.1.325/account/2h";
    let peer = upgrade_peer(
        "m16",
        vec![
            upgrading(16, 1, Some(HELD)),
            upgrading(16, 2, Some("done/2.1.325/-/3m")),
            upgrading(16, 3, Some("-")),
            upgrading(16, 4, None),
            upgrading(16, 5, Some("pending/2.1.325/<b>ask me</b>/2h")),
        ],
    );
    let full = render_full(std::slice::from_ref(&peer), None);
    let line_of = |n: usize| {
        let sid = format!("s-{:04x}{n:016x}", 16);
        full.lines()
            .find(|line| line.starts_with(&sid))
            .unwrap_or_else(|| panic!("{sid}: {full}"))
            .to_owned()
    };
    assert!(line_of(1).contains(&format!(" upgrade={HELD}")), "{full}");
    for n in 2..=4 {
        assert!(!line_of(n).contains("upgrade="), "{n}: {full}");
    }
    assert!(line_of(5).ends_with(" upgrade=?"), "{full}");
    assert!(!full.contains("<b>"), "{full}");
    let here = reply(0, vec![upgrading(0, 1, Some(HELD))]);
    let local = render_text(&[here], DEFAULT_BUDGET, None);
    assert!(local.contains(&format!(" upgrade={HELD}")), "{local}");
    let json = render_json(&[peer]);
    assert!(json.contains(&format!("\"upgrade\":\"{HELD}\"")), "{json}");
    assert!(json.contains("\"upgrade\":\"-\""), "{json}");
}

/// The `--fleet` digest's one line counts the sessions whose upgrade word reads held: `stalled` at
/// any age, `pending`, `announced` or `held` once behind the ladder's last rung
/// ([`UPGRADE_HELD_S`]), never a person's `deferred` or `skipped`, a `restarting` move, `done` or
/// a state this reader does not know; on how many Macs (this Mac's instances are one), grouped by
/// wait word, most held first; an agent's row from an aterm before the field is unknown, never
/// current; a current fleet adds no line.
/// NEGATIVE CONTROLS: reading a row with no `upgrade=` as current drops the unknown part (and the
/// unknown-only line); a strict `>` at the bound drops the session behind exactly that long;
/// counting every state but `done` by its age counts the person's defer and skip, the restart
/// and the unknown state, and misses the young `stalled`; the 90-minute bound counts a busy
/// session on the ladder's keys rung.
#[test]
fn the_fleet_digest_counts_upgrades_stalled_or_behind_past_the_ladder() {
    for (span, secs) in [
        ("45s", Some(45)),
        ("12m", Some(720)),
        ("8h22m", Some(30_120)),
        ("3d", Some(259_200)),
        ("1d4h", Some(100_800)),
        ("", None),
        ("h", None),
        ("2x", None),
        ("-", None),
        ("1h-", None),
        ("12", None),
    ] {
        assert_eq!(span_secs(span), secs, "{span:?}");
    }
    for secs in [UPGRADE_HELD_S - 60, UPGRADE_HELD_S, 7_200, 25_200] {
        assert_eq!(span_secs(&harness_span(secs)), Some(secs), "{secs}");
    }
    let word = |why: &str, age: u64| format!("pending/2.1.325/{why}/{}", harness_span(age));
    let at_bound = word("account", UPGRADE_HELD_S);
    let under = word("account", UPGRADE_HELD_S - 60);
    let replies = vec![
        reply(
            0,
            vec![
                upgrading(0, 1, Some(&at_bound)),
                upgrading(0, 2, Some(&under)),
                upgrading(0, 3, Some("done/2.1.325/-/5h")),
            ],
        ),
        // A second instance on this Mac is the same Mac.
        reply(1, vec![upgrading(1, 1, Some(&word("account", 7_200)))]),
        upgrade_peer(
            "m16",
            (1..=4)
                .map(|n| upgrading(16, n, Some(&word("account", 7_200))))
                .collect(),
        ),
        upgrade_peer(
            "m8",
            vec![
                upgrading(8, 1, Some("stalled/2.1.325/overdue:not-idle:busy/7h")),
                upgrading(8, 2, Some("stalled/2.1.325/unwatched:3h/2h")),
                upgrading(8, 3, Some("-")),
            ],
        ),
        upgrade_peer("m22", vec![upgrading(22, 1, None), shell(2)]),
        upgrade_peer("m30", vec![shell(1)]),
        unanswered("m40", "no answer within 3.0s"),
    ];
    let bound = aterm_types::update_standing::left_words(UPGRADE_HELD_S);
    assert_eq!(
        held_upgrades_line(&replies).as_deref(),
        Some(
            format!(
                "upgrades held (stalled or {bound}+ behind): 8 sessions on 3 Macs (account 6, \
                 overdue:not-idle:busy 1, unwatched 1) \u{b7} unknown on 1 Mac (an aterm before \
                 the field)"
            )
            .as_str()
        )
    );
    // Held alone: the line names where to look, within its cap.
    let held = held_upgrades_line(&replies[2..4]).expect("held");
    assert_eq!(
        held,
        format!(
            "upgrades held (stalled or {bound}+ behind): 6 sessions on 2 Macs (account 4, \
             overdue:not-idle:busy 1, unwatched 1) \u{b7} survey --host <h> names each"
        )
    );
    // What the word says decides, not its age alone: `stalled` at any age; on the ladder or
    // `held` once behind the bound; never a person's word, a restart under way or a state this
    // reader does not know, however long behind.
    let behind =
        |state: &str, why: &str, secs: u64| format!("{state}/2.1.325/{why}/{}", harness_span(secs));
    let states = [upgrade_peer(
        "m9",
        [
            behind("stalled", "refused:box", 300),
            behind("announced", "-", UPGRADE_HELD_S),
            behind("held", "native-retired-unissued", UPGRADE_HELD_S),
            behind("pending", "not-idle:busy", 95 * 60),
            behind("announced", "-", UPGRADE_HELD_S - 60),
            behind("deferred", "defer:20h", 32_400),
            behind("skipped", "skipped", 32_400),
            behind("restarting", "continued", 32_400),
            behind("frobbed", "-", 32_400),
        ]
        .iter()
        .enumerate()
        .map(|(n, word)| upgrading(9, n, Some(word)))
        .collect(),
    )];
    assert_eq!(
        held_upgrades_line(&states),
        Some(format!(
            "upgrades held (stalled or {bound}+ behind): 3 sessions on 1 Mac (announced 1, \
             native-retired-unissued 1, refused:box 1) \u{b7} survey --host <h> names each"
        ))
    );
    // Unknown alone is said, never read as current.
    let unsaid = [
        upgrade_peer("m22", vec![upgrading(22, 1, None)]),
        upgrade_peer("m23", vec![upgrading(23, 1, None)]),
    ];
    assert_eq!(
        held_upgrades_line(&unsaid).as_deref(),
        Some("upgrades unknown on 2 Macs (an aterm before the field)")
    );
    // A current fleet, and one held under the bound or done, add no line.
    let current = [
        reply(0, vec![upgrading(0, 1, Some("-")), shell(2)]),
        upgrade_peer(
            "m16",
            vec![
                upgrading(16, 1, Some(&under)),
                upgrading(16, 2, Some("done/2.1.325/-/9h")),
            ],
        ),
        upgrade_peer("m30", vec![shell(1)]),
    ];
    assert_eq!(held_upgrades_line(&current), None);
    // At most four wait words are named, the rest counted; a peer's non-word is `?`.
    let many = [upgrade_peer(
        "m16",
        ["a", "b", "c", "d", "e", "f g"]
            .iter()
            .enumerate()
            .map(|(n, why)| upgrading(16, n, Some(&word(why, 7_200))))
            .collect(),
    )];
    let many = held_upgrades_line(&many).expect("held");
    assert!(
        many.contains("(? 1, a 1, b 1, c 1, +2 more)") && chars(&many) <= HEADER_CHARS,
        "{many}"
    );
}

fn upgrading_fleet(_: &FleetAsk<'_>) -> Result<FleetRead, FleetRefused> {
    let mut lines =
        vec!["instance pid=7 host=m16 version=0.200.0 sessions=5 level=content at_ms=1".to_owned()];
    lines.extend((1..=5).map(|n| upgrading(16, n, Some("pending/2.1.325/account/2h"))));
    Ok(FleetRead {
        peers: vec![PeerSurvey {
            authenticated: None,
            reason: None,
            label: "m16".to_owned(),
            answer: Ok(lines),
            fault: PeerFault::Other,
            unproven: false,
            node: None,
            addr: None,
        }],
        note: None,
        alone: None,
    })
}

/// `survey --fleet` leads with the held-upgrades line under its data line, reserved within the
/// budget as the data line is (and kept only where every block shows with it); a local read and
/// `--host` show each session's word and no such line; `--json` carries the field.
/// NEGATIVE CONTROL: a read that never passes the line to the digest fails the first arm.
#[test]
fn a_fleet_read_leads_with_its_held_upgrades_within_the_budget() {
    let parts = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    let here = || Ok(vec![reply(0, vec![shell(2)])]);
    let read = |words: &[&str]| {
        read_survey(
            &parse_args(&parts(words)).unwrap(),
            Some(upgrading_fleet),
            here,
            None,
        )
    };
    let printed = read(&["survey", "--fleet"]);
    let text = printed.out.expect("the digest");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(DATA_LINE), "{text}");
    let lead = lines.next().unwrap_or_default();
    assert!(
        lead.starts_with("upgrades held ") && lead.contains(" 5 sessions on 1 Mac (account 5)"),
        "{text}"
    );
    let headers = |text: &str| text.lines().filter(|line| line.starts_with("== ")).count();
    let blocks = headers(&text);
    assert!(blocks > 1, "{text}");
    for budget in (240..4_000).step_by(37) {
        let budget_word = budget.to_string();
        let printed = read(&["survey", "--fleet", "--budget", &budget_word]);
        let text = printed.out.expect("the digest");
        assert!(chars(&text) <= budget, "{budget}: {} {text}", chars(&text));
        for (n, line) in text.lines().enumerate() {
            assert!(
                n == 1 || !line.starts_with("upgrades held "),
                "{budget}: only under the data line: {text}"
            );
        }
        if text.lines().nth(1) == Some(lead) {
            assert_eq!(
                headers(&text),
                blocks,
                "{budget}: every block with it: {text}"
            );
        }
    }
    for words in [&["survey", "--host", "m16"][..], &["survey"]] {
        let text = read(words).out.expect("the read");
        assert!(
            !text.lines().any(|line| line.starts_with("upgrades held ")),
            "{words:?}: {text}"
        );
    }
    let host = read(&["survey", "--host", "m16"]).out.expect("the read");
    assert_eq!(
        host.matches(" upgrade=pending/2.1.325/account/2h").count(),
        5,
        "{host}"
    );
    let printed = read(&["survey", "--fleet", "--json"]);
    let json = printed.out.expect("the records");
    assert_eq!(
        json.matches("\"upgrade\":\"pending/2.1.325/account/2h\"")
            .count(),
        5,
        "{json}"
    );
    assert!(
        !printed
            .err
            .iter()
            .any(|line| line.contains("upgrades held")),
        "{:?}",
        printed.err
    );
}

/// The `--fleet` lead costs no block: at every budget where each block's `==` header fits folded
/// into its counts but not with the lead too, the read leaves the lead out and shows every
/// header; with room for both it shows both; where blocks are cut anyway the lead is left out
/// too, so it never takes a block's line.
/// NEGATIVE CONTROL: keeping the lead wherever any reply line fits with it (no
/// [`Fitted::cut`] check) cuts the last Macs' blocks into the closing line in the first arm.
#[test]
fn the_fleet_lead_is_left_out_before_any_block() {
    let replies: Vec<HostReply> = (0..20)
        .map(|n| {
            upgrade_peer(
                &format!("m{n}"),
                vec![upgrading(n, 1, Some("pending/2.1.325/account/2h"))],
            )
        })
        .collect();
    let lead = held_upgrades_line(&replies).expect("held");
    let headers = |text: &str| text.lines().filter(|line| line.starts_with("== ")).count();
    let blocks = blocks_of(&replies, None, false);
    let folded = render(&blocks, &Shown::filled(&blocks, &[], 0));
    assert_eq!(headers(&folded), replies.len(), "{folded}");
    let floor = chars(DATA_LINE) + 1 + chars(&folded);
    let both = floor + chars(&lead) + 1;
    for budget in floor..both {
        let text = render_read(&replies, budget, Some(&lead), &[], None);
        assert!(chars(&text) <= budget, "{budget}: {text}");
        assert!(
            !text.contains(&lead),
            "{budget}: the lead before a block: {text}"
        );
        assert_eq!(
            headers(&text),
            replies.len(),
            "{budget}: every block: {text}"
        );
    }
    let text = render_read(&replies, both, Some(&lead), &[], None);
    assert_eq!(text.lines().nth(1), Some(lead.as_str()), "{text}");
    assert_eq!(headers(&text), replies.len(), "{text}");
    for budget in [floor - 1, floor - 200, floor / 2] {
        let text = render_read(&replies, budget, Some(&lead), &[], None);
        assert!(chars(&text) <= budget, "{budget}: {text}");
        assert!(
            text.contains(" more lines past the "),
            "{budget}: cut: {text}"
        );
        assert!(
            !text.contains(&lead),
            "{budget}: no lead in a cut read: {text}"
        );
    }
}
