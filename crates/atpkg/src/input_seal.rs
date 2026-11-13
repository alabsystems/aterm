// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content identity of a caller's input roots (a toolchain, an SDK, the drivers that reach them),
//! sealed once and rechecked by status.
//!
//! The identity [`Process::identity`] returns is a SHA-256 over the caller's [`Schema`], the
//! content digest of every input root (each file read in full) and this machine's [`Facts`] (its
//! OS build, its model, the SDK's settings and the Trust build). Reading every input's bytes takes
//! seconds, and a caller may ask for the identity many times across its processes. So a process's
//! first call takes a seal (the content digest and, from the same walk, a status digest of each
//! root, [`aterm_toolchain::stat_digest`]) and every later call rechecks it by status alone,
//! reading no input file, and hashes again only when the roots, the facts or a status differ. A
//! take whose inputs changed too recently for their status to tell a later write ([`SETTLE_NS`])
//! is not held, so the next call hashes again. A process armed with a directory
//! ([`Process::arm`]) keeps its seal there, and a process handed a kept seal ([`Process::begin`])
//! adopts it once the same recheck holds. A caller told to stop asks [`Process::holds`] whether
//! the seal it holds still stands, by the same recheck walked to its end.
//!
//! The caller owns its root list (from the resolution helpers here: [`trust_roots`] and
//! [`rust_library_roots`], and on macOS `trust_sysroot`, `trust_host`, `macos_sdk` and
//! `apple_roots`), the order it resolves them in, its schema, how a kept seal is handed between
//! its processes, and what each call says ([`Report`]). The release's build inputs
//! (`aterm-release`'s `build_inputs`) are one caller.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use aterm_toolchain::stat_digest::{SETTLE_NS, StatDigest, StatKey};

/// The two schemas a caller's identity binds. Two callers of different schemas never share an
/// identity or a kept seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schema {
    /// The content walk's schema, the first field of its digest and part of the identity. Any
    /// change to what the walk reads into the content digest, or how it encodes it, ships with a
    /// new walk schema in every caller: a seal kept by a process of the old walk otherwise still
    /// stands for the new one, as nothing else in a kept seal names the walk's code. Each caller
    /// holds its schema to a test vector of a fixed tree's digest.
    pub walk: &'static [u8],
    /// The seal's own schema, part of the identity: a seal of another schema is no seal here.
    pub seal: &'static str,
}

/// The largest kept seal read: one is about a kilobyte per root.
const MAX_SEAL_BYTES: u64 = 1024 * 1024;

/// A labelled input root: its label and its path, resolved now.
pub type Root = (String, PathBuf);

/// Opens an input file to read it for the content digest.
pub type Open<'a> = dyn FnMut(&Path) -> std::io::Result<std::fs::File> + 'a;

/// Lists an input directory's names for the content digest.
type List<'a> = dyn FnMut(&Path) -> std::io::Result<Vec<std::ffi::OsString>> + 'a;

/// How a take reads the inputs, as seams: each file it hashes, and each directory's names.
struct Reads<'a> {
    open: &'a mut Open<'a>,
    list: &'a mut List<'a>,
}

/// What a seal reads, as seams: a caller's process-wide seal reads this machine, a test a scratch
/// tree.
pub struct Sources<'a> {
    /// The labelled input roots, resolved now.
    pub roots: &'a mut dyn FnMut() -> Result<Vec<Root>, String>,
    /// This machine's facts, given those roots (`machine_facts`, on macOS).
    pub facts: &'a mut dyn FnMut(&[Root]) -> Facts,
    /// Opens an input file to hash it.
    pub open: &'a mut Open<'a>,
    /// The wall clock, in nanoseconds since the Unix epoch ([`SETTLE_NS`], [`now_ns`]).
    pub now: &'a mut dyn FnMut() -> i128,
    /// Whether the caller was interrupted: a walk asks before each entry and between a file's
    /// chunks, and refuses once it is.
    pub stopped: &'a dyn Fn() -> bool,
    /// Where each call says what it did.
    pub report: &'a mut dyn FnMut(Report<'_>),
}

/// What a call did, for its caller to say. Each call that returns an identity reports exactly one
/// of `Hashed`, `Rechecked` and `Adopted`; the notes come where the protocol reaches them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Report<'a> {
    /// The inputs were hashed: this many entries walked, in this many seconds.
    Hashed { entries: u64, secs: f64 },
    /// The held seal's recheck held: this many entries stat-ed, in this many seconds.
    Rechecked { entries: u64, secs: f64 },
    /// The seal kept at `from` was adopted once its recheck held.
    Adopted {
        entries: u64,
        secs: f64,
        from: &'a Path,
    },
    /// Why the held seal, or a kept one, no longer stands; the call hashes again.
    Changed(&'a str),
    /// The take's newest status change was less than [`SETTLE_NS`] old, so its seal is neither
    /// held nor kept and the next call hashes again.
    Unsettled,
    /// Why the held seal was not kept: it stands in this process only, and every other process
    /// hashes for itself.
    NotKept(&'a str),
}

/// The wall clock, in nanoseconds since the Unix epoch: the clock a status-change time is stamped
/// from.
#[must_use]
pub fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX))
}

/// What the machine adds to the inputs' content: an OS update or another Mac is another identity,
/// though every input's bytes are the same. The Trust build the seal covers (its build number and
/// tree root, which a locked tree's discovery holds to its trust.lock) is one more fact; the Trust
/// roots stay in the status walk, so a store build relinked or repaired mid-run is still caught.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Facts {
    /// `kern.osversion` (`aterm_sysprobe::os_build`).
    pub os_build: Option<String>,
    /// `hw.model` (`aterm_sysprobe::machine_model`).
    pub model: Option<String>,
    /// `sdk_settings` of the [`SDK`] root.
    pub sdk_settings: Option<String>,
    /// `selected_trust_build` of the [`TRUST_SELECTED`] root: `None` for a toolchain outside the
    /// store. Not a fact [`Facts::complete`] waits on, since such a toolchain has none.
    pub trust_build: Option<String>,
}

impl Facts {
    /// Whether the OS build and the model were both read. A seal of facts that were not is held
    /// only by the process that took it: kept, `None` would match the `None` of a later process
    /// after an OS update, and the facts would catch nothing.
    #[must_use]
    pub const fn complete(&self) -> bool {
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

/// The label of the macOS SDK root (`apple_roots`), whose settings are a fact.
pub const SDK: &str = "macos-sdk";

/// The label of the Trust toolchain `bin` the process selected (`trust_roots`), whose store
/// build is a fact.
pub const TRUST_SELECTED: &str = "trust/selected-bin";

/// This machine's [`Facts`] for `roots`: the SDK's settings from the [`SDK`] root, the Trust build
/// from the [`TRUST_SELECTED`] root, as the store this machine's packages configuration names
/// holds it.
#[cfg(target_os = "macos")]
#[must_use]
pub fn machine_facts(roots: &[Root]) -> Facts {
    Facts {
        os_build: aterm_sysprobe::os_build(),
        model: aterm_sysprobe::machine_model(),
        sdk_settings: roots
            .iter()
            .find(|(label, _)| label == SDK)
            .map(|(_, sdk)| sdk_settings(sdk)),
        trust_build: roots
            .iter()
            .find(|(label, _)| label == TRUST_SELECTED)
            .and_then(|(_, bin)| {
                let home = aterm_types::dirs::home_dir();
                let configured = crate::config::load().prefix_path(home.as_deref());
                let layout = crate::store::resolve(configured.as_deref())?;
                selected_trust_build(&layout.prefix, bin)
            }),
    }
}

/// `<build> <tree root>` of the store's Trust build whose `bin` is `selected` (canonical, as the
/// caller resolved it), the root as recorded for the tree there now, or `<build> unrecorded` with
/// no such record; `None` when `selected` is not a Trust build in the store at `prefix` (a
/// dev-linked checkout, an explicit `TRUST_STAGE2_BIN` elsewhere).
#[cfg(target_os = "macos")]
fn selected_trust_build(prefix: &Path, selected: &Path) -> Option<String> {
    let prefix = prefix.canonicalize().ok()?;
    let (program, build) = crate::ops::store_build_of(&prefix, selected)?;
    let build_dir = selected.parent()?;
    if program != crate::trust_lock::PROGRAM
        || selected.file_name()? != "bin"
        || build_dir.parent()? != prefix.join("store").join(&program)
    {
        return None;
    }
    let root = crate::trust_lock::recorded_root_at(build_dir);
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
        match read_regular(&sdk.join(name), MAX_SEAL_BYTES * 4) {
            Ok((bytes, _)) => {
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

/// Up to `max` bytes of the regular file at `path`, with its status. A link at `path` fails the
/// open itself on Unix (`ELOOP`, `FilesystemLoop`) and is `InvalidInput` on Windows (a reparse
/// point); a directory or a special file is `InvalidInput`, and a file larger than `max` is
/// `FileTooLarge`. `sdk_settings` folds the kind into the identity.
fn read_regular(path: &Path, max: u64) -> std::io::Result<(Vec<u8>, std::fs::Metadata)> {
    let (file, meta) = crate::metadata_io::open_regular_with_meta(path)?;
    let too_large = || std::io::Error::new(std::io::ErrorKind::FileTooLarge, "file exceeds bound");
    if meta.len() > max {
        return Err(too_large());
    }
    let cap = usize::try_from(meta.len().min(max).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max {
        return Err(too_large());
    }
    Ok((buf, meta))
}

/// One take of the inputs, and what its recheck compares.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Seal {
    /// The caller's [`Schema::seal`].
    schema: String,
    /// What [`Process::identity`] returns: [`identity_under`] the digest and the facts.
    identity: String,
    /// The content digest of the roots (under the caller's [`Schema::walk`]).
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SealedRoot {
    label: String,
    path: PathBuf,
    /// Its [`StatDigest`], lowercase hex.
    stat: String,
    entries: u64,
}

/// What a recheck found.
#[derive(Debug, PartialEq, Eq)]
enum Rechecked {
    /// Every root, fact and status is the seal's: this many entries were stat-ed.
    Held(u64),
    /// Why the seal no longer stands.
    Changed(String),
}

impl Seal {
    /// Hash `roots` under `schema` and read the facts: the seal, and whether its inputs had
    /// settled when the take began, every status it recorded having last changed at least
    /// [`SETTLE_NS`] before. On a coarse clock (HFS+ keeps whole seconds) a later write in the
    /// same tick as a more recent change leaves that status as it was, so a recheck of such a
    /// seal could hold over bytes the take never read.
    fn hashed(
        schema: Schema,
        roots: &[Root],
        src: &mut Sources<'_>,
    ) -> Result<(Self, bool), String> {
        let facts = (src.facts)(roots);
        let began = (src.now)();
        let walked = walk_roots(
            schema.walk,
            &borrowed(roots),
            Some(Reads {
                open: &mut *src.open,
                list: &mut list_names,
            }),
            src.stopped,
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
            schema: schema.seal.into(),
            identity: identity_under(schema, &digest, &facts),
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
    /// the facts, and each root's status digest; no input file is read. Its walk ends early when
    /// the caller's [`Sources::stopped`] says so.
    fn recheck_by_stat(&self, roots: &[Root], src: &mut Sources<'_>) -> Rechecked {
        let stopped = src.stopped;
        self.recheck_by_stat_until(roots, src, stopped)
    }

    /// [`Self::recheck_by_stat`], ending its walk early when `stopped` says so, whatever the
    /// caller's [`Sources::stopped`] says.
    fn recheck_by_stat_until(
        &self,
        roots: &[Root],
        src: &mut Sources<'_>,
        stopped: &dyn Fn() -> bool,
    ) -> Rechecked {
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
        // A recheck reads no content, so no walk schema enters it.
        let walked = match walk_roots(b"", &borrowed(roots), None, stopped) {
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

/// The identity of a content digest that the walk of `schema` produced, on a machine with
/// `facts`. Binding both schemas is what makes a kept seal of another caller or another walk no
/// seal here ([`load`]).
fn identity_under(schema: Schema, digest: &str, facts: &Facts) -> String {
    let mut sha = aterm_digest::Sha256::new();
    field(&mut sha, schema.seal.as_bytes());
    field(&mut sha, schema.walk);
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

/// The seal kept at `path`, when it is one: a regular file only its owner can read or write, in a
/// private directory, of `schema`'s seal schema, whose identity is its digest's and facts' under
/// `schema`, with complete facts ([`Facts::complete`]). Anything else (missing, unreadable,
/// another schema or walk, facts unread, edited) is no seal.
fn load(schema: Schema, path: &Path) -> Option<Seal> {
    private_dir(path.parent()?).ok()?;
    let (bytes, meta) = read_regular(path, MAX_SEAL_BYTES).ok()?;
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
    (seal.schema == schema.seal
        && seal.facts.complete()
        && seal.identity == identity_under(schema, &seal.digest, &seal.facts))
    .then_some(seal)
}

/// The file in the seal directory `dir` that holds one internally valid kept seal of `schema`
/// whose identity is `expected`, for the resolved root set `roots`. This deliberately does not
/// recheck the roots: the receiving process does that through [`Process::identity`] before it
/// adopts or persists the seal.
#[must_use]
pub fn matching_kept(
    schema: Schema,
    dir: &Path,
    expected: &str,
    roots: &[Root],
) -> Option<PathBuf> {
    if !aterm_codec::hex::is_lower(expected, 64) {
        return None;
    }
    let path = dir.join(kept_name(roots));
    load(schema, &path)
        .is_some_and(|seal| seal.identity == expected)
        .then_some(path)
}

/// Keep `seal` at `path`: written whole (0600) in a private directory, or not at all.
fn save(seal: &Seal, path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("a kept seal needs a directory")?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // Only unix sets a mode, so only there is the builder changed after it is made.
    #[cfg(unix)]
    let builder = {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("create {}: {e}", dir.display())),
    }
    private_dir(dir)?;
    let text = aterm_toml::to_string(seal).map_err(|e| format!("serialize the seal: {e}"))?;
    write_durable(path, text.as_bytes()).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Stage `bytes` (0600) in a private temporary directory beside `path`, flush them, rename them
/// over `path`, and flush the directory.
fn write_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stage = aterm_tempfile::Builder::new()
        .prefix(".stage-")
        .suffix(".noindex")
        .tempdir_in(dir)?;
    let staged = stage.path().join("data");
    let mut file = crate::platform::open_create_new_write(&staged, 0o600)?;
    crate::platform::set_mode_on(&file, 0o600)?;
    std::io::Write::write_all(&mut file, bytes)?;
    aterm_durable::durable(&file)?;
    drop(file);
    std::fs::rename(&staged, path)?;
    aterm_durable::durable_dir(dir)
}

/// One process's seal under one caller's [`Schema`], and where it keeps and finds one.
pub struct Process {
    schema: Schema,
    /// The seal this process holds, once taken or adopted.
    seal: Option<Seal>,
    /// The seal this process was handed ([`Process::begin`]).
    handed: Option<PathBuf>,
    /// The directory it keeps its seal in: [`Process::arm`]'s, else the handed seal's.
    keeps: Option<PathBuf>,
    /// Where else it finds a kept seal ([`Process::begin`]).
    finds: Option<PathBuf>,
    /// The file in `keeps` that holds `seal`.
    saved: Option<PathBuf>,
}

impl Process {
    /// A process of `schema` that holds, keeps and was handed no seal.
    #[must_use]
    pub const fn unstarted(schema: Schema) -> Self {
        Self {
            schema,
            seal: None,
            handed: None,
            keeps: None,
            finds: None,
            saved: None,
        }
    }

    /// Start with `handed`, the kept seal this process was handed, and `finds`, a directory where
    /// another process may keep one: a handed seal's directory is where this process keeps its
    /// own, unless [`Process::arm`] named one.
    pub fn begin(&mut self, handed: Option<PathBuf>, finds: Option<PathBuf>) {
        if self.keeps.is_none() {
            self.keeps = handed
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
        }
        self.handed = handed;
        self.finds = finds;
    }

    /// Keep this process's seal in `dir` from its next call on.
    pub fn arm(&mut self, dir: PathBuf) {
        if self.keeps.as_ref() != Some(&dir) {
            self.saved = None;
        }
        self.keeps = Some(dir);
    }

    /// Let go of the held seal, and of the file that kept it: the next call adopts or takes one.
    pub fn forget(&mut self) {
        self.seal = None;
        self.saved = None;
    }

    /// The file that holds this process's seal once it is kept, which another process may be
    /// handed; `None` before then, and when nothing armed or handed persistence.
    #[must_use]
    pub fn saved(&self) -> Option<&Path> {
        self.saved.as_deref()
    }

    /// Whether the held seal is `identity` and still stands by status (its roots, facts and every
    /// entry's status are still the ones it hashed), walked to its end whatever
    /// [`Sources::stopped`] says. Unlike [`Process::identity`] it opens no input file, takes,
    /// adopts and keeps no seal, and reports nothing: a caller told to stop asks this once its
    /// work has ended, to learn whether that work ran under the identity it was given. `false` is
    /// no verdict on the inputs: it also answers a process that holds no seal and roots that
    /// cannot be resolved.
    #[must_use]
    pub fn holds(&self, src: &mut Sources<'_>, identity: &str) -> bool {
        let Some(seal) = self.seal.as_ref().filter(|seal| seal.identity == identity) else {
            return false;
        };
        let Ok(roots) = (src.roots)() else {
            return false;
        };
        matches!(
            seal.recheck_by_stat_until(&roots, src, &|| false),
            Rechecked::Held(_)
        )
    }

    /// The identity: the held seal's when its recheck holds, else a kept one's whose recheck
    /// holds (the handed one first), else a new seal's; `fresh` takes a new one at once. A new
    /// seal is held only once its inputs have settled ([`SETTLE_NS`]), and a held or adopted
    /// one is kept where this process keeps one ([`Process::arm`], [`Process::begin`]) when its
    /// facts are complete ([`Facts::complete`]).
    /// # Errors
    /// The roots cannot be resolved, or the walk refuses (a root missing or unreadable, an input
    /// changed while it was hashed, `stopped`).
    pub fn identity(&mut self, src: &mut Sources<'_>, fresh: bool) -> Result<String, String> {
        let begun = std::time::Instant::now();
        let roots = (src.roots)()?;
        let name = kept_name(&roots);
        if !fresh {
            let mut changed = None;
            if let Some(seal) = &self.seal {
                match seal.recheck_by_stat(&roots, src) {
                    Rechecked::Held(entries) => {
                        let identity = seal.identity.clone();
                        (src.report)(Report::Rechecked {
                            entries,
                            secs: begun.elapsed().as_secs_f64(),
                        });
                        self.keep(&name, src);
                        return Ok(identity);
                    }
                    Rechecked::Changed(why) => changed = Some(why),
                }
            }
            for path in self.kept_paths(&name) {
                let Some(found) = load(self.schema, &path) else {
                    continue;
                };
                if self.seal.as_ref() == Some(&found) {
                    continue;
                }
                match found.recheck_by_stat(&roots, src) {
                    Rechecked::Held(entries) => {
                        (src.report)(Report::Adopted {
                            entries,
                            secs: begun.elapsed().as_secs_f64(),
                            from: &path,
                        });
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
                (src.report)(Report::Changed(&why));
            }
        }
        let (seal, settled) = Seal::hashed(self.schema, &roots, src)?;
        (src.report)(Report::Hashed {
            entries: seal.entries,
            secs: begun.elapsed().as_secs_f64(),
        });
        let identity = seal.identity.clone();
        self.saved = None;
        if settled {
            self.seal = Some(seal);
            self.keep(&name, src);
        } else {
            self.seal = None;
            (src.report)(Report::Unsettled);
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
    /// every other process then hashes for itself.
    fn keep(&mut self, name: &str, src: &mut Sources<'_>) {
        let (Some(dir), Some(seal)) = (&self.keeps, &self.seal) else {
            return;
        };
        let path = dir.join(name);
        if self.saved.as_ref() == Some(&path) {
            return;
        }
        if !seal.facts.complete() {
            (src.report)(Report::NotKept(
                "this machine's OS build or model could not be read",
            ));
            return;
        }
        match save(seal, &path) {
            Ok(()) => self.saved = Some(path),
            Err(why) => (src.report)(Report::NotKept(&why)),
        }
    }
}

/// The sysroot the selected Trust `bin`'s `trustc --print sysroot` names.
/// # Errors
/// `trustc` cannot run, fails, or prints no absolute path.
#[cfg(target_os = "macos")]
pub fn trust_sysroot(bin: &Path) -> Result<PathBuf, String> {
    output_path(
        aterm_uds::spawnfd::command(bin.join("trustc")).args(["--print", "sysroot"]),
        "Trust sysroot",
    )
}

/// The one host target the selected Trust `bin`'s `trustc -vV` names ([`output_host`]).
/// # Errors
/// `trustc` cannot run, fails, or names no one valid host.
#[cfg(target_os = "macos")]
pub fn trust_host(bin: &Path) -> Result<String, String> {
    output_host(
        aterm_uds::spawnfd::command(bin.join("trustc")).arg("-vV"),
        "Trust host",
    )
}

/// The Trust toolchain roots of the selected `bin` (canonical), given the `sysroot` and `host`
/// it names (`trust_sysroot`, `trust_host`, on macOS): that `bin` ([`TRUST_SELECTED`]), the
/// sysroot's `bin`, and the host's libraries ([`rust_library_roots`]). A caller resolves the two
/// first, in the order its errors and its other resolutions need; this runs no program and reads
/// only the sysroot's `lib`.
/// # Errors
/// What [`rust_library_roots`] refuses for `sysroot` and `host`.
pub fn trust_roots(bin: PathBuf, sysroot: &Path, host: &str) -> Result<Vec<Root>, String> {
    let mut roots = vec![
        (TRUST_SELECTED.to_string(), bin),
        ("trust/bin".to_string(), sysroot.join("bin")),
    ];
    roots.extend(rust_library_roots("trust", sysroot, &[host])?);
    Ok(roots)
}

/// The macOS SDK's path, as `xcrun --sdk macosx --show-sdk-path` names it.
/// # Errors
/// `xcrun` cannot run, fails, or prints no absolute path.
#[cfg(target_os = "macos")]
pub fn macos_sdk() -> Result<PathBuf, String> {
    output_path(
        xcrun().args(["--sdk", "macosx", "--show-sdk-path"]),
        "macOS SDK",
    )
}

/// The macOS SDK at `sdk` ([`SDK`], from [`macos_sdk`]), `/usr/bin`'s drivers, and the Apple
/// toolchain `bin` and `lib` trees that `clang`, `ld`, `dsymutil`, `strip` and `lipo` resolve to,
/// labelled.
/// # Errors
/// `xcrun` cannot name a tool, or a tool's tree cannot be resolved.
#[cfg(target_os = "macos")]
pub fn apple_roots(sdk: PathBuf) -> Result<Vec<Root>, String> {
    use std::collections::BTreeMap;
    let mut roots = vec![(SDK.to_string(), sdk)];
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
            xcrun().args(["--sdk", "macosx", "--find", tool]),
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

/// `/usr/bin/xcrun` with only the system's `PATH`, run from `/`.
#[cfg(target_os = "macos")]
fn xcrun() -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/xcrun");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir("/");
    command
}

/// Compiler runtime libraries and the target libraries this build can use.
/// rustup's manifests and unrelated installed targets are installation state,
/// not inputs to either slice of the universal application.
/// # Errors
/// No target is named, a target name is not one, a selected target's libraries are missing,
/// `root`'s `lib` cannot be read, or a name in it is not UTF-8.
pub fn rust_library_roots(label: &str, root: &Path, targets: &[&str]) -> Result<Vec<Root>, String> {
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

fn valid_rust_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= 128
        && target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The one host target a compiler's `-vV` names, run by `command`.
/// # Errors
/// It cannot run, fails, or names no one valid host.
#[cfg(target_os = "macos")]
pub fn output_host(
    command: &mut aterm_uds::spawnfd::Command,
    what: &str,
) -> Result<String, String> {
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

/// The absolute path `command` prints.
/// # Errors
/// It cannot run, fails, or prints no absolute path.
#[cfg(target_os = "macos")]
pub fn output_path(
    command: &mut aterm_uds::spawnfd::Command,
    what: &str,
) -> Result<PathBuf, String> {
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
const MAX_ENTRIES: usize = 2_000_000;

/// The content digest of labeled `roots` under the walk schema `walk`, with no seal: what a
/// take records as its digest, by the roots' logical names and content, independent of absolute
/// placement (but for the target text an absolute link below contributes). Files, directories,
/// modes and resolved symlink contents contribute; a symlink beneath a root that dangles (the
/// Command Line Tools' `usr/bin/crashlog`), or that resolves to a directory the walk is inside
/// (the SDK's `Ruby.framework/…/Headers/ruby/ruby` is `.`), contributes its target text instead,
/// as it has no content of its own to add; timestamps do not. Root labels must be unique. A
/// caller's test vector holds its walk schema to it.
/// # Errors
/// What a take's walk refuses (`walk_roots`, whose `stopped` never holds here): no roots, a label
/// empty or repeated, a root missing or dangling, a root or descendant that cannot be resolved (a
/// link loop included), inspected or read, or is a special file, nesting past 256 levels or more
/// than [`MAX_ENTRIES`] entries, a real directory reached again inside itself, or a file or
/// directory changed while its content was read.
#[cfg(any(test, debug_assertions))]
pub fn content_digest(walk: &[u8], roots: &[(&str, &Path)]) -> Result<String, String> {
    walk_roots(
        walk,
        roots,
        Some(Reads {
            open: &mut crate::metadata_io::open_regular,
            list: &mut list_names,
        }),
        &|| false,
    )?
    .digest
    .ok_or_else(|| "the build input walk read no content".into())
}

/// `dir`'s names, as the walk lists them.
fn list_names(dir: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// `roots` as the walk takes them.
fn borrowed(roots: &[Root]) -> Vec<(&str, &Path)> {
    roots
        .iter()
        .map(|(label, path)| (label.as_str(), path.as_path()))
        .collect()
}

/// What one walk of the roots found.
struct Walked {
    /// The content digest, when the walk read the files.
    digest: Option<String>,
    /// Each root's status digest and entries, in label order.
    roots: Vec<RootStat>,
    /// The newest status-change time recorded across the roots ([`StatDigest::newest_change_ns`]).
    newest_change_ns: Option<i128>,
}

struct RootStat {
    /// Its [`StatDigest`], lowercase hex.
    stat: String,
    entries: u64,
}

/// Walk labeled roots once: always each root's status digest ([`StatDigest`]: each entry's own
/// status, a link's target text and what it resolves to, a directory's names), and, when `reads`
/// is given, the content digest over them all under the walk schema `walk`, its first field
/// (their logical names and content, independent of absolute placement but for the target text an
/// absolute link below contributes). Files, directories, modes and resolved symlink contents
/// contribute to the content; a symlink beneath a root that dangles (the Command Line Tools'
/// `usr/bin/crashlog`), or that resolves to a directory the walk is inside (the SDK's
/// `Ruby.framework/…/Headers/ruby/ruby` is `.`), contributes its target text instead, as it has
/// no content of its own to add; timestamps do not. A file is held to the status taken before it
/// is read, by its descriptor's status after the read, and a directory's listing to its status
/// before the listing; without `reads` no file is opened. `stopped` is asked before each entry and
/// between a file's chunks. Root labels must be unique.
/// # Errors
/// There are no roots; a label is empty or repeated; a root is missing or dangles; a root or
/// descendant cannot be resolved (a link loop included), inspected or read, or is a special
/// file; nesting passes 256 levels or the walk passes [`MAX_ENTRIES`] entries; a real
/// directory is reached again inside itself, which only a link above it (to the root's parent,
/// say) can do; a file or directory changed while its content was read; or `stopped` held.
fn walk_roots(
    walk_schema: &[u8],
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
        field(&mut digest, walk_schema);
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

fn field(digest: &mut aterm_digest::Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// What one root's walk feeds: the content digest with the reads that feed it (`None` in a
/// recheck, which reads no file), and the root's status digest.
struct Sink<'a, 'r> {
    stopped: &'a dyn Fn() -> bool,
    content: Option<(
        &'a mut aterm_digest::Sha256,
        &'a mut Reads<'r>,
        &'a mut [u8; 64 * 1024],
    )>,
    stat: StatDigest,
}

impl Sink<'_, '_> {
    fn field(&mut self, bytes: &[u8]) {
        if let Some((digest, _, _)) = &mut self.content {
            field(digest, bytes);
        }
    }
}

fn changed_while_hashed(path: &Path) -> String {
    format!(
        "build input changed while it was hashed: {}",
        path.display()
    )
}

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
#[path = "input_seal_tests.rs"]
mod tests;
