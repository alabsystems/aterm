// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATERM'S OWN VOICE on macOS.

/// What macOS has said about aterm posting its own notices as itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "UNUserNotificationCenter is macOS's")
)]
pub(crate) enum Center {
    /// No center to post through.
    Absent,
    /// A bundle macOS has not been asked about yet (`UNAuthorizationStatusNotDetermined`).
    Unasked,
    /// macOS lets aterm post quietly, to Notification Center only
    /// (`UNAuthorizationStatusProvisional`): asked at launch with no dialog
    /// ([`NoticeCenter::prime`]). aterm posts nothing on it; turning Notify Me on asks for the
    /// rest, as from [`Center::Unasked`].
    Provisional,
    /// macOS said no, or aterm's notifications are off in System Settings ▸ Notifications.
    Refused,
    /// macOS lets aterm post as itself.
    Granted,
}

/// `UNNotificationDefaultActionIdentifier`: the owner clicked the notice itself.
#[cfg(any(target_os = "macos", test))]
pub(crate) const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";

/// What macOS's `UNAuthorizationStatus` says, as a [`Center`].
#[cfg(any(target_os = "macos", test))]
pub(crate) const fn status_answer(status: isize) -> Center {
    match status {
        0 => Center::Unasked,
        3 => Center::Provisional,
        2 | 4 => Center::Granted,
        _ => Center::Refused,
    }
}

/// What aterm asks macOS for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "UNUserNotificationCenter is macOS's")
)]
pub(crate) enum Request {
    /// Quiet delivery to Notification Center, which macOS grants with no dialog: asked at
    /// launch ([`NoticeCenter::prime`]).
    Provisional,
    /// Banners: macOS's one dialog, asked once Notify Me is on ([`NoticeCenter::ask`]).
    Full,
}

/// Whether aterm asks macOS for `request` where it has said `answer`: the quiet ask only of a
/// bundle never asked, the dialog of one never asked or only granted the quiet delivery.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "UNUserNotificationCenter is macOS's")
)]
pub(crate) const fn asks(answer: Center, request: Request) -> bool {
    matches!(
        (answer, request),
        (Center::Unasked, _) | (Center::Provisional, Request::Full)
    )
}

/// The session a response goes to.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn clicked_session(
    action: &str,
    session: Option<&str>,
    pid: Option<&str>,
    instance: Option<&str>,
    own_pid: u32,
    own_instance: &str,
) -> Option<u64> {
    if action != DEFAULT_ACTION || pid?.parse::<u32>().ok()? != own_pid || instance? != own_instance
    {
        return None;
    }
    session?.parse().ok()
}

/// One notice to post as aterm.
#[cfg(target_os = "macos")]
#[cfg_attr(test, expect(dead_code, reason = "a test build posts nothing"))]
pub(crate) struct Notice<'a> {
    /// The banner's title.
    pub(crate) title: &'a str,
    /// The banner's body.
    pub(crate) body: &'a str,
    /// The session a click goes to; `None` for a notice about no tab.
    pub(crate) session: Option<u64>,
}

/// The center's arms.
#[derive(Clone, Copy)]
pub(crate) struct NoticeCenter {
    /// What macOS has said ([`Center`]), read fresh: a bounded wait for the
    /// center's settings, on the thread that asks.
    #[cfg(target_os = "macos")]
    answer: fn() -> Center,
    /// Hand one notice to the center; `false` when it could not be.
    #[cfg(target_os = "macos")]
    post: fn(&Notice<'_>) -> bool,
    /// Ask macOS to let aterm post as itself, where it has not been asked
    /// yet, then call `then` (on another thread) once it has answered; a
    /// `then` dropped uncalled is an ask that could not be made.
    #[cfg(target_os = "macos")]
    ask: fn(Box<dyn FnOnce() + Send>),
    /// Ask macOS, where it has never been asked, for quiet delivery
    /// ([`Request::Provisional`]: no dialog), then call `then` as `ask` does.
    #[cfg(target_os = "macos")]
    prime: fn(Box<dyn FnOnce() + Send>),
    /// Become the center's delegate, so a click comes back (main thread;
    /// once per process, and only once a read has opened the center).
    #[cfg(target_os = "macos")]
    adopt_clicks: fn(),
}

impl NoticeCenter {
    /// The arms for this instance: live in a windowed macOS instance, inert in a headless one, in a unit test and off macOS.
    #[must_use]
    pub(crate) fn for_instance(headless: bool) -> Self {
        #[cfg(all(target_os = "macos", not(test)))]
        if !headless {
            return Self {
                answer: live::answer,
                post: live::post,
                ask: live::ask,
                prime: live::prime,
                adopt_clicks: live::adopt_clicks,
            };
        }
        let _ = headless;
        Self::inert()
    }

    /// The inert arms: macOS has nothing to say ([`Center::Absent`]), nothing is posted, nothing asked, no delegate set.
    #[must_use]
    pub(crate) const fn inert() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            answer: inert_answer,
            #[cfg(target_os = "macos")]
            post: inert_post,
            #[cfg(target_os = "macos")]
            ask: inert_ask,
            #[cfg(target_os = "macos")]
            prime: inert_prime,
            #[cfg(target_os = "macos")]
            adopt_clicks: inert_adopt_clicks,
        }
    }

    /// A test's stand-in arms: what macOS says and what an ask does are the test's own functions, so no center is reached.
    #[cfg(all(target_os = "macos", test))]
    #[must_use]
    pub(crate) const fn stand_in(
        answer: fn() -> Center,
        ask: fn(Box<dyn FnOnce() + Send>),
    ) -> Self {
        Self {
            answer,
            post: inert_post,
            ask,
            prime: inert_prime,
            adopt_clicks: inert_adopt_clicks,
        }
    }

    /// [`Self::stand_in`]'s arms with the test's own quiet ask.
    #[cfg(all(target_os = "macos", test))]
    #[must_use]
    pub(crate) const fn priming(self, prime: fn(Box<dyn FnOnce() + Send>)) -> Self {
        Self { prime, ..self }
    }
}

#[cfg(target_os = "macos")]
impl NoticeCenter {
    /// What macOS has said about aterm posting as itself.
    pub(crate) fn answer(self) -> Center {
        (self.answer)()
    }

    /// Post `notice` as aterm; `false` when the center could not take it.
    pub(crate) fn post(self, notice: &Notice<'_>) -> bool {
        (self.post)(notice)
    }

    /// Ask macOS, if it has not been asked or granted only quiet delivery, and call `then` once
    /// it has answered.
    pub(crate) fn ask(self, then: Box<dyn FnOnce() + Send>) {
        (self.ask)(then);
    }

    /// Ask macOS, if it has never been asked, for quiet delivery (no dialog), and call `then` once
    /// it has answered: the launch's ask, so turning Notify Me on later is the only dialog.
    pub(crate) fn prime(self, then: Box<dyn FnOnce() + Send>) {
        (self.prime)(then);
    }

    /// Hear clicks: become the center's delegate (main thread).
    pub(crate) fn adopt_clicks(self) {
        (self.adopt_clicks)();
    }
}

#[cfg(target_os = "macos")]
const fn inert_answer() -> Center {
    Center::Absent
}

#[cfg(target_os = "macos")]
const fn inert_post(_notice: &Notice<'_>) -> bool {
    false
}

/// The inert ask: macOS is not asked, and `then` is dropped uncalled (there
/// is no answer to read again).
#[cfg(target_os = "macos")]
fn inert_ask(_then: Box<dyn FnOnce() + Send>) {
    #[cfg(test)]
    ASKS.with(|asks| asks.set(asks.get() + 1));
}

/// The inert quiet ask: as [`inert_ask`].
#[cfg(target_os = "macos")]
fn inert_prime(_then: Box<dyn FnOnce() + Send>) {
    #[cfg(test)]
    PRIMES.with(|primes| primes.set(primes.get() + 1));
}

#[cfg(target_os = "macos")]
const fn inert_adopt_clicks() {}

#[cfg(all(target_os = "macos", test))]
thread_local! {
    static ASKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static PRIMES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// How many times this thread's inert center was asked since the last call (test-only).
#[cfg(all(target_os = "macos", test))]
pub(crate) fn take_asks() -> u32 {
    ASKS.with(|asks| asks.replace(0))
}

/// How many times this thread's inert center was asked for quiet delivery since the last call
/// (test-only).
#[cfg(all(target_os = "macos", test))]
pub(crate) fn take_primes() -> u32 {
    PRIMES.with(|primes| primes.replace(0))
}

/// THE PROTOTYPES the notices state for UserNotifications, named so a test
/// can hold each to the runtime's own record without the center
/// (`tests::every_prototype_the_notices_state_is_the_runtimes`).
#[cfg(target_os = "macos")]
mod abi {
    use aterm_objc::{BlockPtr, Bool, Id, Sel};

    /// `-[UNUserNotificationCenter getNotificationSettingsWithCompletionHandler:]`.
    pub(super) type GetSettings = unsafe extern "C-unwind" fn(Id, Sel, BlockPtr);
    /// Its handler: `void (^)(UNNotificationSettings *settings)`.
    pub(super) type SettingsHandler = fn(Id);
    /// `-[UNUserNotificationCenter requestAuthorizationWithOptions:completionHandler:]`; the options are an `NSUInteger` bitmask.
    pub(super) type RequestAuthorization = unsafe extern "C-unwind" fn(Id, Sel, usize, BlockPtr);
    /// Its handler: `void (^)(BOOL granted, NSError *error)`.
    pub(super) type AuthorizationHandler = fn(Bool, Id);
    /// `-[UNUserNotificationCenter addNotificationRequest:withCompletionHandler:]`.
    pub(super) type AddRequest = unsafe extern "C-unwind" fn(Id, Sel, Id, BlockPtr);
    /// Its handler: `void (^)(NSError *error)`.
    pub(super) type AddHandler = fn(Id);
    /// `+[UNNotificationRequest requestWithIdentifier:content:trigger:]`.
    pub(super) type MakeRequest = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) -> Id;
    /// `+[NSDictionary dictionaryWithObjects:forKeys:count:]`, for a notice's `userInfo`.
    pub(super) type MakeDictionary =
        unsafe extern "C-unwind" fn(Id, Sel, *const Id, *const Id, usize) -> Id;
    /// The handler a delegate's `-userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:` calls: `void (^)(void)`.
    pub(super) type ResponseDone = fn();
    /// The handler a delegate's `-userNotificationCenter:willPresentNotification:withCompletionHandler:` calls.
    pub(super) type PresentDone = fn(usize);
}

/// UserNotifications' binary (in the shared cache; `dlopen` finds it there).
#[cfg(target_os = "macos")]
const FRAMEWORK: &std::ffi::CStr =
    c"/System/Library/Frameworks/UserNotifications.framework/UserNotifications";

/// Load UserNotifications, so its classes register with the runtime; `false`
/// when it would not load.
#[cfg(target_os = "macos")]
fn open_framework() -> bool {
    use std::ffi::{c_char, c_int, c_void};
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    }
    const RTLD_LAZY: c_int = 0x1;
    const RTLD_LOCAL: c_int = 0x4;
    // SAFETY: a NUL-terminated path and a valid mode.
    !unsafe { dlopen(FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) }.is_null()
}

/// The `userInfo` key naming the session a notice is about: a decimal sid.
#[cfg(target_os = "macos")]
const SESSION_KEY: &str = "aterm.session";

/// The `userInfo` key naming the process that posted the notice: a decimal
/// pid.
#[cfg(target_os = "macos")]
const PID_KEY: &str = "aterm.pid";

/// The existing opaque identity of the exact process that posted the notice
/// (`control_auth::process_instance_id`), retained with it across PID reuse.
#[cfg(target_os = "macos")]
const INSTANCE_KEY: &str = "aterm.instance";

/// A notice's `userInfo`: this process's pid ([`PID_KEY`]) and opaque
/// incarnation ([`INSTANCE_KEY`]) and, for a notice about a tab, its session
/// ([`SESSION_KEY`]), each a decimal string — what the live arm's post writes
/// and the delegate reads back ([`clicks`]).
#[cfg(target_os = "macos")]
fn user_info(session: Option<u64>) -> Option<aterm_objc::Id> {
    use aterm_objc::{Id, Obj, checked_msg, class, sel};
    let mut pairs = vec![
        (PID_KEY, std::process::id().to_string()),
        (
            INSTANCE_KEY,
            crate::control_auth::process_instance_id().to_string(),
        ),
    ];
    if let Some(session) = session {
        pairs.push((SESSION_KEY, session.to_string()));
    }
    let mut keys = Vec::with_capacity(pairs.len());
    let mut values = Vec::with_capacity(pairs.len());
    for (key, value) in &pairs {
        keys.push(aterm_objc::ns_string(key)?);
        values.push(aterm_objc::ns_string(value)?);
    }
    let keys: Vec<Id> = keys.iter().map(Obj::id).collect();
    let values: Vec<Id> = values.iter().map(Obj::id).collect();
    // SAFETY: `+dictionaryWithObjects:forKeys:count:` is
    // `abi::MakeDictionary`; both arrays hold `count` live objects, which
    // the dictionary retains (and copies, for keys). Autoreleased.
    let info = unsafe {
        let make: abi::MakeDictionary = checked_msg();
        make(
            class(c"NSDictionary").as_id(),
            sel!(dictionaryWithObjects:forKeys:count:),
            values.as_ptr(),
            keys.as_ptr(),
            keys.len(),
        )
    };
    (!info.is_null()).then_some(info)
}

/// The live arms: UserNotifications, opened at run time.
#[cfg(all(target_os = "macos", not(test)))]
mod live {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::Duration;

    use aterm_objc::send::{send_id, send_isize, send_v_id};
    use aterm_objc::{BlockPtr, Bool, Id, Obj, RcBlock, checked_msg, class, sel};

    use super::clicks::NoticeDelegate;
    use super::{Center, Notice, Request, abi};

    /// `UNAuthorizationOptionAlert`.
    const ASK_OPTIONS: usize = 1 << 2;

    /// `UNAuthorizationOptionProvisional`: granted with no dialog, delivered to Notification
    /// Center only.
    const PROVISIONAL: usize = 1 << 6;

    const ANSWER_WAIT: Duration = Duration::from_secs(5);

    static CENTER: OnceLock<Option<usize>> = OnceLock::new();

    static LAST: Mutex<Option<Center>> = Mutex::new(None);

    static ADOPTED: AtomicBool = AtomicBool::new(false);

    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);

    const fn at(addr: usize) -> Id {
        Id::from_ptr(std::ptr::with_exposed_provenance_mut(addr))
    }

    fn center() -> Option<Id> {
        (*CENTER.get_or_init(open_center)).map(at)
    }

    fn open_center() -> Option<usize> {
        if crate::menu::process_is_headless()
            || crate::signing_identity::running_bundle_id().is_none()
        {
            return None;
        }
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: `+mainBundle` and `-bundleIdentifier` are `-(id)` accessors; both answers are borrowed for this pool.
            let named = unsafe {
                let bundle = send_id(class(c"NSBundle").as_id(), sel!(mainBundle));
                !bundle.is_null() && !send_id(bundle, sel!(bundleIdentifier)).is_null()
            };
            if !named {
                aterm_log::info!(
                    "notices: Foundation names no bundle for this aterm.app, so it has no \
                     center and posts no notice"
                );
                return None;
            }
            if !super::open_framework() {
                aterm_log::info!(
                    "notices: UserNotifications did not load, so this aterm.app has no center and \
                     posts no notice"
                );
                return None;
            }
            let cls = class(c"UNUserNotificationCenter");
            if cls.is_null() {
                aterm_log::info!(
                    "notices: UserNotifications has no UNUserNotificationCenter, so this \
                     aterm.app posts no notice"
                );
                return None;
            }
            let center = aterm_objc::contain(
                "+[UNUserNotificationCenter currentNotificationCenter]",
                // SAFETY: `+currentNotificationCenter` is `-(id)`, the process's singleton, borrowed for this pool.
                || unsafe { send_id(cls.as_id(), sel!(currentNotificationCenter)) },
            )
            .ok()?;
            // SAFETY: a live (or nil) object; retained once and never released, so its address stays good for the process.
            let Some(held) = (unsafe { Obj::retain(center) }) else {
                aterm_log::info!("notices: macOS gave aterm no center, so it posts no notice");
                return None;
            };
            let addr = held.id().expose_provenance();
            std::mem::forget(held);
            aterm_log::info!("notices: the center is open");
            Some(addr)
        })
    }

    /// What macOS has said: the center's settings, read fresh within [`ANSWER_WAIT`], else the last answer read.
    pub(super) fn answer() -> Center {
        let Some(center) = center() else {
            return Center::Absent;
        };
        let (tx, rx) = std::sync::mpsc::sync_channel::<isize>(1);
        let read = move |settings: Id| {
            let status = if settings.is_null() {
                -1
            } else {
                // SAFETY: the settings object is live for the call, and `-authorizationStatus` is `-(NSInteger)`.
                unsafe { send_isize(settings, sel!(authorizationStatus)) }
            };
            let _ = tx.try_send(status);
        };
        let sent = aterm_objc::autoreleasepool(|_| {
            // SAFETY: the handler is `abi::SettingsHandler`, exactly `read`'s.
            let Some(handler) = (unsafe { RcBlock::of::<abi::SettingsHandler, _>(read) }) else {
                return false;
            };
            // SAFETY: `-getNotificationSettingsWithCompletionHandler:` is `abi::GetSettings` on the live center, which copies the handler.
            unsafe {
                let get: abi::GetSettings = checked_msg();
                get(
                    center,
                    sel!(getNotificationSettingsWithCompletionHandler:),
                    handler.as_block_ptr(),
                );
            }
            true
        });
        let fresh = if sent {
            rx.recv_timeout(ANSWER_WAIT).ok()
        } else {
            None
        };
        let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
        match fresh {
            Some(status) => {
                let now = super::status_answer(status);
                if *last != Some(now) {
                    aterm_log::info!(
                        "notices: macOS's answer is {now:?} (authorizationStatus {status})"
                    );
                }
                *last = Some(now);
            }
            None if sent => aterm_log::warn!(
                "notices: the center's settings did not come within {ANSWER_WAIT:?}; the last \
                 answer ({:?}) stands",
                last.unwrap_or(Center::Refused)
            ),
            None => aterm_log::warn!(
                "notices: the center's settings could not be asked for (no handler block); the \
                 last answer ({:?}) stands",
                last.unwrap_or(Center::Refused)
            ),
        }
        last.unwrap_or(Center::Refused)
    }

    /// Ask macOS, on a thread of its own, where it has not been asked or granted only quiet delivery: the system's one consent prompt for the bundle.
    pub(super) fn ask(then: Box<dyn FnOnce() + Send>) {
        request(Request::Full, then);
    }

    /// Ask macOS, on a thread of its own, where it has never been asked, for quiet delivery: no dialog.
    pub(super) fn prime(then: Box<dyn FnOnce() + Send>) {
        request(Request::Provisional, then);
    }

    fn request(what: Request, then: Box<dyn FnOnce() + Send>) {
        let spawned = std::thread::Builder::new()
            .name("aterm-notice-ask".to_string())
            .spawn(move || {
                let Some(center) = center().filter(|_| super::asks(answer(), what)) else {
                    then();
                    return;
                };
                let (options, quiet) = match what {
                    Request::Full => (ASK_OPTIONS, ""),
                    Request::Provisional => (
                        ASK_OPTIONS | PROVISIONAL,
                        " quietly, to Notification Center only",
                    ),
                };
                let then = Mutex::new(Some(then));
                let answered = move |granted: Bool, _error: Id| {
                    aterm_log::info!(
                        "notices: macOS {} aterm to post as itself{quiet}",
                        if granted.as_bool() {
                            "allowed"
                        } else {
                            "did not allow"
                        }
                    );
                    let then = then.lock().unwrap_or_else(PoisonError::into_inner).take();
                    if let Some(then) = then {
                        then();
                    }
                };
                aterm_objc::autoreleasepool(|_| {
                    // SAFETY: the handler is `abi::AuthorizationHandler`, exactly `answered`'s.
                    let Some(handler) =
                        (unsafe { RcBlock::of::<abi::AuthorizationHandler, _>(answered) })
                    else {
                        aterm_log::warn!("notices: macOS was not asked: no handler block");
                        return;
                    };
                    // SAFETY: `-requestAuthorizationWithOptions:completionHandler:` is `abi::RequestAuthorization`, and the center copies the handler.
                    unsafe {
                        let request: abi::RequestAuthorization = checked_msg();
                        request(
                            center,
                            sel!(requestAuthorizationWithOptions:completionHandler:),
                            options,
                            handler.as_block_ptr(),
                        );
                    }
                });
            });
        if let Err(e) = spawned {
            aterm_log::warn!("notices: macOS was not asked: {e}");
        }
    }

    /// Post `notice` as aterm.
    pub(super) fn post(notice: &Notice<'_>) -> bool {
        let Some(center) = center() else {
            return false;
        };
        aterm_objc::autoreleasepool(|_| {
            let content_class = class(c"UNMutableNotificationContent");
            let request_class = class(c"UNNotificationRequest");
            if content_class.is_null() || request_class.is_null() {
                return false;
            }
            let (Some(title), Some(body)) = (
                aterm_objc::ns_string(notice.title),
                aterm_objc::ns_string(notice.body),
            ) else {
                return false;
            };
            // SAFETY: `+alloc` then `-init` give a +1 content object, which `content` owns.
            let content = unsafe {
                Obj::from_owned(send_id(aterm_objc::send::alloc(content_class), sel!(init)))
            };
            let Some(content) = content else {
                return false;
            };
            let info = super::user_info(notice.session);
            // SAFETY: each setter is `-(void)(id)` on the live content, which copies what it is given.
            unsafe {
                send_v_id(content.id(), sel!(setTitle:), title.id());
                send_v_id(content.id(), sel!(setBody:), body.id());
                if let Some(info) = info {
                    send_v_id(content.id(), sel!(setUserInfo:), info);
                }
            }
            let identifier = format!(
                "aterm.{}.{}",
                std::process::id(),
                NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
            );
            let Some(identifier) = aterm_objc::ns_string(&identifier) else {
                return false;
            };
            // SAFETY: `+requestWithIdentifier:content:trigger:` is
            // `abi::MakeRequest`; a nil trigger delivers at once. The request is
            // autoreleased into this pool.
            let request = unsafe {
                let make: abi::MakeRequest = checked_msg();
                make(
                    request_class.as_id(),
                    sel!(requestWithIdentifier:content:trigger:),
                    identifier.id(),
                    content.id(),
                    Id::NIL,
                )
            };
            if request.is_null() {
                return false;
            }
            let refused = |error: Id| {
                if !error.is_null() {
                    aterm_log::warn!(
                        "notices: macOS did not take one of aterm's notices: {}",
                        // SAFETY: the error is a live NSError for the call.
                        unsafe { aterm_objc::ns_error_string(error) }
                    );
                }
            };
            // SAFETY: the handler is `abi::AddHandler`, exactly `refused`'s.
            let handler = unsafe { RcBlock::of::<abi::AddHandler, _>(refused) };
            let block = handler.as_ref().map_or(
                BlockPtr::from_ptr(std::ptr::null_mut()),
                RcBlock::as_block_ptr,
            );
            // SAFETY: `-addNotificationRequest:withCompletionHandler:` is
            // `abi::AddRequest` on the live center; the handler may be nil, and
            // the center copies it.
            unsafe {
                let add: abi::AddRequest = checked_msg();
                add(
                    center,
                    sel!(addNotificationRequest:withCompletionHandler:),
                    request,
                    block,
                );
            }
            true
        })
    }

    /// Become the center's delegate, so a click on one of aterm's notices comes back ([`NoticeDelegate`]).
    pub(super) fn adopt_clicks() {
        if ADOPTED.load(Ordering::Acquire) {
            return;
        }
        let Some(center) = CENTER.get().copied().flatten().map(at) else {
            return;
        };
        let Some(main) = aterm_objc::MainThread::new() else {
            return;
        };
        let Some(delegate) = NoticeDelegate::alloc_init(main, ()) else {
            return;
        };
        // SAFETY: `-setDelegate:` is `-(void)(id)` on the live center, which
        // holds its delegate weakly — so the delegate is kept for the process
        // below.
        unsafe { send_v_id(center, sel!(setDelegate:), delegate.as_id()) };
        std::mem::forget(delegate);
        ADOPTED.store(true, Ordering::Release);
        aterm_log::info!("notices: aterm is the center's delegate; a click comes back");
    }
}

/// The center's delegate and what it reads off a response: declared in a test
/// build too, where a test reads the encodings it registered against the
/// protocol's (`tests::every_prototype_the_notices_state_is_the_runtimes`)
/// and sends it a click and a presentation built of stand-ins
/// (`tests::a_click_on_a_notice_goes_to_its_session`,
/// `tests::a_notice_shows_while_aterm_is_in_front`).
#[cfg(target_os = "macos")]
mod clicks {
    use aterm_objc::send::{send_bool_cls, send_id, send_id_id};
    use aterm_objc::{BlockPtr, Id, class, sel};

    use super::{INSTANCE_KEY, PID_KEY, SESSION_KEY, abi};

    const PRESENT_OPTIONS: usize = (1 << 3) | (1 << 4);

    aterm_objc::declare_class! {
        /// The center's delegate.
        pub(super) struct NoticeDelegate: NSObject {
            const NAME: &str = "ATermNoticeDelegate";
            type Ivars = ();
            protocols: [NSObject, UNUserNotificationCenterDelegate];

            @sel(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:)
            fn did_receive(&self, _center: Id, response: Id, done: BlockPtr) {
                if let Some(session) = clicked(response) {
                    crate::notify::raise_clicked(session);
                }
                // SAFETY: the handler is `abi::ResponseDone` (`UNUserNotificationCenter.h`).
                unsafe { done.call::<abi::ResponseDone>(()) };
            }

            @sel(userNotificationCenter:willPresentNotification:withCompletionHandler:)
            fn will_present(&self, _center: Id, _notification: Id, done: BlockPtr) {
                // SAFETY: the handler is `abi::PresentDone`.
                unsafe { done.call::<abi::PresentDone>((PRESENT_OPTIONS,)) };
            }
        }
    }

    fn clicked(response: Id) -> Option<u64> {
        if response.is_null() {
            return None;
        }
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: each is a `-(id)` accessor on a live object or nil (a send to nil answers nil); the answers are borrowed for this pool.
            unsafe {
                let action =
                    aterm_objc::ns_string_to_rust(send_id(response, sel!(actionIdentifier)));
                let notification = send_id(response, sel!(notification));
                let request = send_id(notification, sel!(request));
                let content = send_id(request, sel!(content));
                let info = send_id(content, sel!(userInfo));
                let session = string_for(info, SESSION_KEY);
                let pid = string_for(info, PID_KEY);
                let instance = string_for(info, INSTANCE_KEY);
                super::clicked_session(
                    &action,
                    session.as_deref(),
                    pid.as_deref(),
                    instance.as_deref(),
                    std::process::id(),
                    crate::control_auth::process_instance_id(),
                )
            }
        })
    }

    /// # Safety
    /// `info` must be a live `NSDictionary`, or nil.
    unsafe fn string_for(info: Id, key: &str) -> Option<String> {
        if info.is_null() {
            return None;
        }
        let key = aterm_objc::ns_string(key)?;
        // SAFETY: `-objectForKey:` is `-(id)(id)` and `-isKindOfClass:`
        // `-(BOOL)(Class)`, on live objects; the value is borrowed from
        // `info`.
        unsafe {
            let value = send_id_id(info, sel!(objectForKey:), key.id());
            if value.is_null() || !send_bool_cls(value, sel!(isKindOfClass:), class(c"NSString")) {
                return None;
            }
            Some(aterm_objc::ns_string_to_rust(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Center, DEFAULT_ACTION, clicked_session, status_answer};

    #[test]
    fn a_response_routes_to_the_session_its_notice_names() {
        let own = 4242;
        let ours = Some("4242");
        let incarnation = "p4242-captured-new-launch";
        let instance = Some(incarnation);
        assert_eq!(
            clicked_session(DEFAULT_ACTION, Some("7"), ours, instance, own, incarnation),
            Some(7)
        );
        assert_eq!(
            clicked_session(DEFAULT_ACTION, Some("0"), ours, instance, own, incarnation),
            Some(0)
        );
        for (case, action, session, pid) in [
            (
                "a dismissal",
                "com.apple.UNNotificationDismissActionIdentifier",
                Some("7"),
                ours,
            ),
            ("a custom action", "reply", Some("7"), ours),
            ("another process's", DEFAULT_ACTION, Some("7"), Some("4243")),
            ("no pid", DEFAULT_ACTION, Some("7"), None),
            ("a garbled pid", DEFAULT_ACTION, Some("7"), Some("42x")),
            ("about no tab", DEFAULT_ACTION, None, ours),
            ("a garbled session", DEFAULT_ACTION, Some("-1"), ours),
        ] {
            assert_eq!(
                clicked_session(action, session, pid, instance, own, incarnation),
                None,
                "{case}"
            );
        }
        for previous in [None, Some(""), Some("p4242-captured-old-launch")] {
            assert_eq!(
                clicked_session(DEFAULT_ACTION, Some("7"), ours, previous, own, incarnation),
                None,
                "an unbound or earlier incarnation must not select this launch's tab"
            );
        }
    }

    #[test]
    fn macos_answers_read_as_ask_refuse_or_grant() {
        assert_eq!(status_answer(0), Center::Unasked);
        assert_eq!(status_answer(1), Center::Refused);
        assert_eq!(status_answer(3), Center::Provisional, "quiet delivery only");
        for granted in [2, 4] {
            assert_eq!(status_answer(granted), Center::Granted, "{granted}");
        }
        for unknown in [-1, 5, 99] {
            assert_eq!(status_answer(unknown), Center::Refused, "{unknown}");
        }
    }

    /// The quiet ask goes only to a bundle never asked; the dialog to one never asked or granted
    /// only quiet delivery; neither to one that answered.
    #[test]
    fn the_quiet_ask_is_made_once_and_the_dialog_only_while_it_is_owed() {
        use super::{Request, asks};
        assert!(asks(Center::Unasked, Request::Provisional));
        assert!(asks(Center::Unasked, Request::Full));
        assert!(!asks(Center::Provisional, Request::Provisional), "once");
        assert!(
            asks(Center::Provisional, Request::Full),
            "Notify Me asks for the rest"
        );
        for answered in [Center::Granted, Center::Refused, Center::Absent] {
            for request in [Request::Provisional, Request::Full] {
                assert!(!asks(answered, request), "{answered:?} {request:?}");
            }
        }
    }

    /// A unit test's center is inert whatever it is asked for: a windowed
    /// instance's arms are not even compiled into a test build, so the
    /// center reads absent, posts nothing, and an ask is only counted.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_test_build_never_reaches_the_notification_center() {
        for center in [
            super::NoticeCenter::for_instance(false),
            super::NoticeCenter::for_instance(true),
            super::NoticeCenter::inert(),
        ] {
            assert_eq!(center.answer(), Center::Absent);
            assert!(!center.post(&super::Notice {
                title: "aterm",
                body: "needs you",
                session: Some(0),
            }));
            center.adopt_clicks();
            let _ = super::take_asks();
            center.ask(Box::new(|| unreachable!("the inert ask answers nothing")));
            assert_eq!(super::take_asks(), 1);
        }
    }

    /// EVERY PROTOTYPE THE NOTICES STATE IS THE RUNTIME'S, read without the
    /// center: the live arms run only in a bundle, where no test reaches them,
    /// and a release build checks no send at the call.
    #[cfg(target_os = "macos")]
    #[test]
    fn every_prototype_the_notices_state_is_the_runtimes() {
        use std::ffi::CStr;

        use aterm_objc::{
            ClassPtr, Id, Sel, block_check, block_signatures, class, method_types, protocol,
            protocol_method_extended_types, protocol_method_types, prototype_check, sel,
            strip_method_offsets,
        };

        use super::abi;

        type SendId = unsafe extern "C-unwind" fn(Id, Sel) -> Id;
        type SendVoidId = unsafe extern "C-unwind" fn(Id, Sel, Id);
        type SendIsize = unsafe extern "C-unwind" fn(Id, Sel) -> isize;
        type Alloc = unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id;

        assert!(super::open_framework(), "UserNotifications did not load");
        let named = |name: &'static CStr| {
            let cls = class(name);
            assert!(!cls.is_null(), "{name:?} is not registered");
            cls
        };
        // SAFETY: a live class's own class, its metaclass.
        let meta = |name: &'static CStr| unsafe { aterm_objc::class_of(named(name).as_id()) };
        let center = named(c"UNUserNotificationCenter");
        let content = named(c"UNMutableNotificationContent");

        // SAFETY: live classes; `prototype_check` reads an encoding and sends nothing.
        let sends: Vec<(&str, Result<(), String>)> = unsafe {
            vec![
                (
                    "getNotificationSettingsWithCompletionHandler:",
                    prototype_check::<abi::GetSettings>(
                        center,
                        sel!(getNotificationSettingsWithCompletionHandler:),
                    ),
                ),
                (
                    "requestAuthorizationWithOptions:completionHandler:",
                    prototype_check::<abi::RequestAuthorization>(
                        center,
                        sel!(requestAuthorizationWithOptions:completionHandler:),
                    ),
                ),
                (
                    "addNotificationRequest:withCompletionHandler:",
                    prototype_check::<abi::AddRequest>(
                        center,
                        sel!(addNotificationRequest:withCompletionHandler:),
                    ),
                ),
                (
                    "+requestWithIdentifier:content:trigger:",
                    prototype_check::<abi::MakeRequest>(
                        meta(c"UNNotificationRequest"),
                        sel!(requestWithIdentifier:content:trigger:),
                    ),
                ),
                (
                    "+dictionaryWithObjects:forKeys:count:",
                    prototype_check::<abi::MakeDictionary>(
                        meta(c"NSDictionary"),
                        sel!(dictionaryWithObjects:forKeys:count:),
                    ),
                ),
                (
                    "+currentNotificationCenter",
                    prototype_check::<SendId>(
                        meta(c"UNUserNotificationCenter"),
                        sel!(currentNotificationCenter),
                    ),
                ),
                (
                    "setDelegate:",
                    prototype_check::<SendVoidId>(center, sel!(setDelegate:)),
                ),
                (
                    "authorizationStatus",
                    prototype_check::<SendIsize>(
                        named(c"UNNotificationSettings"),
                        sel!(authorizationStatus),
                    ),
                ),
                (
                    "+alloc",
                    prototype_check::<Alloc>(meta(c"UNMutableNotificationContent"), sel!(alloc)),
                ),
                ("init", prototype_check::<SendId>(content, sel!(init))),
                (
                    "setTitle:",
                    prototype_check::<SendVoidId>(content, sel!(setTitle:)),
                ),
                (
                    "setBody:",
                    prototype_check::<SendVoidId>(content, sel!(setBody:)),
                ),
                (
                    "setUserInfo:",
                    prototype_check::<SendVoidId>(content, sel!(setUserInfo:)),
                ),
                (
                    "actionIdentifier",
                    prototype_check::<SendId>(
                        named(c"UNNotificationResponse"),
                        sel!(actionIdentifier),
                    ),
                ),
                (
                    "notification",
                    prototype_check::<SendId>(named(c"UNNotificationResponse"), sel!(notification)),
                ),
                (
                    "request",
                    prototype_check::<SendId>(named(c"UNNotification"), sel!(request)),
                ),
                (
                    "content",
                    prototype_check::<SendId>(named(c"UNNotificationRequest"), sel!(content)),
                ),
                (
                    "userInfo",
                    prototype_check::<SendId>(named(c"UNNotificationContent"), sel!(userInfo)),
                ),
            ]
        };
        for (row, result) in sends {
            assert_eq!(result, Ok(()), "{row}");
        }
        // SAFETY: as above.
        let signed_twin = unsafe {
            prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> usize>(
                named(c"UNNotificationSettings"),
                sel!(authorizationStatus),
            )
        };
        assert!(signed_twin.is_err(), "an unsigned status read passed");

        let server = protocol(c"UNUserNotificationServerProtocol");
        assert!(
            !server.is_null(),
            "UserNotifications no longer registers UNUserNotificationServerProtocol: \
             find where this macOS records the center's handler signatures"
        );
        let handler = |row: Sel| {
            // SAFETY: a live protocol object; a read-only query.
            let types = unsafe { protocol_method_extended_types(server, row, true) }
                .unwrap_or_else(|| panic!("{row:?} is not in the server protocol"));
            let blocks = block_signatures(&types);
            assert_eq!(blocks.len(), 1, "{row:?}: {types}");
            blocks[0].clone()
        };
        let settings =
            handler(sel!(getNotificationSettingsForBundleIdentifier:withCompletionHandler:));
        for (row, result) in [
            ("settings", block_check::<abi::SettingsHandler>(&settings)),
            (
                "authorization",
                block_check::<abi::AuthorizationHandler>(&handler(sel!(
                    requestAuthorizationWithOptions:forBundleIdentifier:completionHandler:
                ))),
            ),
            (
                "add",
                block_check::<abi::AddHandler>(&handler(sel!(
                    addNotificationRequest:forBundleIdentifier:withCompletionHandler:
                ))),
            ),
        ] {
            assert_eq!(result, Ok(()), "the {row} handler");
        }
        assert!(
            block_check::<fn(usize)>(&settings).is_err(),
            "a settings handler taking an options word passed"
        );

        let (delegate, cls) = delegate_class();
        let receive =
            sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:);
        let present = sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:);
        for row in [receive, present] {
            // SAFETY: a live protocol and a class this module declared; read-only queries.
            let (want, got) = unsafe {
                (
                    protocol_method_types(delegate, row, true),
                    method_types(cls, row),
                )
            };
            let want = want.unwrap_or_else(|| panic!("{row:?} is not a delegate row"));
            assert_eq!(
                got.as_deref(),
                Some(strip_method_offsets(&want).as_str()),
                "{row:?}"
            );
        }
        assert_eq!(
            block_check::<abi::ResponseDone>(&delegate_block(delegate, receive)),
            Ok(()),
            "the response handler"
        );
        assert_eq!(
            block_check::<abi::PresentDone>(&delegate_block(delegate, present)),
            Ok(()),
            "the presentation handler"
        );

        unsafe extern "C" {
            fn dlsym(
                handle: *mut std::ffi::c_void,
                symbol: *const std::ffi::c_char,
            ) -> *mut std::ffi::c_void;
        }
        const RTLD_DEFAULT: *mut std::ffi::c_void = -2_isize as *mut std::ffi::c_void;
        // SAFETY: a NUL-terminated symbol name; the answer is the address of an `NSString *const`, or null.
        let symbol = unsafe {
            dlsym(
                RTLD_DEFAULT,
                c"UNNotificationDefaultActionIdentifier".as_ptr(),
            )
        };
        assert!(
            !symbol.is_null(),
            "no UNNotificationDefaultActionIdentifier"
        );
        // SAFETY: the exported constant is an immortal `NSString *`.
        let exported = unsafe { aterm_objc::ns_string_to_rust(*symbol.cast::<Id>()) };
        assert_eq!(exported, super::DEFAULT_ACTION);
    }

    /// THE DELEGATE'S CLASS, claiming the host's
    /// `UNUserNotificationCenterDelegate`, with that protocol: the protocol
    /// must be the host's before the class claims it, or the claim registers a
    /// stand-in with no rows that every later reader sees, so each test that
    /// declares the class asks here first.
    #[cfg(target_os = "macos")]
    fn delegate_class() -> (aterm_objc::ProtocolPtr, aterm_objc::ClassPtr) {
        use aterm_objc::ClassType as _;
        let delegate = aterm_objc::protocol(c"UNUserNotificationCenterDelegate");
        assert!(
            !delegate.is_null()
                && !aterm_objc::protocols_registered_by_aterm()
                    .contains(&c"UNUserNotificationCenterDelegate"),
            "this host registers no UNUserNotificationCenterDelegate of its own"
        );
        (delegate, super::clicks::NoticeDelegate::class())
    }

    /// Stand-ins for what macOS hands the delegate: a response
    /// (`UNNotificationResponse`) for the notice it answers (`UNNotification`)
    /// for its request (`UNNotificationRequest`) for its content
    /// (`UNNotificationContent`), each an `NSObject` subclass made at run time
    /// that answers only the selectors the delegate reads off that link, with
    /// the runtime's `-(id)` encoding.
    #[cfg(target_os = "macos")]
    mod stand_ins {
        use aterm_objc::{Id, Obj, Retained};

        aterm_objc::declare_class! {
            /// A `UNNotificationResponse`: the action taken, and the notice.
            pub(super) struct Response: NSObject {
                const NAME: &str = "ATermTestNoticeResponse";
                type Ivars = (Obj, Retained<Notification>);

                @sel(actionIdentifier)
                fn action_identifier(&self) -> Id {
                    self.ivars().0.id()
                }

                @sel(notification)
                fn notification(&self) -> Id {
                    self.ivars().1.as_id()
                }
            }
        }

        aterm_objc::declare_class! {
            /// A `UNNotification`: its request.
            pub(super) struct Notification: NSObject {
                const NAME: &str = "ATermTestNotice";
                type Ivars = Retained<Request>;

                @sel(request)
                fn request(&self) -> Id {
                    self.ivars().as_id()
                }
            }
        }

        aterm_objc::declare_class! {
            /// A `UNNotificationRequest`: its content.
            pub(super) struct Request: NSObject {
                const NAME: &str = "ATermTestNoticeRequest";
                type Ivars = Retained<Content>;

                @sel(content)
                fn content(&self) -> Id {
                    self.ivars().as_id()
                }
            }
        }

        aterm_objc::declare_class! {
            /// A `UNNotificationContent`: its `userInfo`, or nil.
            pub(super) struct Content: NSObject {
                const NAME: &str = "ATermTestNoticeContent";
                type Ivars = Option<Obj>;

                @sel(userInfo)
                fn user_info(&self) -> Id {
                    self.ivars().as_ref().map_or(Id::NIL, Obj::id)
                }
            }
        }

        /// A notice whose content carries `info`.
        pub(super) fn notification(info: Option<Obj>) -> Option<Retained<Notification>> {
            let witness = crate::appkit::test_witness();
            let content = Content::alloc_init(witness, info)?;
            let request = Request::alloc_init(witness, content)?;
            Notification::alloc_init(witness, request)
        }

        /// The response `action` makes to a notice whose content carries `info`.
        pub(super) fn response(action: &str, info: Option<Obj>) -> Option<Retained<Response>> {
            let action = aterm_objc::ns_string(action)?;
            Response::alloc_init(crate::appkit::test_witness(), (action, notification(info)?))
        }
    }

    /// The `userInfo` a post writes for `session` ([`super::user_info`]),
    /// owned.
    #[cfg(target_os = "macos")]
    fn posted_info(session: Option<u64>) -> Option<aterm_objc::Obj> {
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: an autoreleased dictionary, live for this pool, retained to outlive it.
            super::user_info(session).and_then(|info| unsafe { aterm_objc::Obj::retain(info) })
        })
    }

    /// A CLICK ON ONE OF aterm's NOTICES GOES TO ITS SESSION, through the
    /// real delegate: `-userNotificationCenter:didReceiveNotificationResponse:
    /// withCompletionHandler:` sent to a `NoticeDelegate`, with a response
    /// built of stand-ins ([`stand_ins`]) around the very `userInfo` a post
    /// writes ([`super::user_info`]) and a real block for the completion
    /// handler.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_click_on_a_notice_goes_to_its_session() {
        use std::cell::Cell;
        use std::rc::Rc;

        use aterm_objc::{BlockPtr, Id, RcBlock, Sel, checked_msg, sel};

        type DidReceive = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr);

        let _ = delegate_class();
        let delegate = super::clicks::NoticeDelegate::alloc_init(crate::appkit::test_witness(), ())
            .expect("a delegate");
        let respond = |action: &str, session: Option<u64>| {
            let response = stand_ins::response(action, posted_info(session)).expect("a response");
            let calls = Rc::new(Cell::new(0_u32));
            let counted = Rc::clone(&calls);
            // SAFETY: `abi::ResponseDone` is the handler's prototype, and the closure's.
            let done = unsafe {
                RcBlock::of::<super::abi::ResponseDone, _>(move || counted.set(counted.get() + 1))
            }
            .expect("a handler block");
            let _ = crate::notify::take_wakes();
            aterm_objc::autoreleasepool(|_| {
                // SAFETY: the delegate row is `DidReceive`; the delegate and
                // the response are live, the center nil (the delegate reads
                // none) and the block a live `abi::ResponseDone`.
                unsafe {
                    let send: DidReceive = checked_msg();
                    send(
                        delegate.as_id(),
                        sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                        Id::NIL,
                        response.as_id(),
                        done.as_block_ptr(),
                    );
                }
            });
            (crate::notify::take_wakes(), calls.get())
        };

        let (wakes, calls) = respond(DEFAULT_ACTION, Some(7));
        assert!(
            matches!(
                wakes.as_slice(),
                [crate::Wake::NoticeClicked { session: 7 }]
            ),
            "the click goes to session 7: {wakes:?}"
        );
        assert_eq!(calls, 1, "the handler runs once");
        for (case, action, session) in [
            ("a notice about no tab", DEFAULT_ACTION, None),
            (
                "a dismissal",
                "com.apple.UNNotificationDismissActionIdentifier",
                Some(7),
            ),
        ] {
            let (wakes, calls) = respond(action, session);
            assert!(wakes.is_empty(), "{case} goes nowhere: {wakes:?}");
            assert_eq!(calls, 1, "{case}: the handler runs once");
        }
    }

    /// A NOTICE THAT ARRIVES WHILE aterm IS IN FRONT STILL SHOWS, through the
    /// real delegate's `-userNotificationCenter:willPresentNotification:
    /// withCompletionHandler:`, with a stand-in notice about a tab and a real
    /// block: the handler is handed List | Banner
    /// (`UNNotificationPresentationOptions`, `1 << 3 | 1 << 4`: no sound, no
    /// badge), exactly once, and the presentation goes to no tab, since it is
    /// not a click.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_notice_shows_while_aterm_is_in_front() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use aterm_objc::{BlockPtr, Id, RcBlock, Sel, checked_msg, sel};

        type WillPresent = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr);
        const LIST_AND_BANNER: usize = (1 << 3) | (1 << 4);

        let _ = delegate_class();
        let delegate = super::clicks::NoticeDelegate::alloc_init(crate::appkit::test_witness(), ())
            .expect("a delegate");
        let notice = stand_ins::notification(posted_info(Some(7))).expect("a notice");
        let handed = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&handed);
        // SAFETY: `abi::PresentDone` is the handler's prototype, and the closure's.
        let done = unsafe {
            RcBlock::of::<super::abi::PresentDone, _>(move |options: usize| {
                seen.borrow_mut().push(options);
            })
        }
        .expect("a handler block");
        let _ = crate::notify::take_wakes();
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: the delegate row is `WillPresent`; the delegate and the
            // notice are live, the center nil and the block a live
            // `abi::PresentDone`.
            unsafe {
                let send: WillPresent = checked_msg();
                send(
                    delegate.as_id(),
                    sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                    Id::NIL,
                    notice.as_id(),
                    done.as_block_ptr(),
                );
            }
        });
        assert_eq!(*handed.borrow(), vec![LIST_AND_BANNER]);
        let wakes = crate::notify::take_wakes();
        assert!(wakes.is_empty(), "showing is not a click: {wakes:?}");
    }

    /// The one block signature a delegate protocol row records.
    #[cfg(target_os = "macos")]
    fn delegate_block(delegate: aterm_objc::ProtocolPtr, row: aterm_objc::Sel) -> String {
        // SAFETY: a live protocol object; a read-only query.
        let extended = unsafe { aterm_objc::protocol_method_extended_types(delegate, row, true) }
            .unwrap_or_else(|| panic!("{row:?} has no extended row"));
        let blocks = aterm_objc::block_signatures(&extended);
        assert_eq!(blocks.len(), 1, "{row:?}: {extended}");
        blocks[0].clone()
    }
}
