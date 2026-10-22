// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared helpers: a fake clock (the crate reads none), a session with an
//! agent run open, the runs' time order every log keeps, and a realistic
//! session's records.

use std::collections::HashMap;

use aterm_slog::{
    Actor, Close, Conf, Fields, Key, Kind, Log, Lvl, Mark, Node, Open, Out, RunOut, Src, Stamp,
    TurnOut,
};

/// Wall time of the test epoch (2025-10-09), so `t=` reads like a real one.
pub const WALL0: u64 = 1_760_000_000_000;

/// The stamp `ms` into the test: wall and monotonic move together.
#[must_use]
pub fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

/// A session log with its session opened and an agent run (`claude`) open
/// under it, at `ms`.
pub fn agent_session(log: &mut Log, ms: u64) -> Node {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(ms),
    )
    .expect("the session opens");
    let f = Fields::new()
        .tok(Key::Agent, "claude")
        .expect("agent is a token key");
    log.open(
        Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(f),
        at(ms),
    )
    .expect("the agent run opens")
}

/// Every record of `log`'s window, encoded at the content tier.
#[must_use]
pub fn lines(log: &Log) -> Vec<String> {
    log.window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
        .collect()
}

/// THE RUNS' TIME ORDER over the records `lines` (lifted from
/// `aterm-gui`'s `session_log_tests.rs`, which keeps its own copy for the
/// timeline's logs): (1) a child's close `t=` and `dur_ms` never exceed its
/// parent's; (2) no node closes before it opened; (3) a `cmd` run never
/// opens before the `cmd` run logged before it closed; (4) no node opens
/// before the node it hangs under (the session node aside). `Err` names the
/// first record that breaks one.
pub fn time_order_holds(lines: &[String]) -> Result<(), String> {
    let field = |l: &str, k: &str| -> Option<u64> {
        l.split(' ')
            .find_map(|w| w.strip_prefix(k))
            .and_then(|v| v.parse().ok())
    };
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut opened: HashMap<String, u64> = HashMap::new();
    let mut closed: HashMap<String, (u64, u64)> = HashMap::new();
    let mut cmds: Vec<String> = Vec::new();
    for l in lines {
        let w: Vec<&str> = l.split(' ').collect();
        let t = field(l, "t=").unwrap_or(0);
        match (w.get(1), w.get(2), w.get(3)) {
            (Some(&"open"), Some(node), kind) => {
                if let Some(p) = l.split(' ').find_map(|x| x.strip_prefix("par=")) {
                    if let Some(pt) = opened.get(p).filter(|_| p != "-")
                        && t < *pt
                    {
                        return Err(format!(
                            "{node} opened t={t}, before its parent {p} opened (t={pt})"
                        ));
                    }
                    parent.insert((*node).to_string(), p.to_string());
                }
                if kind == Some(&"cmd") {
                    if let Some(prev) = cmds.last()
                        && let Some((pt, _)) = closed.get(prev)
                        && t < *pt
                    {
                        return Err(format!(
                            "{node} opened t={t}, before {prev} closed (t={pt})"
                        ));
                    }
                    cmds.push((*node).to_string());
                }
                opened.insert((*node).to_string(), t);
            }
            (Some(&"close"), Some(node), _) => {
                if let Some(ot) = opened.get(*node)
                    && t < *ot
                {
                    return Err(format!("{node} closed t={t}, before it opened (t={ot})"));
                }
                let dur = field(l, "dur_ms=").unwrap_or(0);
                closed.insert((*node).to_string(), (t, dur));
            }
            _ => {}
        }
    }
    for (child, (t, dur)) in &closed {
        if let Some(p) = parent.get(child)
            && let Some((pt, pdur)) = closed.get(p)
            && (t > pt || dur > pdur)
        {
            return Err(format!(
                "{child} closed t={t} dur_ms={dur}, after its parent {p} (t={pt} dur_ms={pdur})"
            ));
        }
    }
    Ok(())
}

/// THE MAIL LINK IN `brief`, NEVER WITHOUT ITS `~`,
/// on `log` at `now_wall`: in its brief with nothing shed, a
/// `mail=` token is exactly `mail=@<off>~` and only on the line of a turn
/// the fold links to mail `<off>`, heuristically; a linked turn's line
/// carries it; and no record `log` prints holds one. `Ok` is how many
/// tokens the brief printed (a caller's proof it checked something).
pub fn brief_mail_links_hold(log: &Log, now_wall: u64) -> Result<usize, String> {
    let links = log.reader_fold().mail_links();
    let text = log.brief("s-1", 1 << 24, now_wall).text;
    let mut printed = 0;
    for line in text.lines().skip(1) {
        let link = line
            .split(' ')
            .next()
            .and_then(|head| head.strip_prefix('t'))
            .and_then(|n| n.parse::<u32>().ok())
            .and_then(|n| {
                links
                    .iter()
                    .find(|(node, _)| *node == Node::new(Lvl::Turn, n))
                    .map(|(_, l)| *l)
            });
        let tokens: Vec<&str> = line.split(' ').filter(|w| w.starts_with("mail=")).collect();
        match (link, tokens.as_slice()) {
            (None, []) => {}
            (Some(l), [tok])
                if l.conf == Conf::Heuristic && *tok == format!("mail=@{}~", l.off) =>
            {
                printed += 1;
            }
            _ => {
                return Err(format!(
                    "the brief line `{line}` against the fold's link {link:?}:\n{text}"
                ));
            }
        }
    }
    if let Some(l) = lines(log).iter().find(|l| l.contains("mail=@")) {
        return Err(format!("`log` prints a mail link: `{l}`"));
    }
    Ok(printed)
}

/// A realistic session's records: an agent run with turns of verdict marks,
/// tool-like steps and closes, and shell commands with their exit codes.
#[must_use]
pub fn realistic_log(turns: u64) -> Log {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    for t in 0..turns {
        ms += 100;
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(ms),
            )
            .expect("a turn opens");
        for w in ["busy", "prompt", "busy", "idle"] {
            ms += 37;
            let f = Fields::new()
                .word(Key::Word, w)
                .and_then(|f| f.u(Key::Rev, ms))
                .and_then(|f| f.id(Key::Gen, &format!("0.{ms}")))
                .and_then(|f| f.id(Key::Fp, &format!("{:016x}", ms * 2654435761)))
                .expect("verdict fields");
            log.mark(
                Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
                    .at(run)
                    .fields(f),
                at(ms),
            )
            .expect("a verdict mark");
        }
        let step = log
            .open(
                Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                    .under(turn)
                    .fields(Fields::new().word(Key::Box, "bash").expect("a box")),
                at(ms),
            )
            .expect("a step");
        ms += 900;
        log.close(
            step,
            Close::new(
                Out::Step(aterm_slog::StepOut::Approved),
                Src::Screen,
                Conf::Strong,
            ),
            at(ms),
        )
        .expect("the step closes");
        ms += 1200;
        log.close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(ms),
        )
        .expect("the turn closes");
        if t % 3 == 0 {
            let f = Fields::new()
                .u(Key::Blk, t)
                .and_then(|f| f.tok(Key::Detail, "cargo"))
                .expect("cmd fields");
            let cmd = log
                .open(
                    Open::new(Kind::Cmd, Src::Shell, Conf::Strong).fields(f),
                    at(ms),
                )
                .expect("a command");
            ms += 300;
            log.close(
                cmd,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact)
                    .fields(Fields::new().i(Key::Code, 0).expect("a code")),
                at(ms),
            )
            .expect("the command closes");
        }
    }
    log
}
