// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end install orchestration (§5/§7/§8/§9) — composing every verified
//! primitive into one program install, with the **network abstracted behind
//! [`Fetcher`]** so the whole sequence is unit-testable against synthetic *signed*
//! fixtures (no real release needed). The production [`Fetcher`] is a thin adapter over
//! `aterm-update-core`'s authenticated `curl` plumbing.
//!
//! The ordered, fail-closed pipeline ([`install`]):
//! 1. fetch index candidates (index + the master-signed roster published beside it) →
//!    [`crate::select::select_index`] (admit-roster-then-verify-then-select, §5);
//! 2. **reachability** — the program must be named in the verified index (§5), and pinned
//!    in the requested channel;
//! 3. [`crate::gate::decide`] — `UpToDate`/`Tombstone`/`NotPinned` short-circuit;
//! 4. fetch the per-build `pkg.toml`, verify it under the SAME roster generation that
//!    authorized the index ([`TrustedIndex::verify_pkg`] — revoked and expired machines
//!    already excluded), [`parse_pkg`], and check its signed `program`/`build_number`
//!    bind the request (anti-replay, §4.2);
//! 5. select the artifact for the target triple (a missing triple is a clean skip, §6);
//! 6. download → [`crate::install::verify_and_stage`] (signed digest → extract → tree_root);
//! 7. [`crate::activate::activate_build`] + the `bin/` shim install
//!    ([`crate::activate::install_shims`], entered here through its already-admitted half
//!    because the flow needs the admitted tool set for the bundle resolve check); record.

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

/// [`crate::install::verify_and_stage`], plus the sidecar discipline BOTH stage lanes owe.
/// It deliberately takes the PLAIN name in this module — the import above is dropped — so
/// the singleton lane and `stage_member` cannot drift apart on it.
///
/// Both lanes record the signed `shim_env` beside the build (`<build>.shim-env`, design
/// S7) BEFORE the stage, so a build is never marked ready with shims whose environment
/// could not be written — and `write_sidecar` creates `store/<program>/` in order to do
/// it. A stage that then FAILS must reclaim that sidecar here, because nothing else can:
/// `gc::interrupted_debris` enumerates `store/<program>/` for DIRECTORIES only, and
/// `store::discard_build` — the one verb that takes a sidecar away — is only ever reached
/// with a tree in hand.
///
/// This reclaims the FILE, not the directory, and the distinction is honest: the stage
/// creates `store/<program>/` itself for its `<build>.incoming-<pid>` scratch on every
/// attempt that gets past the sha256 gate, and nothing sweeps an empty program directory
/// — so a program that never installed can still leave one standing. What is closed here
/// is the unreclaimable sidecar, which is the part gc could never have reached.
///
/// The existence test is the load-bearing half. A re-stage OVER a live build fails with
/// the old tree still in place (the stage builds beside it and swaps), and that tree's
/// shims still need the environment just written for its build number — so a sidecar with
/// a build under it is never touched here. A tree that exists but is incomplete is
/// likewise left alone: it is gc's partial, and `discard_build` takes its sidecar with it.
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
/// `codesign --verify --strict` on every Mach-O); for every other program none, as before.
fn alab_pack_gate(program: &str, exposes: &[String]) -> Option<Box<StageGate>> {
    crate::alab::agent(program).map(|_| -> Box<StageGate> {
        Box::new(crate::install::alab_pack_gate(
            exposes,
            crate::install::codesign_strict_check,
        ))
    })
}

/// A pre-swap gate, owned.
type StageGate = dyn Fn(&Path) -> Result<(), StageError>;

/// The stage hooks that run `gate` (none without one).
fn stage_hooks(gate: Option<&StageGate>) -> crate::install::StageHooks<'_> {
    crate::install::StageHooks {
        pre_swap: gate,
        sidecars: None,
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
/// testable. The production impl ([`crate::net::GithubFetcher`]) reads the release
/// download host through `aterm-update-core`'s `head_no_redirect`/`download_bytes`/
/// `download_to_resumable`, with no credential and no API request.
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
    /// [`Fetcher::download`], additionally told WHICH program the asset belongs to and
    /// the row's SIGNED `size` as the byte `cap`.
    /// Default: identical to `download`. The production GitHub fetcher overrides
    /// this to honor a per-program `[packages.links]` `owner/repo` FETCH override
    /// (a possibly-private repo the program's release assets are pulled from).
    /// The host is never an authenticity input (§5/§8): the bytes still pass the
    /// identical signed-manifest sha256 + `tree_root` gates, so a redirected
    /// fetch can only supply bytes, not trust. The flow calls THIS for artifact
    /// downloads so the override reaches both the install and the stage paths.
    ///
    /// `cap` is the row's signed `size`, exactly as [`Fetcher::download_url`] takes it,
    /// because it is the same number the disk preflight was computed from
    /// (`disk_gate(size + disk_installed)`). That preflight runs once, BEFORE the
    /// transfer, and the sha256 gate runs only over the COMPLETE file — so a lane capped
    /// at anything larger writes those extra bytes into `staging/`, through the
    /// free-space floor the preflight had just defended, and refuses them afterwards
    /// (§9, "caps from the SIGNED manifest, not the API"). `0` means the row states no
    /// size ([`crate::vendor::check_row`] requires one only for the `https` protocol) and
    /// the fetcher falls back to its own ceiling.
    ///
    /// The DEFAULT drops it with `download`'s signature, which carries no cap: that is
    /// the `dir:` registry lane, which hardlinks or copies a file already on this disk —
    /// there is no transfer to bound and no host to distrust.
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
    /// Download an arbitrary SIGNED `https` `url` to `dest`, capped at EXACTLY `cap`
    /// bytes — the `https` protocol lane (`fetch_artifact`, keyed on the row's `protocol`).
    /// `cap` is the row's signed `size`, so a vendor re-release under the same URL fails
    /// fast on the cap or the sha256 and can never inflate the download.
    ///
    /// DEFAULT: REFUSED. Fail closed so a `dir:` registry or a test fetcher never reaches
    /// the network by accident — only the production network fetcher opts in, and it
    /// presents NO credential to a vendor host (the GitHub token never leaves GitHub).
    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        let _ = (url, dest, cap);
        Err(
            "this fetcher cannot fetch vendor URLs (the https protocol is network-only)"
                .to_string(),
        )
    }
    /// GET a small vendor-direct document for `program` (a release head, a manifest, a
    /// signature, a SHA256SUMS), at most `cap` bytes, conditionally on `if_none_match` when
    /// given.
    ///
    /// DEFAULT: [`VendorFetchError::Unreachable`] — a fetcher with no network leg (a test
    /// fetcher, the `dir:` registry) never reaches a vendor host. Only the production
    /// network fetcher opts in, anonymously and only for a URL that passes
    /// [`crate::vendor::vendor_direct_url_allowed`] for `program`.
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
    /// [`Fetcher::vendor_get`] for a release HEAD read inside a pass or a door: the same
    /// pins, cap and anonymity in one short attempt, because an unreached head keeps the
    /// installed build and the next pass or head-watch check is the retry. The documents
    /// behind a head that was read, and the payload, stay on the patient `vendor_get`.
    ///
    /// DEFAULT: [`Fetcher::vendor_get`] — a fetcher with no network leg has no attempt
    /// budget to shorten.
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
    /// after redirects — the exact byte cap for a payload no signed document sizes (codex).
    ///
    /// DEFAULT: [`VendorFetchError::Unreachable`], as [`Fetcher::vendor_get`].
    fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
        let _ = (program, url);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot size vendor payloads (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    /// Download `program`'s vendor-direct PAYLOAD at `url` to `dest`, capped at exactly
    /// `cap` bytes, resumably, under the same pins, anonymity and CA-trust scrub as
    /// [`Fetcher::vendor_get`]. The bytes are trusted only after the caller's sha256 check
    /// against an authenticated digest.
    ///
    /// DEFAULT: [`VendorFetchError::Unreachable`], as [`Fetcher::vendor_get`].
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
    /// tagging the index cache so a cache from one source never satisfies a failed fetch from
    /// another (the same-source guard, §14). The default is a non-matching sentinel, so a test
    /// fetcher opts out of cross-run cache reuse.
    fn source_id(&self) -> String {
        "fetcher:unspecified".to_string()
    }
    /// Whether this fetcher's last FAILED [`Fetcher::index_candidates`] failed on the LINK
    /// — no host answered (DNS, a refused or timed-out connect, a TLS failure) — rather
    /// than on a host that answered with something else (a rate limit, a 5xx, a captive
    /// portal's page). Only the first is an offline machine
    /// ([`ResolveProvenance::answered`]): a host that answered is a failure to surface,
    /// never a quiet "offline" retried soon. DEFAULT `false` — offline is a claim that needs
    /// evidence.
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
    /// The `roster_seq` of the master-signed generation that authorized that index. The
    /// caller ratchets the durable ROSTER floor to this on success, which is what makes a
    /// replayed generation refusable forever after. Carried beside `index_build` rather
    /// than derived from it because the two documents move independently: a roster bump
    /// (mint, revoke) does not re-cut the index, and an index re-publish does not bump the
    /// roster.
    pub roster_seq: u64,
    /// Whether it was already current (no change).
    pub already_current: bool,
    /// The `bin/` shims installed.
    pub shimmed: Vec<String>,
    /// `exposes` names refused a shim (sensitive-name collisions, §10).
    pub refused_shims: Vec<String>,
    /// The SIGNED `tree_root` (§8) of the installed build, copied from the release-key-
    /// verified manifest artifact. The CLI records this into `status.toml` so `atpkg verify`
    /// can re-attest the on-disk tree against the signed root (never a self-generated hash).
    /// Empty when the program was already current (no fetch) or the manifest omitted one.
    pub tree_root: String,
    /// Required-dependency pull-in outcomes (flattened transitive closure), each
    /// installed-first, with yanked/unreachable/cyclic deps SKIPPED (never gate-bypassing).
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
    /// Satisfied by a SYSTEM install ([`crate::manifest::Program::system`]): the binary
    /// is on `PATH` outside the managed prefix, so nothing is installed for it — the
    /// owner's rule 2, kept on the `requires` path (a pull-in must never lay a managed
    /// copy beside the user's own). Carries the path; the caller records the canonical
    /// `system: <path> — not managed by aterm` row if it changed.
    System(PathBuf),
    /// Skipped, with a reason (unreachable / tombstoned / not pinned / cycle / fetch error).
    Skipped(String),
}

/// Every program name a VERIFIED index carries, for [`FlowError::NotReachable`]'s
/// "it names: …" answer. `BTreeMap` keys, so the order is stable and alphabetical.
fn index_roster(index: &crate::manifest::Index) -> Vec<String> {
    index.programs.keys().cloned().collect()
}

/// The head of [`FlowError::InsufficientDisk`]'s sentence. The window paints a first run
/// that ended on it (aterm-gui's `toolchain_words`), since freeing disk is the person's
/// to do; it matches this constant, so a rewording here cannot leave it reading the old
/// words.
pub const DISK_REFUSAL_HEAD: &str = "not enough disk space";

/// Why an install failed, fail-closed at each gate.
#[derive(Debug)]
pub enum FlowError {
    /// No signature-valid index at/above the floor was found.
    NoIndex,
    /// The index SOURCE could not be reached at all — offline, DNS failure, a proxy,
    /// a 403 rate-limit — and no §14 cached index was available to stand in.
    ///
    /// Distinct from [`FlowError::NoIndex`] because the two need opposite reactions:
    /// `NoIndex` is a TRUST verdict, and an offline machine must be told to connect, not
    /// sent to key management.
    Unreachable(String),
    /// The program is not named in the verified index (unreachable, §5). The second
    /// field is the roster the index DOES name, captured at construction — the one
    /// moment the just-verified index is in hand — so the error can answer the user's
    /// immediate next question ("then what IS installable?") without any surface
    /// re-fetching, and without any trust change: the printed roster is
    /// master-root-verified. Empty means "roster unknown" and the message stays short.
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
    /// The per-build manifest spells a RETIRED `kind` (`vendor-fetch`); carries the
    /// split that replaced it ([`crate::Reject::RetiredKind`]) so the refusal names the
    /// fix.
    RetiredKind(&'static str),
    /// The per-build manifest's `shim_env` breaks the rule a shim can honour
    /// ([`crate::Reject::ShimEnv`], design S7); carries the entry and the reason, like
    /// `RetiredKind`, so the authoring machine's own install names the fix.
    ShimEnv(String),
    /// The signed `program`/`build_number` did not match the request (anti-replay).
    Mismatch,
    /// No artifact for the target triple (a clean skip, §6).
    NoArtifact(String),
    /// The artifact's `kind` is not installable by this tool path — an unrecognized kind.
    /// Fail-closed (§16.4 dispatch).
    UnsupportedKind(String),
    /// An `app-bundle` member over `github-release` — the aterm app itself (§16.4). atpkg
    /// never installs, stages or swaps it: the app updates itself in-session through its
    /// own notarization-gated updater (aterm-update + aterm-gui's overlap handoff), a
    /// topology the tool-install path does not carry. So this pair is refused, always,
    /// before any byte moves. No live index names such a member; the day one does, staging
    /// it for the app's own lane is new work (docs/TOOLCHAIN-PACKAGE-MANAGER.md §16.4).
    AppBundleRefused(String),
    /// An artifact row failed per-protocol admission ([`crate::vendor::check_row`]) BEFORE
    /// any byte moved: for `https`, a non-https or non-allow-listed `url`, an unknown
    /// `payload`, a raw-binary `entry` that is not an exposed shimmable name, a hostile
    /// `links` target, or a missing signed digest; for any other protocol (the deleted
    /// OS-installer ones included), the protocol itself. The message names the field.
    VendorRefused(String),
    /// The program's signed index row names the RETIRED `requires` relation
    /// ([`crate::manifest::Program::names_requires`], design 2026-09-22 §5.3(b)): this
    /// client keeps no install order, so the program is refused before any byte moves
    /// rather than installed ahead of what it names. Carries the program.
    RequiresRetired(String),
    /// The asset download failed.
    Download(String),
    /// Staging (signed digest / extract / tree_root) failed.
    Stage(StageError),
    /// The pinned build's SIGNED digests are the ones a recent attempt already proved the
    /// published asset does not match, and the cooldown on that refusal has not lapsed —
    /// so this pass did not re-download the asset to reach the same verdict
    /// ([`crate::store::StageRefusal`]). Carries the recorded sentence, which names the
    /// original stage failure and when the next attempt is due. Nothing was fetched,
    /// staged or changed.
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
    /// The program is dev-linked (linkmode, §13) — `update`/`apply` HARD-SKIPS it until
    /// `atpkg unlink`.
    Linked(String),
    /// The vendor-direct lane did not install a vendor program: its verdict line
    /// (refused, failed, or a head it will not move to), versions only.
    Vendor(String),
    /// The vendor's release channel was not reached — the same verdict line, its own
    /// variant so the verb can add the re-run follow-up the index lane's
    /// [`FlowError::Unreachable`] has always had.
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
                // No "…the toolchain retries automatically" tail: that is true only of
                // the windowed app's 6-hour loop, and this Display reaches foreground
                // CLI verbs where nothing retries anything. The CLI edge appends the
                // honest per-verb re-run instead (`print_unreachable_followup`).
                write!(f, "could not reach the toolchain index: {why}")
            }
            FlowError::NotReachable(p, roster) => {
                // The grep-stable phrase, verbatim; the roster rides in a parenthesis.
                write!(f, "{p} is not named in the signed index")?;
                // Whole roster when it is short; first 12 otherwise — the point is a
                // usable answer, not a wall.
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
            // No head: the recorded sentence already names the program, the build, the
            // original stage failure and the way out — a "stage: " prefix would read as
            // if this pass had staged something, and it staged nothing.
            FlowError::StageRefused(m) => f.write_str(m),
            FlowError::Activate(e) => write!(f, "activate: {e}"),
            FlowError::Rollback(m) => {
                // No "rollback: " head: the one CLI edge already prints
                // "atpkg: rollback <p> failed:", and the doubled word read like two
                // errors stacked.
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
/// `triple`, and the currently-`installed` build (if any) for the up-to-date/upgrade
/// decision.
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

/// Re-write `store/<program>/current` at `build` unless
/// the prefix already proves that build live for `program` ([`crate::gc::live_builds`]).
///
/// The up-to-date path's one write. `installed` is what the shims run, so on this path
/// the pin and the shims agree — but a prefix last written by a manager older than the
/// per-program link, a `current` that dangles because its build was removed, or a channel
/// link left at a superseded build has NO witness for the program: GC abstains on it
/// forever and `doctor` names it diverged with `update` as the remedy. Until this existed,
/// that remedy did nothing, because the program was "already current". A healthy store is
/// not touched; a build that is not on disk is left for `doctor` to name.
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

/// The installed-build view [`decide`] may call UP-TO-DATE: the SHIM-derived build, dropped
/// to `None` when the tree it names is not a COMPLETE install **for this slice**
/// ([`crate::store::build_is_complete`] — the sibling `<build>.ready` marker, platform
/// record and all).
///
/// WHY THE SHIM VIEW IS NOT ENOUGH. `installed` reaches every apply path from
/// [`crate::ops::active_builds`], which resolves `bin/<tool>` and stats its target — the
/// authority on what RUNS, and deliberately silent about the marker. So a LIVE build whose
/// marker is missing, or was written by the OTHER slice of the universal binary
/// (`store::running_platform`: an `arch -x86_64` pass installs INTEL trees into the very
/// same `store/<program>/<build>/`), still handed `decide` a number equal to the pin,
/// `decide` answered `UpToDate`, [`reassert_witness`] found a valid witness and returned —
/// and NOTHING ever re-staged that tree. `list_installed`, `doctor` and `gc` all read the
/// marker, so `doctor` FAILed forever ("active <p> build <n> store missing/incomplete")
/// while the structural repair it names, `install <program>`, re-entered through the same
/// shim view and printed "already current". The machine kept running the other
/// architecture's compilers and solvers under Rosetta, silently — the exact outcome the
/// platform record was added to prevent.
///
/// This is the input that makes that record's promise true: an incomplete or other-slice
/// tree reads as not-installed to `decide` as well, so the ordinary Install path re-stages
/// it in place ([`Staged::was_live`] keeps the live tree through an abort) and the marker
/// comes back native.
///
/// Strictly a DOWNGRADE of the decision — `None` can only turn `UpToDate` into `Install`,
/// never suppress a `Tombstone` (which reads the pin, not the installed build) — and
/// `build_is_complete` spends only a marker PROVEN absent (or proven another slice's) on
/// `false`, so a marker this process merely cannot read never triggers a multi-gigabyte
/// re-download.
fn installed_for_decide(layout: &Layout, program: &str, installed: Option<u64>) -> Option<u64> {
    installed.filter(|&build| crate::store::build_is_complete(&layout.build_dir(program, build)))
}

/// Install (or force-upgrade) the program named by `req`, using `fetcher` for all network
/// I/O, `anchor` as the pinned paper-master keyset + durable roster ratchet, and `floor`
/// as the durable index high-water **paired with the roster generation that recorded it**
/// ([`BuildFloor`] — a floor a machine set does not outlive the generation that revoked
/// that machine). See the module docs for the ordered, fail-closed pipeline. An unarmed
/// `anchor` installs nothing: `select_index` yields no candidate and this returns
/// [`FlowError::NoIndex`].
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

/// [`install`], additionally filling `resolved_assets` with `program → pinned asset file
/// name` for every program (the requested one AND each `requires` pull-in) the pass
/// resolved far enough to select an artifact.
///
/// An OUT-PARAM, not an [`InstallReport`] field, because it must survive the `Err` path:
/// the program whose download failed mid-transfer is exactly the one whose `<asset>.part`
/// resume state the caller's pass-end [`crate::gc::run_keeping_pinned_partials`] must
/// spare — and a failed install returns no report to carry the name. The map only ever
/// SHRINKS that gc sweep (see the sparing form's trust-posture note), so over-collecting
/// on the success path is harmless: a finished install has no `.part` left to spare.
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

/// [`install`] with a cycle-guard `seen` map threaded through the `requires` recursion (§17):
/// a dependency that requires back into a program already on the resolution stack (`None`)
/// is skipped rather than looping, and one a sibling edge already resolved in this call
/// (`Some(outcome)`, a diamond) carries that outcome to every later edge naming it.
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
    // 0. Dev-link HARD-SKIP (§13): a linked program is managed from its checkout, not the
    //    registry — short-circuit before any network I/O so link composes ahead of every
    //    other gate.
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
    // 1–2. Resolve + verify-select the index + freshness (§8 gate 2) — the shared
    // [`resolve_verified_index`] prologue (cached-fallback, §14) — then reachability, and
    // the retired `requires` refusal (§5.3(b)) before anything is decided or fetched.
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let row = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    if row.names_requires() {
        return Err(FlowError::RequiresRetired(program.to_string()));
    }
    let repo = &row.repo;
    // The channel AS SEEN FROM THIS HOST: a per-target pin overlay, if the index
    // carries one for `triple`, is resolved here and nowhere else.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let ch = &ch;
    let &pinned = ch
        .pin
        .get(program)
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;

    // 3. The apply decision — from the COMPLETENESS-aware view of what is installed
    //    ([`installed_for_decide`]), never the raw shim view: a LIVE build whose store tree
    //    this slice cannot vouch for must re-stage, not read as already-current.
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
                // An up-to-date program short-circuits before its manifest is fetched, so its
                // `requires` are not resolved on a no-op install (documented gap).
                tree_root: String::new(),
                dependencies: vec![],
                vendor: None,
            });
        }
        ApplyDecision::Tombstone => {
            // Actively DISABLE the revoked build's OLD working shims (§7): install a failing
            // tombstone shim over each currently-exposed tool so a yanked/below-floor build is
            // not left runnable, rather than merely reporting Tombstoned. Best-effort — a
            // tombstone-write failure must not mask the authoritative Tombstoned error.
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
        // The retired `kind = "vendor-fetch"` names the split that replaced it — the
        // one parse refusal whose words reach the authoring machine, so the fix is
        // spelled rather than read as a bare "malformed".
        crate::Reject::RetiredKind(why) => FlowError::RetiredKind(why),
        // So does a `shim_env` the rule refuses (design S7): the entry and the reason.
        crate::Reject::ShimEnv(why) => FlowError::ShimEnv(why),
        _ => FlowError::PkgParse,
    })?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return Err(FlowError::Mismatch);
    }

    // 4b. Runtime `requires` (§17): pull in each MISSING dep FIRST, through the SAME verified
    // pipeline (reachability + freshness + floor/yank gate). Best-effort — a dep failure never
    // fails the parent's install; a Tombstone/NotReachable is SKIPPED, never bypassed.
    // `requires` is the pkg manifest's own SIGNED metadata, parsed from verified bytes, so a
    // repo-write adversary can neither inject nor redirect an edge.
    let requires = &pkg.requires;
    let mut dependencies: Vec<DepOutcome> = Vec::new();
    // ONE `bin/` scan for the whole loop: any program this recursion installs is in
    // `seen` (inserted at entry) and screened by the check below BEFORE the map is
    // consulted, so a pre-loop snapshot decides identically.
    let active = crate::ops::active_builds(layout);
    for dep in requires {
        if let Some(earlier) = seen.get(dep) {
            match earlier {
                // Resolved EARLIER in this call — a sibling's recursion pulled it in (a
                // diamond: top requires [mid, low], mid requires low). Its outcome stands
                // for this edge too. Already in this report ⇒ nothing to add; from another
                // subtree ⇒ replayed, a fresh install as AlreadyPresent so the caller
                // records it once.
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
        // Rule 2 on the requires path: a `system = "<bin>"` dependency the user's own
        // copy satisfies is NOT pulled in — the pass would only retire the managed copy
        // again next tick, and the user's copy is the one that runs anyway.
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
    // The pass's verified answer for this program, recorded BEFORE the download so a
    // transfer that fails midway still reaches the caller's collector — the pass-end
    // `gc::run_keeping_pinned_partials` spares exactly these programs' `<asset>.part`
    // resume files, and the failed download is the one the sparing exists for.
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    // Per-member dispatch (§16.4/§17) on BOTH halves of the row: the tool path installs
    // plain `binary`/`cargo-src` (Shim — over `github-release` or `https`; only the
    // download lane differs, and `fetch_artifact` picks it from the PROTOCOL), a vendor
    // `.app` (VendorApp — the `https` + `app-bundle` + `dmg` lane, which lands in the
    // store and shims through its `links` exactly like Shim), AND `sysroot-bundle`
    // (trust / trust-mc) artifacts. A sysroot-bundle gets bundle-specific wiring
    // ([`apply_sysroot_bundle`]) BEFORE activation plus a fail-loud resolve check AFTER —
    // a failure past activation UNWINDS ([`abort_activated_install`]) so a broken
    // toolchain is neither reported SUCCESS nor left live reading as 'already current'.
    // `app-bundle` over `github-release` (the aterm self-update, applied in-session by the
    // app's own updater) is refused here, always, and is NEVER the vendor-app lane.
    // Unknown pairs — the deleted OS-installer protocols' among them (§5.3(b)) — remain
    // refused CLOSED. (audit: sysroot-bundle silent-broken-install; sysroot-bundle
    // resolve-failure left-active wedge.)
    //
    // Every row is admitted HERE, before the disk preflight and before any byte moves
    // (`vendor::check_row`, protocol-aware: the release lane has only its asset name to
    // check, the https lane keeps every refusal it always had, any other protocol is
    // refused by name).
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
            // ALab's own tools get their `alab-<tool>` aliases; a vendor extra or a
            // system-satisfiable member does not (`activate.rs` module doc).
            aliases: Aliases::for_program(program, index.program(program)),
            installed,
            strategy,
            hooks: &hooks,
            download_turn: None,
        },
        &|dl| fetch_artifact(fetcher, program, repo, artifact, dl),
    )?;
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

/// An install of a vendor-direct program (`req.program`, whose row is `spec`): the vendor
/// lane under `trust` and the door's yanks ([`door_vendor_policy`], the index `anchor` and
/// `floor` verify), as a person's own act — no local-pin hold, and an unread legacy build
/// above the ceiling is replaced. The outcome, so the CLI edge records the program's row
/// from the lane's own verdict.
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

/// The signed yanks a targeted vendor door decides under (design §1.4–§1.5) — a person's
/// `update <program>` or `install <program>`, `claude update`/`codex update`, the head
/// watch's `--head-watch`. When the latch is absent or more than an hour old
/// ([`crate::vendor_direct::policy::YankLatch::is_current`]), the index is verified first,
/// live (no cached fallback), which refreshes the latch, and its yanks bind: a yank
/// published since the last pass binds this door. That resolve never blocks or fails the
/// door — unreached, stale or refused, the door decides on the latch as it stands.
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
            // An aborted transfer leaves bytes in `<dl>.part`, not at `<dl>`: the production
            // fetcher promotes the part onto `dl` only on curl success, so `dl` is either
            // absent or complete. The part is deliberately LEFT for the next attempt to
            // continue from (`aterm_update_core::download_to_resumable`); `dl` itself is
            // reclaimed on this exit exactly as on the stage exit below, because a `dir:`
            // registry's copy lane writes there directly.
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
    // Capture "this build is ALREADY live" BEFORE the stage swaps a new tree into it and
    // before `activate_build` can move the links — by abort time the answer is gone. The
    // `installed` argument cannot answer it: it is the SHIM view and goes silent for a live
    // program whose tools were unlinked or tombstoned, which is the very reason `decide`
    // returned Install for a build that is already active. See `abort_activated_install`.
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    // Preflight again before extract: the asset is already downloaded, so only the extracted
    // tree remains to fit. Reclaim the asset before returning — the ONE failure whose
    // meaning is "the volume is full" must not walk away leaving the thing making it fuller.
    if let Err(e) = disk_gate(
        artifact.cost.disk_installed,
        crate::freespace::available_bytes(&build_dir),
    ) {
        let _ = std::fs::remove_file(&dl);
        return Err(e);
    }
    // Reclaim the compressed asset on EVERY exit, not just the happy one: a stage that fails
    // (bad mirror, tree_root mismatch, full disk) otherwise strands a full archive in
    // `staging/` forever — nothing else ever sweeps that directory, since
    // `gc::interrupted_debris` walks `store/` only — and a member that keeps failing keeps
    // leaking, one copy per distinct asset name.
    // The verify→extract boundary lives inside `verify_and_stage`; the extract scope
    // credits `write_capped`'s loop to this program, and the first written byte flips
    // the phase label honestly (see `progress::extract_scope`).
    // The shim environment the signed manifest declares (design S7) is recorded BESIDE
    // the build (`<build>.shim-env`) before the tree is staged: the verbs that hold no
    // manifest — the transaction's rollback, `rollback`, `unlink`'s restore — re-lay this
    // build's shims from it. Before the stage, so a build the sidecar could not be
    // written for is never marked ready with shims that would lack their environment.
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
    // This build's bytes are good, whatever an earlier pass recorded about them — a
    // publisher who repaired the asset under the same pin is exactly the case the
    // cooldown exists to let through, so the memo goes with the verdict it held.
    crate::store::clear_stage_refusal(&build_dir);

    // 6b. Sysroot-bundle wiring BEFORE activation (self-contained = no-op).
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    // 7. Activate + shim. The raw manifest `exposes` is admitted ONCE here; `tools` is what
    // actually got a shim and `refused` the sensitive/malformed names that did not.
    crate::progress::note_phase(program, crate::progress::Phase::Link);
    activate_build(layout, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(l.exposes);
    // Past activation a failure leaves the broken build LIVE — channel `current`,
    // per-program witness, any shims already written — AND carrying its `.ready`
    // marker, so `active_builds` reports it, `decide` calls it UpToDate, and a retry
    // prints 'already current' with nothing working (the wedge `flip_member` rolls
    // back on the transactional path). Capture the same rollback input here so both
    // error arms below unwind identically. (audit: resolve-failure left-active wedge.)
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
    // The shims export the signed manifest's `shim_env` (design S7) — a managed vendor
    // tool runs with its own updater off; a system copy never runs through a shim.
    if let Err(e) = install_tools_env(layout, build_dir, tools, aliases, l.shim_env) {
        abort_activated_install(layout, program, &staged);
        return Err(FlowError::Activate(e.to_string()));
    }

    // 7b. Fail-loud resolve check: an installed sysroot-bundle's compilers must
    // actually load. A dynamic-loader failure here aborts — and UNWINDS — the install.
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle
        && let Err(e) = bundle_resolve_check(build_dir, tools)
    {
        abort_activated_install(layout, program, &staged);
        return Err(e);
    }
    // NOTE: the shell.d hook refresh runs at the main.rs CLI edge (do_install / cmd_update),
    // NOT here — writing ~/.aterm from flow's synthetic-layout unit tests would pollute the
    // developer's real home (identical hermeticity reasoning as the GC-after-activate edge).
    let shimmed: Vec<String> = tools.iter().map(|t| t.as_str().to_string()).collect();
    Ok(Landed {
        shimmed,
        refused,
        root,
    })
}

/// The ONE place the download lanes fork — on the row's PROTOCOL, never its kind or its
/// apply strategy. A `github-release` member goes through [`Fetcher::download_for`] (the
/// program's `repo` under the account slug, the `[packages.links]` override, the GitHub
/// token) — with the SIGNED `size` as its byte cap, the same number the disk preflight
/// bounded the free-space check with, so no lane may write more than the preflight
/// admitted. An `https` member goes through [`Fetcher::download_url`] with the SIGNED
/// row's `url` and the SIGNED `size` as the exact byte cap — and NEVER through
/// `download_for`: the vendor host is not a release repo, and nothing about the account
/// slug may reach it. Anything else is refused by name. The row was admitted by
/// [`crate::vendor::check_row`] before this is called.
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

/// Re-point `program` at its retained build `to` — the vendor lane's signed-yank
/// rollback: the `current` flip, then shims for `exposes` alone under the build's own
/// `.shim-env`, exactly as a vendor landing lays them. Never the build's `bin/` listing
/// ([`rollback_member`]'s restore set): a vendor's exposes come only from the table.
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

/// Install a failing tombstone shim (§7) over EVERY tool `program`'s currently-active build
/// exposes, so a yanked/below-floor build's old working shims are actively disabled. The tool
/// set is the program's live `bin/` shims (the exact "old working shims" to revoke); if the
/// program is not installed there is nothing to disable. Best-effort per tool (a write failure
/// on one shim never blocks the rest); a tombstone can never shadow a sensitive name because
/// `active_tools` hands back [`crate::store::ToolName`]s, which only exist for admitted names.
pub(crate) fn install_tombstone_shims(layout: &Layout, program: &str, installed: Option<u64>) {
    if let Some(cur) = installed {
        // The `alab-<tool>` aliases forward to the same revoked executable, so they are
        // disabled with their primaries — a revoked build must not stay runnable under
        // its other name.
        let tools = crate::ops::active_tools(layout, program, cur);
        let aliases = crate::ops::active_aliases(layout, program, cur);
        for tool in tools.iter().chain(&aliases) {
            let _ = install_tombstone_shim(layout, tool);
        }
    }
}

/// Unwind a per-program install that failed strictly AFTER [`activate_build`]
/// (`install_tools` / [`bundle_resolve_check`]). Three steps, each already proven
/// elsewhere: [`rollback_member`] restores the prior build's whole shim surface +
/// links (an upgrade reverts; a fresh install removes its shims + witness);
/// [`crate::activate::undo_activation`] then sweeps any pointer STILL naming the
/// doomed build — the channel `current` a fresh-install rollback has no prior to
/// re-point at — and is a no-op after a successful prior-build restore (it only
/// removes links/shims resolving INTO `build_dir`); finally the staged build is
/// DISCARDED so `active_builds`/`decide` can never re-read the broken tree as
/// complete and answer 'already current' on a retry.
///
/// …EXCEPT when `staged.was_live`, i.e. this install re-staged the build that was ALREADY
/// live. The discard's whole justification is "do not leave behind a build a retry will
/// trust", and it is sound for a build this call created. It is not sound for one that was
/// live and complete before the call: the failures that land here are `install_tools`
/// (writing `bin/` shims — an EACCES, EROFS or full disk, i.e. a fact about the environment
/// and not about the tree) and the resolve check. Deleting a multi-gigabyte verified tree in
/// response to a failed shim write leaves the user with NO toolchain, and re-downloading it
/// is exactly what the failing condition tends to forbid. `decide` cannot be fooled by what
/// survives, either: `undo_activation` still runs, so no shim resolves into the tree and
/// `active_builds` stays silent about it, and the next run RE-STAGES this build in place
/// rather than answering 'already current'. Reached whenever the SHIM-derived `installed`
/// view is silent for a live program (`atpkg unlink`, tombstoned tools, dev-link mode) and
/// `decide` therefore returns Install for the build `store/<program>/current` already
/// names — the same blind spot [`Staged::was_live`] exists for in the group transaction.
///
/// **AND THE KEEP HAS TO OUTLIVE THE RUN**, which it did not while this arm merely returned.
/// [`Staged::was_live`] is computed (step 6) from exactly two links, and the two steps above
/// remove BOTH for this member: the shim view being silent is what makes `prior_build`
/// `None`, so `rollback_member`'s fresh-install arm takes `store/<program>/current`, and
/// `undo_activation` takes the channel link. The retry therefore probed `was_live == false`,
/// and the SECOND failure of the same unwritable `bin/` fell through to the discard and
/// deleted the multi-gigabyte verified tree the FIRST abort had deliberately kept — the
/// toolchain gone on the one machine least able to re-download it, and [`crate::gc`]
/// abstaining in the meantime only because the witness was missing rather than because
/// anything claimed the tree. So the per-program witness is re-pointed at the kept build:
/// the state `was_live` just PROVED the store was in before this run, which makes the keep
/// hold for every retry and makes `gc::live_builds` call the tree this program's live build
/// instead of an unwitnessed one. Only when nothing else claims that link — a rollback to a
/// DIFFERENT prior build re-pointed it already, and that restore wins — and only for a tree
/// whose `.ready` marker still reads complete.
fn abort_activated_install(layout: &Layout, program: &str, staged: &Staged) {
    rollback_member(layout, program, staged);
    crate::activate::undo_activation(layout, &staged.build_dir);
    if staged.was_live {
        // Re-point the witness at the tree this arm keeps, or the keep is one-shot: the
        // two steps above removed the very links the next run's `was_live` reads. EXISTS,
        // not resolves, is the claim test — the same one `gc::authority_claims` applies —
        // so a link a prior-build rollback already wrote is never overwritten here.
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

/// A member staged (verified + extracted) but NOT yet flipped, plus what `flip`/`rollback`
/// need: its new build dir, exposed binaries, and the build its shims pointed at BEFORE
/// (for the rollback swap-back). Collected during the stage phase of [`transact`].
struct Staged {
    build: u64,
    build_dir: PathBuf,
    /// The tools that will actually be shimmed — the ADMITTED set, not the raw manifest list.
    /// Holding [`ToolName`]s here rather than `Vec<String>` forces `rollback_member` to probe
    /// the prior build via `exe_file()`, never the bare name — see [`ToolName`]'s docs.
    exposes: Vec<ToolName>,
    /// The member's prior active build (`None` ⇒ a fresh install: rollback removes the shims).
    prior_build: Option<u64>,
    /// `true` when this member's `build_dir` was ALREADY the live build when we staged it: a
    /// `current` authority link named it while the SHIM-derived `installed` view
    /// ([`crate::ops::active_builds`]) was silent (every tool tombstoned, or `atpkg unlink`
    /// removed the dev shims), so `decide` returned Install for the build that is already
    /// active. The abort discard must never delete such a tree.
    was_live: bool,
    /// `Some(reloc_policy)` for a `sysroot-bundle` member (its pre-activation wiring +
    /// post-activation resolve check run during the flip); `None` for a plain Shim.
    reloc: Option<String>,
    /// The SIGNED `tree_root` of the staged build, surfaced in [`ChannelApplyReport::applied`]
    /// so the CLI can record it for `atpkg verify` after an update flip.
    tree_root: String,
    /// Whether the member's tools get their `alab-<tool>` aliases — read off the signed
    /// index entry ([`Aliases::for_program`]) when it is staged, or off the disk
    /// ([`Aliases::laid_for`]) by the rollback verb, which holds no index. The flip lays
    /// them with the primaries and the rollback re-points or removes them with the
    /// primaries.
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
    /// Per member the apply flipped LIVE: its new build + SIGNED `tree_root`, so the CLI can
    /// persist the root into `status.toml` for `atpkg verify`. Keyed by program name.
    pub applied: BTreeMap<String, AppliedMember>,
    /// Members excluded from this apply because they are dev-linked (§13) — reported so the
    /// CLI can note the skip.
    pub skipped_linked: Vec<String>,
    /// `program → pinned artifact asset file name` for every member whose release-verified
    /// manifest this pass resolved far enough to SELECT an artifact — INCLUDING members
    /// whose download or stage then failed, which is the point: the pass-end
    /// [`crate::gc::run_keeping_pinned_partials`] spares exactly these programs'
    /// `<asset>.part` resume files, and the failed member is the one with a partial worth
    /// sparing. A program absent here gets nothing spared (the safe default — the sparing
    /// closure only ever SHRINKS the sweep, so an empty map is plain `gc::run`).
    pub resolved_assets: BTreeMap<String, String>,
}

/// A member the apply flipped live: its new build + the SIGNED `tree_root` for `atpkg verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMember {
    /// The build now active.
    pub build: u64,
    /// The signed `tree_root` of that build.
    pub tree_root: String,
}

/// What a [`rollback`] did: the program moved from `from_build` down to `to_build`, resolved
/// against the signed index at `index_build`. `coherence_group` (if any) lets the CLI WARN
/// that rolling back one member of a locked tuple splits it.
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
    /// The local pins that keep it on `to_build` through the next update: its own, or — in
    /// a coherence group — any member's, since one pinned member holds the whole tuple (the
    /// update pass's pin gate). Only pins that gate reads: never an uninstalled member's
    /// ([`deliberately_absent`]), and none while [`Self::revoked_member`] is set. Empty when
    /// the next update moves it forward again.
    pub held_by: Vec<String>,
    /// A member of its group whose current build is revoked (yanked or below the floor), with
    /// that build: the next update then moves the whole group forward whatever is pinned
    /// (the pin gate's `all_current_valid`).
    pub revoked_member: Option<(String, u64)>,
}

/// Update EVERY program the channel pins, **group by group**, each coherence group applied
/// as an all-or-nothing transaction (§7): the `rustc`-locked tuple (`trust`/`ay`/…) moves
/// atomically — stage every member, and only once ALL staged, flip every member; a stage
/// failure flips nothing, a flip failure rolls the already-flipped members back — while an
/// ungrouped tool applies independently so it can never wedge the tuple.
///
/// The index is resolved + verified ONCE (unlike the per-program [`install`] loop, which
/// re-verified it each call and could not move a group atomically). `installed` maps each
/// program to its currently-active build. Never re-fetches the index between members, so a
/// mid-run index change can't split a group across two index states.
///
/// ONCE *PER CALL*, which is not once per PASS: a caller that runs a second lane after
/// this one (`atpkg update`'s attestation repair and set completion) must hand the index
/// it already holds to [`apply_channel_gated`] rather than call this and resolve again.
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
    // 1–2. Resolve + verify-select the index ONCE + freshness (§8) — the shared
    //      [`resolve_verified_index`] prologue (cached-fallback, §14).
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

/// [`apply_channel`] over a CALLER-RESOLVED, already-verified index — the shape
/// [`bootstrap_group`] has always had, for a caller whose pass has more than one lane —
/// under `flip`, the unattended lanes' form: the coherence group holding the Trust
/// toolchain stages its new builds and holds its flip while that toolchain is in use
/// ([`crate::quiet`], [`TxnOutcome::Deferred`]). Every other group, and every group under
/// [`crate::quiet::FlipGate::NOW`] (what `aterm pkg update` runs), applies at once.
///
/// `atpkg update` is that caller: one pass applies the channel, repairs a missing
/// attestation and completes the default set on ONE resolved index. Resolving once is
/// cheaper and also the stronger property: a pass that re-resolves could read two index
/// states, which is the split §7 exists to forbid.
///
/// The gates are the caller's, exactly as they are for [`bootstrap_group`]: this
/// function verifies nothing itself, so `index` must come from
/// [`resolve_verified_index`] (anti-rollback floor, roster admission, machine signature,
/// freshness), and a caller holding one across a long pass re-checks its freshness window
/// before handing it to the next lane.
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
    // The channel as THIS target sees it (`pin_by_target` laid over `pin`): every decide,
    // plan and fetch below reads this view, never the raw platform-agnostic pin.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;

    // 3. Partition the channel's pins into coherence groups (grouped tuples + singletons)
    //    and apply each as its own all-or-nothing transaction.
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
    // A HELD FLIP NO GROUP HOLDS ANY LONGER is over: the toolchain's group was skipped
    // whole this pass (dev-linked, or nothing of it installed any more), so no outcome
    // above ended its record, and a record left standing would keep the window's park
    // looking at it. A group that held it again or aborted it keeps it.
    if !report.groups.iter().any(|(g, o)| {
        g.members.iter().any(|m| m == crate::seam::SEAM_PROGRAM)
            && matches!(o, TxnOutcome::Deferred { .. } | TxnOutcome::Aborted { .. })
    }) {
        crate::quiet::clear(layout);
    }
    // (Shell.d hook refresh runs at the main.rs CLI edge, not here — see the note in
    // `install` — to keep apply_channel's unit tests hermetic w.r.t. the real ~/.aterm.)
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
        // Dev-linked HARD-SKIP (§13): a coherence tuple with ANY linked member is skipped
        // whole (can't partial-move a locked group over a dev link).
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

/// Is any of `programs` INSTALLED HERE BUT DISABLED — a build this prefix still witnesses
/// as live, whose `bin/` shims were replaced by the failing TOMBSTONE scripts a revocation
/// writes (§7)? The one test both lanes use to revisit a tombstone their shim view hides.
///
/// The update lane's group selection reads the shim view, which is deliberately SILENT for
/// a tombstone ([`crate::platform::resolve_shim`] yields `None` for a script with no `exec`
/// line). That silence is the self-heal for a member of a group that still has an installed
/// sibling, and a dead end for a group that has none — the case this answers.
///
/// Both halves are required, and each is narrow on purpose:
///
/// * **A shim that is present and refuses to run.** Not a MISSING shim: that is an
///   interrupted uninstall or a hand-removed file, and treating it as "installed" would let
///   this lane reinstall a program nobody asked it to (the update pass must never install a
///   missing dependency — it holds the dependent instead).
/// * **A witness for the program's live build, whose `bin/` holds that tool's executable.**
///   A tombstone carries no target, so the only honest way back to its program is the build
///   [`crate::gc::live_builds`] proves live: a non-resolving `bin/<tool>` whose name matches
///   an executable of THAT build is this program's disabled command. Without the match, any
///   tombstone anywhere would speak for every program in the prefix.
///
/// One `live_builds` scan and one `bin/` scan per call, on the path that was otherwise
/// about to skip the group.
pub(crate) fn tombstoned_in_place<'a>(
    layout: &Layout,
    programs: impl IntoIterator<Item = &'a str>,
) -> bool {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return false;
    };
    // Every `bin/` name that is a shim shape this manager could have written AND currently
    // forwards nowhere — the tombstones, plus anything else inert that got the same name.
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
            // `presence`, not `exists`: an EACCES/EPERM on the build dir is not proof the
            // tool is gone, and the fail-closed direction for a REVIVAL is to keep looking.
            tombstones
                .iter()
                .any(|tool| !crate::store::presence(&build_bin.join(tool.exe_file())).is_absent())
        })
    })
}

/// Whether the update pass drops `m` from its coherence group before any gate reads the
/// group ([`apply_group`]): recorded removed, on a machine that declined the set, or named by
/// `[packages].exclude` — AND absent from `installed`. [`rollback`]'s pin report reads the
/// same test, so it never names a pin the pass does not read.
fn deliberately_absent<'a>(
    layout: &Layout,
    installed: &'a BTreeMap<String, u64>,
    excluded: &'a [String],
) -> impl Fn(&String) -> bool + use<'a> {
    let removed = layout.removed_programs();
    // `uninstall --all` writes `declined` — the durable "this machine does not want the
    // bundled toolset" — and clears nothing per-program. Without this, installing ONE
    // program afterwards would let the next unattended pass pull the rest of its
    // coherence tuple back, gigabytes, unannounced.
    let declined = layout.declined().is_file();
    // `[packages].exclude` arrives as the `excluded` PARAMETER, the same way the
    // layout's own records do — never via `config::cached()`. That distinction is
    // load-bearing: the global is a process-wide OnceLock over the INVOKING user's
    // aterm.toml, and this layer decides against a caller-supplied `layout` — reading
    // the global would apply another prefix's exclusions and tie unit tests to the
    // developer's real config. The CALLER (cli.rs) owns whose config speaks.
    //
    // The absence guard applies to exclusions exactly as it does to removals:
    // exclude means "do not PULL THIS IN as a sibling", and `uninstall` names it
    // as the way to drop one program while staying adopted. A member the user has
    // since explicitly installed is present, so the ordinary update path keeps it
    // current — an exclusion never freezes or drops what is deliberately here.
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

/// Apply ONE coherence group as an all-or-nothing transaction (the per-group body factored
/// out of [`apply_channel`], shared with the transactional [`apply_program`] update path).
/// `None` ⇒ the group has no installed member and was skipped (that would be a fresh
/// `install`, not an update). Otherwise `Some(outcome)`.
///
/// [`crate::gate::decide`] is the security authority, evaluated first. The consumer gates
/// run strictly after it and can only suppress or abort, never move a build:
/// * **local pin** — if any member is pinned AND the group is not tombstoning AND it wants an
///   upgrade, the whole tuple is held on its current builds ([`TxnOutcome::Pinned`]); a
///   Tombstone anywhere makes the pin IGNORED so a revoked build never keeps running;
/// * **missing-triple hold** — if a member the group would move has a new pin proven (from
///   its verified manifest, bound to that pin) to publish no artifact for this triple, the
///   whole tuple is held on its current builds ([`TxnOutcome::Unpublished`]), unless a
///   current build is revoked, which aborts with that build's commands disabled; a
///   fetch/verify failure proves nothing and defers to the real stage;
/// * **disk preflight** — a group-aggregated shortfall aborts the group before staging so it
///   stays coherent on its current builds; fails OPEN on any query/fetch failure.
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
    // `update` touches INSTALLED groups only: skip a group with no installed member (that
    // would be a fresh `install`, not an update). A coherence group with even ONE member
    // installed IS processed in full — the locked tuple must stay coherent, so a missing
    // sibling is pulled in to the pin (decide → Install).
    //
    // A TOMBSTONED MEMBER STILL COUNTS AS INSTALLED. `installed` reaches this lane from
    // [`crate::ops::active_builds`], which resolves `bin/<tool>` — and a TOMBSTONE (the
    // failing script §7 writes over a revoked build's shims) resolves to nothing, by
    // design. So a program whose commands this manager itself disabled is absent from that
    // map, and a TOMBSTONED SINGLETON — or a tuple every member of which was tombstoned —
    // was skipped here, whole and unreported. The tombstone says `was yanked/revoked — run
    // "aterm pkg update"`; the user ran exactly that; this returned `None`; the row stayed
    // `tombstoned` and every command kept failing. The self-heal `cmd_update_all_code`
    // documents (the shim view's silence makes `decide` return Install, and the transaction
    // re-stages the build `store/<p>/current` already names) was reachable ONLY when some
    // OTHER member of the same group was still installed — so the one shape the message is
    // written for, a yanked singleton whose pin has since been re-pointed at a good build,
    // was the one shape it never fixed. Only `aterm pkg update <name>` recovered it, and
    // set completion cannot stand in on a machine that installed by name and never adopted
    // the toolset.
    //
    // THE TEST IS THE TOMBSTONE, NOT THE WITNESS. A member counts here only when a shim
    // this manager wrote is still SITTING on `PATH` and refusing to run
    // ([`tombstoned_in_place`]) — never merely because the prefix still has a
    // store witness for it. A shim that is simply GONE is a different machine state (an
    // interrupted uninstall, a hand-removed file, a lane that only ever laid some names),
    // and reviving it here would make the update lane reinstall a program nobody asked it
    // to.
    //
    // `installed` ITSELF, the map handed to `decide`, is untouched here: widening it would answer
    // UpToDate forever for a tombstoned program whose pin had not moved, re-point shims into a
    // revoked build on rollback, and revive the set on a declined machine. Only the question "is
    // this group on this machine at all?" reads the tombstone; `decide` still sees `None` for that
    // member and still returns Install — the re-stage the tombstone promises ([`Staged::was_live`]
    // exists for exactly this member) — and never `UpToDate`.
    //
    // Nor can it revive a removal: `ops::uninstall` deletes the shims, the store tree and
    // the links, so there is no tombstone and no witness to find, and the
    // `removed`/`declined`/`excluded` filter below still runs AFTER this and returns `None`
    // for a group left with no member. The scan runs only when the shim view was silent for
    // every member — the path that was about to skip the group anyway — so the ordinary
    // six-hourly tick pays nothing for it.
    if group.members.iter().all(|m| !installed.contains_key(m))
        && !tombstoned_in_place(layout, group.members.iter().map(String::as_str))
    {
        return None;
    }
    // A DELIBERATELY REMOVED MEMBER IS EXCLUDED FROM THE TUPLE — AND NOTHING ELSE.
    //
    // `aterm pkg uninstall trust` frees ~3.2 GB, records the removal durably, and says
    // this machine no longer auto-completes the toolset. The coherence rule — a group
    // with any installed member is applied whole, missing siblings pulled in — would
    // otherwise put it straight back on the next six-hourly tick.
    //
    // The hold must not freeze the members that ARE here: a publisher ships a fix as a
    // new pin without yanking the old one, and a hold that skipped the ordinary tick
    // would withhold it silently on every machine that ever uninstalled a member.
    //
    // So the hold does ONE thing: drop the members that are recorded-removed AND
    // ABSENT, then run the ordinary transaction on what remains. Revocation, routine
    // upgrades, the pin gate, tombstoning and reporting all take their normal path —
    // there is no second implementation of them here to get wrong. The trigger also
    // requires actual ABSENCE: a stale record for a member that a signed `requires`
    // pull-in has since reinstalled is not a reason to hold anything. Declined machines and
    // `[packages].exclude` join the same test ([`deliberately_absent`]).
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
    // Nothing left to act on. `transact` reports `UpToDate` for an empty decision
    // set, which would print the update lane's most reassuring line over a group
    // that was not looked at.
    if acted.members.is_empty() {
        return None;
    }
    // REPORT THE GROUP THAT WAS ACTED ON, not the nominal one. The caller wrote
    // per-program status rows from the group it was handed, so a filtered tuple
    // produced an `active` row for the very program the user deleted — a phantom
    // installation in the surface the user is sent to check.
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

/// §11 bootstrap: apply ONE coherence group all-or-nothing against a caller-resolved,
/// ALREADY-verified index — the fresh-install twin of [`apply_channel`]'s per-group body,
/// for the default-set bootstrap. Transaction semantics are IDENTICAL to the update path
/// (decide-first, local-pin hold, group-aggregated disk preflight, stage-all → flip-all →
/// rollback, abort discard) with ONE difference: the update-only "at least one member
/// installed" guard is dropped, because a bootstrap group is typically absent entirely.
/// The caller resolves + verifies the index ONCE for its whole pass and hands it in, so a
/// mid-pass index publish can never split a fresh tuple across two index states (§7) —
/// the hole a per-member `install` loop (which re-resolves each call) cannot close.
///
/// `resolved_assets` collects `program → pinned asset file name` for every member the
/// transaction resolved far enough to select an artifact — a member whose download then
/// FAILED included, which is the point: the caller's pass-end
/// [`crate::gc::run_keeping_pinned_partials`] spares that member's `.part` resume state.
/// An out-param (not part of the return) because it must survive every failure shape.
///
/// `flip` is the caller's flip gate, as for [`apply_channel_gated`]. A tuple with nothing
/// installed supersedes no toolchain and never waits, but one completed with `trust` live
/// and a sibling absent moves the live compiler, and under the unattended gate that move
/// waits for quiet exactly as the update lane's does.
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
    // The per-target view, exactly as the caller's plan and [`group_missing_triple`]
    // prescan saw it — staging the raw pin would fetch another target's build.
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

/// §11 bootstrap prescan: `Some(member)` iff one of `members` has NO artifact for `triple`
/// in its pinned, release-verified pkg manifest — the caller then skips the WHOLE group
/// cleanly, lifting the singleton `NoArtifact` skip doctrine to coherence groups (a tuple
/// that cannot fully exist on this host is a correct state the 6h loop must not scream
/// about, not a failure). `None` ⇒ no missing triple was PROVEN: any fetch/verify/parse
/// failure defers to the real stage, which fails the transaction loudly. Verify-before-
/// parse is the shared [`verified_pkg`] sequence.
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
/// [`bootstrap_group`] (§11 fresh install): decide-first, the retired-`requires` refusal
/// ([`FlowError::RequiresRetired`], §5.3(b)), the local-pin hold, one walk over the Install
/// members' manifests feeding the missing-triple hold ([`TxnOutcome::Unpublished`], for a
/// tuple with something installed) and the group-aggregated disk preflight, then
/// stage-all → flip-all → rollback via [`transact`], with the abort discard,
/// tombstone-shim disable, and applied `tree_root` capture.
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
    // THE DEFERRAL'S RECORD ENDS with every outcome that is not another wait or a failed
    // attempt at the flip: the group moved, was held for another reason (a pin, an
    // unpublished triple), was tombstoned, or had nothing to move. An abort keeps it — the
    // next pass tries the same flip, and the ceiling still counts from the first wait.
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
/// [`decide`] on the COMPLETENESS-aware view ([`installed_for_decide`]), and the two gates
/// that read nothing else, the retired-`requires` refusal and the local-pin hold. The
/// default set's disk check reads the same decisions ([`planned_landing`]), so the members
/// it sums for a tuple are the ones the tuple's own disk gate sums.
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
        // Per member, from the COMPLETENESS-aware view ([`installed_for_decide`]): on the raw
        // shim view a member whose `<build>.ready` is missing or the other slice's is UpToDate
        // forever, so the tuple holds a tree that cannot run here and nothing re-stages it.
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
        // An UPGRADE MOVES the tuple to a different build. A member that is `Install` only
        // because its tree is not complete for this slice is re-staged AT the build the pin
        // already names — a repair, and not what a local pin may suppress: holding it would
        // freeze the tuple on a tree that cannot run here, which is not what a pin means. On
        // the shim view alone the two predicates coincide (`decide` returns UpToDate exactly
        // when the installed build equals the pin), so this is the old test plus the case the
        // completeness input introduced.
        let wants_upgrade = decisions
            .iter()
            .any(|(m, d)| *d == ApplyDecision::Install && installed.get(m) != ch.pin.get(m));
        // Whether every installed member's CURRENT build is itself still gate-valid — the
        // guard BOTH holds must pass. If any current build is yanked or below the floor,
        // decide() returned Install to force-upgrade OFF it (not Tombstone), and a hold
        // would keep a revoked/below-floor build running; revocation outranks every
        // consumer gate, so the tuple force-upgrades to the valid pin instead.
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

    /// The members decide() marked Install — exactly the members `transact` would stage, in
    /// member order.
    fn install_members(&self) -> Vec<&String> {
        self.decisions
            .iter()
            .filter(|(_, d)| *d == ApplyDecision::Install)
            .map(|(n, _)| n)
            .collect()
    }

    /// THE RETIRED `requires` (§5.3(b)): the first member whose signed row names
    /// the relation this client no longer honours, when the group has something to install.
    /// Never over a Tombstone (transact tombstones the group — a revoked build never keeps
    /// running) and never over a force-upgrade off a revoked current build, exactly like the
    /// pin ([`Self::pin_hold`]).
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

    /// The LOCAL PIN GATE: the pinned members, when one pinned member freezes the WHOLE tuple
    /// on its current builds ([`TxnOutcome::Pinned`]) — never splits it. Only for a tuple that
    /// wants an upgrade, and never over a Tombstone: if ANY member is tombstoned the pin is
    /// IGNORED (transact tombstones the group — a revoked build never keeps running).
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
        // CRITICAL: a hold freezes EVERY member on its current build, so it is only safe when
        // every installed member's current build is itself still gate-valid
        // (`all_current_valid`) — honoring the pin otherwise would keep a revoked/below-floor
        // build running via a purely local pin. In that case the pin is IGNORED and the tuple
        // force-upgrades to the valid pin.
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

    // THE RETIRED `requires` — strictly AFTER decide() and in the body the update lane
    // shares with the bootstrap lane, so `apply_channel` and `apply_program` refuse it
    // exactly as `install` does: a group with something to install, one of whose members'
    // signed row names the relation, aborts before anything is resolved or downloaded, the
    // tuple staying on its current builds.
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

    // LOCAL PIN GATE — strictly AFTER decide(), suppression-only, coherence-preserving:
    // nothing is staged or flipped.
    if let Some(held) = decided.pin_hold(layout) {
        return (TxnOutcome::Pinned(held), BTreeMap::new());
    }
    let decisions = &decided.decisions;
    let (any_tombstone, all_current_valid) = (decided.any_tombstone, decided.all_current_valid);

    // One walk over the manifests of the members decide() marked Install — exactly the
    // members `transact` would stage — feeds both gates below: the missing-triple hold and
    // the group-aggregated disk preflight. It makes the requests the disk preflight always
    // made, in the same order, so the hold costs no extra fetch. Only when nonempty, so an
    // UpToDate group walks nothing and is never aborted on a low-but-nonzero disk. Manifests
    // only; the asset download stays in `stage_member`.
    let install_members = decided.install_members();
    let need = if install_members.is_empty() {
        InstallNeed::Unknown
    } else {
        group_install_need(fetcher, index, ch, &install_members, triple)
    };

    // The missing-triple hold: the bootstrap's clean-skip doctrine ([`group_missing_triple`]
    // — a tuple that cannot fully exist on this host is a correct state, not a failure)
    // lifted to an installed tuple. Without it, a channel that moved a group to a pin the
    // publisher has not built for this triple sent the update lane into `stage_member`, whose
    // `artifact_for(triple)` miss aborted the group on every six-hourly pass while the tuple
    // kept running its previous builds.
    //
    // Strictly after decide(), suppression-only, and proof-only: the walk must show the
    // missing row in the verified manifest of a member this pass would stage, bound to that
    // member's pinned program and build. An UpToDate member is never read, and a
    // fetch/verify failure or an unbound manifest proves nothing, so the real stage still
    // runs and fails loudly. Only a group with something installed has builds to stay on, so
    // the fresh install is untouched. Never over a Tombstone, and never a quiet hold over a
    // force-upgrade off a revoked current build — that would keep the revoked build running
    // with no replacement, so its commands are disabled and the group aborts loudly.
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
        // The abort names the program it disabled, not the unpublished member, because that
        // row has to outlive this pass. The tombstone shims drop the disabled program from
        // `active_builds`, so the next pass sees it as not installed and the tuple takes the
        // quiet hold above, which rewrites only installed members' rows — leaving the
        // disabled program's row reading `managed`, the silent tombstone doctor exists to
        // catch. On the disabled program, the row stands until an apply replaces the
        // tombstone.
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
        // Stage NOTHING — the group stays coherent on its current builds. But
        // "coherent" must not mean "the revoked build keeps working": `decide` returns
        // Install to FORCE-UPGRADE off a yanked or below-floor build, so an abort here
        // leaves exactly that build runnable, on exactly the machine this path is most
        // likely to meet — the one whose owner uninstalled a multi-gigabyte sibling to
        // reclaim space. The upgrade is what could not be afforded; disabling the
        // revoked build costs nothing and is the half of the decision that must still
        // happen.
        let disabled = disable_revoked_currents(layout, ch, decisions, installed);
        // SAY IT. Disabling a program's tools is the loudest thing this pass can do to a
        // machine, and the abort below says the group "stays coherent on its previous
        // builds" — the opposite of what just happened to these members.
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
        // Stage NOTHING — the group stays coherent on its current builds.
        return (
            TxnOutcome::Aborted {
                failed: install_members[0].clone(),
                during_flip: false,
                why: short.to_string(),
            },
            BTreeMap::new(),
        );
    }

    // FLIP WHEN QUIET ([`crate::quiet`], gap #12(b)) — strictly after decide() and every
    // consumer gate above, suppression-only like them. An unattended pass whose group holds
    // the Trust toolchain asks, before it extracts anything, whether the toolchain it would
    // supersede is in use: leased by a run, run from, or not known. In use, and short of the
    // ceiling, the group STAGES — every member it would move has its signed archive in
    // `staging/`, digest-checked, so the flip needs no network — records the wait, and
    // flips nothing. A revoked build never waits (`decide` flips a forced group at once),
    // and a person's pass never asks ([`crate::quiet::FlipPolicy::Now`]).
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
                    // A fresh download is checked against its signed digest NOW, so what
                    // waits in `staging/` is a verified archive; a carried one already was.
                    // A mismatch is the stage's own failure, recorded the stage's way — the
                    // refusal memo that keeps the next passes off the wire
                    // ([`digest_refusal_note`]), the archive and its partial reclaimed. The
                    // digest alone, as the stage checks it: [`carried_archive`]'s one-link
                    // rule is about reusing a file, and refused a `dir:` registry's fresh
                    // download (a hard link to its archive) as a mismatch of equal digests.
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
    // member's archive in them, and the last look reads them back into the record of a wait
    // it holds (the archives that wait needs spared, [`crate::quiet::held_archives`]).
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
        // THE LAST LOOK, between the last extract and the first flip: a build that started
        // while a multi-gigabyte tree was being extracted holds the flip after all. Not
        // once the ceiling has spoken.
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
            // The flip is the group lane's link phase (label-only, per the schema).
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
    // On abort, DISCARD the builds this transaction staged. They were never left active (a
    // stage-phase abort flipped nothing; a flip-phase abort re-pointed every shim back to the
    // prior build via rollback), so leaving a complete-but-inactive build on disk would make
    // list_installed/`decide` mis-read it as the active build next run — silently splitting
    // the coherence tuple and wedging the update while reporting success. Within THIS
    // process the staged `build_dir` is USUALLY the NEW pinned build — but not always, and
    // the earlier claim that it always is ("decide only stages Install members, so new !=
    // the prior active build") was WRONG: `decide` reads the SHIM-derived `installed` view
    // ([`crate::ops::active_builds`]), which goes SILENT when a program's tools are all
    // tombstoned or `atpkg unlink` removed its dev shims, so it legitimately returns Install
    // for the build `store/<program>/current` already names. `Staged::was_live` catches
    // exactly that member below. Across processes the discard rests on the
    // SINGLE-WRITER-PER-STORE contract ([`crate::lock`]): every mutating verb try-acquires
    // the store-wide `store.lock` at the CLI edge, so no OTHER atpkg process can be staging
    // or activating builds in this store while this transaction runs — without that lock, a
    // concurrent process could have just activated one of these very builds, and this
    // discard would leave its shims dangling on a deleted tree.
    // A HELD FLIP ([`TxnOutcome::Deferred`] from the last look) discards the same way: its
    // trees were never flipped, and a complete inactive build above the live one is what
    // this discard exists to prevent. The archives stay (below), so the flip that follows
    // re-stages without a download.
    if matches!(
        outcome,
        TxnOutcome::Aborted { .. } | TxnOutcome::Deferred { .. }
    ) {
        for s in staged.borrow().values() {
            // NEVER delete a build that was already LIVE when this transaction re-staged it.
            // [`crate::gc::live_builds`] calls exactly that build live and protects it;
            // deleting it here leaves both `current` links dangling and forces a full
            // re-download of a multi-GB toolchain — triggered by the very network failure
            // that aborted the group and that makes re-downloading impossible.
            if s.was_live {
                continue;
            }
            crate::store::discard_build(&s.build_dir);
        }
    } else {
        // …but NOT their downloads. The compressed archive of every member that staged is
        // reclaimed HERE, once the tuple's fate is known, instead of at the end of each
        // member's own stage ([`carried_archive`], `stage_member`):
        //
        //   - settled (applied/tombstoned/up-to-date): the group is done with these bytes
        //     and nobody ever comes back for them, so they go exactly as they always did;
        //   - ABORTED: they are KEPT. The next pass re-stages every member of this tuple,
        //     and each kept archive is one multi-gigabyte download that pass does not
        //     repeat. The abort above already took the extracted trees, which is the
        //     invariant that matters (an inactive complete build would split the tuple);
        //     an archive is inert — nothing resolves, executes or `list_installed`s into
        //     `staging/`.
        //
        // The strandage bound is the `.part` bound, by the same argument: the pass-end
        // `gc::run_keeping_pinned_partials` spares `<asset>` beside `<asset>.part` for
        // exactly the programs this pass resolved (`ChannelApplyReport::resolved_assets`),
        // so at most one file of each name per program survives a pass; a plain `atpkg gc`
        // reclaims them whenever the user asks; and all of this runs under the store-wide
        // writer lock. A member that FAILED already reclaimed its own archive inside
        // `stage_member`, so this loop can only ever remove — it never resurrects a bad
        // asset.
        for program in staged.borrow().keys() {
            if let Some(asset) = resolved_assets.get(program)
                && let Ok(dl) = staged_download_path(layout, program, asset)
            {
                let _ = std::fs::remove_file(&dl);
            }
        }
    }
    // A tombstoned member's OLD working shims must be actively DISABLED, not just reported
    // (§7). `transact` returns Tombstoned when decide() tombstoned any member; install a
    // failing tombstone shim over each such program's currently-exposed tools so a revoked
    // build is not left runnable. Best-effort — the report is still the source of truth.
    if let TxnOutcome::Tombstoned(members) = &outcome {
        for m in members {
            install_tombstone_shims(layout, m, installed.get(m).copied());
        }
    }
    // Capture the SIGNED tree_root of each LIVE-flipped member so the CLI can record it for
    // `atpkg verify` (the `staged` map still holds this group's entries).
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
/// built only when the gate applies — an unattended pass ([`crate::quiet::FlipPolicy::
/// WhenQuiet`]) moving the Trust compiler itself off a live build.
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
        // The COMPILER must be one of the members that move: a pass that moves only a
        // sibling leaves every build in flight on the trustc it started with. And it must
        // have a LIVE build to move off: a trust the tuple re-stages
        // from a tombstone or pulls back in supersedes no compiler anything could be
        // running, and holding it kept a disabled toolchain disabled behind a SIBLING's
        // process for up to four hours.
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
    /// resolved it (`resolved`, member → asset file name) — the archives every gc spares
    /// while the wait stands ([`crate::quiet::held_archives`]).
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

/// Is `dl` a COMPLETE, already-verified copy of `artifact`'s compressed asset — an
/// archive an earlier pass downloaded and this one may stage without refetching?
///
/// # What this bounds
///
/// A coherence group aborts all-or-nothing, and the abort DISCARDS every staged sibling's
/// extracted tree (`apply_group_txn`'s abort discard — a complete-but-inactive build would
/// make `list_installed`/`decide` mis-read it as the active build and silently split the
/// tuple). That invariant is untouched. What must NOT go with the tree is the DOWNLOAD:
/// before this gate, a 4-member tuple that failed on member 3 threw away members 1-2 and
/// refetched them from byte 0 on the next tick, so the resume-across-passes guarantee (R4)
/// covered only the member that actually failed, and a multi-gigabyte tuple on a flaky
/// link could never converge. The archive of a member that staged is therefore kept until
/// the tuple's fate is known, and this is what the next pass admits it back through.
///
/// # Trust posture
///
/// A REUSE gate, never a verification shortcut — it can only ever SKIP A DOWNLOAD, never
/// admit bytes. The file is accepted only when its digest equals the SIGNED one
/// ([`crate::install::check_signed_digest`]), so the bytes it stands for are by
/// construction the ones the fetcher would have produced; `verify_and_stage` then
/// re-checks that same digest AND the signed `tree_root` after extraction, exactly as it
/// does for a freshly downloaded archive. A row with no signed digest (none to match) is
/// refused, and so is anything that is not a regular file. A file that fails is not
/// reused: the caller removes it and fetches, which is the old behaviour unchanged.
///
/// The digest is the whole gate — deliberately NOT pre-filtered on the signed `size`,
/// which is the download CAP and not a promise about a file already on disk. Reading the
/// file to hash it is the only cost, it is paid only when an archive is actually sitting
/// there (a settled tuple leaves none), and it buys a multi-gigabyte transfer.
fn carried_archive(dl: &Path, artifact: &crate::manifest::Artifact) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dl) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // A hard link into a `dir:` registry (its stale staging entry) is never carried: it
    // is the registry's own inode, and reading it as the download would be right while
    // any later write through the name would be into the registry — one
    // link, one owner, or it is fetched afresh.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() != 1 {
            return false;
        }
    }
    crate::install::check_signed_digest(artifact, dl).is_ok()
}

/// Disable the live commands of every member `decide` asked to force off its current build
/// — an Install whose current build is yanked or below the floor — by laying failing
/// tombstone shims over them, and return their names. An abort that leaves the tuple on its
/// current builds (a disk shortfall, a replacement not published for this triple) must not
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

/// The line the missing-triple hold prints for each `program` it disables: the build this
/// `triple` lacks — `member`'s pinned `build`, the tuple's new pin — and that the program's
/// commands stay disabled until it publishes. The two names differ whenever a sibling's new
/// pin is the missing one: the recalled program's own replacement may be published.
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

/// Reclaim every `*.part` in `dl`'s directory EXCEPT `dl`'s own.
///
/// A resumable artifact download keeps its partial across attempts, so the partial for
/// the asset we are about to fetch must survive; one for an asset this program has moved
/// past (a superseded build, a renamed artifact) never will be finished by anyone. The
/// bound this establishes is "at most one stranded partial per program between passes",
/// instead of one per abandoned build — `gc`'s `staging/` sweep is the other reclaimer,
/// but it only runs when the user runs it.
///
/// Deliberately narrow: only regular files whose name ends in `.part`, only in `dl`'s own
/// directory, never recursive. `dl` itself and any other file are untouched.
/// Reclaim `dl`'s own sibling `<asset>.part`, if one exists.
///
/// The belt-and-braces arm of the digest gate: an asset that just FAILED its signed
/// sha256 must not leave a partial behind to seed the next attempt — a poisoned prefix
/// would cost that attempt a full download plus a second guaranteed mismatch. Nearly
/// vacuous on the resumable lane (the fetcher renamed the `.part` onto the asset before
/// verify ran, so a mismatch-failing download usually has no sibling partial) — this is
/// a cheap invariant, not a load-bearing gate; the load-bearing gate is the sha256
/// check itself, which is untouched.
fn discard_sibling_partial(dl: &Path) {
    let Some(name) = dl.file_name() else {
        return;
    };
    let mut part = name.to_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(dl.with_file_name(part));
}

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
        // A `.part` of another asset, or — since archives of a failed stage are
        // RETAINED for reuse ([`carried_archive`]) — a full archive of another asset
        // name (a vendor-direct download is `<program>-<version>-<triple>[.ext]`): at most
        // one of each survives per program, and it is this pass's.
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

/// What a FAILED stage leaves in `staging/`: nothing for a digest failure (the bytes
/// are wrong — the archive and any `.part` that would seed the next attempt go, see
/// [`discard_sibling_partial`]) or a signer refusal (the bytes are refused, and the memo
/// keeps them from being fetched again); the verified archive, kept in place, for every
/// other failure, so the next attempt stages it again without the download
/// ([`carried_archive`]). One archive per program at most survives —
/// [`sweep_foreign_partials`] removes the others — so a member that keeps failing keeps
/// one copy, never one per attempt.
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
/// the same one stops paying to be told so ([`crate::store::StageRefusal`]).
///
/// THREE failures are recorded, differently, because they are evidence of different
/// things:
///
/// * the signed-digest mismatch COOLS DOWN. Its retry costs a TRANSFER — it is the
///   failure that (rightly) deletes the archive and its sibling partial: the bytes are
///   wrong, and a poisoned prefix must never seed the next attempt, so the next attempt
///   starts from byte 0. But one such verdict can be a truncated transfer or a proxy, so
///   the first retry is free and only identical verdicts lengthen the cooldown;
/// * a signer refusal ([`StageError::SignerRefused`]) is recorded as a signer memo that
///   binds until the digests move: its bytes MATCHED them, so a re-download can only
///   fetch the same refusal, and its archive is deleted with the verdict;
/// * the `tree_root` mismatch is HELD. Its archive passed the signed digest and is KEPT
///   ([`carried_archive`], pinned by
///   `a_failed_stage_retains_one_verified_archive_and_a_digest_failure_none`), so no
///   transfer is at stake — but the re-stage is deterministic over the same bytes and
///   the same row, and every pass paid it regardless — a failed pass is re-run on the
///   window's backoff (10 minutes doubling to 2 hours), not after six hours: 41
///   unattended re-stages on one Windows box between 09-14 and 09-22, every one the
///   same verdict,
///   and a `claude` stub telling every shell the last attempt FAILED. A held memo binds
///   until the pin's signed digests move, a newer atpkg asks, or a person does
///   (`aterm pkg install <p>`, `aterm pkg update --retry`) — and the kept archive makes
///   that retry download nothing.
///
/// Every other failure is a fact about THIS MACHINE (a full disk, an unreadable store, a
/// refused installer lane), keeps its verified archive for the same reason, and retries
/// freely.
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
    // Best-effort: a store this process cannot write is a machine fault, and the pass
    // that could not record the refusal still reports the stage failure itself.
    let _ = record(
        build_dir,
        digest,
        &artifact.tree_root,
        &e.to_string(),
        now_unix(),
    );
}

/// The row a still-binding refusal ([`record_digest_refusal`]) owes this pass — the
/// canonical `held: last attempt failed with <why>; <how to retry>` state
/// ([`crate::state::held_failed`]), which is what the pass log prints, the status record
/// keeps, and `which`/`doctor`/the pending stub repeat — or `None` when there is nothing
/// recorded, the pin's signed digests have moved, another atpkg wrote the memo, or a
/// cooldown has lapsed — in which case the caller downloads and stages exactly as it
/// always did.
///
/// The WALL clock, deliberately, and not the `now_unix` the freshness gates take: this is
/// a bandwidth cooldown (or a hold on a deterministic verdict), never a trust decision,
/// and it is the only thing in this file a memo can influence. [`now_unix`] fails closed
/// to `i64::MAX`, which makes every cooled-down memo read as lapsed — an unreadable clock
/// can therefore only cost a download, never block an install (a signer or held memo
/// does not read the clock at all: its verdict does not age).
///
/// The door the row names is the caller's. The program lane ([`land_artifact`]) names
/// `aterm pkg install <p>`, which forgets the memo and lands that program. The group lane
/// (`grouped`, [`fetch_member`]) names `aterm pkg update --retry`, which forgets every memo
/// and runs the pass that moves or completes the tuple whole: `install <p>` would land that
/// one member at its new pin beside siblings on the old one, and `update --retry <p>`
/// sends a member that is not installed to the same program lane.
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
    // Built by hand rather than wrapped into one `format!` string: the sentence is long,
    // and a continuation inside a string literal is how it grows a run of spaces nobody
    // sees until it is printed at an operator.
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
/// [`crate::cost::disk_ok`] against `required` + the [`crate::cost::FREE_FLOOR`]. `None`
/// available (the query failed) fails OPEN — preflight is a safety net, not a security gate.
/// `available` is injected so the gate is unit-testable without a real `statvfs`.
fn disk_gate(required: u64, available: Option<u64>) -> Result<(), FlowError> {
    match available {
        Some(avail) if !crate::cost::disk_ok(required, avail, crate::cost::FREE_FLOOR) => {
            // The need shown is the whole need: the install's bytes plus the reserve the
            // check holds back, so "needs X, Y free here" never reads Y > X.
            Err(FlowError::InsufficientDisk {
                required: required.saturating_add(crate::cost::FREE_FLOOR),
                available: avail,
            })
        }
        _ => Ok(()), // None (query failed) => fail OPEN
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
/// artifact, or a coherence tuple's Install members together, whose archives all stay
/// until the transaction knows the tuple's fate ([`stage_fetched`]). The figure every disk
/// gate checks — [`land_artifact`] for a program, [`group_install_need`] for a tuple — and
/// the one the default set's up-front refusal (`cli::set_disk_refusal`) is computed from.
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

/// What a pass that lands `units` one after another needs free, before the reserve, in
/// whatever order it takes them: every tree stays, and a unit's archives are reclaimed once
/// it has staged, so the peak is every tree plus the largest unit's archives — what its own
/// gate sees when it lands last. A volume with that much free passes each unit's gate in
/// every order the priority queue can choose.
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
    /// The pass lands nothing of the unit, so the unit must not decide whether the rest
    /// fits. Its lane stages nothing for a revoked pin (the unit is tombstoned), a signed
    /// row naming the retired `requires`, a local pin holding the tuple
    /// ([`TxnOutcome::Pinned`]), a unit with nothing to install, or a pin proven unpublished
    /// for the triple. A member whose digest-refusal memo binds ([`digest_refusal_note`])
    /// is skipped by the program lane, and the group lane aborts the tuple at it — after
    /// staging the members ahead of it, whose trees the abort discards and whose archives
    /// it keeps for the next pass. The set's figure leaves those bytes out; the tuple's own
    /// disk gate, which counts every Install member, runs before any of them is fetched.
    Nothing,
    /// The bytes its landing needs: what its own disk gate checks.
    Bytes(LandingBytes),
    /// No answer: a manifest or its row for the triple is unavailable, or a row declares no
    /// installed size.
    Unknown,
}

/// One install unit of the default set as its up-front disk check reads it: whether its
/// lane lands anything this pass and, if so, the bytes its own disk gate checks. `group` is
/// the unit as the pass hands it to its lane. An ungrouped program is read as [`install`]
/// reads an absent program (the set installs it with no installed build, so `installed` is
/// not consulted). A coherence tuple is read through the decisions and the manifest walk
/// [`apply_group_txn`] runs on the same `installed` view ([`GroupDecisions`],
/// [`group_install_need`]), so its figure counts every member the transaction would stage —
/// an installed member whose tree is not complete for this slice included — and no other.
/// The verified manifests are the ones the install fetches; the fetcher memoises them.
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
        // The program's `NoArtifact` skip; for a tuple, the transaction's missing-triple hold
        // or the bootstrap's clean skip ahead of it.
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

/// Each of `install_members`' pins and the bytes they land together ([`LandingBytes`]:
/// each member's signed `size` (compressed asset) + `disk_installed` (extracted tree)) —
/// walked in order. Verify before parse, through the shared [`verified_pkg`] sequence. Stops at the first
/// member whose manifest fails to fetch, verify or parse ([`InstallNeed::Unknown`]) or has
/// no artifact for `triple`: that is [`InstallNeed::Unpublished`] only when the manifest
/// binds to the member's pinned program and build, and [`InstallNeed::Unknown`] when it does
/// not, because another program's or build's manifest says nothing about this pin.
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

/// Resolve + verify-select the SIGNED index (cached-fallback, §14) and enforce
/// its freshness window (§8) — the shared prologue of every flow entry point,
/// exposed so the CLI's default-set bootstrap (§11) can read the verified
/// program set itself (`Index::installable`) without re-implementing the gate
/// order. Verify-before-parse and the floor are IDENTICAL to `install`/
/// `apply_channel`; this returns the index and installs nothing.
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
        // The prefix, which the cache write made before selection when it came first: the
        // roster ratchet and the yank latch write into it.
        let _ = layout.ensure_dir(&layout.prefix);
        let selected =
            verify_select_fresh_from(layout, anchor, candidates.clone(), floor, now_unix, live);
        // A SERVED SET THAT SELECTS NOTHING IS A FETCH THAT FOUND NOTHING, as an empty one
        // is — a run found again below the floor while the floor's own release is away
        // (`net::GithubFetcher::walk_candidates`), or bytes no chain admits — WHILE THE
        // CACHE STILL SERVES. It is weighed BESIDE what was served, so a newer generation
        // the source carries still drops it (`select::select_index`'s pass 2); when it
        // serves, the resolve says it ran on the cache, and the cache becomes what the
        // source served plus the cached candidate that won, fresh or lapsed (`Stale`, which
        // the cache gates the same way). The served releases are what
        // `doctor` reads back as downloaded and not accepted: a refused run lies wholly
        // ABOVE the floor once it is longer than the walk fetches (`INDEX_CANDIDATE_CAP`).
        // Offline the pair weighs to the same winner, every served candidate having been
        // refused on its own.
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
        // Otherwise the source's own verdict stands — selected (fresh, or lapsed: `Stale`,
        // which the cache gates the same way) or refused with nothing left to serve — and
        // what it served is the cache: the candidates `doctor` reads back as the last pass
        // that reached the channel downloaded, accepted or not.
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

/// The verify-select + freshness half of [`resolve_verified_index`], over candidates
/// fetched live for the lanes a TYPED single-program verb takes ([`rollback`],
/// [`apply_program`], [`plan_update`]), with NO §14 cached fallback — a fetch that could
/// not reach the source is [`FlowError::Unreachable`] carrying the transport's reason (the
/// same verdict the pass lanes give, and the one the CLI edge answers with a re-run line),
/// never an install decided from a cache the network could not corroborate this pass. A
/// source that WAS reached and listed nothing falls through to verification as an empty
/// set, which is [`FlowError::NoIndex`]. It neither refreshes the cache nor records what
/// the resolve learned about the network: only the pass lanes stamp `status.toml` from
/// [`last_resolve`].
/// Freshness (§8 gate 2): refuse a selected index whose window has lapsed; a
/// `valid_until` we cannot parse is treated as lapsed (fail closed).
///
/// # The roster ratchet turns HERE, on observation
///
/// The durable `roster_seq` high-water advances to the newest generation this pass ADMITTED,
/// before the freshness gate and before any caller decides whether to install. That is the
/// only ordering under which the replay defence works, and it is the one `aterm-update`'s
/// sibling tier already uses: a client that merely SAW generation *n* must refuse *n-1*
/// forever after, whether or not it went on to install anything. Ratcheting after a
/// completed install instead — which is what atpkg did — left every no-install outcome
/// (a local pin holding, a staging failure an attacker can induce, a plan that decided
/// there was nothing to do) with a floor BELOW a revocation it had already verified, and
/// the still-genuine pre-revocation roster then re-authorized the revoked machine.
///
/// Raising it here cannot lock the client out of a document it is still meant to use:
/// `observed_roster_seq` is only ever a generation that was master-signed, fresh, and
/// already at-or-above the current floor, and the ratchet refuses only STRICTLY older ones.
/// Best-effort, like every other floor write — a failed write leaves the older floor, which
/// is the direction that refuses nothing it should accept.
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
    // Every fresh verification carries its vendor yanks forward, so staleness can never
    // un-apply one (design §1.4); only one of bytes the source served LIVE stamps the
    // latch current, so a targeted vendor door knows whether to verify again.
    // Best-effort: an unwritten latch keeps the older yanks.
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

/// Durably record that this client has ADMITTED roster generation `seq` — the replay
/// ratchet's write half (`<prefix>/roster.floor`).
///
/// `0` means no generation was admitted at all (an unarmed anchor, a suppressed or
/// unverifiable roster), and writing it would be meaningless; the guard keeps a refused
/// pass from touching the file at all. Everything else is a master signature this process
/// checked itself.
pub(crate) fn observe_roster_generation(layout: &Layout, seq: u64) {
    if seq == 0 {
        return;
    }
    // The discarded Result is the accept/refuse DECISION (a Rollback here just means a
    // concurrent pass already recorded something newer — nothing to act on). A failure
    // to PERSIST the advance is not discarded: `check_and_record` reports it on stderr
    // itself, so a standing replay window is never silent.
    let _ = crate::sig::Floor::new(layout.roster_floor()).check_and_record(seq);
}

/// The §8 gate-2 freshness predicate over a verified index: whether its signed
/// `valid_until` window is still open at `now_unix`. A `valid_until` we cannot parse
/// is lapsed (fail closed).
///
pub(crate) fn index_is_fresh(index: &Index, now_unix: i64) -> bool {
    matches!(
        rfc3339_to_unix(&index.valid_until),
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok()
    )
}

/// Resolve the index candidates with a SAME-SOURCE cached fallback (§14): a successful
/// NON-EMPTY fetch is returned `served` (`true`), and [`resolve_verified_index`] refreshes
/// the cache with it (beside the cached index, when that is what serves); a fetch failure — or an
/// EMPTY success, which is a fetch that FOUND nothing (an index tag pushed off the release
/// listing, a repo with no index release) — falls back to the last cached
/// candidates FOR THE SAME SOURCE (a `dir:` cache never satisfies a failed `github:`
/// fetch). Both the write and the load are keyed by [`Fetcher::source_id`]. Cached bytes
/// are RAW — everything downstream (verify-then-select, freshness, floor) is unchanged, so
/// a tampered/stale cache installs nothing the live path wouldn't.
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
        // Reached the source, and it genuinely carried no index — a repo with no
        // index release. A trust-shaped answer is the right one here. When the cache
        // serves instead, the resolve ran on the cache, as it does beside a served set
        // that selects nothing (`resolve_verified_index`): not reached, so its bytes are
        // neither counted a live verification nor stamp the yank latch current.
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
        // Could NOT reach it. Keep the reason: telling an offline user their
        // signatures failed sends them to the wrong problem entirely. Whether a host
        // answered at all is kept too — a refusal is not an offline machine.
        Err(why) => {
            note_resolve(false, Some(why.clone()), !fetcher.index_link_down());
            cache
                .load(src)
                .map(|c| (c, false))
                .ok_or(FlowError::Unreachable(why))
        }
    }
}

/// What the last index resolve in this process learned about the NETWORK: whether the
/// source was reached, and if not why — so a pass that ran on the §14 cache can SAY so
/// instead of recording a green "up to date" while a managed `claude` freezes at an old
/// pin. The pass end reads this
/// ([`last_resolve`]) into its outcome, and a pass whose resolve did not reach the source
/// stamps a failure, never a success (`cli::record_pass_end`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveProvenance {
    /// The source served the candidates over the network this resolve (not the §14 cache).
    pub reached: bool,
    /// Why it did not, when it did not — the transport's own sentence.
    pub why: Option<String>,
    /// Whether a host ANSWERED at all — the source, or a refusal with a status (a rate
    /// limit, a 5xx, a portal's page). `false` only when the link itself failed
    /// ([`Fetcher::index_link_down`]): the one resolve an offline pass is made of.
    pub answered: bool,
}

// PER THREAD: a pass reads what ITS resolve measured, and a pass is one thread — so two
// passes in one process (the test suite's) can never read each other's network.
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

// PER THREAD, like the resolve's provenance: how many indexes this thread's resolves
// VERIFIED from what the channel served — not the §14 cache's bytes.
thread_local! {
    static LIVE_VERIFICATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// This thread's count of [`resolve_verified_index`] successes whose candidates the channel
/// served this resolve. A pass that reads it before and after itself knows whether it
/// REACHED the signed index — the one fact its success stamp may record
/// (`cli::finish_update_pass`, the derived model `AtpkgPassStamps`).
#[must_use]
pub(crate) fn live_verifications() -> u32 {
    LIVE_VERIFICATIONS.with(std::cell::Cell::get)
}

// PER THREAD, like the resolve's provenance: the members this pass lost AT A FETCH.
thread_local! {
    static FETCH_FAULTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Count one member that failed AT A FETCH — its manifest or its artifact did not arrive.
/// A pass that reached nothing is offline only when every failure it counts is one of these
/// (`cli::run_update_pass` reads [`fetch_faults`] before and after); a full disk, a refused
/// stage or a shim that could not be laid is a failure, whatever the network did.
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

/// Roll `program` back to the highest RETAINED build strictly below its current active build
/// that STILL passes the floor/yank gate (§9/§11). Re-points its shims + the channel
/// `current` to that build via the tested [`rollback_member`] primitive — creating/removing
/// ONLY symlinks, never mutating any retained build's extracted tree (its signed `tree_root`
/// is untouched). The index is resolved + verify-selected so the floor/yank state is
/// authoritative: the target predicate ([`rollback_lands_on`]) is EXACTLY the one [`decide`]
/// tombstones on (at/above THIS program's [`Channel::min_build_for`] AND not yanked), so a
/// rollback can never land below the floor or on a revoked build — it errors instead.
/// `excluded` is the caller's `[packages].exclude`, read only to report which pins the next
/// update honours.
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
    // 1. The ACTIVE build (shim-derived), never a merely-staged one.
    let current = *crate::ops::active_builds(layout)
        .get(program)
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "{program} is not installed \u{2014} nothing to roll back (aterm pkg list shows what is)"
            ))
        })?;
    // 2. Resolve + verify-select the SIGNED index so the floor/yank gate is authoritative
    //    (live — the single-program paths take no §14 cached fallback on failure).
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
    // 6. THE gate-valid selection: the highest build below current that a rollback may land
    //    on ([`rollback_lands_on`], the one predicate gc keeps its build below live by).
    let target = lower
        .iter()
        .rev()
        .copied()
        .find(|&b| rollback_lands_on(layout, Some(ch), program, b))
        .ok_or_else(|| {
            // What stays and why nothing older can run — the gate's name is not a fact a
            // person can act on. `lower` is sorted ascending, so its
            // last entry is the newest retained build below current, and it is the one
            // the arms describe.
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
        // Honest (this IS the live build we are rolling off) though never read here: only the
        // group transaction's abort discard consults `was_live`, and rollback stages nothing.
        was_live: true,
        reloc: None,
        tree_root: String::new(),
        // The SIGNED index just verified above is the policy, exactly as at install: an
        // ALab program keeps its aliases across the rollback, a vendor extra or a
        // system-satisfiable member grows none — whatever a hand-made `alab-*` link in
        // bin/ might suggest.
        aliases: Aliases::for_program(program, index.program(program)),
    };
    rollback_member(layout, program, &staged);
    // 8. What the next update does with it, read as that pass's pin gate reads its planned
    //    group ([`pin_gate_members`] — an uninstalled sibling's pin is never read): a
    //    member's revoked current build moves the whole group whatever is pinned
    //    (`all_current_valid`), and otherwise the members pinned here hold it.
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

/// The transactional `update <grouped-member>` path (§11 tuple-split fix): verify-select the
/// index + freshness like [`apply_channel`], but through `verify_select_fresh` — the
/// single-program paths take none of the §14 cache's failure-time fallback, so a transient
/// index-fetch failure is `Unreachable` rather than a cached index.
/// Then find the ONE coherence group containing
/// `program`, and apply THAT WHOLE group atomically via [`apply_group`]. A grouped member
/// therefore stages-all → flips-all → rolls-back atomically and can NEVER move
/// independently. A program the channel does not pin yields [`FlowError::NotPinned`].
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
    // The channel as THIS target sees it (`pin_by_target` laid over `pin`): every decide,
    // plan and fetch below reads this view, never the raw platform-agnostic pin.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    refuse_unrouted(layout, &index, &ch, program, triple)?;
    let groups = crate::apply::plan_groups_here(layout, &index, &ch, triple);
    let group = groups
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;
    // (Shell.d hook refresh runs at the main.rs CLI edge — see the note in `install`.)
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

/// Read-only routing decision for the `update` verb (§11): resolve + verify-select the index
/// (verify-before-parse), then return `program`'s coherence group (to pick the transactional-
/// vs-single path) AND the authoritative [`decide`] result (so an ungrouped pin gate can be
/// applied strictly AFTER it, never hiding a Tombstone).
///
/// Read-only as to the STORE; it takes `layout` because the roster ratchet turns on
/// observation ([`verify_select_fresh`]), and this verb is the clearest case for why: it
/// routinely admits a generation and then installs nothing at all (the local-pin hold), and
/// that outcome must still leave the client refusing every older generation afterwards.
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
    // Decide against the pin THIS target installs (`pin_by_target` overlay), or a build
    // `install` placed from the overlay reads as out of date against another target's pin.
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
        // Whether the CURRENTLY-installed build is still gate-valid — a local pin may
        // suppress the ungrouped update only when this is true (never keep a yanked/below-
        // floor build running). See [`crate::gate::current_build_ok`].
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

/// The verified per-build manifest for `program`'s channel pin: pin lookup → repo lookup →
/// fetch → [`TrustedIndex::verify_pkg`] → [`parse_pkg`], in that order (VERIFY-BEFORE-PARSE, §4.2).
/// Returns `(pinned build, repo, manifest)`; `None` on a missing pin/program or any
/// fetch/verify/parse failure. A caller that must bind the signed `program`/`build_number`
/// to its request (anti-replay) checks that on the returned manifest.
///
/// `pub(crate)` for the CLI's pre-install disclosure: the size a user is told before
/// committing gigabytes of disk is summed from these signed manifests'
/// `[cost].disk_installed` (`cli::seed_install_bytes`), so the number on screen comes
/// from the same verified bytes as the install itself rather than a separate estimate.
pub(crate) fn verified_pkg(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Option<(u64, String, crate::manifest::PkgManifest)> {
    verified_pkg_or_miss(fetcher, index, ch, program).ok()
}

/// [`verified_pkg`], saying on a miss whether the manifest was FETCHED (`Err(true)`: it
/// did not verify or parse, or the index names no pin) or not (`Err(false)`: the transfer
/// failed — a fetch fault, [`note_fetch_fault`]).
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

/// `program`'s SIGNED manifest for `build` — any build, not the pin — verified under the
/// index like every manifest read and bound to program and build; `None` on any
/// fetch/verify/parse miss. One manifest fetch (a few KB).
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

/// The version `program`'s SIGNED manifest states for `build`, and the `tree_root` it
/// states for `triple` when it states one ([`signed_pkg_of_build`]). The vendor lane names
/// a legacy index build by its version with it, and keeps the root, from the same one
/// fetch, for a rollback to that build.
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

/// The SIGNED asset size `program`'s pinned build ships for `triple`, or `None` on
/// any fetch/verify/parse miss. The live-progress plan uses it to fix the overall
/// bar's denominator honestly BEFORE bytes move (verify-before-parse preserved via
/// the shared [`verified_pkg`] sequence); a `None` degrades the plan to an
/// unmetered row, never a failure.
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

/// Recover the SIGNED `tree_root` for a build that is already installed.
///
/// The attestation is recorded when a member is flipped, and a pass that dies inside
/// the flip window leaves the member LIVE with no row: `atpkg list` shows it, its
/// shims work, and `atpkg verify` fails closed forever with "no signed tree_root
/// recorded; reinstall to enable verification" — while `seed` says "fully installed"
/// and `update` says "up to date", so nothing repairs it. That window is one power
/// loss during a first run.
///
/// This re-derives it from the same authority the install used — fetch, verify under
/// the index, bind program and build — so a recovered row is exactly the row the
/// original flip would have written, and never a locally computed guess. `None` on
/// any doubt: an unrecoverable root leaves the fail-closed state untouched.
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

/// The one line a failed group member owes the log: WHICH program, at WHICH build, and WHY
/// it could not be staged.
///
/// [`stage_member`] collapses a dozen distinct failures into `None` so [`transact`] can
/// abort the tuple, and the abort report (`cli::report_channel_apply`, the
/// `TxnOutcome::Aborted` arm) can then say only "ABORTED at <program> during stage — the
/// group stays coherent on its previous builds". That sentence names the member and the
/// phase and NOTHING ELSE, and it is the only surface there is: the store row is the
/// equally mute `aborted: stage`, and `TxnOutcome::Aborted` carries no reason field for
/// anyone to print: a signature mismatch, a 403, a full disk and a tar-slip refusal would
/// all read the same.
///
/// The cause IS known at the exits that throw it away: the fetcher hands back its own
/// words, and [`StageError`] renders sha256/tree_root mismatches and extract failures — the
/// very strings the SINGLETON lane already shows the user through [`FlowError::Download`]
/// and [`FlowError::Stage`]. Printed to stderr so it lands immediately above the abort line
/// in the same log, and deliberately NOT a failure of its own: reporting can never change
/// what the transaction does.
fn stage_failure_note(program: &str, build: u64, why: &str) -> String {
    format!("atpkg: {program} build {build} could not be staged: {why}")
}

/// Stage one group member: fetch + verify + parse its per-build manifest, bind program +
/// build, select the artifact (Shim kinds only — sysroot-bundle fails closed), download, and
/// `verify_and_stage` into its build dir. NO activation. `Some(Staged)` on success (with the
/// prior build captured for rollback); `None` on any failure so [`transact`] aborts the group.
/// Every exit that HOLDS a diagnosis says it first, through [`stage_failure_note`].
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
/// `why` filled, on any failure — the same exits, in the same order, as before the split.
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
    // The pass's verified answer for this member, recorded BEFORE the download so a
    // member that fails mid-transfer still reaches the report — the pass-end
    // `gc::run_keeping_pinned_partials` spares exactly these programs' `<asset>.part`
    // resume files, and the failed member is the one the sparing exists for.
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    // Same admission as `install`: a row that fails `check_row` aborts the group before
    // any byte moves.
    if let Err(e) = crate::vendor::check_row(artifact, &pkg.exposes) {
        *why = format!("the signed row for {program} was refused: {e}");
        return None;
    }
    // Shim, vendor-app and sysroot-bundle members stage on this path; the aterm
    // app-bundle and unknown pairs (the deleted OS-installer protocols' among them) fail
    // closed (return None → the group aborts), exactly like `install`.
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    let reloc = match strategy {
        crate::dispatch::ApplyStrategy::Shim | crate::dispatch::ApplyStrategy::VendorApp => None,
        crate::dispatch::ApplyStrategy::SysrootBundle => Some(artifact.reloc.clone()),
        crate::dispatch::ApplyStrategy::AppBundle | crate::dispatch::ApplyStrategy::Unknown => {
            // A kind/protocol pair this lane cannot stage is a PUBLISHING fact, not a
            // machine fault, and it aborts a whole tuple — so name it rather than leaving
            // the operator to guess at "ABORTED at <program> during stage".
            let note = format!(
                "this aterm cannot install this kind of package ({}/{})",
                artifact.kind, artifact.protocol
            );
            eprintln!("{}", stage_failure_note(program, pinned, &note));
            *why = note;
            return None;
        }
    };
    // The same digest-refusal memo the singleton lane consults, in the lane that meets
    // it on the fleet: a tuple whose biggest member is pinned to digests the published
    // asset does not match re-downloaded that member every six hours. The abort is the
    // same one the stage failure would have produced — minus the transfer — and it names
    // the tuple's door, not the member's.
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
    // THE CARRIED ARCHIVE ([`carried_archive`]). An earlier pass of this tuple can have
    // aborted on some OTHER member after this one downloaded, verified and staged: that
    // abort discarded this member's extracted TREE (it must — see `apply_group_txn`), but
    // the archive the tree came out of is the signed artifact's own bytes, and re-using it
    // is what bounds the retry to the members that actually failed. The same archive is
    // what THIS member's own failed stage leaves behind when the failure was not the
    // bytes' ([`reclaim_after_failed_stage`]) — otherwise a non-digest refusal
    // re-downloads the whole archive (743 MB for `trust`) at every launch and tick.
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
            // SAY WHY (see `stage_failure_note`). The transfer is the failure
            // this exit meets most often and `e` carries the fetcher's own words — HTTP
            // status, curl exit, the signed byte cap — which no later surface can
            // reconstruct, because the abort report only ever learns the member's NAME.
            eprintln!("{}", stage_failure_note(program, pinned, &e));
            // An aborted transfer leaves bytes in `<dl>.part`, not at `<dl>`: the production
            // fetcher promotes the part onto `dl` only on curl success, so `dl` is either
            // absent or complete. The part is deliberately LEFT for the next attempt to
            // continue from (`aterm_update_core::download_to_resumable`); `dl` itself is
            // reclaimed on this exit exactly as on the stage exit below, because a `dir:`
            // registry's copy lane writes there directly.
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
    // Capture "this build is ALREADY live" BEFORE the stage swaps a new tree into it, and
    // before any flip can move the links: on a flip-phase abort `flip_member`/`rollback_member`
    // re-point or REMOVE the very links this asks about, so by discard time the answer is
    // gone. `installed` is the shim view and can be silent for a live build (see the abort
    // discard in `apply_group_txn`), which is the whole reason this flag exists.
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    // Reclaim the compressed asset on every FAILING exit: a group member that fails to
    // stage otherwise strands its archive in `staging/` forever, and nothing else ever
    // sweeps that directory (`gc::interrupted_debris` walks `store/` only). The HAPPY exit
    // keeps it instead, and `apply_group_txn` reclaims it once the TUPLE's fate is known —
    // so a sibling's failure costs this member its tree but never its download.
    // The signed `shim_env` rides beside the build (design S7) so the flip — and a
    // rollback that has no manifest in hand — lays this build's shims with it.
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, &pkg.shim_env()) {
        // A store the process cannot write is the likeliest reading of this exit, and it
        // aborts the tuple exactly like a bad signature would — so the log must be able to
        // tell the two apart.
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
        // Reclaimed on THIS exit for a DIGEST failure only (the bytes are wrong — the
        // archive and any `.part` that would seed the next attempt go); every other
        // failure keeps the verified archive for the next attempt to stage without a
        // download ([`reclaim_after_failed_stage`]). A member that STAGED keeps
        // its archive until the transaction knows the whole tuple's fate.
        record_digest_refusal(&build_dir, artifact, &e);
        reclaim_after_failed_stage(&dl, &e);
        // SAY WHY (see `stage_failure_note`). `StageError`'s Display is the
        // same text the singleton lane already shows through `FlowError::Stage` — a
        // sha256 or tree_root mismatch with both digests, a tar-slip/size-cap refusal —
        // and it is the ONE sentence that separates "the
        // publisher shipped a bad asset" from "this machine could not unpack it".
        eprintln!("{}", stage_failure_note(program, pinned, &e.to_string()));
        *why = format!("staging {} failed: {e}", artifact.asset);
        return None;
    }
    // This member's bytes are good, whatever an earlier pass recorded about them (the
    // singleton lane's reasoning, member by member).
    crate::store::clear_stage_refusal(&build_dir);
    // The archive of a member that STAGED stays until the transaction knows the whole
    // tuple's fate (`carried_archive`): a sibling's abort discards this tree, and the
    // archive is what spares the retry the download.
    Some(Staged {
        build: pinned,
        build_dir,
        // Refused (sensitive/malformed) names are dropped here rather than at flip time —
        // same outcome as before, one admission instead of one per flip/rollback pass. A
        // group member's refusals are not a stage failure, matching `install`.
        exposes: crate::store::split_exposed(&pkg.exposes).0,
        prior_build,
        was_live,
        reloc,
        tree_root: artifact.tree_root.clone(),
        aliases: Aliases::for_program(program, index.program(program)),
    })
}

/// Flip a staged member live: point its `store/<program>/current` at the new build and (re)install
/// its shims. `true` on success. A partial flip (shims IO error after `current` was already
/// re-pointed) is self-undone via [`rollback_member`] so a `false` return leaves NO live
/// pointer into the new build — the abort cleanup then discards it. (Shim refusals for
/// sensitive names are not a flip failure; they are honestly dropped, matching `install`.)
///
/// A bundle's resolve check is `ahead`'s verdict when it was started at the stage
/// ([`ResolveAhead`]), else run here.
fn flip_member(layout: &Layout, program: &str, s: &Staged, ahead: Option<ResolveAhead>) -> bool {
    flip_member_with(layout, program, s, ahead, activate_build)
}

/// [`flip_member`] with the activation write injected. The one link write is atomic on
/// POSIX, so a real failure there leaves the link as it was; the case the repair below
/// exists for — a Windows junction removed before `mklink` recreates it, failing with the
/// link ALREADY gone or half-made — is one only an injected write can stage on the hosts
/// the tests run on.
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
        // The one link write is atomic on POSIX, but a Windows junction is removed
        // before `mklink` recreates it, so a failure there can leave the witness
        // ABSENT rather than unchanged. Point it back at the prior build (fresh
        // install: remove it) so it keeps agreeing with the shims, which were never
        // touched. Where the write failed atomically both repairs are no-ops —
        // re-pointing writes what was already there, and removing a link that does
        // not exist does nothing.
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
    // The build's own `shim_env` (its sidecar, written from the signed manifest when
    // it was staged) rides on every shim the flip lays — design S7.
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

/// `staging/<program>/<asset>` — where a byte-moving row downloads to — or
/// [`FlowError::VendorRefused`] when `asset` is not one plain file name. Both callers
/// unlink this path and sweep every `*.part` beside it BEFORE any download, and
/// `vendor::check_row` holds `https` rows to a bare name but not `github-release` ones:
/// an absolute asset (`Path::join` replaces the base) or a `..` climb would have a signed
/// row delete files anywhere the user can write. Checked on the
/// path itself, so no protocol lane can reach those deletions with one that escapes.
/// Make the directory a download lands in (`staging/<program>/`, and `staging/` above
/// it) when it is missing, each `0700` whatever the umask
/// ([`crate::platform::make_dirs_born`]). The fetcher (curl) creates the asset at
/// `0666 & ~umask`, and the stage hashes it and then opens it again to extract it: in a
/// directory other users could traverse or write (`create_dir_all` under umask `000` or
/// `002`), one of them could hold the asset open for writing, or rename another into
/// its place, across that gap (AT3, 2026-10-10). Nothing but the store's owner reads
/// `staging/` (the stage and gc), so it is its owner's alone in a system prefix too.
/// One that exists is left as it is.
fn make_staging_dir(dl: &Path) -> std::io::Result<()> {
    match dl.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, 0o700)
        }
        _ => Ok(()),
    }
}

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

/// THE ROLLBACK TARGET PREDICATE: whether [`rollback`] may land `program` on its retained
/// `build`. The channel `ch` still admits the build — at/above THIS program's floor and not
/// yanked ([`crate::gate::current_build_ok`]: the floor is `min_build_for`, never the
/// channel-wide number, so a floor published to revoke another program's builds refuses
/// none of this one's) — and the build has something in `bin/` to run
/// ([`build_can_be_rolled_onto`]). `None`: no verified channel is in hand, so no build is
/// known revoked.
///
/// ONE predicate for both readers: [`rollback`] lands on the highest retained build below the
/// active one that it admits, and gc keeps exactly that build below the live one
/// ([`crate::gc::reclaimable`]). Kept by its number alone, a yanked build just below the live
/// one was the "rollback" every pass preserved while the build the verb would land on was
/// reclaimed, so `aterm pkg rollback` then refused ("ay 17 is revoked — ay 18 stays") on a
/// store that had a build it could land on until that pass.
pub(crate) fn rollback_lands_on(
    layout: &Layout,
    ch: Option<&Channel>,
    program: &str,
    build: u64,
) -> bool {
    ch.is_none_or(|ch| crate::gate::current_build_ok(ch, program, Some(build)))
        && build_can_be_rolled_onto(layout, program, build)
}

/// Whether `build` of `program` is a build a rollback may actually LAND ON: its `bin/` still
/// holds at least one file [`rollback_member`] could re-point a shim at.
///
/// **A CORPSE IS NEVER THE ROLLBACK TARGET.** Selection above reads
/// [`crate::ops::list_installed`], which counts a build as installed on the strength of its
/// `<n>.ready` marker. A marker is a file; the tools are the tree; and the two can disagree
/// (a build dir with no `bin/`, its marker still saying `ok`). Selected, that build reaches
/// [`rollback_member`], whose restore loop asks `target.exists()` of every tool and takes
/// the `else` arm — `remove_file(layout.shim(tool))` — for every one of them. The verb would
/// have reported a successful rollback and left the machine with no compiler, no verifier
/// and no `ay`: a disarm, reported as a success.
///
/// Two fixes stand behind this one and neither makes it redundant. The corpse cannot be
/// MADE ([`crate::store::discard_build`], [`crate::ops::uninstall`]: the marker comes down
/// before the tree goes), and a marker RECORDS its `bin/` and is refuted by the disk when
/// that `bin/` is gone (the `contents=` record [`crate::store::build_is_complete`] checks).
/// This clause also covers builds whose markers recorded nothing, and every
/// way a tree can lose its contents that neither of those reaches — a person's interrupted
/// `rm -rf`, a restored backup, a volume that dropped a directory.
///
/// The predicate is deliberately weak: ONE surviving entry is enough. It is not this
/// function's job to decide that a build is complete — [`crate::store::build_is_complete`]
/// owns that question and has the marker to reason from — only to refuse the one shape that
/// turns a rollback into a disarm, and to refuse it without ever declining a build whose
/// `bin/` this process merely cannot read: `read_dir` failing for any reason other than
/// `NotFound` is a bound on what we may know, and answers `true`.
fn build_can_be_rolled_onto(layout: &Layout, program: &str, build: u64) -> bool {
    let bin = layout.build_dir(program, build).join("bin");
    match std::fs::read_dir(&bin) {
        Ok(entries) => entries.flatten().next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // EACCES on a shared prefix, macOS privacy consent, EIO: nobody looked, so nothing
        // is refuted. A rollback onto such a build behaves exactly as it did before.
        Err(_) => true,
    }
}

/// Roll a flipped (or partially-flipped) member back to the build it pointed at before this
/// transaction — its `prior_build`. For a tool the prior build actually contains, the shim
/// is re-pointed there; a tool the NEW build ADDED but the prior lacks has its shim REMOVED
/// (re-pointing it would dangle). A fresh install (`prior_build == None`) removes the shims.
///
/// The "does the prior build contain it?" probe must name the EXECUTABLE
/// ([`ToolName::exe_file`]), never the bare tool name — see [`ToolName`]'s docs.
fn rollback_member(layout: &Layout, program: &str, s: &Staged) {
    match s.prior_build {
        Some(prior) if prior != s.build => {
            let prior_dir = layout.build_dir(program, prior);
            // The restore set is the UNION of the new build's exposes and the prior
            // build's actual `bin/` contents. `s.exposes` alone misses a tool the
            // prior build shipped and the new one DROPPED: the new build's shim
            // prune (`install_tools`) already deleted that tool's shim — same
            // program, different build, exactly its job — so restoring only the new
            // list would re-activate the prior build with part of its PATH surface
            // silently missing. A sensitive name in the prior `bin/` never had a
            // shim (`ToolName::new` refuses it) and drops out here the same way.
            //
            // The LISTING half is admitted only through `rollback_may_claim`: a sysroot
            // bundle's `bin/` carries backends belonging to OTHER atpkg programs — trust's
            // carries `ay`, `clean` and `ty` — and restoring every file in it re-pointed
            // those programs' shims (and `alab-` aliases) at `store/trust/<prior>`, after
            // which the next trust flip's prune deleted them as stale trust shims (`ty`,
            // `ay`, `clean` gone from PATH). `unlink`'s restore has the same guard.
            let mut restore: std::collections::BTreeSet<crate::store::ToolName> =
                s.exposes.iter().cloned().collect();
            // Names that came from the listing alone: their aliases get the same ownership
            // check as the primary before they are re-pointed.
            let mut listing_only: std::collections::BTreeSet<crate::store::ToolName> =
                std::collections::BTreeSet::new();
            if let Ok(entries) = std::fs::read_dir(prior_dir.join("bin")) {
                for e in entries.flatten() {
                    // Files only: a stray subdirectory in `bin/` is not a tool and
                    // must not grow a shim.
                    if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    if let Some(name) = e.file_name().to_str() {
                        // Total strip, same semantics as `ToolName::from_shim_file`:
                        // on Unix `EXE_SUFFIX` is `""` and this is the identity.
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
            // `current` FIRST, then the shims — activation's own order — so the one divergence
            // an undo can leave is `current` ahead of the shims on the PRIOR build, which the
            // next pass's roll-forward (`cli::roll_forward_interrupted_activations`)
            // completes. The reverse order would let roll-forward put the rolled-off build
            // back.
            if let Err(e) = activate_build(layout, &prior_dir) {
                eprintln!(
                    "atpkg: {program}: the rollback to build {prior} could not re-point \
                     `current` ({e}) — its shims are re-laid all the same"
                );
            }
            // The PRIOR build's shims export the PRIOR build's `shim_env` — its sidecar,
            // written from its own signed manifest when it was staged (design S7). A
            // build staged before the sidecar existed has none, and reads as none.
            let env = crate::shim_env::read_sidecar(&prior_dir);
            // THE PRIOR BUILD'S EXEC ROOT BEFORE ITS SHIMS (`crate::compat`). This lay
            // renders through `platform::install_shim_env`, not `install_tools_env`, so the
            // root must be ensured here: otherwise a rollback to an affected trust build
            // re-points `tippy` at a build whose own tippy refuses its store path, and the
            // single-program update and install lanes run no reconcile after it.
            // Best-effort like the rest of an undo: a root that cannot be laid leaves the
            // shims plain, running the store path as before, and says so.
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
                    // A binary the new build added but the prior build lacks — drop the shim
                    // rather than leave it dangling at a nonexistent prior-build path.
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
            // A fresh install has no prior link to re-point, but `flip_member`'s
            // `activate_build` DID write `store/<program>/current` — and the abort
            // cleanup is about to delete the build it names. Remove the link too
            // (platform::remove_link — on Windows it is a junction that remove_file
            // refuses), or the program is left with a permanently dangling witness
            // link: harmless to GC (a broken own link means abstain), but a state no
            // ordinary operation should ever leave behind.
            crate::platform::remove_link(&layout.program_current(program));
        }
    }
    // THE FRONT-OF-PATH TWIN FOLLOWS ITS PRIMARY. This undo re-points — or removes —
    // `bin/<tool>` through `platform::install_shim_env`, never `install_tools_env`, so the
    // reconcile that lay ends with must run here: `agents/` is FIRST on every PATH, and a
    // stale `agents/<agent>` would keep running the build rolled away from — and, once gc
    // reclaimed it (its claim union never reads `agents/`), a deleted file. Placed after
    // the match so it covers the fresh-install arm too, where the sweep inside takes a twin
    // whose primary has just gone. Best-effort and idempotent, like the rest of an undo.
    crate::activate::reconcile_agents(layout);
}

/// Whether a rollback of `program` may lay `bin/<tool>` for a name it found ONLY in the
/// prior build's `bin/` listing — not in the signed exposes it is rolling off.
///
/// A listing is not an exposes list: a sysroot bundle's `bin/` carries backends that belong
/// to OTHER atpkg programs (trust's carries `ay`, `clean` and `ty`). So a listing name is
/// claimed only when it is not another installed program's OWN name
/// ([`another_program_is_installed_as`]) — that program owns it even when its shim has gone
/// missing — and its shim is absent or already resolves into `store/<program>/` at some
/// build (see [`shim_free_or_owned_by`]). The first case is the tool the new build dropped,
/// whose shim the flip's prune deleted, which is what the listing half exists to restore.
fn rollback_may_claim(layout: &Layout, program: &str, tool: &ToolName) -> bool {
    if tool.as_str() != program && another_program_is_installed_as(layout, tool.as_str()) {
        return false;
    }
    shim_free_or_owned_by(layout, program, &layout.shim(tool))
}

/// Whether a program named `name` is installed in this prefix: its `store/<name>/current`
/// link exists, or — on a prefix older than that per-program link, which `relay_shims`
/// also reads — `store/<name>/` holds a COMPLETE build ([`crate::store::build_is_complete`],
/// the rule [`crate::ops::list_installed`] counts by). Either one makes `<name>` that
/// program's own name, never a listing name a sysroot bundle may take.
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

/// Sysroot-bundle pre-activation wiring, dispatched on the signed `reloc` policy
/// (§10.1). `self-contained` (the pack-time-relocated default) needs nothing — the
/// payload already carries its dependencies, which is the only policy the trust
/// toolchain ships. Every other policy fails closed.
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

/// Current Unix epoch second — the `now_unix` input every freshness gate takes as a
/// parameter (the flow entry points stay clock-free for determinism; the CLI edge reads
/// the real clock through THIS one definition). It is the roster's clock, which fails
/// CLOSED: an unreadable clock reads as `i64::MAX`, so every window looks expired, never
/// as 1970.
pub(crate) use aterm_update_core::roster::now_unix;

/// Parse an RFC3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` to a Unix epoch second. Pure (no
/// clock), so the freshness gate stays deterministic; `None` on any malformed field, which
/// the caller treats as lapsed (fail closed). The `Z` suffix is REQUIRED and the length
/// exact: a timezone-offset stamp (`…+09:00`) must not be silently read as UTC — up to 14h
/// of fail-open skew on the freshness gate — and trailing bytes past the seconds field must
/// not parse at all; both are refused so the producer contract (`tools/atpkg-*.sh` and
/// `now_rfc3339` both emit exactly this shape) is enforced instead of assumed. It IS the
/// workspace's one strict parser, `aterm_types::rfc3339::parse_utc`.
pub(crate) use aterm_types::rfc3339::parse_utc as rfc3339_to_unix;

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
