// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sys left`: the leftover view ([`crate::left::LeftView`]) as text lines and as JSON, scoped
//! to the target's pane or tab, or with `scope=aterm` every session's
//! (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.1).

use super::json::Jv;
use super::query::{ScopeArg, SysQuery};
use super::view::{Line, scope_keys};
use super::{Ctx, clock_ms, pct};
use crate::left::{ENTRY_AFTER_MS, LeftRow, LeftView, LinkRow, RunRow, Source, State};
use crate::model::Snapshot;

/// `status`'s `left=` field for the session `sid` from `view`, `age_ms` being the view's age: how
/// many of its agent's left rows the view names ([`crate::left::agents_left_count`]) and the
/// oldest's age to now, ms (`-` where no row's age was read); `left=0:-` with none, `left=-` with
/// no view, no session id or a partial view, which can say a row is left but never that none is.
#[must_use]
pub fn status_field(view: Option<&LeftView>, sid: Option<&str>, age_ms: u64) -> String {
    let counted = view
        .zip(sid)
        .and_then(|(view, sid)| crate::left::agents_left_count(view, sid));
    match counted {
        None => "left=-".to_owned(),
        Some((0, _)) => "left=0:-".to_owned(),
        Some((n, oldest)) => format!(
            "left={n}:{}",
            oldest.map_or_else(
                || "-".to_owned(),
                |ms| ms.saturating_add(age_ms).to_string()
            )
        ),
    }
}

/// The rows and runs a reply shows, by its scope.
struct Shown<'a> {
    rows: Vec<&'a LeftRow>,
    runs: Vec<&'a RunRow>,
    links: &'a [LinkRow],
}

/// The sessions' ids of `keys`, as the host names them.
fn sids_of<'a>(ctx: &'a Ctx<'_>, keys: &[u64]) -> Vec<&'a str> {
    ctx.tabs
        .iter()
        .filter(|t| keys.contains(&t.key) && !t.sid.is_empty())
        .map(|t| t.sid.as_str())
        .chain(
            (keys.contains(&ctx.target.key) && !ctx.target.sid.is_empty())
                .then_some(ctx.target.sid.as_str()),
        )
        .collect()
}

/// What `scope` shows of `view`: with `aterm` every row, run and link; with a pane or a tab the
/// rows whose source is one of its sessions (W-session's tie, a held row's job or session-mate,
/// a waiting row's job) or a run that rode in one, and those runs. A closed tab's rows are no
/// live session's, so only `aterm` shows them.
fn shown<'a>(ctx: &Ctx<'_>, view: &'a LeftView, scope: ScopeArg) -> Shown<'a> {
    if scope == ScopeArg::Aterm {
        return Shown {
            rows: view.rows.iter().collect(),
            runs: view.runs.iter().collect(),
            links: &view.links,
        };
    }
    let keys = scope_keys(ctx, scope);
    let sids = sids_of(ctx, &keys);
    let run_ours = |id: &str| {
        view.runs
            .iter()
            .any(|r| r.id == id && r.sid.as_deref().is_some_and(|s| sids.contains(&s)))
    };
    Shown {
        rows: view
            .rows
            .iter()
            .filter(|r| match &r.from {
                Source::Tab(k) => keys.contains(k),
                Source::Run(id) => run_ours(id),
                Source::Closed(_) | Source::Unknown => false,
            })
            .collect(),
        runs: view
            .runs
            .iter()
            .filter(|r| r.sid.as_deref().is_some_and(|s| sids.contains(&s)))
            .collect(),
        links: &[],
    }
}

/// The session key a run rode in, where this instance hosts it.
fn run_key(ctx: &Ctx<'_>, run: &RunRow) -> Option<u64> {
    let sid = run.sid.as_deref()?;
    ctx.tabs.iter().find(|t| t.sid == sid).map(|t| t.key)
}

/// `from=`'s word: `tab:<n>`, `closed:tab:<n>`, `run:<id>`, or `-`.
fn from_word(ctx: &Ctx<'_>, view: &LeftView, from: &Source) -> String {
    let n = |n: Option<usize>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
    match from {
        Source::Tab(k) => format!("tab:{}", n(ctx.tab_number(*k))),
        Source::Closed(k) => format!(
            "closed:tab:{}",
            n(view
                .closed
                .iter()
                .find(|t| t.key == *k)
                .and_then(|t| t.number))
        ),
        Source::Run(id) => format!("run:{}", pct(id)),
        Source::Unknown => "-".to_owned(),
    }
}

/// `proof=`'s words, comma-joined, or `-`.
fn proof_words(row: &LeftRow) -> String {
    if row.proof.is_empty() {
        "-".to_owned()
    } else {
        row.proof
            .iter()
            .map(|p| p.word())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// How many held rows `rows` show, and the longest held, ms.
fn held_count(rows: &[&LeftRow]) -> (usize, Option<u64>) {
    let held: Vec<u64> = rows
        .iter()
        .filter(|r| r.state == State::ProgramCheck)
        .map(|r| r.check_ms.unwrap_or(0))
        .collect();
    (held.len(), held.iter().copied().max())
}

fn proc_text(ctx: &Ctx<'_>, view: &LeftView, row: &LeftRow) -> String {
    let mut t = format!("proc pid={} state={}", row.pid, row.state.word());
    if row.state == State::Waiting {
        t.push_str(" wait=program-check");
        if let Some(peer) = row.peer {
            t.push_str(&format!(" peer={peer}"));
        }
    }
    if let Some(ms) = row.check_ms {
        t.push_str(&format!(" check_ms={ms}"));
    }
    let num = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
    t.push_str(&format!(
        " started_ms={} cpu_ms={} exe={} tier={} why={} proof={}",
        num(row.started_ms),
        num(row.cpu_ms),
        row.exe_word(),
        row.tier.word(),
        row.why.word(),
        proof_words(row),
    ));
    if row.listen {
        t.push_str(" holds=listen");
    }
    t.push_str(&format!(" from={}", from_word(ctx, view, &row.from)));
    if let Some(sid) = &row.teller {
        t.push_str(&format!(" teller={}", pct(sid)));
    }
    if let Some(sid) = crate::left::left_of(view, row) {
        t.push_str(&format!(" left_of={}", pct(sid)));
    }
    t.push_str(&format!(" end={}", row.token.as_deref().unwrap_or("-")));
    t
}

fn run_root(ctx: &Ctx<'_>, run: &RunRow) -> Option<String> {
    let keys: Vec<u64> = run_key(ctx, run).into_iter().collect();
    let ok = ctx.caller_panes.is_none() || (!keys.is_empty() && ctx.path_ok(&keys));
    ok.then(|| run.roots.first().map(|r| pct(r))).flatten()
}

fn run_text(ctx: &Ctx<'_>, run: &RunRow) -> String {
    let mut t = format!(
        "run id={} state={}",
        pct(&run.id),
        if run.dead { "dead" } else { "live" }
    );
    if let Some(ms) = run.dead_ms {
        t.push_str(&format!(" dead_ms={ms}"));
    }
    let from = run_key(ctx, run)
        .and_then(|k| ctx.tab_number(k))
        .map_or_else(|| "-".to_owned(), |n| format!("tab:{n}"));
    t.push_str(&format!(
        " from={from} root={} left={}",
        run_root(ctx, run).unwrap_or_else(|| run.roots.len().to_string()),
        run.left
    ));
    if run.next_verify {
        t.push_str(" next=verify");
    }
    t
}

/// The `left` view's lines: the header, then `proc` rows (at most `top=`, with a `more` line),
/// `run` rows and `link` rows.
pub(super) fn left_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let Some(view) = s.left.as_deref() else {
        lines.push(Line {
            key: "left".into(),
            text: format!(
                "left  unavailable({}): the leftover reader has not read yet",
                super::jobs_view::missing_why(s)
            ),
            value: true,
        });
        return;
    };
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let shown = shown(ctx, view, scope);
    let (held, oldest) = held_count(&shown.rows);
    let num = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
    lines.push(Line {
        key: "left".into(),
        text: format!(
            "left as_of={} age_ms={} complete={} bounded={} program_check={held} \
             program_check_oldest_ms={}",
            clock_ms(ctx.as_of_unix_ms),
            ctx.age_ms,
            u8::from(view.complete),
            u8::from(view.bounded),
            num(oldest.map(|ms| ms.saturating_add(ctx.age_ms))),
        ),
        value: true,
    });
    for row in shown.rows.iter().take(q.top) {
        lines.push(Line {
            key: format!("proc:{}", row.pid),
            text: proc_text(ctx, view, row),
            value: true,
        });
    }
    if shown.rows.len() > q.top {
        lines.push(Line {
            key: "more".into(),
            text: format!("more procs={}", shown.rows.len() - q.top),
            value: true,
        });
    }
    for run in &shown.runs {
        lines.push(Line {
            key: format!("run:{}", run.id),
            text: run_text(ctx, run),
            value: true,
        });
    }
    for (i, link) in shown.links.iter().enumerate() {
        let mut text = format!(
            "link state=dangling root={} kind={} target={} seen={}",
            link_root(ctx, link),
            pct(&link.kind),
            link.target.word(),
            pct(&link.seen)
        );
        if ctx.caller_panes.is_none() {
            text.push_str(&format!(" path={}", pct(&link.path)));
        }
        lines.push(Line {
            key: format!("link:{i}"),
            text,
            value: true,
        });
    }
}

/// `root=`'s word for a link: its checkout's path with no caller session (no session owns a
/// link), else its number among the checkouts the view's links name.
fn link_root(ctx: &Ctx<'_>, link: &LinkRow) -> String {
    if ctx.caller_panes.is_none() {
        pct(&link.checkout)
    } else {
        link.root.to_string()
    }
}

fn opt_u(v: Option<u64>) -> Jv {
    v.map_or_else(|| Jv::why("not_sampled"), Jv::u)
}

fn proc_json(ctx: &Ctx<'_>, view: &LeftView, row: &LeftRow) -> Jv {
    let mut o = Jv::obj([
        ("pid", Jv::u(u64::from(row.pid))),
        ("state", Jv::s(row.state.word())),
    ]);
    if let Some(peer) = row.peer {
        o.set("wait", Jv::s("program-check"));
        o.set("peer", Jv::u(u64::from(peer)));
    }
    if let Some(ms) = row.check_ms {
        o.set("check_ms", Jv::u(ms));
    }
    o.set("started_ms", opt_u(row.started_ms));
    o.set("cpu_ms", opt_u(row.cpu_ms));
    o.set("exe", Jv::s(row.exe_word()));
    o.set("tier", Jv::s(row.tier.word()));
    // `why` is the contract's key of an unknown figure (`{"why": <word>}`), so the reason a row
    // is listed is `reason` here.
    o.set("reason", Jv::s(row.why.word()));
    o.set(
        "proof",
        Jv::Arr(row.proof.iter().map(|p| Jv::s(p.word())).collect()),
    );
    o.set("listen", Jv::Bool(row.listen));
    o.set("from", Jv::s(from_word(ctx, view, &row.from)));
    if let Some(sid) = &row.teller {
        o.set("teller", Jv::s(pct(sid)));
    }
    if let Some(sid) = crate::left::left_of(view, row) {
        o.set("left_of", Jv::s(pct(sid)));
    }
    o.set(
        "end",
        row.token.as_deref().map_or_else(|| Jv::s("-"), Jv::s),
    );
    if let Some(ms) = crate::left::owner_gone_ms(row, view) {
        o.set("owner_gone_ms", Jv::u(ms));
    }
    o.set(
        "entry",
        Jv::Bool(crate::left::eligible(row, view, ENTRY_AFTER_MS)),
    );
    o
}

fn run_json(ctx: &Ctx<'_>, run: &RunRow) -> Jv {
    let mut o = Jv::obj([
        ("id", Jv::s(pct(&run.id))),
        ("state", Jv::s(if run.dead { "dead" } else { "live" })),
    ]);
    if let Some(ms) = run.dead_ms {
        o.set("dead_ms", Jv::u(ms));
    }
    if let Some(n) = run_key(ctx, run).and_then(|k| ctx.tab_number(k)) {
        o.set("tab", Jv::u(n as u64));
    }
    match run_root(ctx, run) {
        Some(root) => o.set("root", Jv::s(root)),
        None => o.set("roots", Jv::u(run.roots.len() as u64)),
    }
    o.set("left", Jv::u(run.left as u64));
    o.set("next_verify", Jv::Bool(run.next_verify));
    o
}

/// The `left` view's JSON body.
pub(super) fn left_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, o: &mut Jv) {
    let Some(view) = s.left.as_deref() else {
        let why = super::jobs_view::missing_why(s);
        o.set("left", Jv::why(why));
        o.set("procs", Jv::why(why));
        return;
    };
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let shown = shown(ctx, view, scope);
    let (held, oldest) = held_count(&shown.rows);
    o.set(
        "left",
        Jv::obj([
            ("complete", Jv::Bool(view.complete)),
            ("bounded", Jv::Bool(view.bounded)),
            ("program_check", Jv::u(held as u64)),
            (
                "program_check_oldest_ms",
                oldest.map_or_else(
                    || Jv::why("not_sampled"),
                    |ms| Jv::u(ms.saturating_add(ctx.age_ms)),
                ),
            ),
        ]),
    );
    o.set(
        "procs",
        Jv::Arr(
            shown
                .rows
                .iter()
                .take(q.top)
                .map(|r| proc_json(ctx, view, r))
                .collect(),
        ),
    );
    o.set("more", Jv::u(shown.rows.len().saturating_sub(q.top) as u64));
    o.set(
        "runs",
        Jv::Arr(shown.runs.iter().map(|r| run_json(ctx, r)).collect()),
    );
    o.set(
        "links",
        Jv::Arr(
            shown
                .links
                .iter()
                .map(|l| {
                    let mut o = Jv::obj([
                        ("state", Jv::s("dangling")),
                        ("root", Jv::u(l.root as u64)),
                        ("kind", Jv::s(pct(&l.kind))),
                        ("target", Jv::s(l.target.word())),
                        ("seen", Jv::s(pct(&l.seen))),
                    ]);
                    if ctx.caller_panes.is_none() {
                        o.set("checkout", Jv::s(pct(&l.checkout)));
                        o.set("path", Jv::s(pct(&l.path)));
                    }
                    o
                })
                .collect(),
        ),
    );
}

#[cfg(test)]
#[path = "left_tests.rs"]
pub(crate) mod tests;
