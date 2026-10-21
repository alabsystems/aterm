// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launch-fatal alert ([`crate::fatal_launch_error`]): what a launch that
//! cannot start shows before it exits, and the order it asks AppKit for it in.
//!
//! Every fatal a macOS window launch meets on its main thread comes before the
//! event loop builds `NSApplication`: session 0's spawn (LAG-11 starts the shell
//! ahead of the loop), the containment refusal and the backend worker's early
//! death. The alert there was a bare `NSAlert` `-runModal`. AppKit made the
//! application object behind it and never activated it, so the alert came up
//! behind the window the person launched from, with the app not frontmost.
//! Measured 2026-10-20 on 0.107.0 (WIN-2: its bundle executed directly, a
//! `shell` whose `#!` interpreter is missing, stderr not a tty): a 260x234
//! layer-0 window under another app's window, never frontmost in 6 s. An
//! unbundled binary was worse: its policy read Prohibited, which AppKit never
//! activates.
//!
//! So [`raise`] makes the application object first, under the policy the event
//! loop would give it; puts the alert's panel on screen; asks to be the active
//! app when the launch's posture would at `applicationDidFinishLaunching:`
//! (`launch_posture`); and only then runs the alert. The panel comes before the
//! ask because WindowServer refuses to activate an app that presents no window
//! (its own words in `launch_posture`'s record: "presents 0 windows ... Denying
//! the request"). Measured the same day in a bare AppKit program: the ask with
//! no window up was refused every way it was made (before or after
//! `-finishLaunching`, from `applicationDidFinishLaunching:`, `-activate` as
//! well as `-activateIgnoringOtherApps:`); made once the panel was in, the app
//! was frontmost within 137-244 ms and the alert a layer-8 panel in front.
//!
//! An update's handoff candidate never asks to activate at launch, and its alert
//! does not either. A headless launch never gets here (`launch_alert_wanted`,
//! `menu::os_ui_refused`), so it never reaches WindowServer. The backend
//! worker's own fatal (no system monospace font) runs off the main thread and
//! keeps `fatal_launch_error`'s `osascript` leg, a process of its own that comes
//! to the front by itself (measured the same day: frontmost, a layer-8 panel).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::{AppActivation, LaunchPosture};

/// The steps [`raise`] takes, injected so a unit test watches their order with
/// no WindowServer (AGENTS.md: a platform probe goes through an injected field
/// with an inert test arm).
pub(crate) trait AlertHost {
    /// Make the application object, under `policy` (`None`: what winit gives
    /// it, Regular when unbundled and the bundle's `LSUIElement` otherwise).
    /// `false` when it is not made here: off the main thread, without the
    /// windowed launch's proof, or off macOS.
    fn make_app(&mut self, policy: Option<AppActivation>) -> bool;
    /// Build the alert and put its panel on screen, not yet run. `false` when
    /// nothing is on screen (off macOS, where [`AlertHost::run`] shows it).
    fn present(&mut self, title: &str, body: &str) -> bool;
    /// Ask to be the active app.
    fn activate(&mut self);
    /// Run the alert until it is dismissed.
    fn run(&mut self);
}

/// Show the launch-fatal alert through `host` under the launch's `posture`: the
/// application object, the alert's panel, the activation the posture asks for,
/// then the alert's run.
pub(crate) fn raise(host: &mut impl AlertHost, posture: LaunchPosture, title: &str, body: &str) {
    let made = host.make_app(posture.policy);
    if host.present(title, body) && made && posture.activate_on_launch {
        host.activate();
    }
    host.run();
}

/// Whether this launch is an update's handoff candidate: it names a rendezvous to
/// dial or the overlap pipes. Marked once by `main_entry`, before any launch
/// fatal can run (some run before the handoff is claimed or refused, so this
/// reads the launch's names, not the claim).
static HANDOFF_CANDIDATE: AtomicBool = AtomicBool::new(false);

/// Record that this launch is an update's handoff candidate.
pub(crate) fn mark_handoff_candidate() {
    HANDOFF_CANDIDATE.store(true, Ordering::Relaxed);
}

/// Show the launch-fatal alert of a launch that is `headless` or not, under its
/// own posture, through the live platform.
pub(crate) fn show(headless: bool, title: &str, body: &str) {
    let posture = crate::launch_posture(headless, HANDOFF_CANDIDATE.load(Ordering::Relaxed));
    #[cfg(target_os = "macos")]
    aterm_objc::autoreleasepool(|_| raise(&mut Live::default(), posture, title, body));
    #[cfg(not(target_os = "macos"))]
    raise(&mut Live::default(), posture, title, body);
}

/// The live [`AlertHost`]: AppKit on macOS; elsewhere `menu::notify` alone.
#[derive(Default)]
struct Live {
    /// The application object [`AlertHost::make_app`] made.
    #[cfg(target_os = "macos")]
    app: Option<aterm_objc::Id>,
    /// The alert [`AlertHost::present`] put on screen.
    #[cfg(target_os = "macos")]
    alert: Option<aterm_objc::Obj>,
    /// What [`AlertHost::run`] shows off macOS.
    #[cfg(not(target_os = "macos"))]
    words: Option<(String, String)>,
}

#[cfg(target_os = "macos")]
impl AlertHost for Live {
    fn make_app(&mut self, policy: Option<AppActivation>) -> bool {
        use crate::appkit::{self, MainThread};
        use aterm_objc::{class, sel};
        // The send below creates the application object, and with it the
        // process's WindowServer connection: only a windowed launch makes it.
        let Some(_windowed) = crate::platform::Windowed::get() else {
            return false;
        };
        if MainThread::new().is_none() {
            return false;
        }
        // `NSRunningApplication.h`: `NSApplicationActivationPolicyRegular` = 0,
        // `…Accessory` = 1, `…Prohibited` = 2 (`NSInteger`).
        let code = |p: AppActivation| match p {
            AppActivation::Regular => 0,
            AppActivation::Accessory => 1,
            AppActivation::Prohibited => 2,
        };
        // SAFETY: main thread (checked above). `+sharedApplication` is
        // `@16#0:8` on `NSApplication`, +0 and process-lifetime;
        // `+currentApplication` is `@16#0:8` on `NSRunningApplication` and
        // `-bundleIdentifier` is `@16@0:8`, nil for an unbundled binary;
        // `-setActivationPolicy:` is `B24@0:8q16`, whose answer winit ignores
        // too.
        unsafe {
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            if app.is_null() {
                return false;
            }
            // winit's own rule at `applicationDidFinishLaunching:`
            // (`aterm-window` `app_state.rs`): an explicit policy, else Regular
            // for an unbundled binary, else the bundle's manifest.
            let policy = policy.map(code).or_else(|| {
                let running = appkit::send_id(
                    class(c"NSRunningApplication").as_id(),
                    sel!(currentApplication),
                );
                (running.is_null() || appkit::send_id(running, sel!(bundleIdentifier)).is_null())
                    .then_some(code(AppActivation::Regular))
            });
            if let Some(policy) = policy {
                let _ = appkit::send_bool_isize(app, sel!(setActivationPolicy:), policy);
            }
            self.app = Some(app);
        }
        true
    }

    fn present(&mut self, title: &str, body: &str) -> bool {
        use crate::appkit::{self, MainThread};
        use aterm_objc::{Id, sel};
        if crate::menu::os_ui_refused(title) || MainThread::new().is_none() {
            return false;
        }
        let Some(alert) = crate::menu::ok_alert(title, body) else {
            return false;
        };
        // SAFETY: main thread (checked above), the live alert `ok_alert` built.
        // `-layout` is `v16@0:8`; `-window` is `@16@0:8`, the alert's own +0
        // panel; `-makeKeyAndOrderFront:` is `v24@0:8@16`, sent with a nil
        // sender. `-runModal` later shows this same panel.
        let shown = unsafe {
            appkit::send_v(alert.id(), sel!(layout));
            let panel = appkit::send_id(alert.id(), sel!(window));
            if !panel.is_null() {
                appkit::send_v_id(panel, sel!(makeKeyAndOrderFront:), Id::NIL);
            }
            !panel.is_null()
        };
        self.alert = Some(alert);
        shown
    }

    fn activate(&mut self) {
        let Some(app) = self.app else {
            return;
        };
        // SAFETY: `app` is the live shared application [`Self::make_app`] got on
        // this main thread; `-activateIgnoringOtherApps:` is `v20@0:8B16`, the
        // send winit makes at launch.
        unsafe {
            crate::appkit::send_v_bool(app, aterm_objc::sel!(activateIgnoringOtherApps:), true);
        }
    }

    fn run(&mut self) {
        let Some(alert) = self.alert.take() else {
            return;
        };
        // `-runModal` spins a nested run loop until the alert is dismissed;
        // park the main-thread watchdog for exactly that long.
        let _park = crate::watchdog::park_modal();
        // SAFETY: main thread ([`Self::present`] checked it and built the alert
        // on it); `-runModal` is `-(NSModalResponse)` on the live alert.
        let _ = unsafe { crate::appkit::send_isize(alert.id(), aterm_objc::sel!(runModal)) };
    }
}

#[cfg(not(target_os = "macos"))]
impl AlertHost for Live {
    fn make_app(&mut self, _policy: Option<AppActivation>) -> bool {
        false
    }

    fn present(&mut self, title: &str, body: &str) -> bool {
        self.words = Some((title.to_owned(), body.to_owned()));
        false
    }

    fn activate(&mut self) {}

    fn run(&mut self) {
        if let Some((title, body)) = self.words.take() {
            crate::menu::notify(&title, &body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertHost, raise};
    use crate::{AppActivation, LaunchPosture, launch_posture};

    /// What a host was asked, in order.
    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        MakeApp(Option<AppActivation>),
        Present(String),
        Activate,
        Run,
    }

    /// A host that records each step; `makes` and `shows` are whether it can
    /// make the application object and put the panel on screen.
    struct Recorder {
        makes: bool,
        shows: bool,
        steps: Vec<Step>,
    }

    impl AlertHost for Recorder {
        fn make_app(&mut self, policy: Option<AppActivation>) -> bool {
            self.steps.push(Step::MakeApp(policy));
            self.makes
        }

        fn present(&mut self, title: &str, body: &str) -> bool {
            self.steps.push(Step::Present(format!("{title}: {body}")));
            self.shows
        }

        fn activate(&mut self) {
            self.steps.push(Step::Activate);
        }

        fn run(&mut self) {
            self.steps.push(Step::Run);
        }
    }

    fn steps(makes: bool, shows: bool, posture: LaunchPosture) -> Vec<Step> {
        let mut host = Recorder {
            makes,
            shows,
            steps: Vec::new(),
        };
        raise(&mut host, posture, "aterm could not start", "spawn failed");
        host.steps
    }

    fn present() -> Step {
        Step::Present("aterm could not start: spawn failed".into())
    }

    /// A window launch's fatal alert asks to be the active app after the
    /// application object exists and the alert's panel is on screen, and runs
    /// after that: a bare `-runModal` before `NSApplication` came up behind the
    /// person's window, and an ask with no window up is refused.
    #[test]
    fn a_window_launch_activates_after_its_app_and_panel_and_before_the_run() {
        assert_eq!(
            steps(true, true, launch_posture(false, false)),
            [Step::MakeApp(None), present(), Step::Activate, Step::Run]
        );
    }

    /// An update's candidate never asks to activate at launch, its fatal alert
    /// included; nor does a launch whose application object or panel is not
    /// there (no windowed proof, off the main thread, off macOS). The alert is
    /// still run in every case.
    #[test]
    fn a_candidate_or_a_missing_app_or_panel_is_never_activated_and_still_runs() {
        for (makes, shows, candidate) in [
            (true, true, true),
            (false, true, false),
            (true, false, false),
        ] {
            assert_eq!(
                steps(makes, shows, launch_posture(false, candidate)),
                [Step::MakeApp(None), present(), Step::Run],
                "makes {makes}, shows {shows}, candidate {candidate}"
            );
        }
    }
}
