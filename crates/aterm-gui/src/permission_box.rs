// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission chip's box (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): a card on the fact
//! menus' engine ([`crate::autopilot_menu`]), hung from the `yolo` chip, that says in plain words
//! what the permission mode is and what the click that opened it did. It has no row to choose:
//! the click already acted, and a right-click or Control-click opens it without acting.

use aterm_agent::harness::lights::{Light, Mode};
use aterm_agent::supervise::policy::claude_settings::{PermissionDefault, Resolved};

use crate::autopilot_menu::{self, AutopilotMenu, MenuOwner, MenuRow, NoteTone, PointerRest};
use crate::claude_lights::{Chip, GlassAt};
use crate::{App, WindowId};

/// What a switch of the mode costs, said wherever the box names one.
pub(crate) const COST: &str = "Switching restarts the conversation in place. It waits for the \
                               turn, an open box or a draft, and ends background shells.";

/// What stands between a mode that asks and a person being asked: under Cruise or Land the
/// harness's approval policy answers Claude's boxes (`docs/DESIGN-yolo-binary-2026-11-04.md` §6).
pub(crate) const APPROVE: &str = "Under Cruise or Land, aterm's approve = \"all\" still answers \
                                  Claude's boxes for you.";

/// The Claude Code builds an unset default was measured on, and the mode it came up in
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` §10).
const UNSET_MEASURED: (&str, Mode) = ("2.1.318 and 2.1.319", Mode::Auto);

/// What a box opened on a standing restart says a click does, before its signal.
pub(crate) const TAKE_BACK: &str = "A restart is waiting. A click takes it back.";

/// What a box opened on a standing restart says once the restart is signalled.
pub(crate) const SIGNALLED: &str = "The restart has started. A click no longer takes it back.";

/// A permission mode's name, as the box's header says it.
#[must_use]
pub(crate) const fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Bypass => "bypass permissions (yolo)",
        Mode::Manual => "default (manual)",
        Mode::AcceptEdits => "accept edits",
        Mode::Plan => "plan",
        Mode::Auto => "auto",
        Mode::DontAsk => "don't ask",
    }
}

/// What a permission mode does, in one plain sentence: Claude Code's documented modes.
#[must_use]
pub(crate) const fn mode_means(mode: Mode) -> &'static str {
    match mode {
        Mode::Bypass => {
            "Claude runs every tool call - commands, file edits, network - without asking."
        }
        Mode::Manual => {
            "Claude asks before each command or file edit that no allow rule already covers."
        }
        Mode::AcceptEdits => "File edits apply without asking; commands still ask.",
        Mode::Plan => "Claude reads and plans only, and asks before it starts making changes.",
        Mode::Auto => {
            "A classifier checks each action first; what passes runs, the rest is blocked or asked."
        }
        Mode::DontAsk => {
            "Claude runs only what an allow rule covers and refuses the rest without asking."
        }
    }
}

/// What a click on the chip would do, as a box opened without one says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Would {
    /// Turn yolo on.
    TurnOn,
    /// Leave yolo for the settings' default.
    Leave(Resolved),
    /// Nothing: the settings' default is yolo, as the named document says.
    StayYolo(String),
    /// Nothing: the named document turns bypass off.
    StayOff(String),
    /// Take a standing restart back.
    TakeBack,
    /// Nothing: the standing restart is signalled, and nothing takes it back
    /// ([`crate::permission_click::Clicks::take_back`]).
    Signalled,
}

impl Would {
    /// Whether a click that would do this stands a restart.
    #[must_use]
    pub(crate) const fn stands(&self) -> bool {
        matches!(self, Self::TurnOn | Self::Leave(_))
    }
}

/// What the box says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Told {
    /// A click stood the restart onto the bypass line.
    TurningOn,
    /// A click stood the restart onto the line with no permission flag, which lands in the
    /// settings' default.
    Leaving(Resolved),
    /// A click on the on chip changed nothing: the named document makes yolo the default.
    DefaultIsYolo(String),
    /// A click on the off chip changed nothing: the named document turns bypass off.
    BypassOff(String),
    /// No click: the mode the pill reads, and what a click would do where this host restarts.
    Shown(Mode, Option<Would>),
}

/// What [`Told`] a click on a chip reading `pill` is, under the settings' `default`, and whether
/// it stands a restart: the one decision of the click
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` §3).
#[must_use]
pub(crate) fn click_told(pill: Mode, default: PermissionDefault) -> (Told, bool) {
    match would(pill, default) {
        Would::TurnOn => (Told::TurningOn, true),
        Would::Leave(to) => (Told::Leaving(to), true),
        Would::StayYolo(source) => (Told::DefaultIsYolo(source), false),
        Would::StayOff(source) => (Told::BypassOff(source), false),
        // Never [`would`]'s answer: a click on a standing restart is its take-back, or nothing
        // once it is signalled, decided before the settings are read.
        held @ (Would::TakeBack | Would::Signalled) => (Told::Shown(pill, Some(held)), false),
    }
}

/// What a click on a chip reading `pill` would do under the settings' `default`, no click
/// standing.
#[must_use]
pub(crate) fn would(pill: Mode, default: PermissionDefault) -> Would {
    if pill != Mode::Bypass {
        return match default.bypass_disabled {
            Some(source) => Would::StayOff(source),
            None => Would::TurnOn,
        };
    }
    match default.mode {
        Resolved::Mode {
            mode: Mode::Bypass,
            source,
        } => Would::StayYolo(source),
        to => Would::Leave(to),
    }
}

/// `path` with the person's home written `~`, as a box names a settings file.
fn place(path: &str) -> String {
    aterm_primer::home_dir()
        .and_then(|home| {
            let home = home.to_string_lossy().into_owned();
            path.strip_prefix(&home)
                .filter(|rest| rest.starts_with('/'))
                .map(|rest| format!("~{rest}"))
        })
        .unwrap_or_else(|| path.to_owned())
}

/// The header and the mode whose meaning the box gives, for the settings' default `to`.
fn landing(to: &Resolved) -> (String, Option<Mode>) {
    match to {
        Resolved::Mode { mode, .. } => (mode_name(*mode).to_owned(), Some(*mode)),
        Resolved::Unset => ("Claude Code's own default".to_owned(), None),
        Resolved::Unread(_) => ("the settings' default".to_owned(), None),
    }
}

/// What the box says of the settings' default `to` beyond its mode's meaning.
fn landing_words(to: &Resolved) -> Vec<String> {
    match to {
        Resolved::Mode { source, .. } => vec![format!("Set by {}.", place(source))],
        Resolved::Unset => vec![
            "No settings file names a default mode, so Claude Code picks its own.".to_owned(),
            format!(
                "On Claude Code {} that was {}: {}",
                UNSET_MEASURED.0,
                mode_name(UNSET_MEASURED.1),
                mode_means(UNSET_MEASURED.1)
            ),
        ],
        Resolved::Unread(why) => {
            vec![format!("aterm could not tell which mode that is: {why}.")]
        }
    }
}

/// What a box opened without a click says a click would do, and whether that click restarts.
fn would_words(would: &Would) -> (String, bool) {
    match would {
        Would::TurnOn => ("A click turns yolo on.".to_owned(), true),
        Would::Leave(to) => (
            match to {
                Resolved::Mode { mode, source } => format!(
                    "A click leaves yolo for {}, the default {} sets.",
                    mode_name(*mode),
                    place(source)
                ),
                Resolved::Unset => {
                    "A click leaves yolo for Claude Code's own default mode.".to_owned()
                }
                Resolved::Unread(_) => {
                    "A click leaves yolo for the default mode of Claude Code's settings.".to_owned()
                }
            },
            true,
        ),
        Would::StayYolo(source) => (
            format!(
                "A click changes nothing: {} makes yolo the default.",
                place(source)
            ),
            false,
        ),
        Would::StayOff(source) => (
            format!("A click changes nothing: {} turns yolo off.", place(source)),
            false,
        ),
        Would::TakeBack => (TAKE_BACK.to_owned(), false),
        Would::Signalled => (SIGNALLED.to_owned(), false),
    }
}

/// `words` wrapped whole into notes of `tone`, never cut: the first line carries what a screen
/// reader is told for all of them.
fn wrapped(out: &mut Vec<MenuRow>, words: &str, room: usize, tone: NoteTone) {
    let width = if room == 0 {
        autopilot_menu::MAX_CONTENT
    } else {
        room.min(autopilot_menu::MAX_CONTENT)
    };
    let lines = crate::native_settings::wrap_to_box(words, width as f32, &|text| {
        crate::tab_menu::display_cells(text) as f32
    });
    let mut spoken = Some(words);
    for line in lines {
        out.push(MenuRow::Note {
            text: line,
            spoken: spoken.take().unwrap_or_default().to_owned(),
            tone,
        });
    }
}

/// The box's rows for `told` in `room` cells of content, and what a screen reader names the box.
#[must_use]
pub(crate) fn rows(told: &Told, room: usize) -> (Vec<MenuRow>, String) {
    let yolo = mode_name(Mode::Bypass).to_owned();
    // (header, what happened, the mode explained, more words, the switch's cost, the approval line)
    let (header, did, mode, more, cost, approve): (String, Option<String>, _, Vec<String>, _, _) =
        match told {
            Told::TurningOn => (
                yolo,
                Some("Turning yolo on.".to_owned()),
                Some(Mode::Bypass),
                Vec::new(),
                true,
                false,
            ),
            Told::Leaving(to) => {
                let (header, mode) = landing(to);
                (
                    header,
                    Some("Leaving yolo for this mode.".to_owned()),
                    mode,
                    landing_words(to),
                    true,
                    mode != Some(Mode::Bypass),
                )
            }
            Told::DefaultIsYolo(source) => (
                yolo,
                Some("Nothing changed: yolo is the default here.".to_owned()),
                Some(Mode::Bypass),
                vec![format!(
                    "{} sets permissions.defaultMode to bypassPermissions, so a restart would \
                     land back in yolo.",
                    place(source)
                )],
                false,
                false,
            ),
            Told::BypassOff(source) => (
                yolo,
                Some("Nothing changed: yolo is turned off here.".to_owned()),
                Some(Mode::Bypass),
                vec![format!(
                    "{} sets permissions.disableBypassPermissionsMode, so Claude Code would not \
                     start in it.",
                    place(source)
                )],
                false,
                false,
            ),
            Told::Shown(mode, would) => {
                let said = would.as_ref().map(would_words);
                (
                    mode_name(*mode).to_owned(),
                    None,
                    Some(*mode),
                    said.iter().map(|(words, _)| words.clone()).collect(),
                    said.is_some_and(|(_, restarts)| restarts),
                    *mode != Mode::Bypass,
                )
            }
        };
    let spoken = format!("Permission mode, {header}");
    let mut out = vec![MenuRow::Header(header)];
    if let Some(did) = did {
        wrapped(&mut out, &did, room, NoteTone::Value);
    }
    if let Some(mode) = mode {
        wrapped(&mut out, mode_means(mode), room, NoteTone::Label);
    }
    if approve {
        wrapped(&mut out, APPROVE, room, NoteTone::Label);
    }
    if !more.is_empty() || cost {
        out.push(MenuRow::Rule);
    }
    for words in &more {
        wrapped(&mut out, words, room, NoteTone::Label);
    }
    if cost {
        wrapped(&mut out, COST, room, NoteTone::Label);
    }
    (out, spoken)
}

impl App {
    fn visible_light_hits(
        &self,
        wid: WindowId,
        session: u64,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.windows
            .get(&wid)
            .into_iter()
            .flat_map(move |ws| ws.claude_lights.light_cells(session))
            .filter(move |&(_, term_row, col)| !self.chrome_owns_terminal_cell(wid, term_row, col))
            .map(|(frame_row, _, col)| (frame_row, col))
    }

    /// Whether the box open in `wid` for `session` keeps its place: its chip drawn on `row`, or,
    /// with the chip gone while the agent relaunches, the click that opened it still standing.
    pub(crate) fn permission_box_anchored(&self, wid: WindowId, session: u64, row: usize) -> bool {
        self.visible_light_hits(wid, session)
            .any(|(frame_row, _)| frame_row == row)
            || self
                .session_sid(session)
                .is_some_and(|tab| self.permission_clicks.clicks.get(&tab).is_some())
    }

    /// Open the box saying `told` at `session`'s permission chip in `wid`. Where the chip is not
    /// drawn there (a click a screen reader made on a covered row) nothing opens.
    pub(crate) fn open_permission_box(&mut self, wid: WindowId, session: u64, told: &Told) -> bool {
        let Some(anchor) = self.visible_light_hits(wid, session).min() else {
            return false;
        };
        self.clear_strip_press(wid);
        self.settle_rename_edit(wid);
        self.close_autopilot_menu();
        self.autopilot_menu_spent = None;
        let room = self.permission_box_room(wid, session);
        let (rows, spoken) = rows(told, room);
        let focus = self.focused_session_id(wid);
        let px = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px);
        self.autopilot_menu = Some(AutopilotMenu {
            owner: MenuOwner::Permission,
            wid,
            session,
            focus,
            anchor,
            rows,
            spoken,
            highlight: None,
            viewport: crate::menu_viewport::MenuViewport::default(),
            page_rows: 0,
            wheel_residual: 0.0,
            wheel_residual_x: 0.0,
            rect: None,
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(px),
            told: Some((told.clone(), room)),
            plate: Default::default(),
            wash: Default::default(),
        });
        self.request_menu_frame(wid);
        true
    }

    /// Open the box for `session`'s permission chip in `wid` without acting (a right-click, a
    /// Control-click): the mode its pill reads now and what a click would do. Nothing opens where
    /// no pill is read.
    pub(crate) fn explain_permission(&mut self, wid: WindowId, session: u64) -> bool {
        let Some(mode) = self.permission_pill(session) else {
            return false;
        };
        let would = crate::permission_click::RESTARTS
            .then(|| self.session_sid(session))
            .flatten()
            .map(|tab| match self.permission_clicks.clicks.get(&tab) {
                Some(standing) if standing.signalled => Would::Signalled,
                Some(_) => Would::TakeBack,
                None => would(mode, self.permission_default(&tab)),
            });
        self.open_permission_box(wid, session, &Told::Shown(mode, would))
    }

    /// The cells of content a box of `session`'s pane in `wid` wraps its words to.
    fn permission_box_room(&self, wid: WindowId, session: u64) -> usize {
        let (_, pane_cols) = self.autopilot_menu_pane(wid, session);
        autopilot_menu::content_room(pane_cols.len())
    }

    /// Bring `session`'s open permission box to what now holds: a box on a restart that no
    /// longer stands (landed, refused, failed, taken back) closes, since what it says is over;
    /// a box that offered the take-back says the signal once the restart is signalled; and its
    /// words are wrapped again to the pane's room, so a narrower pane cuts none.
    pub(crate) fn refresh_permission_box(&mut self, session: u64) {
        let Some((wid, told, room)) = self
            .autopilot_menu
            .as_ref()
            .filter(|menu| menu.session == session && menu.owner == MenuOwner::Permission)
            .and_then(|menu| {
                let (told, room) = menu.told.clone()?;
                Some((menu.wid, told, room))
            })
        else {
            return;
        };
        let tab = self.session_sid(session);
        let standing = tab
            .as_ref()
            .and_then(|tab| self.permission_clicks.clicks.get(tab));
        let pill = self.permission_pill(session);
        let now = match (&told, &standing) {
            (
                Told::TurningOn
                | Told::Leaving(_)
                | Told::Shown(_, Some(Would::TakeBack | Would::Signalled)),
                None,
            ) => {
                self.close_autopilot_menu();
                return;
            }
            (Told::Shown(mode, Some(Would::TakeBack)), Some(standing)) if standing.signalled => {
                Told::Shown(*mode, Some(Would::Signalled))
            }
            // The pill moved under a box that said what a click on the old one would do (the
            // agent's own step to plan, say): it says the mode the pill reads now, and what a
            // click would do from there, so its words never promise the other way's restart.
            (Told::Shown(mode, would), None) if pill.is_some_and(|pill| pill != *mode) => {
                let pill = pill.unwrap_or(*mode);
                let would = would
                    .as_ref()
                    .and(tab.as_ref())
                    .map(|tab| self::would(pill, self.permission_default(tab)));
                Told::Shown(pill, would)
            }
            _ => told.clone(),
        };
        let fits = self.permission_box_room(wid, session);
        if now == told && fits == room {
            return;
        }
        let (rows, spoken) = rows(&now, fits);
        if let Some(menu) = self.autopilot_menu.as_mut() {
            menu.replace_rows(rows, spoken);
            menu.told = Some((now, fits));
        }
        self.request_menu_frame(wid);
    }

    /// [`Self::refresh_permission_box`] for the box open in `wid`, before a frame lays it: a
    /// pane resized since its words were wrapped has them wrapped again, and a box that says a
    /// mode the pill no longer reads says the one it does.
    pub(crate) fn fit_permission_box(&mut self, wid: WindowId) {
        let Some((session, room, shown)) = self
            .autopilot_menu
            .as_ref()
            .filter(|menu| menu.wid == wid && menu.owner == MenuOwner::Permission)
            .and_then(|menu| {
                let (told, room) = menu.told.as_ref()?;
                let shown = match told {
                    Told::Shown(mode, _) => Some(*mode),
                    _ => None,
                };
                Some((menu.session, *room, shown))
            })
        else {
            return;
        };
        let moved = shown.is_some_and(|mode| {
            self.permission_pill(session)
                .is_some_and(|pill| pill != mode)
        });
        if moved || self.permission_box_room(wid, session) != room {
            self.refresh_permission_box(session);
        }
    }

    /// [`Self::explain_permission`] for the permission chip at window pixel `(x, y)` of `wid`:
    /// whether a box opened.
    pub(crate) fn open_permission_box_at(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((session, GlassAt::Chip(Some(Chip::Light(Light::Mode))))) =
            self.claude_glass_at(wid, x, y)
        else {
            return false;
        };
        self.explain_permission(wid, session)
    }

    /// A left press at `at` on window pixel `(x, y)` while `wid`'s permission box is open, where
    /// it lands on the box's own chip: a later press of the click's own double click is that
    /// gesture's and leaves the box open; any other closes the box and, where a click acts (it
    /// takes a standing restart back, or stands one), is the chip's click too. Where a click
    /// would only open this box again, the press just closes it, so the chip dismisses its own
    /// box. A box that said what a click on a mode the pill no longer reads would do acts on
    /// nothing: its press only closes it, since the click would now do something else.
    /// Whether the press was the chip's.
    pub(crate) fn permission_box_chip_press(
        &mut self,
        wid: WindowId,
        (x, y): (f64, f64),
        at: std::time::Instant,
    ) -> bool {
        let Some((session, acts)) = self
            .autopilot_menu
            .as_ref()
            .filter(|menu| menu.wid == wid && menu.owner == MenuOwner::Permission)
            .map(|menu| {
                let acts = match menu.told.as_ref().map(|(told, _)| told) {
                    Some(Told::TurningOn | Told::Leaving(_)) => true,
                    // A take-back is decided before the pill is read; a restart the box offered
                    // stands only from the mode the box said.
                    Some(Told::Shown(_, Some(Would::TakeBack))) => true,
                    Some(Told::Shown(mode, Some(would))) => {
                        would.stands() && self.permission_pill(menu.session) == Some(*mode)
                    }
                    Some(Told::DefaultIsYolo(_) | Told::BypassOff(_) | Told::Shown(_, None))
                    | None => false,
                };
                (menu.session, acts)
            })
        else {
            return false;
        };
        if !matches!(
            self.claude_glass_at(wid, x, y),
            Some((over, GlassAt::Chip(Some(Chip::Light(Light::Mode))))) if over == session
        ) {
            return false;
        }
        if self
            .windows
            .get_mut(&wid)
            .is_some_and(|ws| ws.claude_lights.light_just_pressed(session, at))
        {
            return true;
        }
        self.close_autopilot_menu();
        if acts {
            self.press_claude_light(wid, at);
        }
        true
    }
}

#[cfg(test)]
#[path = "permission_box_tests.rs"]
mod tests;
