// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Context menus and traffic cards for Claude Code footer facts.

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

/// An admitted roster label kept with a menu action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AccountLabel {
    bytes: [u8; aterm_agent::harness::accounts::LABEL_MAX],
    len: u8,
}

impl AccountLabel {
    fn new(label: &str) -> Option<Self> {
        if aterm_agent::harness::accounts::label_refusal(label).is_some() {
            return None;
        }
        let mut bytes = [0; aterm_agent::harness::accounts::LABEL_MAX];
        bytes[..label.len()].copy_from_slice(label.as_bytes());
        Some(Self {
            bytes,
            len: u8::try_from(label.len()).ok()?,
        })
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("an admitted account label is ASCII")
    }

    fn row(
        self,
        roster: &aterm_agent::harness::accounts::Roster,
    ) -> Option<&aterm_agent::harness::accounts::Row> {
        roster.row(self.as_str())
    }
}

/// Action performed when a fact menu row is chosen.
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
    /// Switch/pin onto the account named by the displayed roster row.
    UseAccount(AccountLabel),
    /// Unpin and return to automatic account rotation (dedicated → pool).
    AutoRotate,
    /// Open a tab at the account settings directory (`~/.aterm`).
    AccountSettings,
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
            Self::UseAccount(label) => format!("use/{}", label.as_str()),
            Self::AutoRotate => "auto-rotate".to_string(),
            Self::AccountSettings => "account-settings".to_string(),
        }
    }
}

/// One account row in [`Fact::Account`]'s pulldown menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountMenuRow {
    /// The canonical identity, independent of the formatted label and row order.
    pub(crate) account: AccountLabel,
    /// The row's label on the card (`m27 · dedicated · 0% left (limited)`).
    pub(crate) label: String,
    /// Whether this account is currently active (`●` at the right edge).
    pub(crate) active: bool,
    /// Whether clicking this row can switch to or enrol this account.
    pub(crate) enabled: bool,
}

/// Format a weekly availability string with a 5-cell meter bar.
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

/// Inputs used to build a fact menu.
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
    /// The session's absolute working directory.
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) repo_root: Option<PathBuf>,
    /// Folder / worktree / temp disk hygiene notes for [`Fact::Path`] (`repo_work::RepoHygiene`).
    pub(crate) folder_hygiene: Vec<String>,
    /// Repository hygiene & drift notes for [`Fact::Branch`] (`repo_work::RepoHygiene`).
    pub(crate) git_hygiene: Vec<String>,
    /// Whether the top-effort key toggle row offers to turn the key on (`Some(true)`) or off (`Some(false)`).
    pub(crate) key_row: Option<bool>,
    /// Refusal note shown above disabled typed rows.
    pub(crate) note: Option<String>,
    /// A slash press is in flight for the session: the typed rows wait.
    pub(crate) typing: bool,
}

const HIDEABLE: [FactKind; 5] = [
    FactKind::Model,
    FactKind::Effort,
    FactKind::Path,
    FactKind::Branch,
    FactKind::Context,
];

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

/// Build the menu rows for `fact` from `facts`.
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
            for line in &facts.folder_hygiene {
                out.push(MenuRow::Note(line.clone()));
            }
            let here = facts.cwd.is_some();
            out.push(item(fact, "+", "New tab here", FactAct::NewTabHere, here));
            out.push(item(fact, "\u{29c9}", "Copy path", FactAct::CopyPath, here));
        }
        Fact::Branch => {
            out.push(MenuRow::Note(format!("\u{2387}  {}", value(&facts.branch))));
            for line in &facts.git_hygiene {
                out.push(MenuRow::Note(line.clone()));
            }
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
        Fact::Context => {
            out.push(MenuRow::Note(format!(
                "{}  {}",
                aterm_agent::harness::footer::CONTEXT_MARK,
                value(&facts.context)
            )));
            note(&mut out);
            out.push(item(
                fact,
                "\u{22ef}",
                "Compact context\u{2026}  /compact",
                FactAct::Type(Slash::Compact),
                typed,
            ));
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
                out.push(MenuRow::Item {
                    glyph: "\u{22ef}",
                    label: "Account settings\u{2026}".to_string(),
                    spoken: "Account settings".to_string(),
                    act: MenuAct::Fact(fact, FactAct::AccountSettings),
                    enabled: true,
                    checked: false,
                });
                return out;
            }
            out.push(MenuRow::Rule);
            for acct in &facts.accounts {
                out.push(MenuRow::Item {
                    glyph: "\u{2386}",
                    label: acct.label.clone(),
                    spoken: acct.label.clone(),
                    act: MenuAct::Fact(fact, FactAct::UseAccount(acct.account)),
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
            out.push(MenuRow::Item {
                glyph: "\u{22ef}",
                label: "Account settings\u{2026}".to_string(),
                spoken: "Account settings".to_string(),
                act: MenuAct::Fact(fact, FactAct::AccountSettings),
                enabled: true,
                checked: false,
            });
            return out;
        }
        Fact::Usage => return out,
    }
    out.push(MenuRow::Rule);
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

const LABEL_CELLS: usize = 6;

/// Map `v` against `peak` into 0..=16 eighths across a two-row band.
#[must_use]
pub(crate) fn band_level(v: u64, peak: u64) -> u32 {
    if v == 0 || peak == 0 {
        return 0;
    }
    let level = (u128::from(v) * 16).div_ceil(u128::from(peak)).clamp(1, 16);
    u32::try_from(level).unwrap_or(16)
}

/// Return the `(top, bottom)` block glyphs for a band column at `level`.
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

/// Build the Traffic card's body rows (`cols` content cells wide) from `view`.
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
    /// Folders requested for new tabs in test builds.
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
    account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        session_account,
        session_email,
        session_pct_left,
        now,
    )
}

#[must_use]
pub(crate) fn account_menu_rows_from(
    roster: &aterm_agent::harness::accounts::Roster,
    state: &aterm_agent::harness::accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    session_account: Option<&str>,
    session_email: Option<&str>,
    session_pct_left: Option<u32>,
    now: i64,
) -> (Vec<AccountMenuRow>, bool) {
    use aterm_agent::harness::accounts;
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
        let Some(account) = AccountLabel::new(&row.label) else {
            continue;
        };
        let is_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = accounts::condition(row, state, is_enrolled, now);
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
        let has_email = row.email.is_some();
        let avail = match (cond, pct_left, has_email) {
            (accounts::Condition::Limited { .. }, _, _) | (_, Some(0), _) => {
                "0% left (limited)".to_string()
            }
            (accounts::Condition::SignedOut, _, true) => "signed out (auto-login)".to_string(),
            (accounts::Condition::SignedOut, _, false) => "signed out".to_string(),
            (accounts::Condition::NotEnrolled, Some(pct), true) => {
                format!("{pct}% left (auto-login)")
            }
            (accounts::Condition::NotEnrolled, _, false) => "not enrolled".to_string(),
            (_, Some(pct), _) => format!("{pct}% left"),
            (_, None, _) => cond.as_str().to_string(),
        };
        let enabled = !matches!(
            cond,
            accounts::Condition::SignedOut | accounts::Condition::NotEnrolled
        ) || has_email;
        rows.push(AccountMenuRow {
            account,
            label: format!("{} \u{b7} {class_word} \u{b7} {avail}", row.label),
            active: is_active,
            enabled,
        });
    }
    (rows, !state.pinned)
}

impl App {
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
        if matches!(fact, Fact::Path | Fact::Branch)
            && !cfg!(test)
            && let Some(dir) = out.repo_root.as_deref().or(out.cwd.as_deref())
            && let Some(hygiene) = aterm_agent::harness::repo_work::probe_hygiene(
                dir,
                aterm_primer::home_dir().as_deref(),
                std::time::Duration::from_millis(500),
            )
        {
            if fact == Fact::Path {
                out.folder_hygiene = hygiene.folder_notes();
            } else {
                out.git_hygiene = hygiene.notes();
            }
        }
        if key {
            out.key_row = self.engine_top_tag(session).map(|tag| !tag);
        }
        out
    }

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

    /// Whether window `wid` visibly shows `session`'s `fact` on frame row `row`.
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

    /// Return the first visible `(frame_row, col)` cell of `session`'s `fact` in window `wid`.
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

    /// Open `session`'s `fact` menu in window `wid` anchored at `anchor`.
    pub(crate) fn open_fact_menu(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
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

    /// Open a fact menu at window pixel `(x, y)` in `wid`, returning whether one opened.
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

    /// Rebuild the open fact menu's rows when `session`'s facts change.
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

    /// Execute a typed `slash` row from `session`'s `fact` menu in window `wid`.
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

    /// Execute non-typed action `act` from `session`'s fact menu in window `wid`.
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
            FactAct::UseAccount(label) => {
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
                    if let Some(row) = label.row(&roster).cloned() {
                        let enrolled = accounts::enrolled(&roster);
                        let is_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
                        let mut note = None;
                        let pinned = is_enrolled
                            && store
                                .transact_within(&accounts::LOCK_LADDER, now, &mut note, |state| {
                                    accounts::pin(state, &roster, &enrolled, &row.label, now)
                                        .map(|ev| vec![ev])
                                })
                                .is_ok();
                        if !pinned
                            && row.email.is_some()
                            && let (crate::harness_host::AccountsUpkeep::Live, Some(home)) = (
                                crate::harness_host::live_accounts_upkeep(),
                                aterm_primer::home_dir(),
                            )
                            && let Ok(request) = store.request_pin(&row.label, now)
                        {
                            let worker_request = request.clone();
                            let started = std::thread::Builder::new()
                                .name("aterm-acct-renew".to_string())
                                .spawn(move || {
                                    let worker_store = accounts::Store::at(&state_dir);
                                    let started = aterm_agent::harness::accounts_sync::auto_renew_account_by_label(
                                        &home,
                                        &state_dir,
                                        &roster_path,
                                        worker_request.clone(),
                                        now,
                                    );
                                    if !started {
                                        let now_done = aterm_agent::supervise::limit::unix_now().max(now);
                                        let _ = worker_store.cancel_pin_request(&worker_request, now_done);
                                    }
                                });
                            if started.is_err() {
                                let _ = store.cancel_pin_request(&request, now);
                            }
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
                    let upkeep = crate::harness_host::live_accounts_upkeep();
                    let _ = std::thread::Builder::new()
                        .name("aterm-acct-rotate".to_string())
                        .spawn(move || {
                            upkeep.make(aterm_agent::harness::accounts_sync::UpkeepStep::Both);
                            if matches!(upkeep, crate::harness_host::AccountsUpkeep::Live)
                                && let (Some(home), Some(roster_path)) =
                                    (aterm_primer::home_dir(), accounts::default_roster_path())
                            {
                                let _ = aterm_agent::harness::accounts_sync::auto_renew_next_account_interactive(
                                    &home,
                                    &state_dir,
                                    &roster_path,
                                    now,
                                );
                            }
                        });
                    self.redraw_claude_footer(session);
                }
            }
            FactAct::AccountSettings => {
                if let Some(at) = aterm_agent::harness::accounts::default_roster_path()
                    .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
                {
                    #[cfg(test)]
                    NEW_TABS_ASKED.with(|asked| asked.borrow_mut().push(at.clone()));
                    self.open_tab_in_cwd(wid, Some(&at));
                    self.note_fact_notice(wid, session, FactNotice::NewTab);
                }
            }
            FactAct::Type(_) => {}
        }
    }

    /// Open `fact`'s menu for the focused session of the front window.
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

    /// Close any open fact menu while leaving an open Autopilot menu intact.
    pub(crate) fn close_fact_menu(&mut self) {
        if self
            .autopilot_menu
            .as_ref()
            .is_some_and(|menu| matches!(menu.owner, MenuOwner::Fact(_)))
        {
            self.close_autopilot_menu();
        }
    }

    /// Return `controls fact-menu` lines for the front window.
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

    /// Open `session`'s `fact` menu in window `wid` for a screen reader context-menu action.
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
