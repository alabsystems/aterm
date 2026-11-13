// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sharing services through launchd: Remote Apple Events.

use super::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;

/// `launchctl`.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// `print system/<label>`'s exit status for a job launchd has not loaded.
pub const NOT_LOADED: i32 = 113;

/// `bootout`'s exit status for a job that is not loaded (`No such process`).
pub const NO_SUCH_PROCESS: i32 = 3;

/// A system launchd service: its label and the plist that defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Service {
    /// The launchd label.
    pub label: &'static str,
    /// Its plist, which `bootstrap` loads and whose `Disabled` key is the default state.
    pub plist: &'static str,
    /// The System Settings name.
    pub title: &'static str,
}

/// Remote Apple Events: `com.apple.AEServer`, defined by `com.apple.eppc.plist`.
pub const REMOTE_APPLE_EVENTS: Service = Service {
    label: "com.apple.AEServer",
    plist: "/System/Library/LaunchDaemons/com.apple.eppc.plist",
    title: "Remote Apple Events",
};

/// Remote Login (probe only: the `person` family reads it).
pub const REMOTE_LOGIN: Service = Service {
    label: "com.openssh.sshd",
    plist: "/System/Library/LaunchDaemons/ssh.plist",
    title: "Remote Login",
};

/// Screen Sharing (probe only: the `person` family reads it).
pub const SCREEN_SHARING: Service = Service {
    label: "com.apple.screensharing",
    plist: "/System/Library/LaunchDaemons/com.apple.screensharing.plist",
    title: "Screen Sharing",
};

/// The `[sharing]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `system/com.apple.AEServer` on (enabled and loaded) or off (disabled and not loaded).
    pub remote_apple_events: Option<bool>,
    /// Refused whatever its value: Remote Login is a person step (`[person] remote_login`), and
    /// turning it off would cut the Mac off from the controller.
    pub remote_login: Option<bool>,
    /// Refused whatever its value: only the System Settings switch grants Screen Sharing control,
    /// so it is a person step (`[person] screen_sharing`).
    pub screen_sharing: Option<bool>,
}

/// The items `table` asks for.
/// # Errors
/// `remote_login` or `screen_sharing` named, with the reason.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    match table.remote_login {
        Some(false) => {
            return Err(
                "[sharing] remote_login = false is refused: the controller reaches this \
                        Mac only over Remote Login, so turning it off would cut the Mac off from \
                        every later run (Remote Login is a person step: [person] remote_login)"
                    .to_owned(),
            );
        }
        Some(true) => {
            return Err(
                "[sharing] remote_login is not a setting: Remote Login is on whenever \
                        the agent runs, and [person] remote_login counts it"
                    .to_owned(),
            );
        }
        None => {}
    }
    if table.screen_sharing.is_some() {
        return Err(
            "[sharing] screen_sharing is refused: launchd can start Screen Sharing, but \
                    only the System Settings switch grants it control (design §1.6); count it \
                    with [person] screen_sharing"
                .to_owned(),
        );
    }
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(on) = table.remote_apple_events {
        items.push(Box::new(Switch {
            id: "sharing.remote-apple-events",
            service: REMOTE_APPLE_EVENTS,
            on,
        }));
    }
    Ok(items)
}

/// Each `(label, enabled)` row of `launchctl print-disabled system`.
#[must_use]
pub fn parse_print_disabled(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let (label, state) = l.trim().split_once(" => ")?;
            let label = label.strip_prefix('"')?.strip_suffix('"')?;
            let enabled = match state.trim() {
                "enabled" => true,
                "disabled" => false,
                _ => return None,
            };
            Some((label.to_owned(), enabled))
        })
        .collect()
}

/// Whether launchd will run `service`: its row in the override database, or, when it has none, the
/// plist's `Disabled` key (no key = enabled).
#[must_use]
pub fn enabled(sys: &dyn System, service: &Service) -> Read<bool> {
    let cmd = Cmd::new(LAUNCHCTL).args(["print-disabled", "system"]);
    let ran = match sys.run(&cmd) {
        Ok(r) if r.success() => r,
        Ok(r) => return Read::Unusable(format!("{}: {}", cmd.display(), r.describe())),
        Err(e) => return Read::Unusable(format!("{}: {e}", cmd.display())),
    };
    let text = ran.out();
    if let Some((_, on)) = parse_print_disabled(&text)
        .into_iter()
        .find(|(label, _)| label == service.label)
    {
        return Read::Value(on);
    }
    let quoted = format!("\"{}\" =>", service.label);
    if let Some(row) = text.lines().find(|l| l.contains(&quoted)) {
        return Read::Unusable(format!("{} answered {:?}", cmd.display(), row.trim()));
    }
    let cmd = Cmd::new("/usr/bin/plutil").args(["-extract", "Disabled", "raw", service.plist]);
    match sys.run(&cmd) {
        Ok(r) if r.success() => match r.out().trim() {
            "true" => Read::Value(false),
            "false" => Read::Value(true),
            other => Read::Unusable(format!("{}: answered {other:?}", cmd.display())),
        },
        Ok(r) if r.err().contains("No value at that key path") => Read::Value(true),
        Ok(r) => Read::Unusable(format!("{}: {}", cmd.display(), r.describe())),
        Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
    }
}

/// Whether launchd has loaded `label` in the system domain.
#[must_use]
pub fn loaded(sys: &dyn System, label: &str) -> Read<bool> {
    let cmd = print(label);
    match sys.run(&cmd) {
        Ok(r) if r.success() => Read::Value(true),
        Ok(r) if r.status == Some(NOT_LOADED) => Read::Value(false),
        Ok(r) => Read::Unusable(format!("{}: {}", cmd.display(), r.describe())),
        Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
    }
}

fn print(label: &str) -> Cmd {
    Cmd::new(LAUNCHCTL).args(["print", &format!("system/{label}")])
}

fn verb(verb: &str, label: &str) -> Cmd {
    Cmd::new(LAUNCHCTL).args([verb, &format!("system/{label}")])
}

fn bootstrap(service: &Service) -> Cmd {
    Cmd::new(LAUNCHCTL).args(["bootstrap", "system", service.plist])
}

fn run(sys: &dyn System, cmd: &Cmd) -> Result<Ran, String> {
    sys.run(cmd).map_err(|e| format!("{}: {e}", cmd.display()))
}

const fn render(enabled: bool, loaded: bool) -> &'static str {
    match (enabled, loaded) {
        (true, true) => "on",
        (false, false) => "off",
        (true, false) => "enabled, not loaded",
        (false, true) => "disabled, still loaded",
    }
}

struct Switch {
    id: &'static str,
    service: Service,
    on: bool,
}

impl Item for Switch {
    fn id(&self) -> String {
        self.id.to_owned()
    }

    fn family(&self) -> &'static str {
        "sharing"
    }

    fn title(&self) -> String {
        format!(
            "{} {}",
            self.service.title,
            if self.on { "on" } else { "off" }
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let state = answered(enabled(sys, &self.service))
            .and_then(|e| answered(loaded(sys, self.service.label)).map(|l| (e, l)));
        let (enabled, loaded) = match state {
            Ok(both) => both,
            Err(why) => return Probe::Unusable { why },
        };
        let now = render(enabled, loaded);
        if enabled == self.on && loaded == self.on {
            Probe::Ok {
                now: now.to_owned(),
            }
        } else {
            Probe::Change {
                from: now.to_owned(),
                to: render(self.on, self.on).to_owned(),
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let label = self.service.label;
        if self.on {
            let cmd = verb("enable", label);
            let ran = run(sys, &cmd)?;
            if !ran.success() {
                return Err(format!("{}: {}", cmd.display(), ran.describe()));
            }
            if loaded(sys, label) == Read::Value(true) {
                return Ok(());
            }
            let cmd = bootstrap(&self.service);
            let ran = run(sys, &cmd)?;
            if !ran.success() {
                return Err(format!("{}: {}", cmd.display(), ran.describe()));
            }
        } else {
            let cmd = verb("disable", label);
            let ran = run(sys, &cmd)?;
            if !ran.success() {
                return Err(format!("{}: {}", cmd.display(), ran.describe()));
            }
            let cmd = verb("bootout", label);
            let ran = run(sys, &cmd)?;
            if !ran.success() && ran.status != Some(NO_SUCH_PROCESS) {
                return Err(format!("{}: {}", cmd.display(), ran.describe()));
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        let label = self.service.label;
        let undo = if self.on {
            [verb("disable", label), verb("bootout", label)]
        } else {
            [verb("enable", label), bootstrap(&self.service)]
        };
        Some(undo.map(|c| c.display()).join("; "))
    }
}

fn answered(read: Read<bool>) -> Result<bool, String> {
    match read {
        Read::Value(v) => Ok(v),
        Read::Absent => Err("launchd gave no answer".to_owned()),
        Read::Unusable(why) => Err(why),
    }
}

#[cfg(test)]
#[path = "sharing_tests.rs"]
mod tests;
