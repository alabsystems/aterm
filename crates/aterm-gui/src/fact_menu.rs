// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Context menus and traffic cards for Claude Code footer facts.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::profile::{WeeklyFigure, WeeklyGap};
use aterm_agent::harness::session_usage::{TrafficView, runtime_short, tokens_short};

use crate::autopilot_menu::{
    self, AutopilotMenu, MenuAct, MenuOwner, MenuRow, PlotCell, PlotInk, PointerRest,
    TRAFFIC_MAX_CONTENT, content_room, content_room_up_to, step,
};
use crate::claude_facts::{runtime_spoken, tokens_spoken};
use crate::claude_lights::{Clock, Fact, FactNotice, GlassAt};
use crate::claude_slash::Slash;
use crate::{App, WindowId};

/// Admitted roster label kept with a menu action.
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
    /// Open a tab in the folder of the account roster (`accounts.toml`, beside `aterm.toml`).
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

/// Account row in [`Fact::Account`]'s pulldown menu.
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
    let Some(pct) = weekly_pct(text) else {
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

/// The whole percent weekly words name (`weekly 73% left`, `weekly 61.5% left · 16m ago`: 61),
/// tenths cut as [`whole_left`] cuts them.
fn weekly_pct(text: &str) -> Option<u32> {
    let n = text.split_whitespace().find_map(|w| w.strip_suffix('%'))?;
    let (whole, tenths) = n.split_once('.').unwrap_or((n, ""));
    let whole = whole.parse::<u32>().ok()?;
    let tenths = tenths.bytes().any(|b| (b'1'..=b'9').contains(&b));
    Some(whole_left(whole.saturating_mul(10) + u32::from(tenths)))
}

/// The whole percent left of `left_x10` tenths, the footer's tenths cut, but never to the `0`
/// an account row reads as limited while a tenth is left (`0.5% left`: 1).
fn whole_left(left_x10: u32) -> u32 {
    if left_x10 == 0 {
        0
    } else {
        (left_x10 / 10).max(1)
    }
}

/// The account menu's full why under the footer's short word for a weekly figure it has not
/// got ([`WeeklyGap::words`]), in lines the card's widest content holds.
pub(crate) fn weekly_gap_why(gap: WeeklyGap) -> &'static [&'static str] {
    use aterm_agent::harness::tap::Refusal;
    match gap {
        WeeklyGap::TapOff => &[
            "Claude Code hands the weekly figure only",
            "to its status line, and aterm's usage tap",
            "there is off ([harness] usage_tap, or the",
            "harness is off for Claude Code), so the",
            "harness reads no figure for this one.",
        ],
        WeeklyGap::Unoffered(Refusal::Exe) => &[
            "Claude Code hands the weekly figure only",
            "to its status line, and the usage tap's",
            "command cannot name this aterm (a",
            "temporary copy, or a path with a space),",
            "so the harness reads no figure for it.",
        ],
        WeeklyGap::Unoffered(Refusal::Home) => &[
            "aterm's usage tap rides only on the",
            "machine's own Claude home, and this one",
            "runs under a CLAUDE_CONFIG_DIR or HOME of",
            "its own, so the harness reads no figure.",
        ],
        WeeklyGap::Unoffered(Refusal::Login) => &[
            "aterm's usage tap rides on this account's",
            "current login, and a renewal moved it on",
            "from the login this Claude Code still runs",
            "on, so the harness reads no figure for it.",
        ],
        WeeklyGap::Unoffered(Refusal::Line) => &[
            "aterm's usage tap never rides beside a",
            "session's own --settings or --bare, and",
            "this Claude Code was started with one, so",
            "the harness reads no figure for it.",
        ],
        WeeklyGap::Unoffered(Refusal::Argv) => &[
            "aterm cannot repeat this Claude Code's",
            "options on a relaunch line (one it does",
            "not know, or one that cannot resume), so",
            "no line carries its usage tap and the",
            "harness reads no figure for it.",
        ],
        WeeklyGap::Unoffered(Refusal::Settings) => &[
            "aterm's usage tap carries your own",
            "statusLine from your settings.json, and",
            "never rides with hooks turned off, beside",
            "an apiKeyHelper or a credential, past a",
            "settings file it cannot read, or over a",
            "statusLine set anywhere else; this Claude",
            "Code's settings have one of them, so the",
            "harness reads no figure for it.",
        ],
        WeeklyGap::Untapped => &[
            "This Claude Code was started without",
            "aterm's usage tap, and the next relaunch",
            "aterm makes of it (a live upgrade, a",
            "restart in place) adds it; the week shows",
            "after the first reply that follows.",
        ],
        WeeklyGap::Unrestarted => &[
            "This Claude Code was started without",
            "aterm's usage tap, and aterm relaunches",
            "only a Claude Code it supervises (Autopilot",
            "on) that a relaunch brings back, so the",
            "harness reads no figure for it.",
        ],
        WeeklyGap::NoReply => &[
            "This Claude Code carries aterm's usage",
            "tap: it sends the weekly figure with its",
            "next reply, and no reply since it started",
            "brought one.",
        ],
        WeeklyGap::Stale => &[
            "This Claude Code carries aterm's usage",
            "tap: it sends the weekly figure with each",
            "reply, and no reply in the last hour",
            "brought one.",
        ],
    }
}

/// What the account menu takes from a footer's subscription `sub` at `now` (`walled`: a 7d wall
/// in its transcript): the session's own row figure (as [`row_figure`] gives it), the meter
/// line, and the footer's gap where there is neither. A figure whose week has reset since the
/// footer read it gives no row or meter; a wall's or a screen's words, with no figure behind
/// them, give their own percent.
pub(crate) fn weekly_facts(
    sub: &aterm_agent::harness::profile::SubscriptionDisplay,
    walled: bool,
    now: i64,
) -> (Option<(u32, String)>, Option<String>, Option<WeeklyGap>) {
    let held = session_figure(sub, now);
    let expired = held.is_none() && sub.weekly.is_some() && sub.weekly_figure.is_some();
    let weekly = sub
        .weekly
        .clone()
        .filter(|_| !expired)
        .or_else(|| walled.then(|| "weekly 0% left".to_string()));
    match weekly {
        Some(weekly) => {
            let row = match held {
                Some(figure) => Some(row_figure(figure, now)),
                None => weekly_pct(&weekly).map(|pct| (pct, String::new())),
            };
            (row, Some(weekly_meter_bar(&weekly)), None)
        }
        None => (None, None, sub.weekly_gap),
    }
}

/// The figure behind the footer's weekly words in `sub` while its week still runs at `now`
/// (`None`: a wall's or a screen's words, no figure, or one whose week has reset since): what
/// the session's own account row compares with the store's by when each was read
/// ([`account_menu_rows_from`]).
pub(crate) fn session_figure(
    sub: &aterm_agent::harness::profile::SubscriptionDisplay,
    now: i64,
) -> Option<WeeklyFigure> {
    sub.weekly_figure
        .filter(|figure| sub.weekly.is_some() && figure.resets_at > now)
}

/// An account row's whole percent left and, once the figure is past [`WeeklyFigure::current`],
/// its age (` · 40m ago`, ` · 3h ago`, ` · 2d ago`); a figure stamped ahead of `now` has no age.
fn row_figure(figure: WeeklyFigure, now: i64) -> (u32, String) {
    let left = whole_left(1000u32.saturating_sub(figure.used_x10.min(1000)));
    let age = now.saturating_sub(figure.at);
    if figure.current(now) || age <= 0 {
        return (left, String::new());
    }
    let (n, unit) = if age < 3600 {
        (age / 60, 'm')
    } else if age < 2 * 86_400 {
        (age / 3600, 'h')
    } else {
        (age / 86_400, 'd')
    };
    (left, format!(" \u{b7} {n}{unit} ago"))
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
    /// Why the footer has no weekly figure, where it says ([`weekly_gap_why`]).
    pub(crate) weekly_gap: Option<WeeklyGap>,
    /// The pooled CLI the session runs, whose roster [`Self::accounts`] lists (`None`: Claude
    /// Code's).
    pub(crate) cli: Option<aterm_agent::harness::accounts::Cli>,
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
    /// Whether the top-effort key toggle row offers to turn the key on (`Some(true)`) or off
    /// (`Some(false)`).
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
            out.push(MenuRow::note(note.as_str()));
        }
    };
    match fact {
        Fact::Model => {
            out.push(MenuRow::note(format!("\u{25c6}  {}", value(&facts.model))));
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
            out.push(MenuRow::note(format!("\u{25c9}  {}", value(&facts.effort))));
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
            out.push(MenuRow::note(format!("\u{2302}  {}", value(&facts.path))));
            for line in &facts.folder_hygiene {
                out.push(MenuRow::note(line.as_str()));
            }
            let here = facts.cwd.is_some();
            out.push(item(fact, "+", "New tab here", FactAct::NewTabHere, here));
            out.push(item(fact, "\u{29c9}", "Copy path", FactAct::CopyPath, here));
        }
        Fact::Branch => {
            out.push(MenuRow::note(format!("\u{2387}  {}", value(&facts.branch))));
            for line in &facts.git_hygiene {
                out.push(MenuRow::note(line.as_str()));
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
            out.push(MenuRow::note(format!(
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
                out.push(MenuRow::note(format!(
                    "{}  {sub}",
                    aterm_agent::harness::footer::ACCOUNT_MARK
                )));
            }
            if let Some(meter) = &facts.weekly_meter {
                out.push(MenuRow::note(format!(
                    "{}  {meter}",
                    aterm_agent::harness::footer::WEEKLY_MARK
                )));
            } else if let Some(gap) = facts.weekly_gap {
                out.push(MenuRow::note_spoken(
                    format!(
                        "{}  {}",
                        aterm_agent::harness::footer::WEEKLY_MARK,
                        gap.words()
                    ),
                    gap.spoken(),
                ));
                // The why is prose wrapped to the card: a reader hears it whole,
                // on its first line, not cut at each wrap.
                let why = weekly_gap_why(gap);
                let mut said = Some(why.join(" "));
                for line in why {
                    out.push(MenuRow::note_spoken(*line, said.take().unwrap_or_default()));
                }
            }
            note(&mut out);
            let other = facts
                .cli
                .filter(|cli| *cli != aterm_agent::harness::accounts::Cli::Claude);
            if facts.accounts.is_empty()
                && let Some(cli) = other
            {
                out.push(MenuRow::note(format!(
                    "No {} rotation accounts listed",
                    cli.connector().product
                )));
                out.push(MenuRow::note(format!(
                    "run aterm harness accounts add --cli {} <label>",
                    cli.id()
                )));
                out.push(MenuRow::Rule);
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
            if facts.accounts.is_empty() {
                out.push(MenuRow::note(
                    "No rotation accounts configured \u{2014} run aterm init",
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
        (view.unread > 0).then(|| MenuRow::note(format!("+{} unread not graphed", view.unread)));
    if view.is_empty() || view.width_s <= 0 {
        return std::iter::once(MenuRow::note("No traffic yet"))
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
    let said = format!(
        "{} in, {} out",
        tokens_spoken(total_in),
        tokens_spoken(total_out)
    );
    out.push(match view.runtime_s {
        Some(secs) => MenuRow::note_spoken(
            format!("{} working \u{b7} {tokens}", runtime_short(secs)),
            format!("{} working, {said}", runtime_spoken(secs)),
        ),
        None => MenuRow::note_spoken(tokens, said),
    });
    let per = span_s.div_ceil(u64::try_from(plot).unwrap_or(1));
    // The graph has no words of its own: its caption, the peak, says what
    // it spans for a reader.
    out.push(MenuRow::note_spoken(
        format!(
            "peak \u{2191}{} \u{b7} \u{2193}{} per {}",
            tokens_short(peak.0),
            tokens_short(peak.1),
            runtime_short(per)
        ),
        format!(
            "graph from {} to now, peak {} in, {} out per {}",
            start.as_str(),
            tokens_spoken(peak.0),
            tokens_spoken(peak.1),
            runtime_spoken(per)
        ),
    ));
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
    cli: aterm_agent::harness::accounts::Cli,
    session_account: Option<&str>,
    session_email: Option<&str>,
    session: SessionWeek,
) -> (Vec<AccountMenuRow>, bool) {
    use aterm_agent::harness::accounts;
    if cfg!(test) {
        return (Vec::new(), true);
    }
    if cli != accounts::Cli::Claude {
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(0);
        let (Some(config), Some(state_dir), Some(home)) = (
            aterm_types::dirs::aterm_config_path(),
            crate::harness_host::harness_state(),
            aterm_primer::home_dir(),
        ) else {
            return (Vec::new(), true);
        };
        return pool_menu_rows(
            cli,
            (&config, &state_dir, &home),
            (session_account, session_email),
            now,
        );
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
        session,
        now,
    )
}

/// The account rows of another CLI's pool for a session of it ([`build_account_menu_rows`]): its
/// roster beside `config` (`accounts-<cli>.toml`), bound to the logins its renewals made, each
/// row's condition from its own store (`accounts::Store::at_for`); a row is enrolled once its
/// login file exists (`accounts::enrolled_for`). No roster, or a refused one, is no rows.
#[must_use]
pub(crate) fn pool_menu_rows(
    cli: aterm_agent::harness::accounts::Cli,
    (config, state_dir, home): (&std::path::Path, &std::path::Path, &std::path::Path),
    (session_account, session_email): (Option<&str>, Option<&str>),
    now: i64,
) -> (Vec<AccountMenuRow>, bool) {
    use aterm_agent::harness::accounts;
    let Some(roster) = accounts::roster_beside_for(config, cli)
        .and_then(|path| accounts::Roster::load_for(cli, &path).ok())
    else {
        return (Vec::new(), true);
    };
    let store = accounts::Store::at_for(state_dir, cli, home);
    let roster = store.bound(&roster, now).unwrap_or(roster);
    let (state, _) = store.load(now).unwrap_or_default();
    let enrolled = accounts::enrolled_for(cli, &roster);
    account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        session_account,
        session_email,
        SessionWeek::default(),
        now,
    )
}

/// Selects `label` in `cli`'s pool from the fact menu: pins an eligible row at once, or saves a
/// sign-in request and hands it to `dispatch` when `upkeep_live` holds (cancelling it otherwise).
pub(crate) fn select_menu_account_for(
    cli: aterm_agent::harness::accounts::Cli,
    (roster_path, state_dir, home): (&std::path::Path, &std::path::Path, Option<&std::path::Path>),
    label: &AccountLabel,
    now: i64,
    upkeep_live: bool,
    dispatch: impl FnOnce(
        aterm_agent::harness::accounts::PinRequest,
        std::path::PathBuf,
    ) -> Result<(), ()>,
) -> bool {
    use aterm_agent::harness::accounts;
    let store = match (cli, home) {
        (accounts::Cli::Claude, _) => accounts::Store::at(state_dir),
        (_, Some(home)) => accounts::Store::at_for(state_dir, cli, home),
        (_, None) => return false,
    };
    let Ok(roster) = accounts::Roster::load_for(cli, roster_path) else {
        return false;
    };
    if label.row(&roster).is_none() {
        return false;
    }
    if let Ok(accounts::PinSelection::Renew(request)) =
        store.select_account_for(cli, &roster, label.as_str(), now)
    {
        if let (true, Some(home)) = (upkeep_live, home) {
            if dispatch(request.clone(), home.to_path_buf()).is_err() {
                let _ = store.cancel_pin_request(&request, now);
            }
        } else {
            let _ = store.cancel_pin_request(&request, now);
        }
    }
    true
}

/// The session's own week as the footer holds it, for its own account's row only
/// ([`account_menu_rows_from`]): the row [`Self::label`] names, else the one whose email the
/// session's profile names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionWeek {
    /// The roster label of the account the week is (`SubscriptionDisplay::label`).
    pub(crate) label: Option<String>,
    /// The figure behind the footer's words while its week runs ([`session_figure`]), which the
    /// row takes over the store's where it was read later.
    pub(crate) figure: Option<WeeklyFigure>,
    /// The row's whole percent left and age from the footer's words ([`weekly_facts`]), where
    /// the store holds nothing of the account.
    pub(crate) left: Option<(u32, String)>,
}

#[must_use]
pub(crate) fn account_menu_rows_from(
    roster: &aterm_agent::harness::accounts::Roster,
    state: &aterm_agent::harness::accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    session_account: Option<&str>,
    session_email: Option<&str>,
    session: SessionWeek,
    now: i64,
) -> (Vec<AccountMenuRow>, bool) {
    use aterm_agent::harness::accounts;
    // The session's week is its own account's: the row its login is judged to (on a one-row
    // roster too, where the footer names no account), else the one its profile's email names;
    // never a row whose email contradicts the profile's, nor the store's active fallback.
    let session_row = match session.label.as_deref().or(session_account) {
        Some(label) => roster.row(label).filter(|row| {
            session_email.is_none_or(|email| {
                row.email
                    .as_deref()
                    .is_none_or(|expected| accounts::email_matches(expected, email))
            })
        }),
        None => session_email.and_then(|email| {
            roster.rows.iter().find(|row| {
                row.email
                    .as_deref()
                    .is_some_and(|expected| accounts::email_matches(expected, email))
            })
        }),
    };
    let session_label = session_row.map(|row| row.label.as_str());
    let active_label = session_label.or(state.active.as_deref());
    let mut rows = Vec::with_capacity(roster.rows.len());
    for row in &roster.rows {
        let Some(account) = AccountLabel::new(&row.label) else {
            continue;
        };
        let is_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = accounts::condition(row, state, is_enrolled, now);
        let is_active = active_label == Some(row.label.as_str());
        let pct_left = if matches!(cond, accounts::Condition::Limited { .. }) {
            Some((0, String::new()))
        } else {
            state
                .accounts
                .get(&row.label)
                .and_then(|acct| {
                    let [tapped, sampled] = crate::claude_footer::store_figures(acct, now);
                    let own = session
                        .figure
                        .filter(|_| session_label == Some(row.label.as_str()));
                    WeeklyFigure::newer(WeeklyFigure::newer(tapped, sampled), own)
                        .map(|figure| row_figure(figure, now))
                        .or_else(|| {
                            acct.warned_week
                                .as_ref()
                                .and_then(|w| w.figure(accounts::WEEK_WINDOW_S, now))
                                .map(|(pct, _)| {
                                    (100u32.saturating_sub(pct.min(100)), String::new())
                                })
                        })
                })
                .or_else(|| {
                    (session_label == Some(row.label.as_str()))
                        .then(|| session.left.clone())
                        .flatten()
                })
                .or_else(|| {
                    matches!(
                        cond,
                        accounts::Condition::Usable
                            | accounts::Condition::Unseen
                            | accounts::Condition::NotEnrolled
                    )
                    .then(|| (100, String::new()))
                })
        };
        let class_word = match row.class {
            accounts::Class::Dedicated => "dedicated",
            accounts::Class::Shared => "pool",
        };
        let has_email = row.email.is_some();
        let avail = match (cond, pct_left, has_email) {
            (accounts::Condition::Limited { .. }, _, _) => "0% left (limited)".to_string(),
            (_, Some((0, age)), _) => format!("0% left{age} (limited)"),
            (accounts::Condition::SignedOut, _, true) => "signed out (auto-login)".to_string(),
            (accounts::Condition::SignedOut, _, false) => "signed out".to_string(),
            (accounts::Condition::NotEnrolled, Some((pct, age)), true) => {
                format!("{pct}% left{age} (auto-login)")
            }
            (accounts::Condition::NotEnrolled, _, false) => "not enrolled".to_string(),
            (_, Some((pct, age)), _) => format!("{pct}% left{age}"),
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
    /// The pooled CLI `session` runs ([`crate::harness_host::pool_cli`] of the program its screen
    /// reader names), Claude Code's where it names none.
    pub(crate) fn session_cli(&self, session: u64) -> aterm_agent::harness::accounts::Cli {
        self.pool
            .get(session)
            .and_then(|entry| {
                let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.agent().reader
            })
            .and_then(crate::harness_host::pool_cli)
            .unwrap_or(aterm_agent::harness::accounts::Cli::Claude)
    }

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
        let mut session_week = SessionWeek::default();
        if let Some(entry) = self.pool.get(session) {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let non_claude = match timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of)
            {
                Some(aterm_phase::Program::Claude) | None => false,
                Some(
                    aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse
                    | aterm_phase::Program::Codex
                    | aterm_phase::Program::Grok
                    | aterm_phase::Program::Kimi
                    | aterm_phase::Program::Gemini
                    | aterm_phase::Program::Generic,
                ) => true,
            };
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
                if !non_claude {
                    session_acct.clone_from(&facts.account);
                }
                if let Some(sub) = &facts.subscription {
                    if !non_claude {
                        session_email.clone_from(&sub.email);
                    }
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
                    let walled = facts
                        .usage
                        .as_ref()
                        .and_then(|u| u.wall.as_ref())
                        .is_some_and(|w| w.label == "7d");
                    let now = aterm_time::unix_secs()
                        .and_then(|n| i64::try_from(n).ok())
                        .unwrap_or(0);
                    let (left, weekly_meter, weekly_gap) = weekly_facts(sub, walled, now);
                    out.weekly_meter = weekly_meter;
                    out.weekly_gap = weekly_gap;
                    if !non_claude {
                        session_week.left = left;
                        session_week.figure = session_figure(sub, now);
                        session_week.label.clone_from(&sub.label);
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
            let cli = self.session_cli(session);
            out.cli = Some(cli);
            (out.accounts, out.auto_rotate) = build_account_menu_rows(
                cli,
                session_acct.as_deref(),
                session_email.as_deref(),
                session_week,
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
                let cli = self.session_cli(session);
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                let roster_path = if cli == accounts::Cli::Claude {
                    accounts::default_roster_path()
                } else {
                    aterm_types::dirs::aterm_config_path()
                        .and_then(|cfg| accounts::roster_beside_for(&cfg, cli))
                };
                if let (Some(roster_path), Some(state_dir)) =
                    (roster_path, crate::harness_host::harness_state())
                {
                    let home = aterm_primer::home_dir();
                    let upkeep_live = matches!(
                        crate::harness_host::live_accounts_upkeep(),
                        crate::harness_host::AccountsUpkeep::Live
                    );
                    if select_menu_account_for(
                        cli,
                        (&roster_path, &state_dir, home.as_deref()),
                        &label,
                        now,
                        upkeep_live,
                        |worker_request, worker_home| {
                            let worker_state = state_dir.clone();
                            let worker_roster = roster_path.clone();
                            std::thread::Builder::new()
                                .name("aterm-acct-renew".to_string())
                                .spawn(move || {
                                    let worker_store =
                                        accounts::Store::at_for(&worker_state, cli, &worker_home);
                                    let dispatched = aterm_agent::harness::accounts_sync::auto_renew_account_by_label_for(
                                        cli,
                                        &worker_home,
                                        &worker_state,
                                        &worker_roster,
                                        worker_request.clone(),
                                        now,
                                    );
                                    if dispatched
                                        == aterm_agent::harness::accounts_sync::RenewalDispatch::Failed
                                    {
                                        let now_done =
                                            aterm_agent::supervise::limit::unix_now().max(now);
                                        let _ = worker_store
                                            .cancel_pin_request(&worker_request, now_done);
                                    }
                                })
                                .map(|_| ())
                                .map_err(|_| ())
                        },
                    ) {
                        self.redraw_claude_footer(session);
                    }
                }
            }
            FactAct::AutoRotate => {
                use aterm_agent::harness::accounts;
                let cli = self.session_cli(session);
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                if let Some(state_dir) = crate::harness_host::harness_state() {
                    let home = aterm_primer::home_dir();
                    let store = match (cli, home.as_deref()) {
                        (accounts::Cli::Claude, _) => Some(accounts::Store::at(&state_dir)),
                        (_, Some(h)) => Some(accounts::Store::at_for(&state_dir, cli, h)),
                        (_, None) => None,
                    };
                    if let Some(store) = store {
                        let mut note = None;
                        let _ = store.transact_within(
                            &accounts::LOCK_LADDER,
                            now,
                            &mut note,
                            |state| {
                                Ok(accounts::unpin(state, "fact-menu", now)
                                    .into_iter()
                                    .collect())
                            },
                        );
                        let roster_path = if cli == accounts::Cli::Claude {
                            accounts::default_roster_path()
                        } else {
                            aterm_types::dirs::aterm_config_path()
                                .and_then(|cfg| accounts::roster_beside_for(&cfg, cli))
                        };
                        let upkeep = crate::harness_host::live_accounts_upkeep();
                        let _ = std::thread::Builder::new()
                            .name("aterm-acct-rotate".to_string())
                            .spawn(move || {
                                upkeep.make(aterm_agent::harness::accounts_sync::UpkeepStep::Both);
                                if matches!(upkeep, crate::harness_host::AccountsUpkeep::Live)
                                    && let (Some(home), Some(roster_path)) = (home, roster_path)
                                {
                                    let _ = aterm_agent::harness::accounts_sync::auto_renew_next_account_interactive_for(
                                        cli,
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
