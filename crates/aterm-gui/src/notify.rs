// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native desktop notification delivery for OSC 9 / OSC 99 / OSC 777.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;

/// One pending notification handed from a tab's engine callback to the delivery thread.
pub(crate) struct NotifyMsg {
    /// Originating session, for focus suppression.
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub session: u64,
    /// Notification title (OSC 99 carries one; OSC 9/777 do not — `None`).
    #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
    pub title: Option<String>,
    /// Notification body.
    pub body: String,
}

impl NotifyMsg {
    /// One program notification (OSC 9/99/777), already admitted by `allow_notifications`.
    pub(crate) fn program(session: u64, title: Option<String>, body: String) -> Self {
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux", test)))]
        let _ = (session, title);
        Self {
            #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
            session,
            #[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
            title,
            body,
        }
    }
}

/// The notices the process-wide queue holds at once: a tab's `try_send` past them is refused as
/// full and never blocks it.
const NOTIFY_QUEUE_CAP: usize = 16;

/// Whether `msg` is still wanted: not while the person is looking at its session's tab.
#[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
fn delivers(msg: &NotifyMsg, suppressed: &HashSet<u64>) -> bool {
    !suppressed.contains(&msg.session)
}

/// The shared settings read by the notification delivery thread.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[derive(Clone)]
struct DeliveryPolicy {
    suppress: Arc<Mutex<HashSet<u64>>>,
    silent: Arc<AtomicBool>,
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
impl DeliveryPolicy {
    fn wanted(&self, msg: &NotifyMsg) -> bool {
        delivers(
            msg,
            &self.suppress.lock().unwrap_or_else(|p| p.into_inner()),
        )
    }
}

/// The delivery thread's admission, retaining the settings until delivery.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
struct AdmittedDelivery {
    msg: NotifyMsg,
    policy: DeliveryPolicy,
    #[cfg(target_os = "macos")]
    silent: bool,
}

/// The first dequeue's decision, shared with tests that inject only the transport.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
fn admit_delivery(msg: NotifyMsg, policy: &DeliveryPolicy) -> Option<AdmittedDelivery> {
    if !policy.wanted(&msg) {
        return None;
    }
    Some(AdmittedDelivery {
        #[cfg(target_os = "macos")]
        silent: policy.silent.load(Ordering::Acquire),
        msg,
        policy: policy.clone(),
    })
}

/// Whether this build has a host that can turn a queued notification into an operating-system notification.
#[must_use]
pub(crate) const fn delivery_available() -> bool {
    cfg!(any(target_os = "macos", windows, target_os = "linux"))
}

/// Spawn the single, process-wide notification delivery thread and return the bounded `SyncSender` each tab clones into its engine callbacks.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn spawn_delivery(
    suppress: Arc<Mutex<HashSet<u64>>>,
    silent: Arc<AtomicBool>,
    center: crate::notice_center::NoticeCenter,
) -> SyncSender<NotifyMsg> {
    #[cfg(not(target_os = "macos"))]
    let _ = center;
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    let policy = DeliveryPolicy { suppress, silent };
    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            let Some(admitted) = admit_delivery(msg, &policy) else {
                continue;
            };
            #[cfg(any(target_os = "macos", windows))]
            let msg = &admitted.msg;
            #[cfg(target_os = "macos")]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                admitted.silent,
                Some(msg.session),
                center,
                || admitted.policy.wanted(msg),
            );
            #[cfg(windows)]
            deliver(
                msg.title.as_deref(),
                &msg.body,
                || admitted.policy.silent.load(Ordering::Acquire),
                msg.session,
                || admitted.policy.wanted(msg),
            );
            #[cfg(target_os = "linux")]
            deliver(admitted);
        }
    });
    tx
}

/// Stub for a target with no notifier wired up (neither macOS, Windows nor Linux): drain the channel so senders never block and the workspace builds everywhere.
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn spawn_delivery(
    _suppress: Arc<Mutex<HashSet<u64>>>,
    _silent: Arc<AtomicBool>,
    _center: crate::notice_center::NoticeCenter,
) -> SyncSender<NotifyMsg> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<NotifyMsg>(NOTIFY_QUEUE_CAP);
    std::thread::spawn(move || while rx.recv().is_ok() {});
    tx
}

#[cfg(target_os = "macos")]
fn deliver(
    title: Option<&str>,
    body: &str,
    _silent: bool,
    session: Option<u64>,
    center: crate::notice_center::NoticeCenter,
    wanted: impl Fn() -> bool,
) {
    use aterm_uds::spawnfd::Stdio;

    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let title = title.unwrap_or("aterm");

    // A bundle's voice is aterm's own: posted in-process as the bundle, or
    // not at all. A center that would not take the notice, or none to post
    // through, is one log line, never a subprocess.
    match post_of(center, running_bundled(), &wanted) {
        Some(Post::Center) => {
            let posted = center.post(&crate::notice_center::Notice {
                title,
                body,
                session,
            });
            if !posted {
                aterm_log::warn!("notices: one not posted: the center would not take it");
            }
        }
        // A bare binary: the OS's osascript, by its absolute path (an aterm the Dock started has
        // launchd's PATH), and the script is one `-e` argv entry, never a shell string.
        Some(Post::Osascript) => {
            let _ = aterm_uds::spawnfd::command(aterm_uds::os_path!("osascript"))
                .arg("-e")
                .arg(osascript_script(title, body))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        None => {}
    }
}

/// How [`deliver`] posts a notice.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Post {
    /// In-process, through the bundle's notification center.
    Center,
    /// Through the OS's osascript (a bare binary's only voice).
    Osascript,
}

/// The post a notice gets as `center` stands now. A notice reads what macOS said and never asks it:
/// asking is [`ask_macos`]'s.
#[cfg(target_os = "macos")]
fn post_of(
    center: crate::notice_center::NoticeCenter,
    bundled: bool,
    wanted: impl Fn() -> bool,
) -> Option<Post> {
    post_for(route(center.answer(), bundled), wanted)
}

/// The post a notice routed `way` gets: the center's for [`Route::Center`] and osascript's for
/// [`Route::Osascript`], each only while `wanted`; any other way is one log line and no post.
#[cfg(target_os = "macos")]
fn post_for(way: Route, wanted: impl Fn() -> bool) -> Option<Post> {
    match way {
        Route::Center => wanted().then_some(Post::Center),
        Route::Osascript => wanted().then_some(Post::Osascript),
        Route::Withheld => {
            aterm_log::info!(
                "notices: one not posted: aterm's notifications are off in System Settings \
                 \u{25B8} Notifications"
            );
            None
        }
        Route::Unasked => {
            aterm_log::info!(
                "notices: one not posted: macOS has not authorized notifications; a person \
                 turning Allow programs to show desktop notifications off and on in Settings \
                 \u{25B8} Security asks it"
            );
            None
        }
        Route::Centerless if crate::signing_identity::running_bundle_id().is_none() => {
            aterm_log::info!(
                "notices: one not posted: aterm does not read this aterm.app's identity (it sits \
                 in a folder macOS guards, such as Downloads, or was started through a link), so \
                 it has none to post as; moved to /Applications or ~/Applications and opened \
                 there, it posts as itself"
            );
            None
        }
        Route::Centerless => {
            aterm_log::info!(
                "notices: one not posted: macOS gave this aterm.app no notification center (an \
                 earlier notices: line says why), and an app posts only as itself"
            );
            None
        }
    }
}

/// The osascript script that posts `body` under `title`: both land in an AppleScript string
/// literal, so both are escaped.
#[cfg(target_os = "macos")]
fn osascript_script(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title),
    )
}

/// Ask macOS to let aterm post as itself, and wake the App once it has answered, so the center's
/// delegate is adopted and a click comes back. A person turning the Settings row on is the only
/// caller (`App::publish_native_config_origin`). The center's ask runs on a thread of its own, so
/// the main thread never waits, and a Mac that has already answered sees nothing.
#[cfg(target_os = "macos")]
pub(crate) fn ask_macos(center: crate::notice_center::NoticeCenter) {
    center.ask(Box::new(|| wake(crate::Wake::NoticeReady)));
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Center,
    Withheld,
    Unasked,
    Centerless,
    Osascript,
}

#[cfg(any(target_os = "macos", test))]
const fn route(center: crate::notice_center::Center, bundled: bool) -> Route {
    use crate::notice_center::Center;
    match center {
        Center::Granted => Route::Center,
        Center::Refused => Route::Withheld,
        Center::Unasked | Center::Provisional => Route::Unasked,
        Center::Absent if bundled => Route::Centerless,
        Center::Absent => Route::Osascript,
    }
}

#[cfg(target_os = "macos")]
fn running_bundled() -> bool {
    use aterm_containment::consent;
    static BUNDLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BUNDLED.get_or_init(|| {
        consent::fence_exe().is_some_and(|exe| consent::path_is_in_app_bundle(&exe))
    })
}

#[cfg(windows)]
fn deliver(
    title: Option<&str>,
    body: &str,
    silent: impl Fn() -> bool,
    session: u64,
    wanted: impl Fn() -> bool,
) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    let (title, body) = balloon_text(title, body);
    win_balloon::deliver(title, body, silent, session, wanted);
}

/// A notice's balloon title and text: a missing title is `aterm`, and a notice with no body
/// shows its title as the text under `aterm`, since a balloon with empty text is removed, never
/// shown.
#[cfg(any(test, windows))]
fn balloon_text<'a>(title: Option<&'a str>, body: &'a str) -> (&'a str, &'a str) {
    let title = title.unwrap_or("aterm");
    if body.is_empty() {
        ("aterm", title)
    } else {
        (title, body)
    }
}

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

#[cfg(any(test, windows))]
const fn balloon_clicked(lparam: isize) -> bool {
    const NIN_BALLOONUSERCLICK: isize = 0x0400 + 5;
    lparam & 0xFFFF == NIN_BALLOONUSERCLICK
}

#[cfg(all(any(windows, target_os = "macos", target_os = "linux"), not(test)))]
static PROXY: std::sync::OnceLock<aterm_window::event_loop::EventLoopProxy<crate::Wake>> =
    std::sync::OnceLock::new();

/// Install the proxy a notice's wakes ride.
pub(crate) fn install_proxy(proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
    #[cfg(all(any(windows, target_os = "macos", target_os = "linux"), not(test)))]
    let _ = PROXY.set(proxy.clone());
    #[cfg(not(all(any(windows, target_os = "macos", target_os = "linux"), not(test))))]
    let _ = proxy;
}

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
    static WAKES: std::cell::RefCell<Vec<crate::Wake>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The wakes this thread's notices handed the App since the last call (test-only): what a click or an ask would have sent through the proxy. macOS's only: its readers are the center's click tests and the ask's wake test, and Windows' balloon has none in a test build.
#[cfg(all(target_os = "macos", test))]
pub(crate) fn take_wakes() -> Vec<crate::Wake> {
    WAKES.with(|wakes| std::mem::take(&mut *wakes.borrow_mut()))
}

/// A click on the notification `session` posted: take the owner there, as the ❯ menu's Go does (`App::go_to_session`: that session's window raised, its tab selected, its pane given the keyboard).
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn raise_clicked(session: u64) {
    wake(crate::Wake::NoticeClicked { session });
}

#[cfg(target_os = "linux")]
fn deliver(admitted: AdmittedDelivery) {
    if cfg!(test) || crate::menu::process_is_headless() {
        return;
    }
    freedesktop::POSTER.with(|poster| {
        if let Some(poster) = poster.get_or_init(freedesktop::Poster::session) {
            post_admitted(poster, admitted, freedesktop::CALL_TIMEOUT);
        }
    });
}

/// The actual Linux enqueue, with only Poster selection injected by the host test.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn post_admitted(
    poster: &freedesktop::Poster,
    admitted: AdmittedDelivery,
    bound: std::time::Duration,
) {
    poster.admitted(admitted, bound);
}

#[cfg(all(test, target_os = "linux"))]
fn post(
    poster: &freedesktop::Poster,
    title: Option<&str>,
    body: &str,
    silent: bool,
    session: u64,
    bound: std::time::Duration,
) {
    if let Ok(call) = freedesktop::notify_call(title.unwrap_or("aterm"), body, silent) {
        poster.post(call, session, bound);
    }
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
mod freedesktop {
    #[cfg(target_os = "linux")]
    use std::cell::OnceCell;
    use std::collections::VecDeque;
    use std::io::{ErrorKind, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::time::Duration;

    #[cfg(all(test, target_os = "linux"))]
    use aterm_dbus::address::Address;
    use aterm_dbus::{Connection, Kind, Message, Type, Value};

    const SERVER: &str = "org.freedesktop.Notifications";
    const OBJECT: &str = "/org/freedesktop/Notifications";

    const SIGNALS: &str = "type='signal',interface='org.freedesktop.Notifications',\
                           path='/org/freedesktop/Notifications'";

    const DEFAULT_ACTION: &str = "default";
    const DEFAULT_LABEL: &str = "Go to tab";

    pub(super) const POSTED_CAP: usize = 64;

    #[cfg(target_os = "linux")]
    pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(2);

    #[cfg(target_os = "linux")]
    thread_local! {
        pub(super) static POSTER: OnceCell<Option<Poster>> = const { OnceCell::new() };
    }

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
                Value::strings([DEFAULT_ACTION, DEFAULT_LABEL]),
                Value::dict(Type::Str, Type::Variant, hints)?,
                Value::I32(-1),
            ]),
        )
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Posted {
        pub(super) server: Option<String>,
        pub(super) id: u32,
    }

    pub(super) enum Observation<'a> {
        ConnectionReset,
        Message(&'a Message),
    }

    pub(super) struct Bus {
        pub(super) conn: Option<Connection>,
    }

    enum Delivery {
        #[cfg(all(test, target_os = "linux"))]
        Fixed(Message),
        Live(super::AdmittedDelivery),
    }

    impl Delivery {
        /// Build the call only after transport setup, again on a retry. Queued
        /// events hold no settings lock over I/O.
        fn prepare(&self) -> Option<Message> {
            match self {
                #[cfg(all(test, target_os = "linux"))]
                Self::Fixed(call) => Some(call.clone()),
                Self::Live(admitted) => {
                    let super::AdmittedDelivery { msg, policy, .. } = admitted;
                    if !policy.wanted(msg) {
                        return None;
                    }
                    notify_call(
                        msg.title.as_deref().unwrap_or("aterm"),
                        &msg.body,
                        policy.silent.load(super::Ordering::Acquire),
                    )
                    .ok()
                }
            }
        }
    }

    impl Bus {
        #[cfg(all(test, target_os = "linux"))]
        pub(super) fn notify(
            &mut self,
            call: &Message,
            bound: Duration,
            open: &dyn Fn() -> aterm_dbus::Result<Connection>,
            heard: &mut dyn FnMut(Observation<'_>),
        ) -> Option<Posted> {
            self.notify_current(&Delivery::Fixed(call.clone()), bound, open, heard)
        }

        fn notify_current(
            &mut self,
            delivery: &Delivery,
            bound: Duration,
            open: &dyn Fn() -> aterm_dbus::Result<Connection>,
            heard: &mut dyn FnMut(Observation<'_>),
        ) -> Option<Posted> {
            for _ in 0..2 {
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
                let call = delivery.prepare()?;
                match conn.call(call, bound) {
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

        fn end_connection(&mut self, heard: &mut dyn FnMut(Observation<'_>)) {
            if let Some(conn) = self.conn.take() {
                drop(conn);
                heard(Observation::ConnectionReset);
            }
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn notify_now(
        call: &Message,
        addresses: &[Address],
        bound: Duration,
    ) -> Option<Posted> {
        let open = || Connection::open_within(addresses, bound);
        Bus { conn: None }.notify(call, bound, &open, &mut |_| {})
    }

    struct Notice {
        at: Posted,
        session: u64,
        token: Option<String>,
    }

    #[derive(Default)]
    pub(super) struct Clicks {
        posted: VecDeque<Notice>,
    }

    impl Clicks {
        pub(super) fn posted(&mut self, at: Posted, session: u64) {
            self.posted.retain(|notice| notice.at != at);
            if self.posted.len() >= POSTED_CAP {
                self.posted.pop_front();
            }
            self.posted.push_back(Notice {
                at,
                session,
                token: None,
            });
        }

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

    struct Post {
        delivery: Delivery,
        session: u64,
        bound: Duration,
    }

    pub(super) struct Poster {
        posts: SyncSender<Post>,
        wake: UnixStream,
    }

    impl Poster {
        #[cfg(target_os = "linux")]
        pub(super) fn session() -> Option<Self> {
            Self::start(Connection::session, super::wake)
        }

        pub(super) fn start(
            open: impl Fn() -> aterm_dbus::Result<Connection> + Send + 'static,
            clicked: impl Fn(crate::Wake) + Send + 'static,
        ) -> Option<Self> {
            let pair = aterm_uds::kept_out(UnixStream::pair).and_then(|(wake, rung)| {
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

        #[cfg(all(test, target_os = "linux"))]
        pub(super) fn post(&self, call: Message, session: u64, bound: Duration) {
            self.enqueue(Post {
                delivery: Delivery::Fixed(call),
                session,
                bound,
            });
        }

        pub(super) fn admitted(&self, admitted: super::AdmittedDelivery, bound: Duration) {
            self.enqueue(Post {
                session: admitted.msg.session,
                delivery: Delivery::Live(admitted),
                bound,
            });
        }

        fn enqueue(&self, post: Post) {
            if self.posts.send(post).is_ok() {
                // Non-blocking: a full socket already holds a ring.
                let _ = (&self.wake).write(&[1]);
            }
        }
    }

    fn run(
        open: &dyn Fn() -> aterm_dbus::Result<Connection>,
        clicked: &dyn Fn(crate::Wake),
        posts: &Receiver<Post>,
        rung: &UnixStream,
    ) {
        fn hear(clicks: &mut Clicks, observation: Observation<'_>, clicked: &dyn Fn(crate::Wake)) {
            if let Some((session, activation)) = clicks.observe(observation) {
                #[cfg(target_os = "linux")]
                clicked(crate::Wake::NoticeClicked {
                    session,
                    activation,
                });
                // This host-only protocol test arm exercises no desktop click.
                #[cfg(all(test, target_os = "macos"))]
                let _ = (session, activation, clicked);
            }
        }

        let mut bus = Bus { conn: None };
        let mut clicks = Clicks::default();
        loop {
            loop {
                match posts.try_recv() {
                    Ok(post) => {
                        // A connection dialled for this notice asks for the
                        // server's signals within the notice's own bound.
                        let subscribed = || {
                            let mut conn = open()?;
                            if let Err(e) = conn.add_match(SIGNALS, post.bound) {
                                aterm_log::debug!(
                                    "notices: the bus refused {SIGNALS} ({e}); clicks go nowhere"
                                );
                            }
                            Ok(conn)
                        };
                        let posted = bus.notify_current(
                            &post.delivery,
                            post.bound,
                            &subscribed,
                            &mut |msg| hear(&mut clicks, msg, clicked),
                        );
                        if let Some(at) = posted {
                            clicks.posted(at, post.session);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
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
            // SAFETY: `polls` is an initialised array of `count` pollfd records, exclusively borrowed for the call; its descriptors are the wake socket's and the connection's, which the caller holds open across it. -1 waits until one is ready or a signal interrupts.
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

#[cfg(windows)]
mod win_balloon {
    #![allow(non_snake_case, clippy::upper_case_acronyms)]

    use std::cell::{Cell, OnceCell};
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    type HWND = isize;

    const HWND_MESSAGE: HWND = -3;
    const PM_REMOVE: u32 = 0x1;
    const NIM_ADD: u32 = 0x0;
    const NIM_DELETE: u32 = 0x2;
    const NIF_MESSAGE: u32 = 0x1;
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    const WM_TRAY_CALLBACK: u32 = 0x8000 + 1;
    const IDI_APPLICATION: usize = 32512;
    const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

    const BALLOON_LINGER: Duration = Duration::from_secs(6);

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: u32,
        wParam: usize,
        lParam: isize,
        time: u32,
        pt: POINT,
    }

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

    struct Notifier {
        hwnd: HWND,
        hicon: isize,
        hballoon: isize,
        _icons: (
            Option<crate::win_icon::OwnedIcon>,
            Option<crate::win_icon::OwnedIcon>,
        ),
    }

    thread_local! {
        static NOTIFIER: OnceCell<Option<Notifier>> = const { OnceCell::new() };
        static CLICK_TARGET: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// Show one balloon (blocking this thread for [`BALLOON_LINGER`]); silently a no-op when the hidden window could not be created.
    pub(super) fn deliver(
        title: &str,
        body: &str,
        silent: impl Fn() -> bool,
        session: u64,
        wanted: impl Fn() -> bool,
    ) {
        NOTIFIER.with(|cell| {
            if let Some(n) = cell.get_or_init(Notifier::create) {
                n.balloon(title, body, &silent, session, &wanted);
            }
        });
    }

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
        // SAFETY: the arguments are exactly those the system passed this procedure for `hwnd`.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    impl Notifier {
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

        fn balloon(
            &self,
            title: &str,
            body: &str,
            silent: &impl Fn() -> bool,
            session: u64,
            wanted: &impl Fn() -> bool,
        ) {
            // SAFETY: all-zero is a valid NOTIFYICONDATAW (null handles, empty NUL-terminated strings, no flags).
            let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_TIP | NIF_INFO | NIF_MESSAGE;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            if self.hicon != 0 {
                nid.uFlags |= NIF_ICON;
                nid.hIcon = self.hicon;
                nid.hBalloonIcon = self.hballoon;
            }
            super::fold_to_utf16("aterm", &mut nid.szTip);
            super::fold_to_utf16(title, &mut nid.szInfoTitle);
            super::fold_to_utf16(body, &mut nid.szInfo);
            if !wanted() {
                return;
            }
            nid.dwInfoFlags = super::windows_info_flags(silent(), self.hicon != 0);
            CLICK_TARGET.with(|target| target.set(Some(session)));
            // SAFETY: `nid` is fully initialized and `hwnd` is a live window owned by this thread.
            if unsafe { Shell_NotifyIconW(NIM_ADD, &mut nid) } == 0 {
                CLICK_TARGET.with(Cell::take);
                return;
            }
            pump_for(BALLOON_LINGER);
            // SAFETY: same `nid` identifies the icon added above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &mut nid) };
            CLICK_TARGET.with(Cell::take);
        }
    }

    impl Drop for Notifier {
        fn drop(&mut self) {
            // SAFETY: `hwnd` was created on this thread and is destroyed at most once (Drop runs once; nothing else destroys it).
            unsafe { DestroyWindow(self.hwnd) };
        }
    }

    fn pump_for(dur: Duration) {
        let deadline = Instant::now() + dur;
        loop {
            // SAFETY: `msg` is a valid out-param; PeekMessageW fills it before Translate/Dispatch read it.
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

#[cfg(all(test, target_os = "macos"))]
#[path = "notify_live_policy_tests.rs"]
mod live_policy_tests;

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
