// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STOPPED WORK FIRST (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7, slice 7a): what `lab
//! forever` wants of the menu bar's Needs you and of the window's pressing session once a need
//! that stops an agent stands beside a `NEEDS YOU:` report its agent carries on past.
//!
//! Where the window is read and `cruise-n` runs, a plain shell tab stands beside the chains
//! ([`ASK_TAB`]), no agent in it. Once N4's report (two needs) is read, and [`ASK_AFTER_MS`] past
//! it, the lab writes that tab a stand-in direct ask (`meta set attention owner=lab kind=ask …`,
//! [`ask_step`]): newer than the report, and like it `◇` decide. It reads `chrome` until Needs
//! you lists both, keeps what it read in [`Window::stopped`](super::Window::stopped), and takes
//! the ask down again. N4 then wants ([`first`]): the ask's row above the report's, `presence
//! pressing=` naming the ask's tab, and the report row's tip in its program's own name
//! ([`report_words`]). Main before 7a lists the older report first, presses the front tab's
//! report, and gives its row no tip.

use std::time::Duration;

use aterm_agent::supervise::mode_words::report_words;

use super::super::{Chain, NEED_C, Seen, Tab, Verdict, at, on, word};
use super::{Ctx, is_head, menu_rows, quoted, row_names, unquote};
use crate::door::Door;
use crate::util::{now_ms, one_line};

/// The stand-in tab's label (its title pin): a plain shell beside the chains.
pub const ASK_TAB: &str = "lab-ask";

/// The stand-in ask's attention owner: another owner's direct request, which stops nobody's work
/// but asks the person, as an approval box or a question does.
pub const ASK_OWNER: &str = "lab";

/// The stand-in ask's words, one word an argument.
pub const ASK_WORDS: [&str; 4] = ["approve", "the", "stand-in", "request"];

/// How long past N4's report the ask is written: more than a whole second, so it is younger than
/// the report on every surface that counts ages in seconds.
pub const ASK_AFTER_MS: i64 = 2_000;

/// The hang detector of a menu read the ask must make (a wait for something that must happen).
const STEP_WAIT: Duration = Duration::from_secs(60);

/// The want [`first`] composes under.
pub const WANT: &str = "stopped first (§14.7): a direct ask newer than its report lists first under Needs you, `presence pressing=` names the ask's tab, and the report row's tip says its program carries on meanwhile";

/// When N4's report was read: the `EVENT idle` quoting its last need after the chain's third act.
fn n4_read(seen: &Seen) -> Option<i64> {
    let acts = seen.all_acts();
    let third = acts.get(2)?.0;
    seen.yield_after(third, None, Some(NEED_C))
        .event
        .map(|(t, _)| *t)
}

/// What one `chrome` read shows of the ask and the report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// The Needs you rows, in the menu's order: each top-level row under the header, less the
    /// header and its `and k more` row.
    pub rows: Vec<String>,
    /// The ask's row among [`Read::rows`].
    pub ask: Option<usize>,
    /// The report's row among [`Read::rows`].
    pub report: Option<usize>,
    /// The report row's tip (`statusmenu … tip="…"`).
    pub tip: Option<String>,
    /// `presence pressing=` (`@<sid>` or `-`).
    pub pressing: String,
}

impl Read {
    /// What `chrome` shows of the ask's tab and of the tab labelled `report`.
    #[must_use]
    pub fn of(chrome: &str, report: &str) -> Read {
        let rows: Vec<String> = menu_rows(chrome)
            .into_iter()
            .filter(|r| {
                r.section == "needs" && r.indent == 0 && !is_head(r) && !r.label.starts_with("and ")
            })
            .map(|r| r.label)
            .collect();
        let tip = chrome
            .lines()
            .filter_map(|l| l.strip_prefix("statusmenu \""))
            .map(unquote)
            .find(|(label, _)| row_names(label, report))
            .and_then(|(_, tail)| quoted(tail, "tip"));
        let pressing = chrome
            .lines()
            .find(|l| l.starts_with("presence "))
            .and_then(|l| word(l, "pressing"))
            .unwrap_or_else(|| "-".to_string());
        Read {
            ask: rows.iter().position(|r| row_names(r, ASK_TAB)),
            report: rows.iter().position(|r| row_names(r, report)),
            rows,
            tip,
            pressing,
        }
    }

    /// Both rows are listed.
    #[must_use]
    pub const fn listed(&self) -> bool {
        self.ask.is_some() && self.report.is_some()
    }

    /// The line [`ask_step`] keeps: `ask=<i|-> report=<i|-> pressing=<ask|report|@sid|-> tip="…"
    /// listed=<yes|no>` (`tip=-` where the row has none), `ask_sid` and `report_sid` the two tabs'.
    #[must_use]
    pub fn line(&self, ask_sid: &str, report_sid: &str) -> String {
        let pressing = match self.pressing.strip_prefix('@') {
            Some(sid) if sid == ask_sid => "ask".to_string(),
            Some(sid) if sid == report_sid => "report".to_string(),
            _ => self.pressing.clone(),
        };
        let index = |i: Option<usize>| i.map_or_else(|| "-".to_string(), |i| i.to_string());
        format!(
            "ask={} report={} pressing={pressing} tip={} listed={} rows={:?}",
            index(self.ask),
            index(self.report),
            self.tip
                .as_ref()
                .map_or_else(|| "-".to_string(), |t| format!("{t:?}")),
            if self.listed() { "yes" } else { "no" },
            self.rows
        )
    }
}

/// THE ASK: once `cruise-n`'s N4 report has been read [`ASK_AFTER_MS`] ago, the stand-in ask
/// written to `ask_sid`'s tab, `chrome` read until Needs you lists it and the report (the hang
/// detector [`STEP_WAIT`]), that read kept ([`Read::line`], with `cleared=` whether the ask's row
/// was gone again within the detector once taken down), and the ask taken down. Runs once.
pub(in crate::forever) fn ask_step(door: &Door<'_>, tab: &mut Tab, ask_sid: Option<&str>) {
    let Some(ask_sid) = ask_sid else {
        return;
    };
    if tab.chain != Chain::CruiseN || tab.seen.window.stopped.is_some() {
        return;
    }
    let Some(report_at) = n4_read(&tab.seen) else {
        return;
    };
    if now_ms() - report_at < ASK_AFTER_MS {
        return;
    }
    let owner = format!("owner={ASK_OWNER}");
    let mut set = vec!["meta", "set", "attention", owner.as_str(), "kind=ask"];
    set.extend_from_slice(&ASK_WORDS);
    let said = on(door, ask_sid, &set);
    let asked = now_ms();
    let label = tab.seen.window.label.clone();
    let mut read = Read::default();
    let _ = crate::wait_until(STEP_WAIT, || {
        read = Read::of(&door.out(&["chrome"]), &label);
        read.listed()
    });
    let t = now_ms();
    let unset = on(
        door,
        ask_sid,
        &["meta", "unset", "attention", owner.as_str()],
    );
    let cleared = crate::wait_until(STEP_WAIT, || {
        Read::of(&door.out(&["chrome"]), &label).ask.is_none()
    });
    let line = format!(
        "{} cleared={}",
        read.line(ask_sid, &tab.sid),
        if cleared { "yes" } else { "no" }
    );
    println!(
        "  [{}] +{} stopped ask on {ASK_TAB} @{ask_sid} at +{} ({}), read {} ms later: {}; unset: {}",
        tab.chain.name(),
        at(tab.t0, t),
        at(tab.t0, asked),
        one_line(&said, 60),
        t - asked,
        one_line(&line, 400),
        one_line(&unset, 60)
    );
    tab.seen.window.stopped = Some((t, line));
}

/// The product name of the program word `program` (`claude` → `Claude Code`).
fn product(program: &str) -> Option<&'static str> {
    aterm_phase::Program::ALL
        .into_iter()
        .find(|p| p.name() == program)
        .map(aterm_phase::Program::product)
}

/// N4's stopped-first want: the ask's row above the report's under Needs you, `presence
/// pressing=` the ask's tab, the report row's tip [`report_words`] in its program's name, and the
/// ask's row gone once it was taken down.
#[must_use]
pub fn first(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((t, l)) = &seen.window.stopped else {
        return Verdict::Unobserved(format!(
            "its ask step never ran: no N4 report read {} s before its watch ended",
            ASK_AFTER_MS / 1000
        ));
    };
    let said = format!("+{} {}", at(cx.t0, *t), one_line(l, 400));
    let Some(product) = product(cx.program) else {
        return Verdict::Unobserved(format!("{said}: no product name for {}", cx.program));
    };
    let mut bad = Vec::new();
    if word(l, "listed").as_deref() != Some("yes") {
        bad.push(format!(
            "Needs you did not list the ask and the report within {} s",
            STEP_WAIT.as_secs()
        ));
    }
    let index = |key: &str| word(l, key).and_then(|v| v.parse::<usize>().ok());
    match (index("ask"), index("report")) {
        (Some(a), Some(r)) if a < r => {}
        (Some(a), Some(r)) => bad.push(format!(
            "the ask is row {a}, after its older report's row {r}"
        )),
        _ => {}
    }
    match word(l, "pressing").as_deref() {
        Some("ask") => {}
        other => bad.push(format!(
            "presence pressing={}, not the ask's tab",
            other.unwrap_or("?")
        )),
    }
    let want_tip = report_words(product, false);
    let tip = quoted(l, "tip");
    if tip.as_deref() != Some(want_tip.as_str()) {
        bad.push(format!(
            "the report row's tip is {}, not {want_tip:?}",
            tip.map_or_else(|| "none".to_string(), |t| format!("{t:?}"))
        ));
    }
    if word(l, "cleared").as_deref() != Some("yes") {
        bad.push("the ask's row stood on after it was taken down".to_string());
    }
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

#[cfg(test)]
#[path = "forever_stopped_tests.rs"]
mod tests;
