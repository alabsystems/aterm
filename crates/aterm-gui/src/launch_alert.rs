// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launch-fatal alert presentation and AppKit activation sequencing.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::{AppActivation, LaunchPosture};

/// Platform interface for presenting and running a launch-fatal alert.
pub(crate) trait AlertHost {
    /// Ensures the application object exists under `policy`.
    fn make_app(&mut self, policy: Option<AppActivation>) -> bool;
    /// Builds the alert and places its panel on screen.
    fn present(&mut self, title: &str, body: &str) -> bool;
    /// Ask to be the active app.
    fn activate(&mut self);
    /// Run the alert until it is dismissed.
    fn run(&mut self);
}

/// Presents and runs the launch-fatal alert through `host` under `posture`.
pub(crate) fn raise(host: &mut impl AlertHost, posture: LaunchPosture, title: &str, body: &str) {
    let made = host.make_app(posture.policy);
    if host.present(title, body) && made && posture.activate_on_launch {
        host.activate();
    }
    host.run();
}

/// Whether this launch is an update's handoff candidate: it names a rendezvous to dial or the overlap pipes.
static HANDOFF_CANDIDATE: AtomicBool = AtomicBool::new(false);

/// Record that this launch is an update's handoff candidate.
pub(crate) fn mark_handoff_candidate() {
    HANDOFF_CANDIDATE.store(true, Ordering::Relaxed);
}

/// Shows the launch-fatal alert through the live platform host.
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
        let Some(_windowed) = crate::platform::Windowed::get() else {
            return false;
        };
        if MainThread::new().is_none() {
            return false;
        }
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

    /// A host that records each step; `makes` and `shows` are whether it can make the application object and put the panel on screen.
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

    /// A window launch's fatal alert asks to be the active app after the application object exists and the alert's panel is on screen, and runs after that: a bare `-runModal` before `NSApplication` came up behind the person's window, and an ask with no window up is refused.
    #[test]
    fn a_window_launch_activates_after_its_app_and_panel_and_before_the_run() {
        assert_eq!(
            steps(true, true, launch_posture(false, false)),
            [Step::MakeApp(None), present(), Step::Activate, Step::Run]
        );
    }

    /// An update's candidate never asks to activate at launch, its fatal alert included; nor does a launch whose application object or panel is not there (no windowed proof, off the main thread, off macOS).
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
