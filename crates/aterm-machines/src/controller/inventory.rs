// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `machines.toml` (the inventory), the controller's directory layout, serials and reports (design §2, §4.2, §9).

use crate::model::HostReport;
use crate::port::System;
use aterm_toml::edit::{ArrayOfTables, DocumentMut, Item, Table, Value};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Component, Path, PathBuf};

/// The longest ControlPath OpenSSH's master can listen on.
pub const CONTROL_PATH_MAX: usize = 86;

/// The mode of every file this module writes: the inventory, state, the serial.
const FILE_MODE: u32 = 0o600;

/// The mode of every directory it creates, the root first.
const DIR_MODE: u32 = 0o700;

/// The controller's directory: `<aterm config dir>/machines/` by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The directory itself (0700).
    pub root: PathBuf,
}

impl Paths {
    /// The default directory, under aterm's config directory.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        aterm_types::dirs::aterm_config_dir().map(|d| Self {
            root: d.join("machines"),
        })
    }

    /// The directory `--controller <dir>` names, made absolute against the current directory.
    #[must_use]
    pub fn at(dir: &Path) -> Self {
        Self {
            root: std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()),
        }
    }

    /// `machines.toml`.
    #[must_use]
    pub fn inventory(&self) -> PathBuf {
        self.root.join("machines.toml")
    }

    /// `baselines/`.
    #[must_use]
    pub fn baselines(&self) -> PathBuf {
        self.root.join("baselines")
    }

    /// `baselines/<name>.toml`.
    #[must_use]
    pub fn baseline(&self, name: &str) -> PathBuf {
        self.baselines().join(format!("{name}.toml"))
    }

    /// `keys/`: the fleet SSH key, the signing keys, the GitHub token file and the control
    /// sockets.
    #[must_use]
    pub fn keys(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// `keys/ssh_ed25519` (the fleet SSH key; `.pub` beside it).
    #[must_use]
    pub fn ssh_key(&self) -> PathBuf {
        self.keys().join("ssh_ed25519")
    }

    /// `keys/signing` (the daily signing key; `.pub` beside it).
    #[must_use]
    pub fn signing_key(&self) -> PathBuf {
        self.keys().join("signing")
    }

    /// `keys/github_token` (0600): the `[github]` machine user's token, as `aterm machines
    /// github login` stores it, never in the login keychain.
    #[must_use]
    pub fn github_token(&self) -> PathBuf {
        self.keys().join("github_token")
    }

    /// `keys/recovery.pub` (the recovery key's public half; the private half goes offline).
    #[must_use]
    pub fn recovery_pub(&self) -> PathBuf {
        self.keys().join("recovery.pub")
    }

    /// `allowed_signers`, as enrollment writes it to every host.
    #[must_use]
    pub fn allowed_signers(&self) -> PathBuf {
        self.root.join("allowed_signers")
    }

    /// `known_hosts`, keyed by each host's UUID (`HostKeyAlias`).
    #[must_use]
    pub fn known_hosts(&self) -> PathBuf {
        self.root.join("known_hosts")
    }

    /// Where control sockets live: `keys/ctl/` when a socket there fits [`CONTROL_PATH_MAX`], else `/private/tmp/aterm-machines-<uid>/` (a deep root, e.g. a scratch HOME, would make OpenSSH refuse the master).
    #[must_use]
    pub fn control_dir(&self) -> PathBuf {
        let under_root = self.keys().join("ctl");
        if under_root.as_os_str().len() + 1 + 8 <= CONTROL_PATH_MAX {
            under_root
        } else {
            PathBuf::from(format!(
                "/private/tmp/aterm-machines-{}",
                aterm_uds::peer::our_uid()
            ))
        }
    }

    /// A short ControlPath for `host` (design §1.16): `<control dir>/<8 hex>`, the first eight hex digits of the SHA-256 of `host`, so the path stays short whatever `host` is.
    #[must_use]
    pub fn control_path(&self, host: &str) -> PathBuf {
        let digest = aterm_digest::Sha256::digest(host.as_bytes());
        let hex = aterm_codec::hex::encode_lower(&digest);
        self.control_dir().join(&hex[..8])
    }

    /// `state/`.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `state/<uuid>.json`: the newest report and facts for a host.
    #[must_use]
    pub fn state(&self, uuid: &str) -> PathBuf {
        self.state_dir().join(format!("{uuid}.json"))
    }

    /// `state/retired/<uuid>.json`: a retired host's last state.
    #[must_use]
    pub fn retired_state(&self, uuid: &str) -> PathBuf {
        self.state_dir()
            .join("retired")
            .join(format!("{uuid}.json"))
    }

    /// `cache/`.
    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `cache/<sha256>`: a verified artifact.
    #[must_use]
    pub fn cached(&self, sha256: &str) -> PathBuf {
        self.cache().join(sha256.to_ascii_lowercase())
    }

    /// `serial`: the controller's envelope counter.
    #[must_use]
    pub fn serial(&self) -> PathBuf {
        self.root.join("serial")
    }

    /// Create the root and its directories (0700): `keys`, `baselines`, `state`, `cache`.
    /// # Errors
    /// One line naming the directory that could not be made.
    pub fn ensure(&self, sys: &dyn System) -> Result<(), String> {
        for dir in [
            self.root.clone(),
            self.keys(),
            self.baselines(),
            self.state_dir(),
            self.cache(),
        ] {
            sys.create_dir_all(&dir, DIR_MODE)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }

    /// Make [`Paths::control_dir`] and check it is this process's own: a real directory (not a link), owned by this uid, open to nobody else.
    /// # Errors
    /// One line: the directory could not be made, or it is not ours alone.
    pub fn ensure_control_dir(&self, sys: &dyn System) -> Result<PathBuf, String> {
        let dir = self.control_dir();
        sys.create_dir_all(&dir, DIR_MODE)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let st = sys
            .stat(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        if st.symlink || !st.dir {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if st.uid != sys.euid() {
            return Err(format!(
                "{} belongs to uid {}, not this uid ({}); remove it",
                dir.display(),
                st.uid,
                sys.euid()
            ));
        }
        if st.mode & 0o077 != 0 {
            return Err(format!(
                "{} is open to other accounts (mode {:o}); chmod 700 it",
                dir.display(),
                st.mode
            ));
        }
        Ok(dir)
    }
}

/// `[controller]` in machines.toml.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Controller {
    /// Concurrent hosts per command (default 8).
    #[serde(default)]
    pub parallel: Option<usize>,
    /// The principal name this controller's signing key has in `allowed_signers`.
    #[serde(default)]
    pub principal: Option<String>,
}

/// `[github]` in machines.toml (design §7).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Github {
    /// The machine user's login.
    pub account: String,
    /// `keys = "gh" | "agh-inline" | "agh-fleet"`; absent is `gh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<KeysVia>,
}

impl Github {
    /// `keys`, or `KeysVia::Gh` when absent.
    #[must_use]
    pub fn keys_via(&self) -> KeysVia {
        self.keys.unwrap_or_default()
    }
}

/// How the fleet's SSH keys reach GitHub (§17, machines 5a and 5b).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeysVia {
    /// The REST key path, as today (`System::github_rest`): the default until WP15.
    #[default]
    Gh,
    /// agh's library, the token from the controller's `keys/github_token` (5a).
    AghInline,
    /// agh's library, the token from agh.toml's `fleet` profile (5b).
    AghFleet,
}

/// One `[[host]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// The inventory name (`lab-07`), what `{name}` fills.
    pub name: String,
    /// IOPlatformUUID: the identity (`HostKeyAlias`, envelope binding).
    pub uuid: String,
    /// Where to connect (`lab-07.local`, an address).
    pub address: String,
    /// The admin account the fleet key is authorized for.
    pub user: String,
    /// The baseline's name (`baselines/<name>.toml`), `default` when absent.
    #[serde(default)]
    pub baseline: Option<String>,
    /// Accounts user-scope items apply to (default: `user`).
    #[serde(default)]
    pub users: Vec<String>,
}

/// The whole inventory.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// `schema = 1`.
    pub schema: u32,
    /// `[controller]`.
    #[serde(default)]
    pub controller: Controller,
    /// `[github]`.
    #[serde(default)]
    pub github: Option<Github>,
    /// Custom host display names (`[names]` table mapping a host selector or tag to its name).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub names: std::collections::BTreeMap<String, String>,
    /// `[[host]]`.
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
}

impl Inventory {
    /// Concurrent hosts per command: `[controller] parallel`, 8 when unset.
    #[must_use]
    pub fn parallel(&self) -> usize {
        self.controller.parallel.unwrap_or(8)
    }
}

/// Read `machines.toml`.
/// # Errors
/// One line: missing (run `aterm machines init`), unreadable, or not an inventory.
pub fn load(sys: &dyn System, paths: &Paths) -> Result<Inventory, String> {
    let path = paths.inventory();
    let text = read_inventory_text(sys, &path)?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse an inventory document and check it: `schema = 1`, no unknown key, and hosts the rest of the controller can use as they are (names and UUIDs unique, every word safe to hand to `ssh` and to name a file by).
/// # Errors
/// One line saying what is wrong.
pub fn parse(text: &str) -> Result<Inventory, String> {
    let inv: Inventory = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
    if inv.schema != crate::SCHEMA {
        return Err(format!(
            "schema = {}; this aterm reads schema = {}",
            inv.schema,
            crate::SCHEMA
        ));
    }
    if inv.controller.parallel == Some(0) {
        return Err("[controller] parallel must be at least 1".into());
    }
    if let Some(github) = &inv.github
        && github.account.trim().is_empty()
    {
        return Err("[github] account is empty".into());
    }
    for (k, v) in &inv.names {
        if k.trim().is_empty() || !plain_word(v) {
            return Err(format!(
                "[names] {k:?} = {v:?}: name must be letters, digits, `.`, `_` or `-`"
            ));
        }
    }
    for (i, host) in inv.hosts.iter().enumerate() {
        check_host(host).map_err(|e| format!("[[host]] #{}: {e}", i + 1))?;
        for earlier in &inv.hosts[..i] {
            if earlier.name.eq_ignore_ascii_case(&host.name) {
                return Err(format!("two hosts are named {}", host.name));
            }
            if earlier.uuid.eq_ignore_ascii_case(&host.uuid) {
                return Err(format!(
                    "{} and {} have the same uuid {}",
                    earlier.name, host.name, host.uuid
                ));
            }
        }
    }
    Ok(inv)
}

/// Whether `host`'s words are usable: a name and accounts made of `[A-Za-z0-9._-]` not starting with `-` or `.`, an IOPlatformUUID, and an address `ssh` cannot read as an option.
/// # Errors
/// One line naming the field.
pub fn check_host(host: &Host) -> Result<(), String> {
    if !plain_word(&host.name) {
        return Err(format!(
            "name {:?} must be letters, digits, `.`, `_` or `-`",
            host.name
        ));
    }
    if !is_uuid(&host.uuid) {
        return Err(format!(
            "{}: uuid {:?} is not an IOPlatformUUID (8-4-4-4-12 hex digits)",
            host.name, host.uuid
        ));
    }
    if !plain_address(&host.address) {
        return Err(format!(
            "{}: address {:?} is not a host name or address",
            host.name, host.address
        ));
    }
    if !plain_word(&host.user) {
        return Err(format!(
            "{}: user {:?} is not an account name",
            host.name, host.user
        ));
    }
    if let Some(baseline) = &host.baseline
        && !plain_word(baseline)
    {
        return Err(format!(
            "{}: baseline {:?} must name a file in baselines/ (letters, digits, `.`, `_`, `-`)",
            host.name, baseline
        ));
    }
    if let Some(user) = host.users.iter().find(|u| !plain_word(u)) {
        return Err(format!(
            "{}: users holds {user:?}, which is not an account name",
            host.name
        ));
    }
    Ok(())
}

/// A non-empty word of `[A-Za-z0-9._-]` that starts with neither `-` (an option to every
/// tool) nor `.` (a hidden or parent path).
fn plain_word(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with(['-', '.'])
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A host name, IPv4 or IPv6 address (a `%zone` allowed): printable, no space, no `@`, and
/// not starting with `-`, so `ssh` reads `user@address` as its destination.
fn plain_address(address: &str) -> bool {
    !address.is_empty()
        && !address.starts_with('-')
        && address
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'%'))
}

/// `8-4-4-4-12` hex digits, as `ioreg` prints IOPlatformUUID.
#[must_use]
pub fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The inventory's text, or the one-line error every verb shows before `init` has run.
fn read_inventory_text(sys: &dyn System, path: &Path) -> Result<String, String> {
    match sys.read_file(path) {
        Ok(bytes) => {
            String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
            "no inventory at {}: run `aterm machines init` first",
            path.display()
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Append `host` to `machines.toml`, keeping every comment and hand edit, or update the block with the same `uuid` in place.
/// # Errors
/// One line: no inventory, an inventory that does not parse, a name another host has, or a write that failed.
pub fn upsert_host(sys: &dyn System, paths: &Paths, host: &Host) -> Result<(), String> {
    check_host(host)?;
    let path = paths.inventory();
    let text = read_inventory_text(sys, &path)?;
    let before = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(other) = before.hosts.iter().find(|h| {
        h.name.eq_ignore_ascii_case(&host.name) && !h.uuid.eq_ignore_ascii_case(&host.uuid)
    }) {
        return Err(format!(
            "the name {} is taken by the host with uuid {}",
            host.name, other.uuid
        ));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let root = doc.as_table_mut();
    match root.get_mut("host") {
        Some(item) => {
            let Some(old) = item.as_array_of_tables().cloned() else {
                return Err(format!(
                    "{}: `host` is not an array of [[host]] tables",
                    path.display()
                ));
            };
            let mut rebuilt = ArrayOfTables::new();
            let mut found = false;
            for table in old.iter() {
                let mut table = table.clone();
                if table
                    .get("uuid")
                    .and_then(Item::as_str)
                    .is_some_and(|u| u.eq_ignore_ascii_case(&host.uuid))
                {
                    update_table(&mut table, host);
                    found = true;
                }
                rebuilt.push(table);
            }
            if !found {
                rebuilt.push(new_table(host));
            }
            *item = Item::ArrayOfTables(rebuilt);
        }
        None => {
            let mut hosts = ArrayOfTables::new();
            hosts.push(new_table(host));
            root.insert("host", Item::ArrayOfTables(hosts));
        }
    }
    let out = doc.to_string();
    let after = parse(&out).map_err(|e| format!("the edited inventory does not parse: {e}"))?;
    let landed = after
        .hosts
        .iter()
        .find(|h| h.uuid.eq_ignore_ascii_case(&host.uuid))
        .is_some_and(|h| {
            h.name == host.name
                && h.address == host.address
                && h.user == host.user
                && host
                    .baseline
                    .as_ref()
                    .is_none_or(|b| h.baseline.as_ref() == Some(b))
                && (host.users.is_empty() || h.users == host.users)
        });
    let others_kept = before
        .hosts
        .iter()
        .filter(|b| !b.uuid.eq_ignore_ascii_case(&host.uuid))
        .all(|b| after.hosts.contains(b));
    if !landed || !others_kept {
        return Err("the edited inventory does not hold the host as written".into());
    }
    sys.write_file(&path, out.as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A fresh `[[host]]` table for `host`, keys in the order the design lists them.
fn new_table(host: &Host) -> Table {
    let mut table = Table::new();
    table.insert("name", Item::Value(Value::from(host.name.as_str())));
    table.insert("uuid", Item::Value(Value::from(host.uuid.as_str())));
    table.insert("address", Item::Value(Value::from(host.address.as_str())));
    table.insert("user", Item::Value(Value::from(host.user.as_str())));
    if let Some(baseline) = &host.baseline {
        table.insert("baseline", Item::Value(Value::from(baseline.as_str())));
    }
    if !host.users.is_empty() {
        table.insert("users", Item::Value(users_value(&host.users)));
    }
    table
}

/// Rewrite the values of an existing `[[host]]` that differ from `host`.
fn update_table(table: &mut Table, host: &Host) {
    set_str(table, "name", &host.name);
    set_str(table, "address", &host.address);
    set_str(table, "user", &host.user);
    if let Some(baseline) = &host.baseline {
        set_str(table, "baseline", baseline);
    }
    if !host.users.is_empty() {
        let same = table
            .get("users")
            .and_then(Item::as_array)
            .is_some_and(|a| {
                a.len() == host.users.len()
                    && a.iter()
                        .zip(&host.users)
                        .all(|(v, u)| v.as_str() == Some(u.as_str()))
            });
        if !same {
            replace_value(table, "users", users_value(&host.users));
        }
    }
}

fn users_value(users: &[String]) -> Value {
    Value::from(
        users
            .iter()
            .map(String::as_str)
            .collect::<aterm_toml::edit::Array>(),
    )
}

/// Set `key` to the string `value` unless it already holds it.
fn set_str(table: &mut Table, key: &str, value: &str) {
    if table.get(key).and_then(Item::as_str) != Some(value) {
        replace_value(table, key, Value::from(value));
    }
}

/// Set `key` to `value`, keeping the old value's surroundings: the spacing after `=` and the
/// comment at the end of its line.
fn replace_value(table: &mut Table, key: &str, mut value: Value) {
    if let Some(old) = table.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    table.insert(key, Item::Value(value));
}

/// Comment out the `[[host]]` block with `uuid`, move its state to `state/retired/`, and drop its pinned key from `known_hosts` (design §4.2, `unenroll` step 3).
/// # Errors
/// One line: no host with `uuid`, an inventory that does not parse, or a write that failed.
pub fn retire_host(sys: &dyn System, paths: &Paths, uuid: &str) -> Result<(), String> {
    let path = paths.inventory();
    let text = read_inventory_text(sys, &path)?;
    let before = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(host) = before
        .hosts
        .iter()
        .find(|h| h.uuid.eq_ignore_ascii_case(uuid))
    else {
        return Err(format!("no host with uuid {uuid} in {}", path.display()));
    };
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (start, end) = host_block(&doc, &text, uuid).ok_or_else(|| {
        format!(
            "{}: cannot find {}'s [[host]] lines",
            path.display(),
            host.name
        )
    })?;
    let mut out = String::with_capacity(text.len() + 256);
    out.push_str(&text[..start]);
    out.push_str(&format!(
        "# Retired by `aterm machines unenroll` (unix time {}): {} {}\n",
        sys.now(),
        host.name,
        host.uuid
    ));
    for line in text[start..end].split_inclusive('\n') {
        if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str("# ");
            out.push_str(line);
        }
    }
    if !text[start..end].ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&text[end..]);
    let after = parse(&out).map_err(|e| format!("the edited inventory does not parse: {e}"))?;
    let rest_kept = after.hosts.len() + 1 == before.hosts.len()
        && before
            .hosts
            .iter()
            .filter(|h| !h.uuid.eq_ignore_ascii_case(uuid))
            .zip(&after.hosts)
            .all(|(a, b)| a == b);
    if !rest_kept {
        return Err("commenting out the host would change other hosts; edit it by hand".into());
    }
    sys.write_file(&path, out.as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    retire_state(sys, paths, &host.uuid)?;
    forget_host_key(sys, paths, &host.uuid)
}

/// The byte range of the `[[host]]` block with `uuid` in `text` (which `doc` was parsed
/// from): from the start of its header's line to the end of the line its last value ends on.
fn host_block(doc: &DocumentMut, text: &str, uuid: &str) -> Option<(usize, usize)> {
    let hosts = doc.get("host")?.as_array_of_tables()?;
    let table = hosts.iter().find(|t| {
        t.get("uuid")
            .and_then(Item::as_str)
            .is_some_and(|u| u.eq_ignore_ascii_case(uuid))
    })?;
    let header = table.span()?;
    let start = text[..header.start].rfind('\n').map_or(0, |i| i + 1);
    let last = table
        .get_values()
        .iter()
        .filter_map(|(_, v)| v.span())
        .map(|s| s.end)
        .chain(std::iter::once(header.end))
        .max()?;
    let end = text[last..].find('\n').map_or(text.len(), |i| last + i + 1);
    Some((start, end))
}

/// Move `state/<uuid>.json` to `state/retired/<uuid>.json`; nothing to move is success.
fn retire_state(sys: &dyn System, paths: &Paths, uuid: &str) -> Result<(), String> {
    let from = paths.state(uuid);
    match sys.stat(&from) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", from.display())),
        Ok(_) => {}
    }
    let to = paths.retired_state(uuid);
    if let Some(dir) = to.parent() {
        sys.create_dir_all(dir, DIR_MODE)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    sys.rename(&from, &to)
        .map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))
}

/// Whether the `known_hosts` line `line` pins a key under `alias`: one of the comma-separated
/// names in its first field is `alias` (or `[alias]:port`), as OpenSSH matches a host name,
/// without regard to case.
pub(crate) fn pins(line: &str, alias: &str) -> bool {
    let named = |pattern: &str| {
        pattern
            .strip_prefix('[')
            .and_then(|p| p.split_once("]:"))
            .map_or(pattern, |(name, _)| name)
            .eq_ignore_ascii_case(alias)
    };
    line.split_whitespace()
        .next()
        .is_some_and(|hosts| hosts.split(',').any(named))
}

/// Drop every `known_hosts` line pinned under `alias` (the host's UUID; OpenSSH matches it without regard to case).
/// # Errors
/// One line when the file cannot be read or written.
pub fn forget_host_key(sys: &dyn System, paths: &Paths, alias: &str) -> Result<(), String> {
    let path = paths.known_hosts();
    let text = match sys.read_file(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let kept: String = text
        .split_inclusive('\n')
        .filter(|line| !pins(line, alias))
        .collect();
    if kept.len() == text.len() {
        return Ok(());
    }
    sys.write_file(&path, kept.as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The hosts `names` select (`--all` when `all`), in inventory order.
/// # Errors
/// One line for a selector that matches multiple hosts, each name that matches no host (and the hosts there are), or a request that names no host at all.
pub fn select<'a>(
    inv: &'a Inventory,
    names: &[String],
    all: bool,
) -> Result<Vec<&'a Host>, String> {
    if all {
        if !names.is_empty() {
            return Err("name hosts or pass --all, not both".into());
        }
        return Ok(inv.hosts.iter().collect());
    }
    if names.is_empty() {
        return Err("name a host (its name, uuid or address), or pass --all".into());
    }
    let mut picked = vec![false; inv.hosts.len()];
    let mut unknown: Vec<&str> = Vec::new();
    for name in names {
        let mut hit: Option<usize> = None;
        for (i, host) in inv.hosts.iter().enumerate() {
            if host.name.eq_ignore_ascii_case(name)
                || host.uuid.eq_ignore_ascii_case(name)
                || host.address.eq_ignore_ascii_case(name)
            {
                if let Some(earlier) = hit {
                    return Err(format!(
                        "selector {name:?} is ambiguous: it names both {} and {}; use an \
                         unambiguous name or UUID",
                        inv.hosts[earlier].name, host.name
                    ));
                }
                hit = Some(i);
            }
        }
        if let Some(i) = hit {
            picked[i] = true;
        } else {
            unknown.push(name);
        }
    }
    if !unknown.is_empty() {
        return Err(format!(
            "no host {} in machines.toml ({})",
            unknown.join(", "),
            known_hosts_hint(inv)
        ));
    }
    Ok(inv
        .hosts
        .iter()
        .zip(picked)
        .filter_map(|(h, p)| p.then_some(h))
        .collect())
}

/// `hosts: a, b, c` (the first eight, then a count), or `it has no hosts yet`.
fn known_hosts_hint(inv: &Inventory) -> String {
    const SHOWN: usize = 8;
    if inv.hosts.is_empty() {
        return "it has no hosts yet: `aterm machines enroll`".into();
    }
    let names: Vec<&str> = inv
        .hosts
        .iter()
        .take(SHOWN)
        .map(|h| h.name.as_str())
        .collect();
    let more = inv.hosts.len().saturating_sub(SHOWN);
    if more == 0 {
        format!("hosts: {}", names.join(", "))
    } else {
        format!("hosts: {} and {more} more", names.join(", "))
    }
}

/// The next envelope serial: `max(stored, floor) + 1`, stored before it is returned.
/// # Errors
/// One line: an unreadable or corrupt counter (never silently reset), or a failed write.
pub fn next_serial(sys: &dyn System, paths: &Paths, floor: u64) -> Result<u64, String> {
    let path = paths.serial();
    let stored = match sys.read_file(&path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            crate::parse_uint::<u64>(text.trim()).ok_or_else(|| {
                format!(
                    "{} holds {:?}, not a serial; restore it (or write the highest serial a \
                     host reports)",
                    path.display(),
                    text.trim()
                )
            })?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let next = stored
        .max(floor)
        .checked_add(1)
        .ok_or_else(|| "the serial counter is exhausted".to_string())?;
    sys.create_dir_all(&paths.root, DIR_MODE)
        .map_err(|e| format!("{}: {e}", paths.root.display()))?;
    sys.write_file(&path, format!("{next}\n").as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(next)
}

/// What the controller keeps per host between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HostState {
    /// The newest `hello`, as JSON text.
    #[serde(default)]
    pub hello: Option<String>,
    /// Unix seconds of the last successful contact.
    #[serde(default)]
    pub last_seen: u64,
    /// Unix seconds of the first failed contact since the last success.
    #[serde(default)]
    pub unreachable_since: Option<u64>,
    /// The newest report.
    #[serde(default)]
    pub report: Option<HostReport>,
    /// The OS build at the last apply.
    #[serde(default)]
    pub applied_build: Option<String>,
    /// GitHub key ids registered for this host: `(user, key id)`.
    #[serde(default)]
    pub github_keys: Vec<(String, u64)>,
    /// The key each id in `github_keys` registered: `(key id, "<type> <base64>")`.
    #[serde(default)]
    pub github_key_text: Vec<(u64, String)>,
    /// Keys `github revoke` deleted (`<type> <base64>`): never registered again whatever a report carries.
    #[serde(default)]
    pub github_revoked: Vec<String>,
    /// A `hello` that reached something and was refused or answered wrong (a changed host key, a fleet key it no longer takes, another Mac at the address): since when, and why.
    #[serde(default)]
    pub contact_error: Option<(u64, String)>,
    /// The agent's refusal of the newest run sent to it (busy, replay, expired, a bad signature): when, and why.
    #[serde(default)]
    pub last_refusal: Option<(u64, String)>,
    /// The SHA-256 of the `allowed_signers` this host was last given (at enrollment, or by a rekey it took).
    #[serde(default)]
    pub signers_sha256: Option<String>,
    /// `plan|apply --only` envelopes sent to this host and not yet reported: each one's serial and the tables it carried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only_sent: Vec<(u64, Vec<String>)>,
    /// What `report` covers when its newest run was an `--only` one: `None` for a whole baseline's report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Only>,
}

/// The coverage of a report whose newest run was `plan|apply --only` (design §9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Only {
    /// The tables `--only` runs have reported since the last whole run.
    pub tables: Vec<String>,
    /// When the whole run whose rows the other tables keep finished; `None` when no whole plan or apply was ever reported, so the other tables were never run at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<u64>,
}

/// Read `state/<uuid>.json` (default when absent).
/// # Errors
/// One line when `uuid` is not a UUID, or the file exists and does not parse.
pub fn load_state(sys: &dyn System, paths: &Paths, uuid: &str) -> Result<HostState, String> {
    if !is_uuid(uuid) {
        return Err(format!("{uuid:?} is not a host uuid"));
    }
    let path = paths.state(uuid);
    match sys.read_file(&path) {
        Ok(bytes) => aterm_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(HostState::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `state/<uuid>.json` (0600, `state/` made 0700 when missing).
/// # Errors
/// One line when `uuid` is not a UUID or the write failed.
pub fn save_state(
    sys: &dyn System,
    paths: &Paths,
    uuid: &str,
    state: &HostState,
) -> Result<(), String> {
    if !is_uuid(uuid) {
        return Err(format!("{uuid:?} is not a host uuid"));
    }
    let dir = paths.state_dir();
    sys.create_dir_all(&dir, DIR_MODE)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut json = aterm_json::to_string(state).map_err(|e| format!("state for {uuid}: {e}"))?;
    json.push('\n');
    let path = paths.state(uuid);
    sys.write_file(&path, json.as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether `path` is inside `root` (for refusing a `baseline` name that escapes): it starts with `root` and climbs out through no `..` after it.
#[must_use]
pub fn inside(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|rest| {
        rest.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    })
}

/// `raw` without its surrounding whitespace, its trailing dots and a `.local` suffix in any
/// case (an mDNS name is read without regard to case).
fn without_dot_local(raw: &str) -> &str {
    let clean = raw.trim().trim_end_matches('.');
    clean
        .len()
        .checked_sub(".local".len())
        .filter(|&at| {
            clean
                .get(at..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(".local"))
        })
        .map_or(clean, |at| &clean[..at])
}

/// Extract a canonical short tag from a host selector or Bonjour name (`m14` from `m14 Air M5 32GB 4TB prime`, `spark-49f7` from `spark-49f7-SSH`).
#[must_use]
pub fn host_tag_key(raw: &str) -> String {
    let clean = without_dot_local(raw);
    let tokens: Vec<&str> = clean
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|t| !t.is_empty())
        .collect();
    if let Some(&first) = tokens.first() {
        let lower = first.to_ascii_lowercase();
        if lower == "spark"
            && let Some(&second) = tokens.get(1)
        {
            return format!("spark-{}", second.to_ascii_lowercase());
        }
        if lower.starts_with('m')
            && lower[1..].chars().all(|c| c.is_ascii_digit())
            && lower.len() > 1
        {
            return lower;
        }
    }
    clean.to_ascii_lowercase()
}

/// Normalize any discovered or local host name into a consistent canonical hyphenated identifier (`m1-MacBook-Air-M2-24GB-1TB`, `m4-MacBook-Pro-3.1GHz-i7-16GB-1TB`, `m8-MacBook-Pro-M4-Max-36GB-1TB`, `m9-MacBook-Pro-M2-32GB-8TB`, `m10-MacBook-Pro-M4-Max-36GB-1TB`, `m14-MacBook-Air-M5-32GB-4TB-prime`, `m100-MacBook-Pro-M4-Pro-48GB-500GB`, `spark-49f7`, …), honoring any custom override in `custom_names` (`[names]` in `machines.toml`).
#[must_use]
pub fn canonical_host_name(
    raw: &str,
    custom_names: &std::collections::BTreeMap<String, String>,
) -> String {
    let trimmed = without_dot_local(raw).trim();
    if trimmed.is_empty() {
        return "unknown-host".to_owned();
    }
    let tag = host_tag_key(trimmed);
    let slug: String = trimmed.split_whitespace().collect::<Vec<_>>().join("-");
    for candidate in [trimmed, slug.as_str(), tag.as_str()] {
        if let Some((_, v)) = custom_names
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(candidate))
        {
            return v.clone();
        }
    }
    let mut raw_tokens: Vec<&str> = trimmed
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|t| !t.is_empty())
        .collect();
    if raw_tokens
        .last()
        .is_some_and(|last| last.eq_ignore_ascii_case("ssh"))
        && raw_tokens.len() > 1
    {
        raw_tokens.pop();
    }
    if raw_tokens.len() >= 2 && raw_tokens[0].eq_ignore_ascii_case("spark") {
        return format!("spark-{}", raw_tokens[1].to_ascii_lowercase());
    }
    let mut norm: Vec<String> = Vec::with_capacity(raw_tokens.len() + 1);
    for (idx, &tok) in raw_tokens.iter().enumerate() {
        let low = tok.to_ascii_lowercase();
        let mapped = if idx == 0
            && low.starts_with('m')
            && low.len() > 1
            && low[1..].chars().all(|c| c.is_ascii_digit())
        {
            low
        } else if idx > 0
            && low.starts_with('m')
            && low.len() == 2
            && low.as_bytes()[1].is_ascii_digit()
        {
            tok.to_ascii_uppercase()
        } else {
            match low.as_str() {
                "macbook" => "MacBook".to_owned(),
                "air" => "Air".to_owned(),
                "pro" => "Pro".to_owned(),
                "max" => "Max".to_owned(),
                "ultra" => "Ultra".to_owned(),
                "mini" => "mini".to_owned(),
                "imac" => "iMac".to_owned(),
                "studio" => "Studio".to_owned(),
                "intel" => "Intel".to_owned(),
                "core" => "Core".to_owned(),
                "i3" | "i5" | "i7" | "i9" | "prime" => low,
                _ if low.ends_with("gb")
                    && low[..low.len() - 2].bytes().any(|b| b.is_ascii_digit())
                    && low[..low.len() - 2]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}GB", &tok[..tok.len() - 2])
                }
                _ if low.ends_with("tb")
                    && low[..low.len() - 2].bytes().any(|b| b.is_ascii_digit())
                    && low[..low.len() - 2]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}TB", &tok[..tok.len() - 2])
                }
                _ if low.ends_with("ghz")
                    && low[..low.len() - 3].bytes().any(|b| b.is_ascii_digit())
                    && low[..low.len() - 3]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}GHz", &tok[..tok.len() - 3])
                }
                _ => tok.to_owned(),
            }
        };
        norm.push(mapped);
    }
    if norm.len() >= 2
        && norm[0].starts_with('m')
        && norm[0].len() > 1
        && norm[0][1..].chars().all(|c| c.is_ascii_digit())
    {
        if norm[1] == "Air" {
            norm.insert(1, "MacBook".to_owned());
        } else if norm[1] == "MacBook"
            && norm.get(2).is_some_and(|t| {
                t != "Pro"
                    && t != "Air"
                    && (t.starts_with('M') || t.chars().next().is_some_and(|c| c.is_ascii_digit()))
            })
        {
            norm.insert(2, "Pro".to_owned());
        }
    }
    norm.join("-")
}

/// Set or rename any host (`selector` may be a tag like `m14`, a canonical or raw Bonjour name, an address, or an enrolled UUID) to `new_name` in `machines.toml`.
/// # Errors
/// One line if `new_name` is not a valid host name or the inventory cannot be written.
pub fn set_host_name(
    sys: &dyn System,
    paths: &Paths,
    selector: &str,
    new_name: &str,
) -> Result<String, String> {
    let sel = selector.trim();
    if sel.is_empty() {
        return Err("host selector cannot be empty".into());
    }
    if !plain_word(new_name) {
        return Err(format!(
            "name {new_name:?} must be letters, digits, `.`, `_` or `-`"
        ));
    }
    sys.create_dir_all(&paths.root, DIR_MODE)
        .map_err(|e| format!("{}: {e}", paths.root.display()))?;
    let path = paths.inventory();
    let text = match sys.read_file(&path) {
        Ok(bytes) => {
            String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => "schema = 1\n".to_owned(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let before = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let tag_key = host_tag_key(sel);
    if let Some(other) = before.hosts.iter().find(|h| {
        h.name.eq_ignore_ascii_case(new_name)
            && !h.name.eq_ignore_ascii_case(sel)
            && !h.uuid.eq_ignore_ascii_case(sel)
            && !h.address.eq_ignore_ascii_case(sel)
            && host_tag_key(&h.name) != tag_key
    }) {
        return Err(format!(
            "the name {new_name} is taken by the host with uuid {}",
            other.uuid
        ));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let root = doc.as_table_mut();

    if let Some(item) = root.get_mut("host")
        && let Some(old) = item.as_array_of_tables().cloned()
    {
        let mut rebuilt = ArrayOfTables::new();
        for table in old.iter() {
            let mut table = table.clone();
            let matches = table
                .get("name")
                .and_then(Item::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case(sel) || host_tag_key(n) == tag_key)
                || table
                    .get("uuid")
                    .and_then(Item::as_str)
                    .is_some_and(|u| u.eq_ignore_ascii_case(sel))
                || table
                    .get("address")
                    .and_then(Item::as_str)
                    .is_some_and(|a| a.eq_ignore_ascii_case(sel) || host_tag_key(a) == tag_key);
            if matches {
                set_str(&mut table, "name", new_name);
            }
            rebuilt.push(table);
        }
        *item = Item::ArrayOfTables(rebuilt);
    }

    if !root.contains_key("names") {
        root.insert("names", Item::Table(Table::new()));
    }
    let Some(names_table) = root.get_mut("names").and_then(Item::as_table_mut) else {
        return Err(format!("{}: `names` is not a table", path.display()));
    };
    set_str(names_table, &tag_key, new_name);
    if !sel.eq_ignore_ascii_case(&tag_key) {
        set_str(names_table, sel, new_name);
    }

    let out = doc.to_string();
    let _after = parse(&out).map_err(|e| format!("the edited inventory does not parse: {e}"))?;
    sys.write_file(&path, out.as_bytes(), FILE_MODE)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(tag_key)
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
