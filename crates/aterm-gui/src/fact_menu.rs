// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Context menus and traffic cards for Claude Code footer facts.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::outlook::Figure;
use aterm_agent::harness::profile::{WEEKLY_OUT, WeeklyFigure, WeeklyGap};
use aterm_agent::harness::session_usage::{TrafficView, runtime_short, tokens_short};

use crate::autopilot_menu::{
    self, AutopilotMenu, MenuAct, MenuOwner, MenuRow, PlotCell, PlotInk, PointerRest, Stop,
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
    /// A choice of the model card: type `/model` with the family's newest model on the session's
    /// build (`crate::claude_facts::ModelPick`; `None`: the build offers none, and the row
    /// cannot be chosen).
    SetModel(
        crate::claude_facts::ModelFamily,
        Option<crate::claude_slash::ModelArg>,
    ),
    /// A stop of the effort slider: type `/effort <level>` ([`App::pick_effort`]).
    SetEffort(&'static str),
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
            Self::SetModel(family, _) => format!("set-model/{}", family.word()),
            Self::SetEffort(level) => format!("set-effort/{level}"),
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
    /// The account's name on the card: its email (its roster label where it has none).
    pub(crate) name: String,
    /// What the row stands at, apart from its name: what is left of its week (`61% left`, an
    /// older reading's upper bound `≤61% left`, a wall's `out`), or why it has none (`signed
    /// out`, `not signed in`, `not read`); never a bare percentage.
    pub(crate) trail: String,
    /// What a screen reader is told of it (`m27@example.com, at most 61 percent left, this
    /// tab's account`).
    pub(crate) spoken: String,
    /// Whether this account is currently active (`●` at the right edge).
    pub(crate) active: bool,
    /// Whether clicking this row can switch to or enrol this account.
    pub(crate) enabled: bool,
}

/// Whether `figure` says the week is spent: a wall, or a reading of 100% (now or before), which
/// an account row says as limited.
fn spent(figure: Figure) -> bool {
    matches!(
        figure,
        Figure::Out | Figure::Exact(1000..) | Figure::AtLeast(1000..)
    )
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

/// What the account menu takes from a footer's subscription `sub` at `now` (`walled`: a 7d wall
/// in its transcript): the session's own row week (as [`row_figure`] gives it), the footer's
/// weekly words, and the footer's gap where there is neither. A figure whose week has reset or whose
/// observation clock is not admitted gives no row figure and no words; a screen's words, with no
/// figure behind them, give the figure they say (`profile::weekly_figure_said`), a wall's ([`WEEKLY_OUT`])
/// [`Figure::Out`].
pub(crate) fn weekly_facts(
    sub: &aterm_agent::harness::profile::SubscriptionDisplay,
    walled: bool,
    now: i64,
) -> (Option<Figure>, Option<String>, Option<WeeklyGap>) {
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
                Some(figure) => row_figure(figure, now),
                None => aterm_agent::harness::profile::weekly_figure_said(&weekly),
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

/// An account row's week of a reading at `now` ([`Figure::of_weekly`]): exact while current, a
/// lower bound of use after (said `≤62% left`, never an age), none once its window reset. Its caller admits
/// observation time first.
fn row_figure(figure: WeeklyFigure, now: i64) -> Option<Figure> {
    Some(Figure::of_weekly(&figure, now)).filter(|figure| *figure != Figure::Unread)
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
    /// The address of the account the footer names (`sol-m27@example.test`).
    pub(crate) email: Option<String>,
    /// Its plan as the footer says it (`Max 20x`).
    pub(crate) plan: Option<String>,
    /// The footer says the program is signed out.
    pub(crate) signed_out: bool,
    /// How the tab's program is signed in, where the footer knows.
    pub(crate) route: Option<aterm_agent::harness::accounts::AuthRoute>,
    /// The session's own week figure, as the footer holds it ([`weekly_facts`]: a reading while
    /// its week runs, a wall's `Out`): the Budget verdict's first source.
    pub(crate) own: Option<aterm_agent::harness::outlook::Figure>,
    /// When that week resets and when it was read, where a reading stands behind it.
    pub(crate) own_reset: Option<i64>,
    pub(crate) own_at: Option<i64>,
    /// The session's own five-hour figure, where its tap read one.
    pub(crate) five_hour: Option<WeeklyFigure>,
    /// The footer's word for extra-usage credits (`SubscriptionDisplay::credits`).
    pub(crate) credits: Option<&'static str>,
    /// The window of the limit the session's transcript shows it stopped at (`5h`, `7d`); the
    /// notice's own words are never quoted.
    pub(crate) wall: Option<&'static str>,
    /// Why the session's recovery permits no forecast, where it refuses one.
    pub(crate) forecast_refusal: Option<aterm_agent::harness::outlook::Refusal>,
    /// How many of this process's tabs run on the account, this one included; `None` where
    /// nothing names this tab's account to count the others by.
    pub(crate) tabs_here: Option<usize>,
    /// What the token read found (`None`: nothing read yet).
    pub(crate) tokens_read: Option<crate::token_shown::TokensRead>,
    /// The account's card, once the account reader answered ([`crate::account_menu_read`]).
    pub(crate) card: Option<aterm_agent::harness::account_card::AccountCard>,
    /// The shown name of the account this Mac's pool is pinned to (empty where it has none).
    pub(crate) pinned: Option<String>,
    /// The local clock's offset from UTC now, in seconds, for a card with no account read.
    pub(crate) offset_s: i64,
    /// The instant the card is built at.
    pub(crate) now: i64,
    /// The program's name in a sentence (`Codex`, `Muse`, `Claude Code`).
    pub(crate) product: &'static str,
    /// Why the footer has no weekly figure, where it says ([`weekly_gap_why`]).
    pub(crate) weekly_gap: Option<WeeklyGap>,
    /// The pooled CLI the session runs, whose roster [`Self::accounts`] lists (`None`: the
    /// program keeps its own login and has no rotation pool).
    pub(crate) cli: Option<aterm_agent::harness::accounts::Cli>,
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
    /// The facts hidden in this session.
    pub(crate) off: FactMask,
    /// The facts the session's program starts with hidden, its own status line printing them
    /// (`claude_facts::default_mask_for`): the folder's, the branch's and the context's cards
    /// hide and show theirs only there.
    pub(crate) starts_off: FactMask,
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
    /// The model card's choices (`crate::claude_facts::model_picks`); `None` where the session
    /// is no Claude Code, whose model card only hides and shows.
    pub(crate) picks: Option<Vec<crate::claude_facts::ModelPick>>,
    /// The effort slider's stops and the one its thumb stands on
    /// (`crate::claude_facts::effort_stops`); `None` where the session is no Claude Code.
    pub(crate) stops: Option<(Vec<&'static str>, Option<usize>)>,
    /// The accounts outlook of Claude Code's pool, from the resolver's snapshot
    /// ([`crate::accounts_outlook::snapshot`]).
    pub(crate) outlook: Option<std::sync::Arc<aterm_agent::harness::outlook::Outlook>>,
    /// The roster label of this session's own account, the outlook's row for "this tab".
    pub(crate) account_row: Option<String>,
    /// The rotation can move this tab ([`aterm_agent::harness::accounts::AuthRoute::on_account`]
    /// of the footer's route, as its glance reads it): else the card's `Forecast` row says so
    /// and projects nothing (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4).
    pub(crate) movable: bool,
    /// This tab's account's vault token, on a Claude Code card
    /// ([`crate::token_shown::shown`]).
    pub(crate) token: Option<crate::token_shown::TokenFact>,
}

const HIDEABLE: [FactKind; 5] = [
    FactKind::Model,
    FactKind::Effort,
    FactKind::Path,
    FactKind::Branch,
    FactKind::Context,
];

/// The model card's choices: one item a pick, the session's own checked, and under a pick the
/// quiet line it carries (how the plan bills Fable).
fn pick_rows(
    out: &mut Vec<MenuRow>,
    fact: Fact,
    picks: &[crate::claude_facts::ModelPick],
    typed: bool,
) {
    for pick in picks {
        out.push(MenuRow::Item {
            glyph: "\u{25c6}",
            label: pick.label.clone(),
            // The one in use is said as itself, checked; the other as the switch it is.
            spoken: if pick.current {
                pick.label.clone()
            } else {
                format!("Switch to {}", pick.label)
            },
            act: MenuAct::Fact(fact, FactAct::SetModel(pick.family, pick.arg)),
            enabled: typed && pick.arg.is_some(),
            checked: pick.current,
            trail: String::new(),
        });
        if let Some(note) = &pick.note {
            out.push(MenuRow::note_spoken(
                format!("   {note}"),
                format!("{}: {note}", pick.label),
            ));
        }
    }
}

/// What the effort card says where the shown model takes none of the slider's levels.
const NO_STOPS: &str = "this model takes no effort level";

/// The effort card's slider: a stop a level, the thumb on `at`.
fn stops_row(fact: Fact, stops: &[&'static str], at: Option<usize>) -> MenuRow {
    let stops: Vec<Stop> = stops
        .iter()
        .map(|level| Stop {
            label: (*level).to_owned(),
            act: MenuAct::Fact(fact, FactAct::SetEffort(level)),
            enabled: true,
        })
        .collect();
    MenuRow::stops("Effort", stops, at)
}

fn item(fact: Fact, glyph: &'static str, label: &str, act: FactAct, enabled: bool) -> MenuRow {
    MenuRow::Item {
        glyph,
        label: label.to_string(),
        spoken: label.to_string(),
        act: MenuAct::Fact(fact, act),
        enabled,
        checked: false,
        trail: String::new(),
    }
}

/// The pin, rotation and usage refresh items' glyph
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.2): `⇄`, never `↻`, which reads as the
/// reset's `↺`.
pub(crate) const PIN_GLYPH: &str = "\u{21c4}";

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
            // A Claude Code session's card is its two choices alone; elsewhere, where nothing
            // types `/model`, it hides and shows.
            if let Some(picks) = &facts.picks {
                pick_rows(&mut out, fact, picks, typed);
                return out;
            }
        }
        Fact::Effort | Fact::TopEffort => {
            out.push(MenuRow::note(format!("\u{25c9}  {}", value(&facts.effort))));
            note(&mut out);
            // The slider stays live under a refusal's note and while a press is in flight: a
            // pick is then said in place, or waits its turn ([`App::pick_effort`]).
            // The slider is the effort's own card's: the top rule's key's card keeps its row.
            match facts.stops.as_ref().filter(|_| fact == Fact::Effort) {
                Some((stops, _)) if stops.is_empty() => out.push(MenuRow::note(NO_STOPS)),
                Some((stops, at)) => out.push(stops_row(fact, stops, *at)),
                None => {}
            }
            // The key's toggle is its own card's: the effort's card is the slider alone, so
            // Return one row down from it types no other command.
            if let Some(on) = facts.key_row.filter(|_| fact == Fact::TopEffort) {
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
        Fact::Account => return account_rows(facts),
        Fact::Usage => return out,
    }
    let own = fact.kind().unwrap_or(FactKind::Effort);
    // A click on the folder, the branch or the context does nothing, so their cards hide
    // nothing either: only where the program's own line prints the fact and the rule starts
    // with it behind a stub does its card put it back there, or show it again.
    let inert = !fact.clicks(false);
    if inert && !facts.starts_off.has(own) {
        return out;
    }
    out.push(MenuRow::Rule);
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
            .filter(|kind| !inert && *kind != own && facts.off.has(*kind))
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

/// What one read of a CLI's pool holds, off the event-loop thread: its bound roster, its judged
/// state, the rows whose login is made on this Mac, and the store they came from. The pool's
/// rows ([`Self::rows`]) and the account's card ([`crate::account_menu_read`]) are both built
/// from this one read, so they never disagree about a row.
pub(crate) struct PoolRead {
    pub(crate) cli: aterm_agent::harness::accounts::Cli,
    pub(crate) store: aterm_agent::harness::accounts::Store,
    pub(crate) roster: aterm_agent::harness::accounts::Roster,
    pub(crate) state: aterm_agent::harness::accounts::State,
    pub(crate) enrolled: std::collections::BTreeSet<String>,
}

impl PoolRead {
    /// Reads `cli`'s roster beside `config`, and its store under `state_dir` (for `home`).
    /// Claude keeps its existing enrollment; other providers use their own default home.
    /// # Errors
    /// The roster, state or login bindings could not be read, or a provider needs an
    /// unavailable home.
    pub(crate) fn load(
        cli: aterm_agent::harness::accounts::Cli,
        (config, state_dir, home): (&std::path::Path, &std::path::Path, Option<&std::path::Path>),
        now: i64,
    ) -> Result<Self, String> {
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
        Ok(Self {
            cli,
            store,
            roster,
            state,
            enrolled,
        })
    }

    /// The pool's rows for a session on `session_account` and `session_email` whose own week
    /// is `week` (Claude Code's alone), with the pin and the queue observation.
    pub(crate) fn rows(
        &self,
        (session_account, session_email): (Option<&str>, Option<&str>),
        week: SessionWeek,
        now: i64,
    ) -> crate::account_menu_read::PoolRows {
        use aterm_agent::harness::accounts;
        let (cli, roster, state) = (self.cli, &self.roster, &self.state);
        let mut rows = account_menu_rows_from(
            roster,
            state,
            &self.enrolled,
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
            roster,
            state,
            &self.enrolled,
            now,
            accounts::manual_signin_capability(),
        );
        crate::account_menu_read::PoolRows {
            rows,
            pinned: state.pinned.then(|| {
                state.active.as_deref().map_or_else(String::new, |label| {
                    roster
                        .row(label)
                        .map_or(label, accounts::Row::shown_name)
                        .to_owned()
                })
            }),
            queue: crate::account_menu_read::queue_status(&self.store, cli, roster, state, now),
        }
    }
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
    /// The row's week as the footer's words say it ([`weekly_facts`]), where the store holds
    /// nothing of the account.
    pub(crate) said: Option<Figure>,
}

/// The Account menu's rows of `roster`, each with its condition and quota and whether it can be
/// chosen. `auto_login` is whether choosing a row with an email starts its sign-in with nobody at
/// the page (Claude Code's, and another CLI's whose renewal approves its page headlessly,
/// `Cli::renews_headlessly`): such
/// a row that is signed out or not signed in can be chosen, and a screen reader is told that
/// choosing it signs it in. Where it is
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
) -> Vec<AccountMenuRow> {
    use aterm_agent::harness::accounts;
    // The session's week is its own account's: the row its login is judged to (on a one-row
    // roster too, where the footer names no account), else the one its profile's email names;
    // never a row whose email contradicts the profile's, nor the store's active fallback.
    let session_row = aterm_agent::harness::account_card::session_row(
        roster,
        session.label.as_deref().or(session_account),
        session_email,
    );
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
        let said_admitted = session_admitted
            && match session.figure {
                Some(_) => own.is_some(),
                None => {
                    barrier.is_none_or(|window| window.epoch_at.is_none())
                        || session.said.is_some_and(spent)
                }
            };
        let week = if matches!(cond, accounts::Condition::Limited { .. }) {
            Some(Figure::Out)
        } else {
            state
                .accounts
                .get(&row.label)
                .and_then(|acct| {
                    let [tapped, sampled] = crate::claude_footer::store_figures(acct, now);
                    WeeklyFigure::newer(WeeklyFigure::newer(tapped, sampled), own)
                        .and_then(|figure| row_figure(figure, now))
                        .or_else(|| {
                            let warning = acct.warned_week.as_ref().filter(|warning| {
                                aterm_agent::harness::profile::usage_read_age(warning.at, now)
                                    .is_some()
                                    && barrier
                                        .is_none_or(|window| window.admits_reading(warning.at))
                            })?;
                            // A warning's share used, as of when it was said: a reading.
                            let (pct, resets_at) = warning.figure(accounts::WEEK_WINDOW_S, now)?;
                            row_figure(
                                WeeklyFigure {
                                    used_x10: pct.min(100) * 10,
                                    resets_at,
                                    at: warning.at,
                                },
                                now,
                            )
                        })
                })
                .or_else(|| {
                    session.said.filter(|figure| {
                        said_admitted
                            && session_label == Some(row.label.as_str())
                            && *figure != Figure::Unread
                    })
                })
        };
        let has_email = auto_login && row.email.is_some();
        // What the row stands at, the first that holds: a wall is `out`, never a percentage;
        // `0% left` only where a reading said 100; a login that is gone or was never made
        // before any figure, which nothing can spend until it is signed in; then the week as
        // what is left, as the glass says it (`harness::left`).
        let (trail, said): (String, String) = match (cond, week) {
            (accounts::Condition::Limited { .. }, _) | (_, Some(Figure::Out)) => {
                ("out".into(), "out".into())
            }
            (_, Some(figure)) if spent(figure) => ("0% left".into(), "0 percent left".into()),
            (accounts::Condition::SignedOut, _) => ("signed out".into(), "signed out".into()),
            (accounts::Condition::NotEnrolled, _) => {
                ("not signed in".into(), "not signed in".into())
            }
            (_, Some(figure)) => (figure.words(), figure.spoken()),
            (_, None) => ("not read".into(), "not read".into()),
        };
        let signs_in = has_email
            && matches!(
                cond,
                accounts::Condition::SignedOut | accounts::Condition::NotEnrolled
            );
        let enabled = !matches!(
            cond,
            accounts::Condition::SignedOut | accounts::Condition::NotEnrolled
        ) || has_email;
        // Named by its email, as the glass names it (§2.5); the label stays the row's key.
        let name = row.shown_name();
        let mut spoken = format!("{name}, {said}");
        if signs_in {
            spoken.push_str(", choosing it signs it in");
        }
        if session_label == Some(row.label.as_str()) {
            spoken.push_str(", this tab's account");
        } else if is_active {
            spoken.push_str(", this Mac's current account");
        }
        rows.push(AccountMenuRow {
            account,
            name: name.to_owned(),
            trail,
            spoken,
            active: is_active,
            enabled,
        });
    }
    rows
}

/// The program's name in a sentence on the Account card: the pooled CLI's, else the program the
/// tab's reader names.
fn product_of(
    cli: Option<aterm_agent::harness::accounts::Cli>,
    program: Option<aterm_phase::Program>,
) -> &'static str {
    use aterm_agent::harness::accounts::Cli;
    match (cli, program) {
        (Some(Cli::Claude), _) => "Claude Code",
        (Some(Cli::Codex), _) => "Codex",
        (Some(Cli::Grok), _) => "Grok",
        (Some(Cli::Kimi), _) => "Kimi",
        (Some(Cli::Agy), _) => "Antigravity",
        (None, Some(aterm_phase::Program::Muse)) => "Muse",
        (None, Some(aterm_phase::Program::Agy)) => "Antigravity",
        // A tab whose reader names no program yet, or one whose pool this tab is not on:
        // the card claims nothing of its login.
        (
            None,
            Some(
                aterm_phase::Program::Claude
                | aterm_phase::Program::Codex
                | aterm_phase::Program::Grok
                | aterm_phase::Program::Kimi
                | aterm_phase::Program::Generic,
            )
            | None,
        ) => "",
    }
}

impl App {
    /// How many live tabs run on `session`'s account, itself included, from each tab's footer
    /// in memory. Another pooled CLI's tabs all run on its store's active row (their footers
    /// name no login), so every tab of that CLI counts. A Claude Code tab counts those whose
    /// footer names the same `email`, or, where either names none, the same roster `label`.
    /// `None` where nothing names the account to count by: the card then claims no count.
    fn tabs_on_account(
        &self,
        session: u64,
        cli: Option<aterm_agent::harness::accounts::Cli>,
        email: Option<&str>,
        label: Option<&str>,
    ) -> Option<usize> {
        use aterm_agent::harness::accounts::{Cli, email_matches};
        let cli = cli?;
        if cli == Cli::Claude && email.is_none() && label.is_none() {
            return None;
        }
        let others = self
            .pool
            .iter()
            .filter(|entry| entry.id != session && self.session_cli(entry.id) == Some(cli))
            .filter(|entry| {
                if cli != Cli::Claude {
                    return true;
                }
                let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                let Some(facts) = timeline.claude_footer() else {
                    return false;
                };
                let sub = facts.subscription.as_ref();
                let same_row = || {
                    label.is_some()
                        && sub
                            .and_then(|sub| sub.label.as_deref())
                            .or(facts.account.as_deref())
                            == label
                };
                // By address where both name one; a tab that names none, by its roster row.
                match (email, sub.and_then(|sub| sub.email.as_deref())) {
                    (Some(email), Some(other)) => email_matches(other, email),
                    _ => same_row(),
                }
            })
            .count();
        Some(others + 1)
    }

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

    pub(crate) fn fact_menu_facts(&self, session: u64, fact: Fact) -> FactMenuFacts {
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
            starts_off: self.starting_fact_mask(session),
            note: state
                .and_then(|f| f.menu_note.as_ref())
                .filter(|(of, _)| *of == fact)
                .map(|(_, note)| note.clone()),
            typing: state.is_some_and(|f| f.press.is_some()),
            picks: (fact == Fact::Model)
                .then(|| self.model_picks(session))
                .flatten(),
            stops: (fact == Fact::Effort)
                .then(|| self.effort_stops(session))
                .flatten(),
            ..FactMenuFacts::default()
        };
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(0);
        out.now = now;
        let mut key = false;
        let mut program: Option<aterm_phase::Program> = None;
        if let Some(entry) = self.pool.get(session) {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            program = timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of);
            if let Some(facts) = timeline.claude_footer() {
                out.forecast_refusal = facts.account_forecast_refusal();
                out.account_progress = facts
                    .account_progress_view()
                    .map(std::borrow::Cow::into_owned);
                let pgid = timeline.agent().program_pgid;
                let latched = state
                    .and_then(|f| f.latch.as_ref())
                    .is_some_and(|latch| latch.holds(pgid, facts.model.as_deref()));
                key = fact == Fact::TopEffort
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
                out.route = facts.route;
                out.wall = facts
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.wall.as_ref())
                    .and_then(|wall| match wall.label {
                        "5h" => Some("5h"),
                        "7d" => Some("7d"),
                        _ => None,
                    });
                if let Some(sub) = &facts.subscription {
                    out.email = sub.email.clone().filter(|email| !email.is_empty());
                    out.plan = sub.plan.clone().filter(|plan| !plan.is_empty());
                    out.signed_out = sub.signed_out;
                    out.five_hour = sub.five_hour;
                    out.credits = sub.credits;
                    let (own, _, weekly_gap) = weekly_facts(sub, out.wall == Some("7d"), now);
                    out.own = own;
                    out.weekly_gap = weekly_gap;
                    let held = session_figure(sub, now);
                    out.own_reset = held.map(|figure| figure.resets_at);
                    out.own_at = held.map(|figure| figure.at);
                } else if out.wall == Some("7d") {
                    // A weekly wall in the transcript says the week whatever else is known.
                    out.own = Some(Figure::Out);
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
            let claude = cli == Some(aterm_agent::harness::accounts::Cli::Claude);
            out.product = product_of(cli, program);
            out.offset_s = crate::autopilot_timers::local_offset_at(now).unwrap_or(0);
            // The outlook the resolver last posted: no file is read on this thread. A recovery
            // that permits no forecast is said on the card's `Forecast` row, and the outlook's
            // observations are read as they are.
            out.outlook = claude
                .then(crate::accounts_outlook::snapshot)
                .flatten()
                .and_then(|s| s.outlook.clone());
            let tokens = claude.then(crate::token_shown::shown).flatten();
            out.tokens_read = tokens.as_ref().map(|tokens| tokens.read);
            out.token = tokens.and_then(|tokens| {
                tokens
                    .of(out.account_row.as_deref(), out.email.as_deref())
                    .cloned()
            });
            // Muse Code alone has a usage figure a press reads again.
            out.refresh_usage = match program {
                Some(aterm_phase::Program::Muse) => true,
                Some(
                    aterm_phase::Program::Claude
                    | aterm_phase::Program::Codex
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Grok
                    | aterm_phase::Program::Kimi
                    | aterm_phase::Program::Generic,
                )
                | None => false,
            };
            out.tabs_here = self.tabs_on_account(
                session,
                cli,
                out.email.as_deref(),
                out.account_row.as_deref(),
            );
            if cli.is_some() {
                match self.account_menu_rows_cached(session) {
                    Some(read) => {
                        out.card.clone_from(&read.card);
                        match &read.pool {
                            Ok(pool) => {
                                out.accounts.clone_from(&pool.rows);
                                out.pinned.clone_from(&pool.pinned);
                                out.account_queue.clone_from(&pool.queue);
                            }
                            Err(why) => out.accounts_refused = Some(why.clone()),
                        }
                    }
                    None => out.accounts_loading = true,
                }
            }
            // The recovery's words name its accounts as the card's items do, by email (§2.5).
            if let Some(progress) = out.account_progress.as_mut() {
                let (card, outlook) = (out.card.as_ref(), out.outlook.as_deref());
                *progress = progress.named(|label| {
                    card.and_then(|card| card.names.get(label).cloned())
                        .or_else(|| outlook.map(|outlook| outlook.name_of(label).to_owned()))
                        .unwrap_or_else(|| label.to_owned())
                });
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
        let (_, pane_cols) = self.autopilot_menu_pane(wid, session);
        let pane_cols = pane_cols.len();
        if fact != Fact::Usage {
            let facts = self.fact_menu_facts(session, fact);
            // The Account card is up to 60 cells wide, as the traffic card (§8.1); it takes the
            // width only beside a chart (`account_rows`).
            let facts = FactMenuFacts {
                room: if fact == Fact::Account {
                    content_room_up_to(pane_cols, TRAFFIC_MAX_CONTENT)
                } else {
                    content_room(pane_cols)
                },
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
                    && !self.chrome_owns_terminal_span(wid, hit.term_row, hit.cols.0..hit.cols.1)
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
        // A keyboard open of the Account card selects its first item. While the account
        // reader has not answered, that item is not on the card yet: the selection waits for
        // the first result ([`crate::account_menu_read`]) and does not settle on the one
        // item a card still being read has.
        let pending = fact == Fact::Account
            && self.session_cli(session).is_some()
            && self.account_menu_rows_cached(session).is_none();
        let highlight = (keyboard && !pending)
            .then(|| step(&rows, None, true))
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
            highlight,
            viewport: crate::menu_viewport::MenuViewport::default(),
            page_rows: 0,
            wheel_residual: 0.0,
            wheel_residual_x: 0.0,
            rect: None,
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(px),
            told: None,
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
            .filter(|menu| menu.session == session)
            .and_then(|menu| match menu.owner {
                MenuOwner::Fact(fact) => Some((menu.wid, fact)),
                MenuOwner::Permission => None,
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
            .filter(|menu| menu.session == session)
            .and_then(|menu| match menu.owner {
                MenuOwner::Fact(fact) => Some((menu.wid, fact)),
                MenuOwner::Permission => None,
            })
        else {
            return;
        };
        let (fresh, spoken) = self.fact_menu_rows(wid, session, fact);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return;
        };
        if menu.refresh_rows(fresh, spoken) {
            self.request_menu_frame(wid);
        }
    }

    /// Execute a typing row (`act`: a [`FactAct::Type`] or [`FactAct::SetModel`]) from
    /// `session`'s `fact` menu in window `wid`.
    pub(crate) fn choose_fact_type(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        act: FactAct,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        let pressed = match act {
            FactAct::Type(slash) => self.press_slash(wid, session, slash),
            FactAct::SetModel(_, Some(model)) => {
                self.press_slash(wid, session, Slash::Model(model))
            }
            _ => return,
        };
        match pressed {
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

    /// Pick a slider's stop (`act`: a [`FactAct::SetEffort`]) on `session`'s `fact` card in
    /// window `wid`, which stays open: the thumb moves to the pick at once and then follows
    /// the footer, and a refusal is the card's note in place.
    pub(crate) fn choose_fact_stop(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        act: FactAct,
    ) {
        let FactAct::SetEffort(level) = act else {
            return;
        };
        let refused = self
            .pick_effort(wid, session, level)
            .err()
            .map(|why| (fact, why.says("", None)));
        self.claude_facts.entry(session).or_default().menu_note = refused;
        self.rebuild_fact_menu_rows(session);
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
                // Only what the card offers now: a row left on a stale card copies nothing.
                let facts = self.fact_menu_facts(session, Fact::Account);
                let offered = facts.account_progress.is_some()
                    || (facts.cli == Some(aterm_agent::harness::accounts::Cli::Claude)
                        && facts.email.is_some());
                if offered && crate::control::pbcopy(&details_report(&facts)) {
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
                self.open_network_lens_in(wid);
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
            FactAct::Type(_) | FactAct::SetModel(..) | FactAct::SetEffort(_) => {}
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

    /// Close any open fact menu (`open fact-menu close`).
    pub(crate) fn close_fact_menu(&mut self) {
        self.close_autopilot_menu();
    }

    /// Return `controls fact-menu` lines for the front window.
    #[must_use]
    pub(crate) fn fact_menu_controls(&self) -> Vec<String> {
        match self
            .autopilot_menu
            .as_ref()
            .filter(|m| Some(m.wid) == self.frontmost_window)
        {
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

#[path = "fact_menu_account.rs"]
mod account;
pub(crate) use account::{account_rows, details_report};

#[cfg(test)]
#[path = "fact_menu_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "independent_work_consumer_tests.rs"]
mod independent_work_tests;
