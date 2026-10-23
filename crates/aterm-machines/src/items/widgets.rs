// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Notification Center's widget list with no widget.

use super::defaults::{self, Relaunch, Relaunches, Target};
use super::{Activation, Ctx, Item};
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::port::{Cmd, System};
use std::sync::{Arc, Mutex, PoisonError};

/// The process that reads the list, and is relaunched to read a write of it.
pub const READER: &str = "NotificationCenter";

/// What `read` (Notification Center's `widgets` key) should hold to show no widget.
/// # Errors
/// The read failed, or the list is not the dictionary of an instances array it is known as; it is
/// then left untouched.
pub fn cleared(
    sys: &dyn System,
    user: &User,
    read: &Read<PValue>,
) -> Result<Option<PValue>, String> {
    let fields = match read {
        Read::Unusable(why) => return Err(why.clone()),
        Read::Absent => {
            return Ok(Some(PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ])));
        }
        Read::Value(PValue::Dict(fields)) => fields,
        Read::Value(_) => {
            return Err("widget configuration is not a dictionary; left untouched".into());
        }
    };
    let instances = fields.iter().find(|(k, _)| k == "instances");
    match instances {
        Some((_, PValue::Array(values))) if values.is_empty() => {
            match fields
                .iter()
                .find(|(key, _)| key == "DesktopWidgetPlacementStorage")
            {
                None => return Ok(None),
                Some((_, value)) if empty_placement(sys, user, value)? => return Ok(None),
                Some(_) => {}
            }
        }
        None => {}
        Some((_, PValue::Array(_))) => {}
        _ => return Err("widget instances are not an array; left untouched".into()),
    }
    let mut next: Vec<_> = fields
        .iter()
        .filter(|(k, _)| k != "instances" && k != "DesktopWidgetPlacementStorage")
        .cloned()
        .collect();
    next.push(("instances".into(), PValue::Array(Vec::new())));
    Ok(Some(PValue::Dict(next)))
}

/// The conversion of the desktop placement Notification Center stores as binary data.
pub fn placement_conversion(user: &User) -> Cmd {
    Cmd::new("/usr/bin/plutil")
        .args(["-convert", "xml1", "-o", "-", "--", "-"])
        .as_user(user)
}

fn empty_placement(sys: &dyn System, user: &User, value: &PValue) -> Result<bool, String> {
    let PValue::Data(bytes) = value else {
        return Ok(false);
    };
    let ran = sys
        .run_complete_stdout_input(&placement_conversion(user), bytes)
        .map_err(|_| "cannot completely inspect desktop widget placement".to_owned())?;
    if !ran.success() {
        return Ok(false);
    }
    let PValue::Dict(fields) = crate::plist::parse(&ran.stdout)
        .map_err(|_| "unrecognized desktop widget placement property list".to_owned())?
    else {
        return Ok(false);
    };
    Ok(fields.len() == 2
        && fields
            .iter()
            .any(|(key, value)| key == "CompatibilityVersion" && *value == PValue::Int(1))
        && fields.iter().any(|(key, value)| {
            key == "NumberedDisplays"
                && matches!(value, PValue::Array(displays) if displays.is_empty())
        }))
}

fn shown(read: &Read<PValue>) -> String {
    match read {
        Read::Value(PValue::Dict(fields)) => match fields.iter().find(|(k, _)| k == "instances") {
            Some((_, PValue::Array(widgets))) if widgets.is_empty() => {
                "no widget, and the placement of removed ones".into()
            }
            Some((_, PValue::Array(widgets))) if widgets.len() == 1 => "1 widget".into(),
            Some((_, PValue::Array(widgets))) => format!("{} widgets", widgets.len()),
            _ => "a widget list with no instances".into(),
        },
        other => defaults::render(other),
    }
}

/// A fleet run's item of the clearing row for one account.
#[derive(Debug)]
pub struct NoWidgets {
    id: String,
    title: String,
    target: Target,
    user: User,
    effect: Effect,
    relaunch: Arc<Relaunch>,
    /// What the probe that planned the change read: the list the revert line restores.
    before: Mutex<Option<Read<PValue>>>,
}

impl NoWidgets {
    /// `user`'s list under `domain` `key`, as the row `id` titled `title` clears it.
    #[must_use]
    pub fn new(
        id: String,
        title: String,
        (domain, key): (&str, &str),
        user: &User,
        effect: Effect,
        relaunches: &Relaunches,
    ) -> Self {
        Self {
            id,
            title,
            target: Target::user(domain, key, user),
            user: user.clone(),
            effect,
            relaunch: relaunches.of(user, READER),
            before: Mutex::new(None),
        }
    }
}

impl Item for NoWidgets {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn family(&self) -> &'static str {
        "consumer"
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        let read = self.target.read(sys);
        Ok(match cleared(sys, &self.user, &read)? {
            None => Vec::new(),
            Some(_) => vec![(self.target.clone(), read)],
        })
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let read = self.target.read(sys);
        match cleared(sys, &self.user, &read) {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Ok {
                now: "no widget".into(),
            },
            Ok(Some(_)) => {
                let from = shown(&read);
                *self.before.lock().unwrap_or_else(PoisonError::into_inner) = Some(read);
                match defaults::disrupts(READER, &self.user, ctx) {
                    Some(why) => Probe::Deferred { why },
                    None => Probe::Change {
                        from,
                        to: "no widget".into(),
                    },
                }
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let read = self.target.read(sys);
        let Some(want) = cleared(sys, &self.user, &read)? else {
            return Ok(());
        };
        let cmd = self.target.write_cmd(&want);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        self.relaunch.owe(&self.id, &self.target);
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        let before = self
            .before
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        defaults::revert_line(&self.target, &before, Some(READER))
    }

    fn effect(&self) -> Effect {
        self.effect
    }

    fn activation(&self) -> Option<Arc<dyn Activation>> {
        Some(Arc::clone(&self.relaunch) as Arc<dyn Activation>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::{export, user};
    use crate::port::{Fake, Ran};

    fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
        let PValue::Dict(entries) = value else {
            return None;
        };
        entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn widget_clear_removes_instances_and_placement_but_keeps_unknown_widget_fields() {
        let sys = Fake::root();
        let who = user("alice", 501);
        sys.on(
            &placement_conversion(&who).display(),
            Ran::exit(1, "invalid plist"),
        );
        let read = Read::Value(PValue::Dict(vec![
            (
                "instances".into(),
                PValue::Array(vec![PValue::Data(vec![1, 2])]),
            ),
            (
                "DesktopWidgetPlacementStorage".into(),
                PValue::Data(vec![3]),
            ),
            ("vers".into(), PValue::Int(1)),
            ("future".into(), PValue::Bool(true)),
        ]));
        let want = cleared(&sys, &who, &read).unwrap().unwrap();
        assert_eq!(field(&want, "instances"), Some(&PValue::Array(vec![])));
        assert_eq!(field(&want, "future"), Some(&PValue::Bool(true)));
        assert_eq!(field(&want, "DesktopWidgetPlacementStorage"), None);
        assert_eq!(cleared(&sys, &who, &Read::Value(want)).unwrap(), None);
        assert!(cleared(&sys, &who, &Read::Absent).unwrap().is_some());
        let residual = Read::Value(PValue::Dict(vec![
            ("instances".into(), PValue::Array(Vec::new())),
            (
                "DesktopWidgetPlacementStorage".into(),
                PValue::Data(vec![3]),
            ),
            ("future".into(), PValue::Bool(true)),
        ]));
        let clear = cleared(&sys, &who, &residual).unwrap().unwrap();
        assert_eq!(field(&clear, "DesktopWidgetPlacementStorage"), None);
        assert_eq!(field(&clear, "future"), Some(&PValue::Bool(true)));
        assert_eq!(cleared(&sys, &who, &Read::Value(clear)).unwrap(), None);
    }

    #[test]
    fn notificationcenters_regenerated_empty_binary_placement_is_already_clear() {
        let bytes = aterm_codec::base64::decode("YnBsaXN0MDDSAQIDBF8QFENvbXBhdGliaWxpdHlWZXJzaW9uXxAQTnVtYmVyZWREaXNwbGF5cxQAAAAAAAAAAAAAAAAAAAABoAgNJDdIAAAAAAAAAQEAAAAAAAAABQAAAAAAAAAAAAAAAAAAAEk=").unwrap();
        let sys = Fake::root();
        let who = user("alice", 501);
        let cmd = placement_conversion(&who);
        let empty = PValue::Dict(vec![
            ("CompatibilityVersion".into(), PValue::Int(1)),
            ("NumberedDisplays".into(), PValue::Array(Vec::new())),
        ]);
        sys.on(&cmd.display(), export(&crate::plist::fragment(&empty)));
        let read = Read::Value(PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            ("instances".into(), PValue::Array(Vec::new())),
            ("DesktopWidgetPlacementStorage".into(), PValue::Data(bytes)),
        ]));
        assert_eq!(cleared(&sys, &who, &read).unwrap(), None);
        assert_eq!(sys.calls(), [cmd.display()]);
        for shape in [
            PValue::Dict(vec![
                ("CompatibilityVersion".into(), PValue::Int(1)),
                (
                    "NumberedDisplays".into(),
                    PValue::Array(vec![PValue::Int(1)]),
                ),
            ]),
            PValue::Dict(vec![
                ("CompatibilityVersion".into(), PValue::Int(1)),
                ("NumberedDisplays".into(), PValue::Array(Vec::new())),
                ("future".into(), PValue::Bool(true)),
            ]),
        ] {
            let next = Fake::root();
            next.on(&cmd.display(), export(&crate::plist::fragment(&shape)));
            assert!(cleared(&next, &who, &read).unwrap().is_some());
        }
        sys.refuse_complete_stdout(&cmd.display(), "incomplete XML capture");
        assert!(cleared(&sys, &who, &read).is_err());
    }
}
