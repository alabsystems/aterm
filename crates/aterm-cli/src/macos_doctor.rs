// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's macOS workstation profile. Preference changes go through cfprefsd's
//! `defaults` client; the recovery journal contains only the keys this profile owns.

use aterm_json::{Value, json};
use aterm_machines::init::WorkstationGroup;
use aterm_machines::{model::PValue, plist};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PROFILE: &str = "macos-workstation-v1";

/// The operation requested by `aterm doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Read preferences, without creating state or changing the desktop.
    Check,
    /// Back up and apply the workstation profile.
    Apply,
    /// Restore the original values and disable the automatic first-run apply.
    Restore,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Apply => "apply",
            Self::Restore => "restore",
        }
    }
}

/// One preference's observed outcome.
#[derive(Debug)]
pub struct Row {
    /// Human-readable setting name.
    pub name: String,
    /// `ok`, `drift`, `applied`, `restored`, `blocked`, `unverified`, or `error`.
    pub status: String,
    /// Action or failure detail, without account identifiers.
    pub detail: String,
}

/// The machine-readable and human-readable result of a profile pass.
#[derive(Debug)]
pub struct Report {
    /// The requested operation.
    pub action: Action,
    /// Outcomes for settings this tool can configure.
    pub rows: Vec<Row>,
    /// Supported UI steps that require the account owner.
    pub manual_steps: Vec<String>,
    /// Original per-key values retained for `--restore`.
    pub backup: PathBuf,
}

impl Report {
    /// Whether a setting or account remains out of policy, blocked, or unverified.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.rows.iter().any(|r| {
            matches!(
                r.status.as_str(),
                "drift" | "error" | "blocked" | "unverified"
            )
        })
    }

    /// A compact JSON report, suitable for fleet automation.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                json!({
                    "name": r.name, "status": r.status, "detail": r.detail,
                })
            })
            .collect();
        aterm_json::to_string(&json!({
            "profile": PROFILE, "action": self.action.name(),
            "needs_attention": self.needs_attention(), "settings": rows,
            "manual_steps": self.manual_steps,
            "backup": self.backup.to_string_lossy().as_ref(),
        }))
        .unwrap_or_else(|e| format!("{{\"error\":\"JSON report: {e}\"}}"))
    }

    /// The same report for a terminal.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = format!("macOS workstation profile ({})\n", self.action.name());
        for row in &self.rows {
            text.push_str(&format!(
                "  {}: {} — {}\n",
                row.name, row.status, row.detail
            ));
        }
        for step in &self.manual_steps {
            text.push_str(&format!("  owner action: {step}\n"));
        }
        text.push_str(&format!("  backup: {}\n", self.backup.display()));
        text
    }
}

/// Check, apply, or restore the macOS workstation profile for the actual user.
/// An isolated instance never changes the login user's machine preferences.
pub fn run(action: Action) -> Result<Report, String> {
    let root = state_root()?;
    let mut host = NativeHost::new();
    if action == Action::Check {
        return execute(&mut host, &root, action);
    }
    ensure_person_scope()?;
    let _lock = HeldLock::take(&root)?;
    execute(&mut host, &root, action)
}

/// Apply once on the first real interactive launch. A restored or successfully
/// configured machine is left alone on later launches; explicit apply repairs drift.
pub fn apply_on_first_run() -> Result<Option<Report>, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    ensure_person_scope()?;
    let root = state_root()?;
    let Some(_lock) = HeldLock::try_take(&root)? else {
        return Ok(None);
    };
    if first_run_complete(&root)? {
        return Ok(None);
    }
    let sys = aterm_machines::port::Live::new();
    let accessibility = aterm_machines::init_accounts::accessibility_ready(&sys).ok();
    if !automatic_retry_due(&root, accessibility)? {
        return Ok(None);
    }
    write_json(
        &root.join("automatic-attempt.json"),
        &json!({ "profile": PROFILE, "accessibility_ready": accessibility }),
    )?;
    let consent = root.join("accessibility-request.json");
    let mut request_failure = None;
    if accessibility != Some(true)
        && !consent
            .try_exists()
            .map_err(|e| format!("{}: {e}", consent.display()))?
        && aterm_machines::init_accounts::accessibility_needed(&sys).unwrap_or(false)
    {
        // Record the request before showing native consent. Crashes and the
        // retry worker must not keep reopening the same permission prompt.
        write_json(
            &consent,
            &json!({ "profile": PROFILE, "state": "requested" }),
        )?;
        if let Err(detail) = aterm_machines::init_accounts::request_accessibility(&sys) {
            request_failure = Some(detail);
        }
    }
    let mut report = execute(&mut NativeHost::new(), &root, Action::Apply)?;
    if report.needs_attention()
        && let Some(detail) = request_failure
    {
        report.rows.push(Row {
            name: "Native account permission request".into(),
            status: "blocked".into(),
            detail,
        });
    }
    Ok(Some(report))
}

/// Whether first-run setup was fully verified or explicitly restored. This
/// reads only aterm's marker, so a background worker can await a pending retry.
pub fn first_run_finished() -> Result<bool, String> {
    first_run_complete(&state_root()?)
}

fn first_run_complete(root: &Path) -> Result<bool, String> {
    let path = root.join("first-run.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let marker: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("first-run marker: {e}"))?;
    if marker["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized first-run profile".into());
    }
    Ok(marker["state"].as_str() == Some("restored")
        || (marker["state"].as_str() == Some("applied")
            && marker["accounts_verified"].as_bool() == Some(true)))
}

// A permission refusal should not reopen account settings on each launch. An
// explicit --apply always retries; automatic setup retries after a short crash
// cooldown, or immediately when Accessibility permission changed.
fn automatic_retry_due(root: &Path, accessibility: Option<bool>) -> Result<bool, String> {
    let path = root.join("automatic-attempt.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let marker: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("automatic attempt: {e}"))?;
    if marker["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic-attempt profile".into());
    }
    if marker["accessibility_ready"].as_bool() != accessibility {
        return Ok(true);
    }
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("{}: {e}", path.display()))?
        .elapsed();
    Ok(age.map_or(true, |age| age >= std::time::Duration::from_secs(60)))
}

fn state_root() -> Result<PathBuf, String> {
    aterm_types::dirs::state_dir()
        .map(|d| d.join("doctor"))
        .ok_or_else(|| "cannot resolve aterm's state directory".to_owned())
}

/// Whether this process may change the person's macOS profile.
#[derive(Debug, PartialEq, Eq)]
enum PersonScope {
    /// It may.
    Person,
    /// It never may: an isolated instance, sudo, or a HOME that is not the
    /// account's. Each is fixed for the life of the process.
    Refused(String),
    /// The account home or HOME could not be read just now. A later pass may.
    Unresolved(String),
}

impl PersonScope {
    /// The refusal fixed for the life of the process ([`lasting_scope_refusal`]).
    fn lasting(self) -> Option<String> {
        match self {
            Self::Refused(why) => Some(why),
            Self::Person | Self::Unresolved(_) => None,
        }
    }
}

// `defaults` selects its account from the process uid, not $HOME. In particular,
// a lab's scratch HOME is not a sandbox around the real person's preferences.
fn person_scope() -> PersonScope {
    let uid = aterm_uds::peer::our_uid();
    scope_of(
        aterm_types::dirs::runs_under_private_state_root(),
        uid,
        aterm_types::dirs::home_dir().as_deref(),
        || account_home(uid),
    )
}

/// The pure half of [`person_scope`]: `private` whether the process runs under a
/// private state root, `uid` its uid, `home` its `$HOME`, and `account_home` the
/// account record's home, read only once the rest admits the process.
fn scope_of(
    private: bool,
    uid: u32,
    home: Option<&Path>,
    account_home: impl FnOnce() -> Result<PathBuf, String>,
) -> PersonScope {
    if private {
        return PersonScope::Refused(
            "macOS profile changes are disabled for an isolated aterm instance".into(),
        );
    }
    if uid == 0 {
        return PersonScope::Refused(
            "run the macOS profile as the logged-in account, without sudo".into(),
        );
    }
    let Some(home) = home else {
        return PersonScope::Unresolved("cannot resolve HOME".into());
    };
    let canonical = account_home().and_then(|actual_home| {
        let canon =
            |p: &Path| std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
        Ok((canon(home)?, canon(&actual_home)?))
    });
    match canonical {
        Err(why) => PersonScope::Unresolved(why),
        Ok((home, actual_home)) if home != actual_home => PersonScope::Refused(
            "macOS profile changes are disabled when HOME differs from the account's home".into(),
        ),
        Ok(_) => PersonScope::Person,
    }
}

fn ensure_person_scope() -> Result<(), String> {
    match person_scope() {
        PersonScope::Person => Ok(()),
        PersonScope::Refused(why) | PersonScope::Unresolved(why) => Err(why),
    }
}

/// Why this process may never change the person's macOS profile: an isolated
/// instance, sudo, or a HOME that is not the account's, each fixed for the life
/// of the process, so a caller that gets one need not ask again in it. `None`
/// when it may, and when the account home or HOME cannot be read just now: that
/// passing failure is the profile pass's own error, which it reports and retries.
pub fn lasting_scope_refusal() -> Option<String> {
    person_scope().lasting()
}

fn account_home(uid: u32) -> Result<PathBuf, String> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt as _;
    let mut storage = vec![0u8; 65_536];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    // SAFETY: the result and scratch buffers are live and exclusively borrowed
    // for the call; getpwuid_r's strings point into storage until copied below.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            storage.as_mut_ptr().cast(),
            storage.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return Err(format!(
            "cannot resolve the account home (getpwuid_r: {status})"
        ));
    }
    // SAFETY: a non-null successful result initialized entry.
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() {
        return Err("the account has no home directory".into());
    }
    // SAFETY: getpwuid_r's non-null dir is NUL-terminated and points into storage.
    let dir = unsafe { CStr::from_ptr(entry.pw_dir) };
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

struct HeldLock(std::fs::File);

impl HeldLock {
    fn take(root: &Path) -> Result<Self, String> {
        Self::try_take(root)?.ok_or_else(|| "another doctor profile pass is running".into())
    }

    fn try_take(root: &Path) -> Result<Option<Self>, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(root.join("profile.lock"))
            .map_err(|e| format!("doctor lock: {e}"))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "HeldLock::drop releases by LOCK_UN before closing, including child-spawn paths"
        )]
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(format!("doctor profile lock: {e}")),
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    domain: String,
    key: String,
    host: bool,
}

impl Target {
    #[cfg(test)]
    fn new(domain: &str, key: &str) -> Self {
        Self {
            domain: domain.into(),
            key: key.into(),
            host: false,
        }
    }

    fn describe(&self) -> String {
        format!(
            "{}{} {}",
            if self.host { "-currentHost " } else { "" },
            self.domain,
            self.key
        )
    }
}

trait Host {
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String>;
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String>;
    fn preference_changes(&mut self) -> Result<Vec<Saved>, String>;
    fn attempted_preferences(&mut self) -> Vec<Saved>;
    fn restart(&mut self, name: &str) -> Result<(), String>;
    fn workstation(&mut self, group: WorkstationGroup, apply: bool) -> Result<Vec<Row>, String>;
    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<Row>;
}

struct NativeHost {
    domains: BTreeMap<(String, bool), plist::Entries>,
    prepared: Option<aterm_machines::init::Workstation>,
    attempted: Vec<Saved>,
}

impl NativeHost {
    fn new() -> Self {
        Self {
            domains: BTreeMap::new(),
            prepared: None,
            attempted: Vec::new(),
        }
    }

    fn entries(&mut self, domain: &str, host: bool) -> Result<&plist::Entries, String> {
        let cache_key = (domain.to_owned(), host);
        if !self.domains.contains_key(&cache_key) {
            let mut args = Vec::new();
            if host {
                args.push("-currentHost".into());
            }
            args.extend(["export".into(), domain.into(), "-".into()]);
            let out = tool("/usr/bin/defaults", &args)?;
            self.domains
                .insert(cache_key.clone(), plist::parse_domain(&out)?);
        }
        self.domains
            .get(&cache_key)
            .ok_or_else(|| "missing cached preference domain".into())
    }
}

impl Host for NativeHost {
    fn attempted_preferences(&mut self) -> Vec<Saved> {
        std::mem::take(&mut self.attempted)
    }
    fn preference_changes(&mut self) -> Result<Vec<Saved>, String> {
        use aterm_machines::items::defaults::Scope;
        use aterm_machines::model::Read;
        let sys = aterm_machines::port::Live::new();
        let mut prepared =
            aterm_machines::init::prepare_workstation(&sys, WorkstationGroup::Preferences)?;
        let changes = prepared
            .preference_changes(&sys)?
            .into_iter()
            .map(|(target, read)| {
                if target.scope == Scope::System {
                    return Err(
                        "workstation repair unexpectedly targets a system preference".into(),
                    );
                }
                let value = match read {
                    Read::Absent => None,
                    Read::Value(v) => Some(v),
                    Read::Unusable(why) => return Err(why),
                };
                Ok(Saved {
                    target: Target {
                        domain: target.domain,
                        key: target.key,
                        host: target.scope == Scope::Host,
                    },
                    value,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.prepared = Some(prepared);
        Ok(changes)
    }
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String> {
        self.entries(&target.domain, target.host)?
            .iter()
            .find(|(key, _)| *key == target.key)
            .map(|(_, value)| value.clone())
            .transpose()
            .map_err(|e| format!("{}: {e}", target.describe()))
    }

    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
        let mut args = Vec::new();
        if target.host {
            args.push("-currentHost".into());
        }
        args.extend([
            if value.is_some() { "write" } else { "delete" }.into(),
            target.domain.clone(),
            target.key.clone(),
        ]);
        if let Some(value) = value {
            args.push(plist::fragment(value));
        }
        // A timed-out or failed client may already have changed cfprefsd's
        // value. Rollback must reread the machine, never the before-image cache.
        self.domains.remove(&(target.domain.clone(), target.host));
        tool("/usr/bin/defaults", &args)?;
        Ok(())
    }

    fn restart(&mut self, name: &str) -> Result<(), String> {
        let uid = aterm_uds::peer::our_uid().to_string();
        // An absent process needs no restart. killall's only expected failure is 1.
        let out = tool_output("/usr/bin/killall", &["-u".into(), uid, name.into()])?;
        if out.status.success() || out.status.code() == Some(1) {
            Ok(())
        } else {
            Err(format!(
                "restart {name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn workstation(&mut self, group: WorkstationGroup, apply: bool) -> Result<Vec<Row>, String> {
        use aterm_machines::model::{Mode, Outcome};
        let mode = if apply { Mode::Apply } else { Mode::Plan };
        let sys = aterm_machines::port::Live::new();
        let prepared = if group == WorkstationGroup::Preferences {
            match self.prepared.take() {
                Some(prepared) => prepared,
                None => aterm_machines::init::prepare_workstation(&sys, group)?,
            }
        } else {
            aterm_machines::init::prepare_workstation(&sys, group)?
        };
        self.domains.clear();
        let rows = if apply && group == WorkstationGroup::Preferences {
            use aterm_machines::items::defaults::Scope;
            use aterm_machines::model::Read;
            let result = prepared.run_recording(&sys, mode)?;
            self.attempted = result
                .attempted_preferences
                .into_iter()
                .map(|(target, read)| {
                    let value = match read {
                        Read::Absent => None,
                        Read::Value(value) => Some(value),
                        Read::Unusable(why) => return Err(why),
                    };
                    Ok(Saved {
                        target: Target {
                            domain: target.domain,
                            key: target.key,
                            host: target.scope == Scope::Host,
                        },
                        value,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            result.rows
        } else {
            prepared.run(&sys, mode)?
        };
        self.domains.clear();
        Ok({
            rows.into_iter()
                .map(|row| {
                    let status = match row.outcome {
                        Outcome::Ok | Outcome::NotApplicable => "ok",
                        Outcome::Changed => "applied",
                        Outcome::Planned => "drift",
                        Outcome::Person | Outcome::Blocked | Outcome::Deferred => "blocked",
                        Outcome::Unknown => "unverified",
                        Outcome::Failed => "error",
                    };
                    Row {
                        name: row.title,
                        status: status.into(),
                        detail: row
                            .why
                            .or(row.to)
                            .unwrap_or_else(|| "workstation item completed".into()),
                    }
                })
                .collect()
        })
    }

    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<Row> {
        use aterm_machines::port::System as _;
        let mut rows = Vec::new();
        if let Some(original) = saved.iter().find(|s| {
            !s.target.host
                && s.target.domain == "-g"
                && s.target.key == "com.apple.swipescrolldirection"
        }) {
            let value = match original.value.as_ref() {
                Some(PValue::Bool(value)) => Some(*value),
                Some(PValue::Int(0)) => Some(false),
                Some(PValue::Int(1)) => Some(true),
                _ => None,
            };
            if let Some(value) = value {
                let sys = aterm_machines::port::Live::new();
                let result = sys.set_natural_scrolling(value).and_then(|()| {
                    if sys.natural_scrolling()? == value {
                        Ok(())
                    } else {
                        Err("the original scroll direction did not activate".into())
                    }
                });
                rows.push(match result {
                    Ok(()) => Row {
                        name: "Original scroll direction".into(),
                        status: "restored".into(),
                        detail: "saved and active scroll direction restored".into(),
                    },
                    Err(detail) => Row {
                        name: "Original scroll direction".into(),
                        status: "error".into(),
                        detail,
                    },
                });
            } else {
                rows.push(Row { name: "Original active scroll direction".into(), status: "blocked".into(), detail: "the previous implicit or non-boolean setting has no recorded runtime value; log out and back in to load its restored preference".into() });
            }
        }
        if saved.iter().any(|s| {
            s.target.host
                && s.target
                    .key
                    .starts_with("com.apple.keyboard.modifiermapping.")
        }) {
            rows.push(Row { name: "Original active keyboard mapping".into(), status: "blocked".into(), detail: "saved mappings restored; the previous live HID service mappings were not recorded, so log out and back in to reload them".into() });
        }
        rows
    }
}

fn tool_output(program: &str, args: &[String]) -> Result<std::process::Output, String> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args).env("LC_ALL", "C");
    aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .map_err(|e| format!("{program}: {e}"))
}

fn tool(program: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let out = tool_output(program, args)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[derive(Clone)]
struct Saved {
    target: Target,
    value: Option<PValue>,
}

fn load_backup(path: &Path) -> Result<Vec<Saved>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes).map_err(|e| format!("backup: {e}"))?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized doctor backup profile".into());
    }
    let keys = value["keys"]
        .as_array()
        .ok_or("doctor backup has no keys")?;
    keys.iter()
        .map(|entry| {
            let domain = entry["domain"].as_str().ok_or("backup domain is missing")?;
            let key = entry["key"].as_str().ok_or("backup key is missing")?;
            let host = entry["host"].as_bool().ok_or("backup scope is missing")?;
            let target = Target {
                domain: domain.into(),
                key: key.into(),
                host,
            };
            let value = if entry["xml"].is_null() {
                None
            } else {
                Some(plist::parse(
                    entry["xml"]
                        .as_str()
                        .ok_or("backup XML is invalid")?
                        .as_bytes(),
                )?)
            };
            Ok(Saved { target, value })
        })
        .collect()
}

fn save_backup(path: &Path, keys: &[Saved]) -> Result<(), String> {
    let rows: Vec<Value> = keys
        .iter()
        .map(|s| {
            json!({
                "domain": s.target.domain, "key": s.target.key, "host": s.target.host,
                "xml": s.value.as_ref().map(plist::document),
            })
        })
        .collect();
    write_json(path, &json!({ "profile": PROFILE, "keys": rows }))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    use aterm_machines::port::System as _;
    let sys = aterm_machines::port::Live::new();
    write_json_with_flush(path, value, |directory| sys.durable_dir(directory))
}

// The backup precedes writes outside this filesystem. Every namespace entry,
// including one left by an interrupted earlier attempt, must be published too.
// A post-rename flush error may leave the new record visible, but is not success.
fn write_json_with_flush(
    path: &Path,
    value: &Value,
    mut flush: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("doctor record has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let bytes = aterm_json::to_vec(value).map_err(|e| e.to_string())?;
    aterm_machines::port::write_atomic(path, &bytes, 0o600)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    for directory in parent.ancestors() {
        flush(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    }
    Ok(())
}

fn append_workstation(
    host: &mut dyn Host,
    group: WorkstationGroup,
    apply: bool,
    report: &mut Report,
) {
    let name = match group {
        WorkstationGroup::Accounts => "Messages and FaceTime sign-out",
        WorkstationGroup::Preferences => "Workstation preferences",
        WorkstationGroup::InputActivation => "Active keyboard and trackpad settings",
        WorkstationGroup::All => "Workstation settings",
    };
    match host.workstation(group, apply) {
        Ok(rows) if rows.is_empty() => report.rows.push(Row {
            name: name.into(),
            status: "unverified".into(),
            detail: "the shared setup engine returned no settings to verify".into(),
        }),
        Ok(rows) => report.rows.extend(rows),
        Err(detail) => report.rows.push(Row {
            name: name.into(),
            status: "unverified".into(),
            detail,
        }),
    }
}

fn execute(host: &mut dyn Host, root: &Path, action: Action) -> Result<Report, String> {
    let backup = root.join("original-preferences.json");
    let mut report = Report {
        action,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup,
    };
    if action == Action::Restore {
        return restore(host, root, report);
    }
    let previous_activation = pending_refresh(root)?;
    if action == Action::Check {
        if !previous_activation.is_empty() {
            report.rows.push(Row {
                name: "Preference activation".into(),
                status: "drift".into(),
                detail: "a previous pass still needs activation; run doctor --apply to retry"
                    .into(),
            });
        }
        append_workstation(host, WorkstationGroup::All, false, &mut report);
        return Ok(report);
    }
    let mut saved = load_backup(&report.backup)?;
    let before = host.preference_changes()?;
    for setting in &before {
        if !saved.iter().any(|s| s.target == setting.target) {
            saved.push(setting.clone());
        }
    }
    // The shared item set supplies exact typed before-images. The recovery
    // record is durable before any preference changes, without another policy.
    if !before.is_empty() {
        save_backup(&report.backup, &saved)?;
    }
    let mut activation = previous_activation.clone();
    for setting in &before {
        if !activation.contains(&setting.target) {
            activation.push(setting.target.clone());
        }
    }
    save_pending_refresh(root, &activation)?;
    // An explicit repair also supersedes an earlier completion/restoration.
    // Failed account work must remain retryable until the shared pass verifies it.
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "pending" }),
    )?;
    append_workstation(host, WorkstationGroup::Preferences, true, &mut report);
    if report.needs_attention() {
        let mut errors = Vec::new();
        let attempted = host.attempted_preferences();
        for previous in attempted.iter().rev() {
            if let Err(error) = restore_one(host, &previous.target, previous.value.as_ref()) {
                errors.push(format!("rollback {}: {error}", previous.target.describe()));
            }
        }
        for row in &mut report.rows {
            if row.status == "applied" {
                row.status = "unverified".into();
                row.detail = "the preference pass failed; recovery results follow".into();
            }
        }
        report.rows.extend(host.restore_runtime(&attempted));
        if !errors.is_empty() {
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "error".into(),
                detail: errors.join("; "),
            });
        }
        if refresh(host, &activation.iter().collect::<Vec<_>>(), &mut report) {
            clear_pending_refresh(root)?;
        }
        return Ok(report);
    }
    // The common desktop items already activate their own writes. An old
    // doctor's recovery record may predate those items, so discharge it once.
    if refresh(
        host,
        &previous_activation.iter().collect::<Vec<_>>(),
        &mut report,
    ) {
        clear_pending_refresh(root)?;
    }
    append_workstation(host, WorkstationGroup::Accounts, true, &mut report);
    if !report.needs_attention() {
        write_json(
            &root.join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
        )?;
    }
    Ok(report)
}

fn restore_one(host: &mut dyn Host, target: &Target, value: Option<&PValue>) -> Result<(), String> {
    let observed = host.read(target)?;
    if same_optional(observed.as_ref(), value) {
        return Ok(());
    }
    host.write(target, value)?;
    if same_optional(host.read(target)?.as_ref(), value) {
        Ok(())
    } else {
        Err("macOS did not retain the original value".into())
    }
}

fn same_optional(a: Option<&PValue>, b: Option<&PValue>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_original(a, b),
        _ => false,
    }
}

// Policy comparison accepts macOS's equivalent bool/int values. Recovery has
// the stronger contract: restore the original plist type, recursively.
fn same_original(a: &PValue, b: &PValue) -> bool {
    match (a, b) {
        (PValue::Array(a), PValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_original(a, b))
        }
        (PValue::Dict(a), PValue::Dict(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(k, _)| k == key)
                        .is_some_and(|(_, other)| same_original(value, other))
                })
        }
        _ => a == b,
    }
}

fn restore(host: &mut dyn Host, root: &Path, mut report: Report) -> Result<Report, String> {
    let saved = load_backup(&report.backup)?;
    let mut activation = pending_refresh(root)?;
    for saved in &saved {
        if !activation.contains(&saved.target) {
            activation.push(saved.target.clone());
        }
    }
    save_pending_refresh(root, &activation)?;
    // Restoration is an explicit decision to stop automatic convergence, even
    // when reloading a prior runtime-only keyboard mapping needs a new login.
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "restored" }),
    )?;
    if saved.is_empty() {
        report.rows.push(Row {
            name: "Original preferences".into(),
            status: "ok".into(),
            detail: "no changed preferences were backed up".into(),
        });
    }
    for item in saved.iter().rev() {
        match restore_one(host, &item.target, item.value.as_ref()) {
            Ok(()) => report.rows.push(Row {
                name: item.target.describe(),
                status: "restored".into(),
                detail: "original value restored".into(),
            }),
            Err(detail) => report.rows.push(Row {
                name: item.target.describe(),
                status: "error".into(),
                detail,
            }),
        }
    }
    if refresh(host, &activation.iter().collect::<Vec<_>>(), &mut report) {
        clear_pending_refresh(root)?;
    }
    report.rows.extend(host.restore_runtime(&saved));
    if !report.needs_attention() {
        write_json(
            &root.join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "restored" }),
        )?;
    }
    Ok(report)
}

fn pending_refresh(root: &Path) -> Result<Vec<Target>, String> {
    let path = root.join("pending-refresh.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("pending activation: {e}"))?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized pending activation profile".into());
    }
    value["targets"]
        .as_array()
        .ok_or("pending activation has no targets")?
        .iter()
        .map(|entry| {
            Ok(Target {
                domain: entry["domain"]
                    .as_str()
                    .ok_or("pending activation domain is missing")?
                    .into(),
                key: entry["key"]
                    .as_str()
                    .ok_or("pending activation key is missing")?
                    .into(),
                host: entry["host"]
                    .as_bool()
                    .ok_or("pending activation scope is missing")?,
            })
        })
        .collect()
}

fn save_pending_refresh(root: &Path, targets: &[Target]) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let targets: Vec<Value> = targets
        .iter()
        .map(|t| json!({ "domain": t.domain, "key": t.key, "host": t.host }))
        .collect();
    write_json(
        &root.join("pending-refresh.json"),
        &json!({ "profile": PROFILE, "targets": targets }),
    )
}

fn clear_pending_refresh(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join("pending-refresh.json")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear pending activation: {e}")),
    }
}

fn refresh(host: &mut dyn Host, targets: &[&Target], report: &mut Report) -> bool {
    let mut refreshed = true;
    for (process, affected) in [
        (
            "Dock",
            targets
                .iter()
                .any(|t| t.domain == "com.apple.dock" || t.domain == "com.apple.WindowManager"),
        ),
        (
            "Finder",
            targets.iter().any(|t| t.domain == "com.apple.finder"),
        ),
        (
            "NotificationCenter",
            targets
                .iter()
                .any(|t| t.domain == "com.apple.notificationcenterui"),
        ),
    ] {
        if affected && let Err(detail) = host.restart(process) {
            refreshed = false;
            report.rows.push(Row {
                name: format!("Reload {process}"),
                status: "error".into(),
                detail,
            });
        }
    }
    if targets
        .iter()
        .any(|t| t.host || t.domain.contains("Trackpad") || t.domain.contains("trackpad"))
    {
        report.rows.push(Row { name: "Keyboard and trackpad activation".into(), status: "ok".into(), detail: "native preferences saved; log out and back in if an already connected device still uses its previous setting".into() });
    }
    refreshed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        values: BTreeMap<Target, PValue>,
        writes: Vec<Target>,
        fail_once: bool,
        accounts_blocked: bool,
        fail_restart_once: bool,
        restarts: Vec<String>,
        account_passes: usize,
        attempted: Vec<Saved>,
        runtime_changed: bool,
    }

    fn wanted() -> Vec<(Target, PValue)> {
        vec![
            (
                Target::new("com.apple.dock", "show-recents"),
                PValue::Bool(false),
            ),
            (
                Target::new("com.apple.WindowManager", "StandardHideWidgets"),
                PValue::Bool(true),
            ),
        ]
    }

    impl Host for Fake {
        fn attempted_preferences(&mut self) -> Vec<Saved> {
            std::mem::take(&mut self.attempted)
        }
        fn restore_runtime(&mut self, _saved: &[Saved]) -> Vec<Row> {
            self.runtime_changed = false;
            Vec::new()
        }
        fn read(&mut self, target: &Target) -> Result<Option<PValue>, String> {
            Ok(self.values.get(target).cloned())
        }
        fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
            self.writes.push(target.clone());
            match value {
                Some(value) => {
                    self.values.insert(target.clone(), value.clone());
                }
                None => {
                    self.values.remove(target);
                }
            }
            Ok(())
        }
        fn preference_changes(&mut self) -> Result<Vec<Saved>, String> {
            self.attempted.clear();
            Ok(wanted()
                .into_iter()
                .filter(|(t, v)| !self.values.get(t).is_some_and(|now| now.same(v)))
                .map(|(target, _)| Saved {
                    value: self.values.get(&target).cloned(),
                    target,
                })
                .collect())
        }
        fn restart(&mut self, name: &str) -> Result<(), String> {
            self.restarts.push(name.into());
            if self.fail_restart_once {
                self.fail_restart_once = false;
                return Err("injected restart failure".into());
            }
            Ok(())
        }
        fn workstation(
            &mut self,
            group: WorkstationGroup,
            apply: bool,
        ) -> Result<Vec<Row>, String> {
            let mut rows = Vec::new();
            if group != WorkstationGroup::Accounts {
                for (target, value) in wanted() {
                    let correct = self.values.get(&target).is_some_and(|now| now.same(&value));
                    let mut status = if correct { "ok" } else { "drift" };
                    if apply && !correct {
                        self.attempted.push(Saved {
                            target: target.clone(),
                            value: self.values.get(&target).cloned(),
                        });
                        self.write(&target, Some(&value))?;
                        self.runtime_changed = true;
                        status = "applied";
                        if self.fail_once {
                            self.fail_once = false;
                            status = "error";
                        }
                    }
                    rows.push(Row {
                        name: target.describe(),
                        status: status.into(),
                        detail: "injected shared item".into(),
                    });
                }
            }
            if matches!(group, WorkstationGroup::Accounts | WorkstationGroup::All) {
                self.account_passes += 1;
                rows.push(Row {
                    name: "Account sign-out".into(),
                    status: if self.accounts_blocked {
                        "blocked"
                    } else {
                        "ok"
                    }
                    .into(),
                    detail: "injected account result".into(),
                });
            }
            Ok(rows)
        }
    }

    #[test]
    fn shared_apply_is_idempotent_and_restore_preserves_unowned_keys() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        let unowned = Target::new("com.apple.dock", "orientation");
        host.values
            .insert(unowned.clone(), PValue::Str("left".into()));
        let first = execute(&mut host, root.path(), Action::Apply).expect("apply");
        assert!(!first.needs_attention(), "{}", first.to_text());
        assert!(first_run_complete(root.path()).expect("complete marker"));
        let count = host.writes.len();
        let backup = std::fs::read(&first.backup).expect("backup");
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("reapply")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), count);
        assert_eq!(
            backup,
            std::fs::read(&first.backup).expect("same originals")
        );
        host.values
            .insert(unowned.clone(), PValue::Str("right".into()));
        assert!(
            !execute(&mut host, root.path(), Action::Restore)
                .expect("restore")
                .needs_attention()
        );
        assert_eq!(host.values.len(), 1);
        assert_eq!(host.values[&unowned], PValue::Str("right".into()));
        assert!(first_run_complete(root.path()).expect("restore disables automatic setup"));
    }

    #[test]
    fn failed_shared_preference_apply_rolls_back_partial_mutations() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            fail_once: true,
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("failure report");
        assert!(report.needs_attention());
        assert!(
            host.values.is_empty(),
            "even a failed write may already have mutated a preference"
        );
        assert!(
            !host.runtime_changed,
            "rollback must recover active settings as well as saved preferences"
        );
        assert!(
            !report.rows.iter().any(|row| row.status == "restored"),
            "recovery must not claim every runtime item was restored"
        );
        assert!(host.restarts.iter().any(|p| p == "Dock"));
        assert_eq!(host.account_passes, 0);
        assert!(!first_run_complete(root.path()).expect("failed pass remains pending"));
        assert!(report.backup.exists());
    }

    #[test]
    fn blocked_accounts_keep_preference_progress_but_not_completion() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            accounts_blocked: true,
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("blocked");
        assert!(report.needs_attention());
        assert!(!first_run_complete(root.path()).expect("blocked accounts remain pending"));
        assert!(!host.values.is_empty());
        let writes = host.writes.len();
        host.accounts_blocked = false;
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("permission retry")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), writes);
        assert!(first_run_complete(root.path()).expect("fully verified"));
    }

    #[test]
    fn failed_activation_is_reported_and_retried_without_preference_writes() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        execute(&mut host, root.path(), Action::Apply).expect("initial apply");
        save_pending_refresh(root.path(), &[wanted()[0].0.clone()])
            .expect("interrupted activation");
        assert!(
            execute(&mut host, root.path(), Action::Check)
                .expect("pending check")
                .needs_attention()
        );
        let writes = host.writes.len();
        host.fail_restart_once = true;
        assert!(
            execute(&mut host, root.path(), Action::Apply)
                .expect("failed activation")
                .needs_attention()
        );
        assert!(root.path().join("pending-refresh.json").exists());
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("retry activation")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), writes);
        assert!(!root.path().join("pending-refresh.json").exists());
    }

    #[test]
    fn read_only_and_bad_recovery_records_never_write_preferences() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        let report = execute(&mut host, root.path(), Action::Check).expect("check");
        assert!(report.needs_attention());
        assert!(host.writes.is_empty());
        assert!(!report.backup.exists());
        std::fs::write(&report.backup, b"broken").expect("bad backup");
        assert!(execute(&mut host, root.path(), Action::Apply).is_err());
        assert!(host.writes.is_empty());
        std::fs::write(root.path().join("pending-refresh.json"), b"broken").expect("bad pending");
        assert!(execute(&mut host, root.path(), Action::Check).is_err());
        assert!(execute(&mut host, root.path(), Action::Restore).is_err());
        assert!(host.writes.is_empty());

        // The record may already be visible when its namespace flush fails.
        // That failure must not authorize the external preference writes.
        let record = root
            .path()
            .join("new-state")
            .join("doctor")
            .join("backup.json");
        let value = json!({"profile": PROFILE, "keys": []});
        let mut attempted = Vec::new();
        let refused = write_json_with_flush(&record, &value, |directory| {
            attempted.push(directory.to_path_buf());
            if directory == root.path() {
                Err(std::io::Error::other("injected ancestor flush refusal"))
            } else {
                Ok(())
            }
        });
        assert!(refused.is_err(), "a visible rename is not a durable backup");
        assert_eq!(attempted.last().map(PathBuf::as_path), Some(root.path()));
        assert_eq!(
            aterm_json::from_slice::<Value>(&std::fs::read(&record).unwrap()).unwrap(),
            value,
            "failure after publication is retained as failure"
        );
        let mut reached_root = false;
        write_json_with_flush(&record, &value, |directory| {
            reached_root |= directory.parent().is_none();
            Ok(())
        })
        .expect("a retry discharges the complete namespace chain");
        assert!(
            reached_root,
            "existing ancestor entries also need publication"
        );
    }

    #[test]
    fn recovery_restores_plist_types_and_old_marker_does_not_skip_accounts() {
        let mut host = Fake::default();
        let target = wanted()[0].0.clone();
        host.values.insert(target.clone(), PValue::Bool(true));
        restore_one(&mut host, &target, Some(&PValue::Int(1))).expect("typed original");
        assert_eq!(host.values[&target], PValue::Int(1));
        let root = aterm_tempfile::tempdir().expect("scratch");
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile": PROFILE, "state": "applied"}),
        )
        .expect("old marker");
        assert!(!first_run_complete(root.path()).expect("not verified"));
    }

    #[test]
    fn automatic_permission_retries_do_not_repeat_on_every_launch() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        assert!(automatic_retry_due(root.path(), Some(false)).expect("first attempt"));
        write_json(
            &root.path().join("automatic-attempt.json"),
            &json!({"profile": PROFILE, "accessibility_ready": false}),
        )
        .expect("attempt");
        assert!(!automatic_retry_due(root.path(), Some(false)).expect("same permission"));
        assert!(automatic_retry_due(root.path(), Some(true)).expect("new permission"));
    }

    /// A real first run keeps its setup: HOME is the account's own home, as
    /// written or through a link, so the profile may change and the launch's
    /// startup gate (`first_run_doctor_eligible` in crates/aterm) has no lasting
    /// refusal to stop it. A scratch HOME, a private state root and sudo are
    /// refused for the life of the process, so such a launch starts no setup
    /// worker and says nothing (crates/aterm/tests/session_launch_silence.rs). A
    /// home that cannot be read just now is no lasting refusal: the worker
    /// reports it and retries.
    #[test]
    fn a_real_home_may_change_the_profile_and_a_scratch_home_never_may() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let account = root.path().join("account");
        let scratch = root.path().join("scratch");
        let link = root.path().join("link");
        std::fs::create_dir(&account).expect("account home");
        std::fs::create_dir(&scratch).expect("scratch home");
        std::os::unix::fs::symlink(&account, &link).expect("a link to the account home");
        let own = || -> Result<PathBuf, String> { Ok(account.clone()) };
        for home in [&account, &link] {
            assert_eq!(
                scope_of(false, 501, Some(home), own),
                PersonScope::Person,
                "{}",
                home.display()
            );
            assert_eq!(scope_of(false, 501, Some(home), own).lasting(), None);
        }
        for scope in [
            scope_of(false, 501, Some(&scratch), own),
            scope_of(true, 501, Some(&account), own),
            scope_of(false, 0, Some(&account), own),
        ] {
            assert!(matches!(scope, PersonScope::Refused(_)), "{scope:?}");
            assert!(scope.lasting().is_some());
        }
        let unread = || -> Result<PathBuf, String> { Err("getpwuid_r: 5".into()) };
        for scope in [
            scope_of(false, 501, None, own),
            scope_of(false, 501, Some(&account), unread),
            scope_of(false, 501, Some(&root.path().join("missing")), own),
        ] {
            assert!(matches!(scope, PersonScope::Unresolved(_)), "{scope:?}");
            assert_eq!(scope.lasting(), None);
        }
    }
}
