// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS' MENUS (owner, 2026-10-19: "I need to also be able to click the
//! other text on that line"): a right-click on a fact of Claude Code's
//! composer rule — a ctrl-click on macOS, `↓`, shift+F10 or the Menu key on
//! a fact the keyboard selected, a screen reader's context menu, `open
//! fact-menu <model|effort|path|branch|context|usage>` — opens that fact's glass menu: the
//! Autopilot menu's card (`crate::autopilot_menu`, owned by the fact), hung
//! from the fact's first cell.
//!
//! ```text
//!  Folder · tab 1 “fix the footer”
//!  ⌂  ~/aterm
//!  +  New tab here
//!  ⧉  Copy path
//!  ──────────────────────────────
//!  ○  Hide folder
//!  ●  Show branch                      for each other fact hidden here
//! ```
//!
//! **Three rows type, and only Claude's own commands** (owner: "clicking
//! [the top-effort key] toggles it on and off … and likewise for model,
//! effort"): the model's `⋯  Change model…  /model` and the effort's
//! `⋯  Effort slider…  /effort` open Claude's own picker, where the person
//! picks and aterm presses nothing; the effort's `◈  Turn <key> on|off for
//! this session` — offered on Claude Code newer than 2.1.303, while Claude
//! has not refused the key for the model — types the session-only toggle.
//! Each goes through the facts' one typing path
//! (`crate::claude_slash::App::press_slash`), decided as the row is chosen;
//! a refusal keeps the menu open, its rows built again under a note naming
//! why, the typed rows disabled.
//!
//! **No other row types into any program.** The folder's rows act on the session's
//! absolute working directory as the resolver published it beside the facts
//! (`claude_footer::FactSide`): a new tab there, or the path on the clipboard
//! — both off where no side was published. The branch's copies its name, and
//! opens a new tab at the repository's root where the climb found one.
//! Nothing opens a path in Finder (a directory named `.app` would launch),
//! nothing switches a branch and nothing runs git. Hide and Show flip the
//! session's mask as a click on the fact does (`crate::claude_facts`); every
//! menu of the session lists `●  Show <fact>` for each fact hidden there, so
//! a stub the rule gave up for room is still shown again. A copy or a new
//! tab says so for two seconds in the lights' title slot, where a block of
//! chips is drawn (`✓ copied`, `✓ new tab`).
//!
//! **The usage's card is a graph over time** (owner: "the traffic shows a
//! graph over time"): a click on `Σ` — Return, Space or `↓` on it selected,
//! a right-click, `open fact-menu usage` — opens the Traffic card, the
//! session's tokens in (`↑`) and out (`↓`) as two bands of two rows each,
//! sixteen levels a band, each linear in its own peak; a column in which
//! Claude was working stands on a shaded ground; under them the axis (the
//! first bucket's local time, the span, `now`) and the totals
//! ([`traffic_rows`]). It reads the traffic the resolver published beside the
//! facts (`claude_footer::FactSide::traffic`) at the open and at each wake
//! of the facts while it is open — never on a frame, and nothing while shut.
//! A second click on `Σ` lands off the card, and closes it.
//!
//! ```text
//!  Traffic · tab 1 “fix the footer”
//!  ↑ in       ▂▅█▃      ▁
//!          ▁▃▇████▂   ▁▂█
//!  ↓ out        ▃
//!          ▂▁▂███▅▁    ▃
//!        14:02     6m33s     now
//!  6m33s working · ↑20M in · ↓82k out
//!  peak ↑1.2M · ↓9.8k per 2m00s
//! ```
//!
//! **It stays while its fact slides along its row** (a fact before it grew
//! or shrank) and closes at the first painted frame whose glass shows the
//! fact on its row no more, on a focus or tab change, on Esc, on any other
//! key and on a click outside it — the Autopilot menu's rules. One menu is
//! open at a time: opening one closes the other.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::session_usage::{TrafficView, runtime_short, tokens_short};

use crate::autopilot_menu::{
    self, AutopilotMenu, MenuAct, MenuOwner, MenuRow, PlotCell, PlotInk, PointerRest,
    TRAFFIC_MAX_CONTENT, content_room, content_room_up_to, step,
};
use crate::claude_lights::{Clock, Fact, FactNotice, GlassAt};
use crate::claude_slash::Slash;
use crate::{App, WindowId};

/// What a fact menu's row does when chosen. Only [`FactAct::Type`] types,
/// and only Claude's own command, through `App::press_slash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FactAct {
    /// The absolute working directory onto the clipboard.
    CopyPath,
    /// A new tab in the working directory.
    NewTabHere,
    /// The branch's name onto the clipboard.
    CopyBranch,
    /// A new tab at the repository's root.
    NewTabAtRoot,
    /// Hide the fact behind its greyed `off`, as a click on it does.
    Hide(FactKind),
    /// Show the hidden fact again, as a click on its `off` does.
    Show(FactKind),
    /// Type Claude's own command (`crate::claude_slash`).
    Type(Slash),
    /// Switch/pin onto the roster account at this index.
    UseAccount(usize),
    /// Unpin and return to automatic account rotation (dedicated → pool).
    AutoRotate,
}

impl FactAct {
    /// The act's part of its row's stable key (`fact-menu/<fact>/<act>`).
    #[must_use]
    pub(crate) fn key(self) -> String {
        match self {
            Self::CopyPath => "copy-path".to_string(),
            Self::NewTabHere => "new-tab-here".to_string(),
            Self::CopyBranch => "copy-branch".to_string(),
            Self::NewTabAtRoot => "new-tab-at-root".to_string(),
            Self::Hide(kind) => format!("hide/{}", Fact::of(kind).key()),
            Self::Show(kind) => format!("show/{}", Fact::of(kind).key()),
            Self::Type(slash) => format!("type/{}", slash.key()),
            Self::UseAccount(idx) => format!("use/{idx}"),
            Self::AutoRotate => "auto-rotate".to_string(),
        }
    }
}

/// One account row in [`Fact::Account`]'s pulldown menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountMenuRow {
    /// The row's label on the card (`m27 · dedicated · 0% left (limited)`).
    pub(crate) label: String,
    /// Whether this account is currently active (`●` at the right edge).
    pub(crate) active: bool,
    /// Whether clicking this row can switch to or enrol this account.
    pub(crate) enabled: bool,
}

/// Format a weekly availability string (`"weekly 73% left"`) with a 5-cell
/// meter bar (`"▰▰▰▰▱  weekly 73% left"`).
#[must_use]
pub(crate) fn weekly_meter_bar(text: &str) -> String {
    let pct = text
        .split_whitespace()
        .find_map(|w| w.strip_suffix('%'))
        .and_then(|n| n.parse::<u32>().ok());
    let Some(pct) = pct else {
        return text.to_string();
    };
    let filled = if pct == 0 {
        0
    } else {
        (pct.min(100) * 5).div_ceil(100).clamp(1, 5) as usize
    };
    let mut bar = String::with_capacity(15 + 2 + text.len());
    for i in 0..5 {
        bar.push(if i < filled { '\u{25b0}' } else { '\u{25b1}' });
    }
    bar.push_str("  ");
    bar.push_str(text);
    bar
}

/// What a fact's menu is built from, read at the open and again whenever the
/// facts move under it ([`App::refresh_fact_menu`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FactMenuFacts {
    /// The menu's name: the fact and the session's place (`Folder · tab 1`).
    pub(crate) title: String,
    /// The session's own name, in quotes after the place where both fit.
    pub(crate) name: Option<String>,
    /// The content cells the card has in its pane.
    pub(crate) room: usize,
    /// Its name as a screen reader says it (`Folder, tab 1, fix the footer`).
    pub(crate) spoken: String,
    /// The facts as the rule shows them.
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) branch: Option<String>,
    /// The context window's indicator (`ctx 84% left`).
    pub(crate) context: Option<String>,
    /// The active subscription summary (`sol-m27@example.test · max 20x`).
    pub(crate) subscription: Option<String>,
    /// The weekly meter line (`▱▱▱▱▱  weekly 0% left`).
    pub(crate) weekly_meter: Option<String>,
    /// The rotation accounts in roster order (dedicated first, then pool).
    pub(crate) accounts: Vec<AccountMenuRow>,
    /// Whether automatic rotation (`!state.pinned`) is active.
    pub(crate) auto_rotate: bool,
    /// The facts hidden in this session.
    pub(crate) off: FactMask,
    /// The absolute working directory and the repository's root the
    /// resolver published beside the facts (`claude_footer::FactSide`).
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) repo_root: Option<PathBuf>,
    /// The top-effort key's row: `Some(true)` offers to turn it on,
    /// `Some(false)` off (the ENGINE's top rule shows its tag); `None`, no
    /// row (Claude Code 2.1.303 or older, the key latched off by Claude's
    /// refusal, no composer to read).
    pub(crate) key_row: Option<bool>,
    /// Why the last typed row chosen here was refused: said over the typed
    /// rows, which it disables.
    pub(crate) note: Option<String>,
    /// A slash press is in flight for the session: the typed rows wait.
    pub(crate) typing: bool,
}

/// The facts a menu can hide or show, in the rule's order.
const HIDEABLE: [FactKind; 5] = [
    FactKind::Model,
    FactKind::Effort,
    FactKind::Path,
    FactKind::Branch,
    FactKind::Context,
];

/// One choice of `fact`'s menu, on or off.
fn item(fact: Fact, glyph: &'static str, label: &str, act: FactAct, enabled: bool) -> MenuRow {
    MenuRow::Item {
        glyph,
        label: label.to_string(),
        spoken: label.to_string(),
        act: MenuAct::Fact(fact, act),
        enabled,
        checked: false,
    }
}

/// THE MODEL of `fact`'s menu (design §10.6.14): its header, the value it shows, the
/// rows that act on it where it has any, then — under a rule — its own Hide
/// (or Show, while it is hidden) and a Show for every other fact hidden
/// here. The usage has no menu of rows (its Traffic card is its own): its
/// header alone. The top rule's key's menu is the effort's, hung from the
/// key. Pure.
#[must_use]
pub(crate) fn rows(fact: Fact, facts: &FactMenuFacts) -> Vec<MenuRow> {
    let mut out = autopilot_menu::header_rows(&facts.title, facts.name.as_deref(), facts.room);
    let value = |v: &Option<String>| v.clone().unwrap_or_default();
    let typed = facts.note.is_none() && !facts.typing;
    let note = |out: &mut Vec<MenuRow>| {
        if let Some(note) = &facts.note {
            out.push(MenuRow::Note(note.clone()));
        }
    };
    match fact {
        Fact::Model => {
            out.push(MenuRow::Note(format!("\u{25c6}  {}", value(&facts.model))));
            note(&mut out);
            out.push(item(
                fact,
                "\u{22ef}",
                "Change model\u{2026}  /model",
                FactAct::Type(Slash::ModelPicker),
                typed,
            ));
        }
        Fact::Effort | Fact::TopEffort => {
            out.push(MenuRow::Note(format!("\u{25c9}  {}", value(&facts.effort))));
            note(&mut out);
            if let Some(on) = facts.key_row {
                out.push(item(
                    fact,
                    "\u{25c8}",
                    &format!(
                        "Turn {} {} for this session",
                        aterm_primer::CLAUDE_TOP_EFFORT_KEY,
                        if on { "on" } else { "off" }
                    ),
                    FactAct::Type(Slash::TopEffort(on)),
                    typed,
                ));
            }
            out.push(item(
                fact,
                "\u{22ef}",
                "Effort slider\u{2026}  /effort",
                FactAct::Type(Slash::EffortPicker),
                typed,
            ));
        }
        Fact::Path => {
            out.push(MenuRow::Note(format!("\u{2302}  {}", value(&facts.path))));
            let here = facts.cwd.is_some();
            out.push(item(fact, "+", "New tab here", FactAct::NewTabHere, here));
            out.push(item(fact, "\u{29c9}", "Copy path", FactAct::CopyPath, here));
        }
        Fact::Branch => {
            out.push(MenuRow::Note(format!("\u{2387}  {}", value(&facts.branch))));
            out.push(item(
                fact,
                "\u{29c9}",
                "Copy branch name",
                FactAct::CopyBranch,
                facts.branch.is_some(),
            ));
            if facts.repo_root.is_some() {
                out.push(item(
                    fact,
                    "+",
                    "New tab at the repository root",
                    FactAct::NewTabAtRoot,
                    true,
                ));
            }
        }
        // The context's menu is its value and the Hide or Show row alone.
        Fact::Context => {
            out.push(MenuRow::Note(format!(
                "{}  {}",
                aterm_agent::harness::footer::CONTEXT_MARK,
                value(&facts.context)
            )));
        }
        Fact::Account => {
            if let Some(sub) = &facts.subscription {
                out.push(MenuRow::Note(format!(
                    "{}  {sub}",
                    aterm_agent::harness::footer::ACCOUNT_MARK
                )));
            }
            if let Some(meter) = &facts.weekly_meter {
                out.push(MenuRow::Note(format!(
                    "{}  {meter}",
                    aterm_agent::harness::footer::WEEKLY_MARK
                )));
            }
            note(&mut out);
            if facts.accounts.is_empty() {
                out.push(MenuRow::Note(
                    "No rotation accounts configured \u{2014} run aterm init".to_string(),
                ));
                out.push(MenuRow::Rule);
                out.push(MenuRow::Item {
                    glyph: "\u{21bb}",
                    label: "Sync & Auto-rotate (dedicated \u{2192} pool)".to_string(),
                    spoken: "Sync and auto-rotate accounts".to_string(),
                    act: MenuAct::Fact(fact, FactAct::AutoRotate),
                    enabled: true,
                    checked: false,
                });
                return out;
            }
            out.push(MenuRow::Rule);
            for (idx, acct) in facts.accounts.iter().enumerate() {
                out.push(MenuRow::Item {
                    glyph: "\u{2386}",
                    label: acct.label.clone(),
                    spoken: acct.label.clone(),
                    act: MenuAct::Fact(fact, FactAct::UseAccount(idx)),
                    enabled: acct.enabled,
                    checked: acct.active,
                });
            }
            out.push(MenuRow::Rule);
            out.push(MenuRow::Item {
                glyph: "\u{21bb}",
                label: "Auto-rotate (dedicated \u{2192} pool)".to_string(),
                spoken: "Auto-rotate, dedicated then pool".to_string(),
                act: MenuAct::Fact(fact, FactAct::AutoRotate),
                enabled: true,
                checked: facts.auto_rotate,
            });
            return out;
        }
        Fact::Usage => return out,
    }
    out.push(MenuRow::Rule);
    // The top rule's key's menu is the effort's own.
    let own = fact.kind().unwrap_or(FactKind::Effort);
    let show = |kind: FactKind| {
        item(
            fact,
            "\u{25cf}",
            &format!("Show {}", Fact::of(kind).word()),
            FactAct::Show(kind),
            true,
        )
    };
    out.push(if facts.off.has(own) {
        show(own)
    } else {
        item(
            fact,
            "\u{25cb}",
            &format!("Hide {}", fact.word()),
            FactAct::Hide(own),
            true,
        )
    });
    out.extend(
        HIDEABLE
            .into_iter()
            .filter(|kind| *kind != own && facts.off.has(*kind))
            .map(show),
    );
    out
}

/// The cells a band's label takes at the graph's left (`↑ in  `, `↓ out `).
const LABEL_CELLS: usize = 6;

/// A band's level for `v` against the band's own `peak`: none for no
/// tokens, else one to sixteen eighths of its two rows, linear, and never
/// less than one — a trickle is still seen.
#[must_use]
pub(crate) fn band_level(v: u64, peak: u64) -> u32 {
    if v == 0 || peak == 0 {
        return 0;
    }
    let level = (u128::from(v) * 16).div_ceil(u128::from(peak)).clamp(1, 16);
    u32::try_from(level).unwrap_or(16)
}

/// The two rows' glyphs of a band column at `level` (`(top, bottom)`): the
/// bottom row fills first, in eighths (`▁` … `▇`, then `█`), and the top
/// row takes the rest; none for none.
#[must_use]
pub(crate) fn band_glyphs(level: u32) -> (char, char) {
    let eighths = |n: u32| char::from_u32(0x2580 + n).unwrap_or(' ');
    let bottom = match level {
        0 => ' ',
        1..=7 => eighths(level),
        _ => '\u{2588}',
    };
    let top = if level > 8 { eighths(level - 8) } else { ' ' };
    (top, bottom)
}

/// `plot` cells of axis: `left` at the start, `right` at the end and `mid`
/// between, centred, where they fit with a blank between each; then `left`
/// and `right`; then `right` alone.
fn axis_text(plot: usize, left: &str, mid: &str, right: &str) -> Vec<char> {
    let mut out = vec![' '; plot];
    let put = |out: &mut Vec<char>, at: usize, text: &str| {
        for (slot, ch) in out.iter_mut().skip(at).zip(text.chars()) {
            *slot = ch;
        }
    };
    let (l, m, r) = (
        left.chars().count(),
        mid.chars().count(),
        right.chars().count(),
    );
    if l + m + r + 2 <= plot {
        put(&mut out, 0, left);
        let centre = (plot - m) / 2;
        put(&mut out, centre.clamp(l + 1, plot - r - 1 - m), mid);
        put(&mut out, plot - r, right);
    } else if l + r < plot {
        put(&mut out, 0, left);
        put(&mut out, plot - r, right);
    } else {
        put(&mut out, plot.saturating_sub(r), right);
    }
    out
}

/// THE TRAFFIC CARD'S BODY (under its header), `cols` content cells wide:
/// the in band and the out band, two rows each — a label, then one column
/// per slice of `view`'s span from its first bucket to `now_s`
/// ([`TrafficView::columns`]), each at its band's level ([`band_level`],
/// [`band_glyphs`]), shaded where Claude was working — the axis (the start
/// at the local `offset`, the span, `now`), the totals, the peaks per
/// column's slice, and the rows no graph could place. An empty series is
/// `No traffic yet`. Pure.
#[must_use]
pub(crate) fn traffic_rows(
    view: &TrafficView,
    now_s: i64,
    offset: Option<i64>,
    cols: usize,
) -> Vec<MenuRow> {
    let unread =
        (view.unread > 0).then(|| MenuRow::Note(format!("+{} unread not graphed", view.unread)));
    if view.is_empty() || view.width_s <= 0 {
        return std::iter::once(MenuRow::Note("No traffic yet".to_string()))
            .chain(unread)
            .collect();
    }
    let plot = cols.saturating_sub(LABEL_CELLS).max(1);
    let from = view.start_s;
    let to = now_s.max(from.saturating_add(view.width_s));
    let columns = view.columns(from, to, plot);
    let peak = columns.iter().fold((0, 0), |(i, o), &(ci, co)| {
        (u64::max(i, ci), u64::max(o, co))
    });
    let span = i128::from(to) - i128::from(from);
    let n = i128::try_from(plot).unwrap_or(1);
    let edge = |c: usize| {
        let c = i128::try_from(c).unwrap_or(0);
        i64::try_from(i128::from(from) + c * span / n).unwrap_or(i64::MAX)
    };
    let busy: Vec<bool> = (0..plot)
        .map(|c| {
            let (lo, hi) = (edge(c), edge(c + 1));
            view.busy.iter().any(|&(a, b)| a < hi && b > lo)
        })
        .collect();
    let lead = |text: &str, ink: PlotInk| -> Vec<PlotCell> {
        text.chars()
            .chain(std::iter::repeat(' '))
            .take(LABEL_CELLS)
            .map(|ch| PlotCell {
                ch,
                ink,
                busy: false,
            })
            .collect()
    };
    let mut out = Vec::with_capacity(9);
    for (label, ink, peak, pick) in [
        ("\u{2191} in", PlotInk::In, peak.0, 0),
        ("\u{2193} out", PlotInk::Out, peak.1, 1),
    ] {
        let levels: Vec<u32> = columns
            .iter()
            .map(|&(i, o)| band_level(if pick == 0 { i } else { o }, peak))
            .collect();
        for top in [true, false] {
            let mut cells = lead(if top { label } else { "" }, ink);
            cells.extend(levels.iter().zip(&busy).map(|(&level, &busy)| {
                let (t, b) = band_glyphs(level);
                PlotCell {
                    ch: if top { t } else { b },
                    ink,
                    busy,
                }
            }));
            out.push(MenuRow::Plot(cells.into()));
        }
    }
    let minute = |t: i64| {
        let local = t.saturating_add(offset.unwrap_or(0));
        u16::try_from(local.rem_euclid(86_400) / 60).unwrap_or(0)
    };
    let span_s = u64::try_from(span).unwrap_or(0);
    let start = Clock::of_minute(minute(from));
    let mut axis = lead("", PlotInk::Axis);
    axis.extend(
        axis_text(plot, start.as_str(), &runtime_short(span_s), "now")
            .into_iter()
            .map(|ch| PlotCell {
                ch,
                ink: PlotInk::Axis,
                busy: false,
            }),
    );
    out.push(MenuRow::Plot(axis.into()));
    let (total_in, total_out) = view.totals;
    let tokens = format!(
        "\u{2191}{} in \u{b7} \u{2193}{} out",
        tokens_short(total_in),
        tokens_short(total_out)
    );
    out.push(MenuRow::Note(match view.runtime_s {
        Some(secs) => format!("{} working \u{b7} {tokens}", runtime_short(secs)),
        None => tokens,
    }));
    let per = span_s.div_ceil(u64::try_from(plot).unwrap_or(1));
    out.push(MenuRow::Note(format!(
        "peak \u{2191}{} \u{b7} \u{2193}{} per {}",
        tokens_short(peak.0),
        tokens_short(peak.1),
        runtime_short(per)
    )));
    out.extend(unread);
    out
}

#[cfg(test)]
thread_local! {
    /// The folders a new tab was asked for, in a test build: a headless App
    /// has no event loop to spawn the tab's shell on, so the ask is what a
    /// test can see.
    pub(crate) static NEW_TABS_ASKED: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn build_account_menu_rows(
    session_account: Option<&str>,
    session_email: Option<&str>,
    session_pct_left: Option<u32>,
) -> (Vec<AccountMenuRow>, bool) {
    use aterm_agent::harness::accounts;
    if cfg!(test) {
        return (Vec::new(), true);
    }
    let now = aterm_time::unix_secs()
        .and_then(|n| i64::try_from(n).ok())
        .unwrap_or(0);
    let Some(roster_path) = accounts::default_roster_path() else {
        return (Vec::new(), true);
    };
    let state_dir = crate::harness_host::harness_state();
    let roster = accounts::Roster::load(&roster_path)
        .ok()
        .unwrap_or_default();
    let (roster, state) = match state_dir.as_deref() {
        Some(dir) => {
            let store = accounts::Store::at(dir);
            let roster = store.bound(&roster, now).unwrap_or(roster);
            let (state, _) = store.load(now).unwrap_or_default();
            (roster, state)
        }
        None => (roster, accounts::State::default()),
    };
    let enrolled = accounts::enrolled(&roster);
    let active_label = session_account.or(state.active.as_deref()).or_else(|| {
        session_email.and_then(|email| {
            roster
                .rows
                .iter()
                .find(|r| {
                    r.email
                        .as_deref()
                        .is_some_and(|re| accounts::email_matches(re, email))
                })
                .map(|r| r.label.as_str())
        })
    });
    let mut rows = Vec::with_capacity(roster.rows.len());
    for row in &roster.rows {
        let is_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = accounts::condition(row, &state, is_enrolled, now);
        let is_active = active_label == Some(row.label.as_str());
        let pct_left = if matches!(cond, accounts::Condition::Limited { .. }) {
            Some(0)
        } else {
            state
                .accounts
                .get(&row.label)
                .and_then(|acct| {
                    acct.windows
                        .get("seven_day")
                        .filter(|fig| fig.current(now) && !fig.cut())
                        .map(|fig| 100u32.saturating_sub(fig.whole().min(100)))
                        .or_else(|| {
                            acct.sample
                                .as_ref()
                                .and_then(|s| s.week(now))
                                .map(|(pct, _)| 100u32.saturating_sub(pct.min(100)))
                        })
                        .or_else(|| {
                            acct.warned_week
                                .as_ref()
                                .and_then(|w| w.figure(accounts::WEEK_WINDOW_S, now))
                                .map(|(pct, _)| 100u32.saturating_sub(pct.min(100)))
                        })
                })
                .or_else(|| is_active.then_some(session_pct_left).flatten())
                .or_else(|| {
                    matches!(
                        cond,
                        accounts::Condition::Usable
                            | accounts::Condition::Unseen
                            | accounts::Condition::NotEnrolled
                    )
                    .then_some(100)
                })
        };
        let class_word = match row.class {
            accounts::Class::Dedicated => "dedicated",
            accounts::Class::Shared => "pool",
        };
        let avail = match (cond, pct_left) {
            (accounts::Condition::Limited { .. }, _) | (_, Some(0)) => {
                "0% left (limited)".to_string()
            }
            (accounts::Condition::SignedOut, _) => "signed out".to_string(),
            (accounts::Condition::NotEnrolled, Some(pct)) => format!("{pct}% left (auto-login)"),
            (_, Some(pct)) => format!("{pct}% left"),
            (_, None) => cond.as_str().to_string(),
        };
        rows.push(AccountMenuRow {
            label: format!("{} \u{b7} {class_word} \u{b7} {avail}", row.label),
            active: is_active,
            enabled: cond != accounts::Condition::SignedOut || row.email.is_some(),
        });
    }
    (rows, !state.pinned)
}

impl App {
    /// What `session`'s `fact` menu is built from, read once: an event's
    /// read, never a frame's.
    fn fact_menu_facts(&self, session: u64, fact: Fact) -> FactMenuFacts {
        let (place, name) = self.menu_place_and_name(session);
        let what = fact.name();
        let spoken = match &name {
            Some(name) => format!("{what}, {place}, {name}"),
            None => format!("{what}, {place}"),
        };
        let state = self.claude_facts.get(&session);
        let mut out = FactMenuFacts {
            title: format!("{what} \u{b7} {place}"),
            name,
            room: autopilot_menu::MAX_CONTENT,
            spoken,
            off: self.fact_mask(session),
            note: state
                .and_then(|f| f.menu_note.as_ref())
                .filter(|(of, _)| *of == fact)
                .map(|(_, note)| note.clone()),
            typing: state.is_some_and(|f| f.press.is_some()),
            ..FactMenuFacts::default()
        };
        let mut key = false;
        let mut session_acct: Option<String> = None;
        let mut session_email: Option<String> = None;
        let mut session_pct: Option<u32> = None;
        if let Some(entry) = self.pool.get(session) {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(facts) = timeline.claude_footer() {
                let pgid = timeline.agent().program_pgid;
                let latched = state
                    .and_then(|f| f.latch.as_ref())
                    .is_some_and(|latch| latch.holds(pgid, facts.model.as_deref()));
                key = matches!(fact, Fact::Effort | Fact::TopEffort)
                    && !latched
                    && facts
                        .version
                        .as_deref()
                        .is_some_and(crate::claude_slash::takes_key_toggle);
                let facts = facts.filled_from(timeline.claude_card());
                session_acct.clone_from(&facts.account);
                if let Some(sub) = &facts.subscription {
                    session_email.clone_from(&sub.email);
                    let email_str = sub.email.as_deref().filter(|s| !s.is_empty());
                    let plan_str = sub.plan.as_deref().filter(|s| !s.is_empty());
                    let summary = match (email_str, plan_str) {
                        (Some(email), Some(plan)) => format!("{email} \u{b7} {plan}"),
                        (Some(email), None) => email.to_string(),
                        (None, Some(plan)) => plan.to_string(),
                        (None, None) => String::new(),
                    };
                    if !summary.is_empty() {
                        out.subscription = Some(summary);
                    }
                    let weekly = sub.weekly.clone().or_else(|| {
                        facts
                            .usage
                            .as_ref()
                            .and_then(|u| u.wall.as_ref())
                            .filter(|w| w.label == "7d")
                            .map(|_| "weekly 0% left".to_string())
                    });
                    if let Some(weekly) = weekly {
                        session_pct = weekly
                            .split_whitespace()
                            .find_map(|w| w.strip_suffix('%'))
                            .and_then(|n| n.parse::<u32>().ok());
                        out.weekly_meter = Some(weekly_meter_bar(&weekly));
                    }
                }
                (out.model, out.effort, out.path, out.branch, out.context) = (
                    facts.model,
                    facts.effort,
                    facts.path,
                    facts.branch,
                    facts.context,
                );
            }
            if let Some(side) = timeline.claude_side() {
                out.cwd.clone_from(&side.cwd);
                out.repo_root.clone_from(&side.repo_root);
            }
        }
        if fact == Fact::Account {
            (out.accounts, out.auto_rotate) = build_account_menu_rows(
                session_acct.as_deref(),
                session_email.as_deref(),
                session_pct,
            );
        }
        // The key's row says what a choice does from the ENGINE's top rule
        // as it is now: off while Claude draws its tag, else on.
        if key {
            out.key_row = self.engine_top_tag(session).map(|tag| !tag);
        }
        out
    }

    /// `session`'s `fact` menu's rows and its name in words, for a card in
    /// window `wid`: the fact's rows ([`rows`]), or for the usage the Traffic
    /// card ([`traffic_rows`]) from the traffic published beside the facts —
    /// read here, at an open or a wake of the facts, never on a frame.
    fn fact_menu_rows(&self, wid: WindowId, session: u64, fact: Fact) -> (Vec<MenuRow>, String) {
        let pane_cols = self.autopilot_menu_pane(wid, session).1.len();
        if fact != Fact::Usage {
            let facts = FactMenuFacts {
                room: content_room(pane_cols),
                ..self.fact_menu_facts(session, fact)
            };
            return (rows(fact, &facts), facts.spoken);
        }
        #[cfg(test)]
        crate::work_counts::traffic_side_read();
        let room = content_room_up_to(pane_cols, TRAFFIC_MAX_CONTENT);
        let (place, name) = self.menu_place_and_name(session);
        let spoken = match &name {
            Some(name) => format!("Traffic, {place}, {name}"),
            None => format!("Traffic, {place}"),
        };
        let mut out = autopilot_menu::header_rows(
            &format!("{} \u{b7} {place}", fact.name()),
            name.as_deref(),
            room,
        );
        let traffic = self.pool.get(session).and_then(|entry| {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.claude_side().and_then(|side| side.traffic.clone())
        });
        let now_s = crate::autopilot_timers::wall_ms().div_euclid(1000);
        let offset = crate::autopilot_timers::local_offset_at(now_s);
        let view = traffic.unwrap_or_default();
        out.extend(traffic_rows(&view, now_s, offset, room));
        (out, spoken)
    }

    /// The hits of `session`'s `fact` on this frame's glass of window `wid`
    /// that no band covers.
    fn visible_fact_hits(
        &self,
        wid: WindowId,
        session: u64,
        fact: Fact,
    ) -> impl Iterator<Item = &crate::claude_lights::FactHit> {
        self.windows
            .get(&wid)
            .into_iter()
            .flat_map(|ws| ws.claude_lights.fact_hits.iter())
            .filter(move |hit| {
                hit.session == session
                    && hit.fact == fact
                    && !self.chrome_owns_terminal_row(wid, hit.term_row)
            })
    }

    /// Whether this frame's glass shows `session`'s `fact` on frame row `row`
    /// of window `wid`, no band over it: what keeps its open menu, wherever
    /// along the row the fact slid.
    pub(crate) fn shows_fact_on_row(
        &self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        row: usize,
    ) -> bool {
        self.visible_fact_hits(wid, session, fact)
            .any(|hit| hit.frame_row == row)
    }

    /// The first cell of `session`'s `fact` as this frame's glass shows it in
    /// window `wid`, no band over it: where its menu hangs.
    pub(crate) fn fact_anchor(
        &self,
        wid: WindowId,
        session: u64,
        fact: Fact,
    ) -> Option<(usize, usize)> {
        self.visible_fact_hits(wid, session, fact)
            .map(|hit| (hit.frame_row, hit.cols.0))
            .min()
    }

    /// OPEN `session`'s `fact` menu in window `wid` — the usage's Traffic
    /// card — hung from the fact's first cell `anchor`. A keyboard open
    /// lights the first choice; a pointer's lights none (a card has none).
    /// Any menu open elsewhere closes. One frame.
    pub(crate) fn open_fact_menu(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        // A fresh open says no refusal of an earlier choice.
        if let Some(f) = self.claude_facts.get_mut(&session) {
            f.menu_note = None;
        }
        self.open_fact_menu_keeping_note(wid, session, fact, anchor, keyboard);
    }

    /// [`Self::open_fact_menu`], a refusal's note kept.
    pub(crate) fn open_fact_menu_keeping_note(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        self.close_autopilot_menu();
        // A card up again: a double-click's second half is no longer the
        // next press (`App::spends_menu_follow_up`).
        self.autopilot_menu_spent = None;
        let (rows, spoken) = self.fact_menu_rows(wid, session, fact);
        let highlight = keyboard
            .then(|| step(&rows, None, true, rows.len()))
            .flatten();
        let focus = self.focused_session_id(wid);
        let px = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px);
        self.autopilot_menu = Some(AutopilotMenu {
            owner: MenuOwner::Fact(fact),
            wid,
            session,
            focus,
            anchor,
            rows,
            spoken,
            cancels: None,
            highlight,
            rect: None,
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(px),
        });
        self.request_menu_frame(wid);
    }

    /// A press on a fact at window pixel `(x, y)` that asks for its menu (a
    /// right-click, a ctrl-click on macOS): opened, hung from the fact's
    /// first cell. Whether it opened.
    pub(crate) fn open_fact_menu_at(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((session, GlassAt::Fact(fact, _))) = self.claude_glass_at(wid, x, y) else {
            return false;
        };
        let Some(anchor) = self.fact_anchor(wid, session, fact) else {
            return false;
        };
        self.open_fact_menu(wid, session, fact, anchor, false);
        true
    }

    /// THE OPEN FACT MENU FOLLOWS ITS FACTS: `session`'s facts, its side or
    /// its hidden facts moved under it, so its rows are built again — in the
    /// places the shown rows hold (`autopilot_menu::keep_places`), the
    /// highlight kept on a choice still offered in the same words. Nothing
    /// (one `Option` test) unless a fact menu is open on `session`.
    pub(crate) fn refresh_fact_menu(&mut self, session: u64) {
        let Some((wid, fact)) = self
            .autopilot_menu
            .as_ref()
            .and_then(|menu| match menu.owner {
                MenuOwner::Fact(fact) if menu.session == session => Some((menu.wid, fact)),
                _ => None,
            })
        else {
            return;
        };
        let (fresh, spoken) = self.fact_menu_rows(wid, session, fact);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return;
        };
        if menu.refresh_rows(fresh, spoken, None) {
            self.request_menu_frame(wid);
        }
    }

    /// CHOOSE a typed row of `session`'s `fact` menu in window `wid`, the
    /// menu still open, hung from `anchor`: `slash` pressed as the person's
    /// (`App::press_slash`), decided now. Begun, the menu closes; refused,
    /// it stays open — rebuilt under a note naming why, its typed rows
    /// disabled — and the press typed nothing.
    pub(crate) fn choose_fact_type(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        slash: Slash,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        match self.press_slash(wid, session, slash) {
            Ok(()) => {
                if let Some(f) = self.claude_facts.get_mut(&session) {
                    f.menu_note = None;
                }
                self.close_autopilot_menu();
            }
            Err(why) => {
                self.claude_facts.entry(session).or_default().menu_note =
                    Some((fact, why.says("", None)));
                self.open_fact_menu_keeping_note(wid, session, fact, anchor, keyboard);
            }
        }
    }

    /// CHOOSE `act` of a fact menu of `session`, chosen in window `wid` (the
    /// menu already closed): each read from what is published now, never
    /// from the rows' snapshot. Nothing reaches any program — a typed row
    /// goes through [`Self::choose_fact_type`] before the menu closes.
    pub(crate) fn choose_fact_act(&mut self, wid: WindowId, session: u64, act: FactAct) {
        let (side, branch) = self
            .pool
            .get(session)
            .map(|entry| {
                let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                (
                    timeline.claude_side().cloned(),
                    timeline.claude_footer().and_then(|f| f.branch.clone()),
                )
            })
            .unwrap_or_default();
        let dir = |root: bool| {
            side.as_deref()
                .and_then(|s| {
                    if root {
                        s.repo_root.as_ref()
                    } else {
                        s.cwd.as_ref()
                    }
                })
                .map(|p| p.to_string_lossy().into_owned())
        };
        match act {
            FactAct::CopyPath | FactAct::CopyBranch => {
                let text = if act == FactAct::CopyPath {
                    dir(false)
                } else {
                    branch
                };
                if let Some(text) = text
                    && crate::control::pbcopy(&text)
                {
                    self.note_fact_notice(wid, session, FactNotice::Copied);
                }
            }
            FactAct::NewTabHere | FactAct::NewTabAtRoot => {
                if let Some(at) = dir(act == FactAct::NewTabAtRoot) {
                    #[cfg(test)]
                    NEW_TABS_ASKED.with(|asked| asked.borrow_mut().push(at.clone()));
                    self.open_tab_in_cwd(wid, Some(&at));
                    self.note_fact_notice(wid, session, FactNotice::NewTab);
                }
            }
            FactAct::Hide(kind) | FactAct::Show(kind) => {
                let hide = matches!(act, FactAct::Hide(_));
                let off = self.fact_mask(session).with(kind, hide);
                self.set_fact_mask(session, off);
            }
            FactAct::UseAccount(idx) => {
                use aterm_agent::harness::accounts;
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                if let (Some(roster_path), Some(state_dir)) = (
                    accounts::default_roster_path(),
                    crate::harness_host::harness_state(),
                ) {
                    let store = accounts::Store::at(&state_dir);
                    let raw = accounts::Roster::load(&roster_path)
                        .ok()
                        .unwrap_or_default();
                    let roster = store.bound(&raw, now).unwrap_or(raw);
                    if let Some(row) = roster.rows.get(idx).cloned() {
                        let enrolled = accounts::enrolled(&roster);
                        let is_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
                        let mut note = None;
                        if is_enrolled {
                            let _ = store.transact_within(
                                &accounts::LOCK_LADDER,
                                now,
                                &mut note,
                                |state| {
                                    accounts::pin(state, &roster, &enrolled, &row.label, now)
                                        .map(|ev| vec![ev])
                                },
                            );
                        } else if let (crate::harness_host::AccountsUpkeep::Live, Some(home)) = (
                            crate::harness_host::live_accounts_upkeep(),
                            aterm_primer::home_dir(),
                        ) {
                            // A sign-in only an instance run as its account's own starts: never
                            // a test's, whose upkeep is inert by construction.
                            let target_label = row.label.clone();
                            let _ = std::thread::Builder::new()
                                .name("aterm-acct-renew".to_string())
                                .spawn(move || {
                                    aterm_agent::harness::accounts_sync::auto_renew_account_by_label(
                                        &home,
                                        &state_dir,
                                        &roster_path,
                                        &target_label,
                                        true,
                                        now,
                                    );
                                });
                        }
                        self.redraw_claude_footer(session);
                    }
                }
            }
            FactAct::AutoRotate => {
                use aterm_agent::harness::accounts;
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                if let Some(state_dir) = crate::harness_host::harness_state() {
                    let store = accounts::Store::at(&state_dir);
                    let mut note = None;
                    let _ =
                        store.transact_within(&accounts::LOCK_LADDER, now, &mut note, |state| {
                            Ok(accounts::unpin(state, "fact-menu", now)
                                .into_iter()
                                .collect())
                        });
                    // The host's own upkeep arm, inert in every test instance.
                    let upkeep = crate::harness_host::live_accounts_upkeep();
                    let _ = std::thread::Builder::new()
                        .name("aterm-acct-rotate".to_string())
                        .spawn(move || {
                            upkeep.make(aterm_agent::harness::accounts_sync::UpkeepStep::Both);
                        });
                    self.redraw_claude_footer(session);
                }
            }
            // Chosen with the menu open ([`Self::choose_fact_type`]).
            FactAct::Type(_) => {}
        }
    }

    /// `open fact-menu <fact>`: the front window's focused session's `fact`
    /// menu, hung from the fact as this frame's glass shows it, its first
    /// choice lit as for a keyboard open.
    pub(crate) fn open_front_fact_menu(&mut self, fact: Fact) -> Result<(), String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no front window to open a fact menu on".to_string())?;
        let session = self
            .focused_session_id(wid)
            .ok_or_else(|| "the front window has no focused session".to_string())?;
        let anchor = self
            .fact_anchor(wid, session, fact)
            .ok_or_else(|| format!("the focused session shows no {} fact", fact.key()))?;
        self.open_fact_menu(wid, session, fact, anchor, true);
        Ok(())
    }

    /// `open fact-menu close`: the fact menu closed, wherever it is open; an
    /// open Autopilot menu stays.
    pub(crate) fn close_fact_menu(&mut self) {
        if self
            .autopilot_menu
            .as_ref()
            .is_some_and(|menu| matches!(menu.owner, MenuOwner::Fact(_)))
        {
            self.close_autopilot_menu();
        }
    }

    /// `controls fact-menu`: the open fact menu of the front window, or
    /// `fact-menu open=false`.
    #[must_use]
    pub(crate) fn fact_menu_controls(&self) -> Vec<String> {
        match self.autopilot_menu.as_ref().filter(|m| {
            Some(m.wid) == self.frontmost_window && matches!(m.owner, MenuOwner::Fact(_))
        }) {
            Some(menu) => {
                let sid = self
                    .pool
                    .get(menu.session)
                    .map(|s| s.ctx.self_id.as_str().to_string())
                    .unwrap_or_default();
                menu.controls_lines(&sid)
            }
            None => vec!["fact-menu open=false".to_string()],
        }
    }

    /// A screen reader asked for `session`'s `fact` menu on window `wid`
    /// (`Action::ShowContextMenu`): opened as the keyboard opens it, while
    /// the fact is on that window's glass and no band covers it. Whether it
    /// opened.
    #[cfg(a11y_tree)]
    pub(crate) fn open_fact_menu_for_reader(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
    ) -> bool {
        let Some(anchor) = self.fact_anchor(wid, session, fact) else {
            return false;
        };
        self.open_fact_menu(wid, session, fact, anchor, true);
        true
    }
}

#[cfg(test)]
#[path = "fact_menu_tests.rs"]
mod tests;
