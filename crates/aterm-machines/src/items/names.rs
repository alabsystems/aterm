// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Computer names (`ComputerName`, `LocalHostName`, `HostName`, and SMB `NetBIOSName`)
//! ([`crate::hardware::describe`], [`planned_local_host_name`], [`Read::Absent`]).

use super::{Ctx, Item};
use crate::hardware::{self, Description, PLACEHOLDER};
use crate::model::{PValue, Probe, Read};
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::sync::Mutex;

/// `scutil`.
pub const SCUTIL: &str = aterm_uds::os_path!("scutil");

/// The SMB server's preferences (a `defaults` domain given as a path).
pub const SMB_SERVER: &str = "/Library/Preferences/SystemConfiguration/com.apple.smb.server";

/// SystemConfiguration's preferences.
pub const SC_PREFERENCES: &str = "/Library/Preferences/SystemConfiguration/preferences.plist";

/// NetBIOS names are at most 15 characters.
pub const NETBIOS_MAX: usize = 15;

/// A DNS label, and so a Local hostname, is at most 63 characters.
pub const LABEL_MAX: usize = 63;

/// The `[names]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `ComputerName`; `{name}` and `{tag}` fill from the inventory, `{hardware}` from the Mac.
    pub computer_name: Option<String>,
    /// `LocalHostName`, the Bonjour `.local` name.
    pub local_host_name: Option<String>,
    /// `HostName`, a fully qualified name.
    pub host_name: Option<String>,
    /// `true` puts the rule in place that lets the console account set the three names to a
    /// swarm label later, with no password ([`crate::root_steps::NameRule`]).
    pub admitted_name_rule: Option<bool>,
}

/// The items `table` asks for, `{hardware}` filled from `ctx.facts`.
///
/// # Errors
/// One line naming the name that is still a template or is not a name macOS accepts.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    expand_with_smb(table, ctx, true)
}

/// Setup's generated naming authority covers the three system names and its naming rule,
/// without the general baseline's implicit SMB preference changes.
pub(crate) fn expand_system_names(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    expand_with_smb(table, ctx, false)
}

fn expand_with_smb(table: &Table, ctx: &Ctx, smb: bool) -> Result<Vec<Box<dyn Item>>, String> {
    let hardware = hardware::describe(&ctx.facts);
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if table.admitted_name_rule == Some(true) {
        items.push(Box::new(crate::root_steps::NameRule::Current));
    }
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
                if smb {
                    items.push(Box::new(NetBios::new(Err(why))));
                }
            }
            Ok(name) => {
                check_computer_name(&name)?;
                items.push(Box::new(Scutil::new(
                    Pref::Computer,
                    Wanted::Value(name.clone()),
                )));
                if smb && let Some(netbios) = netbios_name(&name) {
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

/// The Local hostname `template` asks for on a Mac described by `hardware`.
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

/// The Local hostname `template` resolves to with `{hardware}` expanded.
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
/// `{hardware}`, so the hardware is said once.
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
/// with no part left is the tag).
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

/// The short handle an inventory name starts with.
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

/// The NetBIOS name for `computer_name`.
#[must_use]
pub fn netbios_name(computer_name: &str) -> Option<String> {
    let mut out = hardware::label_form(computer_name).to_ascii_uppercase();
    out.truncate(NETBIOS_MAX);
    let out = out.trim_end_matches('-').to_owned();
    (!out.is_empty()).then_some(out)
}

/// What `scutil --get <pref>` read.
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
    /// Not known: the template holds `{hardware}` and the Mac's hardware could not be read (why).
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
        if let (Some(expected), Read::Value(observed)) = (wanted, &now)
            && aterm_types::machine_name::OsSlot::from_macos_preference(self.pref.key())
                .is_some_and(|slot| slot.collision(expected, observed))
        {
            return Probe::Blocked {
                why: format!(
                    "{} reports {observed} instead of {expected}; a name collision remains, so setup leaves that alias in place",
                    self.pref.title(),
                ),
            };
        }
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

/// `plutil -extract NetworkServices xml1 -o - <SC_PREFERENCES>`.
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
/// One line when it is not a dictionary of services keyed by UUID.
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
            if !crate::hardware::uuid_shaped(&id) {
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

/// The `scutil --prefs` commands that set (`Some`) or remove (`None`) `NetBIOSName`.
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
        if !crate::hardware::uuid_shaped(id) {
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
#[path = "names_tests.rs"]
mod tests;
