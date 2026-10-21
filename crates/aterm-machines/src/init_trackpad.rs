// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local init's scroll direction, verified both in preferences and through the
//! native preference getter used by System Settings. Merely writing defaults is
//! insufficient to apply an input preference to an already running login session;
//! Apple's getter itself does not independently query WindowServer input state.
//! Every explicit apply therefore refreshes the native setter even when saved
//! values already match. Plan/check only inspect the configured preference.

use crate::items::defaults::{Target, Want};
use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, User};
use crate::port::System;

const KEY: &str = "com.apple.swipescrolldirection";

/// Turn Natural scrolling off for the resolved local user and current session.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init trackpad settings require exactly one local user".into());
    };
    Ok(vec![Box::new(ScrollDirection { user: user.clone() })])
}

#[derive(Debug)]
struct ScrollDirection {
    user: User,
}

impl ScrollDirection {
    fn target(&self) -> Target {
        Target::user("-g", KEY, &self.user)
    }

    fn persisted(&self, sys: &dyn System) -> Result<bool, String> {
        Want::Value(PValue::Bool(false)).satisfied_by(&self.target().read(sys))
    }

    fn activate(&self, sys: &dyn System) -> Result<(), String> {
        sys.set_natural_scrolling(false)?;
        if sys.natural_scrolling()? || !self.persisted(sys)? {
            return Err("Natural scrolling did not read back as off in both the saved preference and System Settings".into());
        }
        Ok(())
    }
}

impl Item for ScrollDirection {
    fn id(&self) -> String {
        format!("init.trackpad.scroll-direction@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "trackpad"
    }

    fn title(&self) -> String {
        "Natural scrolling off, immediately and at login".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let stored = match self.persisted(sys) {
            Ok(stored) => stored,
            Err(why) => return Probe::Unusable { why },
        };
        match sys.natural_scrolling() {
            Err(why) => Probe::Unusable { why },
            Ok(false) if stored => Probe::Ok {
                now: "Natural scrolling is saved as off in macOS and System Settings".into(),
            },
            Ok(natural) => Probe::Change {
                from: format!(
                    "Natural scrolling: {} in System Settings; preferred value {}persisted",
                    if natural { "on" } else { "off" },
                    if stored { "" } else { "not " },
                ),
                to: self.title(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        // Confirm native access before altering persistence; an unavailable API
        // must not turn this into a silently login-only setting.
        sys.natural_scrolling()?;
        let persisted = self.persisted(sys)?;
        if !persisted {
            let cmd = self.target().write_cmd(&PValue::Bool(false));
            let ran = sys
                .run(&cmd)
                .map_err(|e| format!("{}: {e}", cmd.display()))?;
            if !ran.success() {
                return Err(format!("{}: {}", cmd.display(), ran.describe()));
            }
        }
        self.activate(sys)
    }

    fn refresh(&self, sys: &dyn System, _ctx: &Ctx) -> Option<Result<(), String>> {
        Some(self.activate(sys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    const READ: &str = "alice$ /usr/bin/defaults export -g -";
    const WRITE: &str =
        "alice$ /usr/bin/defaults write -g com.apple.swipescrolldirection -bool false";

    fn pref(value: bool) -> Ran {
        export(&format!(
            "<dict><key>{KEY}</key><{}/></dict>",
            if value { "true" } else { "false" }
        ))
    }

    fn context() -> Ctx {
        ctx(&[user("alice", 501)])
    }

    #[test]
    fn a_correct_saved_setting_gets_exactly_one_native_refresh_without_a_defaults_write() {
        let sys = Fake::root();
        sys.scroll_state(false).on(READ, pref(false));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.as_str() == "scroll-direction set false")
                .count(),
            1
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
    }

    #[test]
    fn correct_persistence_with_stale_native_state_still_applies_immediately() {
        let sys = Fake::root();
        sys.scroll_state(true).on(READ, pref(false));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert!(sys.calls().contains(&"scroll-direction set false".into()));
        assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
    }

    #[test]
    fn changing_the_preference_also_applies_native_state_and_checks_both_readbacks() {
        let sys = Fake::root();
        sys.scroll_state(true)
            .on(READ, pref(true))
            .on(READ, pref(true))
            .on(READ, pref(false));
        sys.on(WRITE, Ran::ok(""));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert!(sys.calls().contains(&WRITE.into()));
        assert!(!sys.natural_scrolling().unwrap());
    }

    #[test]
    fn a_refusing_native_setter_or_a_failed_runtime_readback_never_reports_success() {
        for mismatches in [false, true] {
            let sys = Fake::root();
            sys.scroll_state(true).on(READ, pref(false));
            if mismatches {
                sys.scroll_readback_after_set(true);
            } else {
                sys.refuse_scroll_set("synthetic native refusal");
            }
            let rows = engine::run(
                &items(&context()).unwrap(),
                Mode::Apply,
                &sys,
                &context(),
                &mut NoJournal,
            );
            assert_eq!(rows[0].outcome, Outcome::Failed);
        }
    }

    #[test]
    fn a_persistent_readback_mismatch_is_also_a_failure() {
        let sys = Fake::root();
        sys.scroll_state(true)
            .on(READ, pref(true))
            .on(WRITE, Ran::ok(""));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(!sys.natural_scrolling().unwrap());
    }

    #[test]
    fn plan_only_probes_preferences_and_native_state() {
        let sys = Fake::root();
        sys.scroll_state(true).on(READ, pref(true));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Plan,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(sys.calls(), [READ, "scroll-direction get"]);
    }

    #[test]
    fn a_correct_setting_is_also_read_only_in_plan_and_a_failed_refresh_is_not_success() {
        let sys = Fake::root();
        sys.scroll_state(false).on(READ, pref(false));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Plan,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(sys.calls(), [READ, "scroll-direction get"]);
        sys.refuse_scroll_set("synthetic refresh refusal");
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert_eq!(rows[0].why.as_deref(), Some("synthetic refresh refusal"));
    }
}
