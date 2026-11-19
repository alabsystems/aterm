// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS Software Update's settings: checking for updates as `softwareupdate --schedule` reports
//! it (macOS 27 keeps no preference key for it), and the `com.apple.SoftwareUpdate` keys the
//! `[updates]` table drives. `aterm machines`' updates items (`aterm_machines::items::updates`)
//! and the System Resources page's macOS configuration section share the keys, the schedule's
//! parse and the judgment. Only aterm-machines runs `softwareupdate` (census
//! `machines-root-door`): the window reads the [`super::readout::Readout`] `aterm machines
//! macos-reading` printed.

use super::baseline::Updates;

/// The domain, under `/Library/Preferences`.
pub const DOMAIN: &str = "com.apple.SoftwareUpdate";

/// Which `[updates]` field a key follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `automatic_download`.
    AutomaticDownload,
    /// `automatic_macos_install`.
    AutomaticMacosInstall,
    /// `critical`, which drives two keys.
    Critical,
}

impl Field {
    /// The field's wanted value in `table`.
    #[must_use]
    pub const fn of(self, table: &Updates) -> Option<bool> {
        match self {
            Self::AutomaticDownload => table.automatic_download,
            Self::AutomaticMacosInstall => table.automatic_macos_install,
            Self::Critical => table.critical,
        }
    }
}

/// One key the table drives: its field, its item id, the key, and what it does in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// The `[updates]` field it follows.
    pub field: Field,
    /// The item's id.
    pub id: &'static str,
    /// The preference key.
    pub key: &'static str,
    /// What it does, in a sentence.
    pub what: &'static str,
    /// Whether turning it off lowers this Mac's security: the page never offers that press.
    pub installs: bool,
}

/// Every key the table drives, in the order System Settings lists them.
pub const KEYS: [Key; 4] = [
    Key {
        field: Field::AutomaticDownload,
        id: "updates.automatic-download",
        key: "AutomaticDownload",
        what: "download new updates when available",
        installs: false,
    },
    Key {
        field: Field::AutomaticMacosInstall,
        id: "updates.automatic-macos-install",
        key: "AutomaticallyInstallMacOSUpdates",
        what: "install macOS updates",
        installs: true,
    },
    Key {
        field: Field::Critical,
        id: "updates.critical",
        key: "CriticalUpdateInstall",
        what: "install Security Responses and system files",
        installs: true,
    },
    Key {
        field: Field::Critical,
        id: "updates.config-data",
        key: "ConfigDataInstall",
        what: "install XProtect and other configuration data",
        installs: true,
    },
];

/// Whether `softwareupdate --schedule`'s output says checking is on; `None` for anything else.
#[must_use]
pub fn schedule(text: &str) -> Option<bool> {
    if text.contains("is turned on") {
        Some(true)
    } else if text.contains("is turned off") {
        Some(false)
    } else {
        None
    }
}

/// What a key of [`DOMAIN`] holds, as `aterm machines`' updates items judge it
/// (`aterm_machines::items::defaults::Want::satisfied_by`): a boolean, the `1`/`0` integer that
/// satisfies one, nothing, or a value of another kind, which satisfies no wanted switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// The switch, on or off.
    Switch(bool),
    /// Not set: macOS's default applies.
    Unset,
    /// A value that is no switch, as `defaults export` printed it.
    Other(String),
}

/// How a side reads Software Update.
pub trait Reads {
    /// `softwareupdate --schedule`'s standard output.
    ///
    /// # Errors
    /// Why it did not answer.
    fn schedule(&self) -> Result<String, String>;

    /// What the preference `key` of [`DOMAIN`] holds.
    ///
    /// # Errors
    /// Why it could not be read.
    fn pref(&self, key: &str) -> Result<Held, String>;
}

/// Software Update as read now: checking, and each of [`KEYS`] in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesNow {
    /// Checking for updates, or why it did not read.
    pub check: Result<bool, String>,
    /// What each key holds, or why it did not read.
    pub keys: Vec<(Key, Result<Held, String>)>,
}

/// Read Software Update through `reads`.
#[must_use]
pub fn now(reads: &dyn Reads) -> UpdatesNow {
    let check = reads.schedule().and_then(|text| {
        schedule(&text)
            .ok_or_else(|| format!("softwareupdate --schedule printed {:?}", text.trim()))
    });
    let keys = KEYS.iter().map(|key| (*key, reads.pref(key.key))).collect();
    UpdatesNow { check, keys }
}

/// What differs between `now` and `wanted`, each in words (`check for updates`, the key's what).
/// Checking that is on while wanted off is no difference: macOS will not turn it off
/// (`aterm_machines::items::updates` blocks it). A key differs where it holds anything but its
/// wanted switch, as the items' judgment has it; an unread value differs in nothing.
#[must_use]
pub fn differs(now: &UpdatesNow, wanted: &Updates) -> Vec<&'static str> {
    let mut out = Vec::new();
    if wanted.automatic_check == Some(true) && now.check == Ok(false) {
        out.push("check for updates");
    }
    for (key, value) in &now.keys {
        let Some(want) = key.field.of(wanted) else {
            continue;
        };
        if let Ok(held) = value
            && *held != Held::Switch(want)
        {
            out.push(key.what);
        }
    }
    out
}

/// Whether applying `wanted` over `now` would turn an automatic install off, which lowers this
/// Mac's security: the page never offers that press.
#[must_use]
pub fn lowers_security(now: &UpdatesNow, wanted: &Updates) -> bool {
    now.keys.iter().any(|(key, value)| {
        key.installs && key.field.of(wanted) == Some(false) && *value != Ok(Held::Switch(false))
    })
}
