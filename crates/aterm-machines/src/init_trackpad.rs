// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local init trackpad scroll direction and scrolling behavior preferences.

use crate::items::defaults::{Pref, Target, Want};
use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, Read, User};
use crate::port::System;
use aterm_types::macos_setup::Setting;

const KEY: &str = "com.apple.swipescrolldirection";

/// Turn Natural scrolling off for the resolved local user and current session.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init trackpad settings require exactly one local user".into());
    };
    Ok(vec![Box::new(ScrollDirection {
        user: user.clone(),
        reading: crate::init_observe::LastReading::default(),
    })])
}

/// Returns trackpad scrolling behavior preference items for the resolved local user.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn behavior_items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("trackpad behavior requires exactly one local user".into());
    };
    Ok(behavior_keys()
        .into_iter()
        .map(|(domain, key, by_host, value)| {
            let target = if by_host {
                Target::host(domain, key, user)
            } else {
                Target::user(domain, key, user)
            };
            let id = format!("init.trackpad.{}@{}", target.describe(), user.name);
            let title = format!("Trackpad scrolling: {}", target.describe());
            let item: Box<dyn Item> =
                Box::new(Pref::new("trackpad", id, title, target, Want::Value(value)));
            item
        })
        .collect())
}

/// The trackpad scrolling keys [`behavior_items`] turns on: each one's domain, key, whether it
/// is a ByHost key, and the value it wants.
fn behavior_keys() -> Vec<(&'static str, &'static str, bool, PValue)> {
    let mut keys = Vec::new();
    for domain in [
        "com.apple.AppleMultitouchTrackpad",
        "com.apple.driver.AppleBluetoothMultitouch.trackpad",
    ] {
        for key in [
            "TrackpadScroll",
            "TrackpadHorizScroll",
            "TrackpadMomentumScroll",
        ] {
            keys.push((domain, key, false, PValue::Bool(true)));
        }
    }
    keys.push((
        "-g",
        "com.apple.trackpad.momentumScroll",
        true,
        PValue::Bool(true),
    ));
    keys.push((
        "-g",
        "com.apple.trackpad.scrollBehavior",
        true,
        PValue::Int(2),
    ));
    keys
}

/// The System State row whose item here writes `key` of `domain`, if one does: Natural
/// scrolling's direction, or one of Trackpad scrolling's keys ([`behavior_keys`]).
pub(crate) fn setting_of(domain: &str, key: &str) -> Option<Setting> {
    if domain == "-g" && key == KEY {
        return Some(Setting::NaturalScrolling);
    }
    behavior_keys()
        .iter()
        .any(|(each_domain, each_key, ..)| *each_domain == domain && *each_key == key)
        .then_some(Setting::TrackpadScrolling)
}

#[derive(Debug)]
struct ScrollDirection {
    user: User,
    /// Whether the last probe found natural scrolling on, for System State.
    reading: crate::init_observe::LastReading,
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
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        let target = self.target();
        let read = target.read(sys);
        Ok(if Want::Value(PValue::Bool(false)).satisfied_by(&read)? {
            Vec::new()
        } else {
            vec![(target, read)]
        })
    }
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
        self.reading.set(None);
        let stored = match self.persisted(sys) {
            Ok(stored) => stored,
            Err(why) => return Probe::Unusable { why },
        };
        match sys.natural_scrolling() {
            Err(why) => Probe::Unusable { why },
            Ok(false) if stored => Probe::Ok {
                now: "Natural scrolling is saved as off in macOS and System Settings".into(),
            },
            Ok(natural) => {
                // Off now, as wanted, while the saved preference macOS loads at the next login
                // is not (a Restore of a direction the Mac had never set): off until then.
                self.reading.set(Some(if natural {
                    crate::init_observe::Reading::NaturalOn
                } else {
                    crate::init_observe::Reading::UntilLogin
                }));
                Probe::Change {
                    from: format!(
                        "Natural scrolling: {} in System Settings; preferred value {}persisted",
                        if natural { "on" } else { "off" },
                        if stored { "" } else { "not " },
                    ),
                    to: self.title(),
                }
            }
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
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

    /// After a Restore of a scroll direction the Mac had never set, the saved key is gone and
    /// the live direction stays off until the next login: System State reads Off, marked so,
    /// and the row differs, as the Mac will be on natural scrolling after that login.
    #[test]
    fn natural_scrolling_off_until_the_next_login_reads_so() {
        let seen = |live: bool, saved: &str| {
            let sys = Fake::root();
            sys.scroll_state(live).on(READ, export(saved));
            let rows = engine::run(
                &items(&context()).unwrap(),
                Mode::Plan,
                &sys,
                &context(),
                &mut NoJournal,
            );
            let rows: Vec<&crate::model::ItemReport> = rows.iter().collect();
            crate::init_observe::observation(Setting::NaturalScrolling, &rows)
        };
        let restored = seen(false, "<dict></dict>");
        assert_eq!(
            restored.standing,
            aterm_types::macos_setup::Standing::Differs
        );
        assert_eq!(restored.now.as_deref(), Some("Off · until login"));
        assert_eq!(restored.after_login.as_deref(), Some("On"));
        let saved_on = seen(false, &format!("<dict><key>{KEY}</key><true/></dict>"));
        assert_eq!(saved_on.now.as_deref(), Some("Off · until login"));
        assert_eq!(saved_on.after_login.as_deref(), Some("On"));
        // On now reads On, whatever is saved.
        assert_eq!(seen(true, "<dict></dict>").now.as_deref(), Some("On"));
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
        // System State reads natural scrolling as on from the probe itself.
        assert_eq!(
            crate::init_observe::Reading::of(&rows[0]),
            Some(crate::init_observe::Reading::NaturalOn)
        );
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
