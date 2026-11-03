// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native confirmation driver through the macOS runtime adapter on real `NSAlert`s.
//! The owner confirmation defaults to Cancel; the general menu confirmation defaults to
//! Proceed. Both check their words, buttons, clicks, Return, keypad Enter and Escape.
//!
//! A confirmation is the person's: aterm drops a control client's keys at one
//! (`alert_keys::ConfirmKey::Drop`), so no lab can answer it. This drive answers in process: a
//! helper thread reads the modal window and clicks a button, or posts a key-down to it, through
//! the main queue ([`aterm_objc::run_on_main`]), which AppKit's modal loop serves. A key posted in
//! process is no `hwkey` key, so the sheet's key watch takes it as the person's.

/// Every check passed.
#[cfg(target_os = "macos")]
pub(crate) const PASS: i32 = 0;
/// At least one finding.
#[cfg(target_os = "macos")]
pub(crate) const FINDING: i32 = 1;
/// The drive could not execute here.
#[cfg(target_os = "macos")]
pub(crate) const NOT_RUN: i32 = 2;
/// The sheet never came up within a minute.
#[cfg(target_os = "macos")]
pub(crate) const HUNG: i32 = 3;

/// Raise each confirmation five times (click proceed, click Cancel, press Return, keypad Enter,
/// and Escape) and return the exit code above.
#[cfg(target_os = "macos")]
#[must_use]
pub fn run_confirm_drive() -> i32 {
    macos::run(crate::platform::Windowed::launch())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use aterm_objc::{CGPoint, Id, class, sel};

    use super::{FINDING, HUNG, NOT_RUN, PASS};
    use crate::appkit;
    use crate::platform::{AppRt as _, AppRtMacOS, Windowed};

    /// A wait for what must happen (the sheet coming up, going down): a hang detector.
    const HANG: Duration = Duration::from_secs(60);

    /// How long the helper sleeps between looks, and the main thread runs its loop between checks.
    const STEP: Duration = Duration::from_millis(20);

    /// `NSApplicationActivationPolicyAccessory`: windows and no Dock tile.
    const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;

    /// `NSEventTypeKeyDown`.
    const NS_EVENT_TYPE_KEY_DOWN: usize = 10;

    /// The worker the sheet names.
    const LABEL: &str = "Claude Code";

    /// How the helper answers the sheet once it is up.
    #[derive(Clone, Copy, Debug)]
    enum Answer {
        /// Click the button with this title, as a person's click does.
        Click(&'static str),
        /// Post a key-down to the sheet's window: its name, its characters and its virtual key.
        Key(&'static str, &'static str, u16),
    }

    impl Answer {
        fn name(self) -> &'static str {
            match self {
                Self::Click(title) => title,
                Self::Key(name, _, _) => name,
            }
        }
    }

    /// What the helper read off the modal window, and what became of its answer.
    #[derive(Debug, Default)]
    struct Seen {
        /// Every non-empty text field's string, in view order.
        texts: Vec<String>,
        /// Every button's title, in view order.
        buttons: Vec<String>,
        /// The answer was given: the button was there and clicked, or the key was posted.
        given: bool,
        /// The sheet was still up a minute after the answer, so the helper ended its modal loop.
        stuck: bool,
    }

    pub(super) fn run(windowed: Windowed) -> i32 {
        if appkit::MainThread::new().is_none() {
            eprintln!("objc-confirm-drive: NOT RUN — not on the main thread");
            return NOT_RUN;
        }
        // SAFETY: on the main thread (checked above). `+sharedApplication` is `+(NSApplication *)`
        // (+0, never nil); `-setActivationPolicy:` is `-(BOOL)(NSInteger)` and `-finishLaunching`
        // is `-(void)`.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let _ = appkit::send_bool_isize(
                app,
                sel!(setActivationPolicy:),
                NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY,
            );
            appkit::send_v(app, sel!(finishLaunching));
        }
        let (title, body, proceed) = crate::app_overview::stop_worker_sheet(LABEL);
        let runtime = AppRtMacOS {
            probe_preferences: false,
        };
        let mut findings = Vec::new();
        for (owner, title, body, proceed) in [
            (true, title, body.as_str(), proceed),
            (
                false,
                "Menu confirmation",
                "Confirm this menu action?",
                "Proceed",
            ),
        ] {
            let rounds = [
                (Answer::Click(proceed), true),
                (Answer::Click("Cancel"), false),
                (Answer::Key("Return", "\r", 0x24), !owner),
                (Answer::Key("keypad Enter", "\u{3}", 0x4c), !owner),
                (Answer::Key("Escape", "\u{1b}", 0x35), false),
            ];
            for (answer, want) in rounds {
                let done = Arc::new(AtomicBool::new(false));
                let helper = {
                    let done = Arc::clone(&done);
                    std::thread::spawn(move || answer_when_up(answer, &done, windowed))
                };
                let got = if owner {
                    runtime.confirm_owner(title, body, proceed)
                } else {
                    runtime.confirm(title, body, proceed)
                };
                done.store(true, Ordering::Release);
                // The helper may be waiting on the main queue: serve it until it ends.
                let deadline = Instant::now() + HANG;
                while !helper.is_finished() {
                    if Instant::now() > deadline {
                        eprintln!(
                            "objc-confirm-drive: HUNG — the helper did not end within a minute"
                        );
                        return HUNG;
                    }
                    run_loop_once();
                }
                let Ok(seen) = helper.join() else {
                    eprintln!("objc-confirm-drive: FINDING — the helper panicked");
                    return FINDING;
                };
                judge(
                    answer,
                    want,
                    got,
                    &seen,
                    (title, body, proceed),
                    &mut findings,
                );
            }
        }
        if findings.is_empty() {
            println!(
                "objc-confirm-drive: PASS — owner and menu confirmations showed their words and \
                 buttons; proceed and Cancel clicks, Return/keypad Enter defaults and Escape matched each policy"
            );
            PASS
        } else {
            for finding in &findings {
                eprintln!("objc-confirm-drive: FINDING — {finding}");
            }
            FINDING
        }
    }

    /// What one answer showed, against what the sheet must be.
    fn judge(
        answer: Answer,
        want: bool,
        got: Option<bool>,
        seen: &Seen,
        (title, body, proceed): (&str, &str, &str),
        findings: &mut Vec<String>,
    ) {
        let name = format!("{title}: {}", answer.name());
        println!(
            "objc-confirm-drive: {name} answered {got:?}; texts {:?}; buttons {:?}",
            seen.texts, seen.buttons
        );
        if !seen.given {
            findings.push(format!("{name}: no way to give it: {:?}", seen.buttons));
        }
        if seen.stuck {
            findings.push(format!("{name}: the sheet stayed up a minute after it"));
        }
        if got != Some(want) {
            findings.push(format!("{name} answered {got:?}, not Some({want})"));
        }
        for text in [title, body] {
            if !seen.texts.iter().any(|seen| seen == text) {
                findings.push(format!(
                    "{name}: the sheet did not say {text:?}: {:?}",
                    seen.texts
                ));
            }
        }
        let mut titles: Vec<&str> = seen.buttons.iter().map(String::as_str).collect();
        titles.sort_unstable();
        let mut expected = ["Cancel", proceed];
        expected.sort_unstable();
        if titles != expected {
            findings.push(format!(
                "{name}: its buttons were {titles:?}, not {expected:?}"
            ));
        }
    }

    /// The helper: wait (a minute at most) for the sheet, read it, give `answer`, and wait for the
    /// sheet to go down, ending its modal loop where it stays up a minute. A sheet that never comes
    /// up ends the process as [`HUNG`].
    fn answer_when_up(answer: Answer, done: &AtomicBool, windowed: Windowed) -> Seen {
        let deadline = Instant::now() + HANG;
        loop {
            if done.load(Ordering::Acquire) {
                return Seen::default();
            }
            if let Some(mut seen) =
                aterm_objc::run_on_main(move |_| read_and_answer(answer, windowed))
            {
                let gone = Instant::now() + HANG;
                while !done.load(Ordering::Acquire) {
                    if Instant::now() > gone {
                        seen.stuck = true;
                        aterm_objc::run_on_main(move |_| abort_modal(windowed));
                        break;
                    }
                    std::thread::sleep(STEP);
                }
                return seen;
            }
            if Instant::now() > deadline {
                eprintln!("objc-confirm-drive: HUNG — no sheet came up within a minute");
                std::process::exit(HUNG);
            }
            std::thread::sleep(STEP);
        }
    }

    /// On the main thread, inside the modal loop: read the modal window's texts and buttons and
    /// give `answer`, ending the modal loop where the button to click is missing. `None` while no
    /// modal window is up.
    fn read_and_answer(answer: Answer, windowed: Windowed) -> Option<Seen> {
        // SAFETY: run on the main thread by `run_on_main`. `+sharedApplication` is
        // `+(NSApplication *)` and `-modalWindow` `-(NSWindow *)` (+0, nil with no modal loop);
        // `-contentView` is `-(NSView *)` (+0); `-performClick:` is `-(void)(id)`.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let window = appkit::send_id(app, sel!(modalWindow));
            if window.is_null() {
                return None;
            }
            let press = match answer {
                Answer::Click(title) => title,
                Answer::Key(..) => "",
            };
            let mut seen = Seen::default();
            let mut target = None;
            walk(
                appkit::send_id(window, sel!(contentView)),
                press,
                &mut seen,
                &mut target,
            );
            match (answer, target) {
                (Answer::Click(_), Some(button)) => {
                    appkit::send_v_id(button, sel!(performClick:), Id::NIL);
                    seen.given = true;
                }
                (Answer::Click(_), None) => abort_modal(windowed),
                (Answer::Key(_, chars, key_code), _) => {
                    seen.given = post_key(app, window, chars, key_code);
                    if !seen.given {
                        abort_modal(windowed);
                    }
                }
            }
            Some(seen)
        }
    }

    /// Post a key-down of `chars` (virtual key `key_code`) to `window` at the queue's end, as a
    /// person's key arrives. Whether it was posted.
    ///
    /// # Safety
    /// On the main thread; `app` is the shared `NSApplication` and `window` a live `NSWindow`.
    unsafe fn post_key(app: Id, window: Id, chars: &str, key_code: u16) -> bool {
        let Some(chars) = appkit::nsstring(chars) else {
            return false;
        };
        // SAFETY: the caller's contract. `-windowNumber` is `-(NSInteger)`;
        // `+keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:
        // charactersIgnoringModifiers:isARepeat:keyCode:` is `+(NSEvent *)(NSEventType, NSPoint,
        // NSEventModifierFlags, NSTimeInterval, NSInteger, id, NSString *, NSString *, BOOL,
        // unsigned short)` (+0, nil on bad input); `-postEvent:atStart:` is `-(void)(NSEvent *,
        // BOOL)`.
        unsafe {
            let number = appkit::send_isize(window, sel!(windowNumber));
            let event = appkit::send_id_usize_point_usize_f64_isize_id_id_id_bool_u16(
                class(c"NSEvent").as_id(),
                sel!(keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:),
                NS_EVENT_TYPE_KEY_DOWN,
                CGPoint { x: 0.0, y: 0.0 },
                0,
                0.0,
                number,
                Id::NIL,
                chars.id(),
                chars.id(),
                false,
                key_code,
            );
            if event.is_null() {
                return false;
            }
            appkit::send_v_id_bool(app, sel!(postEvent:atStart:), event, false);
            true
        }
    }

    /// End the modal loop that serves this queue: its sheet answers no.
    fn abort_modal(_windowed: Windowed) {
        // SAFETY: run on the main thread, inside the modal loop (`run_on_main` from the helper).
        // `+sharedApplication` is `+(NSApplication *)` (+0) and `-abortModal` is `-(void)`.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            appkit::send_v(app, sel!(abortModal));
        }
    }

    /// Gather `view`'s tree into `seen`, naming the first button titled `press` in `target`.
    ///
    /// # Safety
    /// On the main thread; `view` is nil or a live `NSView`.
    unsafe fn walk(view: Id, press: &str, seen: &mut Seen, target: &mut Option<Id>) {
        if view.is_null() {
            return;
        }
        // SAFETY: `view` is a live NSView (the caller's contract). `-isKindOfClass:` is
        // `-(BOOL)(Class)`; `-title` and `-stringValue` are `-(NSString *)` (+0) on the NSButton
        // or NSTextField it was found to be; `-subviews` is `-(NSArray *)` (+0), `-count`
        // `-(NSUInteger)` and `-objectAtIndex:` `-(id)(NSUInteger)` within it.
        unsafe {
            if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSButton")) {
                let title = appkit::nsstring_to_rust(appkit::send_id(view, sel!(title)));
                if !press.is_empty() && title == press && target.is_none() {
                    *target = Some(view);
                }
                seen.buttons.push(title);
            } else if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSTextField")) {
                let text = appkit::nsstring_to_rust(appkit::send_id(view, sel!(stringValue)));
                if !text.is_empty() {
                    seen.texts.push(text);
                }
            }
            let subviews = appkit::send_id(view, sel!(subviews));
            let count = if subviews.is_null() {
                0
            } else {
                appkit::send_usize(subviews, sel!(count))
            };
            for i in 0..count {
                let child = appkit::send_id_usize(subviews, sel!(objectAtIndex:), i);
                walk(child, press, seen, target);
            }
        }
    }

    /// Run the main thread's loop once in its default mode, for at most one [`STEP`], so the main
    /// queue serves a helper waiting on it.
    fn run_loop_once() {
        let mode = appkit::nsstring("kCFRunLoopDefaultMode");
        // SAFETY: on the main thread (only `run` calls this). `+currentRunLoop` is
        // `+(NSRunLoop *)` (+0); `+dateWithTimeIntervalSinceNow:` is `+(NSDate *)(NSTimeInterval)`
        // (+0, autoreleased); `-runMode:beforeDate:` is `-(BOOL)(NSString *, NSDate *)`.
        aterm_objc::autoreleasepool(|_| unsafe {
            let Some(mode) = mode else {
                std::thread::sleep(STEP);
                return;
            };
            let run_loop = appkit::send_id(class(c"NSRunLoop").as_id(), sel!(currentRunLoop));
            let until = appkit::send_id_f64(
                class(c"NSDate").as_id(),
                sel!(dateWithTimeIntervalSinceNow:),
                STEP.as_secs_f64(),
            );
            let _ = appkit::send_bool_id_id(run_loop, sel!(runMode:beforeDate:), mode.id(), until);
        });
    }
}
