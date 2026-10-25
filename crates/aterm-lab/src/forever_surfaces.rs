// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window surface reads and scenario assertions for `lab forever` ([`super::judge`]).

use std::time::Duration;

use super::{
    CARRYING_ON, Chain, DEFENSE_HEAD, NEED, NEED_A, NEED_B, NEED_C, Opening, Scenario, Seen, Tab,
    Verdict, at, attention_text, needs_up, on, word,
};
use crate::door::Door;
use crate::stop;
use crate::util::{now_ms, one_line};
use crate::{Fail, wait_until};

/// Minimum busy duration (`30s`) before a task end can land.
pub const END_WORK_FLOOR_MS: i64 = 30_000;

/// Duration of the end-of-run idle measurement window.
pub const IDLE_MS: i64 = 20_000;

/// How long every tab's journal stood still before the idle window opens.
const IDLE_QUIET_BEFORE_MS: i64 = 10_000;

/// How far before a tab's next due act the idle window closes.
const IDLE_CLEAR_AFTER_MS: i64 = 15_000;

/// The hang detector of the search for an idle window.
const IDLE_SEARCH_MS: i64 = 8 * 60_000;

/// How many idle windows END tries for one with no act and no agent output.
const IDLE_TRIES: usize = 3;

/// The front tab's capture: one file, overwritten (a bare name is confined to the instance's images directory).
const FRONT_IMAGE: &str = "forever-front.png";

/// The hang detector of a look's or a press's effect (a wait for something that must happen).
const STEP_WAIT: Duration = Duration::from_secs(60);

/// Recorded window surface observations for a single tab ([`Window::note`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// Whether window surfaces were read (`false` under `--no-surfaces`).
    pub read: bool,
    /// Tab label across surfaces (the chain's name).
    pub label: String,
    /// Whether this tab is the front tab of the chains window.
    pub front_tab: bool,
    /// Observed `chrome` `autopilot` lines after `sid=`.
    pub chip: Vec<(i64, String)>,
    /// Observed `toolbar-tabs` strip entries (`index=<i> selected=<0|1> states=<a,b|->`).
    pub strip: Vec<(i64, String)>,
    /// Observed `driven by aterm (…)` hover tooltip lines.
    pub hover: Vec<(i64, String)>,
    /// Timestamps of all tooltip reads, including unchanged values.
    pub hover_reads: Vec<i64>,
    /// Observed `statusmenu` rows naming this tab (`<section>|<label>`).
    pub menu: Vec<(i64, String)>,
    /// Observed Settings ▸ Harness rows (`head=…`, `state=…`, `today=…`).
    pub page: Vec<(i64, String)>,
    /// Timestamps of all Harness-page reads.
    pub page_reads: Vec<i64>,
    /// End-of-run look steps recorded for this tab (L16, L17).
    pub looked: Vec<(i64, String)>,
    /// Front-sweep `autopilot` line read after capture.
    pub front: Option<(i64, String)>,
    /// End-of-run resting summary line (L7).
    pub rest: Option<String>,
    /// The chip's title as its hover reads it (`chrome`'s ` title=`, a development build's), split
    /// off [`Window::chip`]: `-` where the chip is not drawn or names none, `?` where the line
    /// prints no such field.
    pub title: Vec<(i64, String)>,
    /// The opening sweep's `autopilot` line, title and all: the tab brought in front and captured
    /// before its first prompt unless the manager's spawn already supplied its task
    /// ([`opening_sweep`]).
    pub opening: Option<(i64, String)>,
    /// Its row one level into its manager's submenu ([`worker_of`]): `-` where none names it.
    pub worker: Vec<(i64, String)>,
    /// A same-reply chain's looks in front past its hold ([`held::look_step`]): each its
    /// `autopilot` line, title and all (`-` where its label was not in the strip).
    pub held: Vec<(i64, String)>,
}

impl Window {
    /// Appends changed surface observations for `sid` from `g`, returning each change.
    pub fn note(&mut self, g: &Glance, sid: &str) -> Vec<(&'static str, String)> {
        self.read = true;
        let label = self.label.clone();
        let (chip, title) = chip_of(&g.chrome, sid).map_or_else(
            || ("-".to_string(), "-".to_string()),
            |line| {
                let (chip, title) = split_title(&line);
                (chip, title.unwrap_or_else(|| "?".into()))
            },
        );
        let worker = worker_of(&g.sub, &label);
        let tab = g.strip.iter().find(|s| s.label == label);
        let strip = tab.map_or_else(|| "-".to_string(), StripTab::words);
        let hover = tab
            .and_then(StripTab::driven)
            .unwrap_or_else(|| "-".to_string());
        let menu = menu_of(&g.menu, &label);
        let page = page_of(&g.page, &label).unwrap_or_else(|| "-".into());
        let hover_read = g.hover_read && tab.is_some();
        if hover_read {
            self.hover_reads.push(g.t);
        }
        if g.page_read {
            self.page_reads.push(g.t);
        }
        let mut said = Vec::new();
        for (what, list, v) in [
            ("chip", &mut self.chip, chip),
            ("strip", &mut self.strip, strip),
            ("hover", &mut self.hover, hover),
            ("menu", &mut self.menu, menu),
            ("page", &mut self.page, page),
            ("title", &mut self.title, title),
            ("worker", &mut self.worker, worker),
        ] {
            if (what == "hover" && !hover_read) || (what == "page" && !g.page_read) {
                continue;
            }
            if put(list, g.t, v.clone()) {
                said.push((what, v));
            }
        }
        said
    }
}

/// `v` added at `t` where it is not the last; whether it was.
fn put(list: &mut Vec<(i64, String)>, t: i64, v: String) -> bool {
    if list.last().is_some_and(|(_, last)| *last == v) {
        return false;
    }
    list.push((t, v));
    true
}

/// Whole-desk surface observations across all tabs and windows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Desk {
    /// Settings window ID and Harness page view ID (`inspect app/v1 tabs`).
    pub page_window: String,
    pub page_view: String,
    /// Chains window ID (`ls` `window=`).
    pub chains_window: String,
    /// Session ID of the chains window's front tab.
    pub front_sid: String,
    /// Observed `This Mac — …` menu rows.
    pub here: Vec<(i64, String)>,
    /// Observed `presence` lines with ages masked.
    pub presence: Vec<(i64, String)>,
    /// Observed `Sessions: …` page rows.
    pub sessions: Vec<(i64, String)>,
    /// Observed today summary lines under STATUS ([`today_of`]).
    pub today: Vec<(i64, String)>,
    /// Observed `Every session` rows under SESSIONS ([`every_of`]).
    pub every: Vec<(i64, String)>,
    /// End-of-run idle window observation, if reached.
    pub idle: Option<Idle>,
    /// Ordered end-of-run step log.
    pub steps: Vec<(i64, String)>,
}

impl Desk {
    /// Appends changed desk observations from `g`, returning each change.
    pub fn note(&mut self, g: &Glance) -> Vec<(&'static str, String)> {
        let here = g
            .menu
            .iter()
            .find(|r| r.section == "here")
            .map_or_else(|| "-".to_string(), |r| r.label.clone());
        let presence = g
            .chrome
            .lines()
            .find(|l| l.starts_with("presence "))
            .map_or_else(|| "-".to_string(), masked);
        let sessions = g
            .page
            .iter()
            .find(|(k, _)| k == "harness/status/row/sessions")
            .map_or_else(|| "-".to_string(), |(_, l)| l.clone());
        let today = today_of(&g.page).unwrap_or_else(|| "-".into());
        let every = every_of(&g.page).unwrap_or_else(|| "-".into());
        let mut said = Vec::new();
        for (what, list, v) in [
            ("here", &mut self.here, here),
            ("presence", &mut self.presence, presence),
            ("sessions", &mut self.sessions, sessions),
            ("today", &mut self.today, today),
            ("every", &mut self.every, every),
        ] {
            if put(list, g.t, v.clone()) {
                said.push((what, v));
            }
        }
        said
    }
}

/// End-of-run idle window metrics and activity log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Idle {
    pub from: i64,
    pub ms: i64,
    /// `metrics` snapshot captured after the idle window.
    pub metrics: String,
    /// `metrics percentiles` snapshot captured after the idle window.
    pub percentiles: String,
    /// Tab journal lines or acts recorded during the idle window.
    pub moved: Vec<String>,
    /// Duplicate journal lines rewritten during the idle window.
    pub relogged: Vec<String>,
    /// Tabs with unseen ends standing during the idle window.
    pub ends: Vec<String>,
    /// Tabs whose Land hold stood as it opened ([`holds_of`]).
    pub holds: Vec<String>,
    /// Whether the Harness page remained open during the idle window.
    pub page_open: bool,
}

/// Single snapshot of `chrome` and Harness page text across all tabs.
#[derive(Clone, Debug, Default)]
pub struct Glance {
    pub t: i64,
    pub chrome: String,
    pub strip: Vec<StripTab>,
    pub menu: Vec<MenuRow>,
    /// The rows one level into each submenu ([`sub_rows`]).
    pub sub: Vec<SubRow>,
    /// Parsed `text key=… label=…` rows from the Harness page.
    pub page: Vec<(String, String)>,
    /// Whether `chrome` included a complete tooltip table.
    pub hover_read: bool,
    /// Whether the Harness page text was read.
    pub page_read: bool,
}

impl Glance {
    /// Parses a [`Glance`] at timestamp `t` from `chrome` and `page` text.
    #[must_use]
    pub fn of(t: i64, chrome: &str, page: &str) -> Glance {
        let strip = strip_tabs(chrome);
        let hover_read = chrome
            .lines()
            .find(|l| l.starts_with("toolbar-tabs "))
            .is_some_and(|line| {
                word(line, "count")
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| {
                        n == strip.len() && tooltips(line).is_some_and(|tips| tips.len() == n)
                    })
            });
        let page = page_rows(page);
        let page_read = page.iter().any(|(key, _)| key.starts_with("harness/"));
        Glance {
            t,
            chrome: chrome.to_string(),
            strip,
            menu: menu_rows(chrome),
            sub: sub_rows(chrome),
            page,
            hover_read,
            page_read,
        }
    }
}

/// Parsed tab entry from the front window's `toolbar-tabs` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StripTab {
    pub index: usize,
    pub label: String,
    pub selected: bool,
    pub states: Vec<String>,
    /// Tab tooltip string, if present.
    pub tip: Option<String>,
}

impl StripTab {
    /// Formats `index=<i> selected=<0|1> states=<a,b|->`.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "index={} selected={} states={}",
            self.index,
            u8::from(self.selected),
            if self.states.is_empty() {
                "-".to_string()
            } else {
                self.states.join(",")
            }
        )
    }

    /// Extracts the `driven by aterm (…)` line from the tab's tooltip.
    #[must_use]
    pub fn driven(&self) -> Option<String> {
        self.tip.as_deref().and_then(|tip| {
            tip.lines()
                .find(|l| l.starts_with("driven by "))
                .map(str::to_string)
        })
    }
}

/// Parsed `statusmenu` row with its section header, label, enabled flag, and tip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuRow {
    pub section: String,
    pub label: String,
    pub at: String,
    pub indent: u8,
}

/// Extracts and unescapes a `key="…"` value from `line`.
#[must_use]
pub fn quoted(line: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let mut from = 0;
    while let Some(i) = line[from..].find(&pat) {
        let at = from + i;
        if at == 0 || line[..at].ends_with(' ') {
            return Some(unquote(&line[at + pat.len()..]).0);
        }
        from = at + pat.len();
    }
    None
}

/// A quoted string's body up to its closing `"`, read back, and what follows.
fn unquote(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return (out, &s[i + 1..]),
            '\\' => match it.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, '0')) => out.push('\0'),
                Some((j, 'u')) => {
                    let body = s[j + 1..].strip_prefix('{');
                    if let Some(body) = body
                        && let Some(end) = body.find('}')
                    {
                        if let Some(ch) = u32::from_str_radix(&body[..end], 16)
                            .ok()
                            .and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                        for _ in 0..end + 2 {
                            it.next();
                        }
                    }
                }
                Some((_, other)) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    (out, "")
}

/// A printed list of strings (`["a", "b"]`) at the head of `s`, and what follows.
fn strings(s: &str) -> Option<(Vec<String>, &str)> {
    let mut rest = s.strip_prefix('[')?;
    let mut v = Vec::new();
    loop {
        rest = rest.trim_start_matches([',', ' ']);
        if let Some(r) = rest.strip_prefix(']') {
            return Some((v, r));
        }
        let (item, r) = unquote(rest.strip_prefix('"')?);
        v.push(item);
        rest = r;
    }
}

/// A printed list of optional strings (`[None, Some("a")]`) at the head of `s`.
fn options(s: &str) -> Option<Vec<Option<String>>> {
    let mut rest = s.strip_prefix('[')?;
    let mut v = Vec::new();
    loop {
        rest = rest.trim_start_matches([',', ' ']);
        if rest.starts_with(']') {
            return Some(v);
        }
        if let Some(r) = rest.strip_prefix("None") {
            v.push(None);
            rest = r;
            continue;
        }
        let (item, r) = unquote(rest.strip_prefix("Some(\"")?);
        v.push(Some(item));
        rest = r.strip_prefix(')')?;
    }
}

fn tooltips(line: &str) -> Option<Vec<Option<String>>> {
    let after = |key: &str| line.find(key).map(|i| &line[i + key.len()..]);
    after(" pushed_tooltips=")
        .or_else(|| after(" tooltips="))
        .and_then(options)
}

/// A printed list of such lists (`[["a"], []]`) at the head of `s`.
fn lists(s: &str) -> Option<Vec<Vec<String>>> {
    let mut rest = s.strip_prefix('[')?;
    let mut v = Vec::new();
    loop {
        rest = rest.trim_start_matches([',', ' ']);
        if rest.starts_with(']') {
            return Some(v);
        }
        let (item, r) = strings(rest)?;
        v.push(item);
        rest = r;
    }
}

/// Parses the front window's tab strip entries from `chrome`.
#[must_use]
pub fn strip_tabs(chrome: &str) -> Vec<StripTab> {
    let Some(line) = chrome.lines().find(|l| l.starts_with("toolbar-tabs ")) else {
        return Vec::new();
    };
    let selected = word(line, "selected").and_then(|w| w.parse::<usize>().ok());
    let after = |key: &str| line.find(key).map(|i| &line[i + key.len()..]);
    let labels = after(" labels=")
        .and_then(strings)
        .map(|(v, _)| v)
        .unwrap_or_default();
    let states = after(" states=").and_then(lists).unwrap_or_default();
    let tips = tooltips(line).unwrap_or_default();
    labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| StripTab {
            index,
            label: team_unmarked(&label).to_string(),
            selected: selected == Some(index),
            states: states.get(index).cloned().unwrap_or_default(),
            tip: tips.get(index).cloned().flatten(),
        })
        .collect()
}

/// A strip entry's title without its team marks (`tab_bar::team_title`): `↳ ` before a worker's
/// title and ` ⇉N` after a manager's. The lab titles each tab with its chain's name and reads the
/// strip by that name, whoever spawned it or whatever it spawned.
#[must_use]
pub fn team_unmarked(shown: &str) -> &str {
    let title = shown.strip_prefix("\u{21b3} ").unwrap_or(shown);
    title
        .rsplit_once(" \u{21c9}")
        .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(title, |(t, _)| t)
}

/// The section an Info row heads, by its words.
fn section_of(label: &str) -> Option<&'static str> {
    [
        ("Needs you (", "needs"),
        ("Done for you (", "done"),
        ("This Mac", "here"),
        ("To look at (", "look"),
    ]
    .into_iter()
    .find(|(head, _)| label.starts_with(head))
    .map(|(_, s)| s)
}

/// Parses `statusmenu` rows from `chrome` under their section headers.
#[must_use]
pub fn menu_rows(chrome: &str) -> Vec<MenuRow> {
    let mut section = "-";
    let mut out = Vec::new();
    for line in chrome.lines() {
        let Some(rest) = line.strip_prefix("statusmenu ") else {
            continue;
        };
        if rest.starts_with("---") {
            section = "-";
            continue;
        }
        let Some(body) = rest.strip_prefix('"') else {
            continue;
        };
        let (label, tail) = unquote(body);
        if word(tail, "enabled").as_deref() == Some("no")
            && let Some(head) = section_of(&label)
        {
            section = head;
        }
        out.push(MenuRow {
            section: section.to_string(),
            label,
            at: word(tail, "at").unwrap_or_default(),
            indent: word(tail, "indent")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
        });
    }
    out
}

/// Whether menu row `row` names the tab labelled `tab`.
#[must_use]
pub fn row_names(row: &str, tab: &str) -> bool {
    let mut cs = row.chars();
    cs.next();
    cs.as_str()
        .strip_prefix(' ')
        .and_then(|r| r.strip_prefix(tab))
        .is_some_and(|r| r.starts_with(" \u{2014} ") || r.starts_with(" (worker of "))
}

/// Whether a menu row is its section's header (`Needs you (2)`).
fn is_head(r: &MenuRow) -> bool {
    section_of(&r.label) == Some(r.section.as_str())
}

/// Formats all menu rows in `menu` naming `tab` for [`Window::menu`].
#[must_use]
pub fn menu_of(menu: &[MenuRow], tab: &str) -> String {
    let named: Vec<(&MenuRow, &[MenuRow])> = menu
        .iter()
        .enumerate()
        .filter(|(_, r)| r.indent == 0 && row_names(&r.label, tab))
        .map(|(i, r)| {
            let under = &menu[i + 1..];
            let n = under.iter().take_while(|u| u.indent > 0).count();
            (r, &under[..n])
        })
        .collect();
    let mut lines = Vec::new();
    let mut sections: Vec<&str> = named.iter().map(|(r, _)| r.section.as_str()).collect();
    sections.dedup();
    for section in sections
        .into_iter()
        .filter(|s| matches!(*s, "needs" | "done"))
    {
        let Some(head) = menu.iter().find(|r| r.section == section && is_head(r)) else {
            continue;
        };
        let rows = menu
            .iter()
            .filter(|r| {
                r.section == section && r.indent == 0 && !is_head(r) && !r.label.starts_with("and ")
            })
            .count();
        lines.push(format!(
            "{section}-head|{} rows={rows}",
            masked(&head.label)
        ));
    }
    for (r, under) in named {
        lines.push(format!("{}|{}", r.section, masked(&r.label)));
        lines.extend(
            under
                .iter()
                .map(|u| format!("{}|{} indent={}", u.section, masked(&u.label), u.indent)),
        );
    }
    lines.join("\n")
}

/// One row one level into a submenu of `chrome`'s `statusmenu` lines (printed with two spaces
/// before it, which [`menu_rows`] passes over): the submenu's own row, its words and its tip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubRow {
    pub parent: String,
    pub label: String,
    pub tip: Option<String>,
}

/// Parses the rows one level into each submenu of `chrome`'s `statusmenu` lines.
#[must_use]
pub fn sub_rows(chrome: &str) -> Vec<SubRow> {
    let mut parent: Option<String> = None;
    let mut out = Vec::new();
    for line in chrome.lines() {
        let Some(rest) = line.strip_prefix("statusmenu ") else {
            continue;
        };
        if let Some(body) = rest.strip_prefix('"') {
            let (label, tail) = unquote(body);
            parent = tail
                .split_whitespace()
                .any(|w| w == "submenu")
                .then_some(label);
            continue;
        }
        if rest.starts_with("---") {
            parent = None;
            continue;
        }
        let (Some(body), Some(p)) = (rest.strip_prefix("  \""), &parent) else {
            continue;
        };
        let (label, tail) = unquote(body);
        out.push(SubRow {
            parent: p.clone(),
            label,
            tip: quoted(tail, "tip"),
        });
    }
    out
}

/// Whether submenu row `parent` is a manager's (`<label> — <n> worker(s): …`, `manager_line`).
fn is_manager(parent: &str) -> bool {
    parent
        .split_once(" \u{2014} ")
        .and_then(|(_, r)| r.split_once(' '))
        .is_some_and(|(n, w)| {
            n.parse::<usize>().is_ok() && (w.starts_with("worker: ") || w.starts_with("workers: "))
        })
}

/// Formats tab `tab`'s row one level into its manager's submenu for [`Window::worker`]
/// (`mgr|<the manager's row>`, `row|<its row>`, then `tip|<its tip>` where it has one), ages
/// masked; `-` where no manager's submenu names it.
#[must_use]
pub fn worker_of(rows: &[SubRow], tab: &str) -> String {
    let Some(r) = rows
        .iter()
        .find(|r| is_manager(&r.parent) && row_names(&r.label, tab))
    else {
        return "-".into();
    };
    let mut out = vec![
        format!("mgr|{}", masked(&r.parent)),
        format!("row|{}", masked(&r.label)),
    ];
    if let Some(tip) = &r.tip {
        out.push(format!("tip|{tip}"));
    }
    out.join("\n")
}

/// A [`Window::worker`] entry's `field|` line.
fn worker_field<'a>(entry: &'a str, field: &str) -> Option<&'a str> {
    entry
        .lines()
        .find_map(|l| l.strip_prefix(field)?.strip_prefix('|'))
}

/// Replaces relative age tokens (`6s`, `3m`, `2h`, `1d`) in `s` with `#`.
#[must_use]
pub fn masked(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let n = w.trim_end_matches(['s', 'm', 'h', 'd']);
            if !n.is_empty() && n.len() + 1 == w.len() && n.bytes().all(|b| b.is_ascii_digit()) {
                "#"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `(key, label)` pairs from `inspect app/v1 view <id> text` output.
#[must_use]
pub fn page_rows(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            if !l.starts_with("text ") {
                return None;
            }
            Some((quoted(l, "key")?, quoted(l, "label").unwrap_or_default()))
        })
        .collect()
}

/// Formats the Harness page rows (`head=`, `state=`, `today=`) for `tab`.
#[must_use]
pub fn page_of(rows: &[(String, String)], tab: &str) -> Option<String> {
    let head = format!("{tab}: ");
    let at = rows.iter().position(|(k, l)| {
        k.strip_prefix("harness/session/")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            && l.starts_with(&head)
    })?;
    let (key, label) = &rows[at];
    let own = format!("{key}/");
    let mut out = vec![format!("head={label}")];
    for field in ["state", "today"] {
        let k = format!("{key}/{field}");
        if let Some((_, l)) = rows.iter().find(|(r, _)| *r == k) {
            out.push(format!("{field}={l}"));
        }
    }
    let stated = out.len() > 1 && out[1].starts_with("state=");
    let more_below = rows[at + 1..]
        .iter()
        .any(|(k, _)| k.starts_with("harness/") && !k.starts_with(&own));
    (stated || more_below).then(|| out.join("\n"))
}

/// Extracts the STATUS today summary line from Harness page `rows`.
#[must_use]
pub fn today_of(rows: &[(String, String)]) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k == "harness/status/today")
        .map(|(_, l)| l.clone())
}

/// Extracts the SESSIONS `Every session` summary row from Harness page `rows`.
#[must_use]
pub fn every_of(rows: &[(String, String)]) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k == EVERY_KEY)
        .map(|(_, l)| l.clone())
}

/// The page's key of SESSIONS' Every session row.
const EVERY_KEY: &str = "harness/session/all";

/// Its Timer's disclosure (key and action alike).
const EVERY_TIMER: &str = "harness/session/all/timer";

/// Extracts the integer count preceding `word` in a summary `line`.
#[must_use]
pub fn count_in(line: &str, word: &str) -> usize {
    line.split([',', ':', '\u{b7}'])
        .find_map(|part| {
            let mut it = part.split_whitespace();
            let n = it.next()?.parse::<usize>().ok()?;
            (it.next() == Some(word)).then_some(n)
        })
        .unwrap_or(0)
}

/// Extracts tab `sid`'s `autopilot` line from `chrome` after `sid=`.
#[must_use]
pub fn chip_of(chrome: &str, sid: &str) -> Option<String> {
    let head = format!("autopilot sid={sid} ");
    chrome
        .lines()
        .find_map(|l| l.strip_prefix(head.as_str()))
        .map(str::to_string)
}

/// An `autopilot` line's ` title=` field split off it (a development build's, §12.5.4's 5d): the
/// line without it, and the title (`Some("-")` where it printed `title=-`, `None` where it printed
/// no such field: a release build's, or one before 5d).
#[must_use]
pub fn split_title(line: &str) -> (String, Option<String>) {
    let Some(at) = line.rfind(" title=") else {
        return (line.to_string(), None);
    };
    let value = &line[at + " title=".len()..];
    let title = match value.strip_prefix('"') {
        Some(body) => unquote(body).0,
        None => value.split_whitespace().next().unwrap_or("-").to_string(),
    };
    (line[..at].to_string(), Some(title))
}

/// Parsed fields of an `autopilot` chrome line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chip {
    pub face: String,
    pub drawn: Option<String>,
    pub why: Option<String>,
}

/// Parses a [`Chip`] from an `autopilot` line.
#[must_use]
pub fn chip(line: &str) -> Chip {
    Chip {
        face: word(line, "face").unwrap_or_else(|| "-".into()),
        drawn: quoted(line, "drawn"),
        why: word(line, "why"),
    }
}

/// Full-width drawn cell text expected for `face`.
#[must_use]
pub fn face_cells(face: &str) -> Option<&'static str> {
    Some(match face {
        "cruise" => "\u{221e}",
        "cruise-until" => "\u{221e} \u{2913}",
        "land" => "\u{2913} land",
        "landed" => "\u{2713} landed",
        "theirs" => "\u{2913} quiet",
        "off" => "\u{25cb} off",
        "master-off" | "agent-off" | "unhosted" => "\u{2298} off",
        _ => return None,
    })
}

/// Whether `c`'s drawn cells match its face.
#[must_use]
pub fn drawn_is_face(c: &Chip) -> bool {
    match (face_cells(&c.face), &c.drawn) {
        (Some(cells), Some(d)) if c.face == "cruise-until" => d.starts_with(cells),
        (Some(cells), Some(d)) => d == cells,
        _ => false,
    }
}

/// Whether a chip line is the footless one.
fn no_footer(c: &Chip) -> bool {
    c.face == "-" && c.drawn.is_none() && c.why.as_deref() == Some("no-footer")
}

/// Whether harness `program` leaves `c`'s footer bare: agy with its harness switch off, under
/// the master's or its agent's switch (`claude_footer::bare_while_off`); Autopilot Off keeps the
/// harness on, and its footer.
fn bare_under_switch(program: &str, c: &Chip) -> bool {
    program == "agy" && matches!(c.face.as_str(), "agent-off" | "master-off")
}

/// Whether `c` is `chrome`'s `drawn=- why=switch-only` line.
fn switch_only(c: &Chip) -> bool {
    c.drawn.is_none() && c.why.as_deref() == Some("switch-only")
}

/// The entries of `list` standing in `[from, until)`: the last one at or
/// before `from` (what stood when it opened), then each one inside.
fn standing(list: &[(i64, String)], from: i64, until: i64) -> Vec<&(i64, String)> {
    let first = list.iter().rposition(|(t, _)| *t <= from);
    list.iter()
        .enumerate()
        .filter(|(i, (t, _))| Some(*i) == first || (*t > from && *t < until))
        .map(|(_, e)| e)
        .collect()
}

/// Returns values from `series` observed within `(from, until)` using `reads` timestamps.
pub(super) fn observed<'a>(
    list: &'a [(i64, String)],
    reads: &[i64],
    from: i64,
    until: i64,
) -> Vec<(i64, &'a str)> {
    let times: std::collections::BTreeSet<_> = reads
        .iter()
        .copied()
        .chain(list.iter().map(|(t, _)| *t))
        .filter(|t| *t > from && *t < until)
        .collect();
    times
        .into_iter()
        .filter_map(|t| {
            list.iter()
                .rfind(|(at, _)| *at <= t)
                .map(|(_, value)| (t, value.as_str()))
        })
        .collect()
}

/// The entry of `list` standing last before `until` (the settled read of a span).
fn settled(list: &[(i64, String)], until: i64) -> Option<&(i64, String)> {
    list.iter().rfind(|(t, _)| *t < until)
}

/// The states of a [`Window::strip`] entry.
fn states(strip: &str) -> Vec<&str> {
    strip
        .split_whitespace()
        .find_map(|w| w.strip_prefix("states="))
        .filter(|s| *s != "-")
        .map(|s| s.split(',').collect())
        .unwrap_or_default()
}

/// The menu rows of a [`Window::menu`] entry in `section`.
fn rows_in<'a>(menu: &'a str, section: &str) -> Vec<&'a str> {
    menu.lines()
        .filter_map(|l| l.strip_prefix(section)?.strip_prefix('|'))
        .collect()
}

/// A [`Window::page`] entry's `field=` line.
fn page_field<'a>(page: &'a str, field: &str) -> Option<&'a str> {
    page.lines()
        .find_map(|l| l.strip_prefix(field)?.strip_prefix('='))
}

/// Context passed to a window surface [`Want`] judge.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub chain: Chain,
    pub program: &'a str,
    pub t0: i64,
}

/// Scope where a [`Want`] is evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Under {
    Scenario(Scenario),
    /// Evaluated on the first scenario of every chain in [`Chain::ALL`] except the listed ones.
    First(&'static [Chain]),
    /// Evaluated on the first scenario of each listed chain, whatever its mode.
    FirstOf(&'static [Chain]),
}

/// Window surface assertion bound to an [`Under`] scope and judge function.
#[derive(Clone, Copy)]
pub struct Want {
    pub under: Under,
    pub name: &'static str,
    pub judge: fn(&Seen, &Ctx<'_>) -> Verdict,
}

impl Want {
    /// Whether it is judged under scenario `s` of `chain`.
    fn of(&self, chain: Chain, s: Scenario) -> bool {
        match self.under {
            Under::Scenario(w) => w == s,
            Under::First(but) => {
                Chain::ALL.contains(&chain)
                    && !but.contains(&chain)
                    && chain.scenarios().first() == Some(&s)
            }
            Under::FirstOf(these) => {
                these.contains(&chain) && chain.scenarios().first() == Some(&s)
            }
        }
    }
}

/// Chains whose task ends have dedicated end-of-run scenarios (L16, L17).
pub const ENDING: [Chain; 2] = [Chain::LandJ, Chain::LandK];

/// Priority order of chains for selecting the chains window's front tab ([`front_of`]).
pub const FRONT: [Chain; 4] = [
    Chain::CruiseN,
    Chain::Off,
    Chain::Cruise,
    Chain::InstantCruise,
];

/// Index in `chains` of the chains window's front tab ([`FRONT`]).
#[must_use]
pub fn front_of(chains: &[Chain]) -> Option<usize> {
    FRONT
        .iter()
        .find_map(|f| chains.iter().position(|c| c == f))
}

/// Whether chain `c` stands HELD at the end of the run (§12.3's Land hold, `land_held=`): a
/// measured chain under Land that neither lands nor ends, so its hold stands through END.
#[must_use]
pub fn holds_at_end(c: Chain) -> bool {
    Chain::ALL.contains(&c) && !c.lands() && !ENDING.contains(&c) && c.autopilot() == "land"
}

/// The chains whose chip's title is read and judged ([`title_words`]): the measured pass's and
/// the coverage chains (`--mode cover`), each brought in front once before its first prompt
/// ([`opening_sweep`]); the worker once it is spawned.
pub const TITLED: [Chain; 25] = [
    Chain::LandA,
    Chain::LandB,
    Chain::LandC,
    Chain::LandD,
    Chain::LandE,
    Chain::LandF,
    Chain::LandG,
    Chain::LandH,
    Chain::LandI,
    Chain::Cruise,
    Chain::Off,
    Chain::InstantLand,
    Chain::InstantCruise,
    Chain::LandJ,
    Chain::LandK,
    Chain::CruiseN,
    Chain::LandQ,
    Chain::LandP,
    Chain::LandW,
    Chain::WroteC,
    Chain::LandT,
    Chain::Worker,
    Chain::CruiseO,
    Chain::SameLand,
    Chain::SameCruise,
];

/// Reason a run of `chains` skips reading window surfaces, or `None` if it reads them: it reads
/// them wherever one of its chains has a want in [`WANTS`] (a cover chain run alone among them),
/// so a want never passes as a skip for want of a read.
#[must_use]
pub fn unread_why(surfaces_on: bool, chains: &[Chain]) -> Option<&'static str> {
    if !surfaces_on {
        Some("--no-surfaces")
    } else if chains.iter().any(|c| {
        c.scenarios()
            .iter()
            .any(|s| WANTS.iter().any(|w| w.of(*c, *s)))
    }) {
        None
    } else {
        Some("no chain of this run has a window want")
    }
}

/// Whether the end-of-run surface sequence runs for `chains` (requires both [`ENDING`] chains).
#[must_use]
pub fn end_runs(chains: &[Chain]) -> bool {
    ENDING.iter().all(|c| chains.contains(c))
}

/// The chains a need is scripted for (L3, L6, N1–N4).
const NEEDING: [Chain; 3] = [Chain::LandA, Chain::LandD, Chain::CruiseN];

/// All window surface assertions evaluated across scenarios.
pub const WANTS: &[Want] = &[
    Want {
        under: Under::First(&[]),
        name: "every chip face its Autopilot's, a tab behind drawn=- why=not-drawn",
        judge: chip_faces,
    },
    Want {
        under: Under::FirstOf(&TITLED),
        name: "the chip's title says its mode's sentence from MODE_WORDS (a read hold's words in its place under Land)",
        judge: title_words,
    },
    Want {
        under: Under::First(&[Chain::LandA, Chain::LandJ, Chain::LandK]),
        name: "never an end (status end=-)",
        judge: never_ended,
    },
    Want {
        under: Under::First(&[Chain::LandA, Chain::LandJ, Chain::LandK]),
        name: "never a Done for you row or a •, never a need row where none is scripted",
        judge: never_a_row,
    },
    Want {
        under: Under::Scenario(Scenario::L1),
        name: "face=land while it lands",
        judge: l1_face,
    },
    Want {
        under: Under::Scenario(Scenario::L1),
        name: "no hold before land@v1; after it land_since= its second, land_nudges= the nudges since",
        judge: hold::begins,
    },
    Want {
        under: Under::Scenario(Scenario::L2),
        name: "land_held=open; the hover and state line name the words its nudge named, unquoted",
        judge: |s, c| hold::legs(s, c, hold::L2),
    },
    Want {
        under: Under::Scenario(Scenario::L3),
        name: "status needs=ask needs_n=1",
        judge: l3_count,
    },
    Want {
        under: Under::Scenario(Scenario::L3),
        name: "land_held=needs; the hover and state line say not landed yet, needs you",
        judge: |s, c| hold::legs(s, c, hold::L3),
    },
    Want {
        under: Under::Scenario(Scenario::L4),
        name: "land_held=repo; the hover and state line say 1 untracked file, no other count",
        judge: |s, c| hold::legs(s, c, hold::L4),
    },
    Want {
        under: Under::Scenario(Scenario::L6),
        name: "land_held=needs (the tall reply read whole); the hover and state line say the need",
        judge: |s, c| hold::legs(s, c, hold::L6),
    },
    Want {
        under: Under::Scenario(Scenario::L8),
        name: "land_held=repo; the hover and state line say 1 changed file, no other count",
        judge: |s, c| hold::legs(s, c, hold::L8),
    },
    Want {
        under: Under::Scenario(Scenario::L9),
        name: "land_held=repo; the hover and state line say 1 stash, no other count",
        judge: |s, c| hold::legs(s, c, hold::L9),
    },
    Want {
        under: Under::Scenario(Scenario::L10),
        name: "land_held=repo; the hover and state line say 1 unpushed commit and 1 commit not on the remote main",
        judge: |s, c| hold::legs(s, c, hold::L10),
    },
    Want {
        under: Under::Scenario(Scenario::L11),
        name: "land_held=repo; the hover and state line say 1 commit not on the remote main, no other count",
        judge: |s, c| hold::legs(s, c, hold::L11),
    },
    Want {
        under: Under::Scenario(Scenario::L3),
        name: "its row under Needs you (N) in the worker's words, N its rows; its tab's attention state",
        judge: l3_row,
    },
    Want {
        under: Under::Scenario(Scenario::L5),
        name: "under the floor: end=- throughout",
        judge: never_ended,
    },
    Want {
        under: Under::Scenario(Scenario::L5),
        name: "under the floor: no •, no Done for you row",
        judge: never_a_row,
    },
    Want {
        under: Under::Scenario(Scenario::L5),
        name: "face=landed, the page's state landed HH:MM",
        judge: l5_landed,
    },
    Want {
        under: Under::Scenario(Scenario::L5),
        name: "the hold gone at the landing: land_held=- land_nudges=- land_since=-, none said",
        judge: hold::gone,
    },
    Want {
        under: Under::Scenario(Scenario::L7),
        name: "its Done for you row ✓ … — landed; This Mac counts every landed tab",
        judge: l7_rows,
    },
    Want {
        under: Under::Scenario(Scenario::L7),
        name: "This Mac counts every held Land tab as landing, less those whose need it counts",
        judge: l7_here,
    },
    Want {
        under: Under::Scenario(Scenario::L7),
        name: "the page counts every landing: today N landed, Every session N landed beside the landing",
        judge: l7_page,
    },
    Want {
        under: Under::Scenario(Scenario::L7),
        name: "the page's Landed row: Landed: N today · M in 7 days, N every landing, M at least N",
        judge: l7_did,
    },
    Want {
        under: Under::Scenario(Scenario::P5),
        name: "land_held=defense; the hover and state line say no “Nothing left:” defense yet",
        judge: |s, c| hold::legs(s, c, hold::DEFENSE),
    },
    Want {
        under: Under::Scenario(Scenario::L12),
        name: "land_held=defense; the hover and state line say no “Nothing left:” defense yet",
        judge: |s, c| hold::legs(s, c, hold::DEFENSE),
    },
    Want {
        under: Under::Scenario(Scenario::L15),
        name: "its row under its manager: ● … — working before land@v1, ● … — landing while it holds, its tip the hold's",
        judge: hold::worker_row,
    },
    Want {
        under: Under::Scenario(Scenario::L16),
        name: "status end=<t> end_seen=0 once landed",
        judge: end_stands,
    },
    Want {
        under: Under::Scenario(Scenario::L16),
        name: "a hold from land@v1, gone at the landing",
        judge: hold::begins_then_gone,
    },
    Want {
        under: Under::Scenario(Scenario::L16),
        name: "its • and its Done for you row; after person look and tab: end_seen=1, both gone",
        judge: l16_look,
    },
    Want {
        under: Under::Scenario(Scenario::L17),
        name: "status end=<t> end_seen=0 once landed",
        judge: end_stands,
    },
    Want {
        under: Under::Scenario(Scenario::L17),
        name: "a hold from land@v1, gone at the landing",
        judge: hold::begins_then_gone,
    },
    Want {
        under: Under::Scenario(Scenario::L17),
        name: "its • and its Done for you row; after the press: in front, end_seen=1, the row gone",
        judge: l17_press,
    },
    Want {
        under: Under::Scenario(Scenario::N1),
        name: "status needs_n=2, meta's entry the first need's",
        judge: |s, c| n_status(s, c, 0),
    },
    Want {
        under: Under::Scenario(Scenario::N1),
        name: "drawn= equal to face=, the menu's need with no count (its other on a row of its own), the page's need (+1 more)",
        judge: |s, c| n_window(s, c, 0),
    },
    Want {
        under: Under::Scenario(Scenario::N1),
        name: "(b) on a row one level in under its row (indent=1)",
        judge: |s, c| n_further(s, c, 0),
    },
    Want {
        under: Under::Scenario(Scenario::N2),
        name: "status needs_n=-, meta's entry gone",
        judge: |s, c| n_status(s, c, 1),
    },
    Want {
        under: Under::Scenario(Scenario::N2),
        name: "drawn= equal to face=, no need row, no need on the page",
        judge: |s, c| n_window(s, c, 1),
    },
    Want {
        under: Under::Scenario(Scenario::N2),
        name: "no row one level in",
        judge: |s, c| n_further(s, c, 1),
    },
    Want {
        under: Under::Scenario(Scenario::N3),
        name: "status needs_n=1, meta's entry the first need's",
        judge: |s, c| n_status(s, c, 2),
    },
    Want {
        under: Under::Scenario(Scenario::N3),
        name: "drawn= equal to face=, the menu's and the page's need alone",
        judge: |s, c| n_window(s, c, 2),
    },
    Want {
        under: Under::Scenario(Scenario::N3),
        name: "no row one level in: its one need",
        judge: |s, c| n_further(s, c, 2),
    },
    Want {
        under: Under::Scenario(Scenario::N4),
        name: "status needs_n=2, meta's entry the first need's",
        judge: |s, c| n_status(s, c, 3),
    },
    Want {
        under: Under::Scenario(Scenario::N4),
        name: "drawn= equal to face=, the menu's need with no count (its other on a row of its own), the page's need (+1 more)",
        judge: |s, c| n_window(s, c, 3),
    },
    Want {
        under: Under::Scenario(Scenario::N4),
        name: "(c), never (b), on a row one level in under its row (indent=1)",
        judge: |s, c| n_further(s, c, 3),
    },
];

/// Folds applicable [`WANTS`] assertions into scenario `s`'s record verdict `v`.
#[must_use]
pub fn with_window(s: Scenario, seen: &Seen, cx: &Ctx<'_>, v: Verdict) -> Verdict {
    let mut skipped = Vec::new();
    let mut out = v;
    for w in WANTS.iter().filter(|w| w.of(cx.chain, s)) {
        if !seen.window.read {
            skipped.push(w.name);
            continue;
        }
        let got = (w.judge)(seen, cx);
        out = compose(out, &format!("window: {}", w.name), got);
    }
    if skipped.is_empty() {
        return out;
    }
    let skip = format!(
        "; window wants SKIPPED (--no-surfaces): {}",
        skipped.join("; ")
    );
    match out {
        Verdict::Pass(w) => Verdict::Pass(w + &skip),
        Verdict::Fail(w) => Verdict::Fail(w + &skip),
        Verdict::Unobserved(w) => Verdict::Unobserved(w + &skip),
    }
}

/// `a` and `b` (named `name`) as one: the worse word, both reasons.
fn compose(a: Verdict, name: &str, b: Verdict) -> Verdict {
    let why = format!("{}; {name}: {} ({})", a.why(), b.word(), b.why());
    match (&a, &b) {
        (Verdict::Fail(_), _) | (_, Verdict::Fail(_)) => Verdict::Fail(why),
        (Verdict::Unobserved(_), _) | (_, Verdict::Unobserved(_)) => Verdict::Unobserved(why),
        _ => Verdict::Pass(why),
    }
}

/// The faces a chain's chip may wear at `t`.
fn faces_at(chain: Chain, done: Option<i64>, t: i64) -> Vec<&'static str> {
    match chain.autopilot() {
        "cruise" => vec!["cruise"],
        "off" => vec!["off"],
        _ => {
            let mut v = match done {
                Some(d) if t >= d + 10_000 => vec!["landed"],
                Some(d) if t >= d - 10_000 => vec!["land", "landed"],
                _ => vec!["land"],
            };
            if chain == Chain::LandP {
                v.push("theirs");
            }
            v
        }
    }
}

/// Every chip face its autopilot's (every chain).
fn chip_faces(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let w = &seen.window;
    let lines: Vec<&(i64, String)> = w
        .chip
        .iter()
        .filter(|(t, l)| *t >= cx.t0 && l != "-")
        .collect();
    let Some(last) = lines.last() else {
        return Verdict::Unobserved(format!(
            "no autopilot line for {} in chrome after its first prompt",
            w.label
        ));
    };
    let done = seen.first_after("DONE ", cx.t0).map(|(t, _)| *t);
    let mut bad = Vec::new();
    for (t, l) in &lines {
        let c = chip(l);
        let allowed = faces_at(cx.chain, done, *t);
        if !allowed.contains(&c.face.as_str()) {
            bad.push(format!(
                "+{} face={} (wanted {allowed:?})",
                at(cx.t0, *t),
                c.face
            ));
        }
        if !w.front_tab && (c.drawn.is_some() || c.why.as_deref() != Some("not-drawn")) {
            bad.push(format!(
                "+{} a tab behind reads {} (wanted drawn=- why=not-drawn)",
                at(cx.t0, *t),
                one_line(l, 120)
            ));
        }
    }
    let said = format!(
        "{} line(s) read, the last +{} {}",
        lines.len(),
        at(cx.t0, last.0),
        one_line(&last.1, 140)
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// Never an end (every chain but L16's and L17's, and L5 under the floor).
fn never_ended(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let ends: Vec<&(i64, String)> = seen
        .status
        .iter()
        .filter(|(_, s)| word(s, "end").is_some_and(|e| e != "-" && e != "?"))
        .collect();
    let read = seen
        .status
        .iter()
        .filter(|(_, s)| word(s, "end").is_some_and(|e| e != "?"))
        .count();
    if let Some((t, s)) = ends.first() {
        return Verdict::Fail(format!(
            "an END at +{}: {} (end_seen={})",
            at(cx.t0, *t),
            word(s, "end").unwrap_or_default(),
            word(s, "end_seen").unwrap_or_default()
        ));
    }
    if read == 0 {
        return Verdict::Unobserved("status never printed end= (a release build?)".into());
    }
    Verdict::Pass(format!("end=- at all {read} status read(s)"))
}

/// Never a row (every chain but L16's and L17's, and L5 under the floor).
fn never_a_row(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let w = &seen.window;
    let mut bad = Vec::new();
    if !cx.chain.tasked() {
        if let Some((t, m)) = w.menu.iter().find(|(_, m)| !rows_in(m, "done").is_empty()) {
            bad.push(format!(
                "+{} a Done for you row: {}",
                at(cx.t0, *t),
                rows_in(m, "done").join(" / ")
            ));
        }
        if let Some((t, s)) = w.strip.iter().find(|(_, s)| states(s).contains(&"done")) {
            bad.push(format!("+{} its tab's • ({s})", at(cx.t0, *t)));
        }
    }
    if !NEEDING.contains(&cx.chain)
        && let Some((t, m)) = w.menu.iter().find(|(_, m)| !rows_in(m, "needs").is_empty())
    {
        bad.push(format!(
            "+{} a need row: {}",
            at(cx.t0, *t),
            rows_in(m, "needs").join(" / ")
        ));
    }
    let strips = w.strip.iter().filter(|(_, s)| s != "-").count();
    if bad.is_empty() && strips == 0 {
        return Verdict::Unobserved(format!(
            "its tab ({}) never stood in the front window's strip",
            w.label
        ));
    }
    let said = format!(
        "{} menu read(s), {strips} strip read(s){}",
        w.menu.len(),
        if cx.chain.tasked() {
            " (its task record's row and • are L7's)"
        } else {
            ""
        }
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// The chip lines standing in `[from, until)`, each read.
fn chips_in(seen: &Seen, from: i64, until: i64) -> Vec<(i64, Chip, String)> {
    standing(&seen.window.chip, from, until)
        .into_iter()
        .filter(|(_, l)| l != "-")
        .map(|(t, l)| (*t, chip(l), l.clone()))
        .collect()
}

/// L1: `face=land` while it lands.
fn l1_face(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((t1, _)) = seen
        .acts(aterm_agent::supervise::policy::turn_end::RULE_LAND)
        .first()
        .copied()
    else {
        return Verdict::Unobserved("no land act (L1's records)".into());
    };
    let until = seen.first_after("DONE ", *t1).map_or(i64::MAX, |(t, _)| *t);
    let lines = chips_in(seen, *t1, until);
    let want = |c: &Chip| c.face == "land";
    let faces: Vec<String> = lines.iter().map(|(_, c, _)| c.face.clone()).collect();
    match lines.iter().find(|(_, c, _)| want(c)) {
        Some((t, _, l)) => Verdict::Pass(format!(
            "+{} {} (faces read while landing: {faces:?})",
            at(cx.t0, *t),
            one_line(l, 120)
        )),
        None if lines.is_empty() => Verdict::Unobserved("no chip line read while it landed".into()),
        None => Verdict::Fail(format!("faces read while landing: {faces:?}, never land")),
    }
}

/// The span its need stood in.
fn need_span(seen: &Seen, t0: i64) -> Option<(i64, i64)> {
    let (tn, _) = seen.first_after("NEEDS ", t0)?;
    let until = seen
        .journal
        .iter()
        .find(|(t, l)| {
            *t > *tn
                && ((l.starts_with("CLEARED ") && l.contains(" needs ")) || l.starts_with("DONE "))
        })
        .map_or(seen.end_ms + 1, |(t, _)| *t);
    Some((*tn, until))
}

/// L3: `status needs=ask needs_n=1` while its one need stood.
fn l3_count(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((from, until)) = need_span(seen, cx.t0) else {
        return Verdict::Unobserved("no NEEDS line (L3's records)".into());
    };
    let read: Vec<&(i64, String)> = standing(&seen.status, from, until);
    let counts: Vec<String> = read
        .iter()
        .map(|(_, s)| {
            format!(
                "needs={} needs_n={}",
                word(s, "needs").unwrap_or_default(),
                word(s, "needs_n").unwrap_or_default()
            )
        })
        .collect();
    let ok = read.iter().any(|(_, s)| {
        word(s, "needs").as_deref() == Some("ask") && word(s, "needs_n").as_deref() == Some("1")
    });
    let wrong = read.iter().find(|(_, s)| {
        word(s, "needs").as_deref() == Some("ask") && word(s, "needs_n").is_some_and(|n| n != "1")
    });
    match (ok, wrong) {
        (_, Some((t, s))) => Verdict::Fail(format!(
            "+{} needs_n={} for one need",
            at(cx.t0, *t),
            word(s, "needs_n").unwrap_or_default()
        )),
        (true, None) => Verdict::Pass(format!("while it stood: {counts:?}")),
        (false, None) => {
            Verdict::Fail(format!("no needs=ask needs_n=1 while it stood: {counts:?}"))
        }
    }
}

/// A [`Window::menu`] entry's `<section>-head|` line.
fn head_of(menu: &str, section: &str) -> Option<(Option<usize>, usize)> {
    let head = menu
        .lines()
        .find_map(|l| l.strip_prefix(section)?.strip_prefix("-head|"))?;
    let rows = word(head, "rows")?.parse().ok()?;
    let n = head
        .split_once(" (")
        .and_then(|(_, r)| r.split_once(')'))
        .and_then(|(n, _)| n.parse().ok());
    Some((n, rows))
}

/// L3: its row under `Needs you (N)` in the worker's words (`◇ land-a — rotate the deploy key · #`, never `<program> needs you:`), the header's `N` the rows the menu lists under it (every tab's need, so 1 or more), and `attention` among its tab's states, while its need stood.
fn l3_row(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((from, until)) = need_span(seen, cx.t0) else {
        return Verdict::Unobserved("no NEEDS line (L3's records)".into());
    };
    let w = &seen.window;
    let want = format!(" {} \u{2014} {NEED} \u{b7} #", w.label);
    let menus = standing(&w.menu, from, until);
    let found = menus.iter().find_map(|(t, m)| {
        rows_in(m, "needs")
            .into_iter()
            .find(|r| r.contains(&want))
            .map(|r| (*t, r, m.as_str()))
    });
    let row = found.map(|(t, r, _)| (t, r));
    let strips = standing(&w.strip, from, until);
    let attention = strips
        .iter()
        .find(|(_, s)| states(s).contains(&"attention"));
    let mut bad = Vec::new();
    if let Some((t, _, m)) = found {
        match head_of(m, "needs") {
            Some((Some(n), rows)) if n >= 1 && n == rows => {}
            Some((n, rows)) => bad.push(format!(
                "+{} its header counts {}, the menu lists {rows} row(s) under it",
                at(cx.t0, t),
                n.map_or_else(|| "nothing".to_string(), |n| n.to_string())
            )),
            None => bad.push(format!(
                "+{} no Needs you (N) header over its row",
                at(cx.t0, t)
            )),
        }
    }
    match row {
        Some((_, r)) if r.contains("needs you:") => {
            bad.push(format!("its row reads the entry's words: {r}"));
        }
        Some(_) => {}
        None => bad.push(format!(
            "no Needs you row \"…{want}\" while it stood ({} menu read(s): {:?})",
            menus.len(),
            menus
                .iter()
                .map(|(_, m)| one_line(m, 120))
                .collect::<Vec<_>>()
        )),
    }
    if attention.is_none() {
        bad.push(format!(
            "no attention among its tab's states while it stood ({:?})",
            strips.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>()
        ));
    }
    let said = format!(
        "row {}; header {}; strip {}",
        row.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        ),
        found
            .and_then(|(_, _, m)| m.lines().find_map(|l| l.strip_prefix("needs-head|")))
            .unwrap_or("-"),
        attention.map_or_else(
            || "-".to_string(),
            |(t, s)| format!("+{} {s}", at(cx.t0, *t))
        )
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// L5: `face=landed` after the landing, and the page's state for it `landed <time>`.
fn l5_landed(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Verdict::Unobserved("no landing (L5's records)".into());
    };
    let w = &seen.window;
    let face = chips_in(seen, *d, i64::MAX)
        .into_iter()
        .find(|(_, c, _)| c.face == "landed");
    let page = standing(&w.page, *d, i64::MAX)
        .into_iter()
        .find(|(_, p)| page_field(p, "state").is_some_and(|s| s.starts_with("landed ")));
    let mut bad = Vec::new();
    if face.is_none() {
        bad.push(format!(
            "no face=landed after the landing ({:?})",
            chips_in(seen, *d, i64::MAX)
                .iter()
                .map(|(_, c, _)| c.face.clone())
                .collect::<Vec<_>>()
        ));
    }
    let mut unread = Vec::new();
    if standing(&w.page, *d, i64::MAX)
        .iter()
        .all(|(_, p)| p == "-")
    {
        unread.push(off_page(&w.label));
    } else if page.is_none() {
        bad.push(format!(
            "no page state `landed <time>` after it ({:?})",
            standing(&w.page, *d, i64::MAX)
                .iter()
                .map(|(_, p)| page_field(p, "state").unwrap_or("-").to_string())
                .collect::<Vec<_>>()
        ));
    }
    let said = format!(
        "chip {}; page {}",
        face.map_or_else(
            || "-".to_string(),
            |(t, _, l)| format!("+{} {}", at(cx.t0, t), one_line(&l, 100))
        ),
        page.map_or_else(
            || "-".to_string(),
            |(t, p)| format!(
                "+{} state={}",
                at(cx.t0, *t),
                page_field(p, "state").unwrap_or("-")
            )
        )
    );
    told(said, &bad, &unread)
}

/// L7: the worker's row under Done for you, `✓ land-e — landed #`, after its landing.
fn l7_rows(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Verdict::Unobserved("no landing (L7's records)".into());
    };
    let w = &seen.window;
    let want = format!("\u{2713} {} \u{2014} landed", w.label);
    let row = standing(&w.menu, *d, i64::MAX)
        .into_iter()
        .flat_map(|(t, m)| rows_in(m, "done").into_iter().map(move |r| (*t, r)))
        .find(|(_, r)| r.starts_with(&want));
    let mut bad = Vec::new();
    if row.is_none() {
        bad.push(format!(
            "no Done for you row \"{want} …\" after the landing"
        ));
    }
    let rest = w.rest.as_deref();
    let here = rest.and_then(|r| quoted(r, "here"));
    let tabs: Option<usize> = rest.and_then(|r| word(r, "landed_tabs")?.parse().ok());
    let counted = here.as_deref().and_then(|h| {
        h.split(" \u{b7} ")
            .find_map(|p| {
                p.trim_start_matches("This Mac \u{2014} ")
                    .strip_suffix(" landed")
            })
            .and_then(|n| n.parse::<usize>().ok())
    });
    let mut unread = Vec::new();
    match (counted, tabs) {
        (_, None) => unread.push(UNREST.to_string()),
        (Some(n), Some(k)) if n == k && k > 0 => {}
        (n, Some(k)) => bad.push(format!(
            "This Mac reads {} landed, {k} tab(s) said task=landed: {}",
            n.map_or_else(|| "no".to_string(), |n| n.to_string()),
            here.as_deref().unwrap_or("-")
        )),
    }
    let said = format!(
        "row {}; at rest {}",
        row.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        ),
        rest.unwrap_or("-")
    );
    told(said, &bad, &unread)
}

/// Whether a Land tab's `status` says its hold stands: `land_nudges=` a count, where `-` is no
/// hold (the line's `land_fields`) — Land's instruction typed, the tab not landed. `None` where
/// the line has no `land_nudges=` at all: a hold this read cannot see either way.
#[must_use]
pub fn held_at_rest(status: &str) -> Option<bool> {
    word(status, "land_nudges").map(|n| n != "-")
}

/// L7 widened (§14.6): at rest, This Mac counts every Land tab that holds as `landing`, less
/// those whose need it counts first (`need you`): `held_landing` less `held_needs` of the rest
/// read, exactly, and a build whose This Mac calls a held tab `resting` fails. It never passes on
/// `-` (§14.0 limit 7): UNOBSERVED where an open Land tab's `status` names no hold either way
/// (`held_unread`), where no Land tab holds at rest, and where This Mac counts a wait, which
/// counts first and which no `status` attributes to a tab.
fn l7_here(seen: &Seen, _cx: &Ctx<'_>) -> Verdict {
    let Some(rest) = seen.window.rest.as_deref() else {
        return Verdict::Unobserved(UNREST.into());
    };
    let n = |key: &str| word(rest, key).and_then(|v| v.parse::<usize>().ok());
    let (Some(held), Some(needs), Some(unread)) =
        (n("held_landing"), n("held_needs"), n("held_unread"))
    else {
        return Verdict::Unobserved(format!("the rest read lacks its counts: {rest}"));
    };
    let Some(counts) = quoted(rest, "here")
        .as_deref()
        .and_then(|h| h.strip_prefix("This Mac \u{2014} "))
        .map(str::to_string)
    else {
        return Verdict::Unobserved("the menu's This Mac row was never read at rest".into());
    };
    let here = format!("This Mac \u{2014} {counts}");
    if unread > 0 {
        return Verdict::Unobserved(format!(
            "{unread} open Land tab(s) whose status names no land_nudges=, so no hold of theirs was read; This Mac \"{here}\""
        ));
    }
    if held == 0 {
        return Verdict::Unobserved(format!(
            "no Land tab held at rest, so nothing for This Mac to count landing; This Mac \"{here}\""
        ));
    }
    let waits = count_in(&counts, "waiting");
    if waits > 0 {
        return Verdict::Unobserved(format!(
            "This Mac counts {waits} waiting, which count before landing and which no status names by tab; This Mac \"{here}\""
        ));
    }
    let want = held.saturating_sub(needs);
    let landing = count_in(&counts, "landing");
    let said = format!(
        "This Mac \"{here}\"; {held} Land tab(s) held at rest, {needs} of them with a need it counts first"
    );
    let bad = if landing == want {
        Vec::new()
    } else {
        vec![format!("{landing} landing, not {want}")]
    };
    told(said, &bad, &[])
}

/// What one title read says of its chain's mode ([`title_says`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TitleSays {
    /// `autopilot: <mode> — <its sentence>`, whole.
    Sentence,
    /// Under Land, a READ hold's words in the sentence's place (`not landed yet: …`, §12.4.12).
    Held,
    /// A face past Land's own (`land, landed`, `land, quiet`): no mode sentence to say. It holds
    /// the `face=` that says it (`landed`, `theirs`). A landed title says
    /// [`LANDED_WORDS`](aterm_agent::supervise::mode_words::LANDED_WORDS) whole (§14.9, 9a).
    Past(&'static str),
    /// Anything else, and why.
    Wrong(String),
}

/// What title `t` says of chain mode `mode` (`cruise`, `land`, `off`): its phrase, ` — `, and the
/// mode's sentence from [`MODE_WORDS`](aterm_agent::supervise::mode_words::MODE_WORDS) whole, then
/// the title's end or its next part (` · `). A landed title says
/// [`LANDED_WORDS`](aterm_agent::supervise::mode_words::LANDED_WORDS) so, never "remote main"
/// (§14.9, 9a).
#[must_use]
pub fn title_says(t: &str, mode: &str) -> TitleSays {
    use aterm_agent::supervise::Autopilot;
    use aterm_agent::supervise::mode_words::{LANDED_WORDS, mode_words};
    let Some(m) = Autopilot::ALL.into_iter().find(|m| m.word() == mode) else {
        return TitleSays::Wrong(format!("no mode {mode:?}"));
    };
    let sentence = mode_words(m).chip;
    if m == Autopilot::Land && t.starts_with("autopilot: land, landed") {
        let landed = t
            .split_once(" \u{2014} ")
            .and_then(|(_, rest)| rest.strip_prefix(LANDED_WORDS))
            .is_some_and(|after| after.is_empty() || after.starts_with(" \u{b7} "));
        if !landed || t.contains("remote main") {
            return TitleSays::Wrong(format!(
                "a landed title whose sentence is not {LANDED_WORDS:?}"
            ));
        }
        return TitleSays::Past("landed");
    }
    if m == Autopilot::Land && t.starts_with("autopilot: land, quiet") {
        return TitleSays::Past("theirs");
    }
    let head = format!("autopilot: {mode} \u{2014} ");
    let Some(rest) = t.strip_prefix(&head) else {
        return TitleSays::Wrong(format!("not {head:?} …"));
    };
    if rest
        .strip_prefix(sentence)
        .is_some_and(|after| after.is_empty() || after.starts_with(" \u{b7} "))
    {
        return TitleSays::Sentence;
    }
    if m == Autopilot::Land && rest.starts_with("not landed yet: ") && !rest.contains(sentence) {
        return TitleSays::Held;
    }
    TitleSays::Wrong(format!("its sentence is not {sentence:?}"))
}

/// Every chain's chip title says its mode's sentence (§12.4.1, 4d), read where its chip is
/// drawn: the opening sweep, the front tab's reads, END's sweep. The opening precedes the
/// first prompt except for a spawned worker, whose spawn supplies its task before the sweep.
/// A tab whose chip is not drawn names no title (agy's bare footer under its off switch).
fn title_words(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let w = &seen.window;
    let mode = cx.chain.autopilot();
    let spawned = cx.chain.opening() == Opening::Spawned;
    let mut reads: Vec<(i64, &str, Chip, Option<String>)> = Vec::new();
    for (place, line) in [("opening", &w.opening), ("front", &w.front)] {
        if let Some((t, l)) = line
            && l != "-"
        {
            let (rest, title) = split_title(l);
            reads.push((*t, place, chip(&rest), title));
        }
    }
    let mut times: Vec<i64> = w.title.iter().chain(&w.chip).map(|(t, _)| *t).collect();
    times.sort_unstable();
    times.dedup();
    for t in times {
        let (Some((_, l)), Some((_, title))) = (
            w.chip.iter().rfind(|(at, _)| *at <= t),
            w.title.iter().rfind(|(at, _)| *at <= t),
        ) else {
            continue;
        };
        let c = chip(l);
        if title == "-" && c.drawn.is_none() {
            continue;
        }
        let title = (title != "?").then(|| title.clone());
        reads.push((t, "drawn", c, title));
    }
    reads.sort_by_key(|(t, ..)| *t);
    if reads.is_empty() {
        return Verdict::Unobserved(format!(
            "{}'s chip was never read in front (no opening sweep, no END)",
            w.label
        ));
    }
    if reads.iter().all(|(.., title)| title.is_none()) {
        return Verdict::Unobserved(
            "chrome prints no title= (a release build, or one before 5d)".into(),
        );
    }
    let mut bad = Vec::new();
    let mut said = Vec::new();
    let mut observed = 0;
    let mut bare = 0;
    let mut undrawn_n = 0;
    for (t, place, c, title) in &reads {
        let at_t = format!("{place} +{}", at(cx.t0, *t));
        let spawned_opening = *place == "opening" && spawned;
        let title = title.as_deref().unwrap_or("?");
        if c.drawn.is_none() && c.why.as_deref() == Some("not-drawn") && title == "-" {
            undrawn_n += 1;
            continue;
        }
        if bare_under_switch(cx.program, c) && switch_only(c) {
            if title == "-" {
                bare += 1;
            } else {
                bad.push(format!("{at_t}: an undrawn chip names a title: {title:?}"));
            }
            continue;
        }
        if title == "-" || title == "?" {
            bad.push(format!("{at_t}: face={} drawn, yet title={title}", c.face));
            continue;
        }
        match title_says(title, mode) {
            TitleSays::Sentence => {
                observed += 1;
                said.push(format!("{at_t} {}", one_line(title, 160)));
            }
            TitleSays::Held | TitleSays::Past(_) if *place == "opening" && !spawned_opening => bad
                .push(format!(
                    "{at_t}: before its first prompt, not its mode's sentence: {}",
                    one_line(title, 200)
                )),
            TitleSays::Held if c.face == "land" => {
                observed += usize::from(spawned_opening);
                said.push(format!("{at_t} {}", one_line(title, 160)));
            }
            TitleSays::Past(face) if c.face == face => {
                observed += usize::from(spawned_opening);
                said.push(format!("{at_t} {}", one_line(title, 160)));
            }
            TitleSays::Held | TitleSays::Past(_) => bad.push(format!(
                "{at_t}: not the words of its chip's face={}: {}",
                c.face,
                one_line(title, 200)
            )),
            TitleSays::Wrong(why) => {
                bad.push(format!("{at_t}: {why}: {}", one_line(title, 200)));
            }
        }
    }
    let head = format!(
        "{} read(s) in front: {}{}{}",
        reads.len(),
        if said.is_empty() {
            "-".to_string()
        } else {
            said.join("; ")
        },
        if bare > 0 {
            format!(
                "; {bare} undrawn (agy's footer bare under its off switch), no title, as no chip is drawn to hover"
            )
        } else {
            String::new()
        },
        if undrawn_n > 0 {
            format!("; {undrawn_n} in front with its chip not drawn yet (why=not-drawn), no title")
        } else {
            String::new()
        }
    );
    let mut unread = Vec::new();
    if observed == 0 && bare == 0 && bad.is_empty() {
        unread.push(format!("no read said {mode}'s sentence"));
    }
    told(head, &bad, &unread)
}

/// The page's key of WHAT THE HARNESS DID's Landed row (4c).
const DID_LANDED: &str = "harness/did/row/landed";

/// The page's key of WHAT THE HARNESS DID's last count row (6a), which names each count whose week
/// is 0 in place of its row (`None in 7 days: declined · landed · upgraded`).
const DID_NONE: &str = "harness/did/row/none-in-7-days";

/// What the None row's label starts with, and the word it names a Landed row of none by.
const DID_NONE_HEAD: &str = "None in 7 days: ";
const DID_NONE_LANDED: &str = "landed";

/// Whether `label`, the None row's, names the Landed count: a whole ` · ` part, so `landed` never
/// matches inside another count's words.
fn none_names_landed(label: &str) -> bool {
    label
        .strip_prefix(DID_NONE_HEAD)
        .is_some_and(|names| names.split(" \u{b7} ").any(|n| n == DID_NONE_LANDED))
}

/// The page's HARNESS UPDATES card, which stands below WHAT THE HARNESS DID and its settings.
const UPDATES: &str = "harness/updates";

/// The most wheel steps END's read of WHAT THE HARNESS DID takes down the page.
const DID_WHEELS: usize = 60;

/// Extracts WHAT THE HARNESS DID's Landed row from Harness page `rows`, or, where no Landed row
/// stands, the None row in its place (6a), which names `landed` in a week with no landing. The
/// None row is the last of the counts, under the Landed row's place, so a None row that does not
/// name `landed` is a page that dropped the count: it is read, and [`l7_did`] fails it.
#[must_use]
pub fn did_landed_of(rows: &[(String, String)]) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k == DID_LANDED)
        .or_else(|| rows.iter().find(|(k, _)| k == DID_NONE))
        .map(|(_, l)| l.clone())
}

/// What a screen of the page, read from its top down, says of WHAT THE HARNESS DID's Landed row.
#[derive(Debug, PartialEq, Eq)]
pub enum DidStep {
    /// The row (or the None row in its place, [`did_landed_of`]), on this screen.
    Read(String),
    /// HARNESS UPDATES is on the screen and the row is not: the row passed no screen, since a step
    /// never moves more than the screen shows.
    Past,
    /// The row is further down.
    Wheel,
}

/// [`DidStep`] of one screen's `rows`.
#[must_use]
pub fn did_step(rows: &[(String, String)]) -> DidStep {
    if let Some(row) = did_landed_of(rows) {
        DidStep::Read(row)
    } else if rows.iter().any(|(k, _)| k.starts_with(UPDATES)) {
        DidStep::Past
    } else {
        DidStep::Wheel
    }
}

/// The view of window `window`'s front tab where that tab is Settings (`inspect app/v1 tabs`).
#[must_use]
pub fn front_page_of(tabs: &str, window: &str) -> Option<String> {
    let (mut here, mut front) = (false, false);
    for l in tabs.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("window ") {
            here = word(rest, "id").as_deref() == Some(window);
        } else if t.starts_with("tab ") {
            front = word(t, "active").as_deref() == Some("true");
        } else if here
            && front
            && t.starts_with("leaf ")
            && t.contains(" kind=app")
            && word(t, "app").as_deref() == Some("settings")
        {
            return word(t, "view");
        }
    }
    None
}

/// The middle of the page's list (`settings/list`'s `rect=` in `inspect … controls`), where the
/// wheel turns.
#[must_use]
pub fn list_middle(controls: &str) -> Option<(i64, i64)> {
    let line = controls
        .lines()
        .find(|l| quoted(l.trim_start(), "key").as_deref() == Some("settings/list"))?;
    let rect: Vec<f64> = word(line, "rect")?
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, w, h] = rect[..] else {
        return None;
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a point on the page, well inside i64"
    )]
    Some(((x + w / 2.0).round() as i64, (y + h / 2.0).round() as i64))
}

/// END's read of WHAT THE HARNESS DID's Landed row, which stands past the page's one screen and no
/// control owns, so no `act` brings it there. The read opens Settings ▸ Harness as a tab of the
/// chains' window, in front, and turns the wheel down its list from the top, as a person scrolls,
/// until the row is on the screen ([`did_step`]). It then closes that tab and brings back the tab
/// that was in front. The caller has turned the person's look off, so no tab it brings in front is
/// looked at. Returns the row (if read) and the step's words.
fn read_did(door: &Door<'_>, desk: &Desk) -> Result<(Option<String>, String), Fail> {
    let back = strip_tabs(&door.out(&["chrome"]))
        .into_iter()
        .find(|t| t.selected)
        .map(|t| t.index.to_string());
    let opened = door.out(&["open", "app", "settings", "/harness"]);
    let tabs = || door.out(&["inspect", "app/v1", "tabs"]);
    let mut view = None;
    let shown = wait_until(STEP_WAIT, || {
        view = front_page_of(&tabs(), &desk.chains_window);
        view.is_some()
    });
    let Some(view) = view.filter(|_| shown) else {
        return Ok((
            None,
            format!(
                "Settings opened in the chains' window ({}), but no Settings tab in its front \
                 within {} s",
                one_line(&opened, 80),
                STEP_WAIT.as_secs()
            ),
        ));
    };
    let middle = list_middle(&door.out(&["inspect", "app/v1", "view", &view, "controls"]));
    let (mut found, mut wheels, mut said) = (None, 0, "row further down");
    for _ in 0..=DID_WHEELS {
        let rows = page_rows(&door.out(&["inspect", "app/v1", "view", &view, "text"]));
        match did_step(&rows) {
            DidStep::Read(row) => {
                found = Some(row);
                said = "read";
                break;
            }
            DidStep::Past => {
                said = "HARNESS UPDATES reached and no Landed row passed";
                break;
            }
            DidStep::Wheel if wheels == DID_WHEELS => break,
            DidStep::Wheel => {}
        }
        let Some((x, y)) = middle else {
            said = "no list to turn the wheel over";
            break;
        };
        let xy = format!("xy={x},{y}");
        if !door
            .out(&["pointer", "wheel", "dy=3", &xy])
            .starts_with("OK")
        {
            said = "the wheel refused";
            break;
        }
        wheels += 1;
        stop::sleep(Duration::from_millis(300))?;
    }
    let _ = door.out(&["pointer", "leave"]);
    let closed = if front_page_of(&tabs(), &desk.chains_window).as_deref() == Some(&view) {
        door.out(&["invoke", "CloseTab"])
    } else {
        "not closed: the chains' window's front tab is no longer it".into()
    };
    let again = back.map_or_else(
        || "no tab was in front".to_string(),
        |i| format!("tab {i} again ({})", one_line(&door.out(&["tab", &i]), 40)),
    );
    let words = format!(
        "Settings opened in the chains' window ({}, view {view}), wheeled {wheels} step(s) down: \
         {}; closed ({}), {again}",
        one_line(&opened, 40),
        found.as_deref().unwrap_or(said),
        one_line(&closed, 40),
    );
    Ok((found, words))
}

/// L7 on the page's WHAT THE HARNESS DID (4c), at rest: `Landed: N today · M in 7 days`, N the
/// run's landings this local day (as the today line counts them), M at least N; a week with none
/// names `landed` in `None in 7 days` (6a), read as `0 today · 0 in 7 days`, and a None row that
/// does not name it, with no Landed row, fails.
fn l7_did(seen: &Seen, _cx: &Ctx<'_>) -> Verdict {
    let Some(rest) = seen.window.rest.as_deref() else {
        return Verdict::Unobserved(UNREST.into());
    };
    let Some(landings) = word(rest, "landings").and_then(|v| v.parse::<usize>().ok()) else {
        return Verdict::Unobserved(format!("the rest read lacks its counts: {rest}"));
    };
    let Some(did) = quoted(rest, "did").filter(|d| d != "-") else {
        return Verdict::Unobserved(
            "WHAT THE HARNESS DID's Landed row was never on the page's screen".into(),
        );
    };
    let counts = if none_names_landed(&did) {
        Some((0, 0))
    } else {
        did.strip_prefix("Landed: ").and_then(|r| {
            let (t, w) = r.split_once(" \u{b7} ")?;
            let t = t.strip_suffix(" today")?.parse::<usize>().ok()?;
            let w = w.strip_suffix(" in 7 days")?.parse::<usize>().ok()?;
            Some((t, w))
        })
    };
    let said = format!("the Landed row \"{did}\"; {landings} landing(s) in the run today");
    let bad = match counts {
        None if did.starts_with(DID_NONE_HEAD) => {
            vec!["`landed` is neither its own row nor named in `None in 7 days`".to_string()]
        }
        None => vec!["not `Landed: N today · M in 7 days`".to_string()],
        Some((t, _)) if t != landings => vec![format!("{t} today, not {landings}")],
        Some((t, w)) if w < t => vec![format!("{w} in 7 days, fewer than {t} today")],
        Some(_) => Vec::new(),
    };
    told(said, &bad, &[])
}

/// Why a want of the window at rest is unread: END never read it.
const UNREST: &str = "the window never read at rest (END not reached)";

/// L7 on the page (4c), at rest.
fn l7_page(seen: &Seen, _cx: &Ctx<'_>) -> Verdict {
    let Some(rest) = seen.window.rest.as_deref() else {
        return Verdict::Unobserved(UNREST.into());
    };
    let n = |key: &str| word(rest, key).and_then(|v| v.parse::<usize>().ok());
    let (Some(landed), Some(landing), Some(landings)) =
        (n("landed_tabs"), n("landing_tabs"), n("landings"))
    else {
        return Verdict::Unobserved(format!("the rest read lacks its counts: {rest}"));
    };
    let today = quoted(rest, "today").filter(|t| t != "-");
    let every = quoted(rest, "every").filter(|e| e != "-");
    let mut bad = Vec::new();
    let mut unread = Vec::new();
    match &today {
        None => unread.push("the page's today line was not on its screen".to_string()),
        Some(t) if landings == 0 => {
            if count_in(t, "landed") != 0 {
                bad.push(format!("today counts a landing the run never made: {t}"));
            }
        }
        Some(t) if !t.starts_with(&format!("{landings} landed today")) => bad.push(format!(
            "today reads \"{t}\", not \"{landings} landed today …\" ({landings} landing(s) in the run's journals)"
        )),
        Some(_) => {}
    }
    match &every {
        None => {
            unread.push("SESSIONS' Every session row was never brought on the page's screen".into())
        }
        Some(e) => {
            let (l, g) = (count_in(e, "landed"), count_in(e, "landing"));
            if l != landed || g != landing {
                bad.push(format!(
                    "Every session reads {l} landed, {g} landing; the tabs under Land: {landed} landed, {landing} landing ({e})"
                ));
            }
        }
    }
    let said = format!(
        "at rest: today {}; every {}; {landings} landing(s) in the run, {landed} tab(s) landed and {landing} landing under Land",
        today.as_deref().unwrap_or("-"),
        every.as_deref().unwrap_or("-")
    );
    told(said, &bad, &unread)
}

/// L16, L17: its end on `status` once landed: `end=<unix s> end_seen=0`.
fn end_stands(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Verdict::Unobserved("no landing (its records)".into());
    };
    let read = standing(&seen.status, *d, i64::MAX);
    let end = read.iter().find(|(_, s)| {
        word(s, "end").is_some_and(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()))
    });
    match end {
        Some((t, s)) if word(s, "end_seen").as_deref() == Some("0") => Verdict::Pass(format!(
            "+{} end={} end_seen=0 end_cheered={}",
            at(cx.t0, *t),
            word(s, "end").unwrap_or_default(),
            word(s, "end_cheered").unwrap_or_default()
        )),
        Some((t, s)) => Verdict::Fail(format!(
            "+{} end={} but end_seen={} before anyone looked",
            at(cx.t0, *t),
            word(s, "end").unwrap_or_default(),
            word(s, "end_seen").unwrap_or_default()
        )),
        None => Verdict::Fail(format!(
            "no end=<t> after the landing: {:?}",
            read.iter()
                .map(|(_, s)| word(s, "end").unwrap_or_default())
                .collect::<Vec<_>>()
        )),
    }
}

/// Its `•` and its Done for you row (`✓ <label> — landed #`) once landed, before the end's look.
fn end_marks(seen: &Seen, cx: &Ctx<'_>) -> Result<String, Verdict> {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Err(Verdict::Unobserved("no landing (its records)".into()));
    };
    let w = &seen.window;
    if let Some((t, l)) = standing(&w.strip, *d, *d + 1).first()
        && word(l, "selected").as_deref() == Some("1")
    {
        return Err(Verdict::Unobserved(format!(
            "in front when it landed (+{} {l}): the • is never on the front tab",
            at(cx.t0, *t)
        )));
    }
    let want = format!("\u{2713} {} \u{2014} landed", w.label);
    let dot = standing(&w.strip, *d, i64::MAX)
        .into_iter()
        .find(|(_, s)| states(s).contains(&"done"));
    let row = standing(&w.menu, *d, i64::MAX)
        .into_iter()
        .flat_map(|(t, m)| rows_in(m, "done").into_iter().map(move |r| (*t, r)))
        .find(|(_, r)| r.starts_with(&want));
    let said = format!(
        "• {}; row {}",
        dot.map_or_else(
            || "-".to_string(),
            |(t, s)| format!("+{} {s}", at(cx.t0, *t))
        ),
        row.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        )
    );
    if dot.is_none() || row.is_none() {
        return Err(Verdict::Fail(format!(
            "{said}: landed past the floor, yet no {} once landed",
            match (dot.is_none(), row.is_none()) {
                (true, true) => "• and no Done for you row",
                (true, false) => "•",
                _ => "Done for you row",
            }
        )));
    }
    Ok(said)
}

/// A [`Window::looked`] line of `step`.
fn step<'a>(w: &'a Window, name: &str) -> Option<&'a (i64, String)> {
    w.looked
        .iter()
        .find(|(_, l)| word(l, "step").as_deref() == Some(name))
}

/// Its `step=<name>` line of END left its end standing, unseen with its `•` and its Done for you
/// row (`end_seen=0 … done_row=yes`, `done` among its states where it is behind).
fn left_it(w: &Window, cx: &Ctx<'_>, name: &str, why: &str) -> Result<String, Verdict> {
    let Some((t, l)) = step(w, name) else {
        return Err(Verdict::Unobserved(format!(
            "END's {name} step never read it"
        )));
    };
    let behind = word(l, "selected").as_deref() == Some("0");
    let stands = word(l, "end_seen").as_deref() == Some("0")
        && word(l, "done_row").as_deref() == Some("yes")
        && (!behind || word(l, "states").is_some_and(|s| s.split(',').any(|x| x == "done")));
    if !stands {
        return Err(Verdict::Fail(format!(
            "+{} {why}, yet: {l} (wanted end_seen=0, its • and its row)",
            at(cx.t0, *t)
        )));
    }
    Ok(format!("{name} +{} {l}", at(cx.t0, *t)))
}

/// L16: its `•` and its row once landed.
fn l16_look(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let marks = match end_marks(seen, cx) {
        Ok(s) => s,
        Err(v) => return v,
    };
    let w = &seen.window;
    let mut before = vec![marks];
    for (name, why) in [
        ("sweep", "brought forward with no look"),
        ("look-on", "behind the tab the look began on"),
    ] {
        match left_it(w, cx, name, why) {
            Ok(s) => before.push(s),
            Err(v) => return v,
        }
    }
    let Some((t, l)) = step(w, "look") else {
        return Verdict::Unobserved(format!("{}; END's look never ran", before.join("; ")));
    };
    let said = format!("{}; look +{} {l}", before.join("; "), at(cx.t0, *t));
    let gone = word(l, "end_seen").as_deref() == Some("1")
        && word(l, "selected").as_deref() == Some("1")
        && !word(l, "states").is_some_and(|s| s.split(',').any(|x| x == "done"))
        && word(l, "done_row").as_deref() == Some("no");
    if gone {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!(
            "{said}: wanted selected=1 end_seen=1, no •, no row"
        ))
    }
}

/// L17: its `•` and its row once landed.
fn l17_press(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let marks = match end_marks(seen, cx) {
        Ok(s) => s,
        Err(v) => return v,
    };
    let w = &seen.window;
    let mut before = vec![marks];
    for (name, why) in [
        ("sweep", "brought forward with no look"),
        ("look-on", "behind the tab the look began on"),
        (
            "still",
            "behind land-j, which a tab switch under the look saw",
        ),
    ] {
        match left_it(w, cx, name, why) {
            Ok(s) => before.push(s),
            Err(v) => return v,
        }
    }
    let Some((t, l)) = step(w, "press") else {
        return Verdict::Unobserved(format!("{}; END's press never ran", before.join("; ")));
    };
    let said = format!("{}; press +{} {l}", before.join("; "), at(cx.t0, *t));
    let ok = word(l, "pressed").as_deref() == Some("OK")
        && word(l, "selected").as_deref() == Some("1")
        && word(l, "end_seen").as_deref() == Some("1")
        && word(l, "done_row").as_deref() == Some("no");
    if ok {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!(
            "{said}: wanted pressed=OK selected=1 end_seen=1 done_row=no"
        ))
    }
}

/// N1–N4: the words each step's reply's yield is told by (its last row, which `EVENT idle` quotes: its last need, or the defense) and the count `status` wants of its needs.
const N_STEPS: [(&str, usize); 4] = [(NEED_B, 2), (DEFENSE_HEAD, 0), (NEED_A, 1), (NEED_C, 2)];

/// Step `k` of N1–N4's span.
fn n_span(seen: &Seen, t0: i64, k: usize) -> Result<(i64, i64), Verdict> {
    let acts: Vec<i64> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t >= t0)
        .map(|(t, _)| *t)
        .collect();
    let asked = if k == 0 {
        t0
    } else {
        *acts.get(k - 1).ok_or_else(|| {
            Verdict::Unobserved(format!(
                "N{}'s act was never typed ({} act(s) in all)",
                k + 1,
                acts.len()
            ))
        })?
    };
    let until = acts.get(k).copied();
    let y = seen.yield_after(asked, until, Some(N_STEPS[k].0));
    let Some((te, _)) = y.event else {
        return Err(Verdict::Unobserved(format!(
            "N{}'s reply was never read (no EVENT idle with \"{}\" after +{})",
            k + 1,
            N_STEPS[k].0,
            at(t0, asked)
        )));
    };
    Ok((*te, until.unwrap_or(seen.end_ms + 1)))
}

/// The act that asked step `k` of N1–N4.
const N_ASKED: [Option<&str>; 4] = [
    None,
    Some(aterm_agent::supervise::policy::turn_end::RULE_CONTINUE),
    Some(aterm_agent::supervise::policy::turn_end::RULE_DONE_CHECK),
    Some(aterm_agent::supervise::policy::turn_end::RULE_CONTINUE),
];

/// Judges step `k` of the N1–N4 needs chain from `seen`.
#[must_use]
pub fn judge_needs_step(seen: &Seen, t0: i64, k: usize) -> Verdict {
    let (from, until) = match n_span(seen, t0, k) {
        Ok(s) => s,
        Err(v) => return v,
    };
    if let Some((d, l)) = seen.first_after("DONE ", t0) {
        return Verdict::Fail(format!(
            "ENDED under Cruise at +{}: {}",
            at(t0, *d),
            one_line(l, 90)
        ));
    }
    let acts: Vec<&(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t >= t0)
        .collect();
    let asked = k.checked_sub(1).and_then(|i| acts.get(i)).copied();
    let rule = asked.and_then(|(_, l)| super::act_rule(l));
    let mut bad = Vec::new();
    if rule != N_ASKED[k] {
        bad.push(format!("asked by {rule:?}, not {:?}", N_ASKED[k]));
    }
    let n = N_STEPS[k].1;
    let moved = seen.journal.iter().find(|(t, l)| {
        *t >= from - 2000
            && *t < until
            && if n == 0 {
                l.starts_with("CLEARED ") && l.contains(" needs ")
            } else {
                l.starts_with("NEEDS ") && word(l, "count") == Some(n.to_string())
            }
    });
    if moved.is_none() {
        bad.push(if n == 0 {
            "no CLEARED … needs in its span".to_string()
        } else {
            format!("no NEEDS … count={n} in its span")
        });
    }
    let said = format!(
        "{}its reply read at +{}; {}",
        asked.map_or_else(String::new, |(t, l)| format!(
            "{} at +{}, ",
            super::act_rule(l).unwrap_or("?"),
            at(t0, *t)
        )),
        at(t0, from),
        moved.map_or_else(
            || "the entry did not move".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), one_line(l, 120))
        )
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// N1–N4 on `status` and `meta`.
fn n_status(seen: &Seen, cx: &Ctx<'_>, k: usize) -> Verdict {
    let (from, until) = match n_span(seen, cx.t0, k) {
        Ok(s) => s,
        Err(v) => return v,
    };
    let want_n = N_STEPS[k].1;
    let Some((ts, s)) = settled(&seen.status, until).filter(|(t, _)| *t >= from - 5000) else {
        return Verdict::Unobserved(format!("no status read in N{}'s span", k + 1));
    };
    let n = word(s, "needs_n").unwrap_or_default();
    let needs = word(s, "needs").unwrap_or_default();
    let count_ok = if want_n == 0 {
        n == "-" && needs != "ask"
    } else {
        needs == "ask" && n == want_n.to_string()
    };
    let meta = settled(&seen.meta, until);
    let want_text = format!("{} needs you: {NEED_A}", cx.program);
    let meta_ok = meta.is_some_and(|(_, m)| {
        if want_n == 0 {
            !needs_up(m)
        } else {
            let t = attention_text(m);
            needs_up(m) && t.starts_with(&want_text) && t.trim_end().ends_with(CARRYING_ON)
        }
    });
    let said = format!(
        "settled +{} needs={needs} needs_n={n}; meta {}",
        at(cx.t0, *ts),
        meta.map_or_else(|| "-".to_string(), |(_, m)| one_line(m, 160))
    );
    if count_ok && meta_ok {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!(
            "{said}: wanted needs_n={} and {}",
            if want_n == 0 {
                "-".to_string()
            } else {
                want_n.to_string()
            },
            if want_n == 0 {
                "no needs entry".to_string()
            } else {
                format!("an entry \"{want_text}… {CARRYING_ON}\"")
            }
        ))
    }
}

/// N1–N4 on the window: every chip line standing in the span drawn as its face (the front tab), and, settled, its need row naming the first need with no count (every other need has a row of its own one level in, [`n_further`]), and its page state naming the first need with the others' count (` (+1 more)`), the need alone, or none.
fn n_window(seen: &Seen, cx: &Ctx<'_>, k: usize) -> Verdict {
    let (from, until) = match n_span(seen, cx.t0, k) {
        Ok(s) => s,
        Err(v) => return v,
    };
    let w = &seen.window;
    let mut bad = Vec::new();
    let chips = chips_in(seen, from, until);
    if !w.front_tab {
        return Verdict::Unobserved(format!(
            "{} was not the front tab: its chip is never drawn",
            w.label
        ));
    } else {
        for (t, c, l) in &chips {
            if !drawn_is_face(c) {
                bad.push(format!(
                    "+{} drawn≠face: {}",
                    at(cx.t0, *t),
                    one_line(l, 120)
                ));
            }
        }
    }
    if chips.is_empty() {
        bad.push("no chip line in its span".into());
    }
    let more = N_STEPS[k].1.saturating_sub(1);
    let want_row = match N_STEPS[k].1 {
        0 => None,
        _ => Some(format!(" {} \u{2014} {NEED_A} \u{b7} #", w.label)),
    };
    let menu = settled(&w.menu, until).map_or("", |(_, m)| m.as_str());
    let rows = rows_in(menu, "needs");
    match &want_row {
        Some(want) if !rows.iter().any(|r| r.contains(want.as_str())) => {
            bad.push(format!("no need row \"…{want}\" settled ({rows:?})"));
        }
        None if !rows.is_empty() => {
            bad.push(format!("a need row stands with none named: {rows:?}"))
        }
        _ => {}
    }
    let page = settled(&w.page, until).map_or("-", |(_, p)| p.as_str());
    let state = page_field(page, "state").unwrap_or("");
    let mut unread = Vec::new();
    if page == "-" {
        unread.push(off_page(&w.label));
    }
    let want_state = match N_STEPS[k].1 {
        0 => None,
        _ if more > 0 => Some(format!("needs you: {NEED_A} (+{more} more); carrying on")),
        _ => Some(format!("needs you: {NEED_A}; carrying on")),
    };
    match &want_state {
        _ if page == "-" => {}
        Some(want) if !state.starts_with(want.as_str()) => {
            bad.push(format!(
                "the page's state reads \"{state}\", not \"{want}…\""
            ));
        }
        None if state.starts_with("needs you:") => {
            bad.push(format!("the page still names a need: \"{state}\""));
        }
        _ => {}
    }
    let said = format!(
        "{} chip line(s) standing, the last {}; rows {rows:?}; state \"{state}\"",
        chips.len(),
        chips
            .last()
            .map_or_else(|| "-".to_string(), |(_, _, l)| one_line(l, 100))
    );
    told(said, &bad, &unread)
}

/// N1–N4's further needs, each on a row of its own one level in under the tab's row (`indent=1`).
const N_OTHERS: [&[&str]; 4] = [&[NEED_B], &[], &[], &[NEED_C]];

/// N1–N4's rows one level in.
fn n_further(seen: &Seen, cx: &Ctx<'_>, k: usize) -> Verdict {
    let until = match n_span(seen, cx.t0, k) {
        Ok((_, until)) => until,
        Err(v) => return v,
    };
    let menu = settled(&seen.window.menu, until);
    let under: Vec<&str> = menu
        .map(|(_, m)| rows_in(m, "needs"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r.strip_suffix(" indent=1"))
        .collect();
    let said = format!(
        "settled {}: rows one level in {under:?}",
        menu.map_or_else(|| "-".to_string(), |(t, _)| format!("+{}", at(cx.t0, *t)))
    );
    if under == N_OTHERS[k] {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: wanted {:?}", N_OTHERS[k]))
    }
}

/// Why a want of a chain's page row is unread.
fn off_page(label: &str) -> String {
    format!("{label}'s row was never on the page's screen")
}

/// A want's verdict from what it found wrong (`bad`: FAIL) and what it could not read (`unread`: UNOBSERVED), else PASS.
fn told(said: String, bad: &[String], unread: &[String]) -> Verdict {
    if !bad.is_empty() {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    } else if !unread.is_empty() {
        Verdict::Unobserved(format!("{said}: {}", unread.join("; ")))
    } else {
        Verdict::Pass(said)
    }
}

/// The value of `key=` in a `metrics` line, `-` where absent.
fn metric(line: &str, key: &str) -> String {
    word(line, key).unwrap_or_else(|| "-".into())
}

/// The owners of a `deadline_arms_by_owner=` (`owner:n/m,…`) or `deadline_fires_by_owner=` (`owner:n,…`) value, `none` and `-` naming none.
fn owners(v: &str) -> Vec<&str> {
    if matches!(v, "" | "-" | "none") {
        return Vec::new();
    }
    v.split(',')
        .filter_map(|p| p.split(':').next())
        .filter(|o| !o.is_empty())
        .collect()
}

/// A `wakes_by_variant=` value's count of `variant` (`Output:25,…`).
fn wakes_of(v: &str, variant: &str) -> u64 {
    v.split(',')
        .find_map(|p| p.strip_prefix(variant)?.strip_prefix(':')?.parse().ok())
        .unwrap_or(0)
}

/// Judges the end-of-run idle window and resting surfaces across `desk` and `tabs`.
#[must_use]
pub fn judge_end(desk: &Desk, tabs: &[(Chain, &Window)], program: &str) -> Verdict {
    let Some(idle) = &desk.idle else {
        return Verdict::Unobserved("END's idle window was never had".into());
    };
    let m = &idle.metrics;
    let p = &idle.percentiles;
    let timer = metric(m, "timer_wakes");
    let n_render = metric(p, "n_render");
    let arms = metric(m, "deadline_arms_by_owner");
    let fires = metric(m, "deadline_fires_by_owner");
    let wakes = metric(m, "wakes_by_variant");
    let output = wakes_of(&wakes, "Output");
    let heal_arms = count_of(&arms, WAKE_HEAL);
    let heal_fires = count_of(&fires, WAKE_HEAL);
    let heals = metric(m, "wake_heals");
    let watched = output > 0 && (heal_arms > 0 || heal_fires > 0);
    let allowed: &[&str] = if output == 0 {
        &[PRESENCE]
    } else {
        &[PRESENCE, SWEEP, WAKE_HEAL]
    };
    let mut bad = Vec::new();
    if watched {
        if heals != "0" {
            bad.push(format!(
                "the output-wake watch's recheck (armed {heal_arms}, fired {heal_fires}) beside wake_heals={heals}: a program's wake lost at rest, or unread"
            ));
        }
        if heal_arms.max(heal_fires) > output {
            bad.push(format!(
                "the output-wake watch armed {heal_arms} and fired {heal_fires} time(s) for {output} Output wake(s): more than once a write"
            ));
        }
    }
    if n_render != "0" {
        bad.push(format!("n_render={n_render}"));
    }
    let fired: Vec<&str> = owners(&fires)
        .into_iter()
        .filter(|o| output == 0 || (*o != SWEEP && *o != WAKE_HEAL))
        .collect();
    if !fired.is_empty() {
        bad.push(format!("deadline_fires_by_owner={fires}"));
    }
    let sweep_fires = count_of(&fires, SWEEP);
    if output == 0 && timer != "0" {
        bad.push(format!("timer_wakes={timer}"));
    } else if output > 0 && timer.parse::<u64>().ok() != Some(sweep_fires + heal_fires) {
        bad.push(format!(
            "timer_wakes={timer}, not the status sweep's {sweep_fires} fire(s){}",
            if heal_fires > 0 {
                format!(" and the output-wake watch's {heal_fires}")
            } else {
                String::new()
            }
        ));
    }
    if sweep_fires > SWEEP_FIRES_PER_WRITE * output && output > 0 {
        bad.push(format!(
            "the status sweep fired {sweep_fires} time(s) for {output} Output wake(s), past {SWEEP_FIRES_PER_WRITE} a write: a sweep that re-arms itself"
        ));
    }
    let armed: Vec<&str> = owners(&arms)
        .into_iter()
        .filter(|o| !allowed.contains(o))
        .collect();
    if !armed.is_empty() {
        bad.push(format!("armed beside {allowed:?}: {armed:?} ({arms})"));
    }
    let mut unread = Vec::new();
    if idle.ends.is_empty() {
        unread.push("no end stood unseen in it".to_string());
    }
    let holding = tabs.iter().any(|(c, _)| holds_at_end(*c));
    let unheld: Vec<&str> = tabs
        .iter()
        .filter(|(c, _)| holds_at_end(*c))
        .map(|(c, _)| c.name())
        .filter(|n| {
            !idle
                .holds
                .iter()
                .any(|h| h.split_once('(').is_some_and(|(name, _)| name == *n))
        })
        .collect();
    if idle.holds.is_empty() && holding {
        unread.push("no hold stood in it".to_string());
    } else if !unheld.is_empty() {
        bad.push(format!(
            "no Land hold stood on {unheld:?}, which neither land nor end"
        ));
    }
    if !idle.page_open {
        unread.push("the page was not open".to_string());
    }
    let said_idle = format!(
        "idle {} s: timer_wakes={timer} n_render={n_render} deadline_arms_by_owner={arms} deadline_fires_by_owner={fires} wakes_by_variant={wakes} deadline_owner={} (ends {:?}, holds {:?}{}, page {}){}",
        idle.ms / 1000,
        metric(m, "deadline_owner"),
        idle.ends,
        idle.holds,
        if holding {
            ""
        } else {
            " (none wanted: no chain of the run holds)"
        },
        if idle.page_open { "open" } else { "NOT open" },
        if idle.relogged.is_empty() {
            String::new()
        } else {
            format!(
                "; written again in it, each the same as its tab's line before: {}",
                idle.relogged.join("; ")
            )
        }
    );
    let mut sweep = Vec::new();
    let mut bare = Vec::new();
    let mut swept = 0;
    for (chain, w) in tabs {
        let Some((_, l)) = &w.front else {
            sweep.push(format!("{} never brought in front", chain.name()));
            continue;
        };
        swept += 1;
        if l == "-" {
            if *chain != Chain::Off {
                sweep.push(format!("{}: no autopilot line in front", chain.name()));
            }
            continue;
        }
        let c = chip(l);
        let (ok, want) = if bare_under_switch(program, &c) {
            let ok = switch_only(&c);
            if ok {
                bare.push(chain.name());
            }
            (
                ok,
                " (wanted drawn=- why=switch-only: agy's footer is bare under its off switch)",
            )
        } else {
            (drawn_is_face(&c), "")
        };
        if !ok {
            sweep.push(format!("{}: {}{want}", chain.name(), one_line(l, 120)));
        }
    }
    let said = format!(
        "{said_idle}; front sweep: {swept} of {} tab(s) in front{}",
        tabs.len(),
        if sweep.is_empty() {
            ", each drawn as its face".to_string()
        } else {
            format!(", NOT as their faces: {}", sweep.join("; "))
        }
    );
    let said = if bare.is_empty() {
        said
    } else {
        format!(
            "{said} ({}: drawn=- why=switch-only, the footer bare under the off switch by design)",
            bare.join(", ")
        )
    };
    let said = if output > 0 {
        format!(
            "{said}; the programs' own writes: {output} Output wake(s), answered by the status sweep alone ({sweep_fires} fire(s), {:.2} a write, at most {SWEEP_FIRES_PER_WRITE})",
            ratio(sweep_fires, output)
        )
    } else {
        said
    };
    let said = if watched {
        format!(
            "{said}; the output-wake watch's recheck armed {heal_arms} time(s) and fired {heal_fires} time(s), wake_heals={heals} output_wake_late={}: allowed at rest while it arms and fires at most once a write and heals nothing (a write's wake met in flight at a loop pass; a fire is that wake still unhandled when the recheck came due, the main thread held up, late, not lost)",
            metric(m, "output_wake_late")
        )
    } else {
        said
    };
    let at_rest = idle.moved.is_empty();
    if !bad.is_empty() && at_rest {
        return Verdict::Fail(format!("{said}: at rest, {}", bad.join(", ")));
    }
    if !sweep.is_empty() {
        return Verdict::Fail(said);
    }
    if !at_rest {
        return Verdict::Unobserved(format!(
            "{said}: a window that was not at rest ({}; moved: {})",
            if bad.is_empty() {
                "nothing woke beside it".to_string()
            } else {
                bad.join(", ")
            },
            idle.moved.join("; ")
        ));
    }
    if !unread.is_empty() {
        return Verdict::Unobserved(format!("{said}: {}", unread.join("; ")));
    }
    Verdict::Pass(said)
}

/// The deadline owner presence keeps (the pill's age, the claim's lapse): armed at rest, never fired there.
const PRESENCE: &str = "presence";

/// The deadline owner of the status sweep, which answers a program's output.
const SWEEP: &str = "session_status";

/// The deadline owner of the output-wake watch's recheck, which may arm and fire at most once a write at rest, healing nothing.
const WAKE_HEAL: &str = "wake_heal";

/// How many times the status sweep may fire for each `Output` wake at idle.
const SWEEP_FIRES_PER_WRITE: u64 = 3;

/// `n / d` for the report (0 for none).
#[expect(
    clippy::cast_precision_loss,
    reason = "a few hundred wakes, exact in an f64"
)]
fn ratio(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

/// The count `owner` has in a `deadline_fires_by_owner=` (`owner:n,…`) or `deadline_arms_by_owner=` (`owner:n/m,…`: its `n`) value.
fn count_of(v: &str, owner: &str) -> u64 {
    v.split(',')
        .find_map(|p| {
            let n = p.strip_prefix(owner)?.strip_prefix(':')?;
            n.split('/').next()?.parse().ok()
        })
        .unwrap_or(0)
}

/// Captures the front tab and reads `chrome` and the Harness page into a [`Glance`].
pub fn glance(door: &Door<'_>, desk: &Desk) -> Glance {
    if !desk.front_sid.is_empty() {
        let _ = on(door, &desk.front_sid, &["image", FRONT_IMAGE]);
    }
    let chrome = door.out(&["chrome"]);
    let page = if desk.page_view.is_empty() {
        String::new()
    } else {
        door.out(&["inspect", "app/v1", "view", &desk.page_view, "text"])
    };
    Glance::of(now_ms(), &chrome, &page)
}

/// The windows' ids in `inspect app/v1 tabs`.
fn windows_of(tabs: &str) -> (Vec<String>, Option<(String, String)>) {
    let mut windows = Vec::new();
    let mut page = None;
    let mut current = String::new();
    for l in tabs.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("window ") {
            current = word(rest, "id").unwrap_or_default();
            windows.push(current.clone());
        } else if t.starts_with("leaf ")
            && t.contains(" kind=app")
            && word(t, "app").as_deref() == Some("settings")
            && let Some(view) = word(t, "view")
        {
            page = Some((current.clone(), view));
        }
    }
    (windows, page)
}

/// The window an `invoke NewWindow` made.
fn new_window(
    before: &[String],
    within: Duration,
    mut tabs: impl FnMut() -> String,
) -> Option<String> {
    let mut made = None;
    let shown = wait_until(within, || {
        made = windows_of(&tabs())
            .0
            .into_iter()
            .find(|w| !before.contains(w));
        made.is_some()
    });
    made.filter(|_| shown)
}

/// Opens the Settings window (on `/harness`) and the chains window before spawning chain tabs.
///
/// # Errors
///
/// Returns an error if a control verb fails or a window does not appear in time.
pub fn lay_windows(door: &Door<'_>) -> Result<Desk, String> {
    let step = |args: &[&str]| -> Result<String, String> {
        let r = door.ctl(args)?;
        if r.code != 0 || !r.stdout.starts_with("OK") {
            return Err(format!(
                "`{}`: {}{}",
                args.join(" "),
                r.stdout.trim(),
                r.stderr.trim()
            ));
        }
        Ok(r.stdout)
    };
    let tabs = || door.out(&["inspect", "app/v1", "tabs"]);
    let (open, _) = windows_of(&tabs());
    step(&["invoke", "NewWindow"])?;
    new_window(&open, STEP_WAIT, tabs).ok_or_else(|| {
        format!(
            "the Settings window: no new window in `inspect app/v1 tabs` within {} s",
            STEP_WAIT.as_secs()
        )
    })?;
    step(&["open", "app", "settings", "/harness"])?;
    let (before, page) = windows_of(&tabs());
    let (page_window, page_view) =
        page.ok_or_else(|| "no Settings view in `inspect app/v1 tabs`".to_string())?;
    step(&["invoke", "NewWindow"])?;
    let chains_window = new_window(&before, STEP_WAIT, tabs).ok_or_else(|| {
        format!(
            "the chains' window: no new window in `inspect app/v1 tabs` within {} s",
            STEP_WAIT.as_secs()
        )
    })?;
    Ok(Desk {
        page_window,
        page_view,
        chains_window,
        ..Desk::default()
    })
}

/// Extracts `sid`'s `window=` token from `ls` output.
#[must_use]
pub fn window_of(ls: &str, sid: &str) -> Option<String> {
    ls.lines()
        .find(|l| l.split_whitespace().nth(2) == Some(sid))
        .and_then(|l| word(l, "window"))
}

/// Brings `tab` in front of the chains' window, captures it so its chip is drawn, and reads its
/// `autopilot` line (title and all); `None` where its label is not in the front window's strip.
fn in_front(door: &Door<'_>, tab: &Tab) -> Option<(String, String)> {
    let label = &tab.seen.window.label;
    let i = strip_tabs(&door.out(&["chrome"]))
        .into_iter()
        .find(|t| t.label == *label)?
        .index
        .to_string();
    let r = door.out(&["tab", &i]);
    let _ = on(door, &tab.sid, &["image", FRONT_IMAGE]);
    let line = chip_of(&door.out(&["chrome"]), &tab.sid).unwrap_or_else(|| "-".into());
    Some((format!("tab {i}: {}", r.trim()), line))
}

/// [`in_front`], and where its chip was not drawn ([`undrawn`]) captured again until it is, the
/// hang detector [`STEP_WAIT`].
fn in_front_drawn(door: &Door<'_>, tab: &Tab) -> Option<(String, String)> {
    in_front(door, tab).map(|(said, mut line)| {
        if undrawn(&line) {
            let _ = wait_until(STEP_WAIT, || {
                let _ = on(door, &tab.sid, &["image", FRONT_IMAGE]);
                line = chip_of(&door.out(&["chrome"]), &tab.sid).unwrap_or_else(|| "-".into());
                !undrawn(&line)
            });
        }
        (said, line)
    })
}

/// Whether an `autopilot` line says its chip was not drawn in the last frame (`why=not-drawn`),
/// where a footer (no `no-footer`) would draw it.
#[must_use]
pub fn undrawn(line: &str) -> bool {
    chip(line).why.as_deref() == Some("not-drawn")
}

/// THE OPENING SWEEP (5d): each of `tabs` brought in front once, before its first prompt unless
/// its manager's spawn already supplied its task, so its chip is drawn and `chrome` names the
/// title its hover reads ([`Window::opening`]); then the tab labelled `back` (the chains' window's
/// front tab) back in front, with no look. With
/// `until_drawn`, a tab whose chip the first capture did not draw ([`undrawn`]) is captured again
/// until it is, the hang detector [`STEP_WAIT`]: a spawned worker comes in front while its program
/// still starts on its task, before it draws its footer.
pub(super) fn opening_sweep(
    door: &Door<'_>,
    tabs: &mut [Tab],
    back: Option<&str>,
    until_drawn: bool,
) {
    for tab in tabs.iter_mut() {
        let label = tab.seen.window.label.clone();
        let read = if until_drawn {
            in_front_drawn(door, tab)
        } else {
            in_front(door, tab)
        };
        match read {
            Some((said, line)) => {
                println!("  opening   {label} ({said}): {}", one_line(&line, 400));
                tab.seen.window.opening = Some((now_ms(), line));
            }
            None => println!("  opening   {label}: not in the front window's strip"),
        }
    }
    if let Some(label) = back
        && let Some(i) = strip_tabs(&door.out(&["chrome"]))
            .into_iter()
            .find(|t| t.label == label)
    {
        let r = door.out(&["tab", &i.index.to_string()]);
        println!(
            "  opening   {label} back in front (tab {}: {})",
            i.index,
            r.trim()
        );
    }
}

/// Whether `chrome` shows tab `sid` ready to be read in front: its chip names its program (or it
/// draws no footer) and the front window's strip lists it as `label`.
#[must_use]
pub fn worker_shown(chrome: &str, sid: &str, label: &str) -> bool {
    chip_of(chrome, sid).is_some_and(|l| {
        let c = chip(&l);
        c.face != "-" || no_footer(&c)
    }) && strip_tabs(chrome).iter().any(|t| t.label == label)
}

/// A spawned worker's tab, titled with its chain's name (how its strip entry, its page row and its
/// row under its manager are matched to it), then read in front once its chip names its program
/// and the strip lists its title ([`worker_shown`], [`opening_sweep`]); the hang detector
/// [`STEP_WAIT`]. A headless window takes a new title into its strip when it draws, so each look
/// that finds the title missing captures the front tab (`front_sid`) first.
pub(super) fn open_worker(door: &Door<'_>, tab: &mut Tab, back: Option<&str>, front_sid: &str) {
    let name = tab.chain.name();
    let r = on(door, &tab.sid, &["meta", "set", "title", name]);
    tab.seen.window.label = name.to_string();
    let named = wait_until(STEP_WAIT, || {
        worker_shown(&door.out(&["chrome"]), &tab.sid, name) || {
            let _ = on(door, front_sid, &["image", FRONT_IMAGE]);
            worker_shown(&door.out(&["chrome"]), &tab.sid, name)
        }
    });
    println!(
        "  worker    {name} @{}: titled ({}){}",
        tab.sid,
        r.trim(),
        if named {
            ""
        } else {
            "; within 60 s its chip named no program or the strip listed no such title, so its \
             title is not read"
        }
    );
    if named {
        opening_sweep(door, std::slice::from_mut(tab), back, true);
    }
}

/// What one tab's end step read.
fn step_line(door: &Door<'_>, tab: &Tab, name: &str, pressed: Option<&str>) -> String {
    let s = on(door, &tab.sid, &["status"]);
    let g = Glance::of(now_ms(), &door.out(&["chrome"]), "");
    let label = &tab.seen.window.label;
    let strip = g.strip.iter().find(|t| t.label == *label);
    let want = format!("\u{2713} {label} \u{2014} ");
    let row = g
        .menu
        .iter()
        .any(|r| r.section == "done" && r.label.starts_with(&want));
    format!(
        "step={name} end={} end_seen={} selected={} states={} done_row={}{}",
        word(&s, "end").unwrap_or_else(|| "?".into()),
        word(&s, "end_seen").unwrap_or_else(|| "?".into()),
        strip.map_or("-", |t| if t.selected { "1" } else { "0" }),
        strip.map_or_else(
            || "-".to_string(),
            |t| if t.states.is_empty() {
                "-".to_string()
            } else {
                t.states.join(",")
            }
        ),
        if row { "yes" } else { "no" },
        pressed.map_or_else(String::new, |p| format!(" pressed={p}"))
    )
}

/// The soonest instant a tab's journal names its next act at (the last `WAITING … until=<stamp>`, `DONE` or `LEFT` after its last act, where a `WAITING`: its `until`, read as `now` where it passed within the last minute — the act is on its way) and the latest journal line's time.
fn due_and_quiet(tabs: &[Tab], now: i64) -> (Option<i64>, i64) {
    let mut due: Option<i64> = None;
    let mut last = 0;
    for tab in tabs {
        let j = &tab.seen.journal;
        if let Some((t, _)) = news(j).last() {
            last = last.max(*t);
        }
        let pending = j
            .iter()
            .rposition(|(_, l)| super::act_rule(l).is_some())
            .map_or(0, |i| i + 1);
        let Some((_, l)) = j[pending..].iter().rev().find(|(_, l)| {
            l.starts_with("WAITING ") || l.starts_with("DONE ") || l.starts_with("LEFT ")
        }) else {
            continue;
        };
        let until = l
            .split_whitespace()
            .find_map(|w| w.strip_prefix("until="))
            .and_then(unix_ms_of)
            .filter(|_| l.starts_with("WAITING "))
            .filter(|u| *u >= now - 60_000);
        if let Some(u) = until {
            let u = u.max(now);
            due = Some(due.map_or(u, |d| d.min(u)));
        }
    }
    (due, last)
}

/// A journal's lines less those the same as the line before them.
fn news(journal: &[(i64, String)]) -> impl Iterator<Item = &(i64, String)> {
    journal
        .iter()
        .enumerate()
        .filter(|(i, (_, l))| i.checked_sub(1).is_none_or(|p| journal[p].1 != *l))
        .map(|(_, e)| e)
}

/// Tab `name`'s journal lines inside `[from, from + ms]` (`<name> +t <line>`).
fn moves_in(name: &str, j: &[(i64, String)], from: i64, ms: i64) -> (Vec<String>, Vec<String>) {
    let mut moved = Vec::new();
    let mut relogged = Vec::new();
    for (i, (t, l)) in j.iter().enumerate() {
        if *t < from || *t > from + ms {
            continue;
        }
        let line = format!("{name} +{} {}", at(from, *t), one_line(l, 100));
        if i.checked_sub(1).is_some_and(|p| j[p].1 == *l) {
            relogged.push(line);
        } else {
            moved.push(line);
        }
    }
    (moved, relogged)
}

/// Parses an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) into unix milliseconds.
#[must_use]
pub fn unix_ms_of(stamp: &str) -> Option<i64> {
    let (y, m, d) = crate::reduce::iso_day(stamp)?;
    let n = |r: std::ops::Range<usize>| stamp.get(r)?.parse::<i64>().ok();
    let secs = crate::reduce::days_from_civil(y, m, d) * 86_400
        + n(11..13)? * 3600
        + n(14..16)? * 60
        + n(17..19)?;
    Some(secs * 1000)
}

/// `keys` of a `metrics` reply `line`, each as `key=value` (`-` where absent).
fn keep(line: &str, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| format!("{k}={}", metric(line, k)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What END keeps of the idle window's `metrics` reply ([`Idle::metrics`]).
#[must_use]
pub fn idle_metrics(reply: &str) -> String {
    keep(
        reply,
        &[
            "timer_wakes",
            "event_wakes",
            "deadline_owner",
            "deadline_arms_by_owner",
            "deadline_fires_by_owner",
            "wakes_by_variant",
            "wake_heals",
            "output_wake_late",
        ],
    )
}

/// What END keeps of the idle window's `metrics percentiles` reply ([`Idle::percentiles`]).
#[must_use]
pub fn idle_percentiles(reply: &str) -> String {
    keep(
        reply,
        &[
            "n_render",
            "render_p50_ms",
            "render_p95_ms",
            "render_p99_ms",
            "n_input",
        ],
    )
}

/// The end's idle window: once every tab's journal stood still [`IDLE_QUIET_BEFORE_MS`] and no tab's next act falls within it and [`IDLE_CLEAR_AFTER_MS`] after, `metrics reset`, [`IDLE_MS`] with no read, then `metrics` and `metrics percentiles`.
fn idle_window(
    run: &crate::run::Run,
    door: &Door<'_>,
    tabs: &mut [Tab],
    desk: &Desk,
) -> Result<Idle, Fail> {
    let started = now_ms();
    let mut last_idle = None;
    for _ in 0..IDLE_TRIES {
        loop {
            for tab in tabs.iter_mut() {
                super::look(run, door, tab);
            }
            let now = now_ms();
            let (due, last) = due_and_quiet(tabs, now);
            let quiet = now - last >= IDLE_QUIET_BEFORE_MS;
            let clear = due.is_none_or(|d| d - now >= IDLE_MS + IDLE_CLEAR_AFTER_MS);
            if quiet && clear {
                break;
            }
            if now - started > IDLE_SEARCH_MS {
                return Ok(last_idle.unwrap_or_else(|| Idle {
                    moved: vec![format!(
                        "no window clear of every tab's next act within {} min",
                        IDLE_SEARCH_MS / 60_000
                    )],
                    ..Idle::default()
                }));
            }
            stop::sleep(Duration::from_secs(2))?;
        }
        let ends: Vec<String> = tabs
            .iter()
            .filter(|t| {
                t.seen.status.last().is_some_and(|(_, s)| {
                    word(s, "end")
                        .is_some_and(|e| e.bytes().all(|b| b.is_ascii_digit()) && !e.is_empty())
                        && word(s, "end_seen").as_deref() == Some("0")
                })
            })
            .map(|t| t.chain.name().to_string())
            .collect();
        let holds = holds_of(tabs);
        let from = now_ms();
        let _ = door.out(&["metrics", "reset"]);
        stop::sleep(Duration::from_millis(
            u64::try_from(IDLE_MS).unwrap_or(20_000),
        ))?;
        let metrics = door.out(&["metrics"]);
        let percentiles = door.out(&["metrics", "percentiles"]);
        let ms = now_ms() - from;
        let page_open = !desk.page_view.is_empty()
            && door
                .out(&["inspect", "app/v1", "view", &desk.page_view, "text"])
                .contains("harness/status");
        let mut moved = Vec::new();
        let mut relogged = Vec::new();
        for tab in tabs.iter_mut() {
            super::look(run, door, tab);
            let (m, r) = moves_in(tab.chain.name(), &tab.seen.journal, from, ms);
            moved.extend(m);
            relogged.extend(r);
        }
        let idle = Idle {
            from,
            ms,
            metrics: idle_metrics(&metrics),
            percentiles: idle_percentiles(&percentiles),
            moved,
            relogged,
            ends,
            holds,
            page_open,
        };
        let rest = idle.moved.is_empty();
        last_idle = Some(idle);
        if rest {
            break;
        }
    }
    Ok(last_idle.unwrap_or_default())
}

/// The tabs whose last `status` read shows §12.3's Land hold standing (`land_since=` a second),
/// each as `<chain>(<land_held=>)`: END's holds, read off the hold itself, never off a pause.
#[must_use]
pub(super) fn holds_of(tabs: &[Tab]) -> Vec<String> {
    tabs.iter()
        .filter_map(|t| {
            let (_, s) = t.seen.status.last()?;
            let since = word(s, "land_since").filter(|v| v != "-" && v != "?")?;
            let held = word(s, "land_held").unwrap_or_else(|| "?".into());
            Some(format!("{}({held}, since {since})", t.chain.name()))
        })
        .collect()
}

/// Counts landings recorded in `journal` on timestamps matching `today`.
#[must_use]
pub fn landings_of(journal: &[(i64, String)], today: impl Fn(i64) -> bool) -> usize {
    let mut open = true;
    let mut n = 0;
    for (t, l) in journal {
        if super::act_rule(l).is_some_and(|r| r.starts_with("land")) {
            open = true;
        } else if l.starts_with("DONE ")
            && l.split_whitespace().any(|w| {
                w == format!(
                    "rule={}",
                    aterm_agent::supervise::policy::turn_end::RULE_LAND
                )
            })
        {
            if open && today(*t) {
                n += 1;
            }
            open = false;
        }
    }
    n
}

/// The local calendar day of unix ms `t` (the page's `today` is the local one).
fn local_day(t: i64) -> (i32, u32, u32) {
    let (y, m, d, ..) = crate::sys::local_time(t.div_euclid(1000));
    (y, m, d)
}

/// `s` quoted as `chrome` and the rest line quote it (`\\`, `\"`).
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Runs the end-of-run idle measurement, front sweep, look, and menu press sequence.
///
/// # Errors
///
/// Returns an error only if the run is stopped.
pub(super) fn end_steps(
    run: &crate::run::Run,
    door: &Door<'_>,
    tabs: &mut [Tab],
    desk: &mut Desk,
) -> Result<(), Fail> {
    let say = |desk: &mut Desk, what: String| {
        let t = now_ms();
        println!("  END       {what}");
        desk.steps.push((t, what));
    };
    let idle = idle_window(run, door, tabs, desk)?;
    say(
        desk,
        format!(
            "idle {} ms: {} {}{}",
            idle.ms,
            idle.metrics,
            idle.percentiles,
            if idle.moved.is_empty() {
                String::new()
            } else {
                format!(" moved: {}", idle.moved.join("; "))
            },
        ),
    );
    if !idle.relogged.is_empty() {
        say(
            desk,
            format!(
                "idle: {} line(s) written again, each the same as its tab's line before: {}",
                idle.relogged.len(),
                idle.relogged.join("; ")
            ),
        );
    }
    desk.idle = Some(idle);
    let g = Glance::of(now_ms(), &door.out(&["chrome"]), "");
    let here = g
        .menu
        .iter()
        .find(|r| r.section == "here")
        .map_or_else(String::new, |r| r.label.clone());
    let (mut landed_tabs, mut landing_tabs) = (0, 0);
    let (mut held_landing, mut held_needs, mut held_unread) = (0, 0, 0);
    for t in tabs.iter() {
        let s = on(door, &t.sid, &["status"]);
        if word(&s, "autopilot").as_deref() != Some("land") {
            continue;
        }
        match word(&s, "task").as_deref() {
            Some("landed") => landed_tabs += 1,
            Some("theirs") => {}
            _ => {
                landing_tabs += 1;
                match held_at_rest(&s) {
                    Some(true) => {
                        held_landing += 1;
                        if word(&s, "needs").is_some_and(|n| n != "-") {
                            held_needs += 1;
                        }
                    }
                    Some(false) => {}
                    None => held_unread += 1,
                }
            }
        }
    }
    let day = local_day(now_ms());
    let landings: usize = tabs
        .iter()
        .map(|t| landings_of(&t.seen.journal, |at| local_day(at) == day))
        .sum();
    let (today, every) = if desk.page_view.is_empty() {
        (None, None)
    } else {
        let view = desk.page_view.clone();
        let text = || door.out(&["inspect", "app/v1", "view", &view, "text"]);
        let rows = page_rows(&text());
        let every = match every_of(&rows) {
            Some(e) => Some(e),
            None => {
                let open = door.out(&["act", "app/v1", "view", &view, EVERY_TIMER, EVERY_TIMER]);
                let e = every_of(&page_rows(&text()));
                let shut = door.out(&["act", "app/v1", "view", &view, EVERY_TIMER, EVERY_TIMER]);
                say(
                    desk,
                    format!(
                        "the page's Every session row, its Timer pressed open ({}) and shut ({})",
                        one_line(&open, 80),
                        one_line(&shut, 80)
                    ),
                );
                e
            }
        };
        (today_of(&rows), every)
    };
    let _ = door.out(&["person", "look", "off"]);
    let did = if desk.page_view.is_empty() {
        None
    } else {
        let (did, words) = read_did(door, desk)?;
        say(desk, format!("the page's WHAT THE HARNESS DID: {words}"));
        did
    };
    let rest = format!(
        "here={} landed_tabs={landed_tabs} landing_tabs={landing_tabs} held_landing={held_landing} held_needs={held_needs} held_unread={held_unread} landings={landings} today={} every={} did={}",
        q(&here),
        q(today.as_deref().unwrap_or("-")),
        q(every.as_deref().unwrap_or("-")),
        q(did.as_deref().unwrap_or("-"))
    );
    say(desk, format!("at rest {rest}"));
    for tab in tabs.iter_mut() {
        tab.seen.window.rest = Some(rest.clone());
    }
    let index_of = |door: &Door<'_>, label: &str| {
        strip_tabs(&door.out(&["chrome"]))
            .into_iter()
            .find(|t| t.label == label)
            .map(|t| t.index.to_string())
    };
    for tab in tabs.iter_mut() {
        let label = tab.seen.window.label.clone();
        match in_front(door, tab) {
            Some((said, line)) => {
                say(desk, format!("front {label} ({said}): {line}"));
                tab.seen.window.front = Some((now_ms(), line));
            }
            None => say(desk, format!("{label}: not in the front window's strip")),
        }
    }
    for tab in tabs.iter_mut().filter(|t| ENDING.contains(&t.chain)) {
        let l = step_line(door, tab, "sweep", None);
        say(desk, format!("{}: {l}", tab.chain.name()));
        tab.seen.window.looked.push((now_ms(), l));
    }
    let Some(front) = tabs
        .iter()
        .find(|t| t.sid == desk.front_sid && !ENDING.contains(&t.chain))
        .map(|t| t.seen.window.label.clone())
    else {
        say(
            desk,
            "no front tab without an end to begin the look on: the look and the press not run"
                .into(),
        );
        return Ok(());
    };
    let Some(i) = index_of(door, &front) else {
        say(
            desk,
            format!("{front}: not in the front window's strip; the look and the press not run"),
        );
        return Ok(());
    };
    let r = door.out(&["tab", &i]);
    say(desk, format!("tab {i} ({front}): {}", r.trim()));
    let chains_window = desk.chains_window.clone();
    let looked = door.out(&["person", "look", &chains_window]);
    say(
        desk,
        format!("person look {chains_window}: {}", looked.trim()),
    );
    for tab in tabs.iter_mut().filter(|t| ENDING.contains(&t.chain)) {
        let l = step_line(door, tab, "look-on", None);
        say(desk, format!("{}: {l}", tab.chain.name()));
        tab.seen.window.looked.push((now_ms(), l));
    }
    if let Some(tab) = tabs.iter_mut().find(|t| t.chain == Chain::LandJ) {
        let label = tab.seen.window.label.clone();
        if let Some(i) = index_of(door, &label) {
            let r = door.out(&["tab", &i]);
            say(desk, format!("tab {i} ({label}): {}", r.trim()));
            let _ = wait_until(STEP_WAIT, || {
                word(&on(door, &tab.sid, &["status"]), "end_seen").as_deref() == Some("1")
            });
            let l = step_line(door, tab, "look", None);
            say(desk, format!("{label}: {l}"));
            tab.seen.window.looked.push((now_ms(), l));
        }
    }
    if let Some(tab) = tabs.iter_mut().find(|t| t.chain == Chain::LandK) {
        let l = step_line(door, tab, "still", None);
        say(desk, format!("{}: {l}", tab.chain.name()));
        tab.seen.window.looked.push((now_ms(), l));
    }
    if let Some(tab) = tabs.iter_mut().find(|t| t.chain == Chain::LandK) {
        let label = tab.seen.window.label.clone();
        let want = format!("\u{2713} {label} \u{2014} ");
        let row = menu_rows(&door.out(&["chrome"]))
            .into_iter()
            .find(|r| r.section == "done" && r.label.starts_with(&want));
        let pressed = match &row {
            Some(r) => {
                let out = door.ctl(&["person", "menubar", "press", &r.at])?;
                say(
                    desk,
                    format!(
                        "person menubar press {} (\"{}\"): {}{}",
                        r.at,
                        r.label,
                        out.stdout.trim(),
                        out.stderr.trim()
                    ),
                );
                if out.stdout.starts_with("OK pressed") {
                    "OK"
                } else {
                    "ERR"
                }
            }
            None => {
                say(desk, format!("{label}: no Done for you row to press"));
                "no-row"
            }
        };
        if pressed == "OK" {
            let _ = wait_until(STEP_WAIT, || {
                word(&on(door, &tab.sid, &["status"]), "end_seen").as_deref() == Some("1")
            });
        }
        let l = step_line(door, tab, "press", Some(pressed));
        say(desk, format!("{label}: {l}"));
        tab.seen.window.looked.push((now_ms(), l));
    }
    let off = door.out(&["person", "look", "off"]);
    say(desk, format!("person look off: {}", off.trim()));
    if let Some(i) = index_of(door, &front) {
        let r = door.out(&["tab", &i]);
        say(desk, format!("tab {i} ({front}): {}", r.trim()));
    }
    Ok(())
}

#[path = "forever_hold.rs"]
pub mod hold;

#[path = "forever_held.rs"]
pub mod held;

#[cfg(test)]
#[path = "forever_surfaces_tests.rs"]
mod tests;
