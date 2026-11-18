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
//! (`aterm_db::fleet::read::word`). A machine's look (its resources, as its header says) goes on
//! as numbers and closed words, its target only in a triple's shape
//! (`aterm_types::look_words::target_of`, as the machine's own parser reads it), its two ages
//! each a difference of two times on its own clock.

use super::{AgentWord, MACS_MAX, TaskWord, dial_of, field, glance_of, short_name};
use aterm_db::fleet::read::{
    AGENTS_MAX, Access, AccountWord, AgentSeen, Answer, Answered, Harness, Limits, Load, Look,
    MachineSeen, OutSrc, Pressure, Read, Tally, TurnFacts, WaitTally, Work, WorkTally,
    activity_word, need_of, word,
};
use aterm_json::{Map, Value};
use aterm_types::look_words;
use aterm_types::update_standing::UpdateFact;
use std::collections::BTreeSet;

/// A header's or a row's count: digits and nothing else.
fn count(object: &Map, key: &str) -> Option<u64> {
    field(object, key)
        .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
}

/// A `0` or `1` flag: `1` true, `0` false, any other word or none unread.
fn flag(object: &Map, key: &str) -> Option<bool> {
    match field(object, key) {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// A row's percentage.
fn percent(row: &Map, key: &str) -> Option<u8> {
    count(row, key)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| *n <= 100)
}

/// A row's count that fits a `u32`.
fn count32(row: &Map, key: &str) -> Option<u32> {
    count(row, key).and_then(|n| u32::try_from(n).ok())
}

/// What a session row says of its turn and its output (`turn= turn_ms= turn_end_ms= step_ms=
/// out_ms= out_src= bg=`), each read on its own; a row with no `turn=` is an aterm before the
/// turn ([`TurnFacts::older`]), whose turn its agent's own word says ([`TurnFacts::word_turn`]:
/// `busy` and a box, `prompt`, open one, as that aterm's turn tracker reads them).
fn turn_of(row: &Map) -> TurnFacts {
    TurnFacts {
        turn_open: flag(row, "turn"),
        turn_age_ms: count(row, "turn_ms"),
        turn_end_age_ms: count(row, "turn_end_ms"),
        step_age_ms: count(row, "step_ms"),
        out_age_ms: count(row, "out_ms"),
        out_src: field(row, "out_src").and_then(OutSrc::of),
        bg: count32(row, "bg"),
        older: !row.contains_key("turn"),
        word_turn: matches!(field(row, "agent"), Some("busy" | "prompt")),
    }
}

/// When a reply answered, its header's `at_ms=` (Unix ms on its machine's clock), or a row's own
/// where a node's merge stamped it with its window's: `None` where it says none, `-`, or 0,
/// which its writer sends with its clock unread, never a time.
fn at_ms_of(record: &Map) -> Option<u64> {
    count(record, "at_ms").filter(|ms| *ms > 0)
}

/// A row's build wait: `None` where it says no `wait=` (an aterm before it); `Some(None)` where
/// it waits for none (`wait=-`); `Some(Some(mb))` where it waits for memory, `mb` its claim in
/// MiB from `wait_gib=` (GiB to a tenth: `<digits>.<digit>`), `None` where that is `-` or
/// anything else.
fn wait_of(row: &Map) -> Option<Option<Option<u64>>> {
    if !row.contains_key("wait") {
        return None;
    }
    Some((field(row, "wait") == Some("memory")).then(|| {
        let (whole, tenth) = field(row, "wait_gib")?.split_once('.')?;
        let digits = |raw: &str| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || tenth.len() != 1 || !digits(tenth) {
            return None;
        }
        let tenths = whole
            .parse::<u64>()
            .ok()?
            .checked_mul(10)?
            .checked_add(tenth.parse().ok()?)?;
        Some(tenths.checked_mul(1024)?.checked_add(5)? / 10)
    }))
}

/// A header's look: `None` for a header with no `m_at_ms=` (an aterm before the look), else
/// each figure read on its own, by [`count`] (digits alone, in range), a percentage by
/// [`percent`], a closed word by its set, the target by a triple's shape
/// ([`look_words::target_of`]), a verdict by `0` or `1`, the load score by its two parts
/// ([`Load::of`]), and anything else unread. The two ages
/// are the header's own `at_ms=` less its `m_at_ms=` and its `up_since_ms=`: times on the
/// machine's clock, never this Mac's; one that would be negative is unread. A size or a core
/// count of 0 is no capacity and reads unread, as the machine's own parser has it.
fn look_of(instance: &Map) -> Option<Look> {
    if !instance.contains_key("m_at_ms") {
        return None;
    }
    let capacity = |key: &str| count(instance, key).filter(|n| *n > 0);
    let verdict = |key: &str| flag(instance, key);
    let at_ms = at_ms_of(instance);
    let since = |key: &str| {
        at_ms
            .zip(count(instance, key))
            .and_then(|(at, then)| at.checked_sub(then))
    };
    let up_since_own_ms = count(instance, "up_since_ms").filter(|ms| *ms > 0);
    Some(Look {
        age_ms: since("m_at_ms"),
        cores: capacity("cores"),
        load1_x100: count(instance, "load1_x100"),
        cpu_busy_x100: count(instance, "cpu_busy_x100"),
        rate_win_ms: count(instance, "rate_win_ms"),
        mem_total_mb: capacity("mem_total_mb"),
        mem_used_mb: count(instance, "mem_used_mb"),
        mem_avail_mb: count(instance, "mem_avail_mb"),
        swap_used_mb: count(instance, "swap_used_mb"),
        swap_total_mb: count(instance, "swap_total_mb"),
        disk_total_mb: capacity("home_total_mb"),
        gpu_cores: capacity("gpu_cores"),
        gpu_busy_pct: percent(instance, "gpu_busy_pct").map(u64::from),
        net_link_kbps: capacity("net_link_kbps"),
        net_rx_kbps: count(instance, "net_rx_kbps"),
        net_tx_kbps: count(instance, "net_tx_kbps"),
        net_other_kbps: count(instance, "net_other_kbps"),
        mem_pressure: field(instance, "mem_pressure").and_then(Pressure::of),
        limits: Limits {
            cpu: verdict("cpu_lim"),
            mem: verdict("mem_lim"),
            disk: verdict("disk_lim"),
            gpu: verdict("gpu_lim"),
        },
        load: field(instance, "load").and_then(Load::of),
        up_since_own_ms,
        up_for_ms: up_since_own_ms.and_then(|_| since("up_since_ms")),
        os: field(instance, "os").and_then(look_words::os_of),
        target: field(instance, "target").and_then(look_words::target_of),
    })
}

/// A header's fleet access and whether a drive grant there admits this Mac: unread both where
/// it has no `access=` (an aterm that does not say), else its word where this build knows it,
/// and `drive=1` or not.
fn access_of(instance: &Map) -> Option<(Option<Access>, bool)> {
    instance.contains_key("access").then(|| {
        (
            field(instance, "access").and_then(Access::of),
            field(instance, "drive") == Some("1"),
        )
    })
}

/// A header's updater standing: its `update=` and `update_*=` words and the `at_ms=` they were
/// said at, read by [`UpdateFact::of_fields`]; `None` where it has no `update=` (an aterm before
/// these words) or its word is none.
fn update_of(instance: &Map) -> Option<UpdateFact> {
    // A header's `at_ms=0` is its clock unread, never a time (`at_ms_of`).
    let at = at_ms_of(instance).map(|at| at.to_string());
    const KEYS: [&str; 7] = [
        "update",
        "update_at",
        "update_to",
        "update_until",
        "update_why",
        "update_tries",
        "update_checks",
    ];
    let mut fields: Vec<(&str, &str)> = KEYS
        .iter()
        .filter_map(|key| field(instance, key).map(|value| (*key, value)))
        .collect();
    if let Some(at) = &at {
        fields.push(("at_ms", at));
    }
    UpdateFact::of_fields(&fields)
}

/// What one machine's replies said: one reply for a peer, each of its windows' for this Mac.
#[derive(Default)]
struct Heard {
    build: Option<String>,
    /// The first reply's `at_ms=` that said one ([`at_ms_of`]).
    answer_at_ms: Option<u64>,
    home_free_mb: Option<u64>,
    /// The first reply's look that carried one.
    look: Option<Look>,
    /// The first reply's fleet access and drive grant that said them ([`access_of`]).
    access: Option<(Option<Access>, bool)>,
    /// The first reply's updater standing that said one ([`update_of`]).
    update: Option<UpdateFact>,
    label: Option<String>,
    sessions: u32,
    /// A window went unread, or a reply left rows out: the rows are a lower bound.
    partial: bool,
    /// A live row sent no `needs`: an older aterm, whose agents cannot be classed.
    older: bool,
    tally: Tally,
    work: WorkTally,
    /// The sessions waiting for memory, every counted row.
    wait: Option<WaitTally>,
    /// A counted row said no `wait=`: an older aterm, whose waits are unread.
    wait_unsaid: bool,
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
            // The standing is the window's whose build this is, so the two are one window's.
            if self.build.is_some() {
                self.update = update_of(instance);
            }
        }
        if self.home_free_mb.is_none() {
            self.home_free_mb = count(instance, "home_free_mb");
        }
        let at_ms = at_ms_of(instance);
        if self.answer_at_ms.is_none() {
            self.answer_at_ms = at_ms;
        }
        if self.look.is_none() {
            self.look = look_of(instance);
        }
        if self.access.is_none() {
            self.access = access_of(instance);
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
            match wait_of(row) {
                None => self.wait_unsaid = true,
                Some(None) => {}
                Some(Some(mb)) => self.wait.get_or_insert(WaitTally::NONE).add(mb),
            }
            let needs = row.get("needs").and_then(Value::as_str);
            let Some(agent) =
                AgentWord::of_row(field(row, "program_kind"), field(row, "agent"), needs)
            else {
                continue;
            };
            let task = TaskWord::of(field(row, "task"), field(row, "task_conclusion"));
            let (state, wall) = glance_of(&agent, task);
            let state_why = wall.and_then(|kind| word(kind, 32));
            let turn = turn_of(row);
            let work = Work::of(state, &turn);
            self.tally.add(state);
            self.work.add(work);
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
                task: task.map(TaskWord::fleet),
                ctx_left_pct: percent(row, "ctx_left_pct"),
                usage_5h_pct: percent(row, "usage_5h_pct"),
                usage_7d_pct: percent(row, "usage_7d_pct"),
                account: field(row, "account").and_then(AccountWord::new),
                model: field(row, "model").and_then(|model| word(model, 48)),
                pinned: flag(row, "pinned"),
                turn,
                tok_in: count(row, "tok_in"),
                tok_out: count(row, "tok_out"),
                tok_for_ms: count(row, "tok_for_ms"),
                turns: count32(row, "turns"),
                tools: count32(row, "tools"),
                need: need_of(needs),
                activity: field(row, "word").and_then(activity_word),
                work,
                // A row a node's merge sent under a header of another time says its own
                // window's (`-`: that window's clock unread), never the header's.
                answer_at_ms: if row.contains_key("at_ms") {
                    at_ms_of(row)
                } else {
                    at_ms
                },
                rate: None,
            });
        }
    }

    /// The answer these replies make. A partial answer has no session count, and neither a
    /// partial one nor an older aterm's has a tally or a work tally: a lower bound is no count.
    /// Its memory waits are counted where its sessions are, and every row said `wait=`.
    fn answered(mut self) -> Answered {
        self.agents.sort_by(|a, b| a.sid.cmp(&b.sid));
        let tallied = !self.partial && !self.older;
        Answered {
            build: self.build,
            answer_at_ms: self.answer_at_ms,
            home_free_mb: self.home_free_mb,
            look: self.look.unwrap_or(Look::UNREAD),
            access: self.access.and_then(|(access, _)| access),
            drive: self.access.map(|(_, drive)| drive),
            update: self.update,
            sessions: (!self.partial).then_some(self.sessions),
            tally: tallied.then_some(self.tally),
            work: tallied.then_some(self.work),
            wait: (!self.partial && !self.wait_unsaid)
                .then(|| self.wait.unwrap_or(WaitTally::NONE)),
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
/// that are no fleet peer's) are one machine, a session two of them list counted once, with its
/// fleet access as they say it and no drive grant read (a local read asks for none). A local
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
        let mut answered = own.answered();
        // A local read asks for no grant: this Mac's windows say `access=` without `drive=1` to
        // every local reader, which says nothing of whether this Mac may drive itself.
        answered.drive = None;
        machines.push(MachineSeen::own(
            label.as_deref(),
            Answer::Answered(Box::new(answered)),
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
            Answer::Answered(Box::new(heard.answered())),
        );
        machines.push(if unproven { seen.unproven() } else { seen });
    }
    Ok(Read {
        read_at_ms: now_ms,
        machines,
        fleet,
    })
}
