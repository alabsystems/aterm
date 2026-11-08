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
/// say) can do; or, reading content, a file or directory changed while it was read.
#[cfg(any(test, target_os = "macos"))]
fn walk_roots(roots: &[(&str, &Path)], reads: Option<Reads<'_>>) -> Result<Walked, String> {
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
mod tests {
    use super::*;

    /// A call's report reaches the log [`arm`] named (the cutter's transcript, which `release
    /// status` counts its `inputs` events from) line by line, and no file where none was named.
    /// NEGATIVE CONTROL: without the append the transcript holds none of it.
    #[test]
    fn a_report_reaches_the_log_the_release_armed() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let log = dir.path().join(crate::release_timeline::TRANSCRIPT);
        let event = "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"adopted\",\
                     \"entries\":9,\"secs\":0.5}";
        say_to(Some(&log), "inputs: rechecked by stat 9 entries in 500 ms");
        say_to(Some(&log), event);
        say_to(None, "inputs: hashed 9 entries in 1.0 s");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("inputs: rechecked by stat 9 entries in 500 ms\n{event}\n")
        );
        let mut timeline = crate::release_timeline::Timeline::default();
        timeline
            .read(
                crate::release_timeline::TRANSCRIPT,
                std::fs::read(&log).unwrap().as_slice(),
            )
            .unwrap();
        // The cutter's own call in its transcript makes the hash count known: none.
        let value = timeline.value();
        assert_eq!(
            (
                value["full_hashes"].as_u64(),
                value["seal_rechecks"].as_u64()
            ),
            (Some(0), Some(1)),
            "{value}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn selected_rust_libraries_ignore_other_targets_but_bind_every_used_library() {
        let root = aterm_tempfile::tempdir().unwrap();
        let selected = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
        let used = [
            "lib/libLLVM.dylib",
            "lib/rustlib/aarch64-apple-darwin/lib/std.rlib",
            "lib/rustlib/aarch64-apple-darwin/codegen-backends/backend.dylib",
            "lib/rustlib/x86_64-apple-darwin/lib/std.rlib",
        ];
        let write = |name: &str, bytes: &[u8]| {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        for name in used {
            write(name, b"original");
        }
        let digest = || {
            let roots = rust_library_roots("compiler", root.path(), &selected).unwrap();
            digest_roots(
                &roots
                    .iter()
                    .map(|(name, path)| (name.as_str(), path.as_path()))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let original = digest();
        for name in [
            "lib/rustlib/aarch64-unknown-linux-gnu/lib/std.rlib",
            "lib/rustlib/components",
            "lib/rustlib/manifest-rust-std-aarch64-unknown-linux-gnu",
            "lib/rustlib/multirust-config.toml",
        ] {
            write(name, b"installed later");
            assert_eq!(digest(), original, "unrelated installation state: {name}");
        }
        for name in used {
            write(name, b"changed");
            assert_ne!(digest(), original, "selected input: {name}");
            write(name, b"original");
            assert_eq!(digest(), original);
        }
        assert_eq!(
            rust_library_roots("compiler", root.path(), &selected).unwrap(),
            rust_library_roots(
                "compiler",
                root.path(),
                &[selected[1], selected[0], selected[1]]
            )
            .unwrap()
        );
        for targets in [&[][..], &["missing"][..], &["../escape"][..]] {
            assert!(rust_library_roots("compiler", root.path(), targets).is_err());
        }
    }

    #[test]
    fn compiler_host_requires_one_valid_target() {
        assert_eq!(
            host_from_text("rustc 1.0\nhost: aarch64-apple-darwin\n").unwrap(),
            "aarch64-apple-darwin"
        );
        for text in ["", "host: ../escape", "host: ", "host: a\nhost: b"] {
            assert!(host_from_text(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn an_unreadable_input_never_yields_an_identity() {
        let root = aterm_tempfile::tempdir().unwrap();
        let file = root.path().join("compiler");
        std::fs::write(&file, "compiler").unwrap();
        assert!(digest_roots(&[("compiler", root.path())]).is_ok());
        let error = digest_roots_with(&[("compiler", root.path())], &mut |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        })
        .unwrap_err();
        assert!(error.contains("denied"), "{error}");
    }

    #[test]
    fn compiler_library_standard_library_and_sdk_bytes_all_change_identity() {
        let root = aterm_tempfile::tempdir().unwrap();
        let files = [
            "bin/trustc",
            "lib/compiler.dylib",
            "lib/rustlib/target/lib/std.rlib",
            "sdk/usr/include/header.h",
        ];
        for name in files {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"original").unwrap();
        }
        let digest = || digest_roots(&[("inputs", root.path())]).unwrap();
        let original = digest();
        for name in files {
            std::fs::write(root.path().join(name), b"changed!").unwrap();
            assert_ne!(digest(), original, "{name}");
            std::fs::write(root.path().join(name), b"original").unwrap();
            assert_eq!(digest(), original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_aliases_match_but_changed_link_targets_do_not() {
        use std::os::unix::fs::symlink;
        let root = aterm_tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        std::fs::create_dir(&payload).unwrap();
        std::fs::write(payload.join("compiler"), b"one").unwrap();
        let alias = root.path().join("alias");
        symlink(&payload, &alias).unwrap();
        let original = digest_roots(&[("compiler", &payload)]).unwrap();
        assert_eq!(digest_roots(&[("compiler", &alias)]).unwrap(), original);
        let external = root.path().join("external");
        std::fs::write(&external, b"library").unwrap();
        symlink(&external, payload.join("lib")).unwrap();
        let with_link = digest_roots(&[("compiler", &payload)]).unwrap();
        std::fs::write(&external, b"replacement").unwrap();
        assert_ne!(digest_roots(&[("compiler", &payload)]).unwrap(), with_link);
    }

    /// A link back to a directory the walk is inside counts by its target text, once: the
    /// walk ends, the link's presence and its spelling change the identity, and the content
    /// it leads back to still does. CONTROL: the same tree without the link.
    #[cfg(unix)]
    #[test]
    fn a_link_back_to_an_enclosing_directory_counts_by_its_target_text() {
        use std::os::unix::fs::symlink;
        let root = aterm_tempfile::tempdir().unwrap();
        let headers = root.path().join("Headers/ruby");
        std::fs::create_dir_all(&headers).unwrap();
        std::fs::write(headers.join("ruby.h"), b"one").unwrap();
        let digest = || digest_roots(&[("sdk", root.path())]).unwrap();
        let without = digest();
        symlink(".", headers.join("ruby")).unwrap();
        let with_dot = digest();
        assert_ne!(with_dot, without, "the link is part of the identity");
        assert_eq!(digest(), with_dot);
        std::fs::remove_file(headers.join("ruby")).unwrap();
        symlink("..", headers.join("ruby")).unwrap();
        assert_ne!(digest(), with_dot, "so is its target text");
        std::fs::write(headers.join("ruby.h"), b"two").unwrap();
        let changed = digest();
        std::fs::write(headers.join("ruby.h"), b"one").unwrap();
        assert_ne!(changed, digest(), "and the content it leads back to");
    }

    #[test]
    fn missing_roots_and_ambiguous_labels_fail_closed() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert!(digest_roots(&[("missing", &root.path().join("missing"))]).is_err());
        assert!(digest_roots(&[("same", root.path()), ("same", root.path())]).is_err());
        assert!(digest_roots(&[]).is_err());
    }

    /// A dangling link beneath a root counts by its target text: a new target text, or the
    /// target appearing, changes the identity. CONTROL: a dangling root still refuses.
    #[cfg(unix)]
    #[test]
    fn a_dangling_input_link_counts_by_its_target_text() {
        use std::os::unix::fs::symlink;
        let base = aterm_tempfile::tempdir().unwrap();
        let root = base.path().join("bin");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("compiler"), b"compiler").unwrap();
        let digest = || digest_roots(&[("tools", &root)]).unwrap();
        let without = digest();
        let link = root.join("crashlog");
        symlink("../missing", &link).unwrap();
        let dangling = digest();
        assert_ne!(dangling, without, "the link is part of the identity");
        assert_eq!(digest(), dangling);
        std::fs::remove_file(&link).unwrap();
        symlink("../elsewhere", &link).unwrap();
        let retargeted = digest();
        assert_ne!(
            retargeted, dangling,
            "the target text is part of the identity"
        );
        // The target lies outside the root, so only the link can carry its arrival.
        std::fs::write(base.path().join("elsewhere"), b"script").unwrap();
        assert_ne!(
            digest(),
            retargeted,
            "a target that appears changes the identity"
        );
        let dangling_root = base.path().join("root");
        symlink(base.path().join("absent"), &dangling_root).unwrap();
        assert!(digest_roots(&[("tools", &dangling_root)]).is_err());
    }

    /// A link above the root leads the walk back into the root as a real directory, which
    /// refuses, and a loop of links refuses as unresolvable. CONTROL: a link back to an
    /// enclosing directory counts (`a_link_back_to_an_enclosing_directory_counts_…`).
    #[cfg(unix)]
    #[test]
    fn a_walk_that_reenters_a_real_directory_or_loops_refuses() {
        use std::os::unix::fs::symlink;
        let base = aterm_tempfile::tempdir().unwrap();
        let root = base.path().join("sdk");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib/x.h"), b"x").unwrap();
        assert!(digest_roots(&[("sdk", &root)]).is_ok());
        symlink("../..", root.join("lib/up")).unwrap();
        let error = digest_roots(&[("sdk", &root)]).unwrap_err();
        assert!(error.contains("directory cycle"), "{error}");
        std::fs::remove_file(root.join("lib/up")).unwrap();
        symlink("b", root.join("a")).unwrap();
        symlink("a", root.join("b")).unwrap();
        let error = digest_roots(&[("sdk", &root)]).unwrap_err();
        assert!(error.contains("resolve build input"), "{error}");
    }

    #[cfg(unix)]
    /// A scratch tree of inputs, and one machine's view of it: what every process opens and says.
    struct Bench {
        _dir: aterm_tempfile::TempDir,
        root: PathBuf,
        /// A release state's `inputs-seal/`.
        kept: PathBuf,
        os_build: Option<String>,
        /// [`Facts::trust_build`].
        trust_build: Option<String>,
        /// Take a new seal on every call, as every call did before seals.
        fresh: bool,
        /// Whether the inputs were last changed long before each call (an hour, by its clock), as
        /// a release's toolchain was; otherwise the call's clock is the wall clock.
        settled: bool,
        opened: usize,
        said: Vec<String>,
    }

    #[cfg(unix)]
    impl Bench {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path().join("inputs");
            for (name, bytes) in [
                ("bin/trustc", "trustc"),
                ("lib/std.rlib", "std"),
                ("sdk/usr/include/stdio.h", "header"),
            ] {
                let path = root.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, bytes).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).unwrap();
            Self {
                root,
                kept: dir.path().join("state/inputs-seal"),
                _dir: dir,
                os_build: Some("25B78".into()),
                trust_build: Some(format!("16387 {}", "4".repeat(64))),
                fresh: false,
                settled: true,
                opened: 0,
                said: Vec::new(),
            }
        }

        /// `process`'s identity now.
        fn ask(&mut self, process: &mut Process) -> Result<String, String> {
            let Self {
                root,
                os_build,
                trust_build,
                fresh,
                settled,
                opened,
                said,
                ..
            } = self;
            let mut roots = || Ok(vec![("inputs".to_string(), root.clone())]);
            let mut facts = |_: &[Root]| Facts {
                os_build: os_build.clone(),
                model: Some("Mac0,0".into()),
                sdk_settings: None,
                trust_build: trust_build.clone(),
            };
            let mut open = |path: &Path| {
                *opened += 1;
                crate::durable::open_regular_file(path)
            };
            let hour = if *settled { 3_600_000_000_000 } else { 0 };
            let mut now = || wall_clock_ns() + hour;
            let mut say = |line: &str| said.push(line.to_string());
            process.identity(
                &mut Sources {
                    roots: &mut roots,
                    facts: &mut facts,
                    open: &mut open,
                    now: &mut now,
                    say: &mut say,
                },
                *fresh,
            )
        }

        /// Each call's `release-event:` line so far, as its mode and its entries.
        fn events(&self) -> Vec<(String, u64)> {
            self.said
                .iter()
                .filter_map(|line| line.strip_prefix("release-event: "))
                .map(|json| {
                    let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
                    (
                        event["mode"].as_str().unwrap().to_string(),
                        event["entries"].as_u64().unwrap(),
                    )
                })
                .collect()
        }

        /// How many calls so far hashed the inputs, and how many rechecked a seal by stat, by
        /// their `release-event:` lines.
        fn counts(&self) -> (usize, usize) {
            let mut counts = (0, 0);
            for line in &self.said {
                let Some(json) = line.strip_prefix("release-event: ") else {
                    continue;
                };
                let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
                assert_eq!(event["event"].as_str(), Some("inputs"), "{line}");
                assert!(event["entries"].as_u64().is_some_and(|n| n > 0), "{line}");
                assert!(event["secs"].as_f64().is_some(), "{line}");
                match event["mode"].as_str() {
                    Some("hashed") => counts.0 += 1,
                    Some("rechecked" | "adopted") => counts.1 += 1,
                    _ => panic!("{line}"),
                }
            }
            // `release status` counts the same lines in its timeline.
            let log = self.said.iter().map(|line| format!("{line}\n"));
            let mut timeline = crate::release_timeline::Timeline::default();
            timeline
                .read("01-cut.log", log.collect::<String>().as_bytes())
                .unwrap();
            let timeline = timeline.value();
            // A step log's records alone: a mode no call took is unknown, not 0.
            let some = |n: usize| (n > 0).then(|| u64::try_from(n).unwrap());
            assert_eq!(
                (
                    timeline["full_hashes"].as_u64(),
                    timeline["seal_rechecks"].as_u64()
                ),
                (some(counts.0), some(counts.1)),
                "the release timeline counts what the seal said"
            );
            counts
        }
    }

    #[cfg(unix)]
    /// A process started as handed `handed`, keeping its seal in `armed`.
    fn process(handed: Option<&Path>, armed: Option<&Path>) -> Process {
        let mut process = Process::unstarted();
        if let Some(dir) = armed {
            process.arm(dir.to_path_buf());
        }
        process.begin(handed.map(Path::to_path_buf), None);
        process
    }

    #[cfg(unix)]
    fn wall_clock_ns() -> i128 {
        i128::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn facts(os_build: &str) -> Facts {
        Facts {
            os_build: Some(os_build.into()),
            model: Some("Mac0,0".into()),
            sdk_settings: None,
            trust_build: Some(format!("16387 {}", "4".repeat(64))),
        }
    }

    /// A seal's recheck stats every entry, opens no input and holds while nothing changed; the
    /// identity is the content digest's on this machine. CONTROL: the take before it opened
    /// every file, the one a link resolves to again.
    #[cfg(unix)]
    #[test]
    fn a_seal_rechecked_by_stat_opens_no_input_and_holds() {
        let mut bench = Bench::new();
        let mut process = process(None, None);
        let taken = bench.ask(&mut process).unwrap();
        assert_eq!(bench.opened, 4, "three files and the link's file");
        assert_eq!(bench.counts(), (1, 0));
        for _ in 0..3 {
            assert_eq!(bench.ask(&mut process).unwrap(), taken);
        }
        assert_eq!(bench.opened, 4, "a recheck opens no input");
        assert_eq!(bench.counts(), (1, 3));
        let call = |mode: &str| (mode.to_string(), 10);
        assert_eq!(
            bench.events(),
            [
                call("hashed"),
                call("rechecked"),
                call("rechecked"),
                call("rechecked")
            ],
            "every call walks the 10 entries"
        );
        let digest = digest_roots(&[("inputs", &bench.root)]).unwrap();
        assert_eq!(taken, identity_of(&digest, &facts("25B78")));
    }

    #[cfg(unix)]
    /// What one change does to `process`'s seal: it hashes again exactly once, and the next call
    /// holds. Its identity.
    fn after_change(
        bench: &mut Bench,
        process: &mut Process,
        change: impl FnOnce(&Path),
    ) -> String {
        let (hashed, _) = bench.counts();
        change(&bench.root);
        let identity = bench.ask(process).unwrap();
        assert_eq!(
            bench.counts().0,
            hashed + 1,
            "hashed again: {:?}",
            bench.said
        );
        assert_eq!(bench.ask(process).unwrap(), identity);
        assert_eq!(bench.counts().0, hashed + 1, "then holds");
        identity
    }

    #[cfg(unix)]
    fn set_modified(path: &Path, when: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    /// A write of the same size whose modification time is put back hashes the inputs again,
    /// once, into another identity: only the status-change time tells, and APFS stamps it in
    /// nanoseconds. (A jiffy clock, Linux's before 6.13, can stamp that write with the time the
    /// take saw; a take of inputs that changed that recently is never held, as
    /// `a_take_of_inputs_that_just_changed_is_never_held` shows.) NEGATIVE CONTROL: leave
    /// `ctime_ns` out of [`StatKey`]'s encoding and the write holds by stat over bytes no take
    /// read.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_write_with_its_modification_time_put_back_hashes_again() {
        let mut bench = Bench::new();
        let mut process = process(None, None);
        let original = bench.ask(&mut process).unwrap();
        let written = after_change(&mut bench, &mut process, |root| {
            let trustc = root.join("bin/trustc");
            let modified = std::fs::metadata(&trustc).unwrap().modified().unwrap();
            std::fs::write(&trustc, "TRUSTC").unwrap();
            set_modified(&trustc, modified);
        });
        assert_ne!(written, original, "a write is another identity");
    }

    /// Each change a status shows hashes the inputs again, once: a touch, a chmod, a replacement
    /// by identical bytes, a new entry, a retargeted link, and a rewrite of a file outside the
    /// root that a link inside it resolves to. The identity follows the content: the same after a
    /// touch or a replacement, another after a chmod, a new entry, a retarget or the rewrite.
    /// NEGATIVE CONTROL: record a constant status for what a link resolves to and the rewrite
    /// outside the root holds by stat over bytes no take read.
    #[cfg(unix)]
    #[test]
    fn every_change_a_status_shows_hashes_the_inputs_again_once() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut bench = Bench::new();
        let mut process = process(None, None);
        let original = bench.ask(&mut process).unwrap();
        let touched = after_change(&mut bench, &mut process, |root| {
            set_modified(
                &root.join("lib/std.rlib"),
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000),
            );
        });
        assert_eq!(touched, original, "a touch changes no content");
        let chmodded = after_change(&mut bench, &mut process, |root| {
            std::fs::set_permissions(
                root.join("lib/std.rlib"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        });
        assert_ne!(chmodded, touched, "a mode is content");
        let replaced = after_change(&mut bench, &mut process, |root| {
            let staged = root.join("lib/.std.rlib.new");
            std::fs::write(&staged, "std").unwrap();
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::rename(&staged, root.join("lib/std.rlib")).unwrap();
        });
        assert_eq!(replaced, chmodded, "identical bytes in another inode");
        let added = after_change(&mut bench, &mut process, |root| {
            std::fs::write(root.join("lib/extra.rlib"), "extra").unwrap();
        });
        assert_ne!(added, replaced);
        let retargeted = after_change(&mut bench, &mut process, |root| {
            std::fs::remove_file(root.join("bin/rustc")).unwrap();
            std::os::unix::fs::symlink("../lib/std.rlib", root.join("bin/rustc")).unwrap();
        });
        assert_ne!(retargeted, added);
        // The Command Line Tools' usr/bin/python3 resolves into their Library: no root holds it,
        // so only the link's record of its resolved status can tell its rewrite.
        let outside = bench.root.parent().unwrap().join("outside/python3.9");
        let linked = after_change(&mut bench, &mut process, |root| {
            std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
            std::fs::write(&outside, "python").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("bin/python3")).unwrap();
        });
        assert_ne!(linked, retargeted);
        let rewritten = after_change(&mut bench, &mut process, |_| {
            std::fs::write(&outside, "python 3.9.6").unwrap();
        });
        assert_ne!(
            rewritten, linked,
            "the bytes a link resolves to are content"
        );
    }

    /// Another OS build is another identity, hashed again though no input's status moved: an
    /// update mid-run is caught, not taken for the same inputs. CONTROL: the same build holds.
    #[cfg(unix)]
    #[test]
    fn another_os_build_hashes_again_into_another_identity() {
        let mut bench = Bench::new();
        let mut process = process(None, None);
        let before = bench.ask(&mut process).unwrap();
        assert_eq!(bench.ask(&mut process).unwrap(), before);
        assert_eq!(bench.counts(), (1, 1));
        bench.os_build = Some("25C5".into());
        let opened = bench.opened;
        let after = bench.ask(&mut process).unwrap();
        assert_ne!(after, before);
        assert_eq!(bench.counts(), (2, 1));
        assert!(bench.opened > opened, "it read the inputs again");
    }

    /// Another Trust build is another identity, hashed again though no input's status moved: a
    /// seal names the build it covers. CONTROL: the same build holds. NEGATIVE CONTROL: drop
    /// [`Facts::trust_build`] from [`identity_under`], and both builds read one identity.
    #[cfg(unix)]
    #[test]
    fn another_trust_build_hashes_again_into_another_identity() {
        let mut bench = Bench::new();
        let mut process = process(None, None);
        let before = bench.ask(&mut process).unwrap();
        assert_eq!(bench.ask(&mut process).unwrap(), before);
        assert_eq!(bench.counts(), (1, 1));
        bench.trust_build = Some(format!("16500 {}", "5".repeat(64)));
        let after = bench.ask(&mut process).unwrap();
        assert_ne!(after, before);
        assert_eq!(bench.counts(), (2, 1));
        assert!(
            bench
                .said
                .iter()
                .any(|line| line.contains("Trust build (16387 ")),
            "{:?}",
            bench.said
        );
    }

    /// The Trust build of a selected toolchain is the store build its `bin` is, with the tree
    /// root recorded for that tree, `unrecorded` without one. NEGATIVE CONTROLS: a `bin` outside
    /// the store, another program's build, and a directory below a build's `bin` name none.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_selected_toolchain_names_the_store_build_it_is() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let prefix = dir.path().canonicalize().unwrap();
        let laid = prefix.join("store/trust/16387.noindex");
        std::fs::create_dir_all(laid.join("bin/deeper")).unwrap();
        let ay = prefix.join("store/ay/18/bin");
        std::fs::create_dir_all(&ay).unwrap();
        let elsewhere = prefix.join("checkout/bin");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let bin = laid.join("bin");
        assert_eq!(
            selected_trust_build(&prefix, &bin).as_deref(),
            Some("16387 unrecorded")
        );
        let root = "4".repeat(64);
        atpkg::trust_lock::write_tree_root_record(&laid, &root).unwrap();
        assert_eq!(
            selected_trust_build(&prefix, &bin),
            Some(format!("16387 {root}"))
        );
        for none in [elsewhere, ay, bin.join("deeper")] {
            assert_eq!(
                selected_trust_build(&prefix, &none),
                None,
                "{}",
                none.display()
            );
        }
    }

    /// A machine whose OS build could not be read keeps no seal and adopts none: its process
    /// holds its own, and a child or a later run hashes for itself, as `None` would match the
    /// `None` of a run after an OS update. CONTROL: with the OS build read, the same calls keep a
    /// seal that a second process adopts. NEGATIVE CONTROL: drop [`Facts::complete`] from
    /// [`Process::keep`] and [`load`], and the second process adopts the first's.
    #[cfg(unix)]
    #[test]
    fn a_seal_of_facts_unread_is_neither_kept_nor_adopted() {
        let mut bench = Bench::new();
        let state = bench.kept.clone();
        bench.os_build = None;
        let mut first = process(None, Some(&state));
        let identity = bench.ask(&mut first).unwrap();
        assert_eq!(bench.ask(&mut first).unwrap(), identity);
        assert_eq!(bench.counts(), (1, 1), "held in the process that took it");
        assert_eq!(first.saved, None, "and kept nowhere");
        let mut second = process(None, Some(&state));
        assert_eq!(bench.ask(&mut second).unwrap(), identity);
        assert_eq!(
            bench.counts(),
            (2, 1),
            "the second process hashes for itself"
        );
        let mut unread = first.seal.clone().unwrap();
        let path = state.join("unread.toml");
        save(&unread, &path).unwrap();
        assert_eq!(load(&path), None, "a kept seal of unread facts is no seal");
        unread.facts = facts("25B78");
        unread.identity = identity_of(&unread.digest, &unread.facts);
        save(&unread, &path).unwrap();
        assert_eq!(load(&path).as_ref(), Some(&unread));

        bench.os_build = Some("25B78".into());
        let mut kept = process(None, Some(&state));
        bench.ask(&mut kept).unwrap();
        assert!(kept.saved.is_some(), "read facts keep their seal");
        let mut adopting = process(None, Some(&state));
        bench.ask(&mut adopting).unwrap();
        assert_eq!(
            bench.events().last().map(|(mode, _)| mode.as_str()),
            Some("adopted")
        );
    }

    /// A take of inputs whose newest status change is less than [`SETTLE_NS`] old is not held:
    /// a write in the same tick of a coarse clock could leave that status as the take saw it. So
    /// the next call hashes again, and nothing is kept for another process; once the inputs have
    /// settled, a take holds and is kept. NEGATIVE CONTROL: drop the settle check from
    /// [`Seal::hashed`] and the second call rechecks by stat.
    #[cfg(unix)]
    #[test]
    fn a_take_of_inputs_that_just_changed_is_never_held() {
        let mut bench = Bench::new();
        let state = bench.kept.clone();
        bench.settled = false;
        let mut process = process(None, Some(&state));
        let identity = bench.ask(&mut process).unwrap();
        assert_eq!(
            (process.seal.is_none(), process.saved.as_ref()),
            (true, None)
        );
        assert_eq!(bench.ask(&mut process).unwrap(), identity);
        assert_eq!(bench.counts(), (2, 0), "hashed again");
        bench.settled = true;
        assert_eq!(bench.ask(&mut process).unwrap(), identity);
        assert_eq!(bench.ask(&mut process).unwrap(), identity);
        assert_eq!(bench.counts(), (3, 1), "settled, the take holds");
        assert!(process.saved.is_some(), "and is kept");
    }

    /// A file written between the status the walk took and its read refuses the take: the bytes
    /// read are no version that status names. CONTROL: the same walk with no write succeeds.
    /// NEGATIVE CONTROL: drop the descriptor's status check after the read and the take passes.
    #[test]
    fn an_input_written_while_it_is_hashed_refuses_the_take() {
        let root = aterm_tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("compiler"), b"compiler").unwrap();
        let roots = [("inputs", root.path())];
        assert!(digest_roots(&roots).is_ok());
        let error = digest_roots_with(&roots, &mut |path: &Path| {
            std::fs::write(path, b"written while it was hashed").unwrap();
            crate::durable::open_regular_file(path)
        })
        .expect_err("refused");
        assert!(error.contains("changed while it was hashed"), "{error}");
    }

    /// A directory replaced between the status the walk took and its listing refuses the take,
    /// and on APFS so does an entry added there: the names listed are no version that status
    /// names. CONTROL: the same walk with nothing changed succeeds. NEGATIVE CONTROL: drop the
    /// directory's status check after its listing and both takes pass.
    #[cfg(unix)]
    #[test]
    fn a_directory_changed_while_it_is_listed_refuses_the_take() {
        let root = aterm_tempfile::tempdir().unwrap();
        let lib = root.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        std::fs::write(lib.join("std.rlib"), b"std").unwrap();
        let roots = [("inputs", root.path())];
        let lib = lib.canonicalize().unwrap();
        let take = |change: &mut dyn FnMut(&Path)| {
            walk_roots(
                &roots,
                Some(Reads {
                    open: &mut crate::durable::open_regular_file,
                    list: &mut |dir: &Path| {
                        if dir == lib {
                            change(dir);
                        }
                        list_names(dir)
                    },
                }),
            )
            .map(|walked| walked.digest)
        };
        assert!(take(&mut |_| {}).is_ok());
        type Change = Box<dyn FnMut(&Path)>;
        let mut changes: Vec<Change> = vec![Box::new(|dir: &Path| {
            let old = dir.with_extension("old");
            std::fs::rename(dir, &old).unwrap();
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("std.rlib"), b"std").unwrap();
            std::fs::remove_dir_all(&old).unwrap();
        })];
        if cfg!(target_os = "macos") {
            changes.push(Box::new(|dir: &Path| {
                std::fs::write(dir.join("added.rlib"), b"added").unwrap();
            }));
        }
        for mut change in changes {
            let error = take(&mut *change).expect_err("refused");
            assert!(error.contains("changed while it was hashed"), "{error}");
        }
    }

    /// A kept seal reads back as written, 0600 in a 0700 directory; a file of another seal
    /// schema, of another content walk's schema, with an identity its digest and facts do not
    /// give, readable by others, in a directory others can read, or not TOML is no seal.
    /// CONTROL: the round trip, restored after each. NEGATIVE CONTROL: leave the content schema
    /// out of [`identity_under`] and another walk's seal is adopted.
    #[cfg(unix)]
    #[test]
    fn a_kept_seal_round_trips_and_any_other_file_is_no_seal() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let chmod = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let mut bench = Bench::new();
        let dir = bench.kept.clone();
        let mut process = process(None, Some(&dir));
        bench.ask(&mut process).unwrap();
        let path = process.saved.clone().expect("kept");
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert_eq!((mode(&path), mode(&dir)), (0o600, 0o700));
        let seal = process.seal.clone().unwrap();
        assert_eq!(load(&path).as_ref(), Some(&seal));
        let edits: [fn(&mut Seal); 4] = [
            |s| s.schema = "aterm-release-inputs-seal-v0".into(),
            |s| s.identity = "0".repeat(64),
            |s| s.digest = "0".repeat(64),
            // Kept by a cutter of another content walk: its digest is that walk's.
            |s| {
                s.identity = identity_under(b"aterm-release-build-inputs-v1", &s.digest, &s.facts);
            },
        ];
        for edit in edits {
            let mut other = seal.clone();
            edit(&mut other);
            save(&other, &path).unwrap();
            assert_eq!(load(&path), None);
            save(&seal, &path).unwrap();
            assert_eq!(load(&path).as_ref(), Some(&seal));
        }
        chmod(&path, 0o644);
        assert_eq!(load(&path), None);
        chmod(&path, 0o600);
        chmod(&dir, 0o755);
        assert_eq!(load(&path), None);
        chmod(&dir, 0o700);
        assert_eq!(load(&path).as_ref(), Some(&seal));
        std::fs::write(&path, "not = [a seal").unwrap();
        assert_eq!(load(&path), None);
        assert_eq!(load(&dir.join("absent.toml")), None);
    }

    #[cfg(unix)]
    /// `n` calls of `process`, each `identity`.
    fn asks(bench: &mut Bench, process: &mut Process, n: usize, identity: &str) {
        for _ in 0..n {
            assert_eq!(bench.ask(process).unwrap(), identity);
        }
    }

    #[cfg(unix)]
    /// One release's calls for the identity, in the processes that make them (today's callers,
    /// counted per step): the launching cutter's step 1; the cutter it re-runs as, handed the
    /// seal, at its step 1, the build step, verify's before and after, and each step's refresh
    /// of its facts; and the children each is handed it: the build child, the pre-promote check
    /// and the cut child. `mid_run` runs before the cut child starts. The identity.
    fn one_release(bench: &mut Bench, state: &Path, mid_run: impl FnOnce(&Path)) -> String {
        let mut launcher = process(None, Some(state));
        let identity = bench.ask(&mut launcher).unwrap();
        let mut cutter = process(launcher.saved.as_deref(), Some(state));
        asks(bench, &mut cutter, 1, &identity);
        let handed = cutter.saved.clone();
        let mut build = process(handed.as_deref(), None);
        asks(bench, &mut build, 2, &identity);
        asks(bench, &mut cutter, 2, &identity);
        asks(bench, &mut cutter, 4, &identity);
        asks(bench, &mut cutter, 2, &identity);
        let mut check = process(handed.as_deref(), None);
        asks(bench, &mut check, 1, &identity);
        asks(bench, &mut cutter, 2, &identity);
        asks(bench, &mut cutter, 2, &identity);
        mid_run(&bench.root);
        let mut cut = process(handed.as_deref(), None);
        asks(bench, &mut cut, 3, &identity);
        asks(bench, &mut cutter, 2, &identity);
        asks(bench, &mut cutter, 2, &identity);
        identity
    }

    /// One release asks for the identity 24 times across five processes, and hashes its inputs
    /// once; a second release on the same inputs hashes none. A status that moves mid-run costs
    /// one hash more, by the process that saw it, whose seal the others then adopt. CONTROL:
    /// with every call taking a new seal, as each did before seals, the release hashes 24 times.
    #[cfg(unix)]
    #[test]
    fn one_release_hashes_its_inputs_once_and_a_warm_one_not_at_all() {
        let mut bench = Bench::new();
        let state = bench.kept.clone();
        let before = aterm_tempfile::tempdir().unwrap();
        bench.fresh = true;
        let identity = one_release(&mut bench, before.path(), |_| {});
        assert_eq!(bench.counts(), (24, 0), "before: every call hashed");
        bench.fresh = false;
        bench.said.clear();
        assert_eq!(one_release(&mut bench, &state, |_| {}), identity);
        assert_eq!(bench.counts(), (1, 23), "one hash per release");
        bench.said.clear();
        assert_eq!(one_release(&mut bench, &state, |_| {}), identity);
        assert_eq!(bench.counts(), (0, 24), "none from a kept seal");
        bench.said.clear();
        let touched = one_release(&mut bench, &state, |root| {
            std::fs::File::options()
                .write(true)
                .open(root.join("bin/trustc"))
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
                .unwrap();
        });
        assert_eq!(touched, identity, "a touch changes no content");
        assert_eq!(
            bench.counts(),
            (1, 23),
            "the cut child's hash, adopted by the cutter"
        );
    }

    /// The content walk's test vector: a fixed tree's digest under [`SCHEMA`]. A change to what
    /// [`walk`] reads into the digest or how it encodes it changes this vector, and ships with a
    /// new schema and a new vector, never a new vector alone: a kept seal binds the schema, not
    /// the walk's code, so under the old schema the old walk's seals would still be adopted.
    #[cfg(unix)]
    #[test]
    fn the_content_digest_of_a_fixed_tree_is_schema_v2s() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("inputs");
        for (name, bytes, mode) in [
            ("bin/trustc", "trustc", 0o755),
            ("lib/std.rlib", "std", 0o644),
            ("sdk/usr/include/stdio.h", "header", 0o444),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).unwrap();
        std::os::unix::fs::symlink("missing", root.join("bin/crashlog")).unwrap();
        std::os::unix::fs::symlink(".", root.join("sdk/usr/include/self")).unwrap();
        for sub in ["", "bin", "lib", "sdk", "sdk/usr", "sdk/usr/include"] {
            std::fs::set_permissions(root.join(sub), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        assert_eq!(
            (
                std::str::from_utf8(SCHEMA).unwrap(),
                digest_roots(&[("inputs", &root), ("again", &root.join("bin"))])
                    .unwrap()
                    .as_str()
            ),
            (
                "aterm-release-build-inputs-v2",
                "be778f1603d620e4117be30eca49205ee1cedfb5ed39f45a194420b35ceeedf5"
            )
        );
    }
}
