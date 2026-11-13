// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The verify-and-stage chain.

use std::collections::BTreeMap;
use std::io::Read;
#[cfg(all(test, unix))]
#[path = "install_cancel_tests.rs"]
mod cancel_tests;
use std::path::{Component, Path, PathBuf};

use crate::extract::{
    ExtractError, ExtractOptions, FrameChecksum, TreeAccumulator, extract_tar_gz_tree_from,
    extract_tar_zst_tree_from, extract_zip_tree,
};
use crate::manifest::{Artifact, DigestAlg};
use crate::tree::{DeclaredModes, tree_root, tree_root_declared};

const DISK_REVERIFY_ENV: &str = "ATPKG_STAGE_DISK_REVERIFY";

/// Why staging a downloaded bundle failed.
#[derive(Debug)]
pub enum StageError {
    Io(std::io::Error),
    DigestMismatch {
        alg: DigestAlg,
        expected: String,
        got: String,
    },
    Extract(ExtractError),
    TreeRootMismatch {
        expected: String,
        got: String,
    },
    Payload(String),
    DmgAppCount(usize),
    SignerRefused(String),
    VerifyDeferred(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::Io(e) => write!(f, "io: {e}"),
            StageError::DigestMismatch { alg, expected, got } => {
                write!(f, "asset {alg} mismatch: expected {expected}, got {got}")
            }
            StageError::Extract(e) => write!(f, "extract: {e}"),
            StageError::TreeRootMismatch { expected, got } => {
                write!(f, "tree_root mismatch: expected {expected}, got {got}")
            }
            StageError::Payload(m) => write!(f, "payload: {m}"),
            StageError::DmgAppCount(count) => {
                write!(
                    f,
                    "payload: dmg image root must hold exactly one .app, found {count}"
                )
            }
            StageError::SignerRefused(m) => write!(f, "signer refused: {m}"),
            StageError::VerifyDeferred(m) => write!(f, "signature check deferred: {m}"),
        }
    }
}

impl std::error::Error for StageError {}

/// A stage's pre-swap hook: examines the verified `.incoming` tree.
pub(crate) type PreSwapHook<'a> = &'a dyn Fn(&Path) -> Result<(), StageError>;

/// A stage's sidecar hook: the `(suffix, bytes)` sidecars to write beside the build, given
/// the folded `tree_root`.
pub(crate) type SidecarHook<'a> =
    &'a dyn Fn(&str) -> Result<Vec<(&'static str, Vec<u8>)>, StageError>;

/// A stage's npm manifest hook: holds the `package/package.json` an npm binary package
/// carries ([`crate::vendor::NPM_BROTLI_BINARY`]) to the version document that vouched for it.
pub(crate) type NpmPackageHook<'a> = &'a dyn Fn(&[u8]) -> Result<(), StageError>;

/// Caller steps inside [`verify_and_stage`].
#[derive(Clone, Copy, Default)]
pub struct StageHooks<'a> {
    /// Run on the `.incoming` tree after the digest and `tree_root` checks, before anything
    /// flushes or publishes it.
    pub pre_swap: Option<PreSwapHook<'a>>,
    /// Asked once the tree is verified.
    pub sidecars: Option<SidecarHook<'a>>,
    /// `(name, target)` tool names laid in the verified `.incoming` tree before
    /// [`Self::pre_swap`] sees it.
    pub aliases: &'a [(&'a str, &'a str)],
    /// Asked with an npm binary package's `package.json`; that lane refuses to stage without it.
    pub npm_package: Option<NpmPackageHook<'a>>,
}

impl StageHooks<'_> {
    /// No hooks.
    pub const NONE: StageHooks<'static> = StageHooks {
        pre_swap: None,
        sidecars: None,
        aliases: &[],
        npm_package: None,
    };
}

fn io_step(step: &str, e: std::io::Error) -> StageError {
    let mut m = String::from(step);
    m.push_str(": ");
    m.push_str(&e.to_string());
    StageError::Io(std::io::Error::new(e.kind(), m))
}

/// The download-integrity gate: whether `file` holds the bytes `artifact`'s signed digest names.
pub(crate) fn check_signed_digest(artifact: &Artifact, file: &Path) -> Result<(), StageError> {
    let (alg, expected) = artifact.digest().unwrap_or((DigestAlg::Sha256, ""));
    let got = crate::tree::file_digest(file, alg).map_err(|e| io_step("read the archive", e))?;
    if expected.is_empty() || !got.eq_ignore_ascii_case(expected) {
        return Err(StageError::DigestMismatch {
            alg,
            expected: expected.to_string(),
            got,
        });
    }
    Ok(())
}

fn size_cap(artifact: &Artifact) -> u64 {
    let signed = artifact.cost.disk_installed;
    if signed == 0 {
        UNSIGNED_SIZE_CAP
    } else {
        signed.saturating_mul(2).max(1 << 20)
    }
}

/// The most entries a payload may hold: a tar-bomb guard well above any real toolchain bundle.
pub(crate) const MAX_ENTRIES: u64 = 4_000_000;

/// Extraction ceiling when no signed installed-size estimate is available.
pub(crate) const UNSIGNED_SIZE_CAP: u64 = 2u64 << 30;

const STAGING_SCRATCH_MODE: u32 = 0o700;

#[derive(Clone, Copy)]
enum Purpose {
    Install(Option<aterm_toolchain::gnu_runtime::Host>),
    Publication,
}

impl Purpose {
    fn install() -> Result<Self, StageError> {
        aterm_toolchain::gnu_runtime::host()
            .map(Self::Install)
            .map_err(StageError::Payload)
    }

    fn admit(self, artifact: &Artifact, incoming: &Path) -> Result<(), StageError> {
        use aterm_toolchain::gnu_runtime;
        let declared = if artifact.kind == "elf-binary" {
            let arch = artifact
                .target
                .strip_suffix("-unknown-linux-gnu")
                .and_then(gnu_runtime::machine_for_arch)
                .ok_or_else(|| {
                    StageError::Payload("elf-binary requires a supported GNU Linux target".into())
                })?;
            Some(arch)
        } else {
            None
        };
        let host = match self {
            Self::Install(host) => host,
            Self::Publication => None,
        };
        if declared.is_some() && matches!(self, Self::Install(None)) {
            return Err(StageError::Payload(
                "a GNU ELF artifact cannot be made ready on a different runtime".into(),
            ));
        }
        if let (Some(declared), Some(host)) = (declared, host)
            && declared != host.machine
        {
            return Err(StageError::Payload(
                "a GNU ELF artifact cannot be made ready for another CPU architecture".into(),
            ));
        }
        if let Some(machine) = host.map(|host| host.machine).or(declared) {
            let tree = gnu_runtime::inspect_tree(incoming, machine)
                .map_err(|why| StageError::Payload(why.to_string()))?;
            if declared.is_some() && tree.objects == 0 {
                return Err(StageError::Payload(
                    "elf-binary contains no runtime ELF object".into(),
                ));
            }
            if let Some(host) = host
                && let Some(required) = tree.glibc.filter(|required| *required > host.glibc)
            {
                return Err(StageError::Payload(format!(
                    "requires GNU libc {required}; this host has {}",
                    host.glibc
                )));
            }
        }
        Ok(())
    }
}

/// Verify a downloaded `archive` against the signed `artifact` and stage it at `build_dir`.
pub fn verify_and_stage(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
    aterm_update_core::check_operation().map_err(StageError::Io)?;
    verify_stage(artifact, archive, build_dir, hooks, Purpose::install()?)
}

/// Verify and extract a publisher's scratch copy, including `elf-binary`'s declared GNU target,
/// without claiming it can run on the publication host. This never writes a ready marker.
/// The destination must be fresh so publication inspection cannot revoke a live install.
///
/// # Errors
/// A sidecar hook is supplied, the destination is occupied, or digest, tree, payload or ELF
/// inspection refuses it.
pub fn verify_for_publication(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
    if hooks.sidecars.is_some() {
        return Err(StageError::Payload(
            "publication inspection writes no install sidecars".into(),
        ));
    }
    publication_destination(build_dir)?;
    verify_stage(artifact, archive, build_dir, hooks, Purpose::Publication)
}

fn publication_destination(build_dir: &Path) -> Result<(), StageError> {
    let marker = crate::store::ready_marker_path(build_dir).ok_or_else(|| {
        StageError::Payload("publication staging needs a named destination".into())
    })?;
    for path in [build_dir, marker.as_path()] {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StageError::Io(error)),
            Ok(_) => {
                return Err(StageError::Payload(
                    "publication staging needs a fresh destination".into(),
                ));
            }
        }
    }
    Ok(())
}

fn verify_stage(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
    purpose: Purpose,
) -> Result<String, StageError> {
    check_signed_digest(artifact, archive)?;

    let incoming = match purpose {
        Purpose::Install(_) => open_incoming(build_dir)?,
        Purpose::Publication => create_incoming(build_dir)?,
    };
    let (extracted_root, modes) =
        match stage_payload_modes(artifact, archive, &incoming, hooks.npm_package) {
            Ok(staged) => staged,
            Err(e) => {
                let _ = crate::store::remove_tree(&incoming);
                return Err(e);
            }
        };
    stage_extracted(
        artifact,
        build_dir,
        &incoming,
        extracted_root,
        modes,
        hooks,
        purpose,
    )
}

/// [`verify_and_stage`] from its tree-root check on, for the tree extracted at `incoming` that
/// folded to `extracted_root`.
fn stage_extracted(
    artifact: &Artifact,
    build_dir: &Path,
    incoming: &Path,
    extracted_root: String,
    modes: DeclaredModes,
    hooks: &StageHooks<'_>,
    purpose: Purpose,
) -> Result<String, StageError> {
    let root = if artifact.tree_root.is_empty() {
        extracted_root
    } else {
        let got = match reverified_root(incoming, extracted_root, disk_reverify_armed(), &modes) {
            Ok(r) => r,
            Err(e) => {
                let _ = crate::store::remove_tree(incoming);
                return Err(io_step("re-walk the staged tree", e));
            }
        };
        if !got.eq_ignore_ascii_case(&artifact.tree_root) {
            let _ = crate::store::remove_tree(incoming);
            return Err(StageError::TreeRootMismatch {
                expected: artifact.tree_root.clone(),
                got,
            });
        }
        got
    };
    finish_stage(
        artifact,
        build_dir,
        incoming,
        root,
        modes,
        hooks,
        (None, purpose),
    )
}

/// Whether `artifact`'s payload lane extracts a tar stream, so its download can stage as it
/// arrives ([`extract_streamed`]).
pub(crate) fn streams(artifact: &Artifact) -> bool {
    matches!(artifact.payload.as_str(), "" | "tar-zst" | "tar-gz")
}

/// A build's staging scratch that a download stages into as it arrives
/// ([`crate::flow::arriving`]): removed on drop, unless its tree went on to the stage
/// ([`finish_streamed`]).
pub(crate) struct Incoming(PathBuf);

impl Incoming {
    /// The scratch directory.
    pub(crate) fn dir(&self) -> &Path {
        &self.0
    }

    /// The tree extracted here, folded to `root` with `modes`, its archive matched to the
    /// signed digest.
    pub(crate) fn staged(self, root: String, modes: DeclaredModes) -> Streamed {
        Streamed {
            incoming: self,
            root,
            modes,
        }
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = crate::store::remove_tree(&self.0);
        }
    }
}

/// A fresh staging scratch for `build_dir`, made as [`verify_and_stage`] makes one.
pub(crate) fn open_streamed(build_dir: &Path) -> Option<Incoming> {
    open_incoming(build_dir).ok().map(Incoming)
}

/// Lay `artifact`'s tar down under `dest` from `input`, as [`stage_payload_modes`] lays it from
/// a downloaded file, and return the `tree_root` and declared modes of what was laid.
pub(crate) fn extract_streamed(
    artifact: &Artifact,
    input: impl Read + Send,
    dest: &Path,
) -> Result<(String, DeclaredModes), StageError> {
    let tree = tar_lane(artifact, input, dest)
        .unwrap_or_else(|| Err(payload2("no stream lane for payload: ", &artifact.payload)))?;
    fold_staged(artifact, dest, Some(tree))
}

/// A tree a download staged as it arrived, every byte of the download matched to the row's
/// signed digest ([`crate::flow::arriving::fetch_staging`]).
pub(crate) struct Streamed {
    incoming: Incoming,
    root: String,
    modes: DeclaredModes,
}

impl Streamed {
    /// The scratch the tree was laid in.
    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        self.incoming.dir()
    }
}

/// [`verify_and_stage`] for a [`Streamed`] tree, whose archive digest already matched: the
/// stage goes on from its tree-root check, at `build_dir`.
pub(crate) fn finish_streamed(
    artifact: &Artifact,
    streamed: Streamed,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
    let Streamed {
        mut incoming,
        root,
        modes,
    } = streamed;
    let dir = std::mem::take(&mut incoming.0);
    stage_extracted(
        artifact,
        build_dir,
        &dir,
        root,
        modes,
        hooks,
        Purpose::install()?,
    )
}

/// Sweep `build_dir`'s stage scratch and make a fresh, private `.incoming` beside it.
fn open_incoming(build_dir: &Path) -> Result<PathBuf, StageError> {
    crate::store::sweep_stage_scratch(build_dir);
    create_incoming(build_dir)
}

fn create_incoming(build_dir: &Path) -> Result<PathBuf, StageError> {
    let incoming = crate::store::incoming_dir(build_dir).ok_or_else(|| {
        StageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "build dir has no name",
        ))
    })?;
    crate::store::make_build_parent(build_dir)
        .and_then(|()| crate::platform::make_dir_born(&incoming, STAGING_SCRATCH_MODE))
        .map_err(|e| io_step("create the staging scratch", e))?;
    Ok(incoming)
}

/// What [`stage_from_tree`] came to when no step of the stage refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FromTree {
    /// The copy is the build now: the root the stage folded (its aliases' included).
    Staged(String),
    /// Nothing was laid at the build, for this reason (naming an entry by its path in the tree,
    /// and a scratch copy by its own only when it could not be removed): the caller downloads.
    Drift(String),
}

/// Stage at `build_dir`, in place of a download of `artifact`, a copy of `src`: another row's
/// complete build of the same signed bytes, whose record folds its tree to `src_root`
/// ([`crate::flow::land_from_tree`]). The copy is laid in a fresh `.incoming`, each file a clone
/// ([`crate::clone::clone_file`]: no new blocks on APFS, a byte copy elsewhere, and its own
/// inode either way, so nothing done to one tree reaches the other) of a file that carries
/// nothing an extraction would not lay ([`lays_as_extracted`]), each directory made as
/// extraction makes one, each symlink with its raw target. In place of the archive's digest
/// check, the copy must fold to `src_root` and hold no executable a stage would demote
/// ([`crate::macho::foreign_executables`]); `laid` runs once it does, and the stage goes on as
/// [`verify_and_stage`]'s does from its aliases: `hooks`, the re-fold, the sidecars, the swap
/// and the marker.
///
/// # Errors
/// That tail's, as a download's stage returns them. Anything short of it is a
/// [`FromTree::Drift`], with the scratch removed (or the reason saying it could not be) and
/// nothing at `build_dir` touched; off Unix, where no clone is laid, it is always one.
pub(crate) fn stage_from_tree(
    src: &Path,
    src_root: &str,
    artifact: &Artifact,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
    laid: &dyn Fn(),
) -> Result<FromTree, StageError> {
    if !cfg!(unix) {
        return Ok(FromTree::Drift(String::from(
            "this platform lays no clone of a tree",
        )));
    }
    let Ok(incoming) = open_incoming(build_dir) else {
        return Ok(FromTree::Drift(String::from(
            "no scratch for its copy could be made",
        )));
    };
    let drift = |mut why: String| {
        if let Err(e) = crate::store::remove_tree(&incoming) {
            use std::fmt::Write as _;
            let _ = write!(
                why,
                "; its scratch copy {} could not be removed ({e})",
                incoming.display()
            );
        }
        Ok(FromTree::Drift(why))
    };
    let modes = match crate::store::declared_modes(src) {
        Ok(modes) => modes,
        Err(e) => return drift(format!("its declared modes do not read ({})", e.kind())),
    };
    if let Err(why) = clone_tree(src, &incoming, Path::new("")) {
        return drift(why);
    }
    let root = match tree_root_declared(&incoming, &modes) {
        Ok(root) if root.eq_ignore_ascii_case(src_root) => root,
        Ok(_) => {
            return drift(String::from(
                "its copy does not fold to the root its record names",
            ));
        }
        Err(e) => return drift(format!("its copy could not be folded ({})", e.kind())),
    };
    match crate::macho::foreign_executables(&incoming) {
        Ok(foreign) if foreign.is_empty() => {}
        Ok(foreign) => {
            let rel = foreign[0].strip_prefix(&incoming).unwrap_or(&foreign[0]);
            return drift(format!(
                "{} is executable and not a native executable",
                rel.to_string_lossy()
            ));
        }
        Err(_) => return drift(String::from("its copy's executables could not be read")),
    }
    laid();
    finish_stage(
        artifact,
        build_dir,
        &incoming,
        root,
        modes,
        hooks,
        (None, Purpose::install()?),
    )
    .map(FromTree::Staged)
}

/// A local authority's held binary uses the ordinary sealed, durable stage tail, never a swap.
/// No executable is run here; the caller binds its bytes through the mandatory pre-swap hook.
#[cfg(unix)]
pub(crate) fn stage_local_binary(
    parent: &crate::local_harness::fs::Parent,
    artifact: &Artifact,
    input: std::fs::File,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
    if artifact.payload != "raw-binary"
        || artifact.size == 0
        || crate::store::ToolName::new(&artifact.entry).is_none()
        || hooks.pre_swap.is_none()
        || !hooks.aliases.is_empty()
    {
        return Err(payload2(
            "a local binary needs its byte gate and raw, unaliased layout",
            "",
        ));
    }
    let incoming = crate::store::incoming_dir(build_dir)
        .ok_or_else(|| payload2("local build has no name", ""))?;
    let local = crate::local_harness::fs::Stage::new(parent, build_dir, artifact)
        .map_err(StageError::Io)?;
    let tool = crate::store::ToolName::new(&artifact.entry)
        .ok_or_else(|| payload2("invalid local entry: ", &artifact.entry))?;
    let mut tree = TreeAccumulator::new();
    let target = incoming.join("bin").join(tool.exe_file());
    let written = crate::extract::stage_open_file(
        input,
        local.file.try_clone().map_err(StageError::Io)?,
        &target,
        0o755,
        artifact.size,
    )
    .map_err(StageError::Extract)?;
    tree.record_file(
        crate::extract::rel_bytes_under(&incoming, &target).map_err(StageError::Extract)?,
        written.mode,
        written.content_sha_hex,
    );
    let modes = tree.declared_modes();
    finish_stage(
        artifact,
        build_dir,
        &incoming,
        tree.root(),
        modes,
        hooks,
        (Some(&local), Purpose::install()?),
    )
}

/// Lay under the empty directory `to` a copy of everything under `from` (at `rel` in the tree):
/// each regular file a clone once [`lays_as_extracted`] passes it, each directory made at the
/// mode extraction gives one, each symlink with its raw target (the stage that laid `from`
/// admitted it). Anything else is refused. The error is why, naming the entry by its `rel`.
#[cfg(unix)]
fn clone_tree(from: &Path, to: &Path, rel: &Path) -> Result<(), String> {
    let failed = |at: &Path, e: std::io::Error| {
        if at.as_os_str().is_empty() {
            format!("its tree could not be read ({})", e.kind())
        } else {
            format!("{} could not be copied ({})", at.display(), e.kind())
        }
    };
    for entry in std::fs::read_dir(from).map_err(|e| failed(rel, e))? {
        let entry = entry.map_err(|e| failed(rel, e))?;
        let at = rel.join(entry.file_name());
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| failed(&at, e))?;
        if kind.is_symlink() {
            std::fs::read_link(&src)
                .and_then(|target| crate::extract::create_symlink(&target, &dst))
                .map_err(|e| failed(&at, e))?;
        } else if kind.is_dir() {
            std::fs::create_dir(&dst)
                .and_then(|()| crate::platform::set_mode(&dst, crate::extract::safe_mode(0, true)))
                .map_err(|e| failed(&at, e))?;
            clone_tree(&src, &dst, &at)?;
        } else if kind.is_file() {
            lays_as_extracted(&src, &at)?;
            crate::clone::clone_file(&src, &dst).map_err(|e| failed(&at, e))?;
        } else {
            return Err(format!(
                "{} is neither a file, a directory nor a symlink",
                at.display()
            ));
        }
    }
    Ok(())
}

/// Off Unix no clone is laid ([`stage_from_tree`] never asks).
#[cfg(not(unix))]
fn clone_tree(_from: &Path, _to: &Path, _rel: &Path) -> Result<(), String> {
    Err(String::from("this platform lays no clone of a tree"))
}

/// The extended attributes macOS itself puts on a file a stage's extraction writes: its
/// provenance, a quarantine and a last-use date, none of which the Developer ID gate refuses.
#[cfg(target_os = "macos")]
const EXTRACTION_XATTRS: [&[u8]; 3] = [
    b"com.apple.provenance",
    b"com.apple.quarantine",
    b"com.apple.lastuseddate#PS",
];

/// Refuse the file at `src` (`rel` in its tree) when a clone of it would not be what an
/// extraction lays, for a clone keeps both a file's BSD flags and its extended attributes:
/// flags that bind its owner ([`crate::platform::OWNER_BINDING_FLAGS`]: no cleanup could remove
/// the copy, and a build is never laid locked), or an extended attribute outside
/// [`EXTRACTION_XATTRS`] (Finder information or a resource fork fails the Developer ID gate's
/// strict check although the tree's root, which folds neither, still matches). Nothing is
/// written to either file: a lender that carries one is not copied, and its bytes are
/// downloaded as they are for a row with no lender.
#[cfg(target_os = "macos")]
fn lays_as_extracted(src: &Path, rel: &Path) -> Result<(), String> {
    use std::os::macos::fs::MetadataExt as _;
    let unread = |e: std::io::Error| format!("{} could not be read ({})", rel.display(), e.kind());
    let meta = std::fs::symlink_metadata(src).map_err(unread)?;
    if meta.st_flags() & crate::platform::OWNER_BINDING_FLAGS != 0 {
        return Err(format!("{} is locked against change", rel.display()));
    }
    let names = crate::platform::xattr_names(src).map_err(unread)?;
    if let Some(name) = names
        .iter()
        .find(|name| !EXTRACTION_XATTRS.contains(&name.as_slice()))
    {
        return Err(format!(
            "{} carries {}, which no extraction lays",
            rel.display(),
            String::from_utf8_lossy(name)
        ));
    }
    Ok(())
}

/// Off macOS a clone is `std::fs::copy`'s, which keeps a file's permission bits and neither its
/// flags nor its extended attributes: what an extraction lays.
#[cfg(all(unix, not(target_os = "macos")))]
fn lays_as_extracted(_src: &Path, _rel: &Path) -> Result<(), String> {
    Ok(())
}

/// The rest of a stage once its verified tree is laid in `incoming` with `modes` and folds to
/// `root`: the aliases, the pre-swap hook, the re-fold over what they joined, the sidecars, the
/// build's mode, the flush, the swap into `build_dir`, the declared modes and the marker.
fn finish_stage(
    artifact: &Artifact,
    build_dir: &Path,
    incoming: &Path,
    root: String,
    mut modes: DeclaredModes,
    hooks: &StageHooks<'_>,
    #[cfg(unix)] tail: (Option<&crate::local_harness::fs::Stage<'_>>, Purpose),
    #[cfg(not(unix))] tail: (Option<()>, Purpose),
) -> Result<String, StageError> {
    let (local, purpose) = tail;
    #[cfg(not(unix))]
    let _ = local;
    #[cfg(unix)]
    if let Some(local) = local {
        return finish_local_stage(incoming, root, hooks, local, purpose);
    }
    if let Err(e) = lay_aliases(incoming, hooks.aliases, &mut modes) {
        let _ = crate::store::remove_tree(incoming);
        return Err(e);
    }
    if let Some(pre_swap) = hooks.pre_swap
        && let Err(e) = pre_swap(incoming)
    {
        let _ = crate::store::remove_tree(incoming);
        return Err(e);
    }
    if let Err(error) = purpose.admit(artifact, incoming) {
        let _ = crate::store::remove_tree(incoming);
        return Err(error);
    }
    let root = if hooks.aliases.is_empty() {
        root
    } else {
        match tree_root_declared(incoming, &modes) {
            Ok(root) => root,
            Err(e) => {
                let _ = crate::store::remove_tree(incoming);
                return Err(io_step("fold the tree its aliases joined", e));
            }
        }
    };
    let sidecars = match hooks.sidecars.map(|render| render(&root)).transpose() {
        Ok(sidecars) => sidecars.unwrap_or_default(),
        Err(e) => {
            let _ = crate::store::remove_tree(incoming);
            return Err(e);
        }
    };
    if let Some((suffix, _)) = sidecars
        .iter()
        .find(|(suffix, _)| !crate::store::STAGE_SIDECAR_SUFFIXES.contains(suffix))
    {
        let _ = crate::store::remove_tree(incoming);
        return Err(payload2(
            "sidecar suffix the store does not reclaim: ",
            suffix,
        ));
    }

    if let Err(e) = crate::platform::set_mode(incoming, crate::extract::safe_mode(0, true)) {
        let _ = crate::store::remove_tree(incoming);
        return Err(io_step("give the staged tree the build's mode", e));
    }
    if crate::vendor::stages_one_binary(&artifact.payload)
        && let Err(e) = crate::store::seal_bin(incoming)
    {
        let _ = crate::store::remove_tree(incoming);
        return Err(io_step("seal the build's bin", e));
    }
    if let Err(e) = crate::store::sync_tree(incoming) {
        let _ = crate::store::remove_tree(incoming);
        return Err(io_step("flush the staged tree", e));
    }

    if matches!(purpose, Purpose::Publication) {
        let published = publication_destination(build_dir).and_then(|()| {
            crate::platform::rename_noreplace(incoming, build_dir)
                .map_err(|error| io_step("publish the fresh inspection tree", error))
        });
        if let Err(error) = published {
            let _ = crate::store::remove_tree(incoming);
            return Err(error);
        }
        if let Some(parent) = build_dir.parent() {
            crate::store::sync_dir_ordered(parent)
                .map_err(|error| io_step("flush the inspection tree's parent", error))?;
        }
        return Ok(root);
    }

    // Keep the original native gate through every sidecar and Ready write.
    // GC must not reclaim the unready tree between rename and publication.
    let _sdk = match swap_into_place(build_dir, incoming) {
        Ok(guard) => guard,
        Err(e) => {
            let _ = crate::store::remove_tree(incoming);
            return Err(io_step("swap the staged tree into place", e));
        }
    };

    if !sidecars.is_empty() {
        for (suffix, bytes) in &sidecars {
            crate::store::write_sidecar_durably(build_dir, suffix, bytes)
                .map_err(|e| io_step("record a sidecar beside the build", e))?;
        }
        // Required: the sidecars reach the media ahead of the marker that vouches for the
        // build, so a failed flush stops the stage before it (the build reads as not installed).
        if let Some(parent) = build_dir.parent() {
            crate::store::sync_dir_ordered(parent)
                .map_err(|e| io_step("order the sidecars ahead of the build's marker", e))?;
        }
    }

    if let Err(e) = crate::store::write_declared_modes(build_dir, &modes) {
        return Err(io_step("record the declared modes beside the build", e));
    }
    // The signed root this stage verified, bound to the tree now in place: a tree's trust.lock
    // names its build by it (`crate::trust_lock`). The fold of a tree its aliases joined is not
    // the signed root, so only an alias-free stage of a signed root records one.
    if !artifact.tree_root.is_empty()
        && hooks.aliases.is_empty()
        && let Err(e) = crate::trust_lock::write_tree_root_record(build_dir, &root)
    {
        return Err(io_step("record the tree root beside the build", e));
    }

    if let Purpose::Install(host) = purpose
        && let Err(e) = crate::store::mark_build_ready_on(build_dir, host)
    {
        return Err(io_step("mark the build complete", e));
    }
    // Accepted, failed or not: the marker is the stage's last write, and one a power cut
    // takes back reads as not installed, which the next install stages again.
    if let Some(parent) = build_dir.parent() {
        let _marked = crate::store::sync_dir_ordered(parent);
    }
    Ok(root)
}

/// The same local hook/metadata/seal/order/ready sequence, all mutations held by descriptors.
/// Failures before Ready retain an unready attempt without cleaning a name by path. An error
/// after Ready may leave a complete candidate; observe and exactly validate it before retrying.
#[cfg(unix)]
fn finish_local_stage(
    incoming: &Path,
    root: String,
    hooks: &StageHooks<'_>,
    local: &crate::local_harness::fs::Stage<'_>,
    purpose: Purpose,
) -> Result<String, StageError> {
    if let Some(verify) = hooks.pre_swap {
        verify(incoming)?;
    }
    if let Purpose::Install(Some(host)) = purpose {
        aterm_toolchain::gnu_runtime::admit_open_file(&local.file, host)
            .map_err(|why| StageError::Payload(why.to_string()))?;
    }
    let sidecars = hooks
        .sidecars
        .map(|render| render(&root))
        .transpose()?
        .unwrap_or_default();
    if [
        crate::local_harness::SUFFIX,
        crate::store::VERSION_SUFFIX,
        crate::store::EXPOSES_SUFFIX,
    ]
    .iter()
    .any(|required| !sidecars.iter().any(|(suffix, _)| suffix == required))
        || sidecars
            .iter()
            .any(|(suffix, _)| !crate::store::STAGE_SIDECAR_SUFFIXES.contains(suffix))
    {
        return Err(payload2(
            "local stage is missing or has unsupported metadata",
            "",
        ));
    }
    local.seal_sync().map_err(StageError::Io)?;
    local.publish().map_err(StageError::Io)?;
    local.metadata_checkpoint();
    for (suffix, bytes) in sidecars {
        local.sidecar(suffix, &bytes).map_err(StageError::Io)?;
    }
    local.sync().map_err(StageError::Io)?;
    local.ready().map_err(StageError::Io)?;
    Ok(root)
}

fn lay_aliases(
    incoming: &Path,
    aliases: &[(&str, &str)],
    modes: &mut DeclaredModes,
) -> Result<(), StageError> {
    for &(name, target) in aliases {
        let (Some(alias), Some(of)) = (
            crate::store::ToolName::new(name),
            crate::store::ToolName::new(target),
        ) else {
            return Err(payload2(
                "alias: not a tool name pair: ",
                &format!("{name} = {target}"),
            ));
        };
        let from = Path::new("bin").join(of.exe_file());
        let to = Path::new("bin").join(alias.exe_file());
        let shown = |rel: &Path| rel.to_string_lossy().replace('\\', "/");
        if !std::fs::symlink_metadata(incoming.join(&from)).is_ok_and(|m| m.file_type().is_file()) {
            return Err(StageError::Payload(format!(
                "alias {}: {} is not a regular file in the vendor's tree",
                shown(&to),
                shown(&from)
            )));
        }
        if std::fs::symlink_metadata(incoming.join(&to)).is_ok() {
            return Err(StageError::Payload(format!(
                "alias {}: the vendor's tree already has one",
                shown(&to)
            )));
        }
        #[cfg(unix)]
        let laid = crate::clone::clone_file(&incoming.join(&from), &incoming.join(&to));
        #[cfg(not(unix))]
        let laid = std::fs::copy(incoming.join(&from), incoming.join(&to)).map(drop);
        laid.map_err(|e| io_step("lay an alias", e))?;
        if let Some(mode) = modes.get(&crate::tree::rel_line_bytes(&from)).copied() {
            modes.insert(crate::tree::rel_line_bytes(&to), mode);
        }
    }
    Ok(())
}

const DEVELOPER_ID_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// A [`StageHooks::pre_swap`] gate for a darwin agent build.
pub(crate) fn developer_id_gate(
    team: &str,
    exposes: &[&str],
    check: TeamCheck,
) -> impl Fn(&Path) -> Result<(), StageError> + use<> {
    let team = team.to_string();
    let exposes: Vec<String> = exposes.iter().map(|t| (*t).to_string()).collect();
    move |tree: &Path| {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let verify = |path: &Path, deadline: std::time::Instant| check(path, &team, deadline);
        check_developer_id(tree, &team, &exposes, &verify)
    }
}

/// How [`developer_id_gate`] judges one Mach-O against a team by a deadline.
pub(crate) type TeamCheck =
    fn(&Path, &str, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// The real [`TeamCheck`]: `/usr/bin/codesign` under the pinned Developer ID requirement,
/// by the caller's deadline.
pub(crate) fn codesign_team_check(
    path: &Path,
    team: &str,
    deadline: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    aterm_update_core::codesign::verify_developer_id_until(path, team, false, deadline)
}

type VerifyOne<'a> =
    &'a dyn Fn(&Path, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

fn check_developer_id(
    tree: &Path,
    team: &str,
    exposes: &[String],
    verify: VerifyOne<'_>,
) -> Result<(), StageError> {
    if exposes.is_empty() {
        return Err(payload2(
            "developer id gate: no exposed tool to bind for team ",
            team,
        ));
    }
    let mut signer = String::from("Developer ID team ");
    signer.push_str(team);
    check_signed_tree(tree, &signer, exposes, verify)
}

/// A [`StageHooks::pre_swap`] gate for an ALab package of an agent program ([`crate::alab`]).
pub(crate) fn alab_pack_gate(
    exposes: &[String],
    check: StrictCheck,
) -> impl Fn(&Path) -> Result<(), StageError> + use<> {
    let exposes = exposes.to_vec();
    move |tree: &Path| {
        if !crate::alab::is_alab_package(tree) {
            return Err(StageError::Payload(String::from(
                "alab pack gate: the staged tree is not an ALab package (codex-package.json \
                 layout 1 with its entrypoint, and alab-build.json)",
            )));
        }
        let entry = crate::alab::entrypoint_of(tree);
        let shimmed = exposes.iter().any(|name| {
            crate::store::ToolName::new(name).is_some_and(|tool| {
                entry.as_deref() == Some(Path::new("bin").join(tool.exe_file()).as_path())
            })
        });
        if !shimmed {
            return Err(StageError::Payload(String::from(
                "alab pack gate: the package's entrypoint is not what an exposed tool's shim runs",
            )));
        }
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        check_signed_tree(tree, ALAB_SIGNER, &exposes, &|path, deadline| {
            check(path, deadline)
        })
    }
}

/// Run [`alab_pack_gate`] with [`codesign_strict_check`] and verify [`crate::alab::pairing_floor`].
pub fn check_alab_pack(tree: &Path, exposes: &[String]) -> Result<(), StageError> {
    alab_pack_gate(exposes, codesign_strict_check)(tree)?;
    crate::alab::pairing_floor(tree)
        .map(drop)
        .map_err(|why| StageError::Payload(format!("alab pairing: {why}")))
}

const ALAB_SIGNER: &str = "a valid signature (codesign --verify --strict)";

/// How [`alab_pack_gate`] judges one Mach-O by a deadline.
pub(crate) type StrictCheck =
    fn(&Path, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// The real [`StrictCheck`], by the caller's deadline.
pub(crate) fn codesign_strict_check(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    use aterm_update_core::codesign::{CODESIGN, CodesignError, developer_id_verdict};
    if !cfg!(target_os = "macos") {
        return Err(CodesignError::Unsupported);
    }
    let mut cmd = aterm_uds::spawnfd::command(CODESIGN);
    cmd.args(["--verify", "--strict", "--verbose=2"]).arg(path);
    developer_id_verdict(aterm_uds::deadline::output_until(&mut cmd, deadline))
}

fn check_signed_tree(
    tree: &Path,
    signer: &str,
    exposes: &[String],
    verify: VerifyOne<'_>,
) -> Result<(), StageError> {
    check_signed_tree_with_clock(tree, signer, exposes, verify, std::time::Instant::now)
}

fn check_signed_tree_with_clock(
    tree: &Path,
    signer: &str,
    exposes: &[String],
    verify: VerifyOne<'_>,
    now: impl Fn() -> std::time::Instant,
) -> Result<(), StageError> {
    use aterm_update_core::codesign::{CodesignError, VERIFY_TIMEOUT, verify_timeout_for};
    let found = crate::macho::find_macho(tree)?;
    if found.is_empty() {
        let mut m = String::from("no Mach-O in the staged tree; a darwin build must carry ");
        m.push_str("at least one, signed by ");
        m.push_str(signer);
        return Err(StageError::SignerRefused(m));
    }
    for name in exposes {
        let Some(tool) = crate::store::ToolName::new(name) else {
            return Err(payload2("developer id gate: not a tool name: ", name));
        };
        let entry = tree.join("bin").join(tool.exe_file());
        if !found.contains(&entry) {
            let mut m = String::from("bin/");
            m.push_str(&tool.exe_file());
            m.push_str(" is not a Mach-O in the staged tree; its shim would run it unverified");
            return Err(StageError::SignerRefused(m));
        }
    }
    if let Some(stray) = crate::macho::executable_files(tree)?
        .into_iter()
        .find(|p| !found.contains(p))
    {
        let mut m = stray
            .strip_prefix(tree)
            .unwrap_or(&stray)
            .to_string_lossy()
            .into_owned();
        m.push_str(" is executable but not a Mach-O; it would run unverified");
        return Err(StageError::SignerRefused(m));
    }
    let allowance = |path: &Path| {
        let len = std::fs::symlink_metadata(path).map_or(0, |m| m.len());
        verify_timeout_for(len)
    };
    let extra = found
        .iter()
        .map(|p| allowance(p).saturating_sub(VERIFY_TIMEOUT))
        .fold(
            std::time::Duration::ZERO,
            std::time::Duration::saturating_add,
        );
    let far_future = || now() + std::time::Duration::from_secs(365 * 86_400);
    let budget = now()
        .checked_add(DEVELOPER_ID_BUDGET.saturating_add(extra))
        .unwrap_or_else(far_future);
    for path in &found {
        let deadline = budget.min(
            now()
                .checked_add(allowance(path))
                .unwrap_or_else(far_future),
        );
        let rel = path.strip_prefix(tree).unwrap_or(path).to_string_lossy();
        let mut m = String::from(rel.as_ref());
        match verify(path, deadline) {
            Ok(()) => {}
            Err(e) if e.is_verdict() => {
                m.push_str(" is not signed by ");
                m.push_str(signer);
                m.push_str(": ");
                m.push_str(&e.to_string());
                return Err(StageError::SignerRefused(m));
            }
            Err(e @ CodesignError::InvalidTeam(_)) => {
                return Err(payload2("developer id gate: ", &e.to_string()));
            }
            Err(e @ CodesignError::TimedOut) => {
                m.push_str(": ");
                m.push_str(&e.to_string());
                return Err(StageError::VerifyDeferred(m));
            }
            Err(e) => {
                m.push_str(": ");
                m.push_str(&e.to_string());
                return Err(StageError::Io(std::io::Error::other(m)));
            }
        }
    }
    Ok(())
}

/// Lay the verified download at `archive` down under `dest` in the shape `artifact.payload`
/// names, and return the `tree_root` of what was laid down.
pub fn stage_payload(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
) -> Result<String, StageError> {
    stage_payload_modes(artifact, archive, dest, None).map(|(root, _)| root)
}

/// [`stage_payload`], also handing back the permission bits it declared per regular file
/// ([`TreeAccumulator::declared_modes`]).
pub(crate) fn stage_payload_modes(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
    npm_package: Option<NpmPackageHook<'_>>,
) -> Result<(String, DeclaredModes), StageError> {
    let cap = size_cap(artifact);
    let folded: Option<TreeAccumulator> = match artifact.payload.as_str() {
        _ if streams(artifact) => {
            let file = crate::metadata_io::open_regular(archive)
                .map_err(|e| StageError::Extract(ExtractError::from(e)))?;
            tar_lane(artifact, file, dest).transpose()?
        }
        "zip" => Some(
            extract_zip_tree(archive, dest, cap, MAX_ENTRIES, vendor_options(artifact))
                .map_err(StageError::Extract)?,
        ),
        "raw-binary" | "tar-gz-binary" | crate::vendor::NPM_BROTLI_BINARY => {
            Some(stage_binary(artifact, archive, dest, cap, npm_package)?)
        }
        "dmg" => {
            stage_dmg(archive, dest)?;
            None
        }
        other => return Err(payload2("unknown payload lane: ", other)),
    };
    fold_staged(artifact, dest, folded)
}

/// The extraction options of a vendor archive lane (`tar-zst`, `tar-gz`, `zip`).
fn vendor_options(artifact: &Artifact) -> ExtractOptions {
    ExtractOptions {
        strip_components: artifact.strip_components,
        symlinks: crate::extract::SymlinkPolicy::InRoot,
        allow_global_pax: false,
    }
}

/// Extract `artifact`'s tar lane ([`streams`]) from `input` into `dest`, folding its tree;
/// `None` for a payload lane that is not one.
fn tar_lane(
    artifact: &Artifact,
    input: impl Read + Send,
    dest: &Path,
) -> Option<Result<TreeAccumulator, StageError>> {
    let cap = size_cap(artifact);
    let checksum = if artifact.tree_root.is_empty() {
        FrameChecksum::Required
    } else {
        FrameChecksum::IfPresent
    };
    let vendor = vendor_options(artifact);
    let extracted = match artifact.payload.as_str() {
        "" => extract_tar_zst_tree_from(
            input,
            dest,
            cap,
            MAX_ENTRIES,
            ExtractOptions::default(),
            checksum,
        ),
        "tar-zst" => extract_tar_zst_tree_from(input, dest, cap, MAX_ENTRIES, vendor, checksum),
        "tar-gz" => extract_tar_gz_tree_from(input, dest, cap, MAX_ENTRIES, vendor),
        _ => return None,
    };
    Some(extracted.map_err(StageError::Extract))
}

/// The links, spelling check, `tree_root` and declared modes of what [`stage_payload_modes`]
/// laid at `dest` (`folded` when its lane folded the tree as it wrote it).
fn fold_staged(
    artifact: &Artifact,
    dest: &Path,
    mut folded: Option<TreeAccumulator>,
) -> Result<(String, DeclaredModes), StageError> {
    apply_links(dest, &artifact.links, folded.as_mut())?;
    match folded {
        Some(tree) => {
            tree.require_spelled_back(dest)
                .map_err(StageError::Extract)?;
            let modes = tree.declared_modes();
            Ok((tree.root(), modes))
        }
        None => Ok((
            tree_root(dest).map_err(StageError::Io)?,
            DeclaredModes::new(),
        )),
    }
}

fn payload2(head: &str, detail: &str) -> StageError {
    StageError::Payload(format!("{head}{detail}"))
}

/// Stage a single-binary payload as `bin/<entry>`. An npm binary package
/// ([`crate::vendor::NPM_BROTLI_BINARY`]) is decoded to exactly `cost.disk_installed` bytes —
/// the binary's size, which the vendor-direct lane sets from the vendor's own copy — and its
/// `package.json` is handed to `npm_package`, without which it is not staged.
fn stage_binary(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
    cap: u64,
    npm_package: Option<NpmPackageHook<'_>>,
) -> Result<TreeAccumulator, StageError> {
    let Some(tool) = crate::store::ToolName::new(&artifact.entry) else {
        return Err(payload2(
            "the entry is not a single admissible tool name: ",
            &artifact.entry,
        ));
    };
    std::fs::create_dir_all(dest).map_err(StageError::Io)?;
    crate::extract::require_empty_destination(dest).map_err(StageError::Extract)?;
    let bin = dest.join("bin");
    refuse_symlinked_bin(&bin)?;
    let mut tree = TreeAccumulator::new();
    crate::extract::make_dir_exact(dest, &bin, Path::new("bin"), &mut tree)
        .map_err(StageError::Extract)?;
    let target = bin.join(tool.exe_file());
    let written = match artifact.payload.as_str() {
        "tar-gz-binary" => crate::extract::stage_tar_gz_member(archive, &target, 0o755, cap)
            .map_err(StageError::Extract)?,
        crate::vendor::NPM_BROTLI_BINARY => {
            let Some(check) = npm_package else {
                return Err(StageError::Payload(String::from(
                    "an npm binary package is staged only against the version document that \
                     vouched for it",
                )));
            };
            let br = format!("{}.br", tool.exe_file());
            let npm = crate::extract::stage_npm_brotli_binary(
                archive,
                &br,
                &target,
                0o755,
                artifact.cost.disk_installed,
            )
            .map_err(StageError::Extract)?;
            check(&npm.package_json)?;
            npm.written
        }
        _ => {
            let file = crate::metadata_io::open_regular(archive).map_err(StageError::Io)?;
            crate::extract::stage_file(file, &target, 0o755, cap).map_err(StageError::Extract)?
        }
    };
    let rel = crate::extract::rel_bytes_under(dest, &target).map_err(StageError::Extract)?;
    tree.record_file(rel, written.mode, written.content_sha_hex);
    Ok(tree)
}

fn link_target_admissible(target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.ends_with('/')
        || target.contains('\0')
        || target.contains('\\')
    {
        return false;
    }
    target
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(target)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn refuse_symlinked_bin(bin: &Path) -> Result<(), StageError> {
    match std::fs::symlink_metadata(bin) {
        Ok(m) if !m.is_dir() => Err(StageError::Payload(String::from(
            "links: the staged `bin` is a symlink or a file, not a directory",
        ))),
        _ => Ok(()),
    }
}

fn target_ancestor_is_symlink(dest: &Path, target: &str) -> bool {
    let mut cur = dest.to_path_buf();
    let mut comps = Path::new(target).components().peekable();
    while let Some(c) = comps.next() {
        if comps.peek().is_none() {
            break;
        }
        cur.push(c);
        if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.is_symlink()) {
            return true;
        }
    }
    false
}

fn apply_links(
    dest: &Path,
    links: &BTreeMap<String, String>,
    mut tree: Option<&mut TreeAccumulator>,
) -> Result<(), StageError> {
    if links.is_empty() {
        return Ok(());
    }
    let bin = dest.join("bin");
    refuse_symlinked_bin(&bin)?;
    for (name, target) in links {
        if crate::store::ToolName::new(name).is_none() {
            return Err(payload2(
                "links name is not an admissible tool name: ",
                name,
            ));
        }
        if !link_target_admissible(target) {
            return Err(payload2(
                "links target must be a relative, `..`-free path inside the staged tree: ",
                target,
            ));
        }
        if target_ancestor_is_symlink(dest, target) {
            return Err(payload2(
                "links target passes through a symlinked directory: ",
                target,
            ));
        }
        if std::fs::symlink_metadata(dest.join(target)).is_err() {
            return Err(payload2("links target is not in the staged tree: ", target));
        }
        let link = bin.join(name);
        match tree.as_deref_mut() {
            Some(tree) => {
                let raw = Path::new("bin").join(name);
                crate::extract::make_dir_exact(dest, &bin, &raw, tree)
                    .map_err(StageError::Extract)?;
            }
            None => {
                std::fs::create_dir_all(&bin).map_err(StageError::Io)?;
                crate::platform::set_mode(&bin, crate::extract::safe_mode(0, true))
                    .map_err(StageError::Io)?;
            }
        }
        if std::fs::symlink_metadata(&link).is_ok() {
            return Err(payload2(
                "links name collides with a staged entry: bin/",
                name,
            ));
        }
        let mut rel_target = PathBuf::from("..");
        rel_target.push(target);
        crate::extract::create_symlink(&rel_target, &link).map_err(StageError::Io)?;
        if let Some(tree) = tree.as_deref_mut() {
            let rel = crate::extract::rel_bytes_under(dest, &link).map_err(StageError::Extract)?;
            let target_bytes = crate::platform::os_str_bytes(rel_target.as_os_str());
            tree.record_symlink(rel, target_bytes);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[cfg(not(test))]
fn dmg_mount_point(dest: &Path) -> Result<PathBuf, StageError> {
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| payload2("dmg stage dir has no name: ", &dest.to_string_lossy()))?;
    let mut mnt = String::from(name);
    mnt.push_str(".mnt");
    Ok(dest.with_file_name(mnt))
}

/// Run the OS tool at `path` (an `aterm_uds::os_path!`) on `args`, its failure named by
/// the tool's file name.
#[cfg(target_os = "macos")]
fn run_tool(path: &str, args: &[&std::ffi::OsStr]) -> Result<(), StageError> {
    let mut cmd = aterm_uds::spawnfd::command(path);
    cmd.args(args).stdin(aterm_uds::spawnfd::Stdio::null());
    let out = aterm_update_core::run_operation_command(&mut cmd).map_err(StageError::Io)?;
    if out.status.success() {
        return Ok(());
    }
    Err(tool_failed(tool_name(path), out.status.code(), &out.stderr))
}

/// The tool at `path` by its file name, as a failure names it.
#[cfg(target_os = "macos")]
fn tool_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Cleanup must finish even after the operation's owner requested cancellation.
#[cfg(target_os = "macos")]
fn run_cleanup_tool(path: &str, args: &[&std::ffi::OsStr]) -> Result<(), StageError> {
    let out = aterm_uds::spawnfd::command(path)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if out.status.success() {
        return Ok(());
    }
    Err(tool_failed(tool_name(path), out.status.code(), &out.stderr))
}

#[cfg(target_os = "macos")]
fn tool_failed(tool: &str, code: Option<i32>, stderr: &[u8]) -> StageError {
    let mut m = String::from(tool);
    m.push_str(" failed");
    if let Some(code) = code {
        m.push_str(" (exit ");
        m.push_str(&code.unsigned_abs().to_string());
        m.push(')');
    }
    let stderr = String::from_utf8_lossy(stderr);
    let tail = stderr.trim();
    if !tail.is_empty() {
        m.push_str(": ");
        m.push_str(tail.lines().last().unwrap_or(tail));
    }
    StageError::Payload(m)
}

/// [`run_tool`], keeping what the tool printed.
#[cfg(target_os = "macos")]
fn capture_tool(path: &str, args: &[&std::ffi::OsStr]) -> Result<String, StageError> {
    let out = aterm_uds::spawnfd::command(path)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if !out.status.success() {
        return Err(tool_failed(tool_name(path), out.status.code(), &out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_os = "macos")]
struct AttachedImage {
    image: PathBuf,
    node: PathBuf,
    mounted_at: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
fn parse_hdiutil_info(report: &str) -> Vec<AttachedImage> {
    let mut out: Vec<AttachedImage> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in report.lines() {
        if line.starts_with("====") {
            out.extend(parse_hdiutil_block(&block));
            block.clear();
        } else {
            block.push(line);
        }
    }
    out.extend(parse_hdiutil_block(&block));
    out
}

#[cfg(target_os = "macos")]
fn parse_hdiutil_block(lines: &[&str]) -> Option<AttachedImage> {
    let mut image: Option<PathBuf> = None;
    let mut node: Option<PathBuf> = None;
    let mut mounted_at: Vec<PathBuf> = Vec::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("/dev/") {
            let mut fields = rest.split('\t');
            let dev = fields.next().unwrap_or("").trim();
            if node.is_none() && !dev.is_empty() {
                let mut n = String::from("/dev/");
                n.push_str(dev);
                node = Some(PathBuf::from(n));
            }
            if let Some(point) = fields.nth(1).map(str::trim).filter(|p| !p.is_empty()) {
                mounted_at.push(PathBuf::from(point));
            }
        } else if let Some((key, value)) = line.split_once(':')
            && key.trim() == "image-path"
        {
            image = Some(PathBuf::from(value.trim()));
        }
    }
    Some(AttachedImage {
        image: image?,
        node: node?,
        mounted_at,
    })
}

#[cfg(target_os = "macos")]
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

#[cfg(target_os = "macos")]
trait Attached {
    fn root(&self) -> &Path;
    fn detach(&mut self) -> Result<(), StageError>;
}

#[cfg(all(target_os = "macos", not(test)))]
fn attach_image(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
    Ok(Box::new(Mount::attach(image, dest)?))
}

#[cfg(all(target_os = "macos", test))]
fn attach_image(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
    image_arm::attach(image, dest)
}

#[cfg(all(target_os = "macos", test))]
#[path = "install_image_arm_tests.rs"]
mod image_arm;

#[cfg(target_os = "macos")]
struct Mount {
    image: PathBuf,
    point: PathBuf,
    attached: bool,
}

#[cfg(target_os = "macos")]
const ATTACH_ATTEMPTS: u32 = 4;

#[cfg(target_os = "macos")]
const _: () = assert!(
    ATTACH_ATTEMPTS >= 2,
    "a transient EAGAIN gets a second attempt"
);

/// Up to [`ATTACH_ATTEMPTS`] times: `arm` a guard and `attempt` the attach under it, the guard
/// on success, else `reclaim` it and try again; the last failure once every attempt failed.
#[cfg(target_os = "macos")]
fn attach_retrying<M>(
    mut arm: impl FnMut() -> M,
    mut attempt: impl FnMut(&M) -> Result<(), StageError>,
    mut reclaim: impl FnMut(M),
) -> Result<M, StageError> {
    let mut last: Option<StageError> = None;
    for _ in 0..ATTACH_ATTEMPTS {
        let m = arm();
        match attempt(&m) {
            Ok(()) => return Ok(m),
            Err(e) => {
                reclaim(m);
                last = Some(e);
            }
        }
    }
    Err(last
        .unwrap_or_else(|| StageError::Payload(String::from("hdiutil attach was never attempted"))))
}

#[cfg(target_os = "macos")]
impl Mount {
    #[cfg(not(test))]
    fn attach(image: &Path, dest: &Path) -> Result<Self, StageError> {
        let point = dmg_mount_point(dest)?;
        let _ = std::fs::remove_dir(&point);
        std::fs::create_dir_all(&point).map_err(StageError::Io)?;
        attach_retrying(
            || Mount {
                image: image.to_path_buf(),
                point: point.clone(),
                attached: true,
            },
            |m| {
                run_tool(
                    aterm_uds::os_path!("hdiutil"),
                    &[
                        "attach".as_ref(),
                        "-nobrowse".as_ref(),
                        "-readonly".as_ref(),
                        "-noverify".as_ref(),
                        "-noautoopen".as_ref(),
                        "-mountpoint".as_ref(),
                        m.point.as_os_str(),
                        image.as_os_str(),
                    ],
                )
            },
            |mut m| {
                let _ = m.detach_now();
            },
        )
        .inspect_err(|_| {
            let _ = std::fs::remove_dir(&point);
        })
    }

    fn detach_now(&mut self) -> Result<(), StageError> {
        if !self.attached {
            return Ok(());
        }
        self.attached = false;
        let by_point = run_cleanup_tool(
            aterm_uds::os_path!("hdiutil"),
            &["detach".as_ref(), "-quiet".as_ref(), self.point.as_os_str()],
        );
        let r = match by_point {
            Ok(()) => Ok(()),
            Err(_) => self.eject_own_devices(),
        };
        let _ = std::fs::remove_dir(&self.point);
        r
    }

    fn eject_own_devices(&self) -> Result<(), StageError> {
        let nodes = self.own_devices()?;
        if nodes.is_empty() {
            return Ok(());
        }
        let mut last: Option<StageError> = None;
        for node in &nodes {
            if run_cleanup_tool(
                aterm_uds::os_path!("hdiutil"),
                &["detach".as_ref(), "-quiet".as_ref(), node.as_os_str()],
            )
            .is_ok()
            {
                continue;
            }
            if let Err(e) = run_cleanup_tool(
                aterm_uds::os_path!("hdiutil"),
                &[
                    "detach".as_ref(),
                    "-force".as_ref(),
                    "-quiet".as_ref(),
                    node.as_os_str(),
                ],
            ) {
                last = Some(e);
            }
        }
        if self.own_devices()?.is_empty() {
            return Ok(());
        }
        Err(last.unwrap_or_else(|| {
            payload2(
                "hdiutil could not detach the image: ",
                &self.image.to_string_lossy(),
            )
        }))
    }

    fn own_devices(&self) -> Result<Vec<PathBuf>, StageError> {
        let report = capture_tool(aterm_uds::os_path!("hdiutil"), &["info".as_ref()])?;
        Ok(self.reclaimable(&report))
    }

    fn reclaimable(&self, report: &str) -> Vec<PathBuf> {
        parse_hdiutil_info(report)
            .into_iter()
            .filter(|a| same_file(&a.image, &self.image))
            .filter(|a| a.mounted_at.iter().all(|p| same_file(p, &self.point)))
            .map(|a| a.node)
            .collect()
    }
}

#[cfg(target_os = "macos")]
impl Attached for Mount {
    fn root(&self) -> &Path {
        &self.point
    }

    fn detach(&mut self) -> Result<(), StageError> {
        self.detach_now()
    }
}

#[cfg(target_os = "macos")]
fn single_app_in(dir: &Path) -> Result<PathBuf, StageError> {
    let mut apps: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(StageError::Io)?.flatten() {
        let is_app = Path::new(&entry.file_name())
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
        if is_app && entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            apps.push(entry.path());
        }
    }
    apps.sort();
    if apps.len() == 1 {
        return Ok(apps.swap_remove(0));
    }
    Err(StageError::DmgAppCount(apps.len()))
}

#[cfg(target_os = "macos")]
impl Drop for Mount {
    fn drop(&mut self) {
        let _ = self.detach_now();
    }
}

#[cfg(target_os = "macos")]
fn stage_dmg(image: &Path, dest: &Path) -> Result<(), StageError> {
    std::fs::create_dir_all(dest).map_err(StageError::Io)?;
    crate::extract::require_empty_destination(dest).map_err(StageError::Extract)?;
    let mut mount = attach_image(image, dest)?;
    let app = single_app_in(mount.root())?;
    let name = app
        .file_name()
        .ok_or_else(|| payload2("dmg bundle has no name: ", &app.to_string_lossy()))?;
    let out = dest.join(name);
    let (no_acl, no_clone) = ("--noacl".as_ref(), "--noclone".as_ref());
    run_tool(
        aterm_uds::os_path!("ditto"),
        &[no_acl, no_clone, app.as_os_str(), out.as_os_str()],
    )?;
    if let Err(e) = vet_copied_tree(dest, &out) {
        let _ = mount.detach();
        return Err(e);
    }
    mount.detach()
}

#[cfg(target_os = "macos")]
fn vet_copied_tree(root: &Path, dir: &Path) -> Result<(), StageError> {
    use std::os::unix::fs::MetadataExt as _;
    let stage = std::fs::symlink_metadata(root).map_err(StageError::Io)?;
    vet_copied_entries(root, dir, Owner(stage.uid(), stage.gid()))
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct Owner(u32, u32);

#[cfg(target_os = "macos")]
fn vet_copied_entries(root: &Path, dir: &Path, owner: Owner) -> Result<(), StageError> {
    give_safe_mode(dir, true, owner)?;
    for entry in std::fs::read_dir(dir).map_err(StageError::Io)? {
        let entry = entry.map_err(StageError::Io)?;
        let path = entry.path();
        let ft = entry.file_type().map_err(StageError::Io)?;
        if ft.is_symlink() {
            let rel = path.strip_prefix(root).map_err(|_| {
                StageError::Extract(ExtractError::Rejected(
                    crate::extract::ExtractReject::RootEscape,
                    path.clone(),
                ))
            })?;
            let target = std::fs::read_link(&path).map_err(StageError::Io)?;
            crate::extract::vet_symlink(root, rel, &target, 0)
                .map_err(|r| StageError::Extract(ExtractError::Rejected(r, rel.to_path_buf())))?;
            let meta = std::fs::symlink_metadata(&path).map_err(StageError::Io)?;
            give_owner(&path, &meta, owner)?;
        } else if ft.is_dir() {
            vet_copied_entries(root, &path, owner)?;
        } else if ft.is_file() {
            give_safe_mode(&path, false, owner)?;
        } else {
            return Err(payload2(
                "dmg bundle carries an entry that is not a file, directory or symlink: ",
                &path.to_string_lossy(),
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn give_safe_mode(path: &Path, is_dir: bool, owner: Owner) -> Result<(), StageError> {
    use std::os::macos::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).map_err(StageError::Io)?;
    let flags = meta.st_flags();
    if flags & crate::platform::OWNER_BINDING_FLAGS != 0 {
        crate::platform::set_file_flags(path, flags & !crate::platform::OWNER_BINDING_FLAGS)
            .map_err(StageError::Io)?;
    }
    give_owner(path, &meta, owner)?;
    let mode = crate::platform::permission_mode(&meta) & 0o7777;
    let safe = crate::extract::safe_mode(mode, is_dir);
    if mode != safe {
        crate::platform::set_mode(path, safe).map_err(StageError::Io)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn give_owner(
    path: &Path,
    meta: &std::fs::Metadata,
    Owner(uid, gid): Owner,
) -> Result<(), StageError> {
    use std::os::unix::fs::MetadataExt as _;
    if meta.uid() != uid || meta.gid() != gid {
        std::os::unix::fs::lchown(path, Some(uid), Some(gid)).map_err(StageError::Io)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn stage_dmg(_image: &Path, _dest: &Path) -> Result<(), StageError> {
    Err(StageError::Payload(String::from(
        "dmg payloads can only be staged on macOS (hdiutil/ditto)",
    )))
}

fn disk_reverify_armed() -> bool {
    aterm_types::dev_seam!(DISK_REVERIFY_ENV).is_some_and(|v| !v.is_empty())
}

fn reverified_root(
    incoming: &Path,
    extracted_root: String,
    armed: bool,
    modes: &DeclaredModes,
) -> std::io::Result<String> {
    if !armed {
        return Ok(extracted_root);
    }
    let walked = tree_root_declared(incoming, modes)?;
    if !walked.eq_ignore_ascii_case(&extracted_root) {
        let mut msg = String::from("staged tree_root disagreement: the extraction folded ");
        msg.push_str(&extracted_root);
        msg.push_str(" but the on-disk walk read ");
        msg.push_str(&walked);
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, msg));
    }
    Ok(walked)
}

fn swap_into_place(
    build_dir: &Path,
    incoming: &Path,
) -> std::io::Result<Option<crate::lease::ReclaimGuard>> {
    // Cancellation is admitted before the replacement transaction, never between its renames.
    aterm_update_core::check_operation()?;
    let first_exec = crate::store::first_exec_removal(build_dir)?;
    let sdk = crate::compat::sdk_mutation_guard_for_build(build_dir)?;
    first_exec.check(incoming)?;
    for other in [
        crate::store::superseded_dir(build_dir),
        aterm_toolchain::layout::aside_of(build_dir),
    ]
    .into_iter()
    .flatten()
    {
        first_exec.check(&other)?;
    }
    if crate::store::held_unproven(build_dir) {
        let mut msg = build_dir.display().to_string();
        msg.push_str(
            " stays in place: a resolve check's child in it never exited on its own, and \
             macOS may still be assessing that first exec",
        );
        return Err(std::io::Error::other(msg));
    }
    let was_complete = crate::store::build_is_complete(build_dir);
    crate::store::clear_build_ready(build_dir)?;
    // Required: the marker's removal reaches the media ahead of the renames below, or a power
    // cut can leave the old marker standing over the incoming tree, read as complete however
    // little of it landed. A failed flush puts the marker back and swaps nothing.
    if let Some(parent) = build_dir.parent()
        && let Err(e) = crate::store::sync_dir_ordered(parent)
    {
        if was_complete {
            let _ = crate::store::mark_build_ready(build_dir);
        }
        return Err(e);
    }
    // The outgoing tree is behind the build's link when it was laid aside, else at its name;
    // a toolchain comes in aside, in `<build>.noindex` behind a `<build>` link
    // (`store::lays_aside`).
    let linked = aterm_toolchain::layout::is_linked_aside(build_dir);
    let aside = aterm_toolchain::layout::aside_of(build_dir);
    let outgoing = if linked {
        aside.clone()
    } else {
        Some(build_dir.to_path_buf())
    };
    let dest = match aside {
        Some(aside) if crate::store::lays_aside(incoming) => aside,
        _ => build_dir.to_path_buf(),
    };
    let superseded = crate::store::superseded_dir(build_dir);
    let retired = match (&superseded, &outgoing) {
        (Some(old), Some(out)) if out.exists() => {
            let _ = crate::store::remove_tree_under_gate(old, &first_exec);
            if let Err(e) = std::fs::rename(out, old) {
                if was_complete {
                    let _ = crate::store::mark_build_ready(build_dir);
                }
                return Err(e);
            }
            true
        }
        _ => false,
    };
    let restore = |e: std::io::Error| {
        if retired && let (Some(old), Some(out)) = (&superseded, &outgoing) {
            restore_outgoing(old, out, build_dir, was_complete, &first_exec);
        }
        Err(e)
    };
    if dest == build_dir {
        // Coming in plain where a link stood: the link goes, so the tree takes its name.
        if linked && let Err(e) = std::fs::remove_file(build_dir) {
            return restore(e);
        }
    } else if !linked {
        // A tree at `<build>.noindex` that no `<build>` link names belongs to nothing.
        if std::fs::symlink_metadata(&dest).is_ok()
            && crate::store::remove_tree_under_gate(&dest, &first_exec).is_err()
        {
            return restore(std::io::Error::other(format!(
                "{} is in the way and could not be removed",
                dest.display()
            )));
        }
    }
    if let Err(e) = std::fs::rename(incoming, &dest) {
        return restore(e);
    }
    if dest != build_dir
        && !aterm_toolchain::layout::is_linked_aside(build_dir)
        && let Err(e) = crate::store::link_aside(build_dir)
    {
        let _ = std::fs::rename(&dest, incoming);
        return restore(e);
    }
    if retired && let Some(ref old) = superseded {
        let _ = crate::store::remove_tree_under_gate(old, &first_exec);
    }
    // The `.unproven` hold was on the tree this swap replaced, so it is spent.
    if let Some(mark) = crate::store::sidecar_path(build_dir, crate::store::UNPROVEN_SUFFIX) {
        let _ = std::fs::remove_file(mark);
    }
    Ok(sdk)
}

/// Put the outgoing tree parked at `old` back at `out` (the build's name, or its aside, whose
/// `<build>` link is laid again when a failed swap took it down).
fn restore_outgoing(
    old: &Path,
    out: &Path,
    build_dir: &Path,
    was_complete: bool,
    first_exec: &crate::store::FirstExecRemoval,
) -> bool {
    if [old, out, build_dir]
        .into_iter()
        .any(|target| first_exec.check(target).is_err())
    {
        return false;
    }
    if std::fs::rename(old, out).is_err() {
        return false;
    }
    if out != build_dir {
        crate::store::relink_aside(build_dir);
    }
    if was_complete {
        let _ = crate::store::mark_build_ready(build_dir);
    }
    true
}

#[cfg(any(test, all(debug_assertions, target_os = "macos")))]
#[path = "install_acceptance.rs"]
mod acceptance_fixtures;

#[cfg(all(debug_assertions, target_os = "macos"))]
pub use acceptance_fixtures::{
    a_real_image_stages_its_single_app, a_real_image_with_no_app_is_refused_and_detached,
};

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
