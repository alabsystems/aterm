// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agh auth add` and `agh auth remove` (design §6.7).

#[cfg(any(unix, windows))]
use std::collections::BTreeMap;
#[cfg(any(unix, windows))]
use std::path::Path;
#[cfg(any(unix, windows))]
use std::time::Duration;

use aterm_toml::edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value,
};

use crate::client::{Github, Meter};
use crate::config::{self, Dirs};
use crate::error::{Error, Result};
use crate::identity::TokenKind;
use crate::policy::Tier;

#[cfg(any(unix, windows))]
use crate::config::{CONFIG_FILE, ConfigFault, Validated};
#[cfg(any(unix, windows))]
use crate::error::IdentityRef;
#[cfg(any(unix, windows))]
use crate::identity::{self, Credential, LoadError, Profile, Source, tilde};
#[cfg(any(unix, windows))]
use crate::policy::Router;

/// What `auth add` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddRequest {
    /// The profile's name.
    pub name: String,
    /// `--file`: `~/.secrets/NAME`, which names the vault entry NAME (read in place from the
    /// store; no file there is opened), or a private token file of the person's own, read in
    /// place. agh.toml names it as given, and agh never copies it (`config::token_source`).
    pub file: std::path::PathBuf,
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
    /// Where its token is: a vault entry, or a token file of the person's own.
    pub source: crate::identity::Source,
    /// The token's fingerprint.
    pub fingerprint: String,
    /// The token's kind.
    pub kind: TokenKind,
    /// The owners whose rows name it.
    pub owners: Vec<String>,
}

/// The refusal of provisioning on a host with no private files (neither unix nor windows).
#[cfg(not(any(unix, windows)))]
const NO_PRIVATE_FILES: &str =
    "provisioning needs private files, which agh has on unix and windows only";

/// Provision `req`. With no agh.toml and either vault GitHub entry in the store, the new agh.toml
/// starts with `vault = true` (`vault::seed`), so the vault's rows keep joining as adoption
/// derives them, rotations included (design §6.10); a profile named `dev` or `release`, or one
/// whose `--file` names one of those two entries, starts a plain agh.toml that routes alone
/// instead.
/// # Errors
/// Usage for a profile `dev` or `release` beside an agh.toml that says `vault = true`, before
/// anything is sent; refused for a loose `--file` (with its remedy), a fault the edit would
/// make, or a tier change; Unauthenticated when `GET /user` refuses the token; could_not_run on
/// I/O.
pub fn add(gh: &Github, dirs: &Dirs, req: &AddRequest) -> Result<Added> {
    add_with(gh, dirs, req, AddOptions::default())
}

/// `auth add`'s options beyond M1's request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AddOptions {
    /// `--legacy-loose`: write `legacy_loose = true` on the profile and accept a loose
    /// `--file` for the login binding (design §10.3's provisioning commands).
    pub legacy_loose: bool,
}

impl AddOptions {
    /// The same options with `legacy_loose`.
    #[must_use]
    pub fn legacy_loose(self, on: bool) -> AddOptions {
        AddOptions {
            legacy_loose: on,
            ..self
        }
    }
}

/// [`add`] with `options`. `add(gh, dirs, req)` is
/// `add_with(gh, dirs, req, AddOptions::default())`. With `options.legacy_loose`, a
/// `--file` with a group or other read bit (on the file or its directory) is read through
/// `read_owned_into` for the login binding, and the profile's row says `legacy_loose = true`;
/// a link, another owner's file, and a group or other write bit are refused as without it.
/// Without it, a re-add drops the row's `legacy_loose`.
///
/// # Errors
///
/// As [`add`].
pub fn add_with(gh: &Github, dirs: &Dirs, req: &AddRequest, options: AddOptions) -> Result<Added> {
    add_with_metered(gh, dirs, req, options).map(|(added, _)| added)
}

/// `add_with`, with the meter of the one-profile client that sent `GET /user` as the new
/// token.
///
/// # Errors
///
/// As [`add_with`].
pub(crate) fn add_with_metered(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    options: AddOptions,
) -> Result<(Added, Meter)> {
    #[cfg(any(unix, windows))]
    {
        add_edit(gh, dirs, req, options)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (gh, dirs, req, options);
        Err(Error::refused(NO_PRIVATE_FILES))
    }
}

/// What `auth remove` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The profile's name.
    pub profile: String,
    /// The owners whose rows changed.
    pub owners_edited: Vec<String>,
    /// Whether the vault's tokens route after it, as agh adopts them, rotations included: there
    /// is no agh.toml, or it says `vault = true` (`join_vault` sets it when the last profile it
    /// had of the vault's went).
    pub joins_vault: bool,
}

/// Remove profile `name` and every reference to it. With no agh.toml, or one that says
/// `vault = true`, `dev` or `release` forgets the login vault adoption recorded for that token
/// (design §6.10), so the next call learns it again. When the removed profile names one of the
/// vault's two GitHub entries, agh.toml names no other such profile and none named `dev` or
/// `release`, and the store holds either entry, agh.toml is made to join the vault
/// (`join_vault`), which is how a machine provisioned before adoption comes to it.
/// # Errors
/// Usage for an unknown profile; Refused when the result would fault; could_not_run on I/O.
pub fn remove(dirs: &Dirs, name: &str) -> Result<Removed> {
    #[cfg(any(unix, windows))]
    {
        remove_edit(dirs, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (dirs, name);
        Err(Error::refused(NO_PRIVATE_FILES))
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
    edit_config_with(text, req, token_file, login, fingerprint, false)
}

/// `edit_config`, with the profile's row saying `legacy_loose = true` when `legacy_loose`, and
/// the key removed when not, so a re-add without `--legacy-loose` tightens the row.
/// # Errors
/// As `edit_config`.
pub(crate) fn edit_config_with(
    text: &str,
    req: &AddRequest,
    token_file: &str,
    login: &str,
    fingerprint: &str,
    legacy_loose: bool,
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
            row.remove("covers_all_repos");
            if req.drafts {
                set_value(row, "drafts_fp", Value::from(fingerprint));
            } else {
                row.remove("drafts_fp");
            }
            if legacy_loose {
                set_value(row, "legacy_loose", Value::from(true));
            } else {
                row.remove("legacy_loose");
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
            if legacy_loose {
                row.insert("legacy_loose", Item::Value(Value::from(true)));
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
            let mut owners = inline_owners(item)?;
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

/// `new` (what `remove_from_config` left of `old`) made to join the vault (design §6.10):
/// `vault = true` set, and the rows the vault's profiles left behind dropped, so the adopted
/// rows route those owners: `[default]` when it names no profile, and each owner row that names
/// none and either named one in `old`, reads through no step at all, or is the public owner's
/// (on the release floor whatever is written). Any other row that names no profile (an owner
/// read anonymously by choice) stands, as do the comments of what stands.
/// # Errors
/// Usage when either text does not parse, or its `owners` is neither `[[owners]]` tables nor
/// an array of inline tables.
pub fn join_vault(old: &str, new: &str) -> Result<String> {
    let old_doc = parse_document(old)?;
    let mut doc = parse_document(new)?;
    let routed: Vec<String> = inline_or_tables(&old_doc)?
        .into_iter()
        .filter(|(_, routes)| *routes)
        .map(|(name, _)| name)
        .collect();
    let emptied = |table: &dyn TableLike| {
        !routes(table)
            && (read_strs(table).next().is_none()
                || owner_name(table).is_some_and(|name| {
                    routed.iter().any(|r| r.eq_ignore_ascii_case(name))
                        || name.eq_ignore_ascii_case(aterm_types::channel::PUBLIC_OWNER)
                }))
    };
    match doc.get("owners") {
        None => {}
        Some(item) if item.is_array_of_tables() => {
            let owners = owner_tables(&doc)?;
            if owners.iter().any(|table| emptied(table)) {
                let kept: Vec<Table> = owners.into_iter().filter(|table| !emptied(table)).collect();
                if kept.is_empty() {
                    doc.as_table_mut().remove("owners");
                } else {
                    set_owner_tables(&mut doc, kept);
                }
            }
        }
        Some(item) => {
            let owners = inline_owners(item)?;
            if owners.iter().any(|table| emptied(table)) {
                let kept: Array = owners
                    .into_iter()
                    .filter(|table| !emptied(table))
                    .map(Value::from)
                    .collect();
                if kept.is_empty() {
                    doc.as_table_mut().remove("owners");
                } else {
                    set_value(doc.as_table_mut(), "owners", Value::from(kept));
                }
            }
        }
    }
    if doc
        .get("default")
        .and_then(Item::as_table_like)
        .is_some_and(|table| !routes(table))
    {
        doc.as_table_mut().remove("default");
    }
    set_value(doc.as_table_mut(), "vault", Value::from(true));
    Ok(doc.to_string())
}

/// Whether an owner row or `[default]` routes to a profile: it names a writer, or a reader
/// other than anonymous.
fn routes(table: &dyn TableLike) -> bool {
    table.get("write").is_some() || read_strs(table).any(|step| step != "anonymous")
}

/// Each owner row's name, lowercased, and whether it `routes`, whichever form `owners` takes.
fn inline_or_tables(doc: &DocumentMut) -> Result<Vec<(String, bool)>> {
    let row = |table: &dyn TableLike| {
        owner_name(table).map(|name| (name.to_ascii_lowercase(), routes(table)))
    };
    Ok(match doc.get("owners") {
        None => Vec::new(),
        Some(item) if item.is_array_of_tables() => owner_tables(doc)?
            .iter()
            .filter_map(|table| row(table))
            .collect(),
        Some(item) => inline_owners(item)?
            .iter()
            .filter_map(|table| row(table))
            .collect(),
    })
}

/// `owners` as an array of inline tables.
fn inline_owners(item: &Item) -> Result<Vec<InlineTable>> {
    item.as_array()
        .and_then(|array| {
            array
                .iter()
                .map(|value| value.as_inline_table().cloned())
                .collect::<Option<_>>()
        })
        .ok_or_else(not_owner_tables)
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

fn read_strs(table: &dyn TableLike) -> impl Iterator<Item = &str> {
    table
        .get("read")
        .and_then(Item::as_array)
        .into_iter()
        .flat_map(|array| array.iter().filter_map(Value::as_str))
}

fn read_names(table: &dyn TableLike) -> Vec<String> {
    read_strs(table).map(str::to_string).collect()
}

fn drop_from_read(table: &mut dyn TableLike, name: &str) -> bool {
    if !read_strs(table).any(|step| step == name) {
        return false;
    }
    let kept: Vec<String> = read_strs(table)
        .filter(|&step| step != name)
        .map(str::to_string)
        .collect();
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
#[cfg(any(unix, windows))]
const ADD_HELP: &str = "agh help auth add";

/// The current agh.toml's text: `None` when there is none.
#[cfg(any(unix, windows))]
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
#[cfg(any(unix, windows))]
fn load_refused(error: &str, remedy: Option<String>) -> Error {
    config::invalid(
        format!("agh.toml: {error}"),
        remedy.unwrap_or_else(|| config::POLICY_CHECK.to_string()),
    )
}

/// The load's refusal of agh.toml where it refuses one (`read_current`'s), else `None`.
#[cfg(any(unix, windows))]
fn load_refusal_of_dir(dirs: &Dirs) -> Option<Error> {
    match config::read_config(&dirs.config, CONFIG_FILE, &dirs.home) {
        config::ConfigText::Refused { error, remedy } => Some(load_refused(&error, remedy)),
        _ => None,
    }
}

/// The refusal of an edit that would break agh.toml's rules. With no agh.toml before it
/// (`existed` false), there is no file to fix: the rows the command would write are what
/// to change.
#[cfg(any(unix, windows))]
fn would_break(faults: &[ConfigFault], verb: &str, existed: bool) -> Error {
    let next = if existed {
        format!("fix agh.toml, then run agh auth {verb} again")
    } else {
        format!(
            "there is no agh.toml: change the NAME, --owner or --tier of the rows above, then \
             run agh auth {verb} again"
        )
    };
    Error::refused(format!(
        "the edit would break {} rule(s): {}",
        faults.len(),
        ConfigFault::list(faults)
    ))
    .with_next(next)
}

/// `add`'s edit judged by L1–L10 before it is written.
/// # Errors
/// `would_break`'s refusal.
#[cfg(any(unix, windows))]
fn judge_edit(new_text: &str, home: &Path, existed: bool) -> Result<()> {
    config::judge(new_text, home)
        .verdict
        .map(|_| ())
        .map_err(|faults| would_break(&faults, "add", existed))
}

/// An error about `dir` (or a file in it) without the absolute path aterm-uds names it by: its
/// `the directory {dir}: ` lead is dropped.
#[cfg(any(unix, windows))]
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
#[cfg(any(unix, windows))]
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
            Ok(meta) if meta.is_dir() => private_dir_remedy(dir, home),
            _ => None,
        };
        match remedy {
            Some(remedy) => error.with_next(remedy),
            None => error,
        }
    })
}

/// The chmod that makes the existing directory `dir` private, where one of this user's does.
#[cfg(unix)]
fn private_dir_remedy(dir: &Path, home: &Path) -> Option<String> {
    identity::chmod_remedy(dir, Path::new(""), home)
}

/// On windows no chmod changes a folder's access list: the folder made the user's alone, as agh
/// makes it when it is missing.
#[cfg(windows)]
fn private_dir_remedy(dir: &Path, home: &Path) -> Option<String> {
    Some(format!(
        "make {} a private folder of yours (an access list of you and SYSTEM alone, inheriting \
         nothing), as agh makes it when it is missing",
        tilde(dir, home)
    ))
}

/// `create_dir_all(dirs.config.parent())` (`config::make_parents`, which an elevated windows
/// process makes nothing by), then `dirs.config` made private.
#[cfg(any(unix, windows))]
fn config_dir(dirs: &Dirs) -> Result<()> {
    if let Some(parent) = dirs.config.parent().filter(|p| !p.as_os_str().is_empty()) {
        config::make_parents(parent)
            .map_err(|e| Error::could_not_run(format!("{}: {e}", tilde(parent, &dirs.home))))?;
    }
    private_dir(&dirs.config, &dirs.home)
}

/// A token's load, as `auth add` answers it.
#[cfg(any(unix, windows))]
fn load_refusal(error: &LoadError, profile: &Profile, home: &Path) -> Error {
    let shown = source_shown(&profile.source, home);
    match (error, &profile.source) {
        (LoadError::Missing, Source::Entry(entry)) => Error::usage(format!(
            "the vault holds no entry {} on this Mac",
            entry.name()
        ))
        .with_next(super::vault::token_step(profile).unwrap_or_else(|| ADD_HELP.to_string())),
        (LoadError::Missing, _) => {
            Error::usage(format!("no such token file: {shown}")).with_next(ADD_HELP)
        }
        (LoadError::Io(_) | LoadError::Unavailable(_), _) => {
            Error::could_not_run(format!("{shown}: {error}"))
        }
        (
            LoadError::Loose(_)
            | LoadError::Symlink
            | LoadError::Malformed(_)
            | LoadError::Unsupported(_),
            _,
        ) => {
            let refused = Error::refused(format!("{shown}: {error}"));
            match error.remedy(profile, home) {
                Some(remedy) => refused.with_next(remedy),
                None => refused,
            }
        }
    }
}

/// One cooperative auth edit (`auth add`, `auth remove`), or, under its own lock file, one
/// write of vault adoption's record (design §6.10), so that an ordinary read that records a
/// learned login never holds up an auth edit.
#[cfg(any(unix, windows))]
pub(crate) struct AuthEdit(std::fs::File);

/// The auth edit lock's file, in `Dirs::config`.
#[cfg(any(unix, windows))]
const AUTH_EDIT_LOCK: &str = ".auth-edit.lock";

#[cfg(any(unix, windows))]
impl AuthEdit {
    /// The auth edit lock taken, its private directory made first.
    /// # Errors
    /// Refused while another edit holds it or when the directory or lock is not private;
    /// could_not_run on I/O.
    pub(crate) fn take(dirs: &Dirs) -> Result<Self> {
        Self::take_named(
            dirs,
            AUTH_EDIT_LOCK,
            "the auth edit lock",
            Duration::ZERO,
            || {
                Error::refused("another agh auth edit is in progress")
                    .with_next("run the auth command again after that edit finishes")
            },
        )
    }

    /// The lock in `dirs.config/{file}` taken, waiting up to `wait` while another holder has
    /// it (`Duration::ZERO`: one attempt), `what` naming it in each refusal and `busy` the
    /// refusal when another holder kept it past `wait`.
    /// # Errors
    /// `busy()` when another holder kept it past `wait`; Refused when the directory or lock is
    /// not private or the lock file is not empty; could_not_run on I/O.
    pub(crate) fn take_named(
        dirs: &Dirs,
        file: &str,
        what: &str,
        wait: Duration,
        busy: impl FnOnce() -> Error,
    ) -> Result<Self> {
        let as_load = |refused: Error| load_refusal_of_dir(dirs).unwrap_or(refused);
        config_dir(dirs).map_err(as_load)?;
        let path = dirs.config.join(file);
        #[cfg(unix)]
        let (file, meta) = {
            use std::os::unix::fs::OpenOptionsExt as _;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                .open(&path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::could_not_run(format!("{what}: {e}"))),
            }
            aterm_uds::kept_out(|| aterm_uds::private_file::open_private(&dirs.config, file))
                .map_err(|e| as_load(Error::refused(format!("{what}: {e}"))))?
        };
        // On Windows the lock file is made with the user's protected access list where it is
        // missing, opened as itself (a reparse point is refused) and judged on its handle, in one
        // step; a handle reaches no child unless it is made inheritable.
        #[cfg(windows)]
        let (file, meta) = {
            use aterm_uds::private_file::{PrivateFileError, open_private_rw};
            let opened = open_private_rw(&dirs.config, file).map_err(|e| match e {
                PrivateFileError::Io(e) => Error::could_not_run(format!("{what}: {e}")),
                refused => as_load(Error::refused(format!("{what}: {refused}"))),
            })?;
            let meta = opened
                .metadata()
                .map_err(|e| Error::could_not_run(format!("{what}: {e}")))?;
            (opened, meta)
        };
        if meta.len() != 0 {
            let shown = tilde(&path, &dirs.home);
            return Err(
                Error::refused(format!("{what} {shown} is occupied by a nonempty file")).with_next(
                    format!("agh writes nothing into {shown}: remove it, and agh makes it again"),
                ),
            );
        }
        // The guard releases by LOCK_UN before closing (`Drop`).
        let deadline = crate::fx::lock_deadline(wait);
        match aterm_uds::file_lock::lock_exclusive_until(&file, deadline, what) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(busy()),
            Err(e) => return Err(Error::could_not_run(format!("{what}: {e}"))),
        }
        let held = Self(file);
        #[cfg(unix)]
        let same = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::symlink_metadata(&path).is_ok_and(|named| {
                named.file_type().is_file()
                    && (named.dev(), named.ino()) == (meta.dev(), meta.ino())
            })
        };
        // The name still the locked file: its volume and file id, read on a look at the name
        // itself (a reparse point there is not followed, so it reads as another file) and on
        // the locked handle.
        #[cfg(windows)]
        let same = {
            use aterm_uds::line_log::{identity_at, identity_of};
            match (identity_at(&path), identity_of(&held.0)) {
                (Ok(named), Ok(locked)) => (named.dev, named.ino) == (locked.dev, locked.ino),
                _ => false,
            }
        };
        if !same {
            return Err(Error::could_not_run(format!(
                "{what} changed while it was acquired"
            )));
        }
        Ok(held)
    }
}

#[cfg(any(unix, windows))]
impl Drop for AuthEdit {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The auth edit lock, held until the guard drops, as an `auth add` or `auth remove` holds it.
/// # Errors
/// `AuthEdit::take`'s.
#[cfg(all(any(unix, windows), any(test, debug_assertions)))]
pub fn hold_auth_edit_for_tests(dirs: &Dirs) -> Result<impl Sized> {
    if let Some(parent) = dirs.config.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::could_not_run(e.to_string()))?;
    }
    AuthEdit::take(dirs)
}

/// How a refusal names `source`: a token file `~/`-relative, a vault entry by its name.
#[cfg(any(unix, windows))]
fn source_shown(source: &Source, home: &Path) -> String {
    match source {
        Source::File(path) => tilde(path, home),
        Source::Entry(entry) => format!("the vault entry {}", entry.name()),
        Source::Inline(_) => "the inline token".to_string(),
    }
}

/// A staging name this transaction created exclusively.
/// `add` on unix and windows (plan §11.2).
#[cfg(any(unix, windows))]
fn add_edit(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    options: AddOptions,
) -> Result<(Added, Meter)> {
    // The profile the token is loaded as, here and in the login binding: a `~/.secrets/NAME`
    // `--file` is the vault entry NAME, read in place from the store; with `--legacy-loose`,
    // `identity::load` reads a file of the person's own through `read_owned_into`.
    let path = &req.file;
    if let Some(fault) = config::profile_name_fault(&req.name) {
        return Err(Error::usage(fault).with_next(ADD_HELP));
    }
    let Some(text) = path.to_str() else {
        return Err(Error::usage(format!(
            "the token file {} is not a UTF-8 path, and agh.toml can name only a UTF-8 path",
            tilde(path, &dirs.home)
        ))
        .with_next(ADD_HELP));
    };
    let source = config::token_source(text, &dirs.home).ok_or_else(|| {
        Error::usage(if cfg!(windows) {
            format!(
                "{} is inside ~/.secrets, which holds only vault entries, or is spelled so that \
                 Windows may read it there (a . or .. part, a trailing dot or space, an 8.3 \
                 short name): name a vault entry as ~/.secrets/NAME, and a token file by its \
                 plain path",
                tilde(path, &dirs.home)
            )
        } else {
            format!(
                "{} is inside ~/.secrets, which holds only vault entries: name one as \
                 ~/.secrets/NAME",
                tilde(path, &dirs.home)
            )
        })
        .with_next(ADD_HELP)
    })?;
    let mut profile = Profile::new(req.name.clone(), source.clone(), req.tier);
    profile.set_legacy_loose(options.legacy_loose);
    identity::load(&profile).map_err(|e| load_refusal(&e, &profile, &dirs.home))?;
    let _edit = AuthEdit::take(dirs)?;
    let existing = read_current(dirs)?;
    let existed = existing.is_some();
    let text = existing
        .unwrap_or_else(|| super::vault::seed(dirs, &req.name, &source).unwrap_or_default());
    if !text.is_empty() {
        let current = config::judge(&text, &dirs.home).verdict.map_err(|_| {
            Error::refused("agh.toml is invalid; agh auth add edits only a valid file")
                .with_next(config::POLICY_CHECK)
        })?;
        if current.joins_vault()
            && let Some(slot) = super::vault::Slot::named(&req.name)
        {
            return Err(Error::usage(format!(
                "profile {} is the vault entry {}, which agh adopts with nothing to run: agh.toml \
                 says vault = true, so it names no profile dev or release",
                req.name,
                slot.entry()
            ))
            .with_next("choose another NAME: the vault's token already joins agh.toml's rows"));
        }
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
    }
    edit_config_with(&text, req, "", "", "", options.legacy_loose)?;
    let cred = identity::load(&profile).map_err(|e| load_refusal(&e, &profile, &dirs.home))?;
    let (added, new_text, meter) = bind_and_edit(gh, dirs, req, &text, &profile, &cred)?;
    judge_edit(&new_text, &dirs.home, existed)?;
    write_config(dirs, &new_text)?;
    Ok((added, meter))
}

/// Steps 5 and 6 of `add` (plan §11.2): the login bound as `loaded`, the profile `cred` was
/// loaded as (its source and its `legacy_loose`), then the edit made (`judge_edit` judges
/// it), its row saying `legacy_loose = true` exactly when `--legacy-loose` asked for it
/// (`loaded.legacy_loose()`), whether or not the file was loose.
#[cfg(any(unix, windows))]
fn bind_and_edit(
    gh: &Github,
    dirs: &Dirs,
    req: &AddRequest,
    text: &str,
    loaded: &Profile,
    cred: &Credential,
) -> Result<(Added, String, Meter)> {
    let token_file = req.file.as_path();
    let validated = Validated {
        profiles: BTreeMap::from([(req.name.clone(), loaded.clone())]),
        owners: BTreeMap::new(),
        default_read: Vec::new(),
        targets: BTreeMap::new(),
        vault: false,
        origin: config::ConfigOrigin::Text,
        writers: Vec::new(),
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
            "{} changed during agh auth add",
            source_shown(&loaded.source, &dirs.home)
        ))
        .with_next("run agh auth add again"));
    }
    let spelled = match &loaded.source {
        Source::Entry(entry) => entry.spelling(),
        _ => tilde(token_file, &dirs.home),
    };
    let new_text = edit_config_with(
        text,
        req,
        &spelled,
        &login,
        cred.fingerprint(),
        loaded.legacy_loose(),
    )?;
    let added = Added {
        profile: req.name.clone(),
        login,
        source: loaded.source.clone(),
        fingerprint: cred.fingerprint().to_string(),
        kind: cred.kind(),
        owners: req.owners.clone(),
    };
    Ok((added, new_text, meter))
}

/// agh.toml written whole, 0600, in its private directory (`add`'s step 8, plan §11.2).
#[cfg(any(unix, windows))]
fn write_config(dirs: &Dirs, text: &str) -> Result<()> {
    config_dir(dirs)?;
    aterm_uds::private_file::write_private(&dirs.config, CONFIG_FILE, text.as_bytes()).map_err(
        |e| {
            Error::could_not_run(format!(
                "{}: {}",
                tilde(&dirs.config_file(), &dirs.home),
                io_text(&e, &dirs.config, &dirs.home)
            ))
        },
    )
}

/// `remove` on unix and windows (plan §11.2).
#[cfg(any(unix, windows))]
fn remove_edit(dirs: &Dirs, name: &str) -> Result<Removed> {
    let _edit = AuthEdit::take(dirs)?;
    let Some(text) = read_current(dirs)? else {
        if let Some(slot) = super::vault::Slot::named(name)
            && super::vault::forget_locked(dirs, slot)?
        {
            return Ok(Removed {
                profile: name.to_string(),
                owners_edited: Vec::new(),
                joins_vault: true,
            });
        }
        return Err(
            Error::usage("no agh.toml: there is no profile to remove").with_next("agh auth status")
        );
    };
    let current = config::judge(&text, &dirs.home).verdict.map_err(|_| {
        Error::refused("agh.toml is invalid; agh auth remove edits only a valid file")
            .with_next(config::POLICY_CHECK)
    })?;
    let Some(profile) = current.profiles().get(name) else {
        if current.joins_vault()
            && let Some(slot) = super::vault::Slot::named(name)
            && super::vault::forget_locked(dirs, slot)?
        {
            return Ok(Removed {
                profile: name.to_string(),
                owners_edited: Vec::new(),
                joins_vault: true,
            });
        }
        return Err(crate::config::no_profile(name, "agh auth status"));
    };
    let (new_text, owners_edited) = remove_from_config(&text, name)?;
    let after = config::judge(&new_text, &dirs.home)
        .verdict
        .map_err(|faults| would_break(&faults, "remove", true))?;
    let vaults = |p: &Profile| super::vault::is_slot_entry(&p.source);
    let (new_text, joins_vault) = if vaults(profile)
        && !after.joins_vault()
        && !after
            .profiles()
            .values()
            .any(|p| vaults(p) || super::vault::Slot::named(&p.name).is_some())
        && super::vault::any_entry(dirs)
    {
        let joined = join_vault(&text, &new_text)?;
        match config::judge(&joined, &dirs.home).verdict {
            Ok(_) => (joined, true),
            Err(_) => (new_text, false),
        }
    } else {
        (new_text, after.joins_vault())
    };
    write_config(dirs, &new_text)?;
    Ok(Removed {
        profile: name.to_string(),
        owners_edited,
        joins_vault,
    })
}

/// The windows arm's folder rules: what an elevated process makes.
#[cfg(all(test, windows))]
#[path = "provision_tests.rs"]
mod tests;
