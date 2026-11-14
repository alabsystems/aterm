// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local writer succession after a table's directory binding is lost. The old
//! actor and its files are never repaired or reused. One private shadow home
//! is admitted to the vault on the fenced original's signed statement, with no
//! person ([`admit_by_statement`]), and imports independently witnessed signed
//! records before application authoring is enabled.

use crate::admission::deadline as admission_deadline;
use crate::{
    Error, Result,
    catalog::Directory,
    file::materialize::Projecting,
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
    commons::{
        bootstrap::{Bootstrap, Device},
        journal::Journal,
        roster::Access,
    },
    fsx,
    members::{Allowed, Bundle, bundle::Reader},
    transport::Source,
};
use aterm_uds::private_file::{self, PrivateLock};

mod retained;
#[cfg(test)]
pub(crate) use retained::with_retained_session;
pub(crate) use retained::{ReadValidation, RetainedContext, with_retained_context};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const LINK: &str = "writer-generation-origin";
const STATE: &str = "selection";
const FENCE: &str = "reservation";
/// The fenced original's signed statement for its successor ([`admit_by_statement`]).
const STATEMENT: &str = "statement";
/// Why the statement admitted nothing on the last recovery pass ([`note_statement`]).
const REFUSED: &str = "statement-refused";
/// That this home's succession reached its completing save at least once ([`mark_completed`]):
/// it completed, or was about to.
const COMPLETED: &str = "completed";
/// Why a pass admits nothing from a successor home that lost its link ([`advance_at`]).
const UNLINKED: &str = "the successor home this writer reserved does not link to it (emptied, restored or unreadable), so no key is made there and nothing asks from it";
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
pub(crate) fn candidate(paths: &Paths) -> Paths {
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
/// A check that reached its deadline before it finished. It learned nothing about the
/// generation, so it must not read as [`fail`]'s finding that the generation changed: on a
/// loaded Mac a slow, unheld check is out of time, and the next check may pass.
fn out_of_time() -> Error {
    Error::new("Commons writer generation check ran out of time before it finished")
}

/// Every deadline gate of a generation check: refuse with [`out_of_time`] once `deadline` has
/// passed, never with [`fail`].
fn within(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(out_of_time());
    }
    Ok(())
}

// The reservation is published under the old control writer, before candidate
// custody. Neither it nor the child link is removed by recovery. Empty staging
// directories do not fence an original writer whose reservation was refused.
pub(crate) fn fenced(paths: &Paths) -> Result<bool> {
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

/// Record, durably and once, that this home's succession reached its completing save: a file
/// beside the selection that no write removes. [`import_until`] writes it before the save that
/// gives the selection its completion, so no home that completes on this release holds a
/// completed selection without it; a recovery pass ([`advance_at`]) writes it for a selection
/// that holds a completion and has none, as a home that completed on a release without the
/// mark has. It is what tells a selection file lost after its import completed, which
/// [`reserve_until`] writes again with no completion, from the record of a succession that
/// never completed ([`may_have_completed`]).
///
/// Written first, it is also held by a home whose completing save never landed (the process
/// died between the two, or the save failed): nothing completed there and the successor
/// authored nothing, and its held reads refuse until a pass completes the import. Removing
/// the mark after a failed save would not tell that state from a save that landed.
fn mark_completed(paths: &Paths) -> Result<()> {
    let root = root(paths);
    fsx::write_new(&root.join(COMPLETED), b"{\"version\":1}", 0o600)
        .map_err(crate::table::failure)?;
    Ok(aterm_durable::durable_dir(&root)?)
}

/// Whether this home's succession may have completed: it holds the mark ([`mark_completed`]),
/// which every completion of this release leaves and which a completing save that never
/// landed leaves too. Where it completed its successor could author from then on, so this
/// home's records are not known to be its lineage's newest, whatever its selection record
/// says now. A home with no mark is not shown never to have completed: one that completed on
/// a release that wrote none and lost its selection before a recovery pass ran here, or that
/// lost the mark with the selection, holds none.
fn may_have_completed(paths: &Paths) -> Result<bool> {
    Ok(fsx::read_private(&root(paths).join(COMPLETED), MAX_STATE)
        .map_err(crate::table::failure)?
        .is_some())
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
        Ok(Self::read_record(paths)?.map(|(selection, _)| selection))
    }
    /// [`Self::read`], with the record's bytes as the file held them.
    fn read_record(paths: &Paths) -> Result<Option<(Self, aterm_secrets::SecretBytes)>> {
        let Some(record) = fsx::read_private(&root(paths).join(STATE), MAX_STATE)
            .map_err(crate::table::failure)?
        else {
            return Ok(None);
        };
        let selection = Self::parse(record.as_slice())?;
        if selection.original != *paths {
            return Err(fail());
        }
        Ok(Some((selection, record)))
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
            .access_observed_in(&journal)
            .map_err(crate::table::failure)?
            .ok_or_else(fail)?;
        if !access
            .is_current_observed(&journal)
            .map_err(crate::table::failure)?
            || access.local().role() != aterm_secrets::commons::roster::Role::Writer
            || aterm_codec::hex::encode_lower(&access.local().binding().actor()) != self.actor
            || trust(&bootstrap) != self.trust
        {
            return Err(fail());
        }
        within(deadline)?;
        Ok(bootstrap)
    }
}

fn trust(bootstrap: &Bootstrap) -> String {
    trust_hex(bootstrap.trust())
}

/// How a selection record spells a lab's trust: its network, authority and genesis, in hex.
fn trust_hex(trust: aterm_secrets::commons::journal::Trust) -> String {
    let mut bytes = [0u8; 16 + 32 + 32];
    bytes[..16].copy_from_slice(&trust.network);
    bytes[16..48].copy_from_slice(&trust.authority);
    bytes[48..80].copy_from_slice(&trust.genesis);
    aterm_codec::hex::encode_lower(&bytes)
}

/// What lets a read answer a displaced table from the records its home still holds
/// ([`Store::observe_displaced`]): the home's own selection record, as read when it named the
/// session's writer and lab and held no completion. That is what this gate knows, and no more.
/// A home with such a record is fenced: nothing authors or mutates a table there any more
/// ([`Observation::check`], [`allow_mutation`]), so the prefix each of its claims witnesses is
/// what it holds. Where the record is the one its reservation first wrote, the succession has
/// never completed, and its successor, whatever part of an import it holds, has authored
/// nothing, since authoring there needs the completion ([`guard_lineage_until`]). One state
/// reads the same and is not that one: a selection file lost after its import completed is
/// written again by [`reserve_until`] with no completion, in the bytes the first reservation
/// wrote (the kept reservation and link must equal them), so until the import completes again
/// the successor may hold records it authored that this home does not. The home's completion
/// mark, where it holds one, tells the two apart ([`may_have_completed`], which names the
/// homes that hold none): this gate refuses a home that holds one, as [`held_here`], the read
/// gate of every pending succession (FL-14), does. It names no candidate home, and it grants
/// nothing but that read: no writer, no binding and no resource right.
pub(crate) struct Preserving {
    paths: Paths,
    record: Vec<u8>,
}

/// What [`Preserving::of`] refuses a home with whose selection is not the session's standing
/// fence.
pub(crate) const NOT_HELD: &str = "this home's writer succession completed or may have, or names another writer or lab; no read is held from it";

/// What [`Preserving::of`] refuses a home with that is fenced ([`fenced`]) and holds no selection
/// record: the record was lost, or a reservation has written its fence and not yet its selection.
pub(crate) const UNSELECTED: &str = "this home is fenced but its writer succession's selection record is missing; no read is held from it";

impl Preserving {
    /// `session`'s home as [`Self`], read with [`Selection::read`]'s own read and parse; none
    /// where the home has no selection and nothing else fences it ([`fenced`]): no succession
    /// was reserved there.
    ///
    /// The session's currency is not judged here: the read this gates judges it. The claim read
    /// of [`Store::observe_displaced`] (`claim_read`) takes the source reads of the vault that
    /// open holds, which a vault writer's moment refuses as busy, and asks the session's
    /// context under them before it reads the claim; a reader observes a busy read again.
    /// [`Session::control_context`] judges currency with no source read and takes that moment
    /// for membership that changed, so asking it here turned a busy read into a refusal. The
    /// lab compared is the session's own ([`Session::trust`]): the trust its control journal
    /// was read under, which every session's access holds equal to its bootstrap's, spelled as
    /// [`reserve_until`] spelled the reserving session's bootstrap's.
    ///
    /// # Errors
    /// The selection, or where there is none the reservation or the candidate link, cannot be
    /// read; the selection cannot be parsed or names another home; the home is fenced and
    /// holds no selection ([`UNSELECTED`]); or the selection is not this session's standing
    /// fence ([`NOT_HELD`]): it holds a completion (an invalidated completion still holds
    /// one), the home holds a completion mark, so a completion may have preceded this record
    /// ([`may_have_completed`]), or it names another writer than the session's or another lab
    /// than its custody's, as [`reserve_until`] wrote them from the reserving session and its
    /// bootstrap; or that mark cannot be read. The two refusals in words say so, so a reader
    /// that asked for a held read learns why it got none.
    pub(crate) fn of(session: &Session<'_>) -> Result<Option<Self>> {
        #[cfg(test)]
        seams::reached(seams::Point::HeldGate);
        let paths = session.paths();
        let Some((selection, record)) = Selection::read_record(paths)? else {
            if fenced(paths)? {
                return Err(Error::new(UNSELECTED));
            }
            return Ok(None);
        };
        if selection.complete.is_some()
            || selection.actor != aterm_codec::hex::encode_lower(&session.actor())
            || selection.trust != trust_hex(session.trust())
            || may_have_completed(paths)?
        {
            return Err(Error::new(NOT_HELD));
        }
        Ok(Some(Self {
            paths: paths.clone(),
            record: record.as_slice().to_vec(),
        }))
    }

    /// Whether the home's selection record is still, byte for byte, the one [`Self::of`] read.
    /// That is enough to know the record still holds no completion: the one `of` read has none,
    /// and the only write that gives a selection one is the completing save ([`import_until`]),
    /// which replaces the file with other bytes. It does not show that none was ever written:
    /// no write clears a completion in a record, but a selection file that is lost is written
    /// again without one, in the same bytes ([`reserve_until`]). `of` tells that record from
    /// the first by the home's completion mark, which is written before the completing save
    /// and never removed, so in a home that holds its mark a record `of` passed is one no
    /// completion preceded; in one that holds none ([`may_have_completed`]) neither this check
    /// nor `of` tells them apart. (An
    /// invalidated completion is a seal mismatch computed when the record is read, not a
    /// rewrite of it; it never passes `of`.) A session of a home that is no successor keeps no
    /// generation observation ([`Session::observe_selected`]), so its own context checks never
    /// read the selection: for a held read of a displaced table this is the only check that
    /// does.
    ///
    /// # Errors
    /// The selection cannot be read or parsed, or names another home.
    pub(crate) fn stands(&self) -> Result<bool> {
        Ok(Selection::read_record(&self.paths)?
            .is_some_and(|(_, record)| record.as_slice() == self.record.as_slice()))
    }
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

/// One try at the current and ancestor vault read locks, [`local_source_reads_until`]'s: `None`
/// where a source's state lock is held by a writer or is absent, every guard taken before it let
/// go. It never waits.
pub(crate) fn try_local_source_reads(
    paths: &Paths,
) -> Result<Option<Vec<client::CommonsSourceRead>>> {
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
        #[cfg(test)]
        SOURCE_READ_SLEEPS.set(SOURCE_READ_SLEEPS.get() + 1);
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread slept in [`local_source_reads_until`], waiting for a source
    /// a writer held or that was absent.
    pub(crate) static SOURCE_READ_SLEEPS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
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
    /// The home this observation was taken for: the selected generation's.
    pub(crate) const fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The writers of the generations this home succeeds on this disk, oldest first: each
    /// predecessor home's own binding, which this observation proved the current writer of that
    /// home's established custody, the actor its succession record names. Each is still a
    /// Commons member.
    pub(crate) fn succeeded(
        &self,
    ) -> impl Iterator<Item = &aterm_secrets::commons::binding::Binding> {
        self.ancestors
            .iter()
            .rev()
            .map(|ancestor| ancestor.access.local().binding())
    }

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
            if ancestors.len() == MAX_GENERATIONS {
                return Err(fail());
            }
            within(read_by)?;
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
                .access_observed_in(&journal)
                .map_err(crate::table::failure)?
                .ok_or_else(fail)?;
            if !access
                .is_current_observed(&journal)
                .map_err(crate::table::failure)?
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
        within(deadline)?;
        if (authoring || mutation) && fenced(&self.paths)? {
            return Err(Error::new(
                "original Commons writer is fenced by succession",
            ));
        }
        self.check_lineage(authoring, deadline)
    }

    /// A read of the records this home holds: every predecessor's succession complete, as
    /// authoring requires, but no refusal of this home's own fence. A fenced home is one whose
    /// writer recovery is still pending; it may no longer write, and its records stay readable
    /// until its successor has imported them ([`observed_generation_until`]).
    pub(crate) fn check_read(&self, deadline: Instant) -> Result<()> {
        within(deadline)?;
        held_here(&self.paths, false)?;
        self.check_lineage(true, deadline)
    }

    /// [`Self::check_read`] for the shared-file materializer's projection
    /// (docs/DESIGN-shared-files-2026-11-11.md §4): the same lineage checks, every predecessor's
    /// succession complete, and the same refusal of a home a completed succession superseded,
    /// without the refusal of a home whose own succession was completed and then invalidated
    /// ([`Standing::Unsettled`]). What such a home holds is not known to be its lineage's
    /// newest, so only a caller that labels what it reads `held`, authors nothing, ingests
    /// nothing and persists no receipt may read it: `by` is that caller's proof, which only
    /// `file::materialize` can make.
    pub(crate) fn check_materialize(&self, deadline: Instant, _by: &Projecting) -> Result<()> {
        within(deadline)?;
        held_here(&self.paths, true)?;
        self.check_lineage(true, deadline)
    }

    fn check_lineage(&self, authoring: bool, deadline: Instant) -> Result<()> {
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
            #[cfg(test)]
            seams::reached(seams::Point::BeforeDenial);
            let journal = Journal::read_existing(
                ancestor.bootstrap.journal_dir(),
                ancestor.bootstrap.trust(),
            )
            .map_err(crate::table::failure)?;
            // A vault writer's moment is busy, which a read observes again, not a changed
            // generation; a denial, or a state that cannot be judged, still refuses.
            if client::local_recipient_denial_observed(&selected.original)
                .map_err(crate::table::failure)?
            {
                return Err(fail());
            }
            #[cfg(test)]
            seams::reached(seams::Point::BeforeCurrent);
            if !ancestor
                .access
                .is_current_observed(&journal)
                .map_err(crate::table::failure)?
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
        if linked(&child)?.is_some() {
            return Err(fail());
        }
        within(deadline)?;
        Ok(())
    }
}

/// Every table batch in a shadow generation still depends on current original
/// authorization. Completion grants authoring only while old receipts match.
pub(crate) fn guard(paths: &Paths, authoring: bool) -> Result<()> {
    guard_until(paths, authoring, admission_deadline())
}

pub(crate) fn guard_until(paths: &Paths, authoring: bool, deadline: Instant) -> Result<()> {
    within(deadline)?;
    if authoring && fenced(paths)? {
        return Err(Error::new(
            "original Commons writer is fenced by succession",
        ));
    }
    guard_lineage_until(paths, authoring, deadline)
}

/// [`guard_until`] for a read of the records `paths` holds: every predecessor's succession
/// complete, as authoring requires, without refusing `paths`' own fence
/// ([`Observation::check_read`]).
pub(crate) fn guard_read_until(paths: &Paths, deadline: Instant) -> Result<()> {
    within(deadline)?;
    held_here(paths, false)?;
    guard_lineage_until(paths, true, deadline)
}

/// Whether `paths` still holds the newest records of its lineage, as a read of them needs: it
/// has no selection record, or one that holds no completion, and no completion mark
/// ([`may_have_completed`]). That is a home with no succession of its own, or one whose import
/// has never completed, so its successor has authored nothing. A selection file lost after its
/// import completed reads as no succession, and once [`reserve_until`] has written it again,
/// as one that never completed, though the successor may have authored: the mark, which
/// outlives the file, refuses both as a succession still preserving, in a home that holds one
/// (a home whose completing save never landed holds one too, and refuses until a pass
/// completes it). A completed succession
/// supersedes this home, and one whose completion a later source receipt invalidated may have
/// a successor that authored since: both refuse. The second, and a home that holds the mark,
/// refuse unless `unsettled`, which only the shared-file materializer's check passes
/// ([`Observation::check_materialize`]): its walk stands both as [`Standing::Unsettled`]
/// ([`materializer_generation_until`]), so what it projects there is `held`.
///
/// A home with no selection and no mark is admitted either way, and this does not say that
/// nothing fences it: a fenced home whose selection is missing ([`fenced`]) passes here too.
/// The materializer reads that from its walk, which stands such a home as
/// [`Standing::Unsettled`] as well.
fn held_here(paths: &Paths, unsettled: bool) -> Result<()> {
    match Selection::read(paths)? {
        Some(selection) if selection.complete.is_some() => {
            if selection.complete == Some(seal(paths)?) {
                Err(Error::new(
                    "original Commons writer is fenced by succession",
                ))
            } else if unsettled {
                Ok(())
            } else {
                Err(preserving())
            }
        }
        _ if unsettled => Ok(()),
        _ if may_have_completed(paths)? => Err(preserving()),
        _ => Ok(()),
    }
}

fn preserving() -> Error {
    Error::new("Commons writer succession is still preserving old records")
}

fn guard_lineage_until(paths: &Paths, authoring: bool, deadline: Instant) -> Result<()> {
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
            within(deadline)?;
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
    Ok(observed_selection_within(paths, deadline, &|| deadline)?.paths)
}

/// The completed generation [`observed_paths_until`] selects, with the observation of its
/// predecessors that selected it, for a verified "ready" answer that opens no session
/// ([`status`], [`lineage_until`]). The observation predates every wait for the selected home's
/// custody, so a read that opens a session does not keep it: it names the home with
/// [`observed_generation_until`], opens that custody, then observes the predecessors
/// ([`observed_session`]).
///
/// [`observed_session`]: crate::documents::application::observed_session
pub(crate) fn observed_selection_within(
    paths: &Paths,
    read_by: Instant,
    wait: &dyn Fn() -> Instant,
) -> Result<Observation> {
    let observation =
        Observation::observe_within(&completed_generation_until(paths, read_by)?, read_by, wait)?;
    observation.check(true, false, read_by)?;
    Ok(observation)
}

/// The home a read of the records this Mac holds opens: each completed selection is followed to
/// its successor, and the first succession whose import has never completed stops the walk at
/// the home it is preserving. That home is fenced, so it writes nothing more, but every record it
/// holds stays readable until its successor has imported them: a writer recovery that cannot
/// finish (a successor the vault has not admitted, a missing history key) never makes the
/// records already held unreadable. An incomplete candidate is never named, and a succession
/// whose completion was invalidated refuses, as its successor may have authored since; so does
/// a home that holds a completion mark while its selection record is missing or holds no
/// completion ([`may_have_completed`]): the record was lost after the import completed, or the
/// completing save has not landed, and the next recovery pass completes it. No predecessor is
/// observed yet, so this says which home a read opens, never that it may: a reader opens that
/// home's established custody and then observes it ([`observe_generation`]), so the observation
/// it keeps is no older than that custody's wait.
pub(crate) fn observed_generation_until(paths: &Paths, read_by: Instant) -> Result<Paths> {
    generation_until(paths, read_by, false)
}

/// The completed generation a writer, a retained subscriber or a "ready" answer needs: as
/// [`observed_generation_until`], but a succession still preserving old records refuses here,
/// so nothing that authors or retains opens a home whose successor is pending.
pub(crate) fn completed_generation_until(paths: &Paths, read_by: Instant) -> Result<Paths> {
    generation_until(paths, read_by, true)
}

fn generation_until(paths: &Paths, read_by: Instant, completed: bool) -> Result<Paths> {
    #[cfg(test)]
    crate::file::seam::custody();
    // Ordinary recovery may repair local files; a public observation uses
    // only established custody, including every predecessor in the chain.
    let mut selected = paths.clone();
    for depth in 0..=MAX_GENERATIONS {
        within(read_by)?;
        let Some(selection) = Selection::read(&selected)? else {
            // A home whose succession completed and whose selection file is lost names no
            // successor, and is not known to hold the newest records ([`may_have_completed`]).
            if may_have_completed(&selected)? {
                return Err(preserving());
            }
            break;
        };
        if depth == MAX_GENERATIONS {
            return Err(fail());
        }
        if selection.complete != Some(seal(&selected)?) {
            // An import that never completed leaves every record where it was; one whose
            // completion was invalidated, or whose record was lost and written again without
            // it, may have a successor that authored since, so no home in this lineage is
            // known to hold the newest records.
            if completed || selection.complete.is_some() || may_have_completed(&selected)? {
                return Err(preserving());
            }
            break;
        }
        selected = candidate(&selected);
    }
    Ok(selected)
}

/// This Mac's original home, whichever home of its writer lineage `paths` names: a successor's
/// home sits inside its predecessor's vault folder ([`candidate`]), so the walk back reads no
/// file. What is kept once per Mac rather than once per writer generation (the shared files'
/// snapshots) is kept under this home.
pub(crate) fn original(paths: &Paths) -> Paths {
    let mut home = paths.clone();
    for _ in 0..=MAX_GENERATIONS {
        match candidate_origin(&home) {
            Some(origin) => home = origin,
            None => break,
        }
    }
    home
}

/// How the home the shared-file materializer opens stands in this Mac's writer lineage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// No succession fences it: it holds the newest records, and a snapshot of them is `live`.
    Newest,
    /// A succession whose import has never completed fences it (FL-14): every record it held
    /// at the fence is still here, its successor has authored nothing, and a snapshot of them
    /// is `held`, never `live`.
    Preserving,
    /// A succession fences it, and whether its successor has authored is not known. Either the
    /// succession completed and a later source receipt invalidated that completion, or the home
    /// holds the completion mark ([`may_have_completed`]) beside a selection that holds no
    /// completion, or the home is fenced ([`fenced`]) and has no selection to say how its
    /// succession stands: the
    /// selection is the last file a reservation writes, after the reservation and the
    /// successor's link, so a reservation cut short leaves none, and a selection lost after a
    /// completion leaves the other two behind it. The walk stops here until the import
    /// completes, or completes again. The successor may have authored and taken peers'
    /// records, none of which is here, and this home may hold a record its successor has not
    /// imported (the receipt that invalidated a completion, a record still awaiting its
    /// witness). So no home of the lineage is known to hold its newest records; a snapshot of
    /// this one is `held`, never `live`, and never replaces one taken at the successor's home
    /// ([`Held::era`]).
    Unsettled,
}

/// The home the shared-file materializer opens ([`materializer_generation_until`]).
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub paths: Paths,
    /// How many completed successions lead from this Mac's original home to `paths`: 0 the
    /// original. A snapshot taken at a later home is never replaced by an earlier one's.
    pub era: u64,
    pub standing: Standing,
    /// The actor this Mac's writer lineage began as, lower-case hex, where the original home
    /// records it (its succession's selection does); `None` where the original has started no
    /// succession, and the actor of `paths` is the lineage's first.
    pub origin: Option<String>,
}

/// The home `file::materialize` projects shared files from
/// (docs/DESIGN-shared-files-2026-11-11.md §4), and how it stands: the walk of
/// [`observed_generation_until`], which also says how far it went and why it stopped. Each
/// completed selection is followed to its successor. A succession whose import has never
/// completed stops the walk at the home it fences, [`Standing::Preserving`], as every reader's
/// walk does (FL-14), unless that home holds the completion mark ([`may_have_completed`]). One
/// whose completion was invalidated, or that holds the mark, stops it there too, as
/// [`Standing::Unsettled`], where every other reader's walk refuses: the materializer alone
/// reads that home, because it alone labels what it reads `held` and keeps the snapshot of a
/// later home over it. An incomplete candidate is never named.
///
/// A home with no selection is [`Standing::Newest`] only where nothing fences it. The fence is
/// three files ([`fenced`]): the reservation, the successor's link and the selection, written
/// in that order, and recovery removes neither of the first two. A home that holds the
/// reservation, or whose successor holds its link, and has no selection is fenced all the
/// same, and the walk stops there as [`Standing::Unsettled`], since nothing here says whether
/// its succession ever completed.
///
/// `by` is the caller's proof that it is `file::materialize` (a pass, and the standing it
/// answers the registry views with): it authors nothing, ingests nothing and persists no
/// receipt.
pub(crate) fn materializer_generation_until(
    paths: &Paths,
    read_by: Instant,
    _by: &Projecting,
) -> Result<Held> {
    #[cfg(test)]
    crate::file::seam::custody();
    let mut held = Held {
        paths: paths.clone(),
        era: 0,
        standing: Standing::Newest,
        origin: None,
    };
    for depth in 0..=MAX_GENERATIONS {
        within(read_by)?;
        let Some(selection) = Selection::read(&held.paths)? else {
            if fenced(&held.paths)? || may_have_completed(&held.paths)? {
                held.standing = Standing::Unsettled;
            }
            break;
        };
        if depth == 0 {
            held.origin = Some(selection.actor.clone());
        }
        if depth == MAX_GENERATIONS {
            return Err(fail());
        }
        if selection.complete != Some(seal(&held.paths)?) {
            // A selection that holds no completion in a home that holds the mark may be one
            // written again after a completed succession lost it ([`may_have_completed`]).
            held.standing = if selection.complete.is_some() || may_have_completed(&held.paths)? {
                Standing::Unsettled
            } else {
                Standing::Preserving
            };
            break;
        }
        held.paths = candidate(&held.paths);
        held.era += 1;
    }
    Ok(held)
}

/// Observe the home [`materializer_generation_until`] named, as [`observe_generation`] observes
/// the one a reader's walk names: each predecessor observed and the chain complete, under
/// [`Observation::check_materialize`], which also admits a home whose own completion was
/// invalidated. The session a pass opens on this observation is an observer's: a fenced home
/// refuses every authoring and mutation check whatever its succession came to
/// ([`Observation::check`], [`guard_until`]).
pub(crate) fn observe_materializer(
    selected: &Paths,
    read_by: Instant,
    wait: &dyn Fn() -> Instant,
    by: &Projecting,
) -> Result<Observation> {
    let observation = Observation::observe_within(selected, read_by, wait)?;
    observation.check_materialize(read_by, by)?;
    Ok(observation)
}

/// Seams of the shared files' tests (`crate::file`'s, and through [`crate::file::fixture`]
/// another crate's), which take a stand-in home through the writer-succession states a shared
/// file's read must answer in. Absent from release builds.
#[cfg(any(test, debug_assertions))]
#[path = "generation_file_seams_tests.rs"]
pub(crate) mod file_seams;

/// Observe the generation [`observed_generation_until`] named, under
/// [`Observation::observe_within`]'s two bounds: each predecessor observed and the chain
/// complete, as every reader of a successor home must before it reads
/// ([`Observation::check_read`]). A writer that observes this way still checks authoring itself.
pub(crate) fn observe_generation(
    selected: &Paths,
    read_by: Instant,
    wait: &dyn Fn() -> Instant,
) -> Result<Observation> {
    let observation = Observation::observe_within(selected, read_by, wait)?;
    observation.check_read(read_by)?;
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
        .access_observed_in(&journal)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    if !access
        .is_current_observed(&journal)
        .map_err(crate::table::failure)?
        || access.local().role() != aterm_secrets::commons::roster::Role::Writer
        || client::local_recipient_denial_observed(&selected).map_err(crate::table::failure)?
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
    if !access
        .is_current_observed(&latest)
        .map_err(crate::table::failure)?
    {
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
        within(deadline)?;
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
/// With the report comes the home a read of `paths` selects ([`observed_paths_until`], taken once
/// the sources are read): a report that names its sources is `ready` exactly when that selection
/// holds, and a caller reads the selected home from it rather than selecting again.
pub(crate) fn status(paths: &Paths, deadline: Instant) -> Result<(Value, Result<Paths>)> {
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
        let mut reported = json!({"generation":generation, "source_preserved":true,
            "import_complete":selection.complete == Some(seal(&source)?), "tables":tables});
        if let Some(why) = statement_refused(&source) {
            reported
                .as_object_mut()
                .expect("a source's report is an object")
                .insert("statement_refused".into(), json!(why));
        }
        sources.push(reported);
        source = candidate(&source);
    }
    if sources.is_empty() {
        if fenced(paths)? {
            return Ok((
                json!({"state":"pending", "sources":[], "original_writer_fenced":true}),
                observed_paths_until(paths, deadline),
            ));
        }
        return Ok((Value::Null, observed_paths_until(paths, deadline)));
    }
    let selected = observed_paths_until(paths, deadline);
    Ok((
        json!({"state":if selected.is_ok() {"ready"} else {"pending"},
        "sources":sources, "original_writer_fenced":true}),
        selected,
    ))
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
    if depth > MAX_GENERATIONS {
        return Err(fail());
    }
    if let Some(deadline) = deadline {
        within(deadline)?;
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

#[cfg(any(test, debug_assertions))]
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

#[cfg(any(test, debug_assertions))]
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
    // The mark first: a crash between the two leaves a pending selection whose held reads
    // refuse until the next pass completes it, never a completed one with no mark.
    mark_completed(&selection.original)?;
    selection.complete = Some(before);
    selection.save()
}

/// The ordinary runtime calls this between transport lifetimes. A single
/// durable candidate survives retries and process death. No lock over old
/// custody or tables is held during normal vault network I/O.
#[cfg(test)]
fn advance(paths: &Paths, config: &Path) -> Result<Paths> {
    // A test's pass must finish: it waits the hang detector, never the runtime's own budget
    // ([`ADVANCE_WITHIN`]), which a loaded machine can outlast with nothing wrong.
    let budget = Budget {
        until: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        cancelled: &|| false,
    };
    advance_at(paths, config, 0, &budget)
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

/// Whether writer succession enrolls a successor in `vault` by itself: the default ALab's vault
/// alone. Other networks keep their existing receive-only recovery path.
fn enrolls_itself(vault: &client::Config) -> bool {
    #[cfg(test)]
    if seams::stands_in_for_alab(vault) {
        return true;
    }
    client::alab_vault(vault)
}

/// The machine name the successor's seat is admitted under: the original's, as its own plain
/// ask would have named it.
fn successor_name(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    if aterm_secrets::text::valid_machine_name(&lower) {
        lower
    } else {
        "machine".to_owned()
    }
}

/// End a pending succession with no person (ATERM-KI-1109-1,
/// `docs/DESIGN-shared-files-2026-11-11.md` section 13, decision 1). The fenced original is
/// still a current member of its vault. It signs one statement, an [`Allowed`] under its own
/// device key that names the device key its reserved successor home holds, keeps that statement
/// beside the fence before anything else is signed, and admits the successor on it through the
/// vault's membership log ([`client::admit_successor`]); the successor's home then takes the log
/// bundle as any admitted Mac does. Nothing here authors a Commons record or lifts the fence:
/// the import and the seal run afterwards exactly as for a successor a person allowed.
///
/// What holds it to one lineage:
/// - the signer is the selection's own original, authorized as every import step authorizes it
///   (a current writer whose actor is the one the fence recorded), and the successor home is
///   the one that fence linked;
/// - the statement names exactly the device the successor home holds, which is a new actor;
/// - a fenced original names one successor key, once: a kept statement that another key signed
///   or that names another key (a replayed statement of an older generation, a successor home
///   whose device changed) admits nothing, and no second statement is signed. Where the kept
///   statement is gone with the fence (a home restored from a backup), the membership log
///   holds the same rule: a member whose admission of one successor key it holds signs for no
///   other ([`client::admit_successor`]).
///
/// # Errors
/// The original is not authorized, the successor home is not this selection's, the kept
/// statement is another's, or the vault's membership log admits nothing on this pass
/// ([`client::Succession`]).
fn admit_by_statement(selection: &Selection, budget: &Budget<'_>) -> Result<()> {
    let deadline = budget.until;
    budget.check()?;
    let original = selection.authorize_until(deadline)?;
    let next = candidate(&selection.original);
    if linked(&next)?.is_none_or(|link| link.original != selection.original) {
        return Err(fail());
    }
    client::machine_identity(&next).map_err(crate::table::failure)?;
    let device = Device::open_until(&next, deadline)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    let trust = original.trust();
    let request = device
        .request(trust.network, trust.authority)
        .map_err(crate::table::failure)?;
    let binding = request.binding();
    if aterm_codec::hex::encode_lower(&binding.actor()) == selection.actor {
        return Err(Error::new("writer succession cannot reuse the old actor"));
    }
    let path = root(&selection.original).join(STATEMENT);
    let kept = fsx::read_private(&path, MAX_STATE).map_err(crate::table::failure)?;
    let statement = match kept {
        Some(bytes) => Allowed::decode(bytes.as_slice()).map_err(|_| fail())?,
        None => {
            let host = client::config(&selection.original)
                .map_err(crate::table::failure)?
                .ok_or_else(fail)?
                .host;
            let statement = original
                .device()
                .with_seed(|seed| Allowed::sign(binding, &successor_name(&host), seed))
                .map_err(crate::table::failure)?;
            // Kept before the admission is signed: a crash between the two finds this same
            // key named, and a successor home whose device changed finds another.
            fsx::write_new(&path, statement.bytes(), 0o600).map_err(crate::table::failure)?;
            if fsx::read_private(&path, MAX_STATE)
                .map_err(crate::table::failure)?
                .as_ref()
                .map(|kept| kept.as_slice())
                != Some(statement.bytes())
            {
                return Err(fail());
            }
            aterm_durable::durable_dir(&root(&selection.original))?;
            statement
        }
    };
    if !selection
        .actor
        .starts_with(&aterm_codec::hex::encode_lower(statement.signer()))
        || statement.binding() != binding
    {
        return Err(Error::new(
            "writer succession statement names another signer or successor",
        ));
    }
    budget.check()?;
    let held = client::load_records(&next).map_err(crate::table::failure)?;
    let reader =
        Reader::BoundedLog(client::records_bound(&next, &held).map_err(crate::table::failure)?);
    let succeeding = client::Succeeding {
        request: &request,
        statement: &statement,
        reader,
        lab: device.lab().is_some(),
    };
    let admitted = client::admit_successor(
        &selection.original,
        &succeeding,
        aterm_time::unix_millis().unwrap_or(0),
        deadline,
    )
    .map_err(crate::table::failure)?;
    let bundle = match admitted {
        client::Succession::Let { bundle, .. } => bundle,
        client::Succession::Refused(why) => return Err(Error::new(&why)),
        client::Succession::Busy => return Err(Error::busy().context("writer succession")),
        client::Succession::NotHere => {
            return Err(Error::new(
                "this member admits nobody through the membership log",
            ));
        }
    };
    budget.check()?;
    selection.authorize_until(deadline)?;
    let bundle = Bundle::decode(bundle.as_slice()).map_err(crate::table::failure)?;
    client::readmit_from_log_bundle_until(&next, &bundle, deadline)
        .map_err(crate::table::failure)?;
    Ok(())
}

/// Keep, beside the fence of `paths`, why the statement admitted nothing on this pass, or
/// forget it once it has (`refused` is `None`): what [`status`] reports as `statement_refused`,
/// so `aterm db commons-status` on a Mac that stays pending names the reason (a full record
/// budget, a vault read from a path, a freshness read nobody answered, another key named, a
/// successor home that lost its link).
/// Written only when the reason changes. A reason that cannot be kept changes nothing else:
/// the pass goes on as it would have (to the plain ask, or to its refusal for a lost link).
fn note_statement(paths: &Paths, refused: Option<&Error>) {
    let path = root(paths).join(REFUSED);
    let held = fsx::read_private(&path, MAX_STATE).ok().flatten();
    match refused {
        None => {
            if held.is_some() {
                let _ = std::fs::remove_file(&path);
            }
        }
        Some(error) => {
            let why = aterm_codec::tty::one_line(&error.to_string(), 512);
            if held.as_ref().map(|held| held.as_slice()) != Some(why.as_bytes()) {
                let _ = fsx::write_atomic(&path, why.as_bytes(), 0o600);
            }
        }
    }
}

/// Why the statement of the fence at `paths` admitted nothing on its last pass, if it did not
/// ([`note_statement`]).
fn statement_refused(paths: &Paths) -> Option<String> {
    let held = fsx::read_private(&root(paths).join(REFUSED), MAX_STATE)
        .ok()
        .flatten()?;
    String::from_utf8(held.as_slice().to_vec()).ok()
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
    if existing
        .as_ref()
        .is_some_and(|selection| selection.complete.is_some())
        && !may_have_completed(paths)?
    {
        // A home whose import completed on a release that wrote no mark gets one here, before
        // any check that can refuse this pass: the mark is a fact of the home, not of this
        // owner. The link is atomic and answers "taken" to a second writer, so it needs no lock.
        mark_completed(paths)?;
    }
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
        if !enrolls_itself(&vault) || !matches!(vault.source, Source::Url(_)) {
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
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::busy().context("writer succession")
        } else {
            error.into()
        }
    })?;
    budget.check()?;
    let mut selection = reserve_until(paths, config, &bootstrap, budget.until)?;
    if selection.config != config {
        return Err(fail());
    }
    selection.authorize_until(budget.until)?;
    budget.check()?;
    let next = candidate(paths);
    // The successor home is judged once, before either route to a seat. A kept selection
    // reserved it by its link ([`reserve_until`]), and a home that no longer links to this
    // selection (emptied, restored from a copy, unreadable) holds no successor of this writer.
    // The statement names one successor key for good, so a key made there now is admitted on
    // nothing, and a plain ask under it would be one more identity under this Mac's name at
    // every loss. Neither runs, and the reason is kept for `status`.
    if !matches!(linked(&next), Ok(Some(link)) if link.original == selection.original) {
        let lost = Error::new(UNLINKED);
        note_statement(paths, Some(&lost));
        return Err(lost);
    }
    let original_config = client::config(paths)
        .map_err(crate::table::failure)?
        .ok_or_else(fail)?;
    let configured = client::config(&next).map_err(crate::table::failure)?;
    if let Some(configured) = &configured
        && (configured.name != original_config.name
            || !enrolls_itself(configured)
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
    // The fenced original's signed statement admits its successor with no person. Where it
    // admits nothing on this pass (a vault with no membership log, a busy vault state, a full
    // record budget) the reason is kept for `status` and the successor still asks plainly,
    // which a person's Allow answers.
    let refused = if pending {
        admit_by_statement(&selection, budget).err()
    } else {
        None
    };
    note_statement(paths, refused.as_ref());
    if refused.is_some() {
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
    } else if !pending {
        // The ordinary pass also syncs membership records and tries verified
        // ALab server candidates when the initially discovered URL is gone.
        let _ = client::sync_reader_until(&next, budget.until);
    }
    selection.authorize_until(budget.until)?;
    budget.check()?;
    if let Some(configured) = client::config(&next).map_err(crate::table::failure)?
        && (configured.name != original_config.name
            || !enrolls_itself(&configured)
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

/// Test seams of the generation checks.
#[cfg(test)]
#[path = "generation_seams_tests.rs"]
pub(crate) mod seams;

#[cfg(test)]
pub(crate) mod tests;
