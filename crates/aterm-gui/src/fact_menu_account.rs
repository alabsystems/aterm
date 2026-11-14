// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Account card's rows: the account a tab runs on, answered in the order a person asks.
//!
//! Under the head the card states three things, one row each: whether the vault holds a token
//! for the account (`Token`), what is left of its budget (`Budget`), and which Mac is its home
//! and whether it is in use there (`Machine`). The sections that follow give each answer's
//! detail in the same order, then this Mac's other accounts and what automatic switching does
//! for the tab. A verdict and its section are built by one function, so they cannot disagree.
//!
//! Two rules hold for every row, at every width the card is given:
//! - **Nothing is cut.** A value, a sentence, a name or an item's words longer than the room is
//!   wrapped over rows ([`wrap_cells`]); no row is wider than the room, and the only `…` on the
//!   card end the two items that open something (`Network State…`, `Account settings…`).
//! - **Unknown is said as unknown.** A fact the card could not read says so and why in a few
//!   words (`not read yet`, `not checked on this Mac`, `not known: …`); it is never said as
//!   `none`, and a week figure always says `left`.
//!
//! [`account_rows`] is pure: it reads what [`super::FactMenuFacts`] holds, which the window's
//! thread fills from memory alone (the footer's facts, the resolver's snapshots and the card
//! the account reader built off-thread, [`crate::account_menu_read`]).

use aterm_agent::harness::account_card::LoginHalf;
use aterm_agent::harness::account_card::LoginHere;
use aterm_agent::harness::account_card::{
    AccountCard, Claims, CreditsWord, DayPoint, HistoryFrom, Holder, Home, JournalHeld, Mac,
    NoTable, Seen, Who,
};
use aterm_agent::harness::account_progress::{AccountProgress, OwnClaimBlocker, ProgressPhase};
use aterm_agent::harness::accounts::{AuthRoute, Cli};
use aterm_agent::harness::ai_pool::SlotWhy;
use aterm_agent::harness::outlook::{self, Figure, Offsets, Read, Window};
use aterm_agent::harness::token_state::TokenState;

use super::{AccountMenuRow, FactAct, FactMenuFacts, PIN_GLYPH, band_glyphs, band_level};
use crate::autopilot_menu::{
    FACT_LABEL_CELLS, MAX_CONTENT, MenuAct, MenuRow, NoteTone, PlotCell, PlotInk,
};
use crate::claude_lights::Fact;
use crate::tab_menu::display_cells;
use crate::token_shown::TokensRead;

/// The least room the card draws a chart in: the name column and three cells a day of eight.
const CHART_ROOM: usize = 33;
/// The least room a fact's name and value share a row in; under it the value goes below.
const FACT_ROOM: usize = 21;
/// The cells an item's mark takes before its words, and its check after them.
const ITEM_LEAD: usize = 3;
const ITEM_CHECK: usize = 2;
const DAY: i64 = 86_400;

/// Words as the card shows them and as a screen reader is told them, in parts a ` · ` joins on
/// the card and a comma in speech.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Words {
    parts: Vec<(String, String)>,
}

impl Words {
    /// One part, said as it is shown.
    fn of(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            parts: vec![(text.clone(), text)],
        }
    }

    /// One part shown as `text` and said as `spoken`.
    fn said(text: impl Into<String>, spoken: impl Into<String>) -> Self {
        Self {
            parts: vec![(text.into(), spoken.into())],
        }
    }

    /// These words, then `more` as further parts.
    fn and(mut self, more: Words) -> Self {
        self.parts.extend(more.parts);
        self
    }

    /// The last part with `more`'s first joined onto it by a blank, then the rest of `more`.
    fn then(mut self, more: Words) -> Self {
        let mut more = more.parts.into_iter();
        match (self.parts.last_mut(), more.next()) {
            (Some(last), Some(next)) => {
                last.0 = format!("{} {}", last.0, next.0);
                last.1 = format!("{} {}", last.1, next.1);
            }
            (None, Some(next)) => self.parts.push(next),
            (_, None) => {}
        }
        self.parts.extend(more);
        self
    }

    fn text(&self) -> String {
        let parts: Vec<&str> = self.parts.iter().map(|part| part.0.as_str()).collect();
        parts.join(" \u{b7} ")
    }

    fn spoken(&self) -> String {
        let parts: Vec<&str> = self.parts.iter().map(|part| part.1.as_str()).collect();
        parts.join(", ")
    }
}

/// One of the card's three answers: its words and their ink.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Verdict {
    words: Words,
    tone: NoteTone,
}

impl Verdict {
    fn label(text: impl Into<String>) -> Self {
        Self {
            words: Words::of(text),
            tone: NoteTone::Label,
        }
    }

    fn value(words: Words) -> Self {
        Self {
            words,
            tone: NoteTone::Value,
        }
    }

    fn warn(words: Words) -> Self {
        Self {
            words,
            tone: NoteTone::Warn,
        }
    }
}

/// `text` over rows of at most `cells` cells, nothing cut and nothing dropped but the joins.
/// A ` · ` between two parts is the first place a row ends (the dot is not carried over). A part
/// too long for a row is broken at its blanks, and starts on the row the part before it left,
/// after the dot, where its first word fits there: a short part is not left alone on a row above
/// a long one's two. A word too long for a row is broken before its `@`, then at the cell.
#[must_use]
pub(crate) fn wrap_cells(text: &str, cells: usize) -> Vec<String> {
    const DOT: &str = " \u{b7} ";
    let cells = cells.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for part in text.split(DOT) {
        let whole = display_cells(part);
        if !row.is_empty() && display_cells(&row) + 3 + whole <= cells {
            row.push_str(DOT);
            row.push_str(part);
            continue;
        }
        if whole <= cells {
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
            }
            row.push_str(part);
            continue;
        }
        // The part's first piece joins the row before it with the dot; the rest with a blank
        // between words and nothing between a word's own pieces.
        let mut first = !row.is_empty();
        for word in units(part, cells) {
            for (nth, piece) in word_pieces(&word, cells).into_iter().enumerate() {
                let join = match (first, nth) {
                    (true, _) => DOT,
                    (false, 0) => " ",
                    (false, _) => "",
                };
                first = false;
                if row.is_empty() {
                    row = piece;
                } else if display_cells(&row) + display_cells(join) + display_cells(&piece) <= cells
                {
                    row.push_str(join);
                    row.push_str(&piece);
                } else {
                    rows.push(std::mem::replace(&mut row, piece));
                }
            }
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// `part`'s words, a time's words kept as one where a row holds them (`Sat 10:00`, `2 Oct`,
/// `1 Feb 2027`, `Thu 12 Nov 05:00`) and a figure with its `left` (`84% left`): a date is
/// never left half on a row, and no row holds a bare percentage.
fn units(part: &str, cells: usize) -> Vec<String> {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let digits = |word: &str, most: usize| {
        !word.is_empty() && word.len() <= most && word.bytes().all(|b| b.is_ascii_digit())
    };
    let clock = |word: &str| word.len() == 5 && word.as_bytes()[2] == b':';
    let month = |word: &str| MONTHS.contains(&word.trim_end_matches(['.', ',']));
    let mut out: Vec<String> = Vec::new();
    let mut last = "";
    for word in part.split(' ').filter(|word| !word.is_empty()) {
        let glue = (DAYS.contains(&last) && (clock(word) || digits(word, 2)))
            || (digits(last, 2) && month(word))
            || (MONTHS.contains(&last) && (clock(word) || digits(word, 4) && word.len() == 4))
            || (last.ends_with('%') && word.trim_end_matches([',', '.']) == "left");
        match out.last_mut() {
            Some(unit) if glue && display_cells(unit) + 1 + display_cells(word) <= cells => {
                unit.push(' ');
                unit.push_str(word);
            }
            _ => out.push(word.to_owned()),
        }
        last = word;
    }
    out
}

/// `word` as pieces of at most `cells` cells each: itself where it fits, else its part before
/// the `@` and the rest, each cut at the cell where it still does not fit.
fn word_pieces(word: &str, cells: usize) -> Vec<String> {
    if display_cells(word) <= cells {
        return vec![word.to_owned()];
    }
    let halves: Vec<&str> = match word.find('@') {
        Some(at) if at > 0 => vec![&word[..at], &word[at..]],
        _ => vec![word],
    };
    let mut out = Vec::new();
    for half in halves {
        let mut piece = String::new();
        for ch in half.chars() {
            let mut one = [0; 4];
            if display_cells(&piece) + display_cells(ch.encode_utf8(&mut one)) > cells
                && !piece.is_empty()
            {
                out.push(std::mem::take(&mut piece));
            }
            piece.push(ch);
        }
        if !piece.is_empty() {
            out.push(piece);
        }
    }
    out
}

/// A named fact over as many rows as its value needs in `room`: the name and the first of the
/// value on one row, the rest under the value's column; in a room too narrow for both
/// ([`FACT_ROOM`]) the name alone, then the value indented under it. The first row carries the
/// whole of `spoken`.
fn fact_rows(
    out: &mut Vec<MenuRow>,
    label: &str,
    value: &str,
    spoken: &str,
    tone: NoteTone,
    room: usize,
) {
    let said = format!("{label}: {spoken}");
    if room < FACT_ROOM {
        out.push(MenuRow::Note {
            text: label.to_owned(),
            spoken: said,
            tone: NoteTone::Label,
        });
        for line in wrap_cells(value, room.saturating_sub(2)) {
            out.push(MenuRow::Note {
                text: format!("  {line}"),
                spoken: String::new(),
                tone,
            });
        }
        return;
    }
    let column = FACT_LABEL_CELLS.max(display_cells(label) + 1);
    let mut said = Some(said);
    for (i, line) in wrap_cells(value, room.saturating_sub(column))
        .into_iter()
        .enumerate()
    {
        out.push(MenuRow::Fact {
            label: if i == 0 {
                label.to_owned()
            } else {
                String::new()
            },
            value: line,
            spoken: said.take().unwrap_or_default(),
            tone,
        });
    }
}

/// A sentence over as many note rows as it needs in `room`, the first carrying it whole for a
/// screen reader.
fn sentence_rows(out: &mut Vec<MenuRow>, words: &str, room: usize) {
    let mut said = Some(words.to_owned());
    for line in wrap_cells(words, room) {
        out.push(MenuRow::note_spoken(line, said.take().unwrap_or_default()));
    }
}

/// A group's heading: `title`, or `short` in a room the title and its rule do not fit.
fn section(out: &mut Vec<MenuRow>, title: &str, short: &str, room: usize) {
    let title = if display_cells(title) + 2 <= room {
        title
    } else {
        short
    };
    out.push(MenuRow::Section(title.to_owned()));
}

/// An item whose words may outrun the room: the item with as much of `label` as fits, the rest
/// on notes under its words.
fn item_rows(
    out: &mut Vec<MenuRow>,
    glyph: &'static str,
    label: &str,
    spoken: &str,
    act: FactAct,
    enabled: bool,
    room: usize,
) {
    let mut lines = wrap_cells(label, room.saturating_sub(ITEM_LEAD + ITEM_CHECK)).into_iter();
    out.push(MenuRow::Item {
        glyph,
        label: lines.next().unwrap_or_default(),
        spoken: spoken.to_owned(),
        act: MenuAct::Fact(Fact::Account, act),
        enabled,
        checked: false,
        trail: String::new(),
    });
    for line in lines {
        out.push(MenuRow::note_spoken(format!("   {line}"), ""));
    }
}

/// A pool account's item: its whole name, and what it stands at set apart at the row's far end;
/// where the room does not hold both the standing goes under the name, and where it does not
/// hold the name the name breaks before its `@`.
fn account_item(out: &mut Vec<MenuRow>, acct: &AccountMenuRow, room: usize) {
    let (name, trail) = (display_cells(&acct.name), display_cells(&acct.trail));
    let whole = ITEM_LEAD + name + 2 + trail + ITEM_CHECK <= room;
    let mut lines = wrap_cells(&acct.name, room.saturating_sub(ITEM_LEAD + ITEM_CHECK)).into_iter();
    out.push(MenuRow::Item {
        glyph: "\u{2386}",
        label: lines.next().unwrap_or_default(),
        spoken: acct.spoken.clone(),
        act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(acct.account)),
        enabled: acct.enabled,
        checked: acct.active,
        trail: if whole {
            acct.trail.clone()
        } else {
            String::new()
        },
    });
    for line in lines {
        out.push(MenuRow::note_spoken(format!("   {line}"), ""));
    }
    if !whole {
        for line in wrap_cells(&acct.trail, room.saturating_sub(ITEM_LEAD)) {
            out.push(MenuRow::note_spoken(format!("   {line}"), ""));
        }
    }
}

/// What one build of the card reads: the facts, the room, the clock.
struct Ctx<'a> {
    facts: &'a FactMenuFacts,
    room: usize,
    /// The clipboard report: every time carries its RFC 3339 instant too, and no chart is
    /// drawn.
    report: bool,
    offsets: Offsets,
    now: i64,
}

impl Ctx<'_> {
    fn card(&self) -> Option<&AccountCard> {
        self.facts.card.as_ref()
    }

    /// A time of day on the local clock (`20:10`, `Sat 10:00`, `Thu 12 Nov 05:00`).
    fn when(&self, t: i64) -> Words {
        let text = outlook::when(t, self.now, &self.offsets);
        let spoken = outlook::spoken_when(t, self.now, &self.offsets);
        self.stamped(text, spoken, t)
    }

    /// A date on the local clock (`3 Nov`, with its year where that is not this one).
    fn date(&self, t: i64) -> Words {
        let text = outlook::date_words(t, self.now, &self.offsets);
        self.stamped(text.clone(), text, t)
    }

    fn stamped(&self, text: String, spoken: String, t: i64) -> Words {
        if self.report {
            let both = format!("{text} ({})", aterm_agent::harness::usage::rfc3339_utc(t));
            Words::said(both.clone(), both)
        } else {
            Words::said(text, spoken)
        }
    }

    /// The program's name in a sentence (`Codex`, `Muse`).
    fn product(&self) -> &'static str {
        self.facts.product
    }

    /// A Mac as a row's value names it: `this Mac (m20)`, `m15`, or that it has no lab name.
    fn mac(&self, mac: &Mac) -> String {
        match (mac.ours, mac.named) {
            (true, true) => format!("this Mac ({})", mac.name),
            (true, false) => "this Mac".to_owned(),
            (false, true) => mac.name.clone(),
            (false, false) => "a Mac with no lab name".to_owned(),
        }
    }

    /// A Mac inside a verdict or a sentence: `this Mac`, `m15`.
    fn mac_short(&self, mac: &Mac) -> String {
        if mac.ours {
            "this Mac".to_owned()
        } else {
            self.mac(mac)
        }
    }

    fn claude(&self) -> bool {
        self.facts.cli == Some(Cli::Claude)
    }
}

fn figure_words(figure: Figure) -> Words {
    Words::said(figure.words(), figure.spoken_short())
}

/// A window's standing: what is left and until when, or out until its reset.
fn standing(ctx: &Ctx<'_>, figure: Figure, reset: Option<i64>) -> (Words, NoteTone) {
    match (figure, reset) {
        (Figure::Out, Some(reset)) => {
            (Words::of("out until").then(ctx.when(reset)), NoteTone::Warn)
        }
        (Figure::Out, None) => (Words::of("out"), NoteTone::Warn),
        (figure, Some(reset)) => (
            figure_words(figure).and(Words::of("resets").then(ctx.when(reset))),
            NoteTone::Value,
        ),
        (figure, None) => (figure_words(figure), NoteTone::Value),
    }
}

/// The week figure the Budget verdict says, and where it was read.
struct Stand {
    figure: Figure,
    reset: Option<i64>,
    at: Option<i64>,
    /// Read on another Mac: that Mac where it is named.
    elsewhere: Option<Option<Mac>>,
}

/// The week of the tab's account, from the first source that holds one: the session's own
/// figure, the outlook's row, then what the account reader found (this Mac's store, else the
/// journal).
fn week_stand(ctx: &Ctx<'_>) -> Option<Stand> {
    let facts = ctx.facts;
    if let Some(figure) = facts.own.filter(|figure| *figure != Figure::Unread) {
        return Some(Stand {
            figure,
            reset: facts.own_reset,
            at: facts.own_at,
            elsewhere: None,
        });
    }
    let row = facts.outlook.as_deref().and_then(|outlook| {
        let label = facts.account_row.as_deref()?;
        let row = outlook.rows.iter().find(|row| row.label == label)?;
        Some((outlook.as_of, row))
    });
    if let Some((as_of, row)) = row
        && let Some(week) = row.window(Window::Week)
    {
        // The tab's own account: its session is the evidence that it runs, so its week's
        // reading is said whatever its login here ([`RowOutlook::week_figure`]).
        let figure = row.week_figure(as_of);
        if figure != Figure::Unread {
            return Some(Stand {
                figure,
                reset: week.reset.filter(|reset| *reset > ctx.now),
                at: week.at,
                elsewhere: week.source.filter(|source| !source.here).map(|_| None),
            });
        }
    }
    ctx.card()
        .and_then(|card| card.budget.week.as_ref())
        .map(stand_of)
}

fn stand_of(seen: &Seen) -> Stand {
    Stand {
        figure: seen.figure,
        reset: seen.reset,
        at: seen.at,
        elsewhere: seen.by.clone().map(Some),
    }
}

/// The token's answer, and the rows of its section.
fn token_section(ctx: &Ctx<'_>) -> (Verdict, Vec<MenuRow>) {
    let facts = ctx.facts;
    let mut rows = Vec::new();
    if !ctx.claude() {
        let verdict = Verdict::label(match (facts.cli, ctx.product()) {
            (Some(_), product) => format!("not used: {product} signs in by itself"),
            (None, "") => "not known: this tab's program is not named yet".to_owned(),
            (None, product) => format!("not used: {product} keeps its own login"),
        });
        if facts.cli.is_some()
            && let Some(card) = ctx.card()
            && let Some((words, tone)) = login_words(ctx, card)
        {
            fact_rows(
                &mut rows,
                "Login",
                &words.text(),
                &words.spoken(),
                tone,
                ctx.room,
            );
        }
        return (verdict, rows);
    }
    let verdict = match (facts.tokens_read, &facts.token) {
        (None, _) => Verdict::label("not read yet"),
        (Some(TokensRead::NoVault), _) => Verdict::label("no vault on this Mac"),
        (Some(TokensRead::Read), None) => Verdict::label("not checked on this Mac"),
        (Some(TokensRead::Read), Some(token)) => {
            let ends = token.ends_at.and_then(|at| i64::try_from(at).ok());
            // Where the token is: in the vault, or made on this Mac and signed only into its
            // own log so far, whatever state the token itself is in.
            let place = || {
                if token.not_yet {
                    Words::of("made here").and(Words::of("not in the vault yet"))
                } else {
                    Words::of("in the vault")
                }
            };
            let vault = |rest: Words| Verdict::warn(place().and(rest));
            match &token.kind {
                TokenState::Active => Verdict::value(match ends {
                    Some(at) => place().and(Words::of("good until").then(ctx.date(at))),
                    None => place(),
                }),
                TokenState::Soon => match ends {
                    Some(at) => vault(Words::of("ends").then(ctx.date(at))),
                    None => vault(Words::of("ends soon")),
                },
                TokenState::None if token.not_yet => Verdict::value(place()),
                TokenState::None => Verdict::value(Words::of("none in the vault")),
                TokenState::Unknown => Verdict::label("not known: the vault could not be read"),
                TokenState::Unreadable => vault(Words::of("this Mac cannot open it")),
                TokenState::Mismatched(_) => vault(Words::of("failed its check")),
                TokenState::Ended => match ends {
                    Some(at) => vault(Words::of("ended").then(ctx.date(at))),
                    None => vault(Words::of("ended")),
                },
                TokenState::Rejected => vault(Words::of("Claude Code refused it")),
                TokenState::Unverified {
                    minted_here: true, ..
                } if token.not_yet => Verdict::value(place().and(Words::of("not verified yet"))),
                TokenState::Unverified {
                    minted_here: true, ..
                } => Verdict::value(Words::of("made here").and(Words::of("not verified yet"))),
                TokenState::Unverified { minted_on, .. } => {
                    Verdict::value(place().and(Words::of(format!(
                        "works only on {} until verified",
                        if minted_on.is_empty() {
                            "the Mac that made it"
                        } else {
                            minted_on.as_str()
                        }
                    ))))
                }
                TokenState::Exposed(_) => vault(Words::of("replace it: a removed Mac held it")),
            }
        }
    };
    let stored = facts.route == Some(AuthRoute::Stored);
    let login = match (&facts.route, ctx.card()) {
        (Some(AuthRoute::VaultToken { .. }), _) => Some((
            Words::of("not needed: this tab runs on the vault token"),
            NoteTone::Value,
        )),
        (_, Some(card)) => login_words(ctx, card),
        // Before the card: what the token read holds of the login, but not that a token
        // stands in for it where this tab runs on the login itself.
        (_, None) => facts
            .token
            .as_ref()
            .filter(|token| !(stored && token.login.in_use()))
            .and_then(|token| match token.login {
                LoginHere::Half(half) => Some(login_half_words(ctx, half)),
                LoginHere::Absent => {
                    Some((Words::of("never signed in on this Mac"), NoteTone::Value))
                }
                LoginHere::Unread => None,
            }),
    };
    if let Some((mut words, tone)) = login {
        if stored {
            words = words.and(Words::of("this tab runs on it"));
        }
        fact_rows(
            &mut rows,
            "Login",
            &words.text(),
            &words.spoken(),
            tone,
            ctx.room,
        );
    }
    if facts.route == Some(AuthRoute::Outside) {
        sentence_rows(
            &mut rows,
            "This tab runs on a key set outside aterm",
            ctx.room,
        );
    }
    if let Some(offer) = facts.token.as_ref().and_then(|token| token.offer.as_ref()) {
        let label = if offer.replace {
            "Replace the vault token"
        } else {
            "Make a vault token"
        };
        let who = facts.email.as_deref().unwrap_or("this account");
        item_rows(
            &mut rows,
            "+",
            label,
            &format!("{label} for {who}: opens a tab that runs aterm harness accounts token"),
            FactAct::Token,
            true,
            ctx.room,
        );
    }
    (verdict, rows)
}

/// The account's stored login on this Mac as `card` and the token read know it: an outside
/// login is signed in (the tab runs on it) and is given no end; a pool row's is its record's,
/// or not needed where a vault token stands in for it; a row no login was ever made for says
/// so. A tab that runs on the stored login itself is told the login's own record first: the
/// row then ends `this tab runs on it`, which after `not needed: the vault token …` would
/// read as the tab running on the token.
fn login_words(ctx: &Ctx<'_>, card: &AccountCard) -> Option<(Words, NoteTone)> {
    match &card.who {
        Who::Outside { .. } => Some((Words::of("signed in on this Mac"), NoteTone::Value)),
        Who::Unknown => None,
        Who::Pool { enrolled, .. } => {
            let in_use = ctx
                .facts
                .token
                .as_ref()
                .and_then(|token| token.login.half())
                .filter(|half| *half == LoginHalf::InUse);
            let half = if ctx.facts.route == Some(AuthRoute::Stored) {
                card.login.filter(|half| *half != LoginHalf::InUse)
            } else {
                in_use.or(card.login)
            };
            Some(match half {
                Some(half) => login_half_words(ctx, half),
                None if !enrolled => (Words::of("never signed in on this Mac"), NoteTone::Value),
                None => (Words::of("not read yet"), NoteTone::Label),
            })
        }
        Who::Active { .. } => Some(match card.login {
            Some(half) => login_half_words(ctx, half),
            None => (Words::of("never signed in on this Mac"), NoteTone::Value),
        }),
    }
}

fn login_half_words(ctx: &Ctx<'_>, half: LoginHalf) -> (Words, NoteTone) {
    match half {
        LoginHalf::InUse => (
            Words::of("not needed: the vault token signs this account in"),
            NoteTone::Value,
        ),
        LoginHalf::Ok => (Words::of("signed in on this Mac"), NoteTone::Value),
        LoginHalf::Ending { end } => (
            Words::of("signed in").and(Words::of("ends").then(ctx.date(end))),
            NoteTone::Warn,
        ),
        // Its forecast end passed with no record that it is signed out: the half's own word
        // (`ended`), as the token view says `login ended`; only a recorded sign-out says
        // `signed out`.
        LoginHalf::Ended { at: Some(at) } => {
            (Words::of("ended").then(ctx.date(at)), NoteTone::Warn)
        }
        LoginHalf::Ended { at: None } => (Words::of("ended"), NoteTone::Warn),
        LoginHalf::SignedOut { at: Some(at) } => {
            (Words::of("signed out").then(ctx.date(at)), NoteTone::Warn)
        }
        LoginHalf::SignedOut { at: None } => (Words::of("signed out"), NoteTone::Warn),
    }
}

/// The footer's short reason it holds no weekly figure (`tap off`, `no reply yet`).
fn gap_reason(gap: aterm_agent::harness::profile::WeeklyGap) -> &'static str {
    let words = gap.words();
    words.rsplit(" \u{b7} ").next().unwrap_or(words)
}

/// The budget's answer, and the rows of its section.
fn budget_section(ctx: &Ctx<'_>) -> (Verdict, Vec<MenuRow>) {
    let facts = ctx.facts;
    let stand = week_stand(ctx);
    let verdict = match &stand {
        _ if facts.signed_out => Verdict::label("not known: not signed in"),
        Some(stand) => {
            let (words, tone) = standing(ctx, stand.figure, stand.reset);
            Verdict { words, tone }
        }
        None => match facts.weekly_gap {
            Some(gap) => Verdict {
                words: Words::said(
                    format!("not read: {}", gap_reason(gap)),
                    gap.spoken().to_owned(),
                ),
                tone: NoteTone::Label,
            },
            None => Verdict::label("not read"),
        },
    };
    let mut rows = Vec::new();
    let room = ctx.room;
    let fact = |rows: &mut Vec<MenuRow>, label: &str, words: &Words, tone: NoteTone| {
        fact_rows(rows, label, &words.text(), &words.spoken(), tone, room);
    };
    let card = ctx.card();
    let plan = facts
        .outlook
        .as_deref()
        .zip(facts.account_row.as_deref())
        .and_then(|(outlook, label)| outlook.rows.iter().find(|row| row.label == label))
        .and_then(|row| row.plan.title())
        .or_else(|| facts.plan.clone().filter(|plan| !plan.is_empty()));
    if let Some(plan) = plan {
        fact(&mut rows, "Plan", &Words::of(plan), NoteTone::Value);
    }
    let five = facts
        .five_hour
        .map(|figure| (Figure::of_weekly(&figure, ctx.now), Some(figure.resets_at)))
        .filter(|(figure, _)| *figure != Figure::Unread)
        .or_else(|| {
            card.and_then(|card| card.budget.five.as_ref())
                .map(|seen| (seen.figure, seen.reset))
        })
        .or_else(|| (facts.wall == Some("5h")).then_some((Figure::Out, None)));
    if let Some((figure, reset)) = five {
        let (words, tone) = standing(ctx, figure, reset);
        fact(&mut rows, "5-hour", &words, tone);
    }
    for (model, seen) in card
        .map(|card| card.budget.models.as_slice())
        .unwrap_or_default()
    {
        let (words, tone) = standing(ctx, seen.figure, seen.reset);
        let words = Words::of("week").then(words);
        if display_cells(model) < FACT_LABEL_CELLS {
            fact(&mut rows, model, &words, tone);
        } else {
            fact(
                &mut rows,
                "Model",
                &Words::of(model.clone()).then(words),
                tone,
            );
        }
    }
    let own_credits = facts.credits.and_then(CreditsWord::of_state);
    let credits = match (own_credits, card.and_then(|card| card.budget.credits)) {
        (Some(CreditsWord::Out), _) => Some(CreditsWord::Out),
        (own, held) => held.or(own),
    };
    match credits {
        Some(CreditsWord::On) => fact(&mut rows, "Credits", &Words::of("on"), NoteTone::Value),
        Some(CreditsWord::InUse { since }) => fact(
            &mut rows,
            "Credits",
            &Words::of("in use since").then(ctx.when(since)),
            NoteTone::Value,
        ),
        Some(CreditsWord::Off) => fact(&mut rows, "Credits", &Words::of("off"), NoteTone::Value),
        Some(CreditsWord::Out) => fact(&mut rows, "Credits", &Words::of("out"), NoteTone::Warn),
        None => {}
    }
    for (window, until) in card
        .map(|card| card.budget.walls.as_slice())
        .unwrap_or_default()
    {
        let words = match until {
            Some(until) => Words::of(format!("{window} limit until")).then(ctx.when(*until)),
            None => Words::of(format!("{window} limit")),
        };
        fact(&mut rows, "Limits", &words, NoteTone::Warn);
    }
    if let Some(offer) = card.and_then(|card| card.budget.resets.as_ref()) {
        let left = if offer.left == 1 {
            "1 limit reset left".to_owned()
        } else {
            format!("{} limit resets left", offer.left)
        };
        let words = match offer.use_by {
            Some(by) => Words::of(left).and(Words::of("use by").then(ctx.date(by))),
            None => Words::of(left),
        };
        fact(&mut rows, "Resets", &words, NoteTone::Value);
    }
    if let Some(stand) = &stand
        && let Some(at) = stand.at
        && (matches!(stand.figure, Figure::AtLeast(_)) || stand.elsewhere.is_some())
    {
        let on = match &stand.elsewhere {
            None => "on this Mac".to_owned(),
            Some(Some(mac)) => format!("on {}", ctx.mac_short(mac)),
            Some(None) => "on another Mac".to_owned(),
        };
        fact(
            &mut rows,
            "Read",
            &ctx.when(at).then(Words::of(on)),
            NoteTone::Value,
        );
    }
    if let Some((words, tone)) = forecast(ctx) {
        fact(&mut rows, "Forecast", &words, tone);
    }
    history_rows(ctx, &mut rows);
    (verdict, rows)
}

/// The forecast row of a Claude Code pool account, by what permits a forecast at all: a pane
/// whose recovery refuses one says the refusal, a tab the rotation cannot move says that, and
/// only then is the outlook's projection of the account's week said.
fn forecast(ctx: &Ctx<'_>) -> Option<(Words, NoteTone)> {
    let facts = ctx.facts;
    if !ctx.claude() || !matches!(ctx.card()?.who, Who::Pool { .. }) {
        return None;
    }
    // The tab's own refusal first (a paused recovery, a busy observer), then the outlook's:
    // an outlook that refused to project says why itself.
    let refused = facts
        .forecast_refusal
        .or_else(|| match facts.outlook.as_deref()?.verdict {
            outlook::Verdict::Refused(why) => Some(why),
            _ => None,
        });
    if let Some(refusal) = refused {
        return Some((
            Words::of(format!("none: {}", refusal.words())),
            NoteTone::Label,
        ));
    }
    if !facts.movable {
        return Some((
            Words::of("none: aterm cannot switch this tab"),
            NoteTone::Label,
        ));
    }
    let projected = facts.outlook.as_deref().and_then(|outlook| {
        let label = facts.account_row.as_deref()?;
        let row = outlook.rows.iter().find(|row| row.label == label)?;
        Some((outlook, row.window(Window::Week)?.projection?))
    });
    let Some((outlook, projection)) = projected else {
        return Some((Words::of("not worked out yet"), NoteTone::Label));
    };
    let (words, tone) =
        match projection.fills_at {
            // An instant a pace projects is said to its lead's grain, as line E says its own: a
            // constant pace cannot place it to the minute.
            Some(at) => (
                Words::said("runs out ~", "runs out about")
                    .then_tight(ctx.when(outlook::round_down(at, outlook.as_of, &ctx.offsets))),
                NoteTone::Warn,
            ),
            None => (Words::of("lasts to its reset"), NoteTone::Value),
        };
    Some((
        match outlook.day_pct() {
            Some(pct) => words.then(Words::said(
                format!("at {pct}% of a week a day"),
                format!("at {pct} percent of a week a day"),
            )),
            None => words,
        },
        tone,
    ))
}

impl Words {
    /// The last part with `more`'s first joined straight onto it on the card (`~Sat 05:00`) and
    /// by a blank in speech.
    fn then_tight(mut self, more: Words) -> Self {
        let mut more = more.parts.into_iter();
        if let (Some(last), Some(next)) = (self.parts.last_mut(), more.next()) {
            last.0.push_str(&next.0);
            last.1 = format!("{} {}", last.1, next.1);
        }
        self.parts.extend(more);
        self
    }
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn lead(text: &str) -> Vec<PlotCell> {
    text.chars()
        .chain(std::iter::repeat(' '))
        .take(FACT_LABEL_CELLS)
        .map(|ch| PlotCell {
            ch,
            ink: PlotInk::Value,
            busy: false,
        })
        .collect()
}

fn cell(ch: char, ink: PlotInk) -> PlotCell {
    PlotCell {
        ch,
        ink,
        busy: false,
    }
}

/// `labels` (each a cell and its word) written on an axis `cells` wide: a word is placed where
/// it fits whole after the one before it and a blank.
fn axis_cells(cells: usize, labels: &[(usize, String)]) -> Vec<PlotCell> {
    let mut out = vec![cell(' ', PlotInk::Axis); cells];
    let mut free = 0;
    for (at, word) in labels {
        let len = word.chars().count();
        if *at < free || at + len > cells {
            continue;
        }
        for (slot, ch) in out[*at..].iter_mut().zip(word.chars()) {
            slot.ch = ch;
        }
        free = at + len + 1;
    }
    out
}

/// The eight-day chart: the week left as it was read, one cell a few hours of the local clock
/// (a midnight is a cell edge), the last cell the current one. Its first row is its name and
/// its axis, which names each day at its midnight; its two band rows draw each cell's last
/// reading, and on the lower one mark where a new week began (`↺`), where a reading was 0 left
/// (`╳`) and where none was made after the first held (`┄`). Nothing is drawn between readings.
fn eight_day_rows(ctx: &Ctx<'_>, reads: &[Read], whose: &str) -> Vec<MenuRow> {
    let plot = ctx.room.saturating_sub(FACT_LABEL_CELLS);
    let per_day: i64 = match plot {
        48.. => 6,
        32.. => 4,
        24.. => 3,
        _ => return Vec::new(),
    };
    if reads.is_empty() {
        return Vec::new();
    }
    let width = DAY / per_day;
    let cells = usize::try_from(8 * per_day).unwrap_or(0);
    let bucket = |t: i64| (t + ctx.offsets.at(t)).div_euclid(width);
    let last = bucket(ctx.now);
    let first = last - i64::try_from(cells).unwrap_or(0) + 1;
    let mut held: Vec<Option<&Read>> = vec![None; cells];
    let mut began = vec![false; cells];
    let mut before: Option<&Read> = None;
    for read in reads {
        let at = usize::try_from(bucket(read.t) - first)
            .ok()
            .filter(|at| *at < cells);
        if let Some(at) = at {
            held[at] = Some(read);
            if before.is_some_and(|b| b.reset != read.reset || b.epoch != read.epoch) {
                began[at] = true;
            }
        }
        before = Some(read);
    }
    let Some(oldest) = held.iter().position(Option::is_some) else {
        return Vec::new();
    };
    let (mut upper, mut lower) = (lead(""), lead(""));
    for (at, read) in held.iter().enumerate() {
        let (top, bottom) = match read {
            _ if began[at] => (cell(' ', PlotInk::Read), cell('\u{21ba}', PlotInk::Mark)),
            Some(read) if read.x10 >= 1000 => {
                (cell(' ', PlotInk::Read), cell('\u{2573}', PlotInk::Warn))
            }
            Some(read) => {
                let (top, bottom) = band_glyphs(band_level(u64::from(1000 - read.x10), 1000));
                (cell(top, PlotInk::Read), cell(bottom, PlotInk::Read))
            }
            None if at > oldest => (cell(' ', PlotInk::Read), cell('\u{2504}', PlotInk::Track)),
            None => (cell(' ', PlotInk::Read), cell(' ', PlotInk::Read)),
        };
        upper.push(top);
        lower.push(bottom);
    }
    let days: Vec<(usize, String)> = (0..cells)
        .filter_map(|at| {
            let b = first + i64::try_from(at).ok()?;
            (b.rem_euclid(per_day) == 0).then(|| {
                let day = usize::try_from((b / per_day).rem_euclid(7)).unwrap_or(0);
                (at, WEEKDAYS[day].to_owned())
            })
        })
        .collect();
    let mut axis = lead("8 days");
    axis.extend(axis_cells(cells, &days));
    let left = |read: &Read| Figure::Exact(read.x10).spoken();
    let mut said = format!("Week left over the last 8 days, {whose}: ");
    let shown: Vec<&Read> = held.iter().flatten().copied().collect();
    if let (Some(first), Some(last)) = (shown.first(), shown.last()) {
        said.push_str(&format!(
            "{} on {}",
            left(first),
            outlook::spoken_when(first.t, ctx.now, &ctx.offsets)
        ));
        for (at, _) in began.iter().enumerate().filter(|(_, began)| **began) {
            if let Some(read) = held[at] {
                said.push_str(&format!(
                    ", a new week from {}",
                    outlook::spoken_when(read.t, ctx.now, &ctx.offsets)
                ));
            }
        }
        // `now` only for a reading made in the cell now is in; else when it was made.
        if held.last().is_some_and(Option::is_some) {
            said.push_str(&format!(", {} now", left(last)));
        } else if !std::ptr::eq(*first, *last) {
            said.push_str(&format!(
                ", {} on {}",
                left(last),
                outlook::spoken_when(last.t, ctx.now, &ctx.offsets)
            ));
        }
    }
    vec![
        MenuRow::plot(axis, ""),
        MenuRow::plot(upper, ""),
        MenuRow::plot(lower, said),
    ]
}

/// The weeks row: one cell a local day, today at the right, whole weeks; each cell what was
/// left of the week at the day's last reading, `╳` a day that ended at 0 left, `┄` a day with no
/// reading, blank before the oldest day held. Its first row is its name and its axis, the date
/// each week of cells begins, as the eight-day chart's is: a chart is headed by what it is and
/// what its cells span, and that row of words parts the two charts' cells (a full cell of
/// this row straight under the band's lower row reads as one shape with it).
/// With it, how many of the days it draws hold a reading and on how many of those a five-hour
/// limit was reached.
fn weeks_rows(ctx: &Ctx<'_>, days: &[DayPoint]) -> (Vec<MenuRow>, usize, usize) {
    let weeks = (ctx.room.saturating_sub(FACT_LABEL_CELLS) / 7).min(7);
    if weeks == 0 || days.len() < 2 {
        return (Vec::new(), 0, 0);
    }
    let cells = weeks * 7;
    // A day's number on the local clock, from any instant inside it.
    let day_of = |t: i64| (t + ctx.offsets.at(t)).div_euclid(DAY);
    let today = day_of(ctx.now);
    let first = today - i64::try_from(cells).unwrap_or(0) + 1;
    let by_day: std::collections::BTreeMap<i64, &DayPoint> = days
        .iter()
        .map(|day| (day_of(day.midnight + DAY / 2), day))
        .collect();
    let oldest = by_day.keys().next().copied().unwrap_or(today);
    let (mut out_days, mut unread, mut five, mut read_days) = (0, 0, 0, 0);
    let mut row = lead("");
    for at in 0..cells {
        let day = first + i64::try_from(at).unwrap_or(0);
        let point = by_day.get(&day);
        read_days += usize::from(point.is_some());
        five += usize::from(point.is_some_and(|point| point.five_walled));
        row.push(
            match point.and_then(|point| Some((point.left_x10?, point.out))) {
                _ if day < oldest => cell(' ', PlotInk::Read),
                Some((left, out)) if out || left == 0 => {
                    out_days += 1;
                    cell('\u{2573}', PlotInk::Warn)
                }
                Some((left, _)) => {
                    let eighths = (u32::from(left) * 8).div_ceil(1000).clamp(1, 8);
                    cell(
                        char::from_u32(0x2580 + eighths).unwrap_or('\u{2588}'),
                        PlotInk::Read,
                    )
                }
                None => {
                    unread += 1;
                    cell('\u{2504}', PlotInk::Track)
                }
            },
        );
    }
    let dates: Vec<(usize, String)> = (0..weeks)
        .map(|week| {
            let day = first + i64::try_from(week * 7).unwrap_or(0);
            // The day's local noon, near enough to name its date in any zone.
            let noon = day * DAY + DAY / 2 - ctx.offsets.at(ctx.now);
            (week * 7, outlook::date_words(noon, ctx.now, &ctx.offsets))
        })
        .collect();
    let mut axis = lead(&format!("{weeks} weeks"));
    axis.extend(axis_cells(cells, &dates));
    let count = |n: usize, what: &str| format!("{n} day{} {what}", if n == 1 { "" } else { "s" });
    let mut said = format!("Week left at each day's end, {cells} days to today");
    let notes: Vec<String> = [(out_days, "out"), (unread, "not read")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| count(n, what))
        .collect();
    if !notes.is_empty() {
        said.push_str(": ");
        said.push_str(&notes.join(", "));
    }
    (
        vec![MenuRow::plot(axis, ""), MenuRow::plot(row, said)],
        read_days,
        five,
    )
}

/// The charts' legend, under the name column where the room holds both.
fn legend_row(room: usize) -> MenuRow {
    let words = [
        ('\u{21ba}', PlotInk::Mark, " week reset  "),
        ('\u{2573}', PlotInk::Warn, " out  "),
        ('\u{2504}', PlotInk::Track, " not read"),
    ];
    let cells: usize = words
        .iter()
        .map(|(_, _, word)| 1 + word.chars().count())
        .sum();
    let mut row = if FACT_LABEL_CELLS + cells <= room {
        lead("")
    } else {
        Vec::new()
    };
    for (mark, ink, word) in words {
        row.push(cell(mark, ink));
        row.extend(word.chars().map(|ch| cell(ch, PlotInk::Value)));
    }
    MenuRow::plot(row, "Legend: a week reset, out, not read")
}

/// The `History` fact and its charts: whose readings and since when, the eight days by the
/// hour, the weeks by the day, the legend, and the days a five-hour limit was reached. In a
/// room too narrow for a chart ([`CHART_ROOM`]) the span is said in words. No row before the
/// account reader answered.
fn history_rows(ctx: &Ctx<'_>, rows: &mut Vec<MenuRow>) {
    let Some(card) = ctx.card() else {
        return;
    };
    let room = ctx.room;
    let fact = |rows: &mut Vec<MenuRow>, words: &Words, tone: NoteTone| {
        fact_rows(rows, "History", &words.text(), &words.spoken(), tone, room);
    };
    let history = &card.history;
    let (caption, whose) = match history.from {
        HistoryFrom::NotKept => {
            let words = Words::of(format!("not kept for {} accounts", ctx.product()));
            return fact(rows, &words, NoteTone::Label);
        }
        HistoryFrom::NotRead => {
            let words = Words::of(if ctx.facts.accounts_refused.is_some() {
                "not read: this Mac's accounts could not be read"
            } else if matches!(card.who, Who::Unknown) {
                "not read: this tab's account is not named yet"
            } else {
                "not read"
            });
            return fact(rows, &words, NoteTone::Label);
        }
        HistoryFrom::Nothing => {
            // Rows of the account with no reading of its week among them (its plan alone, or
            // readings older than the weeks drawn) are not no rows at all.
            let words = Words::of(if history.since.is_some() {
                "no week reading on this Mac in the last 7 weeks"
            } else {
                "none kept on this Mac for this account"
            });
            return fact(rows, &words, NoteTone::Label);
        }
        HistoryFrom::Series => (
            match history.since {
                Some(since) => Words::of("week left, read on this Mac since").then(ctx.date(since)),
                None => Words::of("week left, read on this Mac"),
            },
            "read on this Mac",
        ),
        HistoryFrom::Journal => (
            Words::of("week left, as the lab's Macs read it"),
            "as the lab's Macs read it",
        ),
    };
    if ctx.report {
        return fact(rows, &caption, NoteTone::Value);
    }
    if room < CHART_ROOM {
        // The same span in words: the oldest and the newest figure held.
        let points: Vec<(i64, u16)> = if history.reads.is_empty() {
            history
                .days
                .iter()
                .filter_map(|day| Some((day.midnight, 1000 - day.left_x10?.min(1000))))
                .collect()
        } else {
            history
                .reads
                .iter()
                .map(|read| (read.t, read.x10))
                .collect()
        };
        // The newest figure is `now` only where it is an hour's reading made within the hour;
        // else it is said with the date it was read, as the oldest is.
        let hour = |t: i64| (t + ctx.offsets.at(t)).div_euclid(3600);
        let fresh = !history.reads.is_empty();
        let on = |t: i64| format!("on {}", outlook::date_words(t, ctx.now, &ctx.offsets));
        let words = match (points.first(), points.last()) {
            (Some((t, from)), Some((last, to))) => {
                let when = if fresh && hour(*last) == hour(ctx.now) {
                    "now".to_owned()
                } else {
                    on(*last)
                };
                if points.len() == 1 {
                    Words::said(
                        format!("{} {when}", Figure::Exact(*to).words()),
                        format!("{} {when}", Figure::Exact(*to).spoken()),
                    )
                } else {
                    Words::said(
                        format!(
                            "from {} {} to {} {when}",
                            Figure::Exact(*from).words(),
                            on(*t),
                            Figure::Exact(*to).words(),
                        ),
                        format!(
                            "from {} {} to {} {when}",
                            Figure::Exact(*from).spoken(),
                            on(*t),
                            Figure::Exact(*to).spoken(),
                        ),
                    )
                }
            }
            _ => caption,
        };
        return fact(rows, &words, NoteTone::Value);
    }
    fact(rows, &caption, NoteTone::Value);
    let eight = eight_day_rows(ctx, &history.reads, whose);
    let (weeks, days, five) = if history.from == HistoryFrom::Series {
        weeks_rows(ctx, &history.days)
    } else {
        (Vec::new(), 0, 0)
    };
    let drawn = !eight.is_empty() || !weeks.is_empty();
    rows.extend(eight);
    rows.extend(weeks);
    if drawn {
        rows.push(legend_row(room));
    }
    if five > 0 {
        sentence_rows(
            rows,
            &format!(
                "5-hour limit reached on {five} of the {days} day{} read",
                if days == 1 { "" } else { "s" }
            ),
            room,
        );
    }
}

/// Whether `a` and `b` are one Mac: both this Mac, or the same lab name.
fn same_mac(a: &Mac, b: &Mac) -> bool {
    (a.ours && b.ours) || (a.named && b.named && a.name == b.name)
}

/// The machine's answer, and the rows of its section.
fn machine_section(ctx: &Ctx<'_>) -> (Verdict, Vec<MenuRow>) {
    let facts = ctx.facts;
    if facts.cli.is_none() {
        return (
            Verdict::value(Words::of("in use here").and(Words::of("other Macs not tracked"))),
            Vec::new(),
        );
    }
    let Some(card) = ctx.card() else {
        // No read could be made at all (the pool's refusal says why under the card), or none
        // has answered yet.
        return (
            Verdict::label(if facts.accounts_refused.is_some() {
                "not read"
            } else {
                "not read yet"
            }),
            Vec::new(),
        );
    };
    let machine = &card.machine;
    let holders: &[Holder] = match &machine.claims {
        Claims::Read(holders) => holders,
        Claims::Senior(holder, _) => std::slice::from_ref(holder),
        Claims::Absent | Claims::Unreadable | Claims::NotTracked | Claims::NotAsked => &[],
    };
    // Why nothing names the account by an address, where that is why a file was not asked.
    let unnamed = if matches!(card.who, Who::Unknown) {
        "not known: this tab's account is not named yet"
    } else {
        "not known: this account has no address on record"
    };
    // What this Mac can say of a Mac that is not among the holders it knows.
    let unseen = match &machine.claims {
        Claims::Read(_) => None,
        Claims::Senior(_, NoTable::Absent) | Claims::Absent => {
            Some("not known: this Mac does not read other Macs")
        }
        Claims::Senior(_, NoTable::Unreadable) | Claims::Unreadable => {
            Some("not known: other Macs could not be read just now")
        }
        Claims::Senior(_, NoTable::NotAsked) | Claims::NotAsked => Some(unnamed),
        Claims::NotTracked => Some("not tracked"),
    };
    // A Mac that claims the account, apart from one this Mac only holds it for: a hold says
    // that Mac may be using it, never that it is.
    let claims = |holder: &&Holder| holder.since.is_some();
    let others: Vec<&Holder> = holders.iter().filter(|holder| !holder.mac.ours).collect();
    let home = match &machine.home {
        Home::Known { mac, .. } => Some(mac),
        Home::NoPoolFile | Home::NotListed | Home::NotAsked => None,
    };
    let home_part = match &machine.home {
        Home::Known { mac, .. } => format!("home {}", ctx.mac_short(mac)),
        Home::NoPoolFile | Home::NotAsked => "home not known".to_owned(),
        Home::NotListed => "no home on record".to_owned(),
    };
    let use_part = match home.filter(|home| !home.ours) {
        Some(home) => {
            let at_home = || holders.iter().filter(|holder| same_mac(&holder.mac, home));
            let there = at_home().any(|holder| holder.since.is_some());
            let held = at_home().any(|holder| holder.since.is_none());
            let here = holders.iter().any(|holder| holder.mac.ours);
            match (there, here, unseen) {
                (true, true, _) => "in use there and here",
                (true, false, _) => "in use there",
                (false, _, _) if held => "use there not known",
                (false, _, None) => "not in use there",
                (false, _, Some("not tracked")) => "use there not tracked",
                (false, _, Some(_)) => "use there not known",
            }
            .to_owned()
        }
        None => {
            let using: Vec<&&Holder> = others.iter().filter(|holder| claims(holder)).collect();
            match using.as_slice() {
                [] => "in use here".to_owned(),
                [one] => format!("in use here and on {}", ctx.mac_short(&one.mac)),
                many => format!("in use here and on {} other Macs", many.len()),
            }
        }
    };
    let verdict = Verdict::value(Words::of(home_part).and(Words::of(use_part)));

    let mut rows = Vec::new();
    let room = ctx.room;
    let fact = |rows: &mut Vec<MenuRow>, label: &str, words: &Words, tone: NoteTone| {
        fact_rows(rows, label, &words.text(), &words.spoken(), tone, room);
    };
    match &machine.home {
        Home::Known {
            mac,
            why,
            since,
            also,
        } => {
            let why = why.map(|why| match why {
                SlotWhy::Pin => "set on the accounts sheet",
                SlotWhy::Fill => "assigned",
                SlotWhy::Stranded => "moved from a silent Mac",
                SlotWhy::Probation => "on trial",
                SlotWhy::Borrow => "borrowed",
            });
            let mut words = Words::of(ctx.mac(mac));
            words = match (why, since) {
                (Some(why), Some(since)) => {
                    words.and(Words::of(format!("{why} since")).then(ctx.date(*since)))
                }
                (Some(why), None) => words.and(Words::of(why)),
                (None, Some(since)) => words.and(Words::of("since").then(ctx.date(*since))),
                (None, None) => words,
            };
            if !also.is_empty() {
                let macs: Vec<String> = also.iter().map(|mac| ctx.mac_short(mac)).collect();
                words = words.and(Words::of(format!("also assigned to {}", macs.join(", "))));
            }
            fact(&mut rows, "Home", &words, NoteTone::Value);
        }
        Home::NoPoolFile => fact(
            &mut rows,
            "Home",
            &Words::of("not known: no pool list on this Mac"),
            NoteTone::Label,
        ),
        Home::NotListed => fact(
            &mut rows,
            "Home",
            &Words::of("no home on record"),
            NoteTone::Label,
        ),
        Home::NotAsked => fact(&mut rows, "Home", &Words::of(unnamed), NoteTone::Label),
    }
    // The home Mac always, then each other Mac that holds the account now. A Mac with no lab
    // name is nobody's twin: each such holder is its own row.
    let mut listed: Vec<(&Mac, Option<&Holder>)> = home
        .filter(|home| !home.ours)
        .map(|home| {
            (
                home,
                holders.iter().find(|holder| same_mac(&holder.mac, home)),
            )
        })
        .into_iter()
        .collect();
    for holder in &others {
        if !listed.iter().any(|(mac, _)| same_mac(mac, &holder.mac)) {
            listed.push((&holder.mac, Some(holder)));
        }
    }
    for (mac, holder) in &listed {
        let name = if mac.named {
            mac.name.as_str()
        } else {
            "Unnamed"
        };
        let (label, words, tone) = match (holder.map(|holder| holder.since), mac.named) {
            (Some(Some(since)), true) => (
                name,
                Words::of("in use").and(Words::of("claimed since").then(ctx.when(since))),
                NoteTone::Value,
            ),
            (Some(Some(since)), false) => (
                name,
                Words::of("a Mac with no lab name, claimed since").then(ctx.when(since)),
                NoteTone::Value,
            ),
            // A hold this Mac keeps for it: its claims cannot be seen, so nothing began.
            (Some(None), _) => (
                name,
                Words::of("held for it: its clock disagrees, use not known"),
                NoteTone::Label,
            ),
            (None, _) => {
                let label = name;
                match unseen {
                    Some("not tracked") => (
                        label,
                        Words::of(format!("not tracked for {} accounts", ctx.product())),
                        NoteTone::Label,
                    ),
                    Some(why) => (label, Words::of(why), NoteTone::Label),
                    None => {
                        let last = machine
                            .last_use
                            .as_ref()
                            .filter(|(by, _)| same_mac(by, mac))
                            .map(|(_, at)| ctx.when(*at))
                            .or_else(|| machine.used_day.map(|day| ctx.date(day)));
                        (
                            label,
                            match last {
                                Some(last) => {
                                    Words::of("not in use").and(Words::of("last used").then(last))
                                }
                                None => Words::of("not in use"),
                            },
                            NoteTone::Value,
                        )
                    }
                }
            }
        };
        // A lab name is at most the column; a longer one leads its value instead.
        if display_cells(label) < FACT_LABEL_CELLS {
            fact(&mut rows, label, &words, tone);
        } else {
            fact(&mut rows, "Mac", &Words::of(label).then(words), tone);
        }
    }
    fact(
        &mut rows,
        "This Mac",
        &Words::of(match facts.tabs_here.map(|tabs| tabs.saturating_sub(1)) {
            // The other tabs could not be counted: nothing names this tab's account to them.
            None => "in use: this tab".to_owned(),
            Some(0) => "in use: this tab only".to_owned(),
            Some(more) => format!("in use: this tab and {more} more"),
        }),
        NoteTone::Value,
    );
    if listed.is_empty() {
        let (words, tone) = match unseen {
            None => ("no other Mac is using it".to_owned(), NoteTone::Value),
            Some("not tracked") => (
                format!("not tracked for {} accounts", ctx.product()),
                NoteTone::Label,
            ),
            Some(why) => (why.to_owned(), NoteTone::Label),
        };
        fact(&mut rows, "Others", &Words::of(words), tone);
    }
    if machine.journal == JournalHeld::Held
        && let Some((starts, since)) = &machine.starts
        && let Some((mac, most)) = starts.first()
    {
        let all: u32 = starts.iter().map(|(_, n)| *n).sum();
        fact(
            &mut rows,
            "Usual",
            &Words::of(ctx.mac_short(mac))
                .and(Words::of(format!("{most} of {all} starts since")).then(ctx.date(*since))),
            NoteTone::Value,
        );
    }
    (verdict, rows)
}

/// The one sentence that says what automatic switching does for this tab now; `None` where
/// there is no observation of it and the account is one of this Mac's.
fn rotation_sentence(ctx: &Ctx<'_>) -> Option<String> {
    let facts = ctx.facts;
    let card = ctx.card();
    let outside = facts.accounts_refused.is_none()
        && card.is_some_and(|card| matches!(card.who, Who::Outside { .. }));
    let progress = facts.account_progress.as_ref();
    if !outside && progress.is_none() {
        return None;
    }
    let mut sentence = if outside {
        match card.map_or(0, |card| card.pool_size) {
            0 => "Off for this tab: this Mac lists no accounts.".to_owned(),
            1 => "Off for this tab: its login is not this Mac's one account.".to_owned(),
            n => format!("Off for this tab: its login is not one of this Mac's {n} accounts."),
        }
    } else {
        phase_sentence(ctx, progress?)
    };
    for work in progress
        .map(|p| p.independent_work.as_slice())
        .unwrap_or_default()
    {
        sentence.push_str(&format!(" Signing in {} in the background.", work.label));
    }
    Some(sentence)
}

fn phase_sentence(ctx: &Ctx<'_>, progress: &AccountProgress) -> String {
    let stay = progress.stay.as_deref();
    match progress.own_claim.map(|claim| claim.blocker) {
        Some(OwnClaimBlocker::HeldElsewhere { until }) => {
            let holder = ctx
                .card()
                .and_then(|card| match &card.machine.claims {
                    Claims::Read(holders) => holders.iter().find(|holder| !holder.mac.ours),
                    Claims::Senior(holder, _) => Some(holder).filter(|holder| !holder.mac.ours),
                    _ => None,
                })
                .filter(|holder| holder.mac.named)
                .map_or_else(
                    || "Another Mac".to_owned(),
                    |holder| holder.mac.name.clone(),
                );
            return format!(
                "{holder} holds this account until {}.",
                ctx.when(until).text()
            );
        }
        Some(OwnClaimBlocker::CannotRecord) => {
            return "This Mac cannot record its claim on this account.".to_owned();
        }
        None => {}
    }
    if progress.phase == ProgressPhase::Disabled || stay == Some("off") {
        return "Off: automatic switching is turned off.".to_owned();
    }
    match stay {
        Some("pinned") => return "Off while this Mac is pinned to this account.".to_owned(),
        Some("other-home" | "not-a-stored-login" | "keyring-store") => {
            return "Off for this tab: it keeps its own login.".to_owned();
        }
        _ => {}
    }
    // Any other stay word ranks under the phase: the host sets one at every ordinary turn end
    // (`nothing-due`), and the phase is what switching does now. It is said only where the
    // phase itself says nothing was read.
    match &progress.phase {
        ProgressPhase::Paused => "Paused for this tab.".to_owned(),
        ProgressPhase::Watching => match (progress.eligible, progress.known) {
            (Some(0), _) => "On, but no other account can take over now.".to_owned(),
            (Some(eligible), Some(known)) => {
                format!("On: {eligible} of this Mac's {known} accounts can take over.")
            }
            _ => "On.".to_owned(),
        },
        ProgressPhase::Limited | ProgressPhase::Waiting => {
            match progress.next_at.filter(|at| *at > 0) {
                Some(at) => format!("At its limit. aterm looks again {}.", ctx.when(at).text()),
                None => "At its limit.".to_owned(),
            }
        }
        ProgressPhase::Switching { to } => format!("Moving this tab to {to}."),
        ProgressPhase::Verifying { to } => format!("Checking {to} before the move."),
        ProgressPhase::SigningIn { label, .. } => format!("Signing in {label}."),
        ProgressPhase::Probing => "Checking this Mac's accounts.".to_owned(),
        ProgressPhase::Ready => "Another account is ready.".to_owned(),
        ProgressPhase::Unknown => match stay {
            Some(word) => {
                let why = crate::harness_host::progress::reason(word);
                format!("{}.", why.trim_end_matches('.'))
            }
            None => "Not read yet.".to_owned(),
        },
        ProgressPhase::Disabled => "Off: automatic switching is turned off.".to_owned(),
    }
}

fn copy_rows(out: &mut Vec<MenuRow>, room: usize) {
    item_rows(
        out,
        "\u{29c9}",
        "Copy details",
        "Copy this account's details",
        FactAct::CopyRecovery,
        true,
        room,
    );
}

fn network_rows(out: &mut Vec<MenuRow>, room: usize) {
    item_rows(
        out,
        "\u{22ef}",
        "Network State\u{2026}",
        "Network State and shared-service access",
        FactAct::NetworkState,
        true,
        room,
    );
}

fn settings_rows(out: &mut Vec<MenuRow>, room: usize) {
    item_rows(
        out,
        "\u{22ef}",
        "Account settings\u{2026}",
        "Account settings: open the configuration folder",
        FactAct::AccountSettings,
        true,
        room,
    );
}

/// The head: the card's name and place (never the tab's own name), the account's address, whose current account it is for
/// a program whose footer names no login, and a refusal's notice.
fn head_rows(ctx: &Ctx<'_>, out: &mut Vec<MenuRow>) {
    let facts = ctx.facts;
    // The tab's own name is not drawn: the header would cut a long one with `…`, and this
    // card cuts nothing. It stays in the card's spoken name.
    for row in crate::autopilot_menu::header_rows(&facts.title, None, ctx.room) {
        match row {
            MenuRow::Header(text) if display_cells(&text) > ctx.room => {
                out.extend(wrap_cells(&text, ctx.room).into_iter().map(MenuRow::Header));
            }
            row => out.push(row),
        }
    }
    let active = ctx.card().and_then(|card| match &card.who {
        Who::Active { name, .. } => Some(name.as_str()),
        _ => None,
    });
    let name = if facts.signed_out {
        Some("not signed in")
    } else {
        facts
            .email
            .as_deref()
            .filter(|email| !email.is_empty())
            .or(active)
    };
    if let Some(name) = name {
        let mut lines = wrap_cells(name, ctx.room.saturating_sub(ITEM_LEAD)).into_iter();
        out.push(MenuRow::Header(format!(
            "{}  {}",
            aterm_agent::harness::footer::ACCOUNT_MARK,
            lines.next().unwrap_or_default()
        )));
        out.extend(lines.map(|line| MenuRow::Header(format!("   {line}"))));
    }
    if active.is_some() && facts.email.is_none() {
        let words = format!("{}'s current account on this Mac", ctx.product());
        let mut said = Some(words.clone());
        for line in wrap_cells(&words, ctx.room.saturating_sub(ITEM_LEAD)) {
            out.push(MenuRow::note_spoken(
                format!("   {line}"),
                said.take().unwrap_or_default(),
            ));
        }
    }
    if let Some(note) = &facts.note {
        sentence_rows(out, note, ctx.room);
    }
}

fn build(facts: &FactMenuFacts, room: usize, report: bool) -> Vec<MenuRow> {
    let ctx = Ctx {
        facts,
        room,
        report,
        offsets: facts
            .card
            .as_ref()
            .map(|card| card.offsets.clone())
            .or_else(|| {
                facts
                    .outlook
                    .as_deref()
                    .map(|outlook| outlook.offsets.clone())
            })
            .unwrap_or_else(|| Offsets::fixed(facts.offset_s)),
        now: facts.now,
    };
    let mut out = Vec::new();
    head_rows(&ctx, &mut out);
    let (token, token_rows) = token_section(&ctx);
    let (budget, budget_rows) = budget_section(&ctx);
    let (machine, machine_rows) = machine_section(&ctx);
    for (label, verdict) in [
        ("Token", &token),
        ("Budget", &budget),
        ("Machine", &machine),
    ] {
        fact_rows(
            &mut out,
            label,
            &verdict.words.text(),
            &verdict.words.spoken(),
            verdict.tone,
            room,
        );
    }
    for (title, rows) in [
        ("Token", token_rows),
        ("Budget", budget_rows),
        ("Machine", machine_rows),
    ] {
        if !rows.is_empty() {
            section(&mut out, title, title, room);
            out.extend(rows);
        }
    }
    let Some(cli) = facts.cli else {
        match ctx.product() {
            "" => {}
            "Muse" => sentence_rows(
                &mut out,
                "Muse keeps its own login: switch accounts with /login in Muse.",
                room,
            ),
            product => sentence_rows(&mut out, &format!("{product} keeps its own login."), room),
        }
        if facts.refresh_usage {
            item_rows(
                &mut out,
                PIN_GLYPH,
                "Refresh usage",
                "Refresh usage",
                FactAct::RefreshUsage,
                true,
                room,
            );
        }
        // A recovery observation of a tab whose program is not named yet is still evidence.
        if facts.account_progress.is_some() {
            copy_rows(&mut out, room);
            network_rows(&mut out, room);
        }
        return out;
    };
    if facts.accounts_loading {
        sentence_rows(&mut out, "Reading this Mac's accounts", room);
        settings_rows(&mut out, room);
        return out;
    }
    if let Some(why) = &facts.accounts_refused {
        sentence_rows(
            &mut out,
            &format!("This Mac's accounts could not be read: {why}"),
            room,
        );
        settings_rows(&mut out, room);
        return out;
    }
    let pin_enabled = !facts.manual_signin_unavailable;
    if facts.accounts.is_empty() {
        if cli == Cli::Claude {
            sentence_rows(
                &mut out,
                "No accounts are listed on this Mac. Add them with aterm init.",
                room,
            );
            item_rows(
                &mut out,
                PIN_GLYPH,
                "Sync accounts & clear this machine\u{2019}s pin",
                "Sync accounts and clear this machine's Claude Code pool pin",
                FactAct::AutoRotate,
                pin_enabled,
                room,
            );
        } else {
            sentence_rows(
                &mut out,
                &format!("No {} accounts are listed on this Mac.", ctx.product()),
                room,
            );
            sentence_rows(
                &mut out,
                &format!(
                    "Add one: aterm harness accounts add --cli {} <label>",
                    cli.id()
                ),
                room,
            );
        }
        settings_rows(&mut out, room);
        return out;
    }
    section(&mut out, "Switch this Mac to", "Switch to", room);
    if facts.manual_signin_unavailable {
        sentence_rows(
            &mut out,
            "Signing in by hand is not available on this platform. An account that is \
             signed in can still be chosen.",
            room,
        );
    } else if let Some(queue) = &facts.account_queue {
        sentence_rows(&mut out, queue, room);
    }
    for acct in &facts.accounts {
        account_item(&mut out, acct, room);
    }
    section(&mut out, "Automatic switching", "Switching", room);
    if let Some(sentence) = rotation_sentence(&ctx) {
        sentence_rows(&mut out, &sentence, room);
    }
    let pinned = facts.pinned.as_deref();
    let pin = match pinned {
        None => "none: aterm picks the account".to_owned(),
        Some("") => "this Mac stays on one account".to_owned(),
        Some(name) => format!("this Mac stays on {name}"),
    };
    fact_rows(&mut out, "Pin", &pin, &pin, NoteTone::Value, room);
    let (label, spoken) = if pinned.is_some() {
        (
            "Unpin and move to the next account",
            format!(
                "Unpin this Mac and move its {} sessions to the next account",
                ctx.product()
            ),
        )
    } else {
        (
            "Move this Mac to the next account",
            format!(
                "Move this Mac's {} sessions to the next account",
                ctx.product()
            ),
        )
    };
    item_rows(
        &mut out,
        PIN_GLYPH,
        label,
        &spoken,
        FactAct::AutoRotate,
        pin_enabled,
        room,
    );
    if cli == Cli::Claude && (facts.email.is_some() || facts.account_progress.is_some()) {
        copy_rows(&mut out, room);
    }
    if facts.account_progress.is_some() {
        network_rows(&mut out, room);
    }
    settings_rows(&mut out, room);
    out
}

/// The Account card's rows for `facts` in `facts.room` cells. The card is wider than
/// [`MAX_CONTENT`] only beside a chart: a build that draws none is made again at that width.
#[must_use]
pub(crate) fn account_rows(facts: &FactMenuFacts) -> Vec<MenuRow> {
    // A caller that names no room gets the card's plain width.
    let room = if facts.room == 0 {
        MAX_CONTENT
    } else {
        facts.room
    };
    let rows = build(facts, room, false);
    if room > MAX_CONTENT && !rows.iter().any(|row| matches!(row, MenuRow::Plot { .. })) {
        return build(facts, MAX_CONTENT, false);
    }
    rows
}

/// Everything the card holds of the account, for the clipboard: each of its facts unwrapped,
/// then what the card leaves out (the token's dates and its vault standing, every Mac the
/// account is assigned to and every live claim, the last use and the starts by Mac, why the
/// footer has no week figure, and today's recovery record). Every time is said on the local
/// clock with its RFC 3339 instant after it. For an account that is a row here, the last line
/// names the command that prints its older days.
#[must_use]
pub(crate) fn details_report(facts: &FactMenuFacts) -> String {
    let ctx = Ctx {
        facts,
        room: usize::MAX / 2,
        report: true,
        offsets: facts
            .card
            .as_ref()
            .map(|card| card.offsets.clone())
            .or_else(|| {
                facts
                    .outlook
                    .as_deref()
                    .map(|outlook| outlook.offsets.clone())
            })
            .unwrap_or_else(|| Offsets::fixed(facts.offset_s)),
        now: facts.now,
    };
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "Account: {}",
        facts.email.as_deref().unwrap_or("not known")
    ));
    let (token, token_rows) = token_section(&ctx);
    let (budget, budget_rows) = budget_section(&ctx);
    let (machine, machine_rows) = machine_section(&ctx);
    for (label, verdict) in [
        ("Token", &token),
        ("Budget", &budget),
        ("Machine", &machine),
    ] {
        lines.push(format!("{label}: {}", verdict.words.text()));
    }
    for row in token_rows.iter().chain(&budget_rows).chain(&machine_rows) {
        match row {
            MenuRow::Fact { label, value, .. } if !label.is_empty() => {
                lines.push(format!("{label}: {value}"));
            }
            MenuRow::Note { text, .. } => lines.push(text.trim_start().to_owned()),
            _ => {}
        }
    }
    if let Some(sentence) = rotation_sentence(&ctx) {
        lines.push(format!("Automatic switching: {sentence}"));
    }
    if let Some(token) = &facts.token {
        let secs = |at: Option<u64>| at.and_then(|at| i64::try_from(at).ok());
        for (what, at) in [
            ("Token made", secs(token.made_at)),
            ("Token ends", secs(token.ends_at)),
            ("Token verified", secs(token.verified_at)),
        ] {
            if let Some(at) = at {
                lines.push(format!("{what}: {}", ctx.when(at).text()));
            }
        }
        if let Some(mac) = &token.made_on {
            lines.push(format!("Token made on: {mac}"));
        }
        if let Some(vault) = &token.vault_words {
            lines.push(format!("Vault: {vault}"));
        }
        match secs(token.peers_read_at) {
            Some(at) => lines.push(format!(
                "Other Macs on the token: {}, read {}",
                if token.peers.is_empty() {
                    "none".to_owned()
                } else {
                    token.peers.join(", ")
                },
                ctx.when(at).text()
            )),
            None => lines.push("Other Macs on the token: not read".to_owned()),
        }
    }
    if let Some(card) = ctx.card() {
        if let Home::Known { mac, also, .. } = &card.machine.home {
            let macs: Vec<String> = std::iter::once(mac)
                .chain(also)
                .map(|mac| ctx.mac(mac))
                .collect();
            lines.push(format!("Assigned to: {}", macs.join(", ")));
        }
        let holders: &[Holder] = match &card.machine.claims {
            Claims::Read(holders) => holders,
            Claims::Senior(holder, _) => std::slice::from_ref(holder),
            _ => &[],
        };
        for holder in holders {
            lines.push(match holder.since {
                Some(since) => format!(
                    "Claim: {}, since {}, until {}",
                    ctx.mac(&holder.mac),
                    ctx.when(since).text(),
                    ctx.when(holder.until).text()
                ),
                None => format!(
                    "Held for {} while its clock disagrees, until {}",
                    ctx.mac(&holder.mac),
                    ctx.when(holder.until).text()
                ),
            });
        }
        if let Some((mac, at)) = &card.machine.last_use {
            lines.push(format!(
                "Last use: {}, {}",
                ctx.mac(mac),
                ctx.when(*at).text()
            ));
        }
        if let Some((starts, since)) = &card.machine.starts {
            let by: Vec<String> = starts
                .iter()
                .map(|(mac, n)| format!("{} {n}", ctx.mac(mac)))
                .collect();
            lines.push(format!(
                "Starts since {}: {}",
                ctx.when(*since).text(),
                by.join(", ")
            ));
        }
    }
    if week_stand(&ctx).is_none()
        && let Some(gap) = facts.weekly_gap
    {
        lines.push(format!(
            "Week not read: {}",
            super::weekly_gap_why(gap).join(" ")
        ));
    }
    if let Some(progress) = &facts.account_progress {
        lines.push(format!(
            "Recovery: {}",
            aterm_agent::harness::footer::account_progress_phase(progress)
        ));
        let count =
            |value: Option<usize>| value.map_or_else(|| "not read".into(), |n| n.to_string());
        lines.push(format!(
            "Accounts: {}; eligible: {}",
            count(progress.known),
            count(progress.eligible)
        ));
        lines.push(format!(
            "Limit: {}",
            progress.wall.as_deref().unwrap_or("not observed")
        ));
        for work in &progress.independent_work {
            lines.push(aterm_agent::harness::footer::account_pool_work_text(work));
            if work.observed_at > 0 {
                lines.push(format!(
                    "Pool work observed {}",
                    ctx.when(work.observed_at).text()
                ));
            }
        }
        lines.extend(progress.details.iter().cloned());
        for diagnostic in &progress.login_diagnostics {
            let label = diagnostic.label();
            let name = ctx
                .card()
                .and_then(|card| card.names.get(label))
                .map_or(label, String::as_str);
            lines.push(format!(
                "Last local sign-in for {name}: {}",
                diagnostic.words()
            ));
            lines.push(format!(
                "Attempt finished {}",
                ctx.when(diagnostic.completed_at).text()
            ));
        }
        lines.push(if progress.observed_at > 0 {
            format!("Observed {}", ctx.when(progress.observed_at).text())
        } else {
            "Not observed yet".to_owned()
        });
        lines.push(match progress.next_at.filter(|at| *at > 0) {
            Some(at) => format!("Next event {}", ctx.when(at).text()),
            None => "No next event scheduled".to_owned(),
        });
    }
    // The command takes a roster label; an account with none here has no older days it prints.
    let older = ctx
        .card()
        .and_then(|card| card.who.label().map(str::to_owned))
        .or_else(|| facts.account_row.clone());
    if let Some(label) = older {
        lines.push(format!(
            "Older days: aterm harness accounts history {label}"
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "fact_menu_account_tests.rs"]
mod tests;
