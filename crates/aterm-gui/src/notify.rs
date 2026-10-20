// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native desktop notification DELIVERY for OSC 9 / OSC 99 / OSC 777.
//!
//! The engine (`aterm-core`) already PARSES these escapes and fires
//! `set_notification_callback` (OSC 9 / 777, a plain body string) and
//! `set_advanced_notification_callback` (OSC 99 / kitty, a structured
//! [`Notification`] with title + body), gated by the host's
//! `authorize_notifications`. This module is the GUI side that turns those
//! callbacks into real macOS / Windows / Linux notifications, with two hard
//! rules borrowed from the OSC 52 clipboard path:
//!
//! 1. **Never block the engine.** The callbacks fire on a tab's PTY reader
//!    thread, under the `Terminal` lock. They MUST NOT spawn a subprocess
//!    there. Instead each callback does a single lock-free, NON-BLOCKING
//!    `mpsc::SyncSender::try_send` of a [`NotifyMsg`] onto a BOUNDED channel (cap
//!    [`NOTIFY_QUEUE_CAP`]) — DROPPING the message when the queue is full, so a
//!    notification flood can never grow the queue unbounded or block the engine;
//!    a dedicated delivery thread ([`spawn_delivery`]) owns the receiver and runs
//!    the (blocking) notifier subprocess off the hot path.
//!
//! 2. **Untrusted content.** A notification body is program output — over SSH it
//!    is fully attacker-controlled. The osascript fallback builds an AppleScript
//!    string literal, so the body is run through [`applescript_escape`] (and
//!    every argument is passed as a real argv entry, never a shell string) to
//!    foreclose AppleScript / shell injection. `terminal-notifier` takes argv
//!    directly, so no escaping is needed on that path. The Windows path writes
//!    title/body into the fixed-size `NOTIFYICONDATAW` WCHAR fields (no
//!    interpreter anywhere), so [`fold_to_utf16`] only folds control bytes and
//!    truncates to the field capacity. The Linux path is a D-Bus call (no
//!    subprocess, no shell); its body is MARKUP to a server that advertises
//!    `body-markup` (GNOME, KDE), so [`markup_escape`] escapes `&`, `<` and `>`
//!    there and folds control bytes.
//!
//! **Linux.** `org.freedesktop.Notifications.Notify` on the session bus, through
//! the first-party `aterm-dbus` client, from the delivery thread: one lazily
//! opened connection, a bounded call, every failure swallowed (no session bus,
//! no notification server). A HEADLESS process delivers nothing — it presents no
//! OS UI, and a test's or an agent's private instance often still carries the
//! person's real session-bus address. Serious mode is the standard
//! `suppress-sound` hint at normal urgency, never `urgency = low`, which GNOME
//! Shell shows with no banner at all.
//!
//! **Focus-aware suppression.** The delivery thread reads a shared SUPPRESSION SET
//! the main (UI) thread keeps current: the active-tab focused-pane session id of
//! EVERY focused window. A notification is suppressed ONLY when its originating
//! session is in that set (the user is already looking at it in some focused
//! window). App unfocused (empty set), OR a background tab fired it → delivered.
//! Carrying the SET (not a single `focused` bool + one `active` id) makes this
//! per-window-correct: with two windows, a focused non-front window's active tab
//! suppresses correctly, and the front window's active tab does NOT suppress when
//! the front window is unfocused. At one window the set is `{active}` (focused) or
//! `{}` (unfocused) — byte-identical to the old two-atomic behavior. This matches
//! the iTerm2 / Terminal.app default and keeps background-tab activity visible.
//!
//! **Identity and consent (macOS).** Both delivery paths are SUBPROCESSES, and a
//! subprocess of aterm runs with aterm as its TCC "responsible" app — whatever it
//! asks the system for is asked in aterm's name, at a program's request (OSC 9 /
//! 99 / 777 are program output; over SSH they are the remote's). That is why the
//! whole module sits behind the fail-closed `allow_notifications` opt-in
//! (`handler_osc_notify.rs`: no callback fires while it is off), the same consent
//! surface as the other fail-closed program-triggered escapes (`allow_window_ops`,
//! `allow_osc52_query`, `allow_kitty_file_transfer`).
//! What an `osascript` launched in aterm's name asks the system for was MEASURED
//! on macOS 26 (2026-09-13, `/usr/bin/log show --predicate 'subsystem ==
//! "com.apple.TCC"'` over 2026-09-11 00:00 to 2026-09-13 12:00): that PREDICATE
//! surfaced TWO `osascript` launches, and only one of them is aterm's. (The window
//! holds more — an unfiltered query over the same span finds four, pids 19482, 44554,
//! 35342 and 27784 — but the other two raised no TCC record at all, which is the
//! question this paragraph is about.)
//! pid 27784 (2026-09-12 22:18:34) ran under `responsible=targo-…` with
//! `binary_path=…/target.noindex/release/aterm-release` — the DMG builder's
//! Finder-layout script (`crates/aterm-release/src/dmg.rs`, `osascript (DMG
//! window layout)`) running under `targo`, nothing to do with this module.
//! pid 35342 (2026-09-12 15:54:46) ran under `responsible=com.aterm.aterm`
//! (`responsible_path` the installed `aterm.app` binary); TCC does not log the
//! script, so which osascript aterm ran is UNIDENTIFIED. The DECIDING fact is the
//! live config: `~/.config/aterm/aterm.toml` sets neither `allow_notifications` nor
//! `allow_osc52_query`, so this fallback was OFF on 2026-09-12 and no launch that day
//! can be its. The binary also holds two osascript sites, not one — this fallback
//! and the macOS-gated `display alert` a failed launch raises off the main thread
//! (`crates/aterm-gui/src/lib.rs`); atpkg's osascript elevation went with the
//! OS-installer door on 2026-09-24 — so the name
//! alone never identifies the caller. An earlier reading of the same log counted three launches
//! under `com.aterm.aterm` and credited them all to this fallback; the attribution
//! was wrong, and the count came from a wider query than the TCC predicate used here. What both launches DID read in TCC is the same: one
//! `kTCCServiceListenEvent` PREFLIGHT each (`AUTHREQ_CTX … preflight=yes,
//! query=1`, then WindowServer's own `AUTHREQ_ATTRIBUTION` for the same pid),
//! answered from the database with no `AUTHREQ_PROMPTING` — WindowServer's
//! standard check on any process that connects to it, not a notification
//! permission, and a preflight never raises a dialog. `usernoted` and
//! `com.apple.usernotifications` logged nothing naming osascript, Script Editor
//! or a display in the two minutes around either launch. So: on the measured
//! platform an osascript under aterm's identity asks for nothing that prompts,
//! but no launch in the log is attributable to THIS fallback, so its own
//! behaviour is unmeasured; it stays behind the fail-closed `allow_notifications`
//! opt-in regardless, because the attribution is aterm's.
//!
//! **aterm's own identity (macOS).** Run from an app bundle, aterm posts only
//! in-process, through `UNUserNotificationCenter` ([`crate::notice_center`]),
//! or not at all: the banner is the bundle's own, under the bundle's own
//! notification consent, its `userInfo` names the session, and a click goes
//! to that tab and pane (`Wake::NoticeClicked`, the ❯ menu's Go).
//! macOS is asked for that consent when the owner turns on Notify Me When I'm
//! Needed and, where `desktop_alerts` was turned on another way (aterm.toml, or
//! before aterm asked), by the first notice that would post while it is on
//! ([`way_for`], once per process), which goes out as aterm's own if macOS says
//! yes and its gates, read again, still admit it (Notify Me still on, its tab
//! not in view: the person answers in their own time). Until macOS has said yes
//! nothing is posted, and once it has said no nothing is either: that answer is
//! the owner's, and a subprocess would only put the same notice up under
//! another name (Script Editor's, without terminal-notifier: the banner the
//! owner called annoying on 2026-09-28). A bundle with no center to post
//! through posts nothing either, with one `aterm.log` line a notice
//! ([`Route::Centerless`]): one under a protected root (~/Desktop,
//! ~/Documents, ~/Downloads and the rest of
//! `aterm_containment::sbpl::PRIVATE_SUBDIRS`), whose `Info.plist` aterm
//! declines to read, so it has no bundle id to post as, and one whose center
//! did not open (UserNotifications did not load, or macOS gave none). Whether
//! the process runs from a bundle at all is a pure path test
//! ([`running_bundled`]), so a bundle aterm may not read is still known for
//! one. Only a bare binary has the two subprocesses above as its way out
//! ([`route`]). A program notification goes out the same way, still behind
//! `allow_notifications`, and asks macOS nothing while `desktop_alerts` is off.
//! No path asks for a sound.
//!
//! **aterm's own notices.** Two producers post text aterm wrote itself, not
//! program output, and so are NOT behind `allow_notifications`: the consent
//! attention path (`consent_observer::AttentionGate`) and the escalation herald
//! (`status_item::Herald`, one notice per transition of a session into a
//! frozen or stopped program, typed `attention`, an agent prompt or question,
//! or an agent wall: usage, spend, context, login, API, overload, memory). Both
//! use this same bounded queue and focus suppression, and both rate-limit
//! themselves before the queue. The herald never runs in a headless instance.
//! The update-health banner calls [`deliver`] directly.
//!
//! **`desktop_alerts` turns aterm's own notices on** (config key, default OFF
//! since 2026-09-28 — the owner: "I don't know what these OSX alerts are from
//! aterm but they are annoying, disable them"; on macOS without
//! `terminal-notifier` they arrive as Script Editor banners — and kept off by
//! the lead's ruling of 2026-10-18). Once on they are needs only
//! (owner-surfaces §3): the herald posts one notice per need and none for a
//! wait or a record, and the consent path's and update health's notices are
//! for what only the person can act on too. The owner opts in with the status
//! item's Notify Me When I'm Needed, whose first turn-on asks macOS to let
//! aterm post as itself, and whose hover says whose banner they would arrive as
//! here ([`own_voice`]): aterm's own once macOS has let it, a click on one
//! about a tab here going to that tab (*aterm's own identity*); none until
//! macOS has, and none once it has said no; none from a bundle with no
//! center, and what gives it one (for a bundle under a protected root: moved
//! to /Applications or ~/Applications, it posts as itself); and from a bare
//! binary, terminal-notifier's where it is found ([`notifier_path`]), else
//! Script Editor's, whose click opens Script Editor.
//! Each such
//! notice is built with [`NotifyMsg::own`], and the delivery thread drops an OWN message
//! while the live switch ([`spawn_delivery`]'s `own_alerts`) reads `false` — one
//! gate every producer on the queue passes, however it was written. The App's
//! producers also check the key themselves (the herald so its `aterm.log` line can
//! say the setting held the notice back; the update-health banner, which does not
//! ride the queue). Nothing else changes: the menu bar, the tab marks, the band
//! and `messages.log` still carry the herald's, the consent path's and update
//! health's facts.
//! Program notifications (OSC 9/99/777) are [`NotifyMsg::program`] and answer
//! to `allow_notifications` alone.

// Real delivery exists on macOS, Windows and Linux; elsewhere (the BSDs) this
// module is a channel-draining stub (`spawn_delivery`), and the
// real-notification helpers and the fields only they read are compiled out there.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;

/// One pending notification handed from a tab's engine callback to the delivery
/// thread. `session` is the originating tab's id (for focus-aware suppression).
pub(crate) struct NotifyMsg {
    /// Originating session/tab id (matched against the active tab to suppress
    /// self-notifications the user is already watching). Only a host with real
    /// delivery reads it.
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub session: u64,
    /// Notification title (OSC 99 carries one; OSC 9/777 do not — `None`).
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub title: Option<String>,
    /// Notification body.
    pub body: String,
    /// `true` for a notice aterm wrote itself ([`Self::own`]), which the
    /// `desktop_alerts` switch governs; `false` for a program's (OSC 9/99/777,
    /// [`Self::program`]), which `allow_notifications` already admitted.
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub own: bool,
}

impl NotifyMsg {
    /// One PROGRAM notification (OSC 9/99/777), already admitted by the
    /// `allow_notifications` opt-in. Off a delivery host (`spawn_delivery` there
    /// only drains the channel) the session and title have no reader, and are
    /// dropped here.
    pub(crate) fn program(session: u64, title: Option<String>, body: String) -> Self {
        Self::with_origin(session, title, body, false)
    }

    /// The escalation herald's notice. Only `status_item`'s herald builds a
    /// [`HeraldNotice`](crate::status_item::HeraldNotice), so this posts only
    /// what it decided.
    pub(crate) fn herald(notice: &crate::status_item::HeraldNotice) -> Self {
        Self::own(
            notice.session,
            Some(notice.title.to_owned()),
            notice.body.clone(),
        )
    }

    /// The consent attention path's notice. Only `consent_observer` builds an
    /// [`AttentionNotice`](crate::consent_observer::AttentionNotice), so this
    /// posts only what its gate decided.
    pub(crate) fn consent(notice: &crate::consent_observer::AttentionNotice) -> Self {
        Self::own(
            notice.session,
            Some(notice.title.to_owned()),
            notice.body.clone(),
        )
    }

    /// One notice aterm wrote ITSELF: the delivery thread drops it while
    /// `desktop_alerts` is off (the module doc's *`desktop_alerts`* paragraph).
    /// Private: aterm's own voice has exactly the two doors above, each
    /// taking what only its decider makes, so a new way to interrupt the owner
    /// is a new door written here.
    fn own(session: u64, title: Option<String>, body: String) -> Self {
        Self::with_origin(session, title, body, true)
    }

    fn with_origin(session: u64, title: Option<String>, body: String, own: bool) -> Self {
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux", test)))]
        let _ = (session, title, own);
        Self {
            #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
            session,
            #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
            title,
            body,
            #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
            own,
        }
    }
}

/// The delivery thread's one decision for a dequeued message: an OWN notice
/// while `desktop_alerts` is off (`own_alerts == false`) is dropped, and so is
/// any notice whose session the person is already looking at (`suppressed`, the
/// focus set). Pure, so the gate is tested without running a notifier.
#[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
fn delivers(msg: &NotifyMsg, suppressed: &HashSet<u64>, own_alerts: bool) -> bool {
    if msg.own && !own_alerts {
        return false;
    }
    !suppressed.contains(&msg.session)
}

/// Bound on the notification delivery queue, mirroring the OSC 52 clipboard
/// pipeline's queue-bounding. Each [`NotifyMsg`] is up to ~4 KiB and the delivery
/// thread blocks ~100 ms per notifier subprocess, so an UNBOUNDED channel let a
/// program flooding OSC 9/99/777 grow the queue without bound (memory exhaustion)
/// and spawn one blocking subprocess per message (subprocess spam). A small
/// `sync_channel` cap bounds queue memory to `N·msg`; the producer (the engine
/// callback) `try_send`s and DROPS on `Full` rather than blocking the PTY reader
/// thread. Unlike the clipboard (last-writer-wins, coalesced), notifications are
/// DISTINCT, so the overflow is dropped — never coalesced.
const NOTIFY_QUEUE_CAP: usize = 16;

/// Whether this build has a host that can turn a queued notification into an
/// operating-system notification. Keep Settings' availability projection tied
/// to the same cfg as [`spawn_delivery`] so a channel-draining portability stub
/// can never be presented as working delivery.
#[must_use]
pub(crate) const fn delivery_available() -> bool {
    cfg!(any(target_os = "macos", windows, target_os = "linux"))
}

/// Spawn the single, process-wide notification delivery thread and return the
/// `SyncSender` each tab clones into its engine callbacks. The thread parks on
/// `recv()` (0% idle when no notifications arrive) and exits when every sender
/// is dropped. `suppress` is the live suppression set the UI thread keeps current
/// (the active-tab focused-pane id of every focused window); the thread reads it
/// to apply focus-aware suppression. `own_alerts` is the live `desktop_alerts`
/// switch the UI thread keeps current: while it reads `false`, every
/// [`NotifyMsg::own`] notice is dropped here ([`delivers`]). `center` is the
/// instance's notice center (macOS: aterm's own voice, inert headless and in
/// tests); elsewhere it holds nothing.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn spawn_delivery(
    suppress: Arc<Mutex<HashSet<u64>>>,
    silent: Arc<AtomicBool>,
    own_alerts: Arc<AtomicBool>,
    center: crate::notice_center::NoticeCenter,
) -> SyncSender<NotifyMsg> {
    #[cfg(not(target_os = "macos"))]
    let _ = center;
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            // Suppress when the firing session is the active tab of SOME
            // focused window — the user is already looking at it. App unfocused
            // (empty set) OR a background tab fired it → deliver (a background
            // tab's activity still surfaces, mirroring `App::on_bell`) — unless
            // it is aterm's own notice and `desktop_alerts` is off.
            let alerts = own_alerts.load(Ordering::Acquire);
            let admitted = delivers(
                &msg,
                &suppress.lock().unwrap_or_else(|p| p.into_inner()),
                alerts,
            );
            if !admitted {
                continue;
            }
            // A click on the Windows notification brings back the tab it came
            // from (`win_balloon`), as one on aterm's own macOS notice does
            // (its `userInfo` names the session); Linux's server owns the click.
            #[cfg(windows)]
            win_balloon::set_click_target(msg.session);
            // The notice that asks macOS waits on the person's answer, and
            // meanwhile they may turn aterm's notices off or go to its tab:
            // it is posted only if the live gates still admit it.
            #[cfg(target_os = "macos")]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                silent.load(Ordering::Acquire),
                Some(msg.session),
                center,
                alerts,
                || {
                    delivers(
                        &msg,
                        &suppress.lock().unwrap_or_else(|p| p.into_inner()),
                        own_alerts.load(Ordering::Acquire),
                    )
                },
            );
            #[cfg(not(target_os = "macos"))]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                silent.load(Ordering::Acquire),
            );
        }
    });
    tx
}

/// Stub for a target with no notifier wired up (neither macOS, Windows nor
/// Linux): drain the channel so senders never block and the workspace builds
/// everywhere.
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn spawn_delivery(
    _suppress: Arc<Mutex<HashSet<u64>>>,
    _silent: Arc<AtomicBool>,
    _own_alerts: Arc<AtomicBool>,
    _center: crate::notice_center::NoticeCenter,
) -> SyncSender<NotifyMsg> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    tx
}

/// Deliver one notification natively: as aterm itself through
/// `UNUserNotificationCenter` where there is a center to post through
/// ([`way_for`], [`crate::notice_center`]), its `userInfo` naming `session` so
/// a click goes to that tab — once macOS has let aterm, after asking it first
/// where `alerts` (`desktop_alerts`, live) is on and no notice has asked, and
/// nowhere until it has or once it has refused. A bundle with no center (one
/// under a protected root, which has no bundle id aterm reads, or one whose
/// center did not open) posts nowhere, with one `aterm.log` line. Only a bare
/// binary posts through a subprocess: `terminal-notifier` (speaking as itself)
/// when installed, else `osascript`'s `display notification`. Runs ONLY on a thread
/// of its own — the delivery thread, or the update-health banner's — since it
/// may wait on the center, on macOS's answer to the ask and on a subprocess.
/// Best-effort: a missing notifier or a non-zero exit is swallowed. Never from
/// a headless process, which presents no OS UI (an agent's or a test's
/// private instance would put a banner on the person's desktop), nor from a
/// unit test.
///
/// Program-originated notifications reach it only through the
/// `allow_notifications` gate (the module doc's *Identity and consent* paragraph
/// says what each subprocess asks the system for under aterm's name, and that
/// the measured answer is: nothing that prompts). aterm's OWN notices — the
/// consent attention path, the escalation herald and the
/// update-health banner — are not program output and do not pass that gate;
/// they pass `desktop_alerts` instead (default off; Notify Me When I'm Needed
/// opts in). See *aterm's own notices* in the module doc. No path asks for a
/// sound, so serious mode (`_silent`) has nothing to mute here. `wanted`
/// reads the notice's gates again, live, for the one notice that waited on
/// macOS's answer to its ask ([`way_for`]): the queue's [`delivers`], the
/// update-health banner's `desktop_alerts`.
#[cfg(target_os = "macos")]
fn deliver(
    title: Option<&str>,
    body: &str,
    _silent: bool,
    session: Option<u64>,
    center: crate::notice_center::NoticeCenter,
    alerts: bool,
    wanted: impl Fn() -> bool,
) {
    use aterm_uds::spawnfd::Stdio;

    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let title = title.unwrap_or("aterm");
    let notifier = notifier_path();

    // A bundle's voice is aterm's own: posted in-process as the bundle, or
    // not at all. A center that would not take the notice, or none to post
    // through, is one log line, never a subprocess.
    match way_for(
        center,
        running_bundled(),
        notifier.is_some(),
        alerts,
        &ASKED,
        wanted,
    ) {
        Route::Center => {
            let posted = center.post(&crate::notice_center::Notice {
                title,
                body,
                session,
            });
            if !posted {
                aterm_log::warn!("notices: one not posted: the center would not take it");
            }
            return;
        }
        Route::Withheld => {
            aterm_log::info!(
                "notices: one not posted: aterm's notifications are off in System Settings \
                 \u{25B8} Notifications"
            );
            return;
        }
        Route::Ask | Route::Unasked => {
            aterm_log::info!(
                "notices: one not posted: macOS has not let aterm post as itself yet (Notify Me \
                 When I'm Needed asks)"
            );
            return;
        }
        Route::Lapsed => {
            aterm_log::info!(
                "notices: one not posted: while macOS asked, aterm's own notices were turned off \
                 or its tab came into view"
            );
            return;
        }
        Route::Centerless if crate::control_privacy::running_bundle_id().is_none() => {
            aterm_log::info!(
                "notices: one not posted: aterm does not read this aterm.app's identity (it sits \
                 in a folder macOS guards, such as Downloads, or was started through a link), so \
                 it has none to post as; moved to /Applications or ~/Applications and opened \
                 there, it posts as itself"
            );
            return;
        }
        Route::Centerless => {
            aterm_log::info!(
                "notices: one not posted: macOS gave this aterm.app no notification center (an \
                 earlier notices: line says why), and an app posts only as itself"
            );
            return;
        }
        Route::Notifier | Route::Osascript => {}
    }

    // A bare binary: terminal-notifier, where a person installs it
    // (`notifier_path`, by its absolute path: an aterm the Dock started has
    // launchd's PATH, where none is). Arguments are real argv entries, so the
    // (untrusted) title/body cannot inject. `.status()` both waits (reaping the
    // child) and tells us whether it ran — `Err` (none found, or gone since)
    // falls through to osascript. A non-zero exit means it IS installed but
    // failed; we do NOT double-notify via osascript in that case.
    let tn = notifier.map(|notifier| {
        let mut tn = aterm_uds::spawnfd::command(notifier);
        notifier_args(&mut tn, title, body);
        tn.status()
    });
    if tn.is_some_and(|status| status.is_ok()) {
        return;
    }

    // Fallback: osascript. The body + title go into an AppleScript string
    // literal, so both are escaped. The script itself is passed as a single
    // `-e` argv entry (never via a shell), closing the shell-injection vector;
    // `applescript_escape` closes the AppleScript-injection vector.
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title),
    );
    let _ = aterm_uds::spawnfd::command("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether a notice has asked macOS to let aterm post as itself in this
/// process ([`way_for`]): the notices ask once, ever. Notify Me's turn-on asks
/// on its own (`App::toggle_desktop_alerts`), whatever this says.
#[cfg(target_os = "macos")]
static ASKED: AtomicBool = AtomicBool::new(false);

/// How long the notice that asked macOS waits for its answer before its route
/// is read again: the prompt waits on the person, so the bound is theirs to
/// read and click, and the notices queued meanwhile wait behind it (the
/// bounded queue drops what overflows). Past it, macOS has not answered and
/// the notice is not posted; the next one after macOS's yes is.
#[cfg(target_os = "macos")]
const ASK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// HOW ONE NOTICE GOES OUT, asking macOS first where its [`route`] is
/// [`Route::Ask`]: the ask is `asked`'s one (spent here, so of every notice
/// this process delivers only the first asks), the notice waits up to
/// [`ASK_WAIT`] for macOS's answer (the ask returns at once, and the answer
/// comes on another thread), the Notify Me hover learns it
/// ([`learn_voice`]), and the route is read again with the ask spent — the
/// center on a yes, nowhere on a no or no answer — and on a yes the notice is
/// posted only if it is still `wanted` (its gates, read again: the person may
/// have turned aterm's notices off, or gone to its tab, before answering).
/// `bundled` is [`running_bundled`]'s. Never [`Route::Ask`].
#[cfg(target_os = "macos")]
fn way_for(
    center: crate::notice_center::NoticeCenter,
    bundled: bool,
    notifier: bool,
    alerts: bool,
    asked: &AtomicBool,
    wanted: impl Fn() -> bool,
) -> Route {
    match route(
        center.answer(),
        bundled,
        notifier,
        alerts,
        asked.load(Ordering::Acquire),
    ) {
        Route::Ask if !asked.swap(true, Ordering::AcqRel) => {}
        Route::Ask => return Route::Unasked,
        way => return way,
    }
    let (answered, answer) = std::sync::mpsc::sync_channel::<()>(1);
    center.ask(Box::new(move || {
        let _ = answered.try_send(());
    }));
    // A `then` the center dropped uncalled (it could not ask) ends the wait
    // as an answer does: the route read below says what macOS has said.
    if answer.recv_timeout(ASK_WAIT) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
        aterm_log::info!("notices: macOS has not answered aterm's ask within {ASK_WAIT:?}");
    }
    learn_voice(center, || wake(crate::Wake::NoticeVoice));
    match route(center.answer(), bundled, notifier, alerts, true) {
        Route::Center if !wanted() => Route::Lapsed,
        way => way,
    }
}

/// HOW ONE NOTICE GOES OUT on macOS ([`deliver`]'s one decision, through
/// [`way_for`]). From an app bundle, as aterm itself once macOS has granted
/// it, and otherwise not at all: once macOS has refused it (the owner's
/// answer, which a subprocess posting the same notice under another name
/// would override), until macOS has answered — asking it first where the
/// person turned aterm's notices on (`desktop_alerts`) and no notice has asked
/// yet, since the prompt is then the expected next step — and where the
/// bundle has no center to post through (one under a protected root, whose id
/// aterm does not read, or one whose center did not open). A bundled aterm
/// never reaches a subprocess: terminal-notifier's banner is not Script
/// Editor's, but it is not aterm's either, its click cannot reach the tab, and
/// moving the bundle out of the protected folder gives it its own voice, which
/// the Notify Me hover says. Only a bare binary (no bundle at all) has the
/// subprocesses: terminal-notifier where it was found ([`notifier_path`]),
/// else `osascript`.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// `UNUserNotificationCenter`, as the bundle.
    Center,
    /// Ask macOS to let aterm post as itself, then route again ([`way_for`]).
    Ask,
    /// Nowhere: macOS refused aterm (or answered what this build cannot
    /// read), so the notice is dropped with one `aterm.log` line.
    Withheld,
    /// Nowhere yet: macOS has not answered, and this notice may not ask
    /// (`desktop_alerts` is off, so it is a program's, or a notice already
    /// asked); one `aterm.log` line.
    Unasked,
    /// Nowhere: the notice waited on macOS's answer to its ask, and by the
    /// yes its gates no longer admit it (aterm's own notices were turned
    /// off, or its tab came into view: [`delivers`], read again); one
    /// `aterm.log` line. Only [`way_for`] gives it.
    #[cfg(target_os = "macos")]
    Lapsed,
    /// Nowhere: aterm runs from a bundle with no center to post through (its
    /// `Info.plist` under a protected root, which aterm declines to read, or
    /// UserNotifications that did not load, or no center from macOS), and a
    /// bundle posts only as itself; one `aterm.log` line saying which.
    Centerless,
    /// terminal-notifier: a bare binary's.
    Notifier,
    /// `osascript`'s `display notification` (Script Editor's banner): a bare
    /// binary's, without terminal-notifier.
    Osascript,
}

/// [`Route`] from what macOS has said, whether aterm runs from an app bundle
/// (`bundled`, [`running_bundled`]), whether terminal-notifier was found,
/// whether aterm's own notices are on (`alerts`) and whether a notice has
/// asked macOS already (`asked`).
#[cfg(any(target_os = "macos", test))]
fn route(
    center: crate::notice_center::Center,
    bundled: bool,
    notifier: bool,
    alerts: bool,
    asked: bool,
) -> Route {
    use crate::notice_center::Center;
    match (center, notifier) {
        (Center::Granted, _) => Route::Center,
        (Center::Refused, _) => Route::Withheld,
        (Center::Unasked, _) if alerts && !asked => Route::Ask,
        (Center::Unasked, _) => Route::Unasked,
        (Center::Absent, _) if bundled => Route::Centerless,
        (Center::Absent, true) => Route::Notifier,
        (Center::Absent, false) => Route::Osascript,
    }
}

/// WHETHER THIS PROCESS RUNS FROM AN APP BUNDLE, by the in-bundle fence's
/// rule (`consent::fence_exe`): its exec path, else the kernel's path for its
/// image (`proc_pidpath`, one syscall), inside a `.app`. Lexical over the two
/// paths: it reads no file, so a bundle under a protected root, whose
/// `Info.plist` aterm declines to read, is still known for one, with no
/// consent dialog raised; and a bundle started through a link from outside
/// it (`tools/install.sh`'s), whose exec path alone reads unbundled, is one
/// too. Read once per process.
#[cfg(target_os = "macos")]
fn running_bundled() -> bool {
    use aterm_containment::consent;
    static BUNDLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BUNDLED.get_or_init(|| {
        consent::fence_exe().is_some_and(|exe| consent::path_is_in_app_bundle(&exe))
    })
}

/// terminal-notifier's argv for one notice, and its quiet stdio. No
/// `-sender`: only a bare binary posts through terminal-notifier
/// ([`route`]), and it has no bundle id to claim, so terminal-notifier speaks
/// as itself, which is true.
#[cfg(target_os = "macos")]
fn notifier_args(tn: &mut aterm_uds::spawnfd::Command, title: &str, body: &str) {
    use aterm_uds::spawnfd::Stdio;
    tn.arg("-title")
        .arg(title)
        .arg("-message")
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
}

/// Where a person installs terminal-notifier besides a directory on PATH:
/// Homebrew's prefix on Apple silicon, then on Intel.
#[cfg(target_os = "macos")]
const NOTIFIER_HOMES: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// TERMINAL-NOTIFIER, WHERE A PERSON INSTALLS IT, by its absolute path: on
/// this process's PATH, else in Homebrew's two prefixes ([`NOTIFIER_HOMES`]),
/// else on the login shell's PATH (`spawn::atpkg_child_path_once`, which the
/// packages lane asks anyway). An aterm the Dock or Finder started inherits
/// launchd's PATH (`/usr/bin:/bin:/usr/sbin:/sbin`, measured on the owner's
/// running aterm), where no installed terminal-notifier is (SIP keeps it out
/// of those four), so a lookup on PATH alone never found one there and every
/// notice was Script Editor's. Looked up once per process, by [`deliver`]
/// (the delivery thread) and the voice's learner ([`learn_voice`]) — never on
/// the event loop, since the login shell's PATH can take seconds the first
/// time; a terminal-notifier installed later is found by the next process. A
/// test build looks nowhere (inert: no PATH walk, no login shell).
#[cfg(target_os = "macos")]
fn notifier_path() -> Option<&'static std::path::Path> {
    use std::path::PathBuf;
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    if cfg!(test) {
        return None;
    }
    FOUND
        .get_or_init(|| {
            let process = std::env::var_os("PATH");
            let near = process
                .iter()
                .flat_map(std::env::split_paths)
                .chain(NOTIFIER_HOMES.iter().map(PathBuf::from));
            find_notifier(near).or_else(|| {
                find_notifier(std::env::split_paths(crate::spawn::atpkg_child_path_once()))
            })
        })
        .as_deref()
}

/// The first of `dirs` that holds an executable `terminal-notifier`.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn find_notifier(dirs: impl IntoIterator<Item = std::path::PathBuf>) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    dirs.into_iter()
        .map(|dir| dir.join("terminal-notifier"))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// [`own_voice`]'s answer once learned ([`learn_voice`]), learned again where
/// it can have moved: after macOS answers the ask, and at each status-menu
/// open (the owner may have changed System Settings meanwhile).
#[cfg(target_os = "macos")]
static VOICE: Mutex<Option<crate::status_item::NoticeVoice>> = Mutex::new(None);

/// Whether a [`learn_voice`] thread is reading: one at a time.
#[cfg(target_os = "macos")]
static LEARNING: AtomicBool = AtomicBool::new(false);

/// WHOSE BANNER aterm's own notices arrive as here, as [`deliver`] posts
/// them — what the status item's Notify Me row's hover says. On macOS:
/// aterm's own once macOS has let it post as itself (a click goes to the
/// tab), and what macOS has said otherwise (not asked yet, or no: none then),
/// beside what a notice is where there is no center to post through
/// ([`voice_of`]): from a bundle, none, and why (its identity unread where it
/// sits, so moving it gives it one, or its center not opened); from a bare
/// binary, terminal-notifier's own banner ([`notifier_path`]), or — without
/// terminal-notifier — `osascript`'s, which is Script Editor's and whose
/// click opens Script Editor. Never blocks: `None` on macOS until
/// [`learn_voice`] has read it off the event loop, and `None` elsewhere, where
/// no menu says it (Windows' balloons hang off aterm's own tray icon). A test
/// reads the inert answer: a bare binary without terminal-notifier.
pub(crate) fn own_voice() -> Option<crate::status_item::NoticeVoice> {
    if cfg!(test) {
        return Some(crate::status_item::NoticeVoice {
            center: crate::notice_center::Center::Absent,
            fallback: crate::status_item::NoticeFallback::ScriptEditor,
        });
    }
    platform_voice()
}

/// [`own_voice`] on macOS: what [`learn_voice`] read last, once it has.
#[cfg(target_os = "macos")]
fn platform_voice() -> Option<crate::status_item::NoticeVoice> {
    *VOICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// [`own_voice`] off macOS: no status menu reads it.
#[cfg(not(target_os = "macos"))]
fn platform_voice() -> Option<crate::status_item::NoticeVoice> {
    None
}

/// Whose banner a notice is, from what macOS has said about aterm posting as
/// itself, whether aterm runs from an app bundle ([`running_bundled`]),
/// whether terminal-notifier was found and whether the running bundle's id
/// was read — the facts [`deliver`] acts on ([`route`], and the
/// `aterm.log` line a centerless bundle's notice leaves). What a notice is
/// without a center matters only where `center` is [`Center::Absent`].
///
/// [`Center::Absent`]: crate::notice_center::Center::Absent
#[cfg(any(target_os = "macos", test))]
fn voice_of(
    center: crate::notice_center::Center,
    bundled: bool,
    notifier: bool,
    bundle_id: bool,
) -> crate::status_item::NoticeVoice {
    use crate::status_item::{NoticeFallback, NoticeVoice};
    let fallback = match (bundled, notifier) {
        (true, _) => NoticeFallback::Withheld { named: bundle_id },
        (false, true) => NoticeFallback::Notifier,
        (false, false) => NoticeFallback::ScriptEditor,
    };
    NoticeVoice { center, fallback }
}

/// LEARN [`own_voice`] off the event loop, then call `then` (the App marks
/// its status item dirty, so the hover says it, and becomes the center's
/// delegate): the notifier lookup can wait on the login shell, the bundle id
/// reads its `Info.plist` and macOS's answer is a wait on the center, so a
/// thread of its own does all three. One at a time: a call while one reads
/// is dropped. macOS only (the status item's install, its menu's open and
/// the answer to either ask, Notify Me's or a notice's, call it); elsewhere
/// no menu says the voice.
#[cfg(target_os = "macos")]
pub(crate) fn learn_voice(
    center: crate::notice_center::NoticeCenter,
    then: impl FnOnce() + Send + 'static,
) {
    if cfg!(test) || LEARNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-notice-voice".to_string())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            let voice = voice_of(
                center.answer(),
                running_bundled(),
                notifier_path().is_some(),
                crate::control_privacy::running_bundle_id().is_some(),
            );
            *VOICE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(voice);
            LEARNING.store(false, Ordering::Release);
            then();
        });
    if spawned.is_err() {
        LEARNING.store(false, Ordering::Release);
    }
}

/// The update-health banner's delivery: owed to a cause only the owner can end
/// (`App::post_update_health_banner`), so it bypasses the queue, and posted
/// only once that has read `desktop_alerts` on, so it is delivered as a notice
/// while aterm's own are on (and may ask macOS, as the queue's may; having
/// asked, it is posted only if `own_alerts`, the live switch, is still on).
/// Its one door: [`deliver`] itself is private, and only `messages_host` mints a
/// [`HealthBannerVoice`](crate::messages_host::HealthBannerVoice). It is about
/// no tab, so a click on it only brings aterm forward.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn deliver_update_health_banner(
    _voice: crate::messages_host::HealthBannerVoice,
    center: crate::notice_center::NoticeCenter,
    own_alerts: &AtomicBool,
    title: &str,
    banner: &str,
) {
    deliver(Some(title), banner, false, None, center, true, || {
        own_alerts.load(Ordering::Acquire)
    });
}

/// Deliver one notification natively on Windows: a `Shell_NotifyIconW`
/// `NIF_INFO` balloon from a transient tray icon owned by a hidden message-only
/// window (on Win10/11 the shell renders it as a toast). Runs ONLY on the
/// delivery thread (the window has thread affinity and blocking is fine there).
/// Best-effort: any Win32 failure is swallowed. Never from a headless process,
/// which presents no OS UI (an agent's or a test's private instance would put a
/// toast on the person's desktop), nor from a unit test.
#[cfg(windows)]
fn deliver(title: Option<&str>, body: &str, silent: bool) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let title = title.unwrap_or("aterm");
    // An empty `szInfo` HIDES the balloon entirely, so a title-only OSC 99
    // (body absent) promotes the title into the body slot.
    let (title, body) = if body.is_empty() {
        ("aterm", title)
    } else {
        (title, body)
    };
    win_balloon::deliver(title, body, silent);
}

/// Windows `NOTIFYICONDATAW.dwInfoFlags` for a normal or serious-mode alert.
/// `0x10` is `NIIF_NOSOUND`; keeping this pure makes the serious-mode sound
/// contract testable on every host, not only Windows CI. With aterm's icon
/// (`own_icon`) the notification carries it, large (`NIIF_USER |
/// NIIF_LARGE_ICON`), where the stock `NIIF_INFO` drew a generic ⓘ.
#[cfg(any(test, windows))]
const fn windows_info_flags(silent: bool, own_icon: bool) -> u32 {
    const NIIF_INFO: u32 = 0x1;
    const NIIF_USER: u32 = 0x4;
    const NIIF_NOSOUND: u32 = 0x10;
    const NIIF_LARGE_ICON: u32 = 0x20;
    const NIIF_RESPECT_QUIET_TIME: u32 = 0x80;
    let icon = if own_icon {
        NIIF_USER | NIIF_LARGE_ICON
    } else {
        NIIF_INFO
    };
    icon | NIIF_RESPECT_QUIET_TIME | if silent { NIIF_NOSOUND } else { 0 }
}

/// Whether a tray-icon callback `lParam` is a click on the notification. Without
/// `NIM_SETVERSION` (version 0) the `lParam` is the event itself:
/// `NIN_BALLOONUSERCLICK` (`WM_USER + 5`). Pure, so it is tested on every host.
#[cfg(any(test, windows))]
const fn balloon_clicked(lparam: isize) -> bool {
    const NIN_BALLOONUSERCLICK: isize = 0x0400 + 5;
    lparam & 0xFFFF == NIN_BALLOONUSERCLICK
}

/// The loop proxy a notice wakes the App through: a click on one goes to its
/// tab (`Wake::NoticeClicked`: Windows' balloon, and aterm's own macOS notice
/// through `notice_center`'s delegate), and macOS's answer to a notice's ask
/// reaches the Notify Me hover (`Wake::NoticeVoice`). Installed once at
/// launch; before that, and in a headless App, a wake goes nowhere. A test
/// build has none: its wakes are recorded ([`take_wakes`]).
#[cfg(all(any(windows, target_os = "macos"), not(test)))]
static PROXY: std::sync::OnceLock<aterm_window::event_loop::EventLoopProxy<crate::Wake>> =
    std::sync::OnceLock::new();

/// Install the proxy a notice's wakes ride. First install wins. Only Windows'
/// notifier and macOS's center wake the App; elsewhere, and in a test build,
/// this is a no-op.
pub(crate) fn install_proxy(proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
    #[cfg(all(any(windows, target_os = "macos"), not(test)))]
    let _ = PROXY.set(proxy.clone());
    #[cfg(not(all(any(windows, target_os = "macos"), not(test))))]
    let _ = proxy;
}

/// Hand `wake` to the App through the installed proxy ([`PROXY`]); a test
/// build records it for [`take_wakes`] instead.
#[cfg(any(windows, target_os = "macos"))]
fn wake(wake: crate::Wake) {
    #[cfg(not(test))]
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(wake);
    }
    #[cfg(test)]
    WAKES.with(|wakes| wakes.borrow_mut().push(wake));
}

#[cfg(all(any(windows, target_os = "macos"), test))]
thread_local! {
    /// The wakes this thread's notices handed the App (test-only).
    static WAKES: std::cell::RefCell<Vec<crate::Wake>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The wakes this thread's notices handed the App since the last call
/// (test-only): what a click or an ask would have sent through the proxy.
/// macOS's only: its readers are the center's click tests, and Windows'
/// balloon has none in a test build.
#[cfg(all(target_os = "macos", test))]
pub(crate) fn take_wakes() -> Vec<crate::Wake> {
    WAKES.with(|wakes| std::mem::take(&mut *wakes.borrow_mut()))
}

/// A click on the notification `session` posted: take the owner there, as the
/// ❯ menu's Go does (`App::go_to_session`: that session's window raised, its
/// tab selected, its pane given the keyboard). A session gone since goes
/// nowhere.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn raise_clicked(session: u64) {
    wake(crate::Wake::NoticeClicked { session });
}

/// Deliver one notification on Linux: `org.freedesktop.Notifications.Notify` on
/// the session bus ([`freedesktop`]). Runs ONLY on the delivery thread (the call
/// blocks up to [`freedesktop::CALL_TIMEOUT`]). Best-effort: no session bus, no
/// notification server or a refused call is swallowed. Never from a headless
/// process (the module doc's *Linux* paragraph), and never from a unit test,
/// whose process carries the developer's own session-bus address (the live
/// test drives [`freedesktop::Bus`] against a private daemon instead).
#[cfg(target_os = "linux")]
fn deliver(title: Option<&str>, body: &str, silent: bool) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let Ok(call) = freedesktop::notify_call(title.unwrap_or("aterm"), body, silent) else {
        return;
    };
    freedesktop::BUS.with(|bus| {
        bus.borrow_mut()
            .notify(&call, &aterm_dbus::Connection::session);
    });
}

/// `org.freedesktop.Notifications` over the first-party D-Bus client: the
/// `Notify` call, and the delivery thread's one connection to the session bus.
#[cfg(target_os = "linux")]
mod freedesktop {
    use std::cell::RefCell;
    use std::time::Duration;

    use aterm_dbus::{Connection, Message, Type, Value};

    /// The notification server's well-known name, object and interface.
    const SERVER: &str = "org.freedesktop.Notifications";
    const OBJECT: &str = "/org/freedesktop/Notifications";

    /// How long the delivery thread waits for the server's answer. A server
    /// answers `Notify` at once; the bound only keeps a wedged bus from holding
    /// the thread (the bounded queue drops what arrives meanwhile).
    pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(2);

    thread_local! {
        /// The delivery thread's connection, opened at its first notification.
        pub(super) static BUS: RefCell<Bus> = const { RefCell::new(Bus { conn: None }) };
    }

    /// One `Notify(susssasa{sv}i)` call. `summary` is plain text by the
    /// specification; `body` is markup to a server that advertises
    /// `body-markup`, so it is escaped ([`super::markup_escape`]). The hints
    /// name aterm's desktop entry (`aterm.desktop`, which tools/install.sh
    /// lays) so the shell attributes the notification to aterm, keep urgency
    /// NORMAL, and add `suppress-sound` in serious mode (`silent`).
    pub(super) fn notify_call(
        summary: &str,
        body: &str,
        silent: bool,
    ) -> aterm_dbus::Result<Message> {
        let mut hints = vec![
            (
                Value::from("desktop-entry"),
                Value::variant(Value::from("aterm")),
            ),
            (Value::from("urgency"), Value::variant(Value::Byte(1))),
        ];
        if silent {
            hints.push((
                Value::from("suppress-sound"),
                Value::variant(Value::Bool(true)),
            ));
        }
        Ok(
            Message::method_call(Some(SERVER), OBJECT, Some(SERVER), "Notify")?.with_body(vec![
                Value::from("aterm"),
                Value::U32(0),
                Value::from("aterm"),
                Value::Str(super::fold_controls(summary)),
                Value::Str(super::markup_escape(body)),
                Value::strings(std::iter::empty::<String>()),
                Value::dict(Type::Str, Type::Variant, hints)?,
                Value::I32(-1),
            ]),
        )
    }

    /// The delivery thread's session-bus connection, opened on first use and
    /// dialled again when the bus has closed it.
    pub(super) struct Bus {
        pub(super) conn: Option<Connection>,
    }

    impl Bus {
        /// Send `call` and wait for its answer, best-effort. `open` dials the bus
        /// (`Connection::session` in the product; a test hands its private
        /// daemon). A connection that fails — closed by the bus, a socket error,
        /// a deadline — is dropped; when it was a cached one, a fresh one is
        /// dialled and the call sent once more, since a long-lived connection
        /// the bus restarted under is the expected way to find it dead. An error
        /// the SERVER answered (no notification server on this bus) leaves the
        /// connection as it is: the bus is fine. Returns whether the server
        /// accepted the notification.
        pub(super) fn notify(
            &mut self,
            call: &Message,
            open: &dyn Fn() -> aterm_dbus::Result<Connection>,
        ) -> bool {
            for _ in 0..2 {
                let dialled = self.conn.is_none();
                if dialled {
                    self.conn = open().ok();
                }
                let Some(conn) = self.conn.as_mut() else {
                    return false;
                };
                // What the bus sent since the last call (`NameAcquired`, a
                // server's `NotificationClosed`) is not this thread's business:
                // drop it, so the connection's retained-message cap never fills.
                while let Ok(Some(_)) = conn.recv(Some(Duration::ZERO)) {}
                match conn.call(call.clone(), CALL_TIMEOUT) {
                    Ok(_) => return true,
                    Err(aterm_dbus::Error::Remote { .. }) => return false,
                    Err(_) => {
                        self.conn = None;
                        if dialled {
                            return false;
                        }
                    }
                }
            }
            false
        }
    }
}

/// Escape a notification body for a freedesktop server that reads it as markup
/// (`body-markup`: GNOME Shell, KDE Plasma): `&`, `<` and `>` become entities, so
/// a program's (over SSH, a remote's) `<b>` or `<a href=…>` is shown as text, never
/// rendered or followed. Control bytes are folded as [`fold_controls`] folds
/// them, keeping a newline, which the server shows as a line break. Defined
/// platform-independently so it is unit-tested on every target.
#[cfg(any(target_os = "linux", test))]
fn markup_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in fold_controls(s).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// A notification string with every C0 control byte but a newline folded to a
/// space (a carriage return and a tab included): notifications carry no
/// terminal controls, and an escape sequence in one only invites a server's
/// quirks. Defined platform-independently so it is unit-tested on every target.
#[cfg(any(target_os = "linux", test))]
fn fold_controls(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' => '\n',
            c if (c as u32) < 0x20 || c == '\u{7f}' => ' ',
            c => c,
        })
        .collect()
}

/// Windows balloon/toast delivery over direct `unsafe extern "system"` FFI
/// (user32 + kernel32 + shell32, the same hand-rolled style as
/// `clipboard_win`). A lazily created hidden MESSAGE-ONLY window (parent
/// `HWND_MESSAGE`) owns a TRANSIENT tray icon: each notification is one
/// `NIM_ADD` carrying `NIF_INFO` (the shell shows the balloon — a toast on
/// Win10/11 — the moment the icon appears), a short message-pumping linger so
/// the tray can validate the window while the balloon displays, then
/// `NIM_DELETE` so no permanent tray icon accumulates. The icon's callback
/// message reaches the window's own procedure during the linger, and a click on
/// the notification brings back the tab that posted it
/// ([`super::raise_clicked`]).
#[cfg(windows)]
mod win_balloon {
    // Win32 ABI names are kept verbatim (PascalCase fields, SCREAMING struct
    // names) so they can be checked against the SDK headers line by line.
    #![allow(non_snake_case, clippy::upper_case_acronyms)]

    use std::cell::{Cell, OnceCell};
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    /// A Win32 HWND/HICON/HINSTANCE as a plain pointer-sized integer (the
    /// `aterm-pty` FFI convention).
    type HWND = isize;

    /// `CreateWindowExW` parent sentinel for a message-only window.
    const HWND_MESSAGE: HWND = -3;
    /// `PeekMessageW` remove flag.
    const PM_REMOVE: u32 = 0x1;
    /// `Shell_NotifyIconW` verbs.
    const NIM_ADD: u32 = 0x0;
    const NIM_DELETE: u32 = 0x2;
    /// `NOTIFYICONDATAW.uFlags`: which fields are valid.
    const NIF_MESSAGE: u32 = 0x1;
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    /// The icon's callback message to the hidden window (`uCallbackMessage`):
    /// `WM_APP + 1`, private to this window class.
    const WM_TRAY_CALLBACK: u32 = 0x8000 + 1;
    /// Stock application icon resource id for `LoadIconW(0, ..)`.
    const IDI_APPLICATION: usize = 32512;
    /// `RegisterClassW` "failure" that is fine: the class already exists.
    const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

    /// How long the transient icon (and thus the balloon/toast) stays before
    /// `NIM_DELETE`. The shell's default balloon display time is ~5 s; this
    /// covers it, and the bounded queue drops overflow while we linger, so a
    /// flood costs at most `NOTIFY_QUEUE_CAP` lingers.
    const BALLOON_LINGER: Duration = Duration::from_secs(6);

    /// `POINT`.
    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    /// `MSG` — pumped, never inspected.
    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: u32,
        wParam: usize,
        lParam: isize,
        time: u32,
        pt: POINT,
    }

    /// `WNDCLASSW` for the hidden window; `DefWindowProcW` handles everything.
    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        lpfnWndProc: unsafe extern "system" fn(HWND, u32, usize, isize) -> isize,
        cbClsExtra: i32,
        cbWndExtra: i32,
        hInstance: isize,
        hIcon: isize,
        hCursor: isize,
        hbrBackground: isize,
        lpszMenuName: *const u16,
        lpszClassName: *const u16,
    }

    /// `NOTIFYICONDATAW` (Vista+ layout, 976 bytes on x64). `guidItem` is a
    /// `GUID` transcribed as bytes (never set; same size and offset).
    #[repr(C)]
    struct NOTIFYICONDATAW {
        cbSize: u32,
        hWnd: HWND,
        uID: u32,
        uFlags: u32,
        uCallbackMessage: u32,
        hIcon: isize,
        szTip: [u16; 128],
        dwState: u32,
        dwStateMask: u32,
        szInfo: [u16; 256],
        uTimeout: u32,
        szInfoTitle: [u16; 64],
        dwInfoFlags: u32,
        guidItem: [u8; 16],
        hBalloonIcon: isize,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: isize,
            hInstance: isize,
            lpParam: *mut c_void,
        ) -> HWND;
        fn DestroyWindow(hWnd: HWND) -> i32;
        fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> isize;
        fn LoadIconW(hInstance: isize, lpIconName: *const u16) -> isize;
        fn PeekMessageW(
            lpMsg: *mut MSG,
            hWnd: HWND,
            wMsgFilterMin: u32,
            wMsgFilterMax: u32,
            wRemoveMsg: u32,
        ) -> i32;
        fn TranslateMessage(lpMsg: *const MSG) -> i32;
        fn DispatchMessageW(lpMsg: *const MSG) -> isize;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
        fn GetLastError() -> u32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(dwMessage: u32, lpData: *mut NOTIFYICONDATAW) -> i32;
    }

    /// The hidden message-only window + icons (the exe's embedded aterm icon at
    /// the shell's two sizes, or the stock fallback) the balloons hang off.
    /// Windows have THREAD AFFINITY, so this lives in a `thread_local` — in practice only the
    /// single delivery thread ever touches it.
    struct Notifier {
        hwnd: HWND,
        hicon: isize,
        /// The notification's own icon (`hBalloonIcon`, drawn large): the
        /// `SM_CXICON` frame, or `hicon` when only the stock icon loaded.
        hballoon: isize,
        /// The aterm icons behind `hicon` and `hballoon`, destroyed after the
        /// window (the stock fallback is the system's and never destroyed).
        _icons: (
            Option<crate::win_icon::OwnedIcon>,
            Option<crate::win_icon::OwnedIcon>,
        ),
    }

    thread_local! {
        static NOTIFIER: OnceCell<Option<Notifier>> = const { OnceCell::new() };
        /// The session whose notification is showing: a click raises it. Set
        /// before each balloon ([`set_click_target`]) and taken by the click or
        /// when the balloon's icon goes.
        static CLICK_TARGET: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// The session the next balloon speaks for.
    pub(super) fn set_click_target(session: u64) {
        CLICK_TARGET.with(|target| target.set(Some(session)));
    }

    /// Show one balloon (blocking this thread for [`BALLOON_LINGER`]); silently
    /// a no-op when the hidden window could not be created.
    pub(super) fn deliver(title: &str, body: &str, silent: bool) {
        NOTIFIER.with(|cell| {
            if let Some(n) = cell.get_or_init(Notifier::create) {
                n.balloon(title, body, silent);
            }
        });
        CLICK_TARGET.with(Cell::take);
    }

    /// The hidden window's procedure: a click on the notification raises the tab
    /// that posted it, once; everything else is `DefWindowProcW`'s. Runs on the
    /// delivery thread, inside [`pump_for`].
    unsafe extern "system" fn notifier_proc(
        hwnd: HWND,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if msg == WM_TRAY_CALLBACK {
            if super::balloon_clicked(lparam)
                && let Some(session) = CLICK_TARGET.with(Cell::take)
            {
                super::raise_clicked(session);
            }
            return 0;
        }
        // SAFETY: the arguments are exactly those the system passed this
        // procedure for `hwnd`.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    impl Notifier {
        /// Register the (idempotent) window class and create the hidden
        /// message-only window; `None` on any failure (delivery then stays a
        /// silent drain, matching the pre-Windows-arm behavior).
        fn create() -> Option<Self> {
            let class: Vec<u16> = "aterm_notify_wnd\0".encode_utf16().collect();
            // SAFETY: `class` is a live NUL-terminated UTF-16 buffer for the
            // duration of both calls; the WNDCLASSW points only at it and at
            // `notifier_proc`. CreateWindowExW's returned window is owned by
            // this thread and destroyed in Drop.
            unsafe {
                let hinstance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: notifier_proc,
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinstance,
                    hIcon: 0,
                    hCursor: 0,
                    hbrBackground: 0,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                };
                if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                    return None;
                }
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    0,
                    hinstance,
                    std::ptr::null_mut(),
                );
                if hwnd == 0 {
                    return None;
                }
                // The notification area's icon and the notification's own: the
                // aterm icon compiled into THIS exe (resource ordinal 1, the
                // `1 ICON` crates/aterm/build.rs embeds, as the window's), each
                // the .ico frame the shell draws at the system DPI: `SM_CXSMICON`
                // in the notification area, `SM_CXICON` in the notification
                // (`NIIF_LARGE_ICON`). That is `LoadIconMetric`'s LIM_SMALL /
                // LIM_LARGE choice without importing it: it lives only in
                // comctl32 v6, which the dev exe and the test binaries have no
                // manifest for. `LoadIconW` gave the 32 px frame for the 16 px
                // slot. Falls back to the stock `IDI_APPLICATION` (the system's,
                // never destroyed) for a binary with no resource section
                // (build.rs already printed a banner).
                let (small_px, big_px) = crate::win_icon::icon_px(crate::win_icon::system_dpi());
                let tray = crate::win_icon::OwnedIcon::load(small_px);
                let large = crate::win_icon::OwnedIcon::load(big_px);
                let hicon = match &tray {
                    Some(icon) => icon.handle(),
                    None => LoadIconW(0, IDI_APPLICATION as *const u16),
                };
                let hballoon = large.as_ref().map_or(hicon, |icon| icon.handle());
                Some(Self {
                    hwnd,
                    hicon,
                    hballoon,
                    _icons: (tray, large),
                })
            }
        }

        /// One `NIM_ADD`(balloon) → pump → `NIM_DELETE` cycle.
        fn balloon(&self, title: &str, body: &str, silent: bool) {
            // SAFETY: all-zero is a valid NOTIFYICONDATAW (null handles, empty
            // NUL-terminated strings, no flags).
            let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_TIP | NIF_INFO | NIF_MESSAGE;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            if self.hicon != 0 {
                nid.uFlags |= NIF_ICON;
                nid.hIcon = self.hicon;
                // The notification's own icon (`NIIF_USER`): aterm's, not ⓘ.
                nid.hBalloonIcon = self.hballoon;
            }
            super::fold_to_utf16("aterm", &mut nid.szTip);
            super::fold_to_utf16(title, &mut nid.szInfoTitle);
            super::fold_to_utf16(body, &mut nid.szInfo);
            nid.dwInfoFlags = super::windows_info_flags(silent, self.hicon != 0);
            // SAFETY: `nid` is fully initialized and `hwnd` is a live window
            // owned by this thread.
            if unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) } == 0 {
                return;
            }
            // Keep the icon alive while the balloon shows, PUMPING messages so
            // the tray's liveness pings on our window are answered (an
            // unresponsive owner gets its icon reaped mid-balloon).
            pump_for(BALLOON_LINGER);
            // SAFETY: same `nid` identifies the icon added above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &mut nid) };
        }
    }

    impl Drop for Notifier {
        fn drop(&mut self) {
            // SAFETY: `hwnd` was created on this thread and is destroyed at
            // most once (Drop runs once; nothing else destroys it).
            unsafe { DestroyWindow(self.hwnd) };
        }
    }

    /// Dispatch pending thread messages, sleeping in short slices, until `dur`
    /// elapses.
    fn pump_for(dur: Duration) {
        let deadline = Instant::now() + dur;
        loop {
            // SAFETY: `msg` is a valid out-param; PeekMessageW fills it before
            // Translate/Dispatch read it.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
        }
    }
}

/// Escape a string for embedding inside an AppleScript double-quoted string
/// literal. Backslash and double-quote are the literal's two metacharacters and
/// are backslash-escaped; newlines/carriage-returns would break the single-line
/// literal and are folded to spaces; any other C0 control byte is also folded to
/// a space (notifications are one-liners — control bytes carry no display value
/// and only invite terminal/AppleScript quirks). Defined platform-independently
/// so it is unit-tested on every target.
#[cfg(any(target_os = "macos", test))]
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Sanitize + truncate a notification string into a fixed-size NUL-terminated
/// UTF-16 field (the Windows `NOTIFYICONDATAW.szInfo`/`szInfoTitle`/`szTip`
/// buffers). Control bytes are folded to spaces exactly as
/// [`applescript_escape`] folds them (no escaping — the bytes land in a plain
/// WCHAR buffer, never an interpreter); the copy stops at `out.len() - 1` units
/// WITHOUT splitting a surrogate pair (truncation is per `char`). Defined
/// platform-independently so it is unit-tested on every target.
#[cfg(any(windows, test))]
fn fold_to_utf16(s: &str, out: &mut [u16]) {
    let Some(cap) = out.len().checked_sub(1) else {
        return;
    };
    let mut len = 0usize;
    for c in s.chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        };
        let mut units = [0u16; 2];
        let encoded = c.encode_utf16(&mut units);
        if len + encoded.len() > cap {
            break;
        }
        out[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    out[len] = 0;
}

#[cfg(test)]
mod tests {
    use super::{
        NotifyMsg, applescript_escape, balloon_clicked, delivers, fold_controls, fold_to_utf16,
        markup_escape, windows_info_flags,
    };
    use std::collections::HashSet;

    /// aterm's own notices are opt-in (the owner, 2026-09-28, kept by the
    /// lead's ruling of 2026-10-18): an unset `desktop_alerts` reads off, and
    /// a set one wins. Whose banner they would arrive as is the facts
    /// `deliver` acts on — what macOS has said about aterm posting as itself,
    /// whether aterm runs from a bundle, terminal-notifier found, and the
    /// bundle's id read: a bundle's is never a subprocess's, whatever is
    /// installed, and a test build's voice is the inert answer, a bare binary
    /// without terminal-notifier.
    #[test]
    fn own_notices_default_off_and_say_whose_banner() {
        use crate::notice_center::Center;
        use crate::status_item::{NoticeFallback, NoticeVoice};
        let mut config = crate::app_config::Config::default();
        assert!(!config.desktop_alerts_or_default());
        config.desktop_alerts = Some(true);
        assert!(config.desktop_alerts_or_default());
        for center in [
            Center::Absent,
            Center::Unasked,
            Center::Refused,
            Center::Granted,
        ] {
            let voice = |fallback| NoticeVoice { center, fallback };
            for notifier in [true, false] {
                for named in [true, false] {
                    assert_eq!(
                        super::voice_of(center, true, notifier, named),
                        voice(NoticeFallback::Withheld { named }),
                        "bundled, notifier={notifier}"
                    );
                }
            }
            assert_eq!(
                super::voice_of(center, false, true, false),
                voice(NoticeFallback::Notifier)
            );
            assert_eq!(
                super::voice_of(center, false, false, false),
                voice(NoticeFallback::ScriptEditor)
            );
        }
        assert_eq!(
            super::own_voice(),
            Some(NoticeVoice {
                center: Center::Absent,
                fallback: NoticeFallback::ScriptEditor
            })
        );
    }

    /// A BUNDLED aterm POSTS ONLY AS ITSELF: a notice goes out as aterm once
    /// macOS has granted it, whatever else is installed; where macOS has not
    /// answered, it asks first only while aterm's own notices are on and no
    /// notice has asked yet, and otherwise the notice is held; and a bundle
    /// with no center to post through (one under a protected root, whose
    /// `Info.plist` aterm declines to read, or one whose center did not open)
    /// posts nowhere. NEGATIVE CONTROLS: once macOS has refused aterm, or
    /// answered what this build cannot read, nothing goes out whatever is
    /// installed — and no answer a bundle can give, with notices on or off,
    /// asked or not and terminal-notifier found or not, reaches a subprocess
    /// (Script Editor's banner, without terminal-notifier: the one the owner
    /// called annoying on 2026-09-28); while a bare binary, which has no
    /// center, still falls back to terminal-notifier where it was found, else
    /// to `osascript`.
    #[test]
    fn a_bundled_aterm_posts_only_as_itself() {
        use super::{Route, route};
        use crate::notice_center::{Center, status_answer};
        let answers = [
            Center::Granted,
            Center::Refused,
            Center::Unasked,
            Center::Absent,
            status_answer(1),
            status_answer(-1),
        ];
        for notifier in [true, false] {
            for alerts in [true, false] {
                for asked in [true, false] {
                    let case = format!("notifier={notifier} alerts={alerts} asked={asked}");
                    assert_eq!(
                        route(Center::Granted, true, notifier, alerts, asked),
                        Route::Center,
                        "{case}"
                    );
                    for refused in [Center::Refused, status_answer(1), status_answer(-1)] {
                        assert_eq!(
                            route(refused, true, notifier, alerts, asked),
                            Route::Withheld,
                            "{case}"
                        );
                    }
                    let unasked = if alerts && !asked {
                        Route::Ask
                    } else {
                        Route::Unasked
                    };
                    assert_eq!(
                        route(Center::Unasked, true, notifier, alerts, asked),
                        unasked,
                        "{case}"
                    );
                    assert_eq!(
                        route(Center::Absent, true, notifier, alerts, asked),
                        Route::Centerless,
                        "Absent while bundled: {case}"
                    );
                    for answer in answers {
                        assert!(
                            !matches!(
                                route(answer, true, notifier, alerts, asked),
                                Route::Notifier | Route::Osascript
                            ),
                            "a bundle reached a subprocess: {answer:?} {case}"
                        );
                    }
                    let fallback = if notifier {
                        Route::Notifier
                    } else {
                        Route::Osascript
                    };
                    assert_eq!(
                        route(Center::Absent, false, notifier, alerts, asked),
                        fallback,
                        "a bare binary: {case}"
                    );
                }
            }
        }
    }

    /// THE FIRST NOTICE ASKS macOS, ONCE, WAITS FOR ITS ANSWER, AND GOES OUT
    /// AS aterm ON A YES ([`super::way_for`], against stand-in arms: no
    /// center is reached). A bundle macOS has not answered, with aterm's own
    /// notices on (turned on in aterm.toml, say, so Notify Me never asked):
    /// the first notice asks, and on macOS's yes goes through the center, as
    /// the next does without asking. The stand-in asks as `live::ask` does:
    /// the ask returns at once, and macOS answers later, on a thread of its
    /// own, once the person clicks — so the first notice reads the center
    /// only because `way_for` waited. NEGATIVE CONTROLS: a route read
    /// straight after the ask, with no wait, is not the center; on a no the
    /// notice is withheld; with no answer (an ask that could not be made) it
    /// is held and the next notice asks no more; with aterm's own notices
    /// off, a program's notification asks nothing and is held; and a notice
    /// whose gates closed while the prompt was up (the person turned Notify
    /// Me off, or went to its tab, before the yes) is not posted, where the
    /// one whose gates stayed open is. None reaches a subprocess.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_first_notice_asks_macos_once() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
        use std::time::Duration;

        use super::{Route, route, way_for};
        use crate::notice_center::{Center, NoticeCenter};

        /// A hang detector for what must happen: macOS's answer once the
        /// person has clicked.
        const HANG: Duration = Duration::from_secs(60);
        /// How long the person reads macOS's prompt before clicking.
        const READING: Duration = Duration::from_millis(50);
        /// What macOS says now.
        static SAID: Mutex<Center> = Mutex::new(Center::Unasked);
        /// What macOS answers an ask (`None`: the ask was not made).
        static ANSWERS: Mutex<Option<Center>> = Mutex::new(None);
        /// How many times it was asked.
        static ASKS: AtomicU32 = AtomicU32::new(0);
        /// Whether the person has clicked macOS's prompt.
        static CLICKED: Mutex<bool> = Mutex::new(false);
        /// The click, to the asks waiting on it.
        static CLICK: Condvar = Condvar::new();
        /// The first notice's gates: whether it is still one to post.
        static WANTED: AtomicBool = AtomicBool::new(true);
        fn lock<T>(held: &Mutex<T>) -> MutexGuard<'_, T> {
            held.lock().unwrap_or_else(PoisonError::into_inner)
        }
        fn click() {
            *lock(&CLICKED) = true;
            CLICK.notify_all();
        }
        fn answer() -> Center {
            *lock(&SAID)
        }
        fn ask(then: Box<dyn FnOnce() + Send>) {
            ASKS.fetch_add(1, Ordering::AcqRel);
            let Some(answered) = *lock(&ANSWERS) else {
                return;
            };
            std::thread::spawn(move || {
                let clicked = *CLICK
                    .wait_timeout_while(lock(&CLICKED), HANG, |clicked| !*clicked)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                if clicked {
                    *lock(&SAID) = answered;
                    then();
                }
            });
        }
        fn macos(answers: Option<Center>) {
            *lock(&SAID) = Center::Unasked;
            *lock(&ANSWERS) = answers;
            *lock(&CLICKED) = false;
            ASKS.store(0, Ordering::Release);
            WANTED.store(true, Ordering::Release);
        }
        let center = NoticeCenter::stand_in(answer, ask);

        // The stand-in answers only once the person clicks, after the ask
        // has returned: a notice that read its route again at once would not
        // be posted.
        macos(Some(Center::Granted));
        let (answered, answer_came) = std::sync::mpsc::channel();
        center.ask(Box::new(move || {
            let _ = answered.send(());
        }));
        assert_eq!(
            route(center.answer(), true, true, true, true),
            Route::Unasked,
            "no wait: macOS has not answered yet"
        );
        click();
        assert_eq!(answer_came.recv_timeout(HANG), Ok(()));
        assert_eq!(
            route(center.answer(), true, true, true, true),
            Route::Center
        );

        // `closes`: the person turns aterm's notices off (or goes to the
        // first notice's tab) before clicking.
        let notices = |answers: Option<Center>, alerts: bool, closes: bool| {
            macos(answers);
            let person = (answers.is_some() && alerts).then(|| {
                std::thread::spawn(move || {
                    std::thread::sleep(READING);
                    if closes {
                        WANTED.store(false, Ordering::Release);
                    }
                    click();
                })
            });
            let asked = AtomicBool::new(false);
            let ways = [
                way_for(center, true, true, alerts, &asked, || {
                    WANTED.load(Ordering::Acquire)
                }),
                way_for(center, true, false, alerts, &asked, || true),
            ];
            if let Some(person) = person {
                person.join().expect("the person clicks");
            }
            (
                ways,
                ASKS.load(Ordering::Acquire),
                asked.load(Ordering::Acquire),
            )
        };

        assert_eq!(
            notices(Some(Center::Granted), true, false),
            ([Route::Center, Route::Center], 1, true),
            "a yes: the first asks and waits for it, and both go out as aterm"
        );
        assert_eq!(
            notices(Some(Center::Granted), true, true),
            ([Route::Lapsed, Route::Center], 1, true),
            "gates closed while macOS asked: the first is not posted, a later one is"
        );
        assert_eq!(
            notices(Some(Center::Refused), true, false),
            ([Route::Withheld, Route::Withheld], 1, true),
            "a no: withheld"
        );
        assert_eq!(
            notices(None, true, false),
            ([Route::Unasked, Route::Unasked], 1, true),
            "no answer: held, and asked once"
        );
        assert_eq!(
            notices(Some(Center::Granted), false, false),
            ([Route::Unasked, Route::Unasked], 0, false),
            "notices off: a program's notification asks nothing"
        );
    }

    /// terminal-notifier is found by its absolute path in the first
    /// directory that holds an executable one, past directories that hold
    /// none. NEGATIVE CONTROL: a file of that name that is not executable is
    /// no notifier, and no directory holding one finds none.
    #[cfg(unix)]
    #[test]
    fn terminal_notifier_is_found_where_it_is_installed() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("aterm-notifier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (empty, plain, brew) = (root.join("empty"), root.join("plain"), root.join("brew"));
        for dir in [&empty, &plain, &brew] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let write = |dir: &std::path::Path, mode: u32| {
            let path = dir.join("terminal-notifier");
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let _ = write(&plain, 0o644);
        let found = write(&brew, 0o755);
        let dirs = || vec![empty.clone(), plain.clone(), brew.clone()];
        assert_eq!(super::find_notifier(dirs()), Some(found));
        assert_eq!(
            super::find_notifier(vec![empty.clone(), plain.clone()]),
            None
        );
        assert_eq!(super::find_notifier(Vec::new()), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `desktop_alerts = false` (the delivery thread's `own_alerts`) drops
    /// every notice aterm wrote itself — the herald's and the consent path's —
    /// and nothing else: a program notification, which
    /// `allow_notifications` already admitted, is still delivered, and the
    /// focus suppression still holds for both origins. Negative control: the
    /// same own notice IS delivered with the switch on, so the drop is the
    /// switch's and not an accident of the notice.
    #[test]
    fn desktop_alerts_off_drops_only_aterms_own_notices() {
        let none = HashSet::new();
        let own = NotifyMsg::own(7, Some("aterm".into()), "waiting".into());
        let program = NotifyMsg::program(7, None, "build done".into());
        assert!(own.own && !program.own, "the constructors carry the origin");
        assert!(
            !delivers(&own, &none, false),
            "switch off: aterm's own is dropped"
        );
        assert!(
            delivers(&own, &none, true),
            "switch on: it is delivered (control)"
        );
        assert!(
            delivers(&program, &none, false),
            "a program's notification is not the switch's to drop"
        );
        let looking = HashSet::from([7]);
        assert!(
            !delivers(&program, &looking, false),
            "focus suppression holds"
        );
        assert!(
            !delivers(&own, &looking, true),
            "for aterm's own notices too"
        );
    }

    /// The consent path's instance-level notice (the warm-up's denial, which
    /// belongs to no tab) is not suppressed by the first tab's focus. Session
    /// `0` is that tab, and the suppression set starts as `{0}`; the notice
    /// used to carry `0`, so it was dropped whenever the first tab was focused,
    /// and its one-shot gate was already spent. NEGATIVE CONTROL: a notice that
    /// does name session `0` is suppressed by the same set.
    #[test]
    fn an_instance_level_consent_notice_is_not_suppressed_by_the_first_tab() {
        use crate::consent_observer::{AttentionEvent, AttentionGate};
        let notice = AttentionGate::new()
            .note(AttentionEvent::ProtectedEperm { session: None })
            .notice
            .expect("a first denial notifies");
        let first_tab_focused = HashSet::from([0u64]);
        let own = NotifyMsg::consent(&notice);
        assert!(own.own, "the consent door speaks in aterm's own voice");
        assert!(
            delivers(&own, &first_tab_focused, true),
            "the first tab's focus does not hide an instance-level notice"
        );
        let first_tab = NotifyMsg::own(0, None, "tab 0".into());
        assert!(
            !delivers(&first_tab, &first_tab_focused, true),
            "session 0's own notice is suppressed (control)"
        );
    }

    #[test]
    fn serious_notifications_set_windows_no_sound_flag() {
        const NIIF_NOSOUND: u32 = 0x10;
        for own_icon in [false, true] {
            assert_eq!(windows_info_flags(false, own_icon) & NIIF_NOSOUND, 0);
            assert_eq!(
                windows_info_flags(true, own_icon) & NIIF_NOSOUND,
                NIIF_NOSOUND
            );
        }
    }

    /// A Windows notification carries aterm's icon, large, whenever the exe
    /// has one (`NIIF_USER | NIIF_LARGE_ICON`); only a binary with no icon
    /// falls back to the stock ⓘ (`NIIF_INFO`). The icon type is the low
    /// nibble (`NIIF_ICON_MASK`), so exactly one is named.
    #[test]
    fn a_windows_notification_shows_aterms_icon() {
        const NIIF_ICON_MASK: u32 = 0xF;
        const NIIF_INFO: u32 = 0x1;
        const NIIF_USER: u32 = 0x4;
        const NIIF_LARGE_ICON: u32 = 0x20;
        let own = windows_info_flags(false, true);
        assert_eq!(own & NIIF_ICON_MASK, NIIF_USER);
        assert_eq!(own & NIIF_LARGE_ICON, NIIF_LARGE_ICON);
        let stock = windows_info_flags(false, false);
        assert_eq!(stock & NIIF_ICON_MASK, NIIF_INFO);
        assert_eq!(stock & NIIF_LARGE_ICON, 0);
    }

    /// The tray icon's callback names a click on the notification by its
    /// `lParam` (`NIN_BALLOONUSERCLICK`, `WM_USER + 5`); the balloon's other
    /// events (shown, hidden, timed out) and mouse moves over the icon are not
    /// clicks.
    #[test]
    fn only_a_click_on_the_notification_raises_its_tab() {
        const WM_USER: isize = 0x0400;
        const WM_MOUSEMOVE: isize = 0x0200;
        const WM_LBUTTONUP: isize = 0x0202;
        assert!(balloon_clicked(WM_USER + 5));
        for other in [
            WM_USER + 2,
            WM_USER + 3,
            WM_USER + 4,
            WM_MOUSEMOVE,
            WM_LBUTTONUP,
        ] {
            assert!(!balloon_clicked(other), "{other:#x}");
        }
    }

    /// The AppleScript string literal: text passes through, every backslash
    /// and quote is escaped, so a crafted body cannot close the literal and
    /// run a command, and control bytes fold to spaces.
    #[test]
    fn applescript_escape_neutralizes_quotes_backslashes_and_controls() {
        for (case, input, want) in [
            ("plain", "build finished", "build finished"),
            ("unicode", "café — 100% ✅", "café — 100% ✅"),
            ("quotes", r#"say "hi""#, r#"say \"hi\""#),
            ("a backslash", r"a\b", r"a\\b"),
            // `\"` -> `\\\"`: the escaped backslash, then the escaped quote.
            ("a backslash before a quote", "\\\"", "\\\\\\\""),
            ("a newline", "line1\nline2", "line1 line2"),
            ("a CRLF", "a\r\nb", "a  b"),
            ("a tab", "tab\there", "tab here"),
            ("a bell", "bell\x07x", "bell x"),
            ("an escape", "esc\x1b[0m", "esc [0m"),
            // NUL folds like any control: not dropped, not a terminator.
            ("a NUL", "a\0b", "a b"),
            (
                "the break-out",
                r#"" & (do shell script "rm -rf ~") & ""#,
                r#"\" & (do shell script \"rm -rf ~\") & \""#,
            ),
        ] {
            assert_eq!(applescript_escape(input), want, "{case}");
        }
    }

    /// The Windows notification field: a NUL-terminated UTF-16 copy, control
    /// bytes folded to spaces as the AppleScript arm folds them, truncated per
    /// `char` to leave the NUL its unit (never a lone lead surrogate), and the
    /// units past the NUL untouched.
    #[test]
    fn fold_to_utf16_folds_truncates_per_char_and_terminates() {
        for (case, input, capacity, want) in [
            ("copies", "build done ✅", 16, "build done ✅"),
            ("folds controls", "a\r\nb\x07c", 16, "a  b c"),
            ("truncates, keeping the NUL", "abcdef", 4, "abc"),
            ("drops a pair that does not fit whole", "a🎉", 3, "a"),
            ("keeps a pair with room", "a🎉", 8, "a🎉"),
            ("only the NUL fits", "abc", 1, ""),
        ] {
            let mut buf = vec![0xFFFFu16; capacity];
            fold_to_utf16(input, &mut buf);
            let mut expected: Vec<u16> = want.encode_utf16().collect();
            expected.push(0);
            expected.resize(capacity, 0xFFFF);
            assert_eq!(buf, expected, "{case}");
        }
        // A zero-length field has nothing to write and must not panic.
        fold_to_utf16("abc", &mut []);
    }

    /// A program's body cannot be markup to a `body-markup` server: `&`, `<` and
    /// `>` are entities, so `<a href=…>` is text; control bytes fold to spaces
    /// and a newline stays one.
    #[test]
    fn a_freedesktop_body_is_escaped_markup_and_folded_controls() {
        assert_eq!(markup_escape("build done ✅"), "build done ✅");
        assert_eq!(
            markup_escape("<a href=\"x\">click</a> & <b>"),
            "&lt;a href=\"x\"&gt;click&lt;/a&gt; &amp; &lt;b&gt;"
        );
        assert_eq!(
            markup_escape("&amp;"),
            "&amp;amp;",
            "an entity is escaped too"
        );
        assert_eq!(markup_escape("a\nb\r\tc\x1b[0m\x7f"), "a\nb  c [0m ");
        assert_eq!(fold_controls("t\x07i\ntle"), "t i\ntle");
    }

    /// The `Notify` call a Linux host sends, end to end through a private
    /// `dbus-daemon` (never the session's bus) to a stand-in server holding
    /// `org.freedesktop.Notifications`: the eight arguments in order, the body
    /// escaped, urgency normal with `suppress-sound` only in serious mode, and
    /// a second notification over the same cached connection. COULD NOT RUN where
    /// no `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_notification_is_a_notify_call_on_the_session_bus() {
        use aterm_dbus::{Connection, Kind, Message, Value};
        use std::io::BufRead as _;

        /// A wait for something that must happen: a hang detector, not a budget.
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);

        let Some(daemon) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("dbus-daemon"))
                .find(|p| p.is_file())
        }) else {
            aterm_spec::host::could_not_run(
                "no dbus-daemon on PATH, so no live Notify round trip here; install the \
                 distribution's dbus (dbus-daemon) to run it",
            );
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let socket = scratch.path().join("bus");
        let conf = scratch.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>\n",
                socket.display()
            ),
        )
        .unwrap();
        struct Daemon(aterm_uds::spawnfd::Child);
        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Daemon(
            aterm_uds::spawnfd::command(&daemon)
                .arg(format!("--config-file={}", conf.display()))
                .arg("--nofork")
                .arg("--print-address")
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        std::io::BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let addresses = aterm_dbus::address::parse(line.trim()).unwrap();

        let mut server = Connection::open(&addresses).unwrap();
        let owned = server
            .call(
                Message::method_call(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "RequestName",
                )
                .unwrap()
                .with_body(vec![
                    Value::from("org.freedesktop.Notifications"),
                    Value::U32(0),
                ]),
                HANG,
            )
            .unwrap();
        assert_eq!(
            owned.body,
            vec![Value::U32(1)],
            "the stand-in owns the name"
        );
        let served = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while seen.len() < 2 {
                let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
                if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                    server.reply(&msg, vec![Value::U32(7)]).unwrap();
                    seen.push(msg);
                }
            }
            seen
        });

        let mut bus = super::freedesktop::Bus { conn: None };
        let open = || Connection::open(&addresses);
        let loud = super::freedesktop::notify_call("build", "done <b>&</b>", false).unwrap();
        assert!(bus.notify(&loud, &open), "the server took the first one");
        let quiet = super::freedesktop::notify_call("aterm", "serious", true).unwrap();
        assert!(
            bus.notify(&quiet, &open),
            "and the second, on the cached connection"
        );
        let seen = served.join().unwrap();

        let hints = |msg: &Message| {
            msg.body[6]
                .as_dict()
                .expect("a{sv} hints")
                .entries()
                .iter()
                .map(|(k, v)| (k.as_str().unwrap().to_owned(), v.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            seen[0].body[..6],
            [
                Value::from("aterm"),
                Value::U32(0),
                Value::from("aterm"),
                Value::from("build"),
                Value::from("done &lt;b&gt;&amp;&lt;/b&gt;"),
                Value::strings(std::iter::empty::<String>()),
            ]
        );
        assert_eq!(seen[0].body[7], Value::I32(-1));
        assert_eq!(
            hints(&seen[0]),
            vec![
                (
                    "desktop-entry".to_owned(),
                    Value::variant(Value::from("aterm"))
                ),
                ("urgency".to_owned(), Value::variant(Value::Byte(1))),
            ]
        );
        assert_eq!(
            hints(&seen[1]),
            vec![
                (
                    "desktop-entry".to_owned(),
                    Value::variant(Value::from("aterm"))
                ),
                ("urgency".to_owned(), Value::variant(Value::Byte(1))),
                (
                    "suppress-sound".to_owned(),
                    Value::variant(Value::Bool(true))
                ),
            ],
            "serious mode is suppress-sound at normal urgency, never urgency low"
        );
        assert_eq!(seen[1].body[4], Value::from("serious"));
    }
}
