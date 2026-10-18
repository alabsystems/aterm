// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Setup Assistant's per-user panes: the DidSee*/LastSeen* keys at this build's versions (design §5.1).
//!
//! Setup Assistant shows a user's panes at login (Apple Account, Siri, Screen Time,
//! Intelligence, what's new, …) unless that user's `com.apple.SetupAssistant` domain
//! says they were seen: a `DidSee<Pane>` boolean, or a `LastSeen<Pane>ProductVersion`
//! at this macOS version. Each key is one [`Pref`], written as the user. A version key
//! follows `ctx.facts.os_version`, so after a macOS update the keys read as changed and
//! are written again (the controller marks such a host `os-changed`).
//!
//! The key list is what macOS 27 reads: the names in 27.0's (26A428) Setup Assistant
//! and its helpers (`mbuseragent`, `MiniLauncher`), plus the two a 27.0.1 (26A434) Mac's
//! domain held that those binaries do not name (`DidSeeAvatarSetup`,
//! `DidSeeNewFeaturesProductVersion`). The 27.0 VM's admin domain was empty; every key
//! below was written there as admin from root, read back with its type, and deleted
//! again. Left out: `LastPreLoginTasksPerformedVersion`/`Build`, which record the
//! system's own post-update tasks rather than a pane, and `LastSeenFDERecoveryFlow`,
//! whose type 27 does not show.

use super::defaults::{Pref, Target, Unmet, Want};
use super::{Ctx, Item};
use crate::model::{Effect, PValue};
use serde::Deserialize;

/// The domain the keys live in.
pub const DOMAIN: &str = "com.apple.SetupAssistant";

/// The `[setup]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Mark every per-user pane seen, so no Apple Account, Siri, Screen Time, Intelligence or what's-new pane appears at login.
    pub skip_assistant: Option<bool>,
}

/// What a key holds once its pane counts as seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// `true`.
    Flag,
    /// This macOS version (`sw_vers -productVersion`).
    ProductVersion,
    /// This macOS build (`sw_vers -buildVersion`).
    BuildVersion,
}

/// The keys, with what each holds and where macOS 27 names it.
pub const KEYS: &[(&str, Seen, &str)] = &[
    ("DidSeeAccessibility", Seen::Flag, BOTH),
    ("DidSeeActivationLock", Seen::Flag, BOTH),
    ("DidSeeAppStore", Seen::Flag, BOTH),
    ("DidSeeAppearance", Seen::Flag, BINARY),
    ("DidSeeAppearanceSetup", Seen::Flag, BOTH),
    ("DidSeeApplePaySetup", Seen::Flag, BOTH),
    ("DidSeeAutoUpdatePrompt", Seen::Flag, BINARY),
    ("DidSeeAvatarSetup", Seen::Flag, DOMAIN_271),
    ("DidSeeCloudSetup", Seen::Flag, BOTH),
    ("DidSeeLockdownMode", Seen::Flag, BOTH),
    ("DidSeePrivacy", Seen::Flag, BOTH),
    ("DidSeeScreenTime", Seen::Flag, BOTH),
    ("DidSeeSiriSetup", Seen::Flag, BOTH),
    ("DidSeeSyncSetup", Seen::Flag, BOTH),
    ("DidSeeSyncSetup2", Seen::Flag, BOTH),
    ("DidSeeTermsOfAddress", Seen::Flag, BOTH),
    ("DidSeeTouchIDSetup", Seen::Flag, BOTH),
    ("DidSeeiCloudLoginForStorageServices", Seen::Flag, BOTH),
    (
        "DidSeeNewFeaturesProductVersion",
        Seen::ProductVersion,
        DOMAIN_271,
    ),
    (
        "LastSeenAgeRangeSelectionProductVersion",
        Seen::ProductVersion,
        BINARY,
    ),
    ("LastSeenCloudProductVersion", Seen::ProductVersion, BOTH),
    (
        "LastSeenDiagnosticsProductVersion",
        Seen::ProductVersion,
        BOTH,
    ),
    (
        "LastSeenGlassTintUpsellProductVersion",
        Seen::ProductVersion,
        BOTH,
    ),
    (
        "LastSeenIntelligenceProductVersion",
        Seen::ProductVersion,
        BOTH,
    ),
    ("LastSeenSiriProductVersion", Seen::ProductVersion, BOTH),
    (
        "LastSeenStorageServicesProductVersion",
        Seen::ProductVersion,
        BINARY,
    ),
    ("LastSeenSyncProductVersion", Seen::ProductVersion, BOTH),
    (
        "LastSeeniCloudStorageServicesProductVersion",
        Seen::ProductVersion,
        BOTH,
    ),
    ("LastSeenBuddyBuildVersion", Seen::BuildVersion, BOTH),
];

const BINARY: &str =
    "named by macOS 27.0 (26A428) Setup Assistant; written, read back and deleted in the 27.0 VM";
const BOTH: &str = "named by macOS 27.0 (26A428) Setup Assistant and held by a 27.0.1 (26A434) domain; written, read back and deleted in the 27.0 VM";
const DOMAIN_271: &str =
    "held by a macOS 27.0.1 (26A434) domain; written, read back and deleted in the 27.0 VM";

/// The items `table` asks for: every key, for every user. `skip_assistant = false`
/// leaves Setup Assistant alone.
///
/// # Errors
/// One line naming what cannot be expanded.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    if table.skip_assistant != Some(true) {
        return Ok(Vec::new());
    }
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for user in &ctx.users {
        for &(key, seen, verified) in KEYS {
            let id = format!("setup.{key}@{}", user.name);
            let (value, fact) = match seen {
                Seen::Flag => (Some(PValue::Bool(true)), ""),
                Seen::ProductVersion => (
                    (!ctx.facts.os_version.is_empty())
                        .then(|| PValue::Str(ctx.facts.os_version.clone())),
                    "macOS version",
                ),
                Seen::BuildVersion => (
                    (!ctx.facts.os_build.is_empty())
                        .then(|| PValue::Str(ctx.facts.os_build.clone())),
                    "macOS build",
                ),
            };
            let Some(value) = value else {
                items.push(Box::new(Unmet {
                    family: "setup",
                    id,
                    title: format!("Setup Assistant {key} for {}", user.name),
                    why: format!("the host's {fact} was not read, so {key} has nothing to hold"),
                }));
                continue;
            };
            let title = format!("Setup Assistant {key} = {value} for {}", user.name);
            items.push(Box::new(
                Pref::new(
                    "setup",
                    id,
                    title,
                    Target::user(DOMAIN, key, user),
                    Want::Value(value),
                )
                .felt(Effect::Login)
                .provenance(verified),
            ));
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, export, missing, user};
    use crate::model::{Mode, Outcome, Probe};
    use crate::port::{Fake, Ran};

    /// The 27.0 VM admin's `com.apple.SetupAssistant` after every key below was written
    /// (`defaults export`, captured before they were deleted again).
    const VM_SKIPPED: &[u8] =
        include_bytes!("../../fixtures/prefs/vm-27.0-setupassistant-skipped.plist");

    /// The DidSee*/LastSeen* entries of a 27.0.1 (26A434) user's domain whose panes
    /// were partly seen at setup on 26.5 and some since (read only).
    const SEEN_ON_27_0_1: &str = "<dict>\n\t<key>DidSeeAccessibility</key>\n\t<true/>\n\t<key>DidSeeAppStore</key>\n\t<false/>\n\t<key>DidSeeNewFeaturesProductVersion</key>\n\t<string>26.5</string>\n\t<key>LastSeenSiriProductVersion</key>\n\t<string>27.0.1</string>\n\t<key>LastSeenBuddyBuildVersion</key>\n\t<string>22F2083</string>\n</dict>";

    const EXPORT: &str = "admin$ /usr/bin/defaults export com.apple.SetupAssistant -";

    #[test]
    fn every_key_is_one_item_per_user_at_this_hosts_versions() {
        let users = [user("admin", 501), user("student", 502)];
        let items = expand(
            &Table {
                skip_assistant: Some(true),
            },
            &ctx(&users),
        )
        .unwrap();
        assert_eq!(items.len(), KEYS.len() * 2);
        assert_eq!(items[0].id(), "setup.DidSeeAccessibility@admin");
        assert_eq!(items[KEYS.len()].id(), "setup.DidSeeAccessibility@student");
        let titles: Vec<String> = items.iter().map(|i| i.title()).collect();
        assert!(titles.contains(
            &"Setup Assistant LastSeenSiriProductVersion = \"27.0\" for admin".to_owned()
        ));
        assert!(titles.contains(
            &"Setup Assistant LastSeenBuddyBuildVersion = \"26A428\" for student".to_owned()
        ));
        assert!(
            items
                .iter()
                .all(|i| i.family() == "setup" && i.effect() == Effect::Login)
        );
    }

    #[test]
    fn skip_assistant_false_or_absent_leaves_setup_assistant_alone() {
        let c = ctx(&[user("admin", 501)]);
        assert!(expand(&Table::default(), &c).unwrap().is_empty());
        assert!(
            expand(
                &Table {
                    skip_assistant: Some(false)
                },
                &c
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn the_vms_empty_domain_plans_every_key_and_its_written_domain_probes_ok() {
        let c = ctx(&[user("admin", 501)]);
        let items = expand(
            &Table {
                skip_assistant: Some(true),
            },
            &c,
        )
        .unwrap();
        let sys = Fake::root();
        sys.on(EXPORT, missing());
        let rows = engine::run(&items, Mode::Plan, &sys, &c, &mut NoJournal);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::Planned),
            "{rows:?}"
        );
        let sys = Fake::root();
        sys.on(
            EXPORT,
            Ran {
                status: Some(0),
                stdout: VM_SKIPPED.to_vec(),
                stderr: Vec::new(),
            },
        );
        let rows = engine::run(&items, Mode::Apply, &sys, &c, &mut NoJournal);
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
        assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
    }

    #[test]
    fn a_pane_seen_on_an_older_version_or_not_at_all_is_planned_again() {
        let mut c = ctx(&[user("admin", 501)]);
        c.facts.os_version = "27.0.1".into();
        c.facts.os_build = "26A434".into();
        let items = expand(
            &Table {
                skip_assistant: Some(true),
            },
            &c,
        )
        .unwrap();
        let sys = Fake::root();
        sys.on(EXPORT, export(SEEN_ON_27_0_1));
        let probe = |key: &str| {
            items
                .iter()
                .find(|i| i.id() == format!("setup.{key}@admin"))
                .unwrap()
                .probe(&sys, &c)
        };
        assert_eq!(
            probe("DidSeeAccessibility"),
            Probe::Ok { now: "true".into() }
        );
        assert_eq!(
            probe("DidSeeAppStore"),
            Probe::Change {
                from: "false".into(),
                to: "true".into()
            }
        );
        assert_eq!(
            probe("LastSeenSiriProductVersion"),
            Probe::Ok {
                now: "\"27.0.1\"".into()
            }
        );
        assert_eq!(
            probe("DidSeeNewFeaturesProductVersion"),
            Probe::Change {
                from: "\"26.5\"".into(),
                to: "\"27.0.1\"".into()
            }
        );
        assert_eq!(
            probe("LastSeenBuddyBuildVersion"),
            Probe::Change {
                from: "\"22F2083\"".into(),
                to: "\"26A434\"".into()
            }
        );
    }

    #[test]
    fn a_version_key_waits_when_the_hosts_version_was_not_read() {
        let mut c = ctx(&[user("admin", 501)]);
        c.facts.os_version.clear();
        let items = expand(
            &Table {
                skip_assistant: Some(true),
            },
            &c,
        )
        .unwrap();
        let siri = items
            .iter()
            .find(|i| i.id() == "setup.LastSeenSiriProductVersion@admin")
            .unwrap();
        let sys = Fake::root();
        assert!(
            matches!(siri.probe(&sys, &c), Probe::Blocked { why } if why.contains("macOS version"))
        );
        let flag = items
            .iter()
            .find(|i| i.id() == "setup.DidSeeSiriSetup@admin")
            .unwrap();
        sys.on(EXPORT, missing());
        assert!(matches!(flag.probe(&sys, &c), Probe::Change { .. }));
    }

    #[test]
    fn the_key_list_matches_what_the_vm_read_back() {
        let crate::model::PValue::Dict(entries) = crate::plist::parse(VM_SKIPPED).unwrap() else {
            panic!("not a dict");
        };
        let mut read: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        let mut keys: Vec<&str> = KEYS.iter().map(|(k, _, _)| *k).collect();
        read.sort_unstable();
        keys.sort_unstable();
        assert_eq!(read, keys);
    }
}
