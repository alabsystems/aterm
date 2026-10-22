// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end install orchestration (§5/§7/§8/§9): every verified primitive composed
//! into one program install, with the network behind [`Fetcher`] so the sequence is
//! unit-testable against synthetic signed fixtures.
//!
//! The ordered, fail-closed pipeline ([`install`]):
//! 1. fetch index candidates (index plus the master-signed roster beside it) →
//!    [`crate::select::select_index`] (admit roster, verify, select; §5);
//! 2. reachability: the program is named in the verified index (§5) and pinned in the
//!    channel;
//! 3. [`crate::gate::decide`]: `UpToDate`/`Tombstone`/`NotPinned` short-circuit;
//! 4. fetch the per-build `pkg.toml`, verify it under the roster generation that
//!    authorized the index ([`TrustedIndex::verify_pkg`]), [`parse_pkg`], and check its
//!    signed `program`/`build_number` bind the request (anti-replay, §4.2);
//! 5. select the artifact for the target triple (a missing triple is a clean skip, §6);
//! 6. download → [`crate::install::verify_and_stage`] (signed digest → extract → tree_root);
//! 7. [`crate::activate::activate_build`] and the `bin/` shims (the admitted half of
//!    [`crate::activate::install_shims`], since the bundle resolve check needs the
//!    admitted tool set); record.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::activate::install_tools;
use crate::activate::{Aliases, activate_build, install_tombstone_shim, install_tools_env};
use crate::apply::{Group, TxnOutcome, transact_holding};
use crate::gate::{ApplyDecision, decide};
use crate::install::StageError;
use crate::manifest::{Channel, Index, parse_pkg};
use crate::select::{Candidate, select_index};
use crate::sig::{Anchor, BuildFloor, TrustedIndex};
use crate::store::{Layout, ToolName};

pub(crate) mod ahead;

/// [`crate::install::verify_and_stage`], plus the sidecar cleanup both stage lanes owe. It
/// takes the plain name here (the `install` one is not imported) so the singleton lane and
/// `stage_member` cannot drift apart.
///
/// Both lanes write the signed `shim_env` sidecar (`<build>.shim-env`, design S7) before the
/// stage, creating `store/<program>/`. A stage that then fails with no tree in place must
/// reclaim the sidecar here: gc enumerates `store/<program>/` for directories only, and
/// `store::discard_build` needs a tree. A sidecar with a build under it (a failed re-stage
/// over a live build, or gc's incomplete partial) is left alone.
fn verify_and_stage(
    artifact: &crate::manifest::Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &crate::install::StageHooks<'_>,
) -> Result<String, StageError> {
    let staged = crate::install::verify_and_stage(artifact, archive, build_dir, hooks);
    if staged.is_err() && !build_dir.exists() {
        crate::shim_env::remove_sidecar(build_dir);
    }
    staged
}

/// The pre-swap gate an index-lane stage of `program` runs: for an ALab-built agent program
/// ([`crate::alab::agent`], which reaches the index lane only where the source rule hands it
/// there) the ALab pack gate ([`crate::install::alab_pack_gate`]: the package marker, and
/// `codesign --verify --strict` on every Mach-O), then the minimum-aterm pairing; for every
/// other program none, as before.
fn alab_pack_gate(program: &str, exposes: &[String]) -> Option<Box<StageGate>> {
    crate::alab::agent(program).map(|_| -> Box<StageGate> {
        let pack = crate::install::alab_pack_gate(exposes, crate::install::codesign_strict_check);
        // THE MINIMUM-ATERM PAIRING ([`crate::alab::pairs_with`]): a pack this aterm is too
        // old to host (its record's `aterm.min`) is never laid. The version is the client's
        // alone: the producer pins for every aterm, and holds the record only to its shape.
        Box::new(move |tree: &Path| {
            pack(tree)?;
            crate::alab::pairs_with(tree, crate::alab::this_aterm()).map_err(StageError::Payload)
        })
    })
}

/// A pre-swap gate, owned.
type StageGate = dyn Fn(&Path) -> Result<(), StageError>;

/// The stage hooks that run `gate` (none without one).
fn stage_hooks(gate: Option<&StageGate>) -> crate::install::StageHooks<'_> {
    crate::install::StageHooks {
        pre_swap: gate,
        sidecars: None,
        aliases: &[],
    }
}

/// What [`Fetcher::vendor_get`] came back with (mirrors `aterm_update_core::VendorResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorGet {
    /// HTTP 200: the whole body (at most the cap), the response's ETag when it carried a
    /// well-formed one, and the URL the fetch ended on after redirects — the caller checks
    /// it with [`crate::vendor::digest_effective_url_ok`] for a digest document.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        effective_url: String,
    },
    /// HTTP 304 to a conditional request: unchanged. `etag` is the validator to keep for
    /// the next poll (the 304's own, else the one sent).
    NotModified { etag: String },
}

/// Why a vendor-direct fetch produced nothing, split the way the lane must treat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorFetchError {
    /// The vendor host was not reached or is not serving now — the network, a timeout, a
    /// 408/429/5xx that outlived the retries, or a fetcher with no network leg. Keep what
    /// is installed, quietly; the next poll asks again.
    Unreachable(String),
    /// A verdict about the request or the document — a URL outside the program's pins, a
    /// body over its cap, a hop off https, a certificate that does not verify, any other
    /// status, an unreadable answer. Refuse it; asking again gets the same answer.
    Refused(String),
}

impl std::fmt::Display for VendorFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(message) | Self::Refused(message) => f.write_str(message),
        }
    }
}

/// The network operations the install flow needs, abstracted so the orchestration is
/// testable. The production impl ([`crate::net::GithubFetcher`]) reads the release download
/// host through `aterm-update-core`, with no credential and no API request.
pub trait Fetcher {
    /// The candidate `index.toml`+sig assets across recent releases of the index repo.
    fn index_candidates(&self) -> Result<Vec<Candidate>, String>;
    /// The `(pkg-<program>-<build>.toml, .sig)` raw bytes for a resolved build, from the
    /// program's own `repo` (the index's `[programs.<name>].repo`, §4.2).
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String>;
    /// Fetch AHEAD, all at once, the [`Fetcher::pkg_manifest`] pairs a caller is about to read
    /// one at a time — `(repo, program, build)` each ([`prefetch_pinned`]). Bytes, never
    /// trust: every read still runs `verify_pkg` → `parse_pkg` on what it is handed, and a
    /// pair that did not arrive is fetched again by the read that wants it, which meets the
    /// failure itself.
    ///
    /// DEFAULT: nothing — a fetcher with no network leg reads each pair as cheaply on demand.
    fn prefetch_manifests(&self, _pins: &[(String, String, u64)]) {}
    /// This fetcher as one a pass may work from several threads at once — the vendor lane's
    /// plans side by side, and its landings beside the set completion's — or `None`, and
    /// the pass works it from its own thread, one request after another.
    ///
    /// DEFAULT: `None`. The network and `dir:` fetchers, whose memos are locked, are shared.
    fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
        None
    }
    /// Download `asset` (from the program's `repo`) to `dest`.
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String>;
    /// [`Fetcher::download`], told which program the asset belongs to and the row's signed
    /// `size` as the byte `cap`. The production GitHub fetcher honours a per-program
    /// `[packages.links]` `owner/repo` fetch override here; the host is never an
    /// authenticity input (§5/§8), since the bytes still pass the signed sha256 and
    /// `tree_root` gates.
    ///
    /// `cap` is the number the disk preflight was computed from, and the sha256 gate runs
    /// only over the complete file, so a larger cap could write past the free-space floor
    /// (§9). `0` means the row states no size and the fetcher uses its own ceiling. The
    /// default (the `dir:` registry, which links or copies a local file) ignores it.
    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), String> {
        let _ = (program, cap);
        self.download(repo, asset, dest)
    }
    /// Download a signed `https` `url` to `dest`, capped at exactly `cap` (the row's signed
    /// `size`), for the `https` protocol lane. Default: refused, so a `dir:` registry or a
    /// test fetcher never reaches the network; the production fetcher presents no
    /// credential to a vendor host.
    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        let _ = (url, dest, cap);
        Err(
            "this fetcher cannot fetch vendor URLs (the https protocol is network-only)"
                .to_string(),
        )
    }
    /// GET a small vendor-direct document for `program` (a release head, a manifest, a
    /// signature, a SHA256SUMS), at most `cap` bytes, conditionally on `if_none_match`.
    /// Default: [`VendorFetchError::Unreachable`]; only the production fetcher opts in,
    /// anonymously and only for a URL [`crate::vendor::vendor_direct_url_allowed`] admits.
    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        let _ = (program, url, cap, if_none_match);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot fetch vendor documents (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    /// [`Fetcher::vendor_get`] for a release head read inside a pass or a door: one short
    /// attempt, since an unreached head keeps the installed build and the next check is
    /// the retry. Default: [`Fetcher::vendor_get`].
    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        self.vendor_get(program, url, cap, if_none_match)
    }
    /// The final `Content-Length` (never zero) of `program`'s pinned vendor-direct URL
    /// after redirects: the byte cap for a payload no signed document sizes (codex).
    /// Default: [`VendorFetchError::Unreachable`].
    fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
        let _ = (program, url);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot size vendor payloads (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    /// Download `program`'s vendor-direct payload at `url` to `dest`, capped at exactly
    /// `cap` bytes, resumably, under the same pins, anonymity and CA-trust scrub as
    /// [`Fetcher::vendor_get`]. Trusted only after the caller's sha256 check against an
    /// authenticated digest. Default: [`VendorFetchError::Unreachable`].
    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), VendorFetchError> {
        let _ = (program, url, dest, cap);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot download vendor payloads (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    /// Canonical identity of this fetcher's source (`github:<owner>/<repo>` or `dir:<path>`),
    /// tagging the index cache so one source's cache never answers another's failed fetch
    /// (§14). The default is a non-matching sentinel.
    fn source_id(&self) -> String {
        "fetcher:unspecified".to_string()
    }
    /// Whether this fetcher's last failed [`Fetcher::index_candidates`] failed on the link
    /// (no host answered: DNS, a refused or timed-out connect, TLS) rather than on a host
    /// that answered (a rate limit, a 5xx, a captive portal). Only the first is an offline
    /// machine ([`ResolveProvenance::answered`]). Default `false`: offline needs evidence.
    fn index_link_down(&self) -> bool {
        false
    }
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// The program installed.
    pub program: String,
    /// The build it is now on.
    pub build: u64,
    /// The `index_build` of the index this install resolved from — the caller advances the
    /// durable high-water [`crate::sig::Floor`] to this on success (§8 gate 3).
    pub index_build: u64,
    /// The `roster_seq` of the master-signed generation that authorized that index; the
    /// caller ratchets the durable roster floor to it on success. Carried apart from
    /// `index_build` because roster and index move independently.
    pub roster_seq: u64,
    /// Whether it was already current (no change).
    pub already_current: bool,
    /// The `bin/` shims installed.
    pub shimmed: Vec<String>,
    /// `exposes` names refused a shim (sensitive-name collisions, §10).
    pub refused_shims: Vec<String>,
    /// The signed `tree_root` (§8) of the installed build, which the CLI records into
    /// `status.toml` for `atpkg verify`. Empty when already current or the manifest omitted
    /// one.
    pub tree_root: String,
    /// Required-dependency pull-in outcomes (flattened transitive closure), with yanked,
    /// unreachable or cyclic deps skipped, never gate-bypassing.
    pub dependencies: Vec<DepOutcome>,
    /// `Some` when the program is vendor-direct: what the vendor lane did, in its own
    /// words. `index_build`/`roster_seq` are then `0` (no index was consulted).
    pub vendor: Option<VendorReport>,
}

/// What the vendor-direct lane did for an `install`/`update` of one of its programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorReport {
    /// The version now active, when one is.
    pub version: Option<String>,
    /// The vendor, as its row names it (`Anthropic`).
    pub vendor: String,
    /// The verdict line (`atpkg: claude 2.1.278 → 2.1.280 (Anthropic latest)`).
    pub line: String,
}

/// One resolved `requires` dependency and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepOutcome {
    /// The dependency program name.
    pub program: String,
    /// The pull-in result.
    pub result: DepResult,
}

/// The outcome of resolving one `requires` dependency. A skip is always safe: a `requires`
/// edge pulls a program IN, never bypasses a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepResult {
    /// Freshly installed at `build`, carrying its SIGNED `tree_root` so the CLI records it.
    Installed {
        /// The build the dependency was installed at.
        build: u64,
        /// The dependency's signed `tree_root`.
        tree_root: String,
    },
    /// Already active at `build` — left as-is (its own status was recorded at its install).
    AlreadyPresent(u64),
    /// Satisfied by a system install ([`crate::manifest::Program::system`]): the binary is
    /// on `PATH` outside the managed prefix, so nothing is installed beside it. Carries the
    /// path; the caller records the canonical `system: <path> — not managed by aterm` row.
    System(PathBuf),
    /// Skipped, with a reason (unreachable / tombstoned / not pinned / cycle / fetch error).
    Skipped(String),
}

/// Every program name a verified index carries, sorted, for [`FlowError::NotReachable`].
fn index_roster(index: &crate::manifest::Index) -> Vec<String> {
    index.programs.keys().cloned().collect()
}

/// The head of [`FlowError::InsufficientDisk`]'s sentence, which aterm-gui's
/// `toolchain_words` matches to paint a first run that ended on it.
pub const DISK_REFUSAL_HEAD: &str = "not enough disk space";

/// Why an install failed, fail-closed at each gate.
#[derive(Debug)]
pub enum FlowError {
    /// No signature-valid index at/above the floor was found.
    NoIndex,
    /// The index source could not be reached (offline, DNS, a proxy, a 403 rate limit) and
    /// no §14 cached index stood in. Distinct from [`FlowError::NoIndex`], a trust verdict:
    /// an offline machine must be told to connect, not sent to key management.
    Unreachable(String),
    /// The program is not named in the verified index (unreachable, §5). The second field
    /// is the roster the index does name, captured while the verified index is in hand, so
    /// the error can say what is installable; empty means unknown.
    NotReachable(String, Vec<String>),
    /// The requested channel does not exist in the index.
    NoChannel(String),
    /// The program is not pinned in the channel.
    NotPinned(String),
    /// The selected index's freshness window has lapsed (`now >= valid_until`, §8).
    Stale,
    /// The pinned build is yanked/below-floor — tombstoned, nothing installed (§7).
    Tombstoned(String),
    /// Fetching the per-build manifest failed.
    PkgFetch(String),
    /// The per-build manifest did not verify under the delegated release key.
    PkgVerify,
    /// The per-build manifest was malformed / a newer schema.
    PkgParse,
    /// The per-build manifest spells a retired `kind` (`vendor-fetch`); carries the split
    /// that replaced it ([`crate::Reject::RetiredKind`]) so the refusal names the fix.
    RetiredKind(&'static str),
    /// The per-build manifest's `shim_env` breaks the rule a shim can honour
    /// ([`crate::Reject::ShimEnv`], design S7); carries the entry and the reason, like
    /// `RetiredKind`, so the authoring machine's own install names the fix.
    ShimEnv(String),
    /// The signed `program`/`build_number` did not match the request (anti-replay).
    Mismatch,
    /// No artifact for the target triple (a clean skip, §6).
    NoArtifact(String),
    /// The artifact's `kind` is not installable by this tool path (fail closed, §16.4).
    UnsupportedKind(String),
    /// An `app-bundle` member over `github-release`, the aterm app itself (§16.4): atpkg
    /// never installs it, since the app updates itself through its own notarization-gated
    /// updater. Refused before any byte moves.
    AppBundleRefused(String),
    /// An artifact row failed per-protocol admission ([`crate::vendor::check_row`]) before
    /// any byte moved: for `https`, a non-https or non-allow-listed `url`, an unknown
    /// `payload`, a raw-binary `entry` that is not an exposed shimmable name, a hostile
    /// `links` target, or a missing signed digest; for any other protocol, the protocol
    /// itself. The message names the field.
    VendorRefused(String),
    /// The program's signed index row names the retired `requires` relation
    /// ([`crate::manifest::Program::names_requires`], §5.3(b)): this client keeps no install
    /// order, so the program is refused before any byte moves. Carries the program.
    RequiresRetired(String),
    /// The asset download failed.
    Download(String),
    /// Staging (signed digest / extract / tree_root) failed.
    Stage(StageError),
    /// A recent attempt proved the published asset does not match the pinned build's signed
    /// digests and that refusal still binds ([`crate::store::StageRefusal`]), so nothing
    /// was fetched, staged or changed. Carries the recorded sentence (the original failure
    /// and when the next attempt is due).
    StageRefused(String),
    /// Activation / shim install failed.
    Activate(String),
    /// Rollback-specific failure: the program is not active, or no retained build below
    /// current satisfies the floor/yank gate.
    Rollback(String),
    /// Disk preflight failed: the volume lacks room for the artifact + its extracted tree
    /// while leaving the free floor (§9). Nothing was downloaded/staged. `required` is the
    /// whole need, the free floor included, so it compares directly with `available`.
    InsufficientDisk { required: u64, available: u64 },
    /// The program is dev-linked (linkmode, §13): `update`/`apply` skip it until
    /// `atpkg unlink`.
    Linked(String),
    /// The vendor-direct lane did not install a vendor program: its verdict line
    /// (refused, failed, or a head it will not move to), versions only.
    Vendor(String),
    /// The vendor's release channel was not reached: the same verdict line, its own variant
    /// so the verb can add the re-run follow-up.
    VendorUnreachable(String),
    /// The vendor publishes nothing aterm can verify for this target: the canonical
    /// `unavailable on <target>: <hint>` state, whole — a state, never an error (design §2).
    VendorUnavailable(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::NoIndex => f.write_str("no signature-valid index at/above the floor"),
            FlowError::Unreachable(why) => {
                // No "retries automatically" tail: this reaches foreground verbs, where
                // nothing retries; the CLI edge appends the per-verb re-run
                // (`print_unreachable_followup`).
                write!(f, "could not reach the toolchain index: {why}")
            }
            FlowError::NotReachable(p, roster) => {
                // The grep-stable phrase, verbatim; the roster rides in a parenthesis.
                write!(f, "{p} is not named in the signed index")?;
                // The whole roster when short, else the first 12.
                match roster.len() {
                    0 => Ok(()),
                    1..=12 => write!(f, " (it names: {})", roster.join(", ")),
                    _ => write!(f, " (it names: {}, …)", roster[..12].join(", ")),
                }
            }
            FlowError::NoChannel(c) => write!(f, "no channel {c:?} in the index"),
            FlowError::NotPinned(p) => write!(f, "{p} is not pinned in the channel"),
            FlowError::Stale => f.write_str("the signed index's freshness window has lapsed"),
            FlowError::Tombstoned(p) => write!(f, "ALab withdrew {p}'s pinned build"),
            FlowError::PkgFetch(e) => write!(f, "fetch manifest: {e}"),
            FlowError::PkgVerify => f.write_str("manifest signature did not verify"),
            FlowError::PkgParse => f.write_str("manifest malformed or newer schema"),
            FlowError::RetiredKind(why) => write!(f, "manifest refused: {why}"),
            FlowError::ShimEnv(why) => write!(f, "manifest refused: {why}"),
            FlowError::Mismatch => f.write_str("manifest program/build did not match the request"),
            FlowError::NoArtifact(t) => write!(f, "no artifact for target {t}"),
            FlowError::UnsupportedKind(k) => write!(
                f,
                "artifact kind {k:?} is not installable by `atpkg install`"
            ),
            FlowError::AppBundleRefused(p) => write!(
                f,
                "{p} updates itself, not through atpkg (`aterm update status`)"
            ),
            FlowError::VendorRefused(why) => write!(f, "artifact row refused: {why}"),
            FlowError::RequiresRetired(p) => write!(
                f,
                "{p}: signed index row names `requires`, which this client no longer honours; \
                 refused"
            ),
            FlowError::Download(e) => write!(f, "download: {e}"),
            FlowError::Stage(e) => write!(f, "stage: {e}"),
            // No head: the recorded sentence names the program, build, failure and the
            // way out, and this pass staged nothing.
            FlowError::StageRefused(m) => f.write_str(m),
            FlowError::Activate(e) => write!(f, "activate: {e}"),
            FlowError::Rollback(m) => {
                // No "rollback: " head: the CLI edge already prints "atpkg: rollback <p>
                // failed:".
                f.write_str(m)
            }
            FlowError::InsufficientDisk {
                required,
                available,
            } => {
                let (need, have) = crate::cost::human_bytes_apart(*required, *available);
                let short = crate::cost::human_bytes(required.saturating_sub(*available));
                write!(
                    f,
                    "{DISK_REFUSAL_HEAD}: needs {need} free, {have} free here ({short} short)"
                )
            }
            FlowError::Linked(p) => write!(
                f,
                "{p} is dev-linked; run `aterm pkg unlink {p}` to release it"
            ),
            FlowError::Vendor(line)
            | FlowError::VendorUnreachable(line)
            | FlowError::VendorUnavailable(line) => f.write_str(line),
        }
    }
}

impl std::error::Error for FlowError {}

/// What to install: the `channel` to resolve the pin from, the `program`, the target
/// `triple`, and the active `installed` build (if any).
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The channel whose pin set names the build.
    pub channel: &'a str,
    /// The program to install.
    pub program: &'a str,
    /// The target triple to select an artifact for.
    pub triple: &'a str,
    /// The currently-active build, if any (the upgrade/up-to-date input).
    pub installed: Option<u64>,
}

/// Re-write `store/<program>/current` at `build` unless the prefix already proves that
/// build live for `program` ([`crate::gc::live_builds`]): the up-to-date path's one write.
/// A prefix from an older manager, a dangling `current`, or a channel link left at a
/// superseded build has no witness, so gc abstains and `doctor` names `update` as the
/// remedy; this makes that remedy work. A build not on disk is left for `doctor`.
fn reassert_witness(layout: &Layout, program: &str, build: u64) -> Result<(), FlowError> {
    if crate::gc::live_builds(layout)
        .get(program)
        .is_some_and(|w| w.build() == build)
    {
        return Ok(());
    }
    let build_dir = layout.build_dir(program, build);
    if !build_dir.is_dir() {
        return Ok(());
    }
    activate_build(layout, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))
}

/// The installed-build view [`decide`] may call up to date: the shim-derived build, dropped
/// to `None` when its tree is not a complete install for this slice
/// ([`crate::store::build_is_complete`]: the `<build>.ready` marker and platform record).
///
/// The shim view ([`crate::ops::active_builds`]) says what runs and ignores the marker, so
/// a live build whose marker is missing or was written by the other slice of the universal
/// binary (`arch -x86_64` installs Intel trees into the same `store/<program>/<build>/`)
/// would read as UpToDate forever while `doctor` reports it incomplete. Read as not
/// installed, it re-stages in place ([`Staged::was_live`] keeps the live tree through an
/// abort). This only turns UpToDate into Install, never suppresses a Tombstone, and a
/// marker this process cannot read is not proof of absence.
fn installed_for_decide(layout: &Layout, program: &str, installed: Option<u64>) -> Option<u64> {
    installed.filter(|&build| crate::store::build_is_complete(&layout.build_dir(program, build)))
}

/// Install (or force-upgrade) the program named by `req`, with `fetcher` for all network
/// I/O, `anchor` as the pinned master keyset and roster ratchet, and `floor` as the
/// durable index high-water paired with the roster generation that recorded it
/// ([`BuildFloor`]). The module docs give the pipeline. An unarmed `anchor` installs
/// nothing ([`FlowError::NoIndex`]).
pub fn install(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<InstallReport, FlowError> {
    install_collecting_assets(
        fetcher,
        layout,
        anchor,
        req,
        floor,
        now_unix,
        &mut BTreeMap::new(),
    )
}

/// [`install`], also filling `resolved_assets` with `program → pinned asset file name` for
/// every program (the requested one and each `requires` pull-in) resolved far enough to
/// select an artifact. An out-param because it must survive the `Err` path: a failed
/// download's `.part` is what the pass-end [`crate::gc::run_keeping_pinned_partials`]
/// spares. Over-collecting is harmless; the map only shrinks that sweep.
pub fn install_collecting_assets(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
    resolved_assets: &mut BTreeMap<String, String>,
) -> Result<InstallReport, FlowError> {
    let mut seen = BTreeMap::new();
    install_inner(
        fetcher,
        layout,
        anchor,
        req,
        floor,
        now_unix,
        &mut seen,
        resolved_assets,
    )
}

/// [`install`] with a cycle-guard `seen` map threaded through the `requires` recursion
/// (§17): a dependency on the resolution stack (`None`) is skipped as a cycle, and one a
/// sibling edge already resolved (`Some(outcome)`, a diamond) carries that outcome.
#[allow(
    clippy::too_many_arguments,
    reason = "install_inner is install plus the recursion cycle-guard set and the resolved-\
              asset collector; every other input is an irreducible dependency of a verified \
              install (fetcher, layout, root key, the request, and the floor + clock the \
              anti-rollback/freshness gates read)"
)]
fn install_inner(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
    seen: &mut BTreeMap<String, Option<DepResult>>,
    resolved_assets: &mut BTreeMap<String, String>,
) -> Result<InstallReport, FlowError> {
    let (channel, program, triple, installed) =
        (req.channel, req.program, req.triple, req.installed);
    // 0. Dev-link skip (§13): a linked program is managed from its checkout; checked
    //    before any network I/O.
    if crate::linkmode::is_linked(layout, program) {
        return Err(FlowError::Linked(program.to_string()));
    }
    // A vendor-direct program never waits on the index resolve below (design §1.5): the
    // vendor lane under the compiled anchors, as the flow's report. The one exception is
    // the source rule's ([`crate::alab`]): an ALab-built program on a target ALab builds it
    // for asks the verified index first, and goes on to the index lane below when that
    // index pins an ALab build for it here. Anything else (no index, the legacy row, a
    // revoked pin) is the vendor lane, whose door keeps an installed ALab build.
    if let Some(spec) = crate::vendor_direct::spec(program)
        && !crate::alab::agent(program).is_some_and(|agent| {
            agent.serves(triple)
                && resolve_verified_index(fetcher, layout, anchor, floor, now_unix)
                    .ok()
                    .is_some_and(|index| {
                        crate::alab::index_owns(layout, program, triple, Some(&index))
                    })
        })
    {
        return vendor_install_outcome(
            fetcher,
            layout,
            anchor,
            floor,
            req,
            spec,
            &crate::vendor_direct::lane::Trust::PRODUCTION,
            now_unix,
        )
        .into_install(layout);
    }
    seen.insert(program.to_string(), None);
    // 1–2. Resolve and verify-select the index with freshness ([`resolve_verified_index`]),
    // then reachability and the retired `requires` refusal (§5.3(b)).
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let row = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    if row.names_requires() {
        return Err(FlowError::RequiresRetired(program.to_string()));
    }
    let repo = &row.repo;
    // The channel as seen from this host: a per-target pin overlay for `triple` is
    // resolved here and nowhere else.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let ch = &ch;
    let &pinned = ch
        .pin
        .get(program)
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;

    // 3. The apply decision, from the completeness-aware view ([`installed_for_decide`]):
    //    a live build whose tree this slice cannot vouch for re-stages.
    match decide(
        ch,
        program,
        installed_for_decide(layout, program, installed),
    ) {
        ApplyDecision::UpToDate => {
            // No fetch, no stage — but the no-op pass still owns the liveness witness.
            reassert_witness(layout, program, pinned)?;
            return Ok(InstallReport {
                program: program.to_string(),
                build: pinned,
                index_build: index.index_build,
                roster_seq: index.roster_seq(),
                already_current: true,
                shimmed: vec![],
                refused_shims: vec![],
                // No manifest is fetched, so `requires` go unresolved on a no-op install.
                tree_root: String::new(),
                dependencies: vec![],
                vendor: None,
            });
        }
        ApplyDecision::Tombstone => {
            // Disable the revoked build's old working shims (§7) rather than merely report.
            // Best-effort: a write failure must not mask the Tombstoned error.
            install_tombstone_shims(layout, program, installed);
            return Err(FlowError::Tombstoned(program.to_string()));
        }
        ApplyDecision::NotPinned => return Err(FlowError::NotPinned(program.to_string())),
        ApplyDecision::Install => {}
    }

    // 4. Fetch + verify + parse the per-build manifest; bind program + build (anti-replay).
    let (raw, sig) = fetcher.pkg_manifest(repo, program, pinned).map_err(|e| {
        note_fetch_fault();
        FlowError::PkgFetch(e)
    })?;
    let verified = index
        .verify_pkg(raw, &sig)
        .map_err(|_| FlowError::PkgVerify)?;
    let pkg = parse_pkg(&verified).map_err(|e| match e {
        // The retired `kind = "vendor-fetch"` names the split that replaced it, so the
        // authoring machine reads the fix rather than a bare "malformed".
        crate::Reject::RetiredKind(why) => FlowError::RetiredKind(why),
        // So does a `shim_env` the rule refuses (design S7): the entry and the reason.
        crate::Reject::ShimEnv(why) => FlowError::ShimEnv(why),
        _ => FlowError::PkgParse,
    })?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return Err(FlowError::Mismatch);
    }

    // 4b. Runtime `requires` (§17): pull in each missing dep first, through the same
    // verified pipeline. Best-effort: a dep failure never fails the parent, and a
    // Tombstone/NotReachable is skipped, never bypassed. `requires` is signed manifest
    // metadata, so a repo-write adversary can neither inject nor redirect an edge.
    let requires = &pkg.requires;
    let mut dependencies: Vec<DepOutcome> = Vec::new();
    // One `bin/` scan for the whole loop: anything this recursion installs is in `seen`,
    // checked first, so a pre-loop snapshot decides identically.
    let active = crate::ops::active_builds(layout);
    for dep in requires {
        if let Some(earlier) = seen.get(dep) {
            match earlier {
                // Resolved earlier in this call by a sibling's recursion (a diamond): its
                // outcome stands for this edge too, a fresh install replayed as
                // AlreadyPresent so the caller records it once.
                Some(outcome) => {
                    if !dependencies.iter().any(|d| d.program == *dep) {
                        let result = match outcome {
                            DepResult::Installed { build, .. } => DepResult::AlreadyPresent(*build),
                            other => other.clone(),
                        };
                        dependencies.push(DepOutcome {
                            program: dep.clone(),
                            result,
                        });
                    }
                }
                // Still on the resolution stack (this program itself, or an ancestor).
                None => dependencies.push(DepOutcome {
                    program: dep.clone(),
                    result: DepResult::Skipped("requires cycle".into()),
                }),
            }
            continue;
        }
        if let Some(b) = active.get(dep).copied() {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::AlreadyPresent(b),
            });
            continue;
        }
        // A `system = "<bin>"` dependency the user's own copy satisfies is not pulled in:
        // the user's copy is the one that runs, and the next pass would retire a managed one.
        let system = index
            .program(dep)
            .and_then(|p| p.system.as_deref())
            .and_then(|bin| {
                crate::vendor::system_binary_on_path(
                    &layout.prefix,
                    bin,
                    crate::vendor::path_var().as_deref(),
                )
            });
        if let Some(path) = system {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::System(path),
            });
            continue;
        }
        let dep_req = InstallRequest {
            channel,
            program: dep.as_str(),
            triple,
            installed: None,
        };
        let (result, transitive) = match install_inner(
            fetcher,
            layout,
            anchor,
            &dep_req,
            floor,
            now_unix,
            seen,
            resolved_assets,
        ) {
            Ok(r) => (
                DepResult::Installed {
                    build: r.build,
                    tree_root: r.tree_root,
                },
                r.dependencies,
            ),
            Err(e) => (DepResult::Skipped(e.to_string()), Vec::new()),
        };
        seen.insert(dep.clone(), Some(result.clone()));
        dependencies.push(DepOutcome {
            program: dep.clone(),
            result,
        });
        dependencies.extend(transitive); // flatten the transitive closure
    }

    // 5. The artifact for this triple (missing triple = clean fail-closed skip).
    let artifact = pkg
        .artifact_for(triple)
        .ok_or_else(|| FlowError::NoArtifact(triple.to_string()))?;
    // Recorded before the download, so a transfer that fails midway still reaches the
    // collector: `gc::run_keeping_pinned_partials` spares these programs' `.part` files.
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    // Per-member dispatch (§16.4/§17) on both halves of the row: Shim (`binary`/`cargo-src`
    // over `github-release` or `https`), VendorApp (the `https` + `app-bundle` + `dmg`
    // lane, shimmed through its `links`) and SysrootBundle (pre-activation wiring and a
    // fail-loud resolve check, unwound on failure by [`abort_activated_install`]) install.
    // `app-bundle` over `github-release` (the aterm app) is refused, as is every unknown
    // pair. Every row is admitted here (`vendor::check_row`), before the disk preflight and
    // before any byte moves.
    crate::vendor::check_row(artifact, &pkg.exposes)?;
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    match strategy {
        crate::dispatch::ApplyStrategy::Shim
        | crate::dispatch::ApplyStrategy::SysrootBundle
        | crate::dispatch::ApplyStrategy::VendorApp => {}
        crate::dispatch::ApplyStrategy::AppBundle => {
            // The app updates itself; atpkg never swaps it ([`FlowError::AppBundleRefused`]).
            return Err(FlowError::AppBundleRefused(program.to_string()));
        }
        crate::dispatch::ApplyStrategy::Unknown => {
            return Err(FlowError::UnsupportedKind(artifact.kind.clone()));
        }
    }

    // 6–7. Download → verify-and-stage → activate → shim, the steps both lanes share.
    let shim_env = pkg.shim_env();
    let alab_gate = alab_pack_gate(program, &pkg.exposes);
    let hooks = stage_hooks(alab_gate.as_deref());
    let landed = land_artifact(
        layout,
        &Landing {
            program,
            build: pinned,
            artifact,
            exposes: &pkg.exposes,
            shim_env: &shim_env,
            // ALab's own tools get `alab-<tool>` aliases; a vendor extra or a
            // system-satisfiable member does not (`activate.rs` module doc).
            aliases: Aliases::for_program(program, index.program(program)),
            installed,
            strategy,
            hooks: &hooks,
            download_turn: None,
        },
        &|dl| fetch_artifact(fetcher, program, repo, artifact, dl),
    )?;
    // The names refused a shim go into the record the doctor's `base.pkg.refused` row reads
    // (design S13), here on the pass's thread where the signed `exposes` was admitted: no
    // verb that lays shims later holds that list, and [`land_artifact`] also runs on the
    // vendor lane's rider threads, which write no status. Diagnostics, never a failure.
    let _ = crate::status::record_refused_shims(layout, program, pinned, &landed.refused);
    Ok(InstallReport {
        program: program.to_string(),
        build: pinned,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        already_current: false,
        shimmed: landed.shimmed,
        refused_shims: landed.refused,
        tree_root: artifact.tree_root.clone(),
        dependencies,
        vendor: None,
    })
}

/// An install of a vendor-direct program (`req.program`, row `spec`): the vendor lane under
/// `trust` and the door's yanks ([`door_vendor_policy`]), as a person's own act (no
/// local-pin hold; an unread legacy build above the ceiling is replaced). Returns the
/// lane's outcome, from which the CLI edge records the program's row.
#[allow(
    clippy::too_many_arguments,
    reason = "the install request plus the index gate the door's yank refresh verifies under"
)]
pub(crate) fn vendor_install_outcome(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    req: &InstallRequest,
    spec: &'static crate::vendor_direct::VendorSpec,
    trust: &crate::vendor_direct::lane::Trust,
    now_unix: i64,
) -> crate::vendor_direct::lane::Outcome {
    // Where nothing is served the lane asks no host, so no index is verified for its yanks.
    let policy = if spec.serves(req.triple) {
        door_vendor_policy(fetcher, layout, anchor, floor, now_unix)
    } else {
        crate::vendor_direct::policy::Policy::default()
    };
    let lane = crate::vendor_direct::lane::Lane {
        layout,
        fetcher,
        triple: req.triple,
        policy: &policy,
        trust,
        legacy_index: None,
        asked: true,
        now: now_unix,
        download_turn: None,
    };
    crate::vendor_direct::lane::install_one(&lane, spec, false)
}

/// The signed yanks a targeted vendor door decides under (design §1.4–§1.5): a person's
/// `update <program>` or `install <program>`, `claude update`/`codex update`,
/// `--head-watch`. When the latch is absent or over an hour old
/// ([`crate::vendor_direct::policy::YankLatch::is_current`]), the index is verified live
/// first, refreshing the latch. That resolve never blocks or fails the door; unreached,
/// stale or refused, the door decides on the latch as it stands.
pub(crate) fn door_vendor_policy(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> crate::vendor_direct::policy::Policy {
    use crate::vendor_direct::policy::{Policy, YankLatch};
    if let Some(latch) = YankLatch::read(layout)
        && latch.is_current(now_unix)
    {
        return latch.policy;
    }
    let fresh = verify_select_fresh(fetcher, layout, anchor, floor, now_unix);
    Policy::current(layout, fresh.as_ref().ok())
}

/// One store landing: the admitted row, the build it becomes, and what its shims carry —
/// the inputs [`land_artifact`] shares between the index lane and the vendor lane.
pub(crate) struct Landing<'a> {
    /// The program.
    pub program: &'a str,
    /// The store build the row becomes.
    pub build: u64,
    /// The admitted row.
    pub artifact: &'a crate::manifest::Artifact,
    /// The tool names the build exposes (admitted here).
    pub exposes: &'a [String],
    /// What the shims export, recorded beside the build before it is staged.
    pub shim_env: &'a crate::shim_env::ShimEnv,
    /// Whether the tools get `alab-<tool>` aliases.
    pub aliases: Aliases,
    /// The active build the caller decided against.
    pub installed: Option<u64>,
    /// How the row applies ([`crate::dispatch::strategy_for`]).
    pub strategy: crate::dispatch::ApplyStrategy,
    /// The caller's gate and sidecars inside the stage.
    pub hooks: &'a crate::install::StageHooks<'a>,
    /// This landing's place in line for the link among landings side by side, waited for
    /// before the transfer and given up the moment it is over ([`DownloadTurns`]). `None`: no
    /// turn to wait for.
    pub download_turn: Option<&'a DownloadTicket<'a>>,
}

/// The order landings side by side take the link in: ONE TRANSFER AT A TIME, in the order
/// their programs were taken ([`Self::ticket`]) — not in the order their threads reach the
/// download, a coin flip between two lanes woken together — so the first program taken has
/// the whole link, however slow, while a later one's landing gets as far as its transfer and
/// waits there, its verify, unpack and activate overlapping the next transfer.
#[derive(Default)]
pub(crate) struct DownloadTurns {
    state: std::sync::Mutex<Turns>,
    changed: std::sync::Condvar,
}

/// The tickets taken and the ones still owed the link ([`DownloadTurns`]).
#[derive(Default)]
struct Turns {
    next: u64,
    open: std::collections::BTreeSet<u64>,
}

impl DownloadTurns {
    /// The next place in line, behind every ticket taken before it. The caller takes it as
    /// it takes the program, under the lock that orders the programs, so the tickets follow
    /// that order.
    pub(crate) fn ticket(&self) -> DownloadTicket<'_> {
        let mut turns = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = turns.next;
        turns.next = n.saturating_add(1);
        turns.open.insert(n);
        DownloadTicket { turns: self, n }
    }
}

/// One landing's place in line ([`DownloadTurns::ticket`]): its transfer waits for every
/// earlier ticket's to be over ([`Self::wait`]), and the ticket is done with the link at
/// [`Self::release`] — after the transfer, or as soon as the landing knows it moves no byte —
/// and at the latest when it is dropped, whatever the landing came to, so no later ticket
/// waits on a landing that ended.
pub(crate) struct DownloadTicket<'t> {
    turns: &'t DownloadTurns,
    n: u64,
}

impl DownloadTicket<'_> {
    /// Wait until every ticket taken before this one is done with the link.
    pub(crate) fn wait(&self) {
        let turns = self
            .turns
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.turns
                .changed
                .wait_while(turns, |turns| !is_next(turns, self.n))
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    /// Done with the link: the next ticket's transfer may start. Idempotent.
    pub(crate) fn release(&self) {
        self.turns
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .open
            .remove(&self.n);
        self.turns.changed.notify_all();
    }
}

impl Drop for DownloadTicket<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

/// Whether ticket `n` has the link: every ticket taken before it is done with it.
fn is_next(turns: &Turns, n: u64) -> bool {
    turns.open.first() == Some(&n)
}

/// Gives a held turn up when dropped: the transfer over, or an early return out of it.
struct TurnHeld<'a>(Option<&'a DownloadTicket<'a>>);

impl Drop for TurnHeld<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.0 {
            ticket.release();
        }
    }
}

/// What a landing laid.
pub(crate) struct Landed {
    /// The tools that got a shim.
    pub shimmed: Vec<String>,
    /// The exposed names refused a shim.
    pub refused: Vec<String>,
    /// The `tree_root` the stage folded.
    pub root: String,
}

/// Fetch the row's bytes with `fetch`, verify and stage them, activate the build and lay
/// its shims (install steps 6–7). Every failure past activation unwinds, and a stage
/// failure leaves the previous build live.
pub(crate) fn land_artifact(
    layout: &Layout,
    l: &Landing<'_>,
    fetch: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Landed, FlowError> {
    let (program, pinned, artifact, installed, strategy) =
        (l.program, l.build, l.artifact, l.installed, l.strategy);
    // This landing's turn at the link ([`Landing::download_turn`]), given up on every way out
    // of the transfer below — an early refusal before it included.
    let turn = TurnHeld(l.download_turn);
    // 6a. THE DIGEST-REFUSAL MEMO first, because it is the one gate that can spare the
    // download entirely: a recent attempt at THIS build's signed digests already proved
    // the published asset does not match them, and nothing about that verdict changes
    // while the pin and its digests do not ([`digest_refusal_note`]). Consulted here —
    // after the index, the manifest and the row have all been verified, so a memo can
    // only ever skip a transfer, never admit bytes or choose a build.
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact, false) {
        return Err(FlowError::StageRefused(note));
    }
    let dl = staged_download_path(layout, program, &artifact.asset)?;
    // THE ARCHIVE FETCHED AHEAD ([`ahead`]): the set completion's download-ahead lane may
    // already hold this program's download, or be moving it. Its answer stands in for the
    // fetch below, and the lane made this path's pre-transfer checks before it fetched
    // (this preflight, the stale-file rule, the partial sweep), so none is repeated over
    // a download already made; the extract's own preflight below still runs.
    let ahead = ahead::claim(program, &dl);
    // Disk preflight (§9): the compressed asset + its extracted tree must fit (they coexist
    // until the asset is reclaimed post-stage) while keeping the free floor. Fails OPEN when
    // free space can't be queried (available_bytes None) — preflight is a safety net.
    if ahead.is_none() {
        let required = LandingBytes::of(artifact).need();
        disk_gate(required, crate::freespace::available_bytes(&dl))?;
    }
    make_staging_dir(&dl).map_err(|e| FlowError::Download(e.to_string()))?;
    // Clear any stale staging file BEFORE fetching — unless it is the verified archive
    // a previous attempt's failed stage RETAINED ([`carried_archive`]):
    // that one is the download, done, and is staged again without a fetch. A leftover
    // from a killed run is not merely junk: for a `dir:` registry it is a HARDLINK to
    // the source, and writing "into" it writes into the registry — `curl -o` truncates
    // it, and `fs::copy` truncates the shared inode. Both destroy a file inside the
    // user's signed app bundle. See `DirFetcher::download`; `carried_archive` refuses
    // a hard link for exactly that reason.
    let reusable = ahead.is_none() && carried_archive(&dl, artifact);
    if ahead.is_none() && !reusable {
        let _ = std::fs::remove_file(&dl);
    }
    // A resumable transfer KEEPS its own `<asset>.part` across attempts — that is the
    // point — so the one thing that can leak here is a partial for an asset this program
    // has since moved past. `gc` reclaims those too (its `staging/` sweep removes every
    // regular file under `staging/<program>/` except a caller-named pinned partial, see
    // `gc::run_keeping_pinned_partials` — the sparing form every install/update pass ends
    // with, fed the `resolved_assets` this function collects; that wiring is what lets
    // THIS pinned partial outlive a failed pass at all), but `gc`'s standalone verb is one
    // a user may never run, so reclaim every OTHER partial now: at most one partial per
    // program survives between passes, and it is always the one the next attempt will
    // finish.
    if ahead.is_none() {
        sweep_foreign_partials(&dl);
    }
    // Live-progress hooks (R5): the download runs in a curl child, so the byte meter
    // is a sibling `.part` poller against the SIGNED size. Every hook is a no-op
    // unless a `--progress-file` pass is live.
    crate::progress::note_build(program, pinned);
    // No landing marker: a `claude` typed during the transfer runs
    // the build it has, silently, and activation's atomic flip below makes the next one
    // run this build ([`crate::landing`]).
    // ONE TRANSFER AT A TIME among landings that take turns ([`Landing::download_turn`]),
    // waited for before the meter starts — a row waiting for the link still reads queued, not
    // a download at 0% — and given back the moment the transfer is over. A landing that moves
    // no byte here (its archive fetched ahead or carried from the last attempt) gives its turn
    // up at once.
    if let Some(ticket) = turn.0 {
        if ahead.is_none() && !reusable {
            ticket.wait();
        } else {
            ticket.release();
        }
    }
    // The lane's download carried its own meter.
    let download_watch = ahead
        .is_none()
        .then(|| crate::progress::watch_download(program, &dl, artifact.size));
    if reusable {
        println!(
            "atpkg: {program}: reusing verified archive {} from the last attempt",
            artifact.asset
        );
    } else {
        let inline = ahead.is_none();
        if let Err(e) = ahead.unwrap_or_else(|| fetch(&dl)) {
            // The `.part` is left for the next attempt to resume; `dl` is reclaimed, since a
            // `dir:` registry's copy lane writes there directly.
            let _ = std::fs::remove_file(&dl);
            note_fetch_fault();
            return Err(FlowError::Download(e));
        }
        if inline {
            ahead::fetched_inline(program, &dl);
        }
    }
    // The transfer is over either way — stop the poller before the phase moves on, and let
    // the next landing's transfer start.
    drop(download_watch);
    drop(turn);
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    // Whether this build is already live, captured before the stage swaps a new tree in and
    // before `activate_build` moves the links; the shim-view `installed` cannot answer it
    // (see `abort_activated_install`).
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    // Preflight again before extract (only the tree remains to fit); a full volume must not
    // keep the archive that fills it.
    if let Err(e) = disk_gate(
        artifact.cost.disk_installed,
        crate::freespace::available_bytes(&build_dir),
    ) {
        let _ = std::fs::remove_file(&dl);
        return Err(e);
    }
    // Every exit reclaims the compressed asset (nothing else sweeps `staging/`), except
    // that a failed stage keeps a verified archive for the next attempt
    // ([`reclaim_after_failed_stage`]). The shim environment the signed manifest declares
    // (design S7) is recorded beside the build before staging, so the verbs that hold no
    // manifest (rollbacks, `unlink`'s restore) can re-lay this build's shims, and a build
    // without it is never marked ready.
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, l.shim_env) {
        let _ = std::fs::remove_file(&dl);
        let mut why = String::from("shim_env sidecar: ");
        why.push_str(&e.to_string());
        return Err(FlowError::Activate(why));
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let staged = verify_and_stage(artifact, &dl, &build_dir, l.hooks);
    drop(extract_scope);
    let root = match staged {
        Ok(root) => root,
        Err(e) => {
            record_digest_refusal(&build_dir, artifact, &e);
            reclaim_after_failed_stage(&dl, &e);
            return Err(FlowError::Stage(e));
        }
    };
    let _ = std::fs::remove_file(&dl);
    // This build's bytes are good, whatever an earlier pass recorded (a publisher who
    // repaired the asset under the same pin), so the memo goes.
    crate::store::clear_stage_refusal(&build_dir);

    // 6b. Sysroot-bundle wiring before activation (self-contained: a no-op).
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    // 7. Activate and shim. `exposes` is admitted once here: `tools` got a shim, `refused`
    //    are the sensitive/malformed names that did not.
    crate::progress::note_phase(program, crate::progress::Phase::Link);
    activate_build(layout, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(l.exposes);
    // Past activation a failure would leave the broken build live and marked ready, so
    // `decide` calls it UpToDate; both error arms below unwind through
    // `abort_activated_install` with this rollback input.
    let aliases = l.aliases;
    let staged = Staged {
        build: pinned,
        build_dir,
        exposes: tools,
        prior_build: installed,
        was_live,
        reloc: None,
        tree_root: String::new(),
        aliases,
    };
    let (build_dir, tools) = (&staged.build_dir, &staged.exposes);
    // The shims export the signed manifest's `shim_env` (design S7).
    if let Err(e) = install_tools_env(layout, build_dir, tools, aliases, l.shim_env) {
        abort_activated_install(layout, program, &staged);
        return Err(FlowError::Activate(e.to_string()));
    }

    // 7b. Fail-loud resolve check: an installed sysroot-bundle's compilers must load; a
    // loader failure aborts and unwinds the install.
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle
        && let Err(e) = bundle_resolve_check(build_dir, tools)
    {
        abort_activated_install(layout, program, &staged);
        return Err(e);
    }
    // The shell.d hook refresh runs at the CLI edge (`crate::hooks::refresh` in `cli.rs`),
    // not here, so flow's synthetic-layout tests never write the developer's ~/.aterm.
    let shimmed: Vec<String> = tools.iter().map(|t| t.as_str().to_string()).collect();
    Ok(Landed {
        shimmed,
        refused,
        root,
    })
}

/// The one place the download lanes fork, on the row's protocol (never its kind or
/// strategy), each capped at the signed `size` the disk preflight admitted. A
/// `github-release` member goes through [`Fetcher::download_for`] (the program's repo, the
/// `[packages.links]` override, the GitHub token); an `https` member through
/// [`Fetcher::download_url`] with the signed `url`, never `download_for`, so nothing about
/// the account slug reaches a vendor host. Anything else is refused by name. The row was
/// admitted by [`crate::vendor::check_row`].
fn fetch_artifact(
    fetcher: &dyn Fetcher,
    program: &str,
    repo: &str,
    artifact: &crate::manifest::Artifact,
    dl: &Path,
) -> Result<(), String> {
    match artifact.protocol.as_str() {
        "github-release" => fetcher.download_for(program, repo, &artifact.asset, dl, artifact.size),
        "https" => fetcher.download_url(&artifact.url, dl, artifact.size),
        other => {
            let mut m = String::from("protocol ");
            m.push_str(other);
            m.push_str(" has no download lane in this client");
            Err(m)
        }
    }
}

/// Re-point `program` at its retained build `to` (the vendor lane's signed-yank rollback):
/// the `current` flip, then shims for `exposes` alone under the build's `.shim-env`. Never
/// the build's `bin/` listing: a vendor's exposes come only from the table.
///
/// # Errors
/// The flip or the shims could not be written; a later pass decides the rollback again.
pub(crate) fn roll_back_to(
    layout: &Layout,
    program: &str,
    to: u64,
    exposes: &[String],
) -> std::io::Result<()> {
    let build_dir = layout.build_dir(program, to);
    activate_build(layout, &build_dir)?;
    let (tools, _) = crate::store::split_exposed(exposes);
    let env = crate::shim_env::read_sidecar(&build_dir);
    install_tools_env(layout, &build_dir, &tools, Aliases::Off, &env)
}

/// Lay a failing tombstone shim (§7) over every tool `program`'s active build exposes (its
/// live `bin/` shims), so a yanked or below-floor build is disabled. Nothing to do when not
/// installed. Best-effort per tool; a tombstone never shadows a sensitive name, since
/// `active_tools` yields only admitted [`crate::store::ToolName`]s. A shim an older atpkg
/// laid under a name no shim may carry now, forwarding into the program's store, is removed
/// rather than tombstoned (a tombstone under that name would itself be the shim the name is
/// refused), so a revoked build's `bin/pwsh` stops running it too.
pub(crate) fn install_tombstone_shims(layout: &Layout, program: &str, installed: Option<u64>) {
    if let Some(cur) = installed {
        // The `alab-<tool>` aliases forward to the same revoked executable, so they go too.
        let tools = crate::ops::active_tools(layout, program, cur);
        let aliases = crate::ops::active_aliases(layout, program, cur);
        for tool in tools.iter().chain(&aliases) {
            let _ = install_tombstone_shim(layout, tool);
        }
        crate::activate::remove_refused_name_shims(layout, Some(program));
    }
}

/// Unwind a per-program install that failed after [`activate_build`] (`install_tools` or
/// [`bundle_resolve_check`]): [`rollback_member`] restores the prior build's shims and
/// links (a fresh install removes its own), [`crate::activate::undo_activation`] sweeps any
/// pointer still naming the doomed build, and the staged build is discarded so
/// `active_builds`/`decide` never re-read a broken tree as current.
///
/// Except when `staged.was_live`: this install re-staged the build that was already live
/// (the shim view was silent, so `decide` returned Install). Its failure is a fact about
/// the environment (a shim write's EACCES, EROFS, full disk), and deleting a verified tree
/// for it would leave no toolchain. `undo_activation` still runs, so the next run
/// re-stages it rather than answering "already current". The per-program witness is then
/// re-pointed at the kept build, since the two steps above removed both links `was_live`
/// reads, and otherwise a second failure would discard the tree; only when nothing else
/// claims that link and the tree's `.ready` marker still reads complete.
fn abort_activated_install(layout: &Layout, program: &str, staged: &Staged) {
    rollback_member(layout, program, staged);
    crate::activate::undo_activation(layout, &staged.build_dir);
    if staged.was_live {
        // Re-point the witness at the kept tree, or the keep is one-shot. Exists, not
        // resolves, is the claim test (as in `gc::authority_claims`), so a link a
        // prior-build rollback wrote is never overwritten.
        if crate::store::build_is_complete(&staged.build_dir)
            && std::fs::symlink_metadata(layout.program_current(program)).is_err()
        {
            let _ = crate::activate::atomic_symlink(
                &staged.build_dir,
                &layout.program_current(program),
            );
        }
        return;
    }
    crate::store::discard_build(&staged.build_dir);
}

/// A member staged (verified and extracted) but not yet flipped, with what flip and
/// rollback need. Collected during the stage phase of [`transact`].
struct Staged {
    build: u64,
    build_dir: PathBuf,
    /// The admitted tools, not the raw manifest list; [`ToolName`]s so `rollback_member`
    /// probes the prior build via `exe_file()`.
    exposes: Vec<ToolName>,
    /// The member's prior active build (`None` ⇒ a fresh install: rollback removes the shims).
    prior_build: Option<u64>,
    /// Whether `build_dir` was already the live build when staged: a `current` link named it
    /// while the shim view ([`crate::ops::active_builds`]) was silent (tools tombstoned or
    /// unlinked), so `decide` returned Install. The abort discard never deletes such a tree.
    was_live: bool,
    /// `Some(reloc_policy)` for a `sysroot-bundle` member (its pre-activation wiring +
    /// post-activation resolve check run during the flip); `None` for a plain Shim.
    reloc: Option<String>,
    /// The signed `tree_root` of the staged build, surfaced in
    /// [`ChannelApplyReport::applied`] for `atpkg verify`.
    tree_root: String,
    /// Whether the member's tools get `alab-<tool>` aliases: from the signed index entry
    /// ([`Aliases::for_program`]) when staged, or from disk ([`Aliases::laid_for`]) by the
    /// rollback verb. Flip and rollback move them with the primaries.
    aliases: Aliases,
}

/// The result of a whole-channel transactional update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelApplyReport {
    /// The `index_build` of the signed index this apply trusted (to advance the floor).
    pub index_build: u64,
    /// The `roster_seq` that authorized it (to advance the durable roster floor).
    pub roster_seq: u64,
    /// Per coherence group: the group and its transaction outcome.
    pub groups: Vec<(Group, TxnOutcome)>,
    /// Per member flipped live, keyed by program: its new build and signed `tree_root`, for
    /// the CLI to persist into `status.toml` for `atpkg verify`.
    pub applied: BTreeMap<String, AppliedMember>,
    /// Members excluded from this apply because they are dev-linked (§13) — reported so the
    /// CLI can note the skip.
    pub skipped_linked: Vec<String>,
    /// `program → pinned artifact asset file name` for every member resolved far enough to
    /// select an artifact, including members whose download or stage then failed: the
    /// pass-end [`crate::gc::run_keeping_pinned_partials`] spares exactly these programs'
    /// `.part` resume files. An empty map is plain `gc::run`.
    pub resolved_assets: BTreeMap<String, String>,
}

/// A member the apply flipped live: its new build and signed `tree_root` for `atpkg verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMember {
    /// The build now active.
    pub build: u64,
    /// The signed `tree_root` of that build.
    pub tree_root: String,
}

/// What a [`rollback`] did: `program` moved from `from_build` down to `to_build` under the
/// signed index at `index_build`. `coherence_group` lets the CLI warn that rolling back
/// one member of a locked tuple splits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    /// The program rolled back.
    pub program: String,
    /// The build it was active on before.
    pub from_build: u64,
    /// The gate-valid build it is now active on (strictly below `from_build`).
    pub to_build: u64,
    /// The `index_build` of the signed index this rollback trusted (advance the floor to it).
    pub index_build: u64,
    /// The `roster_seq` that authorized it (advance the durable roster floor to it).
    pub roster_seq: u64,
    /// The program's coherence group, if it is in one (a per-program rollback splits it).
    pub coherence_group: Option<String>,
    /// The local pins that keep it on `to_build` through the next update: its own, or any
    /// group member's (one pinned member holds the tuple). Only pins the pass's gate reads
    /// ([`deliberately_absent`]), and none while [`Self::revoked_member`] is set. Empty when
    /// the next update moves it forward.
    pub held_by: Vec<String>,
    /// A member of its group whose current build is revoked (yanked or below the floor), with
    /// that build: the next update then moves the whole group forward whatever is pinned
    /// (the pin gate's `all_current_valid`).
    pub revoked_member: Option<(String, u64)>,
}

/// Update every program the channel pins, group by group, each coherence group applied as
/// an all-or-nothing transaction (§7): stage every member, then flip every member; a stage
/// failure flips nothing and a flip failure rolls the flipped members back. An ungrouped
/// tool applies independently. `installed` maps each program to its active build.
///
/// The index is resolved once per call, so a mid-run index change cannot split a group.
/// Once per call is not once per pass: a caller running a second lane hands the index it
/// holds to [`apply_channel_gated`] rather than resolving again.
#[allow(
    clippy::too_many_arguments,
    reason = "every input is an irreducible dependency of a verified channel apply: the \
              network fetcher, the install layout, the pinned root key, the channel + \
              triple selectors, the installed-build map, and the floor + clock the anti-\
              rollback and freshness gates read — bundling them into a struct would only \
              move the count to the single call site"
)]
pub fn apply_channel(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    // 1–2. Resolve and verify-select the index once, with freshness (§8, §14).
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    apply_channel_gated(
        fetcher,
        layout,
        &index,
        channel,
        triple,
        installed,
        excluded,
        &crate::quiet::FlipGate::NOW,
    )
}

/// [`apply_channel`] over a caller-resolved, verified index, under `flip`: the unattended
/// lanes' form, where the group holding the Trust toolchain stages its new builds and holds
/// its flip while that toolchain is in use ([`crate::quiet`], [`TxnOutcome::Deferred`]).
/// Every other group, and every group under [`crate::quiet::FlipGate::NOW`], applies at
/// once. `atpkg update` applies the channel, repairs a missing attestation and completes
/// the default set on one resolved index, so it cannot read two index states (§7).
///
/// It verifies nothing itself: `index` must come from [`resolve_verified_index`], and a
/// caller holding one across a long pass re-checks its freshness before the next lane.
#[allow(
    clippy::too_many_arguments,
    reason = "apply_channel's inputs, with the resolved index in place of its resolve \
              inputs, plus the flip gate the unattended lanes hand in"
)]
pub fn apply_channel_gated(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    flip: &crate::quiet::FlipGate<'_>,
) -> Result<ChannelApplyReport, FlowError> {
    // The channel as this target sees it (`pin_by_target` over `pin`), for every read below.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;

    // 3. Partition the pins into coherence groups (tuples + singletons), each its own
    //    all-or-nothing transaction.
    let report = apply_groups(
        fetcher,
        layout,
        index,
        &ch,
        triple,
        &crate::apply::plan_groups_here(layout, index, &ch, triple),
        installed,
        excluded,
        flip,
    );
    // A held flip no group holds any longer is over (the toolchain's group was skipped
    // whole this pass); clear its record. A group that held it again or aborted keeps it.
    if !report.groups.iter().any(|(g, o)| {
        g.members.iter().any(|m| m == crate::seam::SEAM_PROGRAM)
            && matches!(o, TxnOutcome::Deferred { .. } | TxnOutcome::Aborted { .. })
    }) {
        crate::quiet::clear(layout);
    }
    Ok(report)
}

/// Apply each of `groups` as its own all-or-nothing transaction ([`apply_group`]) — the
/// loop [`apply_channel_gated`] runs over the channel's groups and [`apply_program`] over
/// its one.
#[allow(
    clippy::too_many_arguments,
    reason = "apply_group's inputs, over a slice of groups"
)]
fn apply_groups(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    groups: &[Group],
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    flip: &crate::quiet::FlipGate<'_>,
) -> ChannelApplyReport {
    let mut report = ChannelApplyReport {
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        groups: Vec::with_capacity(groups.len()),
        applied: BTreeMap::new(),
        skipped_linked: Vec::new(),
        resolved_assets: BTreeMap::new(),
    };
    for group in groups {
        // Dev-linked skip (§13): a tuple with any linked member is skipped whole.
        let linked: Vec<String> = group
            .members
            .iter()
            .filter(|m| crate::linkmode::is_linked(layout, m))
            .cloned()
            .collect();
        if !linked.is_empty() {
            report.skipped_linked.extend(linked);
            continue;
        }
        if let Some((acted, outcome, group_applied)) = apply_group(
            fetcher,
            layout,
            index,
            ch,
            triple,
            group,
            installed,
            excluded,
            &mut report.resolved_assets,
            flip,
        ) {
            report.applied.extend(group_applied);
            report.groups.push((acted, outcome));
        }
    }
    report
}

/// Whether any of `programs` is installed here but disabled: a build this prefix still
/// witnesses as live whose `bin/` shims were replaced by the failing tombstone scripts a
/// revocation writes (§7). The shim view is silent for a tombstone
/// ([`crate::platform::resolve_shim`] yields `None`), which strands a group with no other
/// installed member; this is the test that revisits it.
///
/// * The shim must be present and refuse to run: a missing shim is an interrupted
///   uninstall, and the update pass must never install a program nobody asked for.
/// * Its name must match an executable of the program's live build
///   ([`crate::gc::live_builds`]): a tombstone carries no target, and without the match
///   any tombstone would speak for every program.
pub(crate) fn tombstoned_in_place<'a>(
    layout: &Layout,
    programs: impl IntoIterator<Item = &'a str>,
) -> bool {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return false;
    };
    // Every `bin/` name of a shim shape this manager could write that forwards nowhere.
    let tombstones: Vec<ToolName> = entries
        .flatten()
        .filter(|e| crate::platform::resolve_shim(&e.path()).is_none())
        .filter_map(|e| ToolName::from_shim_file(e.file_name().to_str()?))
        .collect();
    if tombstones.is_empty() {
        return false;
    }
    let live = crate::gc::live_builds(layout);
    programs.into_iter().any(|m| {
        live.get(m).is_some_and(|witness| {
            let build_bin = layout.build_dir(m, witness.build()).join("bin");
            // `presence`, not `exists`: an EACCES/EPERM is not proof the tool is gone.
            tombstones
                .iter()
                .any(|tool| !crate::store::presence(&build_bin.join(tool.exe_file())).is_absent())
        })
    })
}

/// Whether the update pass drops `m` from its coherence group before any gate reads it
/// ([`apply_group`]): recorded removed, on a machine that declined the set, or named by
/// `[packages].exclude`, and absent from `installed`. [`rollback`]'s pin report reads the
/// same test.
fn deliberately_absent<'a>(
    layout: &Layout,
    installed: &'a BTreeMap<String, u64>,
    excluded: &'a [String],
) -> impl Fn(&String) -> bool + use<'a> {
    let removed = layout.removed_programs();
    // `uninstall --all` writes `declined` and clears nothing per program, so installing
    // one program afterwards must not pull the rest of its tuple back.
    let declined = layout.declined().is_file();
    // `[packages].exclude` arrives as the `excluded` parameter, never via
    // `config::cached()`: that global is the invoking user's aterm.toml, and this layer
    // decides against a caller-supplied `layout`. The absence guard applies to exclusions
    // as to removals, so an excluded member the user has installed stays current.
    move |m: &String| {
        (declined || removed.contains(m) || excluded.contains(m)) && !installed.contains_key(m)
    }
}

/// The members whose local pins the update pass's group gate reads for `program`
/// ([`apply_group_txn`]): its coherence group as [`plan_groups`] plans it on `ch` (`program`
/// alone when ungrouped), less the members the pass drops unread ([`deliberately_absent`]).
/// [`rollback`]'s pin report and `pin`'s line both read it, so neither names a pin the pass
/// does not read.
pub(crate) fn pin_gate_members(
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    active: &BTreeMap<String, u64>,
    excluded: &[String],
) -> Vec<String> {
    let absent = deliberately_absent(layout, active, excluded);
    crate::apply::plan_groups_here(layout, index, ch, crate::cli::current_triple())
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .map_or_else(|| vec![program.to_string()], |g| g.members)
        .into_iter()
        .filter(|m| !absent(m))
        .collect()
}

/// Apply one coherence group as an all-or-nothing transaction (the per-group body of
/// [`apply_channel`] and [`apply_program`]). `None`: the group has no installed member and
/// was skipped (that would be a fresh `install`).
///
/// [`crate::gate::decide`] is the security authority, evaluated first. The consumer gates
/// run strictly after it and can only suppress or abort, never move a build:
/// * **local pin**: a pinned member holds the whole tuple on its current builds when it
///   wants an upgrade ([`TxnOutcome::Pinned`]); a Tombstone anywhere overrides the pin;
/// * **missing-triple hold**: a new pin proven (from its bound, verified manifest) to
///   publish nothing for this triple holds the tuple ([`TxnOutcome::Unpublished`]), unless
///   a current build is revoked, which aborts with its commands disabled;
/// * **disk preflight**: a group shortfall aborts before staging; fails open on any
///   query/fetch failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the triple \
              selector, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> Option<(Group, TxnOutcome, BTreeMap<String, AppliedMember>)> {
    // `update` touches installed groups only; a group with one member installed is applied
    // whole (missing siblings are pulled in to the pin).
    //
    // A tombstoned member still counts as installed: `installed` comes from the shim view
    // ([`crate::ops::active_builds`]), where a tombstone resolves to nothing, so a
    // tombstoned singleton would otherwise never be repaired by the `aterm pkg update` its
    // message asks for. The test is a tombstone still on `PATH` ([`tombstoned_in_place`]),
    // not a store witness: a shim that is gone is an interrupted uninstall, not a request.
    // `installed` itself, the map `decide` reads, is not widened (that would answer
    // UpToDate for an unmoved pin); `decide` still returns Install, the re-stage
    // [`Staged::was_live`] exists for. The scan runs only when the shim view was silent
    // for every member.
    if group.members.iter().all(|m| !installed.contains_key(m))
        && !tombstoned_in_place(layout, group.members.iter().map(String::as_str))
    {
        return None;
    }
    // A deliberately absent member ([`deliberately_absent`]: removed, declined, excluded)
    // is dropped from the tuple, and nothing else: the coherence rule would otherwise pull
    // it straight back, while the members that are here still take the ordinary
    // transaction (revocation, upgrades, the pin gate, reporting).
    let absent = deliberately_absent(layout, installed, excluded);
    let acted = Group {
        group: group.group.clone(),
        members: group
            .members
            .iter()
            .filter(|m| !absent(m))
            .cloned()
            .collect(),
    };
    // Nothing left to act on (`transact` would report UpToDate for an empty set).
    if acted.members.is_empty() {
        return None;
    }
    // Report the group acted on, not the nominal one, so no status row claims a program
    // the user removed.
    let (outcome, applied) = apply_group_txn(
        fetcher,
        layout,
        index,
        ch,
        triple,
        &acted,
        installed,
        resolved_assets,
        flip,
    );
    Some((acted, outcome, applied))
}

/// §11 bootstrap: apply one coherence group all-or-nothing against a caller-resolved,
/// verified index, with the update path's transaction semantics minus its "at least one
/// member installed" guard. The caller verifies the index once for its whole pass, so a
/// mid-pass index publish never splits a fresh tuple across two index states (§7).
///
/// `resolved_assets` collects `program → pinned asset file name` for every member resolved
/// far enough to select an artifact, a failed download included, so the pass-end
/// [`crate::gc::run_keeping_pinned_partials`] spares its `.part`; an out-param because it
/// must survive every failure. `flip` is the caller's flip gate, as for
/// [`apply_channel_gated`].
#[allow(
    clippy::too_many_arguments,
    reason = "the bootstrap group apply needs the same irreducible inputs as apply_group \
              minus the pre-resolved channel it looks up itself: fetcher, layout, verified \
              index, channel + triple selectors, the group, the installed-build map, the \
              caller's resolved-asset collector the pass-end gc sparing reads, and the \
              caller's flip gate"
)]
pub fn bootstrap_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> Result<(TxnOutcome, BTreeMap<String, AppliedMember>), FlowError> {
    // The per-target view the caller's plan and prescan saw; the raw pin would fetch
    // another target's build.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    Ok(apply_group_txn(
        fetcher,
        layout,
        index,
        &ch,
        triple,
        group,
        installed,
        resolved_assets,
        flip,
    ))
}

/// §11 bootstrap prescan: `Some(member)` iff one of `members` has no artifact for `triple`
/// in its pinned, verified manifest, so the caller skips the whole group cleanly (a tuple
/// that cannot fully exist on this host is a correct state, not a failure). `None`: nothing
/// proven; a fetch/verify/parse failure defers to the real stage.
pub fn group_missing_triple(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    members: &[String],
) -> Option<String> {
    group_missing_triple_reading(fetcher, index, channel, triple, members, &mut |_, _| {})
}

/// [`group_missing_triple`], handing `read` each member whose verified manifest carries this
/// triple, with that manifest: the one verified read per member answers both questions (the
/// set completion's pending stubs take each member's `exposes` from it).
pub(crate) fn group_missing_triple_reading(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    members: &[String],
    read: &mut dyn FnMut(&str, &crate::manifest::PkgManifest),
) -> Option<String> {
    let ch = index.channel_for(channel, triple)?;
    let ch = &ch;
    for m in members {
        let Some((_, _, pkg)) = verified_pkg(fetcher, index, ch, m) else {
            continue;
        };
        if pkg.artifact_for(triple).is_none() {
            return Some(m.clone());
        }
        read(m, &pkg);
    }
    None
}

/// The transaction body shared by [`apply_group`] (update: installed groups only) and
/// [`bootstrap_group`] (§11 fresh install): decide first, the retired-`requires` refusal
/// ([`FlowError::RequiresRetired`]), the local-pin hold, the missing-triple hold
/// ([`TxnOutcome::Unpublished`]) and group disk preflight, then stage-all → flip-all →
/// rollback via [`transact`], with the abort discard, tombstone-shim disable and applied
/// `tree_root` capture.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the triple \
              selector, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group_txn(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    let (outcome, applied) = apply_group_txn_inner(
        fetcher,
        layout,
        index,
        ch,
        triple,
        group,
        installed,
        resolved_assets,
        flip,
    );
    // The deferral's record ends with every outcome but another wait or an abort (the next
    // pass retries the same flip, and the ceiling still counts from the first wait).
    if group.members.iter().any(|m| m == crate::seam::SEAM_PROGRAM)
        && !matches!(
            outcome,
            TxnOutcome::Deferred { .. } | TxnOutcome::Aborted { .. }
        )
    {
        crate::quiet::clear(layout);
    }
    (outcome, applied)
}

/// What [`apply_group_txn`] decides for a group before it reads a manifest: each member's
/// [`decide`] on the completeness-aware view ([`installed_for_decide`]), and the two gates
/// that read nothing else (the retired-`requires` refusal and the local-pin hold).
/// [`planned_landing`] reads the same decisions.
struct GroupDecisions {
    /// Per member, in member order: what `transact` runs.
    decisions: Vec<(String, ApplyDecision)>,
    /// Some member's pin is revoked: `transact` tombstones the group, and no hold applies.
    any_tombstone: bool,
    /// Some Install member moves to a different build, not only a re-stage at its pin.
    wants_upgrade: bool,
    /// Every installed member's current build is still gate-valid: the guard every hold
    /// must pass.
    all_current_valid: bool,
}

impl GroupDecisions {
    fn of(layout: &Layout, ch: &Channel, group: &Group, installed: &BTreeMap<String, u64>) -> Self {
        // The completeness-aware view ([`installed_for_decide`]): on the raw shim view a
        // member whose `<build>.ready` is missing would be UpToDate forever.
        let decisions: Vec<(String, ApplyDecision)> = group
            .members
            .iter()
            .map(|m| {
                let installed = installed_for_decide(layout, m, installed.get(m).copied());
                (m.clone(), decide(ch, m, installed))
            })
            .collect();
        let any_tombstone = decisions
            .iter()
            .any(|(_, d)| *d == ApplyDecision::Tombstone);
        // An upgrade moves the tuple to a different build. A member that is Install only
        // because its tree is incomplete for this slice is re-staged at its pin: a repair a
        // local pin may not suppress.
        let wants_upgrade = decisions
            .iter()
            .any(|(m, d)| *d == ApplyDecision::Install && installed.get(m) != ch.pin.get(m));
        // Whether every installed member's current build is still gate-valid, the guard
        // every hold must pass: a yanked or below-floor current build makes decide() return
        // Install to force-upgrade off it, and a hold would keep it running.
        let all_current_valid = group
            .members
            .iter()
            .all(|m| crate::gate::current_build_ok(ch, m, installed.get(m).copied()));
        Self {
            decisions,
            any_tombstone,
            wants_upgrade,
            all_current_valid,
        }
    }

    /// The members decide() marked Install (what `transact` would stage), in member order.
    fn install_members(&self) -> Vec<&String> {
        self.decisions
            .iter()
            .filter(|(_, d)| *d == ApplyDecision::Install)
            .map(|(n, _)| n)
            .collect()
    }

    /// The retired `requires` (§5.3(b)): the first member whose signed row names the
    /// relation, when the group has something to install. Never over a Tombstone or a
    /// force-upgrade off a revoked current build, like [`Self::pin_hold`].
    fn retired_requires(&self, index: &TrustedIndex) -> Option<&String> {
        if self.any_tombstone
            || !self.all_current_valid
            || !self
                .decisions
                .iter()
                .any(|(_, d)| *d == ApplyDecision::Install)
        {
            return None;
        }
        self.decisions.iter().map(|(m, _)| m).find(|m| {
            index
                .program(m)
                .is_some_and(crate::manifest::Program::names_requires)
        })
    }

    /// The local pin gate: the pinned members, when one freezes the whole tuple on its
    /// current builds ([`TxnOutcome::Pinned`]). Only for a tuple that wants an upgrade, and
    /// never over a Tombstone.
    fn pin_hold(&self, layout: &Layout) -> Option<Vec<String>> {
        if self.any_tombstone || !self.wants_upgrade {
            return None;
        }
        let held: Vec<String> = self
            .decisions
            .iter()
            .map(|(m, _)| m)
            .filter(|m| crate::pin::is_pinned(layout, m))
            .cloned()
            .collect();
        // A hold freezes every member on its current build, so it applies only while every
        // current build is gate-valid; otherwise the pin is ignored and the tuple
        // force-upgrades.
        (!held.is_empty() && self.all_current_valid).then_some(held)
    }
}

/// [`apply_group_txn`]'s body.
#[allow(
    clippy::too_many_arguments,
    reason = "apply_group_txn's inputs, passed through whole"
)]
fn apply_group_txn_inner(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    let decided = GroupDecisions::of(layout, ch, group, installed);

    // The retired `requires`, strictly after decide() and in the body the update and
    // bootstrap lanes share, so every lane refuses it as `install` does: a group with
    // something to install whose member's signed row names it aborts before any download.
    if let Some(member) = decided.retired_requires(index) {
        return (
            TxnOutcome::Aborted {
                failed: member.clone(),
                during_flip: false,
                why: FlowError::RequiresRetired(member.clone()).to_string(),
            },
            BTreeMap::new(),
        );
    }

    // Local pin gate: strictly after decide(), suppression-only; nothing is staged.
    if let Some(held) = decided.pin_hold(layout) {
        return (TxnOutcome::Pinned(held), BTreeMap::new());
    }
    let decisions = &decided.decisions;
    let (any_tombstone, all_current_valid) = (decided.any_tombstone, decided.all_current_valid);

    // One walk over the manifests of the members decide() marked Install (exactly those
    // `transact` would stage) feeds both gates below: the missing-triple hold and the
    // group disk preflight. An UpToDate group walks nothing, so it is never aborted on a
    // low disk.
    let install_members = decided.install_members();
    let need = if install_members.is_empty() {
        InstallNeed::Unknown
    } else {
        group_install_need(fetcher, index, ch, &install_members, triple)
    };

    // The missing-triple hold: the bootstrap's clean-skip doctrine ([`group_missing_triple`]:
    // a tuple that cannot fully exist on this host is a correct state) lifted to an
    // installed tuple, so a pin not built for this triple does not abort every pass.
    //
    // Strictly after decide(), suppression-only and proof-only: the walk must show the
    // missing row in the verified manifest of a member this pass would stage, bound to its
    // pinned program and build; anything less lets the real stage run and fail loudly. Only
    // a group with something installed holds. Never over a Tombstone, and never quietly
    // over a force-upgrade off a revoked build: its commands are disabled and the group
    // aborts loudly.
    if !any_tombstone
        && group.members.iter().any(|m| installed.contains_key(m))
        && let InstallNeed::Unpublished { member, build } = &need
    {
        if all_current_valid {
            return (
                TxnOutcome::Unpublished {
                    member: member.clone(),
                    build: *build,
                    triple: triple.to_string(),
                },
                BTreeMap::new(),
            );
        }
        let disabled = disable_revoked_currents(layout, ch, decisions, installed);
        for program in &disabled {
            println!(
                "{}",
                recalled_unpublished_notice(program, member, *build, triple)
            );
        }
        // The abort names the program it disabled, not the unpublished member: the next
        // pass sees the disabled program as not installed and takes the quiet hold above,
        // which rewrites only installed members' rows, so this row must outlive the pass.
        return (
            TxnOutcome::Aborted {
                failed: disabled.first().unwrap_or(member).clone(),
                during_flip: false,
                why: format!(
                    "its current build was recalled and the group's new pin ({member} build \
                     {build}) is not published for {triple}, so its commands are disabled \
                     until it is"
                ),
            },
            BTreeMap::new(),
        );
    }

    // Group-aggregated disk preflight (§9): one all-or-nothing check for the whole tuple,
    // over the walk's aggregate. A walk that proved no aggregate fails open.
    if let InstallNeed::Published { bytes, .. } = &need
        && let Err(short) = disk_gate(
            bytes.need(),
            crate::freespace::available_bytes(&layout.prefix),
        )
    {
        // Stage nothing; the group stays on its current builds. But `decide` returns
        // Install to force-upgrade off a yanked or below-floor build, so that build is
        // disabled anyway: the upgrade could not be afforded, the disabling costs nothing.
        let disabled = disable_revoked_currents(layout, ch, decisions, installed);
        // Say it: the abort line below says the group stays on its previous builds, the
        // opposite of what just happened to these members.
        if let FlowError::InsufficientDisk {
            required: need,
            available: have,
        } = &short
        {
            let (need_words, have_words) = crate::cost::human_bytes_apart(*need, *have);
            for program in &disabled {
                println!(
                    "atpkg: {program} was recalled and its replacement needs {need_words} free \
                     ({have_words} free here) \u{2014} its commands are disabled until you free \
                     {} and run `aterm pkg update`",
                    crate::cost::human_bytes(need.saturating_sub(*have))
                );
            }
        }
        return (
            TxnOutcome::Aborted {
                failed: install_members[0].clone(),
                during_flip: false,
                why: short.to_string(),
            },
            BTreeMap::new(),
        );
    }

    // Flip when quiet ([`crate::quiet`]): strictly after decide() and every gate above,
    // suppression-only. An unattended pass whose group holds the Trust toolchain asks,
    // before extracting, whether the toolchain it would supersede is in use (leased, run
    // from, or unknown). In use and short of the ceiling, the group stages its signed
    // archives into `staging/`, records the wait, and flips nothing. A revoked build never
    // waits, and a person's pass never asks ([`crate::quiet::FlipPolicy::Now`]).
    let quiet = QuietGate::new(ch, group, installed, &install_members, flip);
    let forced = any_tombstone || !all_current_valid;
    let mut at_ceiling = false;
    // A wait that could not be recorded: nothing would bound it, so the group flips now
    // ([`crate::quiet::note_deferred`]), and the last look does not hold it again.
    let mut unrecorded = false;
    if let Some(q) = &quiet {
        let busy = crate::quiet::probe(layout, &q.live, flip.running);
        match crate::quiet::decide(
            flip.policy,
            forced,
            busy.is_quiet(),
            crate::quiet::since_of(layout, &q.label),
            flip.now,
        ) {
            crate::quiet::Verdict::Flip => {}
            crate::quiet::Verdict::FlipAtCeiling { since } => {
                at_ceiling = true;
                println!(
                    "atpkg: installing the Trust toolchain update ({}) after waiting {}, \
                     though {}; a command already running finishes on the old build",
                    q.moves_words(),
                    waited_words(flip.now.saturating_sub(since)),
                    busy.clause()
                );
            }
            crate::quiet::Verdict::Defer { since } => {
                for m in &install_members {
                    let mut why = String::new();
                    let Some(fetched) = fetch_member(
                        fetcher,
                        layout,
                        index,
                        ch,
                        m,
                        triple,
                        resolved_assets,
                        &mut why,
                    ) else {
                        return (
                            TxnOutcome::Aborted {
                                failed: (*m).clone(),
                                during_flip: false,
                                why,
                            },
                            BTreeMap::new(),
                        );
                    };
                    // A fresh download is checked against its signed digest now, so what
                    // waits in `staging/` is verified (a carried one already was); a mismatch
                    // is recorded and reclaimed as the stage would. The digest alone:
                    // [`carried_archive`]'s one-link rule would refuse a `dir:` registry's
                    // hard-linked download.
                    if !fetched.carried
                        && let Err(err) =
                            crate::install::check_signed_digest(&fetched.artifact, &fetched.dl)
                    {
                        record_digest_refusal(
                            &layout.build_dir(m, fetched.pinned),
                            &fetched.artifact,
                            &err,
                        );
                        reclaim_after_failed_stage(&fetched.dl, &err);
                        eprintln!(
                            "{}",
                            stage_failure_note(m, fetched.pinned, &err.to_string())
                        );
                        return (
                            TxnOutcome::Aborted {
                                failed: (*m).clone(),
                                during_flip: false,
                                why: format!("staging {} failed: {err}", fetched.artifact.asset),
                            },
                            BTreeMap::new(),
                        );
                    }
                }
                if crate::quiet::note_deferred(
                    layout,
                    &q.label,
                    since,
                    flip.now,
                    &busy,
                    &q.moves(resolved_assets),
                ) {
                    return (
                        TxnOutcome::Deferred {
                            members: install_members.iter().map(|m| (*m).clone()).collect(),
                            why: busy.clause(),
                            since,
                        },
                        BTreeMap::new(),
                    );
                }
                unrecorded = true;
                println!(
                    "atpkg: the Trust toolchain update ({}) is installed now, though {}: its \
                     wait could not be recorded in {}; a command already running finishes on \
                     the old build",
                    q.moves_words(),
                    busy.clause(),
                    crate::quiet::record_path(layout).display()
                );
            }
        }
    }

    // Per-group transaction. `staged` is filled by the stage closure and read by flip/rollback.
    let staged: RefCell<BTreeMap<String, Staged>> = RefCell::new(BTreeMap::new());
    // The pass's resolved assets, shared for the transaction: the stage closure records each
    // member's archive, and the last look reads them into a held wait's record
    // ([`crate::quiet::held_archives`]).
    let assets: RefCell<BTreeMap<String, String>> = RefCell::new(std::mem::take(resolved_assets));
    // Each bundle member's resolve check, started the moment it staged ([`ResolveAhead`]) and
    // taken by its flip.
    let ahead: RefCell<BTreeMap<String, ResolveAhead>> = RefCell::new(BTreeMap::new());
    let outcome = transact_holding(
        decisions,
        &mut |m| {
            let mut why = String::new();
            match stage_member(
                fetcher,
                layout,
                index,
                ch,
                m,
                triple,
                installed.get(m).copied(),
                &mut assets.borrow_mut(),
                &mut why,
            ) {
                Some(s) => {
                    if let Some(check) = ResolveAhead::start(&s) {
                        ahead.borrow_mut().insert(m.to_string(), check);
                    }
                    staged.borrow_mut().insert(m.to_string(), s);
                    Ok(())
                }
                None => Err(if why.is_empty() {
                    String::from("the member could not be staged (no reason recorded)")
                } else {
                    why
                }),
            }
        },
        // The last look, between the last extract and the first flip: a build that started
        // during a long extraction holds the flip after all, unless the ceiling has spoken.
        &mut || {
            let q = quiet.as_ref().filter(|_| !at_ceiling && !unrecorded)?;
            let busy = crate::quiet::probe(layout, &q.live, flip.running);
            let crate::quiet::Verdict::Defer { since } = crate::quiet::decide(
                flip.policy,
                forced,
                busy.is_quiet(),
                crate::quiet::since_of(layout, &q.label),
                flip.now,
            ) else {
                return None;
            };
            // Held only when the wait is recorded: an unrecorded one would have no ceiling.
            crate::quiet::note_deferred(
                layout,
                &q.label,
                since,
                flip.now,
                &busy,
                &q.moves(&assets.borrow()),
            )
            .then(|| (busy.clause(), since))
        },
        &mut |m| {
            // The flip is the group lane's link phase.
            crate::progress::note_phase(m, crate::progress::Phase::Link);
            let check = ahead.borrow_mut().remove(m);
            staged
                .borrow()
                .get(m)
                .is_some_and(|s| flip_member(layout, m, s, check))
        },
        &mut |m| {
            if let Some(s) = staged.borrow().get(m) {
                rollback_member(layout, m, s);
            }
        },
    );
    *resolved_assets = assets.into_inner();
    // A check no flip took (a stage-phase abort, a held flip) is waited out here, before
    // any tree it runs from is discarded: none outlives the transaction.
    drop(ahead);
    // On abort, discard the builds this transaction staged. None was left active (a
    // flip-phase abort rolled every shim back), and a complete-but-inactive build would make
    // `list_installed`/`decide` read it as active next run, splitting the tuple. The staged
    // build can be the one `current` already names, because `decide` reads the shim view
    // ([`crate::ops::active_builds`]), which is silent when a program's tools are
    // tombstoned or unlinked; `Staged::was_live` skips it. Across processes this rests on
    // the single-writer store lock ([`crate::lock`]) taken at the CLI edge. A held flip
    // ([`TxnOutcome::Deferred`]) discards the same way; its archives stay (below).
    if matches!(
        outcome,
        TxnOutcome::Aborted { .. } | TxnOutcome::Deferred { .. }
    ) {
        for s in staged.borrow().values() {
            // Never delete a build that was already live when re-staged: gc protects it
            // ([`crate::gc::live_builds`]), and deleting it would leave `current` dangling.
            if s.was_live {
                continue;
            }
            crate::store::discard_build(&s.build_dir);
        }
    } else {
        // …but not their downloads. A staged member's archive is reclaimed here, once the
        // tuple's fate is known: settled, the bytes go; aborted or deferred, they are kept
        // so the next pass does not repeat the download (an archive is inert; nothing
        // resolves or executes into `staging/`). The pass-end
        // `gc::run_keeping_pinned_partials` spares `<asset>` beside `<asset>.part` for the
        // programs this pass resolved, so at most one of each survives per program. A
        // member that failed already reclaimed its own archive in `stage_member`.
        for program in staged.borrow().keys() {
            if let Some(asset) = resolved_assets.get(program)
                && let Ok(dl) = staged_download_path(layout, program, asset)
            {
                let _ = std::fs::remove_file(&dl);
            }
        }
    }
    // A tombstoned member's old working shims are disabled, not just reported (§7), so a
    // revoked build is not left runnable. Best-effort; the report is the source of truth.
    if let TxnOutcome::Tombstoned(members) = &outcome {
        for m in members {
            install_tombstone_shims(layout, m, installed.get(m).copied());
        }
    }
    // The signed tree_root of each flipped member, for the CLI to record for `atpkg verify`.
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    if let TxnOutcome::Applied(members) = &outcome {
        for m in members {
            if let Some(s) = staged.borrow().get(m) {
                applied.insert(
                    m.clone(),
                    AppliedMember {
                        build: s.build,
                        tree_root: s.tree_root.clone(),
                    },
                );
            }
        }
    }
    (outcome, applied)
}

/// What [`apply_group_txn`] reads to hold a toolchain flip for quiet ([`crate::quiet`]):
/// built only for an unattended pass ([`crate::quiet::FlipPolicy::WhenQuiet`]) moving the
/// Trust compiler itself off a live build.
struct QuietGate {
    /// The group's name, the record's key.
    label: String,
    /// Member → the live build the flip would move off (what is probed).
    live: BTreeMap<String, u64>,
    /// Member → the build it would move onto.
    to: BTreeMap<String, u64>,
}

impl QuietGate {
    fn new(
        ch: &Channel,
        group: &Group,
        installed: &BTreeMap<String, u64>,
        install_members: &[&String],
        flip: &crate::quiet::FlipGate<'_>,
    ) -> Option<Self> {
        // The compiler must be one of the members that move (moving only a sibling leaves
        // every build in flight on its trustc), and it must have a live build to move off
        // (a trust re-staged from a tombstone supersedes nothing anything could run).
        let applies = flip.policy == crate::quiet::FlipPolicy::WhenQuiet
            && install_members
                .iter()
                .any(|m| *m == crate::seam::SEAM_PROGRAM)
            && installed.contains_key(crate::seam::SEAM_PROGRAM);
        applies.then(|| Self {
            label: group
                .group
                .clone()
                .unwrap_or_else(|| group.members.join("+")),
            live: group
                .members
                .iter()
                .filter_map(|m| installed.get(m).map(|b| (m.clone(), *b)))
                .collect(),
            to: install_members
                .iter()
                .filter_map(|m| ch.pin.get(*m).map(|b| ((*m).clone(), *b)))
                .collect(),
        })
    }

    /// The moves, as the record says them.
    fn moves_words(&self) -> String {
        crate::quiet::Deferral {
            from: self.live.clone(),
            to: self.to.clone(),
            ..Default::default()
        }
        .moves()
    }

    /// The record's moves: the builds, and each moving member's staged archive as the pass
    /// resolved it (member → asset file name), which every gc spares while the wait stands
    /// ([`crate::quiet::held_archives`]).
    fn moves(&self, resolved: &BTreeMap<String, String>) -> crate::quiet::Moves {
        crate::quiet::Moves {
            from: self.live.clone(),
            to: self.to.clone(),
            assets: self
                .to
                .keys()
                .filter_map(|m| resolved.get(m).map(|asset| (m.clone(), asset.clone())))
                .collect(),
        }
    }
}

/// `4 h 10 min`, `55 min`: how long a flip waited, for the ceiling line.
fn waited_words(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// Whether `dl` is a complete, already-verified copy of `artifact`'s compressed asset that
/// this pass may stage without refetching.
///
/// A coherence group's abort discards every staged sibling's extracted tree (a
/// complete-but-inactive build would read as active and split the tuple), but not its
/// download: a staged member's archive is kept until the tuple's fate is known, and this
/// admits it back, so a retry refetches only the members that failed.
///
/// A reuse gate, never a verification shortcut: it can only skip a download. The file is
/// accepted only when it is a regular file whose digest equals the signed one
/// ([`crate::install::check_signed_digest`]; no signed digest, no reuse), and
/// `verify_and_stage` re-checks that digest and the signed `tree_root` as for a fresh
/// download. Not pre-filtered on the signed `size`, which is the download cap, not a
/// promise about a file on disk.
fn carried_archive(dl: &Path, artifact: &crate::manifest::Artifact) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dl) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // A hard link into a `dir:` registry (a stale staging entry) is the registry's own
    // inode, so it is never carried: one link, one owner, or it is fetched afresh.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() != 1 {
            return false;
        }
    }
    crate::install::check_signed_digest(artifact, dl).is_ok()
}

/// Disable the live commands of every member `decide` forced off its current build (an
/// Install whose current build is yanked or below the floor) with failing tombstone shims,
/// and return their names: an abort that leaves the tuple on its current builds must not
/// leave a revoked build runnable.
fn disable_revoked_currents(
    layout: &Layout,
    ch: &Channel,
    decisions: &[(String, ApplyDecision)],
    installed: &BTreeMap<String, u64>,
) -> Vec<String> {
    let disabled: Vec<String> = decisions
        .iter()
        .filter(|(p, d)| {
            *d == ApplyDecision::Install
                && !crate::gate::current_build_ok(ch, p, installed.get(p).copied())
        })
        .map(|(p, _)| p.clone())
        .collect();
    for program in &disabled {
        install_tombstone_shims(layout, program, installed.get(program).copied());
    }
    disabled
}

/// The line the missing-triple hold prints for each `program` it disables: `member`'s
/// pinned `build` (the tuple's new pin) is not published for `triple`, and the program's
/// commands stay disabled until it is. `member` differs from `program` when a sibling's
/// new pin is the missing one.
pub(crate) fn recalled_unpublished_notice(
    program: &str,
    member: &str,
    build: u64,
    triple: &str,
) -> String {
    if program == member {
        format!(
            "atpkg: {program} was recalled and its replacement ({program} build {build}) is \
             not published for {triple} — its commands are disabled until it is."
        )
    } else {
        format!(
            "atpkg: {program} was recalled and its coherence group's new pin ({member} build \
             {build}) is not published for {triple} — its commands are disabled until it is."
        )
    }
}

/// Reclaim `dl`'s own sibling `<asset>.part`, if one exists, so an asset that failed its
/// signed sha256 leaves no poisoned prefix to seed the next attempt. A cheap invariant: the
/// resumable fetcher has usually renamed the `.part` onto the asset already.
fn discard_sibling_partial(dl: &Path) {
    let Some(name) = dl.file_name() else {
        return;
    };
    let mut part = name.to_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(dl.with_file_name(part));
}

/// Reclaim every `.part` and every other asset's archive in `dl`'s directory except `dl`'s
/// own, so at most one stranded partial and one carried archive survive per program
/// between passes (gc's `staging/` sweep runs only when a person runs it). Regular files
/// in `dl`'s own directory only, never recursive.
fn sweep_foreign_partials(dl: &Path) {
    let (Some(dir), Some(name)) = (dl.parent(), dl.file_name()) else {
        return;
    };
    let mut keep = name.to_os_string();
    keep.push(".part");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_name = entry.file_name();
        if entry_name == keep || entry_name == name {
            continue;
        }
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let is_part = std::path::Path::new(&entry_name)
            .extension()
            .is_some_and(|ext| ext == "part");
        // A `.part` of another asset, or a full archive of another asset name (failed
        // stages keep theirs, [`carried_archive`]; a vendor-direct download is
        // `<program>-<version>-<triple>[.ext]`).
        let is_archive = entry_name.to_str().is_some_and(|n| {
            n.ends_with(".tar.zst")
                || n.ends_with(".dmg")
                || crate::vendor_direct::VENDORS.iter().any(|spec| {
                    n.strip_prefix(spec.program)
                        .is_some_and(|rest| rest.starts_with('-'))
                })
        });
        if is_part || is_archive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What a failed stage leaves in `staging/`: nothing for a digest failure (the archive and
/// any `.part` go, [`discard_sibling_partial`]) or a signer refusal (the memo keeps them
/// from being fetched again); the verified archive for every other failure, so the next
/// attempt stages it without a download ([`carried_archive`]). [`sweep_foreign_partials`]
/// keeps it to one per program.
fn reclaim_after_failed_stage(dl: &Path, e: &StageError) {
    if matches!(
        e,
        StageError::DigestMismatch { .. } | StageError::SignerRefused(_)
    ) {
        let _ = std::fs::remove_file(dl);
        discard_sibling_partial(dl);
    }
}

/// Record a stage failure beside the build it was staging, so a pass that keeps meeting
/// the same one stops paying to be told so ([`crate::store::StageRefusal`]):
///
/// * a signed-digest mismatch cools down: its retry costs a transfer from byte 0 (the
///   archive and partial are deleted), but one verdict can be a truncated transfer or a
///   proxy, so the first retry is free and only identical verdicts lengthen the cooldown;
/// * a signer refusal ([`StageError::SignerRefused`]) binds until the digests move: the
///   bytes matched them, so a re-download can only fetch the same refusal;
/// * a `tree_root` mismatch is held: its archive passed the digest and is kept
///   ([`carried_archive`]), but the re-stage is deterministic over the same bytes. It binds
///   until the pin's digests move, a newer atpkg asks, or a person does
///   (`aterm pkg install <p>`, `aterm pkg update --retry`).
///
/// Every other failure is a fact about this machine (a full disk, an unreadable store),
/// keeps its verified archive, and retries freely.
fn record_digest_refusal(build_dir: &Path, artifact: &crate::manifest::Artifact, e: &StageError) {
    let Some(digest) = artifact.digest() else {
        return;
    };
    let record = match e {
        StageError::DigestMismatch { .. } => crate::store::record_stage_refusal,
        StageError::SignerRefused(_) => crate::store::record_signer_refusal,
        StageError::TreeRootMismatch { .. } => crate::store::record_held_refusal,
        _ => return,
    };
    // Best-effort: the pass still reports the stage failure itself.
    let _ = record(
        build_dir,
        digest,
        &artifact.tree_root,
        &e.to_string(),
        now_unix(),
    );
}

/// The row a still-binding refusal ([`record_digest_refusal`]) owes this pass: the
/// canonical `held: last attempt failed with <why>; <how to retry>` state
/// ([`crate::state::held_failed`]), or `None` when nothing is recorded, the pin's signed
/// digests moved, another atpkg wrote the memo, or a cooldown lapsed.
///
/// The wall clock, not the freshness gates' `now_unix` parameter: this is a bandwidth
/// cooldown or a hold, never a trust decision. [`now_unix`] fails closed to `i64::MAX`,
/// so an unreadable clock lapses every cooldown and can only cost a download (signer and
/// held memos do not age).
///
/// The program lane ([`land_artifact`]) names `aterm pkg install <p>`; the group lane
/// (`grouped`, [`fetch_member`]) names `aterm pkg update --retry`, which forgets every memo
/// and moves the tuple whole (`install <p>` would land one member beside siblings on the
/// old pin).
fn digest_refusal_note(
    layout: &Layout,
    program: &str,
    build: u64,
    artifact: &crate::manifest::Artifact,
    grouped: bool,
) -> Option<String> {
    let now = now_unix();
    let memo = crate::store::stage_refusal(&layout.build_dir(program, build))?;
    if !memo.binds(artifact.digest()?, &artifact.tree_root, now) {
        return None;
    }
    // Built by pushes, not one long `format!` whose line continuations would hide stray
    // spaces.
    let mut how = String::new();
    if memo.signer || memo.held {
        how.push_str("not downloaded again until a new build is published \u{2014} now: ");
    } else {
        let hours = memo
            .retry_after()
            .saturating_sub(now)
            .saturating_add(3599)
            .div_euclid(3600)
            .max(1);
        how.push_str(&memo.attempts.to_string());
        how.push_str(" attempts failed; not downloaded again for about ");
        how.push_str(&u64::try_from(hours).unwrap_or(0).to_string());
        how.push_str("h or until a new build is published \u{2014} now: ");
    }
    if grouped {
        how.push_str("aterm pkg update --retry");
    } else {
        how.push_str("aterm pkg install ");
        how.push_str(program);
    }
    Some(crate::state::held_failed(&memo.why, &how))
}

/// Pure disk gate (§9): `Ok(())` unless `available` is a measured value that fails
/// [`crate::cost::disk_ok`] against `required` plus [`crate::cost::FREE_FLOOR`]. `None`
/// (the query failed) fails open: preflight is a safety net, not a security gate.
fn disk_gate(required: u64, available: Option<u64>) -> Result<(), FlowError> {
    match available {
        Some(avail) if !crate::cost::disk_ok(required, avail, crate::cost::FREE_FLOOR) => {
            // The need shown includes the reserve, so "needs X, Y free" never reads Y > X.
            Err(FlowError::InsufficientDisk {
                required: required.saturating_add(crate::cost::FREE_FLOOR),
                available: avail,
            })
        }
        _ => Ok(()), // None (query failed): fail open
    }
}

/// What one walk over a group's Install members' pinned, release-verified manifests proved
/// — the one walk both the missing-triple hold and the disk preflight read.
enum InstallNeed {
    /// Every Install member's manifest carries an artifact for the triple: each member's pin
    /// as read, in member order, and the bytes they land together — what the disk preflight
    /// gates on.
    Published {
        pins: Vec<PinnedArtifact>,
        bytes: LandingBytes,
    },
    /// `member`'s manifest — bound to its pinned program and `build` — carries no artifact
    /// for the triple: that pin cannot exist on this host.
    Unpublished { member: String, build: u64 },
    /// Nothing proven: a fetch/verify/parse failure, or a manifest with no row for the
    /// triple that does not bind to its pin. Both gates fail open, letting the real stage
    /// surface the failure.
    Unknown,
}

/// One Install member's pin as [`group_install_need`]'s walk read it.
struct PinnedArtifact {
    program: String,
    /// The pinned build its manifest was fetched for.
    build: u64,
    /// Its artifact for the triple.
    artifact: crate::manifest::Artifact,
}

/// The signed bytes one install unit needs on disk while it lands: an ungrouped program's
/// artifact, or a tuple's Install members together (their archives all stay until the
/// tuple's fate is known, [`stage_fetched`]). What [`land_artifact`], [`group_install_need`]
/// and the default set's up-front refusal (`cli::set_disk_refusal`) check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LandingBytes {
    /// The compressed archives (`size`): on disk from the download until the stage ends.
    pub asset: u64,
    /// The extracted trees (`[cost].disk_installed`): what stays.
    pub tree: u64,
}

impl LandingBytes {
    /// One artifact's bytes.
    pub(crate) fn of(artifact: &crate::manifest::Artifact) -> Self {
        Self {
            asset: artifact.size,
            tree: artifact.cost.disk_installed,
        }
    }

    /// `self` and `other` landing together, as a tuple's members do.
    #[must_use]
    pub(crate) fn and(self, other: Self) -> Self {
        Self {
            asset: self.asset.saturating_add(other.asset),
            tree: self.tree.saturating_add(other.tree),
        }
    }

    /// What a disk gate requires free for this unit, before the reserve: its archives and
    /// its trees at once.
    pub(crate) fn need(self) -> u64 {
        self.asset.saturating_add(self.tree)
    }
}

/// What a pass that lands `units` one after another needs free, before the reserve, in any
/// order: every tree stays and a unit's archives are reclaimed once it has staged, so the
/// peak is every tree plus the largest unit's archives.
pub(crate) fn peak_need(units: impl IntoIterator<Item = LandingBytes>) -> u64 {
    peak_need_at_once(units, 1)
}

/// [`peak_need`] for units landing `lanes` at a time: every tree, and the archives of the
/// `lanes` largest units, which can all be on disk together.
pub(crate) fn peak_need_at_once(
    units: impl IntoIterator<Item = LandingBytes>,
    lanes: usize,
) -> u64 {
    let mut trees = 0u64;
    let mut assets = Vec::new();
    for u in units {
        trees = trees.saturating_add(u.tree);
        assets.push(u.asset);
    }
    assets.sort_unstable_by(|a, b| b.cmp(a));
    assets
        .into_iter()
        .take(lanes)
        .fold(trees, u64::saturating_add)
}

/// What the default set's up-front disk check reads of one install unit
/// ([`planned_landing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlannedLanding {
    /// The pass lands nothing of the unit, so it must not decide whether the rest fits: a
    /// revoked pin (tombstoned), a signed row naming the retired `requires`, a local pin
    /// holding the tuple ([`TxnOutcome::Pinned`]), nothing to install, a pin proven
    /// unpublished for the triple, or a binding digest-refusal memo
    /// ([`digest_refusal_note`]). A tuple that aborts at such a memo may already have
    /// staged members ahead of it; their bytes are left out here because the tuple's own
    /// disk gate, which counts every Install member, runs before any is fetched.
    Nothing,
    /// The bytes its landing needs: what its own disk gate checks.
    Bytes(LandingBytes),
    /// No answer: a manifest or its row for the triple is unavailable, or a row declares no
    /// installed size.
    Unknown,
}

/// One install unit of the default set as its up-front disk check reads it: whether its
/// lane lands anything this pass and, if so, the bytes its own disk gate checks. An
/// ungrouped program is read as [`install`] reads an absent program; a coherence tuple
/// through the decisions and manifest walk [`apply_group_txn`] runs on the same
/// `installed` view ([`GroupDecisions`], [`group_install_need`]), so it counts exactly the
/// members the transaction would stage. The fetcher memoises the manifests.
pub(crate) fn planned_landing(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    triple: &str,
) -> PlannedLanding {
    let need = if group.group.is_none() {
        let [program] = group.members.as_slice() else {
            return PlannedLanding::Unknown;
        };
        // [`install`]'s refusals ahead of any manifest: a retired `requires`, and every
        // decision but Install (a revoked pin is tombstoned).
        if index
            .program(program)
            .is_some_and(crate::manifest::Program::names_requires)
            || decide(ch, program, None) != ApplyDecision::Install
        {
            return PlannedLanding::Nothing;
        }
        group_install_need(fetcher, index, ch, &[program], triple)
    } else {
        let decided = GroupDecisions::of(layout, ch, group, installed);
        let members = decided.install_members();
        if decided.any_tombstone
            || members.is_empty()
            || decided.retired_requires(index).is_some()
            || decided.pin_hold(layout).is_some()
        {
            return PlannedLanding::Nothing;
        }
        group_install_need(fetcher, index, ch, &members, triple)
    };
    let (pins, bytes) = match need {
        InstallNeed::Published { pins, bytes } => (pins, bytes),
        // The program's `NoArtifact` skip; for a tuple, the missing-triple hold or the
        // bootstrap's clean skip.
        InstallNeed::Unpublished { .. } => return PlannedLanding::Nothing,
        InstallNeed::Unknown => return PlannedLanding::Unknown,
    };
    let grouped = group.group.is_some();
    if pins
        .iter()
        .any(|p| digest_refusal_note(layout, &p.program, p.build, &p.artifact, grouped).is_some())
    {
        PlannedLanding::Nothing
    } else if pins.iter().any(|p| p.artifact.cost.disk_installed == 0) {
        PlannedLanding::Unknown
    } else {
        PlannedLanding::Bytes(bytes)
    }
}

/// Each of `install_members`' pins and the bytes they land together ([`LandingBytes`]),
/// walked in order through [`verified_pkg`]. Stops at the first member whose manifest
/// fails ([`InstallNeed::Unknown`]) or has no artifact for `triple`: that is
/// [`InstallNeed::Unpublished`] only when the manifest binds to the member's pinned program
/// and build, else [`InstallNeed::Unknown`].
fn group_install_need(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    install_members: &[&String],
    triple: &str,
) -> InstallNeed {
    let mut pins = Vec::with_capacity(install_members.len());
    let mut bytes = LandingBytes::default();
    for &m in install_members {
        let Some((pinned, _, pkg)) = verified_pkg(fetcher, index, ch, m) else {
            return InstallNeed::Unknown;
        };
        let Some(a) = pkg.artifact_for(triple) else {
            return if pkg.is_for(m) && pkg.build_number == pinned {
                InstallNeed::Unpublished {
                    member: m.clone(),
                    build: pinned,
                }
            } else {
                InstallNeed::Unknown
            };
        };
        bytes = bytes.and(LandingBytes::of(a));
        pins.push(PinnedArtifact {
            program: m.clone(),
            build: pinned,
            artifact: a.clone(),
        });
    }
    InstallNeed::Published { pins, bytes }
}

/// Resolve and verify-select the signed index (cached fallback, §14) and enforce its
/// freshness window (§8): the shared prologue of every flow entry point, exposed so the
/// CLI's default-set bootstrap (§11) can read the verified program set itself. Installs
/// nothing.
pub fn resolve_verified_index(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let cache = crate::cache::IndexCache::for_layout(layout);
    let src = fetcher.source_id();
    let (candidates, served) = resolve_candidates(fetcher, &cache, &src)?;
    let live = last_resolve().is_some_and(|r| r.reached);
    let index = if served {
        // The roster ratchet and the yank latch write into the prefix.
        let _ = layout.ensure_dir(&layout.prefix);
        let selected =
            verify_select_fresh_from(layout, anchor, candidates.clone(), floor, now_unix, live);
        // A served set that selects nothing (a run below the floor, bytes no chain admits)
        // is a fetch that found nothing, so the cache still serves: it is weighed beside
        // what was served, so a newer generation the source carries still wins
        // (`select::select_index`), the resolve says it ran on the cache, and the cache
        // becomes the served candidates plus the cached one that won.
        if matches!(selected, Err(FlowError::NoIndex))
            && let Some(cached) = cache.load(&src)
        {
            note_resolve(
                false,
                Some(format!("the source served {}", FlowError::NoIndex)),
                true,
            );
            let served_n = candidates.len();
            let both = candidates.iter().chain(&cached).cloned().collect();
            let (verdict, won) = verify_select_at(layout, anchor, both, floor, now_unix, false);
            if let Some(won) = won {
                let mut kept = candidates;
                kept.extend(
                    won.checked_sub(served_n)
                        .and_then(|i| cached.get(i))
                        .cloned(),
                );
                cache.store(&src, &kept);
                return verdict;
            }
            note_resolve(true, None, true);
        }
        // Otherwise the source's verdict stands, and what it served becomes the cache
        // (what `doctor` reads back as last downloaded, accepted or not).
        cache.store(&src, &candidates);
        selected?
    } else {
        verify_select_fresh_from(layout, anchor, candidates, floor, now_unix, live)?
    };
    if live {
        LIVE_VERIFICATIONS.with(|n| n.set(n.get().saturating_add(1)));
    }
    Ok(index)
}

/// The verify-select + freshness half of [`resolve_verified_index`], over candidates fetched
/// live for the typed single-program verbs ([`rollback`], [`apply_program`],
/// [`plan_update`]) with no §14 cached fallback: an unreachable source is
/// [`FlowError::Unreachable`] with the transport's reason, and a reached source that lists
/// nothing is [`FlowError::NoIndex`]. It neither refreshes the cache nor records
/// [`last_resolve`]. A selected index whose `valid_until` has lapsed or cannot be parsed is
/// refused (§8 gate 2).
///
/// The roster ratchet turns here, on observation: the durable `roster_seq` high-water
/// advances to the newest generation admitted, before the freshness gate and before any
/// caller decides whether to install, so a client that saw generation *n* refuses *n-1*
/// even when it installs nothing. That cannot lock it out: the observed generation was
/// master-signed, fresh and at or above the floor, and the ratchet refuses only strictly
/// older ones. Best-effort; a failed write leaves the older floor.
fn verify_select_fresh(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let candidates = fetcher.index_candidates().map_err(FlowError::Unreachable)?;
    verify_select_fresh_from(layout, anchor, candidates, floor, now_unix, true)
}

/// [`verify_select_fresh`] over caller-supplied `candidates` that came `live` off the
/// source, or from the §14 cache when it was not reached (`live == false`).
fn verify_select_fresh_from(
    layout: &Layout,
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
    live: bool,
) -> Result<TrustedIndex, FlowError> {
    verify_select_at(layout, anchor, candidates, floor, now_unix, live).0
}

/// [`verify_select_fresh_from`], with the position among `candidates` of the one selected:
/// `None` only when nothing was (`NoIndex`), so a selected index whose window has lapsed
/// (`Stale`) still names its candidate.
fn verify_select_at(
    layout: &Layout,
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
    live: bool,
) -> (Result<TrustedIndex, FlowError>, Option<usize>) {
    let pass = select_index(anchor, candidates, floor, now_unix);
    observe_roster_generation(layout, pass.observed_roster_seq);
    let Some(selected) = pass.selected else {
        return (Err(FlowError::NoIndex), None);
    };
    let (index, won) = (selected.index, selected.candidate);
    if !index_is_fresh(&index, now_unix) {
        return (Err(FlowError::Stale), Some(won));
    }
    // Every fresh verification carries its vendor yanks forward (design §1.4); only bytes
    // the source served live stamp the latch current, so a vendor door knows whether to
    // verify again. Best-effort: an unwritten latch keeps the older yanks.
    if layout.prefix.is_dir() {
        use crate::vendor_direct::policy::YankLatch;
        let written = if live {
            YankLatch::update_from_index(layout, &index, now_unix)
        } else {
            YankLatch::carry_from_cached_index(layout, &index)
        };
        if let Err(e) = written {
            eprintln!("atpkg: the vendor yank latch was not written ({e})");
        }
    }
    (Ok(index), Some(won))
}

/// Durably record that this client admitted roster generation `seq`, the replay ratchet's
/// write half (`<prefix>/roster.floor`). `0` means nothing was admitted and is not written.
pub(crate) fn observe_roster_generation(layout: &Layout, seq: u64) {
    if seq == 0 {
        return;
    }
    // The discarded Result is the accept/refuse decision (a Rollback means a concurrent
    // pass recorded something newer); a failure to persist is reported by
    // `check_and_record` itself.
    let _ = crate::sig::Floor::new(layout.roster_floor()).check_and_record(seq);
}

/// The §8 gate-2 freshness predicate: whether the index's signed `valid_until` window is
/// still open at `now_unix`. An unparsable `valid_until` is lapsed (fail closed).
pub(crate) fn index_is_fresh(index: &Index, now_unix: i64) -> bool {
    matches!(
        rfc3339_to_unix(&index.valid_until),
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok()
    )
}

/// Resolve the index candidates with a same-source cached fallback (§14): a non-empty fetch
/// is returned `served` (`true`) and [`resolve_verified_index`] refreshes the cache with
/// it; a fetch failure or an empty success falls back to the cached candidates for the
/// same [`Fetcher::source_id`] (a `dir:` cache never answers a `github:` fetch). Cached
/// bytes are raw and go through the same verify, freshness and floor gates.
fn resolve_candidates(
    fetcher: &dyn Fetcher,
    cache: &crate::cache::IndexCache,
    src: &str,
) -> Result<(Vec<Candidate>, bool), FlowError> {
    match fetcher.index_candidates() {
        Ok(c) if !c.is_empty() => {
            note_resolve(true, None, true);
            Ok((c, true))
        }
        // Reached, and the source carried no index. When the cache serves instead, the
        // resolve counts as not reached: its bytes neither count as a live verification
        // nor stamp the yank latch current.
        Ok(_) => match cache.load(src) {
            Some(cached) => {
                note_resolve(false, Some("the source listed no index".into()), true);
                Ok((cached, false))
            }
            None => {
                note_resolve(true, None, true);
                Err(FlowError::NoIndex)
            }
        },
        // Not reached. Keep the reason (an offline user must not be told their signatures
        // failed) and whether a host answered at all (a refusal is not an offline machine).
        Err(why) => {
            note_resolve(false, Some(why.clone()), !fetcher.index_link_down());
            cache
                .load(src)
                .map(|c| (c, false))
                .ok_or(FlowError::Unreachable(why))
        }
    }
}

/// What the last index resolve learned about the network: whether the source was reached,
/// and if not why, so a pass that ran on the §14 cache says so. The pass end reads it
/// ([`last_resolve`]), and a pass that did not reach the source stamps a failure
/// (`cli::record_pass_end`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveProvenance {
    /// The source served the candidates over the network this resolve (not the §14 cache).
    pub reached: bool,
    /// Why it did not, when it did not — the transport's own sentence.
    pub why: Option<String>,
    /// Whether a host answered at all (the source, or a refusal with a status). `false` only
    /// when the link itself failed ([`Fetcher::index_link_down`]), as on an offline pass.
    pub answered: bool,
}

// Per thread: a pass is one thread, so two passes in one process (the tests') never read
// each other's network.
thread_local! {
    static LAST_RESOLVE: std::cell::RefCell<Option<ResolveProvenance>> =
        const { std::cell::RefCell::new(None) };
}

fn note_resolve(reached: bool, why: Option<String>, answered: bool) {
    LAST_RESOLVE.with(|slot| {
        *slot.borrow_mut() = Some(ResolveProvenance {
            reached,
            why,
            answered: reached || answered,
        });
    });
}

// Per thread: how many indexes this thread's resolves verified from what the channel
// served (not the §14 cache).
thread_local! {
    static LIVE_VERIFICATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// This thread's count of [`resolve_verified_index`] successes on candidates the channel
/// served. Read before and after, it tells a pass whether it reached the signed index, the
/// one fact its success stamp may record (`cli::finish_update_pass`, model
/// `AtpkgPassStamps`).
#[must_use]
pub(crate) fn live_verifications() -> u32 {
    LIVE_VERIFICATIONS.with(std::cell::Cell::get)
}

// Per thread, like the resolve's provenance: the members this pass lost at a fetch.
thread_local! {
    static FETCH_FAULTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Count one member that failed at a fetch (its manifest or artifact did not arrive). A pass
/// that reached nothing is offline only when every failure it counts is one of these
/// (`cli::run_update_pass` reads [`fetch_faults`] before and after).
pub(crate) fn note_fetch_fault() {
    FETCH_FAULTS.with(|n| n.set(n.get().saturating_add(1)));
}

/// This thread's [`note_fetch_fault`] count so far.
#[must_use]
pub(crate) fn fetch_faults() -> u32 {
    FETCH_FAULTS.with(std::cell::Cell::get)
}

/// Count, on the pass's thread, the `n` fetch faults a lane thread of the same pass counted
/// on its own ([`note_fetch_fault`]): a pass's offline verdict reads one tally.
pub(crate) fn note_fetch_faults(n: u32) {
    FETCH_FAULTS.with(|count| count.set(count.get().saturating_add(n)));
}

/// The provenance of this thread's last [`resolve_verified_index`], `None` before the
/// first.
#[must_use]
pub fn last_resolve() -> Option<ResolveProvenance> {
    LAST_RESOLVE.with(|slot| slot.borrow().clone())
}

/// Roll `program` back to the highest retained build strictly below its active build that
/// still passes the floor/yank gate (§9/§11), re-pointing its shims and `current` via
/// [`rollback_member`] (links only; no retained tree is touched). The target predicate
/// ([`rollback_lands_on`]) is the one [`decide`] tombstones on, so a rollback never lands
/// below the floor or on a revoked build; it errors instead. `excluded` is the caller's
/// `[packages].exclude`, read only to report which pins the next update honours.
#[allow(
    clippy::too_many_arguments,
    reason = "rollback needs the fetcher, layout, pinned root key, channel + program \
              selectors, the exclusions the update pass reads, and the floor + clock the \
              freshness/floor gates read — the same irreducible set the install/apply entry \
              points take"
)]
pub fn rollback(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    program: &str,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<RollbackReport, FlowError> {
    // 1. The active build (shim-derived), never a merely-staged one.
    let current = *crate::ops::active_builds(layout)
        .get(program)
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "{program} is not installed \u{2014} nothing to roll back (aterm pkg list shows what is)"
            ))
        })?;
    // 2. Verify-select the signed index so the floor/yank gate is authoritative (live: no
    //    §14 cached fallback).
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    // 3. Reachability — capture the coherence group for the report/warn.
    let coherence_group = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?
        .coherence_group
        .clone();
    // 4. The channel supplies the authoritative min_build + yank list.
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    // 5. Retained builds strictly below current, highest first.
    let mut lower: Vec<u64> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, _)| p == program)
        .map(|(_, b)| b)
        .filter(|&b| b < current)
        .collect();
    lower.sort_unstable();
    lower.dedup();
    // 6. The highest build below current a rollback may land on ([`rollback_lands_on`]).
    let target = lower
        .iter()
        .rev()
        .copied()
        .find(|&b| rollback_lands_on(layout, Some(ch), program, b))
        .ok_or_else(|| {
            // Say what stays and why nothing older can run, about the newest retained
            // build below current (`lower` is ascending).
            FlowError::Rollback(match lower.last() {
                None => {
                    format!("no older {program} build is kept \u{2014} {program} {current} stays")
                }
                Some(&b) if !crate::gate::current_build_ok(ch, program, Some(b)) => {
                    format!("{program} {b} is revoked \u{2014} {program} {current} stays")
                }
                Some(&b) => format!(
                    "{program} {b} has nothing in bin/ to run \u{2014} {program} {current} stays"
                ),
            })
        })?;
    // 7. Re-point via the tested primitive (symlinks only; no tree mutation). reloc:None —
    //    a self-contained bundle needs no pre-activation wiring to re-run.
    let staged = Staged {
        build: current,
        build_dir: layout.build_dir(program, current),
        exposes: crate::ops::active_tools(layout, program, current),
        prior_build: Some(target),
        // The live build being rolled off; only the group abort discard reads it.
        was_live: true,
        reloc: None,
        tree_root: String::new(),
        // The signed index just verified is the alias policy, as at install.
        aliases: Aliases::for_program(program, index.program(program)),
    };
    rollback_member(layout, program, &staged);
    // 8. What the next update does with it, read as its pin gate reads the planned group
    //    ([`pin_gate_members`]): a member's revoked current build moves the whole group
    //    whatever is pinned, and otherwise the members pinned here hold it.
    let active = crate::ops::active_builds(layout);
    let members = pin_gate_members(layout, &index, ch, program, &active, excluded);
    let revoked_member = members.iter().find_map(|m| {
        let build = active.get(m).copied()?;
        (!crate::gate::current_build_ok(ch, m, Some(build))).then(|| (m.clone(), build))
    });
    let pins = crate::pin::pinned_set(layout);
    let held_by = if revoked_member.is_some() {
        Vec::new()
    } else {
        members.into_iter().filter(|m| pins.contains(m)).collect()
    };
    Ok(RollbackReport {
        program: program.to_string(),
        from_build: current,
        to_build: target,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        coherence_group,
        held_by,
        revoked_member,
    })
}

/// The transactional `update <grouped-member>` path (§11): verify-select the index like
/// [`apply_channel`] but with no §14 cached fallback (a fetch failure is `Unreachable`),
/// find the coherence group containing `program`, and apply that whole group atomically
/// via [`apply_group`], so a grouped member never moves alone. A program the channel does
/// not pin is [`FlowError::NotPinned`].
#[allow(
    clippy::too_many_arguments,
    reason = "the transactional single-program update needs the same irreducible inputs as \
              apply_channel plus the program to target: fetcher, layout, root key, channel + \
              triple + program selectors, the installed map, and the floor + clock"
)]
pub fn apply_program(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    program: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    refuse_vendor_program(program, triple)?;
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    // The channel as this target sees it (`pin_by_target` over `pin`), for every read below.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    refuse_unrouted(layout, &index, &ch, program, triple)?;
    let groups = crate::apply::plan_groups_here(layout, &index, &ch, triple);
    let group = groups
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;
    Ok(apply_groups(
        fetcher,
        layout,
        &index,
        &ch,
        triple,
        std::slice::from_ref(&group),
        installed,
        excluded,
        // `aterm pkg update <program>` is a person's verb: it flips at once.
        &crate::quiet::FlipGate::NOW,
    ))
}

/// Read-only routing decision for the `update` verb (§11): verify-select the index, then
/// return `program`'s coherence group (transactional vs single path) and the authoritative
/// [`decide`] result, so an ungrouped pin gate applies strictly after it and never hides a
/// Tombstone. Read-only as to the store; it takes `layout` because the roster ratchet
/// turns on observation ([`verify_select_fresh`]) even when nothing installs.
#[allow(
    clippy::too_many_arguments,
    reason = "the routing decision needs the fetcher, the layout its roster ratchet turns \
              in, the anchor, the channel + triple + program selectors, the installed build, and \
              the floor + clock the anti-rollback/freshness gates read"
)]
pub fn plan_update(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    program: &str,
    installed_build: Option<u64>,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<UpdatePlan, FlowError> {
    refuse_vendor_program(program, triple)?;
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    // Decide against the pin this target installs (`pin_by_target` overlay), or a build
    // `install` placed from the overlay reads as out of date.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    refuse_unrouted(layout, &index, &ch, program, triple)?;
    let p = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    Ok(UpdatePlan {
        group: p.coherence_group.clone(),
        decision: decide(&ch, program, installed_build),
        // A local pin may hold the ungrouped update only while the installed build is
        // gate-valid (never keep a yanked or below-floor build running).
        current_build_ok: crate::gate::current_build_ok(&ch, program, installed_build),
        channel: ch,
    })
}

/// A vendor-direct program's decisions are its vendor lane's; the index's rules never
/// reach it. An ALab-built one ([`crate::alab::agent`]) on a target ALab builds it for is
/// asked again once the index is in hand ([`refuse_unrouted`]): the index lane may own it
/// there. Everywhere else the refusal comes before any index is fetched, as it always did.
fn refuse_vendor_program(program: &str, triple: &str) -> Result<(), FlowError> {
    match crate::vendor_direct::spec(program) {
        Some(spec) if !crate::alab::agent(program).is_some_and(|a| a.serves(triple)) => {
            Err(vendor_refusal(program, spec))
        }
        _ => Ok(()),
    }
}

/// [`refuse_vendor_program`]'s second half, over the verified `index` and its channel `ch`
/// as `triple` sees it: a vendor-table program the source rule does not hand to the index
/// lane here ([`crate::alab::source_in`]) is refused.
fn refuse_unrouted(
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    triple: &str,
) -> Result<(), FlowError> {
    match crate::vendor_direct::spec(program) {
        Some(spec) if !crate::alab::source_in(layout, index, ch, program, triple).index_lane() => {
            Err(vendor_refusal(program, spec))
        }
        _ => Ok(()),
    }
}

/// `<program> is updated from <vendor>, not through the ALab index`.
fn vendor_refusal(program: &str, spec: &crate::vendor_direct::VendorSpec) -> FlowError {
    FlowError::Vendor(format!(
        "{program} is updated from {}, not through the ALab index",
        spec.vendor
    ))
}

/// [`Fetcher::prefetch_manifests`] for the manifests [`verified_pkg`] reads for `members` on
/// `ch`: each member's pinned build, from its index-declared repo. A member with no pin or
/// no program is left to the read, which says so.
pub fn prefetch_pinned<'a>(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    members: impl IntoIterator<Item = &'a str>,
) {
    let mut pins: Vec<(String, String, u64)> = Vec::new();
    for program in members {
        if let (Some(&build), Some(declared)) = (ch.pin.get(program), index.program(program)) {
            pins.push((declared.repo.clone(), program.to_string(), build));
        }
    }
    if !pins.is_empty() {
        fetcher.prefetch_manifests(&pins);
    }
}

/// The read-only plan `plan_update` yields: which path to take (grouped vs ungrouped), the
/// authoritative [`ApplyDecision`], whether the currently-installed build is itself still
/// gate-valid (the guard the ungrouped local-pin hold must pass), and the channel all of it
/// was decided against — whose revocations pick the build the verb's closing gc keeps below
/// the live one ([`rollback_lands_on`]).
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    pub group: Option<String>,
    pub decision: ApplyDecision,
    pub current_build_ok: bool,
    pub channel: Channel,
}

/// The verified per-build manifest for `program`'s channel pin: pin lookup, repo lookup,
/// fetch, [`TrustedIndex::verify_pkg`], [`parse_pkg`], in that order (verify before parse,
/// §4.2). `(pinned build, repo, manifest)`, or `None` on any miss. A caller that binds the
/// signed `program`/`build_number` to its request checks that on the result.
///
/// `pub(crate)` for the CLI's pre-install disclosure (`cli::seed_install_bytes`), so the
/// size shown comes from the same verified bytes as the install.
pub(crate) fn verified_pkg(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Option<(u64, String, crate::manifest::PkgManifest)> {
    verified_pkg_or_miss(fetcher, index, ch, program).ok()
}

/// [`verified_pkg`], saying on a miss whether the manifest was fetched (`Err(true)`: it did
/// not verify or parse, or there is no pin) or not (`Err(false)`: a fetch fault,
/// [`note_fetch_fault`]).
fn verified_pkg_or_miss(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Result<(u64, String, crate::manifest::PkgManifest), bool> {
    let pinned = *ch.pin.get(program).ok_or(true)?;
    let repo = index.program(program).ok_or(true)?.repo.clone();
    let (raw, sig) = fetcher
        .pkg_manifest(&repo, program, pinned)
        .map_err(|_| false)?;
    let verified = index.verify_pkg(raw, &sig).map_err(|_| true)?;
    let pkg = parse_pkg(&verified).map_err(|_| true)?;
    Ok((pinned, repo, pkg))
}

/// `program`'s signed manifest for any `build` (not only the pin), verified under the index
/// and bound to program and build; `None` on any miss.
fn signed_pkg_of_build(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
) -> Option<crate::manifest::PkgManifest> {
    let (raw, sig) = fetcher.pkg_manifest(repo, program, build).ok()?;
    let verified = index.verify_pkg(raw, &sig).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
    (pkg.is_for(program) && pkg.build_number == build).then_some(pkg)
}

/// The signed root `pkg` states for `triple`, when it states a non-empty one.
fn signed_root_for(pkg: &crate::manifest::PkgManifest, triple: &str) -> Option<String> {
    pkg.artifact_for(triple)
        .map(|a| a.tree_root.clone())
        .filter(|root| !root.is_empty())
}

/// The version `program`'s signed manifest states for `build`, and its `tree_root` for
/// `triple` when it states one ([`signed_pkg_of_build`]): the vendor lane names a legacy
/// index build by that version and keeps the root for a rollback to it, from one fetch.
pub(crate) fn legacy_facts_of_build(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
    triple: &str,
) -> Option<(String, Option<String>)> {
    let pkg = signed_pkg_of_build(fetcher, index, repo, program, build)?;
    let root = signed_root_for(&pkg, triple);
    Some((pkg.version, root))
}

/// The signed asset size `program`'s pinned build ships for `triple`, for the live-progress
/// plan's denominator before bytes move; `None` on any miss degrades the plan to an
/// unmetered row.
pub(crate) fn planned_artifact_size(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
) -> Option<u64> {
    let (_, _, pkg) = verified_pkg(fetcher, index, ch, program)?;
    Some(pkg.artifact_for(triple)?.size)
}

/// Recover the signed `tree_root` for an installed build whose attestation row is missing
/// (a pass that died inside the flip window leaves the member live with no row, and
/// `atpkg verify` then fails closed forever). Re-derived from the install's authority
/// (fetch, verify under the index, bind program and build), never computed locally; `None`
/// on any doubt.
pub fn signed_root_for_installed(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    program: &str,
    build: u64,
    triple: &str,
) -> Option<String> {
    let repo = index.program(program)?.repo.clone();
    let pkg = signed_pkg_of_build(fetcher, index, &repo, program, build)?;
    signed_root_for(&pkg, triple)
}

/// The one line a failed group member owes the log: which program, at which build, and why
/// it could not be staged. [`stage_member`] collapses its failures into `None` so
/// [`transact`] can abort the tuple, and the abort report names only the member and the
/// phase, so the cause is printed to stderr just above it. Reporting never changes what
/// the transaction does.
fn stage_failure_note(program: &str, build: u64, why: &str) -> String {
    format!("atpkg: {program} build {build} could not be staged: {why}")
}

/// Stage one group member: fetch, verify and parse its per-build manifest, bind program and
/// build, select and admit the artifact (Shim, VendorApp or SysrootBundle; anything else
/// fails closed), download, and `verify_and_stage` into its build dir. No activation.
/// `None` on any failure so [`transact`] aborts the group; every exit that holds a
/// diagnosis prints it first ([`stage_failure_note`]).
#[allow(
    clippy::too_many_arguments,
    reason = "stage_member is the per-member slice of apply_group_txn's irreducible inputs \
              plus the caller's resolved-asset collector the pass-end gc sparing reads"
)]
fn stage_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
    prior_build: Option<u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    why: &mut String,
) -> Option<Staged> {
    let fetched = fetch_member(
        fetcher,
        layout,
        index,
        ch,
        program,
        triple,
        resolved_assets,
        why,
    )?;
    stage_fetched(layout, index, program, prior_build, why, fetched)
}

/// A group member's signed archive in `staging/<program>/` — downloaded this pass, or
/// carried from an earlier one ([`carried_archive`]) — with what staging it needs: the
/// half of [`stage_member`] that moves bytes over the network, and the whole of what a
/// deferred flip does before it waits ([`crate::quiet`]: stage now, flip when quiet).
struct Fetched {
    pinned: u64,
    pkg: crate::manifest::PkgManifest,
    artifact: crate::manifest::Artifact,
    dl: PathBuf,
    reloc: Option<String>,
    /// Whether the archive was an earlier attempt's, already matched against its signed
    /// digest ([`carried_archive`]) — a fresh download has not been hashed yet.
    carried: bool,
}

/// [`stage_member`]'s first half: resolve the member's signed manifest, admit its row and
/// fetch its archive into `staging/` (or find the one an earlier attempt carried). `None`,
/// `why` filled, on any failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the fetch half of stage_member takes its inputs, minus the prior build only the \
              stage half reads"
)]
fn fetch_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
    resolved_assets: &mut BTreeMap<String, String>,
    why: &mut String,
) -> Option<Fetched> {
    // `why` is the sentence the abort carries: every `None` below names its step, so
    // `aborted: stage` on the record is never the whole story.
    let (pinned, repo, pkg) = match verified_pkg_or_miss(fetcher, index, ch, program) {
        Ok(found) => found,
        Err(fetched) => {
            if !fetched {
                note_fetch_fault();
            }
            *why = format!("the signed manifest for {program} could not be fetched or verified");
            return None;
        }
    };
    if !pkg.is_for(program) || pkg.build_number != pinned {
        *why = format!(
            "the manifest fetched for {program} is not the build the index pins ({pinned})"
        );
        return None;
    }
    let Some(artifact) = pkg.artifact_for(triple) else {
        *why = format!("{program} build {pinned} publishes no artifact for {triple}");
        return None;
    };
    // Recorded before the download, so a member that fails mid-transfer still reaches the
    // report: `gc::run_keeping_pinned_partials` spares exactly these programs' `.part` files.
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    // Same admission as `install`: a row that fails `check_row` aborts the group before
    // any byte moves.
    if let Err(e) = crate::vendor::check_row(artifact, &pkg.exposes) {
        *why = format!("the signed row for {program} was refused: {e}");
        return None;
    }
    // Shim, vendor-app and sysroot-bundle members stage on this path; the aterm app-bundle
    // and unknown pairs fail closed (the group aborts), as in `install`.
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    let reloc = match strategy {
        crate::dispatch::ApplyStrategy::Shim | crate::dispatch::ApplyStrategy::VendorApp => None,
        crate::dispatch::ApplyStrategy::SysrootBundle => Some(artifact.reloc.clone()),
        crate::dispatch::ApplyStrategy::AppBundle | crate::dispatch::ApplyStrategy::Unknown => {
            // A publishing fact, not a machine fault, and it aborts a whole tuple: name it.
            let note = format!(
                "this aterm cannot install this kind of package ({}/{})",
                artifact.kind, artifact.protocol
            );
            eprintln!("{}", stage_failure_note(program, pinned, &note));
            *why = note;
            return None;
        }
    };
    // The singleton lane's digest-refusal memo: a member pinned to digests the published
    // asset does not match aborts here without re-downloading it every pass.
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact, true) {
        eprintln!("{}", stage_failure_note(program, pinned, &note));
        *why = note;
        return None;
    }
    let dl = match staged_download_path(layout, program, &artifact.asset) {
        Ok(dl) => dl,
        Err(e) => {
            *why = format!("no staging path for {program}: {e}");
            return None;
        }
    };
    if let Err(e) = make_staging_dir(&dl) {
        let parent = dl.parent().unwrap_or(&dl);
        *why = format!("cannot create {}: {e}", parent.display());
        return None;
    }
    // Live-progress hooks, mirroring the singleton path (no-ops without a live pass).
    crate::progress::note_build(program, pinned);
    // The carried archive ([`carried_archive`]): an earlier pass can have aborted on another
    // member after this one staged, discarding its tree but not its signed archive, and a
    // failed stage whose fault was not the bytes leaves it too
    // ([`reclaim_after_failed_stage`]). Reusing it bounds a retry to the members that
    // failed.
    //
    // THE ARCHIVE FETCHED AHEAD ([`ahead`]): the set completion's download-ahead lane may
    // already hold this member's download, or be moving it. Its answer stands in for the
    // fetch below — the same lane, file and cap — and the lane made this path's own checks
    // first (it never fetches over a file standing here), so none is repeated over it.
    let ahead = ahead::claim(program, &dl);
    let carried = ahead.is_none() && carried_archive(&dl, artifact);
    if ahead.is_none() {
        if !carried {
            // Same reason as the singleton path: a stale staging entry can be a hardlink
            // into a `dir:` registry, and fetching over it corrupts the registry. The
            // carried arm never needs this: it only READS `dl` — nothing is written over a
            // hardlink — and what it reads already matched the signed digest.
            let _ = std::fs::remove_file(&dl);
        }
        // Same strandage bound as the singleton path: at most one partial — and one
        // carried archive — per program survives between passes, and it is the one the
        // next attempt will finish or reuse.
        sweep_foreign_partials(&dl);
    }
    if carried {
        println!(
            "atpkg: {program}: reusing verified archive {} from the last attempt",
            artifact.asset
        );
    } else {
        let inline = ahead.is_none();
        let fetched = ahead.unwrap_or_else(|| {
            // The transfer is over when this returns — the poller stops before the phase
            // moves on.
            let _download_watch = crate::progress::watch_download(program, &dl, artifact.size);
            fetch_artifact(fetcher, program, &repo, artifact, &dl)
        });
        if inline && fetched.is_ok() {
            ahead::fetched_inline(program, &dl);
        }
        if let Err(e) = fetched {
            // Say why: `e` carries the fetcher's words (HTTP status, curl exit, the signed
            // byte cap), which the abort report never learns.
            eprintln!("{}", stage_failure_note(program, pinned, &e));
            // The `.part` is left for the next attempt to resume; `dl` is reclaimed, since a
            // `dir:` registry's copy lane writes there directly.
            let _ = std::fs::remove_file(&dl);
            note_fetch_fault();
            *why = format!("the download of {} failed: {e}", artifact.asset);
            return None;
        }
    }
    let artifact = artifact.clone();
    Some(Fetched {
        pinned,
        pkg,
        artifact,
        dl,
        reloc,
        carried,
    })
}

/// [`stage_member`]'s second half: verify and extract a [`Fetched`] archive into its build
/// dir and write its sidecars. NO activation.
fn stage_fetched(
    layout: &Layout,
    index: &TrustedIndex,
    program: &str,
    prior_build: Option<u64>,
    why: &mut String,
    fetched: Fetched,
) -> Option<Staged> {
    let Fetched {
        pinned,
        pkg,
        artifact,
        dl,
        reloc,
        carried: _,
    } = fetched;
    let artifact = &artifact;
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    // Whether this build is already live, captured before the stage swaps a new tree in and
    // before a flip-phase abort re-points or removes the links; `installed` (the shim view)
    // can be silent for a live build (see the abort discard in `apply_group_txn`).
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    // Every failing exit reclaims the archive (nothing else sweeps `staging/`); a staged
    // member keeps it until `apply_group_txn` knows the tuple's fate. The signed
    // `shim_env` rides beside the build (design S7) so the flip, and a rollback with no
    // manifest in hand, lay this build's shims with it.
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, &pkg.shim_env()) {
        // Most likely an unwritable store; named so the log tells it from a bad signature.
        let note = format!("its environment file could not be written beside the build: {e}");
        eprintln!("{}", stage_failure_note(program, pinned, &note));
        let _ = std::fs::remove_file(&dl);
        *why = note;
        return None;
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let alab_gate = alab_pack_gate(program, &pkg.exposes);
    let staged = verify_and_stage(
        artifact,
        &dl,
        &build_dir,
        &stage_hooks(alab_gate.as_deref()),
    );
    drop(extract_scope);
    if let Err(e) = staged {
        // A digest failure reclaims the archive and any `.part`; any other failure keeps
        // the verified archive for the next attempt ([`reclaim_after_failed_stage`]).
        record_digest_refusal(&build_dir, artifact, &e);
        reclaim_after_failed_stage(&dl, &e);
        // Say why: `StageError`'s text (a digest mismatch with both digests, a
        // tar-slip/size-cap refusal) separates a bad asset from a machine that could not
        // unpack it.
        eprintln!("{}", stage_failure_note(program, pinned, &e.to_string()));
        *why = format!("staging {} failed: {e}", artifact.asset);
        return None;
    }
    // This member's bytes are good, whatever an earlier pass recorded about them.
    crate::store::clear_stage_refusal(&build_dir);
    // Refused (sensitive/malformed) names are dropped once here, not per flip; they are not
    // a stage failure, matching `install`, which records them the same way. The entry is
    // this build's, kept beside the live build's, so the doctor says the live build's until
    // a flip makes this one current, and a held or aborted flip changes nothing it says.
    let (exposes, refused) = crate::store::split_exposed(&pkg.exposes);
    let _ = crate::status::record_refused_shims(layout, program, pinned, &refused);
    // The archive stays until the transaction knows the tuple's fate (`carried_archive`).
    Some(Staged {
        build: pinned,
        build_dir,
        exposes,
        prior_build,
        was_live,
        reloc,
        tree_root: artifact.tree_root.clone(),
        aliases: Aliases::for_program(program, index.program(program)),
    })
}

/// Flip a staged member live: point `store/<program>/current` at the new build and
/// (re)install its shims. A partial flip is undone via [`rollback_member`], so `false`
/// leaves no live pointer into the new build. A shim refused for a sensitive name is
/// dropped, not a failure.
///
/// A bundle's resolve check is `ahead`'s verdict when it was started at the stage
/// ([`ResolveAhead`]), else run here.
fn flip_member(layout: &Layout, program: &str, s: &Staged, ahead: Option<ResolveAhead>) -> bool {
    flip_member_with(layout, program, s, ahead, activate_build)
}

/// [`flip_member`] with the activation write injected, so tests can stage the non-atomic
/// Windows-junction failure the repair below handles.
fn flip_member_with(
    layout: &Layout,
    program: &str,
    s: &Staged,
    ahead: Option<ResolveAhead>,
    activate: impl FnOnce(&Layout, &Path) -> std::io::Result<()>,
) -> bool {
    // A sysroot-bundle member gets its pre-activation wiring before the flip; a
    // failure here aborts the group (the payload is staged but never activated).
    if let Some(reloc) = &s.reloc
        && apply_sysroot_bundle(reloc).is_err()
    {
        return false;
    }
    if activate(layout, &s.build_dir).is_err() {
        // A Windows junction is removed before `mklink` recreates it, so a failure can
        // leave the link absent. Point it back at the prior build (fresh install: remove
        // it) so it agrees with the untouched shims; after an atomic failure both repairs
        // are no-ops.
        match s.prior_build {
            Some(prior) => {
                let _ = crate::activate::atomic_symlink(
                    &layout.build_dir(program, prior),
                    &layout.program_current(program),
                );
            }
            None => crate::platform::remove_link(&layout.program_current(program)),
        }
        return false;
    }
    // The build's own `shim_env` sidecar rides on every shim the flip lays (design S7).
    let env = crate::shim_env::read_sidecar(&s.build_dir);
    if install_tools_env(layout, &s.build_dir, &s.exposes, s.aliases, &env).is_err() {
        rollback_member(layout, program, s);
        return false;
    }
    // Fail-loud resolve check for a bundle: a broken toolchain rolls the flip back.
    if s.reloc.is_some()
        && ahead
            .map_or_else(
                || bundle_resolve_check(&s.build_dir, &s.exposes),
                ResolveAhead::verdict,
            )
            .is_err()
    {
        rollback_member(layout, program, s);
        return false;
    }
    true
}

/// A staged bundle's [`bundle_resolve_check`], started on a thread of its own the moment the
/// member staged, so the first exec of its compilers runs beside the stages that follow and
/// the flip takes a verdict instead of paying for it. That first exec is the flip's whole
/// cost and is not disk: the trust toolchain's `trustc` loads its 380 MB driver library in
/// 2.4 s from a tree just extracted (its pages already cached), 3.7 s from a cold clone,
/// and 22 ms the second time (measured 2026-10-17, n=4 each); reading the library ahead buys
/// nothing an extraction has not already. The verdict is the one the flip would reach: the
/// same `--version` of the same verified tree, whose libraries resolve inside the bundle
/// whatever `current` and the shims name, and only for the `self-contained` policy, whose
/// pre-activation wiring is none. No `--version` of the trust toolchain's tools runs
/// another program, so a pending-program stub on PATH is never reached. Dropped untaken, it
/// is waited out, so no check outlives its transaction.
struct ResolveAhead(Option<std::thread::JoinHandle<Result<(), FlowError>>>);

impl ResolveAhead {
    /// Start `s`'s check, or `None`: not a bundle, a policy with wiring the flip would apply
    /// first ([`apply_sysroot_bundle`]), or no thread — the flip then runs it as before.
    fn start(s: &Staged) -> Option<Self> {
        #[cfg(test)]
        if let Some(check) = RESOLVE_CHECK.with(|c| c.borrow().clone()) {
            return Self::start_with(s, move |build_dir, exposes| check(build_dir, exposes));
        }
        Self::start_with(s, |build_dir, exposes| {
            bundle_resolve_check(build_dir, exposes)
        })
    }

    /// [`Self::start`] with the check injected.
    fn start_with(
        s: &Staged,
        check: impl FnOnce(&Path, &[ToolName]) -> Result<(), FlowError> + Send + 'static,
    ) -> Option<Self> {
        let reloc = s.reloc.as_deref()?;
        apply_sysroot_bundle(reloc).ok()?;
        let (build_dir, exposes) = (s.build_dir.clone(), s.exposes.clone());
        std::thread::Builder::new()
            .name("atpkg-resolve-ahead".into())
            .spawn(move || check(&build_dir, &exposes))
            .ok()
            .map(|handle| Self(Some(handle)))
    }

    /// The check's verdict, waiting for it if it is still running. A check thread that
    /// panicked answers no verdict, so its tree is refused as a failed check is.
    fn verdict(mut self) -> Result<(), FlowError> {
        let refused = || FlowError::Activate("resolve check: it answered no verdict".to_string());
        let handle = self.0.take().ok_or_else(refused)?;
        handle.join().unwrap_or_else(|_| Err(refused()))
    }
}

impl Drop for ResolveAhead {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.join();
        }
    }
}

/// The check [`ResolveAhead::start`] runs in place of [`bundle_resolve_check`], injected for
/// one scope on one thread ([`with_resolve_check`]). Test-only.
#[cfg(test)]
type InjectedResolveCheck =
    std::sync::Arc<dyn Fn(&Path, &[ToolName]) -> Result<(), FlowError> + Send + Sync>;

#[cfg(test)]
thread_local! {
    static RESOLVE_CHECK: std::cell::RefCell<Option<InjectedResolveCheck>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with `check` as the resolve check every [`ResolveAhead::start`] on this thread
/// starts, so a transaction test can see when its stage started one. Test-only.
#[cfg(test)]
fn with_resolve_check<R>(check: InjectedResolveCheck, f: impl FnOnce() -> R) -> R {
    let prior = RESOLVE_CHECK.with(|c| c.replace(Some(check)));
    let out = f();
    RESOLVE_CHECK.with(|c| *c.borrow_mut() = prior);
    out
}

/// Make the directory a download lands in (`staging/<program>/`, and `staging/` above it)
/// when missing, each `0700` whatever the umask ([`crate::platform::make_dirs_born`]). The
/// fetcher creates the asset at `0666 & ~umask` and the stage opens it twice (hash, then
/// extract); in a directory others could write, one of them could swap it in between.
/// Only the store's owner reads `staging/`. One that exists is left as it is.
fn make_staging_dir(dl: &Path) -> std::io::Result<()> {
    match dl.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, 0o700)
        }
        _ => Ok(()),
    }
}

/// `staging/<program>/<asset>`, where a byte-moving row downloads to, or
/// [`FlowError::VendorRefused`] when `asset` is not one plain file name. Callers unlink
/// this path and sweep the `*.part` files beside it before downloading, and
/// `vendor::check_row` does not hold `github-release` rows to a bare name, so an absolute
/// asset or a `..` climb would let a signed row delete files anywhere the user can write.
fn staged_download_path(layout: &Layout, program: &str, asset: &str) -> Result<PathBuf, FlowError> {
    let mut comps = Path::new(asset).components();
    let one_name = matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !one_name || asset.contains('/') || asset.contains('\\') {
        let mut why = String::from(
            "asset must be a bare file name inside staging (no separators, not `.`/`..`): ",
        );
        why.push_str(asset);
        return Err(FlowError::VendorRefused(why));
    }
    Ok(layout.staging_dir(program).join(asset))
}

/// Whether [`rollback`] may land `program` on its retained `build`: the channel `ch` still
/// admits it (at/above this program's floor `min_build_for` and not yanked,
/// [`crate::gate::current_build_ok`]) and its `bin/` has something to run
/// ([`build_can_be_rolled_onto`]). `None`: no verified channel in hand, so nothing is known
/// revoked.
///
/// One predicate for both readers: [`rollback`] lands on the highest retained build below
/// the active one that this admits, and gc keeps exactly that build
/// ([`crate::gc::reclaimable`]), so gc never reclaims the build a rollback would land on.
pub(crate) fn rollback_lands_on(
    layout: &Layout,
    ch: Option<&Channel>,
    program: &str,
    build: u64,
) -> bool {
    ch.is_none_or(|ch| crate::gate::current_build_ok(ch, program, Some(build)))
        && build_can_be_rolled_onto(layout, program, build)
}

/// Whether `build` of `program` is a build a rollback may land on: its `bin/` still holds
/// at least one file [`rollback_member`] could re-point a shim at.
///
/// Selection reads [`crate::ops::list_installed`], which trusts the `<n>.ready` marker, and
/// a marker can outlive its tree (an interrupted `rm -rf`, a restored backup). Selected,
/// such a build would make [`rollback_member`] remove every shim and report success. One
/// surviving entry is enough (completeness is [`crate::store::build_is_complete`]'s
/// question), and a `read_dir` failing other than `NotFound` answers `true`.
fn build_can_be_rolled_onto(layout: &Layout, program: &str, build: u64) -> bool {
    let bin = layout.build_dir(program, build).join("bin");
    match std::fs::read_dir(&bin) {
        Ok(entries) => entries.flatten().next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // EACCES on a shared prefix, macOS privacy consent, EIO: nothing is refuted.
        Err(_) => true,
    }
}

/// Roll a flipped (or partially-flipped) member back to its `prior_build`: a tool the prior
/// build contains is re-pointed there, one only the new build added has its shim removed,
/// and a fresh install (`prior_build == None`) removes the shims. The probe names the
/// executable ([`ToolName::exe_file`]), never the bare tool name.
fn rollback_member(layout: &Layout, program: &str, s: &Staged) {
    match s.prior_build {
        Some(prior) if prior != s.build => {
            let prior_dir = layout.build_dir(program, prior);
            // The restore set is the union of the new build's exposes and the prior
            // build's `bin/`: the new build's prune already deleted the shim of a tool it
            // dropped, so the exposes alone would restore the prior build with part of its
            // PATH missing. A sensitive name never had a shim (`ToolName::new` refuses it).
            // Listing names pass `rollback_may_claim`, since a sysroot bundle's `bin/`
            // carries other programs' backends; `unlink`'s restore has the same guard.
            let mut restore: std::collections::BTreeSet<crate::store::ToolName> =
                s.exposes.iter().cloned().collect();
            // Names that came from the listing alone: their aliases get the same ownership
            // check as the primary before they are re-pointed.
            let mut listing_only: std::collections::BTreeSet<crate::store::ToolName> =
                std::collections::BTreeSet::new();
            if let Ok(entries) = std::fs::read_dir(prior_dir.join("bin")) {
                for e in entries.flatten() {
                    // Files only: a stray subdirectory is not a tool.
                    if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    if let Some(name) = e.file_name().to_str() {
                        // As `ToolName::from_shim_file` strips (identity on Unix).
                        let logical = name
                            .strip_suffix(crate::platform::EXE_SUFFIX)
                            .unwrap_or(name);
                        if let Some(tool) = crate::store::ToolName::new(logical)
                            && !restore.contains(&tool)
                            && rollback_may_claim(layout, program, &tool)
                        {
                            listing_only.insert(tool.clone());
                            restore.insert(tool);
                        }
                    }
                }
            }
            // `current` first, then the shims (activation's own order), so the one
            // divergence an undo can leave is `current` ahead of the shims on the prior
            // build, which `cli::roll_forward_interrupted_activations` completes.
            if let Err(e) = activate_build(layout, &prior_dir) {
                eprintln!(
                    "atpkg: {program}: the rollback to build {prior} could not re-point \
                     `current` ({e}) — its shims are re-laid all the same"
                );
            }
            // The prior build's shims export the prior build's `shim_env` sidecar (design
            // S7); a build without one reads as none.
            let env = crate::shim_env::read_sidecar(&prior_dir);
            // The prior build's exec root before its shims (`crate::compat`): this lay goes
            // through `platform::install_shim_env`, not `install_tools_env`, so the root is
            // ensured here. Best-effort: a root that cannot be laid leaves the shims plain,
            // and says so.
            if let Some(n) = crate::compat::trust_build_of(layout, &prior_dir) {
                match crate::compat::ensure_root(layout, &prior_dir, crate::seam::Depth::Shallow) {
                    Ok(crate::compat::Ensured::Built) => {
                        println!("{}", crate::compat::laid_line(layout, n));
                    }
                    Ok(crate::compat::Ensured::Plain | crate::compat::Ensured::Present) => {}
                    Err(e) => eprintln!("{}", crate::compat::not_laid_line(layout, n, &e)),
                }
            }
            for tool in &restore {
                let target = prior_dir.join("bin").join(tool.exe_file());
                // The alias goes where the primary goes: re-pointed at the prior build
                // beside it, or dropped with it. `None` when the policy is Off (the alias
                // was never laid) or the tool is its own alias.
                let alias = (s.aliases == Aliases::Alab)
                    .then(|| tool.alias())
                    .flatten()
                    .filter(|alias| {
                        !listing_only.contains(tool)
                            || shim_free_or_owned_by(layout, program, &layout.shim(alias))
                    });
                if target.exists() {
                    let _ = crate::platform::install_shim_env(
                        &prior_dir.join("bin"),
                        tool,
                        &layout.shim(tool),
                        &env,
                    );
                    if let Some(alias) = &alias {
                        let _ = crate::platform::install_shim_env(
                            &prior_dir.join("bin"),
                            tool,
                            &layout.shim(alias),
                            &env,
                        );
                    }
                } else {
                    // A binary the new build added and the prior lacks: drop the shim.
                    let _ = crate::lay::remove_in_process(&layout.shim(tool));
                    if let Some(alias) = &alias {
                        let _ = crate::lay::remove_in_process(&layout.shim(alias));
                    }
                }
            }
        }
        Some(_) => { /* prior == new build: nothing meaningful to undo */ }
        None => {
            for tool in &s.exposes {
                let _ = crate::lay::remove_in_process(&layout.shim(tool));
                if s.aliases == Aliases::Alab
                    && let Some(alias) = tool.alias()
                {
                    let _ = crate::lay::remove_in_process(&layout.shim(&alias));
                }
            }
            // A fresh install has no prior link, but `flip_member`'s `activate_build` wrote
            // `store/<program>/current` and the abort cleanup is about to delete its
            // build: remove the link too (`remove_link`, since a Windows junction refuses
            // `remove_file`) rather than leave it dangling.
            crate::platform::remove_link(&layout.program_current(program));
        }
    }
    // The front-of-PATH twin follows its primary: this undo lays shims through
    // `platform::install_shim_env`, not `install_tools_env`, so the `agents/` reconcile must
    // run here, or a stale `agents/<agent>` (first on every PATH) keeps running the build
    // rolled away from. After the match, so the fresh-install arm is covered too.
    crate::activate::reconcile_agents(layout);
}

/// Whether a rollback of `program` may lay `bin/<tool>` for a name found only in the prior
/// build's `bin/` listing, not in the signed exposes. A sysroot bundle's `bin/` carries
/// other programs' backends (trust's carries `ay`, `clean`, `ty`), so the name is claimed
/// only when it is not another installed program's own name
/// ([`another_program_is_installed_as`]) and its shim is absent or already ours
/// ([`shim_free_or_owned_by`]).
fn rollback_may_claim(layout: &Layout, program: &str, tool: &ToolName) -> bool {
    if tool.as_str() != program && another_program_is_installed_as(layout, tool.as_str()) {
        return false;
    }
    shim_free_or_owned_by(layout, program, &layout.shim(tool))
}

/// Whether a program named `name` is installed in this prefix: its `store/<name>/current`
/// link exists or (on a prefix older than that link) `store/<name>/` holds a complete
/// build ([`crate::store::build_is_complete`]).
fn another_program_is_installed_as(layout: &Layout, name: &str) -> bool {
    if std::fs::symlink_metadata(layout.program_current(name)).is_ok() {
        return true;
    }
    let Ok(builds) = std::fs::read_dir(layout.prefix.join("store").join(name)) else {
        return false;
    };
    builds.flatten().any(|b| {
        b.file_name()
            .to_str()
            .and_then(crate::store::parse_build_name)
            .is_some_and(|_| crate::store::build_is_complete(&b.path()))
    })
}

/// Whether `shim` is absent, or resolves into `store/<program>/` (anchored to this prefix)
/// at any build. Anything else — another program's shim, a dev link into a checkout, a
/// tombstone, a file this manager cannot resolve, or a path it cannot stat — belongs to
/// someone else and is left alone.
fn shim_free_or_owned_by(layout: &Layout, program: &str, shim: &Path) -> bool {
    match std::fs::symlink_metadata(shim) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => crate::platform::resolve_shim(shim)
            .and_then(|t| crate::ops::store_build_of(&layout.prefix, &t))
            .is_some_and(|(owner, _)| owner == program),
    }
}

/// Sysroot-bundle pre-activation wiring, dispatched on the signed `reloc` policy (§10.1).
/// `self-contained` (pack-time relocated, the only policy shipped) needs nothing; every
/// other policy fails closed.
fn apply_sysroot_bundle(reloc: &str) -> Result<(), FlowError> {
    match reloc {
        "self-contained" => Ok(()),
        other => Err(FlowError::UnsupportedKind(format!("reloc={other}"))),
    }
}

/// Run the fail-loud [`crate::sysroot::resolve_check`] over every exposed binary a
/// bundle ships, so a toolchain whose dynamic loader can't resolve its libraries
/// aborts the apply instead of being reported as a successful install.
///
/// Takes the ADMITTED tool set: a name refused a shim is not reachable from `PATH`, so
/// whether its loader resolves is not a property of the install. It also names the binary via
/// [`ToolName::exe_file`] rather than joining the bare tool — the same omission
/// `rollback_member` had, harmless in practice (a PE starts with `MZ`, which fails
/// [`crate::macho::is_native_object`], and the bundle backend errors on Windows anyway) but
/// not worth leaving as the one site that still spells the rule by hand.
///
/// The checks run side by side ([`crate::net::fan_out`]), and the verdict is the first
/// failure in `exposes` order, as when they ran in turn. Each one is a fresh binary's FIRST
/// exec, and that is paid per file in proportion to its size (measured on the trust
/// toolchain's fifteen: 7 s in turn, 3.6 s at once — the floor is `trustc`'s first load of
/// its 380 MB driver library, which the tools that link the same library wait on).
pub(crate) fn bundle_resolve_check(
    build_dir: &Path,
    exposes: &[ToolName],
) -> Result<(), FlowError> {
    bundle_resolve_check_with(build_dir, exposes, &crate::sysroot::resolve_check)
}

/// [`bundle_resolve_check`] with the per-binary check injected.
fn bundle_resolve_check_with(
    build_dir: &Path,
    exposes: &[ToolName],
    check: &(dyn Fn(&Path) -> Result<(), String> + Sync),
) -> Result<(), FlowError> {
    // Only native objects (the compilers that link dylibs) have libraries to resolve; a
    // wrapper script has none, so it is skipped. Sniffed through a symlink, as the loader
    // reaches it: a `bin/<tool>` that links to its object is checked like the object.
    let bins: Vec<PathBuf> = exposes
        .iter()
        .map(|tool| build_dir.join("bin").join(tool.exe_file()))
        .filter(|bin| {
            std::fs::canonicalize(bin).is_ok_and(|object| crate::macho::is_native_object(&object))
        })
        .collect();
    crate::net::fan_out(&bins, &|bin: &PathBuf| check(bin))
        .into_iter()
        .collect::<Result<(), String>>()
        .map_err(FlowError::Activate)
}

/// Current Unix epoch second, the `now_unix` every freshness gate takes as a parameter (the
/// flow entry points stay clock-free; the CLI edge reads the clock through this). The
/// roster's clock fails closed: an unreadable clock reads as `i64::MAX`, never as 1970.
pub(crate) use aterm_update_core::roster::now_unix;

/// Parse an RFC3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` to a Unix epoch second; `None` on
/// any malformed field, which the caller treats as lapsed (fail closed). The `Z` and the
/// exact length are required: an offset stamp (`…+09:00`) read as UTC would be up to 14h of
/// fail-open skew. The workspace's one strict parser, `aterm_types::rfc3339::parse_utc`.
pub(crate) use aterm_types::rfc3339::parse_utc as rfc3339_to_unix;

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
