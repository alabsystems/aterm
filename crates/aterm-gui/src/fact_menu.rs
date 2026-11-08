// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Context menus and traffic cards for Claude Code footer facts.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::profile::{WEEKLY_OUT, WeeklyFigure, WeeklyGap};
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
    /// Copy the complete observed account-recovery report, including clipped notes.
    CopyRecovery,
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
    /// Request this machine's CLI-pool pin release and normal upkeep, without changing
    /// rotation mode or the session's Autopilot.
    AutoRotate,
    /// Open a terminal tab in aterm's configuration folder.
    AccountSettings,
    /// Inspect this instance's network and shared-service availability.
    NetworkState,
    /// Forget the cached Muse usage reading so the next read mints a fresh one.
    RefreshUsage,
    /// The quiet offer's one step for this tab's account: a tab beside this one that runs
    /// `aterm harness accounts token <label> [--replace]`
    /// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9).
    Token,
}

impl FactAct {
    /// The act's part of its row's stable key (`fact-menu/<fact>/<act>`).
    #[must_use]
    pub(crate) fn key(self) -> String {
        match self {
            Self::CopyPath => "copy-path".to_string(),
            Self::NewTabHere => "new-tab-here".to_string(),
            Self::CopyBranch => "copy-branch".to_string(),
            Self::CopyRecovery => "copy-recovery".to_string(),
            Self::NewTabAtRoot => "new-tab-at-root".to_string(),
            Self::Hide(kind) => format!("hide/{}", Fact::of(kind).key()),
            Self::Show(kind) => format!("show/{}", Fact::of(kind).key()),
            Self::Type(slash) => format!("type/{}", slash.key()),
            Self::UseAccount(label) => format!("use/{}", label.as_str()),
            Self::AutoRotate => "auto-rotate".to_string(),
            Self::AccountSettings => "account-settings".to_string(),
            Self::NetworkState => "network-state".to_string(),
            Self::RefreshUsage => "refresh-usage".to_string(),
            Self::Token => "token".to_string(),
        }
    }
}

/// Account row in [`Fact::Account`]'s pulldown menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountMenuRow {
    /// The canonical identity, independent of the formatted label and row order.
    pub(crate) account: AccountLabel,
    /// The row's label on the card (`m27 · preferred · 61% left`; a wall's `out (limited)`).
    pub(crate) label: String,
    /// Whether this account is currently active (`●` at the right edge).
    pub(crate) active: bool,
    /// Whether clicking this row can switch to or enrol this account.
    pub(crate) enabled: bool,
}

/// The whole percent left of the share weekly words say used (`weekly 27% used`: 73; a bound,
/// `weekly ≥27% used`, gives the most that can be left: 73), as [`whole_left`] says it; `None`
/// for words with no figure (a wall's `weekly out`, a gap's).
fn weekly_pct(text: &str) -> Option<u32> {
    let used = aterm_agent::harness::profile::weekly_figure_words(text)?
        .trim_start_matches('\u{2265}')
        .strip_suffix('%')?;
    let used = used
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| used.parse::<u32>().ok())
        .flatten()?;
    Some(whole_left(1000u32.saturating_sub(used.min(100) * 10)))
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
        WeeklyGap::Unoffered(Refusal::Room) => &[
            "The line aterm would type to relaunch",
            "this Claude Code has no room left on the",
            "tty for its usage tap (a long directory",
            "or many options), so a relaunch would",
            "not add it and the harness reads no",
            "figure for it.",
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
        WeeklyGap::ClockInvalid => &[
            "The weekly figure was read, but its",
            "observation time is unconfirmed on this",
            "clock. It is not shown as current.",
        ],
    }
}

/// An account row's week on the Account menu ([`account_menu_rows_from`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowWeek {
    /// Its whole percent left and, once past [`WeeklyFigure::current`], its age (` · 40m ago`).
    Left(u32, String),
    /// A standing wall: `out (limited)`, never a percentage (never an inferred 100% used).
    Out,
}

/// What the account menu takes from a footer's subscription `sub` at `now` (`walled`: a 7d wall
/// in its transcript): the session's own row week (as [`row_figure`] gives it), the footer's
/// weekly words, and the footer's gap where there is neither. A figure whose week has reset or whose
/// observation clock is not admitted gives no row or meter; a screen's words, with no figure
/// behind them, give their own percent, and a wall's ([`WEEKLY_OUT`]) [`RowWeek::Out`].
pub(crate) fn weekly_facts(
    sub: &aterm_agent::harness::profile::SubscriptionDisplay,
    walled: bool,
    now: i64,
) -> (Option<RowWeek>, Option<String>, Option<WeeklyGap>) {
    let held = session_figure(sub, now);
    let expired = held.is_none() && sub.weekly.is_some() && sub.weekly_figure.is_some();
    let weekly = sub
        .weekly
        .clone()
        .filter(|_| !expired)
        .or_else(|| walled.then(|| WEEKLY_OUT.to_string()));
    match weekly {
        Some(weekly) => {
            let row = match held {
                Some(figure) => Some(row_figure(figure, now)),
                None if weekly == WEEKLY_OUT => Some(RowWeek::Out),
                None => weekly_pct(&weekly).map(|pct| RowWeek::Left(pct, String::new())),
            };
            (row, Some(weekly), None)
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
    sub.weekly_figure.filter(|figure| {
        sub.weekly.is_some()
            && figure.resets_at > now
            && aterm_agent::harness::profile::usage_read_age(figure.at, now).is_some()
    })
}

/// An account row's whole percent left and, once the figure is past [`WeeklyFigure::current`],
/// its age (` · 40m ago`, ` · 3h ago`, ` · 2d ago`). Its caller admits observation time first.
fn row_figure(figure: WeeklyFigure, now: i64) -> RowWeek {
    let left = whole_left(1000u32.saturating_sub(figure.used_x10.min(1000)));
    let age = now.saturating_sub(figure.at);
    if figure.current(now) || age <= 0 {
        return RowWeek::Left(left, String::new());
    }
    let (n, unit) = if age < 3600 {
        (age / 60, 'm')
    } else if age < 2 * 86_400 {
        (age / 3600, 'h')
    } else {
        (age / 86_400, 'd')
    };
    RowWeek::Left(left, format!(" \u{b7} {n}{unit} ago"))
}

/// Inputs used to build a fact menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FactMenuFacts {
    /// The actual session recovery observation, independent of the rotation-policy checkmark.
    pub(crate) account_progress: Option<aterm_agent::harness::account_progress::AccountProgress>,
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
    /// The footer's weekly words (`weekly 39% used`; a wall's `weekly out`), which a card with
    /// no outlook says.
    pub(crate) weekly: Option<String>,
    /// The session's own week figure while its week runs ([`session_figure`]): the work row's
    /// lead on the Accounts card, as on line E (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
    /// §2.2, one figure at now).
    pub(crate) own: Option<aterm_agent::harness::outlook::Figure>,
    /// Why the footer has no weekly figure, where it says ([`weekly_gap_why`]).
    pub(crate) weekly_gap: Option<WeeklyGap>,
    /// The pooled CLI the session runs, whose roster [`Self::accounts`] lists (`None`: the
    /// program keeps its own login and has no rotation pool).
    pub(crate) cli: Option<aterm_agent::harness::accounts::Cli>,
    /// Where a program with no rotation pool keeps its login, for the account menu's note.
    pub(crate) own_login: Option<&'static str>,
    /// The account menu offers to mint a fresh Muse usage reading.
    pub(crate) refresh_usage: bool,
    /// The rotation accounts in roster order (dedicated first, then pool).
    pub(crate) accounts: Vec<AccountMenuRow>,
    /// Why the local pool cannot safely supply choices.
    pub(crate) accounts_refused: Option<String>,
    /// Account choices are being read off the event-loop thread.
    pub(crate) accounts_loading: bool,
    /// The exact saved manual selection's last observed queue place, without an ETA.
    pub(crate) account_queue: Option<String>,
    /// Manual queue capability is unavailable; this does not disable existing-account pins.
    pub(crate) manual_signin_unavailable: bool,
    /// This CLI pool on this machine has no committed account pin (`!state.pinned`).
    /// A pending selection may still exist; this is not session recovery authority.
    pub(crate) pool_unpinned: bool,
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
    /// The accounts outlook of Claude Code's pool, from the resolver's snapshot
    /// ([`crate::accounts_outlook::snapshot`]).
    pub(crate) outlook: Option<std::sync::Arc<aterm_agent::harness::outlook::Outlook>>,
    /// The roster label of this session's own account, the outlook's row for "this tab".
    pub(crate) account_row: Option<String>,
    /// The rotation can move this tab ([`aterm_agent::harness::accounts::AuthRoute::on_account`]
    /// of the footer's route, as its glance reads it): else the card projects nothing and says
    /// no verdict (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4).
    pub(crate) movable: bool,
    /// The pane's rows the card may take, where its height is known.
    pub(crate) height: Option<usize>,
    /// This tab's account's vault token, on a Claude Code card
    /// ([`crate::token_shown::shown`]).
    pub(crate) token: Option<crate::token_shown::TokenFact>,
}

/// The card's token rows (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9): its state,
/// value-free, and the quiet offer's one step where it stands.
fn token_rows(out: &mut Vec<MenuRow>, fact: Fact, token: &crate::token_shown::TokenFact) {
    out.push(MenuRow::note(format!("vault token: {}", token.words)));
    if let Some(offer) = &token.offer {
        out.push(item(fact, "+", &offer.line, FactAct::Token, true));
    }
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

/// A roster row's item on the Accounts card: it chooses `acct` ([`FactAct::UseAccount`]), its
/// check and its enabling the store's, its words `label` and `spoken`.
fn account_item(fact: Fact, acct: &AccountMenuRow, label: String, spoken: String) -> MenuRow {
    MenuRow::Item {
        glyph: "\u{2386}",
        label,
        spoken,
        act: MenuAct::Fact(fact, FactAct::UseAccount(acct.account)),
        enabled: acct.enabled,
        checked: acct.active,
    }
}

/// The account card's last item, on every path the card takes (§1.4): one label, so a
/// selection kept across a refresh stays on it.
fn settings_item(fact: Fact) -> MenuRow {
    MenuRow::Item {
        glyph: "\u{22ef}",
        label: "Account settings\u{2026}".to_string(),
        spoken: "Account settings: open the configuration folder".to_string(),
        act: MenuAct::Fact(fact, FactAct::AccountSettings),
        enabled: true,
        checked: false,
    }
}

/// The pin, rotation and usage refresh items' glyph
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.2): `⇄`, never `↻`, which reads as the
/// reset's `↺`.
pub(crate) const PIN_GLYPH: &str = "\u{21c4}";

const fn plot_ink(ink: aterm_agent::harness::outlook::CardInk) -> PlotInk {
    use aterm_agent::harness::outlook::CardInk;
    match ink {
        CardInk::Read => PlotInk::Read,
        CardInk::Ahead => PlotInk::Ahead,
        CardInk::Mark => PlotInk::Mark,
        CardInk::Track => PlotInk::Track,
        CardInk::Warn => PlotInk::Warn,
        CardInk::Value => PlotInk::Value,
        CardInk::Axis => PlotInk::Axis,
    }
}

/// The outlook's card rows ([`aterm_agent::harness::outlook::Outlook::card_rows`]) as menu
/// rows, in the card's order (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4): its
/// notes, plots and rules as they come, and each roster row's item where the card puts it (the
/// named accounts with their lanes, then the folded ones under a rule) with the act, the check
/// and the enabling of [`FactMenuFacts::accounts`]. An item and its lane are one unit: a card
/// account the store does not list draws neither. A row the store lists and the outlook has no
/// words for keeps the store's words, after the card's own items. With them, whether the rows
/// end on the card's legend, which closes the drawn rows in place of a rule.
fn outlook_rows(
    fact: Fact,
    outlook: &aterm_agent::harness::outlook::Outlook,
    facts: &FactMenuFacts,
    height: Option<usize>,
) -> (Vec<MenuRow>, bool) {
    use aterm_agent::harness::outlook::CardRow;
    let tab = facts.account_row.as_deref().unwrap_or_default();
    let mut card = outlook.card_rows(tab, facts.own, facts.movable, facts.room, height);
    // The card is 60 cells wide only beside a drawn row (`autopilot_menu::max_content`).
    if facts.room > autopilot_menu::MAX_CONTENT
        && !card.iter().any(|row| matches!(row, CardRow::Plot { .. }))
    {
        card = outlook.card_rows(
            tab,
            facts.own,
            facts.movable,
            autopilot_menu::MAX_CONTENT,
            height,
        );
    }
    let mut legend_last = card.last().is_some_and(CardRow::is_legend);
    let plot = |cells: Vec<(char, aterm_agent::harness::outlook::CardInk)>, spoken: String| {
        MenuRow::plot(
            cells
                .into_iter()
                .map(|(ch, ink)| PlotCell {
                    ch,
                    ink: plot_ink(ink),
                    busy: false,
                })
                .collect::<Vec<_>>(),
            spoken,
        )
    };
    let mut out = Vec::with_capacity(card.len() + 1);
    let mut placed: Vec<&str> = Vec::new();
    for row in card {
        match row {
            CardRow::Note {
                text,
                spoken,
                warn,
                value,
            } => out.push(MenuRow::Note {
                spoken: spoken.unwrap_or_else(|| autopilot_menu::spoken_words(&text)),
                text,
                tone: if warn {
                    autopilot_menu::NoteTone::Warn
                } else if value {
                    autopilot_menu::NoteTone::Value
                } else {
                    autopilot_menu::NoteTone::Label
                },
            }),
            CardRow::Plot { cells, spoken } => out.push(plot(cells, spoken)),
            CardRow::Rule => out.push(MenuRow::Rule),
            CardRow::Account {
                label,
                text,
                spoken,
                lane,
            } => {
                let Some(acct) = facts
                    .accounts
                    .iter()
                    .find(|acct| acct.account.as_str() == label)
                else {
                    continue;
                };
                placed.push(acct.account.as_str());
                out.push(account_item(fact, acct, text, spoken));
                if let Some(lane) = lane {
                    out.push(plot(lane.cells, lane.spoken));
                }
            }
        }
    }
    let rest: Vec<&AccountMenuRow> = facts
        .accounts
        .iter()
        .filter(|acct| !placed.contains(&acct.account.as_str()))
        .collect();
    if !rest.is_empty() {
        legend_last = false;
        // After the legend, the store's own rows sit under a rule of their own.
        if !matches!(out.last(), Some(MenuRow::Rule)) {
            out.push(MenuRow::Rule);
        }
        for acct in rest {
            out.push(account_item(
                fact,
                acct,
                acct.label.clone(),
                acct.label.clone(),
            ));
        }
    }
    (out, legend_last)
}

fn recovery_observed_line(at: i64) -> String {
    if at > 0 {
        format!("Observed {}", aterm_agent::harness::usage::rfc3339_utc(at))
    } else {
        "Observed unknown".into()
    }
}

fn recovery_next_line(at: Option<i64>) -> String {
    at.filter(|at| *at > 0).map_or_else(
        || "Next event unknown".into(),
        |at| {
            format!(
                "Next event {}",
                aterm_agent::harness::usage::rfc3339_utc(at)
            )
        },
    )
}

fn pool_work_observed_line(at: i64) -> String {
    if at > 0 {
        format!(
            "Pool work observed {}",
            aterm_agent::harness::usage::rfc3339_utc(at)
        )
    } else {
        "Pool work observation time unknown".into()
    }
}

/// Full observed recovery evidence, without the menu's display-width limit.
fn recovery_report(progress: &aterm_agent::harness::account_progress::AccountProgress) -> String {
    let count = |value: Option<usize>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
    let mut lines = vec![
        format!(
            "{}: {}",
            if progress.independent_work.is_empty() {
                "Recovery"
            } else {
                "Recorded decision"
            },
            aterm_agent::harness::footer::account_progress_phase(progress)
        ),
        format!(
            "Account: {}",
            progress.account.as_deref().unwrap_or("unknown")
        ),
        format!(
            "Accounts: {}; eligible: {}",
            count(progress.known),
            count(progress.eligible)
        ),
        format!(
            "Limit: {}",
            progress.wall.as_deref().unwrap_or("not observed")
        ),
    ];
    for work in &progress.independent_work {
        lines.push(aterm_agent::harness::footer::account_pool_work_text(work));
        lines.push(pool_work_observed_line(work.observed_at));
    }
    lines.extend(progress.details.iter().cloned());
    for diagnostic in &progress.login_diagnostics {
        lines.push(format!(
            "Last local sign-in for {}: {}",
            diagnostic.label(),
            diagnostic.words(),
        ));
        lines.push(format!(
            "Attempt finished at {}",
            aterm_agent::harness::usage::rfc3339_utc(diagnostic.completed_at),
        ));
    }
    lines.push(recovery_observed_line(progress.observed_at));
    lines.push(recovery_next_line(progress.next_at));
    lines.join("\n")
}

fn wrapped_note_rows(out: &mut Vec<MenuRow>, words: &str, room: usize) {
    wrapped_note_rows_marked(out, words, room, "-");
}

fn wrapped_note_rows_marked(out: &mut Vec<MenuRow>, words: &str, room: usize, mark: &str) {
    let width = if room == 0 {
        autopilot_menu::MAX_CONTENT
    } else {
        room.min(autopilot_menu::MAX_CONTENT)
    };
    let wrapped = crate::native_settings::wrap_to_box_marked(
        words,
        width as f32,
        &|text| crate::tab_menu::display_cells(text) as f32,
        mark,
    );
    let mut spoken = Some(words);
    for line in wrapped {
        out.push(MenuRow::note_spoken(
            line,
            spoken.take().unwrap_or_default(),
        ));
    }
}

fn login_diagnostic_rows(
    out: &mut Vec<MenuRow>,
    diagnostic: &aterm_agent::harness::accounts_sync::LoginDiagnostic,
    room: usize,
) {
    wrapped_note_rows_marked(
        out,
        &format!("Last local sign-in: {}", diagnostic.label()),
        room,
        "",
    );
    wrapped_note_rows(out, &diagnostic.words(), room);
    out.push(MenuRow::note(format!(
        "Finished {}",
        aterm_agent::harness::usage::rfc3339_utc(diagnostic.completed_at),
    )));
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
            if let Some(progress) = &facts.account_progress {
                let mut summary = progress.clone();
                summary.details.clear();
                summary.independent_work.clear();
                summary.next_at = summary.next_at.filter(|at| *at > 0);
                let summary = aterm_agent::harness::footer::account_progress_text(&summary);
                out.push(MenuRow::note(if progress.independent_work.is_empty() {
                    summary
                } else {
                    format!("Recorded decision: {summary}")
                }));
                out.push(item(
                    fact,
                    "\u{29c9}",
                    "Copy recovery details",
                    FactAct::CopyRecovery,
                    true,
                ));
                if let Some(limit) = &progress.wall {
                    out.push(MenuRow::note(format!("Limit: {limit}")));
                }
                if progress.phase == aterm_agent::harness::account_progress::ProgressPhase::Watching
                    && progress.wall.is_none()
                {
                    out.push(MenuRow::note(
                        aterm_agent::harness::footer::account_progress_phase(progress),
                    ));
                }
                if let Some(detail) = progress.details.first() {
                    out.push(MenuRow::note(detail.clone()));
                }
                // Keep timing visible before a long list of retained route details.
                out.push(MenuRow::note(recovery_observed_line(progress.observed_at)));
                out.push(MenuRow::note(recovery_next_line(progress.next_at)));
                for work in &progress.independent_work {
                    wrapped_note_rows(
                        &mut out,
                        &aterm_agent::harness::footer::account_pool_work_text(work),
                        facts.room,
                    );
                    out.push(MenuRow::note(pool_work_observed_line(work.observed_at)));
                }
                let mut diagnostics: Vec<_> = progress.login_diagnostics.iter().collect();
                diagnostics.sort_by_key(|diagnostic| std::cmp::Reverse(diagnostic.completed_at));
                for diagnostic in diagnostics {
                    login_diagnostic_rows(&mut out, diagnostic, facts.room);
                }
                for detail in progress.details.iter().skip(1) {
                    out.push(MenuRow::note(detail.clone()));
                }
                out.push(MenuRow::Item {
                    glyph: "⋯",
                    label: "Network State…".into(),
                    spoken: "Network State and shared-service access".into(),
                    act: MenuAct::Fact(fact, FactAct::NetworkState),
                    enabled: true,
                    checked: false,
                });
                out.push(MenuRow::Rule);
            }
            // Under the outlook each item says its plan, and the email leaves the glass
            // (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.5).
            if let Some(sub) = facts
                .subscription
                .as_ref()
                .filter(|_| facts.outlook.is_none())
            {
                out.push(MenuRow::note(format!(
                    "{}  {sub}",
                    aterm_agent::harness::footer::ACCOUNT_MARK
                )));
            }
            if let Some(token) = &facts.token {
                token_rows(&mut out, fact, token);
            }
            // Under the outlook the work row and the lanes say the week
            // (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4); without one the week's
            // words lead with no mark of their own: `◷` left the glass (§4.2).
            if let Some(weekly) = facts.weekly.as_ref().filter(|_| facts.outlook.is_none()) {
                out.push(MenuRow::note(format!("   {weekly}")));
            } else if let Some(gap) = facts.weekly_gap {
                out.push(MenuRow::note_spoken(
                    format!("   {}", gap.words()),
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
            let Some(cli) = facts.cli else {
                if let Some(own) = facts.own_login {
                    out.push(MenuRow::note(own.to_string()));
                }
                if facts.refresh_usage {
                    out.push(item(
                        fact,
                        PIN_GLYPH,
                        "Refresh usage",
                        FactAct::RefreshUsage,
                        true,
                    ));
                }
                return out;
            };
            if facts.accounts_loading {
                out.push(MenuRow::note("Loading account choices…"));
                return out;
            }
            if !facts.manual_signin_unavailable
                && let Some(queue) = &facts.account_queue
            {
                out.push(MenuRow::note(queue.clone()));
            }
            if facts.manual_signin_unavailable {
                out.push(MenuRow::note("Manual sign-in is unavailable on this platform; signed-in accounts can still be selected."));
            }
            if let Some(why) = &facts.accounts_refused {
                out.push(MenuRow::note(format!("Account state unavailable: {why}")));
                out.push(settings_item(fact));
                return out;
            }
            let other = (cli != aterm_agent::harness::accounts::Cli::Claude).then_some(cli);
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
                out.push(settings_item(fact));
                return out;
            }
            if facts.accounts.is_empty() {
                out.push(MenuRow::note(
                    "No rotation accounts configured \u{2014} run aterm init",
                ));
                out.push(MenuRow::Rule);
                out.push(MenuRow::Item {
                    glyph: PIN_GLYPH,
                    label: "Sync accounts & clear this machine’s pin".to_string(),
                    spoken: format!(
                        "Sync accounts and clear this machine's {} pool pin",
                        cli.connector().product
                    ),
                    act: MenuAct::Fact(fact, FactAct::AutoRotate),
                    enabled: !facts.manual_signin_unavailable,
                    checked: false,
                });
                out.push(settings_item(fact));
                return out;
            }
            let mut legend_last = false;
            match &facts.outlook {
                Some(outlook) => {
                    // The rows below the head, the auto-rotate and the settings items.
                    let height = facts.height.map(|h| h.saturating_sub(out.len() + 3));
                    let (card, legend) = outlook_rows(fact, outlook, facts, height);
                    out.extend(card);
                    legend_last = legend;
                }
                None => {
                    out.push(MenuRow::Rule);
                    for acct in &facts.accounts {
                        out.push(account_item(
                            fact,
                            acct,
                            acct.label.clone(),
                            acct.label.clone(),
                        ));
                    }
                }
            }
            // The card's legend closes its drawn rows; the pin and settings items follow it
            // (§1.4), as they follow a rule under anything else, a lane included.
            if !legend_last {
                out.push(MenuRow::Rule);
            }
            out.push(MenuRow::Item {
                glyph: PIN_GLYPH,
                label: "No pinned account · this machine".to_string(),
                spoken: format!(
                    "No pinned account in this machine's {} pool",
                    cli.connector().product
                ),
                act: MenuAct::Fact(fact, FactAct::AutoRotate),
                enabled: !facts.manual_signin_unavailable,
                checked: facts.pool_unpinned,
            });
            out.push(settings_item(fact));
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
            // The graph's caption is the peak note below it, for every row of it.
            out.push(MenuRow::plot(cells, String::new()));
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
    out.push(MenuRow::plot(axis, String::new()));
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

/// Reads the pool's roster, judged state and queue observation off the event-loop thread.
/// Claude keeps its existing enrollment and session-week projection; other providers use
/// their own default home and headless sign-in policy.
/// # Errors
/// The roster, state or login bindings could not be read, or a provider needs an unavailable home.
pub(crate) fn pool_menu_rows(
    cli: aterm_agent::harness::accounts::Cli,
    (config, state_dir, home): (&std::path::Path, &std::path::Path, Option<&std::path::Path>),
    (session_account, session_email): (Option<&str>, Option<&str>),
    week: SessionWeek,
    now: i64,
) -> crate::account_menu_read::ResultRows {
    use aterm_agent::harness::accounts;
    let path =
        accounts::roster_beside_for(config, cli).ok_or("account roster path is unavailable")?;
    let roster = accounts::Roster::load_for(cli, &path)?;
    let store = match (cli, home) {
        (_, Some(home)) => accounts::Store::at_for(state_dir, cli, home),
        (accounts::Cli::Claude, None) => accounts::Store::at(state_dir),
        (_, None) => return Err("account home is unavailable".into()),
    };
    let roster = store.bound(&roster, now)?;
    let (state, _) = store.load_judged(now, &roster)?;
    let enrolled = if cli == accounts::Cli::Claude {
        accounts::enrolled(&roster)
    } else {
        accounts::enrolled_in(cli, &roster, home.ok_or("account home is unavailable")?)
    };
    let (mut rows, unpinned) = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        cli == accounts::Cli::Claude || cli.renews_headlessly(),
        session_account,
        session_email,
        if cli == accounts::Cli::Claude {
            week
        } else {
            SessionWeek::default()
        },
        now,
    );
    apply_manual_signin_capability(
        &mut rows,
        &roster,
        &state,
        &enrolled,
        now,
        accounts::manual_signin_capability(),
    );
    let queue = crate::account_menu_read::queue_status(&store, cli, &roster, &state, now);
    Ok((rows, unpinned, queue))
}

fn apply_manual_signin_capability(
    rows: &mut [AccountMenuRow],
    roster: &aterm_agent::harness::accounts::Roster,
    state: &aterm_agent::harness::accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    now: i64,
    capability: aterm_agent::harness::accounts::ManualSigninCapability,
) {
    use aterm_agent::harness::accounts;
    if capability == accounts::ManualSigninCapability::Unavailable {
        for entry in rows {
            if let Some(row) = roster.row(entry.account.as_str())
                && matches!(
                    accounts::condition(row, state, enrolled.contains(&row.label), now),
                    accounts::Condition::SignedOut | accounts::Condition::NotEnrolled
                )
            {
                entry.enabled = false;
            }
        }
    }
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
        (_, Some(home)) => accounts::Store::at_for(state_dir, cli, home),
        (accounts::Cli::Claude, None) => accounts::Store::at(state_dir),
        (_, None) => return false,
    };
    let Ok(roster) = accounts::Roster::load_for(cli, roster_path) else {
        return false;
    };
    if label.row(&roster).is_none() {
        return false;
    }
    let selection = match store.select_account_for(cli, &roster, roster_path, label.as_str(), now) {
        Ok(selection) => selection,
        Err(_) => return false,
    };
    if let accounts::PinSelection::Renew(request) = selection {
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
    /// The row's week from the footer's words ([`weekly_facts`]), where the store holds
    /// nothing of the account.
    pub(crate) left: Option<RowWeek>,
}

/// The Account menu's rows of `roster`, each with its condition and quota and whether it can be
/// chosen. `auto_login` is whether choosing a row with an email starts its sign-in with nobody at
/// the page (Claude Code's, and another CLI's whose renewal approves its page headlessly,
/// `Cli::renews_headlessly`): such
/// a row that is signed out or not enrolled says `(auto-login)` and can be chosen. Where it is
/// false (Grok Build's, whose background renewal never starts) that row is as one with no email:
/// it names its condition and cannot be chosen, since a choice would start no sign-in.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "menu rows bind the roster, its state and enrolled set, whether rows auto-login, the session's account, email and week, and the clock"
)]
pub(crate) fn account_menu_rows_from(
    roster: &aterm_agent::harness::accounts::Roster,
    state: &aterm_agent::harness::accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    auto_login: bool,
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
                row.expected_identity_email()
                    .is_none_or(|expected| accounts::email_matches(expected, email))
            })
        }),
        None => session_email.and_then(|email| {
            roster.rows.iter().find(|row| {
                row.expected_identity_email()
                    .is_some_and(|expected| accounts::email_matches(expected, email))
            })
        }),
    };
    let session_label = session_row.map(|row| row.label.as_str());
    let session_admitted = session.figure.is_none_or(|figure| {
        figure.resets_at > now
            && aterm_agent::harness::profile::usage_read_age(figure.at, now).is_some()
    });
    let active_label = session_label.or(state.active.as_deref());
    let mut rows = Vec::with_capacity(roster.rows.len());
    for row in &roster.rows {
        let Some(account) = AccountLabel::new(&row.label) else {
            continue;
        };
        let is_enrolled = enrolled.contains(&row.label);
        let cond = accounts::condition(row, state, is_enrolled, now);
        let is_active = active_label == Some(row.label.as_str());
        let barrier = state
            .accounts
            .get(&row.label)
            .and_then(|account| account.windows.get(accounts::WEEK_LIMIT));
        let own = session.figure.filter(|figure| {
            session_admitted
                && session_label == Some(row.label.as_str())
                && barrier.is_none_or(|window| window.admits_reading(figure.at))
        });
        let left_admitted = session_admitted
            && match session.figure {
                Some(_) => own.is_some(),
                None => {
                    barrier.is_none_or(|window| window.epoch_at.is_none())
                        || matches!(session.left, Some(RowWeek::Out | RowWeek::Left(0, _)))
                }
            };
        let pct_left = if matches!(cond, accounts::Condition::Limited { .. }) {
            Some(RowWeek::Out)
        } else {
            state
                .accounts
                .get(&row.label)
                .and_then(|acct| {
                    let [tapped, sampled] = crate::claude_footer::store_figures(acct, now);
                    WeeklyFigure::newer(WeeklyFigure::newer(tapped, sampled), own)
                        .map(|figure| row_figure(figure, now))
                        .or_else(|| {
                            acct.warned_week
                                .as_ref()
                                .filter(|warning| {
                                    aterm_agent::harness::profile::usage_read_age(warning.at, now)
                                        .is_some()
                                        && barrier
                                            .is_none_or(|window| window.admits_reading(warning.at))
                                })
                                .and_then(|w| w.figure(accounts::WEEK_WINDOW_S, now))
                                .map(|(pct, _)| {
                                    RowWeek::Left(
                                        100u32.saturating_sub(pct.min(100)),
                                        String::new(),
                                    )
                                })
                        })
                })
                .or_else(|| {
                    (left_admitted && session_label == Some(row.label.as_str()))
                        .then(|| session.left.clone())
                        .flatten()
                })
        };
        let class_word = match row.class {
            accounts::Class::Preferred => "preferred",
            accounts::Class::Pool => "pool",
        };
        let has_email = auto_login && row.email.is_some();
        // A wall is `out`, never a percentage; `0% left` only where a reading said 100% used.
        let avail = match (cond, pct_left, has_email) {
            (accounts::Condition::Limited { .. }, _, _) | (_, Some(RowWeek::Out), _) => {
                "out (limited)".to_string()
            }
            (_, Some(RowWeek::Left(0, age)), _) => format!("0% left{age} (limited)"),
            (accounts::Condition::SignedOut, _, true) => "signed out (auto-login)".to_string(),
            (accounts::Condition::SignedOut, _, false) => "signed out".to_string(),
            (accounts::Condition::NotEnrolled, Some(RowWeek::Left(pct, age)), true) => {
                format!("{pct}% left{age} (auto-login)")
            }
            (accounts::Condition::NotEnrolled, _, false) => "not enrolled".to_string(),
            (_, Some(RowWeek::Left(pct, age)), _) => format!("{pct}% left{age}"),
            (accounts::Condition::NotEnrolled, None, true) => {
                format!("{}; quota unknown (auto-login)", cond.as_str())
            }
            (_, None, _) => format!("{}; quota unknown", cond.as_str()),
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
    pub(crate) fn session_cli(&self, session: u64) -> Option<aterm_agent::harness::accounts::Cli> {
        self.pool.get(session).and_then(|entry| {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline
                .agent()
                .reader
                .and_then(crate::harness_host::pool_cli)
        })
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
            manual_signin_unavailable: aterm_agent::harness::accounts::manual_signin_capability()
                == aterm_agent::harness::accounts::ManualSigninCapability::Unavailable,
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
        let mut forecast_refusal = None;
        let mut program: Option<aterm_phase::Program> = None;
        let mut email: Option<String> = None;
        if let Some(entry) = self.pool.get(session) {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            program = timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of);
            if let Some(facts) = timeline.claude_footer() {
                forecast_refusal = facts.account_forecast_refusal();
                out.account_progress = facts
                    .account_progress_view()
                    .map(std::borrow::Cow::into_owned);
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
                out.movable = facts
                    .route
                    .is_some_and(aterm_agent::harness::accounts::AuthRoute::on_account);
                // The roster row this tab's login is judged to, the outlook's "this tab".
                out.account_row = facts
                    .subscription
                    .as_ref()
                    .and_then(|s| s.label.clone())
                    .or_else(|| facts.account.clone());
                if let Some(sub) = &facts.subscription {
                    email.clone_from(&sub.email);
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
                    let (_, weekly, weekly_gap) = weekly_facts(sub, walled, now);
                    out.weekly = weekly;
                    out.weekly_gap = weekly_gap;
                    out.own = session_figure(sub, now)
                        .map(|f| aterm_agent::harness::outlook::Figure::of_weekly(&f, now));
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
            out.cli = cli;
            // The outlook the resolver last posted (`docs/DESIGN-accounts-outlook-2026-10-25.md`
            // §10): no file is read on this thread, and the rows stay the account reader's.
            out.outlook = (cli == Some(aterm_agent::harness::accounts::Cli::Claude))
                .then(crate::accounts_outlook::snapshot)
                .flatten()
                .and_then(|s| s.outlook.clone())
                .map(|outlook| {
                    if let Some(refusal) = forecast_refusal {
                        std::sync::Arc::new(outlook.observations_only(refusal))
                    } else {
                        outlook
                    }
                });
            out.token = (cli == Some(aterm_agent::harness::accounts::Cli::Claude))
                .then(crate::token_shown::shown)
                .flatten()
                .and_then(|tokens| {
                    tokens
                        .of(out.account_row.as_deref(), email.as_deref())
                        .cloned()
                });
            (out.refresh_usage, out.own_login) = match program {
                Some(aterm_phase::Program::Muse) => (
                    true,
                    Some("Muse's own login — no rotation pool; switch with its /login"),
                ),
                Some(aterm_phase::Program::Agy) => {
                    (false, Some("Antigravity's own login — no rotation pool"))
                }
                Some(
                    aterm_phase::Program::Claude
                    | aterm_phase::Program::Codex
                    | aterm_phase::Program::Grok
                    | aterm_phase::Program::Kimi
                    | aterm_phase::Program::Generic,
                )
                | None => (false, None),
            };
            if cli.is_some() {
                match self.account_menu_rows_cached(session) {
                    Some(Ok((accounts, pool_unpinned, queue))) => {
                        out.accounts.clone_from(accounts);
                        out.pool_unpinned = *pool_unpinned;
                        out.account_queue.clone_from(queue);
                    }
                    Some(Err(why)) => out.accounts_refused = Some(why.clone()),
                    None => out.accounts_loading = true,
                }
            }
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
        let (pane_rows, pane_cols) = self.autopilot_menu_pane(wid, session);
        let pane_cols = pane_cols.len();
        if fact != Fact::Usage {
            let facts = self.fact_menu_facts(session, fact);
            // The Accounts card under the outlook is up to 60 cells wide, as the traffic card
            // (§8.1), and as tall as the pane less its border.
            let facts = FactMenuFacts {
                room: if facts.outlook.is_some() {
                    content_room_up_to(pane_cols, TRAFFIC_MAX_CONTENT)
                } else {
                    content_room(pane_cols)
                },
                height: facts
                    .outlook
                    .is_some()
                    .then(|| pane_rows.len().saturating_sub(2)),
                ..facts
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
        self.clear_strip_press(wid);
        self.settle_rename_edit(wid);
        self.close_autopilot_menu();
        self.autopilot_menu_spent = None;
        if fact == Fact::Account {
            // The resolver looks at the store at its next read and wakes the card on a new
            // snapshot; the card opens on the one it holds (§10).
            crate::accounts_outlook::card_open(Some(session));
            self.request_account_read(session);
        }
        let (rows, spoken) = self.fact_menu_rows(wid, session, fact);
        let highlight = keyboard.then(|| step(&rows, None, true)).flatten();
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
            viewport: crate::menu_viewport::MenuViewport::default(),
            page_rows: 0,
            wheel_residual: 0.0,
            wheel_residual_x: 0.0,
            rect: None,
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(px),
        });
        if fact == Fact::Account {
            self.account_menu_read
                .defer_keyboard_selection(keyboard && highlight.is_none());
            self.prepare_account_menu_read(wid, session);
        }
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
        if fact == Fact::Account {
            self.prepare_account_menu_read(wid, session);
        }
        self.rebuild_fact_menu_rows(session);
    }

    /// Apply cached facts without scheduling another account read from a completion.
    pub(crate) fn rebuild_fact_menu_rows(&mut self, session: u64) {
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
            // A control client's choice (its `key enter` on the open menu) copies nothing.
            FactAct::CopyRecovery | FactAct::CopyPath | FactAct::CopyBranch
                if self.refuse_clipboard_gesture("fact-menu copy") => {}
            FactAct::CopyRecovery => {
                if let Some(progress) = self
                    .fact_menu_facts(session, Fact::Account)
                    .account_progress
                    && crate::control::pbcopy(&recovery_report(&progress))
                {
                    self.note_fact_notice(wid, session, FactNotice::Copied);
                }
            }
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
                let Some(scope) = self.account_work.enter() else {
                    return;
                };
                use aterm_agent::harness::accounts;
                let Some(cli) = self.session_cli(session) else {
                    return;
                };
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
                        crate::harness_host::live_accounts_upkeep(self.account_work.clone()),
                        crate::harness_host::AccountsUpkeep::Live(_)
                    );
                    if select_menu_account_for(
                        cli,
                        (&roster_path, &state_dir, home.as_deref()),
                        &label,
                        now,
                        upkeep_live,
                        |worker_request, worker_home| {
                            let scope = scope.clone();
                            let worker_state = state_dir.clone();
                            let worker_roster = roster_path.clone();
                            std::thread::Builder::new()
                                .name("aterm-acct-renew".to_string())
                                .spawn(scope.clone().wrap(move || {
                                    let worker_store =
                                        accounts::Store::at_for(&worker_state, cli, &worker_home);
                                    let dispatched = aterm_agent::harness::accounts_sync::manual_renew_account_for_scoped(
                                        scope,
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
                                }))
                                .map(|_| ())
                                .map_err(|_| ())
                        },
                    ) {
                        self.account_menu_selection_changed();
                        self.redraw_claude_footer(session);
                    }
                }
            }
            FactAct::AutoRotate => {
                if aterm_agent::harness::accounts::manual_signin_capability()
                    == aterm_agent::harness::accounts::ManualSigninCapability::Unavailable
                {
                    return;
                }
                let Some(scope) = self.account_work.enter() else {
                    return;
                };
                use aterm_agent::harness::accounts;
                let Some(cli) = self.session_cli(session) else {
                    return;
                };
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                if let Some(state_dir) = crate::harness_host::harness_state() {
                    let home = aterm_primer::home_dir();
                    let store = match (cli, home.as_deref()) {
                        (_, Some(h)) => Some(accounts::Store::at_for(&state_dir, cli, h)),
                        (accounts::Cli::Claude, None) => Some(accounts::Store::at(&state_dir)),
                        (_, None) => None,
                    };
                    if let Some(store) = store {
                        let roster_path = if cli == accounts::Cli::Claude {
                            accounts::default_roster_path()
                        } else {
                            aterm_types::dirs::aterm_config_path()
                                .and_then(|cfg| accounts::roster_beside_for(&cfg, cli))
                        };
                        let live = matches!(
                            crate::harness_host::live_accounts_upkeep(self.account_work.clone()),
                            crate::harness_host::AccountsUpkeep::Live(_)
                        );
                        if let Ok(request) = store.request_next(now) {
                            self.account_menu_selection_changed();
                            if let (true, Some(home), Some(roster_path)) = (live, home, roster_path)
                            {
                                let worker_request = request.clone();
                                let spawned = std::thread::Builder::new()
                                    .name("aterm-acct-rotate".to_string())
                                    .spawn(scope.clone().wrap(move || {
                                        let worker_store = accounts::Store::at_for(&state_dir, cli, &home);
                                        let dispatched = aterm_agent::harness::accounts_sync::manual_renew_next_for_scoped(
                                            scope, cli, &home, &state_dir, &roster_path, worker_request.clone(), now,
                                        );
                                        if dispatched == aterm_agent::harness::accounts_sync::RenewalDispatch::Failed {
                                            let now_done = aterm_agent::supervise::limit::unix_now().max(now);
                                            let _ = worker_store.cancel_next_request(&worker_request, now_done);
                                        }
                                    }));
                                if spawned.is_err() {
                                    let _ = store.cancel_next_request(&request, now);
                                }
                            } else {
                                let _ = store.cancel_next_request(&request, now);
                            }
                        }
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
            FactAct::NetworkState => {
                self.open_network_lens();
            }
            FactAct::Token => {
                // The offer as the card showed it, read again at the press: one that no longer
                // stands opens nothing.
                if let Some(token) = self.fact_menu_facts(session, Fact::Account).token
                    && let Some(offer) = &token.offer
                {
                    self.open_token_step(wid, offer);
                    self.note_fact_notice(wid, session, FactNotice::NewTab);
                }
            }
            FactAct::RefreshUsage => {
                let email = self.pool.get(session).and_then(|entry| {
                    let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    timeline
                        .claude_footer()
                        .and_then(|facts| facts.subscription.as_ref())
                        .and_then(|sub| sub.email.clone())
                });
                if let Some(email) = email.as_deref() {
                    aterm_agent::harness::profile::muse_usage_forget(email);
                    self.redraw_claude_footer(session);
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

#[cfg(test)]
#[path = "independent_work_consumer_tests.rs"]
mod independent_work_tests;
