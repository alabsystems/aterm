// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publishing a chosen machine number (`docs/DECISION-name-publication-2026-11-06.md`): once
//! [`super::set_name`] has committed a number, [`publish`] makes every other reader of this
//! machine's name read it too, in order: aterm.toml's three naming keys ([`Policy`]), the checked
//! fleet name, and on macOS the Mac's three system names ([`SystemNames`]), each only while the
//! name file still holds that number. [`publication`] says, writing nothing, which of those a
//! chosen number still owes. `aterm name set` is its caller.

use std::io;
use std::path::Path;
use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use std::time::Duration;

use aterm_toml::edit::{DocumentMut, Item, Table, Value};
use aterm_types::machine_name::{Class, GeneratedNames, Number, OsSlot};
use aterm_uds::atomic_file::ConfigFile;

use super::file::{self, MachineName, Migration};

/// macOS's three system names, in the order they are read and set.
pub const SYSTEM_NAMES: [&str; 3] = ["ComputerName", "LocalHostName", "HostName"];

/// Bound for an optional `scutil --get` observation.
#[cfg(target_os = "macos")]
const SCUTIL_READ_WAIT: Duration = Duration::from_secs(10);

/// What a request a newer set replaced says (`aterm name set`'s words).
const SUPERSEDED: &str = "a newer machine name replaced this request";

/// What aterm.toml's naming hold says.
const HELD_OFF: &str = "[identity] name = false holds machine naming off";

/// aterm.toml's keys that name this machine: its table, its key, and how a person reads them.
const POLICY_KEYS: [(&str, &str, &str); 3] = [
    ("identity", "name", "[identity] name"),
    ("ai", "machine", "[ai] machine"),
    ("fleet", "machine", "[fleet] machine"),
];

/// aterm.toml's naming policy for one number: the file as it was read and the text that names
/// the number in its three keys, other settings and comments kept. It is read before the set, so
/// a naming hold or an aterm.toml that cannot be edited refuses before any name changes, and
/// [`publish`] writes it only over the bytes read here.
pub struct Policy {
    number: Number,
    snapshot: ConfigFile,
    text: String,
}

impl Policy {
    /// The policy that names `number` in the aterm.toml at `config`, where no file reads as an
    /// empty one.
    ///
    /// # Errors
    /// aterm.toml cannot be read ([`ConfigFile::open`]) or is not UTF-8; it is not valid TOML
    /// (`invalid aterm.toml: …`); `[identity] name = false holds machine naming off`; or its
    /// `identity`, `ai` or `fleet` is not a table (`aterm.toml {section} is not a table`).
    pub fn read(config: &Path, number: Number) -> Result<Self, String> {
        let snapshot = ConfigFile::open(config).map_err(|e| e.to_string())?;
        let text = policy_text(
            snapshot.text().map_err(|e| e.to_string())?.unwrap_or(""),
            &number.to_string(),
        )?;
        Ok(Self {
            number,
            snapshot,
            text,
        })
    }
}

/// aterm.toml's text, `text`, parsed as its editor keeps it.
fn parsed(text: &str) -> Result<DocumentMut, String> {
    text.parse().map_err(|e| format!("invalid aterm.toml: {e}"))
}

/// Whether `doc` holds machine naming off (`[identity] name = false`).
fn held_off(doc: &DocumentMut) -> bool {
    doc.get("identity")
        .and_then(Item::as_table_like)
        .and_then(|table| table.get("name"))
        .and_then(Item::as_bool)
        == Some(false)
}

/// `text` with each of [`POLICY_KEYS`] set to `number`.
///
/// # Errors
/// As [`Policy::read`], after the read.
fn policy_text(text: &str, number: &str) -> Result<String, String> {
    let mut doc = parsed(text)?;
    if held_off(&doc) {
        return Err(HELD_OFF.into());
    }
    for (section, key, _) in POLICY_KEYS {
        let root = doc.as_table_mut();
        if root.get(section).is_none_or(Item::is_none) {
            root.insert(section, Item::Table(Table::new()));
        }
        let table = root
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("aterm.toml {section} is not a table"))?;
        table.insert(key, Item::Value(Value::from(number)));
    }
    Ok(doc.to_string())
}

/// Write `policy`'s text under the name file's lock while that file holds its number, through
/// the config service's locked compare-and-replace over the bytes [`Policy::read`] read.
///
/// # Errors
/// The name lock is busy, or the name file is missing or unread ([`file::replace`]); it holds
/// another number (`a newer machine name replaced this request`); or the config service's lock
/// cannot be taken or its replace fails ([`aterm_uds::atomic_file::ConfigEdit::replace`]), as
/// when aterm.toml changed since it was read.
fn write_policy(state: &Path, policy: &Policy) -> io::Result<()> {
    file::replace(state, |held| {
        if held.num != Some(policy.number) {
            return Err(io::Error::other(SUPERSEDED));
        }
        policy.snapshot.lock()?.replace(&policy.text)?;
        Ok(())
    })
    .map(|_| ())
}

/// Whether the name file in `state` holds `number`.
///
/// # Errors
/// The name file cannot be read ([`file::read`]).
fn holds(state: &Path, number: Number) -> io::Result<bool> {
    Ok(file::read(state)?.is_some_and(|name| name.num == Some(number)))
}

/// `Ok` while the name file holds `number` and the current naming policy still names it.
///
/// # Errors
/// [`Pending::Superseded`] once it holds another number or none, [`Pending::NameUnread`] when it
/// cannot be read, or [`Pending::Policy`] when configuration is unreadable, holds naming off
/// or no longer names this number. Unrelated configuration edits remain allowed.
pub fn check_current(state: &Path, config: &Path, number: Number) -> Result<(), Pending> {
    let policy = (|| {
        let snapshot = ConfigFile::open(config).map_err(|e| e.to_string())?;
        let doc = parsed(snapshot.text().map_err(|e| e.to_string())?.unwrap_or(""))?;
        if held_off(&doc) {
            return Err(HELD_OFF.to_owned());
        }
        let missing = missing_policy_keys(&doc, &number.to_string());
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the naming policy changed during synchronization ({})",
                missing.join(", ")
            ))
        }
    })();
    // The config read can block too: keep the canonical-number fence last, retaining its
    // precedence over a policy error when a newer number replaced this request.
    match holds(state, number) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Pending::Superseded(SUPERSEDED.to_owned())),
        Err(e) => Err(Pending::NameUnread(e.to_string())),
    }?;
    policy.map_err(Pending::Policy)
}

/// The generated names of the saved canonical choice, without reading any OS name.
/// # Errors
/// The saved name is unread or no longer holds `number`.
pub fn system_names(state: &Path, number: Number) -> Result<GeneratedNames, Pending> {
    let name = file::read(state).map_err(|why| Pending::NameUnread(why.to_string()))?;
    match name {
        Some(name) if name.num == Some(number) => Ok(GeneratedNames::new(number, &name.nick)),
        _ => Err(Pending::Superseded(SUPERSEDED.into())),
    }
}

/// Fence both parts of an OS-name request after any potentially blocking read.
/// # Errors
/// As [`check_current`], or a newer nickname replaced the captured choice.
pub fn check_system_current(
    state: &Path,
    config: &Path,
    names: &GeneratedNames,
) -> Result<(), Pending> {
    check_current(state, config, names.number())?;
    if system_names(state, names.number())? != *names {
        return Err(Pending::Superseded(
            "a newer machine number or nickname replaced this request".into(),
        ));
    }
    Ok(())
}

/// The current policy keys that do not name `number`.
fn missing_policy_keys(doc: &DocumentMut, number: &str) -> Vec<&'static str> {
    POLICY_KEYS
        .iter()
        .filter(|(section, key, _)| {
            doc.get(section)
                .and_then(Item::as_table_like)
                .and_then(|table| table.get(key))
                .and_then(Item::as_str)
                != Some(number)
        })
        .map(|(_, _, label)| *label)
        .collect()
}

/// A step that did not finish, and `why`, judged again by the name file: superseded, in the
/// step's own words, once the file holds another number or none; otherwise `step`'s, an unread
/// file included.
fn judged(state: &Path, number: Number, why: String, step: fn(String) -> Pending) -> Pending {
    if matches!(holds(state, number), Ok(false)) {
        Pending::Superseded(why)
    } else {
        step(why)
    }
}

/// Whether every observed OS name exactly matches its generated target.
#[must_use]
pub fn all_read(names: &[Option<String>; 3], expected: &GeneratedNames) -> bool {
    SYSTEM_NAMES
        .into_iter()
        .zip(names)
        .all(|(field, name)| exact_system_name(field, expected, name.as_deref()))
}

fn exact_system_name(field: &str, names: &GeneratedNames, observed: Option<&str>) -> bool {
    observed.is_some_and(|observed| {
        OsSlot::from_macos_preference(field).is_some_and(|slot| names.exact(slot, observed))
    })
}

fn all_accepted(names: &[Option<String>; 3], expected: &GeneratedNames) -> bool {
    SYSTEM_NAMES.into_iter().zip(names).all(|(field, value)| {
        OsSlot::from_macos_preference(field).is_some_and(|slot| {
            value
                .as_deref()
                .is_some_and(|value| expected.accepts(slot, value))
        })
    })
}

/// The publisher's macOS names port. Production applies use the same registered,
/// noninteractive naming rule for the CLI and the menu; test arms are inert.
pub trait SystemNames {
    /// Each of [`SYSTEM_NAMES`], in that order: `None` where it is unset or unread.
    fn read(&self) -> [Option<String>; 3];

    /// Apply through existing noninteractive authorization, retaining the supplied source fences.
    ///
    /// # Errors
    /// Why they were not set, worded for the person.
    fn apply(&self, state: &Path, config: &Path, names: &GeneratedNames) -> Result<(), String>;

    /// Read whether the exact current naming permission is present. This is presentation
    /// information only; apply still checks its current intent and actual authorization.
    /// # Errors
    /// The current fixed permission cannot be read safely in this process.
    fn authorization(&self, _state: &Path, _config: &Path) -> Result<bool, String> {
        Err("macOS naming authorization could not be checked".into())
    }
}

type SystemNameApply = fn(&Path, &Path, &GeneratedNames) -> Result<(), String>;
struct SystemNameOperations {
    apply: SystemNameApply,
    authorization: fn(&Path, &Path) -> Result<bool, String>,
}
static SYSTEM_NAME_APPLY: OnceLock<SystemNameOperations> = OnceLock::new();

/// Register the application's narrowly authorized, noninteractive naming operation.
/// Main installs it before any CLI or window dispatch. Missing authority is pending;
/// publication never opens a new password episode.
pub fn on_system_name_apply(
    apply: fn(&Path, &Path, &GeneratedNames) -> Result<(), String>,
    authorization: fn(&Path, &Path) -> Result<bool, String>,
) {
    let _ = SYSTEM_NAME_APPLY.set(SystemNameOperations {
        apply,
        authorization,
    });
}

/// Apply through the application's existing naming authorization.
/// # Errors
/// The operation is unavailable, its current intent moved, or its rule could not apply.
pub fn apply_registered(state: &Path, config: &Path, names: &GeneratedNames) -> Result<(), String> {
    let operations = SYSTEM_NAME_APPLY
        .get()
        .ok_or("macOS naming is unavailable in this process")?;
    (operations.apply)(state, config, names)
}

/// Read the existing rule for an explicit name-menu presentation, without a setter or prompt.
/// # Errors
/// This process has no registered reader, or the current rule cannot be read safely.
pub fn registered_authorization(state: &Path, config: &Path) -> Result<bool, String> {
    let operations = SYSTEM_NAME_APPLY
        .get()
        .ok_or("macOS naming is unavailable in this process")?;
    (operations.authorization)(state, config)
}

/// macOS's three system names as Apple's `scutil --get` reads each within 10 seconds: `None`
/// for one it reports unset, fails to read, or does not answer in time.
#[cfg(target_os = "macos")]
#[must_use]
pub fn read_system_names() -> [Option<String>; 3] {
    SYSTEM_NAMES.map(read_system_name)
}

#[cfg(target_os = "macos")]
fn read_system_name(field: &str) -> Option<String> {
    let mut command = aterm_uds::spawnfd::command("/usr/sbin/scutil");
    command.args(["--get", field]);
    aterm_uds::deadline::output_within(&mut command, SCUTIL_READ_WAIT)
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The step of [`publish`] that did not finish, with why in that step's own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    /// aterm.toml's naming policy was not written.
    Policy(String),
    /// The fleet name was not written.
    FleetName(String),
    /// macOS's names are not exactly aligned with their generated targets.
    SystemNames(String),
    /// The name file could not be read to say whether it still holds the number.
    NameUnread(String),
    /// The name file holds another number (or none): a newer set replaced this one, which owes
    /// nothing more. The words are those of the step that found it.
    Superseded(String),
}

impl Pending {
    /// Why, in the words of the step that found it: what `aterm name set` prints after
    /// `publication pending: `.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Policy(why)
            | Self::FleetName(why)
            | Self::SystemNames(why)
            | Self::NameUnread(why)
            | Self::Superseded(why) => why,
        }
    }

    /// What is still owed, worded to follow `m86 is this Mac's number now, but `.
    #[must_use]
    pub fn phrase(&self) -> String {
        match self {
            Self::Policy(why) => format!("aterm.toml does not name it yet ({why})"),
            Self::FleetName(why) => format!("its fleet name is not published yet ({why})"),
            Self::SystemNames(why) => {
                format!("macOS names are not fully aligned ({why})")
            }
            Self::NameUnread(why) => format!("its name could not be read back ({why})"),
            Self::Superseded(_) => "a newer machine name replaced it".to_owned(),
        }
    }
}

/// Publish `policy`'s number, which [`super::set_name`] has committed in `state`, in order:
/// 1. aterm.toml's policy ([`Policy`]), under the name file's lock while it holds the number;
/// 2. the fleet name in `state`, through `aterm fleet name set`'s checked writer
///    ([`crate::fleet_member::choose_machine_name_at`]), which reads the `[[fleet.peers]]` of
///    the aterm.toml at `config` among the machines that may already answer to it and checks
///    the committed policy and number again under the fleet writer's lock; then the holder's
///    signed membership Name, under the same writer's canonical-number fence;
/// 3. with `system`, macOS's generated names: read, and where an actionable target differs, set
///    through the shared noninteractive naming operation and read again.
///
/// The name file is read again after the fleet name and each OS operation; a file that holds
/// another number or nickname (or none) then ends publication
/// as [`Pending::Superseded`]. The current naming policy is checked alongside it after each
/// step, before an OS apply and before success; a changed naming key or hold retires this work
/// without undoing an earlier step or overwriting the newer configuration.
///
/// # Errors
/// The first step that did not finish; later steps do not run. A step that failed while the
/// name file no longer holds the number is [`Pending::Superseded`], in that step's words; a name
/// file that cannot be read at one of those checks is [`Pending::NameUnread`].
pub fn publish(
    state: &Path,
    config: &Path,
    policy: &Policy,
    system: Option<&dyn SystemNames>,
) -> Result<(), Pending> {
    publish_through(state, config, policy, system, || {
        crate::fleet_member::choose_machine_name_at(state, config, policy.number)?;
        if let Some(paths) = crate::fleet_vault::vault_paths_for(state) {
            publish_log_label(state, config, policy.number, || {
                aterm_secrets::client::publish_local_chosen_name(&paths, policy.number)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })?;
        }
        Ok(())
    })
}

/// Serialize the signature with set_name's book writer. A newer local choice cannot commit
/// between the canonical-number fence and authoring the membership Name record.
fn publish_log_label(
    state: &Path,
    config: &Path,
    number: Number,
    sign: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    aterm_net::fleet_member::write_held(state, aterm_net::fleet_member::Began::Here, |_| {
        check_current(state, config, number).map_err(|pending| pending.why().to_owned())?;
        sign()
    })
    .map_err(|error| error.to_string())?
}

/// [`publish`] with its fleet step given as `fleet`: the fleet name's checked write of
/// `policy`'s number, answering why it did not finish.
///
/// # Errors
/// As [`publish`], with `fleet`'s words for the fleet step.
fn publish_through(
    state: &Path,
    config: &Path,
    policy: &Policy,
    system: Option<&dyn SystemNames>,
    fleet: impl FnOnce() -> Result<(), String>,
) -> Result<(), Pending> {
    let number = policy.number;
    let expected = system_names(state, number)?;
    write_policy(state, policy)
        .map_err(|e| judged(state, number, e.to_string(), Pending::Policy))?;
    check_system_current(state, config, &expected)?;
    fleet().map_err(|why| judged(state, number, why, Pending::FleetName))?;
    check_system_current(state, config, &expected)?;
    let Some(system) = system else {
        return Ok(());
    };
    synchronize_system_names(state, config, &expected, system, &|| {
        check_system_current(state, config, &expected)
    })
}

/// Read, align and read back only the three generated macOS names. The caller supplies
/// its existing source and current-policy fence; this writes no policy or fleet record.
/// Every apply still uses the supplied noninteractive system port.
/// # Errors
/// The source moved, the existing permission refused, or exact alignment remains pending.
pub fn synchronize_system_names(
    state: &Path,
    config: &Path,
    expected: &GeneratedNames,
    system: &dyn SystemNames,
    current: &dyn Fn() -> Result<(), Pending>,
) -> Result<(), Pending> {
    current()?;
    let observed = system.read();
    current()?;
    if all_read(&observed, expected) {
        return Ok(());
    }
    if all_accepted(&observed, expected) {
        return Err(Pending::SystemNames(
            "macOS retains a numeric collision alias; its actual name is shown in the name menu"
                .into(),
        ));
    }
    if let Err(why) = system.apply(state, config, expected) {
        current()?;
        return Err(Pending::SystemNames(why));
    }
    current()?;
    let observed = system.read();
    current()?;
    if !all_read(&observed, expected) {
        if all_accepted(&observed, expected) {
            return Err(Pending::SystemNames(
                "macOS retains a numeric collision alias; its actual name is shown in the name menu".into(),
            ));
        }
        return Err(judged(
            state,
            expected.number(),
            "macOS did not retain all three system names".to_owned(),
            Pending::SystemNames,
        ));
    }
    Ok(())
}

/// What a machine name's number still owes its readers ([`publish`]'s steps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Publication {
    /// aterm.toml holds machine naming off: why.
    Off(String),
    /// Nothing is owed: the number is published, or it is no chosen number.
    Current,
    /// What does not read the chosen number yet, in [`publish`]'s order.
    Owed(Vec<Owed>),
}

/// One step of [`publish`] that does not read a chosen number yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owed {
    /// aterm.toml's naming keys that do not name it, as a person reads them (`[ai] machine`).
    Policy(Vec<&'static str>),
    /// The fleet name as it reads, `None` while unset.
    FleetName(Option<String>),
    /// Each of macOS's names that does not read it, with what it reads (`None`: unset or unread).
    SystemNames(Vec<(&'static str, Option<String>)>),
}

impl Owed {
    /// What this step reads instead of `number`: `aterm.toml's [ai] machine not yet m86`,
    /// `the fleet name is unset`, `macOS's ComputerName is m184 and its HostName is unset`.
    #[must_use]
    pub fn phrase(&self, number: Number) -> String {
        match self {
            Self::Policy(keys) => format!("aterm.toml's {} not yet {number}", keys.join(", ")),
            Self::FleetName(Some(name)) => format!("the fleet name is {name}"),
            Self::FleetName(None) => "the fleet name is unset".to_owned(),
            Self::SystemNames(names) => {
                let said: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(at, (field, value))| {
                        let value = value.as_deref().unwrap_or("unset");
                        if at == 0 {
                            format!("macOS's {field} is {value}")
                        } else {
                            format!("its {field} is {value}")
                        }
                    })
                    .collect();
                match said.split_last() {
                    Some((last, [])) => last.clone(),
                    Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                    None => String::new(),
                }
            }
        }
    }
}

/// [`publication_of`] over the aterm.toml at `config`, the fleet name in `state` and macOS's
/// `names` where they were read. It takes no lock, makes no node key and spawns nothing.
///
/// # Errors
/// aterm.toml or the fleet name cannot be read, or aterm.toml is not valid TOML.
pub fn publication(
    state: &Path,
    config: &Path,
    name: &MachineName,
    names: Option<&[Option<String>; 3]>,
) -> Result<Publication, String> {
    let snapshot = ConfigFile::open(config).map_err(|e| e.to_string())?;
    let text = snapshot.text().map_err(|e| e.to_string())?;
    let fleet = aterm_net::fleet_member::read_primary_name(state)?;
    publication_of(text, fleet.as_deref(), name, names)
}

/// What `name`'s number still owes, from aterm.toml's text (`None`: no file), the fleet name
/// (`None`: unset) and macOS's `names` (`None`: not read, so not judged): [`Publication::Off`]
/// under the naming hold whatever the name; [`Publication::Current`] for a name that is not a
/// chosen number (an automatic or seeded one, an unnumbered one, or one whose migration waits,
/// which publishes no number), since only a chosen number is projected to the other readers;
/// otherwise each step that does not read it.
///
/// # Errors
/// aterm.toml is not valid TOML (`invalid aterm.toml: …`).
pub fn publication_of(
    config_text: Option<&str>,
    fleet_name: Option<&str>,
    name: &MachineName,
    names: Option<&[Option<String>; 3]>,
) -> Result<Publication, String> {
    let doc = parsed(config_text.unwrap_or(""))?;
    if held_off(&doc) {
        return Ok(Publication::Off(HELD_OFF.to_owned()));
    }
    let Some(number) = name
        .num
        .filter(|_| name.class == Class::Chosen && name.migration != Some(Migration::Waiting))
    else {
        return Ok(Publication::Current);
    };
    let word = number.to_string();
    let mut owed = Vec::new();
    let keys = missing_policy_keys(&doc, &word);
    if !keys.is_empty() {
        owed.push(Owed::Policy(keys));
    }
    if fleet_name != Some(word.as_str()) {
        owed.push(Owed::FleetName(fleet_name.map(str::to_owned)));
    }
    if let Some(names) = names {
        let expected = GeneratedNames::new(number, &name.nick);
        let differ: Vec<(&'static str, Option<String>)> = SYSTEM_NAMES
            .into_iter()
            .zip(names)
            .filter(|(field, value)| !exact_system_name(field, &expected, value.as_deref()))
            .map(|(field, value)| (field, value.clone()))
            .collect();
        if !differ.is_empty() {
            owed.push(Owed::SystemNames(differ));
        }
    }
    Ok(if owed.is_empty() {
        Publication::Current
    } else {
        Publication::Owed(owed)
    })
}

#[cfg(test)]
mod tests;
