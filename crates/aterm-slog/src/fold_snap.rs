// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's snapshot (design §5.2, phase 2A): the [`Fold`] as text lines a
//! carried log crosses a seamless update in, and back.
//!
//! A carried log cannot rebuild its fold from the records it carries: the
//! carry sheds records, and refolding a subset changes totals, needs, claims
//! and the last closed nodes ("eviction never changes the state"). So the
//! fold crosses as it stands, field by field, each with the confidence and
//! seq that put it there: every record after it has a higher seq, so the
//! confidence law (§3.7) decides exactly as it would have.
//!
//! Lines (`fold` then a tag; values percent-encoded where they are text):
//!
//! ```text
//! fold v=1 as_of=<seq> ep=<e> rev=<n> last_t=<ms> mail_open=<n>
//! fold phase word=<w> src=<s> conf=<c> t=<ms>        (and `fold shell …`)
//! fold totals turns=<n> done=<n> … appr_by=<a,b,c,d> tools=<10 counts> …
//! fold open <node> <kind> [par=<node>] by=<a> src=<s> conf=<c> t=<ms>
//! fold last <slot> <node> <kind> out=<w> dur_ms=<n> [opened_t=<ms>] t=<ms> by=<a> opened_by=<a> src=<s> conf=<c>
//! fold closed <node> <kind> out=<w> …                (the session's own close)
//! fold f <owner> <key> <conf> <seq> <value>          (owner: t<i>, l<slot>, c)
//! fold need <word> <node> kind=<k> since=<seq> t=<ms> src=<s> conf=<c> by=<a> [detail=<pct>]
//! fold verdict word=<w> by=<a> t=<ms> seq=<seq> [as=<pct>] [re=<id>] [t…] [p…]
//! fold label text=<pct> by=<a> t=<ms> until=<ms> seq=<seq> [as=<pct>] [re=<id>] [t…] [p…]
//! ```
//!
//! A claim line holds the newest claim of any author; the newest of a
//! trusted author (phase 3A ruling R13) rides the same line as `t`-prefixed
//! tokens (`tword=`/`ttext=`, `tby=`, `tt=`, `tuntil=`, `tseq=`, `tas=`,
//! `tre=`) when it is another claim, so a reader that does not know them
//! skips them; none means the trusted one is the line's own claim when its
//! author is trusted, and none otherwise. The newest of a PROVEN author (the
//! harness's, `Actor::is_proven`) rides it the same way as `p`-prefixed
//! tokens when it is not the trusted one; none means it is the trusted one
//! when that author is proven, and none otherwise.
//!
//! Reading back is bounded and fail-closed: a line this build does not read,
//! a value of the wrong shape, more needs, open nodes or fields than a fold
//! holds, refuses the whole snapshot ([`SnapError`]).

use std::fmt::Write as _;

use aterm_codec::pct;

use super::{
    Claim, ClosedNode, Fold, Held, MAX_NEEDS, MAX_TRACKED, Need, NodeFields, OpenNode, Phase,
    SessionState, Totals,
};
use crate::field::{Key, MAX_FIELDS, Tier};
use crate::line::{decode_val, push_val};
use crate::vocab::{
    Actor, Conf, Kind, NEED_WORDS, Node, OUT_WORDS, PHASE_WORDS, ShortId, Src, UNKNOWN, WORD_WORDS,
    lookup, lookup_or_unknown,
};

/// Why a snapshot did not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapError {
    /// No `fold v=1` line first, or another version.
    Version,
    /// A line this build does not read, or a value of the wrong shape.
    Line,
    /// More needs, open nodes, closed slots or fields than a fold holds.
    Bound,
}

/// The `k=v` tokens after the first `skip` words of a line.
fn kv(line: &str, skip: usize) -> impl Iterator<Item = (&str, &str)> {
    line.split(' ')
        .filter(|t| !t.is_empty())
        .skip(skip)
        .filter_map(|t| t.split_once('='))
}

fn get<'a>(line: &'a str, skip: usize, key: &str) -> Option<&'a str> {
    kv(line, skip).find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn num<T: std::str::FromStr>(line: &str, skip: usize, key: &str) -> Result<T, SnapError> {
    get(line, skip, key)
        .and_then(|v| v.parse().ok())
        .ok_or(SnapError::Line)
}

fn opt_num<T: std::str::FromStr>(
    line: &str,
    skip: usize,
    key: &str,
) -> Result<Option<T>, SnapError> {
    match get(line, skip, key) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|_| SnapError::Line),
    }
}

fn word(list: &'static [&'static str], raw: Option<&str>) -> Result<&'static str, SnapError> {
    raw.map(|w| lookup_or_unknown(list, w))
        .ok_or(SnapError::Line)
}

/// A phase word: the agent's (`word=`), the shell's (`phase=`), or `-`.
fn phase_word(raw: &str) -> &'static str {
    lookup(WORD_WORDS, raw)
        .or_else(|| lookup(PHASE_WORDS, raw))
        .unwrap_or(UNKNOWN)
}

fn node_at(line: &str, i: usize) -> Result<Node, SnapError> {
    line.split(' ')
        .filter(|t| !t.is_empty())
        .nth(i)
        .and_then(Node::parse)
        .ok_or(SnapError::Line)
}

fn tok_at(line: &str, i: usize) -> Result<&str, SnapError> {
    line.split(' ')
        .filter(|t| !t.is_empty())
        .nth(i)
        .ok_or(SnapError::Line)
}

fn phase_line(tag: &str, p: &Phase) -> String {
    format!(
        "fold {tag} word={} src={} conf={} t={}",
        p.word,
        p.src.word(),
        p.conf.word(),
        p.t
    )
}

fn read_phase(line: &str) -> Result<Phase, SnapError> {
    Ok(Phase {
        word: phase_word(get(line, 2, "word").ok_or(SnapError::Line)?),
        src: Src::from_word(get(line, 2, "src").ok_or(SnapError::Line)?),
        conf: Conf::from_word(get(line, 2, "conf").ok_or(SnapError::Line)?),
        t: num(line, 2, "t")?,
    })
}

fn counts(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

fn read_counts<const N: usize>(raw: Option<&str>) -> Result<[u32; N], SnapError> {
    let raw = raw.ok_or(SnapError::Line)?;
    let mut out = [0u32; N];
    let mut it = raw.split(',');
    for slot in &mut out {
        *slot = it
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or(SnapError::Line)?;
    }
    if it.next().is_some() {
        return Err(SnapError::Line);
    }
    Ok(out)
}

fn closed_line(head: &str, c: &ClosedNode) -> String {
    let mut s = format!(
        "{head} {} {} out={} dur_ms={}",
        c.node,
        c.kind.word(),
        c.out,
        c.dur_ms
    );
    if let Some(t) = c.opened_t {
        let _ = write!(s, " opened_t={t}");
    }
    let _ = write!(
        s,
        " t={} by={} opened_by={} src={} conf={}",
        c.t,
        c.by.word(),
        c.opened_by.word(),
        c.src.word(),
        c.conf.word()
    );
    s
}

/// A closed node from its line (`skip` words before its node).
fn read_closed(line: &str, skip: usize) -> Result<ClosedNode, SnapError> {
    let at = skip + 2;
    Ok(ClosedNode {
        node: node_at(line, skip)?,
        kind: Kind::from_word(tok_at(line, skip + 1)?),
        out: word(OUT_WORDS, get(line, at, "out"))?,
        dur_ms: num(line, at, "dur_ms")?,
        opened_t: opt_num(line, at, "opened_t")?,
        t: num(line, at, "t")?,
        by: Actor::from_word(get(line, at, "by").ok_or(SnapError::Line)?),
        opened_by: Actor::from_word(get(line, at, "opened_by").ok_or(SnapError::Line)?),
        src: Src::from_word(get(line, at, "src").ok_or(SnapError::Line)?),
        conf: Conf::from_word(get(line, at, "conf").ok_or(SnapError::Line)?),
        f: NodeFields::default(),
    })
}

fn field_lines(out: &mut Vec<String>, owner: &str, f: &NodeFields) {
    for h in f.0.iter().filter(|h| h.key.tier() <= Tier::Host) {
        let mut s = format!(
            "fold f {owner} {} {} {} ",
            h.key.word(),
            h.conf.word(),
            h.seq
        );
        push_val(&mut s, &h.val);
        out.push(s);
    }
}

/// A claim's tokens after its tag, each key prefixed `p` (`` for the line's
/// own claim, `t` for the trusted one riding it).
fn push_claim(s: &mut String, p: &str, c: &Claim) {
    match (&c.word, &c.text) {
        (Some(w), _) => {
            let _ = write!(s, " {p}word={w}");
        }
        (None, text) => {
            let _ = write!(s, " {p}text=");
            pct::encode_into(s, text.as_deref().unwrap_or(""));
        }
    }
    let _ = write!(s, " {p}by={} {p}t={}", c.by.word(), c.t);
    if c.text.is_some() || c.until.is_some() {
        let _ = write!(s, " {p}until={}", c.until.unwrap_or(0));
    }
    let _ = write!(s, " {p}seq={}", c.seq);
    if let Some(a) = &c.as_name {
        let _ = write!(s, " {p}as=");
        pct::encode_into(s, a);
    }
    if let Some(re) = &c.re {
        let _ = write!(s, " {p}re={re}");
    }
}

/// `fold <tag> …`: the newest claim of any author, and the newest trusted
/// one when it is another.
fn claim_line(tag: &str, any: &Claim, trusted: Option<&Claim>, proven: Option<&Claim>) -> String {
    let mut s = format!("fold {tag}");
    push_claim(&mut s, "", any);
    if let Some(t) = trusted.filter(|t| t.seq != any.seq) {
        push_claim(&mut s, "t", t);
    }
    // What a reader derives with no `p…` tokens: the trusted claim when its
    // author is proven (`read_claim_line`).
    let derived = trusted.or(Some(any)).filter(|c| c.by.is_proven());
    if let Some(p) = proven.filter(|p| derived.is_none_or(|d| d.seq != p.seq)) {
        push_claim(&mut s, "p", p);
    }
    s
}

/// One claim of a claim line, its keys prefixed `p`.
fn read_claim(line: &str, p: &str, label: bool) -> Result<Claim, SnapError> {
    let key = |k: &str| format!("{p}{k}");
    let (word, text, until) = if label {
        let raw = pct::decode(get(line, 2, &key("text")).ok_or(SnapError::Line)?);
        let text = crate::field::BoundedText::new(Key::Text, &raw)
            .map(|t| t.as_str().to_string())
            .ok_or(SnapError::Line)?;
        (None, Some(text), Some(num(line, 2, &key("until"))?))
    } else {
        (
            Some(word(WORD_WORDS, get(line, 2, &key("word")))?),
            None,
            None,
        )
    };
    let as_name = match get(line, 2, &key("as")) {
        None => None,
        Some(raw) => Some(
            crate::field::BoundedText::new(Key::As, &pct::decode(raw))
                .map(|t| t.as_str().to_string())
                .ok_or(SnapError::Line)?,
        ),
    };
    let re = match get(line, 2, &key("re")) {
        None => None,
        Some(raw) => Some(ShortId::new(raw).ok_or(SnapError::Line)?),
    };
    Ok(Claim {
        word,
        text,
        by: Actor::from_word(get(line, 2, &key("by")).ok_or(SnapError::Line)?),
        t: num(line, 2, &key("t"))?,
        until,
        seq: num(line, 2, &key("seq"))?,
        as_name,
        re,
    })
}

/// A claim line's three claims: the newest of any author, the newest
/// trusted one (its own `t…` tokens; else the line's claim when its author
/// is trusted, as a 2A snapshot's single slot reads) and the newest proven
/// one (its own `p…` tokens; else the trusted one when its author is
/// proven).
type ClaimSlots = (Claim, Option<Claim>, Option<Claim>);

fn read_claim_line(line: &str, label: bool) -> Result<ClaimSlots, SnapError> {
    let any = read_claim(line, "", label)?;
    let (tkey, pkey) = if label {
        ("ttext", "ptext")
    } else {
        ("tword", "pword")
    };
    let trusted = if get(line, 2, tkey).is_some() {
        Some(read_claim(line, "t", label)?).filter(|c| c.by.is_trusted())
    } else {
        Some(any.clone()).filter(|c| c.by.is_trusted())
    };
    let proven = if get(line, 2, pkey).is_some() {
        Some(read_claim(line, "p", label)?).filter(|c| c.by.is_proven())
    } else {
        trusted.clone().filter(|c| c.by.is_proven())
    };
    Ok((any, trusted, proven))
}

impl Fold {
    /// The fold as `fold …` lines, deterministic (the snapshot this module
    /// documents).
    #[must_use]
    pub fn snapshot(&self) -> Vec<String> {
        let st = &self.st;
        let mut out = vec![format!(
            "fold v=1 as_of={} ep={} rev={} last_t={} mail_open={}",
            st.as_of, st.ep, st.rev, st.last_t, st.mail_open
        )];
        out.push(phase_line("phase", &st.phase));
        if let Some(p) = &self.shell_phase {
            out.push(phase_line("shell", p));
        }
        let t = &st.totals;
        out.push(format!(
            "fold totals turns={} done={} asked={} walled={} interrupted={} errors={} other={} \
             steps={} appr={} appr_by={} qs={} stalls={} tools={} mail_in={} mail_out={} \
             human={} lost={}",
            t.turns,
            t.done,
            t.asked,
            t.walled,
            t.interrupted,
            t.errors,
            t.other,
            t.steps,
            t.appr,
            counts(&t.appr_by),
            t.qs,
            t.stalls,
            counts(&t.tools),
            t.mail_in,
            t.mail_out,
            t.human,
            t.lost
        ));
        for (i, o) in self.tracked.iter().enumerate() {
            let mut s = format!("fold open {} {}", o.node, o.kind.word());
            if let Some(p) = o.par {
                let _ = write!(s, " par={p}");
            }
            let _ = write!(
                s,
                " by={} src={} conf={} t={}",
                o.by.word(),
                o.src.word(),
                o.conf.word(),
                o.t
            );
            out.push(s);
            field_lines(&mut out, &format!("t{i}"), &o.f);
        }
        for (slot, c) in st.last.iter().enumerate() {
            if let Some(c) = c {
                out.push(closed_line(&format!("fold last {slot}"), c));
                field_lines(&mut out, &format!("l{slot}"), &c.f);
            }
        }
        if let Some(c) = &st.closed {
            out.push(closed_line("fold closed", c));
            field_lines(&mut out, "c", &c.f);
        }
        for n in st.needs.iter().flatten() {
            let mut s = format!(
                "fold need {} {} kind={} since={} t={} src={} conf={} by={}",
                n.word,
                n.node,
                n.kind.word(),
                n.since_seq,
                n.t,
                n.src.word(),
                n.conf.word(),
                n.by.word()
            );
            if let Some(d) = &n.detail {
                s.push_str(" detail=");
                pct::encode_into(&mut s, d);
            }
            out.push(s);
        }
        if let Some(v) = &st.verdict {
            out.push(claim_line(
                "verdict",
                v,
                st.verdict_trusted.as_ref(),
                st.verdict_proven.as_ref(),
            ));
        }
        if let Some(l) = &st.label {
            out.push(claim_line(
                "label",
                l,
                st.label_trusted.as_ref(),
                st.label_proven.as_ref(),
            ));
        }
        out
    }

    /// The fold a [`Fold::snapshot`] wrote.
    ///
    /// # Errors
    ///
    /// A [`SnapError`] for anything this build does not read; the whole
    /// snapshot is refused, never a part.
    pub fn from_snapshot(lines: &[&str]) -> Result<Fold, SnapError> {
        let mut it = lines.iter();
        let head = it.next().ok_or(SnapError::Version)?;
        if !head.starts_with("fold v=1 ") {
            return Err(SnapError::Version);
        }
        let mut fold = Fold::default();
        let st: &mut SessionState = &mut fold.st;
        st.as_of = num(head, 2, "as_of")?;
        st.ep = num(head, 2, "ep")?;
        st.rev = num(head, 2, "rev")?;
        st.last_t = num(head, 2, "last_t")?;
        st.mail_open = num(head, 2, "mail_open")?;
        let mut needs: Vec<Need> = Vec::new();
        for line in it {
            let tag = tok_at(line, 1)?;
            match tag {
                "phase" => fold.st.phase = read_phase(line)?,
                "shell" => fold.shell_phase = Some(read_phase(line)?),
                "totals" => {
                    let g = |k: &str| -> Result<u32, SnapError> { num(line, 2, k) };
                    fold.st.totals = Totals {
                        turns: g("turns")?,
                        done: g("done")?,
                        asked: g("asked")?,
                        walled: g("walled")?,
                        interrupted: g("interrupted")?,
                        errors: g("errors")?,
                        other: g("other")?,
                        steps: g("steps")?,
                        appr: g("appr")?,
                        appr_by: read_counts::<4>(get(line, 2, "appr_by"))?,
                        qs: g("qs")?,
                        stalls: g("stalls")?,
                        tools: read_counts::<10>(get(line, 2, "tools"))?,
                        mail_in: g("mail_in")?,
                        mail_out: g("mail_out")?,
                        human: g("human")?,
                        lost: num(line, 2, "lost")?,
                    };
                }
                "open" => {
                    if fold.tracked.len() >= MAX_TRACKED {
                        return Err(SnapError::Bound);
                    }
                    fold.tracked.push(OpenNode {
                        node: node_at(line, 2)?,
                        kind: Kind::from_word(tok_at(line, 3)?),
                        par: match get(line, 4, "par") {
                            Some(p) => Some(Node::parse(p).ok_or(SnapError::Line)?),
                            None => None,
                        },
                        t: num(line, 4, "t")?,
                        by: Actor::from_word(get(line, 4, "by").ok_or(SnapError::Line)?),
                        src: Src::from_word(get(line, 4, "src").ok_or(SnapError::Line)?),
                        conf: Conf::from_word(get(line, 4, "conf").ok_or(SnapError::Line)?),
                        f: NodeFields::default(),
                    });
                }
                "last" => {
                    let slot: usize = tok_at(line, 2)?.parse().map_err(|_| SnapError::Line)?;
                    if slot >= fold.st.last.len() {
                        return Err(SnapError::Bound);
                    }
                    fold.st.last[slot] = Some(read_closed(line, 3)?);
                }
                "closed" => fold.st.closed = Some(read_closed(line, 2)?),
                "f" => {
                    let owner = tok_at(line, 2)?;
                    let key = Key::from_word(tok_at(line, 3)?).ok_or(SnapError::Line)?;
                    let conf = Conf::from_word(tok_at(line, 4)?);
                    let seq: u64 = tok_at(line, 5)?.parse().map_err(|_| SnapError::Line)?;
                    let raw = line
                        .split(' ')
                        .filter(|t| !t.is_empty())
                        .nth(6)
                        .ok_or(SnapError::Line)?;
                    let val = decode_val(key, raw).ok_or(SnapError::Line)?;
                    let target = match owner.split_at_checked(1) {
                        Some(("t", i)) => i
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| fold.tracked.get_mut(i))
                            .map(|o| &mut o.f),
                        Some(("l", i)) => i
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| fold.st.last.get_mut(i))
                            .and_then(Option::as_mut)
                            .map(|c| &mut c.f),
                        Some(("c", "")) => fold.st.closed.as_mut().map(|c| &mut c.f),
                        _ => None,
                    }
                    .ok_or(SnapError::Line)?;
                    if target.0.len() >= MAX_FIELDS * 2 || target.0.iter().any(|h| h.key == key) {
                        return Err(SnapError::Bound);
                    }
                    target.0.push(Held {
                        key,
                        val,
                        conf,
                        seq,
                    });
                }
                "need" => {
                    if needs.len() >= MAX_NEEDS {
                        return Err(SnapError::Bound);
                    }
                    let w = tok_at(line, 2)?;
                    needs.push(Need {
                        word: lookup(NEED_WORDS, w).ok_or(SnapError::Line)?,
                        node: node_at(line, 3)?,
                        kind: Kind::from_word(get(line, 4, "kind").ok_or(SnapError::Line)?),
                        since_seq: num(line, 4, "since")?,
                        t: num(line, 4, "t")?,
                        src: Src::from_word(get(line, 4, "src").ok_or(SnapError::Line)?),
                        conf: Conf::from_word(get(line, 4, "conf").ok_or(SnapError::Line)?),
                        by: Actor::from_word(get(line, 4, "by").ok_or(SnapError::Line)?),
                        detail: get(line, 4, "detail").map(pct::decode),
                    });
                }
                "verdict" => {
                    let (any, trusted, proven) = read_claim_line(line, false)?;
                    fold.st.verdict = Some(any);
                    fold.st.verdict_trusted = trusted;
                    fold.st.verdict_proven = proven;
                }
                "label" => {
                    let (any, trusted, proven) = read_claim_line(line, true)?;
                    fold.st.label = Some(any);
                    fold.st.label_trusted = trusted;
                    fold.st.label_proven = proven;
                }
                _ => return Err(SnapError::Line),
            }
        }
        for (slot, n) in fold.st.needs.iter_mut().zip(needs) {
            *slot = Some(n);
        }
        fold.reproject();
        Ok(fold)
    }
}
