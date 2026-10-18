// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the agent learns about its host before any item runs (design §4.2, §9).
//!
//! Every fact comes from one of macOS's own tools (or one `lstat`/`open`), through the
//! [`System`] port. A fact a tool would not give is left empty (`""`, `0`, `None`, an empty
//! list): the report then says "unknown" rather than a guess. The controller treats an
//! empty `uuid` as a host it cannot bind an envelope to.
//!
//! The parsers are separate functions over the tools' text, so the fixtures in the tests
//! are the tools' real answers, captured in a macOS 27.0 VM and on a 27.0.1 laptop.

use crate::model::{Form, HostFacts, Keyboard, User};
use crate::port::{Cmd, Ran, System};
use std::path::{Path, PathBuf};

/// The system TCC database. Opening it for reading is the measurement of Remote Login's
/// "Allow full disk access for remote users" (design §1.7): without the switch the open
/// fails with `EPERM`, even for root.
pub const SYSTEM_TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// `hidutil list`'s matching dictionary for keyboards (Generic Desktop page, Keyboard usage).
pub const KEYBOARD_MATCHING: &str = r#"{"PrimaryUsagePage":1,"PrimaryUsage":6}"#;

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

/// Gather the host's facts, listing `cache_dir` for `cached`.
///
/// A fact that cannot be read is left empty rather than guessed.
#[must_use]
pub fn gather_with_cache(sys: &dyn System, cache_dir: &Path) -> HostFacts {
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
    HostFacts {
        computer_name: scutil_get(sys, "ComputerName"),
        local_host_name: scutil_get(sys, "LocalHostName"),
        uuid: ioreg_string(&platform, "IOPlatformUUID").unwrap_or_default(),
        serial: ioreg_string(&platform, "IOPlatformSerialNumber").unwrap_or_default(),
        form: form(sys, &model),
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
        idle_secs: run(
            sys,
            &Cmd::new("/usr/sbin/ioreg").args([
                "-r",
                "-d",
                "1",
                "-k",
                "HIDIdleTime",
                "-c",
                "IOHIDSystem",
            ]),
        )
        .filter(Ran::success)
        .and_then(|r| parse_idle_ns(&r.out()))
        .map(|ns| ns / 1_000_000_000),
        remote_full_disk_access: remote_full_disk_access(sys),
        console_user: console_user(sys, &accounts),
        users: people(sys, &accounts),
        keyboards: run(sys, &hidutil_keyboards())
            .filter(Ran::success)
            .map(|r| parse_hidutil_keyboards(&r.out()))
            .unwrap_or_default(),
        cached: cached(sys, cache_dir),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
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

/// The keyboards in `hidutil list --matching '{"PrimaryUsagePage":1,"PrimaryUsage":6}'`.
///
/// Only the `Services:` section is read (it is what the HID system routes keys through,
/// and the only place a VM's built-in keyboard appears); text with no section headers is
/// read as one services table. Each row's columns are cut at the header's column starts,
/// because `Transport` and `Product` may hold spaces. Ids are hex (`0x5ac`); a model seen
/// twice (one keyboard on two services) is listed once, built in if any row says so.
#[must_use]
pub fn parse_hidutil_keyboards(text: &str) -> Vec<Keyboard> {
    let sectioned = text
        .lines()
        .any(|l| l.trim_end() == "Services:" || l.trim_end() == "Devices:");
    let mut in_services = !sectioned;
    let mut columns: Option<Vec<(String, usize)>> = None;
    let mut out: Vec<Keyboard> = Vec::new();
    for row in text.lines() {
        let trimmed = row.trim_end();
        if sectioned && !row.starts_with(' ') && trimmed.ends_with(':') {
            in_services = trimmed == "Services:";
            columns = None;
            continue;
        }
        if !in_services || trimmed.trim().is_empty() {
            continue;
        }
        let Some(cols) = &columns else {
            columns = Some(header_columns(row));
            continue;
        };
        let field = |name: &str| column(row, cols, name);
        let (Some(vendor), Some(product)) = (
            field("VendorID").and_then(|v| parse_hex(&v)),
            field("ProductID").and_then(|v| parse_hex(&v)),
        ) else {
            continue;
        };
        // Built-In is the last column; its last word is its value whatever the alignment.
        let builtin = trimmed.split_whitespace().last() == Some("1");
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
    out
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
            let uid = words.next()?.parse().ok()?;
            words.next().is_none().then(|| (name.to_owned(), uid))
        })
        .collect()
}

/// The attributes of one `dscl . -read <record> <attr>…` answer, in its order.
///
/// A value is on the attribute's line (`UniqueID: 501`) or, when it holds a space, on the
/// indented lines below it (`RealName:` then ` Managed via Tart`), joined by newlines.
/// `No such key: <attr>` lines are skipped, so a missing attribute is simply absent.
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

/// Nanoseconds since the last input, from the first `"HIDIdleTime" = <n>` of `ioreg`.
#[must_use]
pub fn parse_idle_ns(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (_, value) = l.split_once("\"HIDIdleTime\" = ")?;
        value.trim().parse().ok()
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
/// Mac. Parenthesized, so it is no account name (`user` refuses it) and matches no user.
pub const CONSOLE_UNKNOWN: &str = "(unknown)";

/// The owner of `/dev/console` by name; none at the login window (root owns it there).
///
/// `None` is a measurement ("nobody at the console", which lets a run disrupt, design
/// §5.3), so a console this cannot name is never `None`: an unreadable `/dev/console` is
/// [`CONSOLE_UNKNOWN`], and an owner the account list does not name is `(uid <n>)`. Neither
/// is an account name, so nothing resolves them as a user, and a disruption check reads
/// someone at the Mac.
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

/// The people's accounts (uid ≥ 501, no `_` service prefix), each resolved. Empty when the
/// `admin` group cannot be read: an admin bit would otherwise be a guess.
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
            .and_then(|v| v.trim().parse::<u32>().ok())
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
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};

    /// `hidutil list --matching '{"PrimaryUsagePage":1,"PrimaryUsage":6}'` in the macOS 27.0
    /// VM: a built-in virtual keyboard only in Services, and a USB one in both sections.
    /// Each line ends `\n\` so its trailing padding survives byte for byte.
    const VM_HIDUTIL: &str = "Services:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID       Transport                     Class                    Product              UserClass               Built-In \n\
0x5ac    0x1       0x0        1         6     0x7c485324000001 AppleVirtualPlatformHIDBridge (null)                   Virtual Keyboard     (null)                  1        \n\
0x5ac    0x8105    0x10900000 1         6     0x100000335      USB                           AppleUserHIDEventService Virtual USB Keyboard AppleUserHIDEventDriver 0        \n\
\n\
Devices:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class              Product              UserClass                 Built-In \n\
0x5ac    0x8105    0x10900000 1         6     0x100000313 USB       AppleUserHIDDevice Virtual USB Keyboard AppleUserUSBHostHIDDevice 0        \n\
\n\
";

    /// The same command on an Apple silicon MacBook Pro (macOS 27.0.1).
    const LAPTOP_HIDUTIL: &str = "Services:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class                         Product                            UserClass Built-In \n\
0x0      0x0       0xe9       1         6     0x100000b63 FIFO      AppleHIDKeyboardEventDriverV2 Apple Internal Keyboard / Trackpad (null)    1        \n\
\n\
Devices:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class                      Product                            UserClass Built-In \n\
0x0      0x0       0xe9       1         6     0x100000b51 FIFO      AppleHIDTransportHIDDevice Apple Internal Keyboard / Trackpad (null)    1        \n\
\n\
";

    /// `ioreg -rd1 -c IOPlatformExpertDevice` in the VM (the lines the parser reads, and
    /// the binary ones around them).
    const VM_IOREG: &str = r##"+-o VMA2MACOSAP  <class IOPlatformExpertDevice, id 0x10000012f, registered, matched, active, busy 0 (9281 ms), retain 28>
    {
      "#address-cells" = <02000000>
      "serial-number" = <5a4e59564e54524a4e3500000000000000000000000000000000000000000000>
      "IOBusyInterest" = "IOCommand is not serializable"
      "compatible" = <"VMA2MACOSAP","VirtualMac2,1","AppleVirtualPlatformARM">
      "IOPlatformSerialNumber" = "ZNYVNTRJN5"
      "model" = <"VirtualMac2,1">
      "mlb-serial-number" = <0000000000000000000000000000000000000000000000000000000000000000>
      "IOPlatformUUID" = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"
    }
"##;

    /// `dscl . -list /Users UniqueID` in the VM (the service accounts trimmed to a few).
    const VM_DSCL_LIST: &str = "_amavisd                 83
_analyticsd              263
_oahd                    441
_xserverdocs             251
admin                    501
daemon                   1
nobody                   -2
root                     0
";

    /// `dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell`.
    const VM_DSCL_ADMIN: &str = "NFSHomeDirectory: /Users//admin
PrimaryGroupID: 20
UniqueID: 501
UserShell: /bin/zsh
";

    /// `dscl . -read /Groups/admin GroupMembership` in the VM.
    const VM_ADMIN_GROUP: &str = "GroupMembership: root _mbsetupuser admin\n";

    /// `ioreg -r -d 1 -k HIDIdleTime -c IOHIDSystem` in the VM (abridged to its first keys).
    const VM_IDLE: &str = r#"+-o IOHIDSystem  <class IOHIDSystem, id 0x1000001a2, registered, matched, active, busy 0 (1268 ms), retain 12>
    {
      "IOClass" = "IOHIDSystem"
      "HIDScrollCountMaxTimeDeltaBetween" = 600
      "HIDIdleTime" = 1748058687083
      "HIDScrollCountIgnoreMomentumScrolls" = Yes
    }
"#;

    fn vm() -> Fake {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Managed\u{2019}s Virtual Machine\n"),
        )
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("Manageds-Virtual-Machine\n"),
        )
        .on(
            "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
            Ran::ok(VM_IOREG),
        )
        .on("/usr/sbin/sysctl -n hw.model", Ran::ok("VirtualMac2,1\n"))
        .on("/usr/bin/uname -m", Ran::ok("arm64\n"))
        .on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"))
        .on("/usr/bin/sw_vers -buildVersion", Ran::ok("26A428\n"))
        .on(
            "/usr/sbin/sysctl -n kern.boottime",
            Ran::ok("{ sec = 1792190170, usec = 411172 } Fri Oct 16 22:36:10 2026\n"),
        )
        .on(
            "/usr/sbin/ioreg -r -d 1 -k HIDIdleTime -c IOHIDSystem",
            Ran::ok(VM_IDLE),
        )
        .on(
            "/usr/bin/fdesetup isactive",
            Ran {
                status: Some(1),
                stdout: b"false\n".to_vec(),
                stderr: Vec::new(),
            },
        )
        .on(
            "/usr/bin/dscl . -list /Users UniqueID",
            Ran::ok(VM_DSCL_LIST),
        )
        .on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok(VM_ADMIN_GROUP),
        )
        .on(
            "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
            Ran::ok(VM_DSCL_ADMIN),
        )
        .on(&hidutil_keyboards().display(), Ran::ok(VM_HIDUTIL))
        .file(SYSTEM_TCC_DB, b"SQLite format 3\0")
        .file(
            "/c/0b1f8c6a2d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeef",
            b"x",
        )
        .file("/c/.partial.aterm-machines-77", b"x")
        .stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 0,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys
    }

    #[test]
    fn the_vm_facts_come_from_the_tools_real_answers() {
        let f = gather_with_cache(&vm(), Path::new("/c"));
        assert_eq!(f.computer_name, "Managed\u{2019}s Virtual Machine");
        assert_eq!(f.local_host_name, "Manageds-Virtual-Machine");
        assert_eq!(f.uuid, "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43");
        assert_eq!(f.serial, "ZNYVNTRJN5");
        assert_eq!(f.model, "VirtualMac2,1");
        assert_eq!(f.arch, "arm64");
        assert_eq!(f.os_version, "27.0");
        assert_eq!(f.os_build, "26A428");
        assert_eq!(f.form, Form::Virtual);
        assert_eq!(f.filevault, Some(false));
        assert_eq!(f.boot_time, 1_792_190_170);
        assert_eq!(f.idle_secs, Some(1748));
        assert_eq!(f.remote_full_disk_access, Some(true));
        assert_eq!(f.console_user.as_deref(), Some("admin"));
        assert_eq!(
            f.users,
            vec![User {
                name: "admin".into(),
                uid: 501,
                gid: 20,
                home: "/Users//admin".into(),
                shell: "/bin/zsh".into(),
                admin: true,
            }]
        );
        assert_eq!(f.keyboards.len(), 2);
        assert_eq!(
            f.cached,
            vec!["0b1f8c6a2d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeef".to_owned()]
        );
        assert_eq!(f.agent_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_fact_no_tool_answered_stays_empty_rather_than_guessed() {
        // Nothing scripted: every run fails, every file is missing.
        let f = gather_with_cache(&Fake::root(), Path::new("/c"));
        assert_eq!(f.uuid, "");
        assert_eq!(f.computer_name, "");
        assert_eq!(f.form, Form::Unknown);
        assert_eq!(f.filevault, None);
        assert_eq!(f.boot_time, 0);
        assert_eq!(f.idle_secs, None);
        assert_eq!(f.remote_full_disk_access, None);
        // Not "nobody at the console" (None), which would let a run disrupt someone.
        assert_eq!(f.console_user.as_deref(), Some(CONSOLE_UNKNOWN));
        assert!(f.users.is_empty() && f.keyboards.is_empty() && f.cached.is_empty());
    }

    #[test]
    fn a_console_owner_no_account_names_is_someone_never_nobody_and_never_a_user() {
        // The VM's console (uid 501), with the account list unanswered.
        let sys = Fake::root();
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 0,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let f = gather_with_cache(&sys, Path::new("/c"));
        assert_eq!(f.console_user.as_deref(), Some("(uid 501)"));
        for not_a_name in ["(uid 501)", CONSOLE_UNKNOWN] {
            assert!(user(&sys, not_a_name).is_err(), "{not_a_name}");
        }
    }

    #[test]
    fn the_vm_keyboards_are_read_from_services_once_each_with_the_built_in_one_marked() {
        let k = parse_hidutil_keyboards(VM_HIDUTIL);
        assert_eq!(
            k,
            vec![
                Keyboard {
                    vendor: 0x5ac,
                    product: 0x1,
                    name: "Virtual Keyboard".into(),
                    builtin: true,
                },
                Keyboard {
                    vendor: 0x5ac,
                    product: 0x8105,
                    name: "Virtual USB Keyboard".into(),
                    builtin: false,
                },
            ]
        );
    }

    #[test]
    fn an_apple_silicon_laptop_keyboard_is_0_0_and_built_in() {
        let k = parse_hidutil_keyboards(LAPTOP_HIDUTIL);
        assert_eq!(
            k,
            vec![Keyboard {
                vendor: 0,
                product: 0,
                name: "Apple Internal Keyboard / Trackpad".into(),
                builtin: true,
            }]
        );
    }

    #[test]
    fn a_keyboard_listed_only_under_devices_is_not_a_service_and_is_skipped() {
        let only_devices = VM_HIDUTIL.split_once("Devices:").unwrap().1;
        let text = format!("Services:\n\nDevices:{only_devices}");
        assert!(parse_hidutil_keyboards(&text).is_empty());
    }

    #[test]
    fn a_table_without_section_headers_is_read_as_services() {
        let services = VM_HIDUTIL
            .split_once("Services:\n")
            .unwrap()
            .1
            .split_once("\n\n")
            .unwrap()
            .0;
        assert_eq!(parse_hidutil_keyboards(services).len(), 2);
    }

    #[test]
    fn a_product_name_with_non_ascii_text_does_not_break_the_ids() {
        let text = "Services:
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport            Class   Product          UserClass Built-In
0x4d9    0x2a7     0x1        1         6     0x100000aaa Bluetooth Low Energy Driver   Clavier \u{e9}t\u{e9}     (null)    0
";
        let k = parse_hidutil_keyboards(text);
        assert_eq!(k.len(), 1);
        assert_eq!(
            (k[0].vendor, k[0].product, k[0].builtin),
            (0x4d9, 0x2a7, false)
        );
        assert!(k[0].name.starts_with("Clavier"), "{:?}", k[0].name);
    }

    #[test]
    fn dscl_values_on_continuation_lines_and_missing_keys_are_read_as_dscl_prints_them() {
        // `dscl . -read /Users//admin RealName NoSuchKey` in the VM.
        let attrs = parse_dscl_read("No such key: NoSuchKey\nRealName:\n Managed via Tart\n");
        assert_eq!(attrs, vec![("RealName".into(), "Managed via Tart".into())]);
        let attrs = parse_dscl_read(VM_DSCL_ADMIN);
        assert_eq!(attrs.len(), 4);
        assert_eq!(attrs[0], ("NFSHomeDirectory".into(), "/Users//admin".into()));
    }

    #[test]
    fn the_account_list_keeps_negative_uids_so_nobody_is_not_mistaken_for_a_person() {
        let rows = parse_dscl_list(VM_DSCL_LIST);
        assert!(rows.contains(&("nobody".into(), -2)));
        assert!(rows.contains(&("admin".into(), 501)));
        assert_eq!(rows.len(), 8);
    }

    #[test]
    fn a_missing_account_is_named_and_an_unreadable_admin_group_is_not_guessed() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok(VM_ADMIN_GROUP),
        )
        .on(
            "/usr/bin/dscl . -read /Users//nosuchuser UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
            Ran::exit(56, "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)"),
        );
        assert_eq!(
            user(&sys, "nosuchuser").unwrap_err(),
            "no account nosuchuser"
        );
        // No admin group answer: the account cannot be resolved honestly.
        let err = user(&Fake::root(), "admin").unwrap_err();
        assert!(err.contains("/Groups/admin"), "{err}");
    }

    #[test]
    fn one_account_resolves_with_its_admin_bit() {
        let u = user(&vm(), "admin").unwrap();
        assert_eq!((u.uid, u.gid, u.admin), (501, 20, true));
        assert_eq!(u.home, "/Users//admin");
    }

    #[test]
    fn an_account_name_that_would_change_the_record_path_is_refused_before_dscl_runs() {
        let sys = Fake::root();
        for bad in ["", "../Groups/admin", "-x", "a b"] {
            assert!(user(&sys, bad).is_err());
        }
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn nobody_at_the_console_is_root_owning_it() {
        let sys = vm();
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let f = gather_with_cache(&sys, Path::new("/c"));
        assert_eq!(f.console_user, None);
    }

    #[test]
    fn a_laptop_is_known_by_its_internal_battery_and_a_desktop_by_its_absence() {
        // `pmset -g batt` on the MacBook Pro, and in the desktop-form VM.
        let laptop = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=36044899)\t80%; AC attached; not charging present: true\n";
        let sys = Fake::root();
        sys.on("/usr/bin/pmset -g batt", Ran::ok(laptop));
        assert_eq!(form(&sys, "Mac17,4"), Form::Laptop);
        let sys = Fake::root();
        sys.on(
            "/usr/bin/pmset -g batt",
            Ran::ok("Now drawing from 'AC Power'\n"),
        );
        assert_eq!(form(&sys, "Mac16,10"), Form::Desktop);
        assert_eq!(form(&Fake::root(), "Mac16,10"), Form::Unknown);
    }

    #[test]
    fn filevault_is_read_from_the_exit_status_and_its_word_together() {
        let sys = Fake::root();
        sys.on("/usr/bin/fdesetup isactive", Ran::ok("true\n"));
        assert_eq!(filevault(&sys), Some(true));
        let sys = Fake::root();
        sys.on("/usr/bin/fdesetup isactive", Ran::exit(2, "error"));
        assert_eq!(filevault(&sys), None);
    }

    /// A port whose system TCC database open fails with `errno`: the one answer the fake
    /// cannot give by itself.
    struct TccRefuses {
        inner: Fake,
        errno: i32,
    }

    impl System for TccRefuses {
        fn scratch_dir(
            &self,
            parent: Option<&Path>,
            label: &str,
        ) -> std::io::Result<crate::port::Scratch<'_>> {
            self.inner.scratch_dir(parent, label)
        }

        fn run(&self, cmd: &Cmd) -> Result<Ran, crate::port::RunFailed> {
            self.inner.run(cmd)
        }
        fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, crate::port::RunFailed> {
            self.inner.run_tty(cmd)
        }
        fn read_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            if path == Path::new(SYSTEM_TCC_DB) {
                return Err(std::io::Error::from_raw_os_error(self.errno));
            }
            self.inner.read_file(path)
        }
        fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
            self.inner.write_file(path, bytes, mode)
        }
        fn write_user_file(
            &self,
            user: &User,
            path: &Path,
            bytes: &[u8],
            mode: u32,
        ) -> std::io::Result<()> {
            self.inner.write_user_file(user, path, bytes, mode)
        }
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            self.inner.remove(path)
        }
        fn create_dir_all(&self, path: &Path, mode: u32) -> std::io::Result<()> {
            self.inner.create_dir_all(path, mode)
        }
        fn chown(&self, path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
            self.inner.chown(path, uid, gid)
        }
        fn stat(&self, path: &Path) -> std::io::Result<Stat> {
            self.inner.stat(path)
        }
        fn list_dir(&self, path: &Path) -> std::io::Result<Vec<String>> {
            self.inner.list_dir(path)
        }
        fn sha256(&self, path: &Path) -> std::io::Result<String> {
            self.inner.sha256(path)
        }
        fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> std::io::Result<()> {
            self.inner.copy_file(from, to, mode)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.inner.rename(from, to)
        }
        fn euid(&self) -> u32 {
            self.inner.euid()
        }
        fn now(&self) -> u64 {
            self.inner.now()
        }
    }

    #[test]
    fn remote_full_disk_access_is_off_only_when_tcc_refuses_the_open() {
        // VM, from a root LaunchDaemon (no SSH FDA): `head: …/TCC.db: Operation not permitted`.
        let refused = TccRefuses {
            inner: Fake::root(),
            errno: libc::EPERM,
        };
        assert_eq!(remote_full_disk_access(&refused), Some(false));
        let other = TccRefuses {
            inner: Fake::root(),
            errno: libc::EIO,
        };
        assert_eq!(remote_full_disk_access(&other), None);
        // A missing database is not a measurement either.
        assert_eq!(remote_full_disk_access(&Fake::root()), None);
    }

    #[test]
    fn boot_time_and_idle_time_parse_the_tools_own_formats() {
        assert_eq!(
            parse_boottime("{ sec = 1792190170, usec = 411172 } Fri Oct 16 22:36:10 2026\n"),
            Some(1_792_190_170)
        );
        assert_eq!(parse_boottime("garbage"), None);
        assert_eq!(parse_idle_ns(VM_IDLE), Some(1_748_058_687_083));
        assert_eq!(parse_idle_ns("{}"), None);
        assert_eq!(
            ioreg_string(VM_IOREG, "IOPlatformUUID").as_deref(),
            Some("C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43")
        );
        assert_eq!(
            ioreg_string(VM_IOREG, "IOBusyInterest").as_deref(),
            Some("IOCommand is not serializable")
        );
        assert_eq!(ioreg_string(VM_IOREG, "missing"), None);
    }

    #[test]
    fn the_live_keyboard_list_parses_where_hidutil_answers() {
        // Read-only: `hidutil list` changes nothing. Skips where it is absent or refuses.
        let Ok(ran) = crate::port::Live::new().run(&hidutil_keyboards()) else {
            return;
        };
        if !ran.success() {
            return;
        }
        let text = ran.out();
        let rows = text
            .split_once("Services:")
            .map_or("", |(_, s)| s.split("\n\n").next().unwrap_or(""))
            .lines()
            .filter(|l| l.starts_with("0x"))
            .count();
        let parsed = parse_hidutil_keyboards(&text);
        // Every services row is a keyboard, listed once per model.
        assert_eq!(parsed.is_empty(), rows == 0, "{text}");
        assert!(parsed.len() <= rows, "{text}");
    }
}
