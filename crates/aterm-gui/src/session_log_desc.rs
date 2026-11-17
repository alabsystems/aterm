// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `described` verb: a session's card. Its claims slot by slot (`aterm_slog`'s fold keeps
//! them and says which stands, [`aterm_slog::shown`]), each with its author, that author's rank
//! and the evidence class the rank gives it; a judge's `verdict word=done` beside the `done`
//! slot; its newest history lines ([`aterm_slog::shown_history`]); and eight lines aterm derives
//! of the session from records, never from a claim's text: what it is doing, what aterm observes
//! of it, aterm's judgement and the label drawn from it, what its records say of done, its birth
//! and its agent (the `born` line's `title_src` and `matters_src` read which claim stands and that
//! claim's record alone: its author's rank, `src`, `conf` and `as=`). Every claim and line is text
//! from a screen: the header says `trust=screen`. `described history` prints the history lines
//! alone.
//! A section cited as § is one of `docs/DESIGN-comprehension-2026-11-14.md`'s.

use std::fmt::Write as _;

use aterm_control::wire::{json_escape_into, json_ok, json_str_field};
use aterm_db::words::{Activity, TrackWhy};
use aterm_slog::{
    Actor, Claim, DESC_HISTORY, DescClaim, HistRow, KNOWN_SLOTS, Key, Kind, Node, Rank,
    SessionState,
};

use crate::comprehension_words::RowWords;

/// The `described` usage line.
pub(crate) const DESCRIBED_USAGE: &str = "ERR usage: described [history [n=<1-16>]] [--json]\n";

/// The history lines the card prints after its slots (§2.2: "Peek and `described` 8");
/// `described history` prints up to [`DESC_HISTORY`].
const DESCRIBED_HISTORY: usize = 8;

/// What a `described` request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// The whole card.
    Card,
    /// The newest history lines alone, at most this many (1 to [`DESC_HISTORY`]).
    History(usize),
}

/// The request `rest` makes (the `--json` flag already taken off it): nothing, the card; or
/// `history` with at most one `n=<1-16>`, [`DESC_HISTORY`] without one. Anything else is the usage
/// line: a count the fold cannot hold is refused, never answered short.
fn asked(rest: &str) -> Result<Asked, String> {
    let usage = || DESCRIBED_USAGE.to_string();
    let mut words = rest.split_ascii_whitespace();
    let asked = match words.next() {
        None => return Ok(Asked::Card),
        Some("history") => match words.next() {
            None => Asked::History(DESC_HISTORY),
            Some(count) => count
                .strip_prefix("n=")
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| (1..=DESC_HISTORY).contains(n))
                .map(Asked::History)
                .ok_or_else(usage)?,
        },
        Some(_) => return Err(usage()),
    };
    if words.next().is_some() {
        return Err(usage());
    }
    Ok(asked)
}

/// THE EVIDENCE CLASS of a line whose author has `rank` (§2.2 "Evidence classes"): what kind of
/// source produced it, read from the rank and never from the text. A person, the session's own
/// program, its creator, a control client, a program's printed title and an author who never
/// stands each stated it (`declared`); a writer aterm ran inferred it (`inferred`); aterm made it
/// itself without a model (`derived`). The class names who produced a line, never that its words
/// are true or aterm's: every line is still text from a screen.
const fn evidence(rank: Rank) -> &'static str {
    match rank {
        Rank::Person | Rank::Program | Rank::Creator | Rank::Ctl | Rank::Inband | Rank::Listed => {
            "declared"
        }
        Rank::Writer => "inferred",
        Rank::Derived => "derived",
    }
}

/// One claim as `described` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DescLine {
    /// What it describes: the session (`-`) or an agent run (`r<n>`).
    pub(crate) scope: Node,
    pub(crate) value: String,
    pub(crate) by: Actor,
    pub(crate) rank: Rank,
    /// The name its writer gave itself (`as=`): listed, never the author.
    pub(crate) as_name: Option<String>,
    /// The record it is about (`re=`).
    pub(crate) re: Option<String>,
    /// Turns closed on its subject since the seq it covers; `None` when nobody knows.
    pub(crate) behind: Option<u32>,
    pub(crate) seq: u64,
    /// Whether its `ttl` ran out before the read.
    pub(crate) lapsed: bool,
}

impl DescLine {
    fn of(scope: Node, claim: &DescClaim, rank: Rank, behind: Option<u32>, live: bool) -> DescLine {
        DescLine {
            scope,
            value: claim.value.clone(),
            by: claim.by,
            rank,
            as_name: claim.as_name.clone(),
            re: claim.re.map(|id| id.as_str().to_owned()),
            behind,
            seq: claim.seq,
            lapsed: !live,
        }
    }
}

/// A judge's `verdict word=done`, as `described` lists it under `done` (§2.5): its author stated
/// it (`ev=declared`), it never stands in the slot, and it has no rank. The fold keeps a verdict
/// for the session, so its scope is `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerdictLine {
    pub(crate) by: Actor,
    /// The name its author gave itself (`as=`).
    pub(crate) as_name: Option<String>,
    /// The record it is about (`re=`).
    pub(crate) re: Option<String>,
    pub(crate) seq: u64,
}

impl VerdictLine {
    fn of(claim: &Claim) -> VerdictLine {
        VerdictLine {
            by: claim.by,
            as_name: claim.as_name.clone(),
            re: claim.re.map(|id| id.as_str().to_owned()),
            seq: claim.seq,
        }
    }
}

/// One slot as `described` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DescSlotView {
    pub(crate) name: String,
    /// The claim a reader is shown, over the session and its open agent run.
    pub(crate) shown: Option<DescLine>,
    /// Every other claim of the slot in those two scopes: best rank first, then oldest.
    pub(crate) others: Vec<DescLine>,
    /// Claims the slot dropped for a ninth author, over both scopes.
    pub(crate) dropped: u32,
    /// For `done` only, each verdict a reader is shown whose word is `done`, oldest first.
    pub(crate) verdicts: Vec<VerdictLine>,
}

/// One history line as `described` prints it ([`HistRow`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct HistView {
    scope: Node,
    /// Its record's time, wall ms.
    t: u64,
    value: String,
    by: Actor,
    /// Its author's rank when it was admitted, never judged again.
    rank: Rank,
    as_name: Option<String>,
    /// The event it reports: only on a line aterm's own writer wrote
    /// ([`aterm_slog::HistLine::event`]).
    event: Option<&'static str>,
    re: Option<String>,
    seq: u64,
}

impl HistView {
    fn of(row: &HistRow<'_>) -> HistView {
        HistView {
            scope: row.scope,
            t: row.line.t,
            value: row.line.value.clone(),
            by: row.line.by,
            rank: row.rank,
            as_name: row.line.as_name.clone(),
            event: row.event,
            re: row.line.re.map(|id| id.as_str().to_owned()),
            seq: row.line.seq,
        }
    }
}

/// The track judgement (§5.6): the reason that stands, which names its track word
/// ([`TrackWhy::track`]). Nothing derives it yet: [`Self::UNREAD`] is every card's. It holds no
/// time: how long a judgement has stood is the platform's to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackRead {
    why: Option<TrackWhy>,
}

impl TrackRead {
    /// Unread: the `judged` line's `track=` and `track_why=` read `-`, and the activity read
    /// with no verdict.
    const UNREAD: TrackRead = TrackRead { why: None };
}

/// The observations the judgement reads, in the order the `observed` line and
/// `derived.observed` name them. Nothing observes them yet: each reads `-` in the lines and
/// `null` in the JSON, and so do the judgement's `used=` and the label.
const OBSERVED: [&str; 20] = [
    "tries",
    "try",
    "on_main",
    "running",
    "red",
    "reds",
    "conflicts",
    "landed",
    "receipt",
    "ahead",
    "files",
    "turns",
    "work_min",
    "errors",
    "task",
    "need",
    "limit",
    "says_done",
    "working",
    "ended",
];

/// The `agent` line: the open agent run and what this session's row says of its agent.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentView {
    /// The agent its run's open record names (`claude`, `codex`).
    kind: Option<String>,
    /// The newest agent run still open.
    run: Option<Node>,
    /// Turns the session opened, counted: 0 is a count.
    turns: u32,
    /// The standing need's word.
    need: Option<&'static str>,
    tok_in: Option<u64>,
    tok_out: Option<u64>,
    /// Seconds since the open agent run opened.
    wall_s: Option<u64>,
}

/// The eight lines aterm derives of the session, from records and never from a claim's text:
/// `title_src` and `matters_src` read only the standing claim's record ([`aterm_slog::title_src`],
/// [`aterm_slog::matters_src`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct DerivedView {
    /// The activity word (§5.6), `None` where it cannot be read.
    state: Option<Activity>,
    track: TrackRead,
    /// The task's conclusion word (`finished`, `failed`).
    conclusion: Option<&'static str>,
    /// HEAD's 12 digits where Land's receipt stands on main beside a landed face.
    landed: Option<String>,
    /// When the session's lineage first opened, Unix ms, and how long ago at the read.
    opened_ms: Option<u64>,
    age_ms: Option<u64>,
    /// Who created it: an id and its kind (`session`, `ctl`).
    creator: Option<(String, &'static str)>,
    /// Where the standing title and reason came from ([`aterm_slog::title_src`],
    /// [`aterm_slog::matters_src`]).
    title_src: Option<&'static str>,
    matters_src: &'static str,
    agent: AgentView,
}

/// What `described` prints: a plain snapshot, so a reply is a function of it alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DescribedView {
    /// The newest seq the read saw, and its epoch.
    pub(crate) as_of: u64,
    pub(crate) ep: u32,
    /// The log's base epoch: the cursor `log since=` pages from.
    base: u32,
    /// The card's revision ([`aterm_slog::Described::card_rev`]).
    rev: u64,
    /// The known slots, each always present, then every other slot that holds a claim, by name.
    pub(crate) slots: Vec<DescSlotView>,
    /// The newest history lines of the session and its open agent run, newest first.
    history: Vec<HistView>,
    /// The lines the two subjects' kept lists pushed out.
    older: u32,
    derived: DerivedView,
}

/// What the derived lines read beside the fold's state, copied under the timeline's lock.
#[derive(Clone, Debug)]
struct CardFacts {
    /// The words the session's survey row says of what it is doing ([`crate::survey::row_words`]).
    row: RowWords<'static>,
    /// Its task's `from=` names a control client.
    task_from_ctl: bool,
    /// HEAD's 12 digits of the landing that stands, on main.
    landed: Option<String>,
    /// The conversation's whole token counters ([`crate::survey::whole_tokens`]).
    tok_in: Option<u64>,
    tok_out: Option<u64>,
    /// The lineage's first open ([`crate::session_log::SessionLog::opened_ms`]).
    opened_ms: Option<u64>,
}

/// What `described` needs of a session, copied under the timeline's leaf lock: the state a
/// reader is shown, the log's base epoch, and what the derived lines read beside them.
#[derive(Clone, Debug)]
pub(crate) struct DescribedCopy {
    state: SessionState,
    base: u32,
    facts: CardFacts,
}

impl DescribedCopy {
    /// The copy of `tl`'s session. It reads the timeline alone: no metadata lock, no process
    /// table, no engine.
    pub(crate) fn copied_from(tl: &crate::session_timeline::SessionTimeline) -> DescribedCopy {
        use aterm_agent::harness::acts::{ReceiptLeg, TaskFace};
        let log = tl.slog().log();
        let agent = tl.agent();
        let whole = crate::survey::whole_tokens(
            agent.reader,
            agent.program_pgid,
            tl.claude_footer()
                .and_then(|f| Some((f.usage.as_ref()?, f.owner.as_ref()?))),
            tl.codex_survey(),
        );
        // What `status landed_head=` reads, beside a landed face only, and only a head the
        // receipt found on main: a pushed branch or a repository with no remote landed nothing
        // there.
        let landed = tl
            .land_receipt()
            .filter(|_| {
                tl.task_face()
                    .is_some_and(|(face, _)| face == TaskFace::Landed)
            })
            .filter(|receipt| receipt.leg == ReceiptLeg::OnMain)
            .map(|receipt| receipt.head.clone());
        DescribedCopy {
            state: log.reader_state().clone(),
            base: log.base_ep(),
            facts: CardFacts {
                row: crate::survey::row_words(tl),
                task_from_ctl: tl
                    .task()
                    .is_some_and(|task| task.from == crate::task_record::FROM_CTL),
                landed,
                tok_in: whole.as_ref().and_then(|w| w.tok_in),
                tok_out: whole.as_ref().and_then(|w| w.tok_out),
                opened_ms: tl.slog().opened_ms(),
            },
        }
    }

    /// The card at `now_wall`, its newest `history` lines (a `ttl` lapse is judged here, at the
    /// read).
    pub(crate) fn view(&self, now_wall: u64, history: usize) -> DescribedView {
        let st = &self.state;
        let desc = &st.desc;
        let scopes: Vec<Node> = std::iter::once(Node::SESSION)
            .chain(desc.open_agent())
            .collect();
        let rows: Vec<aterm_slog::DescRow<'_>> = desc
            .listing(now_wall)
            .into_iter()
            .filter(|r| scopes.contains(&r.scope))
            .collect();
        let mut unknown: Vec<&str> = rows
            .iter()
            .map(|r| r.slot)
            .filter(|s| !KNOWN_SLOTS.contains(s))
            .collect();
        unknown.sort_unstable();
        unknown.dedup();
        let mut done: Vec<VerdictLine> = aterm_slog::shown_verdicts(st)
            .into_iter()
            .filter(|claim| claim.word == Some("done"))
            .map(VerdictLine::of)
            .collect();
        done.sort_by_key(|v| v.seq);
        let slots = KNOWN_SLOTS
            .iter()
            .copied()
            .chain(unknown)
            .map(|name| {
                let shown = aterm_slog::shown(st, name, now_wall);
                let stands = |r: &aterm_slog::DescRow<'_>| {
                    shown.is_some_and(|s| s.scope == r.scope && s.claim.seq == r.claim.seq)
                };
                let mut others: Vec<DescLine> = rows
                    .iter()
                    .filter(|r| r.slot == name && !stands(r))
                    .map(|r| DescLine::of(r.scope, r.claim, r.rank, r.behind, r.live))
                    .collect();
                others.sort_by_key(|l| (l.rank, l.seq));
                let dropped = scopes
                    .iter()
                    .filter_map(|scope| desc.subject(*scope)?.slot(name))
                    .fold(0u32, |n, slot| n.saturating_add(slot.dropped));
                DescSlotView {
                    name: name.to_string(),
                    shown: shown.map(|s| DescLine::of(s.scope, s.claim, s.rank, s.behind, true)),
                    others,
                    dropped,
                    verdicts: if name == "done" {
                        std::mem::take(&mut done)
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect();
        let older = scopes
            .iter()
            .filter_map(|scope| desc.subject(*scope))
            .fold(0u32, |n, s| n.saturating_add(s.lines_older));
        DescribedView {
            as_of: st.as_of,
            ep: st.ep,
            base: self.base,
            rev: desc.card_rev(),
            slots,
            history: aterm_slog::shown_history(st, history)
                .iter()
                .map(HistView::of)
                .collect(),
            older,
            derived: self.derived(now_wall),
        }
    }

    /// The derived lines at `now_wall` (§2.2's Derived table).
    fn derived(&self, now_wall: u64) -> DerivedView {
        let st = &self.state;
        let facts = &self.facts;
        let track = TrackRead::UNREAD;
        // The registry's lifecycle word is not the timeline's: the log's own close, the record
        // a session's end leaves on this Mac, says the session ended.
        let row = RowWords {
            state: st.closed.is_some().then_some("exited"),
            ..facts.row
        };
        let open_agent = st.desc.open_agent();
        // An agent run opens under the shell's command run, so while it is open it is the
        // deepest run, and its open record names the agent.
        let agent_run = st
            .run()
            .filter(|run| run.kind == Kind::Agent && Some(run.node) == open_agent);
        let creator = st
            .desc
            .ident()
            .creator
            .map(|sid| (sid.as_str().to_owned(), "session"))
            .or_else(|| {
                facts
                    .task_from_ctl
                    .then(|| (crate::task_record::FROM_CTL.to_owned(), "ctl"))
            });
        DerivedView {
            state: crate::comprehension_words::activity_here(&row, track.why),
            track,
            conclusion: facts.row.task_conclusion,
            landed: facts.landed.clone(),
            opened_ms: facts.opened_ms,
            age_ms: facts.opened_ms.map(|at| now_wall.saturating_sub(at)),
            creator,
            title_src: aterm_slog::title_src(st, now_wall).map(aterm_slog::TitleSrc::word),
            matters_src: aterm_slog::matters_src(st, now_wall).word(),
            agent: AgentView {
                kind: agent_run
                    .and_then(|run| run.f.str(Key::Agent))
                    .map(str::to_owned),
                run: open_agent,
                turns: st.totals.turns,
                need: facts.row.needs.filter(|word| *word != "-"),
                tok_in: facts.tok_in,
                tok_out: facts.tok_out,
                wall_s: agent_run.map(|run| now_wall.saturating_sub(run.t) / 1000),
            },
        }
    }
}

impl DescribedView {
    /// The reply's lines: the header `OK <n> described v=1 sid=<sid> as_of=<seq> ep=<e>
    /// base=<b> chapter=- rev=<card rev> trust=screen`; then for each slot the claim that stands
    /// (`<slot> <scope> value=<pct> by=<actor> rank=<word> ev=<class> [as=<name>] [re=<id>]
    /// behind=<n|-> seq=<seq> [dropped=<n>]`, or `<slot> - value=-` with none) and one `<slot>
    /// <scope> other …` line for each other claim (`lapsed=1` on one whose `ttl` ran out), the
    /// `done` slot followed by a `done - other verdict=done by=<actor> ev=declared [as=<name>]
    /// [re=<id>] seq=<seq>` line for each verdict `done`; then the newest history lines, newest
    /// first (`history <scope> t=<ms> value=<pct> by=<actor> rank=<word> ev=<class> [as=<name>]
    /// [event=<word>] [re=<id>] seq=<seq>`); then the eight derived lines, `state`, `observed`,
    /// `observed conflict`, `judged`, `label`, `is`, `born` and `agent`. `-` is unread.
    pub(crate) fn lines(&self, sid: &str) -> String {
        #[cfg(test)]
        super::encode_probe();
        let mut body = String::new();
        for slot in &self.slots {
            match &slot.shown {
                Some(line) => {
                    let _ = write!(body, "{} {} {}", slot.name, line.scope, claim_tail(line));
                    if slot.dropped > 0 {
                        let _ = write!(body, " dropped={}", slot.dropped);
                    }
                    body.push('\n');
                }
                None => {
                    let _ = writeln!(body, "{} - value=-", slot.name);
                }
            }
            for line in &slot.others {
                let _ = write!(
                    body,
                    "{} {} other {}",
                    slot.name,
                    line.scope,
                    claim_tail(line)
                );
                if line.lapsed {
                    body.push_str(" lapsed=1");
                }
                body.push('\n');
            }
            for verdict in &slot.verdicts {
                let _ = writeln!(
                    body,
                    "{} {} other {}",
                    slot.name,
                    Node::SESSION,
                    verdict_tail(verdict)
                );
            }
        }
        for line in &self.history {
            let _ = writeln!(body, "history {} {}", line.scope, history_tail(line));
        }
        self.derived.write_lines(&mut body);
        format!(
            "OK {} described v=1 sid={sid} {} trust=screen\n{body}",
            body.lines().count(),
            self.head()
        )
    }

    /// The reply as one JSON object: `{"v":1,"sid","as_of","ep","base","chapter":null,"rev",
    /// "slots":{"<slot>":…},"history":[…],"derived":{…}}`, [`Self::lines`]'s read. THE NULL RULE:
    /// every member is always there; one the lines print `-` is `null`, never 0, and a number the
    /// lines print is a JSON number, a counted 0 included. A slot that holds no claim and no
    /// verdict is `null`; one that does is an object of `value`, `scope`, `by`, `rank`, `ev`, `as`,
    /// `re`, `behind`, `seq` (each `null` where no claim stands), `dropped` and `others`, the
    /// other claims with the same members (and `"lapsed":true` on one whose `ttl` ran out), then
    /// each verdict as `{"verdict":"done", …}` with `value`, `rank` and `behind` `null`. Each
    /// history line is `t`, `value`, `scope`, `by`, `rank`, `ev`, `as`, `event`, `re`, `seq`.
    /// `derived` holds `state` (`word`), `observed` (one member per key of the `observed` line,
    /// [`OBSERVED`]; the conflict paths line has none), `judged` (`track`, `why`, `used`), `label`
    /// (`word`, `line`), `is` (`conclusion`, `landed`, `away`), `opened_ms`, `age_ms`,
    /// `title_src`, `matters_src`, `creator` (`id`, `kind`, `via`) and `agent` (`kind`, `run`,
    /// `turns`, `need`, `tok_in`, `tok_out`, `wall_s`). No member holds a time a judgement or a
    /// word has stood. Strings are the text itself, not percent-encoded.
    pub(crate) fn json(&self, sid: &str) -> String {
        #[cfg(test)]
        super::encode_probe();
        let slots: Vec<String> = self
            .slots
            .iter()
            .map(|slot| {
                let mut out = String::from("\"");
                json_escape_into(&mut out, &slot.name);
                out.push_str("\":");
                if slot.shown.is_none() && slot.others.is_empty() && slot.verdicts.is_empty() {
                    out.push_str("null");
                    return out;
                }
                let others: Vec<String> = slot
                    .others
                    .iter()
                    .map(|line| {
                        let lapsed = if line.lapsed { ",\"lapsed\":true" } else { "" };
                        format!("{{{}{lapsed}}}", claim_json(Some(line)))
                    })
                    .chain(slot.verdicts.iter().map(verdict_json))
                    .collect();
                let _ = write!(
                    out,
                    "{{{},\"dropped\":{},\"others\":[{}]}}",
                    claim_json(slot.shown.as_ref()),
                    slot.dropped,
                    others.join(",")
                );
                out
            })
            .collect();
        json_ok(&format!(
            "{{\"v\":1,{},{},\"slots\":{{{}}},\"history\":[{}],\"derived\":{}}}",
            json_str_field("sid", sid),
            self.head_json(),
            slots.join(","),
            self.history_json_entries(),
            self.derived.json()
        ))
    }

    /// `described history`'s lines: `OK <k> described history v=1 sid=<sid> as_of=<seq> ep=<e>
    /// base=<b> chapter=- rev=<card rev> older=<m> trust=screen`, then `k` history lines as
    /// [`Self::lines`] prints them, newest first; `older=` counts the lines the two subjects' kept
    /// lists pushed out.
    pub(crate) fn history_lines(&self, sid: &str) -> String {
        #[cfg(test)]
        super::encode_probe();
        let mut body = String::new();
        for line in &self.history {
            let _ = writeln!(body, "history {} {}", line.scope, history_tail(line));
        }
        format!(
            "OK {} described history v=1 sid={sid} {} older={} trust=screen\n{body}",
            self.history.len(),
            self.head(),
            self.older
        )
    }

    /// `described history --json`: `{"v":1,"sid","as_of","ep","base","chapter":null,"rev",
    /// "older","history":[…]}`, the entries as [`Self::json`]'s.
    pub(crate) fn history_json(&self, sid: &str) -> String {
        #[cfg(test)]
        super::encode_probe();
        json_ok(&format!(
            "{{\"v\":1,{},{},\"older\":{},\"history\":[{}]}}",
            json_str_field("sid", sid),
            self.head_json(),
            self.older,
            self.history_json_entries()
        ))
    }

    /// The header's position keys: `as_of=<seq> ep=<e> base=<b> chapter=- rev=<card rev>`. No
    /// chapter is kept.
    fn head(&self) -> String {
        format!(
            "as_of={} ep={} base={} chapter=- rev={}",
            self.as_of, self.ep, self.base, self.rev
        )
    }

    /// [`Self::head`] as JSON members.
    fn head_json(&self) -> String {
        format!(
            "\"as_of\":{},\"ep\":{},\"base\":{},\"chapter\":null,\"rev\":{}",
            self.as_of, self.ep, self.base, self.rev
        )
    }

    /// The history lines as JSON objects, comma-joined.
    fn history_json_entries(&self) -> String {
        self.history
            .iter()
            .map(history_json)
            .collect::<Vec<String>>()
            .join(",")
    }
}

impl DerivedView {
    /// The eight lines, each ending in a newline: `state word=`, the two `observed` lines (each
    /// of [`OBSERVED`], then the newest conflict's paths), `judged track= track_why= used=`,
    /// `label word= line=`, `is`, `born` and `agent`.
    fn write_lines(&self, out: &mut String) {
        let (by, kind) = self
            .creator
            .as_ref()
            .map_or(("-".to_string(), "-"), |(id, kind)| {
                (crate::control::meta_value(Some(id)), *kind)
            });
        let agent = &self.agent;
        let _ = writeln!(out, "state word={}", self.state.map_or("-", Activity::word));
        out.push_str("observed");
        for name in OBSERVED {
            let _ = write!(out, " {name}=-");
        }
        out.push_str("\nobserved conflict paths=-\n");
        let _ = writeln!(
            out,
            "judged track={} track_why={} used=-",
            self.track.why.map_or("-", |why| why.track().word()),
            self.track.why.map_or("-", TrackWhy::word)
        );
        out.push_str("label word=- line=-\n");
        let _ = writeln!(
            out,
            "is conclusion={} landed={} away=-",
            self.conclusion.unwrap_or("-"),
            crate::control::meta_value(self.landed.as_deref())
        );
        let _ = writeln!(
            out,
            "born opened_ms={} age_ms={} by={by} kind={kind} via=- title_src={} matters_src={}",
            dash(self.opened_ms),
            dash(self.age_ms),
            self.title_src.unwrap_or("-"),
            self.matters_src
        );
        let _ = writeln!(
            out,
            "agent kind={} run={} turns={} need={} tok_in={} tok_out={} wall_s={}",
            crate::control::meta_value(agent.kind.as_deref()),
            dash(agent.run),
            agent.turns,
            agent.need.unwrap_or("-"),
            dash(agent.tok_in),
            dash(agent.tok_out),
            dash(agent.wall_s)
        );
    }

    /// The `derived` object.
    fn json(&self) -> String {
        let (id, kind) = self
            .creator
            .as_ref()
            .map_or((None, None), |(id, kind)| (Some(id.as_str()), Some(*kind)));
        let agent = &self.agent;
        let run = agent.run.map(|node| node.to_string());
        let observed: Vec<String> = OBSERVED
            .iter()
            .map(|name| format!("\"{name}\":null"))
            .collect();
        format!(
            "{{\"state\":{{{}}},\"observed\":{{{}}},\"judged\":{{{},{},\"used\":null}},\
             \"label\":{{\"word\":null,\"line\":null}},\
             \"is\":{{{},{},\"away\":null}},\"opened_ms\":{},\"age_ms\":{},{},{},\
             \"creator\":{{{},{},\"via\":null}},\"agent\":{{{},{},\"turns\":{},{},\
             \"tok_in\":{},\"tok_out\":{},\"wall_s\":{}}}}}",
            str_or_null("word", self.state.map(Activity::word)),
            observed.join(","),
            str_or_null("track", self.track.why.map(|why| why.track().word())),
            str_or_null("why", self.track.why.map(TrackWhy::word)),
            str_or_null("conclusion", self.conclusion),
            str_or_null("landed", self.landed.as_deref()),
            null(self.opened_ms),
            null(self.age_ms),
            str_or_null("title_src", self.title_src),
            json_str_field("matters_src", self.matters_src),
            str_or_null("id", id),
            str_or_null("kind", kind),
            str_or_null("kind", agent.kind.as_deref()),
            str_or_null("run", run.as_deref()),
            agent.turns,
            str_or_null("need", agent.need),
            null(agent.tok_in),
            null(agent.tok_out),
            null(agent.wall_s)
        )
    }
}

/// `v` as the lines print it, `-` for none.
fn dash(v: Option<impl std::fmt::Display>) -> String {
    v.map_or_else(|| "-".to_string(), |v| v.to_string())
}

/// `v` as a JSON number, `null` for none.
fn null(v: Option<u64>) -> String {
    v.map_or_else(|| "null".to_string(), |v| v.to_string())
}

/// `"key":"<v>"`, or `"key":null` for none.
fn str_or_null(key: &str, v: Option<&str>) -> String {
    v.map_or_else(|| format!("\"{key}\":null"), |v| json_str_field(key, v))
}

/// The author `described` names: the actor's wire word, but `inband` for a title a program
/// printed, whose record names no actor (`by=-` is a rename a control client's keys touched).
fn author_word(by: &Actor, rank: Rank) -> &str {
    if rank == Rank::Inband {
        Rank::Inband.word()
    } else {
        by.word()
    }
}

/// `value=<pct> by=<actor> rank=<word> ev=<class> [as=<name>]`: the tokens a claim and a history
/// line both open with. A value that is itself `-` prints `%2D`, so it never reads as unset.
fn authored(value: &str, by: &Actor, rank: Rank, as_name: Option<&str>) -> String {
    let mut out = format!(
        "value={} by={} rank={} ev={}",
        crate::control::meta_value(Some(value)),
        author_word(by, rank),
        rank.word(),
        evidence(rank)
    );
    if let Some(name) = as_name {
        let _ = write!(out, " as={}", crate::control::meta_value(Some(name)));
    }
    out
}

/// ` re=<id>`, where `re` names a record; else nothing.
fn re_token(re: Option<&str>) -> String {
    re.map_or_else(String::new, |re| {
        format!(" re={}", crate::control::meta_value(Some(re)))
    })
}

/// A claim's tokens after its slot and scope: [`authored`]'s, then `[re=<id>] behind=<n|->
/// seq=<seq>`.
fn claim_tail(line: &DescLine) -> String {
    format!(
        "{}{} behind={} seq={}",
        authored(&line.value, &line.by, line.rank, line.as_name.as_deref()),
        re_token(line.re.as_deref()),
        dash(line.behind),
        line.seq
    )
}

/// A verdict's tokens after its slot, scope and `other`: `verdict=done by=<actor> ev=declared
/// [as=<name>] [re=<id>] seq=<seq>`.
fn verdict_tail(verdict: &VerdictLine) -> String {
    let mut out = format!("verdict=done by={} ev=declared", verdict.by.word());
    if let Some(name) = &verdict.as_name {
        let _ = write!(out, " as={}", crate::control::meta_value(Some(name)));
    }
    let _ = write!(
        out,
        "{} seq={}",
        re_token(verdict.re.as_deref()),
        verdict.seq
    );
    out
}

/// A history line's tokens after `history <scope>`: `t=<ms>`, [`authored`]'s, then
/// `[event=<word>] [re=<id>] seq=<seq>`. The event word is its own key, read from aterm's own
/// line alone; `as=` prints on every line that names one.
fn history_tail(line: &HistView) -> String {
    let mut out = format!(
        "t={} {}",
        line.t,
        authored(&line.value, &line.by, line.rank, line.as_name.as_deref())
    );
    if let Some(event) = line.event {
        let _ = write!(out, " event={event}");
    }
    let _ = write!(out, "{} seq={}", re_token(line.re.as_deref()), line.seq);
    out
}

/// A claim's JSON members, without braces: `value`, `scope`, `by`, `rank`, `ev`, `as`, `re`,
/// `behind`, `seq`. With no claim every one is `null`.
fn claim_json(line: Option<&DescLine>) -> String {
    let Some(line) = line else {
        return "\"value\":null,\"scope\":null,\"by\":null,\"rank\":null,\"ev\":null,\"as\":null,\
                \"re\":null,\"behind\":null,\"seq\":null"
            .to_string();
    };
    format!(
        "{},{},{},{},{},{},{},\"behind\":{},\"seq\":{}",
        json_str_field("value", &line.value),
        json_str_field("scope", &line.scope.to_string()),
        json_str_field("by", author_word(&line.by, line.rank)),
        json_str_field("rank", line.rank.word()),
        json_str_field("ev", evidence(line.rank)),
        str_or_null("as", line.as_name.as_deref()),
        str_or_null("re", line.re.as_deref()),
        null(line.behind.map(u64::from)),
        line.seq
    )
}

/// A verdict as one of its slot's `others`: `verdict` `"done"`, and `value`, `rank` and `behind`
/// `null`, since a verdict holds no text and no rank.
fn verdict_json(verdict: &VerdictLine) -> String {
    format!(
        "{{\"verdict\":\"done\",\"value\":null,{},{},\"rank\":null,\"ev\":\"declared\",{},{},\
         \"behind\":null,\"seq\":{}}}",
        json_str_field("scope", &Node::SESSION.to_string()),
        json_str_field("by", verdict.by.word()),
        str_or_null("as", verdict.as_name.as_deref()),
        str_or_null("re", verdict.re.as_deref()),
        verdict.seq
    )
}

/// One history line as a JSON object.
fn history_json(line: &HistView) -> String {
    format!(
        "{{\"t\":{},{},{},{},{},{},{},{},{},\"seq\":{}}}",
        line.t,
        json_str_field("value", &line.value),
        json_str_field("scope", &line.scope.to_string()),
        json_str_field("by", author_word(&line.by, line.rank)),
        json_str_field("rank", line.rank.word()),
        json_str_field("ev", evidence(line.rank)),
        str_or_null("as", line.as_name.as_deref()),
        str_or_null("event", line.event),
        str_or_null("re", line.re.as_deref()),
        line.seq
    )
}

/// What `rest` asks of `ctx`'s session, and the view of it: its copy taken under the timeline's
/// lock, the card worked out after it.
fn view_of(ctx: &crate::SessionCtx, rest: &str) -> Result<(Asked, DescribedView), String> {
    let asked = asked(rest)?;
    let (copy, now) = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (DescribedCopy::copied_from(&tl), tl.slog().now().wall_ms)
    };
    let history = match asked {
        Asked::Card => DESCRIBED_HISTORY,
        Asked::History(n) => n,
    };
    Ok((asked, copy.view(now, history)))
}

/// `described [history [n=<1-16>]]`: the session's card ([`DescribedView::lines`]), or its
/// newest history lines alone ([`DescribedView::history_lines`]). A pure reader of the session's
/// timeline lock, like `log`: the reply is formatted with no lock held.
pub(crate) fn cmd_described(ctx: &crate::SessionCtx, rest: &str) -> String {
    match view_of(ctx, rest) {
        Ok((Asked::Card, view)) => view.lines(ctx.self_id.as_str()),
        Ok((Asked::History(_), view)) => view.history_lines(ctx.self_id.as_str()),
        Err(usage) => usage,
    }
}

/// `described --json` (the dispatcher's `--json` route, the flag already taken off `rest`):
/// [`cmd_described`] as one JSON object ([`DescribedView::json`],
/// [`DescribedView::history_json`]).
pub(crate) fn cmd_described_json(ctx: &crate::SessionCtx, rest: &str) -> String {
    match view_of(ctx, rest) {
        Ok((Asked::Card, view)) => view.json(ctx.self_id.as_str()),
        Ok((Asked::History(_), view)) => view.history_json(ctx.self_id.as_str()),
        Err(usage) => usage,
    }
}
