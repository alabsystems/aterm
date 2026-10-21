// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S READS OF A FOREVER RUN (slice 5c,
//! `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.4): what a person at the
//! window would see of each chain, read at every look the run takes, and the
//! wants each scenario holds it to beside its records ([`super::judge`]).
//!
//! The run lays its windows out before the chains' tabs ([`lay_windows`]): a
//! window of its own for Settings ▸ Harness (`invoke NewWindow`, then `open
//! app settings /harness` there; `inspect app/v1 tabs` names the page's view),
//! then the chains' own (`invoke NewWindow` again), where every tab spawns,
//! `cruise-n` first (the page lists its rows on its first screen). A headless
//! instance's frontmost window is the newest, so the chains' window is the one
//! `person look` and the `toolbar-tabs` line read. Once every tab is open, one
//! is brought in front with `tab` ([`front_of`]: `cruise-n`, else the Off
//! control, never a tab whose end END looks at): the one a capture draws.
//! Each tab is titled with its chain's name (`meta set title`): the
//! label the strip, the menu and the page name it by. At each look ([`glance`]):
//!
//! * `@<front> image forever-front.png`: the front tab captured, so its chip's
//!   `drawn=` cells are the frame's (a headless window composes none until a
//!   capture);
//! * `chrome`: each tab's `autopilot` line ([`Window::chip`]), its entry of the
//!   front window's `toolbar-tabs` ([`Window::strip`]: its `states`, `done` its
//!   `•`) and its tooltip's who-drives line ([`Window::hover`]: `driven by aterm
//!   (land: not landed yet, …)`), the `statusmenu` rows naming it ([`Window::menu`], by section, each
//!   section's header with it), and the desk's `This Mac` and `presence` lines
//!   ([`Desk`]);
//! * `inspect app/v1 view <page> text`: its Settings ▸ Harness rows
//!   ([`Window::page`]: its head, state and today rows), and the desk's
//!   STATUS rows (`Sessions:`, the today line) and, where it is on the
//!   page's screen, SESSIONS' Every session row ([`Desk`]);
//! * `status`'s `end= end_seen= end_cheered=`, `needs_n=` and Land's hold's
//!   `land_held= land_nudges= land_since=` ride the record's status words
//!   ([`super::status_words`]; the hold's wants are [`hold`]'s).
//!
//! Each read is kept with its time where it changed, ages masked (`#`), so the
//! report is a list of changes. After every chain is over, THE END
//! ([`end_steps`]): a 20 s window with no read, clear of every tab's next act
//! (its ends and pauses standing, the page open: [`Idle`]); This Mac and the
//! page's counts at rest ([`Window::rest`]); then each chain's tab brought in
//! front once with `person look off` (`tab <index>`, a capture, `chrome`:
//! [`Window::front`]), which sees no end; then the look: the front tab (no
//! end) brought back, `person look <the chains' window>` there (land-j's and
//! land-k's ends still stand: `step=look-on`), `tab <land-j>` under the look
//! (L16's end seen by the tab switch: `step=look`; land-k's still stands:
//! `step=still`), `person menubar press` on `land-k`'s Done for you row
//! (L17's: the press brings it in front and the look sees it:
//! `step=press`), then `person look off` and the front tab back
//! ([`Window::looked`]).
//!
//! The wants ([`WANTS`], a table a later slice extends) are read by token, row
//! and field, never by prose; each is a named skip under `--no-surfaces`
//! ([`Window::read`] false). Gemini CLI draws no chip: every line of its wants
//! `face=- drawn=- why=no-footer`.

use std::time::Duration;

use super::{
    CARRYING_ON, Chain, DEFENSE_HEAD, NEED, NEED_A, NEED_B, NEED_C, Scenario, Seen, Tab, Verdict,
    at, attention_text, needs_up, on, word,
};
use crate::door::Door;
use crate::stop;
use crate::util::{now_ms, one_line};
use crate::{Fail, wait_until};

/// aterm-gui's `END_WORK_MS` (`harness_task.rs`): the least busy time since
/// its task opened at which a landing marks the tab's end (its `•`, its Done
/// for you row, `status end=`). Under it a landing ends nothing (L5).
pub const END_WORK_FLOOR_MS: i64 = 30_000;

/// THE END's idle window: no read, no act due.
pub const IDLE_MS: i64 = 20_000;

/// How long every tab's journal stood still before the idle window opens.
const IDLE_QUIET_BEFORE_MS: i64 = 10_000;

/// How far before a tab's next due act the idle window closes.
const IDLE_CLEAR_AFTER_MS: i64 = 15_000;

/// The hang detector of the search for an idle window.
const IDLE_SEARCH_MS: i64 = 8 * 60_000;

/// How many idle windows END tries for one with no act and no agent output.
const IDLE_TRIES: usize = 3;

/// The front tab's capture: one file, overwritten (a bare name is confined to
/// the instance's images directory).
const FRONT_IMAGE: &str = "forever-front.png";

/// The hang detector of a look's or a press's effect (a wait for something
/// that must happen).
const STEP_WAIT: Duration = Duration::from_secs(60);

/// What the window showed of one tab, each read with its time where it
/// changed ([`Window::note`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// The window was read: false under `--no-surfaces` (its wants are named
    /// skips) and in a fixture that reads none.
    pub read: bool,
    /// The tab's label on every surface: its title, the chain's name.
    pub label: String,
    /// The tab is the chains' window's front tab while the chains run: its
    /// chip is drawn at each look (a capture), every other tab's is not.
    pub front_tab: bool,
    /// Its `chrome` `autopilot` line after `sid=`: `window=<id>
    /// program=<name> face=<word|-> drawn="<cells>"|drawn=- why=<word>
    /// spoken=…`; `-` where chrome printed none for it.
    pub chip: Vec<(i64, String)>,
    /// Its entry of the front window's `toolbar-tabs`: `index=<i>
    /// selected=<0|1> states=<a,b|->`; `-` where the strip holds no tab of
    /// its label.
    pub strip: Vec<(i64, String)>,
    /// Its tab's hover, who drives it: the `driven by aterm (…)` line of its
    /// `toolbar-tabs` tooltip as the strip was last stamped with it
    /// ([`StripTab::tip`]; §12.4.1; slice 3c says why a landing holds there,
    /// `driven by aterm (land: not landed yet, …)`); `-` where the tooltip
    /// names no driver, or the strip holds no tab of its label.
    pub hover: Vec<(i64, String)>,
    /// Actual tooltip reads, including unchanged values. A change is also a read.
    pub hover_reads: Vec<i64>,
    /// The `statusmenu` rows naming it, one a line, each `<section>|<label>`
    /// (`needs`, `done`, `here`, `look` or `-`), ages masked, and before them
    /// the header of each section one of them is in,
    /// `<section>-head|<label> rows=<n>` (`needs-head|Needs you (2) rows=2`:
    /// its rows the menu lists under it, every tab's); empty for none.
    pub menu: Vec<(i64, String)>,
    /// Its Settings ▸ Harness rows, one a line: `head=…`, `state=…`,
    /// `today=…`; `-` where the page lists no session of its label.
    pub page: Vec<(i64, String)>,
    /// Actual Harness-page reads, including a row read off screen or unchanged.
    pub page_reads: Vec<i64>,
    /// THE END's steps on it (L16, L17): `step=<sweep|look-on|look|still|press>
    /// end=… end_seen=… selected=<0|1|-> states=… done_row=<yes|no>
    /// [pressed=…]` — `sweep` brought in front with no look, `look-on` the
    /// look on another tab, `look` (land-j) brought in front under it,
    /// `still` (land-k) behind land-j under it, `press` (land-k) its row
    /// pressed.
    pub looked: Vec<(i64, String)>,
    /// THE END's front sweep: its `autopilot` line read in front, after a
    /// capture (`-`: none printed).
    pub front: Option<(i64, String)>,
    /// THE END at rest (L7, [`Rest`]): `here="<This Mac line>"
    /// landed_tabs=<n> landing_tabs=<n> landings=<n> today="<the page's
    /// today line>" every="<the page's Every session row>"` (`-` for a row
    /// not read).
    pub rest: Option<String>,
}

impl Window {
    /// The window's reads of tab `sid` in glance `g` added where they changed:
    /// each change, said (`chip`, `strip`, `menu`, `page`).
    pub fn note(&mut self, g: &Glance, sid: &str) -> Vec<(&'static str, String)> {
        self.read = true;
        let label = self.label.clone();
        let chip = chip_of(&g.chrome, sid).unwrap_or_else(|| "-".into());
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

/// The desk's reads, every tab's at once: the run's windows, the `This Mac`
/// and `presence` lines and the page's STATUS sessions row where they
/// changed, and THE END's idle window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Desk {
    /// The Settings window's id and the page's view id (`inspect app/v1 tabs`).
    pub page_window: String,
    pub page_view: String,
    /// The chains' window's id (`ls`'s `window=`).
    pub chains_window: String,
    /// The front tab's session, captured at each look.
    pub front_sid: String,
    /// The menu's `This Mac — …` row.
    pub here: Vec<(i64, String)>,
    /// `presence rim=… pressing=… pill="…"`, ages masked.
    pub presence: Vec<(i64, String)>,
    /// The page's `Sessions: …` row.
    pub sessions: Vec<(i64, String)>,
    /// The page's today line under STATUS ([`today_of`]; 4c: landings first).
    pub today: Vec<(i64, String)>,
    /// SESSIONS' Every session row ([`every_of`]): `-` while it is past the
    /// page's screen (it stands after every session's row).
    pub every: Vec<(i64, String)>,
    /// THE END's idle window (`None`: never reached).
    pub idle: Option<Idle>,
    /// What THE END did, in order, as it said it.
    pub steps: Vec<(i64, String)>,
}

impl Desk {
    /// The desk's reads of glance `g` added where they changed: each change, said.
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

/// THE END's idle window as read: when it opened, how long it held, the
/// metrics after it, and why a better one was not had.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Idle {
    pub from: i64,
    pub ms: i64,
    /// `metrics` after it: `timer_wakes`, `event_wakes`,
    /// `deadline_arms_by_owner`, `deadline_fires_by_owner`,
    /// `wakes_by_variant`, `deadline_owner` (as printed).
    pub metrics: String,
    /// `metrics percentiles` after it: `n_render`, `render_p*`, `n_input`.
    pub percentiles: String,
    /// What moved in it beside the window: a journal line or an act in some
    /// tab (`<chain> +t <line>`); empty for none. A line the same as the
    /// tab's line before it is no move ([`news`]): it is in `relogged`.
    pub moved: Vec<String>,
    /// The journal lines written again in it, each the same as its tab's line
    /// before it (`<chain> +t <line>`): the loop saying the same decision again.
    pub relogged: Vec<String>,
    /// The tabs whose ends stood unseen in it, and whose holds stood: a pause
    /// (`WAITING … until=` still ahead, [`due_and_quiet`]). §12.3's Land hold
    /// is on `status` (`land_held=`, slice 3c, [`hold`]); 5d reads it here.
    pub ends: Vec<String>,
    pub holds: Vec<String>,
    /// The page was open in it (its view read after).
    pub page_open: bool,
}

/// One look's reads of the window, every tab's from the same calls.
#[derive(Clone, Debug, Default)]
pub struct Glance {
    pub t: i64,
    pub chrome: String,
    pub strip: Vec<StripTab>,
    pub menu: Vec<MenuRow>,
    /// The page's `text key=… label=…` rows.
    pub page: Vec<(String, String)>,
    /// A complete tooltip table, rather than another or partial chrome read.
    pub hover_read: bool,
    /// The Harness page was read, rather than an omitted or unrelated page.
    pub page_read: bool,
}

impl Glance {
    /// A glance of `chrome`'s and the page's text at `t`.
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
            page,
            hover_read,
            page_read,
        }
    }
}

/// One tab of the front window's `toolbar-tabs` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StripTab {
    pub index: usize,
    pub label: String,
    pub selected: bool,
    pub states: Vec<String>,
    /// Its tooltip, the hover a person reads (`None`: none): as the strip
    /// was last stamped with it (`pushed_tooltips=`, which a debug build's
    /// stripless line prints), else as composed at the read (`tooltips=`).
    /// Composed at the read, a hover whose push was never made again reads
    /// fresh; as pushed, it reads stale, as the person would see it.
    pub tip: Option<String>,
}

impl StripTab {
    /// `index=<i> selected=<0|1> states=<a,b|->`.
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

    /// Who drives it, as its hover says: the tooltip's `driven by aterm
    /// (…)` line.
    #[must_use]
    pub fn driven(&self) -> Option<String> {
        self.tip.as_deref().and_then(|tip| {
            tip.lines()
                .find(|l| l.starts_with("driven by "))
                .map(str::to_string)
        })
    }
}

/// One `statusmenu` row: its section (the Info header above it: `needs`,
/// `done`, `here`, `look`; `-` past a separator), its label, its `at=` and
/// its `indent=` (how many levels in it is drawn within its own menu, 0
/// where the line names none: a harness's further need, one level in under
/// its session's row, slice 2d; never a submenu's row, which `chrome` leads
/// with two spaces and this reader passes over).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuRow {
    pub section: String,
    pub label: String,
    pub at: String,
    pub indent: u8,
}

/// The value of a `key="…"` among `line`'s words, its escapes read back
/// (`\"`, `\\`, `\n`, `\u{…}`: the quoting `chrome` and `inspect` print).
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
                        // `{`, the hex digits, `}`: one byte a character
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

/// A printed list of optional strings (`[None, Some("a")]`) at the head of
/// `s`.
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

/// The front window's strip from `chrome`'s `toolbar-tabs count=… selected=<i>
/// labels=[…] icons=[…] states=[[…], …] tooltips=[None, Some("…"), …]
/// [pushed_tooltips=[…]]` line: each tab's tip the pushed one where the line
/// prints them ([`StripTab::tip`]).
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
            label,
            selected: selected == Some(index),
            states: states.get(index).cloned().unwrap_or_default(),
            tip: tips.get(index).cloned().flatten(),
        })
        .collect()
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

/// `chrome`'s `statusmenu "<label>" enabled=… [checked=…] [tip="…"] at=<path>`
/// rows, each under the Info row that heads its section.
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

/// Whether a menu row names the tab labelled `tab`: its mark, a space, the
/// label, then ` — ` or ` (worker of `.
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

/// The menu rows naming the tab labelled `tab`, as [`Window::menu`] keeps
/// them: the header of each section a want counts under (Needs you, Done
/// for you) with the rows the menu lists under it (`<section>-head|<label>
/// rows=<n>`: the sessions' own rows, never a row one level in), then each
/// row naming it (`<section>|<label>`), each followed by the rows one level
/// in under it, its further needs (`<section>|<label> indent=<n>`, slice
/// 2d), ages masked.
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

/// `s` with every age word (`6s`, `3m`, `2h`, `1d`) read as `#`, so a row
/// that only aged is no change.
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

/// `inspect app/v1 view <id> text`'s `text key="…" role=… label="…"` rows.
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

/// The page's rows of the session labelled `tab`: `head=<its row>`, then its
/// `state=` and `today=` rows where the page prints them.
#[must_use]
pub fn page_of(rows: &[(String, String)], tab: &str) -> Option<String> {
    let head = format!("{tab}: ");
    let (key, label) = rows.iter().find(|(k, l)| {
        k.strip_prefix("harness/session/")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            && l.starts_with(&head)
    })?;
    let mut out = vec![format!("head={label}")];
    for field in ["state", "today"] {
        let k = format!("{key}/{field}");
        if let Some((_, l)) = rows.iter().find(|(r, _)| *r == k) {
            out.push(format!("{field}={l}"));
        }
    }
    Some(out.join("\n"))
}

/// The page's today line (STATUS, §10.3.1; 4c puts landings first): its
/// block's label, both lines (`5 landed today · 3 kept going · …`).
#[must_use]
pub fn today_of(rows: &[(String, String)]) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k == "harness/status/today")
        .map(|(_, l)| l.clone())
}

/// SESSIONS' Every session row (`Every session, 16 sessions: 4 cruising,
/// 6 landing, 5 landed, 1 off`; 4c splits Land's by the chips' faces).
#[must_use]
pub fn every_of(rows: &[(String, String)]) -> Option<String> {
    rows.iter()
        .find(|(k, _)| k == EVERY_KEY)
        .map(|(_, l)| l.clone())
}

/// The page's key of SESSIONS' Every session row.
const EVERY_KEY: &str = "harness/session/all";

/// Its Timer's disclosure (key and action alike): pressed open, the page
/// brings the row on its screen first, as a hand reaching it would; pressed
/// again, closed. It writes no session.
const EVERY_TIMER: &str = "harness/session/all/timer";

/// The count `word` has in a page line of `<n> <word>` parts (`5 landed`,
/// `6 landing`, `5 landed today`), each part ended by `,`, `:`, `·` or the
/// line; 0 where the line says none (a zero part is never said).
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

/// Tab `sid`'s `autopilot` line in `chrome`, after its `sid=`.
#[must_use]
pub fn chip_of(chrome: &str, sid: &str) -> Option<String> {
    let head = format!("autopilot sid={sid} ");
    chrome
        .lines()
        .find_map(|l| l.strip_prefix(head.as_str()))
        .map(str::to_string)
}

/// An `autopilot` line read: its face word (`-` for none), its drawn cells
/// (`None` for `drawn=-`), its `why=`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chip {
    pub face: String,
    pub drawn: Option<String>,
    pub why: Option<String>,
}

/// [`Chip`] of an `autopilot` line (or [`Window::chip`]'s rest of one).
#[must_use]
pub fn chip(line: &str) -> Chip {
    Chip {
        face: word(line, "face").unwrap_or_else(|| "-".into()),
        drawn: quoted(line, "drawn"),
        why: word(line, "why"),
    }
}

/// The cells a face is drawn with at full width (§12.5.3's face table):
/// `cruise-until`'s is the head its clock follows.
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

/// Whether the drawn cells are the face's.
#[must_use]
pub fn drawn_is_face(c: &Chip) -> bool {
    match (face_cells(&c.face), &c.drawn) {
        (Some(cells), Some(d)) if c.face == "cruise-until" => d.starts_with(cells),
        (Some(cells), Some(d)) => d == cells,
        _ => false,
    }
}

/// Whether harness `program` draws no chip (its line `face=- drawn=-
/// why=no-footer`): Gemini CLI.
fn footless(program: &str) -> bool {
    program == "gemini"
}

/// Whether a chip line is the footless one.
fn no_footer(c: &Chip) -> bool {
    c.face == "-" && c.drawn.is_none() && c.why.as_deref() == Some("no-footer")
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

/// Values actually observed in `(from, until)`. Unchanged reads resolve the
/// value standing then; a value-change timestamp is itself an observation.
fn observed<'a>(
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

/// What a want is judged with.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub chain: Chain,
    pub program: &'a str,
    pub t0: i64,
}

/// Where a want is judged: under one scenario, or under every chain's first
/// scenario of those the chain names ([`Want::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Under {
    Scenario(Scenario),
    /// Every chain of the measured pass ([`Chain::ALL`]) but these.
    First(&'static [Chain]),
}

/// ONE WANT ON THE WINDOW: where it is judged, its name and its judge. Each
/// reads what the window showed ([`Window`]) or what `status` printed of it
/// (`end=`, `needs_n=`, §12.5.2), and each is a named skip where the window
/// was not read (`--no-surfaces`).
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
        }
    }
}

/// The chains whose ends are scenarios of their own (L16, L17).
pub const ENDING: [Chain; 2] = [Chain::LandJ, Chain::LandK];

/// The chains the chains' window's front tab is chosen from, the first the
/// run holds ([`front_of`]): `cruise-n`, whose N1–N4 judge its drawn cells,
/// then the Off control, which every pass mode holds, then the other Cruise
/// chains. None of them lands, so none has a `•` or an end the front tab
/// would hide (`App::stamp_presence_chips` never marks the front tab), and
/// END's look begins on it, seeing no end.
pub const FRONT: [Chain; 4] = [
    Chain::CruiseN,
    Chain::Off,
    Chain::Cruise,
    Chain::InstantCruise,
];

/// The index in `chains` of the chains' window's front tab: the first of
/// [`FRONT`] the run holds; `None` where it holds none (`--chains`
/// brings the Off control into a run that reads the window and holds none,
/// so only a run of no pass chain gets `None`).
#[must_use]
pub fn front_of(chains: &[Chain]) -> Option<usize> {
    FRONT
        .iter()
        .find_map(|f| chains.iter().position(|c| c == f))
}

/// Whether a chain's loop stands paused at END (a `WAITING` back-off after
/// its last act): the pass's chains that neither land nor end (a Cruise
/// chain, a Land chain held by its repository or its need), never the Off
/// control. END wants a pause standing only in a run that holds one.
#[must_use]
pub fn pauses(c: Chain) -> bool {
    Chain::ALL.contains(&c) && !c.lands() && !ENDING.contains(&c) && c.autopilot() != "off"
}

/// Why a run of `chains` reads no window (`None`: it reads it):
/// `--no-surfaces`, or no chain of the run with a window want (the cover,
/// cap and wall chains: the wants are the main pass's, [`Chain::ALL`]).
#[must_use]
pub fn unread_why(surfaces_on: bool, chains: &[Chain]) -> Option<&'static str> {
    if !surfaces_on {
        Some("--no-surfaces")
    } else if chains.iter().any(|c| Chain::ALL.contains(c)) {
        None
    } else {
        Some("no chain of this run has a window want")
    }
}

/// Whether END runs over a run of `chains`: only with both [`ENDING`]
/// chains, whose ends it wants standing unseen, then seen.
#[must_use]
pub fn end_runs(chains: &[Chain]) -> bool {
    ENDING.iter().all(|c| chains.contains(c))
}

/// The chains a need is scripted for (L3, L6, N1–N4).
const NEEDING: [Chain; 3] = [Chain::LandA, Chain::LandD, Chain::CruiseN];

/// THE WINDOW'S WANTS (§12.5.4's table, the rows main gives, and §12.3.7's,
/// slice 3c's: why Land still holds, [`hold`]): each judged beside its
/// scenario's records ([`with_window`]). The `indent=1` rows of a need's
/// others (N1–N4) are 2d's, here ([`n_further`]). A later slice (5d) adds
/// its rows here: END's holds read as Land's (`land_held=`, read as pauses
/// until then: [`Idle::holds`]), the page's Landed row (4c's, in WHAT THE
/// HARNESS DID, past the page's screen and no control's to bring on it),
/// the title's words.
pub const WANTS: &[Want] = &[
    Want {
        under: Under::First(&[]),
        name: "every chip face its Autopilot's, a tab behind drawn=- why=not-drawn",
        judge: chip_faces,
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
        name: "the page counts every landing: today N landed, Every session N landed beside the landing",
        judge: l7_page,
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

/// Scenario `s`'s verdict `v` from its records, with every want of
/// [`WANTS`] under it composed in: FAIL over UNOBSERVED over PASS, each want
/// named in the words; where the window was not read ([`Window::read`]),
/// each a named skip.
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

/// The faces a chain's chip may wear at `t`: its Autopilot's, a landing
/// Land's `landed` from its DONE on (either within 10 s of it: the face moves
/// at the next read), a person's exchange's `theirs` (L13).
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

/// EVERY CHIP FACE ITS AUTOPILOT'S (every chain): each `autopilot` line read
/// since its first prompt wears a face its Autopilot allows ([`faces_at`]),
/// and a tab behind the front one draws nothing (`drawn=- why=not-drawn`,
/// never a stale face); Gemini CLI's every line `face=- drawn=-
/// why=no-footer`.
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
        if footless(cx.program) {
            if !no_footer(&c) {
                bad.push(format!(
                    "+{} {l} (wanted face=- drawn=- why=no-footer)",
                    at(cx.t0, *t)
                ));
            }
            continue;
        }
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

/// NEVER AN END (every chain but L16's and L17's, and L5 under the floor):
/// `status end=-` at every read; unread (`?`) throughout is unobserved.
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

/// NEVER A ROW (every chain but L16's and L17's, and L5 under the floor): no
/// Done for you row and no `done` in its tab's states — but a worker's own
/// task record's (L7: its row is that record's) — and no need row where its
/// replies name none.
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

/// L1: `face=land` while it lands: a chip line `face=land` standing between
/// the land act and its landing (Gemini CLI: its no-footer line).
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
    let want = |c: &Chip| {
        if footless(cx.program) {
            no_footer(c)
        } else {
            c.face == "land"
        }
    };
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

/// The span its need stood in: from its `NEEDS` line to the `CLEARED …
/// needs` (or the landing, or the watch's end) after it.
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

/// A [`Window::menu`] entry's `<section>-head|` line: the header's count
/// (`Needs you (2)`: 2) and the rows the menu listed under it.
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

/// L3: its row under `Needs you (N)` in the worker's words (`◇ land-a —
/// rotate the deploy key · #`, never `<program> needs you:`), the header's
/// `N` the rows the menu lists under it (every tab's need, so 1 or more),
/// and `attention` among its tab's states, while its need stood.
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

/// L5: `face=landed` after the landing (Gemini CLI: its no-footer line), and
/// the page's state for it `landed <time>`.
fn l5_landed(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Verdict::Unobserved("no landing (L5's records)".into());
    };
    let w = &seen.window;
    let face = chips_in(seen, *d, i64::MAX).into_iter().find(|(_, c, _)| {
        if footless(cx.program) {
            no_footer(c)
        } else {
            c.face == "landed"
        }
    });
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

/// L7: the worker's row under Done for you, `✓ land-e — landed #`, after its
/// landing; and at rest, the menu's This Mac `N landed` counting every tab
/// whose `status` says `task=landed`, it among them.
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

/// Why a want of the window at rest is unread: END never read it.
const UNREST: &str = "the window never read at rest (END not reached)";

/// L7 ON THE PAGE (4c), at rest: its today line leads with the run's
/// landings this day (`5 landed today`: each landed tab's `DONE …
/// rule=land@v1`, as the A/B book counts them), and SESSIONS' Every session
/// row counts the tabs under Land by their faces — `N landed` every tab whose
/// `status` says `task=landed` under Land, `M landing` the others under Land
/// — never calling a landed tab landing.
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

/// Its `•` and its Done for you row (`✓ <label> — landed #`) once landed,
/// before the END's look: what is wrong, and what was read.
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

/// Its `step=<name>` line of END left its end standing, unseen with its `•`
/// and its Done for you row (`end_seen=0 … done_row=yes`, `done` among its
/// states where it is behind): `sweep`, a tab brought forward with no look,
/// sees no end; `look-on`, the look begun on another tab, sees none behind
/// it; `still`, a tab switch to land-j under the look, none of land-k's.
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

/// L16: its `•` and its row once landed; brought forward with no look, and
/// behind the tab the look began on, its end stands; brought in front under
/// the look (`tab <land-j>`): `end_seen=1`, the `•` and the row gone.
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

/// L17: its `•` and its row once landed; brought forward with no look, behind
/// the tab the look began on, and behind land-j once a tab switch under the
/// look saw land-j's, its end stands; after the press on its row (the look
/// on): it in front (`selected=1`), `end_seen=1`, the row gone.
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

/// N1–N4: the words each step's reply's yield is told by (its last row,
/// which `EVENT idle` quotes: its last need, or the defense) and the count
/// `status` wants of its needs.
const N_STEPS: [(&str, usize); 4] = [(NEED_B, 2), (DEFENSE_HEAD, 0), (NEED_A, 1), (NEED_C, 2)];

/// Step `k` of N1–N4's span: from its reply's yield (the first `EVENT idle`
/// holding its head after the act that asked it; the first prompt's for N1)
/// to the next act, or the watch's end. `Err` names the step a failed one
/// left unreached.
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

/// The act that asked step `k` of N1–N4: none for N1 (the first prompt), a
/// continuation for N2 and N4, the done check for N3.
const N_ASKED: [Option<&str>; 4] = [
    None,
    Some(aterm_agent::supervise::policy::turn_end::RULE_CONTINUE),
    Some(aterm_agent::supervise::policy::turn_end::RULE_DONE_CHECK),
    Some(aterm_agent::supervise::policy::turn_end::RULE_CONTINUE),
];

/// N1–N4 ON THE RECORDS: step `k`'s reply read at its yield (its last row,
/// [`N_STEPS`]) after the act that asked it ([`N_ASKED`]), and the needs
/// entry moved with it — a `NEEDS … count=<n>` line in its span (N1 2, N3 1,
/// N4 2), N2's `CLEARED … needs` — and never a DONE (Cruise).
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

/// N1–N4 on `status` and `meta`: settled at the span's end, `needs_n=` the
/// step's count (`-` for none) and the entry the first need's, carrying on.
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

/// N1–N4 on the window: every chip line standing in the span drawn as its
/// face (the front tab; Gemini CLI its no-footer line), and, settled, its
/// need row naming the first need with no count (every other need has a
/// row of its own one level in, [`n_further`]: slice 2d), and its page
/// state naming the first need with the others' count (` (+1 more)`), the
/// need alone, or none.
fn n_window(seen: &Seen, cx: &Ctx<'_>, k: usize) -> Verdict {
    let (from, until) = match n_span(seen, cx.t0, k) {
        Ok(s) => s,
        Err(v) => return v,
    };
    let w = &seen.window;
    let mut bad = Vec::new();
    let chips = chips_in(seen, from, until);
    if footless(cx.program) {
        if let Some((t, _, l)) = chips.iter().find(|(_, c, _)| !no_footer(c)) {
            bad.push(format!(
                "+{} {l} (wanted face=- drawn=- why=no-footer)",
                at(cx.t0, *t)
            ));
        }
    } else if !w.front_tab {
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

/// N1–N4's further needs, each on a row of its own one level in under the
/// tab's row (slice 2d, `indent=1`): (b) at N1, none at N2 and N3, (c) and
/// never (b) at N4 — a set that moved under the same first need.
const N_OTHERS: [&[&str]; 4] = [&[NEED_B], &[], &[], &[NEED_C]];

/// N1–N4's ROWS ONE LEVEL IN (§12.2.1, §12.5.4's N1–N4 row; slice 2d):
/// settled at the span's end, the rows the menu draws one level in under
/// the tab's own need row ([`menu_of`]'s `indent=1` lines) are exactly that
/// step's other needs, whole ([`N_OTHERS`]). A row printed as a submenu's
/// (`chrome`'s two-space lead) is no such row.
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

/// Why a want of a chain's page row is unread: its row was never on the
/// page's screen (Settings ▸ Harness draws one screen of its rows, and
/// `inspect … text` reads that screen), so the page judges nothing of it.
fn off_page(label: &str) -> String {
    format!("{label}'s row was never on the page's screen")
}

/// A want's verdict from what it found wrong (`bad`: FAIL) and what it
/// could not read (`unread`: UNOBSERVED), else PASS.
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

/// The owners of a `deadline_arms_by_owner=` (`owner:n/m,…`) or
/// `deadline_fires_by_owner=` (`owner:n,…`) value, `none` and `-` naming none.
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

/// THE END (once a harness): the idle window — `timer_wakes=0`, `n_render=0`,
/// no deadline fired, none armed but `presence`, with ends standing, a pause
/// standing where the run holds a chain that pauses ([`pauses`]), and the
/// page open (an act or a journal line in it is no rest: unobserved; a
/// program's own writes are answered by the status sweep alone, at most
/// [`SWEEP_FIRES_PER_WRITE`] fires a write) — then each chain brought in
/// front, its chip's `drawn=` equal to its `face=` (Gemini CLI: its
/// no-footer line).
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
    // What may wake the instance in it: with no Output wake, nothing
    // (timer_wakes=0, no fire, nothing armed beside presence). A program
    // that writes at idle (Muse Code puts an SGR reset and a cursor move,
    // changing no cell, every 10 s) is answered by the status sweep alone:
    // `session_status` arms and fires, every timer wake is one of its
    // fires, and it fires at most `SWEEP_FIRES_PER_WRITE` times a write.
    let allowed: &[&str] = if output == 0 {
        &[PRESENCE]
    } else {
        &[PRESENCE, SWEEP]
    };
    let mut bad = Vec::new();
    if n_render != "0" {
        bad.push(format!("n_render={n_render}"));
    }
    let fired: Vec<&str> = owners(&fires)
        .into_iter()
        .filter(|o| output == 0 || *o != SWEEP)
        .collect();
    if !fired.is_empty() {
        bad.push(format!("deadline_fires_by_owner={fires}"));
    }
    let sweep_fires = count_of(&fires, SWEEP);
    if output == 0 && timer != "0" {
        bad.push(format!("timer_wakes={timer}"));
    } else if output > 0 && timer.parse::<u64>().ok() != Some(sweep_fires) {
        bad.push(format!(
            "timer_wakes={timer}, not the status sweep's {sweep_fires} fire(s)"
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
    let pausing = tabs.iter().any(|(c, _)| pauses(*c));
    if idle.holds.is_empty() && pausing {
        unread.push("no hold stood in it".to_string());
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
        if pausing {
            ""
        } else {
            " (none wanted: no chain of the run pauses)"
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
    // the sweep: each chain in front, drawn as its face
    let mut sweep = Vec::new();
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
        let ok = if footless(program) {
            no_footer(&c)
        } else {
            drawn_is_face(&c)
        };
        if !ok {
            sweep.push(format!("{}: {}", chain.name(), one_line(l, 120)));
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
    let said = if output > 0 {
        format!(
            "{said}; the programs' own writes: {output} Output wake(s), answered by the status sweep alone ({sweep_fires} fire(s), {:.2} a write, at most {SWEEP_FIRES_PER_WRITE})",
            ratio(sweep_fires, output)
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

/// The deadline owner presence keeps (the pill's age, the claim's lapse):
/// armed at rest, never fired there.
const PRESENCE: &str = "presence";

/// The deadline owner of the status sweep, which answers a program's output.
const SWEEP: &str = "session_status";

/// How many times the status sweep may fire for each `Output` wake at idle:
/// a write owes the agent's follow-up look (`SessionStatus::next_wake`, once
/// the output pauses) and the transition that look may owe — two, measured
/// 1.75 and 2.1 a write over Muse Code's 16 tabs — with room for one more.
/// More is a sweep that re-arms itself while output trickles: the timer spin
/// END's idle window is there to catch.
const SWEEP_FIRES_PER_WRITE: u64 = 3;

/// `n / d` for the report (0 for none).
#[expect(
    clippy::cast_precision_loss,
    reason = "a few hundred wakes, exact in an f64"
)]
fn ratio(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

/// The count `owner` has in a `deadline_fires_by_owner=` (`owner:n,…`) or
/// `deadline_arms_by_owner=` (`owner:n/m,…`: its `n`) value; 0 where absent.
fn count_of(v: &str, owner: &str) -> u64 {
    v.split(',')
        .find_map(|p| {
            let n = p.strip_prefix(owner)?.strip_prefix(':')?;
            n.split('/').next()?.parse().ok()
        })
        .unwrap_or(0)
}

/// `chrome` and the page's text, after a capture of the front tab: one glance.
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

/// The windows' ids in `inspect app/v1 tabs`: each `window id=<n>`, and the
/// Settings page's `(window, view)` where a leaf is `kind=app app=settings`.
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

/// THE WINDOWS, LAID OUT BEFORE THE CHAINS' TABS: the Settings window
/// (`invoke NewWindow`, `open app settings /harness` there, its view read off
/// `inspect app/v1 tabs`), then the chains' own (`invoke NewWindow`), the
/// newest, so a headless instance's frontmost: where every flagless `spawn`
/// opens its tab.
///
/// # Errors
/// A step the instance refused, named.
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
    step(&["invoke", "NewWindow"])?;
    step(&["open", "app", "settings", "/harness"])?;
    let (before, page) = windows_of(&door.out(&["inspect", "app/v1", "tabs"]));
    let (page_window, page_view) =
        page.ok_or_else(|| "no Settings view in `inspect app/v1 tabs`".to_string())?;
    step(&["invoke", "NewWindow"])?;
    let (after, _) = windows_of(&door.out(&["inspect", "app/v1", "tabs"]));
    let chains_window = after
        .into_iter()
        .find(|w| !before.contains(w))
        .ok_or_else(|| "the chains' window: no new window in `inspect app/v1 tabs`".to_string())?;
    Ok(Desk {
        page_window,
        page_view,
        chains_window,
        ..Desk::default()
    })
}

/// A session's `window=` in `ls`.
#[must_use]
pub fn window_of(ls: &str, sid: &str) -> Option<String> {
    ls.lines()
        .find(|l| l.split_whitespace().nth(2) == Some(sid))
        .and_then(|l| word(l, "window"))
}

/// What one tab's END step read: `step=<name> end=… end_seen=…
/// selected=<0|1|-> states=… done_row=<yes|no>[ pressed=…]`.
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

/// The soonest instant a tab's journal names its next act at (the last
/// `WAITING … until=<stamp>`, `DONE` or `LEFT` after its last act, where a
/// `WAITING`: its `until`, read as `now` where it passed within the last
/// minute — the act is on its way) and the latest journal line's time.
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

/// A journal's lines less those the same as the line before them: the loop
/// writing one decision again (a `WAITING` with the same seq, `until` and
/// reason) says nothing new.
fn news(journal: &[(i64, String)]) -> impl Iterator<Item = &(i64, String)> {
    journal
        .iter()
        .enumerate()
        .filter(|(i, (_, l))| i.checked_sub(1).is_none_or(|p| journal[p].1 != *l))
        .map(|(_, e)| e)
}

/// Tab `name`'s journal lines inside `[from, from + ms]` (`<name> +t
/// <line>`): those that moved, and those written again, each the same as the
/// line before it ([`news`]).
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

/// The unix ms of an RFC 3339 UTC stamp (`2026-10-21T10:24:06Z`).
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

/// THE END'S IDLE WINDOW: once every tab's journal stood still
/// [`IDLE_QUIET_BEFORE_MS`] and no tab's next act falls within it and
/// [`IDLE_CLEAR_AFTER_MS`] after, `metrics reset`, [`IDLE_MS`] with no read,
/// then `metrics` and `metrics percentiles`; a window an act or a journal
/// line fell in is tried again, [`IDLE_TRIES`] in all.
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
        let holds: Vec<String> = tabs
            .iter()
            .filter(|t| {
                due_and_quiet(std::slice::from_ref(*t), now_ms())
                    .0
                    .is_some()
            })
            .map(|t| t.chain.name().to_string())
            .collect();
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
        let keep = |line: &str, keys: &[&str]| {
            keys.iter()
                .map(|k| format!("{k}={}", metric(line, k)))
                .collect::<Vec<_>>()
                .join(" ")
        };
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
            metrics: keep(
                &metrics,
                &[
                    "timer_wakes",
                    "event_wakes",
                    "deadline_owner",
                    "deadline_arms_by_owner",
                    "deadline_fires_by_owner",
                    "wakes_by_variant",
                ],
            ),
            percentiles: keep(
                &percentiles,
                &[
                    "n_render",
                    "render_p50_ms",
                    "render_p95_ms",
                    "render_p99_ms",
                    "n_input",
                ],
            ),
            moved,
            relogged,
            ends,
            holds,
            page_open,
        };
        // a window an act or a journal line fell in is no rest; a program's
        // own writes are judged ([`judge_end`])
        let rest = idle.moved.is_empty();
        last_idle = Some(idle);
        if rest {
            break;
        }
    }
    Ok(last_idle.unwrap_or_default())
}

/// The landings a tab's journal notes on the day `today` holds (4b's rule,
/// `aterm_agent::harness::stats`' THE LANDINGS): a `DONE … rule=land@v1`
/// line counts where it is the tab's first such note or a land act
/// (`land@v1`, `land-nudge@v1`, `land-answer@v1`) came since the last one; a
/// note again with no act before it restates a landing and counts nothing.
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

/// THE END'S STEPS ON THE WINDOW, once every chain is over: the idle window
/// ([`Desk::idle`]); This Mac and the page's counts at rest
/// ([`Window::rest`]: SESSIONS' Every session row brought on the page's
/// screen by its Timer's disclosure, pressed open and closed); each chain's
/// tab brought in front with no look (`person look off`, `tab <index>`, a
/// capture, its chip line: [`Window::front`]; L16's and L17's ends read
/// after, still unseen: `step=sweep`); then the look, begun on the front
/// tab, which has no end (`tab <front>`, `person look <the chains'
/// window>`: `step=look-on`, both ends still standing), `tab <land-j>` under
/// it (`step=look`, land-k's `step=still`), the press on `land-k`'s Done for
/// you row (`step=press`), `person look off` and `tab <front>`.
///
/// # Errors
/// Only a stop.
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
    // ---- the idle window
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
    // ---- This Mac and the page's counts at rest
    let g = Glance::of(now_ms(), &door.out(&["chrome"]), "");
    let here = g
        .menu
        .iter()
        .find(|r| r.section == "here")
        .map_or_else(String::new, |r| r.label.clone());
    // the tabs under Land by their tasks: landed (the chip's `✓ landed`),
    // or landing (`⤓ land`, the task open); a person's own exchange
    // (`theirs`, the chip's `⤓ quiet`) is neither
    let (mut landed_tabs, mut landing_tabs) = (0, 0);
    for t in tabs.iter() {
        let s = on(door, &t.sid, &["status"]);
        if word(&s, "autopilot").as_deref() != Some("land") {
            continue;
        }
        match word(&s, "task").as_deref() {
            Some("landed") => landed_tabs += 1,
            Some("theirs") => {}
            _ => landing_tabs += 1,
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
            // past the page's screen (it stands after every session's row):
            // its Timer's disclosure, pressed, brings it on, as a hand does
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
    let rest = format!(
        "here={} landed_tabs={landed_tabs} landing_tabs={landing_tabs} landings={landings} today={} every={}",
        q(&here),
        q(today.as_deref().unwrap_or("-")),
        q(every.as_deref().unwrap_or("-"))
    );
    say(desk, format!("at rest {rest}"));
    for tab in tabs.iter_mut() {
        tab.seen.window.rest = Some(rest.clone());
    }
    // ---- each chain in front, with no look
    let _ = door.out(&["person", "look", "off"]);
    let index_of = |door: &Door<'_>, label: &str| {
        strip_tabs(&door.out(&["chrome"]))
            .into_iter()
            .find(|t| t.label == label)
            .map(|t| t.index.to_string())
    };
    for tab in tabs.iter_mut() {
        let label = tab.seen.window.label.clone();
        let Some(i) = index_of(door, &label) else {
            say(desk, format!("{label}: not in the front window's strip"));
            continue;
        };
        let r = door.out(&["tab", &i]);
        let _ = on(door, &tab.sid, &["image", FRONT_IMAGE]);
        let line = chip_of(&door.out(&["chrome"]), &tab.sid).unwrap_or_else(|| "-".into());
        say(
            desk,
            format!("front {label} (tab {i}: {}): {line}", r.trim()),
        );
        tab.seen.window.front = Some((now_ms(), line));
    }
    for tab in tabs.iter_mut().filter(|t| ENDING.contains(&t.chain)) {
        let l = step_line(door, tab, "sweep", None);
        say(desk, format!("{}: {l}", tab.chain.name()));
        tab.seen.window.looked.push((now_ms(), l));
    }
    // ---- the look, begun on the front tab: a look marks the SELECTED tab's
    // end seen at once, so it begins where no end stands, and both ends behind
    // it still stand
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
    // ---- land-j brought in front under the look: the tab switch sees its end
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
    // ---- land-k behind it, its end still standing
    if let Some(tab) = tabs.iter_mut().find(|t| t.chain == Chain::LandK) {
        let l = step_line(door, tab, "still", None);
        say(desk, format!("{}: {l}", tab.chain.name()));
        tab.seen.window.looked.push((now_ms(), l));
    }
    // ---- the press: land-k's Done for you row, the look on
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
    // ---- the look off, the front tab back in front with no look
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

#[cfg(test)]
#[path = "forever_surfaces_tests.rs"]
mod tests;
