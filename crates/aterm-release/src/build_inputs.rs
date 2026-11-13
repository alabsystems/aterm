// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identity of the universal release's Rust and Apple toolchains and SDK, sealed once per
//! release run.
//!
//! The identity [`current`] returns is a SHA-256 over the seal's schema, the content digest of
//! every input root ([`walk_roots`]: each file read in full) and this machine's facts (its OS
//! build, its model and the SDK's settings). Reading every input's bytes takes seconds, and one
//! release asks for the identity about two dozen times across its processes. So a process's
//! first call takes a seal (the content digest and, from the same walk, a status digest of each
//! root, [`aterm_toolchain::stat_digest`]) and every later call rechecks it by status alone,
//! reading no input file, and hashes again only when the roots, the facts or a status differ. A
//! take whose inputs changed too recently for their status to tell a later write
//! ([`aterm_toolchain::stat_digest::SETTLE_NS`]) is not held, so the next call hashes again. A
//! release arms persistence at its first step ([`arm`]): its seal is kept in the release state,
//! and every child it starts is handed the kept seal's path ([`SEAL_ENV`]), which the child
//! adopts once the same recheck holds.

#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(any(test, target_os = "macos"))]
use std::collections::BTreeSet;
#[cfg(any(test, target_os = "macos"))]
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[cfg(any(test, target_os = "macos"))]
use aterm_toolchain::stat_digest::{SETTLE_NS, StatDigest, StatKey};

/// The content walk's schema, the first field of its digest and part of the identity. Any change
/// to what [`walk`] reads into the content digest, or how it encodes it, ships with a new schema:
/// a seal kept by a cutter of the old walk otherwise still stands for the new one, as nothing
/// else in a kept seal names the walk's code. `the_content_digest_of_a_fixed_tree_is_schema_v2s`
/// holds the walk to it.
#[cfg(any(test, target_os = "macos"))]
const SCHEMA: &[u8] = b"aterm-release-build-inputs-v2";

/// The variable that hands a child its parent's kept seal: the path of the file. A release sets
/// it for each child it starts once its seal is kept ([`handed_path`]), and never passes on one
/// it inherited.
pub const SEAL_ENV: &str = "ATERM_RELEASE_INPUTS_SEAL";

/// The seal's own schema, part of the identity: a seal of another schema is no seal here.
#[cfg(any(test, target_os = "macos"))]
const SEAL_SCHEMA: &str = "aterm-release-inputs-seal-v2";

/// The largest kept seal read: one is about a kilobyte per root.
#[cfg(any(test, target_os = "macos"))]
const MAX_SEAL_BYTES: u64 = 1024 * 1024;

/// This machine's build-input identity: the selected drivers, the Rust and Apple toolchain
/// bin/lib trees and the macOS SDK by content, with the OS build, the model and the SDK's
/// settings. The first call in a process adopts a handed or kept seal whose recheck holds, or
/// takes one; each later call rechecks the seal by status and takes a new one only on a
/// difference. Each call says on stderr, and in the log [`arm`] named, what it did (`inputs:
/// hashed …` or `inputs: rechecked by stat …`, and one `release-event:` line). Linux's native
/// handoff has its own compiler provenance protocol.
/// # Errors
/// A compiler or SDK cannot be resolved or read, an input changed while it was hashed, or this
/// is not a macOS host.
pub fn current() -> Result<String, String> {
    let _timing = crate::cut_timing::Timer::new("inputs seal recheck");
    #[cfg(target_os = "macos")]
    {
        on_this_machine(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("universal release build input identity requires the macOS SDK; native Linux handoffs use their own provenance protocol".into())
    }
}

/// Keep this process's seal under `state_dir` (the release's state) from its next call on, and
/// look for a kept one there: the release's first step arms it, under its toolchain lease. A
/// dry run arms nothing, so it keeps no seal and hands none on. Each later call's report also
/// goes to `log` where one is named: the cutter names its transcript, as its stderr reaches no
/// log, while a child's stderr is its step's log.
pub fn arm(state_dir: &Path, log: Option<&Path>) {
    #[cfg(target_os = "macos")]
    {
        let mut process = sealed();
        process.arm(state_dir.join("inputs-seal"));
        process.log = log.map(Path::to_path_buf);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (state_dir, log);
}

/// Arm the real release state and offer one authenticated pre-freeze seal to this process's
/// first identity call. The call still rechecks every root, fact and status before adoption;
/// a changed offer is ignored and the current inputs are hashed instead.
pub(crate) fn arm_prefrozen(state_dir: &Path, log: Option<&Path>, handed: Option<&Path>) {
    #[cfg(target_os = "macos")]
    {
        let mut process = sealed();
        process.arm(state_dir.join("inputs-seal"));
        // An earlier cheap gate may have taken an unpersisted process-local seal. The exact
        // pre-freeze path must exercise the handed record's own recheck, not merely observe that
        // an independently held identity happens to match it.
        process.seal = None;
        process.saved = None;
        process.begin(handed.map(Path::to_path_buf), None);
        process.log = log.map(Path::to_path_buf);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (state_dir, log, handed);
}

/// The file that holds this process's seal once it is kept, which a child is handed as
/// [`SEAL_ENV`]; `None` before then, when nothing armed or handed persistence, and off macOS.
#[must_use]
pub fn handed_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        sealed().saved.clone()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The kept seal prepared under `state_dir` when it authenticates `expected` for this
/// machine's resolved input roots. The caller may hand this file to another process; that
/// process still rechecks every root, fact and status before it adopts the seal.
#[must_use]
pub(crate) fn prefrozen_seal(state_dir: &Path, expected: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let roots = machine_roots().ok()?;
        matching_kept(state_dir, expected, &roots)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state_dir, expected);
        None
    }
}

/// `inputs [--recheck]`, a measurement: arm the release state's seal, then take a new one, or,
/// with `recheck`, adopt or recheck the kept one (hashing only on a difference); print the
/// identity on stdout.
/// # Errors
/// What [`current`] refuses, and no HOME.
pub fn measure(recheck: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or("HOME is not set")?;
        arm(&crate::release_steps::release_state(Path::new(&home)), None);
        println!("{}", on_this_machine(!recheck)?);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = recheck;
        current().map(drop)
    }
}

#[cfg(target_os = "macos")]
static INPUTS_SEAL: std::sync::Mutex<Process> = std::sync::Mutex::new(Process::unstarted());

#[cfg(target_os = "macos")]
fn sealed() -> std::sync::MutexGuard<'static, Process> {
    INPUTS_SEAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This process's identity from this machine's roots and facts; `fresh` takes a new seal
/// whatever is held or kept.
#[cfg(target_os = "macos")]
fn on_this_machine(fresh: bool) -> Result<String, String> {
    let mut roots = machine_roots;
    let mut facts = machine_facts;
    let mut open = crate::durable::open_regular_file;
    let mut now = now_ns;
    let mut process = sealed();
    let log = process.log.clone();
    let mut say = |line: &str| say_to(log.as_deref(), line);
    process.identity(
        &mut Sources {
            roots: &mut roots,
            facts: &mut facts,
            open: &mut open,
            now: &mut now,
            say: &mut say,
        },
        fresh,
    )
}

/// Say a call's report `line` on stderr and, where [`arm`] named one, in `log`.
#[cfg(any(test, target_os = "macos"))]
fn say_to(log: Option<&Path>, line: &str) {
    use std::io::Write as _;
    eprintln!("{line}");
    if let Some(log) = log
        && let Ok(mut file) = crate::durable::open_append(log, true)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// The wall clock, in nanoseconds since the Unix epoch: the clock a status-change time is
/// stamped from.
#[cfg(target_os = "macos")]
fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX))
}

/// A labelled input root: its label and its path, resolved now.
#[cfg(any(test, target_os = "macos"))]
type Root = (String, PathBuf);

/// Opens an input file to read it for the content digest.
#[cfg(any(test, target_os = "macos"))]
type Open<'a> = dyn FnMut(&Path) -> std::io::Result<std::fs::File> + 'a;

/// Lists an input directory's names for the content digest.
#[cfg(any(test, target_os = "macos"))]
type List<'a> = dyn FnMut(&Path) -> std::io::Result<Vec<std::ffi::OsString>> + 'a;

/// How a take reads the inputs, as seams: each file it hashes, and each directory's names.
#[cfg(any(test, target_os = "macos"))]
struct Reads<'a> {
    open: &'a mut Open<'a>,
    list: &'a mut List<'a>,
}

/// What a seal reads, as seams: the process-wide seal reads this machine, a test a scratch tree.
#[cfg(any(test, target_os = "macos"))]
struct Sources<'a> {
    /// The labelled input roots, resolved now.
    roots: &'a mut dyn FnMut() -> Result<Vec<Root>, String>,
    /// This machine's facts, given those roots.
    facts: &'a mut dyn FnMut(&[Root]) -> Facts,
    /// Opens an input file to hash it.
    open: &'a mut Open<'a>,
    /// The wall clock, in nanoseconds since the Unix epoch ([`SETTLE_NS`]).
    now: &'a mut dyn FnMut() -> i128,
    /// Where each call says what it did.
    say: &'a mut dyn FnMut(&str),
}

/// What the machine adds to the inputs' content: an OS update or another Mac is another
/// identity, though every input's bytes are the same. The Trust build the seal covers (its build
/// number and tree root, which a locked tree's discovery holds to its trust.lock) is one more
/// fact; the Trust roots stay in the status walk, so a store build relinked or repaired mid-run
/// is still caught.
#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Facts {
    /// `kern.osversion` ([`aterm_sysprobe::os_build`]).
    os_build: Option<String>,
    /// `hw.model` ([`aterm_sysprobe::machine_model`]).
    model: Option<String>,
    /// [`sdk_settings`] of the `macos-sdk` root.
    sdk_settings: Option<String>,
    /// [`selected_trust_build`] of the `trust/selected-bin` root: `None` for a toolchain outside the
    /// store. Not a fact [`Facts::complete`] waits on, since such a toolchain has none.
    trust_build: Option<String>,
}

#[cfg(any(test, target_os = "macos"))]
impl Facts {
    /// Whether the OS build and the model were both read. A seal of facts that were not is held
    /// only by the process that took it: kept, `None` would match the `None` of a later process
    /// after an OS update, and the facts would catch nothing.
    const fn complete(&self) -> bool {
        self.os_build.is_some() && self.model.is_some()
    }

    /// The facts that differ between `self` and `now`, named.
    fn changed(&self, now: &Self) -> String {
        fn show(v: &Option<String>) -> &str {
            v.as_deref().unwrap_or("unread")
        }
        [
            ("OS build", &self.os_build, &now.os_build),
            ("model", &self.model, &now.model),
            ("SDK settings", &self.sdk_settings, &now.sdk_settings),
            ("Trust build", &self.trust_build, &now.trust_build),
        ]
        .into_iter()
        .filter(|(_, was, is)| was != is)
        .map(|(what, was, is)| format!("{what} ({} → {})", show(was), show(is)))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// This machine's [`Facts`] for `roots`.
#[cfg(target_os = "macos")]
fn machine_facts(roots: &[Root]) -> Facts {
    Facts {
        os_build: aterm_sysprobe::os_build(),
        model: aterm_sysprobe::machine_model(),
        sdk_settings: roots
            .iter()
            .find(|(label, _)| label == "macos-sdk")
            .map(|(_, sdk)| sdk_settings(sdk)),
        trust_build: roots
            .iter()
            .find(|(label, _)| label == TRUST_SELECTED)
            .and_then(|(_, bin)| {
                let home = aterm_types::dirs::home_dir();
                let configured = atpkg::config::load().prefix_path(home.as_deref());
                let layout = atpkg::store::resolve(configured.as_deref())?;
                selected_trust_build(&layout.prefix, bin)
            }),
    }
}

/// The `machine_roots` label of the Trust toolchain the process selected.
#[cfg(target_os = "macos")]
const TRUST_SELECTED: &str = "trust/selected-bin";

/// `<build> <tree root>` of the store's Trust build whose `bin` is `selected` (canonical, as
/// [`crate::gates::trust_stage2_bin`] gives it), the root as recorded for the tree there now, or
/// `<build> unrecorded` with no such record; `None` when `selected` is not a Trust build in the
/// store at `prefix` (a dev-linked checkout, an explicit `TRUST_STAGE2_BIN` elsewhere).
#[cfg(target_os = "macos")]
fn selected_trust_build(prefix: &Path, selected: &Path) -> Option<String> {
    let prefix = prefix.canonicalize().ok()?;
    let (program, build) = atpkg::ops::store_build_of(&prefix, selected)?;
    let build_dir = selected.parent()?;
    if program != atpkg::trust_lock::PROGRAM
        || selected.file_name()? != "bin"
        || build_dir.parent()? != prefix.join("store").join(&program)
    {
        return None;
    }
    let root = atpkg::trust_lock::recorded_root_at(build_dir);
    Some(format!(
        "{build} {}",
        root.as_deref().unwrap_or("unrecorded")
    ))
}

/// SHA-256 of the SDK's `SDKSettings.json` and `SDKSettings.plist`, each by its bytes, or by why
/// it could not be read.
#[cfg(target_os = "macos")]
fn sdk_settings(sdk: &Path) -> String {
    let mut digest = aterm_digest::Sha256::new();
    for name in ["SDKSettings.json", "SDKSettings.plist"] {
        field(&mut digest, name.as_bytes());
        match crate::durable::read_regular_bytes(&sdk.join(name), MAX_SEAL_BYTES * 4) {
            Ok(bytes) => {
                field(&mut digest, b"bytes");
                field(&mut digest, &aterm_digest::Sha256::digest(&bytes));
            }
            Err(error) => {
                field(&mut digest, b"unread");
                field(&mut digest, error.kind().to_string().as_bytes());
            }
        }
    }
    aterm_codec::hex::encode_lower(&digest.finalize())
}

/// One take of the inputs, and what its recheck compares.
#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Seal {
    /// [`SEAL_SCHEMA`].
    schema: String,
    /// What [`current`] returns: [`identity_of`] the digest and the facts.
    identity: String,
    /// The content digest of the roots (schema [`SCHEMA`]).
    digest: String,
    /// Every entry the walk visited, across the roots.
    entries: u64,
    /// When it was taken, in seconds since the Unix epoch, and by which process: for a reader.
    taken_at: u64,
    taken_by: u32,
    facts: Facts,
    /// Each root as resolved, in label order, with its status digest.
    roots: Vec<SealedRoot>,
}

/// One root of a [`Seal`].
#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SealedRoot {
    label: String,
    path: PathBuf,
    /// Its [`StatDigest`], lowercase hex.
    stat: String,
    entries: u64,
}

/// What a recheck found.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, PartialEq, Eq)]
enum Rechecked {
    /// Every root, fact and status is the seal's: this many entries were stat-ed.
    Held(u64),
    /// Why the seal no longer stands.
    Changed(String),
}

#[cfg(any(test, target_os = "macos"))]
impl Seal {
    /// Hash `roots` and read the facts: the seal, and whether its inputs had settled when the take
    /// began, every status it recorded having last changed at least [`SETTLE_NS`] before. On a
    /// coarse clock (HFS+ keeps whole seconds) a later write in the same tick as a more recent
    /// change leaves that status as it was, so a recheck of such a seal could hold over bytes the
    /// take never read.
    fn hashed(roots: &[Root], src: &mut Sources<'_>) -> Result<(Self, bool), String> {
        let facts = (src.facts)(roots);
        let began = (src.now)();
        let walked = walk_roots(
            &borrowed(roots),
            Some(Reads {
                open: &mut *src.open,
                list: &mut list_names,
            }),
        )?;
        let settled = walked
            .newest_change_ns
            .is_none_or(|newest| newest.saturating_add(SETTLE_NS) <= began);
        let digest = walked
            .digest
            .ok_or("the build input walk read no content")?;
        let mut sorted = roots.to_vec();
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let seal = Self {
            schema: SEAL_SCHEMA.into(),
            identity: identity_of(&digest, &facts),
            digest,
            entries: walked.roots.iter().map(|root| root.entries).sum(),
            taken_at: u64::try_from(began.div_euclid(1_000_000_000)).unwrap_or(0),
            taken_by: std::process::id(),
            facts,
            roots: sorted
                .into_iter()
                .zip(walked.roots)
                .map(|((label, path), walked)| SealedRoot {
                    label,
                    path,
                    stat: walked.stat,
                    entries: walked.entries,
                })
                .collect(),
        };
        Ok((seal, settled))
    }

    /// Whether this seal still stands for `roots` on this machine, by the roots' labels and paths,
    /// the facts, and each root's status digest; no input file is read.
    fn recheck_by_stat(&self, roots: &[Root], src: &mut Sources<'_>) -> Rechecked {
        let mut sorted: Vec<_> = roots.iter().collect();
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        if self.roots.len() != sorted.len()
            || self
                .roots
                .iter()
                .zip(&sorted)
                .any(|(held, (label, path))| held.label != *label || held.path != *path)
        {
            return Rechecked::Changed(
                "the build inputs resolve to other roots than were hashed".into(),
            );
        }
        let facts = (src.facts)(roots);
        if facts != self.facts {
            return Rechecked::Changed(format!(
                "this machine's {} changed since the inputs were hashed",
                self.facts.changed(&facts)
            ));
        }
        let walked = match walk_roots(&borrowed(roots), None) {
            Ok(walked) => walked,
            Err(why) => return Rechecked::Changed(why),
        };
        match self
            .roots
            .iter()
            .zip(&walked.roots)
            .find(|(held, now)| held.stat != now.stat || held.entries != now.entries)
        {
            Some((held, _)) => Rechecked::Changed(format!(
                "{} changed by stat since it was hashed",
                held.label
            )),
            None => Rechecked::Held(self.entries),
        }
    }
}

/// The identity of a content digest on a machine with `facts`, under this walk's [`SCHEMA`].
#[cfg(any(test, target_os = "macos"))]
fn identity_of(digest: &str, facts: &Facts) -> String {
    identity_under(SCHEMA, digest, facts)
}

/// The identity of a content digest that the walk of content schema `schema` produced, on a
/// machine with `facts`. Binding the schema is what makes a kept seal of another walk no seal
/// here ([`load`]).
#[cfg(any(test, target_os = "macos"))]
fn identity_under(schema: &[u8], digest: &str, facts: &Facts) -> String {
    let mut sha = aterm_digest::Sha256::new();
    field(&mut sha, SEAL_SCHEMA.as_bytes());
    field(&mut sha, schema);
    field(&mut sha, digest.as_bytes());
    for fact in [
        &facts.os_build,
        &facts.model,
        &facts.sdk_settings,
        &facts.trust_build,
    ] {
        match fact {
            Some(value) => {
                field(&mut sha, b"some");
                field(&mut sha, value.as_bytes());
            }
            None => field(&mut sha, b"none"),
        }
    }
    aterm_codec::hex::encode_lower(&sha.finalize())
}

/// The name a seal of `roots` is kept under: one file per resolved root set, so clones whose
/// toolchains differ keep theirs side by side.
#[cfg(any(test, target_os = "macos"))]
fn kept_name(roots: &[Root]) -> String {
    let mut sorted: Vec<_> = roots.iter().collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut sha = aterm_digest::Sha256::new();
    for (label, path) in sorted {
        field(&mut sha, label.as_bytes());
        field(&mut sha, path.as_os_str().as_encoded_bytes());
    }
    format!("{}.toml", aterm_codec::hex::encode_lower(&sha.finalize()))
}

/// Whether `dir` is a real directory only its owner can read or write.
#[cfg(any(test, target_os = "macos"))]
fn private_dir(dir: &Path) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(dir).map_err(|e| format!("inspect {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.is_dir() && meta.permissions().mode() & 0o077 == 0 {
            return Ok(());
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    Err(format!("{} is not a private directory", dir.display()))
}

/// The seal kept at `path`, when it is one: a regular file only its owner can read or write, in
/// a private directory, of this seal schema, whose identity is its digest's and facts' under this
/// walk's [`SCHEMA`], with complete facts ([`Facts::complete`]). Anything else (missing,
/// unreadable, another schema or walk, facts unread, edited) is no seal.
#[cfg(any(test, target_os = "macos"))]
fn load(path: &Path) -> Option<Seal> {
    private_dir(path.parent()?).ok()?;
    let (bytes, meta) = crate::durable::read_regular_bytes_with_meta(path, MAX_SEAL_BYTES).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            return None;
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let seal: Seal = aterm_toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
    (seal.schema == SEAL_SCHEMA
        && seal.facts.complete()
        && seal.identity == identity_of(&seal.digest, &seal.facts))
    .then_some(seal)
}

/// Locate one internally valid kept seal for the resolved root set. This deliberately does
/// not recheck the roots: the receiving process does that through [`Process::identity`] before
/// it adopts or persists the seal.
#[cfg(any(test, target_os = "macos"))]
fn matching_kept(state_dir: &Path, expected: &str, roots: &[Root]) -> Option<PathBuf> {
    if !aterm_codec::hex::is_lower(expected, 64) {
        return None;
    }
    let path = state_dir.join("inputs-seal").join(kept_name(roots));
    load(&path)
        .is_some_and(|seal| seal.identity == expected)
        .then_some(path)
}

/// Keep `seal` at `path`: written whole (0600) in a private directory, or not at all.
#[cfg(any(test, target_os = "macos"))]
fn save(seal: &Seal, path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("a kept seal needs a directory")?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("create {}: {e}", dir.display())),
    }
    private_dir(dir)?;
    let text = aterm_toml::to_string(seal).map_err(|e| format!("serialize the seal: {e}"))?;
    crate::durable::write_atomic_durable(path, text.as_bytes(), 0o600)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// What a call did, as its counting lines say it.
#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy)]
enum Done {
    Hashed,
    Rechecked,
    Adopted,
}

#[cfg(any(test, target_os = "macos"))]
fn report(src: &mut Sources<'_>, done: Done, entries: u64, secs: f64, adopted: Option<&Path>) {
    let (mode, line) = match done {
        Done::Hashed => (
            "hashed",
            format!("inputs: hashed {entries} entries in {secs:.1} s"),
        ),
        Done::Rechecked | Done::Adopted => (
            if matches!(done, Done::Adopted) {
                "adopted"
            } else {
                "rechecked"
            },
            format!(
                "inputs: rechecked by stat {entries} entries in {:.0} ms{}",
                secs * 1000.0,
                adopted.map_or_else(String::new, |p| format!(" (adopted {})", p.display()))
            ),
        ),
    };
    (src.say)(&line);
    (src.say)(&format!(
        "release-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "inputs", "mode": mode, "entries": entries, "secs": secs,
        })
    ));
}

/// One process's seal, and where it keeps and finds one.
#[cfg(any(test, target_os = "macos"))]
struct Process {
    /// The seal this process holds, once taken or adopted.
    seal: Option<Seal>,
    /// The seal this process was handed ([`SEAL_ENV`]).
    handed: Option<PathBuf>,
    /// The directory it keeps its seal in: [`arm`]'s, else the handed seal's.
    keeps: Option<PathBuf>,
    /// Where else it finds a kept seal: the release state's directory.
    finds: Option<PathBuf>,
    /// The file in `keeps` that holds `seal`.
    saved: Option<PathBuf>,
    /// The log each call's report also goes to ([`arm`]'s).
    #[cfg(target_os = "macos")]
    log: Option<PathBuf>,
    /// Whether the environment has been read.
    started: bool,
}

#[cfg(any(test, target_os = "macos"))]
impl Process {
    const fn unstarted() -> Self {
        Self {
            seal: None,
            handed: None,
            keeps: None,
            finds: None,
            saved: None,
            #[cfg(target_os = "macos")]
            log: None,
            started: false,
        }
    }

    /// Read the environment, once: the handed seal, and the release state's directory.
    #[cfg(target_os = "macos")]
    fn start(&mut self) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let handed = std::env::var_os(SEAL_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        let finds = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| crate::release_steps::release_state(Path::new(&home)).join("inputs-seal"));
        self.begin(handed, finds);
    }

    #[cfg(not(target_os = "macos"))]
    fn start(&mut self) {
        self.started = true;
    }

    /// Start with `handed` and `finds`: a handed seal's directory is where this process keeps its
    /// own, unless [`arm`] named one.
    fn begin(&mut self, handed: Option<PathBuf>, finds: Option<PathBuf>) {
        self.started = true;
        if self.keeps.is_none() {
            self.keeps = handed
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
        }
        self.handed = handed;
        self.finds = finds;
    }

    fn arm(&mut self, dir: PathBuf) {
        if self.keeps.as_ref() != Some(&dir) {
            self.saved = None;
        }
        self.keeps = Some(dir);
    }

    /// The identity: the held seal's when its recheck holds, else a kept one's whose recheck
    /// holds (the handed one first), else a new seal's; `fresh` takes a new one at once. A new
    /// seal is held only once its inputs have settled ([`Seal::hashed`]), and a held or adopted
    /// one is kept where this process keeps one ([`Process::keep`]).
    fn identity(&mut self, src: &mut Sources<'_>, fresh: bool) -> Result<String, String> {
        if !self.started {
            self.start();
        }
        let begun = std::time::Instant::now();
        let roots = (src.roots)()?;
        let name = kept_name(&roots);
        if !fresh {
            let mut changed = None;
            if let Some(seal) = &self.seal {
                match seal.recheck_by_stat(&roots, src) {
                    Rechecked::Held(entries) => {
                        let identity = seal.identity.clone();
                        report(
                            src,
                            Done::Rechecked,
                            entries,
                            begun.elapsed().as_secs_f64(),
                            None,
                        );
                        self.keep(&name, src);
                        return Ok(identity);
                    }
                    Rechecked::Changed(why) => changed = Some(why),
                }
            }
            for path in self.kept_paths(&name) {
                let Some(found) = load(&path) else {
                    continue;
                };
                if self.seal.as_ref() == Some(&found) {
                    continue;
                }
                match found.recheck_by_stat(&roots, src) {
                    Rechecked::Held(entries) => {
                        report(
                            src,
                            Done::Adopted,
                            entries,
                            begun.elapsed().as_secs_f64(),
                            Some(&path),
                        );
                        let identity = found.identity.clone();
                        self.saved = self
                            .keeps
                            .as_ref()
                            .is_some_and(|dir| path == dir.join(&name))
                            .then_some(path);
                        self.seal = Some(found);
                        self.keep(&name, src);
                        return Ok(identity);
                    }
                    Rechecked::Changed(why) => {
                        changed.get_or_insert(why);
                    }
                }
            }
            if let Some(why) = changed {
                (src.say)(&format!("inputs: {why}; hashing them again"));
            }
        }
        let (seal, settled) = Seal::hashed(&roots, src)?;
        report(
            src,
            Done::Hashed,
            seal.entries,
            begun.elapsed().as_secs_f64(),
            None,
        );
        let identity = seal.identity.clone();
        self.saved = None;
        if settled {
            self.seal = Some(seal);
            self.keep(&name, src);
        } else {
            self.seal = None;
            (src.say)(
                "inputs: an input's status changed too recently to tell a later write by it, so \
                 this seal is neither held nor kept and the next call hashes again",
            );
        }
        Ok(identity)
    }

    /// Where a kept seal may be, the handed one first.
    fn kept_paths(&self, name: &str) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.handed.iter().cloned().collect();
        for dir in [&self.keeps, &self.finds].into_iter().flatten() {
            let path = dir.join(name);
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }

    /// Write the held seal where this process keeps one, unless that file holds it already or its
    /// facts are incomplete ([`Facts::complete`]). A seal not kept still stands in this process;
    /// each child then hashes for itself.
    fn keep(&mut self, name: &str, src: &mut Sources<'_>) {
        let (Some(dir), Some(seal)) = (&self.keeps, &self.seal) else {
            return;
        };
        let path = dir.join(name);
        if self.saved.as_ref() == Some(&path) {
            return;
        }
        if !seal.facts.complete() {
            (src.say)(
                "inputs: the seal was not kept, so each child hashes for itself: this machine's \
                 OS build or model could not be read",
            );
            return;
        }
        match save(seal, &path) {
            Ok(()) => self.saved = Some(path),
            Err(why) => (src.say)(&format!(
                "inputs: the seal was not kept, so each child hashes for itself: {why}"
            )),
        }
    }
}

/// The resolved input roots of this machine's universal build, labelled.
#[cfg(target_os = "macos")]
fn machine_roots() -> Result<Vec<Root>, String> {
    use aterm_uds::spawnfd::command;

    let trust = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let trust_root = output_path(
        command(trust.join("trustc")).args(["--print", "sysroot"]),
        "Trust sysroot",
    )?;
    let home = std::env::var_os("HOME").ok_or("compiler identity requires HOME")?;
    let shim = crate::buildplan::release_rustup_shim_dir()?;
    let rustup_home = atpkg::seam::rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        Some(Path::new(&home)),
    )
    .filter(|path| path.is_absolute())
    .ok_or("compiler identity requires an absolute RUSTUP_HOME")?;
    let path = std::env::join_paths([
        shim.as_path(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .map_err(|e| format!("compiler identity PATH: {e}"))?;
    let stable_root = output_path(
        // STOCK EXCEPTION: the universal release's x86_64 slice uses upstream stable.
        command(shim.join("rustc"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", path)
            .env("RUSTUP_HOME", rustup_home)
            .env("RUSTUP_TOOLCHAIN", "stable")
            .args(["--print", "sysroot"])
            .current_dir("/"),
        "stable sysroot",
    )?;
    let trust_host = output_host(command(trust.join("trustc")).arg("-vV"), "Trust host")?;
    let stable_host = output_host(
        command(stable_root.join("bin/rustc")).arg("-vV"),
        "stable host",
    )?;
    let sdk = output_path(
        command("/usr/bin/xcrun")
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .current_dir("/"),
        "macOS SDK",
    )?;
    let mut roots = vec![
        (TRUST_SELECTED.to_string(), trust),
        ("trust/bin".to_string(), trust_root.join("bin")),
        ("stable/proxies".to_string(), shim),
        ("stable/bin".to_string(), stable_root.join("bin")),
        ("macos-sdk".to_string(), sdk),
    ];
    roots.extend(rust_library_roots("trust", &trust_root, &[&trust_host])?);
    roots.extend(rust_library_roots(
        "stable",
        &stable_root,
        &[&stable_host, "x86_64-apple-darwin"],
    )?);
    // Build scripts and Rust's linker reach these system dispatchers through
    // the scrubbed PATH; their selected toolchain alone does not identify them.
    for tool in [
        "xcrun",
        "cc",
        "c++",
        "clang",
        "ld",
        "as",
        "ar",
        "ranlib",
        "libtool",
        "dsymutil",
        "strip",
        "lipo",
        "ditto",
        "dwarfdump",
    ] {
        roots.push((
            format!("macos-driver/{tool}"),
            Path::new("/usr/bin").join(tool),
        ));
    }
    let mut apple_roots: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for tool in ["clang", "ld", "dsymutil", "strip", "lipo"] {
        let executable = output_path(
            command("/usr/bin/xcrun")
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .args(["--sdk", "macosx", "--find", tool])
                .current_dir("/"),
            &format!("Apple {tool}"),
        )?
        .canonicalize()
        .map_err(|e| format!("resolve Apple {tool}: {e}"))?;
        let bin = executable
            .parent()
            .ok_or("Apple tool has no bin directory")?;
        let prefix = bin
            .parent()
            .ok_or("Apple tool has no toolchain directory")?;
        for (kind, path) in [("bin", bin.to_path_buf()), ("lib", prefix.join("lib"))] {
            let path = path
                .canonicalize()
                .map_err(|e| format!("resolve Apple {tool} {kind}: {e}"))?;
            apple_roots
                .entry(path)
                .or_default()
                .push(format!("{tool}/{kind}"));
        }
    }
    // The usual five tools share one tree. Hash that tree once, retaining the
    // selected-tool associations without incorporating an installation path.
    for (root, mut names) in apple_roots {
        names.sort_unstable();
        roots.push((format!("apple/{}", names.join("+")), root));
    }
    Ok(roots)
}

/// Compiler runtime libraries and the target libraries this build can use.
/// rustup's manifests and unrelated installed targets are installation state,
/// not inputs to either slice of the universal application.
#[cfg(any(test, target_os = "macos"))]
fn rust_library_roots(label: &str, root: &Path, targets: &[&str]) -> Result<Vec<Root>, String> {
    if targets.is_empty() {
        return Err(format!("{label} has no selected Rust target"));
    }
    let lib = root.join("lib");
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(&lib).map_err(|e| format!("read {}: {e}", lib.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", lib.display()))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("Rust library name is not UTF-8")?;
        if name != "rustlib" {
            roots.push((format!("{label}/lib/{name}"), entry.path()));
        }
    }
    for target in targets.iter().copied().collect::<BTreeSet<_>>() {
        if !valid_rust_target(target) {
            return Err(format!("invalid {label} Rust target {target:?}"));
        }
        let path = lib.join("rustlib").join(target);
        if !path.is_dir() {
            return Err(format!(
                "selected {label} Rust target is missing: {}",
                path.display()
            ));
        }
        roots.push((format!("{label}/rustlib/{target}"), path));
    }
    roots.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(roots)
}

#[cfg(any(test, target_os = "macos"))]
fn valid_rust_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= 128
        && target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(target_os = "macos")]
fn output_host(command: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|e| format!("resolve {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "resolve {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|e| format!("resolve {what}: {e}"))?;
    host_from_text(text).map_err(|why| format!("{what}: {why}"))
}

#[cfg(any(test, target_os = "macos"))]
fn host_from_text(text: &str) -> Result<String, String> {
    let mut hosts = text.lines().filter_map(|line| line.strip_prefix("host: "));
    match (hosts.next(), hosts.next()) {
        (Some(host), None) if valid_rust_target(host) => Ok(host.to_owned()),
        _ => Err("compiler did not name one valid host target".into()),
    }
}

#[cfg(target_os = "macos")]
fn output_path(command: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<PathBuf, String> {
    let output = command
        .output()
        .map_err(|e| format!("resolve {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "resolve {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|e| format!("resolve {what}: {e}"))?;
    let path = PathBuf::from(text.trim());
    if !path.is_absolute() {
        return Err(format!("{what} did not name an absolute path"));
    }
    Ok(path)
}

/// The most entries one identity walks, across all its roots. Links that alias directories make
/// the walk visit some trees more than once (the SDK's frameworks: about 69,000 visits for its
/// 32,000 files on macOS 26.4), and links that lead between sibling directories can multiply
/// it without end; past this, the identity refuses rather than run on.
#[cfg(any(test, target_os = "macos"))]
const MAX_ENTRIES: usize = 2_000_000;

/// Hash labeled roots by their logical names and content, independent of absolute placement
/// (but for the target text an absolute link below contributes). Files, directories, modes and
/// resolved symlink contents contribute; a symlink beneath a root that dangles (the Command
/// Line Tools' `usr/bin/crashlog`), or that resolves to a directory the walk is inside (the
/// SDK's `Ruby.framework/…/Headers/ruby/ruby` is `.`), contributes its target text instead, as
/// it has no content of its own to add; timestamps do not. Root labels must be unique.
#[cfg(test)]
fn digest_roots(roots: &[(&str, &Path)]) -> Result<String, String> {
    digest_roots_with(roots, &mut crate::durable::open_regular_file)
}

#[cfg(test)]
fn digest_roots_with(roots: &[(&str, &Path)], open: &mut Open<'_>) -> Result<String, String> {
    walk_roots(
        roots,
        Some(Reads {
            open,
            list: &mut list_names,
        }),
    )?
    .digest
    .ok_or_else(|| "the build input walk read no content".into())
}

/// `dir`'s names, as the walk lists them.
#[cfg(any(test, target_os = "macos"))]
fn list_names(dir: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// `roots` as the walk takes them.
#[cfg(any(test, target_os = "macos"))]
fn borrowed(roots: &[Root]) -> Vec<(&str, &Path)> {
    roots
        .iter()
        .map(|(label, path)| (label.as_str(), path.as_path()))
        .collect()
}

/// What one walk of the roots found.
#[cfg(any(test, target_os = "macos"))]
struct Walked {
    /// The content digest, when the walk read the files.
    digest: Option<String>,
    /// Each root's status digest and entries, in label order.
    roots: Vec<RootStat>,
    /// The newest status-change time recorded across the roots ([`StatDigest::newest_change_ns`]).
    newest_change_ns: Option<i128>,
}

#[cfg(any(test, target_os = "macos"))]
struct RootStat {
    /// Its [`StatDigest`], lowercase hex.
    stat: String,
    entries: u64,
}

/// Walk labeled roots once: always each root's status digest ([`StatDigest`]: each entry's own
/// status, a link's target text and what it resolves to, a directory's names), and, when `reads`
/// is given, the content digest over them all (their logical names and content, independent of
/// absolute placement but for the target text an absolute link below contributes). Files,
/// directories, modes and resolved symlink contents contribute to the content; a symlink beneath
/// a root that dangles (the Command Line Tools' `usr/bin/crashlog`), or that resolves to a
/// directory the walk is inside (the SDK's `Ruby.framework/…/Headers/ruby/ruby` is `.`),
/// contributes its target text instead, as it has no content of its own to add; timestamps do
/// not. A file is held to the status taken before it is read, by its descriptor's status after
/// the read, and a directory's listing to its status before the listing; without `reads` no file
/// is opened. Root labels must be unique.
/// # Errors
/// There are no roots; a label is empty or repeated; a root is missing or dangles; a root or
/// descendant cannot be resolved (a link loop included), inspected or read, or is a special
/// file; nesting passes 256 levels or the walk passes [`MAX_ENTRIES`] entries; a real
/// directory is reached again inside itself, which only a link above it (to the root's parent,
/// say) can do; a file or directory changed while its content was read; or the walk
/// was interrupted.
#[cfg(any(test, target_os = "macos"))]
fn walk_roots(roots: &[(&str, &Path)], reads: Option<Reads<'_>>) -> Result<Walked, String> {
    walk_roots_until(roots, reads, &|| crate::interrupt::caught().is_some())
}

#[cfg(any(test, target_os = "macos"))]
fn walk_roots_until(
    roots: &[(&str, &Path)],
    reads: Option<Reads<'_>>,
    stopped: &dyn Fn() -> bool,
) -> Result<Walked, String> {
    if roots.is_empty() {
        return Err("build input identity has no roots".into());
    }
    let mut roots = roots.to_vec();
    roots.sort_unstable_by_key(|(label, _)| *label);
    let mut content = reads.map(|reads| {
        let mut digest = aterm_digest::Sha256::new();
        field(&mut digest, SCHEMA);
        (digest, reads, Box::new([0u8; 64 * 1024]))
    });
    let mut newest_change_ns = None;
    let mut previous = None;
    let mut visits = 0;
    let mut stats = Vec::with_capacity(roots.len());
    let mut relative = Vec::new();
    let mut ancestors = BTreeSet::new();
    for (label, root) in roots {
        if label.is_empty() || previous == Some(label) {
            return Err(format!(
                "build input root label is empty or duplicated: {label:?}"
            ));
        }
        previous = Some(label);
        if let Some((digest, _, _)) = &mut content {
            field(digest, label.as_bytes());
        }
        // Resolve the root itself before walking, so a root alias has the same identity.
        let root = root
            .canonicalize()
            .map_err(|e| format!("resolve build input {}: {e}", root.display()))?;
        let mut sink = Sink {
            stopped,
            content: content
                .as_mut()
                .map(|(digest, reads, buf)| (digest, reads, &mut **buf)),
            stat: StatDigest::new(label.as_bytes()),
        };
        relative.clear();
        ancestors.clear();
        walk(
            &mut sink,
            &root,
            &mut relative,
            &mut ancestors,
            0,
            &mut visits,
        )?;
        newest_change_ns = newest_change_ns.max(sink.stat.newest_change_ns());
        let (stat, entries) = sink.stat.finish();
        stats.push(RootStat {
            stat: aterm_codec::hex::encode_lower(&stat),
            entries,
        });
    }
    Ok(Walked {
        digest: content.map(|(digest, _, _)| aterm_codec::hex::encode_lower(&digest.finalize())),
        roots: stats,
        newest_change_ns,
    })
}

#[cfg(any(test, target_os = "macos"))]
fn field(digest: &mut aterm_digest::Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// What one root's walk feeds: the content digest with the reads that feed it (`None` in a
/// recheck, which reads no file), and the root's status digest.
#[cfg(any(test, target_os = "macos"))]
struct Sink<'a, 'r> {
    stopped: &'a dyn Fn() -> bool,
    content: Option<(
        &'a mut aterm_digest::Sha256,
        &'a mut Reads<'r>,
        &'a mut [u8; 64 * 1024],
    )>,
    stat: StatDigest,
}

#[cfg(any(test, target_os = "macos"))]
impl Sink<'_, '_> {
    fn field(&mut self, bytes: &[u8]) {
        if let Some((digest, _, _)) = &mut self.content {
            field(digest, bytes);
        }
    }
}

#[cfg(any(test, target_os = "macos"))]
fn changed_while_hashed(path: &Path) -> String {
    format!(
        "build input changed while it was hashed: {}",
        path.display()
    )
}

#[cfg(any(test, target_os = "macos"))]
fn walk(
    sink: &mut Sink<'_, '_>,
    path: &Path,
    relative: &mut Vec<u8>,
    ancestors: &mut BTreeSet<PathBuf>,
    depth: usize,
    visits: &mut usize,
) -> Result<(), String> {
    if (sink.stopped)() {
        return Err("build input walk interrupted".into());
    }
    if depth > 256 {
        return Err(format!(
            "build input nesting exceeds 256 levels: {}",
            path.display()
        ));
    }
    *visits += 1;
    if *visits > MAX_ENTRIES {
        return Err(format!(
            "build input walk passes {MAX_ENTRIES} entries at {}: links multiply the tree",
            path.display()
        ));
    }
    let link = std::fs::symlink_metadata(path)
        .map_err(|e| format!("inspect build input {}: {e}", path.display()))?;
    let is_symlink = link.file_type().is_symlink();
    sink.field(relative);
    sink.field(if is_symlink { b"link" } else { b"entry" });
    sink.stat.entry(relative, &StatKey::of(&link));
    let mut target = None;
    let (resolved, metadata) = if is_symlink {
        let text = std::fs::read_link(path)
            .map_err(|e| format!("read build input link {}: {e}", path.display()))?;
        let resolved = match path.canonicalize() {
            Ok(resolved) => resolved,
            // A dangling link has no content: its target text is all of it, and that text is read
            // in full, so it is evidence rather than an absence (its target appearing changes the
            // identity). Apple's Command Line Tools ship one in usr/bin (`crashlog`, into an LLDB
            // framework they do not install). Any other failure to resolve still refuses.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                sink.field(b"dangling");
                sink.field(text.as_os_str().as_encoded_bytes());
                sink.stat.link(text.as_os_str().as_encoded_bytes(), None);
                return Ok(());
            }
            Err(e) => return Err(format!("resolve build input {}: {e}", path.display())),
        };
        let metadata = std::fs::metadata(&resolved)
            .map_err(|e| format!("inspect build input {}: {e}", resolved.display()))?;
        sink.stat.link(
            text.as_os_str().as_encoded_bytes(),
            Some(&StatKey::of(&metadata)),
        );
        target = Some(text);
        (std::borrow::Cow::Owned(resolved), metadata)
    } else {
        (std::borrow::Cow::Borrowed(path), link)
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        sink.field(&(metadata.permissions().mode() & 0o7777).to_le_bytes());
    }
    #[cfg(not(unix))]
    sink.field(&[u8::from(metadata.permissions().readonly())]);
    let before = StatKey::of(&metadata);
    if metadata.is_dir() {
        sink.field(b"directory");
        // A link that resolves to a directory the walk is inside adds only its target text:
        // that directory's content is this walk's already, and following it would never end.
        // A real directory reached again inside itself (after a link above it) still refuses.
        if let Some(target) = &target
            && ancestors.contains(resolved.as_ref())
        {
            sink.field(b"ancestor");
            sink.field(target.as_os_str().as_encoded_bytes());
            sink.stat.ancestor();
            return Ok(());
        }
        let mut resolved_buf = resolved.into_owned();
        if !ancestors.insert(resolved_buf.clone()) {
            return Err(format!("build input directory cycle: {}", path.display()));
        }
        let mut names = match &mut sink.content {
            Some((_, reads, _)) => (reads.list)(&resolved_buf),
            None => list_names(&resolved_buf),
        }
        .map_err(|e| format!("read build input {}: {e}", resolved_buf.display()))?;
        names.sort_unstable_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
        if sink.content.is_some() {
            let after = std::fs::metadata(&resolved_buf)
                .map_err(|e| format!("inspect build input {}: {e}", resolved_buf.display()))?;
            if !before.unchanged(&StatKey::of(&after)) {
                return Err(changed_while_hashed(path));
            }
        }
        sink.stat
            .names(names.iter().map(|name| name.as_encoded_bytes()));
        let prefix_len = relative.len();
        for name in &names {
            if prefix_len > 0 {
                relative.push(b'/');
            }
            relative.extend_from_slice(name.as_encoded_bytes());
            resolved_buf.push(name);
            let step = walk(sink, &resolved_buf, relative, ancestors, depth + 1, visits);
            resolved_buf.pop();
            step?;
            relative.truncate(prefix_len);
        }
        ancestors.remove(&resolved_buf);
    } else if metadata.is_file() {
        sink.field(b"file");
        if let Some((digest, reads, buffer)) = &mut sink.content {
            let mut file = (reads.open)(&resolved)
                .map_err(|e| format!("read build input {}: {e}", resolved.display()))?;
            let mut content = aterm_digest::Sha256::new();
            loop {
                if (sink.stopped)() {
                    return Err("build input hash interrupted".into());
                }
                let read = file
                    .read(&mut **buffer)
                    .map_err(|e| format!("read build input {}: {e}", resolved.display()))?;
                if read == 0 {
                    break;
                }
                content.update(&buffer[..read]);
            }
            let after = file
                .metadata()
                .map_err(|e| format!("inspect build input {}: {e}", resolved.display()))?;
            if !before.unchanged(&StatKey::of(&after)) {
                return Err(changed_while_hashed(path));
            }
            field(digest, &content.finalize());
        }
    } else {
        return Err(format!("unsupported build input: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "build_inputs_tests.rs"]
mod tests;
