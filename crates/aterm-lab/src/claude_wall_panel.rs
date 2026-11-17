// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `claude-wall-panel [--keep] [--record]`: MEASURE CLAUDE CODE'S OWN `/usage` PANEL OVER A STANDING USAGE
//! WALL, and record what it found in `ai::connector::CLAUDE_ACCOUNTS.wall_panel_measured_on`.
//!
//! The accounts probe types `/usage` only at a quiet turn end: `ProbeGate::Wall` shuts it wherever
//! a wall stands (`supervise/policy/turn_end.rs`, `probe_gate`), because nobody had seen what the
//! panel does there. This verb looks, on the lab's stub alone (no real account is reached, no
//! reset is spent and no real quota is used):
//!
//! 1. the build the managed Claude Code says it is ([`crate::claude_measure::installed_build`]);
//! 2. a lab of its own (the harness off, so nothing else is pressed; the folder trusted), Claude
//!    Code launched on the subscriber path with fake credentials and the profile scope
//!    ([`LAUNCH_ENV`], the composed `oauthAccount`), on the fullscreen renderer the managed launch
//!    runs and the probe reads; the stub's answers carry the lab's figures for the session, the
//!    week and Fable's week ([`blocks`], [`FIGURES`]);
//! 3. a turn answered, then for each wall ([`WallAt`]: the five-hour limit, then the weekly one,
//!    as `lab limit` sets them) a turn into it, and with the wall standing ([`measure`]): the
//!    screen, the transcript and the logs' lengths kept; `/usage` typed and Enter pressed only on
//!    the composer's own row and never where the reset program's dialog stands (a dialog takes the
//!    keys, and nothing is pressed into one); the panel read with `usage_panel_read`; Escape; the
//!    screen, the transcript and what each log gained since;
//! 4. the judges ([`judge`], pure functions of that evidence, held by
//!    `claude_wall_panel_tests.rs` to frames kept as fixtures): every step happened (a refused key
//!    or a wait into [`GUARD`] fails, never reads as a shut panel); whether the panel opened and at
//!    which read ([`FINAL_READS`]: `refused` wherever the account's body cannot arrive, as in this
//!    lab, so what the tab draws from that body over a wall, each model's week among it, stays
//!    unmeasured here), what it drew against the stub's figures, how many `GET /api/oauth/usage`
//!    the `/usage` sent, open or shut (Claude Code's debug log names each `fetchUtilization` it
//!    sends; the jail refuses it, and the stub's log names any that reached the stub) and how many
//!    `/v1/messages` turns (the stub's log), that the transcript's standing wall reads
//!    as before (`upgrade::transcript_standing_wall`, what `upgrade_drive::named_standing_wall`
//!    reads), and that Escape left the wall, Claude Code's own continuation and its dialog as they
//!    were;
//! 5. on a PASS, the frames this build has none of yet (all of them with `--record`) written to
//!    aterm-phase's fixtures (`claude-<build>-wall-panel-*.txt`,
//!    the lab's project folder spelled `/private/tmp/at.lab0`) and the connector's field written to
//!    what the run found ([`record_field`]): the build where the panel opened at every wall it
//!    was typed at, else `None`; on a FAIL, the frames and logs kept in a world of their own and
//!    the checkout left as it was.
//!
//! The last line is `claude-wall-panel: PASS build=<b> panel=opens|shut read=<…> gets=<n>
//! turns=<n> dialog=<…> field=<…>` ([`pass_line`]), or a FAIL naming each failed judge. What stays a
//! person's or an agent's: the commit.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_agent::harness::upgrade::{StandingWall, transcript_standing_wall};
use aterm_agent::harness::usage::{
    FABLE_WEEK_WINDOW, PanelRead, PanelWindow, USAGE_COMMAND, usage_panel_read, usage_panel_windows,
};

use crate::door::Door;
use crate::instance::{World, take_flag};
use crate::lab::{Launch, UpOpts};
use crate::paths::paths;
use crate::pyjson::J;
use crate::run::Run as Lab;
use crate::{Fail, LAB, Res};

/// The verb's name, as its lines say it.
pub(crate) const VERB: &str = "claude-wall-panel";

/// How long a wait for what must happen holds: a one-minute hang detector, never a latency budget.
/// A screen wait counts it from the screen's last change ([`screen_until`]): the minute is Claude
/// Code's minute without progress, not the wall's.
const HANG: Duration = Duration::from_mins(1);

/// [`HANG`] in seconds, as the door's waits take it.
const HANG_S: u64 = 60;

/// How long a screen that keeps changing may go without what a wait waits for: not a latency
/// budget (`aterm_spec::wait::WORKING_GUARD`'s length); a wait that reaches it went unanswered and
/// fails the run, never a measured outcome.
const GUARD: Duration = Duration::from_mins(15);

/// How long the screen's reading ([`reading`]) and the stub's log stay still before the screen is
/// read as settled.
const QUIET: Duration = Duration::from_secs(3);

/// The reads of an open Usage tab that end a load ([`panel_of`]): `settled`, the account's body
/// drawn, and `refused`, the tab saying it could not refresh (the lab's usage read refused in its
/// jail: Claude Code sends it to its fixed claude.ai base, never to the stub). `unsettled` (a
/// load in flight) and `fold` (the tab's tail off the grid) are no measurement.
pub(crate) const FINAL_READS: [&str; 2] = ["settled", "refused"];

/// The connector file the verdict is recorded in, from the checkout's root.
pub(crate) const CONNECTOR: &str = "crates/aterm-agent/src/ai/connector.rs";

/// The fixtures folder the frames are written to, from the checkout's root.
pub(crate) const FIXTURES: &str = "crates/aterm-phase/src/fixtures";

/// The connector's field this verb writes.
pub(crate) const FIELD: &str = "wall_panel_measured_on";

/// The connector's constant whose field it writes.
const CLAUDE_CONST: &str = "pub const CLAUDE_ACCOUNTS: Accounts = Accounts {";

/// The project folder every frame spells, whatever the lab's own random one was.
pub(crate) const PROJ_SPELLED: &str = "/private/tmp/at.lab0";

/// The launch's environment beside `--subscriber`'s token: the fullscreen renderer the managed
/// launch runs (`atpkg`'s launch tap) and the only one the probe reads a panel on, and a Max 5x
/// subscriber with the profile scope, without which `/usage` sends no usage read (2.1.301's lab).
pub(crate) const LAUNCH_ENV: [&str; 4] = [
    "CLAUDE_CODE_NO_FLICKER=1",
    "CLAUDE_CODE_OAUTH_SCOPES=user:inference user:profile",
    "CLAUDE_CODE_SUBSCRIPTION_TYPE=max",
    "CLAUDE_CODE_RATE_LIMIT_TIER=default_claude_max_5x",
];

/// A made-up word every prompt of the session carries.
const WORD: &str = "wombatfern";

/// The stub's figures, in percent used: the session's, the week's and Fable's week, each the
/// utilization its answers carry ([`blocks`]). A window whose limit stands is at 100 on the
/// limit's 429 and at its figure here on every 200 before it.
pub(crate) const FIGURES: [(&str, u32); 3] = [
    ("five_hour", 62),
    ("seven_day", 71),
    (FABLE_WEEK_WINDOW, 40),
];

/// A wall the verb drives into, as `lab limit` sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WallAt {
    /// The five-hour limit, resetting an hour on: Claude Code arms its own continuation.
    Session,
    /// The weekly limit, resetting 26 h on: Claude Code opens its usage-limit dialog itself,
    /// where the build draws one.
    Week,
}

impl WallAt {
    /// Both, in the order the session meets them.
    pub(crate) const ALL: [WallAt; 2] = [WallAt::Session, WallAt::Week];

    /// The limit's claim (`lab limit`'s CLAIM).
    #[must_use]
    pub(crate) const fn claim(self) -> &'static str {
        match self {
            WallAt::Session => "five_hour",
            WallAt::Week => "seven_day",
        }
    }

    /// The word a line and a fixture's name give it.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            WallAt::Session => "session",
            WallAt::Week => "week",
        }
    }

    /// How far on its limit resets (seconds): under a day for the session, so Claude Code
    /// continues by itself; over one for the week, so it opens its dialog.
    #[must_use]
    pub(crate) const fn reset_in(self) -> i64 {
        match self {
            WallAt::Session => 3600,
            WallAt::Week => 93_600,
        }
    }

    /// The notice the wall's turn ends on, as a pattern: 2.1.325 says `5-hour` where earlier
    /// builds said `session`.
    #[must_use]
    pub(crate) const fn notice(self) -> &'static str {
        match self {
            WallAt::Session => "hit your (session|5-hour) limit",
            WallAt::Week => "hit your (weekly|week) limit",
        }
    }

    /// The utilization header family of its window.
    const fn family(self) -> &'static str {
        match self {
            WallAt::Session => "5h",
            WallAt::Week => "7d",
        }
    }

    /// The figures the panel may draw for `window` at this wall: the stub's figure, and 100 for
    /// the window whose limit stands (its 429 carries it).
    #[must_use]
    pub(crate) fn may_draw(self, window: &str) -> Vec<u32> {
        let mut out: Vec<u32> = FIGURES
            .iter()
            .filter(|(w, _)| *w == window)
            .map(|(_, pct)| *pct)
            .collect();
        if window == self.claim() {
            out.push(100);
        }
        out
    }
}

/// The `{"on200": …, "on429": …}` headers the stub's answers carry at `wall` ([`FIGURES`]): every
/// window allowed at its figure on a 200, the walled one rejected at 1.0 on the limit's 429
/// (`@RESET@`: the limit's own reset), the others at their figures there too; `now` the unix
/// second they are laid at.
#[must_use]
pub(crate) fn blocks(wall: WallAt, now: i64) -> J {
    let reset = |family: &str| match family {
        "5h" => now + 2 * 3600,
        _ => now + 4 * 86_400,
    };
    let fraction = |pct: u32| format!("{:.2}", f64::from(pct) / 100.0);
    let families = [
        ("5h", FIGURES[0].1),
        ("7d", FIGURES[1].1),
        ("7d_oi", FIGURES[2].1),
    ];
    let window = |family: &str, pct: u32, walled: bool| {
        let h = |k: &str| format!("anthropic-ratelimit-unified-{family}-{k}");
        if walled {
            vec![
                (h("status"), J::s("rejected")),
                (h("utilization"), J::s("1.00")),
                (h("reset"), J::s("@RESET@")),
            ]
        } else {
            vec![
                (h("status"), J::s("allowed")),
                (h("utilization"), J::s(fraction(pct))),
                (h("reset"), J::Int(reset(family))),
            ]
        }
    };
    let mut ok = vec![(
        "anthropic-ratelimit-unified-status".to_string(),
        J::s("allowed"),
    )];
    let mut walled = Vec::new();
    for (family, pct) in families {
        ok.extend(window(family, pct, false));
        walled.extend(window(family, pct, family == wall.family()));
    }
    ok.push((
        "anthropic-ratelimit-unified-representative-claim".to_string(),
        J::s("five_hour"),
    ));
    J::obj([("on200", J::Obj(ok)), ("on429", J::Obj(walled))])
}

/// What a wall's measurement kept ([`measure`]): the screen at the wall, after `/usage` and after
/// Escape, the transcript before and after, and what each log gained over the measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Evidence {
    /// The screen's rows with the wall standing, before anything was typed.
    pub before: Vec<String>,
    /// Whether `/usage` was typed, and why not where it was not.
    pub typed: Result<(), String>,
    /// The screen's rows once `/usage` drew what it draws (`None`: not typed).
    pub panel: Option<Vec<String>>,
    /// Whether Escape was pressed (only over an open panel).
    pub escaped: bool,
    /// The screen's rows at the end of the measurement.
    pub after: Vec<String>,
    /// The session's transcript before and after.
    pub transcript_before: String,
    pub transcript_after: String,
    /// What the stub's log gained over the measurement.
    pub stub_log: String,
    /// What Claude Code's debug log gained over the measurement.
    pub debug_log: String,
    /// Each step that did not happen: a key or a send the lab's door refused, a wait that ran into
    /// [`GUARD`]. Each fails the run.
    pub unanswered: Vec<String>,
}

impl Default for Evidence {
    fn default() -> Self {
        Evidence {
            before: Vec::new(),
            typed: Err("not measured".into()),
            panel: None,
            escaped: false,
            after: Vec::new(),
            transcript_before: String::new(),
            transcript_after: String::new(),
            stub_log: String::new(),
            debug_log: String::new(),
            unanswered: Vec::new(),
        }
    }
}

/// `text` as rows, the no-break spaces `ctl text` may give read as spaces.
#[must_use]
pub(crate) fn rows_of(text: &str) -> Vec<String> {
    text.replace('\u{a0}', " ")
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// What the screen showed of the panel ([`panel_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Panel {
    /// The Settings panel stands on its Usage tab: `usage_panel_read`'s word, and the windows
    /// it drew (the settled read's, else every window the rows show).
    Opens {
        read: &'static str,
        windows: Vec<PanelWindow>,
    },
    /// No Usage tab stands: why.
    Shut(String),
}

/// `usage_panel_read`'s word for a read (an `Unsettled` that says it could not refresh is
/// `refused` in [`panel_of`]).
#[must_use]
pub(crate) fn read_word(read: &PanelRead) -> &'static str {
    match read {
        PanelRead::NotUsage => "not-usage",
        PanelRead::Unsettled => "unsettled",
        PanelRead::Fold => "fold",
        PanelRead::Settled(_) => "settled",
    }
}

/// What `rows` show of the Usage tab: [`Panel::Opens`] where the Settings panel stands with a
/// row only the Usage tab draws under its header (`usage_panel_read` reads it as anything but
/// `NotUsage`), its read `refused` where an unsettled tab says it could not refresh; else
/// [`Panel::Shut`].
#[must_use]
pub(crate) fn panel_of(rows: &[String]) -> Panel {
    let Some(panel) = aterm_phase::settings_panel(rows) else {
        return Panel::Shut("no Settings panel".into());
    };
    let under = &rows[panel.header..];
    let read = usage_panel_read(rows, panel.header);
    match read {
        PanelRead::NotUsage => Panel::Shut("a Settings panel with no Usage tab".into()),
        PanelRead::Settled(windows) => Panel::Opens {
            read: "settled",
            windows,
        },
        PanelRead::Unsettled
            if under.iter().any(|r| {
                r.trim()
                    .starts_with(aterm_phase::anchor!("usage.could_not_refresh"))
            }) =>
        {
            Panel::Opens {
                read: "refused",
                windows: usage_panel_windows(under),
            }
        }
        other => Panel::Opens {
            read: read_word(&other),
            windows: usage_panel_windows(under),
        },
    }
}

/// Whether `rows` show the Usage tab at a read that ends its load ([`FINAL_READS`]).
#[must_use]
pub(crate) fn final_panel(rows: &[String]) -> bool {
    matches!(panel_of(rows), Panel::Opens { read, .. } if FINAL_READS.contains(&read))
}

/// Whether the composer on `rows` holds `/usage` alone.
#[must_use]
pub(crate) fn holds_usage(rows: &[String]) -> bool {
    crate::screen::composer_row("claude", &rows.join("\n"))
        .is_some_and(|t| t.trim() == USAGE_COMMAND)
}

/// The dialog of the reset program standing on `rows`, by its word (`dialog`, `offer`, …), where
/// one stands ([`aterm_phase::reset_sighting`]).
#[must_use]
pub(crate) fn dialog_of(rows: &[String]) -> Option<&'static str> {
    aterm_phase::reset_sighting(rows).map(|s| s.sight.word())
}

/// The `GET /api/oauth/usage` reads a measurement's logs name: each `fetchUtilization` Claude
/// Code's debug log names, or each usage read the stub's log served, whichever is more (a read
/// that reached the stub is in both).
#[must_use]
pub(crate) fn gets(debug_log: &str, stub_log: &str) -> usize {
    let debug = debug_log
        .lines()
        .filter(|l| l.contains("fetchUtilization") && l.contains("GET /api/oauth/usage"))
        .count();
    let stub = stub_log
        .lines()
        .filter(|l| l.contains(" GET /api/oauth/usage -> "))
        .count();
    debug.max(stub)
}

/// The `/v1/messages` requests the stub's log names (its `count_tokens` is no turn).
#[must_use]
pub(crate) fn turns(stub_log: &str) -> usize {
    stub_log
        .lines()
        .filter(|l| l.contains(" /v1/messages -> "))
        .count()
}

/// The wall, the continuation and the dialog a screen shows, as a comparison reads them: the
/// wall's kind, words and reset ([`aterm_phase::wall`], never its row), how Claude Code's own
/// continuation stands ([`aterm_phase::auto_continue_end`] and the rows that name it), and the
/// reset program's sight ([`dialog_of`]).
#[must_use]
pub(crate) fn standing(rows: &[String]) -> String {
    let wall = aterm_phase::wall(rows).map_or_else(
        || "no wall".to_string(),
        |w| format!("{:?}: {} (reset {:?})", w.kind, w.message, w.reset),
    );
    let said: Vec<&str> = rows
        .iter()
        .map(|r| r.trim())
        .filter(|t| {
            let l = t.to_ascii_lowercase();
            l.contains("continuing automatically")
                || l.contains("continuing shortly")
                || l.contains("automatic continue")
        })
        .collect();
    format!(
        "{wall}; continue {:?} {said:?}; dialog {:?}",
        aterm_phase::auto_continue_end(rows),
        dialog_of(rows)
    )
}

/// The windows a panel at `read` must draw: the session's and the week's, which the wall turn's
/// answer headers carry, always; Fable's week, which only the account's body carries, on a
/// `settled` read alone (a `refused` read never had the body).
#[must_use]
pub(crate) fn required(read: &str) -> Vec<&'static str> {
    let mut out = vec![FIGURES[0].0, FIGURES[1].0];
    if read == "settled" {
        out.push(FIGURES[2].0);
    }
    out
}

/// The figures a panel at `read` drew against the stub's at `wall`: one `window:pct` per window
/// of [`FIGURES`] (`-` where it drew none), and each window drawn at a figure the stub never gave
/// or [`required`] and not drawn.
#[must_use]
pub(crate) fn figures(wall: WallAt, read: &str, windows: &[PanelWindow]) -> (String, Vec<String>) {
    let mut said = Vec::new();
    let mut wrong = Vec::new();
    let must = required(read);
    for (window, _) in FIGURES {
        match windows.iter().find(|w| w.name == window) {
            Some(w) => {
                said.push(format!("{window}:{}", w.used_pct));
                let may = wall.may_draw(window);
                if !may.contains(&w.used_pct) {
                    wrong.push(format!(
                        "{window} drawn at {}% where the stub gave {may:?}",
                        w.used_pct
                    ));
                }
            }
            None => {
                said.push(format!("{window}:-"));
                if must.contains(&window) {
                    wrong.push(format!("{window} not drawn on a {read} read"));
                }
            }
        }
    }
    for w in windows {
        if !FIGURES.iter().any(|(name, _)| *name == w.name) {
            said.push(format!("{}:{}", w.name, w.used_pct));
        }
    }
    (said.join(","), wrong)
}

/// What the judges found of one wall ([`judge`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Judged {
    pub wall: WallAt,
    /// The panel, where `/usage` was typed (`None`: it was not).
    pub panel: Option<Panel>,
    /// The reset program's dialog standing at the wall, by its word.
    pub dialog: Option<&'static str>,
    /// The usage reads and the turns the measurement made.
    pub gets: usize,
    pub turns: usize,
    /// The figures it drew ([`figures`]), where it opened.
    pub figures: String,
    /// The transcript's standing wall, as it read before.
    pub standing_wall: Option<StandingWall>,
    /// Each judge that failed.
    pub fails: Vec<String>,
}

/// THE JUDGES of one wall's evidence: every step happened (no refused key, no wait into
/// [`GUARD`]); `/usage`, typed, left the composer and made one usage read, open panel or shut
/// ([`gets`]: a shut panel with no read is a `/usage` that never ran), and no turn ([`turns`]); an
/// open panel at a read that ended its load ([`FINAL_READS`]) drawing the stub's figures
/// ([`figures`]) and gone after Escape; the transcript's standing wall read as before, and some
/// wall standing there; the screen's wall, continuation and dialog as before ([`standing`]), and
/// no `/usage` left in the composer.
#[must_use]
pub(crate) fn judge(wall: WallAt, e: &Evidence) -> Judged {
    let mut fails: Vec<String> = e
        .unanswered
        .iter()
        .map(|u| format!("the {} wall: {u}", wall.word()))
        .collect();
    let dialog = dialog_of(&e.before);
    let gets_n = gets(&e.debug_log, &e.stub_log);
    let turns_n = turns(&e.stub_log);
    if turns_n != 0 {
        fails.push(format!(
            "the {} wall's measurement sent {turns_n} /v1/messages request(s)",
            wall.word()
        ));
    }
    let standing_wall = transcript_standing_wall(&e.transcript_before);
    let after_wall = transcript_standing_wall(&e.transcript_after);
    if standing_wall.is_none() {
        fails.push(format!(
            "the {} wall: the transcript names no standing wall before /usage",
            wall.word()
        ));
    } else if after_wall != standing_wall {
        fails.push(format!(
            "the {} wall: /usage changed the transcript's standing wall ({standing_wall:?} to {after_wall:?})",
            wall.word()
        ));
    }
    let (was, now) = (standing(&e.before), standing(&e.after));
    if was != now {
        fails.push(format!(
            "the {} wall: the screen's wall, continuation or dialog changed ({was} to {now})",
            wall.word()
        ));
    }
    let mut figures_said = String::new();
    let panel = match (&e.typed, &e.panel) {
        (Err(_), _) => None,
        (Ok(()), None) => {
            fails.push(format!(
                "the {} wall: /usage typed with no screen read after it",
                wall.word()
            ));
            None
        }
        (Ok(()), Some(rows)) => {
            let p = panel_of(rows);
            if gets_n != 1 {
                fails.push(format!(
                    "the {} wall: /usage made {gets_n} GET /api/oauth/usage, not one",
                    wall.word()
                ));
            }
            if holds_usage(rows) {
                fails.push(format!(
                    "the {} wall: /usage still stood in the composer after Enter",
                    wall.word()
                ));
            }
            if let Panel::Opens { read, windows } = &p {
                if !FINAL_READS.contains(read) {
                    fails.push(format!(
                        "the {} wall: the Usage tab never ended its load (read {read})",
                        wall.word()
                    ));
                }
                let (said, wrong) = figures(wall, read, windows);
                figures_said = said;
                fails.extend(
                    wrong
                        .into_iter()
                        .map(|w| format!("the {} wall: {w}", wall.word())),
                );
                if !e.escaped {
                    fails.push(format!("the {} wall: Escape not pressed", wall.word()));
                } else if matches!(panel_of(&e.after), Panel::Opens { .. }) {
                    fails.push(format!(
                        "the {} wall: the panel still open after Escape",
                        wall.word()
                    ));
                }
            }
            Some(p)
        }
    };
    if e.typed.is_ok() && holds_usage(&e.after) {
        fails.push(format!(
            "the {} wall: the measurement left /usage in the composer",
            wall.word()
        ));
    }
    if let Err(why) = &e.typed
        && dialog.is_none()
    {
        fails.push(format!(
            "the {} wall: /usage not typed with no dialog standing ({why})",
            wall.word()
        ));
    }
    Judged {
        wall,
        panel,
        dialog,
        gets: gets_n,
        turns: turns_n,
        figures: figures_said,
        standing_wall,
        fails,
    }
}

/// What the run found across its walls ([`verdict`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// `opens` where the panel opened at every wall `/usage` was typed at (one at least), else
    /// `shut`.
    pub panel: &'static str,
    /// The read of each panel that opened ([`panel_of`]): one word where they agree, `-` where
    /// none opened.
    pub read: String,
    /// The usage reads of each `/usage` typed: one number where they agree.
    pub gets: String,
    /// The turns every measurement sent.
    pub turns: usize,
    /// `none` where no wall drew the reset program's dialog, else `<sight>:<opens|shut|untyped>`
    /// for each wall that did.
    pub dialog: String,
    /// Each judge that failed, across the walls.
    pub fails: Vec<String>,
}

/// The run's verdict from each wall's judged evidence.
#[must_use]
pub(crate) fn verdict(judged: &[Judged]) -> Verdict {
    let typed: Vec<&Judged> = judged.iter().filter(|j| j.panel.is_some()).collect();
    let opens = !typed.is_empty()
        && typed
            .iter()
            .all(|j| matches!(j.panel, Some(Panel::Opens { .. })));
    let mut gets: Vec<String> = typed.iter().map(|j| j.gets.to_string()).collect();
    gets.dedup();
    let mut reads: Vec<&str> = typed
        .iter()
        .filter_map(|j| match &j.panel {
            Some(Panel::Opens { read, .. }) => Some(*read),
            _ => None,
        })
        .collect();
    reads.dedup();
    let dialogs: Vec<String> = judged
        .iter()
        .filter_map(|j| {
            let sight = j.dialog?;
            let how = match &j.panel {
                None => "untyped",
                Some(Panel::Opens { .. }) => "opens",
                Some(Panel::Shut(_)) => "shut",
            };
            Some(format!("{sight}:{how}"))
        })
        .collect();
    let mut fails: Vec<String> = judged.iter().flat_map(|j| j.fails.clone()).collect();
    if typed.is_empty() {
        fails.push("/usage was typed at no wall".into());
    }
    Verdict {
        panel: if opens { "opens" } else { "shut" },
        read: if reads.is_empty() {
            "-".into()
        } else {
            reads.join(",")
        },
        gets: if gets.is_empty() {
            "-".into()
        } else {
            gets.join(",")
        },
        turns: judged.iter().map(|j| j.turns).sum(),
        dialog: if dialogs.is_empty() {
            "none".into()
        } else {
            dialogs.join(",")
        },
        fails,
    }
}

/// The value the connector's field takes from a passing `verdict` on `build`: the build where the
/// panel opened, else none.
#[must_use]
pub(crate) fn field_wants(v: &Verdict, build: &str) -> Option<String> {
    (v.fails.is_empty() && v.panel == "opens").then(|| build.to_string())
}

/// The line of [`CLAUDE_CONST`]'s block that holds [`FIELD`] in `source`: its index among
/// `source`'s lines.
fn field_line(source: &str) -> Result<usize, String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == CLAUDE_CONST)
        .ok_or_else(|| format!("{CONNECTOR} holds no `{CLAUDE_CONST}`"))?;
    let head = format!("    {FIELD}: ");
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        if l.starts_with("};") {
            break;
        }
        if l.starts_with(&head) {
            return Ok(i);
        }
    }
    Err(format!(
        "{CONNECTOR}'s CLAUDE_ACCOUNTS holds no `{FIELD}` line"
    ))
}

/// The field's value as `source`'s `CLAUDE_ACCOUNTS` holds it: `None`, or the build it names.
/// # Errors
/// The block or the line is missing, or the value is neither `None` nor `Some("…")`.
pub(crate) fn field_in(source: &str) -> Result<Option<String>, String> {
    let line = source.lines().nth(field_line(source)?).unwrap_or_default();
    let value = line
        .trim()
        .strip_prefix(FIELD)
        .and_then(|r| r.strip_prefix(':'))
        .map(str::trim)
        .and_then(|r| r.strip_suffix(','))
        .unwrap_or_default();
    if value == "None" {
        return Ok(None);
    }
    value
        .strip_prefix("Some(\"")
        .and_then(|r| r.strip_suffix("\")"))
        .filter(|b| is_build(b))
        .map(|b| Some(b.to_string()))
        .ok_or_else(|| format!("{CONNECTOR}'s `{FIELD}` reads `{value}`, not None or a build"))
}

/// Whether `b` is a build as `claude --version` says one (digits and dots, a digit at each end).
#[must_use]
pub(crate) fn is_build(b: &str) -> bool {
    crate::claude_measure::version_of(b).as_deref() == Some(b)
}

/// `source` with `CLAUDE_ACCOUNTS`' [`FIELD`] set to `value`, every other byte as it was.
/// # Errors
/// As [`field_in`]; or `value` is no build.
pub(crate) fn record_field(source: &str, value: Option<&str>) -> Result<String, String> {
    let at = field_line(source)?;
    let said = match value {
        None => "None".to_string(),
        Some(b) if is_build(b) => format!("Some(\"{b}\")"),
        Some(b) => return Err(format!("`{b}` is no build")),
    };
    let mut out = String::with_capacity(source.len() + 16);
    for (i, l) in source.split_inclusive('\n').enumerate() {
        if i == at {
            out.push_str(&format!("    {FIELD}: {said},\n"));
        } else {
            out.push_str(l);
        }
    }
    Ok(out)
}

/// The last line of a run that passed: the build, the panel and its read, the usage reads and
/// turns, the dialog and the field as the checkout holds it.
#[must_use]
pub(crate) fn pass_line(build: &str, v: &Verdict, field: Option<&str>) -> String {
    format!(
        "{VERB}: PASS build={build} panel={} read={} gets={} turns={} dialog={} field={}",
        v.panel,
        v.read,
        v.gets,
        v.turns,
        v.dialog,
        field.unwrap_or("None")
    )
}

/// Where a fixture spells the lab's own run folder (its scratch, its debug log).
pub(crate) const RUN_SPELLED: &str = "/private/tmp/at.lab0/run";

/// `text` with each of the lab's own folders (`(folder, spelled)`: its project folder as
/// [`PROJ_SPELLED`], its run folder as [`RUN_SPELLED`]), as `/tmp/…` and `/private/tmp/…` alike,
/// spelled as given, so no fixture names a run's random folder.
#[must_use]
pub(crate) fn scrub(text: &str, folders: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (folder, spelled) in folders {
        let bare = folder.trim_end_matches('/');
        if bare.is_empty() {
            continue;
        }
        let private = if bare.starts_with("/private/") {
            bare.to_string()
        } else {
            format!("/private{bare}")
        };
        out = out.replace(&private, spelled);
        if let Some(plain) = private.strip_prefix("/private") {
            out = out.replace(plain, spelled);
        }
    }
    out
}

/// A fixture's text: its provenance line, then the rows (trailing blank rows dropped).
#[must_use]
pub(crate) fn fixture(build: &str, date: &str, what: &str, rows: &[String]) -> String {
    let mut end = rows.len();
    while end > 0 && rows[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut out = format!(
        "# claude-code {build} \u{b7} MEASURED {date}, `aterm ctl text` of a private headless aterm (144x50) running the real Claude Code by `{LAB} {VERB}`, launched by path with a scratch `CLAUDE_CONFIG_DIR`, a fake claude.ai OAuth token (`claude --subscriber`, the scopes user:inference user:profile, the composed oauthAccount of ana@example.com) and `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` (a local stub of the API, crates/aterm-lab), the fullscreen renderer (CLAUDE_CODE_NO_FLICKER=1), the harness off; the stub's answers carry 5h 0.62, 7d 0.71 and 7d_oi 0.40, a walled window 1.00 on its 429 \u{b7} {what} \u{b7} the lab's random project dir is spelled {PROJ_SPELLED} here, its run dir {RUN_SPELLED}\n"
    );
    for r in &rows[..end] {
        out.push_str(r.trim_end());
        out.push('\n');
    }
    out
}

/// The frames of one wall's evidence, as `(name stem, what, rows)`.
#[must_use]
pub(crate) fn frames(wall: WallAt, e: &Evidence) -> Vec<(String, String, Vec<String>)> {
    let w = wall.word();
    let mut out = vec![(
        format!("wall-panel-{w}-wall"),
        format!(
            "the {} limit's wall standing (`lab limit {} {}`), before anything was typed",
            w,
            wall.claim(),
            wall.reset_in()
        ),
        e.before.clone(),
    )];
    if let Some(p) = &e.panel {
        out.push((
            format!("wall-panel-{w}-usage"),
            format!("what `/usage` and Enter on the composer drew over the {w} wall, settled"),
            p.clone(),
        ));
        out.push((
            format!("wall-panel-{w}-escaped"),
            format!(
                "after {} over the {w} wall's `/usage`, settled",
                if e.escaped {
                    "one Escape"
                } else {
                    "nothing pressed"
                }
            ),
            e.after.clone(),
        ));
    }
    out
}

/// The verb's own lab, taken down when it drops (a refusal, an interrupt, a panic).
struct Own(String);

impl Drop for Own {
    fn drop(&mut self) {
        let _ = crate::lab::down(&self.0, false);
    }
}

/// The length of the file at `p` (0 where it is missing).
fn len_of(p: &Path) -> usize {
    crate::util::scratch_text(p).len()
}

/// What the file at `p` gained past `from` bytes.
fn gained(p: &Path, from: usize) -> String {
    let t = crate::util::scratch_text(p);
    t.get(from..).unwrap_or_default().to_string()
}

/// The session's transcript (the largest `.jsonl` under the scratch `projects/`).
fn transcript(lab: &Lab) -> String {
    let mut found = Vec::new();
    crate::util::walk_files(&lab.w.join("home/.claude/projects"), &mut found);
    found
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .map(|p| crate::util::scratch_text(&p))
        .max_by_key(String::len)
        .unwrap_or_default()
}

/// What a screen comparison reads of `rows`: the panel ([`panel_of`]) and the wall, continuation
/// and dialog ([`standing`]), never the rows themselves, so a figure Claude Code keeps redrawing
/// (a duration, a spinner) is no change.
#[must_use]
pub(crate) fn reading(rows: &[String]) -> String {
    format!("{:?}; {}", panel_of(rows), standing(rows))
}

/// How a screen wait ended ([`screen_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitEnd {
    /// What it waited for holds.
    Held,
    /// The screen did not change for [`HANG`] and what it waited for does not hold: Claude Code
    /// did what it does and drew it not.
    Quiet,
    /// The screen kept changing for [`GUARD`] without it: unanswered.
    Guard,
}

/// Poll the screen until `done` holds of its rows, the screen stays unchanged for [`HANG`]
/// (Claude Code's minute without progress), or [`GUARD`] passes: the rows last read and how the
/// wait ended.
fn screen_until(
    door: &Door<'_>,
    done: &dyn Fn(&[String]) -> bool,
) -> Result<(Vec<String>, WaitEnd), Fail> {
    let start = Instant::now();
    let mut last: Option<(Vec<String>, Instant)> = None;
    loop {
        let rows = rows_of(&door.out(&["text"]));
        if done(&rows) {
            return Ok((rows, WaitEnd::Held));
        }
        match &last {
            Some((seen, at)) if *seen == rows => {
                if at.elapsed() >= HANG {
                    return Ok((rows, WaitEnd::Quiet));
                }
            }
            _ => last = Some((rows.clone(), Instant::now())),
        }
        if start.elapsed() >= GUARD {
            return Ok((rows, WaitEnd::Guard));
        }
        crate::stop::sleep(Duration::from_millis(250))?;
    }
}

/// The screen settled: its [`reading`] and the stub's log both unchanged for [`QUIET`]. `None`
/// once it did; why not where [`GUARD`] passed first.
fn settle(door: &Door<'_>, lab: &Lab) -> Result<Option<String>, Fail> {
    let log = lab.w.join("stub.log");
    let start = Instant::now();
    let mut last = (String::new(), usize::MAX, Instant::now());
    loop {
        let now = (reading(&rows_of(&door.out(&["text"]))), len_of(&log));
        if (&now.0, now.1) != (&last.0, last.1) {
            last = (now.0, now.1, Instant::now());
        } else if last.2.elapsed() >= QUIET {
            return Ok(None);
        }
        if start.elapsed() >= GUARD {
            return Ok(Some(format!(
                "the screen or the stub's log kept changing for {GUARD:?}: never settled"
            )));
        }
        crate::stop::sleep(Duration::from_millis(250))?;
    }
}

/// `ctl <args>` into the lab, `true` where the door took it; a refusal is kept in `e.unanswered`.
fn press(door: &Door<'_>, args: &[&str], e: &mut Evidence) -> Result<bool, Fail> {
    let r = door.ctl(args)?;
    if r.code != 0 {
        e.unanswered.push(format!(
            "`ctl {}` refused (exit {}): {}",
            args.join(" "),
            r.code,
            r.stderr.trim()
        ));
    }
    Ok(r.code == 0)
}

/// Keep a wait that ran into [`GUARD`] in `e.unanswered`; the rows it read.
fn waited(e: &mut Evidence, what: &str, (rows, end): (Vec<String>, WaitEnd)) -> Vec<String> {
    if end == WaitEnd::Guard {
        e.unanswered.push(format!(
            "the screen kept changing for {GUARD:?} without {what}"
        ));
    }
    rows
}

/// STEP 3, one wall's measurement, with the wall standing and settled: the screen, the
/// transcript and the logs' lengths; `/usage` typed only where the composer stands and no dialog
/// of the reset program does, standing alone in the composer before Enter; the screen once the
/// Usage tab came to a read that ends its load, or once the screen went a minute unchanged with
/// none (a shut panel, which the judges hold to its one usage read); Escape only over an open
/// panel, then the panel gone; the screen settled, the transcript and what each log gained. A
/// refused key and a wait into [`GUARD`] are kept as steps that did not happen.
fn measure(door: &Door<'_>, lab: &Lab) -> Result<Evidence, Fail> {
    let (stub, debug) = (lab.w.join("stub.log"), crate::stats::debug_log(lab));
    let screen = door.out(&["text"]);
    let mut e = Evidence {
        before: rows_of(&screen),
        transcript_before: transcript(lab),
        ..Evidence::default()
    };
    let (stub_at, debug_at) = (len_of(&stub), len_of(&debug));
    e.typed = if let Some(sight) = dialog_of(&e.before) {
        Err(format!("the reset program's {sight} stands"))
    } else if crate::screen::composer_row("claude", &screen).is_none() {
        Err("no composer row".into())
    } else {
        Ok(())
    };
    if e.typed.is_ok() && press(door, &["send", USAGE_COMMAND], &mut e)? {
        let drafted = screen_until(door, &|rows| holds_usage(rows))?;
        let drafted = waited(&mut e, "/usage in the composer", drafted);
        if holds_usage(&drafted) {
            if press(door, &["key", "enter"], &mut e)? {
                let shown = screen_until(door, &|rows| final_panel(rows))?;
                waited(&mut e, "the Usage tab's final read", shown);
                // The read the judges take: the screen once its reading stood still.
                if let Some(why) = settle(door, lab)? {
                    e.unanswered.push(format!("after Enter, {why}"));
                }
                let panel = rows_of(&door.out(&["text"]));
                if matches!(panel_of(&panel), Panel::Opens { .. })
                    && press(door, &["key", "escape"], &mut e)?
                {
                    e.escaped = true;
                    let gone =
                        screen_until(door, &|rows| aterm_phase::settings_panel(rows).is_none())?;
                    waited(&mut e, "the panel gone", gone);
                }
                e.panel = Some(panel);
            }
        } else {
            e.typed = Err(format!(
                "the composer never held `{USAGE_COMMAND}` alone: {:?}",
                crate::screen::composer_row("claude", &drafted.join("\n"))
            ));
        }
    }
    if let Some(why) = settle(door, lab)? {
        e.unanswered.push(format!("at the end, {why}"));
    }
    e.after = rows_of(&door.out(&["text"]));
    e.transcript_after = transcript(lab);
    e.stub_log = gained(&stub, stub_at);
    e.debug_log = gained(&debug, debug_at);
    Ok(e)
}

/// STEPS 2 AND 3: the session in a lab of the verb's own, each wall's evidence once every step
/// before it held; the lab is down when it returns unless `keep`.
/// # Errors
/// The run's interrupt; the lab could not come up, runs another build, or a step that must
/// happen (the banner, a turn, a wall) did not.
fn session(build: &str, keep: bool) -> Result<(Lab, Vec<(WallAt, Evidence)>), Fail> {
    let tag = format!("cwp{}", std::process::id());
    let o = UpOpts {
        trust: true,
        harness_keys: "enabled = false".into(),
        cols: "144".into(),
        lines: "50".into(),
        lifeline: true,
        ..UpOpts::default()
    };
    let lab = crate::lab::up(&tag, &o)?;
    let own = Own(tag.clone());
    if lab.claude_ver != build {
        return Err(Fail::die(format!(
            "{VERB}: the lab's Claude Code ({}) says it is {} where the managed one is {build}",
            lab.claude_bin, lab.claude_ver
        )));
    }
    crate::util::atomic_write(
        &lab.w.join("account.json"),
        crate::stub::account::composed().py().as_bytes(),
    )
    .map_err(|e| Fail::die(format!("the stub's account file: {e}")))?;
    crate::stats::set_claude_json(
        &lab,
        "oauthAccount",
        crate::stub::account::composed_oauth_account(crate::util::now_ms()),
    )?;
    let now = crate::util::now_secs();
    crate::verbs::write_headers(&lab.w, blocks(WallAt::Session, now), true)?;
    let launch = Launch {
        subscriber: true,
        debug_log: true,
        envs: LAUNCH_ENV.iter().map(|s| (*s).to_string()).collect(),
        ..Launch::default()
    };
    crate::lab::launch_claude(&lab, &launch)?;
    let lab = Lab::load(&tag)?;
    let door = Door::new(&lab)?;
    let must = |ok: bool, what: &str| {
        if ok {
            return Ok(());
        }
        // The lab goes down with the refusal: what it showed goes with it, the screen's
        // rows and the stub's last lines.
        let screen: Vec<String> = rows_of(&door.out(&["text"]))
            .into_iter()
            .filter(|r| !r.trim().is_empty())
            .collect();
        let stub = crate::util::scratch_text(&lab.w.join("stub.log"));
        let stub: Vec<&str> = stub.lines().rev().take(8).collect();
        Err(Fail::die(format!(
            "{VERB}: FAIL {what}\n  the screen:\n    {}\n  the stub's last lines:\n    {}",
            screen.join("\n    "),
            stub.into_iter().rev().collect::<Vec<_>>().join("\n    ")
        )))
    };
    must(
        door.seen("Claude Code v", HANG_S)?,
        "Claude Code drew no banner",
    )?;
    door.await_idle(1500, HANG_S)?;
    crate::verbs::set_mode(&lab, "ok")?;
    must(
        crate::verbs::turn(&door, &format!("say hello {WORD}"))?,
        "the first turn was not taken",
    )?;
    must(
        door.seen("local lab stub", HANG_S)?,
        "the first turn was not answered",
    )?;
    door.await_idle(1500, HANG_S)?;
    let mut out = Vec::new();
    for wall in WallAt::ALL {
        let now = crate::util::now_secs();
        crate::verbs::write_headers(&lab.w, blocks(wall, now), true)?;
        crate::verbs::set_limit(&lab, wall.claim(), now + wall.reset_in(), "")?;
        must(
            crate::verbs::turn(
                &door,
                &format!("one more {WORD} into the {} wall", wall.word()),
            )?,
            &format!("the {} wall's turn was not taken", wall.word()),
        )?;
        must(
            door.seen(wall.notice(), HANG_S)?,
            &format!("the {} wall's turn did not meet its limit", wall.word()),
        )?;
        let unsettled = settle(&door, &lab)?;
        must(
            unsettled.is_none(),
            &format!(
                "the {} wall: {}",
                wall.word(),
                unsettled.as_deref().unwrap_or_default()
            ),
        )?;
        let e = measure(&door, &lab)?;
        println!(
            "{VERB}: the {} wall: typed {}, panel {}, escaped {}",
            wall.word(),
            match &e.typed {
                Ok(()) => "yes".to_string(),
                Err(why) => format!("no ({why})"),
            },
            e.panel
                .as_deref()
                .map_or("-".to_string(), |r| format!("{:?}", panel_of(r))),
            e.escaped
        );
        out.push((wall, e));
    }
    if keep {
        std::mem::forget(own);
        println!("{VERB}: kept the lab '{tag}' (`{LAB} down {tag}`)");
    }
    Ok((lab, out))
}

/// Whether a passing run writes its capture of a frame: where this build has no
/// committed frame of that name, or where the run was asked to (`--record`). Claude
/// Code's screen differs between runs of one build beyond when they were taken (its
/// spinner word, an effort row, the status line's hints, the order rows settle in),
/// so a committed frame is a build's first capture, kept until someone records again:
/// a re-run judges its own fresh frames and leaves the checkout as it was.
pub(crate) fn writes_frame(committed: bool, record: bool) -> bool {
    record || !committed
}

/// Write `text` to `path`.
fn write(path: &Path, text: &str) -> Result<(), Fail> {
    crate::util::atomic_write(path, text.as_bytes())
        .map_err(|e| Fail::die(format!("{}: {e}", path.display())))
}

/// `claude-wall-panel [--keep] [--record]`.
/// # Errors
/// Usage; the run's interrupt; NOT RUN where the managed Claude Code or the lab is missing; a
/// FAIL where a step that must happen did not.
pub fn cmd_claude_wall_panel(args: &[String]) -> Res {
    crate::stop::install();
    let mut a = args.to_vec();
    let keep = take_flag(&mut a, "--keep");
    let record = take_flag(&mut a, "--record");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "{VERB}: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let repo = paths().repo.clone();
    let bin = crate::lab::find_claude().map_err(crate::measure::not_run)?;
    let build = crate::claude_measure::installed_build(&bin).map_err(Fail::from)?;
    println!("{VERB}: 1. the managed Claude Code is {build}");
    let (lab, walls) = session(&build, keep)?;
    let judged: Vec<Judged> = walls.iter().map(|(w, e)| judge(*w, e)).collect();
    for j in &judged {
        println!(
            "{VERB}: {} wall: panel {:?}, dialog {}, gets {}, turns {}, figures {}, transcript wall {:?}",
            j.wall.word(),
            j.panel,
            j.dialog.unwrap_or("none"),
            j.gets,
            j.turns,
            if j.figures.is_empty() {
                "-"
            } else {
                &j.figures
            },
            j.standing_wall
        );
    }
    let v = verdict(&judged);
    let date = crate::measure::today();
    let frames: Vec<(String, String)> = walls
        .iter()
        .flat_map(|(w, e)| frames(*w, e))
        .map(|(stem, what, rows)| {
            (
                format!("claude-{build}-{stem}.txt"),
                scrub(
                    &fixture(&build, &date, &what, &rows),
                    &[
                        (lab.run.to_string_lossy().as_ref(), RUN_SPELLED),
                        (&lab.sockdir, PROJ_SPELLED),
                    ],
                ),
            )
        })
        .collect();
    if !v.fails.is_empty() {
        let mut world = World::new("").map_err(crate::measure::not_run)?;
        world.keep();
        for (name, text) in &frames {
            write(&world.dir.join(name), text)?;
        }
        for (w, e) in &walls {
            write(
                &world.dir.join(format!("{}-stub.log", w.word())),
                &e.stub_log,
            )?;
            write(
                &world.dir.join(format!("{}-debug.log", w.word())),
                &e.debug_log,
            )?;
        }
        for f in &v.fails {
            println!("{VERB}: FAIL {f}");
        }
        println!(
            "{VERB}: FAIL build={build} panel={} read={} gets={} turns={} dialog={} ({} failed; the frames and logs kept in {}; the checkout as it was)",
            v.panel,
            v.read,
            v.gets,
            v.turns,
            v.dialog,
            v.fails.len(),
            world.dir.display()
        );
        return Ok(1);
    }
    let fixtures = repo.join(FIXTURES);
    for (name, text) in &frames {
        let path = fixtures.join(name);
        if writes_frame(path.exists(), record) {
            write(&path, text)?;
        }
    }
    let connector = repo.join(CONNECTOR);
    let source = crate::util::read_regular_text(&connector)
        .map_err(|e| Fail::die(format!("{}: {e}", connector.display())))?;
    let wants = field_wants(&v, &build);
    if field_in(&source)? != wants {
        write(&connector, &record_field(&source, wants.as_deref())?)?;
        println!(
            "{VERB}: recorded CLAUDE_ACCOUNTS.{FIELD} = {wants:?} in {CONNECTOR} (this binary was built with {:?})",
            aterm_agent::ai::connector::CLAUDE_ACCOUNTS.wall_panel_measured_on
        );
    }
    let held = field_in(&crate::util::read_regular_text(&connector).unwrap_or_default())?;
    if held != wants {
        println!(
            "{VERB}: FAIL build={build}: {CONNECTOR} holds {FIELD} = {held:?} where the run found {wants:?}"
        );
        return Ok(1);
    }
    println!(
        "{VERB}: wrote {} frame(s) to {FIXTURES} (claude-{build}-wall-panel-*.txt)",
        frames.len()
    );
    println!("{}", pass_line(&build, &v, held.as_deref()));
    Ok(0)
}

#[cfg(test)]
#[path = "claude_wall_panel_tests.rs"]
mod tests;
