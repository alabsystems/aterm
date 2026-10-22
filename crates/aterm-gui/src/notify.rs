// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native desktop notification delivery for OSC 9 / OSC 99 / OSC 777.
//!
//! The engine (`aterm-core`) already PARSES these escapes and fires
//! `set_notification_callback` (OSC 9 / 777, a plain body string) and
//! `set_advanced_notification_callback` (OSC 99 / kitty, a structured
//! [`Notification`] with title + body), gated by the host's
//! `authorize_notifications`. This module is the GUI side that turns those
//! callbacks into real macOS / Windows / Linux notifications, with two hard
//! rules borrowed from the OSC 52 clipboard path:
//!
//! 1. Never block the engine. The callbacks run on a tab's PTY reader thread under the
//!    `Terminal` lock, so each only `try_send`s a [`NotifyMsg`] onto a bounded channel
//!    ([`NOTIFY_QUEUE_CAP`]); a delivery thread ([`spawn_delivery`]) runs the notifier.
//! 2. Untrusted content. A body is program output (over SSH, the remote's): the osascript
//!    fallback escapes it into an AppleScript literal ([`applescript_escape`]) passed as one
//!    argv entry, never a shell string; `terminal-notifier` takes argv directly; the Windows
//!    path writes fixed-size WCHAR fields, so [`fold_to_utf16`] only folds control bytes and
//!    truncates.
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
//! the first-party `aterm-dbus` client, from a bus thread of its own that the
//! delivery thread starts at its first notice and hands each notice to: one
//! lazily opened connection, a bounded call, every failure swallowed (no session
//! bus, no notification server). A notice about a session carries the `default`
//! action, and a click on it goes to that session's tab and pane as on macOS and
//! Windows: the bus thread hears the server's `ActionInvoked` for the id `Notify`
//! returned and wakes the App (`Wake::NoticeClicked`), with the activation token
//! the server sent just before it (`ActivationToken`, notification spec 1.2) for
//! the raise ([`freedesktop`]). A HEADLESS process delivers nothing — it presents no
//! OS UI, and a test's or an agent's private instance often still carries the
//! person's real session-bus address. Serious mode is the standard
//! `suppress-sound` hint at normal urgency, never `urgency = low`, which GNOME
//! Shell shows with no banner at all.
//!
//! Identity and consent (macOS): both delivery paths are subprocesses that run with aterm as
//! their TCC "responsible" app, so they ask the system for things in aterm's name at a
//! program's request. Program notifications therefore sit behind the fail-closed
//! `allow_notifications` opt-in (`handler_osc_notify.rs` fires no callback while it is off).
//! On macOS 26 an osascript under aterm raises only WindowServer's `kTCCServiceListenEvent`
//! preflight, which never prompts. An in-process `UNUserNotificationCenter` path would move the
//! identity to the bundle and need its own notification consent.
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
//! the lead's ). Once on they are needs only
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

/// One pending notification handed from a tab's engine callback to the delivery thread.
pub(crate) struct NotifyMsg {
    /// Originating session, for focus suppression. Only a host with real delivery reads it.
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub session: u64,
    /// Notification title (OSC 99 carries one; OSC 9/777 do not — `None`).
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub title: Option<String>,
    /// Notification body.
    pub body: String,
    /// `true` for a notice aterm wrote itself ([`Self::own`]), governed by `desktop_alerts`;
    /// `false` for a program's ([`Self::program`]), which `allow_notifications` admitted.
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub own: bool,
}

impl NotifyMsg {
    /// One program notification (OSC 9/99/777), already admitted by `allow_notifications`.
    /// Off a delivery host the session and title have no reader and are dropped here.
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

/// The delivery thread's one decision for a dequeued message: an own notice while
/// `desktop_alerts` is off is dropped, and so is any notice whose session is in the focus set
/// `suppressed`. Pure, so the gate is tested without a notifier.
#[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
fn delivers(msg: &NotifyMsg, suppressed: &HashSet<u64>, own_alerts: bool) -> bool {
    if msg.own && !own_alerts {
        return false;
    }
    !suppressed.contains(&msg.session)
}

/// Bound on the delivery queue. The delivery thread blocks ~100 ms per notifier subprocess, so
/// a flood of OSC 9/99/777 must grow neither memory nor subprocesses: producers `try_send` and
/// drop on `Full`. Notifications are distinct (unlike OSC 52's last-writer-wins clipboard), so
/// overflow is dropped, not coalesced.
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
            // (its `userInfo` names the session) and one on Linux's does (its
            // `default` action, `freedesktop`).
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
            #[cfg(windows)]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                silent.load(Ordering::Acquire),
            );
            #[cfg(target_os = "linux")]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                silent.load(Ordering::Acquire),
                msg.session,
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
        Route::Centerless if crate::signing_identity::running_bundle_id().is_none() => {
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

    // Fallback: osascript. Body and title land in an AppleScript string literal, so both are
    // escaped, and the script is one `-e` argv entry, never a shell string.
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
                crate::signing_identity::running_bundle_id().is_some(),
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
    // An empty `szInfo` hides the balloon entirely, so a title-only OSC 99
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
/// tab (`Wake::NoticeClicked`: Windows' balloon, aterm's own macOS notice
/// through `notice_center`'s delegate, and a Linux notice's `default` action
/// through [`freedesktop`]'s bus thread), and macOS's answer to a notice's ask
/// reaches the Notify Me hover (`Wake::NoticeVoice`). Installed once at
/// launch; before that, and in a headless App, a wake goes nowhere. A test
/// build has none: its wakes are recorded ([`take_wakes`]).
#[cfg(all(any(windows, target_os = "macos", target_os = "linux"), not(test)))]
static PROXY: std::sync::OnceLock<aterm_window::event_loop::EventLoopProxy<crate::Wake>> =
    std::sync::OnceLock::new();

/// Install the proxy a notice's wakes ride. First install wins. Only Windows'
/// notifier, macOS's center and Linux's bus thread wake the App; elsewhere, and
/// in a test build, this is a no-op.
pub(crate) fn install_proxy(proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
    #[cfg(all(any(windows, target_os = "macos", target_os = "linux"), not(test)))]
    let _ = PROXY.set(proxy.clone());
    #[cfg(not(all(any(windows, target_os = "macos", target_os = "linux"), not(test))))]
    let _ = proxy;
}

/// Hand `wake` to the App through the installed proxy ([`PROXY`]); a test
/// build records it for [`take_wakes`] instead.
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn wake(wake: crate::Wake) {
    #[cfg(not(test))]
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(wake);
    }
    #[cfg(test)]
    WAKES.with(|wakes| wakes.borrow_mut().push(wake));
}

#[cfg(all(any(windows, target_os = "macos", target_os = "linux"), test))]
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
/// the session bus, posted by the bus thread ([`freedesktop::Poster`]) that this
/// delivery thread starts at its first notice. Runs ONLY on the delivery thread,
/// which waits while the bus thread takes the notice, as it waited when it
/// posted itself (each call is bounded by [`freedesktop::CALL_TIMEOUT`]).
/// Best-effort: no session bus, no notification server or a refused call is
/// swallowed. Never from a headless process (the module doc's *Linux*
/// paragraph), and never from a unit test, whose process carries the
/// developer's own session-bus address (the live tests drive
/// [`freedesktop::Poster`] and [`freedesktop::Bus`] against a private daemon
/// instead).
#[cfg(target_os = "linux")]
fn deliver(title: Option<&str>, body: &str, silent: bool, session: u64) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    freedesktop::POSTER.with(|poster| {
        if let Some(poster) = poster.get_or_init(freedesktop::Poster::session) {
            post(poster, title, body, silent, session);
        }
    });
}

/// The session an instance-level notice names (`consent_observer`'s
/// `AttentionNotice`): no session has it, so it has no tab to go to.
#[cfg(target_os = "linux")]
const NO_SESSION: u64 = u64::MAX;

/// Hand one notice to the bus thread. A notice about a session carries the
/// `default` action, and a click on it goes to that session's tab
/// ([`freedesktop::Clicks`]). An instance-level one carries no action, so the
/// server's own activation of aterm answers its click (GNOME Shell raises the
/// window aterm was last used in).
#[cfg(target_os = "linux")]
fn post(poster: &freedesktop::Poster, title: Option<&str>, body: &str, silent: bool, session: u64) {
    let session = (session != NO_SESSION).then_some(session);
    if let Ok(call) =
        freedesktop::notify_call(title.unwrap_or("aterm"), body, silent, session.is_some())
    {
        poster.post(call, session);
    }
}

/// Deliver one notification on the calling thread, at once (blocking up to
/// twice [`freedesktop::CALL_TIMEOUT`]: the bus's handshake, then the call,
/// each bounded by it): the Linux arm of `menu::notify`, whose callers (a
/// launch that cannot start, a file that cannot open) may end the process right
/// after it, before the delivery thread would run. A file that cannot open
/// calls it on the event loop, which a bus that stopped answering would
/// otherwise hold for aterm-dbus's 25 s handshake bound. A desktop launch has
/// no terminal, so its stderr reaches only the journal and this is what the
/// person sees. It is about no tab, so it carries no action and the server's
/// own activation of aterm answers a click. The same guards as [`deliver`].
#[cfg(target_os = "linux")]
pub(crate) fn deliver_now(title: &str, body: &str) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let (Ok(addresses), Ok(call)) = (
        aterm_dbus::address::session_bus(),
        freedesktop::notify_call(title, body, false, false),
    ) else {
        return;
    };
    freedesktop::notify_now(&call, &addresses, freedesktop::CALL_TIMEOUT);
}

/// `org.freedesktop.Notifications` over the first-party D-Bus client: the
/// `Notify` call, the bus thread that posts each notice and hears the server's
/// signals about it, and what a click on one is.
///
/// One thread holds aterm's one connection, both to post and to hear, so a
/// server that sends its signals only to the connection that posted the
/// notification reaches it as one that broadcasts them (GNOME Shell's) does.
/// It waits on nothing but `poll(2)` over that connection and a
/// wake socket the delivery thread rings with each notice, so it costs nothing
/// while idle, and the UI thread never touches the bus: a click reaches the App
/// as a wake through the event loop's proxy.
#[cfg(target_os = "linux")]
mod freedesktop {
    use std::cell::OnceCell;
    use std::collections::VecDeque;
    use std::io::{ErrorKind, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::time::Duration;

    use aterm_dbus::address::Address;
    use aterm_dbus::{Connection, Kind, Message, Type, Value};

    /// The notification server's well-known name, object and interface.
    const SERVER: &str = "org.freedesktop.Notifications";
    const OBJECT: &str = "/org/freedesktop/Notifications";

    /// The server's signals the bus thread asks the bus for: a click
    /// (`ActionInvoked`), the activation token a server sends just before one
    /// (`ActivationToken`, specification 1.2) and a close (`NotificationClosed`).
    const SIGNALS: &str = "type='signal',interface='org.freedesktop.Notifications',\
                           path='/org/freedesktop/Notifications'";

    /// The action a click on a notification's body invokes (the specification's
    /// `default`), and its label, which a server that lists actions may show.
    const DEFAULT_ACTION: &str = "default";
    const DEFAULT_LABEL: &str = "Go to tab";

    /// How many notices about a session the bus thread remembers. Past it the
    /// oldest is forgotten, so a server that never says one closed (or a tray
    /// nobody clears) holds no more than this.
    pub(super) const POSTED_CAP: usize = 64;

    /// How long the bus thread waits for the server's answer to a notice
    /// ([`Poster::post`]), and [`super::deliver_now`] for the bus's handshake
    /// and then the answer ([`notify_now`]). A running server
    /// answers `Notify` at once; the bound only keeps a wedged bus or a slow
    /// server from holding the thread (and the delivery thread waiting on it:
    /// the bounded queue drops what arrives meanwhile).
    pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(2);

    thread_local! {
        /// The delivery thread's end of the bus thread, started at its first
        /// notice; `None` when the OS gave no socket pair or thread.
        pub(super) static POSTER: OnceCell<Option<Poster>> = const { OnceCell::new() };
    }

    /// One `Notify(susssasa{sv}i)` call. `summary` is plain text by the
    /// specification; `body` is markup to a server that advertises
    /// `body-markup`, so it is escaped ([`super::markup_escape`]). A
    /// `clickable` notice offers the `default` action, which a click on its
    /// body invokes, and nothing else. The hints name aterm's desktop entry
    /// (`aterm.desktop`, which tools/install.sh lays) so the shell attributes
    /// the notification to aterm, keep urgency NORMAL, and add
    /// `suppress-sound` in serious mode (`silent`).
    pub(super) fn notify_call(
        summary: &str,
        body: &str,
        silent: bool,
        clickable: bool,
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
        let actions: &[&str] = if clickable {
            &[DEFAULT_ACTION, DEFAULT_LABEL]
        } else {
            &[]
        };
        Ok(
            Message::method_call(Some(SERVER), OBJECT, Some(SERVER), "Notify")?.with_body(vec![
                Value::from("aterm"),
                Value::U32(0),
                Value::from("aterm"),
                Value::Str(super::fold_controls(summary)),
                Value::Str(super::markup_escape(body)),
                Value::strings(actions.iter().copied()),
                Value::dict(Type::Str, Type::Variant, hints)?,
                Value::I32(-1),
            ]),
        )
    }

    /// Where a server put one notice: the id `Notify` returned, and the unique
    /// name of the connection that answered (the server's: its signals come
    /// from that name, so a server started since that hands out the same id
    /// again is not taken for this one).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Posted {
        pub(super) server: Option<String>,
        pub(super) id: u32,
    }

    /// What the bus thread observed. A connection's names and notice ids belong
    /// to that connection's bus epoch: a replacement may reuse both.
    pub(super) enum Observation<'a> {
        ConnectionReset,
        Message(&'a Message),
    }

    /// A session-bus connection, opened on first use and dialled again when the
    /// bus has closed it.
    pub(super) struct Bus {
        pub(super) conn: Option<Connection>,
    }

    impl Bus {
        /// Send `call` and wait up to `bound` for its answer, best-effort. `open`
        /// dials the bus (`Connection::session` on the bus thread, a dial
        /// bounded as the call is for [`notify_now`]; a test hands its private
        /// daemon). A connection that fails — closed by the bus, a socket error,
        /// a call the bus did not take whole by the deadline — is dropped; when
        /// it was a cached one, a fresh one is dialled and the call sent once
        /// more, since a long-lived connection the bus restarted under is the
        /// expected way to find it dead. An error the SERVER answered (no
        /// notification server on this bus) leaves the connection as it is: the
        /// bus is fine. So does a call that went out whole and was not answered
        /// by the deadline ([`aterm_dbus::Error::NoReply`]): a slow server (a
        /// stalled shell, one the bus is still starting) is no dead connection.
        /// Dropping it would forget where each notice still showing goes, and
        /// sending again would show this one twice; this one is left unmapped
        /// instead, so a click on it goes nowhere, and its late answer reaches
        /// `heard` as any other message. Returns where the server put the
        /// notification, `None` when it took none or did not say in time. What
        /// the bus sent the connection before the answer goes to `heard`, in
        /// order.
        pub(super) fn notify(
            &mut self,
            call: &Message,
            bound: Duration,
            open: &dyn Fn() -> aterm_dbus::Result<Connection>,
            heard: &mut dyn FnMut(Observation<'_>),
        ) -> Option<Posted> {
            for _ in 0..2 {
                // What the bus sent since the last call (a click on an earlier
                // notice, `NameAcquired`) is heard first, which also keeps the
                // connection's retained-message cap from filling and finds a
                // connection the bus has closed.
                self.drain(heard);
                let dialled = self.conn.is_none();
                if dialled {
                    self.conn = open().ok();
                    if self.conn.is_some() {
                        // Before even a retained message from the new connection
                        // can be heard (including this method's retry).
                        heard(Observation::ConnectionReset);
                    }
                }
                let conn = self.conn.as_mut()?;
                match conn.call(call.clone(), bound) {
                    Ok(answer) => {
                        let id = answer.body.first().and_then(Value::as_u32)?;
                        return Some(Posted {
                            server: answer.sender,
                            id,
                        });
                    }
                    Err(aterm_dbus::Error::Remote { .. } | aterm_dbus::Error::NoReply) => {
                        return None;
                    }
                    Err(_) => {
                        self.end_connection(heard);
                        if dialled {
                            return None;
                        }
                    }
                }
            }
            None
        }

        /// Hand `heard` every message the bus has sent this connection, without
        /// waiting. One this side cannot read is skipped; a connection that
        /// failed is dropped, to be dialled again at the next notice.
        pub(super) fn drain(&mut self, heard: &mut dyn FnMut(Observation<'_>)) {
            while let Some(conn) = self.conn.as_mut() {
                match conn.recv(Some(Duration::ZERO)) {
                    Ok(Some(msg)) => heard(Observation::Message(&msg)),
                    Ok(None) => return,
                    Err(
                        aterm_dbus::Error::Malformed { .. } | aterm_dbus::Error::Unsupported(_),
                    ) => {}
                    Err(_) => self.end_connection(heard),
                }
            }
        }

        /// Revoke this connection's click mappings at the loss itself, before
        /// a retry or a later notice can expose another connection's traffic.
        fn end_connection(&mut self, heard: &mut dyn FnMut(Observation<'_>)) {
            if let Some(conn) = self.conn.take() {
                drop(conn);
                heard(Observation::ConnectionReset);
            }
        }
    }

    /// Post `call` on a connection of its own to `addresses`, the dial and the
    /// call each bounded by `bound`: [`super::deliver_now`]'s delivery
    /// ([`CALL_TIMEOUT`]), which must not wait out aterm-dbus's 25 s handshake
    /// bound on the event loop. Where the server put the notice, as
    /// [`Bus::notify`] says.
    pub(super) fn notify_now(
        call: &Message,
        addresses: &[Address],
        bound: Duration,
    ) -> Option<Posted> {
        let open = || Connection::open_within(addresses, bound);
        Bus { conn: None }.notify(call, bound, &open, &mut |_| {})
    }

    /// One notice about a session, as the bus thread remembers it.
    struct Notice {
        at: Posted,
        session: u64,
        /// The activation token the server sent for the click it is about to
        /// report (`ActivationToken`), until that click takes it.
        token: Option<String>,
    }

    /// What a click on a notice is: the bus thread's memory of the notices it
    /// posted about a session, read against each message the bus sends it.
    #[derive(Default)]
    pub(super) struct Clicks {
        posted: VecDeque<Notice>,
    }

    impl Clicks {
        /// The server put a notice `at`. Replacing a key revokes its old target
        /// and token, even when the new notice names no session.
        pub(super) fn posted(&mut self, at: Posted, session: Option<u64>) {
            self.posted.retain(|notice| notice.at != at);
            let Some(session) = session else { return };
            if self.posted.len() >= POSTED_CAP {
                self.posted.pop_front();
            }
            self.posted.push_back(Notice {
                at,
                session,
                token: None,
            });
        }

        /// The common bus-observation path, before any message can route a
        /// click. No label or activation token survives a connection epoch.
        pub(super) fn observe(
            &mut self,
            observation: Observation<'_>,
        ) -> Option<(u64, Option<String>)> {
            match observation {
                Observation::ConnectionReset => {
                    self.posted.clear();
                    None
                }
                Observation::Message(msg) => self.heard(msg),
            }
        }

        /// One message from the bus: the session a click goes to, with the
        /// activation token the server sent for it, where it is a click on one
        /// of these notices (`ActionInvoked` with the `default` action). The
        /// server's `ActivationToken` for one is kept for its click, and
        /// `NotificationClosed` forgets it. A signal about another program's
        /// notification (GNOME Shell broadcasts them all), from another sender
        /// than the server that took the notice, or naming an action aterm
        /// never offered, is no click; nor is anything else.
        pub(super) fn heard(&mut self, msg: &Message) -> Option<(u64, Option<String>)> {
            if msg.kind != Kind::Signal
                || msg.interface.as_deref() != Some(SERVER)
                || msg.path.as_ref().map(|path| path.as_str()) != Some(OBJECT)
            {
                return None;
            }
            let id = msg.body.first().and_then(Value::as_u32)?;
            let index = self
                .posted
                .iter()
                .position(|notice| notice.at.id == id && notice.at.server == msg.sender)?;
            let arg = msg.body.get(1).and_then(Value::as_str);
            match msg.member.as_deref()? {
                "ActivationToken" => {
                    self.posted[index].token = arg.map(str::to_owned);
                    None
                }
                "ActionInvoked" if arg == Some(DEFAULT_ACTION) => {
                    let notice = &mut self.posted[index];
                    Some((notice.session, notice.token.take()))
                }
                "NotificationClosed" => {
                    self.posted.remove(index);
                    None
                }
                _ => None,
            }
        }
    }

    /// One notice for the bus thread, the session a click on it goes to, and
    /// how long the thread waits for the server's answer.
    struct Post {
        call: Message,
        session: Option<u64>,
        bound: Duration,
    }

    /// The delivery thread's end of the bus thread: dropped, it ends the thread.
    pub(super) struct Poster {
        posts: SyncSender<Post>,
        wake: UnixStream,
    }

    impl Poster {
        /// The product's bus thread: the session bus, and a click handed to the
        /// App through the event loop's proxy (`super::wake`).
        pub(super) fn session() -> Option<Self> {
            Self::start(Connection::session, super::wake)
        }

        /// Start a bus thread. At its first notice it dials with `open` (and
        /// again after the bus closes the connection), asks the bus for the
        /// server's signals ([`SIGNALS`]) before it posts, and hands `clicked`
        /// the wake a click on a notice about a session is
        /// (`Wake::NoticeClicked`). `None`, logged, when the OS gives no socket
        /// pair or thread.
        pub(super) fn start(
            open: impl Fn() -> aterm_dbus::Result<Connection> + Send + 'static,
            clicked: impl Fn(crate::Wake) + Send + 'static,
        ) -> Option<Self> {
            let pair = UnixStream::pair().and_then(|(wake, rung)| {
                wake.set_nonblocking(true)?;
                rung.set_nonblocking(true)?;
                Ok((wake, rung))
            });
            let (wake, rung) = match pair {
                Ok(pair) => pair,
                Err(e) => {
                    aterm_log::warn!("notices: no wake socket ({e}); none is posted");
                    return None;
                }
            };
            // One notice in hand: the delivery thread waits while the bus
            // thread posts, so the bounded queue before it still drops a flood.
            let (posts, taken) = mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("aterm-notices".to_owned())
                .spawn(move || run(&open, &clicked, &taken, &rung));
            if let Err(e) = spawned {
                aterm_log::warn!("notices: no bus thread ({e}); none is posted");
                return None;
            }
            Some(Self { posts, wake })
        }

        /// Hand the bus thread one `Notify` call, a click on which goes to
        /// `session`, and ring it. The thread waits [`CALL_TIMEOUT`] for the
        /// server's answer.
        pub(super) fn post(&self, call: Message, session: Option<u64>) {
            self.post_within(call, session, CALL_TIMEOUT);
        }

        /// [`Poster::post`], the server's answer awaited for `bound`.
        pub(super) fn post_within(&self, call: Message, session: Option<u64>, bound: Duration) {
            if self
                .posts
                .send(Post {
                    call,
                    session,
                    bound,
                })
                .is_ok()
            {
                // Non-blocking: a full socket already holds a ring.
                let _ = (&self.wake).write(&[1]);
            }
        }
    }

    /// The bus thread: post each notice handed over, hear what the bus sends,
    /// and otherwise wait in `poll(2)` for either. Ends when the delivery
    /// thread's end ([`Poster`]) is dropped, or (logged) when poll fails.
    fn run(
        open: &dyn Fn() -> aterm_dbus::Result<Connection>,
        clicked: &dyn Fn(crate::Wake),
        posts: &Receiver<Post>,
        rung: &UnixStream,
    ) {
        fn hear(clicks: &mut Clicks, observation: Observation<'_>, clicked: &dyn Fn(crate::Wake)) {
            if let Some((session, activation)) = clicks.observe(observation) {
                clicked(crate::Wake::NoticeClicked {
                    session,
                    activation,
                });
            }
        }

        let subscribed = || {
            let mut conn = open()?;
            if let Err(e) = conn.add_match(SIGNALS, CALL_TIMEOUT) {
                aterm_log::debug!("notices: the bus refused {SIGNALS} ({e}); clicks go nowhere");
            }
            Ok(conn)
        };
        let mut bus = Bus { conn: None };
        let mut clicks = Clicks::default();
        loop {
            loop {
                match posts.try_recv() {
                    Ok(post) => {
                        let posted = bus.notify(&post.call, post.bound, &subscribed, &mut |msg| {
                            hear(&mut clicks, msg, clicked);
                        });
                        if let Some(at) = posted {
                            clicks.posted(at, post.session);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            // What came meanwhile, a call's own notice's signals among it: the
            // call kept them for `recv`, so the socket may not say so.
            bus.drain(&mut |msg| hear(&mut clicks, msg, clicked));
            let mut fds = vec![rung.as_raw_fd()];
            fds.extend(bus.conn.as_ref().map(|conn| conn.as_raw_fd()));
            if let Err(e) = wait(&fds) {
                aterm_log::warn!("notices: the bus thread's poll failed ({e}); none is posted");
                return;
            }
            let mut sink = [0u8; 64];
            while matches!((&*rung).read(&mut sink), Ok(n) if n > 0) {}
        }
    }

    /// Wait until one of `fds` is readable or hung up.
    fn wait(fds: &[RawFd]) -> std::io::Result<()> {
        let mut polls: Vec<libc::pollfd> = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let count = libc::nfds_t::try_from(polls.len()).unwrap_or(libc::nfds_t::MAX);
        loop {
            // SAFETY: `polls` is an initialised array of `count` pollfd records,
            // exclusively borrowed for the call; its descriptors are the wake
            // socket's and the connection's, which the caller holds open across
            // it. -1 waits until one is ready or a signal interrupts.
            if unsafe { libc::poll(polls.as_mut_ptr(), count, -1) } >= 0 {
                return Ok(());
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != ErrorKind::Interrupted {
                return Err(e);
            }
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

    /// How long the transient icon (and so the balloon) stays before `NIM_DELETE`: the shell's
    /// default display time is ~5 s, and the bounded queue drops overflow meanwhile, so a flood
    /// costs at most `NOTIFY_QUEUE_CAP` lingers.
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
        /// Register the (idempotent) window class and create the hidden message-only window;
        /// `None` on any failure, and delivery then drains silently.
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
            // Keep the icon alive while the balloon shows, pumping messages so the tray's
            // liveness pings are answered (an unresponsive owner's icon is reaped mid-balloon).
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

/// Escape a string for an AppleScript double-quoted literal: backslash and double-quote are
/// backslash-escaped, and newlines and every other C0 control fold to spaces (a notification
/// is one line). Built on every target for tests.
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

/// Fold and truncate a notification string into a fixed-size NUL-terminated UTF-16 field
/// (`NOTIFYICONDATAW.szInfo`/`szInfoTitle`/`szTip`). Control bytes become spaces as in
/// [`applescript_escape`] (no escaping: a WCHAR buffer is no interpreter); the copy stops at
/// `out.len() - 1` units without splitting a surrogate pair. Built on every target for tests.
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
    /// lead's ): an unset `desktop_alerts` reads off, and
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

    /// The consent path's instance-level notice (the warm-up's denial, which belongs to no tab)
    /// is not suppressed by the first tab's focus (session `0`, the set's initial member).
    /// Negative control: a notice that does name session `0` is suppressed.
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

    /// A wait in a live Linux test for something that must happen: a hang
    /// detector, not a budget.
    #[cfg(target_os = "linux")]
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);

    /// The notification server's well-known name (also its interface), and its
    /// object, as a stand-in serves them.
    #[cfg(target_os = "linux")]
    const SERVER: &str = "org.freedesktop.Notifications";
    #[cfg(target_os = "linux")]
    const OBJECT: &str = "/org/freedesktop/Notifications";

    /// A private `dbus-daemon` for a live Linux test, never the session's bus:
    /// killed when dropped. COULD NOT RUN where no `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    struct PrivateBus {
        addresses: Vec<aterm_dbus::address::Address>,
        _daemon: Daemon,
        _scratch: aterm_tempfile::TempDir,
    }

    /// The daemon's process, killed and reaped when dropped.
    #[cfg(target_os = "linux")]
    struct Daemon(aterm_uds::spawnfd::Child);

    #[cfg(target_os = "linux")]
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(target_os = "linux")]
    impl PrivateBus {
        fn start() -> Self {
            use std::io::BufRead as _;
            let Some(daemon) = std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("dbus-daemon"))
                    .find(|p| p.is_file())
            }) else {
                aterm_spec::host::could_not_run(
                    "no dbus-daemon on PATH, so no live notification round trip here; install \
                     the distribution's dbus (dbus-daemon) to run it",
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
            Self {
                addresses: aterm_dbus::address::parse(line.trim()).unwrap(),
                _daemon: daemon,
                _scratch: scratch,
            }
        }

        /// A stand-in notification server: a connection to this bus that owns
        /// `org.freedesktop.Notifications`.
        fn server(&self) -> aterm_dbus::Connection {
            use aterm_dbus::{Connection, Message, Value};
            let mut server = Connection::open(&self.addresses).unwrap();
            let owned = server
                .call(
                    Message::method_call(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        Some("org.freedesktop.DBus"),
                        "RequestName",
                    )
                    .unwrap()
                    .with_body(vec![Value::from(SERVER), Value::U32(0)]),
                    HANG,
                )
                .unwrap();
            assert_eq!(
                owned.body,
                vec![Value::U32(1)],
                "the stand-in owns the name"
            );
            server
        }
    }

    /// The `Notify` call a Linux host sends, end to end through a private
    /// `dbus-daemon` (never the session's bus) to a stand-in server holding
    /// `org.freedesktop.Notifications`: the eight arguments in order, the body
    /// escaped, the `default` action on a notice about a session and none on
    /// one about no tab, urgency normal with `suppress-sound` only in serious
    /// mode, and a second notification over the same cached connection; each
    /// answer says where the server put the notice (its id, and the server's
    /// own name). COULD NOT RUN where no `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_notification_is_a_notify_call_on_the_session_bus() {
        use aterm_dbus::{Connection, Kind, Message, Value};

        let private = PrivateBus::start();
        let mut server = private.server();
        let server_name = server.unique_name().to_owned();
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
        let open = || Connection::open(&private.addresses);
        let loud = super::freedesktop::notify_call("build", "done <b>&</b>", false, true).unwrap();
        let posted = bus.notify(&loud, HANG, &open, &mut |_| {});
        assert_eq!(
            posted,
            Some(super::freedesktop::Posted {
                server: Some(server_name),
                id: 7
            }),
            "the server took the first one, and said where"
        );
        let quiet = super::freedesktop::notify_call("aterm", "serious", true, false).unwrap();
        assert!(
            bus.notify(&quiet, HANG, &open, &mut |_| {}).is_some(),
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
                Value::strings(["default", "Go to tab"]),
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
        assert_eq!(
            seen[1].body[5],
            Value::strings(std::iter::empty::<String>()),
            "a notice about no tab offers no action: the server's own activation answers it"
        );
    }

    /// WHAT A CLICK IS on Linux ([`super::freedesktop::Clicks`]): the server's
    /// `ActionInvoked` with the `default` action, for a notice aterm posted
    /// about a session, from the server that took it, goes to that session,
    /// with the activation token the server sent just before it
    /// (`ActivationToken`); the token goes with that one click. NEGATIVE
    /// CONTROLS: a click with no token before it carries none; and a signal
    /// about another program's notification (GNOME Shell broadcasts them), the
    /// same id from another sender (a server started since), an action aterm
    /// never offered, a method call or another interface's signal of the same
    /// shape, a click on a closed notice, and one on the oldest of more notices
    /// than the bus thread remembers, are no click.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_click_is_the_default_action_on_a_notice_aterm_posted() {
        use super::freedesktop::{Clicks, Observation, POSTED_CAP, Posted};
        use aterm_dbus::{Message, Value};

        const TOKEN: &str = "gnome-shell/aterm/4242-1-host_TIME5678";
        let server = Some(":1.7".to_owned());
        let sent = |sender: &Option<String>, mut msg: Message, id: u32, arg: Value| {
            msg.sender.clone_from(sender);
            msg.with_body(vec![Value::U32(id), arg])
        };
        let signal = |member: &str| Message::signal(OBJECT, SERVER, member).unwrap();
        let token = |id| sent(&server, signal("ActivationToken"), id, Value::from(TOKEN));
        let click = |id| sent(&server, signal("ActionInvoked"), id, Value::from("default"));
        let closed = |id| sent(&server, signal("NotificationClosed"), id, Value::U32(2));
        let at = |id| Posted {
            server: server.clone(),
            id,
        };

        let mut clicks = Clicks::default();
        clicks.posted(at(41), Some(3));
        clicks.posted(at(42), Some(5));
        assert_eq!(clicks.heard(&token(42)), None, "a token is no click");
        assert_eq!(
            clicks.heard(&click(42)),
            Some((5, Some(TOKEN.to_owned()))),
            "a click goes to its notice's session, with the token sent before it"
        );
        assert_eq!(
            clicks.heard(&click(42)),
            Some((5, None)),
            "the token went with that click"
        );
        assert_eq!(clicks.heard(&click(41)), Some((3, None)), "no token sent");

        let other_sender = Some(":1.9".to_owned());
        for (case, msg) in [
            ("another program's notification", click(99)),
            (
                "another sender",
                sent(
                    &other_sender,
                    signal("ActionInvoked"),
                    42,
                    Value::from("default"),
                ),
            ),
            (
                "an action aterm never offered",
                sent(&server, signal("ActionInvoked"), 42, Value::from("reply")),
            ),
            (
                "a method call",
                sent(
                    &server,
                    Message::method_call(Some(SERVER), OBJECT, Some(SERVER), "ActionInvoked")
                        .unwrap(),
                    42,
                    Value::from("default"),
                ),
            ),
            (
                "another interface",
                sent(
                    &server,
                    Message::signal(OBJECT, "org.example.Notifications", "ActionInvoked").unwrap(),
                    42,
                    Value::from("default"),
                ),
            ),
        ] {
            assert_eq!(clicks.heard(&msg), None, "{case}");
        }

        assert_eq!(clicks.heard(&closed(41)), None);
        assert_eq!(
            clicks.heard(&click(41)),
            None,
            "a closed notice is forgotten"
        );
        assert_eq!(
            clicks.heard(&click(42)),
            Some((5, None)),
            "control: the open one still clicks"
        );

        for id in 100..100 + u32::try_from(POSTED_CAP).unwrap() {
            clicks.posted(at(id), Some(u64::from(id)));
        }
        assert_eq!(
            clicks.heard(&click(42)),
            None,
            "the oldest of more than the bus thread remembers is forgotten"
        );
        assert_eq!(
            clicks.heard(&click(100)),
            Some((100, None)),
            "control: the newer ones still click"
        );

        // A bus connection ended. Its replacement may reuse the server name
        // and id: neither the old target nor its token can route new traffic.
        let mut epochs = Clicks::default();
        epochs.posted(at(41), Some(3));
        assert_eq!(epochs.observe(Observation::Message(&token(41))), None);
        assert_eq!(epochs.observe(Observation::ConnectionReset), None);
        assert_eq!(epochs.observe(Observation::Message(&click(41))), None);
        epochs.posted(at(41), Some(5));
        assert_eq!(
            epochs.observe(Observation::Message(&click(41))),
            Some((5, None)),
            "the replacement's notice routes to its own session, with no old token"
        );
        assert_eq!(epochs.observe(Observation::Message(&token(41))), None);
        epochs.posted(at(41), Some(7));
        assert_eq!(
            epochs.observe(Observation::Message(&click(41))),
            Some((7, None)),
            "a repeated key replaces its target and token within an epoch too"
        );
        epochs.posted(at(42), Some(9));
        epochs.posted(at(41), None);
        assert_eq!(
            epochs.observe(Observation::Message(&click(41))),
            None,
            "an instance-level replacement cannot route to an old session"
        );
        assert_eq!(
            epochs.observe(Observation::Message(&click(42))),
            Some((9, None)),
            "an unrelated open notice still routes"
        );
    }

    /// A CLICK ON A LINUX NOTIFICATION GOES TO THE TAB THAT POSTED IT, end to
    /// end through a private `dbus-daemon` (never the session's bus). The
    /// stand-in server answers `Notify` with ids and, as GNOME Shell 46 does,
    /// reports a click on a notice that offers the `default` action by
    /// broadcasting `ActivationToken` and then `ActionInvoked` (on a notice
    /// without it, the shell activates the app itself and says nothing on the
    /// bus). The owner is on tab A; tab A's and tab B's notices are posted
    /// through the bus thread ([`super::freedesktop::Poster`], fed as the
    /// delivery thread feeds it, by `post`), and tab B's is clicked. The bus
    /// thread hands over `Wake::NoticeClicked` for tab B's session with the
    /// token, and the App's answer ([`crate::App::notice_clicked`]) selects tab
    /// B, raises its window and presents the token. Before that click the
    /// server broadcast a click on another program's notification, closed tab
    /// A's notice and clicked it: none of these reached the App, since the
    /// first wake is tab B's. A click on a notice whose session has gone raises
    /// the window aterm was last used in. NEGATIVE CONTROL (by hand): without
    /// the `default` action the server reports no click, and the wait for tab
    /// B's fails. COULD NOT RUN where no `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_click_on_a_linux_notification_goes_to_the_tab_that_posted_it() {
        use aterm_dbus::{Connection, Kind, Message, Value};

        use crate::app_tabs::{take_raised_activations, take_raised_windows};

        const TOKEN: &str = "gnome-shell/aterm/4242-1-host_TIME5678";

        fn emit(server: &mut Connection, member: &str, id: u32, arg: Value) {
            server
                .emit(OBJECT, SERVER, member, vec![Value::U32(id), arg])
                .unwrap();
        }
        /// GNOME Shell 46's answer to a click on the notice `id` that `notify` posted.
        fn click(server: &mut Connection, (id, notify): &(u32, Message)) {
            let offers_default = notify.body[5].as_array().is_some_and(|actions| {
                actions
                    .items()
                    .iter()
                    .any(|a| a.as_str() == Some("default"))
            });
            if offers_default {
                emit(server, "ActivationToken", *id, Value::from(TOKEN));
                emit(server, "ActionInvoked", *id, Value::from("default"));
            }
        }

        let private = PrivateBus::start();
        let mut server = private.server();
        let served = std::thread::spawn(move || {
            let mut notices = Vec::new();
            let mut next = 41;
            while notices.len() < 2 {
                let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
                if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                    server.reply(&msg, vec![Value::U32(next)]).unwrap();
                    notices.push((next, msg));
                    next += 1;
                }
            }
            emit(&mut server, "ActionInvoked", 99, Value::from("default"));
            emit(
                &mut server,
                "NotificationClosed",
                notices[0].0,
                Value::U32(2),
            );
            click(&mut server, &notices[0]);
            click(&mut server, &notices[1]);
            (server, notices)
        });

        let wid = crate::WindowId(0);
        let mut app = crate::App::headless_for_test();
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let (tab_a, tab_b) = (0, 1);
        app.switch_tab_in(wid, 0);
        assert_eq!(
            app.windows[&wid].tabs.active, 0,
            "fixture: the owner is on tab A"
        );
        let _ = (take_raised_windows(), take_raised_activations());

        let (woke, wakes) = std::sync::mpsc::channel();
        let addresses = private.addresses.clone();
        let poster = super::freedesktop::Poster::start(
            move || Connection::open(&addresses),
            move |wake| {
                let _ = woke.send(wake);
            },
        )
        .expect("the bus thread starts");
        super::post(&poster, Some("tab A"), "build done", false, tab_a);
        super::post(&poster, Some("tab B"), "tests done", false, tab_b);

        let wake = wakes
            .recv_timeout(HANG)
            .expect("the click on tab B's notice reaches aterm");
        let crate::Wake::NoticeClicked {
            session,
            activation,
        } = wake
        else {
            panic!("not a click: {wake:?}");
        };
        assert_eq!(
            (session, activation.as_deref()),
            (tab_b, Some(TOKEN)),
            "tab B's notice, with the server's token: the other program's click and the closed \
             notice's went nowhere"
        );
        app.notice_clicked(session, activation);
        assert_eq!(app.windows[&wid].tabs.active, 1, "tab B is selected");
        assert_eq!(take_raised_windows(), vec![wid], "its window raised");
        assert_eq!(
            take_raised_activations(),
            vec![Some(TOKEN.to_owned())],
            "with the click's activation token"
        );

        let (_server, notices) = served.join().unwrap();
        for (_, notify) in &notices {
            assert_eq!(
                notify.body[5],
                Value::strings(["default", "Go to tab"]),
                "a notice about a session offers the default action"
            );
        }

        app.notice_clicked(999, None);
        assert_eq!(
            app.windows[&wid].tabs.active, 1,
            "a session gone moves no tab"
        );
        assert_eq!(
            take_raised_windows(),
            vec![wid],
            "and raises the window aterm was last used in"
        );
        drop(poster);
    }

    /// A SLOW LINUX NOTIFICATION SERVER IS NO DEAD CONNECTION, end to end
    /// through a private `dbus-daemon` (never the session's bus). The stand-in
    /// server answers tab A's notice at once and holds its answer to tab B's
    /// until tab C's reaches it, past the bus thread's deadline for tab B's
    /// (the product's [`super::freedesktop::CALL_TIMEOUT`]), as a stalled
    /// GNOME Shell or a notification daemon the bus is still starting does;
    /// then it answers both and reports a click on tab A's notice. Tab A's and
    /// tab C's answers must come, so their deadlines are hang detectors. The
    /// server saw each notice once, and the click goes to tab A: the late
    /// answer kept the connection, and with it where each notice still showing
    /// goes. Before, the deadline dropped the connection, so the bus thread
    /// sent tab B's notice again on a new one (the server showed it twice) and
    /// forgot tab A's, whose click went nowhere. COULD NOT RUN where no
    /// `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_slow_linux_server_shows_a_notice_once_and_keeps_its_clicks() {
        use aterm_dbus::{Connection, Kind, Value};

        let private = PrivateBus::start();
        let mut server = private.server();
        let served = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut held = Vec::new();
            let mut next = 41;
            loop {
                let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
                if msg.kind != Kind::MethodCall || msg.member.as_deref() != Some("Notify") {
                    continue;
                }
                let summary = msg.body[3].as_str().unwrap().to_owned();
                held.push((next, msg));
                next += 1;
                let last = summary == "tab C";
                seen.push(summary);
                // Tab A's is answered at once; every other waits for tab C's,
                // which the bus thread sends only once it gave up on tab B's.
                if seen.len() == 1 || last {
                    for (id, notify) in held.drain(..) {
                        server.reply(&notify, vec![Value::U32(id)]).unwrap();
                    }
                }
                if last {
                    server
                        .emit(
                            OBJECT,
                            SERVER,
                            "ActionInvoked",
                            vec![Value::U32(41), Value::from("default")],
                        )
                        .unwrap();
                    return (server, seen);
                }
            }
        });

        let (woke, wakes) = std::sync::mpsc::channel();
        let addresses = private.addresses.clone();
        let poster = super::freedesktop::Poster::start(
            move || Connection::open(&addresses),
            move |wake| {
                let _ = woke.send(wake);
            },
        )
        .expect("the bus thread starts");
        let (tab_a, tab_b, tab_c) = (3, 5, 7);
        for (tab, summary, bound) in [
            (tab_a, "tab A", HANG),
            (tab_b, "tab B", super::freedesktop::CALL_TIMEOUT),
            (tab_c, "tab C", HANG),
        ] {
            let call = super::freedesktop::notify_call(summary, "done", false, true).unwrap();
            poster.post_within(call, Some(tab), bound);
        }

        let (_server, seen) = served.join().unwrap();
        assert_eq!(
            seen,
            ["tab A", "tab B", "tab C"],
            "the server saw each notice once: an answer past the deadline sent none again"
        );
        let wake = wakes
            .recv_timeout(HANG)
            .expect("the click on tab A's notice reaches aterm");
        let crate::Wake::NoticeClicked {
            session,
            activation,
        } = wake
        else {
            panic!("not a click: {wake:?}");
        };
        assert_eq!(
            (session, activation),
            (tab_a, None),
            "tab A's notice still goes to tab A: the connection and its clicks were kept"
        );
        drop(poster);
    }

    /// A NOTICE POSTED AT ONCE GIVES UP ON A SILENT BUS WITHIN ITS OWN BOUND:
    /// [`super::deliver_now`] runs on the event loop for a file that cannot
    /// open, so its dial is bounded by [`super::freedesktop::CALL_TIMEOUT`],
    /// never aterm-dbus's 25 s handshake default, which froze every window.
    /// The bus is a listener that never accepts: the kernel completes the
    /// connection into its backlog, and nothing answers the handshake. The
    /// control: a private `dbus-daemon` (never the session's bus) whose
    /// stand-in server takes the same notice, its dial and answer awaited as
    /// what must happen (a hang detector). COULD NOT RUN where no
    /// `dbus-daemon` is on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_notice_posted_at_once_gives_up_on_a_silent_bus_within_its_bound() {
        use aterm_dbus::{Kind, Value};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("silent");
        let _silent = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let silent = aterm_dbus::address::parse(&format!("unix:path={}", path.display())).unwrap();
        let call = super::freedesktop::notify_call("aterm", "cannot open", false, false).unwrap();
        let asked = std::time::Instant::now();
        let posted =
            super::freedesktop::notify_now(&call, &silent, super::freedesktop::CALL_TIMEOUT);
        let waited = asked.elapsed();
        assert_eq!(posted, None, "a bus that never answered took nothing");
        // The two bounds compared, not the machine's speed.
        assert!(
            waited < aterm_dbus::conn::HANDSHAKE_TIMEOUT,
            "the notice gave up after {waited:?}, the handshake default's, not its own bound"
        );

        let private = PrivateBus::start();
        let mut server = private.server();
        let served = std::thread::spawn(move || {
            loop {
                let msg = server.recv(Some(HANG)).unwrap().expect("a Notify call");
                if msg.kind == Kind::MethodCall && msg.member.as_deref() == Some("Notify") {
                    server.reply(&msg, vec![Value::U32(7)]).unwrap();
                    return server;
                }
            }
        });
        let posted = super::freedesktop::notify_now(&call, &private.addresses, HANG);
        let _server = served.join().unwrap();
        assert_eq!(
            posted.map(|at| at.id),
            Some(7),
            "a bus that answers takes the notice"
        );
    }
}
