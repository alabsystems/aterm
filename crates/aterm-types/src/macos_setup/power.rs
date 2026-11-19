// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac's power settings as `pmset` reports them: the one reading `aterm machines`' power items
//! (`aterm_machines::items::power`) and the System Resources page's macOS configuration section
//! take. Only aterm-machines runs `pmset` (census `machines-root-door`): its items answer
//! [`Reads`] from its port's `System`, and the window from the [`super::readout::Readout`]
//! `aterm machines macos-reading` printed. The parse and the judgment of a setting's value are
//! here once; neither side keeps a copy.

use super::baseline::PowerProfile;

/// A power source, as `pmset` names its profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// On a charger or a desktop's mains.
    Ac,
    /// On battery.
    Battery,
}

impl Source {
    /// `pmset`'s profile flag.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Ac => "-c",
            Self::Battery => "-b",
        }
    }

    /// The section header `pmset -g custom` prints, without its colon.
    #[must_use]
    pub const fn section(self) -> &'static str {
        match self {
            Self::Ac => "AC Power",
            Self::Battery => "Battery Power",
        }
    }

    /// The profile's word in an item's id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ac => "ac",
            Self::Battery => "battery",
        }
    }
}

/// How a setting's value reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Minutes, 0 = never.
    Minutes,
    /// 0 or 1.
    Switch,
}

/// One baseline field and the `pmset` key behind it.
#[derive(Debug, PartialEq, Eq)]
pub struct Setting {
    /// The id's last part (the field, kebab-cased).
    pub id: &'static str,
    /// The `pmset` key.
    pub key: &'static str,
    /// How its value reads.
    pub kind: Kind,
    /// Its name in a sentence.
    pub title: &'static str,
}

/// `sleep`.
pub const SLEEP: Setting = Setting {
    id: "sleep",
    key: "sleep",
    kind: Kind::Minutes,
    title: "system sleep",
};
/// `displaysleep`.
pub const DISPLAY_SLEEP: Setting = Setting {
    id: "display-sleep",
    key: "displaysleep",
    kind: Kind::Minutes,
    title: "display sleep",
};
/// `disksleep`.
pub const DISK_SLEEP: Setting = Setting {
    id: "disk-sleep",
    key: "disksleep",
    kind: Kind::Minutes,
    title: "disk sleep",
};
/// `powernap`.
pub const POWER_NAP: Setting = Setting {
    id: "power-nap",
    key: "powernap",
    kind: Kind::Switch,
    title: "Power Nap",
};
/// `womp`.
pub const WAKE_ON_LAN: Setting = Setting {
    id: "wake-on-lan",
    key: "womp",
    kind: Kind::Switch,
    title: "wake for network access",
};
/// `autorestart`, only where `pmset -g cap` lists it.
pub const RESTART_AFTER_POWER_LOSS: Setting = Setting {
    id: "restart-after-power-loss",
    key: "autorestart",
    kind: Kind::Switch,
    title: "restart after a power failure",
};
/// `standby`.
pub const STANDBY: Setting = Setting {
    id: "standby",
    key: "standby",
    kind: Kind::Switch,
    title: "standby",
};
/// `tcpkeepalive`.
pub const TCP_KEEPALIVE: Setting = Setting {
    id: "tcp-keepalive",
    key: "tcpkeepalive",
    kind: Kind::Switch,
    title: "TCP keepalive while asleep",
};

/// Each setting `profile` asks for and its wanted value, in the baseline's order.
#[must_use]
pub fn wanted(profile: &PowerProfile) -> Vec<(&'static Setting, u64)> {
    let minutes = |m: Option<u32>| m.map(u64::from);
    let switch = |b: Option<bool>| b.map(u64::from);
    [
        (&SLEEP, minutes(profile.sleep)),
        (&DISPLAY_SLEEP, minutes(profile.display_sleep)),
        (&DISK_SLEEP, minutes(profile.disk_sleep)),
        (&POWER_NAP, switch(profile.power_nap)),
        (&WAKE_ON_LAN, switch(profile.wake_on_lan)),
        (
            &RESTART_AFTER_POWER_LOSS,
            switch(profile.restart_after_power_loss),
        ),
        (&STANDBY, switch(profile.standby)),
        (&TCP_KEEPALIVE, switch(profile.tcp_keepalive)),
    ]
    .into_iter()
    .filter_map(|(setting, value)| value.map(|value| (setting, value)))
    .collect()
}

/// Parse `pmset -g custom` output into `(section_name, Vec<(key, value)>)` pairs.
#[must_use]
pub fn parse_custom(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() {
            continue;
        }
        if !l.starts_with([' ', '\t']) {
            if let Some(name) = l.trim_end().strip_suffix(':') {
                out.push((name.to_owned(), Vec::new()));
            }
            continue;
        }
        let row = l.trim();
        let Some((key, value)) = row.rsplit_once(char::is_whitespace) else {
            continue;
        };
        if let Some((_, rows)) = out.last_mut() {
            rows.push((key.trim_end().to_owned(), value.to_owned()));
        }
    }
    out
}

/// The capabilities `pmset -g cap` lists (one indented key per line).
#[must_use]
pub fn parse_cap(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with([' ', '\t']))
        .map(|l| l.trim().to_owned())
        .filter(|k| !k.is_empty())
        .collect()
}

/// A setting's value in words: `never`, `15 minutes`, `on`.
#[must_use]
pub fn render(kind: Kind, value: u64) -> String {
    match (kind, value) {
        (Kind::Minutes, 0) => "never".to_owned(),
        (Kind::Minutes, 1) => "1 minute".to_owned(),
        (Kind::Minutes, m) => format!("{m} minutes"),
        (Kind::Switch, 0) => "off".to_owned(),
        (Kind::Switch, 1) => "on".to_owned(),
        (Kind::Switch, n) => n.to_string(),
    }
}

/// How a side runs `pmset` with `args`: its standard output, or why it did not answer.
pub trait Reads {
    /// `pmset <args>`'s standard output where it exited 0.
    ///
    /// # Errors
    /// Why it did not run or did not exit 0, naming the command.
    fn pmset(&self, args: &[&str]) -> Result<String, String>;
}

/// Why a setting has no value to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// This Mac does not have it: the words say why.
    NotApplicable(String),
    /// `pmset` did not answer, or answered what is not a number: the words say what it gave.
    Unusable(String),
}

/// `setting`'s value on `source`'s profile, given `custom` (`pmset -g custom`) and, for
/// [`RESTART_AFTER_POWER_LOSS`] only, `cap` (`pmset -g cap`).
///
/// # Errors
/// [`Unread`] where the profile, the capability or the key is absent, or the value is not a
/// number.
pub fn value_in(
    custom: &str,
    cap: Option<&str>,
    source: Source,
    setting: &Setting,
) -> Result<u64, Unread> {
    let sections = parse_custom(custom);
    let Some((_, rows)) = sections.iter().find(|(name, _)| name == source.section()) else {
        return Err(Unread::NotApplicable(format!(
            "this Mac has no {} profile (`pmset -g custom` prints no {} section)",
            source.id(),
            source.section()
        )));
    };
    if setting.key == RESTART_AFTER_POWER_LOSS.key
        && !cap.is_some_and(|cap| parse_cap(cap).iter().any(|k| k == "autorestart"))
    {
        return Err(Unread::NotApplicable(
            "`pmset -g cap` does not list autorestart: this Mac has no restart after a power \
             failure (Apple silicon laptops and VMs do not)"
                .to_owned(),
        ));
    }
    let Some((_, value)) = rows.iter().find(|(k, _)| k == setting.key) else {
        return Err(Unread::NotApplicable(format!(
            "this Mac's {} profile has no {} (`pmset -g custom` does not print it)",
            source.section(),
            setting.key
        )));
    };
    parse_uint(value).ok_or_else(|| {
        Unread::Unusable(format!(
            "`pmset -g custom` gave {} {value:?}, not a number",
            setting.key
        ))
    })
}

/// `setting`'s value on `source`'s profile as `reads` finds it: `pmset -g custom`, and
/// `pmset -g cap` for [`RESTART_AFTER_POWER_LOSS`].
///
/// # Errors
/// [`Unread::Unusable`] where `pmset` did not answer; else as [`value_in`].
pub fn read(reads: &dyn Reads, source: Source, setting: &Setting) -> Result<u64, Unread> {
    let custom = reads.pmset(&["-g", "custom"]).map_err(Unread::Unusable)?;
    let cap = if setting.key == RESTART_AFTER_POWER_LOSS.key {
        Some(reads.pmset(&["-g", "cap"]).map_err(Unread::Unusable)?)
    } else {
        None
    };
    value_in(&custom, cap.as_deref(), source, setting)
}

/// One wanted setting, read: its Now against its Wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The setting.
    pub setting: &'static Setting,
    /// Its wanted value.
    pub wanted: u64,
    /// Its value now, or why it has none.
    pub now: Result<u64, Unread>,
}

impl Row {
    /// Whether it reads, and reads other than wanted.
    #[must_use]
    pub fn differs(&self) -> bool {
        self.now.as_ref().is_ok_and(|now| *now != self.wanted)
    }
}

/// Every setting `profile` asks for on `source`, read with one `pmset -g custom` (and one
/// `pmset -g cap` where restart after a power failure is asked for).
#[must_use]
pub fn rows(reads: &dyn Reads, source: Source, profile: &PowerProfile) -> Vec<Row> {
    let wanted = wanted(profile);
    let custom = reads.pmset(&["-g", "custom"]);
    let cap = wanted
        .iter()
        .any(|(setting, _)| setting.key == RESTART_AFTER_POWER_LOSS.key)
        .then(|| reads.pmset(&["-g", "cap"]));
    wanted
        .into_iter()
        .map(|(setting, value)| {
            let now = match (&custom, &cap) {
                (Err(why), _) => Err(Unread::Unusable(why.clone())),
                (Ok(_), Some(Err(why))) if setting.key == RESTART_AFTER_POWER_LOSS.key => {
                    Err(Unread::Unusable(why.clone()))
                }
                (Ok(custom), cap) => value_in(
                    custom,
                    cap.as_ref().and_then(|cap| cap.as_deref().ok()),
                    source,
                    setting,
                ),
            };
            Row {
                setting,
                wanted: value,
                now,
            }
        })
        .collect()
}

fn parse_uint(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
