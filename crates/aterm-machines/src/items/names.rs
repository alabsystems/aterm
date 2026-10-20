// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Computer names: `scutil --set ComputerName|LocalHostName|HostName` and the SMB NetBIOSName (design §5.1).
//!
//! `scutil` does not set the name SMB advertises, so a Computer Name change also writes
//! `NetBIOSName` in `/Library/Preferences/SystemConfiguration/com.apple.smb.server`, derived
//! the way macOS derives a Local hostname (apostrophes dropped, other punctuation and spaces
//! as hyphens), uppercased and cut to NetBIOS's 15 characters.
//!
//! A name may hold `{hardware}`, which the agent fills from its own facts
//! ([`crate::hardware::describe`]): spaced in the Computer Name (`m10 MacBook Pro M4 Max
//! 36GB 1TB`), in label form in the Local hostname (`m10-MacBook-Pro-M4-Max-36GB-1TB`),
//! dropping whole parts from the right (the disk first) when the label would pass 63
//! characters. A Mac whose hardware could not be read keeps its names: each such item is
//! unknown, never a name with a part missing.
//!
//! The Local hostname is the whole name in label form, so a tag or inventory name with `.` or
//! `_` (which `--name` allows) gives one `scutil` takes: `lab_03.b` is `lab-03-b` there and
//! `lab_03.b` in the Computer Name, as written. The controller derives the same name
//! ([`planned_local_host_name`]) to find a Mac whose rename's report it never got.
//!
//! Measured in the macOS 27.0 VM, as root over SSH: `scutil --get HostName` answers
//! `HostName: not set` on standard error with exit 1 when it is unset, which is the
//! documented "not set" ([`Read::Absent`]); `scutil --set HostName ''` unsets it again;
//! `scutil --set LocalHostName 'bad name.x'` fails `SCPreferencesSetLocalHostName() failed:
//! Invalid argument`, and so do `lab_03` and `lab.03` while `lab-03-b` is taken, which is
//! why the Local hostname is put in label form, and checked, when the baseline expands.
//!
//! macOS writes its own NetBIOSName about five seconds after a name changes and again at
//! every restart, from the Local hostname: `lab-03` when it fits, a generated
//! `VIRTUALMAC-F72E` when it does not (VM 27.0, renamed to
//! `lab-03-Virtual-Machine-M5-8GB-84GB`), over whatever `com.apple.smb.server` held. It
//! keeps a name a network service's `SMB` settings carry (System Settings ▸ Network ▸
//! Details ▸ WINS), so the NetBIOS item writes it there for every service as well: in the
//! VM, a name kept that way held through a rename and a restart, and one written only to
//! `com.apple.smb.server` was replaced by both.

use super::{Ctx, Item};
use crate::hardware::{self, Description, PLACEHOLDER};
use crate::model::{PValue, Probe, Read};
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::sync::Mutex;

/// `scutil`.
pub const SCUTIL: &str = "/usr/sbin/scutil";

/// The SMB server's preferences (a `defaults` domain given as a path).
pub const SMB_SERVER: &str = "/Library/Preferences/SystemConfiguration/com.apple.smb.server";

/// SystemConfiguration's preferences, whose network services' `SMB` settings may name the
/// NetBIOS name macOS keeps.
pub const SC_PREFERENCES: &str = "/Library/Preferences/SystemConfiguration/preferences.plist";

/// NetBIOS names are at most 15 characters.
pub const NETBIOS_MAX: usize = 15;

/// A DNS label, and so a Local hostname, is at most 63 characters.
pub const LABEL_MAX: usize = 63;

/// The `[names]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `ComputerName`; `{name}` and `{tag}` fill from the inventory, `{hardware}` from the
    /// Mac. Also sets the SMB NetBIOSName.
    pub computer_name: Option<String>,
    /// `LocalHostName`, the Bonjour `.local` name: letters, digits and hyphens, any other
    /// character a hyphen as macOS makes a Local hostname of a Computer Name (an inventory
    /// name `lab_03.b` is `lab-03-b`; `{hardware}` fills in that form).
    pub local_host_name: Option<String>,
    /// `HostName`, a fully qualified name; `""` unsets it, and leaving it out leaves it alone.
    pub host_name: Option<String>,
}

/// The items `table` asks for, `{hardware}` filled from `ctx.facts`.
///
/// # Errors
/// One line naming the name that is still a template or is not a name macOS accepts.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let hardware = hardware::describe(&ctx.facts);
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(template) = &table.computer_name {
        let template = &said_once(template, &hardware);
        match with_hardware(template, &hardware, |d| {
            template.replace(PLACEHOLDER, &d.spaced())
        }) {
            Err(why) => {
                filled("computer_name", &template.replace(PLACEHOLDER, ""))?;
                items.push(Box::new(Scutil::new(
                    Pref::Computer,
                    Wanted::Unknown(why.clone()),
                )));
                items.push(Box::new(NetBios::new(Err(why))));
            }
            Ok(name) => {
                check_computer_name(&name)?;
                items.push(Box::new(Scutil::new(
                    Pref::Computer,
                    Wanted::Value(name.clone()),
                )));
                if let Some(netbios) = netbios_name(&name) {
                    items.push(Box::new(NetBios::new(Ok(netbios))));
                }
            }
        }
    }
    if let Some(template) = &table.local_host_name {
        let wanted = local_wanted(template, &hardware)?;
        items.push(Box::new(Scutil::new(Pref::LocalHost, wanted)));
    }
    if let Some(name) = &table.host_name {
        filled("host_name", name)?;
        if name.is_empty() {
            items.push(Box::new(Scutil::new(Pref::Host, Wanted::Unset)));
        } else if name.len() <= 253 && name.split('.').all(dns_label) {
            items.push(Box::new(Scutil::new(
                Pref::Host,
                Wanted::Value(name.clone()),
            )));
        } else {
            return Err(format!(
                "[names] host_name {name:?} must be a dotted host name (labels of letters, \
                 digits and hyphens), or \"\" to unset it"
            ));
        }
    }
    Ok(items)
}

/// The Local hostname `template` asks for on a Mac described by `hardware`: the name in the
/// form macOS derives a Local hostname from a Computer Name in ([`hardware::label_form`]: an
/// inventory name `lab_03.b` gives `lab-03-b`, which `scutil` takes where it refuses the
/// name as written), `{hardware}` filled by as many of the Mac's parts as fit a DNS label
/// ([`fit_label`]), the description said once ([`said_once`]). Unknown when it holds
/// `{hardware}` and the hardware could not be read.
///
/// # Errors
/// One line when a placeholder is left or nothing of the name is left for a label.
fn local_wanted(template: &str, hardware: &Result<Description, String>) -> Result<Wanted, String> {
    let template = said_once(template, hardware);
    filled("local_host_name", &template.replace(PLACEHOLDER, ""))?;
    let labels = match (template.contains(PLACEHOLDER), hardware) {
        (false, _) => Vec::new(),
        (true, Ok(d)) => d.labels(),
        (true, Err(why)) => {
            return Ok(Wanted::Unknown(format!(
                "{PLACEHOLDER} is not known: {why}"
            )));
        }
    };
    let name = fit_label(&template, &labels);
    if !dns_label(&name) {
        return Err(format!(
            "[names] local_host_name {template:?} gives {name:?}, not a `.local` name: 1 to \
             63 letters, digits and hyphens (other characters become hyphens, as macOS makes \
             a Local hostname of a Computer Name)"
        ));
    }
    Ok(Wanted::Value(name))
}

/// The Local hostname `template` (a baseline's `[names] local_host_name`, the controller's
/// placeholders filled) gives the Mac `hardware` describes, exactly as the agent's run sets
/// it: the controller finds a Mac its baseline renamed at that `.local` name when the run's
/// report never came back. `None` when `template` holds `{hardware}` and no description is
/// known, or gives no name.
#[must_use]
pub fn planned_local_host_name(template: &str, hardware: Option<&Description>) -> Option<String> {
    let hardware = hardware.cloned().ok_or_else(String::new);
    match local_wanted(template, &hardware) {
        Ok(Wanted::Value(name)) => Some(name),
        _ => None,
    }
}

/// `template` as `fill` fills it when it holds `{hardware}`, else as it is; the reason
/// the hardware is unknown when it holds `{hardware}` and that could not be read.
fn with_hardware(
    template: &str,
    hardware: &Result<Description, String>,
    fill: impl FnOnce(&Description) -> String,
) -> Result<String, String> {
    if !template.contains(PLACEHOLDER) {
        return Ok(template.to_owned());
    }
    hardware
        .as_ref()
        .map(fill)
        .map_err(|why| format!("{PLACEHOLDER} is not known: {why}"))
}

/// `template` without the description of this Mac it already gives just before
/// `{hardware}`, so the hardware is said once. A name with no letters-then-digits tag is
/// its own `{tag}`, so once a Mac is named `lab-03-Virtual-Machine-M5-8GB-90GB`, `here`
/// (which names the Mac by its Local hostname) and a re-enrolled Mac (named by it) fill
/// `{tag}-{hardware}` with that whole name: without this each apply would add the
/// hardware again. Only words that are, in label form and case aside, this Mac's own
/// description (or the first parts of it, as a name cut to 63 characters keeps them) are
/// taken off, from the right, as often as they repeat, and never the whole name before
/// `{hardware}`: `Andrews-MacBook-Pro-{hardware}` on a MacBook Pro is
/// `Andrews-MacBook-Pro-M4-Max-36GB-1TB`, and `lab-Mac-mini-1` keeps its words, so two
/// names that differ still differ once filled.
fn said_once(template: &str, hardware: &Result<Description, String>) -> String {
    let (Ok(d), Some((before, after))) = (hardware, template.split_once(PLACEHOLDER)) else {
        return template.to_owned();
    };
    let labels = d.labels();
    let name_end = before
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .len();
    let (mut name, separator) = before.split_at(name_end);
    while let Some(shorter) = without_description(name, &labels) {
        name = shorter;
    }
    format!("{name}{separator}{PLACEHOLDER}{after}")
}

/// `name` without its last words when they are, in label form and case aside, the first
/// parts of `labels` (the most parts first), as long as a word (in any script) is left
/// before them.
fn without_description<'a>(name: &'a str, labels: &[String]) -> Option<&'a str> {
    for parts in (1..=labels.len()).rev() {
        let said = labels[..parts].join("-");
        for (at, c) in name.char_indices() {
            let starts_word = c.is_alphanumeric()
                && name[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|p| !p.is_alphanumeric());
            if !starts_word {
                continue;
            }
            let head = name[..at].trim_end_matches(|c: char| !c.is_alphanumeric());
            if !head.is_empty() && hardware::label_form(&name[at..]).eq_ignore_ascii_case(&said) {
                return Some(head);
            }
        }
    }
    None
}

/// A Computer Name macOS takes and `scutil` cannot read as its own option.
fn check_computer_name(name: &str) -> Result<(), String> {
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
    Ok(())
}

/// `template` with `{hardware}` filled by as many of `labels` as keep it within a DNS
/// label's 63 characters, whole parts dropped from the right (the disk, then the memory,
/// then the chip), never what the template says around it, all of it in label form
/// ([`hardware::label_form`]: runs of hyphens one, none at either end, so `{tag}-{hardware}`
/// with no part left is the tag). A template with no `{hardware}` is its label form.
fn fit_label(template: &str, labels: &[String]) -> String {
    let mut n = labels.len();
    loop {
        let name = hardware::label_form(&template.replace(PLACEHOLDER, &labels[..n].join("-")));
        if name.len() <= LABEL_MAX || n == 0 {
            return name;
        }
        n -= 1;
    }
}

/// The short handle an inventory name starts with: its leading letters-then-digits word
/// (`m10` of `m10-MacBook-M4-Max-36GB-1TB`, `lab07`), or the whole name when it starts with
/// no such word (`lab-07`). The controller fills `{tag}` with it, and `enroll` names a Mac
/// by its Local hostname's.
#[must_use]
pub fn tag(name: &str) -> &str {
    let end = name
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(name.len());
    let word = &name[..end];
    let letters = word.bytes().take_while(u8::is_ascii_alphabetic).count();
    let digits = word
        .bytes()
        .skip(letters)
        .take_while(u8::is_ascii_digit)
        .count();
    if letters > 0 && digits > 0 && letters + digits == word.len() {
        word
    } else {
        name
    }
}

/// The NetBIOS name for `computer_name`: the Local hostname macOS would derive from it
/// ([`hardware::label_form`]), uppercased, at most 15 characters. `None` when nothing is left.
#[must_use]
pub fn netbios_name(computer_name: &str) -> Option<String> {
    let mut out = hardware::label_form(computer_name).to_ascii_uppercase();
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
             {{name}}, {{tag}}, {{serial}} and {{uuid}}, and the Mac {{hardware}}"
        ));
    }
    Ok(())
}

/// One DNS label: 1 to 63 ASCII letters, digits and hyphens, no hyphen at either end.
#[must_use]
pub fn dns_label(label: &str) -> bool {
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
            Self::Computer => "Computer Name, shown in Finder, AirDrop and Sharing",
            Self::LocalHost => "Local hostname, the network name before .local",
            Self::Host => {
                "host name `hostname`, shell prompts and ssh show (the Local hostname when \
                 not set)"
            }
        }
    }
}

/// What a name should be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wanted {
    /// This name.
    Value(String),
    /// Not set.
    Unset,
    /// Not known: the template holds `{hardware}` and the Mac's hardware could not be read
    /// (why). The item probes unusable and changes nothing.
    Unknown(String),
}

/// One `scutil` name.
struct Scutil {
    pref: Pref,
    wanted: Wanted,
    previous: Mutex<Option<Read<String>>>,
}

impl Scutil {
    const fn new(pref: Pref, wanted: Wanted) -> Self {
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
        let wanted = match &self.wanted {
            Wanted::Value(v) => Some(v.as_str()),
            Wanted::Unset => None,
            Wanted::Unknown(why) => return Probe::Unusable { why: why.clone() },
        };
        let cmd = Cmd::new(SCUTIL).args(["--get", self.pref.key()]);
        let now = match sys.run(&cmd) {
            Ok(ran) => read_scutil(&ran),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        decide(now, wanted, &self.previous)
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        match &self.wanted {
            Wanted::Value(v) => run_ok(sys, &self.set(v)).map(drop),
            Wanted::Unset => run_ok(sys, &self.set("")).map(drop),
            Wanted::Unknown(why) => Err(why.clone()),
        }
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

/// Each network service's id and the NetBIOS name its `SMB` settings keep (`None`: none).
type Pins = Vec<(String, Option<String>)>;

/// `plutil -extract NetworkServices xml1 -o - <SC_PREFERENCES>`: the network services and
/// their settings (12 ms on the MacBook Air).
#[must_use]
pub fn services_cmd() -> Cmd {
    Cmd::new("/usr/bin/plutil").args([
        "-extract",
        "NetworkServices",
        "xml1",
        "-o",
        "-",
        SC_PREFERENCES,
    ])
}

/// The NetBIOS name each network service keeps, from [`services_cmd`]'s XML.
///
/// # Errors
/// One line when it is not a dictionary of services keyed by UUID (the ids go into a
/// `scutil --prefs` script).
pub fn parse_services(xml: &[u8]) -> Result<Pins, String> {
    let PValue::Dict(services) = crate::plist::parse(xml)? else {
        return Err("NetworkServices is not a dictionary".to_owned());
    };
    let field = |dict: &PValue, key: &str| match dict {
        PValue::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    services
        .into_iter()
        .map(|(id, service)| {
            if !crate::agent::platform::uuid_shaped(&id) {
                return Err(format!("network service id {id:?} is not a UUID"));
            }
            let pinned = field(&service, "SMB")
                .and_then(|smb| field(&smb, "NetBIOSName"))
                .and_then(|name| match name {
                    PValue::Str(s) => Some(s),
                    _ => None,
                });
            Ok((id, pinned))
        })
        .collect()
}

/// The `scutil --prefs` commands that set (`Some`) or remove (`None`) `NetBIOSName` in
/// each network service's `SMB` settings, keeping the rest of them: an `SMB` entity a
/// service lacks reads as `No such key` and leaves the empty dictionary `d.init` made
/// (VM 27.0). `None` when a name could not be written into the script as one word.
#[must_use]
pub fn pin_script<'a>(
    pins: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<String> {
    let word = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let mut script = String::from("open\nlock\n");
    for (id, name) in pins {
        if !crate::agent::platform::uuid_shaped(id) {
            return None;
        }
        let path = format!("/NetworkServices/{id}/SMB");
        let change = match name {
            Some(name) if word(name) => format!("d.add NetBIOSName {name}"),
            Some(_) => return None,
            None => "d.remove NetBIOSName".to_owned(),
        };
        script.push_str(&format!("d.init\nget {path}\n{change}\nset {path}\n"));
    }
    script.push_str("commit\napply\nunlock\nclose\nquit\n");
    Some(script)
}

/// The SMB server's `NetBIOSName`, kept across restarts and renames by naming it in each
/// network service's `SMB` settings, as System Settings ▸ Network ▸ Details ▸ WINS does.
///
/// Without that macOS puts back its own name (VM 27.0, measured): about five seconds after
/// a Computer Name or Local hostname change, and at every restart, `smb-sync-preferences`
/// writes the Local hostname when it fits in 15 characters and a generated
/// `VIRTUALMAC-F72E` when it does not, over whatever `com.apple.smb.server` held. Named in
/// a service's `SMB` settings, the name held through a rename and a restart. The item
/// writes both: the services, so macOS keeps it, and `com.apple.smb.server`, so SMB
/// answers by it now rather than five seconds later.
struct NetBios {
    /// The name, or why it is not known (the Computer Name it follows is not).
    wanted: Result<String, String>,
    previous: Mutex<Option<Read<String>>>,
    previous_pins: Mutex<Option<Pins>>,
}

impl NetBios {
    const fn new(wanted: Result<String, String>) -> Self {
        Self {
            wanted,
            previous: Mutex::new(None),
            previous_pins: Mutex::new(None),
        }
    }

    fn write(value: &str) -> Cmd {
        Cmd::new("/usr/bin/defaults").args(["write", SMB_SERVER, "NetBIOSName", "-string", value])
    }

    /// The network services and the name each keeps.
    fn pins(sys: &dyn System) -> Result<Pins, String> {
        parse_services(&run_ok(sys, &services_cmd())?.stdout)
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
        "Windows network name, the Computer Name cut to 15 characters".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let wanted = match &self.wanted {
            Ok(w) => w,
            Err(why) => return Probe::Unusable { why: why.clone() },
        };
        let cmd = Cmd::new("/usr/bin/defaults").args(["read", SMB_SERVER, "NetBIOSName"]);
        let now = match sys.run(&cmd) {
            Ok(ran) => read_defaults_string(&ran),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        // NetBIOS names are case-insensitive, and macOS writes its own: VM (27.0), a
        // Computer Name change set by this family as `LAB-02` read `lab-02` a few minutes
        // later. The same name in another case is the name wanted.
        let now = match now {
            Read::Value(v) if v.eq_ignore_ascii_case(wanted) => Read::Value(wanted.clone()),
            Read::Unusable(why) => return Probe::Unusable { why },
            other => other,
        };
        let pins = match Self::pins(sys) {
            Ok(pins) => pins,
            Err(why) => return Probe::Unusable { why },
        };
        let kept = pins
            .iter()
            .all(|(_, p)| p.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(wanted)));
        let live = now == Read::Value(wanted.clone());
        if live && kept {
            return Probe::Ok { now: render(&now) };
        }
        if let Ok(mut slot) = self.previous_pins.lock() {
            *slot = Some(pins);
        }
        let from = if live {
            format!(
                "{} until a restart (no network service keeps it)",
                render(&now)
            )
        } else {
            render(&now)
        };
        if let Ok(mut slot) = self.previous.lock() {
            *slot = Some(now);
        }
        Probe::Change {
            from,
            to: format!("{wanted:?}"),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let wanted = self.wanted.as_ref().map_err(Clone::clone)?;
        let pins = Self::pins(sys)?;
        if !pins.is_empty() {
            let script = pin_script(
                pins.iter()
                    .map(|(id, _)| (id.as_str(), Some(wanted.as_str()))),
            )
            .ok_or_else(|| format!("{wanted:?} cannot be written as a NetBIOS name"))?;
            let staged = crate::port::staging_file(sys, script.as_bytes())
                .map_err(|e| format!("staging scutil --prefs: {e}"))?;
            run_ok(
                sys,
                &Cmd::new(SCUTIL).arg("--prefs").stdin_file(staged.path()),
            )?;
        }
        run_ok(sys, &Self::write(wanted)).map(drop)
    }

    fn revert(&self) -> Option<String> {
        let previous = self.previous.lock().ok()?.clone()?;
        let live = match previous {
            Read::Value(old) => Self::write(&old).display(),
            Read::Absent => Cmd::new("/usr/bin/defaults")
                .args(["delete", SMB_SERVER, "NetBIOSName"])
                .display(),
            Read::Unusable(_) => return None,
        };
        let pins = self.previous_pins.lock().ok()?.clone().unwrap_or_default();
        let wanted = self.wanted.as_ref().ok()?;
        if pins
            .iter()
            .all(|(_, p)| p.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(wanted)))
        {
            return Some(live);
        }
        let script = pin_script(pins.iter().map(|(id, p)| (id.as_str(), p.as_deref())))?;
        Some(format!(
            "{live} && printf '{}' | {SCUTIL} --prefs",
            script.replace('\n', "\\n")
        ))
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

    /// `plutil -extract NetworkServices xml1 -o - …/preferences.plist` in the VM (27.0): three
    /// services, none keeping a NetBIOS name; and the same once its Ethernet service kept
    /// `LAB-03-SVCPIN` (set with `scutil --prefs`, the name then held through a rename and a
    /// restart).
    const SERVICES: &str = include_str!("../../fixtures/sc/vm-27.0-network-services.plist");
    const SERVICES_PINNED: &str =
        include_str!("../../fixtures/sc/vm-27.0-network-services-pinned.plist");
    const ETHERNET: &str = "364A8646-D38B-4922-B8A2-DD9542E83781";
    const SERVICE_IDS: [&str; 3] = [
        ETHERNET,
        "4B8335D7-375E-423C-823B-D06921276474",
        "6D5E0C47-9620-4E2D-B844-6B696CF4ED58",
    ];

    /// The VM's services, each keeping `name`.
    fn services_keeping(name: &str) -> String {
        SERVICES.replace("<key>SMB</key>\n\t\t<dict/>", &format!(
            "<key>SMB</key>\n\t\t<dict>\n\t\t\t<key>NetBIOSName</key>\n\t\t\t<string>{name}</string>\n\t\t</dict>"
        ))
    }

    fn services_read() -> String {
        services_cmd().display()
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

    /// The Local hostname `local` asks for, as its item would set it.
    fn local_of(local: &str, facts: HostFacts) -> Result<String, String> {
        let items = expand(&table(None, Some(local), None), &on(facts.clone()))?;
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"));
        match items[0].probe(&sys, &on(facts)) {
            Probe::Change { to, .. } => Ok(to.trim_matches('"').to_owned()),
            other => Err(format!("{other:?}")),
        }
    }

    #[test]
    fn a_local_hostname_is_the_label_macos_would_make_of_it_and_never_one_scutil_refuses() {
        // In the VM (27.0) `scutil --set LocalHostName` refused `bad name.x`, `lab_03` and
        // `lab.03` (`SCPreferencesSetLocalHostName() failed: Invalid argument`) and took
        // `lab-03-b`. A name `--name` allows is set in the form macOS makes of it.
        for (asked, set) in [
            ("bad name.x", "bad-name-x"),
            ("lab_03", "lab-03"),
            ("lab.03", "lab-03"),
            ("lab_03.b-{hardware}", "lab-03-b-MacBook-Air-M5-32GB-4TB"),
            ("-lab", "lab"),
            ("lab-", "lab"),
            ("Lab-07", "Lab-07"),
        ] {
            assert_eq!(local_of(asked, air()).as_deref(), Ok(set), "{asked:?}");
        }
        // Nothing left for a label, or more than one label holds: refused.
        let long = "a".repeat(64);
        for bad in ["", "._.", long.as_str()] {
            assert!(
                expand(&table(None, Some(bad), None), &ctx()).is_err(),
                "{bad:?}"
            );
        }
        // The Computer Name keeps the name as written; its NetBIOS name is in label form.
        let items = expand(&described("lab_03.b"), &on(air())).unwrap();
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"))
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"))
            .on(NETBIOS_READ, Ran::ok("X\n"))
            .on(&services_read(), Ran::ok(SERVICES));
        let to: Vec<String> = items
            .iter()
            .map(|i| match i.probe(&sys, &on(air())) {
                Probe::Change { to, .. } => to,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            to,
            [
                "\"lab_03.b MacBook Air M5 32GB 4TB\"",
                "\"LAB-03-B-MACBOO\"",
                "\"lab-03-b-MacBook-Air-M5-32GB-4TB\""
            ]
        );
        // The controller's planned name for the same template is the one the run sets.
        let air = crate::hardware::describe(&air()).unwrap();
        assert_eq!(
            planned_local_host_name("lab_03.b-{hardware}", Some(&air)).as_deref(),
            Some("lab-03-b-MacBook-Air-M5-32GB-4TB")
        );
        assert_eq!(planned_local_host_name("lab_03.b-{hardware}", None), None);
        assert_eq!(
            planned_local_host_name("lab_03", None).as_deref(),
            Some("lab-03")
        );
        assert_eq!(
            planned_local_host_name("{tga}-{hardware}", Some(&air)),
            None
        );
    }

    #[test]
    fn a_host_name_or_computer_name_scutil_would_misread_is_refused_when_the_baseline_expands() {
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
        // The services as probed, as apply reads them, and as read back.
        .on(&services_read(), Ran::ok(SERVICES))
        .on(&services_read(), Ran::ok(SERVICES))
        .on(&services_read(), Ran::ok(&services_keeping("LAB-07")))
        .on("/usr/sbin/scutil --prefs", Ran::ok(""))
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
        // The NetBIOS name is written where macOS keeps it, and its revert takes it off
        // each service again.
        assert!(sys.calls().contains(&"/usr/sbin/scutil --prefs".to_owned()));
        let netbios = rows[1].revert.as_deref().unwrap();
        assert!(
            netbios.starts_with(
                "/usr/bin/defaults write /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName -string VIRTUALMAC-904D && printf 'open\\nlock\\n"
            ) && netbios.ends_with("' | /usr/sbin/scutil --prefs"),
            "{netbios}"
        );
        for id in SERVICE_IDS {
            assert!(
                netbios.contains(&format!(
                    "get /NetworkServices/{id}/SMB\\nd.remove NetBIOSName\\n"
                )),
                "{netbios}"
            );
        }
        // An unset HostName reverts by unsetting it, as `scutil --set HostName ''` does.
        assert_eq!(
            rows[2].revert.as_deref(),
            Some("/usr/sbin/scutil --set HostName ''")
        );
        assert_eq!(rows[2].from.as_deref(), Some("not set"));
    }

    #[test]
    fn the_starter_baseline_takes_a_hand_set_host_name_off_with_the_rename() {
        // The rehearsal's lab Mac (VM 27.0): its old name set by hand in all three places,
        // `sudo scutil --set HostName m201-Air-M2-24GB-1TB-2` among them. A rename that left
        // HostName alone left `hostname`, shell prompts and ssh at the old name.
        let text = crate::baseline::fill(
            crate::controller::templates::DEFAULT_BASELINE,
            &[("name", "m201"), ("tag", "m201")],
        )
        .unwrap();
        let names = crate::baseline::parse(&text).unwrap().names.unwrap();
        let host: Vec<Box<dyn Item>> = expand(&names, &ctx())
            .unwrap()
            .into_iter()
            .filter(|i| i.id() == "names.host-name")
            .collect();
        assert_eq!(host.len(), 1, "the starter [names] leaves HostName alone");
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get HostName",
            Ran::ok("m201-Air-M2-24GB-1TB-2\n"),
        )
        .on("/usr/sbin/scutil --get HostName", not_set("HostName"))
        .on("/usr/sbin/scutil --set HostName ''", Ran::ok(""));
        let rows = engine::run(&host, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert!(
            sys.calls()
                .contains(&"/usr/sbin/scutil --set HostName ''".to_owned())
        );
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
        )
        .on(&services_read(), Ran::ok(&services_keeping("lab-02")));
        let item = NetBios::new(Ok("LAB-02".into()));
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
        )
        .on(&services_read(), Ran::ok(&services_keeping("LAB-07")));
        let item = NetBios::new(Ok("LAB-07".into()));
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "not set".into(),
                to: "\"LAB-07\"".into()
            }
        );
        // Every service already keeps it: only the live name is put back.
        assert_eq!(
            item.revert().as_deref(),
            Some(
                "/usr/bin/defaults delete /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName"
            )
        );
    }

    #[test]
    fn a_netbios_name_no_network_service_keeps_is_a_change_macos_would_undo_at_restart() {
        // VM 27.0: written to com.apple.smb.server alone, `LAB-03-THIRD-LO` read
        // `VIRTUALMAC-F72E` after a restart, and so did a name written five seconds before
        // a rename; kept in the Ethernet service's SMB settings, it held through both.
        let sys = Fake::root();
        sys.on(NETBIOS_READ, Ran::ok("LAB-03\n"))
            .on(&services_read(), Ran::ok(SERVICES));
        let item = NetBios::new(Ok("LAB-03".into()));
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "\"LAB-03\" until a restart (no network service keeps it)".into(),
                to: "\"LAB-03\"".into()
            }
        );
        // One service of three keeping it is not enough: another may be the primary.
        let sys = Fake::root();
        sys.on(NETBIOS_READ, Ran::ok("LAB-03-SVCPIN\n"))
            .on(&services_read(), Ran::ok(SERVICES_PINNED));
        assert_eq!(
            parse_services(SERVICES_PINNED.as_bytes()).unwrap()[0],
            (ETHERNET.to_owned(), Some("LAB-03-SVCPIN".to_owned()))
        );
        let item = NetBios::new(Ok("LAB-03-SVCPIN".into()));
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Change { .. }));
        // Unreadable services, or an unreadable name, leave it unknown, never a guess.
        let sys = Fake::root();
        sys.on(NETBIOS_READ, Ran::ok("LAB-03\n"));
        assert!(matches!(
            NetBios::new(Ok("LAB-03".into())).probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
        let sys = Fake::root();
        sys.on(NETBIOS_READ, Ran::exit(2, "defaults: Permission denied"))
            .on(&services_read(), Ran::ok(SERVICES));
        assert!(matches!(
            NetBios::new(Ok("LAB-03".into())).probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
    }

    #[test]
    fn the_netbios_name_is_kept_in_each_services_smb_settings_with_the_rest_of_them() {
        let ids: Vec<(String, Option<String>)> = parse_services(SERVICES.as_bytes()).unwrap();
        assert_eq!(
            ids.iter()
                .map(|(id, p)| (id.as_str(), p.clone()))
                .collect::<Vec<_>>(),
            SERVICE_IDS.map(|id| (id, None))
        );
        // The commands that pinned LAB-03-SVCPIN in the VM, for every service.
        let script = pin_script(SERVICE_IDS.map(|id| (id, Some("M14-MACBOOK-AIR")))).unwrap();
        let mut want = String::from("open\nlock\n");
        for id in SERVICE_IDS {
            want.push_str(&format!(
                "d.init\nget /NetworkServices/{id}/SMB\nd.add NetBIOSName M14-MACBOOK-AIR\nset /NetworkServices/{id}/SMB\n"
            ));
        }
        want.push_str("commit\napply\nunlock\nclose\nquit\n");
        assert_eq!(script, want);
        // Nothing that is not one plain word goes into the script.
        assert_eq!(pin_script([(ETHERNET, Some("A B"))]), None);
        assert_eq!(pin_script([("x\nquit", Some("A"))]), None);
        assert!(
            parse_services(b"<dict><key>x\nset /</key><dict/></dict>")
                .unwrap_err()
                .contains("not a UUID")
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

    const NETBIOS_READ: &str = "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName";

    /// The MacBook Air's facts as `facts::gather` reads them (Mac17,4, macOS 27.0.1:
    /// product-name "MacBook Air (15-inch, M5)", brand "Apple M5", hw.memsize 34359738368,
    /// its SSD 4002222325760 bytes in ioreg).
    fn air() -> HostFacts {
        HostFacts {
            model_name: "MacBook Air".into(),
            chip: "Apple M5".into(),
            memory_bytes: 34_359_738_368,
            disk_bytes: 4_002_222_325_760,
            ..HostFacts::default()
        }
    }

    fn on(facts: HostFacts) -> Ctx {
        Ctx { facts, ..ctx() }
    }

    /// `[names]` as the default baseline has it, `{tag}` filled by the controller.
    fn described(tag: &str) -> Table {
        table(
            Some(&format!("{tag} {{hardware}}")),
            Some(&format!("{tag}-{{hardware}}")),
            None,
        )
    }

    #[test]
    fn the_air_is_named_for_what_it_is_and_the_plan_shows_each_rename() {
        // The MacBook Air's names, read with scutil and defaults (macOS's own NetBIOS name).
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("m14 Air M5 32GB 4TB prime\n"),
        )
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("m14-Air-M5-32GB-4TB-prime\n"),
        )
        .on(NETBIOS_READ, Ran::ok("m14AirM4TBprime\n"))
        .on(&services_read(), Ran::ok(SERVICES));
        let items = expand(&described("m14"), &on(air())).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &on(air()), &mut NoJournal);
        let changes: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                assert_eq!(r.outcome, Outcome::Planned, "{r:#?}");
                (
                    r.id.as_str(),
                    r.from.as_deref().unwrap(),
                    r.to.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            changes,
            [
                (
                    "names.computer-name",
                    "\"m14 Air M5 32GB 4TB prime\"",
                    "\"m14 MacBook Air M5 32GB 4TB\""
                ),
                (
                    "names.netbios-name",
                    "\"m14AirM4TBprime\"",
                    "\"M14-MACBOOK-AIR\""
                ),
                (
                    "names.local-host-name",
                    "\"m14-Air-M5-32GB-4TB-prime\"",
                    "\"m14-MacBook-Air-M5-32GB-4TB\""
                ),
            ]
        );
        assert!(!sys.calls().iter().any(|c| c.contains("--set")));
    }

    #[test]
    fn bonjours_conflict_suffix_and_stale_specs_are_renamed_to_what_the_mac_reads() {
        // m1-Air-M2-24GB-1TB-2: macOS's "-2", added on a Bonjour conflict, is not the owner's.
        let m1 = HostFacts {
            model_name: "MacBook Air".into(),
            chip: "Apple M2".into(),
            memory_bytes: 25_769_803_776,
            disk_bytes: 1_000_555_581_440,
            ..HostFacts::default()
        };
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("m1-Air-M2-24GB-1TB-2\n"),
        )
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("m1-MacBook-Air-M2-24GB-1TB\n"),
        )
        .on(
            "/usr/sbin/scutil --set LocalHostName m1-MacBook-Air-M2-24GB-1TB",
            Ran::ok(""),
        );
        let items = expand(&table(None, Some("m1-{hardware}"), None), &on(m1.clone())).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &on(m1), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/sbin/scutil --set LocalHostName m1-Air-M2-24GB-1TB-2")
        );
        // An Intel Mac: its speed's point becomes a hyphen in the label, as macOS makes it.
        let m4 = HostFacts {
            model_name: "MacBook Pro".into(),
            chip: "Intel Core i7".into(),
            processor_speed: "3.1 GHz".into(),
            memory_bytes: 17_179_869_184,
            disk_bytes: 1_000_555_581_440,
            ..HostFacts::default()
        };
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("m4 Macbook 3.1GHz i7 16GB 1TB\n"),
        )
        .on(NETBIOS_READ, Ran::ok("M4-MACBOOK-3-1G\n"))
        .on(&services_read(), Ran::ok(SERVICES))
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("m4-Macbook-31GHz-i7-16GB-1TB\n"),
        );
        let items = expand(&described("m4"), &on(m4.clone())).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &on(m4), &mut NoJournal);
        let to: Vec<&str> = rows.iter().map(|r| r.to.as_deref().unwrap()).collect();
        assert_eq!(
            to,
            [
                "\"m4 MacBook Pro Intel Core i7 3.1GHz 16GB 1TB\"",
                "\"M4-MACBOOK-PRO\"",
                "\"m4-MacBook-Pro-Intel-Core-i7-3-1GHz-16GB-1TB\""
            ]
        );
    }

    #[test]
    fn a_mac_whose_hardware_could_not_be_read_keeps_its_names_and_is_unknown() {
        // No facts at all: nothing is read, nothing is set, and each row says why.
        let sys = Fake::root();
        let mut facts = air();
        facts.memory_bytes = 0;
        let items = expand(&described("m14"), &on(facts.clone())).unwrap();
        assert_eq!(items.len(), 3);
        let rows = engine::run(&items, Mode::Apply, &sys, &on(facts), &mut NoJournal);
        for r in &rows {
            assert_eq!(r.outcome, Outcome::Unknown, "{r:#?}");
            assert!(r.why.as_deref().unwrap().contains("hw.memsize"), "{r:#?}");
        }
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        // A placeholder that is still a typo is refused even then.
        let err = expand(&table(None, Some("{tga}-{hardware}"), None), &ctx())
            .err()
            .unwrap();
        assert!(err.contains("placeholder"), "{err}");
    }

    #[test]
    fn a_local_hostname_too_long_for_a_label_drops_hardware_from_the_right_never_the_tag() {
        // Labels: MacBook-Pro, M4-Max, 36GB, 1TB (11 + 6 + 4 + 3, a hyphen between).
        let m10 = HostFacts {
            model_name: "MacBook Pro".into(),
            chip: "Apple M4 Max".into(),
            memory_bytes: 38_654_705_664,
            disk_bytes: 1_000_555_581_440,
            ..HostFacts::default()
        };
        let local = |tag: &str| {
            let items = expand(
                &table(None, Some(&format!("{tag}-{{hardware}}")), None),
                &on(m10.clone()),
            )?;
            let sys = Fake::root();
            sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"));
            match items[0].probe(&sys, &on(m10.clone())) {
                Probe::Change { to, .. } => Ok(to.trim_matches('"').to_owned()),
                other => Err(format!("{other:?}")),
            }
        };
        assert_eq!(local("m10").unwrap(), "m10-MacBook-Pro-M4-Max-36GB-1TB");
        let tag40 = "classroom-b-bench-twelve-left-by-the-door";
        assert_eq!(tag40.len(), 41);
        // 41 + 27 = 68: the disk goes, then the memory (63 at most).
        assert_eq!(local(tag40).unwrap(), format!("{tag40}-MacBook-Pro-M4-Max"));
        let tag60 = "a".repeat(60);
        assert_eq!(local(&tag60).unwrap(), tag60);
        assert!(local(&"a".repeat(64)).is_err());
    }

    #[test]
    fn the_tag_is_the_leading_letters_and_digits_or_the_whole_name() {
        for (name, tagged) in [
            ("m10-MacBook-M4-Max-36GB-1TB", "m10"),
            ("m1-Air-M2-24GB-1TB-2", "m1"),
            ("m14-Air-M5-32GB-4TB-prime", "m14"),
            ("m100-macbook-M4-Pro-48GB-500GB", "m100"),
            ("m10", "m10"),
            ("lab07", "lab07"),
            ("lab-07", "lab-07"),
            ("spark-4d0f-2", "spark-4d0f-2"),
            ("m10abc-x", "m10abc-x"),
            ("10-x", "10-x"),
            ("Manageds-Virtual-Machine", "Manageds-Virtual-Machine"),
            ("", ""),
        ] {
            assert_eq!(tag(name), tagged, "{name}");
        }
    }

    /// The Computer Name and Local hostname `[names]` asks for on a Mac with `facts`, its
    /// `{tag}` filled with `tag` as the controller (or `here`) fills it.
    fn names_for(tag: &str, facts: &HostFacts) -> (String, String) {
        let items = expand(&described(tag), &on(facts.clone())).unwrap();
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"))
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"))
            .on(NETBIOS_READ, Ran::ok("X\n"))
            .on(&services_read(), Ran::ok(SERVICES));
        let to: Vec<String> = items
            .iter()
            .filter(|i| i.id() != "names.netbios-name")
            .map(|i| match i.probe(&sys, &on(facts.clone())) {
                Probe::Change { to, .. } => to.trim_matches('"').to_owned(),
                other => panic!("{other:?}"),
            })
            .collect();
        (to[0].clone(), to[1].clone())
    }

    #[test]
    fn a_name_that_already_describes_the_mac_is_not_described_again() {
        // The VM's facts (27.0: VirtualMac2,1, "Apple M5 (Virtual)", 8 GiB, a 90.0 GB disk).
        let vm = HostFacts {
            model_name: "Virtual Machine".into(),
            chip: "Apple M5 (Virtual)".into(),
            memory_bytes: 8_589_934_592,
            disk_bytes: 90_000_003_072,
            ..HostFacts::default()
        };
        let m10 = HostFacts {
            model_name: "MacBook Pro".into(),
            chip: "Apple M4 Max".into(),
            memory_bytes: 38_654_705_664,
            disk_bytes: 1_000_555_581_440,
            ..HostFacts::default()
        };
        let named = (
            "lab-03 Virtual Machine M5 8GB 90GB".to_owned(),
            "lab-03-Virtual-Machine-M5-8GB-90GB".to_owned(),
        );
        // lab-03 has no letters-then-digits tag, so once renamed, `here` (and a re-enrolled
        // Mac) fill {tag} with the whole new name: the second apply changes nothing.
        assert_eq!(names_for("lab-03", &vm), named);
        assert_eq!(names_for(&named.1, &vm), named);
        // A name that said it twice already, or in another case, is said once.
        assert_eq!(
            names_for(&format!("{}-Virtual-Machine-M5", named.1), &vm),
            named
        );
        assert_eq!(
            names_for("LAB-03-virtual-machine-m5-8gb-90gb", &vm).1,
            "LAB-03-Virtual-Machine-M5-8GB-90GB"
        );
        // A name cut to 63 characters keeps what it kept.
        let cut = "classroom-b-bench-twelve-left-by-the-door-MacBook-Pro-M4-Max";
        assert_eq!(names_for(cut, &m10).1, cut);
        // macOS's own Local hostname names the model once.
        assert_eq!(
            names_for("Andrews-MacBook-Pro", &m10),
            (
                "Andrews MacBook Pro M4 Max 36GB 1TB".to_owned(),
                "Andrews-MacBook-Pro-M4-Max-36GB-1TB".to_owned()
            )
        );
        // Words that are not this Mac's description stay, so names that differ still differ;
        // a name that is nothing but the description keeps it as its tag.
        assert_eq!(
            names_for("lab-MacBook-Pro-1", &m10).1,
            "lab-MacBook-Pro-1-MacBook-Pro-M4-Max-36GB-1TB"
        );
        assert_eq!(
            names_for("MacBook-Pro", &m10).1,
            "MacBook-Pro-MacBook-Pro-M4-Max-36GB-1TB"
        );
        assert_eq!(names_for("m10", &m10).1, "m10-MacBook-Pro-M4-Max-36GB-1TB");
        // A Computer Name in another script keeps its words; one that is punctuation and
        // the description alone keeps all of it.
        let computer = |template: &str| {
            let t = table(Some(template), None, None);
            let item = &expand(&t, &on(m10.clone())).unwrap()[0];
            let sys = Fake::root();
            sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"));
            match item.probe(&sys, &on(m10.clone())) {
                Probe::Change { to, .. } => to,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            computer("\u{7814}\u{7a76}\u{5ba4} MacBook Pro {hardware}"),
            "\"\u{7814}\u{7a76}\u{5ba4} MacBook Pro M4 Max 36GB 1TB\""
        );
        assert_eq!(
            computer("(MacBook Pro) {hardware}"),
            "\"(MacBook Pro) MacBook Pro M4 Max 36GB 1TB\""
        );
    }
}
