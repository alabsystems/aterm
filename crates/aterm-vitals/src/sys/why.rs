// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sys why` (`docs/DESIGN-resource-details-2026-10-20.md` §2): who uses the
//! machine's CPU or memory and why, as text and JSON, rendered from the cause
//! engine's [`Account`] ([`crate::explain`]), the one the CPU close-up's Why
//! now renders, so the two cannot disagree.
//!
//! TEXT, one record per line, each starting with its key: the `why` sentence
//! (closed words and numbers only, never a name a process chose); for the
//! CPU, `split`, `you`, the `who` rows, `more` and `withheld`, the `cause`
//! lines, `pass`, `do` (or `cannot what=remedy` in its place) and the
//! `cannot` lines; for memory, two `classes` lines, `you` and `owners` before
//! the same rows. A calm bare `why` is the sentence and `you` alone.
//!
//! WHO MAY SEE A ROW: an owner-class caller sees every `who` row; a scoped
//! one only a coalition whose live members are all in its tab (never
//! aterm's own, and none while the sweep is cut), the rest summed on
//! `withheld`, so the rows still add up. A scoped caller's `cause` lines
//! leave out other tabs' `tab-work` and `app-work`'s `coalition=`, and its
//! sentence calls another tab's work `another tab's work`.
//!
//! SIZE: rows past `top=`, and in a text reply with no `top=` the rows past
//! [`REPLY_MAX`] bytes with the header, move from the bottom into `more`.

use super::json::Jv;
use super::query::{ScopeArg, SysQuery, View};
use super::view::{
    Line, c2, classes_json, exact_partition, header_line, mem_classes, mem_clauses, rec,
    scope_keys, unavailable,
};
use super::{Ctx, clock_s, exe_word};
use crate::explain::{
    Account, Bare, CauseId, CoalRow, Context, Evidence, Part, Role, Shown, Via, WakeFact,
    WhySubject, account_of, bare_subject,
};
use crate::model::{Snapshot, Throttle};
use crate::words;

/// A text reply's most bytes with its header line, as the server sends it,
/// unless `top=` was given.
pub const REPLY_MAX: usize = 2048;

/// What `q` reads over `snap`: its subject, or the bare reply's choice
/// (the CPU before any snapshot).
fn resolve(q: &SysQuery, snap: Option<&Snapshot>) -> Bare {
    match (q.subject, snap) {
        (Some(subject), _) => Bare::Subject(subject),
        (None, Some(s)) => bare_subject(s),
        (None, None) => Bare::Subject(WhySubject::Cpu),
    }
}

/// The header's `subject=` word for `q` over `snap`: `global.cpu`,
/// `global.mem`, or `global` for the calm reply; `None` off `why`.
#[must_use]
pub fn subject_word(q: &SysQuery, snap: Option<&Snapshot>) -> Option<&'static str> {
    (q.view == View::Why).then(|| match resolve(q, snap) {
        Bare::Calm => "global",
        Bare::Subject(WhySubject::Cpu) => "global.cpu",
        Bare::Subject(WhySubject::Mem) => "global.mem",
    })
}

fn line(key: impl Into<String>, text: impl Into<String>) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: true,
    }
}

/// A team id as `team=` prints it: ten capital letters or digits.
fn team_word(team: &str) -> bool {
    team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Millicores as cores-worth.
fn cores(mc: i64) -> f32 {
    mc as f32 / 1000.0
}

/// A figure that cannot be below 0 (bytes, ms, millicores), as JSON's
/// unsigned ints take it.
fn nonneg(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// The split's parts as they print: the caller's tab, the rest of aterm,
/// others, services and the residual, with the bounds a missing reap credit
/// puts on the tab pair (§3.2).
#[derive(Clone, Copy, Debug)]
struct Printed {
    /// The tab's figure; `Err` with the word where it is not read.
    tab: Result<i64, &'static str>,
    /// `≥` on the tab's figure.
    tab_low: bool,
    aterm_rest: i64,
    /// `≤` on the rest of aterm's.
    rest_high: bool,
    others: i64,
    services: i64,
    /// The signed residual; `None` by subtraction, where services holds it.
    unaccounted: Option<i64>,
}

/// One reply's reading: the account and who asks.
struct Reading<'a> {
    ctx: &'a Ctx<'a>,
    s: &'a Snapshot,
    a: Account,
    /// The caller's tab: its sessions.
    panes: Vec<u64>,
}

impl<'a> Reading<'a> {
    fn new(ctx: &'a Ctx<'a>, s: &'a Snapshot, subject: WhySubject) -> Self {
        let panes = scope_keys(ctx, ScopeArg::Tab);
        Self {
            ctx,
            s,
            a: account_of(subject, &panes, s),
            panes,
        }
    }

    /// Whether the caller may see tab `key`'s work.
    fn tab_shown(&self, key: u64) -> bool {
        self.ctx.owner_class || self.panes.contains(&key)
    }

    /// Whether the caller may see coalition `r`'s row: never aterm's, and
    /// none while the sweep is cut, since a member it dropped could be
    /// outside the tab.
    fn row_shown(&self, r: &CoalRow) -> bool {
        let cut =
            self.s.procs_truncated || self.a.facts.as_deref().is_some_and(|f| f.procs_truncated);
        self.ctx.owner_class
            || (!cut
                && r.part != Part::Aterm
                && r.tabs
                    .as_ref()
                    .is_some_and(|keys| keys.iter().all(|k| self.panes.contains(k))))
    }

    /// The causes the caller may see: no other tab's work.
    fn causes(&self) -> impl Iterator<Item = &Shown> {
        self.a.causes.iter().filter(|c| match c.cause.id {
            CauseId::TabWork(k) => self.tab_shown(k),
            _ => true,
        })
    }

    /// Why the split is not read: still to come, or never on this reading.
    fn split_missing(&self) -> &'static str {
        if self.s.coal_lane.on() || self.s.procs_window_ns.is_none() {
            "warming"
        } else {
            "not_sampled"
        }
    }

    fn printed(&self) -> Option<Printed> {
        let f = self.a.facts.as_deref()?;
        let (tab, p) = match (self.a.parts, self.a.you.role) {
            (Some(p), _) => (Ok(p.tab), p),
            (None, Role::Unavailable(w)) => (Err(w), f.split.parts_of(&[])),
            (None, _) => (Err("not_sampled"), f.split.parts_of(&[])),
        };
        let cut = self.s.procs_truncated || f.procs_truncated;
        let pair = cut || self.a.you.unattributed_mc.is_some_and(|u| u > 0) || tab.is_err();
        let (services, unaccounted) = if f.split.via == Via::Subtraction && p.unaccounted >= 0 {
            (p.services + p.unaccounted, None)
        } else {
            (p.services, Some(p.unaccounted))
        };
        Some(Printed {
            tab,
            tab_low: pair || !self.s.delegated.is_empty(),
            aterm_rest: p.aterm_rest,
            rest_high: pair,
            others: p.others,
            services,
            unaccounted,
        })
    }

    /// The figure the CPU's sentence and `figure` read: the split's busy
    /// cores over its window, or the snapshot's where no split is read.
    fn busy_mc(&self) -> Option<i64> {
        match self.a.facts.as_deref() {
            Some(f) => Some(f.split.global_mc),
            None => self
                .s
                .cpu
                .as_ref()
                .map(|c| (f64::from(c.busy.raw) * 1000.0).round() as i64),
        }
    }

    fn busy_words(&self) -> String {
        let (Some(cpu), Some(busy)) = (self.s.cpu.as_ref(), self.busy_mc()) else {
            return "cores measuring (the first sample has no interval)".into();
        };
        let motion = cpu
            .busy
            .motion(words::CORES_GATE)
            .word()
            .map(|w| format!(", {w}"))
            .unwrap_or_default();
        format!(
            "{} of {} cores busy{motion}",
            words::cores(cores(busy)),
            cpu.total()
        )
    }

    /// A part's people word.
    fn part_word(&self, part: Part) -> &'static str {
        match part {
            Part::Services => self.ctx.machine.services_word(),
            Part::Others => "others",
            Part::Aterm => "aterm",
        }
    }

    /// `mostly macOS services (2.17c)`: the largest part.
    fn mostly(&self) -> Option<String> {
        let p = self.printed()?;
        let aterm = p.tab.unwrap_or(0) + p.aterm_rest;
        let parts = [
            (Part::Services, p.services),
            (Part::Others, p.others),
            (Part::Aterm, aterm),
        ];
        let (part, mc) = parts
            .into_iter()
            .fold((Part::Services, i64::MIN), |best, (part, mc)| {
                if mc > best.1 { (part, mc) } else { best }
            });
        (mc > 0).then(|| format!("mostly {} ({})", self.part_word(part), c2(cores(mc))))
    }

    /// The headline in Why now's first-row words, with no mark, name or chip.
    fn headline(&self, subject: WhySubject) -> String {
        let Some((id, context)) = self.a.headline else {
            // The CPU's covering causes are read with its split.
            return match (subject, self.a.facts.is_some(), self.split_missing()) {
                (WhySubject::Cpu, false, "warming") => "causes not read yet",
                (WhySubject::Cpu, false, _) => "causes not read now",
                _ => "no cause open",
            }
            .into();
        };
        let shown = self.a.causes.iter().find(|c| c.cause.id == id);
        let base = match id {
            CauseId::Activity(a) => a.activity().word.to_owned(),
            CauseId::AppWork(_) => "an app running as you".into(),
            CauseId::TabWork(k) if !self.tab_shown(k) => "another tab's work".into(),
            CauseId::TabWork(k) => self
                .ctx
                .tab_number(k)
                .map_or_else(|| "a tab's work".into(), |n| format!("Tab {n}'s work")),
            CauseId::AtermWork => "aterm itself".into(),
            CauseId::Unexplained => {
                let part = match shown.map(|c| &c.cause.evidence) {
                    Some(Evidence::Uncovered { part }) => *part,
                    _ => Part::Services,
                };
                format!("no cause aterm can name: {}", self.part_word(part))
            }
            CauseId::Context(c) => {
                return match (c, shown.map(|c| &c.cause.evidence)) {
                    (Context::Throttled, Some(Evidence::Throttled { level, .. }))
                        if *level >= Throttle::Light =>
                    {
                        format!("throttled: {}", level.word())
                    }
                    (Context::Throttled, _) => "throttled: low power".into(),
                    (Context::Pressure, _) if subject == WhySubject::Mem => {
                        "memory pressure is the cause".into()
                    }
                    (Context::Pressure, Some(Evidence::Pressure { level, .. })) => {
                        format!("memory pressure {}", level.word())
                    }
                    (Context::Pressure, _) => "memory pressure".into(),
                    (Context::Startup, _) => "after start-up".into(),
                    (Context::Wake, _) => "since wake".into(),
                };
            }
        };
        match context {
            Some(Context::Startup) => format!("{base}, after start-up"),
            Some(Context::Wake) => format!("{base}, since wake"),
            _ => base,
        }
    }

    /// The tab clause, by the `you` role.
    fn tab_clause(&self) -> &'static str {
        match self.a.you.role {
            Role::Main => "this tab is the main cause",
            Role::Part => "this tab is part of the cause",
            Role::Minor => "this tab is not the cause",
            Role::None => "this tab uses none of it",
            Role::Unavailable(_) => "this tab's share is not read now",
        }
    }

    fn sentence(clauses: &[String]) -> Line {
        line("why", format!("{:<8}{}", "why", clauses.join("; ")))
    }

    fn cpu_sentence(&self) -> Line {
        let mut clauses = vec![format!("global cpu: {}", self.busy_words())];
        clauses.extend(self.mostly());
        clauses.push(self.headline(WhySubject::Cpu));
        clauses.push(self.tab_clause().into());
        Self::sentence(&clauses)
    }

    fn mem_sentence(&self) -> Line {
        let mut clauses = Vec::new();
        match self.s.mem.as_ref() {
            Some(m) => {
                let x = &m.sample;
                clauses.push(format!(
                    "global memory: {} of {} GiB used",
                    words::gib_figure(x.used()),
                    words::gib_total(x.total)
                ));
                if x.pressure.rank().is_some() {
                    clauses.push(format!("pressure {}", x.pressure.word()));
                }
                clauses.push(if x.swap_used == 0 {
                    "swap 0".into()
                } else {
                    format!("swap {}", words::mem(x.swap_used))
                });
                match x.compressed {
                    Some(0) => clauses.push("nothing compressed".into()),
                    Some(b) => clauses.push(format!("{} GiB compressed", words::gib_figure(b))),
                    None => {}
                }
            }
            None => clauses.push(format!("global memory: {}", unavailable("unsupported"))),
        }
        clauses.push(self.headline(WhySubject::Mem));
        clauses.push(self.tab_clause().into());
        Self::sentence(&clauses)
    }

    fn calm_sentence(&self) -> Line {
        let mut clauses = vec![format!("calm: {}", self.busy_words())];
        if let Some(m) = self.s.mem.as_ref() {
            clauses.extend(mem_clauses(m));
        }
        clauses.push("no cause open".into());
        Self::sentence(&clauses)
    }

    fn split_line(&self) -> Line {
        let (Some(f), Some(p)) = (self.a.facts.as_deref(), self.printed()) else {
            return line(
                "split",
                format!("split {}", unavailable(self.split_missing())),
            );
        };
        let tab = match p.tab {
            Ok(mc) => format!("{}{mc}", if p.tab_low { "≥" } else { "" }),
            Err(w) => unavailable(w),
        };
        let mut t = format!(
            "split tab_mc={tab} aterm_rest_mc={}{} others_mc={} services_mc={} unaccounted_mc={}",
            if p.rest_high { "≤" } else { "" },
            p.aterm_rest,
            p.others,
            p.services,
            p.unaccounted.map_or_else(
                || unavailable(f.lane.word().unwrap_or("lane_off:coalitions")),
                |u| u.to_string()
            )
        );
        if f.split.delegated_mc > 0 {
            t.push_str(&format!(" delegated_mc={}", f.split.delegated_mc));
        }
        t.push_str(&format!(
            " exact={} via={}",
            if f.split.exact { "exact" } else { "heuristic" },
            f.split.via.word()
        ));
        if f.split.via == Via::Coalitions {
            t.push_str(&format!(" n={}", f.split.listed));
        }
        line("split", t)
    }

    fn you_line(&self, subject: WhySubject) -> Line {
        let y = &self.a.you;
        let mut t = format!(
            "you tab={} role=",
            self.ctx
                .target
                .tab
                .map_or_else(|| "-".to_owned(), |n| n.to_string())
        );
        match y.role {
            Role::Unavailable(w) => t.push_str(&unavailable(w)),
            r => t.push_str(r.word()),
        }
        if y.calm {
            t.push_str(" basis=machine_calm");
        }
        match (subject, y.figure, y.unattributed_mc) {
            (WhySubject::Cpu, Some(fig), Some(u)) => {
                t.push_str(&format!(" cpu_at_least_mc={fig} unattributed_mc={u}"));
            }
            (_, Some(fig), _) if !matches!(y.role, Role::Unavailable(_)) => {
                let key = match subject {
                    WhySubject::Cpu => "cpu_mc",
                    WhySubject::Mem => "fp_bytes",
                };
                t.push_str(&format!(" {key}={fig}"));
                if let Some(pm) = y.share_pm {
                    t.push_str(&format!(" share_pm={pm}"));
                }
            }
            _ => {}
        }
        line("you", t)
    }

    /// Every coalition that answered, ranked for `subject`: CPU, or
    /// footprint (an unread one last).
    fn ranked(&self, subject: WhySubject) -> Vec<&CoalRow> {
        let Some(f) = self.a.facts.as_deref().filter(|f| f.lane.on()) else {
            return Vec::new();
        };
        let mut rows: Vec<&CoalRow> = f.split.rows.iter().collect();
        if subject == WhySubject::Mem {
            rows.sort_by(|a, b| b.footprint.cmp(&a.footprint).then(a.id.cmp(&b.id)));
        }
        rows
    }

    /// A row's figure for `subject`: millicores, or footprint bytes; `None`
    /// where unread (a coalition the window did not measure, or a footprint
    /// the kernel did not write).
    fn figure(r: &CoalRow, subject: WhySubject) -> Option<i64> {
        match subject {
            WhySubject::Cpu => r.measured.then_some(r.cpu_mc),
            WhySubject::Mem => r.footprint.map(|b| i64::try_from(b).unwrap_or(i64::MAX)),
        }
    }

    /// `rows`' figures summed; `None` where a footprint is unread, never a
    /// 0 for it. An unmeasured coalition's CPU is the residual's, so it adds
    /// nothing and the rows still close against `unaccounted`.
    fn sum(rows: &[&CoalRow], subject: WhySubject) -> Option<i64> {
        rows.iter()
            .try_fold(0_i64, |sum, r| match (Self::figure(r, subject), subject) {
                (Some(v), _) => Some(sum.saturating_add(v)),
                (None, WhySubject::Cpu) => Some(sum),
                (None, WhySubject::Mem) => None,
            })
    }

    /// The covering cause a row's coalition is covered by.
    fn cause_of(&self, r: &CoalRow) -> Option<CauseId> {
        self.a
            .causes
            .iter()
            .find(|c| c.cause.id.covering() && c.cause.coalitions.contains(&r.id))
            .map(|c| c.cause.id)
    }

    /// The named activity whose highest-ranked member row `r` is: its read
    /// rate and deferred share print there, once.
    fn activity_head(&self, r: &CoalRow) -> Option<(u64, Option<u16>)> {
        self.a
            .causes
            .iter()
            .find_map(|c| match (&c.cause.id, &c.cause.evidence) {
                (
                    CauseId::Activity(_),
                    Evidence::Activity {
                        deferred_pm,
                        read_bps,
                    },
                ) if c.cause.coalitions.first() == Some(&r.id) => Some((*read_bps, *deferred_pm)),
                _ => None,
            })
    }

    fn who_line(&self, r: &CoalRow, subject: WhySubject) -> Line {
        let mut t = format!("who coalition={}", r.id);
        match r.lead.as_ref() {
            Some(l) => {
                t.push_str(&format!(
                    " pid={} birth_ms={} exe={} voucher={}",
                    l.pid,
                    l.birth_ms,
                    l.exe.as_deref().map_or("-", exe_word),
                    l.voucher.word()
                ));
                if let Some(team) = l.team.as_deref().filter(|t| team_word(t)) {
                    t.push_str(&format!(" team={team}"));
                }
            }
            None => t.push_str(&format!(
                " pid={0} birth_ms={0} exe=- voucher=unknown",
                unavailable("not_sampled")
            )),
        }
        match r.part {
            Part::Aterm => t.push_str(" scope=aterm"),
            part => t.push_str(&format!(
                " uid={} part={}",
                rec(r.lead.as_ref().and_then(|l| l.uid), "not_sampled"),
                part.word()
            )),
        }
        match subject {
            WhySubject::Cpu => {
                t.push_str(&format!(
                    " cpu_mc={}",
                    rec(Self::figure(r, subject), "not_sampled")
                ));
                if r.delegated_mc > 0 {
                    t.push_str(&format!(" delegated_mc={}", r.delegated_mc));
                }
                if let Some((read, deferred)) = self.activity_head(r) {
                    t.push_str(&format!(
                        " read_bps={read} deferred_pm={}",
                        rec(deferred, "not_sampled")
                    ));
                }
                if r.finished {
                    t.push_str(" finished_included=1");
                }
                t.push_str(&format!(
                    " cause={}",
                    self.cause_of(r).map_or("-", CauseId::word)
                ));
            }
            WhySubject::Mem => t.push_str(&format!(
                " fp_bytes={} live={}",
                rec(r.footprint, "not_sampled"),
                r.live
            )),
        }
        line(format!("who {}", r.id), t)
    }

    /// `more`/`withheld who=<n> cpu_mc=|fp_bytes=<sum>`.
    fn sum_line(key: &str, rows: &[&CoalRow], subject: WhySubject) -> Line {
        let unit = match subject {
            WhySubject::Cpu => "cpu_mc",
            WhySubject::Mem => "fp_bytes",
        };
        line(
            key,
            format!(
                "{key} who={} {unit}={}",
                rows.len(),
                rec(Self::sum(rows, subject), "not_sampled")
            ),
        )
    }

    fn wake_words(w: &WakeFact) -> String {
        format!(
            " wake={} wake_reason={} slept_s={}",
            clock_s(nonneg(w.at_ms)),
            w.reason.as_deref().map_or("-", exe_word),
            rec(w.slept_s, "not_sampled")
        )
    }

    fn cause_line(&self, c: &Shown) -> Line {
        let cause = &c.cause;
        let mut t = format!("cause id={}", cause.id.word());
        if cause.id.covering() {
            t.push_str(&format!(" covers_mc={}", cause.covers_mc));
        }
        if let Some(since) = cause.since_ms {
            t.push_str(&format!(" since={}", clock_s(nonneg(since))));
        }
        if let Some(opened) = c.opened_unix_ms {
            t.push_str(&format!(" opened={}", clock_s(nonneg(opened))));
        }
        match (&cause.id, &cause.evidence) {
            (_, Evidence::Activity { deferred_pm, .. }) => {
                t.push_str(&format!(
                    " deferred_pm={}",
                    rec(*deferred_pm, "not_sampled")
                ));
            }
            (CauseId::TabWork(k), _) => match self.ctx.tab_number(*k) {
                Some(n) => t.push_str(&format!(" tab={n}")),
                None => t.push_str(&format!(" local={k}")),
            },
            (CauseId::AppWork(id), _) if self.ctx.owner_class => {
                t.push_str(&format!(" coalition={id}"));
            }
            (_, Evidence::Uncovered { part }) => t.push_str(&format!(" part={}", part.word())),
            (_, Evidence::Startup { wake: Some(w), .. } | Evidence::Wake(w)) => {
                t.push_str(&Self::wake_words(w))
            }
            (_, Evidence::Pressure { level, swap_bps }) => t.push_str(&format!(
                " level={} swap_bps={}",
                level.word(),
                rec(*swap_bps, "not_sampled")
            )),
            (_, Evidence::Throttled { level, low_power }) => t.push_str(&format!(
                " level={} low_power={}",
                level.word(),
                rec(low_power.map(u8::from), "unsupported")
            )),
            _ => {}
        }
        t.push_str(&format!(" sure=held src={}", cause.src().join(",")));
        // One app-work holds at most (the busiest coalition outside aterm's),
        // so its key names no coalition: a `since=` reply's `same` line
        // prints keys, the scoped caller's too.
        let key = match cause.id {
            CauseId::TabWork(k) => format!("cause {} {k}", cause.id.word()),
            id => format!("cause {}", id.word()),
        };
        line(key, t)
    }

    /// The figure's trend word; `None` while the cores are unread (the first
    /// sample), which the text prints as `unavailable(warming)`.
    fn pass_word(&self, subject: WhySubject) -> Option<&'static str> {
        let motion = match subject {
            WhySubject::Cpu => self.s.cpu.as_ref()?.busy.motion(words::CORES_GATE),
            WhySubject::Mem => self
                .s
                .global
                .footprint_gib
                .motion(words::FOOTPRINT_GATE_GIB),
        };
        Some(motion.word().unwrap_or("steady"))
    }

    fn pass_json(&self, subject: WhySubject) -> Jv {
        self.pass_word(subject)
            .map_or_else(|| Jv::why("warming"), Jv::s)
    }

    /// The `pass`, `do` (or the remedy's `cannot` in its place) and `cannot`
    /// lines.
    fn tail(&self, subject: WhySubject) -> Vec<Line> {
        let mut out = vec![line(
            "pass",
            format!(
                "pass trend={} src=kernel",
                self.pass_word(subject)
                    .map_or_else(|| unavailable("warming"), str::to_owned)
            ),
        )];
        if self.a.nothing_needed {
            out.push(line("do none", "do class=none may=agent destructive=0"));
        }
        for (what, why) in &self.a.cannot {
            out.push(line(
                format!("cannot {what}"),
                format!("cannot what={what} why={why}"),
            ));
        }
        out
    }

    fn classes_lines(&self) -> Vec<Line> {
        let Some(m) = self.s.mem.as_ref() else {
            return vec![line(
                "classes",
                format!("classes total_bytes={}", unavailable("unsupported")),
            )];
        };
        let exact = exact_partition(self.ctx);
        let c = mem_classes(m, exact);
        let b = |i: usize| rec(c[i].1, "not_sampled");
        vec![
            line(
                "classes",
                format!(
                    "classes total_bytes={} app_bytes={} wired_bytes={} compressed_bytes={} \
                     used_bytes={} cache_bytes={} free_bytes={} src=kernel",
                    m.sample.total,
                    b(0),
                    b(1),
                    b(2),
                    m.sample.used(),
                    b(3),
                    b(4)
                ),
            ),
            line(
                "classes derived",
                format!("classes {}_bytes={} src=derived", c[5].0, b(5)),
            ),
        ]
    }

    fn owners_line(rows: &[&CoalRow]) -> Line {
        line(
            "owners",
            format!(
                "owners fp_bytes={} coalitions={} live_only=1 src=kernel",
                rec(Self::sum(rows, WhySubject::Mem), "not_sampled"),
                rows.len()
            ),
        )
    }
}

/// The rows a reply lists: those the caller may see, at most `top` of them;
/// the rest it may see; and those it may not.
struct Rows<'r> {
    shown: Vec<&'r CoalRow>,
    more: Vec<&'r CoalRow>,
    withheld: Vec<&'r CoalRow>,
}

impl<'r> Rows<'r> {
    fn of(w: &Reading<'_>, ranked: &[&'r CoalRow], top: usize) -> Self {
        let (seen, withheld): (Vec<&CoalRow>, Vec<&CoalRow>) =
            ranked.iter().partition(|r| w.row_shown(r));
        let cut = top.min(seen.len());
        Self {
            shown: seen[..cut].to_vec(),
            more: seen[cut..].to_vec(),
            withheld,
        }
    }

    /// Move the last listed row into `more`.
    fn cut_one(&mut self) -> bool {
        match self.shown.pop() {
            Some(r) => {
                self.more.insert(0, r);
                true
            }
            None => false,
        }
    }

    fn lines(&self, w: &Reading<'_>, subject: WhySubject) -> Vec<Line> {
        let mut out: Vec<Line> = self.shown.iter().map(|r| w.who_line(r, subject)).collect();
        if !self.more.is_empty() {
            out.push(Reading::sum_line("more", &self.more, subject));
        }
        if !self.withheld.is_empty() {
            out.push(Reading::sum_line("withheld", &self.withheld, subject));
        }
        out
    }
}

/// `sys why`'s text body for `q` over `s`, after the `lines` already in the
/// reply and before its `events` (both counted in the byte cut).
pub fn why_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>, events: &[Line]) {
    let subject = match resolve(q, Some(s)) {
        Bare::Calm => {
            let w = Reading::new(ctx, s, WhySubject::Cpu);
            lines.push(w.calm_sentence());
            lines.push(w.you_line(WhySubject::Cpu));
            return;
        }
        Bare::Subject(subject) => subject,
    };
    let w = Reading::new(ctx, s, subject);
    let head = match subject {
        WhySubject::Cpu => vec![w.cpu_sentence(), w.split_line(), w.you_line(subject)],
        WhySubject::Mem => {
            let mut head = vec![w.mem_sentence()];
            head.extend(w.classes_lines());
            head.push(w.you_line(subject));
            head
        }
    };
    let ranked = w.ranked(subject);
    let mut head = head;
    if subject == WhySubject::Mem && w.a.facts.as_ref().is_some_and(|f| f.lane.on()) {
        head.push(Reading::owners_line(&ranked));
    }
    let mut tail: Vec<Line> = w.causes().map(|c| w.cause_line(c)).collect();
    tail.extend(w.tail(subject));
    let mut rows = Rows::of(&w, &ranked, q.top);
    loop {
        let body = rows.lines(&w, subject);
        let n = lines.len() + head.len() + body.len() + tail.len() + events.len();
        // A `since=` reply's header may say ` delta=1`: counted, so the cut
        // holds whatever folds.
        let header = header_line(
            View::Why,
            None,
            subject_word(q, Some(s)),
            n,
            ctx,
            q.since.is_some(),
        );
        let sent: usize = header.len()
            + 1
            + lines
                .iter()
                .chain(&head)
                .chain(&body)
                .chain(&tail)
                .chain(events)
                .map(|l| l.text.len() + 1)
                .sum::<usize>();
        if q.top_set || sent <= REPLY_MAX || !rows.cut_one() {
            lines.extend(head);
            lines.extend(body);
            lines.extend(tail);
            return;
        }
    }
}

/// One `{fact, <key>, src}` item of a cause's evidence.
fn fact(fact: &str, key: &str, value: Jv, src: &str) -> Jv {
    Jv::obj([("fact", Jv::s(fact)), (key, value), ("src", Jv::s(src))])
}

fn wake_facts(w: &WakeFact) -> [Jv; 3] {
    [
        fact("wake", "at_unix_ms", Jv::u(nonneg(w.at_ms)), "kernel"),
        fact(
            "wake_reason",
            "reason",
            Jv::s(w.reason.as_deref().map_or("-", exe_word)),
            "kernel",
        ),
        fact(
            "slept",
            "slept_s",
            Jv::known_or(w.slept_s, "not_sampled"),
            "kernel",
        ),
    ]
}

impl Reading<'_> {
    fn split_json(&self) -> Jv {
        let (Some(f), Some(p)) = (self.a.facts.as_deref(), self.printed()) else {
            return Jv::why(self.split_missing());
        };
        let tab = match p.tab {
            Ok(mc) if p.tab_low => Jv::at_least(nonneg(mc), "not_sampled"),
            Ok(mc) => Jv::Int(mc),
            Err(w) => Jv::why(w),
        };
        let rest = if p.rest_high {
            Jv::at_most(nonneg(p.aterm_rest), "not_sampled")
        } else {
            Jv::Int(p.aterm_rest)
        };
        Jv::obj([
            ("tab", tab),
            ("aterm_rest", rest),
            ("others", Jv::Int(p.others)),
            ("services", Jv::Int(p.services)),
            (
                "unaccounted",
                p.unaccounted.map_or_else(
                    || Jv::why(f.lane.word().unwrap_or("lane_off:coalitions")),
                    Jv::Int,
                ),
            ),
        ])
    }

    fn figure_json(&self, subject: WhySubject) -> Jv {
        let mut o = match subject {
            WhySubject::Cpu => Jv::obj([
                (
                    "cpu_mc",
                    self.busy_mc()
                        .map_or_else(|| Jv::why("warming"), |mc| Jv::u(nonneg(mc))),
                ),
                (
                    "capacity_mc",
                    self.s
                        .cpu
                        .as_ref()
                        .map_or_else(|| Jv::why("warming"), |c| Jv::u(c.total() as u64 * 1000)),
                ),
            ]),
            WhySubject::Mem => match self.s.mem.as_ref() {
                Some(m) => Jv::obj([
                    ("used_bytes", Jv::u(m.sample.used())),
                    ("total_bytes", Jv::u(m.sample.total)),
                ]),
                None => Jv::obj([
                    ("used_bytes", Jv::why("unsupported")),
                    ("total_bytes", Jv::why("unsupported")),
                ]),
            },
        };
        o.set("trend", self.pass_json(subject));
        match self.a.facts.as_deref() {
            Some(f) => {
                o.set(
                    "exact",
                    Jv::s(if f.split.exact { "exact" } else { "heuristic" }),
                );
                o.set("via", Jv::s(f.split.via.word()));
                if f.split.via == Via::Coalitions {
                    o.set("n", Jv::u(u64::from(f.split.listed)));
                }
                if subject == WhySubject::Cpu && f.split.delegated_mc > 0 {
                    o.set("delegated_mc", Jv::u(nonneg(f.split.delegated_mc)));
                }
            }
            None => {
                o.set("exact", Jv::s("heuristic"));
                o.set("via", Jv::why(self.split_missing()));
            }
        }
        o
    }

    fn you_json(&self, subject: WhySubject) -> Jv {
        let y = &self.a.you;
        let mut o = Jv::obj([
            (
                "tab",
                self.ctx
                    .target
                    .tab
                    .map_or_else(|| Jv::why("gone"), |t| Jv::u(t as u64)),
            ),
            (
                "role",
                match y.role {
                    Role::Unavailable(w) => Jv::why(w),
                    r => Jv::s(r.word()),
                },
            ),
        ]);
        if y.calm {
            o.set("basis", Jv::s("machine_calm"));
        }
        match (subject, y.figure, y.unattributed_mc) {
            (WhySubject::Cpu, Some(fig), Some(u)) => {
                o.set("cpu_at_least_mc", Jv::u(nonneg(fig)));
                o.set("unattributed_mc", Jv::u(nonneg(u)));
            }
            (_, Some(fig), _) if !matches!(y.role, Role::Unavailable(_)) => {
                let key = match subject {
                    WhySubject::Cpu => "cpu_mc",
                    WhySubject::Mem => "fp_bytes",
                };
                o.set(key, Jv::u(nonneg(fig)));
                if let Some(pm) = y.share_pm {
                    o.set("share_pm", Jv::u(u64::from(pm)));
                }
            }
            _ => {}
        }
        o
    }

    fn who_json(&self, r: &CoalRow, subject: WhySubject) -> Jv {
        let unread = || Jv::why("not_sampled");
        let lead = r.lead.as_ref();
        let mut o = Jv::obj([
            ("coalition", Jv::u(r.id)),
            ("pid", lead.map_or_else(unread, |l| Jv::u(u64::from(l.pid)))),
            (
                "birth_ms",
                lead.map_or_else(unread, |l| Jv::u(nonneg(l.birth_ms))),
            ),
            (
                "exe",
                Jv::s(lead.and_then(|l| l.exe.as_deref()).map_or("-", exe_word)),
            ),
            (
                "voucher",
                Jv::s(lead.map_or("unknown", |l| l.voucher.word())),
            ),
        ]);
        if let Some(team) = lead
            .and_then(|l| l.team.as_deref())
            .filter(|t| team_word(t))
        {
            o.set("team", Jv::s(team));
        }
        match r.part {
            Part::Aterm => o.set("scope", Jv::s("aterm")),
            part => {
                o.set(
                    "uid",
                    lead.and_then(|l| l.uid)
                        .map_or_else(unread, |u| Jv::u(u64::from(u))),
                );
                o.set("part", Jv::s(part.word()));
            }
        }
        match subject {
            WhySubject::Cpu => {
                o.set(
                    "cpu_mc",
                    Jv::known_or(Self::figure(r, subject).map(nonneg), "not_sampled"),
                );
                if r.delegated_mc > 0 {
                    o.set("delegated_mc", Jv::u(nonneg(r.delegated_mc)));
                }
                if let Some((read, deferred)) = self.activity_head(r) {
                    o.set("read_bps", Jv::u(read));
                    o.set(
                        "deferred_pm",
                        Jv::known_or(deferred.map(u64::from), "not_sampled"),
                    );
                }
                if r.finished {
                    o.set("finished_included", Jv::Bool(true));
                }
                o.set("cause", Jv::s(self.cause_of(r).map_or("-", CauseId::word)));
            }
            WhySubject::Mem => {
                o.set("fp_bytes", Jv::known_or(r.footprint, "not_sampled"));
                o.set("live", Jv::u(u64::from(r.live)));
            }
        }
        o
    }

    fn sum_json(rows: &[&CoalRow], subject: WhySubject) -> Jv {
        let unit = match subject {
            WhySubject::Cpu => "cpu_mc",
            WhySubject::Mem => "fp_bytes",
        };
        Jv::obj([
            ("who", Jv::u(rows.len() as u64)),
            (
                unit,
                Jv::known_or(Self::sum(rows, subject).map(nonneg), "not_sampled"),
            ),
        ])
    }

    fn cause_json(&self, c: &Shown) -> Jv {
        let cause = &c.cause;
        let mut o = Jv::obj([("cause", Jv::s(cause.id.word())), ("sure", Jv::s("held"))]);
        if let Some(since) = cause.since_ms {
            o.set("since_ms", Jv::u(nonneg(since)));
        }
        if let Some(opened) = c.opened_unix_ms {
            o.set("opened_ms", Jv::u(nonneg(opened)));
        }
        if cause.id.covering() {
            o.set("covers_mc", Jv::u(nonneg(cause.covers_mc)));
        }
        let mut evidence = Vec::new();
        match (&cause.id, &cause.evidence) {
            (
                _,
                Evidence::Activity {
                    deferred_pm,
                    read_bps,
                },
            ) => {
                evidence.push(fact(
                    "deferred",
                    "deferred_pm",
                    Jv::known_or(deferred_pm.map(u64::from), "not_sampled"),
                    "kernel",
                ));
                evidence.push(fact("read", "read_bps", Jv::u(*read_bps), "kernel"));
                evidence.push(fact(
                    "signature",
                    "coalitions",
                    Jv::u(cause.coalitions.len() as u64),
                    "signature",
                ));
            }
            (CauseId::TabWork(k), _) => evidence.push(fact(
                "tab",
                "tab",
                self.ctx
                    .tab_number(*k)
                    .map_or_else(|| Jv::why("gone"), |n| Jv::u(n as u64)),
                "kernel",
            )),
            (CauseId::AppWork(id), _) if self.ctx.owner_class => {
                evidence.push(fact("coalition", "coalition", Jv::u(*id), "kernel"));
            }
            (_, Evidence::Uncovered { part }) => {
                evidence.push(fact("part", "part", Jv::s(part.word()), "derived"));
            }
            (_, Evidence::Startup { userspace_ms, wake }) => {
                evidence.push(fact(
                    "userspace_start",
                    "at_unix_ms",
                    Jv::u(nonneg(*userspace_ms)),
                    "kernel",
                ));
                if let Some(w) = wake {
                    evidence.extend(wake_facts(w));
                }
            }
            (_, Evidence::Wake(w)) => evidence.extend(wake_facts(w)),
            (_, Evidence::Pressure { level, swap_bps }) => {
                evidence.push(fact("pressure", "level", Jv::s(level.word()), "kernel"));
                evidence.push(fact(
                    "swap",
                    "swap_bps",
                    Jv::known_or(*swap_bps, "not_sampled"),
                    "kernel",
                ));
            }
            (_, Evidence::Throttled { level, low_power }) => {
                evidence.push(fact("throttle", "level", Jv::s(level.word()), "kernel"));
                evidence.push(fact(
                    "low_power",
                    "low_power",
                    low_power.map_or_else(|| Jv::why("unsupported"), Jv::Bool),
                    "kernel",
                ));
            }
            _ => {}
        }
        o.set("evidence", Jv::Arr(evidence));
        o
    }
}

/// `sys why`'s JSON for `q` over `s`, into the reply object `o` (no
/// sentence: a top-level `why` key would read as an unknown, §2.4).
pub fn why_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, o: &mut Jv) {
    let subject = match resolve(q, Some(s)) {
        Bare::Calm => {
            let w = Reading::new(ctx, s, WhySubject::Cpu);
            o.set("subject", Jv::obj([("scope", Jv::s("global"))]));
            o.set("calm", Jv::Bool(true));
            o.set("figure", w.figure_json(WhySubject::Cpu));
            o.set("you", w.you_json(WhySubject::Cpu));
            return;
        }
        Bare::Subject(subject) => subject,
    };
    let w = Reading::new(ctx, s, subject);
    let res = match subject {
        WhySubject::Cpu => "cpu",
        WhySubject::Mem => "mem",
    };
    o.set(
        "subject",
        Jv::obj([("scope", Jv::s("global")), ("res", Jv::s(res))]),
    );
    o.set("figure", w.figure_json(subject));
    let ranked = w.ranked(subject);
    match subject {
        WhySubject::Cpu => o.set("split_mc", w.split_json()),
        WhySubject::Mem => {
            o.set(
                "classes",
                s.mem.as_ref().map_or_else(
                    || Jv::why("unsupported"),
                    |m| classes_json(m, exact_partition(ctx)),
                ),
            );
            if w.a.facts.as_ref().is_some_and(|f| f.lane.on()) {
                let fp = Reading::sum(&ranked, WhySubject::Mem).map(nonneg);
                o.set(
                    "owners",
                    Jv::obj([
                        ("fp_bytes", Jv::known_or(fp, "not_sampled")),
                        ("coalitions", Jv::u(ranked.len() as u64)),
                        ("live_only", Jv::Bool(true)),
                    ]),
                );
            }
        }
    }
    o.set("you", w.you_json(subject));
    let rows = Rows::of(&w, &ranked, q.top);
    o.set(
        "who",
        Jv::Arr(rows.shown.iter().map(|r| w.who_json(r, subject)).collect()),
    );
    if !rows.more.is_empty() {
        o.set("more", Reading::sum_json(&rows.more, subject));
    }
    if !rows.withheld.is_empty() {
        o.set("withheld", Reading::sum_json(&rows.withheld, subject));
    }
    o.set(
        "causes",
        Jv::Arr(w.causes().map(|c| w.cause_json(c)).collect()),
    );
    o.set(
        "pass",
        Jv::obj([("trend", w.pass_json(subject)), ("src", Jv::s("kernel"))]),
    );
    o.set(
        "do",
        Jv::Arr(if w.a.nothing_needed {
            vec![Jv::obj([
                ("class", Jv::s("none")),
                ("may", Jv::s("agent")),
                ("destructive", Jv::Int(0)),
            ])]
        } else {
            Vec::new()
        }),
    );
    o.set(
        "cannot",
        Jv::Arr(
            w.a.cannot
                .iter()
                .map(|(what, why)| Jv::obj([("what", Jv::s(*what)), ("reason", Jv::why(why))]))
                .collect(),
        ),
    );
}

#[cfg(test)]
#[path = "why_tests.rs"]
mod why_tests;
