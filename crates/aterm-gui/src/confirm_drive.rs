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

/// Drive the machine-name panel, or also the owner and menu confirmations when `rename_only`
/// is false, and return the exit code above. With `snapshots`, save the pending and synchronized
/// naming fixtures' actual native content views before their answer.
#[cfg(target_os = "macos")]
#[must_use]
pub fn run_confirm_drive(rename_only: bool, snapshots: Option<&std::path::Path>) -> i32 {
    macos::run(crate::platform::Windowed::launch(), rename_only, snapshots)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;
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

    pub(super) fn run(windowed: Windowed, rename_only: bool, snapshots: Option<&Path>) -> i32 {
        if appkit::MainThread::new().is_none() {
            eprintln!("objc-confirm-drive: NOT RUN — not on the main thread");
            return NOT_RUN;
        }
        if let Some(dir) = snapshots
            && let Err(why) = std::fs::create_dir_all(dir)
        {
            eprintln!(
                "objc-confirm-drive: FINDING — snapshots {}: {why}",
                dir.display()
            );
            return FINDING;
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
        let mut findings = Vec::new();
        if !rename_only {
            let (title, body, proceed) = crate::app_overview::stop_worker_sheet(LABEL);
            let runtime = AppRtMacOS {
                probe_preferences: false,
            };
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
        }
        if let Err(code) = rename::rounds(windowed, &mut findings, snapshots) {
            return code;
        }
        println!("\n=== VERDICT ===");
        if findings.is_empty() {
            println!(
                "objc-confirm-drive: PASS — {}",
                if rename_only {
                    "machine-name panel controls and typed-field delivery"
                } else {
                    "confirmation and machine-name panel controls"
                }
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
        use std::path::{Path, PathBuf};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        use aterm_agent::machine_name::{
            InFleet, NameTo, RenamePreview, ensure, publish, rename_preview, set_name,
        };
        use aterm_net::fleet_member::name_record::ByKind;
        use aterm_objc::{Id, Obj, class, sel};
        use aterm_secrets::client::{LabLabels, LabelHold};
        use aterm_types::machine_name::{Class, Nickname, Number};

        use super::{HANG, HUNG, STEP, post_key, run_loop_once};
        use crate::appkit;
        use crate::machine_rename_sheet::{self as sheet, Answer, Field, Held, Snapshot};
        use crate::machine_title::MachineView;
        use crate::machine_title::naming::NamingStatus;
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
            /// The native status document fits its measured text and remains scrollable if capped.
            status_readable: Option<bool>,
            /// The observed distance scrolled from the status document's top.
            status_offset: Option<f64>,
            authorization: Option<bool>,
            primary_trailing: Option<f64>,
            cancel_trailing: Option<f64>,
        }

        /// One step the helper takes once the alert is up.
        #[derive(Clone, Debug)]
        enum Step {
            /// Type into the field at this index, replacing its text.
            Type(usize, String),
            /// Hand the alert the read.
            Deliver,
            /// Hand the alert local facts before any OS query finishes.
            DeliverLocal,
            /// Finish the separate OS observation, or report an unavailable reader.
            Diagnostics(Result<(), String>),
            /// Replace only the inert permission query while keeping OS values unchanged.
            Permission(Result<bool, String>),
            AuthorizationButton(Option<bool>),
            AuthorizeNaming,
            /// Deliver only optional fleet status, retaining edits and first responder.
            Discover(Result<bool, String>),
            /// The status text contains this phrase.
            Status(&'static str),
            /// Scroll the actual native status document this far down from its top.
            ScrollStatus(f64),
            /// The observed native status scroll has kept this offset.
            StatusOffset(f64),
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

        /// Real publication facts, with macOS values supplied by this display-only fixture.
        #[derive(Clone)]
        struct NamingCase {
            state: PathBuf,
            config: PathBuf,
            values: [Option<String>; 3],
            authorization: Result<bool, String>,
        }

        impl publish::SystemNames for NamingCase {
            fn read(&self) -> [Option<String>; 3] {
                self.values.clone()
            }
            fn authorization(&self, _: &Path, _: &Path) -> Result<bool, String> {
                self.authorization.clone() // Inert: the driver never launches authorization.
            }
            fn apply(
                &self,
                _: &std::path::Path,
                _: &std::path::Path,
                _: &aterm_types::machine_name::GeneratedNames,
            ) -> Result<(), String> {
                Err("the display fixture never renames macOS".to_owned())
            }
        }

        impl NamingCase {
            fn local_snapshot(&self, preview: &RenamePreview) -> Snapshot {
                Snapshot::named(
                    preview.clone(),
                    NamingStatus::read_local(&self.state, &self.config, preview.name())
                        .with_fleet_membership(InFleet::Out),
                )
            }

            fn snapshot(&self, preview: &RenamePreview) -> Snapshot {
                Snapshot::named(
                    preview.clone(),
                    NamingStatus::read(&self.state, &self.config, preview.name(), self)
                        .with_fleet_membership(InFleet::Out),
                )
            }
        }

        fn naming_cases(
            now: u64,
        ) -> Result<
            (
                aterm_tempfile::TempDir,
                RenamePreview,
                NamingCase,
                NamingCase,
            ),
            String,
        > {
            use std::os::unix::fs::PermissionsExt as _;
            let root = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
            let state = root.path().join("state");
            std::fs::create_dir(&state).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            let config = root.path().join("aterm.toml");
            let number = Number::new(85).ok_or("m85 is no number")?;
            set_name(
                &state,
                &NameTo {
                    num: number,
                    nick: Some(Nickname::parse("dog").map_err(|why| why.to_string())?),
                },
                || Ok(LabLabels::default()),
                ByKind::Person,
                || Ok(None),
                now,
                false,
            )
            .map_err(|e| e.to_string())?;
            let pending = NamingCase {
                state: state.clone(),
                config: config.clone(),
                values: std::array::from_fn(|_| Some("m6".to_owned())),
                authorization: Ok(false),
            };
            let policy = publish::Policy::read(&config, number)?;
            if !matches!(
                publish::publish(&state, &config, &policy, Some(&pending)),
                Err(publish::Pending::SystemNames(_))
            ) {
                return Err(
                    "the inert macOS refusal did not leave OS publication pending".to_owned(),
                );
            }
            let preview = rename_preview(&state, || Ok(LabLabels::default()), || Ok(None), now)
                .map_err(|e| e.to_string())?;
            let generated =
                aterm_types::machine_name::GeneratedNames::new(number, &preview.name().nick);
            let synchronized = NamingCase {
                values: [
                    Some(generated.computer_name().to_owned()),
                    Some(generated.host_name().to_owned()),
                    Some(generated.host_name().to_owned()),
                ],
                ..pending.clone()
            };
            Ok((root, preview, pending, synchronized))
        }

        /// Run each round; a hung helper is `Err` with the drive's exit code.
        pub(super) fn rounds(
            windowed: Windowed,
            findings: &mut Vec<String>,
            snapshots: Option<&Path>,
        ) -> Result<(), i32> {
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
            let (_naming_root, named_preview, pending, synchronized) = match naming_cases(now) {
                Ok(cases) => cases,
                Err(why) => {
                    findings.push(format!("rename: no inert publication fixture: {why}"));
                    return Ok(());
                }
            };
            assert_eq!(
                preview.name().class,
                Class::Seeded,
                "the native unchanged-Save control must start with a real seeded name"
            );
            let word = preview.name().display_nick();
            let waiting_word = waiting.name().display_nick();
            // A nickname the read does not hold, for a view a change pressed before outdated.
            let other = if word == "Heron" { "Otter" } else { "Heron" };
            let view = |name: &str, word: &str| MachineView {
                rev: 1,
                name: name.to_owned(),
                nick: word.to_owned(),
                word: word.to_owned(),
                fleet: InFleet::Out,
            };
            let w = || word.clone();
            let s = |text: &str| text.to_owned();
            let typed = |text: &str| Some(text.to_owned());
            let rename = |number: Option<String>, nick: Option<String>, requested| Answer::Rename {
                number,
                nick,
                requested: Ok(requested),
            };
            let named = |number| {
                sheet::RenameTo::Name(NameTo {
                    num: Number::new(number).expect("a fixture machine number"),
                    nick: None,
                })
            };
            let nicknamed = |word: &str| {
                sheet::RenameTo::Nick(Nickname::parse(word).expect("a fixture nickname"))
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
                Option<&'a NamingCase>,
                &'static str,
                MachineView,
                Prefill,
                Vec<Step>,
                Answer,
            );
            let unknown = NamingCase {
                authorization: Err("the existing naming permission could not be read".into()),
                ..pending.clone()
            };
            let rounds: [Round<'_>; 14] = [
                (
                    "a number the vault holds",
                    &preview,
                    None,
                    "Save",
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
                    None,
                    "Save",
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
                    rename(typed("m87"), None, named(87)),
                ),
                (
                    "a nickname",
                    &preview,
                    None,
                    "Save",
                    view("m7", &word),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Type(1, s("Marigold")),
                        Step::Expect(None, typed("Marigold"), true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, typed("Marigold"), nicknamed("Marigold")),
                ),
                (
                    "untouched fields over a stale view",
                    &preview,
                    None,
                    "Save",
                    view("m5", other),
                    (None, None),
                    vec![
                        Step::Fields(s("5"), s(other)),
                        Step::Editing(0),
                        Step::Deliver,
                        Step::Fields(s("7"), w()),
                        Step::Editing(0),
                        Step::Expect(None, None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, None, named(7)),
                ),
                (
                    "a nickname typed before a read that moves the number",
                    &preview,
                    None,
                    "Save",
                    view("m5", &word),
                    (None, None),
                    vec![
                        Step::Type(1, s("Marigold")),
                        Step::Deliver,
                        Step::Fields(s("7"), s("Marigold")),
                        Step::Editing(1),
                        Step::Expect(None, typed("Marigold"), true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(None, typed("Marigold"), nicknamed("Marigold")),
                ),
                (
                    "a number typed before a read that changes the nickname",
                    &preview,
                    None,
                    "Save",
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
                    rename(typed("m87"), None, named(87)),
                ),
                (
                    "a failed set's number that the outdated view also shows",
                    &preview,
                    None,
                    "Save",
                    view("m5", other),
                    (typed("m5"), None),
                    vec![
                        Step::Fields(s("m5"), s(other)),
                        Step::Deliver,
                        Step::Fields(s("m5"), w()),
                        Step::Expect(typed("m5"), None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(typed("m5"), None, named(5)),
                ),
                (
                    "a number a migration waits on, claimed by typing it",
                    &waiting,
                    None,
                    "Save",
                    view("m7", &waiting_word),
                    (None, None),
                    vec![
                        Step::Fields(s("7"), waiting_word.clone()),
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
                        requested: Ok(named(7)),
                    },
                ),
                (
                    "saved number with pending macOS publication",
                    &named_preview,
                    Some(&pending),
                    "Authorize & Sync",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Expect(None, None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    Answer::Rename {
                        number: None,
                        nick: None,
                        requested: Ok(sheet::RenameTo::Authorize),
                    },
                ),
                (
                    "synchronized aterm fleet and macOS names",
                    &named_preview,
                    Some(&synchronized),
                    "Save",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Expect(None, None, true),
                        Step::Type(0, s("m")),
                        Step::Expect(typed("m"), None, false),
                        Step::Status("actual m85 Dog"),
                        Step::Status("expected m85-dog.local"),
                        Step::Editing(0),
                        Step::Type(0, s("85")),
                        Step::Type(1, s("@")),
                        Step::Expect(None, typed("@"), false),
                        Step::Status("actual m85 Dog"),
                        Step::Status("expected m85-dog.local"),
                        Step::Editing(1),
                        Step::Type(1, s("研究室研究室研究室研究室研究室研究室研究室")),
                        Step::Expect(
                            None,
                            typed("研究室研究室研究室研究室研究室研究室研究室"),
                            true,
                        ),
                        Step::Type(1, s("dog")),
                        Step::Expect(None, None, true),
                        Step::Keys(&[RETURN]),
                    ],
                    Answer::Rename {
                        number: None,
                        nick: None,
                        requested: Ok(sheet::RenameTo::Keep),
                    },
                ),
                (
                    "fleet discovery preserves typed fields and focus",
                    &named_preview,
                    Some(&synchronized),
                    "Save",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Status("discovering"),
                        Step::Type(0, s("86")),
                        Step::Status("ID: expected m86 · saved m85"),
                        Step::Type(1, s("Blue Heron")),
                        Step::Editing(1),
                        Step::Discover(Ok(true)),
                        Step::Status("Available fleets: ALab"),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Discover(Err("offline".into())),
                        Step::Status("discovery unavailable"),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Type(1, s("研究室研究室研究室研究室研究室研究室研究室")),
                        Step::Type(0, "not a number ".repeat(128)),
                        Step::ScrollStatus(32.0),
                        Step::Discover(Ok(true)),
                        Step::StatusOffset(32.0),
                        Step::Diagnostics(Ok(())),
                        Step::StatusOffset(32.0),
                        Step::Keys(&[ESCAPE]),
                    ],
                    Answer::Cancel,
                ),
                (
                    "OS diagnostics preserve fleet status edits and focus",
                    &named_preview,
                    Some(&synchronized),
                    "Save",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::DeliverLocal,
                        Step::Status("reading current name"),
                        Step::Type(0, s("86")),
                        Step::Type(1, s("Blue Heron")),
                        Step::Discover(Ok(true)),
                        Step::Diagnostics(Ok(())),
                        Step::Status("actual m85 Dog"),
                        Step::Status("Available fleets: ALab"),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Diagnostics(Err("fixture reader unavailable".into())),
                        Step::Status("macOS names could not be read"),
                        Step::Status("Available fleets: ALab"),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Keys(&[ESCAPE]),
                    ],
                    Answer::Cancel,
                ),
                (
                    "unread permission tries existing authorization on Return",
                    &named_preview,
                    Some(&unknown),
                    "Retry Sync",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::Expect(None, None, true),
                        Step::AuthorizationButton(Some(true)),
                        Step::Keys(&[RETURN]),
                    ],
                    rename(
                        None,
                        None,
                        sheet::RenameTo::Synchronize(Number::new(85).unwrap()),
                    ),
                ),
                (
                    "separate authorization carries no draft and follows late permission",
                    &named_preview,
                    Some(&unknown),
                    "Retry Sync",
                    view("m85", named_preview.name().nick.as_str()),
                    (None, None),
                    vec![
                        Step::Deliver,
                        Step::AuthorizationButton(Some(true)),
                        Step::Type(0, s("86")),
                        Step::Type(1, s("Blue Heron")),
                        Step::AuthorizationButton(Some(false)),
                        Step::Editing(1),
                        Step::Permission(Ok(true)),
                        Step::AuthorizationButton(None),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Permission(Err("unread permission".into())),
                        Step::AuthorizationButton(Some(false)),
                        Step::Fields(s("86"), s("Blue Heron")),
                        Step::Editing(1),
                        Step::Type(0, s("85")),
                        Step::Type(1, named_preview.name().display_nick()),
                        Step::AuthorizationButton(Some(true)),
                        Step::AuthorizeNaming,
                    ],
                    rename(None, None, sheet::RenameTo::Authorize),
                ),
            ];
            for (index, (name, preview, naming, primary, view, (number, nick), steps, want)) in
                rounds.into_iter().enumerate()
            {
                let press = sheet::begin();
                let done = Arc::new(AtomicBool::new(false));
                let snapshot = snapshots
                    .filter(|_| naming.is_some())
                    .map(|dir| dir.join(format!("rename-{index:02}.png")));
                let helper = {
                    let (done, preview, naming, view) = (
                        Arc::clone(&done),
                        preview.clone(),
                        naming.cloned(),
                        view.clone(),
                    );
                    std::thread::spawn(move || {
                        drive(
                            windowed,
                            press,
                            &steps,
                            &preview,
                            (naming.as_ref(), primary, snapshot.as_deref()),
                            &view,
                            &done,
                        )
                    })
                };
                let got = sheet::ask(
                    press,
                    sheet::TITLE,
                    &crate::machine_title::rename_alert_body(None),
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
            (naming, primary, snapshot_path): (Option<&NamingCase>, &'static str, Option<&Path>),
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
            let mut action_anchors: Option<(f64, f64)> = None;
            for step in steps {
                match step.clone() {
                    Step::Type(field, text) => {
                        if !aterm_objc::run_on_main(move |_| type_into(windowed, field, &text)) {
                            findings.push(format!("field {field} took no typing"));
                        }
                    }
                    Step::Deliver | Step::DeliverLocal => {
                        let local = matches!(step, Step::DeliverLocal);
                        let snapshot = naming.map_or_else(
                            || Snapshot::read(preview.clone()),
                            |naming| {
                                if local {
                                    naming.local_snapshot(preview)
                                } else {
                                    naming.snapshot(preview)
                                }
                            },
                        );
                        let photograph = snapshot_path.is_some();
                        let captured = aterm_objc::run_on_main(move |_| {
                            sheet::deliver(press, snapshot);
                            photograph.then(|| capture(windowed))
                        });
                        if let Some(captured) = captured
                            && let Some(path) = snapshot_path
                        {
                            match captured.and_then(|(width, height, png)| {
                                let (rgba, decoded_width, decoded_height) =
                                    aterm_png::decode_rgba8(&png, 4096)
                                        .ok_or("native snapshot is not a readable PNG")?;
                                if (decoded_width, decoded_height) != (width, height)
                                    || rgba.is_empty()
                                {
                                    return Err("native snapshot dimensions disagree".into());
                                }
                                let pixels = rgba.as_chunks::<4>().0;
                                if pixels.iter().all(|pixel| *pixel == pixels[0]) {
                                    return Err("native snapshot is blank".into());
                                }
                                std::fs::write(path, png).map_err(|why| why.to_string())?;
                                Ok((width, height))
                            }) {
                                Ok((width, height)) => println!(
                                    "objc-confirm-drive: snapshot {} {width}x{height}",
                                    path.display()
                                ),
                                Err(why) => {
                                    findings.push(format!("snapshot {}: {why}", path.display()))
                                }
                            }
                        }
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
                    Step::Discover(available) => {
                        aterm_objc::run_on_main(move |_| {
                            sheet::deliver_discovery(press, available)
                        });
                    }
                    Step::Diagnostics(result) => {
                        let status = result.and_then(|()| {
                            let naming = naming.ok_or("missing OS observation fixture")?;
                            Ok(NamingStatus::read(
                                &naming.state,
                                &naming.config,
                                preview.name(),
                                naming,
                            ))
                        });
                        aterm_objc::run_on_main(move |_| sheet::deliver_diagnostics(press, status));
                    }
                    Step::Permission(permission) => {
                        let status = naming
                            .ok_or("missing OS observation fixture".to_owned())
                            .map(|naming| {
                                let mut naming = naming.clone();
                                naming.authorization = permission;
                                NamingStatus::read(
                                    &naming.state,
                                    &naming.config,
                                    preview.name(),
                                    &naming,
                                )
                            });
                        aterm_objc::run_on_main(move |_| sheet::deliver_diagnostics(press, status));
                    }
                    Step::AuthorizationButton(want) => {
                        let seen =
                            aterm_objc::run_on_main(|_| look(windowed, None)).unwrap_or_default();
                        let actual = seen.authorization;
                        if actual != want {
                            findings.push(format!(
                                "naming authorization button was {actual:?}, not {want:?}"
                            ));
                        }
                        match (seen.primary_trailing, seen.cancel_trailing) {
                            (Some(primary), Some(cancel)) => {
                                if let Some((before_primary, before_cancel)) = action_anchors
                                    && ((primary - before_primary).abs() > 0.5
                                        || (cancel - before_cancel).abs() > 0.5)
                                {
                                    findings.push("the primary and Cancel moved away from their right anchors".into());
                                }
                                action_anchors = Some((primary, cancel));
                            }
                            _ => {
                                findings.push("the primary and Cancel could not be measured".into())
                            }
                        }
                    }
                    Step::AuthorizeNaming => {
                        let clicked = aterm_objc::run_on_main(move |_| {
                            let mut authorize = None;
                            look_with_status(windowed, None, &mut None, &mut authorize)?;
                            let authorize = authorize?;
                            // SAFETY: borrowed live NSButton, found and used in this main-thread call.
                            unsafe {
                                if appkit::send_bool(authorize, sel!(isHidden))
                                    || !appkit::send_bool(authorize, sel!(isEnabled))
                                {
                                    return None;
                                }
                                appkit::send_v_id(authorize, sel!(performClick:), Id::NIL);
                            }
                            Some(())
                        });
                        if clicked.is_none() {
                            findings.push(
                                "the separate naming authorization button could not be pressed"
                                    .into(),
                            );
                        }
                    }
                    Step::Status(phrase) => {
                        let seen =
                            aterm_objc::run_on_main(|_| look(windowed, None)).unwrap_or_default();
                        if !seen.texts.iter().any(|text| text.contains(phrase)) {
                            findings.push(format!(
                                "status did not contain {phrase:?}: {:?}",
                                seen.texts
                            ));
                        }
                    }
                    Step::ScrollStatus(offset) => {
                        let scrolled = aterm_objc::run_on_main(move |_| {
                            let mut status = None;
                            look_with_status(windowed, None, &mut status, &mut None)
                                .ok_or("no modal alert")?;
                            let status = status.ok_or("no naming status document")?;
                            // SAFETY: the main-thread walk found this live status document in
                            // the still-open modal alert. Its enclosing scroll owns its clip.
                            unsafe {
                                let scroll = appkit::send_id(status, sel!(enclosingScrollView));
                                let clip = appkit::send_id(scroll, sel!(contentView));
                                if clip.is_null() {
                                    return Err("no naming status clip".to_owned());
                                }
                                let document = appkit::send_rect(status, sel!(bounds)).size.height;
                                let visible = appkit::send_rect(clip, sel!(bounds)).size.height;
                                let extent = document - visible;
                                if extent < offset {
                                    let text = appkit::nsstring_to_rust(appkit::send_id(
                                        status,
                                        sel!(stringValue),
                                    ));
                                    return Err(format!(
                                        "document {document}pt, viewport {visible}pt, {} status bytes",
                                        text.len()
                                    ));
                                }
                                let y = if appkit::send_bool(status, sel!(isFlipped)) {
                                    offset
                                } else {
                                    extent - offset
                                };
                                appkit::send_v_point(
                                    clip,
                                    sel!(scrollToPoint:),
                                    aterm_objc::CGPoint { x: 0.0, y },
                                );
                                appkit::send_v_id(scroll, sel!(reflectScrolledClipView:), clip);
                                Ok(())
                            }
                        });
                        if let Err(why) = scrolled {
                            findings.push(format!(
                                "status could not scroll down {offset} points: {why}"
                            ));
                        }
                    }
                    Step::StatusOffset(want) => {
                        let actual = aterm_objc::run_on_main(|_| look(windowed, None))
                            .and_then(|seen| seen.status_offset);
                        if !actual.is_some_and(|actual| (actual - want).abs() < 0.5) {
                            findings.push(format!(
                                "status scroll was {actual:?}, not {want} points from its top"
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
                                naming.map_or_else(
                                    || Snapshot::read(preview.clone()),
                                    |naming| naming.snapshot(preview),
                                ),
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
                        if want.text.starts_with("ID: ") && seen.status_readable != Some(true) {
                            findings.push(
                                "the native naming status is clipped or cannot be scrolled".into(),
                            );
                        }
                        let mut buttons = seen.buttons.clone();
                        buttons.sort_unstable();
                        let mut expected = vec!["Cancel", if delivered { primary } else { "Save" }];
                        if snapshot.authorization_unknown(&held) {
                            expected.push(sheet::AUTHORIZE_NAMING);
                        }
                        expected.sort_unstable();
                        if buttons != expected {
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

        /// Copy only this driver's own rename alert's native content on the main queue.
        fn capture(windowed: Windowed) -> Result<(usize, usize, Vec<u8>), String> {
            let seen = look(windowed, None).ok_or("no rename alert to capture")?;
            if seen.fields != 2 || !seen.texts.iter().any(|text| text == sheet::TITLE) {
                return Err("the modal window is not the rename panel".into());
            }
            aterm_objc::autoreleasepool(|_| {
                // SAFETY: called only by the main-queue callback. The modal window and content
                // view are this process's live NSAlert, retained here through the copy. The
                // NSView caching pair returns a +0 NSBitmapImageRep, retained here, and PNG
                // representation returns +0 NSData copied before this autorelease pool ends.
                unsafe {
                    let app =
                        appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
                    let window = Obj::retain(appkit::send_id(app, sel!(modalWindow)))
                        .ok_or("the rename alert ended before capture")?;
                    let view = Obj::retain(appkit::send_id(window.id(), sel!(contentView)))
                        .ok_or("the rename alert has no content view")?;
                    let bounds = appkit::send_rect(view.id(), sel!(bounds));
                    if !bounds.origin.x.is_finite()
                        || !bounds.origin.y.is_finite()
                        || !bounds.size.width.is_finite()
                        || !bounds.size.height.is_finite()
                        || bounds.size.width <= 0.0
                        || bounds.size.height <= 0.0
                    {
                        return Err("the rename alert has invalid content bounds".into());
                    }
                    let bitmap = Obj::retain(appkit::send_id_rect(
                        view.id(),
                        sel!(bitmapImageRepForCachingDisplayInRect:),
                        bounds,
                    ))
                    .ok_or("the rename alert could not allocate a bitmap")?;
                    appkit::send_v_rect_id(
                        view.id(),
                        sel!(cacheDisplayInRect:toBitmapImageRep:),
                        bounds,
                        bitmap.id(),
                    );
                    let width = usize::try_from(appkit::send_isize(bitmap.id(), sel!(pixelsWide)))
                        .map_err(|_| "the rename bitmap has an invalid width")?;
                    let height = usize::try_from(appkit::send_isize(bitmap.id(), sel!(pixelsHigh)))
                        .map_err(|_| "the rename bitmap has an invalid height")?;
                    if width == 0 || height == 0 || width > 4096 || height > 4096 {
                        return Err("the rename bitmap exceeds its dimension bounds".into());
                    }
                    let properties =
                        Obj::from_owned(appkit::send_id(class(c"NSDictionary").as_id(), sel!(new)))
                            .ok_or("the rename bitmap could not allocate PNG properties")?;
                    let data = appkit::send_id_usize_id(
                        bitmap.id(),
                        sel!(representationUsingType:properties:),
                        appkit::consts::NS_BITMAP_IMAGE_FILE_TYPE_PNG,
                        properties.id(),
                    );
                    if data.is_null() {
                        return Err("the rename bitmap could not encode a PNG".into());
                    }
                    let length = appkit::send_usize(data, sel!(length));
                    if length == 0 || length > 64 * 1024 * 1024 {
                        return Err("the rename PNG exceeds its byte bounds".into());
                    }
                    let mut png = vec![0; length];
                    appkit::send_v_ptr_usize(
                        data,
                        sel!(getBytes:length:),
                        png.as_mut_ptr().cast(),
                        length,
                    );
                    Ok((width, height, png))
                }
            })
        }

        /// The modal alert's texts, buttons, Rename's availability and editable fields, and the
        /// editable field at index `field`, if asked; `None` while no modal window is up. Main
        /// thread only.
        fn look(windowed: Windowed, field: Option<(usize, &mut Option<Id>)>) -> Option<Look> {
            look_with_status(windowed, field, &mut None, &mut None)
        }

        /// Inspect the alert and borrow its status view only within this main-thread call.
        fn look_with_status(
            _windowed: Windowed,
            field: Option<(usize, &mut Option<Id>)>,
            status: &mut Option<Id>,
            authorize: &mut Option<Id>,
        ) -> Option<Look> {
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
                    status,
                    authorize,
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
        unsafe fn walk(
            view: Id,
            look: &mut Look,
            editable: &mut Vec<Id>,
            status: &mut Option<Id>,
            authorize: &mut Option<Id>,
        ) {
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
                    if title == sheet::AUTHORIZE_NAMING {
                        *authorize = Some(view);
                    }
                    if appkit::send_bool(view, sel!(isHidden)) {
                        return;
                    }
                    if title == sheet::AUTHORIZE_NAMING {
                        look.authorization = Some(appkit::send_bool(view, sel!(isEnabled)));
                    }
                    if title == sheet::BUTTONS[0]
                        || title == "Retry Sync"
                        || title == "Authorize & Sync"
                    {
                        look.renames = Some(appkit::send_bool(view, sel!(isEnabled)));
                    }
                    let parent = appkit::send_id(view, sel!(superview));
                    if !parent.is_null() {
                        let frame = appkit::send_rect(view, sel!(frame));
                        let bounds = appkit::send_rect(parent, sel!(bounds));
                        let trailing =
                            bounds.origin.x + bounds.size.width - frame.origin.x - frame.size.width;
                        if title == sheet::BUTTONS[1] {
                            look.cancel_trailing = Some(trailing);
                        } else if title == sheet::BUTTONS[0]
                            || title == "Retry Sync"
                            || title == "Authorize & Sync"
                        {
                            look.primary_trailing = Some(trailing);
                        }
                    }
                    look.buttons.push(title);
                } else if appkit::send_bool_cls(view, sel!(isKindOfClass:), class(c"NSTextField")) {
                    if appkit::send_bool(view, sel!(isEditable)) {
                        editable.push(view);
                    }
                    let text = appkit::nsstring_to_rust(appkit::send_id(view, sel!(stringValue)));
                    if text.starts_with("ID: ") {
                        *status = Some(view);
                        let frame = appkit::send_rect(view, sel!(frame));
                        let cell = appkit::send_id(view, sel!(cell));
                        let scroll = appkit::send_id(view, sel!(enclosingScrollView));
                        look.status_readable = Some(
                            !cell.is_null() && !scroll.is_null() && {
                                let need = appkit::send_size_rect(
                                    cell,
                                    sel!(cellSizeForBounds:),
                                    aterm_objc::CGRect {
                                        origin: Default::default(),
                                        size: aterm_objc::CGSize {
                                            width: frame.size.width,
                                            height: f64::from(u16::MAX),
                                        },
                                    },
                                );
                                let clip = appkit::send_id(scroll, sel!(contentView));
                                if !clip.is_null() {
                                    let visible = appkit::send_rect(clip, sel!(bounds));
                                    look.status_offset =
                                        Some(if appkit::send_bool(view, sel!(isFlipped)) {
                                            visible.origin.y
                                        } else {
                                            frame.size.height
                                                - visible.origin.y
                                                - visible.size.height
                                        });
                                }
                                !clip.is_null()
                                    && need.height <= frame.size.height + 0.5
                                    && (frame.size.height
                                        <= appkit::send_rect(clip, sel!(bounds)).size.height + 0.5
                                        || appkit::send_bool(scroll, sel!(hasVerticalScroller)))
                            },
                        );
                    }
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
                        status,
                        authorize,
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
