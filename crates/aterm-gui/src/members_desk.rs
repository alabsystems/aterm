// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This member's desk for the Macs asking to join ALab (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! F10, §3.3.1). The fleet port's `ask` op hands each asking Mac's stream to the desk
//! ([`ask_handler`]); the band shows one row per request the desk heard ("<name> wants to join
//! ALab") with [Allow] and [Ignore]; Allow shows a phrase in a native sheet on this Mac's own
//! display, for a person to type on the asking Mac. Allow takes only a person's own press
//! (§3.3.5): `notice act`, a fleet drive and a control client's gesture are refused it. The
//! phrase goes to that sheet alone, never to a terminal, a log, `aterm ctl`, a message or a fleet
//! reply.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_messages::need::{Ask, Reason, StandingWhy, Then};
use aterm_messages::{Intent, Message, Severity, tags};
use aterm_secrets::members::desk::{self, COLLAPSE_AT, Desk, Local, Row, RowState};
use aterm_secrets::members::{self, Allowed, PhraseState};
use aterm_types::MutexExt;

use crate::App;
use crate::messages_host::wall_stamp_now;

/// The key every row of a Mac asking to join starts with.
pub(crate) const KEY_PREFIX: &str = "members.ask.";

/// The one row that stands for [`COLLAPSE_AT`] or more asking Macs.
const MANY_KEY: &str = "members.ask.many";

/// How often the lane looks at the desk for a change the band shows.
const READ_EVERY: Duration = Duration::from_secs(2);

/// How often the lane reads this Mac's Commons state again: a new roster binding lets it allow,
/// and a Mac that left the vault drops its desk.
const LOCAL_EVERY: Duration = Duration::from_secs(30);

/// What `allow` and `invite` say in place of the phrase, `lasts` being how long the phrase shown
/// works: an Allow's phrase and an invite's do not last as long.
pub(crate) fn on_screen(lasts: Duration) -> String {
    format!("the phrase is on this Mac's screen, for {}", minutes(lasts))
}

/// `lasts` in whole minutes, rounded up.
fn minutes(lasts: Duration) -> String {
    match lasts.as_secs().div_ceil(60) {
        1 => "1 minute".to_owned(),
        n => format!("{n} minutes"),
    }
}

/// What `allow` and `invite` say on a Mac no person reads.
pub(crate) const NO_SCREEN: &str = "press Allow on a Mac with a screen";

/// Why a press that is not a person's own is refused.
const PERSON_ONLY: &str = "Allow is a person's own press on this Mac's screen";

/// A desk this process serves: the fleet port's ask handler and the window share it.
pub(crate) type Shared = Arc<Mutex<Desk>>;

/// Where a desk is kept: empty until this Mac is a member, and empty again once it left.
type Slot = Mutex<Option<Shared>>;

/// The desk of a windowed instance, made once this Mac holds Commons state and dropped once it
/// left the vault ([`spawn_lane`]).
static LIVE: Slot = Mutex::new(None);

/// Set when the desk changed in a way the band shows; the next park reads it.
static CHANGED: AtomicBool = AtomicBool::new(false);

/// The fleet port's `ask` handler: the asking Mac's stream goes to [`LIVE`]'s desk.
pub(crate) fn ask_handler() -> aterm_net::fleet::AskHandler {
    Arc::new(
        |stream: &mut dyn aterm_net::fleet::Duplex, peer: SocketAddr| {
            let desk = LIVE.lock_or_recover().clone();
            serve_ask(desk.as_ref(), stream, peer);
            Ok(())
        },
    )
}

/// Serve one `ask` exchange from `peer` on `desk`, or refuse it while this Mac has none. A run
/// that let its Mac in posts that Mac's request with its `Allowed` to this Mac's vault server,
/// off this thread, so an admin's approver records the admission whenever one is reachable.
pub(crate) fn serve_ask(
    desk: Option<&Shared>,
    stream: &mut dyn aterm_net::fleet::Duplex,
    peer: SocketAddr,
) {
    let Some(desk) = desk else {
        let _ = members::refuse(stream, "this Mac is not an ALab member yet");
        return;
    };
    let bundle = |handing: &desk::Handing<'_>| {
        let paths = aterm_secrets::client::Paths::live()?;
        aterm_secrets::client::hand_over(&paths, handing)
    };
    let served = desk::serve(desk, stream, peer.ip(), &bundle);
    CHANGED.store(true, Ordering::Release);
    match served {
        Ok(Some(let_in)) => {
            aterm_log::info!(
                "members: let {} in from {peer}{}",
                let_in.allowed.name(),
                if let_in.logged {
                    " by the membership log"
                } else {
                    ""
                }
            );
            if let Some(note) = &let_in.note {
                aterm_log::warn!(
                    "members: {} took F10's files: {note}",
                    let_in.allowed.name()
                );
            }
            post_allowed(let_in.allowed, let_in.logged);
        }
        Ok(None) => {}
        Err(error) => aterm_log::info!("members: an ask from {peer} ended: {error}"),
    }
}

/// The window's end of a relayed `ask` ([`crate::fleet_listen::node_serve`]): once the node
/// job writes `go` (after the relay's `OK`, so the desk's greeting cannot overtake it), the
/// exchange runs on [`LIVE`]'s desk, off the relay's accept thread. Whether its thread started.
pub(crate) fn attach_relayed(stream: aterm_uds::CtlStream, peer: SocketAddr) -> bool {
    let desk = LIVE.lock_or_recover().clone();
    attach_relayed_to(desk, stream, peer)
}

/// [`attach_relayed`] on `desk`.
pub(crate) fn attach_relayed_to(
    desk: Option<Shared>,
    mut stream: aterm_uds::CtlStream,
    peer: SocketAddr,
) -> bool {
    crate::qos::spawn_lane_thread(
        "aterm-members-ask",
        crate::qos::Role::Responsive,
        move || {
            let _ = stream.set_read_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN));
            let _ = stream.set_write_timeout(Some(aterm_net::fleet::HANDSHAKE_WITHIN));
            let mut go = [0u8; 3];
            if std::io::Read::read_exact(&mut stream, &mut go).is_ok() && &go == b"go\n" {
                serve_ask(desk.as_ref(), &mut stream, peer);
            }
            aterm_uds::hangup::hang_up(&stream);
        },
    )
    .is_ok()
}

/// Post a run's records (when its bundle carried the log) and its `Allowed` to this Mac's vault
/// servers, off the desk's thread. The records let the new Mac in on every Mac they reach; the
/// `Allowed` is how an admin's approver still enrolls it in Commons control and the fleet until
/// F17 and F18, and, for a vault with no log, how it records the admission.
fn post_allowed(allowed: Allowed, logged: bool) {
    let posted = crate::qos::spawn_lane_thread(
        "aterm-members-post",
        crate::qos::Role::Background,
        move || {
            let paths = aterm_secrets::client::Paths::live();
            if logged
                && let Ok(live) = &paths
                && let Err(error) = aterm_secrets::client::post_records_to_servers(live)
            {
                aterm_log::warn!(
                    "members: posting {}'s Admit to the vault servers failed: {error}",
                    allowed.name()
                );
            }
            let posted =
                paths.and_then(|paths| aterm_secrets::client::post_allowed(&paths, &allowed));
            if let Err(error) = posted {
                aterm_log::warn!(
                    "members: posting {}'s Allow to the vault server failed: {error}",
                    allowed.name()
                );
            }
        },
    );
    if let Err(error) = posted {
        aterm_log::warn!("members: the Allow's post did not start: {error}");
    }
}

/// Start the desk of a windowed macOS instance, off the event loop: read this Mac's Commons
/// state every [`LOCAL_EVERY`] ([`follow_local`]: the desk made once it is a member, dropped
/// once it left), and mark each change the band shows ([`App::follow_member_requests`]).
/// Whether the thread started.
pub(crate) fn spawn_lane(headless: bool) -> bool {
    if headless || !cfg!(target_os = "macos") {
        return false;
    }
    let spawned =
        crate::qos::spawn_lane_thread("aterm-members-desk", crate::qos::Role::Background, || {
            let Ok(paths) = aterm_secrets::client::Paths::live() else {
                return;
            };
            let mut looked: Option<std::time::Instant> = None;
            let mut before: Option<Snapshot> = None;
            loop {
                if looked.is_none_or(|at| at.elapsed() >= LOCAL_EVERY) {
                    looked = Some(std::time::Instant::now());
                    follow_local(&LIVE, &paths);
                }
                // A desk dropped reads as a change too: the window ends its rows.
                let desk = LIVE.lock_or_recover().clone();
                let now = desk
                    .as_ref()
                    .map(|desk| Snapshot::of(&desk.lock_or_recover(), aterm_time::Instant::now()));
                if before != now {
                    CHANGED.store(true, Ordering::Release);
                    before = now;
                }
                std::thread::sleep(READ_EVERY);
            }
        });
    match spawned {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("members: the join rows did not start: {error}");
            false
        }
    }
}

/// Read this Mac's Commons state under `paths` into `slot`: a member's desk made, or refreshed
/// keeping what it heard; none once this Mac left the vault ([`left`]), so its rows end, an
/// `ask` is refused and Allow finds no desk. A vault mid-apply and unreadable state leave `slot`
/// as it was.
fn follow_local(slot: &Slot, paths: &aterm_secrets::client::Paths) {
    match Local::of(paths) {
        Ok(Some(local)) => {
            let mut slot = slot.lock_or_recover();
            if let Some(desk) = slot.as_ref() {
                desk.lock_or_recover().refresh(local);
            } else {
                *slot = Some(Arc::new(Mutex::new(Desk::new(local))));
            }
        }
        Ok(None) if left(paths) => *slot.lock_or_recover() = None,
        Ok(None) => {}
        Err(error) => {
            aterm_log::warn!("members: this Mac's Commons state: {error}");
        }
    }
}

/// Whether this Mac's vault says it is no member, and not just for a moment: none configured,
/// not admitted, revoked, no Commons control grant, or no identity of its own. A busy vault, an
/// apply still pending or incomplete and every other read leave the desk as it was.
fn left(paths: &aterm_secrets::client::Paths) -> bool {
    use aterm_secrets::commons::{Observation, Unavailable};
    matches!(
        aterm_secrets::commons::observe(paths),
        Ok(Observation::Unavailable(
            Unavailable::Unconfigured
                | Unavailable::NotAdmitted
                | Unavailable::Revoked
                | Unavailable::MissingIdentity
                | Unavailable::ControlNotVisible
        ))
    )
}

/// What the band shows of a desk: the requests still asking, and whether this Mac can allow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    rows: Vec<Row>,
    can_allow: bool,
}

impl Snapshot {
    fn of(desk: &Desk, now: aterm_time::Instant) -> Self {
        Self {
            rows: desk
                .rows(now)
                .into_iter()
                .filter(|row| matches!(row.state, RowState::Asking | RowState::Allowed(_)))
                .collect(),
            can_allow: desk.local().can_allow(),
        }
    }

    /// The band's rows: one per asking Mac, or one for them all from [`COLLAPSE_AT`] on.
    pub(crate) fn notices(&self) -> Vec<Message> {
        if self.rows.len() >= COLLAPSE_AT {
            return vec![many(self.rows.len())];
        }
        self.rows
            .iter()
            .map(|row| notice(row, self.can_allow))
            .collect()
    }
}

/// The row of one Mac asking to join: its name, model and address, [Allow] where this Mac can
/// allow and [Ignore]. Allow is never the row's default ([`Intent::never_default`]).
pub(crate) fn notice(row: &Row, can_allow: bool) -> Message {
    let title = format!(
        "{} wants to join ALab ({}, {})",
        row.name, row.model, row.from
    );
    let message = Message::new(tags::PRIVACY, Severity::Warn, title);
    let (message, then) = if can_allow {
        (
            message.action(Intent::AllowJoin { request: row.id }),
            format!(
                "Press Allow, then type the phrase this Mac shows on {}",
                row.name
            ),
        )
    } else {
        (
            message.sentence("this Mac cannot allow yet: it has no Commons binding"),
            format!(
                "Press Allow on a member that can, then type its phrase on {}",
                row.name
            ),
        )
    };
    message
        .action(Intent::IgnoreJoin { request: row.id })
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new(format!(
                "only a person at a member lets {} into ALab, and it shows code {}",
                row.name, row.code
            ))),
            Then::step(then),
        ))
        .key(&format!("{KEY_PREFIX}{}", row.id))
}

/// The one row for `count` asking Macs: no Allow, which needs a name.
fn many(count: usize) -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Warn,
        format!("{count} Macs want to join ALab"),
    )
    .sentence("aterm members requests lists them; aterm members allow <name> allows one")
    .key(MANY_KEY)
}

/// Where a phrase can show: this Mac's own display, none, or a test's record.
pub(crate) enum Sheets {
    /// A windowed macOS instance: a sheet on the front window.
    Screen,
    /// A headless instance, which no person reads.
    None,
    /// The text of each sheet a test asked for.
    #[cfg(test)]
    Recorded(Vec<String>),
    /// A test's screen, on which no sheet opens.
    #[cfg(test)]
    Refused,
}

/// The window's side of the desk: where it reads the desk ([`LIVE`], or a test's slot), where
/// its phrases show, and the sheet showing one now.
pub(crate) struct Members {
    slot: &'static Slot,
    sheets: Sheets,
    shown: Option<Shown>,
}

/// The phrase on screen now, and its sheet.
struct Shown {
    phrase: aterm_secrets::pake::PhraseId,
    #[cfg(target_os = "macos")]
    _sheet: Option<native::Sheet>,
}

impl Members {
    /// The window's side for an instance: its phrases show on screen unless it is headless.
    pub(crate) const fn new(headless: bool) -> Self {
        Self {
            slot: &LIVE,
            sheets: if headless || !cfg!(target_os = "macos") {
                Sheets::None
            } else {
                Sheets::Screen
            },
            shown: None,
        }
    }

    /// A test's side: `desk` in a slot of its own, and sheets recorded.
    #[cfg(test)]
    pub(crate) fn for_test(desk: Shared) -> Self {
        Self {
            slot: Box::leak(Box::new(Mutex::new(Some(desk)))),
            sheets: Sheets::Recorded(Vec::new()),
            shown: None,
        }
    }

    /// The text of each sheet shown so far (a test's record).
    #[cfg(test)]
    pub(crate) fn recorded(&self) -> &[String] {
        match &self.sheets {
            Sheets::Recorded(shown) => shown,
            _ => &[],
        }
    }

    fn desk(&self) -> Option<Shared> {
        self.slot.lock_or_recover().clone()
    }
}

impl App {
    /// The rows of the Macs asking to join, once the desk changed, and the phrase sheet closed
    /// once its phrase was used, burned or expired.
    pub(crate) fn follow_member_requests(&mut self) {
        if CHANGED.swap(false, Ordering::AcqRel) {
            self.refresh_member_rows();
        }
        self.close_spent_phrase_sheet();
    }

    /// Post, replace and end the rows so they match the desk; with no desk (this Mac is no
    /// member, or it left the vault) every row ends.
    pub(crate) fn refresh_member_rows(&mut self) {
        let wanted = match self.members.desk() {
            Some(desk) => {
                Snapshot::of(&desk.lock_or_recover(), aterm_time::Instant::now()).notices()
            }
            None => Vec::new(),
        };
        let now = std::time::Instant::now();
        let shown: Vec<(String, aterm_messages::MessageId)> = self
            .messages
            .live_rows()
            .filter_map(|live| live.msg.key.clone().map(|key| (key, live.id)))
            .filter(|(key, _)| key.starts_with(KEY_PREFIX))
            .collect();
        for (key, id) in shown {
            if !wanted
                .iter()
                .any(|message| message.key.as_deref() == Some(&key))
            {
                self.messages.withdraw(id, now);
            }
        }
        for message in wanted {
            let message = message.normalized();
            let same = message
                .key
                .as_deref()
                .and_then(|key| self.messages.live_by_key(key))
                .is_some_and(|live| {
                    live.msg.title == message.title
                        && live.msg.actions == message.actions
                        && live.msg.detail == message.detail
                });
            if !same {
                self.post_unsynced(message, wall_stamp_now());
            }
        }
        self.sync_messages();
    }

    fn close_spent_phrase_sheet(&mut self) {
        let Some(phrase) = self.members.shown.as_ref().map(|shown| shown.phrase) else {
            return;
        };
        let live = self.members.desk().is_some_and(|desk| {
            let desk = desk.lock_or_recover();
            matches!(
                desk.phrase_state(phrase),
                Some(PhraseState::Live | PhraseState::Running)
            ) && desk
                .expires(phrase)
                .is_some_and(|at| aterm_time::Instant::now() < at)
        });
        if !live {
            self.members.shown = None;
        }
    }

    /// A person's press of Allow on request `request`: its phrase in a sheet on this Mac's
    /// screen. A control client's gesture is refused.
    ///
    /// # Errors
    /// Why nothing shows: not a person's press, no screen, or the desk refused.
    pub(crate) fn allow_member_request(&mut self, request: u64) -> Result<(), String> {
        if self.person_gesture_refused() {
            return Err(PERSON_ONLY.to_owned());
        }
        self.show_member_phrase(Some(request)).map(|_| ())
    }

    /// Ignore request `request` on this Mac: its row ends.
    pub(crate) fn ignore_member_request(&mut self, request: u64) -> bool {
        let Some(desk) = self.members.desk() else {
            return false;
        };
        let ignored = desk.lock_or_recover().ignore(request).is_ok();
        self.refresh_member_rows();
        self.close_spent_phrase_sheet();
        ignored
    }

    /// A phrase for `request` (Allow), or for whichever Mac types it first (invite), shown in a
    /// sheet on this Mac's screen and nowhere else. A phrase whose sheet does not open is burned
    /// at once: nobody saw it.
    fn show_member_phrase(&mut self, request: Option<u64>) -> Result<Duration, String> {
        let desk = self
            .members
            .desk()
            .ok_or("this Mac is not an ALab member yet")?;
        #[cfg(target_os = "macos")]
        let parent = match self.members.sheets {
            Sheets::Screen => Some(
                self.phrase_sheet_parent()
                    .ok_or("open an aterm window on this Mac's screen, then press Allow")?,
            ),
            _ => None,
        };
        if matches!(self.members.sheets, Sheets::None) {
            return Err(NO_SCREEN.to_owned());
        }
        let now = aterm_time::Instant::now();
        let (phrase, text, lasts) = {
            let mut desk = desk.lock_or_recover();
            let phrase = match request {
                Some(id) => desk.allow(id, now),
                None => desk.invite(now),
            }
            .map_err(|error| error.to_string())?;
            let words = desk
                .words(phrase, now)
                .ok_or("the phrase ended before it showed")?
                .join(" ");
            let lasts = desk
                .expires(phrase)
                .map_or(Duration::ZERO, |end| end.saturating_duration_since(now));
            let name = request.and_then(|id| {
                desk.rows(now)
                    .into_iter()
                    .find(|row| row.id == id)
                    .map(|row| row.name)
            });
            let text = match name {
                Some(name) => format!("Type {words} on {name}."),
                None => format!("Type {words} on the Mac that joins ALab."),
            };
            (phrase, text, lasts)
        };
        let informative = format!(
            "The phrase works once, for the next {}. Read it only to the person at that Mac.",
            minutes(lasts)
        );
        let shown: Result<(), String> = match &mut self.members.sheets {
            Sheets::None => Err(NO_SCREEN.to_owned()),
            #[cfg(test)]
            Sheets::Recorded(shown) => {
                shown.push(text);
                self.members.shown = Some(Shown {
                    phrase,
                    #[cfg(target_os = "macos")]
                    _sheet: None,
                });
                Ok(())
            }
            #[cfg(test)]
            Sheets::Refused => Err("the phrase sheet did not open".to_owned()),
            Sheets::Screen => {
                #[cfg(target_os = "macos")]
                {
                    match parent.and_then(|parent| native::show(parent, &text, &informative)) {
                        Some(sheet) => {
                            self.members.shown = Some(Shown {
                                phrase,
                                _sheet: Some(sheet),
                            });
                            Ok(())
                        }
                        None => Err("the phrase sheet did not open".to_owned()),
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (phrase, text, informative);
                    Err(NO_SCREEN.to_owned())
                }
            }
        };
        if let Err(why) = shown {
            // Nobody saw this phrase: burn it now, so it neither waits out its minutes as a live
            // secret nor tells the asking Mac it was allowed.
            desk.lock_or_recover().withdraw(phrase);
            return Err(why);
        }
        CHANGED.store(true, Ordering::Release);
        Ok(lasts)
    }

    #[cfg(target_os = "macos")]
    fn phrase_sheet_parent(&self) -> Option<aterm_objc::Obj> {
        let wid = self.frontmost_window?;
        self.windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|w| crate::platform::ns_window_of(w))
            .filter(|id| !id.is_null())
            // SAFETY: a live NSWindow borrowed on the main thread; `-window` returned +0,
            // retained into an owning `Obj` for the sheet.
            .and_then(|id| unsafe { aterm_objc::Obj::retain(id) })
    }

    /// `aterm ctl members …`: one reply, its lines ending in a newline. `allow` and `invite`
    /// answer [`on_screen`] in place of the phrase.
    pub(crate) fn take_members(&mut self, asked: &Asked) -> String {
        let Some(desk) = self.members.desk() else {
            return "ERR members: this Mac is not an ALab member yet\n".to_owned();
        };
        let now = aterm_time::Instant::now();
        let named = |name: &str| -> Result<u64, String> {
            let rows: Vec<Row> = Snapshot::of(&desk.lock_or_recover(), now)
                .rows
                .into_iter()
                .filter(|row| row.name == name || row.code.eq_ignore_ascii_case(name))
                .collect();
            match rows.as_slice() {
                [row] => Ok(row.id),
                [] => Err(format!("no Mac asks as {name} on this Mac")),
                _ => Err(format!(
                    "{} Macs ask as {name}: name one by its code ({})",
                    rows.len(),
                    rows.iter()
                        .map(|row| row.code.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        };
        let done = match asked {
            Asked::Requests => {
                let rows = Snapshot::of(&desk.lock_or_recover(), now).rows;
                let mut reply = format!("OK {}\n", rows.len());
                for row in rows {
                    reply.push_str(&format!(
                        "request name={} model={} from={} code={} state={}\n",
                        crate::control::pct_encode(&row.name),
                        crate::control::pct_encode(&row.model),
                        row.from,
                        row.code,
                        if matches!(row.state, RowState::Allowed(_)) {
                            "allowed"
                        } else {
                            "asking"
                        }
                    ));
                }
                return reply;
            }
            Asked::Allow(name) => named(name)
                .and_then(|id| self.show_member_phrase(Some(id)))
                .map(Some),
            Asked::Invite => self.show_member_phrase(None).map(Some),
            Asked::Ignore(name) => named(name).map(|id| {
                self.ignore_member_request(id);
                None
            }),
        };
        match done {
            Ok(Some(lasts)) => format!("OK {}\n", on_screen(lasts)),
            Ok(None) => "OK ignored\n".to_owned(),
            Err(why) => format!("ERR members: {why}\n"),
        }
    }
}

/// One `aterm ctl members` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// `requests`: the Macs asking that this Mac hears.
    Requests,
    /// `allow <name|code>`: its phrase on this Mac's screen.
    Allow(String),
    /// `invite`: a phrase for whichever Mac types it first.
    Invite,
    /// `ignore <name|code>`: hide its row here.
    Ignore(String),
}

/// The usage line every malformed `members` form answers.
pub(crate) const USAGE: &str =
    "ERR usage: members requests | allow <name> | invite | ignore <name>\n";

impl Asked {
    /// Parse `requests`, `allow <name>`, `invite` or `ignore <name>`.
    ///
    /// # Errors
    /// [`USAGE`].
    pub(crate) fn parse(rest: &str) -> Result<Self, &'static str> {
        let words: Vec<&str> = rest.split_whitespace().collect();
        match words.as_slice() {
            ["requests"] => Ok(Self::Requests),
            ["allow", name] => Ok(Self::Allow((*name).to_owned())),
            ["invite"] => Ok(Self::Invite),
            ["ignore", name] => Ok(Self::Ignore((*name).to_owned())),
            _ => Err(USAGE),
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use aterm_objc::{Id, Obj, RcBlock, Sel, sel};

    use crate::appkit::{self, MainThread};

    /// The phrase sheet on one window, ended when dropped.
    pub(super) struct Sheet {
        panel: Obj,
        parent: Obj,
        _alert: Obj,
    }

    impl Drop for Sheet {
        fn drop(&mut self) {
            // SAFETY: the panel and parent are retained `Obj`s on the main thread, where the App
            // that holds this sheet drops it; `-endSheet:returnCode:` ends only a sheet still
            // attached to that parent.
            unsafe {
                if appkit::send_id(self.panel.id(), sel!(sheetParent)) == self.parent.id() {
                    appkit::send_v_id_isize(
                        self.parent.id(),
                        sel!(endSheet:returnCode:),
                        self.panel.id(),
                        appkit::consts::NS_ALERT_FIRST_BUTTON_RETURN,
                    );
                }
            }
        }
    }

    /// Show `text` and `informative` in a sheet on `parent` with one Done button. `None` when
    /// this is not the main thread or AppKit refused a step.
    pub(super) fn show(parent: Obj, text: &str, informative: &str) -> Option<Sheet> {
        let mtm = MainThread::new()?;
        let (message, informative, done) = (
            appkit::nsstring(text)?,
            appkit::nsstring(informative)?,
            appkit::nsstring("Done")?,
        );
        // SAFETY: AppKit invokes the block as `void(^)(NSModalResponse)`; the `(isize)->()`
        // prototype matches (`NSModalResponse` is `NSInteger`), and nothing unwinds out of it.
        let handler = unsafe { RcBlock::new1(|_: isize| {}) }?;
        // An alert whose icon draws with no IconServices call on the main thread.
        let alert = crate::app_icon::alert(mtm)?;
        // SAFETY: standard NSAlert setters on the main thread; text setters copy,
        // `-addButtonWithTitle:` returns +0, `-window` returns +0 retained into an `Obj`, and
        // `-beginSheetModalForWindow:completionHandler:` copies the block.
        unsafe {
            appkit::send_v_id(alert.id(), sel!(setMessageText:), message.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), informative.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), done.id());
            let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)))?;
            let begin_sheet: unsafe extern "C-unwind" fn(Id, Sel, Id, aterm_objc::BlockPtr) =
                aterm_objc::checked_msg();
            begin_sheet(
                alert.id(),
                sel!(beginSheetModalForWindow:completionHandler:),
                parent.id(),
                handler.as_block_ptr(),
            );
            Some(Sheet {
                panel,
                parent,
                _alert: alert,
            })
        }
    }
}

#[cfg(test)]
#[path = "members_desk_tests.rs"]
mod tests;
