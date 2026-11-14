// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Take Over and Hand Back (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): the person's own
//! hold on one tab, and the window side of the menu rows around it.

use crate::platform::AppRt as _;
use crate::{App, WindowId};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the person's hold stands before the digest says so once ([`App::hand_long_holds`]):
/// long enough that the hold is no longer the person at the keyboard, short enough that a
/// forgotten one is found while its harness and manager still matter.
pub(crate) const LONG_HOLD: Duration = Duration::from_secs(30 * 60);

const LONG_HOLD_RESTATE: Duration = Duration::from_secs(60);

/// Why a control client's copy or paste did nothing ([`App::refuse_clipboard_gesture`]).
pub(crate) const CLIPBOARD_REFUSED: &str = "the clipboard is the person's own: a control client \
     cannot copy to it or paste from it (send text with the `paste` verb)";

/// Log that a control client's `what` (a copy or a paste) was refused.
pub(crate) fn note_clipboard_refused(what: &str) {
    aterm_log::info!("{what} refused: {CLIPBOARD_REFUSED}");
}

#[cfg(test)]
thread_local! {
    /// Test clock of [`hold_unix_now`]: `Some(unix seconds)` stands in for the wall clock on
    /// this thread.
    pub(crate) static HOLD_UNIX_CLOCK_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

fn hold_unix_now() -> u64 {
    #[cfg(test)]
    if let Some(now) = HOLD_UNIX_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
        return now;
    }
    aterm_time::unix_secs().unwrap_or(0)
}

#[cfg(any(target_os = "macos", test))]
const AGENTS_LIVE_TTL: Duration = Duration::from_secs(2);

/// The marks of a gesture whose dispatch posts the rest of its work to the event loop (a palette
/// row, a menu item): the post carries them ([`App::gesture_marks`]) and the posted half runs
/// under them ([`App::with_gesture_marks`]), so no mark ends at the hop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GestureMarks {
    /// A control client's gesture ([`AgentsState::controller_gesture`]).
    pub(crate) controller: bool,
    /// An accessibility client's request ([`AgentsState::assistive_gesture`]).
    pub(crate) assistive: bool,
    /// A key, click or scroll another process posted ([`AgentsState::relayed_gesture`]).
    pub(crate) relayed: bool,
}

impl GestureMarks {
    /// The marks of a native key or click from `origin` that chose a menu or toolbar item: a
    /// key an `hwkey` burst posted is a control client's, one another process posted relayed.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn of(origin: aterm_window::event::InputOrigin) -> Self {
        use aterm_window::event::InputOrigin;
        Self {
            controller: origin == InputOrigin::Posted,
            assistive: false,
            relayed: origin == InputOrigin::Relayed,
        }
    }

    /// Every mark either `self` or `other` has.
    pub(crate) const fn union(self, other: Self) -> Self {
        Self {
            controller: self.controller || other.controller,
            assistive: self.assistive || other.assistive,
            relayed: self.relayed || other.relayed,
        }
    }
}

/// The window side of the Agents rows, one field on the App.
#[derive(Debug, Default)]
pub(crate) struct AgentsState {
    /// When this process first saw each session's person hold (or an Owner client's with no
    /// person's beside it), by local id, in unix seconds ([`hold_unix_now`]). Take Over records
    /// the person's; a client's, and one a seamless update carried, are first seen at the next
    /// look ([`App::hand_long_holds`]).
    pub(crate) held_since: std::collections::BTreeMap<u64, u64>,
    /// A hold moved since the last look walked the holds ([`App::hand_long_holds`]): set where
    /// the person takes or hands one back, and where a session's published hold moved
    /// (`App::publish_need`).
    pub(crate) long_holds_moved: bool,
    /// When the next standing hold reaches [`LONG_HOLD`], in unix seconds, as of the last walk;
    /// `None` when none will.
    pub(crate) long_holds_due: Option<u64>,
    /// The session pool's insert epoch at the last walk: a session added since (a seamless
    /// successor's adopted tab) can carry a hold.
    pub(crate) long_holds_epoch: u64,
    /// Test record: how many looks walked the holds.
    #[cfg(test)]
    pub(crate) long_hold_walks: u32,
    /// The gesture being dispatched is a control client's: any input verb (`key`, `text`,
    /// `paste`, `mouse`) through [`App::input_to_session`], a `pointer`, `act` or `invoke`.
    pub(crate) controller_gesture: bool,
    /// The gesture being dispatched came through the window's accessibility tree
    /// ([`App::with_assistive_source`]). No person is known to stand behind it for the owner's
    /// keys ([`crate::owner_keys_save`]), a person-only press
    /// ([`App::person_only_press_refusal`]) or macOS's notification ask
    /// (`App::ask_macos_on_program_notices_turned_on`); everywhere else a screen reader's press is
    /// the person's.
    pub(crate) assistive_gesture: bool,
    /// The gesture being dispatched came from a key, click or scroll another process posted into
    /// the window ([`App::with_origin_mark`], `aterm_window::event::InputOrigin::Relayed`): a
    /// person's own relay (Screen Sharing, an on-screen keyboard) and a program holding aterm's
    /// Accessibility grant, as every tab's program does through aterm, send the same events. No
    /// person is known to stand behind it for the owner's keys ([`crate::owner_keys_save`]), a
    /// person-only press ([`App::person_only_press_refusal`]) or macOS's notification ask
    /// (`App::ask_macos_on_program_notices_turned_on`); everywhere else it is the person's.
    pub(crate) relayed_gesture: bool,
    /// How many control-client gestures were dispatched in this process
    /// ([`App::with_gesture_source`] with `controller` set), anywhere. System
    /// State reads it when it takes a page action, so no control client's
    /// gesture passes for a person's press there
    /// (`docs/DESIGN-system-state-2026-10-21.md` §6).
    pub(crate) controller_gestures: u64,
    /// The words the last [`App::hand_long_holds`] answer stated, one per long hold, so an
    /// unchanged answer is not stated again.
    pub(crate) long_holds_named: Option<Vec<LongHoldWords>>,
    /// Where Next That Needs You last landed: the need's `(session, key)`.
    pub(crate) next_need: Option<(u64, u64)>,
    /// When an open Overview was last snapshotted (`App::overview_prepaint` reads its coarse
    /// ages again a minute after).
    pub(crate) overview_read_at: Option<Instant>,
    /// The Overview's "On the fabric" group's reads of the fabric's log.
    pub(crate) overview_fabric: crate::overview_fabric::FabricGlance,
    /// When the Agents menu's live bits were last published (`App::publish_agents_live`).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) agents_live_at: Option<Instant>,
    /// Test record: how many signals Stop Worker sent.
    #[cfg(test)]
    pub(crate) stop_signals: u32,
    /// Test record: why the last Stop Worker signalled nothing, `None` when it did signal.
    #[cfg(test)]
    pub(crate) last_stop_refused: Option<crate::app_overview::StopRefused>,
    /// Test arm of the tab menu's Stop Worker sheet (`AppRt::confirm_owner`): `Some(answer)`
    /// stands in for the sheet a headless App cannot show (`Some(true)` Stop, `Some(false)`
    /// Cancel, `None` nobody could be asked); unset, the platform's own answer.
    #[cfg(test)]
    pub(crate) owner_sheet_for_test: Option<Option<bool>>,
}

/// One local need, in the order Next That Needs You walks them: stuck before decide; within a severity a need that has stopped
/// its agent before a harness's report it carries on past
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7); then oldest first (since this aterm
/// first saw it published, `NeedAges::need_since`; unknown age last), then by the tab's label,
/// then the session's local id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NeedStop {
    pub(crate) severity: aterm_types::needs::Severity,
    /// It is a harness's `NEEDS YOU:` report ([`crate::needs::Need::harness`]), which its agent
    /// carries on past: it comes after the needs of its severity that stopped their agents.
    pub(crate) carries_on: bool,
    /// Since when it has stood, unix seconds; `i64::MAX` for unknown.
    pub(crate) since: i64,
    /// The tab's label ([`App::tab_label_of_session`]).
    pub(crate) label: String,
    pub(crate) session: u64,
    /// The need's identity within its word ([`crate::needs::Need::key`]).
    pub(crate) key: u64,
}

/// Next That Needs You's step: the stop after `cursor` in `stops`' order, wrapping; the first
/// when the cursor is gone (that need resolved) or was never set.
#[must_use]
pub(crate) fn pick_next_need(stops: &[NeedStop], cursor: Option<(u64, u64)>) -> Option<NeedStop> {
    let mut sorted = stops.to_vec();
    sorted.sort();
    let at = cursor.and_then(|(session, key)| {
        sorted
            .iter()
            .position(|stop| stop.session == session && stop.key == key)
    });
    match at {
        Some(i) => sorted.get((i + 1) % sorted.len()).cloned(),
        None => sorted.first().cloned(),
    }
}

/// `t` as one line of a label (40 bytes at most), `None` where nothing is left of it.
fn label_line(t: &str) -> Option<String> {
    Some(crate::session_timeline::sanitize_presentation_line(t, 40)).filter(|t| !t.is_empty())
}

/// Whether `title` is the one a shell gave its tab for the line `line` it runs (each
/// `aterm_shell_integration` script, as the line starts): the line without its control
/// characters, or, where it is longer than 256, a head of it and `…` (zsh, bash and fish cut it at
/// 255 characters, pwsh at 255 UTF-16 units, and bash and zsh in a C locale at 255 bytes). A line
/// longer than 256 of any of those is longer than 256 bytes, so a shorter one is never cut; a cut
/// by bytes can split the last character it keeps, which the terminal stores as U+FFFD. The
/// terminal drops control and bidi characters from a title it stores, so both are read without
/// them.
fn is_title_of_line(title: &str, line: &str) -> bool {
    let shown = |c: &char| {
        !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    };
    let title = title.trim();
    let (head, cut) = match title.strip_suffix('\u{2026}') {
        Some(head) if line.len() > 256 => (head.trim_end_matches('\u{FFFD}'), true),
        _ => (title, false),
    };
    let mut line = line.trim().chars().filter(shown);
    !head.is_empty()
        && head.chars().filter(shown).all(|c| line.next() == Some(c))
        && (cut || line.next().is_none())
}

/// Whether `t`'s title is its shell's: at a prompt, or once the line it ran has ended, whatever
/// titled it last (each `aterm_shell_integration` script titles a prompt by its folder), and while
/// a line runs, the title its shell gave that line ([`is_title_of_line`]). Without shell
/// integration's marks no title is known to be its shell's.
pub(crate) fn shell_titled(t: &aterm_core::terminal::Terminal) -> bool {
    use aterm_core::terminal::BlockState;
    t.current_block().is_some_and(|block| match block.state {
        BlockState::Executing => block
            .commandline
            .as_deref()
            .is_some_and(|line| is_title_of_line(t.title(), line)),
        BlockState::PromptOnly | BlockState::EnteringCommand | BlockState::Complete => true,
        _ => false,
    })
}

/// Whether nothing `t` runs has titled its tab: its shell gave the title ([`shell_titled`]), or the
/// title says nothing a label reads (empty, or only the glyph and blank an agent leads it with,
/// [`title_without_agent_glyph`], or what [`crate::tab_label::informative`] sheds), so its label
/// would fall to its folder.
pub(crate) fn untitled_by_its_program(t: &aterm_core::terminal::Terminal) -> bool {
    shell_titled(t)
        || crate::tab_label::informative(title_without_agent_glyph(t.title())).is_empty()
}

/// What `ctx`'s session runs, by name: the agent in front by its product's name (`Claude Code`),
/// else what its worker's task started ([`worker_label`]); `None` for none.
fn worker_name(ctx: &crate::SessionCtx) -> Option<String> {
    let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    crate::harness_host::identified_agent(timeline.agent())
        .map(|agent| agent.product().to_owned())
        .or_else(|| timeline.task().and_then(worker_label))
}

/// What `record`'s worker runs ([`crate::task_record::TaskRecord::worker_name`]: its agent, or a
/// program worker's program) as one line of a label; `None` where it names neither.
pub(crate) fn worker_label(record: &crate::task_record::TaskRecord) -> Option<String> {
    record.worker_name().and_then(label_line)
}

const CLAUDE_IDLE_MARK: char = '\u{2733}';

/// A program's OSC title as a label names it: trimmed, and without the status glyph and blank
/// an agent leads it with (a spinner frame, [`crate::chrome_label::is_spinner_frame`], or
/// Claude Code's idle [`CLAUDE_IDLE_MARK`]), so a turn's start and end name the tab alike.
pub(crate) fn title_without_agent_glyph(title: &str) -> &str {
    let title = title.trim_start();
    let mut chars = title.chars();
    let glyph = chars
        .next()
        .is_some_and(|c| c == CLAUDE_IDLE_MARK || crate::chrome_label::is_spinner_frame(c));
    let rest = chars.as_str();
    if glyph && rest.starts_with(char::is_whitespace) {
        rest.trim()
    } else {
        title.trim()
    }
}

/// One standing person hold, as Hand Back All and the long hold's entry read it
/// ([`App::person_holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersonHeld {
    /// The session, by local id.
    pub(crate) session: u64,
    /// Since when this process saw it stand, in unix seconds (the wall clock, so its age counts
    /// through a sleep).
    pub(crate) since: u64,
    /// How many acts it refused since it was set.
    pub(crate) refused: u64,
}

/// What one long hold's digest entry says ([`App::hand_long_holds`]), compared whole so a
/// count, a manager or another hold that moved restates the entry, and nothing else does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LongHoldWords {
    pub(crate) session: u64,
    sid: String,
    label: String,
    manager: Option<String>,
    pub(crate) refused: u64,
    /// Another hold beside the person's, which outlives Hand Back: whose,
    /// and its reason in words.
    other: Option<String>,
    /// An Owner client's hold with no person's beside it: its reason in words.
    client: Option<String>,
}

impl LongHoldWords {
    fn entry(&self) -> aterm_messages::exceptions::Entry {
        if let Some(reason) = &self.client {
            return crate::message_reporters::client_long_hold_entry(
                &self.sid,
                &self.label,
                LONG_HOLD.as_secs() / 60,
                reason,
                self.manager.as_deref(),
            );
        }
        let mut entry = crate::message_reporters::long_hold_entry(
            &self.sid,
            &self.label,
            LONG_HOLD.as_secs() / 60,
            self.manager.as_deref(),
        );
        if self.refused > 0 {
            entry = entry.more(format!("refused {} acts since you took it", self.refused));
        }
        if let Some(other) = &self.other {
            entry = entry.more(other.clone());
        }
        entry
    }
}

/// Which `aterm` command a row reads into a document tab ([`App::run_cli_to_tab`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliVerb {
    /// Agents ▸ Read This Mac: `aterm ctl survey`.
    ReadThisMac,
    /// Agents ▸ Read the Fleet: `aterm ctl survey --fleet --timeout 3`.
    ReadFleet,
    /// Fleet Machines (palette): `aterm fleet peers`.
    FleetPeers,
    /// Message Bus: Status (palette): `aterm fabric`.
    BusStatus,
    /// Message Bus: Turn On… (palette): `aterm fabric on`, behind a confirmation.
    BusOn,
    /// Message Bus: Turn Off… (palette): `aterm fabric off`, behind a confirmation.
    BusOff,
    /// The menu bar's All activity…: `aterm fleet activity`, this Mac's record of the agent
    /// requests between it and the other Macs.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    FleetActivity,
}

impl CliVerb {
    /// The verb's word in a message key and a document name: one live row per verb, so a
    /// rerun replaces its own outcome.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::ReadThisMac => "read-mac",
            Self::ReadFleet => "read-fleet",
            Self::FleetPeers => "fleet-peers",
            Self::BusStatus => "bus-status",
            Self::BusOn => "bus-on",
            Self::BusOff => "bus-off",
            Self::FleetActivity => "fleet-activity",
        }
    }

    /// The CLI's argv after the program.
    pub(crate) fn argv(self) -> Vec<String> {
        let words: &[&str] = match self {
            Self::ReadThisMac => &["ctl", "survey"],
            Self::ReadFleet => &["ctl", "survey", "--fleet", "--timeout", "3"],
            Self::FleetPeers => &["fleet", "peers"],
            Self::BusStatus => &["fabric"],
            Self::BusOn => &["fabric", "on"],
            Self::BusOff => &["fabric", "off"],
            Self::FleetActivity => &["fleet", "activity"],
        };
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// The row as a record's title names it.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::ReadThisMac => "Read This Mac",
            Self::ReadFleet => "Read the Fleet",
            Self::FleetPeers => "Fleet Machines",
            Self::BusStatus => "Message bus status",
            Self::BusOn => "Message Bus On",
            Self::BusOff => "Message Bus Off",
            Self::FleetActivity => "Fleet Activity",
        }
    }

    /// The command as a human types it.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::ReadThisMac => "aterm ctl survey",
            Self::ReadFleet => "aterm ctl survey --fleet --timeout 3",
            Self::FleetPeers => "aterm fleet peers",
            Self::BusStatus => "aterm fabric",
            Self::BusOn => "aterm fabric on",
            Self::BusOff => "aterm fabric off",
            Self::FleetActivity => "aterm fleet activity",
        }
    }

    const fn cap(self) -> Option<Duration> {
        match self {
            Self::ReadThisMac | Self::ReadFleet | Self::FleetPeers | Self::FleetActivity => {
                Some(crate::bounded_child::READ_CAP)
            }
            Self::BusStatus | Self::BusOn | Self::BusOff => None,
        }
    }

    const fn confirmation(self) -> Option<(&'static str, &'static str, &'static str)> {
        match self {
            Self::ReadThisMac
            | Self::ReadFleet
            | Self::FleetPeers
            | Self::BusStatus
            | Self::FleetActivity => None,
            Self::BusOn => Some((
                "Turn the message bus on?",
                "Sessions on this machine can post to each other and to channels.",
                "Turn On",
            )),
            Self::BusOff => Some((
                "Turn the message bus off?",
                "Sessions stop getting messages until you turn it on again.",
                "Turn Off",
            )),
        }
    }
}

/// What a row asked `aterm` to do: the verb, its argv, and the file its output goes to (opened
/// as a tab when the child ends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CliPlan {
    pub(crate) verb: CliVerb,
    pub(crate) argv: Vec<String>,
    pub(crate) out: PathBuf,
    /// The record's and the tab's title: the verb's.
    pub(crate) title: String,
    /// Raise the window its tab opens in ([`App::raise_window`]): the menu bar's reads, whose
    /// click never makes aterm the active app.
    pub(crate) raise: bool,
}

impl CliPlan {
    pub(crate) fn new(verb: CliVerb) -> Self {
        Self {
            verb,
            argv: verb.argv(),
            out: cli_document_path(verb.key()),
            title: verb.title().to_string(),
            raise: false,
        }
    }

    /// The same plan, its tab's window raised when it opens.
    #[must_use]
    pub(crate) fn raised(self) -> Self {
        Self {
            raise: true,
            ..self
        }
    }

    fn words(&self) -> String {
        format!("aterm {}", self.argv.join(" "))
    }
}

fn cli_document_path(kind: &str) -> PathBuf {
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    menu_document_path(kind, &format!("r{n}"))
}

/// The child's end, as `Wake::CliTab` carries it back to the main loop.
#[derive(Debug)]
pub(crate) struct CliTabOutcome {
    pub(crate) wid: WindowId,
    pub(crate) plan: CliPlan,
    /// The exit code (`None` when killed by a signal), or why it could not run.
    pub(crate) status: Result<Option<i32>, String>,
}

/// Where a row's document goes: the temp dir, one file per gesture
/// (`aterm-<kind>[-<sid>]-<unix seconds>.md`), so a tab left open on an earlier one is never
/// rewritten under it.
pub(crate) fn menu_document_path(kind: &str, sid: &str) -> PathBuf {
    let unix_s = aterm_time::unix_secs().unwrap_or(0);
    let safe: String = sid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let name = if safe.is_empty() {
        format!("aterm-{kind}-{unix_s}.md")
    } else {
        format!("aterm-{kind}-{safe}-{unix_s}.md")
    };
    std::env::temp_dir().join(name)
}

/// Write `text` to `path`, created `0600` on unix: a survey of the Mac, or a bus status, is the
/// owner's to read.
pub(crate) fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = opts.open(path)?;
    file.write_all(text.as_bytes())
}

fn cli_markdown(
    plan: &CliPlan,
    status: &Result<Option<i32>, String>,
    stdout: &str,
    stderr: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", plan.words()));
    s.push_str(&format!("{}\n\n", cli_status_words(status)));
    for (label, text) in [("", stdout), ("stderr:\n\n", stderr)] {
        if text.trim().is_empty() {
            continue;
        }
        let fence = crate::bounded_child::fence(text);
        s.push_str(&format!(
            "{label}{fence}text\n{}\n{fence}\n\n",
            text.trim_end()
        ));
    }
    s
}

fn cli_failure_reason(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(code)) => format!("exited {code}"),
        Ok(None) => "killed by a signal".to_string(),
        Err(e) => e.clone(),
    }
}

fn cli_status_words(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(0)) => "Done.".to_string(),
        Ok(Some(code)) => format!("Exited {code}."),
        Ok(None) => "Killed by a signal.".to_string(),
        Err(e) => format!("Could not run: {e}"),
    }
}

impl App {
    /// Every local need, read off each session's published verdict (never classified here),
    /// with its age and its tab's label ([`NeedStop`]).
    pub(crate) fn local_needs(&self) -> Vec<NeedStop> {
        let needs: Vec<(u64, aterm_types::needs::Severity, bool, u64)> = self
            .pool
            .iter()
            .filter_map(|s| {
                let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                let need = tl.need().need.as_ref()?;
                Some((s.id, need.word.severity(), need.harness.is_some(), need.key))
            })
            .collect();
        needs
            .into_iter()
            .map(|(session, severity, carries_on, key)| NeedStop {
                severity,
                carries_on,
                since: self
                    .presence
                    .need_ages
                    .need_since(session)
                    .unwrap_or(i64::MAX),
                label: self.tab_label_of_session(session),
                session,
                key,
            })
            .collect()
    }

    /// Agents ▸ Next That Needs You (⇧⌘J): go to the most pressing local need
    /// ([`pick_next_need`]); pressed again, to the next.
    pub(crate) fn next_need(&mut self) -> Result<(), String> {
        let stops = self.local_needs();
        let stop = pick_next_need(&stops, self.agents.next_need)
            .ok_or_else(|| "nothing here needs you".to_string())?;
        self.agents.next_need = Some((stop.session, stop.key));
        self.raise_session_by_id(stop.session)
    }

    /// The Agents menu's live bits ([`crate::menu::AgentsLive`]): a local need, a fleet, a
    /// window, and why Summarize cannot start.
    pub(crate) fn agents_live(&self) -> crate::menu::AgentsLive {
        crate::menu::AgentsLive {
            needs_local: self.pool.iter().any(|s| {
                s.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .need()
                    .need
                    .is_some()
            }),
            in_fleet: crate::summarize::in_fleet(),
            any_window: self.frontmost_window.is_some(),
            summarize_block: self.summarize_start_block().map(|block| block.hint()),
        }
    }

    /// Publish [`Self::agents_live`] for `validateMenuItem:` on the way to a wait, only where it
    /// can have moved: after a turn that rebuilt the status glance (`rebuilt`), or once its
    /// time-bound looks (the fleet secret, `claude` on the PATH, the worker's shell) are older
    /// than [`AGENTS_LIVE_TTL`].
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn publish_agents_live(&mut self, rebuilt: bool, now: Instant) {
        if !rebuilt
            && self
                .agents
                .agents_live_at
                .is_some_and(|at| now.saturating_duration_since(at) < AGENTS_LIVE_TTL)
        {
            return;
        }
        self.agents.agents_live_at = Some(now);
        crate::menu::set_agents_live(self.agents_live());
    }

    /// Whether Hand Back has a hold to lift on `wid`'s front session: the person's, or an Owner
    /// client's where no person's stands (`fabric::Holds::hand_back_origin`).
    pub(crate) fn front_person_hold(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| s.ctx.fabric.holds().hand_back_origin().is_some())
    }

    /// Whether another Mac holds `wid`'s front session and nothing of this Mac's does (the
    /// step-in's `origin=remote`): Take Over is live beside Hand Back there, and the person wins
    /// either way.
    pub(crate) fn front_remote_hold(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| {
                s.ctx.fabric.holds().hand_back_origin() == Some(crate::fabric::REMOTE_ORIGIN)
            })
    }

    /// Whether the person holds any tab, the front one or not: what Hand Back All lifts
    /// ([`Self::person_holds`]).
    pub(crate) fn any_person_hold(&self) -> bool {
        self.pool
            .iter()
            .any(|s| s.ctx.fabric.holds().person().is_some())
    }

    /// Publish `wid`'s front holds, and whether the person holds any tab, for the native menu's
    /// synchronous validate (Take Over, Hand Back, Hand Back All).
    pub(crate) fn publish_front_hold(&mut self, wid: WindowId) {
        crate::menu::set_front_person_hold(self.front_person_hold(wid));
        crate::menu::set_front_remote_hold(self.front_remote_hold(wid));
        crate::menu::set_any_person_hold(self.any_person_hold());
    }

    /// Agents ▸ Take Over This Tab for `wid`'s focused session.
    pub(crate) fn take_over(&mut self, wid: WindowId) {
        if let Some(session) = self.focused_session_id(wid)
            && let Err(why) = self.take_over_session(session)
        {
            self.pending_action_refusal = Some(why);
        }
    }

    /// Agents ▸ Hand Back This Tab for `wid`'s focused session.
    pub(crate) fn hand_back(&mut self, wid: WindowId) {
        if let Some(session) = self.focused_session_id(wid)
            && let Err(why) = self.hand_back_session(session)
        {
            self.pending_action_refusal = Some(why);
        }
    }

    /// Take Over `session` (by local id): set the person's hold ([`crate::fabric::person_hold`]).
    pub(crate) fn take_over_session(&mut self, session: u64) -> Result<(), String> {
        self.person_hold_session(session, true)
    }

    /// Hand Back `session`: lift the person's slot, else an Owner client's where no person's
    /// stands (`fabric::person_hold`); never the fleet's.
    pub(crate) fn hand_back_session(&mut self, session: u64) -> Result<(), String> {
        self.person_hold_session(session, false)
    }

    /// Whether the gesture being dispatched is a control client's, so a person-only act refuses
    /// it ([`AgentsState::controller_gesture`]).
    pub(crate) const fn person_gesture_refused(&self) -> bool {
        self.agents.controller_gesture
    }

    /// Whether a gateway card is open on `wid`: the palette, the Overview or a tab's in-grid
    /// menu, whose rows hold the person's own acts (Take Over, Hand Back) behind a key.
    pub(crate) fn gateway_card_open(&self, wid: WindowId) -> bool {
        use crate::overlay::OverlayKind;
        self.windows.get(&wid).is_some_and(|ws| {
            ws.tab_menu.is_some()
                || matches!(
                    ws.overlay.as_ref().map(|o| o.kind()),
                    Some(OverlayKind::Palette | OverlayKind::Overview)
                )
        })
    }

    /// Whether System State is the active view of `wid`: its buttons hold the
    /// person's own acts — an Apply that admits a setup plan as the window's
    /// own, a Permissions row's `Open …` and the vault row's `Turn On Sync` —
    /// which only a person's press may take (`docs/DESIGN-system-state-2026-10-21.md` §6, the standing yes).
    pub(crate) fn system_state_active_in(&self, wid: WindowId) -> bool {
        self.active_native_view(wid).is_some_and(|(instance, _)| {
            matches!(
                self.native_runtime.app(instance),
                Some(crate::native_app::NativeApp::SystemState(_))
            )
        })
    }

    /// Read native focus without posting keys, raising a window or publishing input guards.
    pub(crate) fn native_focus(&self) -> Option<crate::platform::NativeFocus> {
        self.native_focus_when_windowed(|| {
            let wid = self.frontmost_window?;
            let window = self.windows.get(&wid)?.os_window.as_deref()?;
            crate::platform::appkit_native_focus(window)
        })
    }

    fn native_focus_when_windowed(
        &self,
        observe: impl FnOnce() -> Option<crate::platform::NativeFocus>,
    ) -> Option<crate::platform::NativeFocus> {
        if self.headless {
            return None;
        }
        observe()
    }

    /// `hwkey`'s main-thread hop (`Wake::HwKeyTarget`): the front window's AppKit number (`0`
    /// with none) and whether it is key, or a refusal where a real key event would pass for the
    /// person's press ([`Self::hwkey_front_refusal`]). It also publishes the answer a running
    /// burst reads before each key ([`Self::publish_hwkey_front`]).
    pub(crate) fn hwkey_target(&self) -> Result<(i64, bool), &'static str> {
        self.publish_hwkey_front();
        let Some(wid) = self.frontmost_window else {
            return Ok((0, false));
        };
        if let Some(refusal) = self.hwkey_front_refusal() {
            return Err(refusal);
        }
        Ok(self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_deref())
            .map_or((0, false), |w| {
                (
                    crate::platform::appkit_window_number(w),
                    crate::platform::appkit_window_is_key_in_active_app(w),
                )
            }))
    }

    /// Why `hwkey` may post nothing into the front window now: a gateway card
    /// open there ([`Self::gateway_card_open`]), System State its active view
    /// ([`Self::system_state_active_in`]), or a private document in any of its
    /// tabs ([`Self::window_holds_private`]). `None` where it may.
    pub(crate) fn hwkey_front_refusal(&self) -> Option<&'static str> {
        let wid = self.frontmost_window?;
        if self.gateway_card_open(wid) {
            return Some(
                "a palette, Overview or tab menu is open: its rows are the person's (close it)",
            );
        }
        if self.system_state_active_in(wid) {
            return Some(
                "System State is in front: its buttons are the person's (press them in the window)",
            );
        }
        self.window_holds_private(wid)
            .then_some(crate::app_control::PRIVATE_WINDOW_REFUSED)
    }

    /// Tell a running `hwkey` burst whether the front window holds the person's
    /// own buttons ([`Self::hwkey_front_refusal`]); it reads that before every
    /// key (`hwkey::run_burst`). One atomic store: the park calls it each turn,
    /// so a page or card that came to the front in a turn stops the burst
    /// before the loop sleeps.
    pub(crate) fn publish_hwkey_front(&self) {
        crate::hwkey::publish_persons_front(self.hwkey_front_refusal().is_some());
    }

    /// Whether a control client's tab-menu chord (the in-grid strip's Menu key, Windows and
    /// Linux) opens no card under a hold: the card is `invoke`'s rows behind a key, so it opens
    /// for a control client only where `open tab-menu` would
    /// ([`crate::fabric::open_halt_refusal`]). The person's own chord always opens it.
    #[cfg(any(windows, target_os = "linux", test))]
    pub(crate) fn controller_card_refused(&self) -> bool {
        self.person_gesture_refused()
            && crate::fabric::app_halt_refusal(&self.store, "invoke").is_some()
    }

    /// Run `f` with the dispatch marked as a control client's gesture when `controller` is set
    /// (and counted in `controller_gestures`), restoring the mark after.
    pub(crate) fn with_gesture_source<R>(
        &mut self,
        controller: bool,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        if controller {
            self.agents.controller_gestures = self.agents.controller_gestures.wrapping_add(1);
        }
        let prior = std::mem::replace(&mut self.agents.controller_gesture, controller);
        let out = f(self);
        self.agents.controller_gesture = prior;
        out
    }

    /// Run `f` with the dispatch marked by where its native input came from (`origin`): a key an
    /// `hwkey` burst posted carries the controller mark ([`Self::with_controller_mark`]), and one
    /// another process posted the relayed mark ([`AgentsState::relayed_gesture`]). A mark an
    /// enclosing dispatch already set is kept, and both are restored after.
    pub(crate) fn with_origin_mark<R>(
        &mut self,
        origin: aterm_window::event::InputOrigin,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        use aterm_window::event::InputOrigin;
        self.with_controller_mark(origin == InputOrigin::Posted, |app| {
            app.with_relayed_mark(origin == InputOrigin::Relayed, f)
        })
    }

    /// Run `f` with the dispatch marked as another process's relayed input when `relayed` is set
    /// ([`AgentsState::relayed_gesture`]), keeping a mark an enclosing dispatch already set, and
    /// restoring the mark after.
    fn with_relayed_mark<R>(&mut self, relayed: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let marked = relayed || self.agents.relayed_gesture;
        let prior = std::mem::replace(&mut self.agents.relayed_gesture, marked);
        let out = f(self);
        self.agents.relayed_gesture = prior;
        out
    }

    /// Where the gesture being dispatched came from, as its window-input marks say: a control
    /// client's ([`Self::person_gesture_refused`]), another process's
    /// ([`AgentsState::relayed_gesture`]), else the person's.
    pub(crate) const fn gesture_origin(&self) -> aterm_window::event::InputOrigin {
        use aterm_window::event::InputOrigin;
        if self.agents.controller_gesture {
            InputOrigin::Posted
        } else if self.agents.relayed_gesture {
            InputOrigin::Relayed
        } else {
            InputOrigin::Person
        }
    }

    /// Run `f` with the dispatch marked as an accessibility client's request
    /// ([`AgentsState::assistive_gesture`]), restoring the mark after.
    pub(crate) fn with_assistive_source<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prior = std::mem::replace(&mut self.agents.assistive_gesture, true);
        let out = f(self);
        self.agents.assistive_gesture = prior;
        out
    }

    /// The marks of the gesture being dispatched, for a post to the event loop to carry
    /// ([`GestureMarks`]).
    pub(crate) const fn gesture_marks(&self) -> GestureMarks {
        GestureMarks {
            controller: self.agents.controller_gesture,
            assistive: self.agents.assistive_gesture,
            relayed: self.agents.relayed_gesture,
        }
    }

    /// Run `f` under `marks`, a gesture's marks a post carried ([`Self::gesture_marks`]): a
    /// control client's as [`Self::with_gesture_source`] runs one, an accessibility client's as
    /// [`Self::with_assistive_source`] does, and another process's relayed input under the relayed
    /// mark.
    pub(crate) fn with_gesture_marks<R>(
        &mut self,
        marks: GestureMarks,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.with_relayed_mark(marks.relayed, |app| {
            if marks.assistive {
                app.with_assistive_source(|app| app.with_gesture_source(marks.controller, f))
            } else {
                app.with_gesture_source(marks.controller, f)
            }
        })
    }

    /// Who stands behind the gesture being dispatched where no person at the window is known
    /// to: a control client ([`Self::person_gesture_refused`]), an accessibility client
    /// ([`AgentsState::assistive_gesture`]) or another process's relayed input
    /// ([`AgentsState::relayed_gesture`]). `None` for a person's own.
    pub(crate) const fn gesture_unvouched(&self) -> Option<crate::owner_keys_save::Unvouched> {
        if self.agents.controller_gesture {
            Some(crate::owner_keys_save::Unvouched::Controller)
        } else if self.agents.assistive_gesture {
            Some(crate::owner_keys_save::Unvouched::Assistive)
        } else if self.agents.relayed_gesture {
            Some(crate::owner_keys_save::Unvouched::Relayed)
        } else {
            None
        }
    }

    /// Run `f` with the dispatch marked as a control client's when `controller` is set, keeping a
    /// mark an enclosing dispatch already set, and restoring the mark after. Unlike
    /// [`Self::with_gesture_source`] it counts no gesture in `controller_gestures`: it marks a
    /// control client's whole input dispatch ([`App::input_to_session`]), so a person-only act
    /// anywhere in it refuses the client, while only the surfaces that count a gesture (an
    /// overlay, the tab menu, a native view) count one.
    pub(crate) fn with_controller_mark<R>(
        &mut self,
        controller: bool,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let marked = controller || self.agents.controller_gesture;
        let prior = std::mem::replace(&mut self.agents.controller_gesture, marked);
        let out = f(self);
        self.agents.controller_gesture = prior;
        out
    }

    /// Why the press being dispatched brings no other app up for `what` (a browser for a link,
    /// Finder for a folder), which would come up over what the person is looking at: no person
    /// is known to stand behind it ([`Self::gesture_unvouched`]: a control client's `act`, an
    /// accessibility client's request, another process's relayed input). Logged; `None` for a
    /// person's own click or key (owner ruling 2026-11-09).
    pub(crate) fn other_app_press_refusal(&self, what: &str) -> Option<String> {
        self.gesture_unvouched()?;
        aterm_log::info!(
            "desk: {what} was not opened for a press no person is known to stand behind"
        );
        Some(format!(
            "{what} opens for a person's own click or key; press it in the window"
        ))
    }

    /// Whether a copy or a paste being dispatched is refused for its clipboard: a control
    /// client's gesture against the person's own clipboard. A debug build pointed at a lab file
    /// copies to nobody's clipboard ([`crate::control::reaches_persons_clipboard`]), so a lab's
    /// gesture there is not refused; a release build always reaches the person's.
    pub(crate) fn clipboard_gesture_refused(&self) -> bool {
        self.person_gesture_refused() && crate::control::reaches_persons_clipboard()
    }

    /// Whether a control client's gesture is being dispatched, so `what` (a copy to, or a paste
    /// from, the person's clipboard) refuses it ([`Self::clipboard_gesture_refused`]): `selection.copy` and `selection.paste` ask for
    /// [`crate::command_registry::ActionAuthority::Clipboard`], which no control client reaches.
    /// A refusal is logged and answers an `invoke` with [`CLIPBOARD_REFUSED`].
    pub(crate) fn refuse_clipboard_gesture(&mut self, what: &str) -> bool {
        if !self.clipboard_gesture_refused() {
            return false;
        }
        note_clipboard_refused(what);
        self.pending_action_refusal = Some(CLIPBOARD_REFUSED.to_string());
        true
    }

    fn person_hold_session(&mut self, session: u64, on: bool) -> Result<(), String> {
        use crate::fabric::PersonHoldOutcome;
        if self.person_gesture_refused() {
            return Err(
                "Take Over and Hand Back are the person's own: a control client cannot press them"
                    .to_string(),
            );
        }
        let ctx = self
            .pool
            .get(session)
            .map(|s| s.ctx.clone())
            .ok_or_else(|| "no such session".to_string())?;
        let outcome = crate::fabric::person_hold(&ctx, on, &|| {});
        if outcome == PersonHoldOutcome::Busy {
            self.post_message(crate::message_reporters::hold_refused(
                on,
                "aterm is switching to its new version right now; nothing changed",
            ));
            return Err("a seamless update is committing this tab".to_string());
        }
        if on {
            crate::control::forget_driven_shell(&ctx.self_id);
            self.agents
                .held_since
                .entry(session)
                .or_insert_with(hold_unix_now);
        } else {
            self.agents.held_since.remove(&session);
        }
        self.agents.long_holds_moved = true;
        self.refresh_presence_session(session);
        if let Some(wid) = self.frontmost_window {
            self.publish_front_hold(wid);
        }
        self.palette_refresh_live();
        self.hand_long_holds();
        self.mark_status_dirty();
        Ok(())
    }

    /// Every standing person hold, oldest first, with its refused count: what Hand Back All
    /// lifts and the long hold's entry reads ([`Self::hand_long_holds`]).
    pub(crate) fn person_holds(&self) -> Vec<PersonHeld> {
        let mut out: Vec<PersonHeld> = self
            .pool
            .iter()
            .filter(|s| s.ctx.fabric.holds().person().is_some())
            .map(|s| PersonHeld {
                session: s.id,
                since: self
                    .agents
                    .held_since
                    .get(&s.id)
                    .copied()
                    .unwrap_or_else(hold_unix_now),
                refused: s.ctx.fabric.person_refusals(),
            })
            .collect();
        out.sort_by_key(|held| (held.since, held.session));
        out
    }

    /// State the long hold's whole answer (`Family::LongHold`, a level family): one entry per tab
    /// the person has held for [`LONG_HOLD`] or more on the wall clock, and one per tab an Owner
    /// client holds with no person's hold beside it (it halts the app verbs as the person's does,
    /// and nothing else names a forgotten one); none otherwise, so a Hand Back sweeps its entry.
    pub(crate) fn hand_long_holds(&mut self) {
        let now = hold_unix_now();
        let quiet = self.agents.long_holds_named.is_some()
            && !self.agents.long_holds_moved
            && self.agents.long_holds_epoch == self.pool.insert_epoch()
            && self.agents.long_holds_due.is_none_or(|due| now < due)
            && self
                .agents
                .held_since
                .keys()
                .all(|session| self.pool.get(*session).is_some());
        if quiet {
            return;
        }
        #[cfg(test)]
        {
            self.agents.long_hold_walks += 1;
        }
        self.agents.long_holds_moved = false;
        self.agents.long_holds_epoch = self.pool.insert_epoch();
        let held: Vec<(u64, Option<String>)> = self
            .pool
            .iter()
            .filter_map(|s| {
                let holds = s.ctx.fabric.holds();
                if holds.person().is_some() {
                    return Some((s.id, None));
                }
                holds
                    .standing()
                    .find(|hold| hold.origin == "local")
                    .map(|hold| (s.id, Some(hold.reason)))
            })
            .collect();
        self.agents
            .held_since
            .retain(|session, _| held.iter().any(|(s, _)| s == session));
        for (session, _) in &held {
            self.agents.held_since.entry(*session).or_insert(now);
        }
        let mut long: Vec<PersonHeld> = self
            .person_holds()
            .into_iter()
            .filter(|h| now.saturating_sub(h.since) >= LONG_HOLD.as_secs())
            .collect();
        let clients: Vec<(u64, String)> = held
            .iter()
            .filter_map(|(session, reason)| Some((*session, reason.clone()?)))
            .filter(|(session, _)| {
                self.agents
                    .held_since
                    .get(session)
                    .is_some_and(|since| now.saturating_sub(*since) >= LONG_HOLD.as_secs())
            })
            .collect();
        long.extend(clients.iter().map(|(session, _)| PersonHeld {
            session: *session,
            since: self.agents.held_since.get(session).copied().unwrap_or(now),
            refused: 0,
        }));
        let next_mark = self
            .agents
            .held_since
            .iter()
            .filter(|(session, _)| !long.iter().any(|h| h.session == **session))
            .map(|(_, since)| since.saturating_add(LONG_HOLD.as_secs()))
            .min();
        let restate = (!long.is_empty()).then(|| now.saturating_add(LONG_HOLD_RESTATE.as_secs()));
        self.agents.long_holds_due = match (next_mark, restate) {
            (Some(mark), Some(restate)) => Some(mark.min(restate)),
            (mark, restate) => mark.or(restate),
        };
        let mut words: Vec<LongHoldWords> = long
            .iter()
            .filter_map(|h| {
                let ctx = self.pool.get(h.session).map(|s| s.ctx.clone())?;
                let other = ctx
                    .fabric
                    .holds()
                    .standing()
                    .find(|hold| hold.origin != crate::fabric::PERSON_ORIGIN)
                    .map(|other| {
                        let whose = if other.origin == "fleet" {
                            "the message bus holds it too"
                        } else {
                            "an Owner client holds it too"
                        };
                        format!(
                            "{whose}: {}",
                            crate::presence::hold_reason_words(&other.reason)
                        )
                    });
                let client = clients
                    .iter()
                    .find(|(session, _)| *session == h.session)
                    .map(|(_, reason)| crate::presence::hold_reason_words(reason));
                Some(LongHoldWords {
                    session: h.session,
                    sid: ctx.self_id.as_str().to_string(),
                    label: self.tab_label_of_session(h.session),
                    manager: self.manager_label_of(h.session),
                    refused: h.refused,
                    other: other.filter(|_| client.is_none()),
                    client,
                })
            })
            .collect();
        words.sort_by_key(|w| w.session);
        if self.agents.long_holds_named.as_ref() == Some(&words) {
            return;
        }
        let entries: Vec<aterm_messages::exceptions::Entry> =
            words.iter().map(LongHoldWords::entry).collect();
        self.agents.long_holds_named = Some(words);
        self.except_replace(aterm_messages::exceptions::Family::LongHold, &entries);
    }

    /// Make the next exceptions look state the long hold's whole answer again: at the resume
    /// settle, once a seamless successor has adopted every entry its predecessor left
    /// (`App::ripen_exceptions`).
    pub(crate) fn restate_long_holds(&mut self) {
        self.agents.long_holds_named = None;
    }

    /// When the next person hold reaches [`LONG_HOLD`], for the exceptions look's wake; `None`
    /// when none will.
    pub(crate) fn long_hold_deadline(&self) -> Option<Instant> {
        let due = self.agents.long_holds_due?;
        let wait = due.saturating_sub(hold_unix_now());
        Some(Instant::now() + Duration::from_secs(wait))
    }

    /// The label a role or a pinned title gives `session` (the first rung of
    /// [`Self::tab_label_of_session`]), `None` where neither names it.
    pub(crate) fn named_label_of_session(&self, session: u64) -> Option<String> {
        let s = self.pool.get(session)?;
        let (role, pin) = {
            let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (meta.role.clone(), meta.presentation_value("title"))
        };
        role.as_deref()
            .and_then(label_line)
            .or_else(|| pin.as_deref().and_then(label_line))
    }

    /// A session's tab label as every Agents row names it (the Overview, a manager's Workers
    /// rows, `Go to <manager>`, the long-hold entry): the words the person can find on the strip
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3, §6). A worker's one chip is the Workers
    /// chip, which reads this label, so while nothing it runs has titled its tab
    /// ([`untitled_by_its_program`]: its shell titled it, or its title says nothing) what it runs
    /// names it ([`worker_name`]): its agent, or a program worker's program.
    pub(crate) fn tab_label_of_session(&self, session: u64) -> String {
        let clip = |t: &str| label_line(title_without_agent_glyph(t));
        if let Some(label) = self.named_label_of_session(session) {
            return label;
        }
        if let Some(s) = self.pool.get(session) {
            // The live title, and whether nothing the tab runs gave it; a busy terminal's, as the
            // strip last read both.
            let rung = |t: &aterm_core::terminal::Terminal| {
                use crate::cwd_native::ReportedCwd as _;
                let cwd = t.native_working_directory();
                let title = crate::app_tabs::resolved_terminal_title_rung(
                    None,
                    title_without_agent_glyph(t.title()),
                    cwd.as_deref(),
                );
                (title, untitled_by_its_program(t))
            };
            let (live, untitled) = match s.term.try_lock() {
                Ok(t) => rung(&t),
                Err(std::sync::TryLockError::Poisoned(p)) => rung(&p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => self
                    .windows
                    .values()
                    .find_map(|ws| ws.tab_title_cache.get(&session))
                    .map_or((None, false), |cached| {
                        (Some(cached.title.clone()), cached.untitled)
                    }),
            };
            // A shell titles its tab by its folder and by each line it starts, and a worker's
            // line opens with environment words or its program's path and words, and a title that
            // says nothing leaves its folder: what it runs names the worker until that titles the
            // tab itself.
            if untitled
                && self.session_is_parked(session)
                && let Some(name) = worker_name(&s.ctx)
            {
                return name;
            }
            if let Some(label) = live.as_deref().and_then(clip) {
                return label;
            }
        }
        let (registry, sid) = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| (h.title.clone(), h.sid.clone()))
            .unzip();
        if let Some(label) = registry.as_deref().and_then(clip) {
            return label;
        }
        // A worker has no place on the strip for a position to name.
        if !self.session_is_parked(session)
            && let Some(position) = self.tab_position_of_session(session)
        {
            return format!("tab {position}");
        }
        match sid {
            Some(sid) => aterm_messages::presence::short_sid(sid.as_str()),
            None => format!("tab {session}"),
        }
    }

    fn tab_position_of_session(&self, session: u64) -> Option<usize> {
        self.windows
            .values()
            .find_map(|ws| {
                ws.tab_set.tabs().iter().position(|tab| {
                    tab.root.any_leaf(&mut |view| {
                        self.view_store
                            .get(*view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(session)
                    })
                })
            })
            .map(|index| index + 1)
    }

    /// The facts the tab menu's AGENT section is composed from for `session`: whether Hand Back
    /// has a hold to lift, the manager its task names, whether its task is live (Stop Worker…),
    /// and its workers, each as its row's words (`● bench — working 12m`).
    pub(crate) fn agent_menu_facts(&self, session: u64) -> crate::session_chrome::AgentMenu {
        use crate::task_record::TaskState;
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return crate::session_chrome::AgentMenu::default();
        };
        let stoppable = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .is_some_and(|t| matches!(t.state, TaskState::Working | TaskState::Stalled));
        let stoppable = stoppable || crate::native_act::renewal_pending(&ctx);
        let team = self.teams.get(&session);
        let manager = self.manager_of(session).and_then(|local| {
            let sid = self.pool.get(local)?.ctx.self_id.clone();
            Some((sid, self.tab_label_of_session(local)))
        });
        let workers = team
            .map(|team| team.members.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|&local| {
                let sid = self.pool.get(local)?.ctx.self_id.clone();
                let (word, since) = self.teams.get(&local)?.task?;
                let glyph = match TaskState::parse(word) {
                    Some(TaskState::Working) => "\u{25cf}",
                    Some(TaskState::Done) => "\u{2713}",
                    Some(TaskState::Stalled) => "\u{25c6}",
                    Some(TaskState::Failed) => crate::claude_lights::FAILED_GLYPH,
                    Some(TaskState::Exited) | None => "\u{00b7}",
                };
                let age = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
                let words = format!(
                    "{glyph} {} \u{2014} {word} {}",
                    self.tab_label_of_session(local),
                    crate::app_overview::coarse_age(age)
                );
                Some((sid, words))
            })
            .collect();
        crate::session_chrome::AgentMenu {
            person_hold: ctx.fabric.holds().hand_back_origin().is_some(),
            manager,
            stoppable,
            workers,
            worker: self.session_is_ephemeral(session),
        }
    }

    /// The session (by local id) `session`'s task names as its manager (`from=` a sid this
    /// instance hosts, read off the team table); `None` for none or `from=ctl`.
    pub(crate) fn manager_of(&self, session: u64) -> Option<u64> {
        self.teams
            .get(&session)?
            .manager
            .as_ref()
            .and_then(|manager| manager.local)
    }

    /// The label of [`Self::manager_of`]'s session.
    pub(crate) fn manager_label_of(&self, session: u64) -> Option<String> {
        self.manager_of(session)
            .map(|manager| self.tab_label_of_session(manager))
    }

    /// `session`'s task record moved (`Wake::TaskChanged`): its own tab menu's AGENT section
    /// (Stop Worker…) and its manager's (the Workers rows) are composed again now, since the
    /// chrome cache's key does not carry the record.
    pub(crate) fn refresh_team_chrome(&mut self, session: u64) {
        let manager = self.manager_of(session);
        for s in std::iter::once(session).chain(manager) {
            self.session_chrome.remove(&s);
            self.refresh_meta_dependent_chrome(s);
        }
    }

    /// Open a file this menu wrote as a Markdown tab on `wid`, through the admission every Open
    /// Markdown… takes; the path is kept for the tests (`last_menu_document`).
    pub(crate) fn open_menu_document(&mut self, wid: WindowId, path: PathBuf) -> bool {
        self.last_menu_document = Some(path.clone());
        let uri = match crate::native_document_host::path_to_file_uri(&path) {
            Ok(uri) => uri,
            Err(e) => {
                self.menu_failure(
                    "document",
                    "Couldn't open the document",
                    &[&e.to_string(), &path.display().to_string()],
                );
                return false;
            }
        };
        if let Err(e) =
            self.request_document_tab_in_window(wid, crate::native_app::AppKind::Markdown, &uri)
        {
            self.menu_failure(
                "document",
                "Couldn't open the document",
                &[&e.to_string(), &path.display().to_string()],
            );
            return false;
        }
        true
    }

    fn menu_failure(&mut self, verb: &str, title: &str, lines: &[&str]) {
        self.post_message(crate::message_reporters::fabric_failure(verb, title, lines));
    }

    /// Read a command into a tab (Read This Mac, Read the Fleet, Fleet Machines, the message
    /// bus's three): confirm when the verb changes something, then run its plan
    /// ([`Self::run_cli_plan`]).
    pub(crate) fn run_cli_to_tab(&mut self, wid: WindowId, verb: CliVerb) {
        let real_window = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_some());
        if real_window && let Some((title, body, proceed)) = verb.confirmation() {
            match self.apprt.confirm(title, body, proceed) {
                Some(true) => {}
                Some(false) => return,
                None => {
                    self.menu_failure(
                        verb.key(),
                        &format!("Couldn't start {}", verb.title()),
                        &[
                            "no confirmation dialog on this platform",
                            &format!("run `{}` in a shell", verb.words()),
                        ],
                    );
                    return;
                }
            }
        }
        self.run_cli_plan(wid, CliPlan::new(verb));
    }

    /// Run an `aterm` command into a document tab, for the Agents rows and the menu bar alike:
    /// keep the plan (`last_cli_plan`) and, on a real window, run the command as a child on its
    /// own thread, never in this process, whose descriptor budget a fleet read would spend.
    pub(crate) fn run_cli_plan(&mut self, wid: WindowId, plan: CliPlan) {
        let real_window = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_some());
        self.last_cli_plan = Some(plan.clone());
        if !real_window {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let (key, title) = (plan.verb.key(), plan.title.clone());
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-cli-tab".to_string(),
            crate::qos::Role::Background,
            move || {
                let cli = crate::ledger_key::aterm_cli();
                let (status, stdout, stderr) =
                    match crate::bounded_child::run_bounded(&cli, &plan.argv, plan.verb.cap()) {
                        Ok(ran) => (Ok(ran.code), ran.stdout, ran.stderr),
                        Err(e) => (Err(e), String::new(), String::new()),
                    };
                let text = cli_markdown(&plan, &status, &stdout, &stderr);
                let status = match write_private(&plan.out, &text) {
                    Ok(()) => status,
                    Err(e) => Err(format!("could not write {}: {e}", plan.out.display())),
                };
                let _ = proxy.send_event(crate::Wake::CliTab(Box::new(CliTabOutcome {
                    wid,
                    plan,
                    status,
                })));
            },
        );
        if let Err(e) = spawned {
            self.menu_failure(key, &format!("Couldn't start {title}"), &[&e.to_string()]);
        }
    }

    /// The child ended (`Wake::CliTab`): its output opens as a tab on the window that asked (or
    /// the front window if that one is gone), and how it ended is a record, never an error row:
    /// a bus that is off, or a fleet Mac that did not answer, is what the tab says
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4).
    pub(crate) fn complete_cli_tab(&mut self, outcome: CliTabOutcome) {
        let verb = outcome.plan.verb;
        let has_tab = outcome.plan.out.is_file();
        let reason = match outcome.status {
            Ok(Some(0)) => String::new(),
            _ => cli_failure_reason(&outcome.status),
        };
        self.post_message(crate::message_reporters::cli_tab_finished(
            verb.key(),
            &outcome.plan.title,
            &reason,
            has_tab,
        ));
        let wid = if self.windows.contains_key(&outcome.wid) {
            Some(outcome.wid)
        } else {
            self.frontmost_window
        };
        if let Some(wid) = wid
            && has_tab
            && self.open_menu_document(wid, outcome.plan.out)
            && outcome.plan.raise
        {
            self.raise_window(wid);
        }
    }

    /// A read the menu bar's status item asked for (All activity…), on the front window (else
    /// the first), raised when its tab opens.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn run_status_read(&mut self, plan: CliPlan) {
        if let Some(wid) = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())
        {
            self.run_cli_plan(wid, plan.raised());
        }
    }
}

#[cfg(test)]
#[path = "app_take_over_tests.rs"]
mod tests;
