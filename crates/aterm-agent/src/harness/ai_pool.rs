// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader and canonical TOML representation for `~/.secrets/ai-pool.toml`
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §4.1, §6.3, §6.5, §12.3 C14).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
#[cfg(not(windows))]
use std::time::Duration;

use super::accounts::{self, Class, Cli, Vendor};
use crate::ai::connector::{Flow, Region, RowKeys};

/// Vault-delivered account pool file in `~/.secrets/` ([`aterm_secrets::entry_class::AI_POOL`]).
pub const AI_POOL_FILE: &str = aterm_secrets::entry_class::AI_POOL;

/// Maximum warm Macs per account in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
pub const MAX_WARM_SLOTS: usize = 3;

/// Holder word recorded in `held_by` when an account has no current warm Mac in `ai-pool.toml`.
pub const POOL_HOLDER_UNASSIGNED: &str = "pool";

/// Reason an account holds a warm slot in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotWhy {
    /// Pinned by the accounting sheet's `computer` cell.
    Pin,
    /// Assigned by the allocator's normal fill pass.
    Fill,
    /// Freed from a silent or `NotEnrolled` holder.
    Stranded,
    /// Single-slot probation assignment for a newly eligible Mac.
    Probation,
    /// Assigned after a walled Mac borrowed an idle account.
    Borrow,
}

impl SlotWhy {
    /// Canonical `ai-pool.toml` string for this slot reason.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Fill => "fill",
            Self::Stranded => "stranded",
            Self::Probation => "probation",
            Self::Borrow => "borrow",
        }
    }

    /// Parse a [`SlotWhy`] from its `ai-pool.toml` word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "pin" => Some(Self::Pin),
            "fill" => Some(Self::Fill),
            "stranded" => Some(Self::Stranded),
            "probation" => Some(Self::Probation),
            "borrow" => Some(Self::Borrow),
            _ => None,
        }
    }
}

/// A canonical `(N, "m<N>")` label only when `name` starts with `m<N>`
/// ([`aterm_types::machine_name::Number::leading`], the one rule).
pub use aterm_secrets::swarm_roster::leading_m_label;

/// This Mac's canonical swarm label and legacy aliases used to match `warm` sets in `ai-pool.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPoolIdentity {
    /// Active canonical `m<N>` label (e.g. `"m100"`).
    pub primary: String,
    /// Sorted legacy `m<N>` aliases (e.g. `["m32"]`), never duplicating `primary`.
    pub aliases: Vec<String>,
    /// True when the label came from `aterm-swarm-roster` (so hostname mid-string parsing never ran).
    pub from_roster: bool,
}

impl LocalPoolIdentity {
    /// Construct an identity with normalized, deduplicated aliases.
    #[must_use]
    pub fn new(primary: String, aliases: Vec<String>, from_roster: bool) -> Self {
        let mut clean = BTreeSet::new();
        for alias in aliases {
            if let Some((_, canon)) = leading_m_label(&alias)
                && canon != primary
            {
                clean.insert(canon);
            }
        }
        Self {
            primary,
            aliases: clean.into_iter().collect(),
            from_roster,
        }
    }

    /// True if `candidate` matches `self.primary` or any of `self.aliases`.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let Some((_, canon)) = leading_m_label(candidate) else {
            return candidate.eq_ignore_ascii_case(&self.primary);
        };
        canon == self.primary || self.aliases.iter().any(|a| a == &canon)
    }
}

/// Pure resolution of this Mac's [`LocalPoolIdentity`] (`DESIGN-alab-swarm` §6.5).
///
/// Once the swarm roster gives this Mac a label (by age recipient or by matching `config_machine`),
/// warm sets match by that label and its roster aliases, never by `normalize_machine_id` of any
/// hostname. Even without a roster, hostname fallback accepts only names that *start* with `m<N>`
/// (`leading_m_label`), so `"Mac-mini-M4"` is never `"m4"`.
#[must_use]
pub fn resolve_pool_identity_pure(
    roster_members: &[(String, String, Vec<String>)],
    recipient: Option<&str>,
    config_machine: Option<&str>,
    machine_override: Option<&str>,
    hostnames: &[String],
) -> Option<LocalPoolIdentity> {
    let find_by_label = |label: &str| {
        roster_members.iter().find(|(_, primary, aliases)| {
            primary == label || aliases.iter().any(|alias| alias == label)
        })
    };

    if let Some(raw) = machine_override {
        let norm = leading_m_label(raw)
            .map(|(_, l)| l)
            .or_else(|| super::accounts_sync::normalize_machine_id(raw))?;
        if let Some((_, primary, aliases)) = find_by_label(&norm) {
            let mut all_aliases = aliases.clone();
            if *primary != norm {
                all_aliases.push(primary.clone());
            }
            return Some(LocalPoolIdentity::new(norm, all_aliases, true));
        }
        return Some(LocalPoolIdentity::new(norm, Vec::new(), false));
    }

    if let Some(rec) = recipient
        && let Some((_, primary, aliases)) = roster_members.iter().find(|(r, _, _)| r == rec)
    {
        return Some(LocalPoolIdentity::new(
            primary.clone(),
            aliases.clone(),
            true,
        ));
    }

    if let Some(cfg_raw) = config_machine
        && let Some(cfg_norm) = leading_m_label(cfg_raw)
            .map(|(_, l)| l)
            .or_else(|| super::accounts_sync::normalize_machine_id(cfg_raw))
    {
        if let Some((_, primary, aliases)) = find_by_label(&cfg_norm) {
            return Some(LocalPoolIdentity::new(
                primary.clone(),
                aliases.clone(),
                true,
            ));
        }
        return Some(LocalPoolIdentity::new(
            cfg_norm,
            Vec::new(),
            !roster_members.is_empty(),
        ));
    }

    if !roster_members.is_empty() {
        return None;
    }

    for host in hostnames {
        if let Some((_, canon)) = leading_m_label(host) {
            return Some(LocalPoolIdentity::new(canon, Vec::new(), false));
        }
    }
    None
}

/// Resolve this Mac's [`LocalPoolIdentity`] from the local vault state, `aterm.toml`, and system
/// hostnames.
///
/// # Errors
/// The roster cannot be read, including a writer still holding its lock after the bounded wait.
pub fn resolve_local_pool_identity(
    home: Option<&Path>,
    config: Option<&Path>,
    machine_override: Option<&str>,
) -> Result<Option<LocalPoolIdentity>, String> {
    #[cfg(unix)]
    let roster = home
        .map(|home| read_pass_roster(&aterm_secrets::client::Paths::for_home(home)))
        .transpose()?
        .flatten();
    Ok(resolve_local_pool_identity_with_roster(
        home,
        config,
        machine_override,
        #[cfg(unix)]
        roster,
    ))
}

pub(super) fn resolve_local_pool_identity_with_roster(
    home: Option<&Path>,
    config: Option<&Path>,
    machine_override: Option<&str>,
    #[cfg(unix)] roster: Option<aterm_secrets::swarm_roster::SwarmRoster>,
) -> Option<LocalPoolIdentity> {
    let config_machine = read_config_machine_raw(config);
    #[cfg(unix)]
    let (roster_members, recipient, vault_fallback, deny_labels, recipient_denied) =
        if let Some(home) = home {
            let paths = aterm_secrets::client::Paths::for_home(home);
            let status_opt = aterm_secrets::client::status(&paths).ok();
            let rec_obj = status_opt
                .as_ref()
                .and_then(|s| s.machine.as_ref())
                .map(|(rec, _)| *rec);
            let rec = rec_obj
                .as_ref()
                .map(aterm_secrets::age::Recipient::to_bech32);
            let config_host = status_opt
                .as_ref()
                .and_then(|s| s.config.as_ref())
                .map(|c| c.host.clone())
                .filter(|h| !h.trim().is_empty());
            let mut deny_labels = Vec::new();
            let mut recipient_denied = status_opt.as_ref().is_some_and(|s| {
                matches!(
                    s.membership,
                    aterm_secrets::client::Membership::Revoked { .. }
                )
            })
                || aterm_secrets::client::local_recipient_denied_in_roster(&paths);
            let members = roster
                .map(|mut r| {
                    if let Some(my_rec) = rec.as_deref()
                        && let Some(rm) = r.member_by_recipient(my_rec)
                    {
                        if r.is_member_denied(rm) {
                            recipient_denied = true;
                        }
                    } else if config_host.as_deref().is_some_and(|h| r.is_label_denied(h)) {
                        recipient_denied = true;
                    }
                    let members = r
                        .members
                        .iter()
                        .filter(|m| !r.is_member_denied(m))
                        .map(|m| (m.recipient.clone(), m.label.clone(), m.aliases.clone()))
                        .collect::<Vec<_>>();
                    deny_labels = std::mem::take(&mut r.deny_labels);
                    members
                })
                .unwrap_or_default();
            let fallback = if members.is_empty() && !recipient_denied {
                aterm_secrets::client::resolve_local_swarm_label(&paths, &[])
                    .ok()
                    .flatten()
            } else {
                None
            };
            (members, rec, fallback, deny_labels, recipient_denied)
        } else {
            (Vec::new(), None, None, Vec::new(), false)
        };
    #[cfg(not(unix))]
    let (roster_members, recipient, vault_fallback, deny_labels, recipient_denied) = {
        let _ = home;
        let members: Vec<(String, String, Vec<String>)> = Vec::new();
        (
            members,
            None::<String>,
            None::<String>,
            Vec::<String>::new(),
            false,
        )
    };

    if machine_override.is_none() && recipient_denied {
        return None;
    }
    let filter_denied = |mut id: LocalPoolIdentity| -> Option<LocalPoolIdentity> {
        if machine_override.is_none()
            && deny_labels
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&id.primary))
        {
            return None;
        }
        id.aliases
            .retain(|a| !deny_labels.iter().any(|d| d.eq_ignore_ascii_case(a)));
        Some(id)
    };

    let effective_config = config_machine.or(vault_fallback);
    if let Some(resolved) = resolve_pool_identity_pure(
        &roster_members,
        recipient.as_deref(),
        effective_config.as_deref(),
        machine_override,
        &[],
    )
    .and_then(filter_denied)
    {
        return Some(resolved);
    }
    if !roster_members.is_empty() || machine_override.is_some() {
        return None;
    }
    let hosts = system_hostnames();
    resolve_pool_identity_pure(
        &roster_members,
        recipient.as_deref(),
        effective_config.as_deref(),
        machine_override,
        &hosts,
    )
    .and_then(filter_denied)
}

/// A background pass waits briefly for a vault apply; contention is never an empty roster.
#[cfg(unix)]
pub(crate) fn read_pass_roster(
    paths: &aterm_secrets::client::Paths,
) -> Result<Option<aterm_secrets::swarm_roster::SwarmRoster>, String> {
    read_pass_roster_until(
        paths,
        std::time::Instant::now() + Duration::from_millis(250),
    )
}

#[cfg(unix)]
pub(super) fn read_pass_roster_until(
    paths: &aterm_secrets::client::Paths,
    deadline: std::time::Instant,
) -> Result<Option<aterm_secrets::swarm_roster::SwarmRoster>, String> {
    #[cfg(any(test, debug_assertions))]
    if UNWAITED_ROSTER.with(std::cell::Cell::get) {
        return aterm_secrets::client::test_unwaited_roster(paths).map_err(|why| why.to_string());
    }
    aterm_secrets::client::swarm_roster_until(paths, deadline).map_err(|why| {
        if why.kind() == aterm_secrets::ErrorKind::VaultBusy {
            format!("swarm roster busy: {why}")
        } else {
            format!("swarm roster unread: {why}")
        }
    })
}

#[cfg(all(unix, any(test, debug_assertions)))]
thread_local! {
    static UNWAITED_ROSTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restore the old reader for a contention test's negative control.
#[cfg(all(unix, any(test, debug_assertions)))]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "negative control called by tests, retained in debug lab builds"
    )
)]
pub(super) fn test_without_roster_wait<R>(read: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNWAITED_ROSTER.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(UNWAITED_ROSTER.with(|flag| flag.replace(true)));
    read()
}

pub(crate) fn read_bounded_bytes(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(
        usize::try_from(meta.len())
            .unwrap_or(0)
            .min(cap)
            .saturating_add(1),
    );
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= cap).then_some(bytes)
}

pub(crate) fn read_bounded_text(path: &Path, max_bytes: u64) -> Option<String> {
    String::from_utf8(read_bounded_bytes(path, max_bytes)?).ok()
}

/// The most of `ai-pool.toml` a reader takes.
const MAX_POOL_BYTES: u64 = 4 * 1024 * 1024;

/// The pool file under `home` (`~/.secrets/ai-pool.toml`) as it parses now; `None` where it is
/// absent, cannot be read, is larger than a pool file is, or is not one. It reads a file, so it
/// runs off a window's thread.
#[must_use]
pub fn read_under(home: &Path) -> Option<Pool> {
    let path = home.join(".secrets").join(AI_POOL_FILE);
    let text = read_bounded_text(&path, MAX_POOL_BYTES)?;
    Pool::from_toml(&text, &path.display().to_string()).ok()
}

fn read_config_machine_raw(config: Option<&Path>) -> Option<String> {
    let text = read_bounded_text(config?, 2 * 1024 * 1024)?;
    let table = aterm_toml::from_str::<aterm_toml::Table>(&text).ok()?;
    for (section, key) in [
        ("swarm", "label"),
        ("ai", "machine"),
        ("fleet", "machine"),
        ("harness", "machine"),
    ] {
        if let Some(m) = table
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get(key))
            .and_then(aterm_toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(m.to_owned());
        }
    }
    None
}

/// This Mac's host names (`LocalHostName`, `ComputerName`, `hostname`). None in this crate's tests,
/// which name the Mac by a scratch `aterm.toml` or roster: on a fleet Mac the host's own `m<N>`
/// name would be a member more than a test counts (7544d9d046, 9271393b1b).
fn system_hostnames() -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    // Windows names itself in process: it has no `scutil` and no `/bin/hostname` to start.
    #[cfg(windows)]
    {
        super::accounts_sync::windows_host_names()
    }
    #[cfg(not(windows))]
    {
        let mut out = Vec::new();
        for (prog, args) in [
            ("/usr/sbin/scutil", &["--get", "LocalHostName"][..]),
            ("/usr/sbin/scutil", &["--get", "ComputerName"][..]),
            ("/bin/hostname", &[][..]),
        ] {
            let mut cmd = aterm_uds::spawnfd::command(prog);
            cmd.args(args);
            if let Ok(cap) = super::align::capture_bounded(cmd, Duration::from_secs(2), 256)
                && cap.code == Some(0)
                && let Ok(s) = std::str::from_utf8(&cap.stdout)
            {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_owned());
                }
            }
        }
        out
    }
}

/// One account row in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolAccount {
    /// Pooled CLI this row belongs to.
    pub provider: Cli,
    /// Vendor keys (`region` for Kimi, `flow` for Codex/Grok).
    pub vendor: Vendor,
    /// Optional explicit roster label; when `None`, derived deterministically.
    pub label: Option<String>,
    /// Account login address (`name@domain`).
    pub email: String,
    /// Optional browser profile hint.
    pub browser: Option<String>,
    /// Up to [`MAX_WARM_SLOTS`] canonical `m<N>` labels in order; the first is the account's home.
    pub warm: Vec<String>,
    /// Per-slot assignment date/timestamp strings (parallel to `warm` when populated).
    pub since: Vec<String>,
    /// Per-slot assignment reasons (parallel to `warm` when populated).
    pub why: Vec<SlotWhy>,
    /// Per-slot last-used UTC day strings (parallel to `warm` when populated).
    pub used: Vec<String>,
}

impl PoolAccount {
    /// The account's home Mac (`warm[0]`), if any warm Mac is assigned.
    #[must_use]
    pub fn home_mac(&self) -> Option<&str> {
        self.warm.first().map(String::as_str)
    }

    /// Canonical holder word for `held_by` when this Mac is not warm for the account.
    #[must_use]
    pub fn holder_word(&self) -> &str {
        self.home_mac().unwrap_or(POOL_HOLDER_UNASSIGNED)
    }

    /// True if `identity` (primary label or any alias) is in this account's `warm` set.
    #[must_use]
    pub fn is_warm_for(&self, identity: &LocalPoolIdentity) -> bool {
        self.warm.iter().any(|slot| identity.matches(slot))
    }

    /// The order the warm set implies (`Preferred` for ≤1 warm Mac, `Pool` for >1), never a limit.
    #[must_use]
    pub fn class(&self) -> Class {
        if self.warm.len() > 1 {
            Class::Pool
        } else {
            Class::Preferred
        }
    }

    /// Canonical provider string written to `ai-pool.toml`.
    #[must_use]
    pub fn provider_str(&self) -> &'static str {
        match (self.provider, self.vendor.region) {
            (Cli::Claude, _) => "claude",
            (Cli::Codex, _) => "codex",
            (Cli::Grok, _) => "grok",
            (Cli::Agy, _) => "agy",
            (Cli::Kimi, Some(Region::MainlandCn)) => "kimi:mainland-cn",
            (Cli::Kimi, Some(Region::Global) | None) => "kimi:global",
        }
    }
}

/// The row of `pool` for `cli`'s account whose login address is `email`
/// ([`accounts::email_matches`]): where its home Mac, its other warm Macs and each slot's reason
/// and date are. `None` where the file lists no such account. For Kimi, whose two regions may
/// share an address, the first row listed.
#[must_use]
pub fn home_of_email<'a>(pool: &'a Pool, cli: Cli, email: &str) -> Option<&'a PoolAccount> {
    pool.accounts
        .iter()
        .find(|account| account.provider == cli && accounts::email_matches(&account.email, email))
}

/// Complete parsed `ai-pool.toml` document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pool {
    /// Human-readable source path or description.
    pub source: String,
    /// Per-label last-heard UTC day (`"m22" -> "2026-10-27"`), carried across lease handoffs.
    pub last_heard: BTreeMap<String, String>,
    /// All pooled accounts across providers.
    pub accounts: Vec<PoolAccount>,
}

impl Pool {
    /// True if `chosen` or `raw` is an `ai-pool.toml` document rather than a legacy
    /// `fleet-accounts.toml` or CSV sheet.
    #[must_use]
    pub fn looks_like_pool(chosen: &str, raw: &str) -> bool {
        if Path::new(chosen)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == AI_POOL_FILE)
        {
            return true;
        }
        let Ok(table) = aterm_toml::from_str::<aterm_toml::Table>(raw) else {
            return false;
        };
        if table.get("last_heard").is_some() {
            return true;
        }
        table
            .get("account")
            .and_then(aterm_toml::Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|item| {
                    item.as_table()
                        .is_some_and(|t| t.get("warm").is_some() || t.get("provider").is_some())
                })
            })
    }

    /// Parse `ai-pool.toml` text into a validated [`Pool`].
    ///
    /// # Errors
    /// Returns a human-readable error if the TOML is malformed, contains invalid labels/emails,
    /// exceeds [`MAX_WARM_SLOTS`] on any row, or duplicates an email in the same provider realm.
    pub fn from_toml(text: &str, source: &str) -> Result<Self, String> {
        let table: aterm_toml::Table = aterm_toml::from_str(text)
            .map_err(|e| format!("account pool at {source} is not valid TOML: {e}"))?;
        for key in table.keys() {
            if !matches!(
                key.as_str(),
                "version" | "last_heard" | "leases" | "lease" | "account"
            ) {
                return Err(format!(
                    "account pool at {source}: unknown top-level key `{key}`"
                ));
            }
        }
        if let Some(ver) = table.get("version") {
            let Some(v) = ver.as_integer() else {
                return Err(format!(
                    "account pool at {source}: `version` must be integer 1"
                ));
            };
            if v != 1 {
                return Err(format!(
                    "account pool at {source}: unsupported pool version `{v}`"
                ));
            }
        }
        let mut last_heard = BTreeMap::new();
        if let Some(lh_val) = table.get("last_heard") {
            let lh = lh_val
                .as_table()
                .ok_or_else(|| format!("account pool at {source}: `last_heard` must be a table"))?;
            for (raw_label, day_val) in lh {
                let (_, canon) = leading_m_label(raw_label).ok_or_else(|| {
                    format!("account pool at {source}: invalid `last_heard` label `{raw_label}`")
                })?;
                let day = day_val.as_str().ok_or_else(|| {
                    format!(
                        "account pool at {source}: `last_heard.{raw_label}` must be a date string"
                    )
                })?;
                last_heard.insert(canon, day.trim().to_owned());
            }
        }
        let mut accounts = Vec::new();
        let Some(raw_rows) = table.get("account") else {
            return Ok(Self {
                source: source.to_owned(),
                last_heard,
                accounts,
            });
        };
        let rows = raw_rows
            .as_array()
            .ok_or_else(|| format!("account pool at {source}: `account` must be `[[account]]`"))?;
        for (idx, item) in rows.iter().enumerate() {
            let n = idx + 1;
            let row = item
                .as_table()
                .ok_or_else(|| format!("account pool at {source} row {n}: expected table"))?;
            let acct = parse_pool_row(row, n, source)?;
            if accounts.iter().any(|prev: &PoolAccount| {
                prev.provider == acct.provider
                    && accounts::same_provider_realm(
                        acct.provider,
                        prev.vendor.region,
                        acct.vendor.region,
                    )
                    && accounts::email_matches(&prev.email, &acct.email)
            }) {
                return Err(format!(
                    "account pool at {source} row {n}: duplicate email `{}` in provider `{}`",
                    acct.email,
                    acct.provider_str()
                ));
            }
            if let Some(label) = &acct.label
                && accounts.iter().any(|prev: &PoolAccount| {
                    prev.provider == acct.provider && prev.label.as_ref() == Some(label)
                })
            {
                return Err(format!(
                    "account pool at {source} row {n}: duplicate label `{label}` in provider `{}`",
                    acct.provider.id()
                ));
            }
            accounts.push(acct);
        }
        Ok(Self {
            source: source.to_owned(),
            last_heard,
            accounts,
        })
    }

    /// Serialize this pool to canonical `ai-pool.toml` text.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        if !self.last_heard.is_empty() {
            out.push_str("[last_heard]\n");
            for (label, day) in &self.last_heard {
                out.push_str(&format!("{label} = {}\n", toml_str(day)));
            }
            out.push('\n');
        }
        for acct in &self.accounts {
            out.push_str("[[account]]\n");
            out.push_str(&format!("provider = {}\n", toml_str(acct.provider_str())));
            if let Some(label) = &acct.label {
                out.push_str(&format!("label = {}\n", toml_str(label)));
            }
            out.push_str(&format!("email = {}\n", toml_str(&acct.email)));
            if let Some(browser) = &acct.browser {
                out.push_str(&format!("browser = {}\n", toml_str(browser)));
            }
            if let Some(flow) = acct.vendor.flow {
                out.push_str(&format!("flow = {}\n", toml_str(flow.as_str())));
            }
            out.push_str(&format!("warm = {}\n", toml_str_array(&acct.warm)));
            if !acct.since.is_empty() {
                out.push_str(&format!("since = {}\n", toml_str_array(&acct.since)));
            }
            if !acct.why.is_empty() {
                let whys: Vec<String> = acct.why.iter().map(|w| w.as_str().to_owned()).collect();
                out.push_str(&format!("why = {}\n", toml_str_array(&whys)));
            }
            if !acct.used.is_empty() {
                out.push_str(&format!("used = {}\n", toml_str_array(&acct.used)));
            }
            out.push('\n');
        }
        out
    }

    /// Return all pool rows for `cli`, paired with deterministic, collision-free labels.
    #[must_use]
    pub fn accounts_for_cli(&self, cli: Cli) -> Vec<(&PoolAccount, String)> {
        let rows: Vec<&PoolAccount> = self.accounts.iter().filter(|a| a.provider == cli).collect();
        let mut used = BTreeSet::new();
        for acct in &rows {
            if let Some(explicit) = &acct.label {
                used.insert(explicit.clone());
            }
        }
        let mut out = Vec::with_capacity(rows.len());
        for (idx, acct) in rows.into_iter().enumerate() {
            if let Some(explicit) = &acct.label {
                out.push((acct, explicit.clone()));
                continue;
            }
            let base = default_pool_label_base(cli, acct, idx + 1);
            let mut candidate = base.clone();
            let mut suffix = 2u32;
            while !accounts::admissible_label(&candidate) || used.contains(&candidate) {
                candidate = format!("{base}-{suffix}");
                suffix = suffix.saturating_add(1);
            }
            used.insert(candidate.clone());
            out.push((acct, candidate));
        }
        out
    }
}

fn default_pool_label_base(cli: Cli, acct: &PoolAccount, ordinal: usize) -> String {
    let stem = if let Some(home) = acct.home_mac() {
        home.to_owned()
    } else if let Some(local) = acct.email.split('@').next() {
        let cleaned: String = local
            .bytes()
            .map(|b| {
                let lower = b.to_ascii_lowercase();
                if lower.is_ascii_alphanumeric() || matches!(lower, b'.' | b'_' | b'-') {
                    char::from(lower)
                } else {
                    '-'
                }
            })
            .collect();
        let trimmed = cleaned.trim_matches(['.', '_', '-']);
        if trimmed.is_empty() || !trimmed.as_bytes()[0].is_ascii_alphanumeric() {
            format!("pool-{ordinal}")
        } else {
            let capped = super::truncate_bytes(trimmed, 40);
            if capped == accounts::RESERVED_LABEL {
                format!("pool-{ordinal}")
            } else {
                capped.to_owned()
            }
        }
    } else {
        format!("pool-{ordinal}")
    };
    match cli {
        Cli::Claude => stem,
        _ => format!("{}-{stem}", cli.id()),
    }
}

fn parse_pool_row(row: &aterm_toml::Table, n: usize, source: &str) -> Result<PoolAccount, String> {
    for key in row.keys() {
        if !matches!(
            key.as_str(),
            "provider"
                | "label"
                | "email"
                | "browser"
                | "region"
                | "flow"
                | "warm"
                | "since"
                | "why"
                | "used"
        ) {
            return Err(format!(
                "account pool at {source} row {n}: unknown key `{key}`"
            ));
        }
    }
    let provider_raw = row
        .get("provider")
        .and_then(aterm_toml::Value::as_str)
        .unwrap_or("claude");
    let (provider, realm_region) = parse_provider_realm(provider_raw).ok_or_else(|| {
        format!("account pool at {source} row {n}: unknown provider `{provider_raw}`")
    })?;
    let email = row
        .get("email")
        .and_then(aterm_toml::Value::as_str)
        .ok_or_else(|| format!("account pool at {source} row {n}: missing `email`"))?
        .to_owned();
    if let Some(why) = accounts::email_refusal(&email) {
        return Err(format!("account pool at {source} row {n}: email {why}"));
    }
    let label = match row.get("label") {
        None => None,
        Some(aterm_toml::Value::String(s)) => {
            if let Some(why) = accounts::label_refusal(s) {
                return Err(format!(
                    "account pool at {source} row {n}: label `{s}` {why}"
                ));
            }
            Some(s.clone())
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `label` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let browser = match row.get("browser") {
        None => None,
        Some(aterm_toml::Value::String(s)) => {
            if let Some(why) = accounts::browser_refusal(s) {
                return Err(format!("account pool at {source} row {n}: browser {why}"));
            }
            Some(s.clone())
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `browser` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let explicit_region = match row.get("region") {
        None => None,
        Some(aterm_toml::Value::String(w)) => Some(
            Region::parse(w)
                .ok_or_else(|| format!("account pool at {source} row {n}: invalid region `{w}`"))?,
        ),
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `region` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let region = match provider.accounts().keys {
        RowKeys::Region => {
            if let (Some(a), Some(b)) = (realm_region, explicit_region)
                && a != b
            {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` conflicts with `region`"
                ));
            }
            Some(explicit_region.or(realm_region).ok_or_else(|| {
                format!(
                    "account pool at {source} row {n}: Kimi row requires `kimi:global` or `kimi:mainland-cn` (or `region`)"
                )
            })?)
        }
        _ => {
            if explicit_region.is_some() {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` takes no `region`"
                ));
            }
            None
        }
    };
    let flow = match row.get("flow") {
        None => None,
        Some(aterm_toml::Value::String(w)) => {
            if !matches!(provider.accounts().keys, RowKeys::Flow(_)) {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` takes no `flow`"
                ));
            }
            Some(
                Flow::parse(w).ok_or_else(|| {
                    format!("account pool at {source} row {n}: invalid flow `{w}`")
                })?,
            )
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `flow` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };

    let raw_warm = string_vec(row, "warm", n, source)?.unwrap_or_default();
    if raw_warm.len() > MAX_WARM_SLOTS {
        return Err(format!(
            "account pool at {source} row {n}: `warm` has {} entries (max {MAX_WARM_SLOTS})",
            raw_warm.len()
        ));
    }
    let mut warm = Vec::with_capacity(raw_warm.len());
    let mut seen_warm = BTreeSet::new();
    for entry in raw_warm {
        let (_, canon) = leading_m_label(&entry).ok_or_else(|| {
            format!("account pool at {source} row {n}: invalid warm label `{entry}`")
        })?;
        if !seen_warm.insert(canon.clone()) {
            return Err(format!(
                "account pool at {source} row {n}: duplicate warm label `{canon}`"
            ));
        }
        warm.push(canon);
    }

    let since = string_vec(row, "since", n, source)?.unwrap_or_default();
    if !since.is_empty() && since.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `since` length ({}) does not match `warm` ({})",
            since.len(),
            warm.len()
        ));
    }
    let raw_why = string_vec(row, "why", n, source)?.unwrap_or_default();
    if !raw_why.is_empty() && raw_why.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `why` length ({}) does not match `warm` ({})",
            raw_why.len(),
            warm.len()
        ));
    }
    let mut why = Vec::with_capacity(raw_why.len());
    for word in raw_why {
        why.push(SlotWhy::parse(&word).ok_or_else(|| {
            format!("account pool at {source} row {n}: unknown `why` reason `{word}`")
        })?);
    }
    let used = string_vec(row, "used", n, source)?.unwrap_or_default();
    if !used.is_empty() && used.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `used` length ({}) does not match `warm` ({})",
            used.len(),
            warm.len()
        ));
    }

    Ok(PoolAccount {
        provider,
        vendor: Vendor { region, flow },
        label,
        email,
        browser,
        warm,
        since,
        why,
        used,
    })
}

fn parse_provider_realm(raw: &str) -> Option<(Cli, Option<Region>)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Some((Cli::Claude, None)),
        "codex" | "openai" => Some((Cli::Codex, None)),
        "grok" | "x" | "xai" => Some((Cli::Grok, None)),
        "agy" | "antigravity" => Some((Cli::Agy, None)),
        "kimi" => Some((Cli::Kimi, None)),
        "kimi:global" | "kimi-global" => Some((Cli::Kimi, Some(Region::Global))),
        "kimi:mainland-cn" | "kimi-mainland-cn" | "kimi:cn" | "kimi-cn" => {
            Some((Cli::Kimi, Some(Region::MainlandCn)))
        }
        _ => None,
    }
}

fn string_vec(
    row: &aterm_toml::Table,
    key: &str,
    n: usize,
    source: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(val) = row.get(key) else {
        return Ok(None);
    };
    let arr = val.as_array().ok_or_else(|| {
        format!(
            "account pool at {source} row {n}: `{key}` is a TOML {}, not an array",
            val.type_str()
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let s = item.as_str().ok_or_else(|| {
            format!(
                "account pool at {source} row {n}: `{key}[{}]` is a TOML {}, not a string",
                i + 1,
                item.type_str()
            )
        })?;
        out.push(s.to_owned());
    }
    Ok(Some(out))
}

fn toml_str(s: &str) -> String {
    aterm_toml::Value::String(s.to_owned()).to_string()
}

fn toml_str_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", parts.join(", "))
}

#[cfg(test)]
#[path = "ai_pool_tests.rs"]
pub(super) mod tests;
