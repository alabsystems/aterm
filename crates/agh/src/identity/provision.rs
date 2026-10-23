// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agh auth add` and `agh auth remove` (design §6.7).

#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::path::Path;

use aterm_toml::edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value,
};

use crate::client::{Github, Meter};
use crate::config::{self, Dirs};
use crate::error::{Error, Result};
use crate::identity::TokenKind;
use crate::policy::Tier;

#[cfg(unix)]
use crate::config::{CONFIG_FILE, ConfigFault, Validated};
#[cfg(unix)]
use crate::error::IdentityRef;
#[cfg(unix)]
use crate::identity::{self, Credential, LoadError, Profile, Source, tilde};
#[cfg(unix)]
use crate::policy::Router;

/// Where `auth add` takes the token from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddFrom {
    /// A private token file, read in place: agh.toml names it, and agh never copies it.
    File(std::path::PathBuf),
    /// Standard input, written to `<config>/tokens/NAME.token`.
    Stdin,
}

/// What `auth add` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddRequest {
    /// The profile's name.
    pub name: String,
    /// Where the token comes from.
    pub from: AddFrom,
    /// The profile's tier.
    pub tier: Tier,
    /// The owners it reads for (`--owner`).
    pub owners: Vec<String>,
    /// Whether it becomes those owners' writer (`--write`).
    pub write: bool,
    /// Whether a person vouches for its drafts (`--drafts`).
    pub drafts: bool,
}

/// What `auth add` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    /// The profile's name.
    pub profile: String,
    /// The login `GET /user` bound it to.
    pub login: String,
    /// Where its token file is.
    pub token_file: std::path::PathBuf,
    /// The token's fingerprint.
    pub fingerprint: String,
    /// The token's kind.
    pub kind: TokenKind,
    /// The owners whose rows name it.
    pub owners: Vec<String>,
}

/// The refusal of provisioning off unix.
#[cfg(not(unix))]
const OFF_UNIX: &str =
    "provisioning needs unix permission checks; agh on Windows is anonymous only";

/// Provision `req`.
/// # Errors
/// Refused for a loose `--file` (with its remedy), a fault the edit would make, or a tier change;
/// Unauthenticated when `GET /user` refuses the token; could_not_run on I/O.
pub fn add(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    stdin: &mut dyn std::io::Read,
) -> Result<Added> {
    add_metered(gh, dirs, req, stdin).map(|(added, _)| added)
}

/// `add`, with the meter of the one-profile client that sent `GET /user` as the new token.
/// # Errors
/// As `add`.
pub(crate) fn add_metered(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    stdin: &mut dyn std::io::Read,
) -> Result<(Added, Meter)> {
    #[cfg(unix)]
    {
        add_unix(gh, dirs, req, stdin)
    }
    #[cfg(not(unix))]
    {
        let _ = (gh, dirs, req, stdin);
        Err(Error::refused(OFF_UNIX))
    }
}

/// What `auth remove` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The profile's name.
    pub profile: String,
    /// Whether its token file was deleted.
    pub token_file_deleted: bool,
    /// The owners whose rows changed.
    pub owners_edited: Vec<String>,
}

/// Remove profile `name` and every reference to it.
/// # Errors
/// Usage for an unknown profile; Refused when the result would fault; could_not_run on I/O.
pub fn remove(dirs: &Dirs, name: &str) -> Result<Removed> {
    #[cfg(unix)]
    {
        remove_unix(dirs, name)
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, name);
        Err(Error::refused(OFF_UNIX))
    }
}

/// The pure edit `add` makes (aterm-toml's `edit` document, comments kept).
/// # Errors
/// Usage when the text does not parse, when `profiles` or the profile's row is not a table, or, for
/// a non-empty `req.owners`, when `owners` is not `[[owners]]` tables.
pub fn edit_config(
    text: &str,
    req: &AddRequest,
    token_file: &str,
    login: &str,
    fingerprint: &str,
) -> Result<String> {
    let mut doc = parse_document(text)?;
    let name = req.name.as_str();
    let root = doc.as_table_mut();
    if root.get("profiles").is_none() {
        let mut profiles = Table::new();
        profiles.set_implicit(true);
        root.insert("profiles", Item::Table(profiles));
    }
    let profiles = root
        .get_mut("profiles")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| {
            Error::usage("agh.toml's profiles is not a table").with_next(config::POLICY_CHECK)
        })?;
    match profiles.get_mut(name) {
        Some(item) => {
            let row = item.as_table_like_mut().ok_or_else(|| {
                Error::usage(format!("agh.toml's profiles.{name} is not a table"))
                    .with_next(config::POLICY_CHECK)
            })?;
            set_value(row, "token_file", Value::from(token_file));
            set_value(row, "login", Value::from(login));
            row.remove("env");
            row.remove("covers_all_repos");
            if req.drafts {
                set_value(row, "drafts_fp", Value::from(fingerprint));
            } else {
                row.remove("drafts_fp");
            }
        }
        None => {
            let mut row = Table::new();
            row.insert("token_file", Item::Value(Value::from(token_file)));
            row.insert("tier", Item::Value(Value::from(req.tier.slug())));
            row.insert("login", Item::Value(Value::from(login)));
            if req.drafts {
                row.insert("drafts_fp", Item::Value(Value::from(fingerprint)));
            }
            profiles.insert(name, Item::Table(row));
        }
    }
    if req.owners.is_empty() {
        return Ok(doc.to_string());
    }
    let mut owners = owner_tables(&doc)?;
    for owner in &req.owners {
        let found = owners
            .iter_mut()
            .find(|table| owner_name(&**table).is_some_and(|n| n.eq_ignore_ascii_case(owner)));
        match found {
            Some(table) => {
                let mut read = vec![name.to_string()];
                read.extend(read_names(table).into_iter().filter(|step| step != name));
                set_value(table, "read", read_array(&read));
                if req.write {
                    set_value(table, "write", Value::from(name));
                }
            }
            None => {
                let mut table = Table::new();
                table.insert("name", Item::Value(Value::from(owner.as_str())));
                table.insert("tier", Item::Value(Value::from(req.tier.slug())));
                table.insert("read", Item::Value(read_array(&[name.to_string()])));
                if req.write {
                    table.insert("write", Item::Value(Value::from(name)));
                }
                owners.push(table);
            }
        }
    }
    set_owner_tables(&mut doc, owners);
    Ok(doc.to_string())
}

/// The pure edit `remove` makes: the new text and the owners whose rows changed.
/// # Errors
/// Usage when the text does not parse, names no profile `name`, or has an `owners` that is neither
/// `[[owners]]` tables nor an array of inline tables.
pub fn remove_from_config(text: &str, name: &str) -> Result<(String, Vec<String>)> {
    let mut doc = parse_document(text)?;
    let present = doc
        .get("profiles")
        .and_then(|profiles| profiles.get(name))
        .is_some();
    if !present {
        return Err(crate::config::no_profile(name, "agh auth status"));
    }
    doc.remove_path_pruning(&["profiles", name]);
    let mut edited = Vec::new();
    match doc.get("owners") {
        None => {}
        Some(item) if item.is_array_of_tables() => {
            let mut owners = owner_tables(&doc)?;
            for table in &mut owners {
                unlist(table, name, &mut edited);
            }
            if !edited.is_empty() {
                set_owner_tables(&mut doc, owners);
            }
        }
        Some(item) => {
            let mut owners: Vec<InlineTable> = item
                .as_array()
                .and_then(|array| {
                    array
                        .iter()
                        .map(|value| value.as_inline_table().cloned())
                        .collect::<Option<_>>()
                })
                .ok_or_else(not_owner_tables)?;
            for table in &mut owners {
                unlist(table, name, &mut edited);
            }
            if !edited.is_empty() {
                let array: Array = owners.into_iter().map(Value::from).collect();
                set_value(doc.as_table_mut(), "owners", Value::from(array));
            }
        }
    }
    if let Some(default) = doc
        .as_table_mut()
        .get_mut("default")
        .and_then(Item::as_table_like_mut)
    {
        drop_from_read(default, name);
    }
    Ok((doc.to_string(), edited))
}

fn parse_document(text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>().map_err(|e| {
        let at = e.span().map_or_else(String::new, |span| {
            format!(" at line {}", crate::config::line_of(text, span.start))
        });
        Error::usage(format!("agh.toml does not parse{at}")).with_next(config::POLICY_CHECK)
    })
}

fn set_value(table: &mut dyn TableLike, key: &str, mut value: Value) {
    if let Some(old) = table.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    table.insert(key, Item::Value(value));
}

fn read_array(names: &[String]) -> Value {
    Value::from(names.iter().map(String::as_str).collect::<Array>())
}

fn owner_name(table: &dyn TableLike) -> Option<&str> {
    table.get("name").and_then(Item::as_str)
}

fn unlist(table: &mut dyn TableLike, name: &str, edited: &mut Vec<String>) {
    let mut touched = drop_from_read(table, name);
    if table.get("write").and_then(Item::as_str) == Some(name) {
        table.remove("write");
        touched = true;
    }
    if touched {
        edited.push(owner_name(table).unwrap_or_default().to_string());
    }
}

fn not_owner_tables() -> Error {
    Error::usage("agh.toml's owners is not [[owners]] tables").with_next(config::POLICY_CHECK)
}

fn read_names(table: &dyn TableLike) -> Vec<String> {
    table
        .get("read")
        .and_then(Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn drop_from_read(table: &mut dyn TableLike, name: &str) -> bool {
    let read = read_names(table);
    if !read.iter().any(|step| step == name) {
        return false;
    }
    let kept: Vec<String> = read.into_iter().filter(|step| step != name).collect();
    set_value(table, "read", read_array(&kept));
    true
}

fn owner_tables(doc: &DocumentMut) -> Result<Vec<Table>> {
    match doc.get("owners") {
        None => Ok(Vec::new()),
        Some(item) => item
            .as_array_of_tables()
            .map(|tables| tables.iter().cloned().collect())
            .ok_or_else(not_owner_tables),
    }
}

fn set_owner_tables(doc: &mut DocumentMut, tables: Vec<Table>) {
    let mut array = ArrayOfTables::new();
    for table in tables {
        array.push(table);
    }
    doc.as_table_mut()
        .insert("owners", Item::ArrayOfTables(array));
}

/// `auth add`'s help, the next of its usage errors.
#[cfg(unix)]
const ADD_HELP: &str = "agh help auth add";

/// A stack buffer scrubbed on every way out of the scope that holds it.
#[cfg(unix)]
struct Scrubbed([u8; config::TOKEN_MAX + 1]);

#[cfg(unix)]
impl Drop for Scrubbed {
    fn drop(&mut self) {
        aterm_uds::secret::scrub(&mut self.0);
    }
}

/// The current agh.toml's text: `None` when there is none.
#[cfg(unix)]
fn read_current(dirs: &Dirs) -> Result<Option<String>> {
    use config::ConfigText;
    match config::read_config(&dirs.config, CONFIG_FILE, &dirs.home) {
        ConfigText::Text(text) => Ok(Some(text)),
        ConfigText::Missing => Ok(None),
        ConfigText::Io(error) => Err(Error::could_not_run(format!("agh.toml: {error}"))),
        ConfigText::NotUtf8 => Err(config::invalid(
            "agh.toml is not UTF-8".to_string(),
            config::POLICY_CHECK,
        )),
        ConfigText::Refused { error, remedy } => Err(load_refused(&error, remedy)),
    }
}

/// `config::load`'s refusal of agh.toml: `error`, with `remedy` or the policy check.
#[cfg(unix)]
fn load_refused(error: &str, remedy: Option<String>) -> Error {
    config::invalid(
        format!("agh.toml: {error}"),
        remedy.unwrap_or_else(|| config::POLICY_CHECK.to_string()),
    )
}

/// The load's refusal of agh.toml where it refuses one (`read_current`'s), else `None`.
#[cfg(unix)]
fn load_refusal_of_dir(dirs: &Dirs) -> Option<Error> {
    match config::read_config(&dirs.config, CONFIG_FILE, &dirs.home) {
        config::ConfigText::Refused { error, remedy } => Some(load_refused(&error, remedy)),
        _ => None,
    }
}

/// The refusal of an edit that would break agh.toml's rules.
#[cfg(unix)]
fn would_break(faults: &[ConfigFault], verb: &str) -> Error {
    Error::refused(format!(
        "the edit would break {} rule(s): {}",
        faults.len(),
        ConfigFault::list(faults)
    ))
    .with_next(format!("fix agh.toml, then run agh auth {verb} again"))
}

/// An error about `dir` (or a file in it) without the absolute path aterm-uds names it by: its
/// `the directory {dir}: ` lead is dropped.
#[cfg(unix)]
fn io_text(e: &std::io::Error, dir: &Path, home: &Path) -> String {
    let text = e.to_string();
    let absolute = dir.display().to_string();
    let text = text
        .strip_prefix(&format!("the directory {absolute}: "))
        .or_else(|| text.strip_prefix(&format!("the directory {absolute} ")))
        .map_or_else(|| text.clone(), str::to_string);
    text.replace(&absolute, &tilde(dir, home))
}

/// `create_private_dir(dir)`: an existing directory that is not private is Refused.
#[cfg(unix)]
fn private_dir(dir: &Path, home: &Path) -> Result<()> {
    let shown = tilde(dir, home);
    aterm_uds::private_file::create_private_dir(dir).map_err(|e| {
        let why = io_text(&e, dir, home);
        let existing = std::fs::symlink_metadata(dir);
        if e.kind() != std::io::ErrorKind::PermissionDenied || existing.is_err() {
            return Error::could_not_run(format!("{shown}: {why}"));
        }
        let error = Error::refused(format!("{shown}: {why}"));
        let remedy = match existing {
            Ok(meta) if meta.is_dir() => identity::chmod_remedy(dir, Path::new(""), home),
            _ => None,
        };
        match remedy {
            Some(remedy) => error.with_next(remedy),
            None => error,
        }
    })
}

/// `create_dir_all(dirs.config.parent())`, then `dirs.config` made private.
#[cfg(unix)]
fn config_dir(dirs: &Dirs) -> Result<()> {
    if let Some(parent) = dirs.config.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::could_not_run(format!("{}: {e}", tilde(parent, &dirs.home))))?;
    }
    private_dir(&dirs.config, &dirs.home)
}

/// A token file's load, as `auth add` answers it.
#[cfg(unix)]
fn load_refusal(error: &LoadError, profile: &Profile, home: &Path, path: &Path) -> Error {
    match error {
        LoadError::Missing => {
            Error::usage(format!("no such token file: {}", tilde(path, home))).with_next(ADD_HELP)
        }
        LoadError::Io(_) | LoadError::Unavailable(_) => {
            Error::could_not_run(format!("{}: {error}", tilde(path, home)))
        }
        LoadError::Loose(_)
        | LoadError::Symlink
        | LoadError::Malformed(_)
        | LoadError::Unsupported(_) => {
            let refused = Error::refused(format!("{}: {error}", tilde(path, home)));
            match error.remedy(profile, home) {
                Some(remedy) => refused.with_next(remedy),
                None => refused,
            }
        }
    }
}

/// Read stdin into `buf` until EOF or the buffer is full; the bytes read.
#[cfg(unix)]
fn read_stdin(stdin: &mut dyn std::io::Read, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match stdin.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::could_not_run(format!("stdin: {e}"))),
        }
    }
    Ok(filled)
}

/// The `.agh-new` file a stdin token is written to before it is put in place.
#[cfg(unix)]
fn new_token_name(name: &str) -> String {
    format!("{name}.token{STAGED_SUFFIX}")
}

/// The suffix of a staged token's name, `.token.agh-new` less its `.token`.
#[cfg(unix)]
const STAGED_SUFFIX: &str = ".agh-new";

/// One cooperative auth edit.
#[cfg(unix)]
struct AuthEdit(std::fs::File);

#[cfg(unix)]
impl AuthEdit {
    fn take(dirs: &Dirs) -> Result<Self> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let as_load = |refused: Error| load_refusal_of_dir(dirs).unwrap_or(refused);
        config_dir(dirs).map_err(as_load)?;
        let path = dirs.config.join(".auth-edit.lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::could_not_run(format!("the auth edit lock: {e}"))),
        }
        let (file, meta) =
            aterm_uds::private_file::open_private(&dirs.config, ".auth-edit.lock")
                .map_err(|e| as_load(Error::refused(format!("the auth edit lock: {e}"))))?;
        if meta.len() != 0 {
            return Err(Error::refused(
                "the auth edit lock is occupied by a nonempty file",
            ));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the auth transaction guard releases by LOCK_UN before closing"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::refused("another agh auth edit is in progress")
                    .with_next("run the auth command again after that edit finishes"));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(Error::could_not_run(format!("the auth edit lock: {e}")));
            }
        }
        let held = Self(file);
        let same = std::fs::symlink_metadata(&path).is_ok_and(|named| {
            named.file_type().is_file() && (named.dev(), named.ino()) == (meta.dev(), meta.ino())
        });
        if !same {
            return Err(Error::could_not_run(
                "the auth edit lock changed while it was acquired",
            ));
        }
        Ok(held)
    }
}

#[cfg(unix)]
impl Drop for AuthEdit {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A staging name this transaction created exclusively.
#[cfg(unix)]
struct StagedToken {
    path: std::path::PathBuf,
    file: std::fs::File,
    placed: bool,
}

#[cfg(unix)]
impl StagedToken {
    fn still_named(&self) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::symlink_metadata(&self.path), self.file.metadata()) {
            (Ok(named), Ok(open)) => {
                named.file_type().is_file()
                    && (named.dev(), named.ino()) == (open.dev(), open.ino())
            }
            _ => false,
        }
    }

    fn place(mut self, dest: &Path) -> Result<()> {
        if !self.still_named() {
            return Err(Error::could_not_run(
                "the staged token changed during agh auth add",
            ));
        }
        std::fs::rename(&self.path, dest)
            .map_err(|e| Error::could_not_run(format!("{}: {e}", dest.display())))?;
        self.placed = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for StagedToken {
    fn drop(&mut self) {
        if !self.placed && self.still_named() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether `path` is (or resolves to) a staged token of agh's.
#[cfg(unix)]
fn is_staged(dirs: &Dirs, path: &Path) -> bool {
    let Ok(tokens) = std::fs::canonicalize(dirs.tokens()) else {
        return false;
    };
    let staged_in_tokens = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(&format!(".token{STAGED_SUFFIX}")))
            && p.parent()
                .and_then(|d| std::fs::canonicalize(d).ok())
                .is_some_and(|d| d == tokens)
    };
    staged_in_tokens(path) || std::fs::canonicalize(path).is_ok_and(|c| staged_in_tokens(&c))
}

/// Whether a profile of `config` names the file `path`, both compared after `canonicalize`.
#[cfg(unix)]
fn names_file(config: &Validated, path: &Path) -> bool {
    let Ok(target) = std::fs::canonicalize(path) else {
        return false;
    };
    config.profiles().values().any(|p| match &p.source {
        Source::File(file) => std::fs::canonicalize(file).is_ok_and(|f| f == target),
        Source::Env(_) => false,
    })
}

/// Validate stdin before making any directory or taking the transaction lock.
#[cfg(unix)]
fn read_stdin_token(
    dirs: &Dirs,
    req: &AddRequest,
    stdin: &mut dyn std::io::Read,
) -> Result<(Scrubbed, usize)> {
    let mut buf = Scrubbed([0u8; config::TOKEN_MAX + 1]);
    let read = read_stdin(stdin, &mut buf.0)?;
    if read > config::TOKEN_MAX {
        return Err(
            Error::usage(format!("a token is at most {} bytes", config::TOKEN_MAX))
                .with_next(ADD_HELP),
        );
    }
    let token = match identity::token_bytes(&buf.0[..read]) {
        Ok(token) => token,
        Err(LoadError::Malformed(why)) => return Err(Error::usage(why).with_next(ADD_HELP)),
        Err(other) => return Err(Error::usage(other.to_string()).with_next(ADD_HELP)),
    };
    if let Err(error) = TokenKind::of(token) {
        let profile = Profile::new(
            req.name.clone(),
            Source::File(dirs.tokens().join(format!("{}.token", req.name))),
            req.tier,
        );
        let refused = Error::refused(error.to_string());
        return Err(match error.remedy(&profile, &dirs.home) {
            Some(remedy) => refused.with_next(remedy),
            None => refused,
        });
    }
    let n = token.len();
    buf.0[n] = b'\n';
    Ok((buf, n + 1))
}

/// Stage the validated token while its auth edit lock is held.
#[cfg(unix)]
fn stage_stdin(dirs: &Dirs, req: &AddRequest, token: &[u8]) -> Result<StagedToken> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    private_dir(&dirs.tokens(), &dirs.home)?;
    let name = new_token_name(&req.name);
    let path = dirs.tokens().join(name);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| {
            let shown = tilde(&path, &dirs.home);
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::refused(format!("the staging path {shown} is occupied"))
                    .with_next("recover or move that file before provisioning this profile again")
            } else {
                Error::could_not_run(format!("{shown}: {e}"))
            }
        })?;
    let mut staged = StagedToken {
        path,
        file,
        placed: false,
    };
    staged
        .file
        .write_all(token)
        .and_then(|()| aterm_durable::durable(&staged.file))
        .map_err(|e| Error::could_not_run(format!("the staged token: {e}")))?;
    Ok(staged)
}

/// `add` on unix (plan §11.2).
#[cfg(unix)]
fn add_unix(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    stdin: &mut dyn std::io::Read,
) -> Result<(Added, Meter)> {
    if let Some(fault) = config::profile_name_fault(&req.name) {
        return Err(Error::usage(fault).with_next(ADD_HELP));
    }
    let named = match &req.from {
        AddFrom::File(path) => path.clone(),
        AddFrom::Stdin => dirs.tokens().join(format!("{}.token", req.name)),
    };
    if named.to_str().is_none() {
        return Err(Error::usage(format!(
            "the token file {} is not a UTF-8 path, and agh.toml can name only a UTF-8 path",
            tilde(&named, &dirs.home)
        ))
        .with_next(ADD_HELP));
    }
    if let AddFrom::File(path) = &req.from
        && is_staged(dirs, path)
    {
        return Err(Error::usage(format!(
            "{} is agh's staging file: move it to a name of its own first",
            tilde(path, &dirs.home)
        ))
        .with_next(ADD_HELP));
    }
    let stdin_token = match &req.from {
        AddFrom::Stdin => Some(read_stdin_token(dirs, req, stdin)?),
        AddFrom::File(path) => {
            let profile = Profile::new(req.name.clone(), Source::File(path.clone()), req.tier);
            identity::load(&profile, gh.env())
                .map_err(|e| load_refusal(&e, &profile, &dirs.home, path))?;
            None
        }
    };
    let _edit = AuthEdit::take(dirs)?;
    let text = read_current(dirs)?.unwrap_or_default();
    let current = if text.is_empty() {
        None
    } else {
        let current = config::judge(&text, &dirs.home).verdict.map_err(|_| {
            Error::refused("agh.toml is invalid; agh auth add edits only a valid file")
                .with_next(config::POLICY_CHECK)
        })?;
        if let Some(old) = current.profiles().get(&req.name)
            && old.tier != req.tier
        {
            return Err(Error::refused(format!(
                "profile {} is tier {}; a tier change is agh auth remove {}, then agh auth add",
                req.name,
                old.tier.slug(),
                req.name
            )));
        }
        Some(current)
    };
    edit_config(&text, req, "", "", "")?;
    match &req.from {
        AddFrom::File(path) => {
            let profile = Profile::new(req.name.clone(), Source::File(path.clone()), req.tier);
            let cred = identity::load(&profile, gh.env())
                .map_err(|e| load_refusal(&e, &profile, &dirs.home, path))?;
            let (added, new_text, meter) = bind_and_edit(gh, dirs, req, &text, path, path, &cred)?;
            write_config(dirs, &new_text, None)?;
            Ok((added, meter))
        }
        AddFrom::Stdin => {
            let staging = dirs.tokens().join(new_token_name(&req.name));
            if current.as_ref().is_some_and(|c| names_file(c, &staging)) {
                return Err(Error::refused(format!(
                    "agh.toml names agh's staging file {} as a token_file",
                    tilde(&staging, &dirs.home)
                ))
                .with_next("move that token to a name of its own and fix agh.toml first"));
            }
            let Some((buf, len)) = stdin_token.as_ref() else {
                return Err(Error::could_not_run("the stdin token was not checked"));
            };
            let staged = stage_stdin(dirs, req, &buf.0[..*len])?;
            let token_file = named;
            let placed: Result<_> = (|| {
                let profile = Profile::new(
                    req.name.clone(),
                    Source::File(staged.path.clone()),
                    req.tier,
                );
                let cred = identity::load(&profile, gh.env())
                    .map_err(|e| load_refusal(&e, &profile, &dirs.home, &staged.path))?;
                let added = bind_and_edit(gh, dirs, req, &text, &staged.path, &token_file, &cred)?;
                staged.place(&token_file)?;
                Ok(added)
            })();
            let (added, new_text, meter) = placed?;
            aterm_durable::durable_dir(&dirs.tokens()).map_err(|e| {
                Error::could_not_run(format!(
                    "{}: {e}; the new token is in place at {}, and agh.toml is as it was",
                    tilde(&dirs.tokens(), &dirs.home),
                    tilde(&token_file, &dirs.home)
                ))
            })?;
            write_config(dirs, &new_text, Some(&token_file))?;
            Ok((added, meter))
        }
    }
}

/// Steps 5 and 6.
#[cfg(unix)]
fn bind_and_edit(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    text: &str,
    source: &Path,
    token_file: &Path,
    cred: &Credential,
) -> Result<(Added, String, Meter)> {
    let profile = Profile::new(
        req.name.clone(),
        Source::File(source.to_path_buf()),
        req.tier,
    );
    let validated = Validated {
        profiles: BTreeMap::from([(req.name.clone(), profile)]),
        owners: BTreeMap::new(),
        default_read: Vec::new(),
        targets: BTreeMap::new(),
    };
    let bound = gh.with_router(Router::from_validated(validated));
    let login = bound.users().me(&req.name)?.login;
    let meter = bound.meter();
    let sent_as_loaded = matches!(
        &meter.as_,
        Some(IdentityRef::Profile { name, fingerprint })
            if *name == req.name && fingerprint == cred.fingerprint()
    );
    if !sent_as_loaded {
        return Err(Error::could_not_run(format!(
            "the token file {} changed during agh auth add",
            tilde(source, &dirs.home)
        ))
        .with_next("run agh auth add again"));
    }
    let new_text = edit_config(
        text,
        req,
        &tilde(token_file, &dirs.home),
        &login,
        cred.fingerprint(),
    )?;
    config::judge(&new_text, &dirs.home)
        .verdict
        .map_err(|faults| would_break(&faults, "add"))?;
    let added = Added {
        profile: req.name.clone(),
        login,
        token_file: token_file.to_path_buf(),
        fingerprint: cred.fingerprint().to_string(),
        kind: cred.kind(),
        owners: req.owners.clone(),
    };
    Ok((added, new_text, meter))
}

/// Step 8.
#[cfg(unix)]
fn write_config(dirs: &Dirs, text: &str, placed: Option<&Path>) -> Result<()> {
    let written = config_dir(dirs).and_then(|()| {
        aterm_uds::private_file::write_private(&dirs.config, CONFIG_FILE, text.as_bytes()).map_err(
            |e| {
                Error::could_not_run(format!(
                    "{}: {}",
                    tilde(&dirs.config_file(), &dirs.home),
                    io_text(&e, &dirs.config, &dirs.home)
                ))
            },
        )
    });
    match (written, placed) {
        (Ok(()), _) => Ok(()),
        (Err(error), None) => Err(error),
        (Err(mut error), Some(token)) => {
            error.why = format!(
                "{}; the new token is in place at {}, and agh.toml is as it was",
                error.why,
                tilde(token, &dirs.home)
            );
            Err(error)
        }
    }
}

/// `remove` on unix (plan §11.2).
#[cfg(unix)]
fn remove_unix(dirs: &Dirs, name: &str) -> Result<Removed> {
    let _edit = AuthEdit::take(dirs)?;
    let Some(text) = read_current(dirs)? else {
        return Err(
            Error::usage("no agh.toml: there is no profile to remove").with_next("agh auth status")
        );
    };
    let current = config::judge(&text, &dirs.home).verdict.map_err(|_| {
        Error::refused("agh.toml is invalid; agh auth remove edits only a valid file")
            .with_next(config::POLICY_CHECK)
    })?;
    let Some(profile) = current.profiles().get(name) else {
        return Err(crate::config::no_profile(name, "agh auth status"));
    };
    let own = dirs.tokens().join(format!("{name}.token"));
    let canonical_own = std::fs::canonicalize(&own).ok();
    let names_own = |source: &Source| match source {
        Source::File(path) => {
            canonical_own.is_some() && std::fs::canonicalize(path).ok() == canonical_own
        }
        Source::Env(_) => false,
    };
    let (new_text, owners_edited) = remove_from_config(&text, name)?;
    let after = config::judge(&new_text, &dirs.home)
        .verdict
        .map_err(|faults| would_break(&faults, "remove"))?;
    let own_dir = aterm_uds::private_file::check_private_dir(&dirs.tokens()).is_ok();
    let deletable = own_dir
        && names_own(&profile.source)
        && !after.profiles().values().any(|p| names_own(&p.source));
    write_config(dirs, &new_text, None)?;
    let token_file_deleted = if deletable {
        aterm_uds::private_file::remove_private(&dirs.tokens(), &format!("{name}.token")).map_err(
            |e| {
                Error::could_not_run(format!(
                    "{}: {e}; agh.toml no longer names profile {name}",
                    tilde(&own, &dirs.home)
                ))
            },
        )?
    } else {
        false
    };
    Ok(Removed {
        profile: name.to_string(),
        token_file_deleted,
        owners_edited,
    })
}
