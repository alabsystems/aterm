// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TAKE OVER / HAND BACK (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): the
//! person's own hold on one tab, and the window side of the menu rows around
//! it. The bar rows live in [`crate::menu`]; the dispatch arms in
//! `app_input.rs` hand each one here.
//!
//! * **Take Over This Tab / Hand Back This Tab** set and lift the PERSON'S
//!   slot ([`crate::fabric::person_hold`], `origin=person`): only the person
//!   types in the tab until they hand it back. No socket can name that slot,
//!   and no controller gesture reaches these acts ([`AgentsState::controller_gesture`]):
//!   a key or pointer a control client drives into an open palette, Overview
//!   or tab menu is refused here, so the held agent cannot press Hand Back
//!   for the person. A hold that stands 30 minutes is one digest entry
//!   ([`aterm_messages::exceptions::Family::LongHold`]).
//! * **Read This Mac / Read the Fleet / Fleet Machines** and the palette's
//!   **Message Bus: Status / Turn On… / Turn Off…** run an `aterm` command as
//!   a child process on a thread and open its output as a tab
//!   ([`App::run_cli_to_tab`]); how it ended is a record, never an error row.
//!   `on`/`off` ask first (the platform's confirm sheet).
//! * **Show Needs in Windows** flips the live bit at the click and queues the
//!   `[presence] rim` leaf's durable write through the serialized config lane
//!   (`App::queue_presence_write`), as Serious Mode persists.
//! * **Show Identity** opens the focused session's identity as a tab.
//!
//! Headless (no OS window, the test harness) every row composes and writes
//! nothing outside the process: the command plan is kept on the App, no child
//! runs, no sheet is shown.

use crate::platform::AppRt as _;
use crate::{App, WindowId};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the person's hold stands before the digest says so once
/// ([`App::hand_long_holds`]): long enough that a hold is no longer the
/// person at the keyboard, short enough that a forgotten one is found while
/// its harness and manager still matter.
pub(crate) const LONG_HOLD: Duration = Duration::from_secs(30 * 60);

/// How often a standing long hold's entry is composed again
/// ([`App::hand_long_holds`]): its refusal count, its manager and another
/// hold beside it move with no hold taken or handed back, so a walk a minute
/// keeps its words at most a minute behind (the digest itself restates
/// moved words at its own quiet pace, `aterm_messages::need::RESTATE_QUIET`).
const LONG_HOLD_RESTATE: Duration = Duration::from_secs(60);

#[cfg(test)]
thread_local! {
    /// TEST CLOCK of [`hold_unix_now`]: `Some(unix seconds)` stands in for
    /// the wall clock on this thread.
    pub(crate) static HOLD_UNIX_CLOCK_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// THE LONG HOLD'S CLOCK, in unix seconds: the WALL clock, because
/// `Instant` does not advance while a Mac sleeps, and a tab taken over and
/// forgotten overnight with the lid closed is the case the entry exists for
/// (the rule the unsupervised limit's recheck follows).
fn hold_unix_now() -> u64 {
    #[cfg(test)]
    if let Some(now) = HOLD_UNIX_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
        return now;
    }
    aterm_time::unix_secs().unwrap_or(0)
}

#[cfg(any(target_os = "macos", test))]
/// How long the Agents menu's published live bits stand on a turn that
/// rebuilt no glance ([`App::publish_agents_live`]): the `claude` look's own
/// bound, so an install or a fleet join greys a row in or out within it.
const AGENTS_LIVE_TTL: Duration = Duration::from_secs(2);

/// The window side of the Agents rows, one field on the App.
#[derive(Debug, Default)]
pub(crate) struct AgentsState {
    /// When this process first saw each session's person hold — or an Owner
    /// client's, with none of the person's beside it — by local id, in unix
    /// seconds ([`hold_unix_now`]; [`App::take_over_session`] records the
    /// person's; a client's, and a hold a seamless update carried, are first
    /// seen at the next look, [`App::hand_long_holds`]).
    pub(crate) held_since: std::collections::BTreeMap<u64, u64>,
    /// A hold moved since the last look walked the holds
    /// ([`App::hand_long_holds`]): set where the person takes one or hands
    /// one back, and where a session's published hold moved (a client's over
    /// the wire, `App::publish_need`).
    pub(crate) long_holds_moved: bool,
    /// The refusals the person's holds had counted when the menu-bar glance
    /// last read them ([`App::note_person_refusals`]).
    pub(crate) refusals_seen: u64,
    /// When the next standing hold reaches [`LONG_HOLD`], in unix seconds,
    /// as of the last walk; `None` when none will.
    pub(crate) long_holds_due: Option<u64>,
    /// The session pool's insert epoch at the last walk: a session added
    /// since (a seamless successor's adopted tab) can carry a hold.
    pub(crate) long_holds_epoch: u64,
    /// TEST RECORD: how many looks walked the holds.
    #[cfg(test)]
    pub(crate) long_hold_walks: u32,
    /// THE GESTURE BEING DISPATCHED IS A CONTROL CLIENT'S: a `key`, `text`
    /// or `pointer` verb driving an open overlay (the palette, Overview, the
    /// tab menu). Take Over and Hand Back are the person's own acts
    /// ([`crate::menu::InvokeAuthority::PersonOnly`]) and refuse while it is
    /// set; it is set only around that dispatch and restored after it.
    pub(crate) controller_gesture: bool,
    /// The words the last [`App::hand_long_holds`] answer stated, one per
    /// long hold, so an unchanged answer is not stated again. `None` until
    /// this process first states one, so its first look states its whole
    /// answer even when that answer is empty: an entry an earlier process
    /// left (a relaunch, or a seamless successor adopting its predecessor's
    /// at Commit) is swept where no hold here is long.
    pub(crate) long_holds_named: Option<Vec<LongHoldWords>>,
    /// Where Next That Needs You last landed: the need's `(session, key)`.
    pub(crate) next_need: Option<(u64, u64)>,
    /// When an open Overview was last snapshotted (`App::overview_prepaint`
    /// reads its coarse ages again a minute after).
    pub(crate) overview_read_at: Option<Instant>,
    /// When the Agents menu's live bits were last published
    /// (`App::publish_agents_live`).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) agents_live_at: Option<Instant>,
    /// TEST RECORD: how many signals Stop Worker sent.
    #[cfg(test)]
    pub(crate) stop_signals: u32,
    /// TEST RECORD: why the last Stop Worker signalled nothing, `None` when
    /// it did signal.
    #[cfg(test)]
    pub(crate) last_stop_refused: Option<crate::app_overview::StopRefused>,
    /// TEST ARM of the tab menu's Stop Worker sheet
    /// (`AppRt::confirm_owner`): `Some(answer)` stands in for the sheet a
    /// headless App cannot show — `Some(true)` Stop, `Some(false)` Cancel,
    /// `None` nobody could be asked; unset, the platform's own answer.
    #[cfg(test)]
    pub(crate) owner_sheet_for_test: Option<Option<bool>>,
}

/// One local need, as Next That Needs You orders them — in the status
/// item's order (`status_item::Glance::build`'s sort of this aterm's needs):
/// stuck before decide, then the oldest first (from when this aterm first
/// saw it published, `Herald::need_since`; a need of unknown age last), then
/// by the tab's label, then the session's local id (stable). So ⇧⌘J's first
/// stop is the first row under ❯'s Needs you.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NeedStop {
    pub(crate) severity: aterm_types::needs::Severity,
    /// Since when it has stood, unix seconds; `i64::MAX` for unknown.
    pub(crate) since: i64,
    /// The tab's label ([`App::tab_label_of_session`], as ❯ names the row).
    pub(crate) label: String,
    pub(crate) session: u64,
    /// The need's identity within its word ([`crate::needs::Need::key`]).
    pub(crate) key: u64,
}

/// NEXT THAT NEEDS YOU's step: the stop after `cursor` in `stops`' order,
/// wrapping; the first when the cursor is gone (that need resolved) or was
/// never set. The cursor is the need's `(session, key)`, so a need that
/// resolved drops out and one that moved to a new episode is a new stop.
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

/// Claude Code's idle mark, the glyph its OSC title leads with between turns
/// (`✳ <title>`; while it works, `◐`/`◑` alternate there).
const CLAUDE_IDLE_MARK: char = '\u{2733}';

/// A program's OSC title as a label names it: trimmed, and without the
/// status glyph an agent leads it with — a spinner frame (any braille frame,
/// Claude Code's `◐`/`◑`, the circle and corner spinners: the one frame
/// classifier, [`crate::chrome_label::is_spinner_frame`]) or Claude Code's
/// idle [`CLAUDE_IDLE_MARK`], followed by a blank — so a turn's start and end
/// name the tab alike, and a label read once never keeps one frame of an
/// animation. Unlike a snapshot's strip
/// ([`crate::chrome_label::without_spinner_frame`], which keeps a lone frame
/// as the title's own text and Claude Code's `✳` too), a glyph and a blank
/// with nothing after them name nothing here, so the tab falls through to
/// its cwd. Only a program's title: a title the person set is theirs, whole.
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

/// One standing person hold, as the menu bar's You hold row and the hover
/// read it ([`App::person_holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersonHeld {
    /// The session, by local id.
    pub(crate) session: u64,
    /// Since when this process saw it stand, in unix seconds (the wall
    /// clock, so its age counts through a sleep).
    pub(crate) since: u64,
    /// How many acts it refused since it was set.
    pub(crate) refused: u64,
}

/// What one long hold's digest entry says ([`App::hand_long_holds`]): the
/// composed words, compared whole so a count, a manager or another hold that
/// moved restates the entry, and nothing else does.
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
    /// AN OWNER CLIENT'S HOLD, no person's beside it: its reason in words.
    /// It halts the instance's app verbs as the person's does, so a
    /// forgotten one is named too, as a client's, with Hand Back as its way
    /// out (`fabric::Holds::hand_back_origin`).
    client: Option<String>,
}

impl LongHoldWords {
    /// The entry these words make.
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

/// The `[presence]` leaf key View ▸ Show Needs in Windows writes — the
/// registered settings key (`prefs::NESTED_LEAVES`), so the config lane
/// admits it. (`[presence] band` is retired with the band row.)
pub(crate) const PRESENCE_RIM_KEY: &str = crate::prefs::EDIT_PRESENCE_RIM;

/// The human name of the needs switch's leaf ([`PRESENCE_RIM_KEY`]) — the
/// not-saved notice's subject. Its resolved bit is
/// `Config::presence_rim_enabled` (absent ⇒ on).
pub(crate) const PRESENCE_RIM_LABEL: &str = "Show Needs in Windows";

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
    /// Message Bus: Turn On… (palette): `aterm fabric on`, behind a
    /// confirmation.
    BusOn,
    /// Message Bus: Turn Off… (palette): `aterm fabric off`, behind a
    /// confirmation.
    BusOff,
    /// The menu bar's read of one fleet Mac, whole or one session (`aterm ctl
    /// survey --host <dial> [--sid <sid>]`): its argv and title are the
    /// plan's ([`CliPlan::peer`]).
    ReadPeer,
    /// The menu bar's Take Over or Hand Back of a fleet Mac's session (`aterm
    /// ctl --host <dial> hold <sid> on|off`, the step-in): its argv and title
    /// are the plan's ([`CliPlan::driven`]).
    DrivePeer,
}

impl CliVerb {
    /// The verb's word in a message key and a document name: one row per verb
    /// on the band, so a rerun replaces its own outcome.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::ReadThisMac => "read-mac",
            Self::ReadFleet => "read-fleet",
            Self::FleetPeers => "fleet-peers",
            Self::BusStatus => "bus-status",
            Self::BusOn => "bus-on",
            Self::BusOff => "bus-off",
            Self::ReadPeer => "read-peer",
            Self::DrivePeer => "drive-peer",
        }
    }

    /// The CLI's argv after the program. The survey names no socket: Read
    /// This Mac is every aterm on this Mac (`aterm ctl survey` walks every
    /// local instance, another aterm's sessions too), and Read the Fleet adds
    /// every peer this Mac knows; the bus and fleet commands walk every local
    /// instance, or the fleet book, themselves.
    pub(crate) fn argv(self) -> Vec<String> {
        let words: &[&str] = match self {
            Self::ReadThisMac => &["ctl", "survey"],
            Self::ReadFleet => &["ctl", "survey", "--fleet", "--timeout", "3"],
            Self::FleetPeers => &["fleet", "peers"],
            Self::BusStatus => &["fabric"],
            Self::BusOn => &["fabric", "on"],
            Self::BusOff => &["fabric", "off"],
            // Named by the plan: the Mac and the session read.
            Self::ReadPeer => &["ctl", "survey"],
            Self::DrivePeer => &["ctl", "--host"],
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
            Self::ReadPeer => "Read a Fleet Mac",
            Self::DrivePeer => "Step In on a Fleet Mac",
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
            Self::ReadPeer => "aterm ctl survey --host",
            Self::DrivePeer => "aterm ctl --host",
        }
    }

    /// How long its child may run before it is killed
    /// (`bounded_child::run_bounded`): a read is bounded — `survey` ends
    /// itself first (3 s a fleet peer), so this is a hang bound, never a
    /// budget — while the bus's commands install and stop a broker and are
    /// never cut off midway.
    const fn cap(self) -> Option<Duration> {
        match self {
            Self::ReadThisMac
            | Self::ReadFleet
            | Self::FleetPeers
            | Self::ReadPeer
            | Self::DrivePeer => Some(crate::bounded_child::READ_CAP),
            Self::BusStatus | Self::BusOn | Self::BusOff => None,
        }
    }

    /// The confirmation `on`/`off` ask before running: title, body, and the
    /// proceed button's label. `None` for a read, which changes nothing.
    const fn confirmation(self) -> Option<(&'static str, &'static str, &'static str)> {
        match self {
            Self::ReadThisMac
            | Self::ReadFleet
            | Self::FleetPeers
            | Self::BusStatus
            | Self::ReadPeer
            | Self::DrivePeer => None,
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

/// What a row asked `aterm` to do: the verb, its argv, and the file its
/// output goes to (opened as a tab when the child ends). Kept on the App
/// (`last_cli_plan`) so the tests and a `chrome`-side read can see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CliPlan {
    pub(crate) verb: CliVerb,
    pub(crate) argv: Vec<String>,
    pub(crate) out: PathBuf,
    /// The record's and the tab's title: the verb's, or a fleet Mac's name.
    pub(crate) title: String,
    /// Raise the window its tab opens in ([`App::raise_window`]): the menu
    /// bar's reads, whose click never makes aterm the active app.
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

    /// The menu bar's read of one fleet Mac: `argv` after the program
    /// (`fleet_glance::fleet_read_args`), the tab titled `title`.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn peer(argv: Vec<String>, title: String) -> Self {
        Self {
            argv,
            title,
            raise: true,
            ..Self::new(CliVerb::ReadPeer)
        }
    }

    /// The menu bar's Take Over or Hand Back on one fleet Mac (`argv` after
    /// the program, `fleet_glance::fleet_hold_args`): its answer in a tab
    /// titled `title`.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn driven(argv: Vec<String>, title: String) -> Self {
        Self {
            argv,
            title,
            raise: true,
            ..Self::new(CliVerb::DrivePeer)
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

    /// The command as a human types it.
    fn words(&self) -> String {
        format!("aterm {}", self.argv.join(" "))
    }
}

/// A command's document: [`menu_document_path`] with this process's count of
/// them, so two reads in one second (two fleet rows' Read This Session) are
/// two files, never one rewritten under an open tab.
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
/// (`aterm-<kind>[-<sid>]-<unix seconds>.md`), so a tab left open on an
/// earlier one is never rewritten under it. `sid` is filtered to its safe
/// characters, the ledger key's rule.
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

/// Write `text` to `path`, created `0600` on unix (a survey of the Mac, or a
/// bus status, is the owner's to read).
pub(crate) fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(text.as_bytes())
}

/// The Show Identity tab's text: a heading, then either the `identities <name>`
/// reply verbatim in a fenced block, or the sentence for a session with none.
pub(crate) fn identity_markdown(sid: &str, identity: Option<&str>, reply: Option<&str>) -> String {
    let mut s = String::with_capacity(256 + reply.map_or(0, str::len));
    match identity {
        Some(name) => {
            s.push_str(&format!("# Identity \u{2014} {name}\n\n"));
            s.push_str(&format!(
                "Session {sid} runs under the agent identity `{name}`: its agents keep \
                 their homes (and logins) in the identity's own directory. Read-only.\n\n"
            ));
            s.push_str("```text\n");
            s.push_str(reply.unwrap_or("").trim_end());
            s.push_str("\n```\n");
        }
        None => {
            s.push_str("# Identity \u{2014} none\n\n");
            s.push_str(&format!(
                "Session {sid} carries no agent identity: it runs under your own agent \
                 configuration. File \u{25b8} New Tab With Identity\u{2026} (or `spawn \
                 identity=<name>`) starts a session that has one.\n"
            ));
        }
    }
    s
}

/// A command's tab text: the command, how it ended, what a Take Over on
/// another Mac that took its session leaves to do ([`driven_next`]), then its
/// stdout and stderr in fenced blocks.
fn cli_markdown(
    plan: &CliPlan,
    status: &Result<Option<i32>, String>,
    stdout: &str,
    stderr: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", plan.words()));
    s.push_str(&format!("{}\n\n", cli_status_words(status)));
    if matches!(status, Ok(Some(0))) {
        s.push_str(&driven_next(plan).unwrap_or_default());
    }
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

/// What to do once a Take Over on another Mac took its session (`plan` a
/// [`CliPlan::driven`] whose argv is `fleet_glance::fleet_hold_args`'s, its
/// way `on`): type into it over the fleet, and hand it back — the hold
/// stands until then, whatever the session's need. `None` for any other
/// command, Hand Back's own among them.
fn driven_next(plan: &CliPlan) -> Option<String> {
    let [_, _, host, _, sid, way] = plan.argv.as_slice() else {
        return None;
    };
    (plan.verb == CliVerb::DrivePeer && way == "on").then(|| {
        format!(
            "This Mac holds `{sid}` on {host} until it hands it back: meanwhile that Mac's \
             harness leaves it alone and its app verbs refuse.\n\n\
             - Type into it: `aterm ctl --host {host} @{sid} send <text>`, or `@{sid} key <k>`\n\
             - Hand it back: \u{276f} \u{25b8} Held from here \u{25b8} Hand Back, or \
             `aterm ctl --host {host} hold {sid} off`\n\n"
        )
    })
}

/// Why a command did not end cleanly, in a few words — the record's reason:
/// `exited 2`, `killed by a signal`, or the spawn's own error.
fn cli_failure_reason(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(code)) => format!("exited {code}"),
        Ok(None) => "killed by a signal".to_string(),
        Err(e) => e.clone(),
    }
}

/// The output tab's line under its heading (the command): how it ended, and
/// nothing the heading or the page itself already says.
fn cli_status_words(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(0)) => "Done.".to_string(),
        Ok(Some(code)) => format!("Exited {code}."),
        Ok(None) => "Killed by a signal.".to_string(),
        Err(e) => format!("Could not run: {e}"),
    }
}

impl App {
    /// View ▸ Show Needs in Windows (`[presence] rim`) as applied: off, no
    /// window draws its border or its pill.
    pub(crate) fn presence_rim_on(&self) -> bool {
        self.presence_rim_on
    }

    /// Adopt `[presence] rim` from the config the App holds — on load and on
    /// every reload — publishing the bit for the native menu's checkmark and
    /// re-projecting every window when it moved. The retired `[presence]
    /// band` is read by nobody: the band row is gone (owner surfaces §5).
    pub(crate) fn adopt_presence_config(&mut self) {
        let rim = self.config.presence_rim_enabled();
        crate::menu::set_needs_in_windows(rim);
        if rim == self.presence_rim_on {
            return;
        }
        self.presence_rim_on = rim;
        self.refresh_presence_all_windows();
    }

    /// Set the needs switch live: publish it, re-project every window, and
    /// re-resolve any open palette's checkmark.
    pub(crate) fn set_presence_bit(&mut self, on: bool) {
        self.presence_rim_on = on;
        crate::menu::set_needs_in_windows(self.presence_rim_on);
        self.refresh_presence_all_windows();
        self.palette_refresh_live();
    }

    /// View ▸ Show Needs in Windows: flip the border-and-pill bit now and
    /// persist it as `[presence] rim`. A write the config lane cannot even
    /// queue (no event-loop proxy, a closed lane) is said on the notice; the
    /// live flip stands for this run.
    pub(crate) fn user_toggle_needs_in_windows(&mut self) {
        let desired = !self.presence_rim_on;
        self.set_presence_bit(desired);
        if let Err(error) = self.queue_presence_write(PRESENCE_RIM_KEY, desired) {
            self.post_message(crate::message_reporters::presence_not_saved(
                PRESENCE_RIM_KEY,
                PRESENCE_RIM_LABEL,
                &error.to_string(),
                false,
            ));
        }
    }

    /// Every local need, read off each session's published verdict (the one
    /// value every reader reads; never classified here), with its age and
    /// label as the status item's row has them ([`NeedStop`]'s order).
    pub(crate) fn local_needs(&self) -> Vec<NeedStop> {
        let needs: Vec<(u64, aterm_types::needs::Severity, u64)> = self
            .pool
            .iter()
            .filter_map(|s| {
                let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                let need = tl.need().need.as_ref()?;
                Some((s.id, need.word.severity(), need.key))
            })
            .collect();
        needs
            .into_iter()
            .map(|(session, severity, key)| NeedStop {
                severity,
                since: self.presence.herald.need_since(session).unwrap_or(i64::MAX),
                label: self.tab_label_of_session(session),
                session,
                key,
            })
            .collect()
    }

    /// AGENTS ▸ NEXT THAT NEEDS YOU (⇧⌘J): go to the most pressing local need
    /// ([`pick_next_need`]); pressed again, to the next. `Err` when nothing
    /// needs the person here.
    pub(crate) fn next_need(&mut self) -> Result<(), String> {
        let stops = self.local_needs();
        let stop = pick_next_need(&stops, self.agents.next_need)
            .ok_or_else(|| "nothing here needs you".to_string())?;
        self.agents.next_need = Some((stop.session, stop.key));
        self.raise_session_by_id(stop.session)
    }

    /// The Agents menu's live bits ([`crate::menu::AgentsLive`]): a local
    /// need, a fleet, a window, and why Summarize cannot start.
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

    /// Publish [`Self::agents_live`] for `validateMenuItem:` on the way to a
    /// wait — only where it can have moved: a turn whose changes rebuilt the
    /// status glance (`rebuilt`: a need published, a hold, a task, a window),
    /// or once its time-bound looks (the fleet secret, `claude` on the PATH,
    /// the worker's shell) are older than [`AGENTS_LIVE_TTL`]. Every other
    /// wait costs one clock compare: no pool walk, no stat, no allocation.
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

    /// Whether Hand Back has a hold to lift on `wid`'s front session: the
    /// person's, or an Owner client's where no person's stands
    /// (`fabric::Holds::hand_back_origin`). Take Over is live where it has
    /// none.
    pub(crate) fn front_person_hold(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| s.ctx.fabric.holds().hand_back_origin().is_some())
    }

    /// Whether ANOTHER MAC holds `wid`'s front session and nothing of this
    /// Mac's does (the step-in's `origin=remote`): Take Over is live beside
    /// Hand Back there — the person wins, taking it or handing it back.
    pub(crate) fn front_remote_hold(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| {
                s.ctx.fabric.holds().hand_back_origin() == Some(crate::fabric::REMOTE_ORIGIN)
            })
    }

    /// Publish `wid`'s front person hold for the native menu's synchronous
    /// validate (Take Over / Hand Back).
    pub(crate) fn publish_front_hold(&mut self, wid: WindowId) {
        crate::menu::set_front_person_hold(self.front_person_hold(wid));
        crate::menu::set_front_remote_hold(self.front_remote_hold(wid));
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

    /// TAKE OVER `session` (by local id): the person's hold
    /// ([`crate::fabric::person_hold`]). `Err` says why nothing moved: a
    /// control client's gesture, no such session, or a committing update
    /// (said on the band too).
    pub(crate) fn take_over_session(&mut self, session: u64) -> Result<(), String> {
        self.person_hold_session(session, true)
    }

    /// HAND BACK `session`: lift the person's slot, else an Owner client's
    /// where no person's stands (`fabric::person_hold`); never the fleet's.
    pub(crate) fn hand_back_session(&mut self, session: u64) -> Result<(), String> {
        self.person_hold_session(session, false)
    }

    /// Whether the gesture being dispatched is a control client's, so a
    /// person-only act refuses it ([`AgentsState::controller_gesture`]).
    pub(crate) fn person_gesture_refused(&self) -> bool {
        self.agents.controller_gesture
    }

    /// Whether a GATEWAY CARD is open on `wid`: the palette, the Overview or
    /// a tab's in-grid menu, whose rows hold the person's own acts (Take
    /// Over, Hand Back) behind a key. The Autopilot menu holds none of them
    /// (its rows are the mode, the timers and Harness settings, which a
    /// control client's own `key` presses as well), so `hwkey` into it
    /// presses nothing the person alone may; under a hold, opening it is
    /// refused with the other gateways (`fabric::open_halt_refusal`).
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

    /// `hwkey`'s main-thread hop (`Wake::HwKeyTarget`): the front window's
    /// AppKit number (`0` with none), or a REFUSAL while a gateway card is
    /// open there ([`Self::gateway_card_open`]). `hwkey` posts real
    /// `NSEvent`s, which take the person's own key path and so carry no
    /// gesture mark: where a control client's `key` into an open card cannot
    /// press Take Over ([`Self::person_gesture_refused`]), its `hwkey` could,
    /// setting a hold the person never took. Under a hold the app-verb halt
    /// already refuses `hwkey`; this closes the road while none stands.
    pub(crate) fn hwkey_target(&self) -> Result<(i64, bool), &'static str> {
        let Some(wid) = self.frontmost_window else {
            return Ok((0, false));
        };
        if self.gateway_card_open(wid) {
            return Err(
                "a palette, Overview or tab menu is open: its rows are the person's (close it)",
            );
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

    /// A CONTROL CLIENT'S TAB-MENU CHORD UNDER A HOLD OPENS NO CARD (the
    /// in-grid strip's Menu key, Windows and Linux): the card is `invoke`'s
    /// rows behind a key, so it opens for a control client only where `open
    /// tab-menu` would ([`crate::fabric::open_halt_refusal`]); the person's
    /// own chord always opens it.
    #[cfg(any(windows, target_os = "linux", test))]
    pub(crate) fn controller_card_refused(&self) -> bool {
        self.person_gesture_refused()
            && crate::fabric::app_halt_refusal(&self.store, "invoke").is_some()
    }

    /// Run `f` with the dispatch marked as a control client's gesture when
    /// `controller` is set, restoring the mark after (the overlay gate and
    /// the `pointer` verb).
    pub(crate) fn with_gesture_source<R>(
        &mut self,
        controller: bool,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prior = std::mem::replace(&mut self.agents.controller_gesture, controller);
        let out = f(self);
        self.agents.controller_gesture = prior;
        out
    }

    /// [`Self::take_over_session`] / [`Self::hand_back_session`]'s body. The
    /// fabric leaf moves under the same guard and records the same timeline
    /// event a wire `hold` does; the need verdict, the menu rows and the
    /// palette are refreshed at once (the fabric wake also arrives; each
    /// refresh is change-gated). NEVER A WAIT ON A COMMITTING UPDATE: the
    /// Commit's fence is this thread's own, so the press is answered at once
    /// (`PersonHoldOutcome::Busy`) and said.
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
        // The `hold` mark rings the session's watchers where it is recorded.
        let outcome = crate::fabric::person_hold(&ctx, on, &|| {});
        if outcome == PersonHoldOutcome::Busy {
            self.post_message(crate::message_reporters::hold_refused(
                on,
                "aterm is switching to its new version right now; nothing changed",
            ));
            return Err("a seamless update is committing this tab".to_string());
        }
        if on {
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

    /// Every standing person hold, oldest first: what the menu bar's You hold
    /// row and the hover read (the refused count beside each).
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

    /// A WRITE THE PERSON'S HOLD REFUSED re-reads the menu bar's You hold
    /// row: the count moves on the control thread
    /// (`fabric::halt_refusal`), which wakes nothing here, so the way to
    /// every wait asks whether the held tabs' counts moved and dirties the
    /// glance where they did. A hold taken or handed back dirties it itself.
    /// With no person hold standing this is one map check.
    pub(crate) fn note_person_refusals(&mut self) {
        if self.agents.held_since.is_empty() {
            self.agents.refusals_seen = 0;
            return;
        }
        let counted: u64 = self
            .agents
            .held_since
            .keys()
            .filter_map(|session| self.pool.get(*session))
            .map(|s| s.ctx.fabric.person_refusals())
            .sum();
        if counted != self.agents.refusals_seen {
            self.agents.refusals_seen = counted;
            self.mark_status_dirty();
        }
    }

    /// THE LONG HOLD'S WHOLE ANSWER (`Family::LongHold`, a level family): one
    /// entry per tab the person has held for [`LONG_HOLD`] or more on the
    /// wall clock ([`hold_unix_now`]), and one per tab an Owner client has
    /// (its `hold … on`, no person's beside it: it halts the instance's app
    /// verbs just as the person's does, and a forgotten one is otherwise
    /// never named), none otherwise, so a Hand Back sweeps its entry. Stated where a hold moves ([`Self::person_hold_session`])
    /// and asked at every exceptions look (`App::ripen_exceptions`), which
    /// [`Self::long_hold_deadline`] wakes at the next one's mark. A hold a
    /// seamless update carried is first seen here, and counts from then.
    /// Says nothing again while its composed words are unchanged
    /// ([`LongHoldWords`]) — except this process's first answer, which is
    /// always stated, an empty one too, and the one after the resume settle
    /// ([`Self::restate_long_holds`]): an entry an earlier process left, or a
    /// seamless successor adopted at Commit, ends where no hold here is long.
    ///
    /// A LOOK WHERE NOTHING CAN HAVE MOVED COSTS A FEW COMPARES, not a walk:
    /// the exceptions look runs on every park. The holds are walked only at
    /// the first look and the settle, where a hold was taken or handed back,
    /// where a session joined the pool (a carried hold) or a held one left
    /// it, once the next mark has passed, and — while a long hold stands —
    /// once a [`LONG_HOLD_RESTATE`], so its refusal count, its manager and
    /// another hold beside it are said as they move.
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
        // The person's holds, and an Owner client's standing with none of
        // the person's: `(session, the client's reason)`.
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
                // Another hold beside the person's outlives Hand Back: said, so
                // the person knows the tab stays halted after it.
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
                    // A client's hold is the entry itself, not one beside it.
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

    /// The next exceptions look states the long hold's whole answer again
    /// whatever it was: at the resume settle, after which a seamless
    /// successor has adopted every entry its predecessor left
    /// (`App::ripen_exceptions`).
    pub(crate) fn restate_long_holds(&mut self) {
        self.agents.long_holds_named = None;
    }

    /// When the next person hold reaches [`LONG_HOLD`], for the exceptions
    /// look's wake; `None` when none will. The mark is on the wall clock, so
    /// the wake is placed from it each time the loop asks: a sleep past the
    /// mark wakes at once.
    pub(crate) fn long_hold_deadline(&self) -> Option<Instant> {
        let due = self.agents.long_holds_due?;
        let wait = due.saturating_sub(hold_unix_now());
        Some(Instant::now() + Duration::from_secs(wait))
    }

    /// A SESSION'S TAB LABEL AS EVERY AGENTS ROW NAMES IT (the Overview's
    /// chips and manages lines, a manager's Workers rows, Go to <manager>, the
    /// long-hold entry): the words the person can find on the strip
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3, §6). Its role, then the
    /// session's title pin (`meta set title`), both whole as they were set
    /// (`SessionHandle::display_title`'s rule: the person's words keep a
    /// leading glyph), then the strip's own rungs — the live OSC title,
    /// without the status glyph an agent leads it with
    /// ([`title_without_agent_glyph`]), then the `~`-abbreviated cwd — read
    /// under ONE term try-lock as [`App::tab_titles`] reads them, the
    /// window's last-read strip label standing in on contention. A worker
    /// started in a background tab has no registry title (only the active
    /// tab's title is published there) and no role, so these rungs are what
    /// name it. Only after them the registry title, then the tab's position
    /// in its window (`tab 3`, one-based, as the pill counts), then the
    /// session's short sid for a session no window shows.
    pub(crate) fn tab_label_of_session(&self, session: u64) -> String {
        // A set word (a role, the person's pin) is clipped whole; a program's
        // title loses its agent's glyph first.
        let whole = |t: &str| {
            Some(crate::session_timeline::sanitize_presentation_line(t, 40))
                .filter(|t| !t.is_empty())
        };
        let clip = |t: &str| whole(title_without_agent_glyph(t));
        if let Some(s) = self.pool.get(session) {
            // The meta leaf lock, dropped before the term lock is taken.
            let (role, pin) = {
                let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                (meta.role.clone(), meta.presentation_value("title"))
            };
            if let Some(label) = role
                .as_deref()
                .and_then(whole)
                .or_else(|| pin.as_deref().and_then(whole))
            {
                return label;
            }
            let rung = |t: &aterm_core::terminal::Terminal| {
                use crate::cwd_native::ReportedCwd as _;
                let cwd = t.native_working_directory();
                crate::app_tabs::resolved_terminal_title_rung(
                    None,
                    title_without_agent_glyph(t.title()),
                    cwd.as_deref(),
                )
            };
            let live = match s.term.try_lock() {
                Ok(t) => rung(&t),
                Err(std::sync::TryLockError::Poisoned(p)) => rung(&p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => self
                    .windows
                    .values()
                    .find_map(|ws| ws.tab_title_cache.get(&session).cloned()),
            };
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
        if let Some(position) = self.tab_position_of_session(session) {
            return format!("tab {position}");
        }
        match sid {
            Some(sid) => aterm_messages::presence::short_sid(sid.as_str()),
            None => format!("tab {session}"),
        }
    }

    /// The one-based position of the tab showing `session` (any pane) in its
    /// window, the lowest window id first; `None` when no window shows it.
    fn tab_position_of_session(&self, session: u64) -> Option<usize> {
        self.windows
            .values()
            .find_map(|ws| {
                ws.tab_set.tabs().iter().position(|tab| {
                    tab.root.leaves().into_iter().any(|view| {
                        self.view_store
                            .get(view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(session)
                    })
                })
            })
            .map(|index| index + 1)
    }

    /// The facts the tab menu's AGENT section is composed from for `session`:
    /// the person's hold, the manager its task names (when this aterm hosts
    /// it), whether its task is live (Stop Worker…), and the workers whose
    /// tasks it started, each as its row's words (`● bench — working 12m`).
    /// Who works for whom is the one team table (`App::rebuild_teams`,
    /// `crate::team`), rebuilt where a task, a role or a tab moves; the
    /// session's own task record says whether it can be stopped.
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
        }
    }

    /// The session (by local id) `session`'s task names as its manager:
    /// `from=` a sid this instance hosts, read off the team table; `None`
    /// for none or `from=ctl`.
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

    /// `session`'s task record moved (`Wake::TaskChanged`): the AGENT section
    /// of its own tab menu (Stop Worker…) and of its manager's (the Workers
    /// rows) read the record, which the chrome cache's key does not carry, so
    /// both are composed again now rather than at the cache's expiry.
    pub(crate) fn refresh_team_chrome(&mut self, session: u64) {
        let manager = self.manager_of(session);
        for s in std::iter::once(session).chain(manager) {
            self.session_chrome.remove(&s);
            self.refresh_meta_dependent_chrome(s);
        }
    }

    /// Show Identity (palette): the focused session's agent identity, read-only,
    /// as a Markdown tab: the `identities <name>` reply when the session wears
    /// one, else a sentence saying it runs under the user's own agent
    /// configuration.
    pub(crate) fn open_session_identity_tab(&mut self, wid: WindowId) {
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        let Some(sid) = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
        else {
            return;
        };
        let identity = self.identity_of_local(session);
        let reply = identity
            .as_deref()
            .map(|name| crate::agent_identity::cmd_identities(&self.store, name));
        let text = identity_markdown(&sid, identity.as_deref(), reply.as_deref());
        let path = menu_document_path("identity", &sid);
        match write_private(&path, &text) {
            Ok(()) => {
                self.open_menu_document(wid, path);
            }
            Err(e) => self.menu_failure(
                "identity",
                "Show Identity did not open",
                &[&e.to_string(), &path.display().to_string()],
            ),
        }
    }

    /// Open a file this menu wrote as a Markdown tab on `wid` (the same
    /// admission every Open Markdown… takes), remembering it for the tests.
    /// `false` (and said on the band) when no tab was asked for.
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

    /// An Agents menu or palette row that could not do its thing: a terse
    /// failure and its cause (then the path), keyed by `verb`.
    fn menu_failure(&mut self, verb: &str, title: &str, lines: &[&str]) {
        self.post_message(crate::message_reporters::fabric_failure(verb, title, lines));
    }

    /// READ A COMMAND INTO A TAB (Read This Mac, Read the Fleet, Fleet
    /// Machines, the message bus's three): confirm when the verb changes
    /// something, compose the plan, and run it ([`Self::run_cli_plan`]).
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

    /// THE ONE ROAD FROM AN `aterm` COMMAND TO A DOCUMENT TAB, the Agents
    /// rows' and the menu bar's alike (owner surfaces §3, §4): keep the plan
    /// (`last_cli_plan`), and — on a real window — run the command as a
    /// child on its own thread, never in this process, whose descriptor
    /// budget a fleet read would spend (§1). A read is bounded
    /// (`bounded_child::run_bounded`: SIGKILL at its cap, its output capped);
    /// its output opens as a tab when it ends ([`Self::complete_cli_tab`]).
    /// Headless composes the plan and runs nothing.
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
                // Waits on a CLI child the human is not blocked on.
                // `aterm.exe` is a CONSOLE image: on Windows `spawnfd::command`
                // gives it a windowless console, where a bare child opened a
                // visible one for its whole run (`ledger_key::launch`).
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

    /// The child ended (`Wake::CliTab`): its output opens as a tab on the
    /// window that asked (or the front window if that one is gone), and how it
    /// ended is a RECORD — a clean exit and a non-zero one alike: a bus that is
    /// off, or a fleet Mac that did not answer, is what the tab says, never an
    /// error row (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4). Only a
    /// command that left no output to open is the person's gesture failing.
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
            // A menu bar click never makes aterm the active app: without
            // the raise the owner, in another app or Space, sees nothing.
            self.raise_window(wid);
        }
    }

    /// A read the menu bar's status item asked for (Read This Mac, Read the
    /// Fleet, a fleet Mac's): on the front window (else the first), its
    /// window raised when the tab opens ([`CliPlan::raised`]).
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
mod tests {
    //! Tests for the Agents rows' App side: Take Over / Hand Back on the
    //! person's slot (and no control client pressing them), the long-hold
    //! entry, Next That Needs You's walk, the AGENT tab-menu facts, Set Role…
    //! writing the role, Show Needs in Windows gating the rim and queueing its
    //! durable write, and the CLI rows composing without running.

    use super::*;
    use crate::menu::MenuAction;
    use crate::palette::PaletteState;
    use crate::session_timeline::MetaField;
    use std::sync::Arc;

    fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.frontmost_window = Some(wid);
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        (app, wid, sid, ctx)
    }

    fn row(app: &App, wid: WindowId, a: MenuAction) -> crate::palette::PaletteRow {
        app.palette_snapshot(wid)
            .rows()
            .iter()
            .find(|r| r.action == a)
            .cloned()
            .unwrap_or_else(|| panic!("{a:?} has a row"))
    }

    /// TAKE OVER SETS THE PERSON'S SLOT AND HAND BACK LIFTS IT FIRST. The
    /// window's act fills `origin=person`; the rows swap (palette mirror and
    /// the published front bit); the published verdict says `hold_by=person`;
    /// the held agent's own wire `hold off` answers `OK hold=1` and lifts
    /// nothing; refusals are counted on the You hold row's read; Hand Back
    /// lifts the person's slot and leaves an Owner's beside it — which a
    /// second Hand Back lifts, as that client's own `hold off` would: a
    /// client's hold, which halts the instance's app verbs, has a way out at
    /// the window. A fleet hold is never the window's to lift.
    #[test]
    fn take_over_sets_the_person_slot_and_hand_back_lifts_it_then_a_clients() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.refresh_presence_window(wid);
        assert!(row(&app, wid, MenuAction::TakeOver).enabled);
        assert!(!row(&app, wid, MenuAction::HandBack).enabled);

        app.take_over(wid);
        assert_eq!(
            ctx.fabric.holds().person(),
            Some(crate::fabric::TAKEN_OVER),
            "the window path sets origin person"
        );
        assert!(app.front_person_hold(wid));
        assert!(!row(&app, wid, MenuAction::TakeOver).enabled);
        assert!(row(&app, wid, MenuAction::HandBack).enabled);
        assert_eq!(
            ctx.timeline.lock().unwrap().need().hold_word(),
            "person",
            "the published verdict names the person's hold"
        );
        let wire = format!("{} off", ctx.self_id.as_str());
        assert_eq!(
            crate::fabric::cmd_hold(&app.store, &wire, crate::fabric::HoldIssuer::Owner),
            "OK hold=1\n",
            "the held agent's own hold off lifts nothing of the person's"
        );
        assert!(ctx.fabric.holds().person().is_some());
        assert!(crate::fabric::halt_refusal(&ctx, "send").is_some());
        let held = app.person_holds();
        assert_eq!(
            held.iter()
                .map(|h| (h.session, h.refused))
                .collect::<Vec<_>>(),
            [(sid, 1)]
        );

        // An Owner's own hold beside it outlives Hand Back.
        let on = format!("{} on reason=mine", ctx.self_id.as_str());
        assert_eq!(
            crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
            "OK hold=1\n"
        );
        app.hand_back(wid);
        assert_eq!(ctx.fabric.holds().origins(), "local");
        assert!(app.person_holds().is_empty());
        // The client's hold stands alone: Hand Back is live for it.
        assert!(app.front_person_hold(wid));
        assert!(!row(&app, wid, MenuAction::TakeOver).enabled);
        assert!(row(&app, wid, MenuAction::HandBack).enabled);
        app.hand_back(wid);
        assert_eq!(ctx.fabric.holds().origins(), "-");
        assert!(!app.front_person_hold(wid));
        assert!(row(&app, wid, MenuAction::TakeOver).enabled);
        assert!(!row(&app, wid, MenuAction::HandBack).enabled);
        // NEGATIVE CONTROL: the fleet's hold is not the window's to lift.
        let fleet = format!("{} on origin=fleet reason=bus", ctx.self_id.as_str());
        assert_eq!(
            crate::fabric::cmd_hold(&app.store, &fleet, crate::fabric::HoldIssuer::Bridge),
            "OK hold=1\n"
        );
        assert!(!app.front_person_hold(wid));
        app.hand_back(wid);
        assert_eq!(ctx.fabric.holds().origins(), "fleet");
    }

    /// NO CONTROL CLIENT PRESSES THE PERSON'S PAIR. A control client's key
    /// into an open palette (`Source::Controller`, the `key` verb's road) on
    /// the Hand Back row lifts nothing, and its mark is gone after the
    /// gesture. CONTROL: the person's own key on the same row hands back.
    #[test]
    fn a_controller_key_into_the_palette_cannot_hand_back_and_a_persons_can() {
        use crate::input::{InputEvent, Source};
        let (mut app, wid, _sid, ctx) = app_with_stub();
        app.take_over(wid);
        assert!(ctx.fabric.holds().person().is_some());
        let enter = InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Enter),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        };
        for (src, lifted) in [(Source::Controller, false), (Source::Human, true)] {
            app.palette_enter();
            app.palette_input_event(wid, &InputEvent::Text("Hand Back".to_string()));
            assert_eq!(
                app.windows[&wid]
                    .palette()
                    .and_then(PaletteState::selected_target),
                Some(crate::palette::PaletteTarget::Menu(MenuAction::HandBack)),
                "the filter lands on Hand Back"
            );
            let _ = app.input_to_session(
                wid,
                enter.clone(),
                src,
                None,
                crate::app_input::PressPhase::Initial,
            );
            assert_eq!(
                ctx.fabric.holds().person().is_none(),
                lifted,
                "{src:?}: the person's slot"
            );
            assert!(
                !app.person_gesture_refused(),
                "the mark ends with the gesture"
            );
        }
        // The window's own path, marked as a controller's, refuses too.
        app.take_over(wid);
        let session = app.focused_session_id(wid).expect("a session");
        assert!(
            app.with_gesture_source(true, |app| app.hand_back_session(session))
                .is_err()
        );
        assert!(ctx.fabric.holds().person().is_some());
    }

    /// THE TAB MENU'S TAKE OVER RUNS IN THE GESTURE'S TURN: chosen under a
    /// control client's mark it takes nothing over; chosen by the person it
    /// takes the CLICKED tab over (the menu's AGENT row, its session).
    #[test]
    fn the_tab_menus_take_over_refuses_a_controller_and_takes_the_persons() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        app.tab_strip_rows = 1;
        let take_over_row = |app: &App| {
            app.windows[&wid]
                .tab_menu
                .as_ref()
                .expect("the menu is open")
                .entries
                .iter()
                .position(|e| {
                    matches!(
                        e,
                        crate::session_chrome::TabMenuEntry::Action {
                            action: MenuAction::TakeOver,
                            ..
                        }
                    )
                })
                .expect("the AGENT section's Take Over row")
        };
        for (controller, taken) in [(true, false), (false, true)] {
            assert!(app.open_active_tab_context_menu(wid));
            let i = take_over_row(&app);
            app.with_gesture_source(controller, |app| app.activate_tab_menu_entry(wid, i));
            assert_eq!(
                ctx.fabric.holds().person().is_some(),
                taken,
                "controller={controller}"
            );
        }
    }

    /// TAKE OVER NEVER WAITS ON A COMMITTING UPDATE: the press is answered at
    /// once, nothing moved, and the refusal is a row. CONTROL: unfenced, the
    /// same press takes the tab over.
    #[test]
    fn a_take_over_under_a_committing_update_answers_at_once() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let _ = ctx.fabric.fence_hold(true);
        let blocks = crate::fabric::fence_blocks_on_this_thread();
        app.take_over(wid);
        assert_eq!(crate::fabric::fence_blocks_on_this_thread(), blocks);
        assert!(ctx.fabric.holds().person().is_none(), "nothing moved");
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::KEY_FABRIC_HOLD)
                .is_some(),
            "the refusal is said"
        );
        ctx.fabric.lift_hold_fence();
        app.take_over(wid);
        assert!(ctx.fabric.holds().person().is_some());
    }

    /// Set this thread's long-hold wall clock ([`HOLD_UNIX_CLOCK_FOR_TEST`]).
    fn set_hold_clock(unix_s: Option<u64>) {
        HOLD_UNIX_CLOCK_FOR_TEST.with(|c| c.set(unix_s));
    }

    /// A HOLD THAT STANDS 30 MINUTES IS ONE DIGEST ENTRY, swept by Hand Back:
    /// the level family's whole answer names the held tab once, past the mark
    /// and never before it. The mark is on the WALL clock: no `Instant`
    /// advances between the two looks below, as none does while a Mac
    /// sleeps. CONTROL: a hold younger than the mark names none.
    #[test]
    fn a_long_hold_is_one_entry_and_hand_back_sweeps_it() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let prefix = aterm_messages::exceptions::Family::LongHold.prefix();
        let named = |app: &App| {
            app.messages
                .digest()
                .entries
                .iter()
                .filter(|e| e.key.starts_with(&prefix))
                .count()
        };
        let t0 = 1_800_000_000;
        set_hold_clock(Some(t0));
        app.take_over(wid);
        app.hand_long_holds();
        assert_eq!(named(&app), 0, "a fresh hold names none");
        assert!(app.long_hold_deadline().is_some());
        set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
        app.hand_long_holds();
        assert_eq!(named(&app), 1, "past the mark: one entry");
        assert_eq!(
            app.agents.long_holds_due,
            Some(t0 + LONG_HOLD.as_secs() + 1 + LONG_HOLD_RESTATE.as_secs()),
            "the next wake composes its words again"
        );
        assert!(ctx.fabric.holds().person().is_some());
        app.hand_back(wid);
        set_hold_clock(None);
        // The family's answer no longer names it: it ends on the digest's own
        // clock, past every pending end.
        if let Some(due) = app.messages_deadline() {
            let _ = app.settle_messages(due + Duration::from_secs(1));
        }
        assert_eq!(named(&app), 0, "Hand Back sweeps it");
    }

    /// A CLIENT'S HOLD THAT STANDS 30 MINUTES IS NAMED TOO: an Owner
    /// client's `hold … on`, set over the wire with no hold of the person's,
    /// halts the instance's app verbs as the person's does, so past the mark
    /// it is one entry of its own (`client-held`), and Hand Back — which lifts
    /// it — sweeps it. CONTROLS: before the mark it names none, and a fleet
    /// hold, which is not the window's to lift, is never named.
    #[test]
    fn a_clients_long_hold_is_named_and_hand_back_sweeps_it() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let prefix = aterm_messages::exceptions::Family::LongHold.prefix();
        let named = |app: &App| -> Vec<String> {
            app.messages
                .digest()
                .entries
                .iter()
                .filter(|e| e.key.starts_with(&prefix))
                .map(|e| e.key.clone())
                .collect()
        };
        let t0 = 1_800_000_000;
        set_hold_clock(Some(t0));
        let on = format!("{} on reason=review", ctx.self_id.as_str());
        assert_eq!(
            crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
            "OK hold=1\n"
        );
        // The wire's hold reaches the walk through the published verdict.
        app.refresh_presence_window(wid);
        app.hand_long_holds();
        assert!(named(&app).is_empty(), "a fresh hold names none");
        assert!(app.long_hold_deadline().is_some(), "its mark is armed");
        set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
        app.hand_long_holds();
        let keys = named(&app);
        assert_eq!(keys.len(), 1, "past the mark: one entry ({keys:?})");
        assert!(keys[0].ends_with(".client-held"), "{keys:?}");
        assert!(app.person_holds().is_empty(), "no hold of the person's");
        app.hand_back(wid);
        assert_eq!(ctx.fabric.holds().origins(), "-", "Hand Back lifted it");
        set_hold_clock(None);
        if let Some(due) = app.messages_deadline() {
            let _ = app.settle_messages(due + Duration::from_secs(1));
        }
        assert!(named(&app).is_empty(), "Hand Back sweeps it");
        // A fleet hold is the bus's: never named here.
        set_hold_clock(Some(t0));
        let fleet = format!("{} on origin=fleet reason=bus", ctx.self_id.as_str());
        assert_eq!(
            crate::fabric::cmd_hold(&app.store, &fleet, crate::fabric::HoldIssuer::Bridge),
            "OK hold=1\n"
        );
        app.refresh_presence_window(wid);
        set_hold_clock(Some(t0 + 2 * LONG_HOLD.as_secs()));
        app.hand_long_holds();
        assert!(named(&app).is_empty(), "a fleet hold is not named");
        set_hold_clock(None);
    }

    /// A STANDING LONG HOLD'S WORDS ARE SAID AS THEY MOVE: an act refused
    /// under the hold after its entry stood is in the entry once the next
    /// restatement minute has passed, where the same set of long holds used
    /// to keep the words it first had; a look inside the minute walks
    /// nothing. CONTROL: with nothing moved, the minute's walk restates
    /// nothing (the words are compared whole).
    #[test]
    fn a_standing_long_hold_says_its_refusals_as_they_move() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let t0 = 1_800_000_000;
        set_hold_clock(Some(t0));
        app.take_over(wid);
        let past = t0 + LONG_HOLD.as_secs() + 1;
        set_hold_clock(Some(past));
        app.hand_long_holds();
        let refused = |app: &App| {
            app.agents
                .long_holds_named
                .as_ref()
                .and_then(|w| w.first())
                .map(|w| w.refused)
        };
        assert_eq!(refused(&app), Some(0));
        // Refused under the person's hold, as a driver's `send` is.
        assert!(crate::fabric::halt_refusal(&ctx, "send").is_some());
        assert_eq!(ctx.fabric.person_refusals(), 1);
        let walks = app.agents.long_hold_walks;
        set_hold_clock(Some(past + 30));
        app.hand_long_holds();
        assert_eq!(
            app.agents.long_hold_walks, walks,
            "inside the minute: no walk"
        );
        assert_eq!(refused(&app), Some(0));
        set_hold_clock(Some(past + LONG_HOLD_RESTATE.as_secs()));
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, walks + 1, "the minute's walk");
        assert_eq!(refused(&app), Some(1), "the refusal is said");
        // CONTROL: the next minute, with nothing moved, states the same words.
        let said = app.agents.long_holds_named.clone();
        set_hold_clock(Some(past + 2 * LONG_HOLD_RESTATE.as_secs()));
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, walks + 2);
        assert_eq!(app.agents.long_holds_named, said);
        set_hold_clock(None);
    }

    /// A LOOK WHERE NOTHING CAN HAVE MOVED WALKS NO HOLDS: the exceptions
    /// look runs on every park, so before the next mark, with no hold taken
    /// or handed back and no session come or gone, it costs compares only.
    /// CONTROL: a Take Over, the mark passing, a held tab closing and the
    /// settle's restatement each walk again.
    #[test]
    fn a_look_where_no_hold_moved_walks_none() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        let t0 = 1_800_000_000;
        set_hold_clock(Some(t0));
        app.hand_long_holds();
        let first = app.agents.long_hold_walks;
        assert!(first >= 1, "the first look walks");
        for _ in 0..3 {
            app.hand_long_holds();
        }
        assert_eq!(app.agents.long_hold_walks, first, "quiet looks walk none");
        app.take_over(wid);
        let taken = app.agents.long_hold_walks;
        assert!(taken > first, "a Take Over walks");
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, taken, "and the next look not");
        set_hold_clock(Some(t0 + LONG_HOLD.as_secs()));
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, taken + 1, "the mark passed");
        app.restate_long_holds();
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, taken + 2, "the settle restates");
        let tab = app.windows[&wid].tab_set.active_index().expect("a tab");
        let _ = app.close_tab_at(wid, tab);
        assert!(app.pool.get(sid).is_none(), "the held tab closed");
        app.hand_long_holds();
        assert_eq!(app.agents.long_hold_walks, taken + 3, "a held tab left");
        set_hold_clock(None);
    }

    /// THE AGENTS MENU'S LIVE BITS ARE PUBLISHED WHERE THEY CAN MOVE: a turn
    /// that rebuilt the glance publishes them at once; a turn that rebuilt
    /// nothing publishes nothing new within the looks' bound, and does past
    /// it. CONTROL: the first publish of a process states them.
    #[test]
    fn the_agents_live_bits_are_published_where_they_can_move() {
        use aterm_types::needs::NeedWord;
        let before = crate::menu::agents_live();
        let (mut app, _wid, _sid, ctx) = app_with_stub();
        let set_need = |need: Option<crate::needs::Need>| {
            let _ = ctx
                .timeline
                .lock()
                .unwrap()
                .set_need(crate::needs::Verdict {
                    need,
                    ..crate::needs::Verdict::default()
                });
        };
        let now = Instant::now();
        app.publish_agents_live(false, now);
        assert!(!crate::menu::agents_live().needs_local, "the first publish");
        set_need(Some(crate::needs::Need {
            word: NeedWord::Ask,
            key: 1,
            what: String::new(),
            harness: None,
        }));
        app.publish_agents_live(false, now + Duration::from_millis(500));
        assert!(
            !crate::menu::agents_live().needs_local,
            "a quiet turn within the bound re-reads nothing"
        );
        app.publish_agents_live(true, now + Duration::from_millis(500));
        assert!(
            crate::menu::agents_live().needs_local,
            "a rebuilt glance publishes"
        );
        set_need(None);
        app.publish_agents_live(false, now + Duration::from_millis(600));
        assert!(
            crate::menu::agents_live().needs_local,
            "still within the bound"
        );
        app.publish_agents_live(false, now + AGENTS_LIVE_TTL + Duration::from_secs(1));
        assert!(
            !crate::menu::agents_live().needs_local,
            "past the bound it re-reads"
        );
        crate::menu::set_agents_live(before);
    }

    /// A LONG-HOLD ENTRY AN EARLIER PROCESS LEFT ENDS where no hold here is
    /// long: the family is told only where a hold moves, so this process
    /// states its whole answer at its first look — an empty one too, which
    /// starts the earlier entry's end — and again at the resume settle, after
    /// which a seamless successor has adopted its predecessor's entries.
    /// Between the two an unchanged answer is not stated again (the adopted
    /// entry's end has not started). NEGATIVE CONTROL: a hold here past the
    /// mark keeps one entry through the digest's clock, this tab's own.
    #[test]
    fn a_long_hold_entry_an_earlier_process_left_ends_where_no_hold_here_is_long() {
        use aterm_messages::exceptions::Family;
        let prefix = Family::LongHold.prefix();
        let keys = |app: &App| -> Vec<String> {
            app.messages
                .digest()
                .entries
                .iter()
                .filter(|e| e.key.starts_with(&prefix))
                .map(|e| e.key.clone())
                .collect()
        };
        let settle = |app: &mut App| {
            let due = app.messages.digest_deadline().expect("an end is due");
            let _ = app.settle_messages(due + Duration::from_secs(1));
        };
        let seed = |app: &mut App| {
            let earlier = crate::message_reporters::long_hold_entry("s-earlier", "bench", 30, None);
            app.except_replace(Family::LongHold, &[earlier]);
            assert_eq!(keys(app).len(), 1, "the earlier process's entry");
        };

        // The first look: a relaunch restored no hold.
        let (mut app, _wid, _sid, _ctx) = app_with_stub();
        app.agents.long_holds_named = None;
        seed(&mut app);
        app.ripen_exceptions(Instant::now());
        assert!(
            app.messages.digest_deadline().is_some(),
            "the first look's empty answer started the entry's end"
        );
        settle(&mut app);
        assert!(keys(&app).is_empty(), "swept");

        // The settle: an entry adopted after the first look (a seamless
        // successor's Commit) is swept by the restatement.
        let (mut app, _wid, _sid, _ctx) = app_with_stub();
        let now = Instant::now();
        app.ripen_exceptions(now);
        seed(&mut app);
        app.ripen_exceptions(now + Duration::from_secs(1));
        assert!(
            app.messages.digest_deadline().is_none(),
            "an unchanged answer is not stated before the settle"
        );
        app.ripen_exceptions(now + crate::exceptions_host::RESUME_SETTLE + Duration::from_secs(1));
        settle(&mut app);
        assert!(keys(&app).is_empty(), "swept at the settle");

        // NEGATIVE CONTROL: a hold here past the mark keeps this tab's entry.
        let (mut app, wid, _sid, ctx) = app_with_stub();
        app.agents.long_holds_named = None;
        seed(&mut app);
        let t0 = 1_800_000_000;
        set_hold_clock(Some(t0));
        app.take_over(wid);
        set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
        let now = Instant::now();
        app.ripen_exceptions(now + LONG_HOLD + Duration::from_secs(1));
        set_hold_clock(None);
        let _ = app.settle_messages(
            now + LONG_HOLD + aterm_messages::need::RESOLVE_AFTER + Duration::from_secs(2),
        );
        let mine =
            crate::message_reporters::long_hold_entry(ctx.self_id.as_str(), "-", 30, None).key();
        assert_eq!(keys(&app), [mine], "this tab's entry, the earlier one gone");
    }

    /// Show Identity: a session with no identity opens a read-only, owner-private
    /// tab saying so; for a session that wears one, the markdown fences the
    /// `identities <name>` reply verbatim.
    #[test]
    fn show_identity_opens_a_read_only_tab_for_the_focused_session() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        app.open_session_identity_tab(wid);
        let path = app
            .last_menu_document
            .clone()
            .expect("a document was written");
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(text.starts_with("# Identity \u{2014} none\n"), "{text}");
        assert!(
            text.contains(&format!(
                "Session {} carries no agent identity",
                ctx.self_id.as_str()
            )),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the document is the owner's");
        }
        let _ = std::fs::remove_file(&path);

        let worn = super::identity_markdown(
            "s-abc",
            Some("worker"),
            Some("OK 2\nworker dir=/x sessions=1 agents=claude:present\nagent=claude\n"),
        );
        assert!(worn.starts_with("# Identity \u{2014} worker\n"));
        assert!(worn.ends_with(
            "```text\nOK 2\nworker dir=/x sessions=1 agents=claude:present\nagent=claude\n```\n"
        ));
    }

    /// Set Role…: the pin's editor, one field over — the commit writes `meta
    /// role`, the pill and the hovers name the tab by it, and the title pin
    /// is untouched.
    #[test]
    fn set_role_writes_the_role_through_the_pins_editor() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        assert!(app.begin_active_session_meta_edit(wid, MetaField::Role));
        let edit = app
            .windows
            .get(&wid)
            .and_then(|ws| ws.rename_edit.as_ref())
            .expect("an edit is live");
        assert_eq!(edit.field, MetaField::Role);
        assert_eq!(edit.session, sid);
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("manager".into()));
        app.commit_session_rename(wid, sid, "manager");
        let meta = ctx.meta.lock().unwrap();
        assert_eq!(meta.role.as_deref(), Some("manager"));
        assert_eq!(meta.user_title, None, "the title pin is untouched");
        drop(meta);
        assert_eq!(
            app.tab_label_of_session(sid),
            "manager",
            "the pill and the hovers name the tab by its role"
        );
        // The dispatch arm reaches the same editor.
        assert!(app.windows.get(&wid).unwrap().rename_edit.is_none());
        assert_eq!(
            crate::menu::MenuAction::SetRole.invoke_authority(),
            crate::menu::InvokeAuthority::WriteInput
        );
    }

    /// SHOW NEEDS IN WINDOWS (persisted as `[presence] rim`): off, the window
    /// draws no border and no pill while the session's need, its tab mark and
    /// the wire stay; each click flips the live bit, publishes it for the
    /// checkmark and queues exactly one durable write (headless has no lane,
    /// which is said); a config with `[presence] rim = false` is adopted on
    /// reload.
    #[test]
    fn show_needs_in_windows_gates_the_border_and_persists() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        ctx.meta.lock().unwrap().attention = Some("deploy?".into());
        app.refresh_presence_session(sid);
        let rim = |app: &App| app.presence_view(wid).map(|v| v.rim);
        assert_eq!(rim(&app), Some(crate::presence::Rim::Decide));
        assert!(app.presence_overlay(wid).is_some());

        // Switch off: no border, no pill; the need and the mark stand.
        app.user_toggle_needs_in_windows();
        assert!(!app.presence_rim_on());
        assert_eq!(rim(&app), Some(crate::presence::Rim::None));
        assert!(app.presence_pill(wid).is_none());
        assert!(app.presence_overlay(wid).is_none());
        assert_eq!(
            app.presence.slot(sid).map(|s| s.chip()),
            Some(crate::presence::ChipLevel::Wait),
            "the tab's mark stays"
        );
        assert_eq!(
            row(&app, wid, MenuAction::ToggleNeedsInWindows).checked,
            Some(false)
        );
        // Headless has no event-loop proxy: the durable write cannot be
        // queued and the notice says so; the live flip stands.
        assert!(app.has_live_message("Couldn't save"));

        // Back on: the border returns.
        app.user_toggle_needs_in_windows();
        assert!(app.presence_rim_on());
        assert_eq!(rim(&app), Some(crate::presence::Rim::Decide));

        // A reload carrying `[presence] rim = false` is adopted.
        app.config = aterm_toml::from_str::<crate::app_config::Config>("[presence]\nrim = false\n")
            .expect("parses");
        app.adopt_presence_config();
        assert!(!app.presence_rim_on());
        assert_eq!(rim(&app), Some(crate::presence::Rim::None));
        assert!(!app.config.presence_rim_enabled());
    }

    /// The CLI rows compose their argv and, headless, run nothing: no child,
    /// no sheet, no file. The reads are the survey (with `--fleet` for the
    /// fleet), naming no socket so every aterm on the Mac is read; only the
    /// bus's on/off ask first.
    #[test]
    fn the_cli_rows_compose_their_argv_and_run_nothing_headless() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        for verb in [
            CliVerb::ReadThisMac,
            CliVerb::ReadFleet,
            CliVerb::FleetPeers,
            CliVerb::BusStatus,
            CliVerb::BusOn,
            CliVerb::BusOff,
        ] {
            app.run_cli_to_tab(wid, verb);
            let plan = app.last_cli_plan.clone().expect("a plan");
            assert_eq!(plan.verb, verb);
            assert!(!plan.out.exists(), "headless writes nothing");
            let first = plan.argv.first().map(String::as_str);
            match verb {
                CliVerb::ReadThisMac | CliVerb::ReadFleet => {
                    assert_eq!(plan.argv[..2], ["ctl", "survey"]);
                    assert!(!plan.argv.contains(&"--sock".to_string()), "the whole Mac");
                    assert_eq!(
                        plan.argv.contains(&"--fleet".to_string()),
                        verb == CliVerb::ReadFleet
                    );
                }
                CliVerb::FleetPeers => assert_eq!(plan.argv, ["fleet", "peers"]),
                CliVerb::BusStatus | CliVerb::BusOn | CliVerb::BusOff => {
                    assert_eq!(first, Some("fabric"));
                }
                CliVerb::ReadPeer | CliVerb::DrivePeer => unreachable!("not a row's verb"),
            }
            assert_eq!(
                verb.confirmation().is_some(),
                matches!(verb, CliVerb::BusOn | CliVerb::BusOff),
                "{verb:?}"
            );
        }
        assert!(app.last_menu_document.is_none());
    }

    /// A COMMAND'S TAB FENCES ITS OUTPUT with more backticks than it holds,
    /// one block per stream that wrote anything, under the command as typed
    /// (a fleet Mac's read names its own argv); a command that did not run
    /// has no block.
    #[test]
    fn the_cli_tab_fences_its_output() {
        let plan = CliPlan::peer(
            ["ctl", "survey", "--host", "mac-17"]
                .map(String::from)
                .to_vec(),
            "mac-17".to_string(),
        );
        let text = cli_markdown(&plan, &Ok(Some(1)), "a ``` b\n", "");
        assert!(
            text.starts_with("# aterm ctl survey --host mac-17\n"),
            "{text}"
        );
        assert!(text.contains("````text\na ``` b\n````"), "{text}");
        assert_eq!(text.matches("````text").count(), 1);
        assert_eq!(
            cli_markdown(&plan, &Ok(Some(1)), "a ``` b\n", "e")
                .matches("text\n")
                .count(),
            2
        );
        assert!(!cli_markdown(&plan, &Err("x".to_string()), "", "").contains("```"));
    }

    /// A TAKE OVER ON ANOTHER MAC SAYS WHAT IS LEFT TO DO: a hold it placed
    /// names how to type into the session over the fleet and how to hand it
    /// back. NEGATIVE CONTROLS: a refused Take Over, and a Hand Back, say
    /// nothing of it.
    #[test]
    fn a_take_over_on_another_mac_says_how_to_hand_it_back() {
        let plan = |way: &str| {
            CliPlan::driven(
                ["ctl", "--host", "mac-17.local", "hold", "s-ab12", way]
                    .map(String::from)
                    .to_vec(),
                "Take Over on mac-17 \u{b7} s-ab12".to_string(),
            )
        };
        let took = cli_markdown(&plan("on"), &Ok(Some(0)), "OK hold=1\n", "");
        assert!(
            took.contains("aterm ctl --host mac-17.local hold s-ab12 off"),
            "{took}"
        );
        assert!(took.contains("@s-ab12 send"), "{took}");
        let refused = cli_markdown(&plan("on"), &Ok(Some(1)), "", "ERR denied\n");
        assert!(!refused.contains("hold s-ab12 off"), "{refused}");
        let back = cli_markdown(&plan("off"), &Ok(Some(0)), "OK hold=0\n", "");
        assert!(!back.contains("hold s-ab12 off`"), "{back}");
    }

    /// THE MENU BAR'S READS RAISE THEIR WINDOW (§3): a read the status item
    /// asked for (`CliPlan::raised`) opens its tab in the asking window and
    /// raises that window — a status item click never activates aterm — and
    /// Read Result does too, marking its done seen only once its tab is asked
    /// for, and so does a "To look at" line, which opens Settings ▸ Messages
    /// at its entry. NEGATIVE CONTROLS: the Agents rows' own read (aterm is
    /// already active) and the control socket's `app open` raise nothing; a
    /// Read Result whose file does not open raises nothing and leaves its
    /// done unseen.
    #[test]
    fn the_menu_bars_reads_raise_their_window_and_nothing_else_does() {
        use crate::native_app::AppKind;
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-status-read-raise")
            .tempdir()
            .expect("scratch dir");
        let file = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("# {name}\n")).unwrap();
            path
        };
        let mut app = App::headless_for_test();
        let wid = app.frontmost_window.expect("the test App's window");
        let _ = crate::app_tabs::take_raised_windows();

        let opened = crate::native_document_host::path_to_file_uri(&file("open.md")).unwrap();
        app.open_document_tab(AppKind::Markdown, &opened)
            .expect("app open opens its tab");
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

        let ended = |plan: CliPlan| CliTabOutcome {
            wid,
            plan,
            status: Ok(Some(0)),
        };
        let mut row = CliPlan::new(CliVerb::ReadThisMac);
        row.out = file("row.md");
        app.complete_cli_tab(ended(row));
        assert_eq!(
            crate::app_tabs::take_raised_windows(),
            Vec::new(),
            "an Agents row's read raises nothing"
        );
        let mut read = CliPlan::new(CliVerb::ReadThisMac).raised();
        read.out = file("read.md");
        app.complete_cli_tab(ended(read));
        assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);

        let result = |path| crate::status_item::ResultRef {
            session: 0,
            entered: 7,
            end_serial: None,
            path,
            label: "lint".to_string(),
        };
        app.read_status_result(&result(dir.path().join("missing.md")));
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());
        assert_eq!(app.done_seen.get(&0), None, "nothing opened: not seen");
        app.read_status_result(&result(file("result.md")));
        assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
        assert_eq!(app.done_seen.get(&0), Some(&7));

        let entry = app.post_message(aterm_messages::Message::new(
            aterm_messages::tags::CONFIG,
            aterm_messages::Severity::Warn,
            "to look at",
        ));
        assert!(app.show_status_exception(entry.raw()));
        assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
    }

    /// NEXT THAT NEEDS YOU walks the needs stuck first, then oldest first;
    /// pressed again it moves on and wraps; a cursor whose need resolved
    /// starts over at the most pressing. Nothing needed: nothing.
    #[test]
    fn next_need_walks_the_needs_stuck_first_and_drops_a_resolved_one() {
        use aterm_types::needs::{NeedWord, Severity};
        let stop = |word: NeedWord, since: i64, session: u64, key: u64| NeedStop {
            severity: word.severity(),
            since,
            label: String::new(),
            session,
            key,
        };
        let stops = [
            stop(NeedWord::Approval, 300, 1, 10),
            stop(NeedWord::Frozen, 500, 2, 20),
            stop(NeedWord::Ask, 100, 3, 30),
        ];
        assert_eq!(stops[1].severity, Severity::Stuck);
        let first = pick_next_need(&stops, None).expect("one");
        assert_eq!(first.session, 2, "stuck first, however young");
        let second = pick_next_need(&stops, Some((2, 20))).expect("two");
        assert_eq!(
            second.session, 3,
            "then the oldest decide, whatever its word"
        );
        let third = pick_next_need(&stops, Some((3, 30))).expect("three");
        assert_eq!(third.session, 1);
        assert_eq!(
            pick_next_need(&stops, Some((1, 10))).map(|s| s.session),
            Some(2)
        );
        // The cursor's need resolved (or moved to a new episode): start over.
        assert_eq!(
            pick_next_need(&stops, Some((2, 99))).map(|s| s.session),
            Some(2)
        );
        assert_eq!(pick_next_need(&[], None), None);

        // The App's walk reads the published verdicts and raises the tab.
        let (mut app, wid, sid, ctx) = app_with_stub();
        assert!(app.next_need().is_err(), "nothing needs you");
        let _ = ctx
            .timeline
            .lock()
            .unwrap()
            .set_need(crate::needs::Verdict {
                need: Some(crate::needs::Need {
                    word: NeedWord::Ask,
                    key: 7,
                    what: String::new(),
                    harness: None,
                }),
                ..crate::needs::Verdict::default()
            });
        assert_eq!(app.local_needs().len(), 1);
        assert_eq!(app.next_need(), Ok(()));
        assert_eq!(app.agents.next_need, Some((sid, 7)));
        let _ = wid;
    }

    /// ⇧⌘J's FIRST STOP IS ❯'S FIRST ROW (§4: "in the status item's order"):
    /// two decide needs, the older one in the later tab, and both lists
    /// start at the older one. NEGATIVE CONTROL: by the session's own order
    /// (the walk's old tie-break) the earlier tab would come first.
    #[test]
    fn next_need_starts_where_the_status_items_needs_start() {
        use aterm_types::needs::NeedWord;
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let second = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(second));
        let second_ctx = app.pool.get(second).expect("pooled").ctx.clone();
        for (ctx, key) in [(&first_ctx, 1), (&second_ctx, 2)] {
            let _ = ctx
                .timeline
                .lock()
                .unwrap()
                .set_need(crate::needs::Verdict {
                    need: Some(crate::needs::Need {
                        word: NeedWord::Ask,
                        key,
                        what: "approve?".to_string(),
                        harness: None,
                    }),
                    ..crate::needs::Verdict::default()
                });
        }
        // The herald first saw the later tab's need an hour before the
        // earlier tab's.
        let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
        for (session, at) in [(second, now - 3600), (first, now)] {
            let handle = app
                .store
                .read()
                .unwrap()
                .by_local(session)
                .cloned()
                .expect("registered");
            let row = app.status_session_row(&handle);
            let _ = app.presence.herald.observe(session, Some(&row), at);
        }
        assert!(
            first < second,
            "control: the session order picks the earlier tab"
        );
        let listed = app.status_glance().local_need_sessions();
        assert_eq!(listed, vec![second, first], "❯ lists the older first");
        assert_eq!(app.next_need(), Ok(()));
        assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(listed[0]));
        assert_eq!(app.next_need(), Ok(()));
        assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(listed[1]));
    }

    /// THE AGENT SECTION'S FACTS: a worker's task names its manager here
    /// (`Go to <manager>`) and is live (Stop Worker…); the manager lists the
    /// worker; the person's hold is read off the slot.
    #[test]
    fn the_agent_facts_name_the_manager_and_its_workers() {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        let worker = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(worker));
        let worker_ctx = app.pool.get(worker).expect("pooled").ctx.clone();
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
        app.on_task_changed(worker); // the wake's arm rebuilds the team table
        let of_worker = app.agent_menu_facts(worker);
        assert_eq!(
            of_worker.manager.as_ref().map(|(sid, _)| sid.clone()),
            Some(manager_ctx.self_id.clone())
        );
        assert!(of_worker.stoppable);
        assert!(of_worker.workers.is_empty());
        let of_manager = app.agent_menu_facts(manager);
        assert_eq!(of_manager.manager, None);
        assert!(!of_manager.stoppable, "no task of its own");
        assert_eq!(
            of_manager
                .workers
                .iter()
                .map(|(sid, _)| sid)
                .collect::<Vec<_>>(),
            [&worker_ctx.self_id]
        );
        assert!(!of_manager.person_hold);
        assert!(app.take_over_session(manager).is_ok());
        assert!(app.agent_menu_facts(manager).person_hold);
    }

    /// A BACKGROUND WORKER IS NAMED AS THE STRIP NAMES IT. A worker started
    /// in a background tab has no role, no title pin and no registry title
    /// (only the active tab's title is published there); its OSC title, its
    /// agent's spinner glyph dropped, is what names it in its manager's
    /// Workers rows, and the manager's OSC title names it in the worker's Go
    /// to row. A tab with no title at all reads its one-based position in
    /// its window. CONTROL: the registry carries neither title, so the
    /// labels come from the terminal's rung. NEGATIVE CONTROL: a title the
    /// person pinned (`⠦ deploy`) is theirs and stays whole.
    #[test]
    fn a_background_worker_is_named_by_its_osc_title() {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        let worker = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(worker));
        let untitled = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(untitled));
        app.switch_tab_in(wid, 0);
        let osc = |app: &App, session: u64, title: &str| {
            let term = app.pool.get(session).expect("pooled").term.clone();
            crate::term_lock(&term).process(format!("\x1b]2;{title}\x07").as_bytes());
        };
        osc(&app, manager, "\u{2733} lab-sweep");
        osc(&app, worker, "\u{25d0} bench");
        for w in [worker, untitled] {
            let ctx = app.pool.get(w).expect("pooled").ctx.clone();
            let mark = TaskMark {
                from: Some(manager_ctx.self_id.as_str().to_string()),
                ..TaskMark::new(TaskState::Working)
            };
            mark_session_task(&ctx, w, &mark).expect("unfenced");
            app.on_task_changed(w);
        }
        let registry = |session: u64| {
            app.store
                .read()
                .unwrap()
                .by_local(session)
                .map(|h| h.title.clone())
        };
        assert_ne!(
            registry(worker).as_deref(),
            Some("bench"),
            "not the registry's"
        );
        assert_ne!(registry(manager).as_deref(), Some("lab-sweep"));

        assert_eq!(app.tab_label_of_session(worker), "bench");
        assert_eq!(app.tab_label_of_session(manager), "lab-sweep");
        // The untitled tab is the window's last.
        let last = format!("tab {}", app.windows[&wid].tab_set.tabs().len());
        assert_eq!(app.tab_label_of_session(untitled), last);
        let of_worker = app.agent_menu_facts(worker);
        assert_eq!(
            of_worker.manager.map(|(_, label)| label).as_deref(),
            Some("lab-sweep")
        );
        let rows = app.agent_menu_facts(manager).workers;
        assert_eq!(rows.len(), 2);
        assert!(rows[0].1.contains(" bench "), "{rows:?}");
        assert!(rows[1].1.contains(&format!(" {last} ")), "{rows:?}");
        // A frame of any spinner family is dropped (a quadrant circle here),
        // never kept as one frozen frame of the animation.
        osc(&app, worker, "\u{25f4} bench");
        assert_eq!(app.tab_label_of_session(worker), "bench");
        // NEGATIVE CONTROL: the person's own title pin is theirs, whole: a
        // leading frame-like glyph in it stays.
        let ctx = app.pool.get(worker).expect("pooled").ctx.clone();
        let pinned = ctx
            .meta
            .lock()
            .unwrap()
            .set("title", Some("\u{2826} deploy".to_string()));
        assert_eq!(pinned, Some(true), "the pin stored");
        assert_eq!(app.tab_label_of_session(worker), "\u{2826} deploy");
        // A role outranks the terminal's rungs.
        app.pool
            .get(worker)
            .expect("pooled")
            .ctx
            .meta
            .lock()
            .unwrap()
            .role = Some("fmt".to_string());
        assert_eq!(app.tab_label_of_session(worker), "fmt");
    }

    /// `hwkey` CANNOT PRESS THE PERSON'S ACTS THROUGH AN OPEN CARD. Its keys
    /// are real NSEvents on the person's unmarked key path, so its hop
    /// refuses while the palette, Overview or a tab menu is open on the
    /// front window. CONTROL: with none open it answers the window's number
    /// (`0` headless). And a control client's tab-menu chord under a hold
    /// opens no card, while the person's would and an unheld instance's
    /// would.
    #[test]
    fn hwkey_is_refused_while_a_gateway_card_is_open() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.tab_strip_rows = 1;
        assert_eq!(app.hwkey_target(), Ok((0, false)), "no card open");
        app.palette_enter();
        assert!(app.hwkey_target().is_err(), "the palette");
        app.palette_exit();
        assert!(app.open_overview().is_ok());
        assert!(app.hwkey_target().is_err(), "the Overview");
        app.overview_exit(wid);
        assert!(app.open_active_tab_context_menu(wid));
        assert!(app.hwkey_target().is_err(), "a tab menu");
        app.close_tab_menu(wid);
        assert_eq!(app.hwkey_target(), Ok((0, false)), "closed again");

        assert!(!app.with_gesture_source(true, |app| app.controller_card_refused()));
        assert!(app.take_over_session(sid).is_ok());
        assert!(app.with_gesture_source(true, |app| app.controller_card_refused()));
        assert!(
            !app.controller_card_refused(),
            "the person's own chord opens it"
        );
    }

    /// THE IN-GRID CARD'S AGENT ROWS ACT: on a worker's tab menu, `Go to
    /// <manager>` (a session-argument row) raises the manager's tab through
    /// the dispatcher the macOS relay reaches. CONTROL: before the choice the
    /// worker's tab is in front.
    #[test]
    fn the_cards_go_to_manager_row_raises_the_manager() {
        use crate::session_chrome::{ConnVerb, TabMenuEntry};
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let (mut app, wid, _manager, manager_ctx) = app_with_stub();
        app.tab_strip_rows = 1;
        let manager_tab = app.windows[&wid].tab_set.active_index();
        let worker = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(worker));
        let worker_ctx = app.pool.get(worker).expect("pooled").ctx.clone();
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
        app.on_task_changed(worker);
        assert_ne!(
            app.windows[&wid].tab_set.active_index(),
            manager_tab,
            "the worker's tab is in front"
        );
        assert!(app.open_active_tab_context_menu(wid));
        let go = app.windows[&wid]
            .tab_menu
            .as_ref()
            .expect("the menu is open")
            .entries
            .iter()
            .position(|e| {
                matches!(
                    e,
                    TabMenuEntry::ConnectionAction { peer_sid, verb: ConnVerb::Show, .. }
                        if *peer_sid == manager_ctx.self_id
                )
            })
            .expect("the worker's menu names its manager");
        app.activate_tab_menu_entry(wid, go);
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            manager_tab,
            "the manager's tab is in front"
        );
    }
}
