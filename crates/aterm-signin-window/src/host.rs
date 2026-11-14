// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's host process: an accessory `NSApplication` with one titled window whose
//! content is a `WKWebView`, driven by the [`crate::wire`] lines on standard input.
//!
//! The opener's first line says what to open ([`wire::Open`]); the host takes no argument, so
//! nothing of the account is in a process listing. The main thread then runs AppKit's loop. A
//! reader thread takes each request line and carries it out on the main thread
//! ([`aterm_objc::run_on_main`]); an evaluation's reply is written when WebKit calls its
//! completion block, and events are written from the delegate's methods. A page is sent only
//! where its [`wire::Open`] lets it go ([`admits`]): any other navigation of a whole page is
//! refused before it starts. The process ends [`LINGER`] after its window closes, so WebKit's
//! network process has written the page's cookies to the account's data store first.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aterm_macos_ui::{appkit, webkit};
use aterm_objc::send::{
    alloc, send_bool, send_bool_cls, send_bool_isize, send_bool_sel, send_id, send_id_id,
    send_id_rect_usize_usize_bool, send_isize, send_v, send_v_bool, send_v_id,
};
use aterm_objc::{
    BlockPtr, CGPoint, CGRect, CGSize, Id, MainThread, Obj, RcBlock, Retained, Sel,
    autoreleasepool, checked_msg, class, ns_string, ns_string_to_rust, sel,
};

use crate::wire::{self, Request};

/// The window's content size, in points: a sign-in page's column and its consent page.
const SIZE: CGSize = CGSize {
    width: 520.0,
    height: 760.0,
};

/// `NSWindowStyleMaskTitled | NSWindowStyleMaskClosable`.
const STYLE_TITLED_CLOSABLE: usize = 1 | 2;
/// `NSBackingStoreBuffered`.
const BACKING_BUFFERED: usize = 2;
/// `NSApplicationActivationPolicyAccessory`: no Dock icon, no menu bar.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;

/// How long the process lives on after its window closed.
const LINGER: Duration = Duration::from_secs(3);

/// The window, its page and their delegate, on the main thread.
struct Window {
    window: Obj,
    web: Obj,
    /// What both pages are made with: the account's data store.
    configuration: Obj,
    /// The account's data store, which both pages keep their session in.
    store: Obj,
    /// The aside page, made at its first request: a page on the same data store that is in
    /// no window, so what it loads is not shown and the window's page stays as it stands.
    aside: Option<Obj>,
    target: Retained<Target>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Whether the window is closing or closed: no request is carried out after it.
static ENDING: AtomicBool = AtomicBool::new(false);

/// Where a page of this window may be sent ([`wire::Open`]'s hosts and loopback port), set
/// once before the window is made.
static ADMITTED: OnceLock<(Vec<String>, Option<u16>)> = OnceLock::new();

/// `WKNavigationActionPolicyCancel` and `WKNavigationActionPolicyAllow`.
const POLICY_CANCEL: isize = 0;
const POLICY_ALLOW: isize = 1;

/// The handler `-webView:decidePolicyForNavigationAction:decisionHandler:` calls:
/// `void (^)(WKNavigationActionPolicy)`.
type Decide = fn(isize);

/// Whether a whole page may be sent to the address with this `scheme`, `host` and `port`:
/// `https` on one of `hosts` at its own port, `http` on this Mac's loopback address at
/// exactly `loopback` (the waiting sign-in's listener), or `about:` (the empty page a web
/// view starts on). Nothing else: no other host, no other scheme, no other local port.
fn admits(
    scheme: &str,
    host: &str,
    port: Option<u16>,
    hosts: &[String],
    loopback: Option<u16>,
) -> bool {
    match scheme.to_ascii_lowercase().as_str() {
        "about" => true,
        "https" => {
            port.is_none_or(|port| port == 443)
                && hosts.iter().any(|named| named.eq_ignore_ascii_case(host))
        }
        "http" => {
            loopback.is_some()
                && port == loopback
                && (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1")
        }
        _ => false,
    }
}

/// Whether the navigation `action` asks for goes ahead: one inside a frame of a page always
/// (a page's own frames are its own), one of a whole page only where [`admits`] lets it.
fn navigation_admitted(action: Id) -> bool {
    let Some((hosts, loopback)) = ADMITTED.get() else {
        return false;
    };
    // SAFETY: `action` is the live `WKNavigationAction` WebKit passed, on the main thread.
    // `-targetFrame` and `-request` are `@16@0:8` and answer a borrowed object or nil;
    // `-isMainFrame` is `B16@0:8`; `-URL`, `-scheme`, `-host` and `-port` are `@16@0:8` on a
    // live object and answer a borrowed object or nil; `-integerValue` is `q16@0:8`.
    unsafe {
        let frame = send_id(action, sel!(targetFrame));
        if !frame.is_null() && !send_bool(frame, sel!(isMainFrame)) {
            return true;
        }
        let request = send_id(action, sel!(request));
        if request.is_null() {
            return false;
        }
        let url = send_id(request, sel!(URL));
        if url.is_null() {
            return false;
        }
        let scheme = ns_string_to_rust(send_id(url, sel!(scheme)));
        let host = ns_string_to_rust(send_id(url, sel!(host)));
        let number = send_id(url, sel!(port));
        let port = if number.is_null() {
            None
        } else {
            match u16::try_from(send_isize(number, sel!(integerValue))) {
                Ok(port) => Some(port),
                Err(_) => return false,
            }
        };
        admits(&scheme, &host, port, hosts, *loopback)
    }
}

/// Whether `web` is the window's own page (and not its aside page).
fn is_windows_page(web: Id) -> bool {
    WINDOW.with_borrow(|window| window.as_ref().is_some_and(|window| window.web.id() == web))
}

/// Writes one line to the opener. An opener that is gone drives nothing: the process ends.
fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
        std::process::exit(0);
    }
}

/// Ends the host once: says `event` where there is one, takes the window off the screen,
/// and exits [`LINGER`] later.
fn end(event: Option<&str>) {
    if ENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(name) = event {
        emit(&wire::event(name));
    }
    WINDOW.with_borrow(|window| {
        if let Some(window) = window {
            // SAFETY: `-orderOut:` is `v24@0:8@16` on the live window, on the main thread.
            unsafe { send_v_id(window.window.id(), sel!(orderOut:), Id::NIL) };
        }
    });
    std::thread::spawn(|| {
        std::thread::sleep(LINGER);
        std::process::exit(0);
    });
}

aterm_objc::declare_class! {
    /// The window's delegate and the page's navigation delegate.
    struct Target: NSObject {
        const NAME: &str = "ATermSignInWindowTarget";
        type Ivars = ();

        /// A person closed the window (the host's own close hides it and never gets here).
        @sel(windowWillClose:)
        fn window_will_close(&self, _note: Id) {
            if MainThread::new().is_some() {
                end(Some(wire::CLOSED));
            }
        }

        /// The window's page finished a navigation. (The aside page says nothing of its
        /// own: its reader asks it what it holds.)
        @sel(webView:didFinishNavigation:)
        fn did_finish(&self, web: Id, _navigation: Id) {
            if !ENDING.load(Ordering::SeqCst) && is_windows_page(web) {
                emit(&wire::event(wire::LOADED));
            }
        }

        /// A committed navigation of the window's page failed.
        @sel(webView:didFailNavigation:withError:)
        fn did_fail(&self, web: Id, _navigation: Id, error: Id) {
            if is_windows_page(web) {
                load_failed(error);
            }
        }

        /// A navigation of the window's page failed before the page committed (an address
        /// nothing answers).
        @sel(webView:didFailProvisionalNavigation:withError:)
        fn did_fail_provisional(&self, web: Id, _navigation: Id, error: Id) {
            if is_windows_page(web) {
                load_failed(error);
            }
        }

        /// A page, the window's or its aside one, is about to be sent somewhere: only where
        /// this window's pages may go ([`navigation_admitted`]). A refused navigation of the
        /// window's page is said ([`wire::REFUSED`]); the page stays where it stands.
        @sel(webView:decidePolicyForNavigationAction:decisionHandler:)
        fn decide_navigation(&self, web: Id, action: Id, decide: BlockPtr) {
            let admitted = !action.is_null() && navigation_admitted(action);
            if !admitted && !ENDING.load(Ordering::SeqCst) && is_windows_page(web) {
                emit(&wire::event(wire::REFUSED));
            }
            // SAFETY: the handler is `Decide` (`WKNavigationDelegate.h`), called once, on the
            // main thread WebKit called this method on.
            unsafe {
                decide.call::<Decide>((if admitted { POLICY_ALLOW } else { POLICY_CANCEL },));
            }
        }
    }
}

/// Says that a navigation failed, with its error's code and nothing else of it.
fn load_failed(error: Id) {
    if ENDING.load(Ordering::SeqCst) {
        return;
    }
    let code = if error.is_null() {
        0
    } else {
        // SAFETY: `error` is the live `NSError` WebKit passed; `-code` is `q16@0:8`.
        unsafe { send_isize(error, sel!(code)) }
    };
    emit(&wire::load_failed(i64::try_from(code).unwrap_or(0)));
}

/// The address the page is at, as the web view says it; empty where it has none.
fn page_url(web: Id) -> String {
    // SAFETY: main-thread sends on the live web view: `-URL` is `@16@0:8` and answers a
    // borrowed `NSURL` or nil; `-absoluteString` is `@16@0:8` on it.
    unsafe {
        let url = send_id(web, sel!(URL));
        if url.is_null() {
            return String::new();
        }
        ns_string_to_rust(send_id(url, sel!(absoluteString)))
    }
}

/// Carries out one request on the main thread.
fn handle(_main: MainThread, request: Request) {
    let id = match &request {
        Request::Load { id, .. }
        | Request::Eval { id, .. }
        | Request::Forget { id }
        | Request::Close { id } => *id,
    };
    if ENDING.load(Ordering::SeqCst) {
        emit(&wire::reply_error(id, "the window is closed"));
        return;
    }
    let aside = matches!(
        &request,
        Request::Load { aside: true, .. } | Request::Eval { aside: true, .. }
    );
    let page = WINDOW.with_borrow_mut(|window| {
        let window = window.as_mut()?;
        if !aside {
            return Some(window.web.clone_retained());
        }
        if window.aside.is_none() {
            window.aside = page_with(&window.configuration);
            if let Some(aside) = &window.aside {
                // The aside page goes only where the window's page may.
                // SAFETY: `-setNavigationDelegate:` is `v24@0:8@16` on the page made here;
                // the delegate is held unretained and lives as long as the window does.
                unsafe {
                    send_v_id(
                        aside.id(),
                        sel!(setNavigationDelegate:),
                        window.target.as_id(),
                    );
                }
            }
        }
        window.aside.as_ref().map(Obj::clone_retained)
    });
    let Some(web) = page else {
        emit(&wire::reply_error(
            id,
            if aside {
                "the aside page could not be made"
            } else {
                "the window is closed"
            },
        ));
        return;
    };
    match request {
        Request::Load { url, .. } => {
            let Some(text) = ns_string(&url) else {
                emit(&wire::reply_error(id, "the address could not be held"));
                return;
            };
            // SAFETY: main-thread sends: `+URLWithString:` and `+requestWithURL:` are
            // `@24@0:8@16` and answer borrowed objects or nil; `-loadRequest:` is
            // `@24@0:8@16` on the live web view.
            unsafe {
                let address = send_id_id(class(c"NSURL").as_id(), sel!(URLWithString:), text.id());
                if address.is_null() {
                    emit(&wire::reply_error(
                        id,
                        "the address is not one a page loads",
                    ));
                    return;
                }
                let request = send_id_id(
                    class(c"NSURLRequest").as_id(),
                    sel!(requestWithURL:),
                    address,
                );
                let _ = send_id_id(web.id(), sel!(loadRequest:), request);
            }
            emit(&wire::reply_done(id));
        }
        Request::Eval { script, .. } => {
            let Some(text) = ns_string(&script) else {
                emit(&wire::reply_error(id, "the script could not be held"));
                return;
            };
            let page = web.clone_retained();
            let answered = move |value: Id, error: Id| {
                if !error.is_null() {
                    emit(&wire::reply_error(id, "the page did not run the script"));
                    return;
                }
                // SAFETY: `value` is nil or the live object WebKit passed; `-isKindOfClass:`
                // is `B24@0:8#16`, and a string's contents are copied before the pool pops.
                let value = unsafe {
                    (!value.is_null()
                        && send_bool_cls(value, sel!(isKindOfClass:), class(c"NSString")))
                    .then(|| ns_string_to_rust(value))
                };
                emit(&wire::reply_value(
                    id,
                    value.as_deref(),
                    &page_url(page.id()),
                ));
            };
            // SAFETY: the block's `(id, id) -> void` is the completion signature
            // `-evaluateJavaScript:completionHandler:` declares, called on the main thread.
            let Some(block) = (unsafe { RcBlock::new2(answered) }) else {
                emit(&wire::reply_error(
                    id,
                    "the script's answer could not be taken",
                ));
                return;
            };
            // SAFETY: `-evaluateJavaScript:completionHandler:` is `v32@0:8@16@?24` on the
            // live web view, which copies the block for its call.
            unsafe {
                let evaluate: unsafe extern "C-unwind" fn(Id, Sel, Id, BlockPtr) = checked_msg();
                evaluate(
                    web.id(),
                    sel!(evaluateJavaScript:completionHandler:),
                    text.id(),
                    block.as_block_ptr(),
                );
            }
        }
        Request::Forget { .. } => {
            let Some(store) = WINDOW
                .with_borrow(|window| window.as_ref().map(|window| window.store.clone_retained()))
            else {
                emit(&wire::reply_error(id, "the window is closed"));
                return;
            };
            // SAFETY: the block's `() -> void` is the completion signature
            // `-removeDataOfTypes:modifiedSince:completionHandler:` declares, called on the
            // main thread once the store's data is gone.
            let Some(block) = (unsafe { RcBlock::new0(move || emit(&wire::reply_done(id))) })
            else {
                emit(&wire::reply_error(
                    id,
                    "the store's answer could not be taken",
                ));
                return;
            };
            // SAFETY: main-thread sends. `+allWebsiteDataTypes` and `+distantPast` are
            // `@16@0:8` class sends answering borrowed objects;
            // `-removeDataOfTypes:modifiedSince:completionHandler:` is `v40@0:8@16@24@?32` on
            // the live store, which copies the block for its call.
            unsafe {
                let types = send_id(
                    webkit::class(c"WKWebsiteDataStore").as_id(),
                    sel!(allWebsiteDataTypes),
                );
                let since = send_id(class(c"NSDate").as_id(), sel!(distantPast));
                let remove: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr) = checked_msg();
                remove(
                    store.id(),
                    sel!(removeDataOfTypes:modifiedSince:completionHandler:),
                    types,
                    since,
                    block.as_block_ptr(),
                );
            }
        }
        Request::Close { .. } => {
            emit(&wire::reply_done(id));
            end(None);
        }
    }
}

/// What to open, from the opener's first line: the host takes no word after its verb, so
/// nothing of the account is in its arguments.
///
/// # Errors
/// Why nothing is opened.
fn opening(args: &[OsString], first_line: Option<&str>) -> Result<wire::Open, &'static str> {
    if !args.is_empty() {
        return Err("the sign-in window takes no argument: its first line says what to open");
    }
    let open = wire::parse_open(first_line.ok_or("the opener sent nothing to open")?)?;
    if open.store.as_deref().is_some_and(|store| !is_uuid(store)) {
        return Err("the store is not a UUID");
    }
    if !open.hosts.iter().all(|host| is_host(host)) {
        return Err("a host its page may go to is not a host name");
    }
    Ok(open)
}

/// Whether `text` is a plain host name: letters, digits, dots and hyphens.
fn is_host(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 253
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// Whether `text` is a UUID as `NSUUID` reads one: 8-4-4-4-12 hex digits.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(at, byte)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Whether this process's audit session has the window server and was not made over the
/// network (Security's `SessionGetInfo`, `AuthSession.h`): an SSH login's has neither.
fn graphic_session() -> bool {
    /// `callerSecuritySession`.
    const CALLER_SECURITY_SESSION: u32 = u32::MAX;
    /// `sessionHasGraphicAccess`.
    const SESSION_HAS_GRAPHIC_ACCESS: u32 = 0x0010;
    /// `sessionIsRemote`.
    const SESSION_IS_REMOTE: u32 = 0x1000;
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SessionGetInfo(session: u32, session_id: *mut u32, attributes: *mut u32) -> i32;
    }
    let mut id = 0u32;
    let mut attributes = 0u32;
    // SAFETY: both out-pointers name live locals of the declared C type (`UInt32`); the call
    // only writes them.
    let status =
        unsafe { SessionGetInfo(CALLER_SECURITY_SESSION, &raw mut id, &raw mut attributes) };
    status == 0
        && attributes & SESSION_HAS_GRAPHIC_ACCESS != 0
        && attributes & SESSION_IS_REMOTE == 0
}

/// The data store `store` names, retained, and whether it persists: the account's own where
/// this macOS has per-identifier stores, else one that is gone with the window.
fn data_store(store: Option<&str>) -> (Option<Obj>, bool) {
    let stores = webkit::class(c"WKWebsiteDataStore");
    // SAFETY: main-thread class sends. `+respondsToSelector:` is `B24@0:8:16`;
    // `-initWithUUIDString:` is `@24@0:8@16`, consuming `+alloc`'s +1 and answering +1 or nil;
    // `+dataStoreForIdentifier:` is `@24@0:8@16` and `+nonPersistentDataStore` `@16@0:8`,
    // each answering a borrowed store, retained here.
    unsafe {
        if let Some(store) = store
            && send_bool_sel(
                stores.as_id(),
                sel!(respondsToSelector:),
                sel!(dataStoreForIdentifier:),
            )
            && let Some(text) = ns_string(store)
            && let Some(uuid) = Obj::from_owned(send_id_id(
                alloc(class(c"NSUUID")),
                sel!(initWithUUIDString:),
                text.id(),
            ))
            && let Some(named) = Obj::retain(send_id_id(
                stores.as_id(),
                sel!(dataStoreForIdentifier:),
                uuid.id(),
            ))
        {
            return (Some(named), true);
        }
        (
            Obj::retain(send_id(stores.as_id(), sel!(nonPersistentDataStore))),
            false,
        )
    }
}

/// A page made with `configuration`, the window's size; `None` where WebKit refused it.
fn page_with(configuration: &Obj) -> Option<Obj> {
    let frame = CGRect::new(CGPoint { x: 0.0, y: 0.0 }, SIZE);
    // SAFETY: a main-thread send: `-initWithFrame:configuration:` is `@56@0:8{CGRect}16@48`,
    // consuming `+alloc`'s +1 and answering +1 or nil; `configuration` is live, and WebKit
    // copies it.
    unsafe {
        let init: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) -> Id = checked_msg();
        Obj::from_owned(init(
            alloc(webkit::class(c"WKWebView")),
            sel!(initWithFrame:configuration:),
            frame,
            configuration.id(),
        ))
    }
}

/// Makes the window and its page; `None` where AppKit or WebKit refused a part.
fn open(main: MainThread, options: &wire::Open) -> Option<(Window, bool)> {
    let frame = CGRect::new(CGPoint { x: 0.0, y: 0.0 }, SIZE);
    let (store, persistent) = data_store(options.store.as_deref());
    let store = store?;
    let title = ns_string(&options.title)?;
    let target = Target::alloc_init(main, ())?;
    // SAFETY: main-thread sends on objects made here. `-init` is `@16@0:8` and the window's
    // initializer is sent through its typed helper, each consuming `+alloc`'s +1
    // and answering +1 or nil; the setters are `v24@0:8@16` (`-setReleasedWhenClosed:`
    // `v20@0:8B16`); `-center` and `-orderFrontRegardless` are `v16@0:8`. The window and the
    // page hold their delegate unretained, and `Window` keeps it alive with them.
    unsafe {
        let configuration = Obj::from_owned(send_id(
            alloc(webkit::class(c"WKWebViewConfiguration")),
            sel!(init),
        ))?;
        send_v_id(configuration.id(), sel!(setWebsiteDataStore:), store.id());
        let web = page_with(&configuration)?;
        let window = Obj::from_owned(send_id_rect_usize_usize_bool(
            alloc(appkit::class(c"NSWindow")),
            sel!(initWithContentRect:styleMask:backing:defer:),
            frame,
            STYLE_TITLED_CLOSABLE,
            BACKING_BUFFERED,
            false,
        ))?;
        send_v_bool(window.id(), sel!(setReleasedWhenClosed:), false);
        send_v_id(window.id(), sel!(setTitle:), title.id());
        send_v_id(window.id(), sel!(setContentView:), web.id());
        send_v(window.id(), sel!(center));
        send_v_id(window.id(), sel!(setDelegate:), target.as_id());
        send_v_id(web.id(), sel!(setNavigationDelegate:), target.as_id());
        // In front of the other windows without making aterm the active application: the
        // keyboard stays with what the person is typing in.
        send_v(window.id(), sel!(orderFrontRegardless));
        Some((
            Window {
                window,
                web,
                configuration,
                store,
                aside: None,
                target,
            },
            persistent,
        ))
    }
}

/// Reads the opener's requests until its end of input, each carried out on the main thread.
fn read_requests() {
    let carried = std::panic::catch_unwind(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match wire::parse_request(&line) {
                Ok(request) => aterm_objc::run_on_main(move |main| handle(main, request)),
                Err(why) => eprintln!("aterm: the sign-in window was sent no request: {why}"),
            }
        }
        // The opener closed its end, or is gone: the window goes with it.
        aterm_objc::run_on_main(|_| end(None));
    });
    if carried.is_err() {
        std::process::exit(70);
    }
}

/// The host: [`crate::main_entry`] on macOS.
pub(crate) fn run(args: &[OsString]) -> ExitCode {
    let mut first = String::new();
    let read = std::io::stdin().lock().read_line(&mut first);
    let options = match opening(args, read.ok().filter(|n| *n > 0).map(|_| first.trim_end())) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("aterm: the sign-in window was not opened: {why}");
            return ExitCode::from(2);
        }
    };
    let _ = ADMITTED.set((options.hosts.clone(), options.loopback));
    if !graphic_session() {
        emit(&wire::event(wire::NO_DESKTOP));
        return ExitCode::from(crate::EXIT_NO_DESKTOP);
    }
    let Some(main) = MainThread::new() else {
        eprintln!("aterm: the sign-in window runs only on the process main thread");
        return ExitCode::FAILURE;
    };
    appkit::load();
    // SAFETY: `+sharedApplication` is `@16@0:8`, on the main thread; the application it
    // answers lives as long as the process. `-setActivationPolicy:` is `B24@0:8q16`.
    let app = unsafe {
        send_id(
            appkit::class(c"NSApplication").as_id(),
            sel!(sharedApplication),
        )
    };
    if app.is_null() {
        eprintln!("aterm: the sign-in window has no application");
        return ExitCode::FAILURE;
    }
    // SAFETY: as above.
    let _ =
        unsafe { send_bool_isize(app, sel!(setActivationPolicy:), ACTIVATION_POLICY_ACCESSORY) };
    let opened = autoreleasepool(|_| open(main, &options));
    let Some((window, persistent)) = opened else {
        eprintln!("aterm: the sign-in window could not be made");
        return ExitCode::FAILURE;
    };
    WINDOW.set(Some(window));
    emit(&wire::ready(persistent));
    std::thread::spawn(read_requests);
    // SAFETY: `-run` is `v16@0:8` on the live application, on the main thread. It returns
    // only if something stops the loop; the process ends through `end`.
    unsafe { send_v(app, sel!(run)) };
    ExitCode::SUCCESS
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
