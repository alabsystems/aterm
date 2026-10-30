// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release keychain aterm owns, `~/Library/Keychains/aterm-release.keychain-db`: `ship
//! provision` imports the Developer ID pair into it and `ship release` signs from it, each
//! opening it with the keychain's own stored password, so no step asks for the login keychain's
//! password. Every `security` and `codesign` call goes through a [`Run`], so the tests record
//! argv and touch no keychain.

#![cfg(unix)]

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::rc::{Rc, Weak};

use aterm_uds::deadline::RunError;

use crate::keychain_tools::{Run, WAIT};

type Arg<'a> = &'a dyn AsRef<OsStr>;

/// A same-thread lease on this user's owned signing keychain. Its Rc deliberately prevents
/// transfer to another thread; nested operations share the lock only on its owning thread.
#[derive(Debug)]
pub struct SigningLease {
    _lock: Rc<LockedFile>,
}

#[derive(Debug)]
struct LockedFile(std::fs::File);

impl Drop for LockedFile {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

thread_local! {
    static SIGNING_LEASES: RefCell<BTreeMap<PathBuf, Weak<LockedFile>>> = const { RefCell::new(BTreeMap::new()) };
}

const SIGNING_BUSY: &str = "signing keychain is busy; automatic setup will retry";

/// The cutter waits up to the same one-minute deadline as a signing tool while an automatic
/// repair owns the keychain. Background convergence uses [`signing_lease`] and defers at once.
pub fn wait_signing_lease(home: &Path) -> Result<SigningLease, String> {
    signing_lease_before(home, std::time::Instant::now() + WAIT)
}

fn signing_lease_before(home: &Path, deadline: std::time::Instant) -> Result<SigningLease, String> {
    loop {
        match signing_lease(home) {
            Err(why) if why == SIGNING_BUSY && std::time::Instant::now() < deadline => {
                std::thread::sleep(
                    std::time::Duration::from_millis(100)
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
            }
            result => return result,
        }
    }
}

/// Acquire the transaction lease used by automatic import and the release cutter. Only
/// nested calls on the owning thread reuse it; other threads and processes return Busy.
pub fn signing_lease(home: &Path) -> Result<SigningLease, String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let home = std::fs::canonicalize(home).map_err(|error| format!("signing home: {error}"))?;
    SIGNING_LEASES.with(|leases| {
        let mut leases = leases.borrow_mut();
        if let Some(lease) = leases.get(&home).and_then(Weak::upgrade) {
            return Ok(SigningLease { _lock: lease });
        }
        let dir = home.join(".aterm/apple");
        crate::fsx::ensure_private_dir(&dir).map_err(|error| error.to_string())?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(".signing.lock"))
            .map_err(|error| format!("signing lock: {error}"))?;
        let meta = file
            .metadata()
            .map_err(|error| format!("signing lock: {error}"))?;
        if !meta.is_file() || meta.uid() != aterm_uds::peer::our_uid() || meta.nlink() != 1 {
            return Err("signing lock is not a singly-linked owned regular file".into());
        }
        if meta.mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("secure signing lock: {error}"))?;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "nonblocking private keychain transaction lease, released by LockedFile"
        )]
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(SIGNING_BUSY.into());
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("signing lock: {error}"));
            }
        }
        let lease = Rc::new(LockedFile(file));
        leases.retain(|_, prior| prior.strong_count() != 0);
        leases.insert(home, Rc::downgrade(&lease));
        Ok(SigningLease { _lock: lease })
    })
}

const SHIP_COMMAND: &str = "targo --unverified ship";
const SECURITY: &str = aterm_uds::os_path!("security");
const CODESIGN: &str = aterm_uds::os_path!("codesign");
/// Named only in the key's access list (`-T`): no cut runs productsign today, and one that
/// does reads the key with no prompt.
const PRODUCTSIGN: &str = "/usr/bin/productsign";

/// The keychain's file name under `~/Library/Keychains`.
pub const FILE_NAME: &str = "aterm-release.keychain-db";

/// The partition list `codesign` and `productsign` need to use the key unattended.
const PARTITIONS: &str = "apple-tool:,apple:,codesign:";

/// The release keychain under `home`.
pub fn keychain_path(home: &Path) -> PathBuf {
    home.join("Library").join("Keychains").join(FILE_NAME)
}

/// The file holding the release keychain's password: beside the Developer ID key it guards,
/// in `~/.aterm/apple` (0700), which agent tabs' containment denies and the vault never fills.
pub fn password_path(home: &Path) -> PathBuf {
    home.join(".aterm")
        .join("apple")
        .join("release-keychain.pass")
}

/// The login keychain under `home`, named only in the fix for a Mac provisioned before the
/// release keychain existed: nothing here writes it.
pub fn login_keychain_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Keychains")
        .join("login.keychain-db")
}

/// The release keychain's password. Its `Debug` is redacted, so no derive or panic prints it.
pub struct Password(String);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

impl AsRef<OsStr> for Password {
    fn as_ref(&self) -> &OsStr {
        OsStr::new(&self.0)
    }
}

/// Why a step stopped, and what the person runs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub what: String,
    pub fix: String,
}

impl Refusal {
    fn new(what: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            fix: fix.into(),
        }
    }
}

/// `ship provision --id …`, as the fixes spell it.
fn provision_command(id: &str) -> String {
    format!("{} provision --id {id}", SHIP_COMMAND)
}

/// What the release keychain is now ([`Keychains::state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No keychain file.
    Missing,
    /// The file is there and does not open with the stored password; `holds` says whether it
    /// holds any of the team's identities (listing them needs no unlock).
    Locked { refusal: Refusal, holds: bool },
    /// Open: the team's valid identities it holds, and whether the user's search list names it.
    Open {
        identities: Vec<String>,
        listed: bool,
    },
}

/// Whether a signing check may write aterm's release keychain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// A cut, which signs next, or provision with leave to change: the key's partition list is
    /// re-asserted before codesign is pointed at the keychain.
    Write,
    /// `ship provision --check` and the release audit, which write nothing: the partition list
    /// is left as it is, so codesign is not run against the release keychain.
    Look,
}

/// What a signing check established for an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// A test signature passed.
    Signed,
    /// A look at an identity the release keychain holds: the keychain opened with its stored
    /// password and no test signature ran, since only re-asserting the partition list (a write)
    /// keeps codesign from asking in a dialog. Provision and every cut re-assert it, then sign.
    Opened,
}

/// What [`Proof::Opened`] reads as on a check line.
pub const OPENED_NOTE: &str = "a check runs no test signature from the release keychain \
     (provision and every cut set its partition list first)";

/// One provision check line about the release keychain ([`judge`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Ready(String),
    Todo { what: String, next: String },
    Blocked { what: String, fix: String },
}

/// The keychains one user signs from, and the [`Run`] every tool call goes through.
pub struct Keychains<'a> {
    home: PathBuf,
    run: &'a dyn Run,
    lease: OnceCell<SigningLease>,
}

impl<'a> Keychains<'a> {
    pub fn new(home: &Path, run: &'a dyn Run) -> Self {
        Self {
            home: home.to_path_buf(),
            run,
            lease: OnceCell::new(),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The release keychain's path.
    pub fn release(&self) -> PathBuf {
        keychain_path(&self.home)
    }

    fn call(&self, program: &str, args: &[Arg<'_>]) -> Result<Output, String> {
        self.lock()?;
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args.iter().map(|a| a.as_ref()));
        self.run.run(&mut cmd, WAIT).map_err(|e| match e {
            RunError::TimedOut => format!(
                "{program} did not answer within {} s (trustd/securityd); automatic setup will retry",
                WAIT.as_secs()
            ),
            e => format!("{program} {e}"),
        })
    }

    fn lock(&self) -> Result<(), String> {
        if self.lease.get().is_none() {
            let _ = self.lease.set(signing_lease(&self.home)?);
        }
        Ok(())
    }

    /// A `security` call's output, whatever its exit.
    pub fn security(&self, args: &[Arg<'_>]) -> Result<Output, String> {
        self.call(SECURITY, args)
    }

    /// A `security` call's stdout, when it ran and succeeded.
    pub fn security_stdout(&self, args: &[Arg<'_>]) -> Option<String> {
        self.security(args)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// A `security` call that must succeed, its failure said by subcommand (never by argv,
    /// which can carry the password).
    fn security_ok(&self, args: &[Arg<'_>], what: &str) -> Result<Output, String> {
        let out = self.security(args)?;
        if out.status.success() {
            return Ok(out);
        }
        Err(format!(
            "security {what} on {} failed: {}",
            self.release().display(),
            first_line(&out.stderr)
        ))
    }

    /// `codesign` with `args`, its first stderr line on failure.
    pub fn codesign(&self, args: &[Arg<'_>]) -> Result<(), String> {
        let out = self.call(CODESIGN, args)?;
        if out.status.success() {
            return Ok(());
        }
        Err(first_line(&out.stderr))
    }

    /// `find-certificate` (or any lookup) in the release keychain by name, then on the
    /// search list: the stdout of each that succeeded, joined.
    pub fn find_certificates(&self, args: &[&str]) -> Option<String> {
        let release = self.release();
        let mut text = String::new();
        if release.is_file() {
            let mut named: Vec<Arg<'_>> = args.iter().map(|a| a as Arg<'_>).collect();
            named.push(&release);
            text.push_str(&self.security_stdout(&named).unwrap_or_default());
        }
        let listed: Vec<Arg<'_>> = args.iter().map(|a| a as Arg<'_>).collect();
        text.push_str(&self.security_stdout(&listed).unwrap_or_default());
        (!text.is_empty()).then_some(text)
    }

    /// `find-identity [-v] -p codesigning` in the release keychain alone. A listing that fails
    /// is an error, never an empty keychain: read as empty, a keychain that holds the identity
    /// and will not open would be left for codesign to meet.
    pub fn release_listing(&self, valid: bool) -> Result<String, String> {
        let release = self.release();
        if !release.is_file() {
            return Ok(String::new());
        }
        let mut args: Vec<Arg<'_>> = vec![&"find-identity"];
        if valid {
            args.push(&"-v");
        }
        args.extend([&"-p" as Arg<'_>, &"codesigning", &release]);
        let out = self.security_ok(&args, "find-identity")?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `find-identity [-v] -p codesigning` on the search list: the read-only fallback, where a
    /// Mac provisioned before the release keychain holds its identity in the login keychain.
    fn search_listing(&self, valid: bool) -> Result<String, String> {
        let mut args: Vec<Arg<'_>> = vec![&"find-identity"];
        if valid {
            args.push(&"-v");
        }
        args.extend([&"-p" as Arg<'_>, &"codesigning"]);
        Ok(String::from_utf8_lossy(&self.security(&args)?.stdout).into_owned())
    }

    /// The team's Developer ID Application identities in the release keychain.
    pub fn release_identities(&self, team: &str, valid: bool) -> Result<Vec<String>, String> {
        Ok(distinct(devid_identities(
            &self.release_listing(valid)?,
            team,
        )))
    }

    /// The team's identities in the release keychain, named, then those only the search list
    /// holds; each SHA-1 once.
    pub fn identities(&self, team: &str, valid: bool) -> Result<Vec<String>, String> {
        let mut ids = self.release_identities(team, valid)?;
        ids.extend(devid_identities(&self.search_listing(valid)?, team));
        Ok(distinct(ids))
    }

    /// Whether the release keychain holds `sha1` as a valid identity. A listing that fails is
    /// an error, never a no: read as one, an identity held there would be sent to the login
    /// keychain's fix.
    pub fn holds(&self, team: &str, sha1: &str) -> Result<bool, String> {
        Ok(self
            .release_identities(team, true)?
            .iter()
            .any(|id| id.eq_ignore_ascii_case(sha1)))
    }

    /// The keychain codesign is told to sign `sha1` from: the release keychain, opened, when it
    /// holds it; `None` for one only the search list holds, which codesign finds as it always
    /// has. codesign never meets the release keychain locked, or its key without codesign in the
    /// partition list: either asks for the keychain's password in a dialog. So a release
    /// keychain that holds `sha1` and will not open, or whose listing cannot be read, is
    /// refused, and with [`Leave::Write`] the partition list is re-asserted with the keychain's
    /// own password first. [`Leave::Look`] leaves it alone, and its callers run no codesign
    /// against the keychain this names.
    pub fn signing_keychain(
        &self,
        team: &str,
        sha1: &str,
        leave: Leave,
    ) -> Result<Option<PathBuf>, String> {
        if !self.release().is_file() {
            return Ok(None);
        }
        let opened = self.open();
        if !self.holds(team, sha1)? {
            return Ok(None);
        }
        let Some(password) = opened.map_err(|r| format!("{} — {}", r.what, r.fix))? else {
            return Ok(None);
        };
        if leave == Leave::Write {
            self.set_partition_list(&password)?;
        }
        Ok(Some(self.release()))
    }

    /// The user's keychain search list, in order. A read that fails is an error, never an empty
    /// list.
    pub fn search_list(&self) -> Result<Vec<PathBuf>, String> {
        let out = self.security(&[&"list-keychains", &"-d", &"user"])?;
        if !out.status.success() {
            return Err(format!(
                "security list-keychains -d user failed: {}",
                first_line(&out.stderr)
            ));
        }
        Ok(crate::keychain_tools::unquote_paths(
            &String::from_utf8_lossy(&out.stdout),
        ))
    }

    /// Put the release keychain at the end of the user's search list, keeping every entry
    /// already there in its order ([`crate::keychain_tools::search_list_with`]). The one
    /// search-list write anything here makes, and only when the keychain is missing from it
    /// (`create-keychain` may already have listed it).
    fn ensure_listed(&self) -> Result<(), String> {
        let release = self.release();
        let Some(list) = crate::keychain_tools::search_list_with(&self.search_list()?, &release)?
        else {
            return Ok(());
        };
        let mut args: Vec<Arg<'_>> = vec![&"list-keychains", &"-d", &"user", &"-s"];
        args.extend(list.iter().map(|p| p as Arg<'_>));
        self.security_ok(&args, "list-keychains -s")?;
        Ok(())
    }

    /// Unlock the release keychain with its stored password: `Ok(false)` when there is none.
    pub fn unlock(&self) -> Result<bool, Refusal> {
        self.open().map(|password| password.is_some())
    }

    /// [`Keychains::unlock`], keeping the password it opened the keychain with.
    fn open(&self) -> Result<Option<Password>, Refusal> {
        self.check_release_path()?;
        let release = self.release();
        if !release.is_file() {
            return Ok(None);
        }
        let Some(password) = read_password(&self.home)? else {
            return Err(self.unopenable(format!(
                "{} exists and its password file {} does not",
                release.display(),
                password_path(&self.home).display()
            )));
        };
        self.open_with(&password)?;
        Ok(Some(password))
    }

    /// The password crosses argv to `security` here and in `create-keychain` and
    /// `set-key-partition-list`, as unattended keychain setups do: other users cannot read
    /// this user's argv, and this user's processes can read the 0600 file and the Developer ID
    /// key beside it anyway.
    fn open_with(&self, password: &Password) -> Result<(), Refusal> {
        let release = self.release();
        let out = self
            .security(&[&"unlock-keychain", &"-p", password, &release])
            .map_err(|e| Refusal::new(e, "re-run once the keychain service answers"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(self.unopenable(format!(
            "{} does not unlock with the password in {}: {}",
            release.display(),
            password_path(&self.home).display(),
            first_line(&out.stderr)
        )))
    }

    /// A release keychain this user cannot open: never deleted here, since it may hold the
    /// only copy of a key.
    fn unopenable(&self, what: String) -> Refusal {
        let release = self.release();
        Refusal::new(
            what,
            format!(
                "move it aside (mv '{}' '{}.old'), then run `{}`: it makes a new one, and \
                 imports the Developer ID pair into it when ~/.aterm/apple holds one, asking \
                 for no password. Keep the old file until the new keychain signs: it may hold \
                 the only copy of a key",
                release.display(),
                release.display(),
                provision_command("<this machine's id>")
            ),
        )
    }

    /// Refuse a release path that leads to a protected keychain before a caller can unlock,
    /// create or modify it, whether the caller provisions or adopts an existing identity.
    fn check_release_path(&self) -> Result<(), Refusal> {
        let release = self.release();
        if crate::keychain_tools::protected(&release, &[]) {
            return Err(Refusal::new(
                format!(
                    "{} is, or leads to, the login or System keychain — refusing to touch it",
                    release.display()
                ),
                format!(
                    "remove that link (ls -l '{}'), then re-run",
                    release.display()
                ),
            ));
        }
        Ok(())
    }

    /// Make the release keychain if it is missing, open it, and set it never to lock; the
    /// password comes from its file, which is written first, so a keychain is never made with
    /// a password nobody kept.
    fn open_or_create(&self) -> Result<Password, Refusal> {
        self.check_release_path()?;
        let release = self.release();
        let fail = |e: String| {
            Refusal::new(
                e,
                format!(
                    "re-run `{}`; `security show-keychain-info {}` says what macOS makes of \
                     the file",
                    provision_command("<this machine's id>"),
                    release.display()
                ),
            )
        };
        if release.is_file() {
            let Some(password) = read_password(&self.home)? else {
                return Err(self.unopenable(format!(
                    "{} exists and its password file {} does not",
                    release.display(),
                    password_path(&self.home).display()
                )));
            };
            self.open_with(&password)?;
            self.security_ok(
                &[&"set-keychain-settings", &release],
                "set-keychain-settings",
            )
            .map_err(fail)?;
            return Ok(password);
        }
        // No keychain uses the file, so its fix makes way for a new one; a moved-aside keychain
        // opens only with a file that reads.
        let pass = password_path(&self.home);
        let password = match stored_password(&self.home) {
            Stored::Password(password) => password,
            Stored::Absent => create_password(&self.home)?,
            Stored::Unreadable(why) => {
                return Err(Refusal::new(
                    format!("cannot read {}: {why}", pass.display()),
                    format!(
                        "make it this user's and readable (ls -l '{}'; it is 0600), then \
                         re-run `{}`; if you moved a keychain aside, this file opens it, so \
                         move the two together",
                        pass.display(),
                        provision_command("<this machine's id>")
                    ),
                ));
            }
            Stored::Malformed => {
                return Err(Refusal::new(
                    format!(
                        "{} does not hold the 64-hex password provision writes, so no keychain \
                         opens with it",
                        pass.display()
                    ),
                    format!(
                        "move it aside (mv '{}' '{}.bad') and re-run `{}`, which writes a new \
                         one",
                        pass.display(),
                        pass.display(),
                        provision_command("<this machine's id>")
                    ),
                ));
            }
        };
        if let Some(dir) = release.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail(format!("{}: {e}", dir.display())))?;
        }
        self.security_ok(
            &[&"create-keychain", &"-p", &password, &release],
            "create-keychain",
        )
        .map_err(fail)?;
        self.open_with(&password)?;
        self.security_ok(
            &[&"set-keychain-settings", &release],
            "set-keychain-settings",
        )
        .map_err(fail)?;
        Ok(password)
    }

    /// `security import` into the release keychain; an item already there is success.
    fn import(&self, args: &[Arg<'_>], what: &str) -> Result<(), String> {
        match self.security_ok(args, what) {
            Ok(_) => Ok(()),
            Err(e) => {
                let lower = e.to_ascii_lowercase();
                if lower.contains("already exists") || lower.contains("duplicate") {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Let codesign and productsign use the release keychain's key with no prompt. It needs
    /// only that keychain's own password.
    fn set_partition_list(&self, password: &Password) -> Result<(), String> {
        let release = self.release();
        self.security_ok(
            &[
                &"set-key-partition-list",
                &"-S",
                &PARTITIONS,
                &"-s",
                &"-k",
                password,
                &release,
            ],
            "set-key-partition-list",
        )?;
        Ok(())
    }

    /// Import a Developer ID certificate and its private key into the release keychain (made
    /// when missing), let codesign use the key unattended, and put the keychain on the search
    /// list. Nothing else is written: no other keychain is named.
    pub fn install_pair(&self, cert: &Path, key: &Path) -> Result<(), Refusal> {
        let password = self.open_or_create()?;
        let release = self.release();
        let import_fix = |what: &str, extra: &str| {
            format!(
                "run it yourself and read the error: security import {what} -k '{}'{extra} \
                 (open it first: security unlock-keychain '{}', its password is in {})",
                release.display(),
                release.display(),
                password_path(&self.home).display()
            )
        };
        self.import(
            &[
                &"import",
                &cert,
                &"-k",
                &release,
                &"-T",
                &CODESIGN,
                &"-T",
                &PRODUCTSIGN,
            ],
            "import (certificate)",
        )
        .map_err(|e| {
            Refusal::new(
                e,
                import_fix(
                    &cert.display().to_string(),
                    &format!(" -T {CODESIGN} -T {PRODUCTSIGN}"),
                ),
            )
        })?;
        self.import(
            &[
                &"import",
                &key,
                &"-k",
                &release,
                &"-t",
                &"priv",
                &"-f",
                &"openssl",
                &"-T",
                &CODESIGN,
                &"-T",
                &PRODUCTSIGN,
                &"-T",
                &SECURITY,
            ],
            "import (private key)",
        )
        .map_err(|e| {
            Refusal::new(
                e,
                import_fix(
                    &key.display().to_string(),
                    &format!(
                        " -t priv -f openssl -T {CODESIGN} -T {PRODUCTSIGN} -T {SECURITY}; the \
                         -T flags are load-bearing: a key imported without them yields an \
                         identity that looks installed and then cannot sign unattended"
                    ),
                ),
            )
        })?;
        self.set_partition_list(&password).map_err(|e| {
            Refusal::new(
                e,
                format!(
                    "re-run `{}`: it sets the partition list with the release keychain's own \
                     password, never your login password",
                    provision_command("<this machine's id>")
                ),
            )
        })?;
        self.ensure_listed()
            .map_err(|e| Refusal::new(e, "re-run once `security list-keychains -d user` answers"))
    }

    /// Import an issuing certificate into the release keychain (made when missing), so its
    /// leaf chains: the login keychain is never written.
    pub fn install_certificate(&self, cert: &Path) -> Result<(), String> {
        self.open_or_create().map_err(|r| r.what)?;
        let release = self.release();
        self.import(
            &[&"import", &cert, &"-k", &release],
            "import (intermediate)",
        )?;
        self.ensure_listed()
    }

    /// What a provision run with leave to change re-asserts on an existing release keychain:
    /// open, never locking, its key usable unattended, on the search list. Nothing is made
    /// when it is missing ([`Keychains::install_pair`] does that), so this is a no-op on a Mac
    /// that signs from its login keychain. One that will not open is refused only when it
    /// holds an identity: holding none, it signs nothing, so provision goes on to the identity
    /// the search list holds and the keychain line reports it.
    pub fn converge(&self, team: &str) -> Result<(), Refusal> {
        if !self.release().is_file() {
            return Ok(());
        }
        let password = match self.open_or_create() {
            Ok(password) => password,
            Err(refused) => {
                let held = self
                    .release_identities(team, false)
                    .map_err(|e| Refusal::new(e, "re-run once the keychain service answers"))?;
                return if held.is_empty() {
                    Ok(())
                } else {
                    Err(refused)
                };
            }
        };
        let held = self
            .release_identities(team, false)
            .map_err(|e| Refusal::new(e, "re-run once the keychain service answers"))?;
        if !held.is_empty() {
            self.set_partition_list(&password).map_err(|e| {
                Refusal::new(
                    e,
                    format!(
                        "re-run `{}`; `security find-identity -p codesigning {}` lists what it \
                         holds",
                        provision_command("<this machine's id>"),
                        self.release().display()
                    ),
                )
            })?;
        }
        self.ensure_listed()
            .map_err(|e| Refusal::new(e, "re-run once `security list-keychains -d user` answers"))
    }

    /// The release keychain's state, opening it with the stored password to look.
    pub fn state(&self, team: &str) -> Result<State, String> {
        match self.unlock() {
            Ok(false) => return Ok(State::Missing),
            Err(refusal) => {
                let holds = !self.release_identities(team, false)?.is_empty();
                return Ok(State::Locked { refusal, holds });
            }
            Ok(true) => {}
        }
        let identities = self.release_identities(team, true)?;
        let release = self.release();
        let listed = self.search_list()?.contains(&release);
        Ok(State::Open { identities, listed })
    }

    /// The provision check line for the release keychain, `probe` proving a test signature.
    pub fn row(&self, id: &str, team: &str, probe: &dyn Fn(&str) -> Result<Proof, String>) -> Row {
        let state = match self.state(team) {
            Ok(state) => state,
            Err(e) => {
                return Row::Blocked {
                    what: format!("could not read {}: {e}", self.release().display()),
                    fix: "check that /usr/bin/security runs, then re-run".into(),
                };
            }
        };
        let elsewhere = || {
            let ours = self.release_identities(team, true).unwrap_or_default();
            self.identities(team, true)
                .unwrap_or_default()
                .into_iter()
                .find(|sha1| !ours.iter().any(|o| o.eq_ignore_ascii_case(sha1)))
        };
        judge(id, &self.release(), state, &elsewhere, probe)
    }
}

/// What the release keychain's state says to whoever runs provision. `elsewhere` names an
/// identity only the search list holds (the login keychain of a Mac provisioned before the
/// release keychain), which signs as it always has; `probe` runs a test signature, or under
/// [`Leave::Look`] only opens the keychain. A fix never asks for the login password.
pub fn judge(
    id: &str,
    release: &Path,
    state: State,
    elsewhere: &dyn Fn() -> Option<String>,
    probe: &dyn Fn(&str) -> Result<Proof, String>,
) -> Row {
    let shown = release.display();
    let provision = provision_command(id);
    let legacy = |sha1: String, how: &str| {
        Row::Ready(format!(
            "{how}; this Mac signs with {sha1} from its keychain search list (provisioned \
             before the release keychain), and provision leaves that as it is"
        ))
    };
    match state {
        State::Missing => match elsewhere() {
            Some(sha1) => legacy(sha1, &format!("{shown} is absent")),
            None => Row::Todo {
                what: format!("{shown} is absent"),
                next: format!(
                    "run `{provision}`: it makes it and imports the Developer ID pair from \
                     ~/.aterm/apple into it, asking for no password"
                ),
            },
        },
        // One that holds no identity signs nothing, so it stops no Mac whose search list holds
        // one (an issuing intermediate provision put there, its password file since lost).
        State::Locked {
            refusal,
            holds: false,
        } if let Some(sha1) = elsewhere() => Row::Ready(format!(
            "{} — it holds no Developer ID identity, so this Mac signs with {sha1} from its \
             keychain search list as before (to use the release keychain again: {})",
            refusal.what, refusal.fix
        )),
        State::Locked { refusal, .. } => Row::Blocked {
            what: refusal.what,
            fix: refusal.fix,
        },
        State::Open { identities, listed } => match identities.first() {
            None => match elsewhere() {
                Some(sha1) => legacy(sha1, &format!("{shown} is open and holds no identity")),
                None => Row::Todo {
                    what: format!(
                        "{shown} is open and holds no Developer ID Application identity for \
                         team {}",
                        aterm_update_core::pins::APPLE_TEAM_ID
                    ),
                    next: format!(
                        "run `{provision}`: it imports the Developer ID pair from \
                         ~/.aterm/apple, asking for no password"
                    ),
                },
            },
            Some(sha1) => match probe(sha1) {
                Err(why) => Row::Blocked {
                    what: format!(
                        "{shown} holds {sha1}, and a test signature from it failed ({why})"
                    ),
                    fix: format!(
                        "run `{provision}`: it sets the partition list with the release \
                         keychain's own stored password, never your login password"
                    ),
                },
                Ok(_) if !listed => Row::Todo {
                    what: format!(
                        "{shown} holds {sha1} and is not on the keychain search list, so the \
                         chain a signature embeds can miss its intermediate"
                    ),
                    next: format!(
                        "run `{provision}`: it appends the keychain to the search list, \
                         keeping every entry there"
                    ),
                },
                Ok(Proof::Signed) => Row::Ready(format!(
                    "{shown}: open, identity {sha1} present, partition list set (a test \
                     signature passed), on the search list"
                )),
                Ok(Proof::Opened) => Row::Ready(format!(
                    "{shown}: open, identity {sha1} present, on the search list; {OPENED_NOTE}"
                )),
            },
        },
    }
}

/// Each SHA-1 once, first place kept, case ignored.
fn distinct(ids: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&id)) {
            out.push(id);
        }
    }
    out
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("failed")
        .trim()
        .to_string()
}

/// What the password file holds.
enum Stored {
    Absent,
    Password(Password),
    /// It is there and could not be read (why).
    Unreadable(String),
    /// It holds something other than the 64 hex digits provision writes.
    Malformed,
}

/// Reads the password file. Bytes that are not the hex provision writes, text or not, are
/// malformed: a file that reads is never sent to be made readable.
fn stored_password(home: &Path) -> Stored {
    match std::fs::read(password_path(home)) {
        Ok(bytes) => match std::str::from_utf8(&bytes).map(str::trim) {
            Ok(text) if aterm_codec::hex::is_hex(text, 64) => {
                Stored::Password(Password(text.to_string()))
            }
            _ => Stored::Malformed,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::Absent,
        Err(e) => Stored::Unreadable(e.to_string()),
    }
}

/// The stored password of an existing release keychain: `None` when its file is absent.
fn read_password(home: &Path) -> Result<Option<Password>, Refusal> {
    let path = password_path(home);
    match stored_password(home) {
        Stored::Absent => Ok(None),
        Stored::Password(password) => Ok(Some(password)),
        Stored::Unreadable(why) => Err(Refusal::new(
            format!("cannot read {}: {why}", path.display()),
            format!(
                "make it this user's and readable (ls -l '{}'; it is 0600)",
                path.display()
            ),
        )),
        Stored::Malformed => Err(Refusal::new(
            format!(
                "{} does not hold the 64-hex password provision writes",
                path.display()
            ),
            format!(
                "restore it from a backup; without it the release keychain cannot be opened. \
                 With none, move the keychain and this file aside together (mv '{kc}' \
                 '{kc}.old'; mv '{pass}' '{pass}.old') and run `{}`, which makes a new pair; \
                 keep the old keychain until the new one signs",
                provision_command("<this machine's id>"),
                kc = keychain_path(home).display(),
                pass = path.display(),
            ),
        )),
    }
}

/// A new random password (256 bits from the system's source, as hex), written 0600 and made
/// durable before any keychain is made with it.
fn create_password(home: &Path) -> Result<Password, Refusal> {
    use std::io::Write as _;
    create_password_with(home, &|file, bytes| {
        file.write_all(bytes)?;
        aterm_durable::durable(file)
    })
}

/// [`create_password`], `fill` writing the bytes into the new file and making them durable (a
/// test passes one that fails part-way).
fn create_password_with(
    home: &Path,
    fill: &dyn Fn(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<Password, Refusal> {
    use ring::rand::SecureRandom as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let path = password_path(home);
    let fail = |e: String| {
        Refusal::new(
            e,
            format!(
                "check ownership of {} and re-run",
                path.parent().unwrap_or(&path).display()
            ),
        )
    };
    let dir = path
        .parent()
        .ok_or_else(|| fail(format!("{} has no folder", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| fail("the system's random source failed".into()))?;
    let password = aterm_codec::hex::encode_lower(&bytes);
    // Written whole beside the file, then linked into place: a full disk or a kill leaves no
    // partial file behind to block the next run, and a link never replaces a file another run
    // put there first.
    let partial = dir.join(format!(
        ".release-keychain.pass.{}.partial",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&partial);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| fill(&mut file, format!("{password}\n").as_bytes()))
        .and_then(|()| std::fs::hard_link(&partial, &path));
    let _ = std::fs::remove_file(&partial);
    written
        .and_then(|()| aterm_durable::durable_dir(dir))
        .map_err(|e| fail(format!("{}: {e}", path.display())))?;
    Ok(Password(password))
}

/// A stand-in for `security` and `codesign` over modelled keychains, logging every argv.
#[cfg(any(test, debug_assertions))]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt as _;

    /// One modelled keychain.
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    pub struct Kc {
        pub password: String,
        pub locked: bool,
        /// SHA-1s of the certificates it holds.
        pub certs: Vec<String>,
        pub keys: usize,
        /// Whether codesign may use its key unattended.
        pub partition: bool,
    }

    impl Kc {
        /// A keychain holding the identity `sha1`, usable unattended (a login keychain a
        /// person provisioned before the release keychain).
        pub fn holding(sha1: &str) -> Self {
            Self {
                password: "the person's login password".into(),
                locked: false,
                certs: vec![sha1.into()],
                keys: 1,
                partition: true,
            }
        }

        fn identity(&self) -> impl Iterator<Item = &String> {
            self.certs.iter().filter(|_| self.keys > 0)
        }
    }

    pub struct World {
        pub keychains: BTreeMap<PathBuf, Kc>,
        pub search_list: Vec<PathBuf>,
        /// Every call: the tool's file name, then its arguments.
        pub log: Vec<Vec<String>>,
        /// The SHA-1 an imported certificate has, and its common name.
        pub cert_sha1: String,
        pub common_name: String,
        /// A `security` subcommand that fails every time it runs.
        pub fail: Option<&'static str>,
    }

    impl World {
        pub fn new(cert_sha1: &str) -> Self {
            Self {
                keychains: BTreeMap::new(),
                search_list: Vec::new(),
                log: Vec::new(),
                cert_sha1: cert_sha1.into(),
                common_name: format!(
                    "Developer ID Application: Placeholder Org ({})",
                    aterm_update_core::pins::APPLE_TEAM_ID
                ),
                fail: None,
            }
        }
    }

    pub struct Fake(pub RefCell<World>);

    pub fn out(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    const NOT_FOUND: &str = "The specified keychain could not be found.";
    const NO_UI: &str = "User interaction is not allowed.";

    impl Fake {
        pub fn new(world: World) -> Self {
            Self(RefCell::new(world))
        }

        /// The logged calls of `tool` whose first argument is `first`.
        pub fn calls(&self, tool: &str, first: &str) -> Vec<Vec<String>> {
            self.0
                .borrow()
                .log
                .iter()
                .filter(|c| c[0] == tool && c.get(1).map(String::as_str) == Some(first))
                .cloned()
                .collect()
        }

        /// Every logged call that changes a keychain or the search list.
        pub fn writes(&self) -> Vec<Vec<String>> {
            self.0
                .borrow()
                .log
                .iter()
                .filter(|c| {
                    c[0] == "security"
                        && (matches!(
                            c[1].as_str(),
                            "create-keychain"
                                | "set-keychain-settings"
                                | "unlock-keychain"
                                | "import"
                                | "set-key-partition-list"
                                | "delete-keychain"
                                | "default-keychain"
                        ) || (c[1] == "list-keychains" && c.iter().any(|a| a == "-s")))
                })
                .cloned()
                .collect()
        }
    }

    impl World {
        fn listing(&self, kcs: &[PathBuf]) -> String {
            let mut text = String::new();
            let mut n = 0;
            for path in kcs {
                if let Some(kc) = self.keychains.get(path) {
                    for sha1 in kc.identity() {
                        n += 1;
                        text.push_str(&format!("  {n}) {sha1} \"{}\"\n", self.common_name));
                    }
                }
            }
            text.push_str(&format!("     {n} valid identities found\n"));
            text
        }

        fn security(&mut self, sub: &str, rest: &[String]) -> Output {
            let last = rest.last().map(PathBuf::from).unwrap_or_default();
            if self.fail == Some(sub) {
                return out(1, "", "security: injected failure");
            }
            match sub {
                "list-keychains" => match rest.iter().position(|a| a == "-s") {
                    Some(at) => {
                        self.search_list = rest[at + 1..].iter().map(PathBuf::from).collect();
                        out(0, "", "")
                    }
                    None => {
                        let text: String = self
                            .search_list
                            .iter()
                            .map(|p| format!("    \"{}\"\n", p.display()))
                            .collect();
                        out(0, &text, "")
                    }
                },
                "create-keychain" => {
                    if self.keychains.contains_key(&last) && last.is_file() {
                        return out(48, "", "A keychain with the same name already exists.");
                    }
                    std::fs::write(&last, b"").expect("the modelled keychain's file");
                    let password = rest[1].clone();
                    self.keychains.insert(
                        last,
                        Kc {
                            password,
                            ..Kc::default()
                        },
                    );
                    out(0, "", "")
                }
                "unlock-keychain" => match self.keychains.get_mut(&last) {
                    Some(kc) if kc.password == rest[1] => {
                        kc.locked = false;
                        out(0, "", "")
                    }
                    Some(_) => out(
                        51,
                        "",
                        "The user name or passphrase you entered is not correct.",
                    ),
                    None => out(50, "", NOT_FOUND),
                },
                // A locked keychain would ask in a dialog; the model refuses instead.
                "set-keychain-settings" => match self.keychains.get(&last) {
                    Some(kc) if kc.locked => out(36, "", NO_UI),
                    Some(_) => out(0, "", ""),
                    None => out(50, "", NOT_FOUND),
                },
                "find-identity" => {
                    let named = PathBuf::from(&rest[rest.len() - 1]);
                    let kcs = if self.keychains.contains_key(&named) {
                        vec![named]
                    } else {
                        self.search_list.clone()
                    };
                    out(0, &self.listing(&kcs), "")
                }
                "import" => {
                    let at = rest
                        .iter()
                        .position(|a| a == "-k")
                        .expect("import names -k");
                    let target = PathBuf::from(&rest[at + 1]);
                    let is_key = rest.iter().any(|a| a == "priv");
                    let sha1 = self.cert_sha1.clone();
                    match self.keychains.get_mut(&target) {
                        Some(kc) if kc.locked => out(1, "", NO_UI),
                        Some(kc) if is_key && kc.keys > 0 => out(
                            1,
                            "",
                            "security: SecKeychainItemImport: The specified item already exists in the keychain.",
                        ),
                        Some(kc) if is_key => {
                            kc.keys += 1;
                            out(0, "1 key imported.", "")
                        }
                        Some(kc) if kc.certs.contains(&sha1) => out(
                            1,
                            "",
                            "security: SecKeychainItemImport: The specified item already exists in the keychain.",
                        ),
                        Some(kc) => {
                            kc.certs.push(sha1);
                            out(0, "1 certificate imported.", "")
                        }
                        None => out(1, "", NOT_FOUND),
                    }
                }
                "set-key-partition-list" => {
                    let at = rest.iter().position(|a| a == "-k").expect("-k");
                    let password = rest[at + 1].clone();
                    match self.keychains.get_mut(&last) {
                        Some(kc) if kc.password == password && kc.keys > 0 => {
                            kc.partition = true;
                            out(0, "", "")
                        }
                        Some(_) => out(
                            1,
                            "",
                            "security: SecKeychainItemSetAccessWithPassword: The user name or passphrase you entered is not correct.",
                        ),
                        None => out(50, "", NOT_FOUND),
                    }
                }
                "find-certificate" => out(
                    44,
                    "",
                    "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.",
                ),
                _ => out(2, "", "unmodelled security subcommand"),
            }
        }

        fn codesign(&mut self, args: &[String]) -> Output {
            let Some(at) = args.iter().position(|a| a == "--sign") else {
                return out(0, "", "");
            };
            let sha1 = &args[at + 1];
            let kcs = match args.iter().position(|a| a == "--keychain") {
                Some(k) => vec![PathBuf::from(&args[k + 1])],
                None => self.search_list.clone(),
            };
            let holder = kcs
                .iter()
                .filter_map(|p| self.keychains.get(p))
                .find(|kc| kc.identity().any(|c| c == sha1));
            match holder {
                None => out(1, "", &format!("{sha1}: no identity found")),
                Some(kc) if kc.locked => out(1, "", NO_UI),
                Some(kc) if !kc.partition => out(1, "", "errSecInternalComponent"),
                Some(_) => out(0, "", ""),
            }
        }
    }

    impl Run for Fake {
        fn run(
            &self,
            cmd: &mut aterm_uds::spawnfd::Command,
            _wait: std::time::Duration,
        ) -> Result<Output, RunError> {
            let program = PathBuf::from(cmd.get_program());
            let tool = program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let mut world = self.0.borrow_mut();
            let mut call = vec![tool.clone()];
            call.extend(args.iter().cloned());
            world.log.push(call);
            Ok(match tool.as_str() {
                "security" => world.security(&args[0], &args[1..]),
                "codesign" => world.codesign(&args),
                "openssl" => out(
                    0,
                    "-----BEGIN PUBLIC KEY-----\nSYNTHETIC\n-----END PUBLIC KEY-----\n",
                    "",
                ),
                _ => out(127, "", "unmodelled tool"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Kc, World};
    use super::*;
    use aterm_update_core::pins::APPLE_TEAM_ID as TEAM;

    const SHA1: &str = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";

    /// A scratch HOME with the folders macOS gives every user, and its login keychain.
    fn home() -> (aterm_tempfile::TempDir, PathBuf) {
        let dir = aterm_tempfile::tempdir().expect("scratch HOME");
        let home = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        std::fs::create_dir_all(home.join("Library/Keychains")).expect("Keychains");
        std::fs::create_dir_all(home.join(".aterm/apple")).expect(".aterm/apple");
        (dir, home)
    }

    fn mentions_login(call: &[String]) -> bool {
        call.iter().any(|a| a.contains("login.keychain"))
    }

    /// A first provision on a fresh Mac makes, opens, fills and lists the release keychain and
    /// names no other keychain in any call; the search list keeps its entries and their order.
    #[test]
    fn a_fresh_home_gets_a_release_keychain_and_the_login_keychain_is_never_named() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        let other = PathBuf::from("/Library/Keychains/Other.keychain");
        let mut world = World::new(SHA1);
        world.keychains.insert(login.clone(), Kc::default());
        world.search_list = vec![login.clone(), other.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        let release = kc.release();
        assert_eq!(
            release,
            home.join("Library/Keychains/aterm-release.keychain-db")
        );

        kc.install_pair(
            &home.join(".aterm/apple/c.cer"),
            &home.join(".aterm/apple/k.key"),
        )
        .expect("install");

        let pass = password_path(&home);
        let stored = std::fs::read_to_string(&pass).expect("the password file");
        let stored = stored.trim();
        assert!(
            aterm_codec::hex::is_hex(stored, 64),
            "256 random bits as hex"
        );
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&pass), 0o600);
            assert_eq!(mode(pass.parent().unwrap()), 0o700);
        }
        let world = fake.0.borrow();
        assert_eq!(
            world.keychains[&release],
            Kc {
                password: stored.to_string(),
                locked: false,
                certs: vec![SHA1.to_string()],
                keys: 1,
                partition: true,
            }
        );
        assert_eq!(world.keychains[&login], Kc::default(), "login untouched");
        assert_eq!(
            world.search_list,
            [login.clone(), other, release.clone()],
            "every entry kept, in order, the release keychain appended"
        );
        // Only the search-list rewrite names the login keychain, to keep it where it was.
        for call in &world.log {
            let keeps_the_list = call[1] == "list-keychains"
                && call.iter().position(|a| a == "-s").is_some_and(|at| {
                    call[at + 1] == login.display().to_string() && call.len() == at + 4
                });
            assert!(
                keeps_the_list || !mentions_login(call),
                "a call names the login keychain: {call:?}"
            );
        }
        drop(world);
        let shown = release.display().to_string();
        for call in fake.writes() {
            let target_is_release = call.last() == Some(&shown)
                || call.windows(2).any(|w| w[0] == "-k" && w[1] == shown);
            let search_list = call[1] == "list-keychains";
            assert!(
                target_is_release || search_list,
                "a write names a keychain other than the release one: {call:?}"
            );
        }
        let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        assert_eq!(
            fake.calls("security", "set-key-partition-list"),
            [argv(&[
                "security",
                "set-key-partition-list",
                "-S",
                "apple-tool:,apple:,codesign:",
                "-s",
                "-k",
                stored,
                &shown
            ])]
        );
        assert_eq!(
            fake.calls("security", "set-keychain-settings"),
            [argv(&["security", "set-keychain-settings", &shown])],
            "no -l or -u: it never locks by itself"
        );
        for import in fake.calls("security", "import") {
            assert!(
                import.windows(2).any(|w| w[0] == "-T" && w[1] == CODESIGN)
                    && import
                        .windows(2)
                        .any(|w| w[0] == "-T" && w[1] == PRODUCTSIGN),
                "{import:?}"
            );
        }
        assert_eq!(kc.identities(TEAM, true), Ok(vec![SHA1.to_string()]));
        assert_eq!(kc.holds(TEAM, SHA1), Ok(true));
        for leave in [Leave::Write, Leave::Look] {
            assert_eq!(
                kc.signing_keychain(TEAM, SHA1, leave),
                Ok(Some(release.clone()))
            );
        }
    }

    /// Converging an existing release keychain changes nothing a second time: no keychain is
    /// made, nothing imported, the search list is not rewritten, the password is kept.
    #[test]
    fn a_second_provision_makes_nothing_and_rewrites_nothing() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        let mut world = World::new(SHA1);
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let password = std::fs::read(password_path(&home)).unwrap();
        let before = fake.0.borrow().keychains.clone();
        let list = fake.0.borrow().search_list.clone();
        for _ in 0..2 {
            fake.0
                .borrow_mut()
                .keychains
                .get_mut(&kc.release())
                .unwrap()
                .locked = true;
            fake.0.borrow_mut().log.clear();
            kc.converge(TEAM).expect("converge");
            let world = fake.0.borrow();
            assert_eq!(world.keychains, before);
            assert_eq!(world.search_list, list);
            drop(world);
            for (sub, why) in [
                ("create-keychain", "nothing is made"),
                ("import", "nothing is imported"),
            ] {
                assert!(fake.calls("security", sub).is_empty(), "{why}");
            }
            assert!(
                fake.writes().iter().all(|c| c[1] != "list-keychains"),
                "the search list is not rewritten"
            );
            let shown = kc.release().display().to_string();
            assert_eq!(
                fake.calls("security", "set-keychain-settings"),
                [["security", "set-keychain-settings", shown.as_str()].map(String::from)],
                "never locks by itself, re-asserted with no -l or -u"
            );
        }
        assert_eq!(std::fs::read(password_path(&home)).unwrap(), password);

        // A Mac with no release keychain converges with no call at all.
        let (_dir, bare) = self::home();
        let fake = Fake::new(World::new(SHA1));
        Keychains::new(&bare, &fake).converge(TEAM).expect("no-op");
        assert!(fake.0.borrow().log.is_empty());
    }

    /// The search list is appended to, never replaced, and never written from an empty read.
    #[test]
    fn the_search_list_write_keeps_every_entry_in_order() {
        let (_dir, home) = home();
        let entries = vec![
            login_keychain_path(&home),
            PathBuf::from("/Users//x/Library/Keychains/aterm-dev.keychain-db"),
            PathBuf::from("/Library/Keychains/System.keychain"),
        ];
        let mut world = World::new(SHA1);
        world.search_list = entries.clone();
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let mut want = entries;
        want.push(kc.release());
        assert_eq!(fake.0.borrow().search_list, want);

        let (_dir, home) = self::home();
        let fake = Fake::new(World::new(SHA1));
        let refused = Keychains::new(&home, &fake)
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err("an empty read is never written back");
        assert!(refused.what.contains("read back empty"), "{refused:?}");
        assert!(fake.0.borrow().search_list.is_empty());
    }

    /// A Mac provisioned before the release keychain signs from its login keychain: the
    /// identity is found there read-only, codesign gets no `--keychain`, and nothing writes.
    #[test]
    fn a_login_keychain_identity_is_accepted_and_left_alone() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        let mut world = World::new(SHA1);
        world.keychains.insert(login.clone(), Kc::holding(SHA1));
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        assert_eq!(kc.identities(TEAM, true), Ok(vec![SHA1.to_string()]));
        assert_eq!(kc.holds(TEAM, SHA1), Ok(false));
        for leave in [Leave::Write, Leave::Look] {
            assert_eq!(kc.signing_keychain(TEAM, SHA1, leave), Ok(None));
        }
        kc.converge(TEAM).expect("nothing to converge");
        assert_eq!(kc.state(TEAM), Ok(State::Missing));
        let row = kc.row("m9", TEAM, &|_| {
            panic!("nothing of the release keychain to probe")
        });
        assert!(
            matches!(&row, Row::Ready(line) if line.contains(SHA1) && line.contains("is absent")),
            "{row:?}"
        );
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        assert_eq!(fake.0.borrow().keychains[&login], Kc::holding(SHA1));
    }

    /// A release keychain whose password is gone, or wrong, is reported and never deleted, and
    /// codesign is never pointed at it (a locked keychain asks in a dialog).
    #[test]
    fn a_release_keychain_that_will_not_open_is_reported_not_removed() {
        let (_dir, home) = home();
        let fake = Fake::new(World::new(SHA1));
        let kc = Keychains::new(&home, &fake);
        fake.0.borrow_mut().search_list = vec![login_keychain_path(&home)];
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let release = kc.release();
        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&release)
            .unwrap()
            .locked = true;
        std::fs::remove_file(password_path(&home)).unwrap();
        let State::Locked {
            refusal,
            holds: true,
        } = kc.state(TEAM).expect("state")
        else {
            panic!("a keychain with no password file, holding the identity, is locked to us");
        };
        let file = password_path(&home).display().to_string();
        assert!(refusal.what.contains(&file), "{refusal:?}");
        assert!(!refusal.what.contains("does not unlock"), "{refusal:?}");
        assert!(refusal.fix.contains("move it aside"), "{refusal:?}");
        assert!(!refusal.fix.contains("login"), "{refusal:?}");
        for leave in [Leave::Write, Leave::Look] {
            assert!(kc.signing_keychain(TEAM, SHA1, leave).is_err());
            assert!(prove_can_sign(&kc, SHA1, leave).is_err());
        }
        assert!(kc.converge(TEAM).is_err());
        assert!(release.is_file(), "never deleted");
        assert!(fake.calls("security", "delete-keychain").is_empty());
        assert!(
            fake.calls("codesign", "--force").is_empty(),
            "the test signature never reached codesign"
        );

        std::fs::write(password_path(&home), format!("{}\n", "0".repeat(64))).unwrap();
        let State::Locked { refusal, .. } = kc.state(TEAM).expect("state") else {
            panic!("a wrong password does not open it");
        };
        assert!(refusal.what.contains("does not unlock"), "{refusal:?}");

        // A listing that fails is no proof the keychain holds nothing: codesign is not run.
        fake.0.borrow_mut().fail = Some("find-identity");
        fake.0.borrow_mut().log.clear();
        for leave in [Leave::Write, Leave::Look] {
            assert!(kc.signing_keychain(TEAM, SHA1, leave).is_err());
            assert!(prove_can_sign(&kc, SHA1, leave).is_err());
        }
        assert!(fake.calls("codesign", "--force").is_empty());
    }

    /// An unopenable release keychain that holds no identity (one provision made only for an
    /// issuing intermediate, its password file since lost) blocks nothing on a Mac whose search
    /// list holds the identity: the row reads Ready and converge leaves it. With no identity
    /// anywhere it still blocks.
    #[test]
    fn an_unopenable_release_keychain_holding_nothing_blocks_no_login_keychain_mac() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        let mut world = World::new(SHA1);
        world.keychains.insert(login.clone(), Kc::holding(SHA1));
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_certificate(&home.join("issuer.cer"))
            .expect("the intermediate");
        let release = kc.release();
        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&release)
            .unwrap()
            .locked = true;
        std::fs::remove_file(password_path(&home)).unwrap();
        fake.0.borrow_mut().log.clear();

        assert!(matches!(
            kc.state(TEAM),
            Ok(State::Locked { holds: false, .. })
        ));
        let row = kc.row("m9", TEAM, &|_| panic!("nothing in it to probe"));
        assert!(
            matches!(&row, Row::Ready(line) if line.contains(SHA1) && line.contains("holds no")),
            "{row:?}"
        );
        kc.converge(TEAM).expect("nothing to converge in it");
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        assert!(release.is_file(), "never deleted");

        fake.0.borrow_mut().keychains.remove(&login);
        assert!(matches!(
            kc.row("m9", TEAM, &|_| panic!("nothing to probe")),
            Row::Blocked { .. }
        ));
    }

    /// A provision re-run converges a release keychain that lost its partition list or its
    /// place on the search list: both are restored with the keychain's own password, every
    /// other search-list entry kept in its order.
    #[test]
    fn converge_restores_the_partition_list_and_the_search_list_entry() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        let other = PathBuf::from("/Library/Keychains/Other.keychain");
        let mut world = World::new(SHA1);
        world.search_list = vec![login.clone(), other.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let release = kc.release();
        {
            let mut world = fake.0.borrow_mut();
            let kept = world.keychains.get_mut(&release).unwrap();
            kept.partition = false;
            kept.locked = true;
            world.search_list = vec![login.clone(), other.clone()];
        }
        kc.converge(TEAM).expect("converge");
        let world = fake.0.borrow();
        assert!(
            world.keychains[&release].partition,
            "partition list restored"
        );
        assert_eq!(world.search_list, [login, other, release]);
    }

    /// codesign is never handed the release keychain while its key lacks the partition list
    /// (a provision stopped between the import and `set-key-partition-list`): the list is
    /// re-asserted with the keychain's own password first, so the test signature passes with
    /// no dialog.
    #[test]
    fn codesign_never_meets_a_key_without_its_partition_list() {
        let (_dir, home) = home();
        let mut world = World::new(SHA1);
        world.search_list = vec![login_keychain_path(&home)];
        world.fail = Some("set-key-partition-list");
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err("stopped before the partition list");
        let release = kc.release();
        {
            let mut world = fake.0.borrow_mut();
            world.fail = None;
            let stopped = world.keychains.get_mut(&release).unwrap();
            assert!(!stopped.partition && stopped.keys == 1, "{stopped:?}");
            stopped.locked = true;
        }
        fake.0.borrow_mut().log.clear();

        assert_eq!(
            prove_can_sign(&kc, SHA1, Leave::Write),
            Ok(Proof::Signed),
            "the test signature"
        );
        let log = fake.0.borrow().log.clone();
        let at = |tool: &str, first: &str| {
            log.iter()
                .position(|c| c[0] == tool && c[1] == first)
                .unwrap_or_else(|| panic!("no {tool} {first}: {log:?}"))
        };
        assert!(at("security", "set-key-partition-list") < at("codesign", "--force"));
        assert!(fake.0.borrow().keychains[&release].partition);
    }

    /// A password file another run put there first is never replaced and no partial file is
    /// left beside it; a malformed one no keychain uses is moved aside, never kept or "restored".
    #[test]
    fn the_password_file_is_never_replaced_and_a_malformed_one_is_moved_aside() {
        let (_dir, home) = home();
        let path = password_path(&home);
        let first = format!("{}\n", "a".repeat(64));
        std::fs::write(&path, &first).unwrap();
        create_password(&home).expect_err("a password file is already there");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        let dir = path.parent().unwrap();
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["release-keychain.pass"], "no partial file is left");

        std::fs::write(&path, "half").unwrap();
        let fake = Fake::new(World::new(SHA1));
        let refused = Keychains::new(&home, &fake)
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err("a malformed password file is not used");
        assert!(refused.fix.contains(".bad'"), "{refused:?}");
        for word in ["backup", "keep"] {
            assert!(!refused.fix.contains(word), "{refused:?}");
        }
        assert!(fake.calls("security", "create-keychain").is_empty());
    }

    /// A release-keychain path that is a link to the login keychain is refused before any call
    /// is made, so nothing is imported into the login keychain through it.
    #[test]
    fn a_release_keychain_path_leading_to_login_is_refused() {
        let (_dir, home) = home();
        let login = login_keychain_path(&home);
        std::fs::write(&login, b"").unwrap();
        let mut world = World::new(SHA1);
        world.keychains.insert(login.clone(), Kc::default());
        world.search_list = vec![login.clone()];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        std::os::unix::fs::symlink(&login, kc.release()).unwrap();
        let refused = kc
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err("a link to login");
        assert!(
            refused.what.contains("login or System keychain"),
            "{refused:?}"
        );
        assert!(kc.install_certificate(&home.join("issuer.cer")).is_err());
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        assert_eq!(fake.0.borrow().keychains[&login], Kc::default());
    }

    /// A release path aliases an owned scratch login keychain. The model accepts the stored
    /// fixture password and exposes its identity at that alias, as security would after
    /// following the link; the path guard must refuse before either operation is attempted.
    fn release_alias() -> (aterm_tempfile::TempDir, PathBuf, Fake) {
        let (dir, home) = home();
        let login = login_keychain_path(&home);
        let release = keychain_path(&home);
        std::fs::write(&login, b"scratch login keychain").unwrap();
        std::os::unix::fs::symlink(&login, &release).unwrap();
        let password = "0".repeat(64);
        std::fs::write(password_path(&home), format!("{password}\n")).unwrap();
        let mut world = World::new(SHA1);
        world.keychains.insert(
            release.clone(),
            Kc {
                password,
                locked: true,
                partition: false,
                ..Kc::holding(SHA1)
            },
        );
        world.search_list = vec![release];
        (dir, home, Fake::new(world))
    }

    /// A matching password never grants leave to unlock or change a protected target through
    /// the release path, in either a real signing pass or a look.
    #[test]
    fn signing_a_release_alias_to_login_is_refused_before_unlock() {
        let (_dir, home, fake) = release_alias();
        let kc = Keychains::new(&home, &fake);
        let mut failures = Vec::new();
        for leave in [Leave::Write, Leave::Look] {
            fake.0.borrow_mut().log.clear();
            let result = kc.signing_keychain(TEAM, SHA1, leave);
            let unlocks = fake.calls("security", "unlock-keychain").len();
            let partitions = fake.calls("security", "set-key-partition-list").len();
            if result.is_ok() || unlocks != 0 || partitions != 0 {
                failures.push(format!(
                    "{leave:?}: expected refusal before calls, got {result:?}, \
                     unlocks={unlocks}, partition writes={partitions}"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Whichever `security` step fails, the refusal says which, never the password itself; the
    /// password's `Debug` is redacted.
    #[test]
    fn no_refusal_carries_the_password() {
        for (step, said_as) in [
            ("create-keychain", "security create-keychain"),
            ("unlock-keychain", "does not unlock"),
            ("set-keychain-settings", "security set-keychain-settings"),
            ("import", "security import (certificate)"),
            ("set-key-partition-list", "security set-key-partition-list"),
            ("list-keychains", "security list-keychains -d user"),
        ] {
            let (_dir, home) = home();
            let mut world = World::new(SHA1);
            world.search_list = vec![login_keychain_path(&home)];
            world.fail = Some(step);
            let fake = Fake::new(world);
            let refused = Keychains::new(&home, &fake)
                .install_pair(&home.join("c.cer"), &home.join("k.key"))
                .expect_err(step);
            let password = read_password(&home)
                .expect("readable")
                .expect("written before any keychain is made");
            let secret = password.0.clone();
            let said = format!("{} {} {refused:?}", refused.what, refused.fix);
            assert!(refused.what.contains(said_as), "{step}: {said}");
            assert!(
                !said.contains(&secret),
                "{step} leaked the password: {said}"
            );
            assert!(!format!("{password:?}").contains(&secret));
        }
    }

    /// A state, what the search list holds, the test signature's outcome, and the row's kind.
    type Case<'a> = (
        State,
        &'a dyn Fn() -> Option<String>,
        &'a dyn Fn(&str) -> Result<Proof, String>,
        &'a str,
    );

    /// Each state reads as one line whose fix runs provision and never asks for the login
    /// password.
    #[test]
    fn each_state_says_what_to_run() {
        let release = Path::new("/h/Library/Keychains/aterm-release.keychain-db");
        let none = || None;
        let legacy = || Some(SHA1.to_string());
        let passes = |_: &str| Ok(Proof::Signed);
        let opened = |_: &str| Ok(Proof::Opened);
        let fails = |_: &str| Err("errSecInternalComponent".to_string());
        let open = |listed| State::Open {
            identities: vec![SHA1.into()],
            listed,
        };
        let locked = |holds| State::Locked {
            refusal: Refusal::new("locked", "move it aside"),
            holds,
        };
        let empty = State::Open {
            identities: Vec::new(),
            listed: true,
        };
        let cases: [Case<'_>; 13] = [
            (State::Missing, &none, &passes, "todo"),
            (State::Missing, &legacy, &passes, "ready"),
            (locked(false), &none, &passes, "blocked"),
            (locked(false), &legacy, &passes, "ready"),
            (locked(true), &none, &passes, "blocked"),
            (locked(true), &legacy, &passes, "blocked"),
            (empty.clone(), &none, &passes, "todo"),
            (empty, &legacy, &passes, "ready"),
            (open(true), &none, &fails, "blocked"),
            (open(false), &none, &passes, "todo"),
            (open(true), &none, &passes, "ready"),
            (open(false), &none, &opened, "todo"),
            (open(true), &none, &opened, "ready"),
        ];
        for (state, elsewhere, probe, want) in cases {
            let row = judge("m9", release, state.clone(), elsewhere, probe);
            let (kind, text) = match &row {
                Row::Ready(line) => ("ready", line.clone()),
                Row::Todo { what, next } => ("todo", format!("{what} {next}")),
                Row::Blocked { what, fix } => ("blocked", format!("{what} {fix}")),
            };
            assert_eq!(kind, want, "{state:?}: {row:?}");
            assert!(
                !text.contains("login password") || text.contains("never your login"),
                "{text}"
            );
            if kind != "ready" && !matches!(state, State::Locked { .. }) {
                assert!(text.contains("provision --id m9"), "{text}");
            }
        }
        match judge("m9", release, open(true), &none, &passes) {
            Row::Ready(line) => {
                for word in ["open", SHA1, "partition list set", "on the search list"] {
                    assert!(line.contains(word), "{line}");
                }
            }
            other => panic!("{other:?}"),
        }
        match judge("m9", release, open(true), &none, &opened) {
            Row::Ready(line) => {
                assert!(line.contains(OPENED_NOTE), "{line}");
                assert!(!line.contains("partition list set"), "{line}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Each state of the password file gets the fix that gets past it. With no keychain: one
    /// that is not provision's hex (text or not) is moved aside, one that cannot be read is
    /// made readable and kept with any keychain moved aside. With the keychain there, a
    /// malformed one is restored or moved aside together with the keychain.
    #[test]
    fn each_password_file_state_gets_a_fix_that_gets_past_it() {
        let make = |kind: &str| {
            let (dir, home) = home();
            let path = password_path(&home);
            match kind {
                "binary" => std::fs::write(&path, [0xff, 0xfe, 0x00, 0x9f]).unwrap(),
                "text" => std::fs::write(&path, "half\n").unwrap(),
                _ => std::fs::create_dir(&path).unwrap(),
            }
            (dir, home)
        };
        for kind in ["binary", "text"] {
            let (_dir, home) = make(kind);
            assert!(
                matches!(stored_password(&home), Stored::Malformed),
                "{kind}"
            );
            let fake = Fake::new(World::new(SHA1));
            let refused = Keychains::new(&home, &fake)
                .install_pair(&home.join("c.cer"), &home.join("k.key"))
                .expect_err(kind);
            assert!(refused.fix.contains(".bad'"), "{kind}: {refused:?}");
            assert!(!refused.fix.contains("readable"), "{kind}: {refused:?}");
            assert!(fake.calls("security", "create-keychain").is_empty());
        }

        let (_dir, home) = make("unreadable");
        assert!(matches!(stored_password(&home), Stored::Unreadable(_)));
        let fake = Fake::new(World::new(SHA1));
        let refused = Keychains::new(&home, &fake)
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err("a file that cannot be read");
        for word in ["readable", "move the two together"] {
            assert!(refused.fix.contains(word), "{refused:?}");
        }
        assert!(fake.calls("security", "create-keychain").is_empty());

        let (_dir, home) = self::home();
        let mut world = World::new(SHA1);
        world.search_list = vec![login_keychain_path(&home)];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        std::fs::write(password_path(&home), "half\n").unwrap();
        let refused = kc.unlock().expect_err("a malformed file opens nothing");
        let pass = password_path(&home).display().to_string();
        let shown = kc.release().display().to_string();
        assert!(refused.fix.contains("backup"), "{refused:?}");
        for moved in [&pass, &shown] {
            assert!(
                refused.fix.contains(&format!("mv '{moved}'")),
                "both move aside together: {refused:?}"
            );
        }
    }

    /// A password write that fails part-way leaves no password file and no partial file, so
    /// the next run writes a whole one.
    #[test]
    fn a_password_write_that_fails_part_way_leaves_nothing() {
        let (_dir, home) = home();
        let refused = create_password_with(&home, &|file, bytes| {
            std::io::Write::write_all(file, &bytes[..10])?;
            Err(std::io::Error::other("no space left on device"))
        })
        .expect_err("the write failed");
        assert!(refused.what.contains("no space left"), "{refused:?}");
        let path = password_path(&home);
        assert!(!path.exists(), "no partial password file");
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");

        let written = create_password(&home).expect("the next run writes one");
        assert!(matches!(
            stored_password(&home),
            Stored::Password(stored) if stored.0 == written.0
        ));
    }
}

/// Developer ID Application identities belonging to the expected team.
pub fn devid_identities(listing: &str, team_id: &str) -> Vec<String> {
    let team_tag = format!("({team_id})");
    listing
        .lines()
        .filter(|line| line.contains("Developer ID Application:") && line.contains(&team_tag))
        .filter_map(|line| {
            line.split_whitespace()
                .find(|word| aterm_codec::hex::is_hex(word, 40))
        })
        .map(str::to_owned)
        .collect()
}

/// Prove that codesign can use this identity after the private keychain's access list is set.
/// A read-only look can only report that a private keychain opened.
pub fn prove_can_sign(kc: &Keychains<'_>, sha1: &str, leave: Leave) -> Result<Proof, String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let keychain = kc.signing_keychain(aterm_update_core::pins::APPLE_TEAM_ID, sha1, leave)?;
    if leave == Leave::Look && keychain.is_some() {
        return Ok(Proof::Opened);
    }
    let dir =
        aterm_tempfile::tempdir().map_err(|error| format!("create signing probe: {error}"))?;
    let probe = dir.path().join("probe");
    let bytes =
        std::fs::read("/bin/echo").map_err(|error| format!("read signing probe: {error}"))?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&probe)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|error| format!("stage signing probe: {error}"))?;
    let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"--force", &"--timestamp=none"];
    if let Some(keychain) = &keychain {
        args.extend([&"--keychain" as &dyn AsRef<std::ffi::OsStr>, keychain]);
    }
    args.extend([&"--sign" as &dyn AsRef<std::ffi::OsStr>, &sha1, &probe]);
    kc.codesign(&args)?;
    kc.codesign(&[&"--verify", &"--strict", &probe])?;
    Ok(Proof::Signed)
}

/// Import an already-authorized Developer ID pair into the private keychain, restore its
/// unattended access list and prove it signs. A matching identity in another keychain does
/// not skip the import. This never generates a certificate or asks for a login password.
pub fn prepare_existing_pair(
    home: &Path,
    cert: &Path,
    key: &Path,
    expected_sha1: &str,
) -> Result<(), Refusal> {
    prepare_existing_pair_with(
        &Keychains::new(home, &crate::keychain_tools::Bounded),
        cert,
        key,
        expected_sha1,
    )
}

fn prepare_existing_pair_with(
    kc: &Keychains<'_>,
    cert: &Path,
    key: &Path,
    expected_sha1: &str,
) -> Result<(), Refusal> {
    use std::os::unix::fs::PermissionsExt as _;
    kc.lock()
        .map_err(|why| Refusal::new(why, "automatic setup will retry"))?;
    let team = aterm_update_core::pins::APPLE_TEAM_ID;
    if !aterm_codec::hex::is_hex(expected_sha1, 40) {
        return Err(Refusal::new(
            "the delivered certificate identity is malformed",
            "wait for a valid signed credential update",
        ));
    }
    validate_pair(kc, cert, key)
        .map_err(|why| Refusal::new(why, "await a matching authorized signing pair"))?;
    recover_owned_keychain(kc)?;
    if !kc.release().is_file()
        || !kc
            .holds(team, expected_sha1)
            .map_err(|why| Refusal::new(why, "retry the keychain service"))?
    {
        kc.install_pair(cert, key)?;
    }
    std::fs::set_permissions(kc.release(), std::fs::Permissions::from_mode(0o600)).map_err(
        |error| {
            Refusal::new(
                format!("secure the owned signing keychain: {error}"),
                "automatic setup will retry",
            )
        },
    )?;
    if !kc
        .holds(team, expected_sha1)
        .map_err(|why| Refusal::new(why, "automatic setup will retry"))?
    {
        repair_issuer(kc, cert)
            .map_err(|why| Refusal::new(why, "automatic chain repair will retry"))?;
    }
    kc.converge(team)?;
    if !kc
        .holds(team, expected_sha1)
        .map_err(|why| Refusal::new(why, "retry the keychain service"))?
    {
        return Err(Refusal::new(
            "the private keychain does not hold the delivered Developer ID identity",
            "wait for a matching signed certificate and key",
        ));
    }
    match prove_can_sign(kc, expected_sha1, Leave::Write) {
        Ok(Proof::Signed) => Ok(()),
        Ok(Proof::Opened) => Err(Refusal::new(
            "the signing proof did not run",
            "retry the automatic signing check",
        )),
        Err(why) => Err(Refusal::new(why, "retry the automatic keychain repair")),
    }
}

fn validate_pair(kc: &Keychains<'_>, cert: &Path, key: &Path) -> Result<(), String> {
    let public_key = kc.call("/usr/bin/openssl", &[&"pkey", &"-in", &key, &"-pubout"])?;
    if !public_key.status.success() {
        return Err("the delivered private signing key is unreadable".into());
    }
    for form in ["PEM", "DER"] {
        let certificate = kc.call(
            "/usr/bin/openssl",
            &[
                &"x509", &"-inform", &form, &"-in", &cert, &"-noout", &"-pubkey",
            ],
        )?;
        if certificate.status.success() {
            if certificate.stdout != public_key.stdout {
                return Err("the delivered certificate and private key do not match".into());
            }
            return Ok(());
        }
    }
    Err("the delivered Developer ID certificate is unreadable".into())
}

/// Fetch only Apple's two published Developer ID issuers. Matching both the leaf's issuer
/// name and authority key identifier prevents installing an unrelated downloaded CA; the
/// platform's valid-identity check still establishes the complete trusted chain afterward.
fn repair_issuer(kc: &Keychains<'_>, leaf: &Path) -> Result<(), String> {
    let report = |path: &Path| -> Result<String, String> {
        for form in ["PEM", "DER"] {
            let out = kc.call(
                "/usr/bin/openssl",
                &[
                    &"x509",
                    &"-inform",
                    &form,
                    &"-in",
                    &path,
                    &"-noout",
                    &"-issuer",
                    &"-subject",
                    &"-text",
                ],
            )?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
        }
        Err("the delivered signing certificate is unreadable".into())
    };
    let leaf = report(leaf)?;
    let issuer = cert_name(&leaf, "issuer=").ok_or("the signing certificate has no issuer")?;
    if !issuer.contains("Developer ID Certification Authority") {
        return Err("the signing certificate has no Apple Developer ID issuer".into());
    }
    let dir = aterm_tempfile::tempdir().map_err(|error| format!("stage Apple issuer: {error}"))?;
    let cert = dir.path().join("issuer.cer");
    for url in [
        "https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer",
        "https://www.apple.com/certificateauthority/DeveloperIDCA.cer",
    ] {
        let out = kc.call(
            "/usr/bin/curl",
            &[
                &"-fsSL",
                &"--proto",
                &"=https",
                &"--proto-redir",
                &"=https",
                &"--max-time",
                &"45",
                &"-o",
                &cert,
                &url,
            ],
        )?;
        if !out.status.success() {
            continue;
        }
        let candidate = report(&cert)?;
        if issuer_matches(&leaf, &candidate) {
            kc.install_certificate(&cert)?;
            return Ok(());
        }
    }
    Err("no published Apple CA matches the delivered certificate's issuer and key".into())
}

fn cert_name<'a>(report: &'a str, field: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|line| line.trim().strip_prefix(field).map(str::trim))
}

fn cert_key_id(report: &str, header: &str) -> Option<String> {
    let mut lines = report
        .lines()
        .skip_while(|line| !line.trim().starts_with(header));
    let inline = lines.next()?.trim().strip_prefix(header)?.trim();
    let raw = if inline.is_empty() {
        lines.next()?.trim()
    } else {
        inline
    };
    let hex: String = raw
        .trim_start_matches("keyid:")
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .collect();
    (!hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_uppercase())
}

fn issuer_matches(leaf: &str, candidate: &str) -> bool {
    cert_name(leaf, "issuer=")
        .zip(cert_name(candidate, "subject="))
        .is_some_and(|(a, b)| a == b)
        && cert_key_id(leaf, "X509v3 Authority Key Identifier:")
            .zip(cert_key_id(candidate, "X509v3 Subject Key Identifier:"))
            .is_some_and(|(a, b)| a == b)
}

/// Preserve an unusable owned keychain and password before rebuilding from authenticated
/// material. A missing or malformed local password is deterministically unusable; a service
/// timeout is not. Renaming a symlink preserves the link itself, never its target.
fn recover_owned_keychain(kc: &Keychains<'_>) -> Result<(), Refusal> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let release = kc.release();
    let password = password_path(kc.home());
    let release_meta = std::fs::symlink_metadata(&release).ok();
    let password_meta = std::fs::symlink_metadata(&password).ok();
    for (path, meta) in [
        (&release, release_meta.as_ref()),
        (&password, password_meta.as_ref()),
    ] {
        if let Some(meta) = meta {
            if meta.uid() != aterm_uds::peer::our_uid() || meta.is_dir() || meta.nlink() != 1 {
                return Err(Refusal::new(
                    "owned signing material has an unexpected owner or file type",
                    "automatic setup will retry",
                ));
            }
            if meta.is_file() && meta.mode() & 0o7777 != 0o600 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                    |error| {
                        Refusal::new(
                            format!("secure owned signing material: {error}"),
                            "automatic setup will retry",
                        )
                    },
                )?;
            }
        }
    }
    let linked_password = password_meta
        .as_ref()
        .is_some_and(|meta| meta.file_type().is_symlink());
    let invalid_password =
        linked_password || matches!(stored_password(kc.home()), Stored::Malformed);
    let missing_password = password_meta.is_none() && release_meta.is_some();
    let linked_keychain = release_meta
        .as_ref()
        .is_some_and(|meta| meta.file_type().is_symlink());
    let rejected_password =
        if !invalid_password && !missing_password && !linked_keychain && release_meta.is_some() {
            let Some(password) = read_password(kc.home())? else {
                return Ok(());
            };
            let out = kc
                .security(&[&"unlock-keychain", &"-p", &password, &release])
                .map_err(|why| Refusal::new(why, "automatic setup will retry"))?;
            if out.status.success() {
                false
            } else {
                let detail = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
                if detail.contains("passphrase you entered is not correct")
                    || detail.contains("not a valid keychain")
                {
                    true
                } else {
                    return Err(Refusal::new(
                        "the keychain service did not unlock the owned keychain",
                        "automatic setup will retry",
                    ));
                }
            }
        } else {
            false
        };
    if !(invalid_password || missing_password || linked_keychain || rejected_password) {
        return Ok(());
    }
    for meta in [release_meta.as_ref(), password_meta.as_ref()]
        .into_iter()
        .flatten()
    {
        if meta.uid() != aterm_uds::peer::our_uid() || meta.is_dir() {
            return Err(Refusal::new(
                "the unusable keychain material is not an owned file",
                "automatic repair awaits ownership of its own files",
            ));
        }
    }
    let root = kc.home().join(".aterm/apple/quarantine");
    crate::fsx::ensure_private_dir(&root)
        .map_err(|error| Refusal::new(error.to_string(), "retry preserving the old keychain"))?;
    let mut slot = None;
    for n in 0..16u32 {
        let candidate = root.join(format!("release-keychain-{}-{n}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => {
                slot = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Refusal::new(
                    error.to_string(),
                    "retry preserving the old keychain",
                ));
            }
        }
    }
    let slot = slot.ok_or_else(|| {
        Refusal::new(
            "no free keychain quarantine slot",
            "automatic repair will retry",
        )
    })?;
    let kept_password = slot.join("release-keychain.pass");
    if password_meta.is_some() {
        std::fs::rename(&password, &kept_password).map_err(|error| {
            Refusal::new(error.to_string(), "retry preserving the old password")
        })?;
    }
    if release_meta.is_some()
        && let Err(error) = std::fs::rename(&release, slot.join(FILE_NAME))
    {
        if password_meta.is_some() {
            let _ = std::fs::rename(&kept_password, &password);
        }
        return Err(Refusal::new(
            error.to_string(),
            "retry preserving the old keychain",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod automatic_tests {
    use super::fake::{Fake, Kc, World};
    use super::*;

    #[test]
    fn a_mismatched_delivered_pair_changes_no_keychain() {
        struct WrongPair;
        impl Run for WrongPair {
            fn run(
                &self,
                cmd: &mut aterm_uds::spawnfd::Command,
                _: std::time::Duration,
            ) -> Result<Output, RunError> {
                assert_eq!(Path::new(cmd.get_program()).file_name().unwrap(), "openssl");
                Ok(super::fake::out(
                    0,
                    if cmd.get_args().any(|arg| arg == "pkey") {
                        "key A"
                    } else {
                        "key B"
                    },
                    "",
                ))
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let kc = Keychains::new(root.path(), &WrongPair);
        let refusal = prepare_existing_pair_with(
            &kc,
            &root.path().join("cert"),
            &root.path().join("key"),
            "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678",
        )
        .unwrap_err();
        assert!(refusal.what.contains("do not match"));
        assert!(!kc.release().exists());
        assert!(!root.path().join(".aterm/apple/quarantine").exists());
    }

    #[test]
    fn issuer_repair_requires_both_the_exact_issuer_and_issuing_key() {
        let leaf = "issuer= CN = Developer ID Certification Authority\nX509v3 Authority Key Identifier:\n keyid:AA:BB:CC\n";
        let ca = "subject= CN = Developer ID Certification Authority\nX509v3 Subject Key Identifier:\n AA:BB:CC\n";
        assert!(issuer_matches(leaf, ca));
        assert!(!issuer_matches(leaf, &ca.replace("AA:BB:CC", "AA:BB:DD")));
        assert!(!issuer_matches(leaf, &ca.replace("Authority", "Other")));
        assert!(!issuer_matches(
            leaf,
            "subject= CN = Developer ID Certification Authority\n"
        ));
    }

    #[test]
    fn nested_same_thread_leases_do_not_admit_a_concurrent_thread() {
        let root = aterm_tempfile::tempdir().unwrap();
        let first = signing_lease(root.path()).unwrap();
        let nested = signing_lease(root.path()).unwrap();
        let home = root.path().to_path_buf();
        assert!(
            std::thread::spawn(
                move || signing_lease_before(&home, std::time::Instant::now())
                    .unwrap_err()
                    .contains("busy")
            )
            .join()
            .unwrap()
        );
        let home = root.path().to_path_buf();
        assert!(
            std::thread::spawn(move || signing_lease(&home).unwrap_err().contains("busy"))
                .join()
                .unwrap()
        );
        drop(nested);
        drop(first);
        let home = root.path().to_path_buf();
        assert!(
            std::thread::spawn(move || signing_lease(&home).is_ok())
                .join()
                .unwrap()
        );
    }

    #[test]
    fn signing_lease_process_child() {
        let Some(home) = std::env::var_os("ATERM_SIGNING_LEASE_TEST_HOME") else {
            return;
        };
        let _lease = signing_lease(Path::new(&home)).unwrap();
        println!("signing-lease-held");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }

    #[test]
    fn another_process_defers_while_the_signing_lease_is_held() {
        use aterm_uds::spawnfd::Stdio;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "release_keychain::automatic_tests::signing_lease_process_child",
                "--nocapture",
            ])
            .env("ATERM_SIGNING_LEASE_TEST_HOME", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        aterm_uds::deadline::prints(&mut child, "signing-lease-held", WAIT);
        assert!(signing_lease(root.path()).unwrap_err().contains("busy"));
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + WAIT;
        let mut exited = aterm_uds::deadline::ExitWait::new(&child);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signing lease child did not exit");
            }
            exited.wait(deadline.saturating_duration_since(std::time::Instant::now()));
        }
        assert!(signing_lease(root.path()).is_ok());
    }

    #[test]
    fn the_owned_lock_is_secured_and_a_link_is_never_followed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path().join(".aterm/apple");
        crate::fsx::ensure_private_dir(&dir).unwrap();
        let lock = dir.join(".signing.lock");
        std::fs::write(&lock, b"").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
        drop(signing_lease(root.path()).unwrap());
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&lock).unwrap();
        let other = root.path().join("untouched");
        std::fs::write(&other, b"keep").unwrap();
        symlink(&other, &lock).unwrap();
        assert!(signing_lease(root.path()).is_err());
        assert_eq!(std::fs::read(other).unwrap(), b"keep");
    }

    #[test]
    fn a_busy_signing_transaction_defers_before_quarantining_any_material() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path();
        std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
        let release = keychain_path(home);
        std::fs::write(&release, b"still in use").unwrap();
        let lock = std::fs::File::create(home.join(".aterm/apple/.signing.lock")).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "stand in for another process holding the signing transaction"
        )]
        lock.lock().unwrap();
        let fake = Fake::new(World::new("A1B2C3D4E5F60718293A4B5C6D7E8F9012345678"));
        let kc = Keychains::new(home, &fake);
        assert!(
            prepare_existing_pair_with(
                &kc,
                &home.join("cert"),
                &home.join("key"),
                "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678"
            )
            .unwrap_err()
            .what
            .contains("busy")
        );
        assert_eq!(std::fs::read(release).unwrap(), b"still in use");
        assert!(!home.join(".aterm/apple/quarantine").exists());
        assert!(fake.calls("security", "create-keychain").is_empty());
    }

    #[test]
    fn a_login_identity_never_skips_the_private_import_and_acl_repair() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path();
        std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
        let sha1 = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";
        let login = login_keychain_path(home);
        let original = Kc::holding(sha1);
        let mut world = World::new(sha1);
        world.keychains.insert(login.clone(), original.clone());
        world.search_list.push(login.clone());
        let fake = Fake::new(world);
        let kc = Keychains::new(home, &fake);
        let cert = home.join(".aterm/apple/devid-shared.cer");
        let key = home.join(".aterm/apple/devid-shared.key");
        prepare_existing_pair_with(&kc, &cert, &key, sha1).unwrap();
        assert_eq!(fake.0.borrow().keychains[&login], original);
        assert_eq!(fake.calls("security", "import").len(), 2);
        assert!(fake.calls("codesign", "--force").iter().all(|args| {
            args.windows(2)
                .any(|pair| pair == ["--keychain", kc.release().to_str().unwrap()])
        }));
        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&kc.release())
            .unwrap()
            .partition = false;
        prepare_existing_pair_with(&kc, &cert, &key, sha1).unwrap();
        assert!(fake.0.borrow().keychains[&kc.release()].partition);
        assert_eq!(fake.calls("security", "import").len(), 2);
        assert_eq!(fake.calls("codesign", "--force").len(), 2);
    }

    #[test]
    fn a_missing_or_rejected_password_preserves_the_old_keychain_and_rebuilds_without_input() {
        for wrong_password in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path();
            std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
            std::fs::create_dir_all(home.join(".aterm/apple")).unwrap();
            let sha1 = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";
            let release = keychain_path(home);
            std::fs::write(&release, b"preserve this unknown old keychain").unwrap();
            let mut world = World::new(sha1);
            world.keychains.insert(release.clone(), Kc::holding(sha1));
            world.search_list.push(release.clone());
            if wrong_password {
                let _ = create_password(home).unwrap();
            }
            let fake = Fake::new(world);
            let kc = Keychains::new(home, &fake);
            prepare_existing_pair_with(&kc, &home.join("cert"), &home.join("key"), sha1).unwrap();
            let quarantine = home.join(".aterm/apple/quarantine");
            let slot = std::fs::read_dir(&quarantine)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(
                std::fs::read(slot.join(FILE_NAME)).unwrap(),
                b"preserve this unknown old keychain"
            );
            assert!(password_path(home).is_file());
            assert_eq!(fake.calls("security", "create-keychain").len(), 1);
            assert_eq!(fake.calls("codesign", "--force").len(), 1);
        }
    }
}
