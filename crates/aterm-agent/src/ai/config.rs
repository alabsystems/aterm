// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `[ai]` configuration table and fleet document parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_toml::Value;

use super::connector::{self, Connector};
use super::permission::Choice;

/// The table's name in `aterm.toml`.
pub const TABLE: &str = "ai";
/// The fleet document's one schema.
pub const FLEET_SCHEMA: i64 = 1;
/// The largest fleet document read.
pub const FLEET_MAX_BYTES: u64 = 64 * 1024;
/// The most reviewers one review asks without naming them.
pub const REVIEW_COUNT_MAX: u8 = 4;
/// A reviewer's time limit, at least.
pub const TIMEOUT_MIN_S: u32 = 30;
/// A reviewer's time limit, at most.
pub const TIMEOUT_MAX_S: u32 = 3600;
/// The default time limit: a reviewer with its tools reads the repository.
pub const TIMEOUT_DEFAULT_S: u32 = 600;
/// The default number of reviewers: every one used here.
pub const REVIEW_COUNT_DEFAULT: u8 = REVIEW_COUNT_MAX;
/// The longest machine name.
pub const MACHINE_MAX: usize = 32;
/// The largest `aterm.toml` read (the window's admission cap).
pub const CONFIG_MAX_BYTES: u64 = 512 * 1024;
/// The fleet document read beside `aterm.toml` when `[ai] fleet` names none.
pub const FLEET_FILE: &str = "ai-fleet.toml";

/// The choices a fleet document may make too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layer {
    /// `reviews`.
    pub reviews: Option<bool>,
    /// `preferred`.
    pub preferred: Option<&'static str>,
    /// `reviewers`, in order.
    pub reviewers: Option<Vec<&'static str>>,
    /// `review_count`.
    pub review_count: Option<u8>,
    /// `review_timeout_s`.
    pub review_timeout_s: Option<u32>,
    /// `review_permissions`.
    pub review_permissions: Option<Choice>,
    /// `[<id>] use`, per connection id.
    pub uses: BTreeMap<&'static str, bool>,
}

/// Where the fleet document comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetSource {
    /// A file on this machine.
    Path(PathBuf),
    /// An `https://` URL, fetched and verified.
    Url(String),
}

impl FleetSource {
    /// How it is shown.
    #[must_use]
    pub fn shown(&self) -> String {
        match self {
            FleetSource::Path(p) => p.display().to_string(),
            FleetSource::Url(u) => u.clone(),
        }
    }
}

/// This machine's `[ai]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Local {
    /// `enabled`.
    pub enabled: Option<bool>,
    /// `machine`.
    pub machine: Option<String>,
    /// `fleet`.
    pub fleet: Option<FleetSource>,
    /// `fleet_key`: the fleet document's signer, pinned.
    pub fleet_key: Option<String>,
    /// The choices.
    pub layer: Layer,
}

/// A fleet document, admitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fleet {
    /// Its top level.
    pub root: Layer,
    /// Its `[machine.<name>]` rows.
    pub machines: BTreeMap<String, Layer>,
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// This machine's `[ai]` table.
    Local,
    /// The fleet document's row for this machine.
    FleetMachine,
    /// The fleet document's top level.
    Fleet,
    /// The built-in default.
    Default,
}

impl From {
    /// The word the doctor prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            From::Local => "aterm.toml",
            From::FleetMachine => "fleet, this machine's row",
            From::Fleet => "fleet",
            From::Default => "default",
        }
    }
}

/// What became of the fleet document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetState {
    /// No fleet document is named and none lies beside `aterm.toml`.
    None,
    /// Read and admitted; `row`: it has a row for this machine.
    Read {
        /// Where it came from.
        source: String,
        /// Whether it has a row for this machine.
        row: bool,
    },
    /// Not read, or refused whole; nothing in it applies.
    Refused {
        /// Where it came from.
        source: String,
        /// Why, in one line.
        why: String,
    },
}

impl FleetState {
    /// `read`, `refused` or `none`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            FleetState::None => "none",
            FleetState::Read { .. } => "read",
            FleetState::Refused { .. } => "refused",
        }
    }
}

/// Every choice, resolved.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The window's look and its messages.
    pub enabled: bool,
    /// Whether reviews run at all.
    pub reviews: (bool, From),
    /// This machine's main AI CLI.
    pub preferred: (&'static Connector, From),
    /// Who reviews, in order: every CLI used here unless a list narrows it.
    pub reviewers: (Vec<&'static Connector>, From),
    /// Reviewers per review when none is named.
    pub review_count: (u8, From),
    /// Each reviewer's time limit.
    pub review_timeout_s: (u32, From),
    /// The reviewers' permission level: the asker's, or a pinned one.
    pub review_permissions: (Choice, From),
    /// Whether each CLI is used on this machine, in `VENDORS` order.
    pub uses: Vec<(&'static Connector, bool, From)>,
    /// This machine's name in the fleet document.
    pub machine: Option<String>,
    /// Where the fleet document comes from, as shown.
    pub fleet_source: Option<String>,
    /// The fleet document's pinned signer.
    pub fleet_key: Option<String>,
    /// The fleet document's fate.
    pub fleet: FleetState,
    /// What was set wrong, one line each, in reading order.
    pub notes: Vec<String>,
}

impl Settings {
    /// Whether `id` is used here.
    #[must_use]
    pub fn wants(&self, id: &str) -> bool {
        self.uses.iter().any(|(c, on, _)| c.id == id && *on)
    }
}

fn one(text: &str) -> String {
    crate::harness::one_line(text, 120)
}

fn connection_id(word: &str) -> Option<&'static str> {
    connector::connections()
        .find(|c| c.id == word)
        .map(|c| c.id)
}

fn accepted_ids() -> String {
    connector::ids().join(", ")
}

/// Reads one key of a choices table into `layer`.
fn read_key(layer: &mut Layer, key: &str, value: &Value, at: &str, notes: &mut Vec<String>) {
    let mut note = |why: String| notes.push(format!("{at}{why}; ignored"));
    match key {
        "reviews" => match value.as_bool() {
            Some(on) => layer.reviews = Some(on),
            None => note("`reviews` takes true or false".to_string()),
        },
        "preferred" => match value.as_str() {
            None => note("`preferred` takes a CLI's name".to_string()),
            Some(word) => match connection_id(word) {
                Some(id) => layer.preferred = Some(id),
                None => note(format!(
                    "`preferred = {:?}` names no AI CLI this build knows ({})",
                    one(word),
                    accepted_ids()
                )),
            },
        },
        "reviewers" => match value.as_array() {
            None => note("`reviewers` takes a list of CLI names".to_string()),
            Some(list) => {
                let mut ids = Vec::new();
                for v in list {
                    match v.as_str().map(|w| (w, connection_id(w))) {
                        Some((_, Some(id))) => {
                            if !ids.contains(&id) {
                                ids.push(id);
                            }
                        }
                        Some((word, None)) => note(format!(
                            "`reviewers` names {:?}, no AI CLI this build knows",
                            one(word)
                        )),
                        None => note("`reviewers` takes a list of CLI names".to_string()),
                    }
                }
                layer.reviewers = Some(ids);
            }
        },
        "review_count" => match value
            .as_integer()
            .filter(|n| (1..=i64::from(REVIEW_COUNT_MAX)).contains(n))
            .and_then(|n| u8::try_from(n).ok())
        {
            Some(n) => layer.review_count = Some(n),
            None => note(format!("`review_count` takes 1 to {REVIEW_COUNT_MAX}")),
        },
        "review_timeout_s" => match value
            .as_integer()
            .filter(|n| (i64::from(TIMEOUT_MIN_S)..=i64::from(TIMEOUT_MAX_S)).contains(n))
            .and_then(|n| u32::try_from(n).ok())
        {
            Some(n) => layer.review_timeout_s = Some(n),
            None => note(format!(
                "`review_timeout_s` takes {TIMEOUT_MIN_S} to {TIMEOUT_MAX_S} seconds"
            )),
        },
        "review_permissions" => match value.as_str().and_then(Choice::parse) {
            Some(c) => layer.review_permissions = Some(c),
            None => {
                note("`review_permissions` takes inherit, yolo, edits, ask or plan".to_string())
            }
        },
        other => match (connection_id(other), value.as_table()) {
            (Some(id), Some(table)) => {
                for (k, v) in table {
                    match (k.as_str(), v.as_bool()) {
                        ("use", Some(on)) => {
                            layer.uses.insert(id, on);
                        }
                        ("use", None) => note(format!("`{id}.use` takes true or false")),
                        (k, _) => note(format!(
                            "`{id}.{}` is not a key (only `use` is per CLI)",
                            one(k)
                        )),
                    }
                }
            }
            (Some(id), None) => note(format!("`{id}` is a table of its own: [{id}]")),
            (None, _) => note(format!(
                "`{}` is not a key this build knows (the AI CLIs here: {})",
                one(other),
                accepted_ids()
            )),
        },
    }
}

fn machine_name(word: &str) -> bool {
    let b = word.as_bytes();
    !b.is_empty()
        && b.len() <= MACHINE_MAX
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Reads `[ai]` out of `aterm.toml`'s text, leniently.
#[must_use]
pub fn read(text: &str) -> (Local, Vec<String>) {
    let mut local = Local::default();
    let mut notes = Vec::new();
    let doc = match aterm_toml::from_str::<Value>(text) {
        Ok(doc) => doc,
        Err(e) => {
            notes.push(format!(
                "aterm.toml is not TOML, so [ai] is read as empty: {}",
                one(&e.to_string())
            ));
            return (local, notes);
        }
    };
    let Some(ai) = doc.get(TABLE) else {
        return (local, notes);
    };
    let Some(table) = ai.as_table() else {
        notes.push("`ai` is not a table: write it as [ai]".to_string());
        return (local, notes);
    };
    for (key, value) in table {
        match key.as_str() {
            "enabled" => match value.as_bool() {
                Some(on) => local.enabled = Some(on),
                None => notes.push("[ai] `enabled` takes true or false; ignored".to_string()),
            },
            "machine" => match value.as_str().filter(|w| machine_name(w)) {
                Some(w) => local.machine = Some(w.to_string()),
                None => notes.push(format!(
                    "[ai] `machine` takes a name of up to {MACHINE_MAX} of a-z, 0-9 and -; ignored"
                )),
            },
            "fleet" => match value.as_str() {
                Some(u) if super::fleet::is_url(u) && !u.chars().any(char::is_whitespace) => {
                    local.fleet = Some(FleetSource::Url(u.to_string()));
                }
                Some(p) if Path::new(p).is_absolute() => {
                    local.fleet = Some(FleetSource::Path(PathBuf::from(p)));
                }
                _ => notes.push(
                    "[ai] `fleet` takes an absolute path or an https:// URL; ignored".to_string(),
                ),
            },
            "fleet_key" => match value
                .as_str()
                .filter(|k| super::fleet::key_bytes(k).is_some())
            {
                Some(k) => local.fleet_key = Some(k.trim().to_string()),
                None => notes.push(
                    "[ai] `fleet_key` takes an Ed25519 public key in base64 \
                     (`aterm ai fleet keygen` prints one); ignored"
                        .to_string(),
                ),
            },
            other => read_key(&mut local.layer, other, value, "[ai] ", &mut notes),
        }
    }
    (local, notes)
}

/// Reads a fleet document. Only text that is not TOML, or a schema this build does not
/// read, refuses it: anything else it cannot use is a note, so a document written for a
/// newer build still applies here. Its `seq` and `valid_until` are the fetch's
/// ([`super::fleet::meta`]).
///
/// # Errors
///
/// Why it is refused, in one line.
pub fn read_fleet(text: &str) -> Result<(Fleet, Vec<String>), String> {
    let doc = aterm_toml::from_str::<Value>(text).map_err(|_| "not TOML".to_string())?;
    let table = doc.as_table().ok_or("not a table")?;
    match table.get("schema").and_then(Value::as_integer) {
        Some(FLEET_SCHEMA) => {}
        Some(n) => {
            return Err(format!(
                "schema {n}; this build reads schema {FLEET_SCHEMA}"
            ));
        }
        None => return Err(format!("no `schema = {FLEET_SCHEMA}`")),
    }
    let mut fleet = Fleet::default();
    let mut notes = Vec::new();
    for (key, value) in table {
        match key.as_str() {
            "schema" | "seq" | "valid_until" => {}
            "machine" => {
                let Some(rows) = value.as_table() else {
                    notes.push(
                        "fleet: `machine` is a table of [machine.<name>] rows; ignored".to_string(),
                    );
                    continue;
                };
                for (name, row) in rows {
                    let at = format!("fleet [machine.{}] ", one(name));
                    match row.as_table().filter(|_| machine_name(name)) {
                        Some(row) => {
                            let mut layer = Layer::default();
                            for (k, v) in row {
                                read_key(&mut layer, k, v, &at, &mut notes);
                            }
                            fleet.machines.insert(name.clone(), layer);
                        }
                        None => notes.push(format!("{at}is not a machine row; ignored")),
                    }
                }
            }
            other => read_key(&mut fleet.root, other, value, "fleet ", &mut notes),
        }
    }
    Ok((fleet, notes))
}

/// The bytes of the file at `path` ([`aterm_uds::shared_write`]).
fn read_private(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NONBLOCK);
    }
    let file = match open.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("no such file".to_string());
        }
        Err(e) => return Err(one(&e.to_string())),
    };
    let meta = file.metadata().map_err(|e| one(&e.to_string()))?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` reads this process's effective uid; it cannot fail.
        let me = unsafe { libc::geteuid() };
        if meta.uid() != me {
            return Err("owned by another user".to_string());
        }
        if aterm_uds::shared_write::others_can_write(&file).map_err(|e| one(&e.to_string()))? {
            return Err("writable by others".to_string());
        }
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| one(&e.to_string()))?;
    if bytes.len() as u64 > max {
        return Err(format!("larger than {} KiB", max / 1024));
    }
    Ok(bytes)
}

/// Reads the fleet document at `path`, bounded; a symbolic link is followed (the owner's
/// synced dotfiles are often one). With `key`, its `<path>.sig` must verify under it.
///
/// # Errors
///
/// Why it is not read or refused, in one line.
pub fn load_fleet(path: &Path, key: Option<&str>) -> Result<(Fleet, Vec<String>), String> {
    let bytes = read_private(path, FLEET_MAX_BYTES)?;
    if let Some(key) = key {
        let mut sig_path = path.as_os_str().to_owned();
        sig_path.push(".sig");
        let sig = read_private(Path::new(&sig_path), super::fleet::SIG_MAX)
            .map_err(|why| format!("its signature file: {why}"))?;
        super::fleet::verify(&bytes, &sig, key)?;
    }
    let text = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    read_fleet(&text)
}

/// Reads the fleet document `source` names: a file as [`load_fleet`] reads it; a URL's as
/// the cache under `ai_dir` holds it, verified again under `key` ([`super::fleet::held`]).
pub fn load_source(
    source: &FleetSource,
    key: Option<&str>,
    ai_dir: Option<&Path>,
    now: i64,
) -> Result<(Fleet, Vec<String>), String> {
    match source {
        FleetSource::Path(p) => load_fleet(p, key),
        FleetSource::Url(url) => {
            let key =
                key.ok_or("a fleet document from a URL needs [ai] fleet_key to pin its signer")?;
            let dir = ai_dir.ok_or("aterm's state directory is unknown")?;
            let text = super::fleet::held(dir, url, key, now)?;
            read_fleet(&text)
        }
    }
}

fn pick<T: Clone>(local: Option<&T>, row: Option<&T>, root: Option<&T>, default: T) -> (T, From) {
    if let Some(v) = local {
        (v.clone(), From::Local)
    } else if let Some(v) = row {
        (v.clone(), From::FleetMachine)
    } else if let Some(v) = root {
        (v.clone(), From::Fleet)
    } else {
        (default, From::Default)
    }
}

/// The built-in reviewers: every AI CLI, in `VENDORS` order (those not used here drop out).
#[must_use]
pub fn default_reviewers() -> Vec<&'static str> {
    connector::ids()
}

/// The built-in preferred CLI: the first connection this platform has a build of.
#[must_use]
pub fn default_preferred() -> &'static Connector {
    connector::connections()
        .find(|c| atpkg::vendor_direct::served_here(c.id))
        .unwrap_or(&connector::CLAUDE)
}

/// The named fleet document, else `ai-fleet.toml` beside `aterm.toml` unless it is absent.
#[must_use]
pub fn fleet_source(local: &Local, config: Option<&Path>) -> Option<FleetSource> {
    local.fleet.clone().or_else(|| {
        config
            .and_then(Path::parent)
            .map(|dir| dir.join(FLEET_FILE))
            .filter(|path| {
                !matches!(
                    std::fs::symlink_metadata(path),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound
                )
            })
            .map(FleetSource::Path)
    })
}

/// Resolves `local` over the fleet document `source` names.
#[must_use]
pub fn resolve(
    local: &Local,
    source: Option<&FleetSource>,
    fleet: Option<Result<(Fleet, Vec<String>), String>>,
    mut notes: Vec<String>,
) -> Settings {
    let (doc, state) = match (source, fleet) {
        (None, _) | (Some(_), None) => (Fleet::default(), FleetState::None),
        (Some(source), Some(Ok((doc, fleet_notes)))) => {
            notes.extend(fleet_notes);
            let row = local
                .machine
                .as_ref()
                .is_some_and(|m| doc.machines.contains_key(m));
            (
                doc,
                FleetState::Read {
                    source: source.shown(),
                    row,
                },
            )
        }
        (Some(source), Some(Err(why))) => (
            Fleet::default(),
            FleetState::Refused {
                source: source.shown(),
                why,
            },
        ),
    };
    let empty = Layer::default();
    let row = local
        .machine
        .as_ref()
        .and_then(|m| doc.machines.get(m))
        .unwrap_or(&empty);
    let root = &doc.root;
    let l = &local.layer;
    let preferred_id = pick(
        l.preferred.as_ref(),
        row.preferred.as_ref(),
        root.preferred.as_ref(),
        default_preferred().id,
    );
    let preferred = (
        connector::connector(preferred_id.0).unwrap_or_else(default_preferred),
        preferred_id.1,
    );
    let reviewer_ids = pick(
        l.reviewers.as_ref(),
        row.reviewers.as_ref(),
        root.reviewers.as_ref(),
        default_reviewers(),
    );
    let reviewers = (
        reviewer_ids
            .0
            .iter()
            .filter_map(|id: &&str| connector::connector(id))
            .collect(),
        reviewer_ids.1,
    );
    let uses = connector::connections()
        .map(|c| {
            let (on, from) = pick(
                l.uses.get(c.id),
                row.uses.get(c.id),
                root.uses.get(c.id),
                true,
            );
            (c, on, from)
        })
        .collect();
    if let FleetState::Read { .. } = state {
        match &local.machine {
            Some(m) if !doc.machines.contains_key(m) && !doc.machines.is_empty() => notes.push(
                format!("the fleet document has no [machine.{m}] row; its top level applies"),
            ),
            None if !doc.machines.is_empty() => notes.push(
                "no [ai] machine is set, so no [machine.*] row of the fleet document applies"
                    .to_string(),
            ),
            _ => {}
        }
    }
    Settings {
        enabled: local.enabled.unwrap_or(true),
        reviews: pick(
            l.reviews.as_ref(),
            row.reviews.as_ref(),
            root.reviews.as_ref(),
            true,
        ),
        preferred,
        reviewers,
        review_count: pick(
            l.review_count.as_ref(),
            row.review_count.as_ref(),
            root.review_count.as_ref(),
            REVIEW_COUNT_DEFAULT,
        ),
        review_timeout_s: pick(
            l.review_timeout_s.as_ref(),
            row.review_timeout_s.as_ref(),
            root.review_timeout_s.as_ref(),
            TIMEOUT_DEFAULT_S,
        ),
        review_permissions: pick(
            l.review_permissions.as_ref(),
            row.review_permissions.as_ref(),
            root.review_permissions.as_ref(),
            Choice::Inherit,
        ),
        uses,
        machine: local.machine.clone(),
        fleet_source: source.map(FleetSource::shown),
        fleet_key: local.fleet_key.clone(),
        fleet: state,
        notes,
    }
}

/// The AI store's directory under aterm's state root, where a URL's fleet document is held.
#[must_use]
pub fn default_ai_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|root| root.join(super::store::DIR))
}

/// Reads `aterm.toml` at `config` (absent or unreadable: every default) and the fleet document.
#[must_use]
pub fn load_with(config: Option<&Path>, ai_dir: Option<&Path>, now: i64) -> Settings {
    let text = config.and_then(|p| {
        crate::harness::accounts::read_bounded(p, CONFIG_MAX_BYTES)
            .ok()
            .flatten()
    });
    let (local, notes) = read(text.as_deref().unwrap_or_default());
    let source = fleet_source(&local, config);
    let fleet = source
        .as_ref()
        .map(|s| load_source(s, local.fleet_key.as_deref(), ai_dir, now));
    resolve(&local, source.as_ref(), fleet, notes)
}

/// [`load_with`] under aterm's own state directory, now.
#[must_use]
pub fn load(config: Option<&Path>) -> Settings {
    load_with(
        config,
        default_ai_dir().as_deref(),
        crate::supervise::limit::unix_now(),
    )
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
