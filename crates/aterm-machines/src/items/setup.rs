// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Setup Assistant's per-user panes (`DidSee*`/`LastSeen*` keys).

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
    /// Mark every per-user pane seen, so no Apple Account, Siri, Screen Time, Intelligence or
    /// what's-new pane appears at login.
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

/// The items `table` asks for: every key, for every user.
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
#[path = "setup_tests.rs"]
mod tests;
