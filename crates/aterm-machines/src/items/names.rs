// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Computer names: `scutil --set ComputerName|LocalHostName|HostName` and the SMB NetBIOSName (design §5.1).
//!
//! `scutil` does not touch the name SMB advertises, so a Computer Name change also writes
//! `NetBIOSName` in `/Library/Preferences/SystemConfiguration/com.apple.smb.server`, derived
//! the way macOS derives a Local hostname (apostrophes dropped, other punctuation and spaces
//! as hyphens), uppercased and cut to NetBIOS's 15 characters.
//!
//! Measured in the macOS 27.0 VM, as root over SSH: `scutil --get HostName` answers
//! `HostName: not set` on standard error with exit 1 when it is unset, which is the
//! documented "not set" ([`Read::Absent`]); `scutil --set HostName ''` unsets it again;
//! `scutil --set LocalHostName 'bad name.x'` fails `SCPreferencesSetLocalHostName() failed:
//! Invalid argument`, which is why the Local hostname is checked when the baseline expands.

use super::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::sync::Mutex;

/// `scutil`.
pub const SCUTIL: &str = "/usr/sbin/scutil";

/// The SMB server's preferences (a `defaults` domain given as a path).
pub const SMB_SERVER: &str = "/Library/Preferences/SystemConfiguration/com.apple.smb.server";

/// NetBIOS names are at most 15 characters.
pub const NETBIOS_MAX: usize = 15;

/// The `[names]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `ComputerName`; `{name}` fills from the inventory. Also sets the SMB NetBIOSName.
    pub computer_name: Option<String>,
    /// `LocalHostName`, the Bonjour `.local` name: letters, digits and hyphens.
    pub local_host_name: Option<String>,
    /// `HostName`, a fully qualified name; `""` unsets it, and leaving it out leaves it alone.
    pub host_name: Option<String>,
}

/// The items `table` asks for.
///
/// # Errors
/// One line naming the name that is still a template or is not a name macOS accepts.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(name) = &table.computer_name {
        filled("computer_name", name)?;
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(format!(
                "[names] computer_name {name:?} must be a non-empty line of text"
            ));
        }
        // scutil's getopt permutes its arguments (in the VM, `scutil --get ComputerName -d`
        // runs with -d), so a value starting `-` is read as options: `-f<path>` would point
        // root's preferences write at <path>.
        if name.starts_with('-') {
            return Err(format!(
                "[names] computer_name {name:?} must not start with a hyphen (scutil reads it as an option)"
            ));
        }
        items.push(Box::new(Scutil::new(Pref::Computer, Some(name.clone()))));
        if let Some(netbios) = netbios_name(name) {
            items.push(Box::new(NetBios::new(netbios)));
        }
    }
    if let Some(name) = &table.local_host_name {
        filled("local_host_name", name)?;
        if !dns_label(name) {
            return Err(format!(
                "[names] local_host_name {name:?} must be 1 to 63 letters, digits and hyphens, \
                 not starting or ending with a hyphen (it is the `.local` name)"
            ));
        }
        items.push(Box::new(Scutil::new(Pref::LocalHost, Some(name.clone()))));
    }
    if let Some(name) = &table.host_name {
        filled("host_name", name)?;
        if name.is_empty() {
            items.push(Box::new(Scutil::new(Pref::Host, None)));
        } else if name.len() <= 253 && name.split('.').all(dns_label) {
            items.push(Box::new(Scutil::new(Pref::Host, Some(name.clone()))));
        } else {
            return Err(format!(
                "[names] host_name {name:?} must be a dotted host name (labels of letters, \
                 digits and hyphens), or \"\" to unset it"
            ));
        }
    }
    Ok(items)
}

/// The NetBIOS name for `computer_name`: apostrophes dropped, every other character that is
/// not an ASCII letter or digit a hyphen (runs collapsed, none at either end), uppercased,
/// at most 15 characters. `None` when nothing is left.
#[must_use]
pub fn netbios_name(computer_name: &str) -> Option<String> {
    let mut out = String::new();
    for c in computer_name.chars() {
        if matches!(c, '\'' | '\u{2019}') {
            continue;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(NETBIOS_MAX);
    let out = out.trim_end_matches('-').to_owned();
    (!out.is_empty()).then_some(out)
}

/// What `scutil --get <pref>` read: the value with its line ending removed, `Absent` for
/// the documented `<pref>: not set` (exit 1), `Unusable` otherwise.
#[must_use]
pub fn read_scutil(ran: &Ran) -> Read<String> {
    if ran.success() {
        return Read::Value(ran.out().trim_end_matches(['\n', '\r']).to_owned());
    }
    let said = format!("{}{}", ran.out(), ran.err());
    if ran.status == Some(1) && said.contains(": not set") {
        Read::Absent
    } else {
        Read::Unusable(ran.describe())
    }
}

/// A leftover `{` means the controller did not fill a placeholder: refuse it rather than
/// name a Mac `{name}`.
fn filled(field: &str, value: &str) -> Result<(), String> {
    if value.contains('{') {
        return Err(format!(
            "[names] {field} {value:?} still holds a placeholder: the controller fills only \
             {{name}}, {{serial}} and {{uuid}}"
        ));
    }
    Ok(())
}

/// One DNS label: 1 to 63 ASCII letters, digits and hyphens, no hyphen at either end.
fn dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

fn render(read: &Read<String>) -> String {
    match read {
        Read::Value(v) => format!("{v:?}"),
        Read::Absent => "not set".to_owned(),
        Read::Unusable(why) => format!("unknown ({why})"),
    }
}

/// Run `cmd`; its result when it exited 0, else one line naming it and how it ended.
fn run_ok(sys: &dyn System, cmd: &Cmd) -> Result<Ran, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        Ok(ran)
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

/// Decide from what the host holds (`now`) and what is wanted (`None` = not set), and keep
/// `now` for the revert when they differ.
fn decide(
    now: Read<String>,
    wanted: Option<&str>,
    previous: &Mutex<Option<Read<String>>>,
) -> Probe {
    let matches = match (&now, wanted) {
        (Read::Unusable(why), _) => return Probe::Unusable { why: why.clone() },
        (Read::Value(v), Some(w)) => v == w,
        (Read::Absent, None) => true,
        _ => false,
    };
    if matches {
        return Probe::Ok { now: render(&now) };
    }
    let from = render(&now);
    if let Ok(mut slot) = previous.lock() {
        *slot = Some(now);
    }
    Probe::Change {
        from,
        to: wanted.map_or_else(|| "not set".to_owned(), |w| format!("{w:?}")),
    }
}

/// The three names `scutil` keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pref {
    Computer,
    LocalHost,
    Host,
}

impl Pref {
    const fn key(self) -> &'static str {
        match self {
            Self::Computer => "ComputerName",
            Self::LocalHost => "LocalHostName",
            Self::Host => "HostName",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Computer => "names.computer-name",
            Self::LocalHost => "names.local-host-name",
            Self::Host => "names.host-name",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Computer => "Computer Name",
            Self::LocalHost => "Local hostname (the .local Bonjour name)",
            Self::Host => "HostName (fully qualified)",
        }
    }
}

/// One `scutil` name.
struct Scutil {
    pref: Pref,
    /// `None` = not set.
    wanted: Option<String>,
    previous: Mutex<Option<Read<String>>>,
}

impl Scutil {
    const fn new(pref: Pref, wanted: Option<String>) -> Self {
        Self {
            pref,
            wanted,
            previous: Mutex::new(None),
        }
    }

    fn set(&self, value: &str) -> Cmd {
        Cmd::new(SCUTIL).args(["--set", self.pref.key(), value])
    }
}

impl Item for Scutil {
    fn id(&self) -> String {
        self.pref.id().to_owned()
    }

    fn family(&self) -> &'static str {
        "names"
    }

    fn title(&self) -> String {
        self.pref.title().to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let cmd = Cmd::new(SCUTIL).args(["--get", self.pref.key()]);
        let now = match sys.run(&cmd) {
            Ok(ran) => read_scutil(&ran),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        decide(now, self.wanted.as_deref(), &self.previous)
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        run_ok(sys, &self.set(self.wanted.as_deref().unwrap_or(""))).map(drop)
    }

    fn revert(&self) -> Option<String> {
        let previous = self.previous.lock().ok()?.clone()?;
        match previous {
            Read::Value(old) => Some(self.set(&old).display()),
            Read::Absent => Some(self.set("").display()),
            Read::Unusable(_) => None,
        }
    }
}

/// The SMB server's `NetBIOSName`.
struct NetBios {
    wanted: String,
    previous: Mutex<Option<Read<String>>>,
}

impl NetBios {
    const fn new(wanted: String) -> Self {
        Self {
            wanted,
            previous: Mutex::new(None),
        }
    }

    fn write(value: &str) -> Cmd {
        Cmd::new("/usr/bin/defaults").args(["write", SMB_SERVER, "NetBIOSName", "-string", value])
    }
}

/// What `defaults read <domain> <key>` read: `Absent` for the documented missing-key and
/// missing-domain answers (exit 1; 27.0 says `Could not find key` and `Domain … not found`,
/// older releases `does not exist`).
fn read_defaults_string(ran: &Ran) -> Read<String> {
    if ran.success() {
        return Read::Value(ran.out().trim_end_matches(['\n', '\r']).to_owned());
    }
    let err = ran.err();
    if ran.status == Some(1)
        && (err.contains("Could not find key")
            || err.contains("not found")
            || err.contains("does not exist"))
    {
        Read::Absent
    } else {
        Read::Unusable(ran.describe())
    }
}

impl Item for NetBios {
    fn id(&self) -> String {
        "names.netbios-name".to_owned()
    }

    fn family(&self) -> &'static str {
        "names"
    }

    fn title(&self) -> String {
        "SMB NetBIOS name (follows the Computer Name)".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let cmd = Cmd::new("/usr/bin/defaults").args(["read", SMB_SERVER, "NetBIOSName"]);
        let now = match sys.run(&cmd) {
            Ok(ran) => read_defaults_string(&ran),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        // NetBIOS names are case-insensitive, and macOS writes its own: VM (27.0), a
        // Computer Name change set by this family as `LAB-02` read `lab-02` a few minutes
        // later. The same name in another case is the name wanted.
        let now = match now {
            Read::Value(v) if v.eq_ignore_ascii_case(&self.wanted) => {
                Read::Value(self.wanted.clone())
            }
            other => other,
        };
        decide(now, Some(&self.wanted), &self.previous)
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        run_ok(sys, &Self::write(&self.wanted)).map(drop)
    }

    fn revert(&self) -> Option<String> {
        let previous = self.previous.lock().ok()?.clone()?;
        match previous {
            Read::Value(old) => Some(Self::write(&old).display()),
            Read::Absent => Some(
                Cmd::new("/usr/bin/defaults")
                    .args(["delete", SMB_SERVER, "NetBIOSName"])
                    .display(),
            ),
            Read::Unusable(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::Fake;

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

    fn table(computer: Option<&str>, local: Option<&str>, host: Option<&str>) -> Table {
        Table {
            computer_name: computer.map(Into::into),
            local_host_name: local.map(Into::into),
            host_name: host.map(Into::into),
        }
    }

    /// `scutil --get HostName` in the VM when it is unset: stderr, exit 1.
    fn not_set(key: &str) -> Ran {
        Ran::exit(1, &format!("{key}: not set"))
    }

    #[test]
    fn a_computer_name_brings_its_netbios_name_and_each_name_is_one_item() {
        let items = expand(
            &table(Some("lab-07"), Some("lab-07"), Some("lab-07.lab.example")),
            &ctx(),
        )
        .unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "names.computer-name",
                "names.netbios-name",
                "names.local-host-name",
                "names.host-name"
            ]
        );
    }

    #[test]
    fn a_placeholder_the_controller_did_not_fill_is_refused() {
        let err = expand(&table(Some("{name}"), None, None), &ctx())
            .err()
            .unwrap();
        assert!(err.contains("placeholder"), "{err}");
        let err = expand(&table(None, Some("lab-{user}"), None), &ctx())
            .err()
            .unwrap();
        assert!(err.contains("local_host_name"), "{err}");
    }

    #[test]
    fn a_local_hostname_scutil_would_refuse_is_refused_when_the_baseline_expands() {
        // `scutil --set LocalHostName 'bad name.x'` in the VM:
        // `SCPreferencesSetLocalHostName() failed: Invalid argument`.
        for bad in ["bad name.x", "lab.07", "-lab", "lab-", "", "l\u{e4}b"] {
            assert!(
                expand(&table(None, Some(bad), None), &ctx()).is_err(),
                "{bad:?}"
            );
        }
        assert!(expand(&table(None, Some("Lab-07"), None), &ctx()).is_ok());
        assert!(expand(&table(None, None, Some("lab_07.example")), &ctx()).is_err());
        assert!(expand(&table(Some(" "), None, None), &ctx()).is_err());
        // scutil would read these as its own options, as root.
        for bad in ["-f/etc/sudoers", "-d", "-"] {
            assert!(
                expand(&table(Some(bad), None, None), &ctx()).is_err(),
                "{bad:?}"
            );
        }
        assert!(expand(&table(Some("Lab 7 - Room B"), None, None), &ctx()).is_ok());
    }

    #[test]
    fn netbios_names_follow_the_local_hostname_rule_uppercased_to_fifteen_characters() {
        assert_eq!(netbios_name("lab-07").as_deref(), Some("LAB-07"));
        // The VM's own Computer Name, whose Local hostname macOS made `Manageds-Virtual-Machine`.
        assert_eq!(
            netbios_name("Managed\u{2019}s Virtual Machine").as_deref(),
            Some("MANAGEDS-VIRTUA")
        );
        assert_eq!(
            netbios_name("Lab 7 \u{2014} Room B").as_deref(),
            Some("LAB-7-ROOM-B")
        );
        assert_eq!(
            netbios_name("abcdefghijklmn-pq").as_deref(),
            Some("ABCDEFGHIJKLMN")
        );
        assert_eq!(netbios_name("\u{2014}"), None);
    }

    #[test]
    fn an_unset_hostname_is_absent_and_an_unanswered_get_is_unusable() {
        assert_eq!(read_scutil(&not_set("HostName")), Read::Absent);
        assert_eq!(
            read_scutil(&Ran::ok("Managed\u{2019}s Virtual Machine\n")),
            Read::Value("Managed\u{2019}s Virtual Machine".into())
        );
        assert!(matches!(
            read_scutil(&Ran::exit(1, "Permission denied.")),
            Read::Unusable(_)
        ));
    }

    #[test]
    fn renaming_the_vm_sets_each_name_as_root_and_reverts_to_what_it_was() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Managed\u{2019}s Virtual Machine\n"),
        )
        .on("/usr/sbin/scutil --get ComputerName", Ran::ok("lab-07\n"))
        .on("/usr/sbin/scutil --set ComputerName lab-07", Ran::ok(""))
        .on(
            "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
            Ran::ok("VIRTUALMAC-904D\n"),
        )
        .on(
            "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
            Ran::ok("LAB-07\n"),
        )
        .on(
            "/usr/bin/defaults write /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName -string LAB-07",
            Ran::ok(""),
        )
        .on("/usr/sbin/scutil --get HostName", not_set("HostName"))
        .on("/usr/sbin/scutil --get HostName", Ran::ok("lab-07.lab.example\n"))
        .on("/usr/sbin/scutil --set HostName lab-07.lab.example", Ran::ok(""));
        let items = expand(
            &table(Some("lab-07"), None, Some("lab-07.lab.example")),
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::Changed),
            "{rows:#?}"
        );
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/sbin/scutil --set ComputerName 'Managed\u{2019}s Virtual Machine'")
        );
        assert_eq!(
            rows[1].revert.as_deref(),
            Some(
                "/usr/bin/defaults write /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName -string VIRTUALMAC-904D"
            )
        );
        // An unset HostName reverts by unsetting it, as `scutil --set HostName ''` does.
        assert_eq!(
            rows[2].revert.as_deref(),
            Some("/usr/sbin/scutil --set HostName ''")
        );
        assert_eq!(rows[2].from.as_deref(), Some("not set"));
    }

    #[test]
    fn an_empty_host_name_unsets_it_and_an_unset_one_is_already_as_wanted() {
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get HostName", not_set("HostName"));
        let items = expand(&table(None, None, Some("")), &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(rows[0].to.as_deref(), Some("not set"));
    }

    #[test]
    fn a_name_that_did_not_take_is_a_failure_and_scutils_refusal_is_its_reason() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("Manageds-Virtual-Machine\n"),
        )
        .on(
            "/usr/sbin/scutil --set LocalHostName lab-07",
            Ran::exit(1, "Permission denied."),
        );
        let items = expand(&table(None, Some("lab-07"), None), &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("Permission denied.")
        );
    }

    #[test]
    fn a_netbios_name_macos_rewrote_in_lower_case_is_the_name_wanted() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
            Ran::ok("lab-02\n"),
        );
        let item = NetBios::new("LAB-02".into());
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Ok { .. }));
    }

    #[test]
    fn a_missing_netbios_name_is_absent_and_reverts_by_deleting_it() {
        // `defaults read …/com.apple.smb.server NoSuchKey` in the VM (27.0's wording).
        let missing = Ran::exit(
            1,
            "Error: Could not find key 'NetBIOSName' in domain 'SystemConfiguration/com.apple.smb.server'.",
        );
        assert_eq!(read_defaults_string(&missing), Read::Absent);
        let sys = Fake::root();
        sys.on(
            "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
            missing,
        );
        let item = NetBios::new("LAB-07".into());
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "not set".into(),
                to: "\"LAB-07\"".into()
            }
        );
        assert_eq!(
            item.revert().as_deref(),
            Some(
                "/usr/bin/defaults delete /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName"
            )
        );
    }

    #[test]
    fn a_name_that_could_not_be_read_is_unknown_never_the_default() {
        let items = expand(&table(Some("lab-07"), None, None), &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::Unknown),
            "{rows:#?}"
        );
        assert!(rows.iter().all(|r| r.revert.is_none()));
    }
}
