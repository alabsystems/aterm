// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the fleet read sighted, for the fleet tables (`aterm_db::fleet`): [`sightings`] reads
//! the same `aterm ctl survey --fleet --json` answer the glance's `parse` reads, and keeps what
//! the glance has no use for: this Mac's own windows' replies, as one machine, and every agent
//! session with its account word, model, state and how long it has stood.
//!
//! It asks nothing: the glance's reader makes the read and hands each answer to this process's
//! folder (`aterm_db::fleet::folder`) on its own thread (`super::offer_sightings`). The account
//! a session names goes on as an `AccountWord`, which the fold resolves to a registry key and
//! drops; a model, a build and a reason token go on only as bounded words
//! (`aterm_db::fleet::read::word`).

use super::{AgentWord, MACS_MAX, TaskWord, dial_of, field, short_name};
use aterm_db::fleet::read::{
    AGENTS_MAX, AccountWord, AgentSeen, Answer, Answered, Harness, MachineSeen, Read, State, Tally,
    Task, word,
};
use aterm_json::{Map, Value};
use std::collections::BTreeSet;

/// A header's or a row's count: digits and nothing else.
fn count(object: &Map, key: &str) -> Option<u64> {
    field(object, key)
        .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
}

/// A row's percentage.
fn percent(row: &Map, key: &str) -> Option<u8> {
    count(row, key)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| *n <= 100)
}

/// What one machine's replies said: one reply for a peer, each of its windows' for this Mac.
#[derive(Default)]
struct Heard {
    build: Option<String>,
    home_free_mb: Option<u64>,
    label: Option<String>,
    sessions: u32,
    /// A window went unread, or a reply left rows out: the rows are a lower bound.
    partial: bool,
    /// A live row sent no `needs`: an older aterm, whose agents cannot be classed.
    older: bool,
    tally: Tally,
    agents: Vec<AgentSeen>,
    agents_more: u32,
    /// The sessions counted so far: two windows of one Mac may both list one.
    sids: BTreeSet<String>,
}

impl Heard {
    /// Add one reply: its header and its session rows.
    fn hear(&mut self, instance: &Map, sessions: &[Value]) {
        let sent: Vec<&Map> = sessions.iter().filter_map(Value::as_object).collect();
        if self.build.is_none() {
            self.build = field(instance, "version").and_then(|build| word(build, 48));
        }
        if self.home_free_mb.is_none() {
            self.home_free_mb = count(instance, "home_free_mb");
        }
        if self.label.is_none() {
            self.label = field(instance, "label").map(str::to_owned);
        }
        // A node service answers for every window it reaches; one that answered alone read
        // itself, and so do both counts where the header sends neither.
        let unread = count(instance, "instances").unwrap_or(1)
            > count(instance, "instances_read").unwrap_or(1);
        let left_out = count(instance, "sessions")
            .is_some_and(|claimed| claimed > u64::try_from(sent.len()).unwrap_or(u64::MAX));
        self.partial |= unread || left_out;
        for row in sent {
            if field(row, "state") == Some("exited") {
                continue;
            }
            let sid = field(row, "sid");
            if sid.is_some_and(|sid| !self.sids.insert(sid.to_owned())) {
                continue;
            }
            self.sessions = self.sessions.saturating_add(1);
            self.older |= !row.contains_key("needs");
            let needs = row.get("needs").and_then(Value::as_str);
            let Some(agent) =
                AgentWord::of_row(field(row, "program_kind"), field(row, "agent"), needs)
            else {
                continue;
            };
            let task = TaskWord::of(field(row, "task"), field(row, "task_conclusion"));
            // A stalled task says more than an agent's `busy` or `idle`; a wait on a person and
            // a limit say more than it.
            let stalled = task == Some(TaskWord::Stalled);
            let (state, state_why) = match agent {
                AgentWord::Waiting => (State::Waiting, None),
                AgentWord::Walled(kind) => (State::Walled, word(&kind, 32)),
                _ if stalled => (State::Stuck, None),
                AgentWord::Busy => (State::Busy, None),
                AgentWord::Idle => (State::Idle, None),
                AgentWord::Unknown => (State::Unknown, None),
            };
            self.tally.add(state);
            let Some(sid) = sid
                .and_then(|sid| word(sid, 40))
                .filter(|_| self.agents.len() < AGENTS_MAX)
            else {
                self.agents_more = self.agents_more.saturating_add(1);
                continue;
            };
            self.agents.push(AgentSeen {
                sid,
                harness: Harness::of_program(field(row, "program")),
                state,
                state_why,
                state_age_ms: count(row, "agent_ms"),
                task: task.map(|task| match task {
                    TaskWord::Working => Task::Working,
                    TaskWord::Done => Task::Done,
                    TaskWord::Failed => Task::Failed,
                    TaskWord::Exited => Task::Exited,
                    TaskWord::Stalled => Task::Stalled,
                }),
                ctx_left_pct: percent(row, "ctx_left_pct"),
                usage_5h_pct: percent(row, "usage_5h_pct"),
                usage_7d_pct: percent(row, "usage_7d_pct"),
                account: field(row, "account").and_then(AccountWord::new),
                model: field(row, "model").and_then(|model| word(model, 48)),
            });
        }
    }

    /// The answer these replies make. A partial answer has no session count, and neither a
    /// partial one nor an older aterm's has a tally: a lower bound is no count.
    fn answered(mut self) -> Answered {
        self.agents.sort_by(|a, b| a.sid.cmp(&b.sid));
        Answered {
            build: self.build,
            home_free_mb: self.home_free_mb,
            sessions: (!self.partial).then_some(self.sessions),
            tally: (!self.partial && !self.older).then_some(self.tally),
            agents: self.agents,
            agents_more: self.agents_more,
        }
    }
}

/// A host object's session rows.
fn rows_of(host: &Map) -> &[Value] {
    host.get("sessions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

/// THE FLEET READ, AS THE FLEET TABLES TAKE IT: every machine the survey `json` names, read at
/// `now_ms` (Unix milliseconds).
///
/// This Mac comes first, where any of its windows replied: their replies (the host objects
/// that are no fleet peer's) are one machine, a session two of them list counted once. A local
/// aterm process that did not answer (its host object holds an error and no header) makes that
/// answer partial: the sessions the others listed are a lower bound, and no count. A fleet
/// peer that proved one of this Mac's own node ids is this Mac answering itself, and is left
/// out. Every other peer is a machine: one that answered, with its header's facts and each
/// agent session; one that did not, silent, with the producer's reason token where it gave one.
/// A peer that proved no node key this Mac binds keeps the name the book dials and not the
/// label it claims, and is marked unproven (`MachineSeen::unproven`). At most [`MACS_MAX`]
/// peers are taken, the most a book holds.
///
/// A read whose question of the whole fleet was refused (this Mac is away, its roster is
/// unread) still heard this Mac's own windows: it is this Mac alone, and says so
/// (`Read::fleet` is false), so no peer is taken for asked.
///
/// # Errors
/// `json` is no survey answer, or it is the row that stands for a refused read of the whole
/// fleet with no window of this Mac replying beside it, which names no machine.
pub(crate) fn sightings(json: &str, now_ms: u64) -> Result<Read, String> {
    let value: Value =
        aterm_json::from_str(json).map_err(|e| format!("the fleet read is no survey: {e}"))?;
    let hosts: Vec<&Map> = value
        .as_array()
        .ok_or_else(|| "the fleet read is no list".to_string())?
        .iter()
        .filter_map(Value::as_object)
        .collect();
    let is_peer = |host: &Map| host.get("fleet").and_then(Value::as_bool) == Some(true);

    let mut own: Option<Heard> = None;
    let mut own_nodes: BTreeSet<String> = BTreeSet::new();
    // A live aterm process of this Mac that did not answer: its sessions are in no reply.
    let mut own_unheard = false;
    for host in hosts.iter().filter(|host| !is_peer(host)) {
        let Some(instance) = host.get("instance").and_then(Value::as_object) else {
            own_unheard = true;
            continue;
        };
        own.get_or_insert_with(Heard::default)
            .hear(instance, rows_of(host));
        own_nodes.extend(field(instance, "node").map(str::to_ascii_lowercase));
    }

    let mut machines: Vec<MachineSeen> = Vec::new();
    if let Some(mut own) = own {
        own.partial |= own_unheard;
        let label = own.label.take();
        machines.push(MachineSeen::own(
            label.as_deref(),
            Answer::Answered(own.answered()),
        ));
    }
    let heard_own = machines.len();
    let mut fleet = true;
    for host in hosts.iter().filter(|host| is_peer(host)).take(MACS_MAX) {
        let dial = dial_of(field(host, "label").unwrap_or("a peer"));
        let silent = |why: Option<String>| {
            MachineSeen::peer(dial, &short_name(dial), None, Answer::Silent { why })
        };
        if let Some(why) = field(host, "error") {
            if field(host, "reason") == Some(aterm_ctl::FLEET_UNREAD) {
                if heard_own == 0 {
                    return Err(crate::needs::fold_clip(
                        &format!("fleet not read: {why}"),
                        120,
                    ));
                }
                // No peer was asked: this Mac's own answer is all this read holds.
                machines.truncate(heard_own);
                fleet = false;
                break;
            }
            machines.push(silent(
                field(host, "reason").and_then(|reason| word(reason, 32)),
            ));
            continue;
        }
        let Some(instance) = host.get("instance").and_then(Value::as_object) else {
            machines.push(silent(Some("no-header".to_owned())));
            continue;
        };
        let unproven = host.get("unproven").and_then(Value::as_bool) == Some(true);
        let node = field(instance, "node").map(str::to_ascii_lowercase);
        if !unproven && node.is_some_and(|node| own_nodes.contains(&node)) {
            continue;
        }
        let mut heard = Heard::default();
        heard.hear(instance, rows_of(host));
        let label = heard.label.take();
        let seen = MachineSeen::peer(
            dial,
            &short_name(dial),
            label.as_deref(),
            Answer::Answered(heard.answered()),
        );
        machines.push(if unproven { seen.unproven() } else { seen });
    }
    Ok(Read {
        read_at_ms: now_ms,
        machines,
        fleet,
    })
}
