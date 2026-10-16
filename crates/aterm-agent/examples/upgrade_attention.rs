// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE REVIEWER'S INSTRUMENT (kept in the tree, linted with `--all-targets`): the REAL `Row::attention` over
// every upgrade state, one line per state (`today=` from `stall()` / `asks_on_its_own`, `now=` from
// `attention`). `targo --unverified run -q -p aterm-agent --example upgrade_attention`.
use aterm_agent::harness::upgrade::{Agent, Phase, Request};
use aterm_agent::harness::upgrade_drive::{Attention, HeldBy, LIMIT_GRACE_S, Row, STALLED_AFTER_S};

const NOW: u64 = 1_790_640_000;
const H: u64 = 3_600;

fn base(agent: Agent, behind_h: u64) -> Row {
    Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: "s-3e79b5bde172ad0088ad".into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        source: "managed".into(),
        behind_since: NOW - behind_h * H,
        agent,
        ..Row::default()
    }
}

fn holders(n: usize) -> Vec<HeldBy> {
    (0..n)
        .map(|i| HeldBy {
            pid: 4000 + i as u32,
            name: "zsh".into(),
            since: NOW - 600,
        })
        .collect()
}

fn show(name: &str, row: &Row) {
    let a = row.attention(NOW);
    let stall = row.stall(NOW);
    let (class, kind, sev, why, then) = match &a {
        Attention::Quiet => (
            "QUIET".to_string(),
            "-".to_string(),
            "-".to_string(),
            String::new(),
            String::new(),
        ),
        Attention::Fault(n) => (
            "FAULT".to_string(),
            n.kind.to_string(),
            format!("{:?}", n.severity),
            format!("{} | {}", n.title, n.why),
            n.then.clone(),
        ),
        Attention::Digest(n) => (
            "DIGEST".to_string(),
            n.kind.to_string(),
            format!("{:?}", n.severity),
            format!("{} | {}", n.title, n.why),
            n.then.clone(),
        ),
        Attention::Interrupt(n) => (
            "INTERRUPT".to_string(),
            n.kind.to_string(),
            format!("{:?}", n.severity),
            format!("{} | {}", n.title, n.why),
            n.then.clone(),
        ),
    };
    let today = match (&stall, row.asks_on_its_own(NOW)) {
        (None, _) => "quiet",
        (Some(_), true) => "record",
        (Some(_), false) => "ROW+MARK",
    };
    println!("{name}\ttoday={today}\tnow={class}\tkind={kind}\tsev={sev}\twhy={why}\tthen={then}");
}

fn main() {
    println!(
        "### CLAUDE, PENDING/ANNOUNCED, every wait word, behind 1h / 7h / 30h, 0 or 1 unanswered rounds"
    );
    let waits = [
        "",
        "settling",
        "attended",
        "busy",
        "not-idle:busy",
        "not-idle:shell",
        "not-idle:waiting",
        "not-idle",
        "status-stale:busy",
        "held",
        "limited",
        "login",
        "queued",
        "draft",
        "box",
        "background",
        "awaiting-ready",
        "release:awaiting-ready",
        // The quiet upgrade's own words (2026-09-29): its first look, a READY
        // on the screen not yet in the transcript, a READY the agent was
        // moved on from.
        "notice-shown",
        "ready-unrecorded",
        "superseded",
        "announce-refused:changed",
        "announce-refused:yield",
        "announce-refused:held",
        "changed",
        "changed-before-signal",
        "signal-refused",
        "notice-owned-by-other-process",
        "tasked",
        "self",
        "not-foreground",
        "ids",
        "terminal:tmux",
        "terminal:none",
        // Main's words of 2026-09-29/30: the watch's neutral word, a tab
        // another supervisor holds, aterm's own words left in the prompt.
        "next-point",
        "claimed",
        "left-typed:unsent",
        "left-typed:mixed",
        // A tab whose Autopilot the owner turned off (2026-10-15).
        "autopilot-off",
    ];
    for phase_name in ["pending", "announced"] {
        for behind in [1u64, 7, 30] {
            for rounds in [0u32, 1] {
                for w in waits {
                    let mut r = base(Agent::Claude, behind);
                    r.phase = if phase_name == "pending" {
                        Phase::Pending
                    } else {
                        Phase::Announced {
                            at_s: NOW - 600,
                            asks: 1,
                        }
                    };
                    r.wait = w.to_string();
                    r.wait_since = if w.is_empty() {
                        0
                    } else {
                        NOW - behind.min(30) * H
                    };
                    // Minted when it became behind: a record nothing visited
                    // since is the `wait=-` rows.
                    r.minted_at = NOW - behind * H;
                    if rounds > 0 {
                        r.streak_why = "unanswered".into();
                        r.stop_streak = rounds;
                    }
                    if matches!(w, "background" | "not-idle:shell") {
                        r.held_by = holders(16);
                    }
                    show(
                        &format!(
                            "C-{phase_name}-{behind}h-r{rounds}\twait={}",
                            if w.is_empty() { "-" } else { w }
                        ),
                        &r,
                    );
                }
            }
        }
    }
    println!("\n### CLAUDE, STOPPED / IN FLIGHT");
    for (name, phase, why, exited) in [
        (
            "gave-up (resting)",
            Phase::Failed("unanswered".into()),
            "",
            0u64,
        ),
        (
            "signal-refused (first)",
            Phase::Failed("signal-refused".into()),
            "",
            0,
        ),
        (
            "relaunch-refused, agent alive",
            Phase::Failed("relaunch-refused".into()),
            "",
            0,
        ),
        (
            "resumed-elsewhere",
            Phase::Failed("resumed-elsewhere".into()),
            "",
            0,
        ),
        (
            "not-a-shell-job",
            Phase::Failed("not-a-shell-job".into()),
            "",
            0,
        ),
        ("argv:--foo", Phase::Failed("argv:--foo".into()), "", 0),
        (
            "line:too-long",
            Phase::Failed("line:too-long".into()),
            "",
            0,
        ),
        (
            "stale-exit AFTER exit",
            Phase::Failed("stale-exit".into()),
            "",
            NOW - 2 * H,
        ),
        (
            "no-resume AFTER exit",
            Phase::Failed("no-resume".into()),
            "",
            NOW - 2 * H,
        ),
        (
            "shell-gone AFTER exit",
            Phase::Failed("shell-gone".into()),
            "",
            NOW - 2 * H,
        ),
        ("exiting 2min", Phase::Exiting { at_s: NOW - 120 }, "", 0),
        (
            "exiting 6min (stuck)",
            Phase::Exiting { at_s: NOW - 360 },
            "",
            0,
        ),
        (
            "exiting 20min (hung)",
            Phase::Exiting { at_s: NOW - 1200 },
            "",
            0,
        ),
        (
            "exited 6min (stuck)",
            Phase::Exiting { at_s: NOW - 360 },
            "",
            NOW - 300,
        ),
        (
            "relaunched 6min (stuck)",
            Phase::Relaunched { at_s: NOW - 360 },
            "",
            NOW - 300,
        ),
        ("done", Phase::Done, "", 0),
    ] {
        let mut r = base(Agent::Claude, 7);
        r.phase = phase;
        r.exited_at = exited;
        let _ = why;
        show(&format!("C-{name}"), &r);
    }
    let mut r = base(Agent::Claude, 7);
    r.phase = Phase::Failed("signal-refused".into());
    r.streak_why = "signal-refused".into();
    r.stop_streak = 3;
    show("C-repeating stop x3", &r);
    let mut r = base(Agent::Claude, 30);
    r.phase = Phase::Failed("unanswered".into());
    r.streak_why = "unanswered".into();
    r.stop_streak = 2;
    r.wait = "failed".into();
    r.held_by = holders(16);
    show("C-gave-up x2, 30h, 16 holders", &r);
    let mut r = base(Agent::Claude, 30);
    r.phase = Phase::Failed("unanswered".into());
    r.streak_why = "unanswered".into();
    r.stop_streak = 2;
    r.wait = "failed".into();
    show("C-gave-up x2, 30h, nothing runs", &r);
    let mut r = base(Agent::Claude, 30);
    r.request = Request::Skip("2.1.284".into());
    r.wait = "background".into();
    show("C-owner skipped", &r);
    println!(
        "\n### THE SECOND REVIEW (2026-09-29): asked again and again, a relaunch that never idles, a refusal"
    );
    for (label, behind_h, asks) in [
        ("announced asks=9, 30h", 30u64, 9u32),
        ("announced asks=9, 100d", 2_400, 9),
        ("announced asks=4, 30h", 30, 4),
        ("announced asks=3, 30h", 30, 3),
    ] {
        let mut r = base(Agent::Claude, behind_h);
        r.phase = Phase::Announced {
            at_s: NOW - 60,
            asks,
        };
        r.minted_at = NOW - behind_h * H;
        r.noticed_at = NOW - 60;
        r.wait = "background".into();
        r.wait_since = NOW - 3_600;
        r.own_work_at = NOW - 60;
        r.held_by = holders(3);
        show(&format!("C-{label}\twait=background"), &r);
    }
    for (label, age) in [
        ("relaunched 6min", 360u64),
        ("relaunched 20min (fault)", 1_200),
        ("relaunched 40min (interrupt)", 2_400),
        ("relaunched 2 days", 2 * 86_400),
    ] {
        let mut r = base(Agent::Claude, 7);
        r.phase = Phase::Relaunched { at_s: NOW - age };
        r.exited_at = NOW - age - 5;
        show(&format!("C-{label}"), &r);
    }
    for why in ["held", "draft", "box", "changed"] {
        for (label, waited) in [("2h", 2 * H), ("2d", 48 * H)] {
            let mut r = base(Agent::Claude, 5);
            r.phase = Phase::Pending;
            r.minted_at = NOW - waited - H;
            r.wait = format!("announce-refused:{why}");
            r.wait_since = NOW - waited;
            r.refused = 5;
            r.refused_at = NOW - waited;
            show(
                &format!("C-refused {label}\twait=announce-refused:{why}"),
                &r,
            );
        }
    }
    println!("\n### A USAGE LIMIT'S NAMED RESET: ahead, inside its grace, passed; and none read");
    for phase_name in ["pending", "announced", "gave-up"] {
        for (label, until) in [
            ("reset ahead", NOW + H),
            ("reset inside its grace", NOW - LIMIT_GRACE_S + 60),
            ("reset passed 4h ago", NOW - 4 * H),
            ("no reset read", 0),
        ] {
            for behind in [1u64, 30] {
                let mut r = base(Agent::Claude, behind);
                r.phase = match phase_name {
                    "pending" => Phase::Pending,
                    "announced" => Phase::Announced {
                        at_s: NOW - 600,
                        asks: 1,
                    },
                    _ => Phase::Failed("unanswered".into()),
                };
                r.wait = "limited".into();
                r.wait_since = NOW - behind.min(30) * H;
                r.minted_at = NOW - behind * H;
                r.limit_until = until;
                show(&format!("L-{phase_name}-{behind}h\t{label}"), &r);
            }
        }
    }
    println!("\n### CODEX (same code, its words)");
    for w in [
        "settling",
        "attended",
        "busy",
        "held",
        "limited",
        "login",
        "draft",
        "box",
        "background",
        "background-terminal",
        "awaiting-ready",
        "daemon-first:busy-thread",
        "daemon-first:background-terminal",
        "daemon-first:owner-held",
        "daemon-first:unseen-client",
        "daemon-first:clients-unreadable",
        "daemon-first:attended",
        "daemon-first:held",
        "daemon-first:update-failed",
        "daemon-first:vendor-ahead",
        "daemon-first:behind",
        "left-typed-backoff",
        "left-typed:x",
        "terminal:tmux",
        "no-shell-integration",
        "thread-locked",
    ] {
        let mut r = base(Agent::Codex, 30);
        r.phase = Phase::Pending;
        r.wait = w.into();
        r.wait_since = NOW - 30 * H;
        show(&format!("X-pending-30h\twait={w}"), &r);
    }
    let _ = STALLED_AFTER_S;
}
