// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the agent learns about its host before any item runs (design §4.2, §9).

use crate::hardware;
use crate::model::{Form, HostFacts, Keyboard, User};
use crate::port::{Cmd, MachineReading, Ran, System};
use aterm_capacity::host_vitals::HostVitals;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The system TCC database.
pub const SYSTEM_TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// `hidutil list`'s keyboard-conformance match, including composite secondary collections.
pub const KEYBOARD_MATCHING: &str = crate::port::keyboard_usage::MATCHING;

/// The lowest uid macOS gives an account a person logs in to.
pub const FIRST_PERSON_UID: i64 = 501;

/// The agent's root-owned artifact cache (design §2): `<AGENT_ROOT>/cache`.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    Path::new(crate::AGENT_ROOT).join("cache")
}

/// Gather the host's facts, with the agent's own cache listed for `cached`.
#[must_use]
pub fn gather(sys: &dyn System) -> HostFacts {
    gather_with_cache(sys, &default_cache_dir())
}

/// Gather the host's facts without inventorying live keyboards.
///
/// Callers that do not expand a keyboard baseline use this form: a live HID
/// search can wait on the device or Bluetooth stack and must not be part of
/// an unrelated hello, status or uninstall path. [`inventory_keyboards`]
/// remains the explicit read for a keyboard-specific run.
#[must_use]
pub fn gather_without_keyboards(sys: &dyn System) -> HostFacts {
    gather_with_cache_without_keyboards(sys, &default_cache_dir())
}

/// Gather the host's facts, listing `cache_dir` for `cached`.
#[must_use]
pub fn gather_with_cache(sys: &dyn System, cache_dir: &Path) -> HostFacts {
    gather_with_cache_inner(sys, cache_dir, true)
}

/// [`gather_with_cache`] without the live keyboard inventory.
#[must_use]
pub fn gather_with_cache_without_keyboards(sys: &dyn System, cache_dir: &Path) -> HostFacts {
    gather_with_cache_inner(sys, cache_dir, false)
}

fn gather_with_cache_inner(
    sys: &dyn System,
    cache_dir: &Path,
    include_keyboards: bool,
) -> HostFacts {
    let accounts = run(sys, &dscl(["-list", "/Users", "UniqueID"]))
        .filter(Ran::success)
        .map(|r| parse_dscl_list(&r.out()))
        .unwrap_or_default();
    let model = line(sys, &Cmd::new("/usr/sbin/sysctl").args(["-n", "hw.model"]));
    let platform = run(
        sys,
        &Cmd::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]),
    )
    .filter(Ran::success)
    .map(|r| r.out())
    .unwrap_or_default();
    let product_name = run(sys, &hardware::product_cmd())
        .filter(Ran::success)
        .and_then(|r| hardware::parse_product_name(&r.out()));
    let (chip, processor_speed) = run(sys, &hardware::brand_cmd())
        .filter(Ran::success)
        .and_then(|r| hardware::parse_brand(&r.out()))
        .unwrap_or_default();
    let disk = run(sys, &hardware::disk_cmd())
        .filter(Ran::success)
        .and_then(|r| hardware::internal_disk(&r.out()))
        .unwrap_or_default();
    let tcc_open = remote_full_disk_access(sys);
    HostFacts {
        computer_name: scutil_get(sys, "ComputerName"),
        local_host_name: scutil_get(sys, "LocalHostName"),
        uuid: ioreg_string(&platform, "IOPlatformUUID").unwrap_or_default(),
        serial: ioreg_string(&platform, "IOPlatformSerialNumber").unwrap_or_default(),
        form: form(sys, &model),
        model_name: hardware::model_name(&model, product_name.as_deref()).unwrap_or_default(),
        chip,
        processor_speed,
        memory_bytes: run(sys, &hardware::memsize_cmd())
            .filter(Ran::success)
            .and_then(|r| hardware::parse_memsize(&r.out()))
            .unwrap_or(0),
        disk_bytes: disk.bytes,
        disk_rotational: disk.rotational,
        model,
        arch: line(sys, &Cmd::new("/usr/bin/uname").arg("-m")),
        os_version: line(sys, &Cmd::new("/usr/bin/sw_vers").arg("-productVersion")),
        os_build: line(sys, &Cmd::new("/usr/bin/sw_vers").arg("-buildVersion")),
        filevault: filevault(sys),
        boot_time: run(
            sys,
            &Cmd::new("/usr/sbin/sysctl").args(["-n", "kern.boottime"]),
        )
        .filter(Ran::success)
        .and_then(|r| parse_boottime(&r.out()))
        .unwrap_or(0),
        idle_secs: run(sys, &idle_cmd())
            .filter(Ran::success)
            .and_then(|r| parse_idle_ns(&r.out()))
            .map(|ns| ns / 1_000_000_000),
        remote_full_disk_access: tcc_open,
        full_disk_access: sys.full_disk_access().or(tcc_open),
        console_user: console_user(sys, &accounts),
        users: people(sys, &accounts),
        keyboards: if include_keyboards {
            inventory_keyboards(sys)
        } else {
            Vec::new()
        },
        cached: cached(sys, cache_dir),
        agent_version: aterm_stamp::app_version().to_owned(),
    }
}

/// The local account `name`, with its admin membership.
///
/// # Errors
/// One line when the account does not exist, or it or the `admin` group cannot be read.
pub fn user(sys: &dyn System, name: &str) -> Result<User, String> {
    if !plain_account_name(name) {
        return Err(format!("{name:?} is not an account name"));
    }
    let admins = admin_members(sys)?;
    read_user(sys, name, &admins)
}

/// The current console account alone, without host names, hardware, preferences or privacy probes.
pub(crate) fn console_account(sys: &dyn System) -> Result<User, String> {
    let accounts = run(sys, &dscl(["-list", "/Users", "UniqueID"]))
        .filter(Ran::success)
        .map(|ran| parse_dscl_list(&ran.out()))
        .ok_or("the console account list could not be read")?;
    let name = console_user(sys, &accounts).ok_or("no account is at this Mac's console")?;
    let user = user(sys, &name)?;
    let console = sys
        .stat(Path::new("/dev/console"))
        .map_err(|why| why.to_string())?;
    if user.uid == 0 || console.uid != user.uid {
        return Err("the console account changed while it was read".into());
    }
    Ok(user)
}

/// The keyboard-capable services returned by [`hidutil_keyboards`], deduplicated by model.
#[must_use]
pub fn parse_hidutil_keyboards(text: &str) -> Vec<Keyboard> {
    hidutil_keyboards_from(text, false)
        .map(|census| census.keyboards)
        .unwrap_or_default()
}

/// A service census used to establish every local keyboard obligation.
pub(crate) fn parse_hidutil_keyboards_complete(text: &str) -> Result<KeyboardCensus, String> {
    hidutil_keyboards_from(text, true)
}

/// HID identities actually observed by one census or mapping read.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyboardCoverage(BTreeSet<u64>);

impl KeyboardCoverage {
    pub(crate) fn of(ids: impl IntoIterator<Item = u64>) -> Self {
        Self(ids.into_iter().collect())
    }

    /// Include another item's actual observations from this pass.
    pub(crate) fn include(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Every requested identity was resolved, including intentional per-device holds.
    pub(crate) fn covers(&self, registries: &[u64]) -> bool {
        registries.iter().all(|registry| self.0.contains(registry))
    }

    pub(crate) fn covers_all(&self, expected: &Self) -> bool {
        expected.0.is_subset(&self.0)
    }
}

/// Each model's native services, bound by one recognized service census.
#[derive(Debug, Default)]
pub(crate) struct KeyboardCohorts(BTreeMap<(u32, u32), KeyboardCoverage>);

impl KeyboardCohorts {
    #[cfg(test)]
    pub(crate) const EMPTY: Self = Self(BTreeMap::new());

    pub(crate) fn expected(&self, vendor: u32, product: u32) -> Result<KeyboardCoverage, String> {
        self.0
            .get(&(vendor, product))
            .cloned()
            .ok_or_else(|| "the attached keyboard model has no native service assignment".into())
    }
}

/// Models and identities from the same complete `hidutil` service census.
pub(crate) struct KeyboardCensus {
    keyboards: Vec<Keyboard>,
    services: BTreeMap<u64, (u32, u32)>,
}

impl KeyboardCensus {
    /// A model list cannot establish readiness for a native service it did not observe.
    #[cfg(test)]
    pub(crate) fn into_keyboards(self, native: &[u64]) -> Result<Vec<Keyboard>, String> {
        self.into_parts(Some(native))
            .map(|(keyboards, _)| keyboards)
    }

    /// Assign only the captured native cohort; attachment reads defer to their requested IDs.
    pub(crate) fn into_parts(
        self,
        native: Option<&[u64]>,
    ) -> Result<(Vec<Keyboard>, KeyboardCohorts), String> {
        let mut cohorts = KeyboardCohorts(
            self.keyboards
                .iter()
                .map(|keyboard| {
                    (
                        (keyboard.vendor, keyboard.product),
                        KeyboardCoverage::default(),
                    )
                })
                .collect(),
        );
        if let Some(native) = native {
            let mut assigned = BTreeSet::new();
            for id in native {
                let model = self.services.get(id).ok_or_else(|| {
                    "the keyboard model census did not observe every native keyboard service"
                        .to_owned()
                })?;
                if !assigned.insert(*id) {
                    return Err("the native keyboard census repeats a service identity".into());
                }
                cohorts
                    .0
                    .get_mut(model)
                    .ok_or("the native keyboard model is absent")?
                    .0
                    .insert(*id);
            }
        }
        Ok((self.keyboards, cohorts))
    }
}

fn hidutil_keyboards_from(text: &str, complete: bool) -> Result<KeyboardCensus, String> {
    let invalid =
        || "hidutil did not return a complete, recognized keyboard service list".to_owned();
    let sectioned = text
        .lines()
        .any(|l| l.trim_end() == "Services:" || l.trim_end() == "Devices:");
    let mut in_services = !sectioned;
    let mut columns: Option<Vec<(String, usize)>> = None;
    let mut out: Vec<Keyboard> = Vec::new();
    let mut service_models = BTreeMap::new();
    let mut services = false;
    for row in text.lines() {
        let trimmed = row.trim_end();
        if sectioned && !row.starts_with(' ') && trimmed.ends_with(':') {
            if complete {
                match trimmed {
                    "Services:" if !services => services = true,
                    "Devices:" => {}
                    _ => return Err(invalid()),
                }
            }
            in_services = trimmed == "Services:";
            columns = None;
            continue;
        }
        if !in_services || trimmed.trim().is_empty() {
            continue;
        }
        let Some(cols) = &columns else {
            let header = header_columns(row);
            if complete
                && ["VendorID", "ProductID", "RegistryID", "Product", "Built-In"]
                    .iter()
                    .any(|required| {
                        header
                            .iter()
                            .filter(|(name, _)| name.as_str() == *required)
                            .count()
                            != 1
                    })
            {
                return Err(invalid());
            }
            columns = Some(header);
            continue;
        };
        let field = |name: &str| column(row, cols, name);
        let (Some(vendor), Some(product)) = (
            field("VendorID").and_then(|v| parse_hex(&v)),
            field("ProductID").and_then(|v| parse_hex(&v)),
        ) else {
            if complete {
                return Err(invalid());
            }
            continue;
        };
        if complete {
            let registry = field("RegistryID")
                .and_then(|value| value.strip_prefix("0x").map(str::to_owned))
                .filter(|value| !value.starts_with('+'))
                .and_then(|value| u64::from_str_radix(&value, 16).ok())
                .ok_or_else(invalid)?;
            if service_models.insert(registry, (vendor, product)).is_some() {
                return Err(invalid());
            }
        }
        let builtin = if complete {
            match trimmed.split_whitespace().last() {
                Some("1") => true,
                Some("0") => false,
                _ => return Err(invalid()),
            }
        } else {
            trimmed.split_whitespace().last() == Some("1")
        };
        let name = field("Product").unwrap_or_default();
        match out
            .iter_mut()
            .find(|k| k.vendor == vendor && k.product == product)
        {
            Some(seen) => seen.builtin |= builtin,
            None => out.push(Keyboard {
                vendor,
                product,
                name,
                builtin,
            }),
        }
    }
    if complete && !services {
        Err(invalid())
    } else {
        Ok(KeyboardCensus {
            keyboards: out,
            services: service_models,
        })
    }
}

/// Inventory the keyboards macOS currently routes keys through.
///
/// This is an explicit live-device read, reserved for runs that expand a
/// keyboard baseline or explicitly request the complete host facts.
#[must_use]
pub fn inventory_keyboards(sys: &dyn System) -> Vec<Keyboard> {
    run(sys, &hidutil_keyboards())
        .filter(Ran::success)
        .map(|r| parse_hidutil_keyboards(&r.out()))
        .unwrap_or_default()
}

/// The `hidutil list` run that names the keyboards.
#[must_use]
pub fn hidutil_keyboards() -> Cmd {
    Cmd::new("/usr/bin/hidutil").args(["list", "--matching", KEYBOARD_MATCHING])
}

/// Each `(name, uid)` row of `dscl . -list /Users UniqueID`.
#[must_use]
pub fn parse_dscl_list(text: &str) -> Vec<(String, i64)> {
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?;
            let uid = crate::parse_int(words.next()?)?;
            words.next().is_none().then(|| (name.to_owned(), uid))
        })
        .collect()
}

/// The attributes of one `dscl . -read <record> <attr>…` answer, in its order.
#[must_use]
pub fn parse_dscl_read(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in text.lines() {
        if l.starts_with("No such key: ") {
            continue;
        }
        if let Some(rest) = l.strip_prefix(' ') {
            if let Some((_, value)) = out.last_mut() {
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(rest);
            }
            continue;
        }
        if let Some((key, value)) = l.split_once(':')
            && !key.is_empty()
            && !key.contains(' ')
        {
            out.push((key.to_owned(), value.trim_start().to_owned()));
        }
    }
    out
}

/// Unix seconds from `sysctl -n kern.boottime` (`{ sec = 1792190170, usec = 411172 } …`).
#[must_use]
pub fn parse_boottime(text: &str) -> Option<u64> {
    let after = text.split_once("sec =")?.1;
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// `ioreg -r -d 1 -k HIDIdleTime -c IOHIDSystem`: the time since the last key, click or scroll on
/// this Mac, whichever app it went to ([`parse_idle_ns`]). The host facts' idle time and a
/// step's keyboard rest (`System::session_reach`) read it alike.
pub(crate) fn idle_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args(["-r", "-d", "1", "-k", "HIDIdleTime", "-c", "IOHIDSystem"])
}

/// Nanoseconds since the last input, from the first `"HIDIdleTime" = <n>` of `ioreg`.
#[must_use]
pub fn parse_idle_ns(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (_, value) = l.split_once("\"HIDIdleTime\" = ")?;
        crate::parse_uint(value.trim())
    })
}

/// The quoted string value of `key` in `ioreg -rd1 -c IOPlatformExpertDevice`
/// (`"IOPlatformUUID" = "C339…"`).
#[must_use]
pub fn ioreg_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = \"");
    text.lines().find_map(|l| {
        let (_, rest) = l.split_once(&needle)?;
        let value = rest.strip_suffix('"')?;
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Run `cmd`, `None` when it did not run to completion.
fn run(sys: &dyn System, cmd: &Cmd) -> Option<Ran> {
    sys.run(cmd).ok()
}

/// The first line of `cmd`'s output when it exited 0, else empty.
fn line(sys: &dyn System, cmd: &Cmd) -> String {
    run(sys, cmd)
        .filter(Ran::success)
        .map(|r| r.out().lines().next().unwrap_or("").trim().to_owned())
        .unwrap_or_default()
}

/// `scutil --get <pref>` with only its line ending removed (a Computer Name may hold
/// spaces and curly apostrophes); empty when it is not set or could not be read.
fn scutil_get(sys: &dyn System, pref: &str) -> String {
    run(sys, &Cmd::new("/usr/sbin/scutil").args(["--get", pref]))
        .filter(Ran::success)
        .map(|r| r.out().trim_end_matches(['\n', '\r']).to_owned())
        .unwrap_or_default()
}

fn dscl<const N: usize>(args: [&str; N]) -> Cmd {
    Cmd::new("/usr/bin/dscl").arg(".").args(args)
}

/// A VM by its model; otherwise a laptop when `pmset -g batt` names an internal battery,
/// a desktop when it answers without one.
fn form(sys: &dyn System, model: &str) -> Form {
    if model.starts_with("VirtualMac") {
        return Form::Virtual;
    }
    match run(sys, &Cmd::new("/usr/bin/pmset").args(["-g", "batt"])) {
        Some(r) if r.success() && r.out().contains("InternalBattery") => Form::Laptop,
        Some(r) if r.success() && r.out().contains("Now drawing from") => Form::Desktop,
        _ => Form::Unknown,
    }
}

/// `fdesetup isactive`: exit 0 and `true` is on, exit 1 and `false` is off; anything else
/// is not an answer.
fn filevault(sys: &dyn System) -> Option<bool> {
    let ran = run(sys, &Cmd::new("/usr/bin/fdesetup").arg("isactive"))?;
    match (ran.status, ran.out().trim()) {
        (Some(0), "true") => Some(true),
        (Some(1), "false") => Some(false),
        _ => None,
    }
}

/// Whether the system TCC database opens for reading: `Some(false)` only on `EPERM`
/// (TCC's refusal), `None` on any other failure.
fn remote_full_disk_access(sys: &dyn System) -> Option<bool> {
    match sys.read_file(Path::new(SYSTEM_TCC_DB)) {
        Ok(_) => Some(true),
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => Some(false),
        Err(_) => None,
    }
}

/// What `console_user` holds when `/dev/console` could not be read: someone may be at the
/// Mac.
pub const CONSOLE_UNKNOWN: &str = "(unknown)";

/// The owner of `/dev/console` by name; none at the login window (root owns it there).
fn console_user(sys: &dyn System, accounts: &[(String, i64)]) -> Option<String> {
    let Ok(stat) = sys.stat(Path::new("/dev/console")) else {
        return Some(CONSOLE_UNKNOWN.to_owned());
    };
    if stat.uid == 0 {
        return None;
    }
    Some(
        accounts
            .iter()
            .find(|(_, uid)| *uid == i64::from(stat.uid))
            .map_or_else(|| format!("(uid {})", stat.uid), |(name, _)| name.clone()),
    )
}

/// The people's accounts (uid ≥ 501, no `_` service prefix), each resolved.
fn people(sys: &dyn System, accounts: &[(String, i64)]) -> Vec<User> {
    let Ok(admins) = admin_members(sys) else {
        return Vec::new();
    };
    accounts
        .iter()
        .filter(|(name, uid)| *uid >= FIRST_PERSON_UID && !name.starts_with('_'))
        .filter_map(|(name, _)| read_user(sys, name, &admins).ok())
        .collect()
}

/// The short names in `dscl . -read /Groups/admin GroupMembership`.
fn admin_members(sys: &dyn System) -> Result<Vec<String>, String> {
    let cmd = dscl(["-read", "/Groups/admin", "GroupMembership"]);
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(parse_dscl_read(&ran.out())
        .into_iter()
        .find(|(k, _)| k == "GroupMembership")
        .map(|(_, v)| v.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default())
}

fn read_user(sys: &dyn System, name: &str, admins: &[String]) -> Result<User, String> {
    let record = format!("/Users/{name}");
    let cmd = dscl([
        "-read",
        record.as_str(),
        "UniqueID",
        "PrimaryGroupID",
        "NFSHomeDirectory",
        "UserShell",
    ]);
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        if ran.err().contains("eDSRecordNotFound") {
            return Err(format!("no account {name}"));
        }
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    let attrs = parse_dscl_read(&ran.out());
    let get = |key: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let number = |key: &str| {
        get(key)
            .and_then(|v| crate::parse_uint::<u32>(v.trim()))
            .ok_or_else(|| format!("account {name}: no {key} in {}", cmd.display()))
    };
    let uid = number("UniqueID")?;
    let gid = number("PrimaryGroupID")?;
    let home = get("NFSHomeDirectory")
        .filter(|h| h.starts_with('/'))
        .ok_or_else(|| format!("account {name}: no NFSHomeDirectory"))?
        .to_owned();
    Ok(User {
        name: name.to_owned(),
        uid,
        gid,
        home,
        shell: get("UserShell").unwrap_or("").to_owned(),
        admin: gid == 80 || admins.iter().any(|a| a == name),
    })
}

/// A short name `dscl` can be asked about without the name changing the record path.
fn plain_account_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The artifacts the cache holds: its `<sha256 hex>` names (temporaries are skipped).
fn cached(sys: &dyn System, cache_dir: &Path) -> Vec<String> {
    sys.list_dir(cache_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.len() == 64 && n.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .collect()
}

/// Each header word and the byte where its column starts.
fn header_columns(header: &str) -> Vec<(String, usize)> {
    let mut cols = Vec::new();
    let mut start = None;
    for (i, c) in header.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                cols.push((header[s..i].to_owned(), s));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        cols.push((header[s..].to_owned(), s));
    }
    cols
}

/// The text of column `name` in `row`, trimmed; `None` when the header has no such column
/// or the row stops before it.
fn column(row: &str, cols: &[(String, usize)], name: &str) -> Option<String> {
    let at = cols.iter().position(|(n, _)| n == name)?;
    let start = floor_boundary(row, cols[at].1);
    let end = cols
        .get(at + 1)
        .map_or(row.len(), |(_, s)| floor_boundary(row, *s));
    let text = row.get(start..end.max(start))?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `at`, moved back to a character boundary of `s` (a product name may be UTF-8), and
/// never past its end.
fn floor_boundary(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.strip_prefix("0x")?;
    (!hex.starts_with('+'))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// This host's vitals ([`HostVitals`]) as `hello` carries them: one look at the machine through
/// the system's reading ([`System::machine_reading`]), its volume the one holding this account's
/// home, mapped by [`vitals_of`], with the uptime since `facts`' boot; `None` where the system
/// reads no machine.
#[must_use]
pub fn gather_vitals(sys: &dyn System, facts: &HostFacts) -> Option<HostVitals> {
    let home = sys.account_home().filter(|home| home.is_absolute());
    let reading = sys.machine_reading(home.as_deref())?;
    let uptime_secs = (facts.boot_time > 0)
        .then(|| sys.now().saturating_sub(facts.boot_time))
        .filter(|u| *u > 0);
    Some(vitals_of(&reading, uptime_secs))
}

/// The vitals one look at the machine `r` gives, `uptime_secs` beside them: each figure from the
/// one reading that owns it, `None` where that reading is absent.
/// - The CPU layout's: the logical cores.
/// - The load averages': the 1-, 5- and 15-minute load.
/// - The memory classes': total, available ([`aterm_sysprobe::MemoryDetail::available`], what
///   admission counts too), used ([`aterm_sysprobe::MemoryDetail::used`]), wired, compressed,
///   swap used and the pressure word.
/// - The GPU's: how busy it is, its name and core count.
/// - The home volume's: its size and free bytes (`f_bavail`).
#[must_use]
pub fn vitals_of(r: &MachineReading, uptime_secs: Option<u64>) -> HostVitals {
    let memory = r.memory.as_ref();
    let gpu = r.gpu.as_ref();
    let volume = r.volume.as_ref();
    HostVitals {
        cpu_cores: r.cores.filter(|n| *n > 0),
        load_avg_100: r.load.map(|l| l.map(hundredths)),
        gpu_name: gpu
            .and_then(|g| g.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        gpu_cores: gpu.and_then(|g| g.cores).filter(|n| *n > 0).map(u32::from),
        gpu_busy_pct: gpu.and_then(|g| g.device_pct).map(|p| p.min(100)),
        mem_total_bytes: memory.map(|m| m.total).filter(|t| *t > 0),
        mem_available_bytes: memory.map(|m| m.available),
        mem_used_bytes: memory.map(aterm_sysprobe::MemoryDetail::used),
        mem_wired_bytes: memory.map(|m| m.wired),
        mem_compressed_bytes: memory.and_then(|m| m.compressed),
        swap_used_bytes: memory.map(|m| m.swap_used),
        mem_pressure: memory
            .and_then(|m| m.pressure)
            .map(|p| p.as_str().to_owned()),
        disk_total_bytes: volume.map(|v| v.total),
        disk_free_bytes: volume.map(|v| v.free),
        uptime_secs,
        rtt_us: None,
    }
}

/// A load average in hundredths (`1.42` reads 142), rounded; a negative or non-finite one
/// (which [`aterm_sysprobe::load_average`] never gives) reads 0.
fn hundredths(load: f64) -> u32 {
    let scaled = (load * 100.0).round();
    if scaled.is_finite() && scaled > 0.0 {
        scaled.min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
