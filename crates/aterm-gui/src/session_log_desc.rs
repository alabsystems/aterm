// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `described` verb: a session's description claims, slot by slot. `aterm_slog`'s fold
//! keeps the claims and says which stands ([`aterm_slog::shown`]); this reads a copy of its
//! state and prints, for each slot, the claim a reader is shown and every other author's
//! claim beside it, each with its author and that author's rank. Every value is text somebody
//! wrote about the session: the header says `trust=screen`.

use std::fmt::Write as _;

use aterm_control::wire::{json_escape_into, json_ok, json_str_field};
use aterm_slog::{Actor, DescClaim, KNOWN_SLOTS, Log, Node, Rank, SessionState};

/// The `described` usage line.
pub(crate) const DESCRIBED_USAGE: &str = "ERR usage: described [--json]\n";

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
            behind,
            seq: claim.seq,
            lapsed: !live,
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
}

/// What `described` prints: a plain snapshot, so a reply is a function of it alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DescribedView {
    /// The newest seq the read saw, and its epoch.
    pub(crate) as_of: u64,
    pub(crate) ep: u32,
    /// The known slots, each always present, then every other slot that holds a claim, by name.
    pub(crate) slots: Vec<DescSlotView>,
}

/// What `described` needs of a log, copied under the timeline's leaf lock: the state a reader
/// is shown.
#[derive(Clone, Debug)]
pub(crate) struct DescribedCopy {
    state: SessionState,
}

impl DescribedCopy {
    /// The reader's state of `log`.
    pub(crate) fn copied_from(log: &Log) -> DescribedCopy {
        DescribedCopy {
            state: log.reader_state().clone(),
        }
    }

    /// The slots at `now_wall`: a `ttl` lapse is judged here, at the read.
    pub(crate) fn view(&self, now_wall: u64) -> DescribedView {
        let desc = &self.state.desc;
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
        let slots = KNOWN_SLOTS
            .iter()
            .copied()
            .chain(unknown)
            .map(|name| {
                let shown = aterm_slog::shown(&self.state, name, now_wall);
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
                }
            })
            .collect();
        DescribedView {
            as_of: self.state.as_of,
            ep: self.state.ep,
            slots,
        }
    }
}

impl DescribedView {
    /// The reply's lines: `OK <n> described v=1 sid=<sid> as_of=<seq> ep=<e> trust=screen`,
    /// then for each slot the claim that stands (`<slot> <scope> value=<pct> by=<actor>
    /// rank=<word> [as=<name>] behind=<n|-> seq=<seq> [dropped=<n>]`, or `<slot> - value=-`
    /// with none) and one `<slot> <scope> other …` line for each other claim (`lapsed=1` on
    /// one whose `ttl` ran out).
    pub(crate) fn lines(&self, sid: &str) -> String {
        #[cfg(test)]
        super::encode_probe();
        let mut body = String::new();
        let mut n = 0usize;
        for slot in &self.slots {
            match &slot.shown {
                Some(line) => {
                    let _ = write!(body, "{} {} {}", slot.name, line.scope, line_tail(line));
                    if slot.dropped > 0 {
                        let _ = write!(body, " dropped={}", slot.dropped);
                    }
                    body.push('\n');
                }
                None => {
                    let _ = writeln!(body, "{} - value=-", slot.name);
                }
            }
            n += 1;
            for line in &slot.others {
                let _ = write!(
                    body,
                    "{} {} other {}",
                    slot.name,
                    line.scope,
                    line_tail(line)
                );
                if line.lapsed {
                    body.push_str(" lapsed=1");
                }
                body.push('\n');
                n += 1;
            }
        }
        format!(
            "OK {n} described v=1 sid={sid} as_of={} ep={} trust=screen\n{body}",
            self.as_of, self.ep
        )
    }

    /// The reply as one JSON object: `{"v":1,"sid":…,"as_of":…,"ep":…,"slots":{"<slot>":…}}`.
    /// A slot nobody claimed is `null`. One that holds a claim is an object of `value`,
    /// `scope`, `by`, `rank`, `as`, `behind`, `seq` (each `null` where no claim stands; `as`
    /// and `behind` also where the claim names none or nobody knows, never 0), `dropped` and
    /// `others`, the other claims with the same members (and `"lapsed":true` on one whose
    /// `ttl` ran out).
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
                if slot.shown.is_none() && slot.others.is_empty() {
                    out.push_str("null");
                    return out;
                }
                let others: Vec<String> = slot
                    .others
                    .iter()
                    .map(|line| {
                        let lapsed = if line.lapsed { ",\"lapsed\":true" } else { "" };
                        format!("{{{}{lapsed}}}", json_members(Some(line)))
                    })
                    .collect();
                let _ = write!(
                    out,
                    "{{{},\"dropped\":{},\"others\":[{}]}}",
                    json_members(slot.shown.as_ref()),
                    slot.dropped,
                    others.join(",")
                );
                out
            })
            .collect();
        json_ok(&format!(
            "{{\"v\":1,{},\"as_of\":{},\"ep\":{},\"slots\":{{{}}}}}",
            json_str_field("sid", sid),
            self.as_of,
            self.ep,
            slots.join(",")
        ))
    }
}

/// A claim's tokens after its slot and scope: `value=<pct> by=<actor> rank=<word> [as=<name>]
/// behind=<n|-> seq=<seq>`. A value that is itself `-` prints `%2D`, so it never reads as
/// unset.
fn line_tail(line: &DescLine) -> String {
    let mut out = format!(
        "value={} by={} rank={}",
        crate::control::meta_value(Some(&line.value)),
        line.by.word(),
        line.rank.word()
    );
    if let Some(name) = &line.as_name {
        let _ = write!(out, " as={}", crate::control::meta_value(Some(name)));
    }
    match line.behind {
        Some(n) => {
            let _ = write!(out, " behind={n}");
        }
        None => out.push_str(" behind=-"),
    }
    let _ = write!(out, " seq={}", line.seq);
    out
}

/// A claim's JSON members, without braces: `value`, `scope`, `by`, `rank`, `as`, `behind`,
/// `seq`. With no claim every one is `null`.
fn json_members(line: Option<&DescLine>) -> String {
    let Some(line) = line else {
        return "\"value\":null,\"scope\":null,\"by\":null,\"rank\":null,\"as\":null,\
                \"behind\":null,\"seq\":null"
            .to_string();
    };
    let name = line
        .as_name
        .as_deref()
        .map_or_else(|| "\"as\":null".to_string(), |a| json_str_field("as", a));
    let behind = line
        .behind
        .map_or_else(|| "null".to_string(), |n| n.to_string());
    format!(
        "{},{},{},{},{name},\"behind\":{behind},\"seq\":{}",
        json_str_field("value", &line.value),
        json_str_field("scope", &line.scope.to_string()),
        json_str_field("by", line.by.word()),
        json_str_field("rank", line.rank.word()),
        line.seq
    )
}

/// The view of `ctx`'s session: its state copied under the timeline's lock, the slots worked
/// out after it.
fn view_of(ctx: &crate::SessionCtx, rest: &str) -> Result<DescribedView, String> {
    if !rest.trim().is_empty() {
        return Err(DESCRIBED_USAGE.to_string());
    }
    let (copy, now) = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (
            DescribedCopy::copied_from(tl.slog().log()),
            tl.slog().now().wall_ms,
        )
    };
    Ok(copy.view(now))
}

/// `described`: each description slot's standing claim, its author, rank and the rest
/// ([`DescribedView::lines`]). A pure reader of the session's timeline lock, like `log`: the
/// reply is formatted with no lock held.
pub(crate) fn cmd_described(ctx: &crate::SessionCtx, rest: &str) -> String {
    match view_of(ctx, rest) {
        Ok(view) => view.lines(ctx.self_id.as_str()),
        Err(usage) => usage,
    }
}

/// `described --json` (the dispatcher's `--json` route, the flag already taken off `rest`):
/// [`cmd_described`] as one JSON object ([`DescribedView::json`]).
pub(crate) fn cmd_described_json(ctx: &crate::SessionCtx, rest: &str) -> String {
    match view_of(ctx, rest) {
        Ok(view) => view.json(ctx.self_id.as_str()),
        Err(usage) => usage,
    }
}
