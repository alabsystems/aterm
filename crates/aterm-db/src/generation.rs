// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local writer succession after a table's directory binding is lost. The old
//! actor and its files are never repaired or reused. One private shadow home
//! joins through ordinary admission and imports independently witnessed signed
//! records before application authoring is enabled.

use crate::admission::deadline as admission_deadline;
use crate::{
    Error, Result,
    catalog::Directory,
    table::{
        Session,
        store::{Region, Store, custody},
    },
};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use aterm_secrets::{
    client::{self, Paths},
    commons::{bootstrap::Bootstrap, journal::Journal, roster::Access},
    fsx,
    transport::Source,
};
use aterm_uds::private_file::{self, PrivateLock};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const LINK: &str = "writer-generation-origin";
const STATE: &str = "selection";
const FENCE: &str = "reservation";
const MAX_STATE: usize = 16 * 1024;
const MAX_GENERATIONS: usize = 8;
const ADVANCE_WITHIN: Duration = Duration::from_secs(20);

struct Budget<'a> {
    until: Instant,
    cancelled: &'a dyn Fn() -> bool,
}
impl Budget<'_> {
    fn check(&self) -> Result<()> {
        if (self.cancelled)() || Instant::now() >= self.until {
            return Err(Error::new("Commons writer recovery pass ended"));
        }
        Ok(())
    }
}

fn root(paths: &Paths) -> PathBuf {
    paths.dir.join("commons/writer-generation")
}
fn candidate(paths: &Paths) -> Paths {
    Paths::for_home(&root(paths).join("home"))
}
fn candidate_origin(paths: &Paths) -> Option<Paths> {
    if !paths
        .home
        .ends_with(".config/aterm/secrets/commons/writer-generation/home")
    {
        return None;
    }
    let original = Paths::for_home(paths.home.ancestors().nth(6)?);
    (candidate(&original) == *paths).then_some(original)
}
/// The bytes of a path a selection record cannot spell as text. A Unix path is bytes, so a home
/// or config that is not UTF-8 is kept as hex under `<name>_hex` beside its null text and reads
/// back the same ([`path_field`]); a UTF-8 path is text alone, so its record is unchanged.
/// Elsewhere a path the record cannot spell has no such form and reads back as unavailable.
fn unspelled(path: &Path) -> Option<&[u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.to_str().is_none().then(|| path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
/// The path a selection record holds under `name`: its text, or, where that is null, the bytes
/// [`unspelled`] kept under `<name>_hex`. Those must not be UTF-8 (text would have spelled
/// them, so each path has one spelling) and hold no NUL.
fn path_field(value: &Value, name: &str) -> Result<PathBuf> {
    if let Some(text) = value[name].as_str() {
        return Ok(PathBuf::from(text));
    }
    #[cfg(unix)]
    if value[name].is_null()
        && let Some(hex) = value[format!("{name}_hex").as_str()].as_str()
    {
        use std::os::unix::ffi::OsStrExt as _;
        let raw = aterm_codec::hex::decode_vec(hex.as_bytes()).ok_or_else(fail)?;
        if std::str::from_utf8(&raw).is_ok() || raw.contains(&0) {
            return Err(fail());
        }
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&raw)));
    }
    Err(fail())
}
fn fail() -> Error {
    Error::new("Commons writer generation is unavailable or changed")
}

// The reservation is published under the old control writer, before candidate
// custody. Neither it nor the child link is removed by recovery. Empty staging
// directories do not fence an original writer whose reservation was refused.
fn fenced(paths: &Paths) -> Result<bool> {
    let root = root(paths);
    for path in [
        root.join(FENCE),
        root.join(STATE),
        candidate(paths).dir.join(LINK),
    ] {
        if fsx::read_private(&path, MAX_STATE)
            .map_err(crate::table::failure)?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Clone)]
struct Selection {
    original: Paths,
    config: PathBuf,
    actor: String,
    trust: String,
    complete: Option<Digest>,
}
impl Selection {
    fn bytes(&self) -> Vec<u8> {
        let mut record = json!({"version":1, "home":self.original.home.to_str(),
            "config":self.config.to_str(), "actor":self.actor, "trust":self.trust,
            "complete":self.complete.map(|v| aterm_codec::hex::encode_lower(&v))});
        if let Some(fields) = record.as_object_mut() {
            for (name, path) in [("home", &self.original.home), ("config", &self.config)] {
                if let Some(raw) = unspelled(path) {
                    fields.insert(
                        format!("{name}_hex"),
                        Value::String(aterm_codec::hex::encode_lower(raw)),
                    );
                }
            }
        }
        record.to_string().into_bytes()
    }
    fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = aterm_json::from_slice(bytes).map_err(|_| fail())?;
        let word = |name: &str| value[name].as_str().ok_or_else(fail);
        if value["version"].as_u64() != Some(1) {
            return Err(fail());
        }
        let original = Paths::for_home(&path_field(&value, "home")?);
        let config = path_field(&value, "config")?;
        if !original.home.is_absolute() || !config.is_absolute() {
            return Err(fail());
        }
        let complete = value["complete"]
            .as_str()
            .map(crate::database::decode_hex)
            .transpose()?;
        Ok(Self {
            original,
            config,
            actor: word("actor")?.into(),
            trust: word("trust")?.into(),
            complete,
        })
    }
    fn read(paths: &Paths) -> Result<Option<Self>> {
        let selection = fsx::read_private(&root(paths).join(STATE), MAX_STATE)
            .map_err(crate::table::failure)?
            .map(|v| Self::parse(v.as_slice()))
            .transpose()?;
        if selection.as_ref().is_some_and(|s| s.original != *paths) {
            return Err(fail());
        }
        Ok(selection)
    }
    fn save(&self) -> Result<()> {
        fsx::write_atomic(&root(&self.original).join(STATE), &self.bytes(), 0o600)
            .map_err(crate::table::failure)
    }
    fn authorize(&self) -> Result<Bootstrap> {
        self.authorize_until(admission_deadline())
    }
    fn authorize_until(&self, deadline: Instant) -> Result<Bootstrap> {
        if !crate::runtime::enabled(&self.config)? {
            return Err(fail());
        }
        guard_until(&self.original, false, deadline)?;
        let bootstrap =
            Bootstrap::required_until(&self.original, deadline).map_err(crate::table::failure)?;
        self.validate_bootstrap(bootstrap, deadline)
    }
    fn observe_until(&self, deadline: Instant) -> Result<Bootstrap> {
        if !crate::runtime::enabled(&self.config)? {
            return Err(fail());
        }
        Observation::observe(&self.original, deadline)?;
        let (bootstrap, _) = Bootstrap::established_with_journal_until(&self.original, deadline)
            .map_err(crate::table::failure)?;
        self.validate_bootstrap(bootstrap, deadline)
    }
    fn validate_bootstrap(&self, bootstrap: Bootstrap, deadline: Instant) -> Result<Bootstrap> {
        bootstrap
            .revalidate_custody()
            .map_err(crate::table::failure)?;
        let journal = aterm_secrets::commons::journal::Journal::read_existing(
            bootstrap.journal_dir(),
            bootstrap.trust(),
        )
        .map_err(crate::table::failure)?;
        let access = bootstrap
            .access_in(&journal)
            .map_err(crate::table::failure)?
            .ok_or_else(fail)?;
        if !access.is_current(&journal)
            || access.local().role() != aterm_secrets::commons::roster::Role::Writer
            || aterm_codec::hex::encode_lower(&access.local().binding().actor()) != self.actor
            || trust(&bootstrap) != self.trust
        {
            return Err(fail());
        }
        if Instant::now() >= deadline {
            return Err(fail());
        }
        Ok(bootstrap)
    }
}

fn trust(bootstrap: &Bootstrap) -> String {
    let trust = bootstrap.trust();
    let mut bytes = [0u8; 16 + 32 + 32];
    bytes[..16].copy_from_slice(&trust.network);
    bytes[16..48].copy_from_slice(&trust.authority);
    bytes[48..80].copy_from_slice(&trust.genesis);
    aterm_codec::hex::encode_lower(&bytes)
}

/// Claim bytes are the independently acknowledged prefix commitments. A later
/// source receipt invalidates completion and requires another idempotent import.
fn claims(paths: &Paths) -> Result<Vec<(Region, Digest, Vec<u8>)>> {
    let mut out = Vec::new();
    for (region, relative) in [
        (Region::Primary, "commons/table-claims"),
        (Region::Recovery, "commons/recovery/table-claims"),
    ] {
        let dir = paths.dir.join(relative);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let judged = private_file::JudgedDir::judge(&dir).map_err(|_| fail())?;
        if !judged.is_private() {
            return Err(fail());
        }
        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(name) = file_name.strip_suffix(".claim") else {
                continue;
            };
            let id = crate::database::decode_hex(name)?;
            if out.len() >= 128 {
                return Err(fail());
            }
            let bytes = judged.read(file_name, 4096).map_err(|_| fail())?;
            out.push((region, id, bytes));
        }
    }
    out.sort_unstable_by_key(|(region, id, _)| (*region == Region::Recovery, *id));
    Ok(out)
}
fn seal(paths: &Paths) -> Result<Digest> {
    let mut hash = Sha256::new();
    hash.update(b"aterm/commons/writer-succession-prefixes/1\0");
    for (region, id, bytes) in claims(paths)? {
        hash.update([u8::from(region == Region::Recovery)]);
        hash.update(id);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(hash.finalize())
}

fn linked(paths: &Paths) -> Result<Option<Selection>> {
    let Some(bytes) =
        fsx::read_private(&paths.dir.join(LINK), MAX_STATE).map_err(crate::table::failure)?
    else {
        if candidate_origin(paths).is_some() {
            return Err(fail());
        }
        return Ok(None);
    };
    let link = Selection::parse(bytes.as_slice())?;
    if candidate(&link.original) != *paths || link.original == *paths {
        return Err(fail());
    }
    let mut ancestor = link.original.clone();
    for depth in 1..=MAX_GENERATIONS {
        let Some(bytes) = fsx::read_private(&ancestor.dir.join(LINK), MAX_STATE)
            .map_err(crate::table::failure)?
        else {
            if candidate_origin(&ancestor).is_some() {
                return Err(fail());
            }
            break;
        };
        let parent = Selection::parse(bytes.as_slice())?;
        if depth == MAX_GENERATIONS || candidate(&parent.original) != ancestor {
            return Err(fail());
        }
        ancestor = parent.original;
    }
    let current = Selection::read(&link.original)?.ok_or_else(fail)?;
    if link.actor != current.actor || link.trust != current.trust || link.config != current.config {
        return Err(fail());
    }
    Ok(Some(current))
}

pub(crate) fn is_candidate(paths: &Paths) -> bool {
    candidate_origin(paths).is_some()
}

/// Preserved predecessor installations constrain immutable application pins.
/// This follows the same checked local links as source locking; it grants no
/// authoring rights and never selects or repairs another installation.
pub(crate) fn predecessors(paths: &Paths) -> Result<Vec<Paths>> {
    let mut predecessors = Vec::new();
    let mut child = paths.clone();
    for _ in 0..MAX_GENERATIONS {
        let Some(selection) = linked(&child)? else {
            return Ok(predecessors);
        };
        child = selection.original;
        predecessors.push(child.clone());
    }
    if linked(&child)?.is_some() {
        return Err(fail());
    }
    Ok(predecessors)
}

/// Pending succession must periodically reattempt preservation. A completed
/// successor keeps its healthy transport and its per-peer repair scheduling;
/// merely living under the generation home is no reason to discard them.
pub(crate) fn refresh_required(paths: &Paths) -> Result<bool> {
    if fenced(paths)? {
        return Ok(true);
    }
    let mut child = paths.clone();
    for _ in 0..MAX_GENERATIONS {
        let Some(selection) = linked(&child)? else {
            return Ok(false);
        };
        if selection.complete != Some(seal(&selection.original)?) {
            return Ok(true);
        }
        child = selection.original;
    }
    if linked(&child)?.is_some() {
        return Err(fail());
    }
    Ok(false)
}

fn try_local_source_reads(paths: &Paths) -> Result<Option<Vec<client::CommonsSourceRead>>> {
    let mut reads = Vec::new();
    let mut child = paths.clone();
    for depth in 0..=MAX_GENERATIONS {
        let Some(read) = client::try_commons_source_read(&child).map_err(crate::table::failure)?
        else {
            // All earlier guards drop before the caller waits for any writer.
            return Ok(None);
        };
        reads.push(read);
        let Some(selection) = linked(&child)? else {
            return Ok(Some(reads));
        };
        if depth == MAX_GENERATIONS {
            return Err(fail());
        }
        child = selection.original;
    }
    Err(fail())
}

/// Take the current and ancestor vault read locks before any control or table
/// custody. A busy attempt releases every acquired guard before waiting. These
/// locks grant no authority; the subsequent session still admits each source.
pub(crate) fn local_source_reads_until(
    paths: &Paths,
    deadline: Instant,
) -> Result<Vec<client::CommonsSourceRead>> {
    let mut backoff = Duration::from_millis(1);
    let unavailable = || Error::new("table repair vault sources are busy or unavailable");
    loop {
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        if let Some(reads) = try_local_source_reads(paths)? {
            if Instant::now() >= deadline {
                return Err(unavailable());
            }
            return Ok(reads);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(unavailable());
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

/// One table session shares its authenticated ancestor observations. Every use
/// still reobserves policy, immutable custody and the current control/log view;
/// only unchanged authority avoids rebuilding the same bootstrap and roster.
pub(crate) struct Observation {
    paths: Paths,
    ancestors: Vec<Ancestor>,
}

struct Ancestor {
    child: Paths,
    selection: Selection,
    bootstrap: Bootstrap,
    access: Access,
    expected: client::Config,
}

impl Observation {
    pub(crate) fn local_source_reads(&self) -> Result<Vec<client::CommonsSourceRead>> {
        self.ancestors
            .iter()
            .map(|ancestor| {
                ancestor
                    .bootstrap
                    .local_source_read()
                    .map_err(crate::table::failure)
            })
            .collect()
    }

    pub(crate) fn capture(paths: &Paths, deadline: Instant) -> Result<Self> {
        Self::capture_in(paths, deadline, &|| deadline, false)
    }

    pub(crate) fn observe(paths: &Paths, deadline: Instant) -> Result<Self> {
        Self::observe_within(paths, deadline, &|| deadline)
    }

    /// Observe under two bounds, for a read given longer to read than any holder should be given.
    /// `read_by` only refuses a read that has run out of its own time. `wait` is asked as each
    /// stage that waits for a predecessor's vault or custody locks starts, and says when that
    /// stage's waits end, the reading between them included. The wait for a predecessor's device
    /// lock runs with its exclusive `commons.lock` held, so waiting can refuse the writers who need
    /// that custody for that long; the reads made under that lock come on top, with no bound.
    pub(crate) fn observe_within(
        paths: &Paths,
        read_by: Instant,
        wait: &dyn Fn() -> Instant,
    ) -> Result<Self> {
        Self::capture_in(paths, read_by, wait, true)
    }

    fn capture_in(
        paths: &Paths,
        read_by: Instant,
        wait: &dyn Fn() -> Instant,
        observe_only: bool,
    ) -> Result<Self> {
        let mut child = paths.clone();
        let mut ancestors = Vec::new();
        while let Some(selection) = linked(&child)? {
            if ancestors.len() == MAX_GENERATIONS || Instant::now() >= read_by {
                return Err(fail());
            }
            let bootstrap = if observe_only {
                let (bootstrap, _) =
                    Bootstrap::established_with_journal_until(&selection.original, wait())
                        .map_err(crate::table::failure)?;
                bootstrap
                    .revalidate_custody()
                    .map_err(crate::table::failure)?;
                if !crate::runtime::enabled(&selection.config)?
                    || trust(&bootstrap) != selection.trust
                {
                    return Err(fail());
                }
                bootstrap
            } else {
                selection.authorize_until(wait())?
            };
            let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust())
                .map_err(crate::table::failure)?;
            let access = bootstrap
                .access_in(&journal)
                .map_err(crate::table::failure)?
                .ok_or_else(fail)?;
            if !access.is_current(&journal)
                || access.local().role() != aterm_secrets::commons::roster::Role::Writer
                || aterm_codec::hex::encode_lower(&access.local().binding().actor())
                    != selection.actor
            {
                return Err(fail());
            }
            let expected = client::config(&selection.original)
                .map_err(crate::table::failure)?
                .ok_or_else(fail)?;
            let parent = selection.original.clone();
            ancestors.push(Ancestor {
                child,
                selection,
                bootstrap,
                access,
                expected,
            });
            child = parent;
        }
        let observation = Self {
            paths: paths.clone(),
            ancestors,
        };
        observation.check(false, false, read_by)?;
        Ok(observation)
    }

    pub(crate) fn check(&self, authoring: bool, mutation: bool, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Err(fail());
        }
        if (authoring || mutation) && fenced(&self.paths)? {
            return Err(Error::new(
                "original Commons writer is fenced by succession",
            ));
        }
        let mut child = self.paths.clone();
        for ancestor in &self.ancestors {
            let selected = linked(&child)?.ok_or_else(fail)?;
            if child != ancestor.child
                || selected.original != ancestor.selection.original
                || selected.config != ancestor.selection.config
                || selected.actor != ancestor.selection.actor
                || selected.trust != ancestor.selection.trust
                || !crate::runtime::enabled(&selected.config)?
            {
                return Err(fail());
            }
            ancestor
                .bootstrap
                .revalidate_custody()
                .map_err(crate::table::failure)?;
            let journal = Journal::read_existing(
                ancestor.bootstrap.journal_dir(),
                ancestor.bootstrap.trust(),
            )
            .map_err(crate::table::failure)?;
            if client::local_recipient_denied_in_roster(&selected.original)
                || !ancestor.access.is_current(&journal)
            {
                return Err(fail());
            }
            if let Some(current) = client::config(&child).map_err(crate::table::failure)? {
                if current.name != ancestor.expected.name
                    || !client::pin_admitted(&current, ancestor.expected.signer)
                {
                    return Err(fail());
                }
            } else if authoring {
                return Err(fail());
            }
            if authoring && selected.complete != Some(seal(&selected.original)?) {
                return Err(Error::new(
                    "Commons writer succession is still preserving old records",
                ));
            }
            child = selected.original;
        }
        if linked(&child)?.is_some() || Instant::now() >= deadline {
            return Err(fail());
        }
        Ok(())
    }
}

/// Every table batch in a shadow generation still depends on current original
/// authorization. Completion grants authoring only while old receipts match.
pub(crate) fn guard(paths: &Paths, authoring: bool) -> Result<()> {
    guard_until(paths, authoring, admission_deadline())
}

pub(crate) fn guard_until(paths: &Paths, authoring: bool, deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(fail());
    }
    if authoring && fenced(paths)? {
        return Err(Error::new(
            "original Commons writer is fenced by succession",
        ));
    }
    let Some(selection) = linked(paths)? else {
        return Ok(());
    };
    selection.authorize_until(deadline)?;
    let expected = client::config(&selection.original)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    if let Some(current) = client::config(paths).map_err(crate::table::failure)? {
        if current.name != expected.name || !client::pin_admitted(&current, expected.signer) {
            return Err(fail());
        }
    } else if authoring {
        return Err(fail());
    }
    if authoring {
        let mut ancestor = Some(selection);
        for _ in 0..MAX_GENERATIONS {
            if Instant::now() >= deadline {
                return Err(fail());
            }
            let Some(selected) = ancestor else {
                return Ok(());
            };
            if selected.complete != Some(seal(&selected.original)?) {
                return Err(Error::new(
                    "Commons writer succession is still preserving old records",
                ));
            }
            ancestor = linked(&selected.original)?;
        }
        if ancestor.is_some() {
            return Err(fail());
        }
    }
    Ok(())
}

/// Retention in the successor remains possible before salvage is complete;
/// the original actor must never mutate even an otherwise healthy old table.
pub(crate) fn allow_mutation(paths: &Paths) -> Result<()> {
    allow_mutation_until(paths, admission_deadline())
}

pub(crate) fn allow_mutation_until(paths: &Paths, deadline: Instant) -> Result<()> {
    if fenced(paths)? {
        return Err(Error::new(
            "original Commons writer is fenced by succession",
        ));
    }
    guard_until(paths, false, deadline)
}

/// Resolve application access only after the candidate has been admitted and
/// the entire retained prefix has been verified and imported. This never joins.
pub(crate) fn selected(paths: &Paths) -> Result<Paths> {
    selected_at(paths, 0, None)
}

pub(crate) fn selected_until(paths: &Paths, deadline: Instant) -> Result<Paths> {
    selected_at(paths, 0, Some(deadline))
}

/// Select a completed generation without repairing current or ancestor custody.
/// The same completion, policy and current membership checks still apply.
pub(crate) fn observed_paths_until(paths: &Paths, deadline: Instant) -> Result<Paths> {
    observed_paths_within(paths, deadline, &|| deadline)
}

/// [`observed_paths_until`] under [`Observation::observe_within`]'s two bounds.
pub(crate) fn observed_paths_within(
    paths: &Paths,
    read_by: Instant,
    wait: &dyn Fn() -> Instant,
) -> Result<Paths> {
    Ok(observed_selection_within(paths, read_by, wait)?.paths)
}

fn observed_selection_within(
    paths: &Paths,
    read_by: Instant,
    wait: &dyn Fn() -> Instant,
) -> Result<Observation> {
    // Ordinary recovery may repair local files; a public observation uses
    // only established custody, including every predecessor in the chain.
    let mut selected = paths.clone();
    for depth in 0..=MAX_GENERATIONS {
        if Instant::now() >= read_by {
            return Err(fail());
        }
        if Selection::read(&selected)?.is_none() {
            break;
        }
        if depth == MAX_GENERATIONS {
            return Err(fail());
        }
        selected = candidate(&selected);
    }
    let observation = Observation::observe_within(&selected, read_by, wait)?;
    observation.check(true, false, read_by)?;
    Ok(observation)
}

pub(crate) fn lineage_until(
    paths: &Paths,
    deadline: Instant,
) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
    let observation = observed_selection_within(paths, deadline, &|| deadline)?;
    let selected = observation.paths.clone();
    let (bootstrap, journal) = Bootstrap::established_with_journal_until(&selected, deadline)
        .map_err(crate::table::failure)?;
    let access = bootstrap
        .access_in(&journal)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    if !access.is_current(&journal)
        || access.local().role() != aterm_secrets::commons::roster::Role::Writer
        || client::local_recipient_denied_in_roster(&selected)
    {
        return Err(fail());
    }
    let mut bindings = Vec::new();
    for ancestor in observation.ancestors.iter().rev() {
        if ancestor.bootstrap.trust() != bootstrap.trust() {
            return Err(fail());
        }
        bindings.push(ancestor.access.local().binding().clone());
    }
    let current = access.local().binding();
    if bindings
        .iter()
        .any(|binding| binding.actor() == current.actor())
    {
        return Err(fail());
    }
    bindings.push(current.clone());
    bootstrap
        .revalidate_custody()
        .map_err(crate::table::failure)?;
    let latest = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust())
        .map_err(crate::table::failure)?;
    if !access.is_current(&latest) {
        return Err(fail());
    }
    observation.check(true, false, deadline)?;
    Ok(bindings)
}

struct Lineage {
    selected: Paths,
    trust: Option<aterm_secrets::commons::journal::Trust>,
    ancestors: Vec<(Paths, aterm_secrets::commons::binding::Binding)>,
}

impl Lineage {
    fn check(
        &self,
        paths: &Paths,
        session: &Session<'_>,
        deadline: Instant,
    ) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
        let current = session.peer_writer(session.actor())?;
        let current_trust = session.control_context()?.0.trust();
        if session.paths() != &self.selected
            || self.trust.is_some_and(|trust| trust != current_trust)
            || self
                .ancestors
                .iter()
                .any(|(_, binding)| binding.actor() == current.actor())
            || selected_until(paths, deadline)? != self.selected
        {
            return Err(fail());
        }
        let mut bindings: Vec<_> = self
            .ancestors
            .iter()
            .map(|(_, binding)| binding.clone())
            .collect();
        bindings.push(current);
        Ok(bindings)
    }
}

/// Reuse the caller's current journal while revalidating every ancestor. No
/// second lock on the selected control journal is acquired during publication.
pub(crate) fn lineage_in(
    paths: &Paths,
    session: &Session<'_>,
    deadline: Instant,
) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
    lineage_prefix_until(paths, deadline)?.check(paths, session, deadline)
}

pub(crate) fn issuer_predecessor(
    paths: &Paths,
    session: &Session<'_>,
    original: &aterm_secrets::commons::binding::Binding,
    deadline: Instant,
) -> Result<Bootstrap> {
    let lineage = lineage_prefix_until(paths, deadline)?;
    lineage.check(paths, session, deadline)?;
    let (path, _) = lineage
        .ancestors
        .iter()
        .find(|(_, binding)| binding == original)
        .ok_or_else(fail)?;
    let bootstrap = Bootstrap::required_until(path, deadline).map_err(crate::table::failure)?;
    if bootstrap
        .request()
        .map_err(crate::table::failure)?
        .binding()
        != original
    {
        return Err(fail());
    }
    Ok(bootstrap)
}

fn lineage_prefix_until(paths: &Paths, deadline: Instant) -> Result<Lineage> {
    let mut path = paths.clone();
    let mut trust = None;
    let mut ancestors = Vec::new();
    for depth in 0..=MAX_GENERATIONS {
        if Instant::now() >= deadline {
            return Err(fail());
        }
        if let Some(selection) = Selection::read(&path)? {
            if depth == MAX_GENERATIONS || selection.complete != Some(seal(&path)?) {
                return Err(fail());
            }
            let bootstrap = selection.authorize_until(deadline)?;
            if trust.is_some_and(|trust| trust != bootstrap.trust()) {
                return Err(fail());
            }
            trust = Some(bootstrap.trust());
            ancestors.push((
                path.clone(),
                bootstrap
                    .request()
                    .map_err(crate::table::failure)?
                    .binding()
                    .clone(),
            ));
            path = candidate(&path);
        } else {
            guard_until(&path, true, deadline)?;
            return Ok(Lineage {
                selected: path,
                trust,
                ancestors,
            });
        }
    }
    Err(fail())
}

#[cfg(any(test, debug_assertions))]
pub(crate) fn test_complete_successor(
    fixture: &Path,
    original: &Paths,
    successor: &Paths,
    config: &Path,
    deadline: Instant,
) -> Result<Paths> {
    let temporary = std::fs::canonicalize(std::env::temp_dir()).map_err(|_| fail())?;
    let fixture = std::fs::canonicalize(fixture).map_err(|_| fail())?;
    if fixture == temporary || !fixture.starts_with(&temporary) {
        return Err(fail());
    }
    for path in [&original.home, &successor.home] {
        let path = std::fs::canonicalize(path).map_err(|_| fail())?;
        if path == fixture || !path.starts_with(&fixture) {
            return Err(fail());
        }
    }
    for path in [&fixture, &original.dir, &successor.dir] {
        aterm_uds::private_file::check_private_dir(path).map_err(|_| fail())?;
    }
    let before = Bootstrap::required_until(original, deadline).map_err(crate::table::failure)?;
    let after = Bootstrap::required_until(successor, deadline).map_err(crate::table::failure)?;
    if before.trust() != after.trust()
        || before
            .request()
            .map_err(crate::table::failure)?
            .binding()
            .actor()
            == after
                .request()
                .map_err(crate::table::failure)?
                .binding()
                .actor()
    {
        return Err(fail());
    }
    Session::open_until(&after, deadline)?.writer()?;
    let mut selection = reserve_until(original, config, &before, deadline)?;
    let next = candidate(original);
    for entry in std::fs::read_dir(&successor.dir).map_err(|_| fail())? {
        let entry = entry.map_err(|_| fail())?;
        let target = next.dir.join(entry.file_name());
        if target.exists() {
            return Err(fail());
        }
        std::fs::rename(entry.path(), target).map_err(|_| fail())?;
    }
    // A real candidate joins at this home and materializes its private files
    // there. The pre-admitted fixture's files still belonged to its old home.
    client::sync(&next, None).map_err(crate::table::failure)?;
    import_until(&mut selection, &|| Ok(()), deadline)?;
    Ok(next)
}

/// Vault facts, local needs and compiled holder paths belong to the original
/// Mac. A linked table generation must never relocate those consumers. Validate
/// every ancestor before returning a path from its private succession link.
pub(crate) fn logical_paths(paths: &Paths) -> Result<Paths> {
    logical_paths_until(paths, admission_deadline())
}

pub(crate) fn logical_paths_until(paths: &Paths, deadline: Instant) -> Result<Paths> {
    logical_paths_in(paths, false, deadline)
}

/// Resolve read-only application facts to the original Mac using established
/// lineage only. Missing ancestor custody is refused without being repaired.
pub(crate) fn observed_logical_paths(paths: &Paths) -> Result<Paths> {
    observed_logical_paths_until(paths, admission_deadline())
}

pub(crate) fn observed_logical_paths_until(paths: &Paths, deadline: Instant) -> Result<Paths> {
    logical_paths_in(paths, true, deadline)
}

fn logical_paths_in(paths: &Paths, observe_only: bool, deadline: Instant) -> Result<Paths> {
    let observation = Observation::capture_in(paths, deadline, &|| deadline, observe_only)?;
    Ok(observation.ancestors.last().map_or_else(
        || paths.clone(),
        |ancestor| ancestor.selection.original.clone(),
    ))
}

/// Names and receipt counts only. Unconfirmed crash tails remain in their
/// immutable sources and are never counted as imported committed operations.
pub(crate) fn status(paths: &Paths, deadline: Instant) -> Result<Value> {
    let mut source = paths.clone();
    let mut sources = Vec::new();
    for generation in 0..=MAX_GENERATIONS {
        let Some(selection) = Selection::read(&source)? else {
            break;
        };
        if generation == MAX_GENERATIONS {
            return Err(fail());
        }
        let bootstrap = selection.observe_until(deadline)?;
        let session = Session::observe_until(&bootstrap, deadline)?;
        let mut tables = Vec::new();
        for (region, id, _) in claims(&source)? {
            if region != Region::Primary {
                continue;
            }
            let evidence = custody::inspect(&session, id)?;
            let (acknowledged, unconfirmed) = match evidence.prefix {
                Some(Ok(prefix)) => (
                    Some(prefix.acknowledged_records),
                    (prefix.committed_records as u64).checked_sub(prefix.acknowledged_records),
                ),
                _ => (None, None),
            };
            tables.push(json!({"table":aterm_codec::hex::encode_lower(&id),
                "retained_acknowledged":acknowledged, "retained_unconfirmed":unconfirmed}));
        }
        sources.push(json!({"generation":generation, "source_preserved":true,
            "import_complete":selection.complete == Some(seal(&source)?), "tables":tables}));
        source = candidate(&source);
    }
    if sources.is_empty() {
        if fenced(paths)? {
            return Ok(json!({"state":"pending", "sources":[], "original_writer_fenced":true}));
        }
        return Ok(Value::Null);
    }
    Ok(
        json!({"state":if observed_paths_until(paths, deadline).is_ok() {"ready"} else {"pending"},
        "sources":sources, "original_writer_fenced":true}),
    )
}

/// Read and authenticate each preserved prefix without importing it or changing
/// either writer. Development inspection makes a pending import's exact table
/// and refusal visible without publishing application payloads or key material.
#[cfg(any(test, debug_assertions))]
pub(crate) fn inspect(paths: &Paths) -> Result<Value> {
    let started = Instant::now();
    let deadline = admission_deadline();
    let selection = Selection::read(paths)?.ok_or_else(fail)?;
    let original = selection.observe_until(deadline)?;
    let source = Session::observe_until(&original, deadline)?;
    let next = candidate(paths);
    let (bootstrap, _) = Bootstrap::established_with_journal_until(&next, deadline)
        .map_err(crate::table::failure)?;
    let verifier = Session::observe_until(&bootstrap, deadline)?;
    if bootstrap.trust() != original.trust() || source.actor() == verifier.actor() {
        return Err(fail());
    }
    let mut tables = Vec::new();
    for (region, id, _) in claims(paths)? {
        let table_started = Instant::now();
        let result = custody::salvage_region(&source, &verifier, id, region);
        let mut table = json!({
            "region":if region == Region::Primary {"primary"} else {"recovery"},
            "table":aterm_codec::hex::encode_lower(&id),
            "elapsed_ms":table_started.elapsed().as_millis(),
        });
        let fields = table.as_object_mut().expect("inspection is an object");
        match result {
            Ok(retained) => {
                fields.insert(
                    "table".into(),
                    json!(aterm_codec::hex::encode_lower(&retained.scope.digest())),
                );
                fields.insert(
                    "authenticated_records".into(),
                    json!(retained.entries.len()),
                );
            }
            Err(error) => {
                fields.insert(
                    "error".into(),
                    json!(aterm_codec::tty::one_line(&error.to_string(), 512)),
                );
            }
        }
        tables.push(table);
    }
    Ok(json!({
        "kind":"writer-recovery-inspection","mutated":false,
        "source_actor":aterm_codec::hex::encode_lower(&source.actor()),
        "candidate_actor":aterm_codec::hex::encode_lower(&verifier.actor()),
        "elapsed_ms":started.elapsed().as_millis(),"tables":tables,
    }))
}

fn selected_at(paths: &Paths, depth: usize, deadline: Option<Instant>) -> Result<Paths> {
    if depth > MAX_GENERATIONS || deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(fail());
    }
    let Some(selection) = Selection::read(paths)? else {
        match deadline {
            Some(deadline) => guard_until(paths, true, deadline)?,
            None => guard(paths, true)?,
        };
        return Ok(paths.clone());
    };
    match deadline {
        Some(deadline) => selection.authorize_until(deadline)?,
        None => selection.authorize()?,
    };
    if selection.complete != Some(seal(paths)?) {
        return Err(Error::new("Commons writer succession is pending"));
    }
    selected_at(&candidate(paths), depth + 1, deadline)
}

#[cfg(test)]
fn reserve(paths: &Paths, config: &Path, bootstrap: &Bootstrap) -> Result<Selection> {
    reserve_until(paths, config, bootstrap, admission_deadline())
}

fn reserve_until(
    paths: &Paths,
    config: &Path,
    bootstrap: &Bootstrap,
    deadline: Instant,
) -> Result<Selection> {
    if let Some(selection) = Selection::read(paths)? {
        selection.authorize_until(deadline)?;
        return Ok(selection);
    }
    if !crate::runtime::enabled(config)? {
        return Err(fail());
    }
    // Ordinary table mutations hold this control writer. Publish the fence
    // after earlier batches finish, before any new generation is used.
    let session = Session::open_until(bootstrap, deadline)?;
    session.may_write()?;
    let selection = Selection {
        original: paths.clone(),
        config: config.into(),
        actor: aterm_codec::hex::encode_lower(&session.actor()),
        trust: trust(bootstrap),
        complete: None,
    };
    let root_dir = root(paths);
    aterm_journal::create_private_dir_durable(&root_dir)?;
    let reservation = selection.bytes();
    let fence_path = root_dir.join(FENCE);
    fsx::write_new(&fence_path, &reservation, 0o600).map_err(crate::table::failure)?;
    if fsx::read_private(&fence_path, MAX_STATE)
        .map_err(crate::table::failure)?
        .as_ref()
        .map(|value| value.as_slice())
        != Some(reservation.as_slice())
    {
        return Err(fail());
    }
    aterm_durable::durable_dir(&root_dir)?;
    let next = candidate(paths);
    // Each component is private and symlinks are refused. The materializer may
    // write only under this independent home, never under the person's home.
    for path in [
        root_dir,
        next.home.clone(),
        next.home.join(".config"),
        next.home.join(".config/aterm"),
        next.dir.clone(),
    ] {
        aterm_journal::create_private_dir_durable(&path)?;
    }
    let link_path = next.dir.join(LINK);
    fsx::write_new(&link_path, &reservation, 0o600).map_err(crate::table::failure)?;
    if fsx::read_private(&link_path, MAX_STATE)
        .map_err(crate::table::failure)?
        .as_ref()
        .map(|v| v.as_slice())
        != Some(reservation.as_slice())
    {
        return Err(fail());
    }
    aterm_durable::durable_dir(&next.dir)?;
    selection.save()?;
    Ok(selection)
}

#[cfg(test)]
fn import(selection: &mut Selection) -> Result<()> {
    import_until(selection, &|| Ok(()), admission_deadline())
}

#[cfg(test)]
fn import_while(selection: &mut Selection, current: &dyn Fn() -> Result<()>) -> Result<()> {
    import_until(selection, current, admission_deadline())
}

fn import_until(
    selection: &mut Selection,
    current: &dyn Fn() -> Result<()>,
    deadline: Instant,
) -> Result<()> {
    current()?;
    let original = selection.authorize_until(deadline)?;
    let source = Session::observe_until(&original, deadline)?;
    let before = seal(&selection.original)?;
    let next = candidate(&selection.original);
    let bootstrap = Bootstrap::required_until(&next, deadline).map_err(crate::table::failure)?;
    if bootstrap.trust() != original.trust() {
        return Err(fail());
    }
    let session = Session::open_until(&bootstrap, deadline)?;
    if session.actor() == source.actor() {
        return Err(Error::new("writer succession cannot reuse the old actor"));
    }
    session.may_write()?;
    let directory = Directory::new(&next, bootstrap.trust());
    let mut retained = claims(&selection.original)?;
    let catalog = directory.catalog_scope();
    retained.sort_unstable_by_key(|(_, id, _)| (*id != catalog.digest(), *id));
    if !retained
        .iter()
        .any(|(region, id, _)| *region == Region::Primary && *id == catalog.digest())
    {
        return Err(fail());
    }
    for (region, id, _) in retained {
        current()?;
        selection.authorize_until(deadline)?;
        let retained = custody::witnessed_prefix(&source, id, region)?;
        let path = directory.path(retained.scope())?;
        private_file::create_private_dir(path.parent().ok_or_else(fail)?)?;
        let mut store = Store::join(&session, &path, retained.scope())?;
        retained.import(&source, &session, &mut store, current)?;
    }
    // Parsing the canonical catalog checks its signed declarations under the
    // successor's current authority. No new table UUID or generation is minted.
    directory.snapshot_existing(&session)?;
    if before != seal(&selection.original)? {
        return Err(Error::snapshot_changed());
    }
    current()?;
    selection.authorize_until(deadline)?;
    selection.complete = Some(before);
    selection.save()
}

/// The ordinary runtime calls this between transport lifetimes. A single
/// durable candidate survives retries and process death. No lock over old
/// custody or tables is held during normal vault network I/O.
#[cfg(test)]
fn advance(paths: &Paths, config: &Path) -> Result<Paths> {
    advance_while(paths, config, &|| false)
}

pub(crate) fn advance_while(
    paths: &Paths,
    config: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Paths> {
    let budget = Budget {
        until: Instant::now() + ADVANCE_WITHIN,
        cancelled,
    };
    advance_at(paths, config, 0, &budget)
}

fn recovery_required(session: &Session<'_>, reserved: bool) -> Result<bool> {
    let paths = session.paths();
    let tables = if reserved {
        vec![
            Directory::new(paths, session.trust())
                .catalog_scope()
                .digest(),
        ]
    } else {
        // Application tables can predate a healthy catalog. Check every retained
        // primary claim, including one the current catalog does not declare.
        claims(paths)?
            .into_iter()
            .filter_map(|(region, table, _)| (region == Region::Primary).then_some(table))
            .collect()
    };
    for table in tables {
        if !reserved && !custody::directory_binding_lost(session, table)? {
            continue;
        }
        let evidence = custody::inspect(session, table)?;
        if !reserved
            && !evidence
                .directory
                .is_some_and(|e| e.binding_error.is_some())
        {
            continue;
        }
        if !matches!(evidence.prefix, Some(Ok(_))) {
            return Err(fail());
        }
        return Ok(true);
    }
    Ok(false)
}

fn advance_at(paths: &Paths, config: &Path, depth: usize, budget: &Budget<'_>) -> Result<Paths> {
    budget.check()?;
    if depth > MAX_GENERATIONS {
        return Err(fail());
    }
    if !crate::runtime::enabled(config)? {
        return Err(fail());
    }
    let existing = Selection::read(paths)?;
    let recovering = fenced(paths)?;
    let bootstrap = match Bootstrap::cached_until(paths, budget.until) {
        Ok(Some(bootstrap)) => bootstrap,
        Ok(None) | Err(_) if existing.is_none() => return Ok(paths.clone()),
        Ok(None) => return Err(fail()),
        Err(error) => return Err(crate::table::failure(error)),
    };
    let session = match Session::observe_until(&bootstrap, budget.until) {
        Ok(session) => session,
        Err(_) if existing.is_none() => return Ok(paths.clone()),
        Err(error) => return Err(error),
    };
    if existing.is_none() {
        let vault = client::config(paths)
            .map_err(crate::table::failure)?
            .ok_or_else(fail)?;
        if !client::alab_vault(&vault) || !matches!(vault.source, Source::Url(_)) {
            // Other networks keep their existing receive-only recovery path;
            // this automatic enrollment policy belongs only to default ALab.
            return Ok(paths.clone());
        }
        if !recovery_required(&session, recovering)? {
            return Ok(paths.clone());
        }
    }
    drop(session);
    if depth == MAX_GENERATIONS && existing.is_none() {
        return Err(fail());
    }
    aterm_journal::create_private_dir_durable(&root(paths))?;
    let _lock = PrivateLock::take(
        &root(paths),
        "lock",
        Duration::ZERO,
        "writer succession is active",
    )?;
    budget.check()?;
    let mut selection = reserve_until(paths, config, &bootstrap, budget.until)?;
    if selection.config != config {
        return Err(fail());
    }
    selection.authorize_until(budget.until)?;
    budget.check()?;
    let next = candidate(paths);
    let original_config = client::config(paths)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    let configured = client::config(&next).map_err(crate::table::failure)?;
    if let Some(configured) = &configured
        && (configured.name != original_config.name
            || !client::alab_vault(configured)
            || !client::pin_admitted(configured, original_config.signer))
    {
        return Err(fail());
    }
    let pending = configured.is_none()
        || matches!(
            client::status(&next)
                .map_err(crate::table::failure)?
                .membership,
            client::Membership::Pending | client::Membership::Unconfigured
        );
    if pending {
        let options = client::JoinOptions {
            source: String::new(),
            host: None,
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        };
        // A lost or expired request is reposted with this same durable identity.
        let _ = client::join_if_unchanged_until(&next, &options, configured.as_ref(), budget.until);
    } else {
        // The ordinary pass also syncs membership records and tries verified
        // ALab server candidates when the initially discovered URL is gone.
        let _ = client::sync_reader_until(&next, budget.until);
    }
    selection.authorize_until(budget.until)?;
    budget.check()?;
    if let Some(configured) = client::config(&next).map_err(crate::table::failure)?
        && (configured.name != original_config.name
            || !client::alab_vault(&configured)
            || !client::pin_admitted(&configured, original_config.signer))
    {
        return Err(fail());
    }
    if selection.complete == Some(seal(paths)?) {
        return advance_at(&next, config, depth + 1, budget);
    }
    // Pending membership and missing history keys remain retryable. The runtime
    // serves this same candidate so ordinary authenticated repair can supply them.
    let _ = import_until(&mut selection, &|| budget.check(), budget.until);
    Ok(next)
}

#[cfg(test)]
mod tests;
