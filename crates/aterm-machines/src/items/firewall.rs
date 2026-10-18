// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The application firewall through `socketfilterfw` (design §5.1).
//!
//! Since macOS 15 `/Library/Preferences/com.apple.alf.plist` is not authoritative; only
//! `socketfilterfw` reads and sets the live state. Its answers, measured in the macOS 27.0
//! VM in both states (exit 0 for every read, root or not):
//!
//! * `--getglobalstate`: `Firewall is enabled. (State = 1)` / `Firewall is disabled. (State = 0)`
//! * `--getstealthmode`: `Firewall stealth mode is on` / `… is off`
//! * `--getallowsigned`: two lines, `Automatically allow built-in signed software
//!   ENABLED.` and `Automatically allow downloaded signed software ENABLED.` (or
//!   `DISABLED.`), the second being `--setallowsignedapp`'s setting
//!
//! A `--set…` as root prints nothing and exits 0; not as root it prints `Must be root to
//! change settings.` and exits 255. Block-all (`--setblockall`) is never touched: it would
//! refuse the SSH connection every later run arrives on.

use super::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, System};
use serde::Deserialize;

/// `socketfilterfw`.
pub const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

/// The `[firewall]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `--setglobalstate`.
    pub enabled: Option<bool>,
    /// `--setstealthmode`.
    pub stealth: Option<bool>,
    /// `--setallowsigned` (built-in software).
    pub allow_signed: Option<bool>,
    /// `--setallowsignedapp` (signed downloads).
    pub allow_signed_downloaded: Option<bool>,
    /// Refused whatever its value: block-all refuses incoming SSH, the only way the
    /// controller reaches the Mac. Kept so the refusal can say why.
    pub block_all: Option<bool>,
}

/// The items `table` asks for.
///
/// # Errors
/// `block_all` named, with the reason.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    if table.block_all.is_some() {
        return Err(
            "[firewall] block_all is refused: blocking all incoming connections \
                    refuses the SSH connection the controller reaches this Mac on, so no \
                    later run could undo it"
                .to_owned(),
        );
    }
    let rows = [
        (Setting::Enabled, table.enabled),
        (Setting::Stealth, table.stealth),
        (Setting::AllowSigned, table.allow_signed),
        (
            Setting::AllowSignedDownloaded,
            table.allow_signed_downloaded,
        ),
    ];
    Ok(rows
        .into_iter()
        .filter_map(|(setting, wanted)| {
            wanted.map(|on| Box::new(Switch { setting, on }) as Box<dyn Item>)
        })
        .collect())
}

/// One `socketfilterfw` switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// The firewall itself.
    Enabled,
    /// Stealth mode (no answer to probes and pings).
    Stealth,
    /// Allow built-in signed software.
    AllowSigned,
    /// Allow downloaded signed software.
    AllowSignedDownloaded,
}

impl Setting {
    const fn get(self) -> &'static str {
        match self {
            Self::Enabled => "--getglobalstate",
            Self::Stealth => "--getstealthmode",
            Self::AllowSigned | Self::AllowSignedDownloaded => "--getallowsigned",
        }
    }

    const fn set(self) -> &'static str {
        match self {
            Self::Enabled => "--setglobalstate",
            Self::Stealth => "--setstealthmode",
            Self::AllowSigned => "--setallowsigned",
            Self::AllowSignedDownloaded => "--setallowsignedapp",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Enabled => "firewall.enabled",
            Self::Stealth => "firewall.stealth",
            Self::AllowSigned => "firewall.allow-signed",
            Self::AllowSignedDownloaded => "firewall.allow-signed-downloaded",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Enabled => "Firewall",
            Self::Stealth => "Firewall stealth mode",
            Self::AllowSigned => "Firewall: allow built-in signed software",
            Self::AllowSignedDownloaded => "Firewall: allow downloaded signed software",
        }
    }

    /// The setting in `socketfilterfw`'s answer to [`Setting::get`].
    #[must_use]
    pub fn parse(self, text: &str) -> Read<bool> {
        let found = match self {
            Self::Enabled => text.lines().find_map(|l| {
                let n = l.split_once("(State = ")?.1.split_once(')')?.0;
                n.trim().parse::<u32>().ok().map(|n| n != 0)
            }),
            Self::Stealth => text.lines().find_map(|l| {
                let state = l.trim().strip_prefix("Firewall stealth mode is ")?;
                word(state, "on", "off")
            }),
            Self::AllowSigned => signed_line(text, "Automatically allow built-in signed software"),
            Self::AllowSignedDownloaded => {
                signed_line(text, "Automatically allow downloaded signed software")
            }
        };
        found.map_or_else(
            || {
                Read::Unusable(format!(
                    "socketfilterfw {} answered {:?}",
                    self.get(),
                    text.trim()
                ))
            },
            Read::Value,
        )
    }
}

fn signed_line(text: &str, prefix: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let state = l.trim().strip_prefix(prefix)?.trim().trim_end_matches('.');
        word(state, "ENABLED", "DISABLED")
    })
}

fn word(text: &str, on: &str, off: &str) -> Option<bool> {
    match text.trim() {
        t if t == on => Some(true),
        t if t == off => Some(false),
        _ => None,
    }
}

const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

struct Switch {
    setting: Setting,
    on: bool,
}

impl Switch {
    fn set(&self, on: bool) -> Cmd {
        Cmd::new(SOCKETFILTERFW).args([self.setting.set(), on_off(on)])
    }
}

impl Item for Switch {
    fn id(&self) -> String {
        self.setting.id().to_owned()
    }

    fn family(&self) -> &'static str {
        "firewall"
    }

    fn title(&self) -> String {
        format!("{} {}", self.setting.title(), on_off(self.on))
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let cmd = Cmd::new(SOCKETFILTERFW).arg(self.setting.get());
        let read = match sys.run(&cmd) {
            Ok(r) if r.success() => self.setting.parse(&r.out()),
            Ok(r) => Read::Unusable(format!("{}: {}", cmd.display(), r.describe())),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        match read {
            Read::Value(now) if now == self.on => Probe::Ok {
                now: on_off(now).to_owned(),
            },
            Read::Value(now) => Probe::Change {
                from: on_off(now).to_owned(),
                to: on_off(self.on).to_owned(),
            },
            Read::Absent => Probe::Unusable {
                why: format!("{} gave no answer", cmd.display()),
            },
            Read::Unusable(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = self.set(self.on);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if ran.success() {
            Ok(())
        } else {
            // `Must be root to change settings.` goes to standard output.
            let said = format!("{} {}", ran.out().trim(), ran.err());
            let how = ran
                .status
                .map_or_else(|| "killed by a signal".to_owned(), |s| format!("exit {s}"));
            Err(format!("{}: {how}: {}", cmd.display(), said.trim()))
        }
    }

    fn revert(&self) -> Option<String> {
        Some(self.set(!self.on).display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::{Fake, Ran};

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    const FW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

    #[test]
    fn every_answer_socketfilterfw_gave_in_the_vm_parses_in_both_states() {
        assert_eq!(
            Setting::Enabled.parse("Firewall is disabled. (State = 0)\n"),
            Read::Value(false)
        );
        assert_eq!(
            Setting::Enabled.parse("Firewall is enabled. (State = 1)\n"),
            Read::Value(true)
        );
        assert_eq!(
            Setting::Stealth.parse("Firewall stealth mode is off\n"),
            Read::Value(false)
        );
        assert_eq!(
            Setting::Stealth.parse("Firewall stealth mode is on\n"),
            Read::Value(true)
        );
        let both_on = "Automatically allow built-in signed software ENABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
        let built_in_off = "Automatically allow built-in signed software DISABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
        let both_off = "Automatically allow built-in signed software DISABLED.\nAutomatically allow downloaded signed software DISABLED.\n";
        assert_eq!(Setting::AllowSigned.parse(both_on), Read::Value(true));
        assert_eq!(Setting::AllowSigned.parse(built_in_off), Read::Value(false));
        assert_eq!(
            Setting::AllowSignedDownloaded.parse(built_in_off),
            Read::Value(true)
        );
        assert_eq!(
            Setting::AllowSignedDownloaded.parse(both_off),
            Read::Value(false)
        );
    }

    #[test]
    fn an_answer_the_parser_does_not_know_is_unusable_not_off() {
        assert!(matches!(
            Setting::Stealth.parse("Firewall stealth mode is maybe"),
            Read::Unusable(_)
        ));
        assert!(matches!(Setting::Enabled.parse(""), Read::Unusable(_)));
    }

    #[test]
    fn the_starter_baseline_turns_the_firewall_and_stealth_on_and_reverts_each_off() {
        let sys = Fake::root();
        sys.on(
            &format!("{FW} --getglobalstate"),
            Ran::ok("Firewall is disabled. (State = 0)\n"),
        )
        .on(
            &format!("{FW} --getglobalstate"),
            Ran::ok("Firewall is enabled. (State = 1)\n"),
        )
        .on(&format!("{FW} --setglobalstate on"), Ran::ok(""))
        .on(
            &format!("{FW} --getstealthmode"),
            Ran::ok("Firewall stealth mode is off\n"),
        )
        .on(
            &format!("{FW} --getstealthmode"),
            Ran::ok("Firewall stealth mode is on\n"),
        )
        .on(&format!("{FW} --setstealthmode on"), Ran::ok(""));
        let t = Table {
            enabled: Some(true),
            stealth: Some(true),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::Changed),
            "{rows:#?}"
        );
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(&*format!("{FW} --setglobalstate off"))
        );
        assert_eq!(
            rows[1].revert.as_deref(),
            Some(&*format!("{FW} --setstealthmode off"))
        );
    }

    #[test]
    fn signed_downloads_are_set_with_setallowsignedapp() {
        let sys = Fake::root();
        let on = "Automatically allow built-in signed software ENABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
        sys.on(&format!("{FW} --getallowsigned"), Ran::ok(on));
        let t = Table {
            allow_signed_downloaded: Some(false),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(rows[0].id, "firewall.allow-signed-downloaded");
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(&*format!("{FW} --setallowsignedapp on"))
        );
    }

    #[test]
    fn a_set_without_root_fails_with_socketfilterfws_own_words() {
        let sys = Fake::root();
        sys.on(
            &format!("{FW} --getstealthmode"),
            Ran::ok("Firewall stealth mode is off\n"),
        )
        .on(
            &format!("{FW} --setstealthmode on"),
            Ran {
                status: Some(255),
                stdout: b"Must be root to change settings.\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let t = Table {
            stealth: Some(true),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.contains("exit 255: Must be root"), "{rows:#?}");
    }

    #[test]
    fn a_baseline_asking_for_block_all_is_refused_whatever_it_asks() {
        for v in [true, false] {
            let t = Table {
                block_all: Some(v),
                ..Table::default()
            };
            let err = expand(&t, &ctx()).err().unwrap();
            assert!(err.contains("SSH"), "{err}");
        }
    }
}
