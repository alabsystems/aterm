// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local init trackpad scroll direction and scrolling behavior preferences.

use crate::items::defaults::{Pref, Target, Want};
use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, Read, User};
use crate::port::System;
use aterm_types::macos_setup::Setting;

const KEY: &str = "com.apple.swipescrolldirection";

/// The family of the Natural scrolling item, apart from the trackpad scrolling keys' `trackpad`
/// (System State's own words for the two rows): `aterm init --only scrolling` sets the one row,
/// `--only trackpad` the other ([`crate::init::naming`]).
pub const NATURAL: &str = "scrolling";

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
        NATURAL
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

    fn refresh_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        Some(current().and_then(|()| self.activate(sys)))
    }
}

#[cfg(test)]
#[path = "init_trackpad_tests.rs"]
mod tests;
