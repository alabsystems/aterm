// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Consumer catalog presets over verified `defaults` rows.

use super::defaults::{Pref, Relaunches, Scope, Target, Unmet, Want};
use super::{Ctx, Item, widgets};
use crate::model::{Effect, PValue};
use serde::Deserialize;

/// The `[consumer]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `false` turns Siri off.
    pub siri: Option<bool>,
    /// `false` opts out of Siri and Dictation improvement.
    pub siri_data_sharing: Option<bool>,
    /// `false` stops diagnostics and usage submission.
    pub analytics: Option<bool>,
    /// `false` turns personalized ads off.
    pub personalized_ads: Option<bool>,
    /// `false` makes AirDrop discoverable by no one.
    pub airdrop: Option<bool>,
    /// `false` hides desktop and Stage Manager widgets and clears Notification Center's;
    /// `true` shows the first two again, and leaves Notification Center's list, which no
    /// write puts back, to the person.
    pub widgets: Option<bool>,
    /// Refused: Universal Control is atpkg's (`[machine] universal_control` in aterm.toml).
    pub universal_control: Option<bool>,
    /// `false` stops the keyboard/mouse setup assistant on desktops with none attached.
    pub bluetooth_setup_assistant: Option<bool>,
    /// `false` stops Finder writing .DS_Store on network and USB volumes.
    pub ds_store_on_network: Option<bool>,
}

impl Table {
    fn preset(&self, name: &str) -> Option<bool> {
        match name {
            "siri" => self.siri,
            "siri_data_sharing" => self.siri_data_sharing,
            "analytics" => self.analytics,
            "personalized_ads" => self.personalized_ads,
            "airdrop" => self.airdrop,
            "widgets" => self.widgets,
            "bluetooth_setup_assistant" => self.bluetooth_setup_assistant,
            "ds_store_on_network" => self.ds_store_on_network,
            _ => None,
        }
    }
}

/// A value a compiled row writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lit {
    /// `-bool`.
    Bool(bool),
    /// `-int`.
    Int(i64),
    /// `-string`.
    Str(&'static str),
    /// Notification Center's widget list with no widget ([`widgets::cleared`]).
    NoWidgets,
}

impl Lit {
    /// As a property-list value: for [`Lit::NoWidgets`], the list written where there was none.
    #[must_use]
    pub fn value(self) -> PValue {
        match self {
            Self::Bool(b) => PValue::Bool(b),
            Self::Int(i) => PValue::Int(i),
            Self::Str(s) => PValue::Str(s.to_owned()),
            Self::NoWidgets => PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ]),
        }
    }
}

/// One compiled row.
#[derive(Debug, Clone, Copy)]
pub struct CatalogRow {
    /// The `[consumer]` field that turns it on or off.
    pub preset: &'static str,
    /// The item id (`@<user>` is appended for a user row).
    pub id: &'static str,
    /// What it controls, for the title.
    pub what: &'static str,
    /// The domain (a name for a user row; a name or absolute path for a system row).
    pub domain: &'static str,
    /// The key.
    pub key: &'static str,
    /// Where it lives.
    pub scope: Scope,
    /// The value that turns the feature off.
    pub off: Lit,
    /// The value that turns it back on, or `None` to delete the key (the OS default).
    pub on: Option<Lit>,
    /// A process in the user's session that reads the key only when it starts.
    pub restart: Option<&'static str>,
    /// When the change is felt.
    pub effect: Effect,
    /// Where it was verified: macOS version and source.
    pub verified: &'static str,
}

/// The catalog, one row per key.
pub const CATALOG: &[CatalogRow] = &[
    CatalogRow {
        preset: "siri",
        id: "consumer.siri",
        what: "Siri",
        domain: "com.apple.assistant.support",
        key: "Assistant Enabled",
        scope: Scope::User,
        off: Lit::Bool(false),
        on: Some(Lit::Bool(true)),
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as a boolean, restored; 27.0.1 (26A434) holds it (1) after Siri setup. Source: NIST mSCP tahoe system_settings_siri_disable",
    },
    CatalogRow {
        preset: "siri_data_sharing",
        id: "consumer.siri-data-sharing",
        what: "Siri and Dictation improvement",
        domain: "com.apple.assistant.support",
        key: "Siri Data Sharing Opt-In Status",
        scope: Scope::User,
        off: Lit::Int(2),
        on: None,
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as an integer, restored; 27.0.1 (26A434) holds 2 after opting out in Setup Assistant. Source: NIST mSCP tahoe system_settings_improve_siri_dictation_disable (2 = opted out)",
    },
    CatalogRow {
        preset: "analytics",
        id: "consumer.analytics.auto-submit",
        what: "Mac Analytics sharing",
        domain: "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory",
        key: "AutoSubmit",
        scope: Scope::System,
        off: Lit::Bool(false),
        on: Some(Lit::Bool(true)),
        restart: None,
        effect: Effect::Now,
        verified: "macOS 27.0 (26A428) VM: written as root, read back as a boolean, restored (cfprefsd rewrote the file root:wheel 0600; the item puts its owner and mode back); 27.0.1 (26A434) holds 0. Source: NIST mSCP tahoe system_settings_diagnostics_reports_disable",
    },
    CatalogRow {
        preset: "analytics",
        id: "consumer.analytics.consent-version",
        what: "the analytics consent version the choice was made under",
        domain: "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory",
        key: "AutoSubmitVersion",
        scope: Scope::System,
        off: Lit::Int(18),
        on: Some(Lit::Int(18)),
        restart: None,
        effect: Effect::Now,
        verified: "macOS 27.0 (26A428) VM: with AutoSubmit false and no AutoSubmitVersion, a reboot removed AutoSubmit; with AutoSubmitVersion 18 it survived the reboot. 27.0.1 (26A434) holds 18 after System Settings",
    },
    CatalogRow {
        preset: "analytics",
        id: "consumer.analytics.third-party",
        what: "sharing analytics with app developers",
        domain: "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory",
        key: "ThirdPartyDataSubmit",
        scope: Scope::System,
        off: Lit::Bool(false),
        on: Some(Lit::Bool(true)),
        restart: None,
        effect: Effect::Now,
        verified: "macOS 27.0 (26A428) VM: written as root, read back as a boolean, restored; 27.0.1 (26A434) holds 0. Source: NIST mSCP tahoe system_settings_diagnostics_reports_disable",
    },
    CatalogRow {
        preset: "personalized_ads",
        id: "consumer.personalized-ads",
        what: "personalized ads",
        domain: "com.apple.AdLib",
        key: "allowApplePersonalizedAdvertising",
        scope: Scope::User,
        off: Lit::Bool(false),
        on: Some(Lit::Bool(true)),
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: holds false after Setup Assistant; written, read back as a boolean, restored; 27.0.1 (26A434) holds 0. Source: NIST mSCP tahoe system_settings_personalized_advertising_disable",
    },
    CatalogRow {
        preset: "airdrop",
        id: "consumer.airdrop",
        what: "AirDrop discoverability",
        domain: "com.apple.sharingd",
        key: "DiscoverableMode",
        scope: Scope::User,
        off: Lit::Str("Off"),
        on: None,
        restart: Some("sharingd"),
        effect: Effect::Relaunch,
        verified: "macOS 27.0 (26A428) VM: written, read back as a string, kept across a relaunch of the user's sharingd, restored; 27.0.1 (26A434) holds Everyone. Values Off, Contacts Only, Everyone (System Settings' AirDrop menu)",
    },
    CatalogRow {
        preset: "widgets",
        id: "consumer.widgets.desktop",
        what: "desktop widgets",
        domain: "com.apple.WindowManager",
        key: "StandardHideWidgets",
        scope: Scope::User,
        off: Lit::Bool(true),
        on: None,
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as a boolean, restored; the key exists on 27.0.1 (26A434) (Desktop & Dock ▸ Widgets ▸ Show Widgets: On Desktop)",
    },
    CatalogRow {
        preset: "widgets",
        id: "consumer.widgets.stage-manager",
        what: "Stage Manager widgets",
        domain: "com.apple.WindowManager",
        key: "StageManagerHideWidgets",
        scope: Scope::User,
        off: Lit::Bool(true),
        on: None,
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as a boolean, restored; the key exists on 27.0.1 (26A434) (Desktop & Dock ▸ Widgets ▸ Show Widgets: In Stage Manager)",
    },
    CatalogRow {
        preset: "widgets",
        id: "consumer.widgets.notification-center",
        what: "Notification Center widgets",
        domain: "com.apple.notificationcenterui",
        key: "widgets",
        scope: Scope::User,
        off: Lit::NoWidgets,
        on: None,
        restart: Some(widgets::READER),
        effect: Effect::Relaunch,
        verified: "macOS 27.0 (26A428) VM, 2026-10-22: the list is in Notification Center's app container; read in full (8053 bytes) only by a process whose app holds Full Disk Access, and without it macOS warns the person and prints an empty domain (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.6d item 6). Cleared as aterm init's widgets row cleared it: no instance, no desktop placement, its other fields kept",
    },
    CatalogRow {
        preset: "bluetooth_setup_assistant",
        id: "consumer.bluetooth-setup-assistant.keyboard",
        what: "the Bluetooth keyboard setup assistant",
        domain: "com.apple.Bluetooth",
        key: "BluetoothAutoSeekKeyboard",
        scope: Scope::System,
        off: Lit::Bool(false),
        on: None,
        restart: None,
        effect: Effect::Restart,
        verified: "macOS 27.0 (26A428) VM: written as root, read back, restored (the VM has no com.apple.Bluetooth domain); 27.0.1 (26A434) holds true. Source: the Bluetooth Setup Assistant that opens at login on a Mac with no keyboard",
    },
    CatalogRow {
        preset: "bluetooth_setup_assistant",
        id: "consumer.bluetooth-setup-assistant.pointing-device",
        what: "the Bluetooth mouse and trackpad setup assistant",
        domain: "com.apple.Bluetooth",
        key: "BluetoothAutoSeekPointingDevice",
        scope: Scope::System,
        off: Lit::Bool(false),
        on: None,
        restart: None,
        effect: Effect::Restart,
        verified: "macOS 27.0 (26A428) VM: written as root, read back, restored; 27.0.1 (26A434) holds true",
    },
    CatalogRow {
        preset: "ds_store_on_network",
        id: "consumer.ds-store.network",
        what: ".DS_Store files on network volumes",
        domain: "com.apple.desktopservices",
        key: "DSDontWriteNetworkStores",
        scope: Scope::User,
        off: Lit::Bool(true),
        on: None,
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as a boolean, restored (the domain is empty by default). Source: Apple support, Adjust SMB browsing behavior in macOS",
    },
    CatalogRow {
        preset: "ds_store_on_network",
        id: "consumer.ds-store.usb",
        what: ".DS_Store files on USB volumes",
        domain: "com.apple.desktopservices",
        key: "DSDontWriteUSBStores",
        scope: Scope::User,
        off: Lit::Bool(true),
        on: None,
        restart: None,
        effect: Effect::Login,
        verified: "macOS 27.0 (26A428) VM: written, read back as a boolean, restored",
    },
];

/// The items `table` asks for.
/// # Errors
/// One line naming what cannot be expanded (`universal_control`, which is atpkg's).
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    expand_with(table, ctx, &Relaunches::default())
}

/// [`expand`] with shared relaunches across `relaunches` ([`Pref::shared`]).
/// # Errors
/// As [`expand`].
pub fn expand_with(
    table: &Table,
    ctx: &Ctx,
    relaunches: &Relaunches,
) -> Result<Vec<Box<dyn Item>>, String> {
    if table.universal_control.is_some() {
        return Err("[consumer] universal_control: Universal Control is atpkg's ([machine] universal_control in aterm.toml, default \"off\"); one key has one owner".into());
    }
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for row in CATALOG {
        let Some(enabled) = table.preset(row.preset) else {
            continue;
        };
        if row.off == Lit::NoWidgets {
            if !enabled {
                items.extend(no_widgets(row, ctx, relaunches));
            }
            continue;
        }
        let want = match (enabled, row.on) {
            (false, _) => Want::Value(row.off.value()),
            (true, Some(on)) => Want::Value(on.value()),
            (true, None) => Want::Unset,
        };
        let state = if enabled { "on" } else { "off" };
        let pref = |target: Target, id: String, whose: &str| {
            let title = format!("{} {state}{whose} ({})", row.what, target.describe());
            let pref = Pref::new("consumer", id, title, target, want.clone())
                .felt(row.effect)
                .provenance(row.verified);
            match row.restart {
                Some(p) => pref.restart(p).shared(relaunches),
                None => pref,
            }
        };
        match row.scope {
            Scope::System => items.push(Box::new(pref(
                Target::system(row.domain, row.key),
                row.id.to_owned(),
                "",
            ))),
            Scope::User | Scope::Host => {
                for user in &ctx.users {
                    let target = if row.scope == Scope::Host {
                        Target::host(row.domain, row.key, user)
                    } else {
                        Target::user(row.domain, row.key, user)
                    };
                    items.push(Box::new(pref(
                        target,
                        format!("{}@{}", row.id, user.name),
                        &format!(" for {}", user.name),
                    )));
                }
            }
        }
    }
    Ok(items)
}

/// The items of a [`Lit::NoWidgets`] row, one per user ([`super::defaults::withheld`]).
fn no_widgets(row: &CatalogRow, ctx: &Ctx, relaunches: &Relaunches) -> Vec<Box<dyn Item>> {
    ctx.users
        .iter()
        .map(|user| {
            let id = format!("{}@{}", row.id, user.name);
            let title = format!(
                "{} off for {} ({} {})",
                row.what, user.name, row.domain, row.key
            );
            match super::defaults::withheld(row.domain, ctx.facts.full_disk_access) {
                Some(why) => Box::new(Unmet {
                    family: "consumer",
                    id,
                    title,
                    why,
                }) as Box<dyn Item>,
                None => Box::new(widgets::NoWidgets::new(
                    id,
                    title,
                    (row.domain, row.key),
                    user,
                    row.effect,
                    relaunches,
                )),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
