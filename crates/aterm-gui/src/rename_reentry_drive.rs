// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exact Rename This Mac modal-reentry seam, in its own process.
//!
//! The real naming alert opens from a Winit `user_event` callback while that callback owns the
//! application handler. Its first field becomes the AppKit field editor, naturally taking key
//! focus from a visible Winit window. After Cancel returns from `runModal`, one bounded default
//! run-loop turn makes the queued focus callback runnable before the outer handler returns. The
//! callback must remain deferred until that return, then precede `Destroyed` and `LoopExiting`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_agent::machine_name::InFleet;
use aterm_objc::cf::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use aterm_objc::{Id, class, sel};
use aterm_window::application::ApplicationHandler;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use aterm_window::platform::macos::EventLoopExtMacOS as _;
use aterm_window::window::{Window, WindowId};

use crate::machine_rename_sheet::{self as sheet, Answer};
use crate::machine_title::MachineView;
use crate::platform::Windowed;
use crate::{AppActivation, LaunchPosture, appkit};

const PASS: i32 = 0;
const FINDING: i32 = 1;
const NOT_RUN: i32 = 2;

/// A wait for the production alert that must appear: a hang detector, not a latency budget. The
/// example process owns the authoritative whole-child watchdog covering startup and the alert.
/// It can expire first; reaching this inner deadline is not promised.
const ALERT_HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);
/// Availability observation, as in the toolbar drive: lack of key focus measures no seam.
/// It is NOT RUN, never a latency finding or a pass; the outer process watchdog stays intact.
const ACTIVATION_WAIT: Duration = Duration::from_secs(10);

/// Run the exact-crash child. The caller must isolate this in a watched process: `run_app` has no
/// in-process deadline because controlling AppKit from a background thread would be a second,
/// invalid execution path.
#[must_use]
pub fn run_rename_reentry_drive() -> i32 {
    eprintln!("rename-reentry child: checking display availability");
    if let Some(why) = crate::driver_display_unavailable() {
        eprintln!("rename-reentry child: NOT RUN — {why}");
        return NOT_RUN;
    }
    eprintln!("rename-reentry child: launching windowed platform");
    let windowed = Windowed::launch();
    eprintln!("rename-reentry child: windowed platform launched");
    run(windowed)
}

#[derive(Clone, Copy, Debug)]
enum Wake {
    OpenRename,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Marker {
    FocusedFalse,
    Destroyed,
    LoopExiting,
}

struct Driver {
    windowed: Windowed,
    window: Option<Window>,
    proxy: EventLoopProxy<Wake>,
    open_sent: bool,
    attempted: bool,
    handler_active: bool,
    alert_returned: bool,
    cancel_answered: bool,
    ordering_armed: bool,
    markers: Vec<Marker>,
    unavailable: Option<String>,
    finding: Option<String>,
    passed: bool,
    activation_deadline: Option<Instant>,
}

impl Driver {
    fn fail(&mut self, why: impl Into<String>) {
        if self.finding.is_none() {
            self.finding = Some(why.into());
        }
    }

    fn is_window(&self, id: WindowId) -> bool {
        self.window.as_ref().map(Window::id) == Some(id)
    }
}

impl ApplicationHandler<Wake> for Driver {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() || self.unavailable.is_some() {
            return;
        }
        eprintln!("rename-reentry child: resumed; creating fixture window");
        let attrs = Window::default_attributes()
            .with_title("aterm Rename This Mac re-entry fixture")
            .with_inner_size(aterm_window::dpi::LogicalSize::new(480.0, 320.0))
            .with_visible(false);
        match event_loop.create_window(attrs) {
            Ok(window) => {
                eprintln!("rename-reentry child: fixture window created; showing it");
                self.window = Some(window);
                let window = self.window.as_ref().expect("the window was just stored");
                let deadline = Instant::now() + ACTIVATION_WAIT;
                self.activation_deadline = Some(deadline);
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                eprintln!("rename-reentry child: waiting for the fixture window to become key");
                window.set_visible(true);
                eprintln!("rename-reentry child: fixture window shown; requesting focus");
                window.focus_window();
                eprintln!("rename-reentry child: fixture focus requested");
            }
            Err(error) => {
                self.unavailable = Some(format!("no fixture window could be created: {error}"));
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self.is_window(window_id) {
            return;
        }
        match event {
            WindowEvent::Focused(true) if !self.open_sent && !self.attempted => {
                eprintln!("rename-reentry child: fixture focused; queuing OpenRename");
                self.activation_deadline = None;
                event_loop.set_control_flow(ControlFlow::Wait);
                self.open_sent = true;
                if self.proxy.send_event(Wake::OpenRename).is_err() {
                    self.unavailable = Some("the fixture event-loop proxy closed".to_owned());
                    event_loop.exit();
                }
            }
            WindowEvent::Focused(false) if self.attempted => {
                eprintln!("rename-reentry child: received focus loss");
                if self.handler_active {
                    self.fail("Focused(false) re-entered the active user_event handler");
                }
                if !self.alert_returned {
                    self.fail("Focused(false) arrived before the real rename alert returned");
                }
                self.markers.push(Marker::FocusedFalse);
                if self
                    .markers
                    .iter()
                    .filter(|marker| **marker == Marker::FocusedFalse)
                    .count()
                    != 1
                {
                    self.fail("the real rename round delivered Focused(false) more than once");
                }
            }
            WindowEvent::Destroyed if self.ordering_armed => {
                eprintln!("rename-reentry child: fixture destroyed");
                self.markers.push(Marker::Destroyed);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(deadline) = self.activation_deadline else {
            return;
        };
        if Instant::now() < deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        self.activation_deadline = None;
        if self.window.as_ref().is_some_and(Window::has_focus) {
            self.fail("the fixture became key without delivering its Focused(true) callback");
        } else {
            self.unavailable = Some(
                "macOS did not grant the fixture key focus; the rename re-entry seam was not measured"
                    .to_owned(),
            );
        }
        event_loop.exit();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: Wake) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        self.handler_active = true;
        eprintln!("rename-reentry child: opening the real rename alert from user_event");

        match rename_cancel_round(self.windowed) {
            Ok(()) => self.cancel_answered = true,
            Err(why) => self.fail(why),
        }

        // `runModal` serves its modal mode. Turn the default mode once while this outer handler
        // still owns the ApplicationHandler. The focus transition remains natural; this only
        // makes the queued Winit callback runnable at the same point as the reported crash.
        // SAFETY: this is the main-thread Winit callback and the mode constant is process-static.
        unsafe {
            let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 1);
        }
        if !self.markers.is_empty() {
            self.fail(format!(
                "a rename focus callback re-entered user_event: {:?}",
                self.markers
            ));
        }

        self.alert_returned = true;
        self.ordering_armed = true;
        event_loop.exit();
        if !event_loop.exiting() {
            self.fail("ActiveEventLoop::exiting() stayed false after the rename round");
        }
        eprintln!("rename-reentry child: user_event returned after Cancel");
        self.handler_active = false;
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        eprintln!("rename-reentry child: LoopExiting");
        if !self.ordering_armed {
            return;
        }
        self.markers.push(Marker::LoopExiting);
        if self.markers == [Marker::FocusedFalse, Marker::Destroyed, Marker::LoopExiting]
            && self.cancel_answered
            && self.finding.is_none()
        {
            self.passed = true;
        } else if self.finding.is_none() {
            self.fail(format!(
                "event order was {:?}, not one Focused(false) after user_event, then Destroyed \
                 and LoopExiting",
                self.markers
            ));
        }
    }
}

fn run(windowed: Windowed) -> i32 {
    // Unlike the quiet AppKit gallery, this exact seam requires a key Winit window so the real
    // alert naturally makes it resign focus. Accessory activation avoids a test Dock tile.
    let posture = LaunchPosture {
        policy: Some(AppActivation::Accessory),
        activate_on_launch: true,
    };
    eprintln!("rename-reentry child: building event loop");
    let mut event_loop = match crate::driver_event_loop_builder::<Wake>(posture).build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("rename-reentry child: NOT RUN — no event loop: {error}");
            return NOT_RUN;
        }
    };
    eprintln!("rename-reentry child: event loop built; creating proxy");
    let proxy = event_loop.create_proxy();
    eprintln!("rename-reentry child: preparing launch");
    event_loop.prepare_launch();
    eprintln!("rename-reentry child: launch prepared");
    let mut driver = Driver {
        windowed,
        window: None,
        proxy,
        open_sent: false,
        attempted: false,
        handler_active: false,
        alert_returned: false,
        cancel_answered: false,
        ordering_armed: false,
        markers: Vec::new(),
        unavailable: None,
        finding: None,
        passed: false,
        activation_deadline: None,
    };

    eprintln!("rename-reentry child: entering run_app");
    let result = event_loop.run_app(&mut driver);
    eprintln!("rename-reentry child: run_app returned");
    if let Some(why) = driver.unavailable {
        eprintln!("rename-reentry child: NOT RUN — {why}");
        return NOT_RUN;
    }
    if let Err(error) = result {
        eprintln!("rename-reentry child: FINDING — run_app: {error}");
        return FINDING;
    }
    if let Some(why) = driver.finding {
        eprintln!("rename-reentry child: FINDING — {why}");
        return FINDING;
    }
    if driver.attempted && driver.passed {
        eprintln!(
            "rename-reentry child: OK — the real field editor cancelled without a rename \
             request; one deferred focus callback preceded Destroyed and LoopExiting"
        );
        PASS
    } else {
        eprintln!(
            "rename-reentry child: FINDING — run_app returned without proving the rename \
             callback and exit order"
        );
        FINDING
    }
}

/// Open the production machine-name panel and have a helper click its real Cancel button. The
/// immutable view is the only machine state handed in; `Answer::Cancel` emits no `RenameTo`
/// request for the production caller to apply.
fn rename_cancel_round(windowed: Windowed) -> Result<(), String> {
    let view = MachineView {
        rev: 1,
        name: "m85".to_owned(),
        nick: "Dog".to_owned(),
        word: "Dog".to_owned(),
        fleet: InFleet::Out,
    };
    let done = Arc::new(AtomicBool::new(false));
    let helper_done = Arc::clone(&done);
    let helper = std::thread::spawn(move || cancel_real_rename_alert(&helper_done, windowed));
    let press = sheet::begin();
    let answer = sheet::ask(
        press,
        sheet::TITLE,
        &crate::machine_title::rename_alert_body(None),
        &view,
        None,
        None,
    );
    sheet::end(press);
    eprintln!("rename-reentry child: rename alert returned; waiting for the Cancel helper");
    done.store(true, Ordering::Release);
    let helper_hang = Instant::now() + ALERT_HANG;
    while !helper.is_finished() {
        if Instant::now() >= helper_hang {
            return Err("the rename Cancel helper did not end within one minute".to_owned());
        }
        // If `ask` returned before the helper's main-queue read, service that read instead of
        // joining a helper which is waiting for this thread. This is also the old-tree trigger:
        // the natural focus callback is runnable while `user_event` still owns the handler.
        // SAFETY: main-thread user-event callback and a process-static run-loop mode.
        unsafe {
            let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, POLL.as_secs_f64(), 1);
        }
    }
    let helper = helper
        .join()
        .map_err(|_| "the rename Cancel helper panicked".to_owned())?;
    if !helper? {
        return Err("the rename alert ended before the helper proved and clicked it".to_owned());
    }
    if answer != Some(Answer::Cancel) {
        return Err(format!(
            "the real rename alert answered {answer:?}, not Cancel"
        ));
    }
    Ok(())
}

#[derive(Default)]
struct AlertLook {
    title: bool,
    editable: Vec<Id>,
    cancel: Option<Id>,
}

/// Wait for the exact production panel, including its active first field editor, then click its
/// Cancel button. All AppKit reads and the click execute on the main queue served by `runModal`.
fn cancel_real_rename_alert(done: &AtomicBool, windowed: Windowed) -> Result<bool, String> {
    eprintln!("rename-reentry child: Cancel helper waiting for the field editor");
    let deadline = Instant::now() + ALERT_HANG;
    loop {
        if done.load(Ordering::Acquire) {
            return Ok(false);
        }
        let clicked = aterm_objc::run_on_main(move |_| inspect_and_cancel(windowed));
        match clicked {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(why) => return Err(why),
        }
        if Instant::now() >= deadline {
            aterm_objc::run_on_main(move |_| abort_modal(windowed));
            return Err("the real rename panel was not ready within one minute".to_owned());
        }
        std::thread::sleep(POLL);
    }
}

/// Main thread only. `Ok(false)` means the modal panel is not fully constructed yet.
fn inspect_and_cancel(_windowed: Windowed) -> Result<bool, String> {
    // SAFETY: this runs only in `run_on_main`. All getters are standard AppKit +0 messages, and
    // `performClick:` accepts one object. The modal window retains its content and button.
    unsafe {
        let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
        let window = appkit::send_id(app, sel!(modalWindow));
        if window.is_null() {
            return Ok(false);
        }
        let content = appkit::send_id(window, sel!(contentView));
        if content.is_null() {
            return Ok(false);
        }
        let mut look = AlertLook::default();
        walk_alert(content, &mut look);
        let (Some(cancel), [first, _second]) = (look.cancel, look.editable.as_slice()) else {
            return Ok(false);
        };
        if !look.title {
            return Ok(false);
        }
        let editor = appkit::send_id(*first, sel!(currentEditor));
        if editor.is_null() || appkit::send_id(window, sel!(firstResponder)) != editor {
            return Ok(false);
        }
        eprintln!("rename-reentry child: field editor ready; clicking Cancel");
        appkit::send_v_id(cancel, sel!(performClick:), Id::NIL);
        Ok(true)
    }
}

/// # Safety
/// Main thread; `view` is nil or a live `NSView` held by the modal window.
unsafe fn walk_alert(view: Id, look: &mut AlertLook) {
    if view.is_null() {
        return;
    }
    // SAFETY: the caller proves a live NSView. The class predicates and view-tree accessors use
    // their documented AppKit signatures; array indices are bounded by `-count`.
    unsafe {
        if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSButton")) {
            let title = appkit::nsstring_to_rust(appkit::send_id(view, sel!(title)));
            if title == sheet::BUTTONS[1] && look.cancel.is_none() {
                look.cancel = Some(view);
            }
        } else if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSTextField")) {
            let text = appkit::nsstring_to_rust(appkit::send_id(view, sel!(stringValue)));
            look.title |= text == sheet::TITLE;
            if appkit::send_bool(view, sel!(isEditable)) {
                look.editable.push(view);
            }
        }
        let subviews = appkit::send_id(view, sel!(subviews));
        let count = if subviews.is_null() {
            0
        } else {
            appkit::send_usize(subviews, sel!(count))
        };
        for index in 0..count {
            walk_alert(
                appkit::send_id_usize(subviews, sel!(objectAtIndex:), index),
                look,
            );
        }
    }
}

/// Main thread only; releases a malformed or incomplete fixture instead of leaving its process
/// in `runModal`. The watched parent remains the authoritative bound.
fn abort_modal(_windowed: Windowed) {
    // SAFETY: called only through `run_on_main`; `-abortModal` takes no arguments.
    unsafe {
        let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
        appkit::send_v(app, sel!(abortModal));
    }
}
