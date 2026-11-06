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
        if let Err(why) = crate::askpass_sheet::check_password_disclosure(
            appkit::MainThread::new().expect("main thread checked"),
        ) {
            findings.push(format!("password details: {why}"));
        }
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
        if let Err(code) = rename::rounds(windowed, &mut findings) {
            return code;
        }
        if findings.is_empty() {
            println!(
                "objc-confirm-drive: PASS — owner and menu confirmations showed their words and \
                 buttons; proceed and Cancel clicks, Return/keypad Enter defaults and Escape matched each policy; \
                 Rename This Mac's live line followed its fields and the read it was handed, Return did nothing \
                 while it refused, each untouched field followed a read newer than the view while a typed one \
                 stayed, the caret staying in the field being edited, a number a migration waits on emptied its \
                 field and was claimed by typing it, and Rename answered only what was typed"
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

    /// Rename This Mac… (`crate::machine_rename_sheet`) on a real `NSAlert`: the helper types into
    /// its fields through their field editor, as typing does, hands the alert a read through the
    /// main queue, as the name writer does, and reads the line and Rename back.
    mod rename {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        use aterm_agent::machine_name::{InFleet, RenamePreview, ensure, rename_preview};
        use aterm_objc::{Id, class, sel};
        use aterm_secrets::client::{LabLabels, LabelHold};
        use aterm_types::machine_name::Number;

        use super::{HANG, HUNG, STEP, post_key, run_loop_once};
        use crate::appkit;
        use crate::machine_rename_sheet::{self as sheet, Answer, Field, Held, Snapshot};
        use crate::machine_title::MachineView;
        use crate::platform::Windowed;

        /// What the helper saw of the alert after one step.
        #[derive(Debug, Default)]
        struct Look {
            texts: Vec<String>,
            buttons: Vec<String>,
            /// Whether Rename was available.
            renames: Option<bool>,
            /// The editable fields found, in view order.
            fields: usize,
            /// What the editable fields hold, in view order.
            values: Vec<String>,
        }

        /// One step the helper takes once the alert is up.
        #[derive(Clone, Debug)]
        enum Step {
            /// Type into the field at this index, replacing its text.
            Type(usize, String),
            /// Hand the alert the read.
            Deliver,
            /// The Number and Nickname fields hold these.
            Fields(String, String),
            /// The field at this index is being edited, the caret in it.
            Editing(usize),
            /// Read the line and Rename, which must be `renames`, with the line for fields the
            /// person typed these into (`None`: left alone).
            Expect(Option<String>, Option<String>, bool),
            /// Post these keys, in order.
            Keys(&'static [(&'static str, u16)]),
        }

        /// Run each round; a hung helper is `Err` with the drive's exit code.
        pub(super) fn rounds(windowed: Windowed, findings: &mut Vec<String>) -> Result<(), i32> {
            let Ok(root) = aterm_tempfile::tempdir() else {
                findings.push("rename: no scratch state folder".to_owned());
                return Ok(());
            };
            let now = aterm_netplace::unix_ms_now();
            let Some(held) = Number::new(20) else {
                findings.push("rename: m20 is no number".to_owned());
                return Ok(());
            };
            let lab = LabLabels {
                held: std::collections::BTreeMap::from([(held, LabelHold::Label)]),
                ..LabLabels::default()
            };
            let read = ensure(root.path(), || Ok(Some("m7".to_owned())), |_| false, now)
                .and_then(|_| rename_preview(root.path(), || Ok(lab), || Ok(None), now));
            let preview = match read {
                Ok(preview) => preview,
                Err(why) => {
                    findings.push(format!("rename: no read of a scratch name: {why}"));
                    return Ok(());
                }
            };
            // A second Mac whose number a migration still waits on: it holds no claim yet.
            let Ok(waiting_root) = aterm_tempfile::tempdir() else {
                findings.push("rename: no second scratch state folder".to_owned());
                return Ok(());
            };
            let read = ensure(
                waiting_root.path(),
                || Ok(Some("m7".to_owned())),
                |_| false,
                now,
            )
            .and_then(|_| {
                aterm_agent::machine_name::file::replace(waiting_root.path(), |name| {
                    name.migration = Some(aterm_agent::machine_name::file::Migration::Waiting);
                    Ok(())
                })
            })
            .and_then(|_| {
                rename_preview(
                    waiting_root.path(),
                    || Ok(LabLabels::default()),
                    || Ok(None),
                    now,
                )
            });
            let waiting = match read {
                Ok(waiting) if waiting.waits() => waiting,
                Ok(_) => {
                    findings.push("rename: the scratch migration did not wait".to_owned());
                    return Ok(());
                }
                Err(why) => {
                    findings.push(format!("rename: no read of a waiting scratch name: {why}"));
                    return Ok(());
                }
            };
            let word = preview.name().nick.as_str().to_owned();
            let waiting_word = waiting.name().nick.as_str().to_owned();
            // A nickname the read does not hold, for a view a change pressed before outdated.
            let other = if word == "Heron" { "Otter" } else { "Heron" };
            let view = |name: &str, word: &str| MachineView {
                rev: 1,
                name: name.to_owned(),
                nick: word.to_owned(),
                word: word.to_owned(),
                fleet: InFleet::Out,
            };
            let fresh = Held::of_read(&preview);
            let w = || word.clone();
            let s = |text: &str| text.to_owned();
            let typed = |text: &str| Some(text.to_owned());
            let rename = |number: Option<String>, nick: Option<String>| Answer::Rename {
                number,
                nick,
                held: fresh.clone(),
            };
            const RETURN: (&str, u16) = ("\r", 0x24);
            const ESCAPE: (&str, u16) = ("\u{1b}", 0x35);
            // Each round: its name, the read its press brings, the window's view, the words a
            // failed set's press typed (the fields that open as typed), the helper's steps, and
            // the answer.
            type Prefill = (Option<String>, Option<String>);
            type Round<'a> = (
                &'a str,
                &'a RenamePreview,
                MachineView,
                Prefill,
                Vec<Step>,
                Answer,
            );
            let rounds: [Round<'_>; 8] = [
                (
                    "a number the vault holds",
                    &preview,
                    view("m7", &word),
                    (None, None),
                    vec![
                        Step::Expect(None, None, true),
                        Step::Deliver,
                        Step::Type(0, s("m20")),
                        Step::Expect(typed("m20"), None, false),
                        Step::Keys(&[RETURN, ESCAPE]),
                    ],
                    Answer::Cancel,
                ),
                (
                    "a free number typed before the read",
                    &preview,
                    view("m7", &word),
                    (None, None),
                    vec![
                        Step::Type(0, s("m87")),
                        Step::Expect(typed("m87"), None, true),
                        Step::Deliver,
                        Step::Fields(s("m87"), w()),
                        Step::Expect(typed("m87"), None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(typed("m87"), None),
                ),
                (
                    "a nickname",
                    &preview,
                    view("m7", &word),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Type(1, s("Marigold")),
                        Step::Expect(None, typed("Marigold"), true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, typed("Marigold")),
                ),
                (
                    "untouched fields over a stale view",
                    &preview,
                    view("m5", other),
                    (None, None),
                    vec![
                        Step::Fields(s("m5"), s(other)),
                        Step::Editing(0),
                        Step::Deliver,
                        Step::Fields(s("m7"), w()),
                        Step::Editing(0),
                        Step::Expect(None, None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, None),
                ),
                (
                    "a nickname typed before a read that moves the number",
                    &preview,
                    view("m5", &word),
                    (None, None),
                    vec![
                        Step::Type(1, s("Marigold")),
                        Step::Deliver,
                        Step::Fields(s("m7"), s("Marigold")),
                        Step::Editing(1),
                        Step::Expect(None, typed("Marigold"), true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, typed("Marigold")),
                ),
                (
                    "a number typed before a read that changes the nickname",
                    &preview,
                    view("m7", other),
                    (None, None),
                    vec![
                        Step::Type(0, s("m87")),
                        Step::Deliver,
                        Step::Fields(s("m87"), w()),
                        Step::Editing(0),
                        Step::Expect(typed("m87"), None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(typed("m87"), None),
                ),
                (
                    "a failed set's number that the outdated view also shows",
                    &preview,
                    view("m5", other),
                    (typed("m5"), None),
                    vec![
                        Step::Fields(s("m5"), s(other)),
                        Step::Deliver,
                        Step::Fields(s("m5"), w()),
                        Step::Expect(typed("m5"), None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(typed("m5"), None),
                ),
                (
                    "a number a migration waits on, claimed by typing it",
                    &waiting,
                    view("m7", &waiting_word),
                    (None, None),
                    vec![
                        Step::Fields(s("m7"), waiting_word.clone()),
                        Step::Deliver,
                        Step::Fields(String::new(), waiting_word.clone()),
                        Step::Expect(None, None, true),
                        Step::Type(0, s("m7")),
                        Step::Expect(typed("m7"), None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    Answer::Rename {
                        number: typed("m7"),
                        nick: None,
                        held: Held::of_read(&waiting),
                    },
                ),
            ];
            for (name, preview, view, (number, nick), steps, want) in rounds {
                let press = sheet::begin();
                let done = Arc::new(AtomicBool::new(false));
                let helper = {
                    let (done, preview, view) = (Arc::clone(&done), preview.clone(), view.clone());
                    std::thread::spawn(move || {
                        drive(windowed, press, &steps, &preview, &view, &done)
                    })
                };
                let got = sheet::ask(
                    press,
                    sheet::TITLE,
                    "Rename drive",
                    &view,
                    number.as_deref(),
                    nick.as_deref(),
                );
                sheet::end(press);
                done.store(true, Ordering::Release);
                let deadline = Instant::now() + HANG;
                while !helper.is_finished() {
                    if Instant::now() > deadline {
                        eprintln!(
                            "objc-confirm-drive: HUNG — the rename helper did not end within a minute"
                        );
                        return Err(HUNG);
                    }
                    run_loop_once();
                }
                let Ok(seen) = helper.join() else {
                    findings.push(format!("rename: {name}: the helper panicked"));
                    continue;
                };
                println!("objc-confirm-drive: rename: {name} answered {got:?}");
                findings.extend(
                    seen.into_iter()
                        .map(|finding| format!("rename: {name}: {finding}")),
                );
                if got.as_ref() != Some(&want) {
                    findings.push(format!(
                        "rename: {name} answered {got:?}, not Some({want:?})"
                    ));
                }
            }
            Ok(())
        }

        /// The helper: wait for the alert, take `steps`, and return what disagreed. The alert that
        /// never comes up ends the process as [`HUNG`].
        fn drive(
            windowed: Windowed,
            press: u64,
            steps: &[Step],
            preview: &RenamePreview,
            view: &MachineView,
            done: &AtomicBool,
        ) -> Vec<String> {
            let deadline = Instant::now() + HANG;
            while aterm_objc::run_on_main(|_| look(windowed, None).is_none()) {
                if done.load(Ordering::Acquire) {
                    return vec!["the alert ended before the helper saw it".to_owned()];
                }
                if Instant::now() > deadline {
                    eprintln!("objc-confirm-drive: HUNG — no rename alert came up within a minute");
                    std::process::exit(HUNG);
                }
                std::thread::sleep(STEP);
            }
            let mut findings = Vec::new();
            let mut delivered = false;
            for step in steps {
                match step.clone() {
                    Step::Type(field, text) => {
                        if !aterm_objc::run_on_main(move |_| type_into(windowed, field, &text)) {
                            findings.push(format!("field {field} took no typing"));
                        }
                    }
                    Step::Deliver => {
                        let snapshot = Snapshot::Read(Box::new(preview.clone()));
                        aterm_objc::run_on_main(move |_| sheet::deliver(press, snapshot));
                        delivered = true;
                    }
                    Step::Fields(number, nick) => {
                        let seen =
                            aterm_objc::run_on_main(|_| look(windowed, None)).unwrap_or_default();
                        if seen.values != [number.clone(), nick.clone()] {
                            findings.push(format!(
                                "the fields held {:?}, not [{number:?}, {nick:?}]",
                                seen.values
                            ));
                        }
                    }
                    Step::Editing(field) => {
                        if !aterm_objc::run_on_main(move |_| editing(windowed, field)) {
                            findings.push(format!("field {field} was not being edited"));
                        }
                    }
                    Step::Expect(number, nick, renames) => {
                        let (snapshot, held) = if delivered {
                            (
                                Snapshot::Read(Box::new(preview.clone())),
                                Held::of_read(preview),
                            )
                        } else {
                            (Snapshot::Reading, Held::of_view(view))
                        };
                        let want = sheet::line(
                            &snapshot,
                            &held,
                            &Field::open(number.as_deref(), held.number_text()),
                            &Field::open(nick.as_deref(), &held.word),
                        );
                        let (number, nick) = (
                            number.unwrap_or_else(|| held.number_text().to_owned()),
                            nick.unwrap_or_else(|| held.word.clone()),
                        );
                        let seen =
                            aterm_objc::run_on_main(|_| look(windowed, None)).unwrap_or_default();
                        if want.renames != renames {
                            findings.push(format!(
                                "the line for {number} {nick} gives Rename {}, not {renames}",
                                want.renames
                            ));
                        }
                        if !seen.texts.contains(&want.text) {
                            findings.push(format!(
                                "the line did not say {:?}: {:?}",
                                want.text, seen.texts
                            ));
                        }
                        if seen.renames != Some(renames) {
                            findings.push(format!(
                                "Rename was {:?} for {number} {nick}, not {renames}",
                                seen.renames
                            ));
                        }
                        if seen.fields != 2 {
                            findings.push(format!("{} editable fields, not 2", seen.fields));
                        }
                        let mut buttons = seen.buttons.clone();
                        buttons.sort_unstable();
                        if buttons != ["Cancel", "Rename"] {
                            findings.push(format!("its buttons were {:?}", seen.buttons));
                        }
                        if !seen.texts.iter().any(|text| text == sheet::TITLE) {
                            findings.push(format!(
                                "it did not say {:?}: {:?}",
                                sheet::TITLE,
                                seen.texts
                            ));
                        }
                    }
                    Step::Keys(keys) => {
                        let posted = aterm_objc::run_on_main(move |_| {
                            // SAFETY: on the main thread (`run_on_main`), inside the modal loop:
                            // `+sharedApplication` and `-modalWindow` are +0 getters, the window
                            // nil-checked before `post_key`.
                            unsafe {
                                let app = appkit::send_id(
                                    class(c"NSApplication").as_id(),
                                    sel!(sharedApplication),
                                );
                                let window = appkit::send_id(app, sel!(modalWindow));
                                !window.is_null()
                                    && keys
                                        .iter()
                                        .all(|(chars, code)| post_key(app, window, chars, *code))
                            }
                        });
                        if !posted {
                            findings.push(format!("keys {keys:?} could not be posted"));
                        }
                    }
                }
            }
            let gone = Instant::now() + HANG;
            while !done.load(Ordering::Acquire) {
                if Instant::now() > gone {
                    findings.push("the alert stayed up a minute after its last key".to_owned());
                    aterm_objc::run_on_main(|_| {
                        // SAFETY: main thread, inside the modal loop; `-abortModal` is `-(void)`.
                        unsafe {
                            let app = appkit::send_id(
                                class(c"NSApplication").as_id(),
                                sel!(sharedApplication),
                            );
                            appkit::send_v(app, sel!(abortModal));
                        }
                    });
                    break;
                }
                std::thread::sleep(STEP);
            }
            findings
        }

        /// The modal alert's texts, buttons, Rename's availability and editable fields, and the
        /// editable field at index `field`, if asked; `None` while no modal window is up. Main
        /// thread only.
        fn look(_windowed: Windowed, field: Option<(usize, &mut Option<Id>)>) -> Option<Look> {
            // SAFETY: on the main thread (every caller is a `run_on_main` closure). The getters
            // are +0: `-modalWindow` (nil with no modal loop) and `-contentView`.
            unsafe {
                let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
                let window = appkit::send_id(app, sel!(modalWindow));
                if window.is_null() {
                    return None;
                }
                let mut look = Look::default();
                let mut editable = Vec::new();
                walk(
                    appkit::send_id(window, sel!(contentView)),
                    &mut look,
                    &mut editable,
                );
                look.fields = editable.len();
                look.values = editable
                    .iter()
                    .map(|field| {
                        appkit::nsstring_to_rust(appkit::send_id(*field, sel!(stringValue)))
                    })
                    .collect();
                if let Some((index, slot)) = field {
                    *slot = editable.get(index).copied();
                }
                Some(look)
            }
        }

        /// Gather `view`'s tree into `look`, and its editable text fields into `editable`.
        ///
        /// # Safety
        /// On the main thread; `view` is nil or a live `NSView`.
        unsafe fn walk(view: Id, look: &mut Look, editable: &mut Vec<Id>) {
            if view.is_null() {
                return;
            }
            // SAFETY: `view` is a live NSView (the caller's contract). `-isKindOfClass:` is
            // `-(BOOL)(Class)`; `-title`, `-stringValue` are `-(NSString *)` (+0) and `-isEnabled`
            // and `-isEditable` `-(BOOL)` on the NSButton or NSTextField it was found to be;
            // `-subviews` is `-(NSArray *)` (+0), `-count` `-(NSUInteger)` and `-objectAtIndex:`
            // `-(id)(NSUInteger)` within it.
            unsafe {
                if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSButton")) {
                    let title = appkit::nsstring_to_rust(appkit::send_id(view, sel!(title)));
                    if title == sheet::BUTTONS[0] {
                        look.renames = Some(appkit::send_bool(view, sel!(isEnabled)));
                    }
                    look.buttons.push(title);
                } else if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSTextField")) {
                    if appkit::send_bool(view, sel!(isEditable)) {
                        editable.push(view);
                    }
                    let text = appkit::nsstring_to_rust(appkit::send_id(view, sel!(stringValue)));
                    if !text.is_empty() {
                        look.texts.push(text);
                    }
                }
                let subviews = appkit::send_id(view, sel!(subviews));
                let count = if subviews.is_null() {
                    0
                } else {
                    appkit::send_usize(subviews, sel!(count))
                };
                for i in 0..count {
                    walk(
                        appkit::send_id_usize(subviews, sel!(objectAtIndex:), i),
                        look,
                        editable,
                    );
                }
            }
        }

        /// Whether the alert's editable field `field` is being edited with the caret in it: its
        /// field editor is the window's first responder.
        fn editing(windowed: Windowed, field: usize) -> bool {
            let mut target = None;
            if look(windowed, Some((field, &mut target))).is_none() {
                return false;
            }
            let Some(target) = target else {
                return false;
            };
            // SAFETY: main thread (`run_on_main`), inside the modal loop, on the live field the
            // alert's accessory holds. `-window` is `-(NSWindow *)` and `-firstResponder`
            // `-(NSResponder *)`, both +0; `-currentEditor` is `-(NSText *)` (+0, nil while the
            // field is not being edited).
            unsafe {
                let window = appkit::send_id(target, sel!(window));
                if window.is_null() {
                    return false;
                }
                let editor = appkit::send_id(target, sel!(currentEditor));
                !editor.is_null() && appkit::send_id(window, sel!(firstResponder)) == editor
            }
        }

        /// Replace the text of the alert's editable field `field` through its field editor, as
        /// typing does, so its delegate hears the change. Whether the field took it.
        fn type_into(windowed: Windowed, field: usize, text: &str) -> bool {
            let mut target = None;
            if look(windowed, Some((field, &mut target))).is_none() {
                return false;
            }
            let (Some(target), Some(text)) = (target, appkit::nsstring(text)) else {
                return false;
            };
            // SAFETY: main thread (`run_on_main`), inside the modal loop, on the live field the
            // alert's accessory holds. `-window` is `-(NSWindow *)` (+0); `-makeFirstResponder:` is
            // `-(BOOL)(NSResponder *)`; `-currentEditor` is `-(NSText *)` (+0, nil while the field
            // is not being edited); the field editor's `-selectAll:` is `-(void)(id)` and
            // `-insertText:` `-(void)(id)`, which edits as a typed string does.
            unsafe {
                let window = appkit::send_id(target, sel!(window));
                if window.is_null()
                    || !appkit::send_bool_id(window, sel!(makeFirstResponder:), target)
                {
                    return false;
                }
                let editor = appkit::send_id(target, sel!(currentEditor));
                if editor.is_null() {
                    return false;
                }
                appkit::send_v_id(editor, sel!(selectAll:), Id::NIL);
                appkit::send_v_id(editor, sel!(insertText:), text.id());
                true
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
