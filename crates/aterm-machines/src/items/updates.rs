// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Software Update: preferences in `/Library/Preferences/com.apple.SoftwareUpdate`, and
//! checking for updates through `softwareupdate --schedule` (design §5.1).
//!
//! These are the keys System Settings ▸ General ▸ Software Update ▸ Automatic Updates
//! writes, as root-scope [`Pref`]s. On macOS 27 the MDM payload for them is gone (only
//! supervised declarative management has a successor), so the preference file is what
//! an unmanaged Mac has. A 27.0.1 (26A434) Mac whose switches were set in System
//! Settings holds `AutomaticDownload`, `AutomaticallyInstallMacOSUpdates`,
//! `ConfigDataInstall` and `CriticalUpdateInstall` (each `<true/>`) and no
//! `AutomaticCheckEnabled`; the 27.0 VM (26A428) holds none of the five, only
//! softwareupdated's own bookkeeping (`LastSuccessfulDate`, `RecommendedUpdates`, …),
//! so each reads absent, the OS default. Each key was written as root there, read back
//! as a boolean with the file left `root:wheel 0644`, and deleted again. Whether
//! softwareupdated honors a key written this way, rather than by System Settings, was
//! not measured.
//!
//! Checking for updates is not one of those keys on 27. VM (27.0, 26A428): an
//! `AutomaticCheckEnabled` written as root was gone again after the next `softwareupdate
//! -l`, `softwareupdate --schedule off` exited 0 and left `--schedule` reading `Automatic
//! checking for updates is turned on`, and so did every state of the key. So
//! [`Schedule`] reads `--schedule` and asks for it on with `--schedule on`, which was never
//! measured turning it on (no Mac read off); a baseline that wants it off is blocked with
//! that measurement, which says so when the Mac is not on 27.0.

use super::defaults::{Pref, Target, Want};
use super::{Ctx, Item};
use crate::model::{Effect, PValue, Probe};
use crate::port::{Cmd, System};
use serde::Deserialize;

/// The domain, under `/Library/Preferences`.
pub const DOMAIN: &str = "com.apple.SoftwareUpdate";

/// The `[updates]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Check for updates (`softwareupdate --schedule`, see the module comment).
    pub automatic_check: Option<bool>,
    /// `AutomaticDownload`.
    pub automatic_download: Option<bool>,
    /// `AutomaticallyInstallMacOSUpdates`.
    pub automatic_macos_install: Option<bool>,
    /// `CriticalUpdateInstall` and `ConfigDataInstall` (security responses, XProtect).
    pub critical: Option<bool>,
}

/// The items `table` asks for, one per key, in the order System Settings lists them.
///
/// # Errors
/// One line naming what cannot be expanded.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let rows: [(Option<bool>, &str, &str, &str); 4] = [
        (
            table.automatic_download,
            "updates.automatic-download",
            "AutomaticDownload",
            "download new updates when available",
        ),
        (
            table.automatic_macos_install,
            "updates.automatic-macos-install",
            "AutomaticallyInstallMacOSUpdates",
            "install macOS updates",
        ),
        (
            table.critical,
            "updates.critical",
            "CriticalUpdateInstall",
            "install Security Responses and system files",
        ),
        (
            table.critical,
            "updates.config-data",
            "ConfigDataInstall",
            "install XProtect and other configuration data",
        ),
    ];
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(on) = table.automatic_check {
        items.push(Box::new(Schedule { on }));
    }
    for (wanted, id, key, what) in rows {
        let Some(on) = wanted else {
            continue;
        };
        let state = if on { "on" } else { "off" };
        let title = format!("Software Update: {what} {state} ({DOMAIN} {key})");
        items.push(Box::new(
            Pref::new(
                "updates",
                id.to_owned(),
                title,
                Target::system(DOMAIN, key),
                Want::Value(PValue::Bool(on)),
            )
            .felt(Effect::Now)
            .provenance(
                "macOS 27.0 (26A428) VM: written as root, read back as a boolean, deleted; held by 27.0.1 (26A434) after System Settings (AutomaticCheckEnabled absent there)",
            ),
        ));
    }
    Ok(items)
}

/// Why checking for updates is not turned off on a Mac running `os_version`: what 27.0
/// did, and, on any other system, that it was measured there and not here.
fn off_blocked(os_version: &str) -> String {
    let why = "checking for updates is not turned off: `softwareupdate --schedule off` \
               changed nothing on macOS 27.0 and the next check removed AutomaticCheckEnabled"
        .to_owned();
    match os_version {
        "27.0" => why,
        "" => format!("{why}; this Mac's macOS version was not read"),
        other => format!("{why}; this Mac runs macOS {other}, where that was not measured"),
    }
}

/// `softwareupdate`, whose `--schedule` reads and sets checking for updates.
const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// Checking for updates, as `softwareupdate --schedule` reports it.
struct Schedule {
    on: bool,
}

impl Schedule {
    fn word(on: bool) -> &'static str {
        if on { "on" } else { "off" }
    }
}

impl Item for Schedule {
    fn id(&self) -> String {
        "updates.automatic-check".into()
    }

    fn family(&self) -> &'static str {
        "updates"
    }

    fn title(&self) -> String {
        format!(
            "Software Update: check for updates {} (softwareupdate --schedule)",
            Self::word(self.on)
        )
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let ran = match sys.run(&Cmd::new(SOFTWAREUPDATE).arg("--schedule")) {
            Ok(ran) if ran.success() => ran,
            Ok(ran) => {
                return Probe::Unusable {
                    why: format!("softwareupdate --schedule: {}", ran.describe()),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("softwareupdate --schedule: {e}"),
                };
            }
        };
        let text = ran.out();
        let now = if text.contains("is turned on") {
            true
        } else if text.contains("is turned off") {
            false
        } else {
            return Probe::Unusable {
                why: format!("softwareupdate --schedule printed {:?}", text.trim()),
            };
        };
        match (now, self.on) {
            (now, on) if now == on => Probe::Ok {
                now: Self::word(now).into(),
            },
            // Never measured: the VM never read `turned off`, so whether `--schedule on`
            // turns checking on is not known. The engine's read-back reports a setter that
            // changes nothing as Failed (`still reads off`), never as Changed.
            (false, true) => Probe::Change {
                from: "off".into(),
                to: "on".into(),
            },
            (_, _) => Probe::Blocked {
                why: off_blocked(&ctx.facts.os_version),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        match sys.run(&Cmd::new(SOFTWAREUPDATE).args(["--schedule", "on"])) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!("softwareupdate --schedule on: {}", ran.describe())),
            Err(e) => Err(format!("softwareupdate --schedule on: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, export};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    /// `defaults export /Library/Preferences/com.apple.SoftwareUpdate -` in the 27.0 VM
    /// (26A428): softwareupdated's bookkeeping and none of the switches.
    const VM_SOFTWARE_UPDATE: &[u8] =
        include_bytes!("../../fixtures/prefs/vm-27.0-softwareupdate.plist");

    /// The switches as a 27.0.1 (26A434) Mac holds them after System Settings (read only).
    const SWITCHED_ON_27_0_1: &str = "<dict>\n\t<key>AutomaticDownload</key>\n\t<true/>\n\t<key>AutomaticallyInstallMacOSUpdates</key>\n\t<true/>\n\t<key>ConfigDataInstall</key>\n\t<true/>\n\t<key>CriticalUpdateInstall</key>\n\t<true/>\n</dict>";

    const EXPORT: &str = "/usr/bin/defaults export /Library/Preferences/com.apple.SoftwareUpdate -";
    const PLIST: &str = "/Library/Preferences/com.apple.SoftwareUpdate.plist";

    const SCHEDULE: &str = "/usr/sbin/softwareupdate --schedule";

    /// `softwareupdate --schedule`, VM (27.0), with and without AutomaticCheckEnabled and
    /// after `--schedule off`.
    const SCHEDULE_ON: &str = "Automatic checking for updates is turned on\n";

    #[test]
    fn checking_reads_from_schedule_and_off_is_blocked_by_what_27_does() {
        let sys = Fake::root();
        sys.on(SCHEDULE, Ran::ok(SCHEDULE_ON));
        let on = expand(
            &Table {
                automatic_check: Some(true),
                ..Table::default()
            },
            &ctx(&[]),
        )
        .unwrap();
        let rows = engine::run(&on, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        let off = expand(
            &Table {
                automatic_check: Some(false),
                ..Table::default()
            },
            &ctx(&[]),
        )
        .unwrap();
        let rows = engine::run(&off, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Blocked);
        // Measured on 27.0 only: another system is blocked too, and its row names it
        // rather than claiming 27.0's behaviour for it.
        let mut older = ctx(&[]);
        older.facts.os_version = "26.6.2".into();
        let rows = engine::run(&off, Mode::Apply, &sys, &older, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Blocked);
        assert!(
            rows[0].why.as_deref().unwrap().contains("26.6.2"),
            "{rows:?}"
        );
        assert!(
            !sys.calls().iter().any(|c| c.ends_with("--schedule off")),
            "{:?}",
            sys.calls()
        );
        let garbled = Fake::root();
        garbled.on(SCHEDULE, Ran::ok("something else\n"));
        let rows = engine::run(&on, Mode::Plan, &garbled, &ctx(&[]), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
    }

    fn lab() -> Table {
        Table {
            automatic_check: Some(true),
            automatic_download: Some(true),
            automatic_macos_install: Some(false),
            critical: Some(true),
        }
    }

    #[test]
    fn critical_covers_both_security_keys_and_each_key_is_a_system_item() {
        let items = expand(&lab(), &ctx(&[])).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "updates.automatic-check",
                "updates.automatic-download",
                "updates.automatic-macos-install",
                "updates.critical",
                "updates.config-data",
            ]
        );
        assert!(
            items
                .iter()
                .all(|i| i.family() == "updates" && i.effect() == Effect::Now)
        );
        assert_eq!(
            items[2].title(),
            "Software Update: install macOS updates off (com.apple.SoftwareUpdate AutomaticallyInstallMacOSUpdates)"
        );
        assert!(expand(&Table::default(), &ctx(&[])).unwrap().is_empty());
    }

    #[test]
    fn the_vms_unset_switches_are_written_as_root_and_read_back() {
        let sys = Fake::root();
        sys.on(
            EXPORT,
            Ran {
                status: Some(0),
                stdout: VM_SOFTWARE_UPDATE.to_vec(),
                stderr: Vec::new(),
            },
        );
        sys.on(EXPORT, export(SWITCHED_ON_27_0_1));
        sys.file(PLIST, b"bplist00");
        sys.on(
            "/usr/bin/defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool true",
            Ran::ok(""),
        );
        let items: Vec<Box<dyn Item>> = expand(
            &Table {
                automatic_download: Some(true),
                ..Table::default()
            },
            &ctx(&[]),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(rows[0].from.as_deref(), Some("absent"));
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "/usr/bin/defaults delete /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload  # it was not set before this run"
            )
        );
        assert!(
            sys.calls().iter().all(|c| !c.contains('$')),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn switches_system_settings_already_set_probe_ok_and_a_differing_one_is_planned() {
        let sys = Fake::root();
        sys.on(EXPORT, export(SWITCHED_ON_27_0_1));
        sys.on(SCHEDULE, Ran::ok(SCHEDULE_ON));
        let items = expand(&lab(), &ctx(&[])).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &ctx(&[]), &mut NoJournal);
        let outcomes: Vec<Outcome> = rows.iter().map(|r| r.outcome).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::Ok, // checking: --schedule reads on
                Outcome::Ok,
                Outcome::Planned, // automatic macOS installs: true, wanted false
                Outcome::Ok,
                Outcome::Ok,
            ]
        );
        assert_eq!(rows[2].from.as_deref(), Some("true"));
        assert_eq!(rows[2].to.as_deref(), Some("false"));
    }
}
