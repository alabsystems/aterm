// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed manifest schemas (§4) and the discovery allow-list (§5), parsed
//! **only after signature verification**.
//!
//! Two TOML document types, each verified as exact raw bytes by [`crate::sig`] before
//! a single byte reaches a parser here:
//!
//! * [`Index`] — the root-signed `index.toml`: the freshness anchor, the `[keys]`
//!   release-key delegation, the **allow-list** of installable programs, and the
//!   channels. A repo NOT named in `[programs]` is unreachable **by construction**
//!   (R4) — private-config repos are excluded because they are never named.
//! * [`PkgManifest`] — a release-key-signed `pkg-<program>-<build>.toml`: the
//!   per-triple artifact table, the `exposes` shim list, and the honest `[cost]`.
//!
//! Every parse *entry point* here ([`parse_index`] / [`parse_pkg`]) takes
//! `&`[`VerifiedBytes`] (which has no public constructor), so the crate's own parse path
//! cannot run on unverified input — the same compile-time guarantee the line-scan stopgap
//! had, scoped to these functions (the schema structs derive `Deserialize` so a *caller*
//! can read the parsed result; that derive is an internal detail, not a sanctioned
//! unverified-parse API). It runs over the **real `toml` parser**: duplicate keys are a
//! hard error and
//! table scoping is intrinsic, so the line-scanner/real-TOML differential the Phase-1
//! `parse_delegation` had to hand-defend against simply cannot arise. Both carry the
//! `SUPPORTED_SCHEMA` **reject-newer** gate (a manifest from a newer format this build
//! cannot safely interpret is refused, fail-closed, rather than misread).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::sig::{Delegation, Reject, VerifiedBytes};

/// The highest manifest `schema` this build understands. A document declaring a higher
/// schema is from a newer format we cannot safely interpret, so it is **rejected** (the
/// client stays put) rather than misread — mirrors `aterm-update`'s `SUPPORTED_SCHEMA`.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The default repository the signed index lives on, under the configurable account:
/// `github.com/<account>/aterm` (§5). The index is a small signed release asset on the
/// **aterm repo itself** — no dedicated repo to administer, 1-to-1 with the existing
/// repos, and coherent with §16 (aterm is itself an index member). Overridable at runtime
/// via `ATPKG_INDEX_REPO` (see [`crate::discovery::index_repo`]).
pub const INDEX_REPO: &str = "aterm";

/// The root-signed `index.toml` (§4.1): allow-list + key delegation + freshness +
/// channels. Unknown top-level keys/tables are ignored (forward-compatible within a
/// schema); a *newer* schema is rejected by [`parse_index`].
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// Monotonic index counter — the durable high-water rollback floor (§8). Required:
    /// an index without one is malformed and fails closed.
    pub index_build: u64,
    /// RFC3339 generation time (informational).
    #[serde(default)]
    pub generated_at: String,
    /// RFC3339 freshness deadline; the client refuses this index at/after it (§8). The
    /// freshness comparison itself is done by the caller via [`crate::sig::check_freshness`].
    pub valid_until: String,
    /// `[keys]` — the rotatable release-key delegation the root index authorizes.
    pub keys: Keys,
    /// `[programs.<name>]` — the open-ended allow-list. The map key is the program name
    /// (`exposes`/install identity); the value names its repo + policy + optional group.
    #[serde(default)]
    pub programs: BTreeMap<String, Program>,
    /// `[[channels]]` — named, pinned program sets (`stable`/`nightly`). Parsed here;
    /// the coherence-group apply semantics land in Phase 4.
    #[serde(default)]
    pub channels: Vec<Channel>,
}

/// `[keys]` — the index's release-key delegation. A real TOML parse means a duplicate
/// key in this table is a hard error (fail-closed), not a silent last-wins.
#[derive(Debug, Clone, Deserialize)]
pub struct Keys {
    /// The rotatable key's identifier (`rk-2026-06`).
    pub release_key_id: String,
    /// The release key's base64 Ed25519 public key (32 raw bytes).
    pub release_key_pubkey: String,
    /// Belt-and-suspenders revocation deny-list: a key id here is refused before crypto.
    #[serde(default)]
    pub revoked_release_keys: Vec<String>,
}

/// One `[programs.<name>]` entry: where the program's release manifests live and how it
/// may be installed.
#[derive(Debug, Clone, Deserialize)]
pub struct Program {
    /// The GitHub repo (under the same account) carrying this program's `pkg-*.toml`.
    pub repo: String,
    /// `"prebuilt-only"` | `"prebuilt-or-build"` (§6). Empty ⇒ treated as prebuilt-only
    /// by later phases (fail-closed: never build from source without an explicit policy).
    #[serde(default)]
    pub policy: String,
    /// Coherence group: members of the same group apply atomically as one tuple (§7).
    /// `None` ⇒ loosely-coupled, applies independently (the open-ended R2 tools, and
    /// `aterm` itself per §16).
    #[serde(default)]
    pub coherence_group: Option<String>,
}

/// One `[[channels]]` entry: a named, pinned set of program builds plus the gating
/// counters and the attested reproducibility tuple (`[channels.meta]`).
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// Channel name (`stable`, `nightly`).
    pub name: String,
    /// Monotonic no-downgrade gate for the channel's coherence group.
    #[serde(default)]
    pub channel_build: u64,
    /// Yank floor: a pinned build below this is force-upgraded / tombstoned at apply (§7).
    #[serde(default)]
    pub min_build: u64,
    /// Per-program revocations (`"trust@4790"`), enforced at apply (§7).
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The pinned SET — exact per-program builds that move together (`program -> build`).
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// `[channels.meta]` — the attested reproducibility tuple (nightly id, trust-mc rev,
    /// …). Stored generically here; Phase 4/5 validate it. Not all fields are attested
    /// (§4.1 — `trust_fork_rev`/`llvm`/`clean_kernel_rev` are net-new, unproven).
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

impl Index {
    /// The release-key [`Delegation`] this (root-verified) index authorizes — the seam
    /// the per-`pkg.toml` verify ([`crate::sig::verify_pkg`]) consumes.
    #[must_use]
    pub fn delegation(&self) -> Delegation {
        Delegation {
            release_key_id: self.keys.release_key_id.clone(),
            release_key_pubkey_b64: self.keys.release_key_pubkey.clone(),
            revoked_release_keys: self.keys.revoked_release_keys.clone(),
        }
    }

    /// The named program, **iff** the verified index names it. `None` ⇒ the repo is
    /// unreachable (R4): private-config repos, half-finished repos, anything unlisted is
    /// never named, so this is exclusion *by construction*, not by heuristic.
    #[must_use]
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.get(name)
    }

    /// Whether `name` is an installable program named by the index. The fail-closed
    /// reachability rule (§5): an unlisted name is not installable, full stop.
    #[must_use]
    pub fn is_program(&self, name: &str) -> bool {
        self.programs.contains_key(name)
    }

    /// The installable program set after applying the **narrowing-only**
    /// `[packages].include`/`exclude` config (R4/§5). The signed index is the sole gate:
    ///
    /// * empty `include` ⇒ start from *every* program the index names;
    /// * non-empty `include` ⇒ start from only those of its names that the index **also**
    ///   names (an `include` entry absent from the index adds **nothing** — it can never
    ///   widen the set or introduce an unlisted repo);
    /// * `exclude` then subtracts.
    ///
    /// So no config can make a private-config / unlisted repo installable.
    #[must_use]
    pub fn installable(&self, include: &[String], exclude: &[String]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = if include.is_empty() {
            self.programs.keys().cloned().collect()
        } else {
            include
                .iter()
                .filter(|n| self.programs.contains_key(n.as_str()))
                .cloned()
                .collect()
        };
        for e in exclude {
            set.remove(e);
        }
        set
    }
}

/// A release-key-signed `pkg-<program>-<build>.toml` (§4.2): the per-triple artifact
/// matrix for one program build.
#[derive(Debug, Clone, Deserialize)]
pub struct PkgManifest {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// The program name — must equal the index `[programs]` key that pointed here; bound
    /// inside the signed bytes so a valid signature can't be paired with a re-pointed
    /// program (the caller cross-checks it against the requested program).
    pub program: String,
    /// Human version string (informational / display).
    #[serde(default)]
    pub version: String,
    /// Monotonic build number — the strictly-greater downgrade gate (reused from the
    /// updater's `build_number` semantics).
    pub build_number: u64,
    /// The binaries to shim into `bin/` (§10) — generic over multi-binary / oddly-named
    /// programs (R2).
    #[serde(default)]
    pub exposes: Vec<String>,
    /// Runtime dependencies — other index-named programs this build needs at runtime.
    /// Resolved at install ([`crate::flow::install`]): each MISSING dep is installed FIRST;
    /// a yanked/below-floor, unreachable, not-pinned, or cyclic dep is SKIPPED with a
    /// warning. A `requires` edge can pull a program IN — it can NEVER bypass the floor/yank
    /// gate ([`crate::gate::decide`]) or the §5 index reachability rule. SIGNED metadata:
    /// parsed only from a [`VerifiedBytes`], so a repo-write adversary cannot inject a
    /// dependency edge.
    #[serde(default)]
    pub requires: Vec<String>,
    /// `[[artifact]]` — one row per target triple. "No row for my triple" is a clean
    /// fail-closed skip, never an error (§6).
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
}

/// One `[[artifact]]` row: the prebuilt asset for a single target triple.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Target triple (`aarch64-apple-darwin`, …).
    pub target: String,
    /// `binary` | `sysroot-bundle` | `cargo-src` | `app-bundle` (§4.2).
    #[serde(default)]
    pub kind: String,
    /// The release asset file name to download.
    pub asset: String,
    /// SHA-256 of the COMPRESSED asset — the download-integrity gate.
    pub sha256: String,
    /// SHA-256 over the sorted extracted-file list — the apply-time re-verify root (§8
    /// TOCTOU). Empty until producers emit it.
    #[serde(default)]
    pub tree_root: String,
    /// Asset size in bytes — drives the per-artifact download cap + disk preflight.
    #[serde(default)]
    pub size: u64,
    /// Relocation policy for a `sysroot-bundle` (§10.1) — decides the install-time
    /// apply branch. `self-contained` (default): the payload was relocated at PACK
    /// time (machine-local deps vendored in), so install just extracts + activates,
    /// needing NO rustup on the user side. `rustup-linked`: the bundle ships a
    /// dangling `toolchain` link the installer re-points at the user's rustup
    /// nightly ([`crate::sysroot::relocate_sysroot`]). Signed (inside the manifest
    /// bytes), so the flag cannot be flipped by a repo-write adversary. Ignored for
    /// non-bundle kinds.
    #[serde(default = "default_reloc")]
    pub reloc: String,
    /// `[artifact.cost]` — honest accounting surfaced before any byte moves (R7).
    #[serde(default)]
    pub cost: Cost,
}

/// A `sysroot-bundle` with no explicit policy is treated as `self-contained` —
/// the safe default (extract-and-run; no assumption the user has rustup).
fn default_reloc() -> String {
    "self-contained".to_string()
}

/// `[artifact.cost]` — the honest, structural accounting block (§4.2/R7).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Cost {
    /// Bytes downloaded.
    #[serde(default)]
    pub download_bytes: u64,
    /// Bytes resident after install.
    #[serde(default)]
    pub disk_installed: u64,
    /// `0` ⇒ prebuilt; nonzero ⇒ from-source build estimate (seconds).
    #[serde(default)]
    pub build_seconds: u64,
}

impl PkgManifest {
    /// The artifact for `target`, if this build ships one for that triple. `None` is the
    /// clean fail-closed skip (§6) — not an error.
    #[must_use]
    pub fn artifact_for(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.target == target)
    }

    /// Whether this manifest's signed `program` field matches the program the client
    /// asked for. The anti-replay bind (§4.2): because `program` is inside the signed
    /// bytes, a valid signature can never be paired with a re-pointed program — but the
    /// caller must still CHECK it, so a `pkg.toml` legitimately signed for program A is
    /// refused when fetched as program B. Fail closed at the call site on `false`.
    #[must_use]
    pub fn is_for(&self, name: &str) -> bool {
        self.program == name
    }
}

/// Parse a root-verified `index.toml` from its [`VerifiedBytes`]. Strict UTF-8 (no lossy
/// substitution — the signature was checked over these exact bytes), real `toml` parse,
/// then the [`SUPPORTED_SCHEMA`] reject-newer gate. Any failure is a fail-closed
/// [`Reject`]; the caller treats every variant as "refuse, install nothing".
pub fn parse_index(verified: &VerifiedBytes) -> Result<Index, Reject> {
    let idx: Index = parse_toml(verified)?;
    if idx.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    Ok(idx)
}

/// Parse a release-verified `pkg-*.toml` from its [`VerifiedBytes`] (same strict UTF-8 +
/// reject-newer discipline as [`parse_index`]).
pub fn parse_pkg(verified: &VerifiedBytes) -> Result<PkgManifest, Reject> {
    let m: PkgManifest = parse_toml(verified)?;
    if m.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    Ok(m)
}

/// Shared strict-UTF-8 + `toml` deserialize over already-verified bytes. Invalid UTF-8
/// or any TOML/shape error (missing required field, duplicate key, wrong type) is
/// [`Reject::Malformed`] — fail closed, never a lossy reinterpretation.
fn parse_toml<T: serde::de::DeserializeOwned>(verified: &VerifiedBytes) -> Result<T, Reject> {
    #[cfg(test)]
    PARSE_CALLS.with(|c| c.set(c.get() + 1));
    let text = std::str::from_utf8(verified.as_slice()).map_err(|_| Reject::Malformed)?;
    toml::from_str(text).map_err(|_| Reject::Malformed)
}

#[cfg(test)]
thread_local! {
    /// Test-only counter proving a parser never runs on unverified bytes: incremented in
    /// [`parse_toml`], asserted to stay flat after a failed verify. Thread-local so
    /// libtest's per-test threads don't race a shared global; `#[cfg(test)]` so it never
    /// ships.
    static PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::{verify_index_with, verify_pkg};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const ROOT_SEED: [u8; 32] = [7u8; 32];
    const RELEASE_SEED: [u8; 32] = [1u8; 32];

    fn keypair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).expect("valid 32-byte seed")
    }
    fn pubkey_b64(kp: &Ed25519KeyPair) -> String {
        STANDARD.encode(kp.public_key().as_ref())
    }

    /// Root-sign `body` and return its [`VerifiedBytes`] (parsing cannot run on raw
    /// bytes — that does not type-check), so `parse_index` runs on genuinely-verified input.
    fn verified(body: &str) -> VerifiedBytes {
        let root = keypair(&ROOT_SEED);
        let raw = body.as_bytes().to_vec();
        let sig = root.sign(&raw).as_ref().to_vec();
        verify_index_with(&pubkey_b64(&root), raw, &sig).expect("root verifies index")
    }

    fn release_pk() -> String {
        pubkey_b64(&keypair(&RELEASE_SEED))
    }

    /// A complete, realistic index naming three programs (and deliberately NOT naming a
    /// private-config repo like `dotfiles`).
    fn full_index() -> String {
        format!(
            r#"
schema = 1
index_build = 41
generated_at = "2026-06-28T12:00:00Z"
valid_until = "2026-07-05T12:00:00Z"

[keys]
release_key_id = "rk-2026-06"
release_key_pubkey = "{pk}"
revoked_release_keys = ["rk-2026-05"]

[programs.ay]
repo = "ay"
policy = "prebuilt-or-build"

[programs.trust]
repo = "trust"
policy = "prebuilt-only"
coherence_group = "rustc"

[programs.aterm]
repo = "aterm"
policy = "prebuilt-only"

[[channels]]
name = "stable"
channel_build = 137
min_build = 120
yanked = ["trust@4790"]
pin = {{ aterm = 1234, trust = 4821, ay = 18 }}

[channels.meta]
nightly = "nightly-2025-12-03"
trust_mc_rev = "0.67.0"
"#,
            pk = release_pk()
        )
    }

    #[test]
    fn parses_a_full_index_and_extracts_delegation() {
        let idx = parse_index(&verified(&full_index())).expect("valid index parses");
        assert_eq!(idx.index_build, 41);
        assert_eq!(idx.valid_until, "2026-07-05T12:00:00Z");
        // Delegation comes from [keys], usable by verify_pkg.
        let d = idx.delegation();
        assert_eq!(d.release_key_id, "rk-2026-06");
        assert_eq!(d.release_key_pubkey_b64, release_pk());
        assert_eq!(d.revoked_release_keys, vec!["rk-2026-05".to_string()]);
        // Programs, channels, pin, meta all parsed.
        assert_eq!(
            idx.program("trust").unwrap().coherence_group.as_deref(),
            Some("rustc")
        );
        assert_eq!(idx.program("ay").unwrap().repo, "ay");
        assert_eq!(idx.channels.len(), 1);
        let ch = &idx.channels[0];
        assert_eq!(ch.name, "stable");
        assert_eq!(ch.pin.get("trust"), Some(&4821));
        assert_eq!(
            ch.meta.get("nightly").map(String::as_str),
            Some("nightly-2025-12-03")
        );
        // The delegation actually verifies a release-signed pkg.
        let release = keypair(&RELEASE_SEED);
        let pkg = b"schema = 1\nprogram = \"ay\"\nbuild_number = 18\n";
        let sig = release.sign(pkg).as_ref().to_vec();
        assert!(verify_pkg(pkg.to_vec(), &sig, &d).is_ok());
    }

    // R4: a private-config repo (never named in the index) is unreachable BY CONSTRUCTION.
    #[test]
    fn unlisted_repo_is_unreachable() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        assert!(
            idx.program("dotfiles").is_none(),
            "an unlisted repo is not a program"
        );
        assert!(!idx.is_program("dotfiles"));
        assert!(
            !idx.installable(&[], &[]).contains("dotfiles"),
            "a private-config repo can never be in the installable set"
        );
        // Sanity: the named programs ARE reachable.
        assert!(idx.is_program("trust") && idx.is_program("ay") && idx.is_program("aterm"));
    }

    // §5: include/exclude are NARROWING-ONLY — an include naming an absent repo adds nothing.
    #[test]
    fn include_exclude_are_narrowing_only() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        // Default (empty include): every named program.
        assert_eq!(
            idx.installable(&[], &[]),
            ["aterm", "ay", "trust"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // include narrows to the intersection with named programs...
        assert_eq!(
            idx.installable(&["ay".into(), "trust".into()], &[]),
            ["ay", "trust"].iter().map(|s| s.to_string()).collect()
        );
        // ...and an include naming an ABSENT repo can never add it (no widening).
        assert_eq!(
            idx.installable(&["ay".into(), "dotfiles".into()], &[]),
            ["ay"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
        );
        // exclude subtracts.
        assert_eq!(
            idx.installable(&[], &["trust".into()]),
            ["aterm", "ay"].iter().map(|s| s.to_string()).collect()
        );
    }

    // reject-newer: a schema beyond this build is refused (the client stays put).
    #[test]
    fn rejects_newer_schema() {
        let body = full_index().replace("schema = 1", "schema = 99");
        assert_eq!(parse_index(&verified(&body)).unwrap_err(), Reject::Schema);
    }

    // A malformed / incomplete index (missing a required field) fails closed.
    #[test]
    fn malformed_index_fails_closed() {
        // Missing index_build (required) → Malformed, not a default-0 silent accept.
        let body = format!(
            "schema = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n[keys]\n\
             release_key_id = \"rk\"\nrelease_key_pubkey = \"{}\"\n",
            release_pk()
        );
        assert_eq!(
            parse_index(&verified(&body)).unwrap_err(),
            Reject::Malformed
        );
    }

    // A real TOML parser rejects a DUPLICATE key in [keys] (the line-scanner had to
    // hand-defend this; toml gives it for free) → fail closed.
    #[test]
    fn duplicate_key_in_keys_table_fails_closed() {
        let pk = release_pk();
        let body = format!(
            "schema = 1\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{pk}\"\n\
             release_key_pubkey = \"{pk}\"\n"
        );
        assert_eq!(
            parse_index(&verified(&body)).unwrap_err(),
            Reject::Malformed
        );
    }

    // Table scoping is intrinsic to the real parser: a `release_key_pubkey` in a SIBLING
    // table cannot shadow the genuine [keys] delegation (it lands in an ignored table).
    #[test]
    fn sibling_table_cannot_hijack_delegation() {
        let good = release_pk();
        let body = format!(
            "schema = 1\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{good}\"\n\
             [meta]\nrelease_key_pubkey = \"AAAA\"\n"
        );
        let idx = parse_index(&verified(&body)).expect("parses; [meta] ignored");
        assert_eq!(idx.delegation().release_key_pubkey_b64, good);
    }

    // A multiline `revoked_release_keys` array — which the Phase-1 line-scanner had to
    // fail closed on — is valid TOML and now parses correctly.
    #[test]
    fn multiline_revoked_array_parses() {
        let pk = release_pk();
        let body = format!(
            "schema = 1\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{pk}\"\n\
             revoked_release_keys = [\n  \"rk-old\",\n  \"rk-older\",\n]\n"
        );
        let idx = parse_index(&verified(&body)).expect("multiline array is valid TOML");
        assert_eq!(
            idx.delegation().revoked_release_keys,
            vec!["rk-old".to_string(), "rk-older".to_string()]
        );
    }

    // pkg-*.toml: per-triple artifact selection + reject-newer + clean missing-triple skip.
    #[test]
    fn parses_pkg_manifest_and_selects_artifact() {
        let body = r#"
schema = 1
program = "trust"
version = "1.96.0-dev"
build_number = 4821
exposes = ["trust", "trust-mc"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "sysroot-bundle"
asset = "trust-4821-aarch64-apple-darwin.tar.zst"
sha256 = "deadbeef"
size = 1837465600
[artifact.cost]
download_bytes = 1837465600
disk_installed = 3221225472
build_seconds = 0
"#;
        let m = parse_pkg(&verified(body)).expect("valid pkg manifest");
        assert_eq!(m.program, "trust");
        assert_eq!(m.build_number, 4821);
        assert_eq!(m.exposes, vec!["trust".to_string(), "trust-mc".to_string()]);
        let a = m
            .artifact_for("aarch64-apple-darwin")
            .expect("triple present");
        assert_eq!(a.sha256, "deadbeef");
        assert_eq!(a.cost.disk_installed, 3221225472);
        // A triple with no row is a clean fail-closed skip, not an error.
        assert!(m.artifact_for("x86_64-unknown-linux-gnu").is_none());
    }

    // The parser NEVER runs on unverified bytes: a tampered index fails verify, so there
    // is no VerifiedBytes to parse, and PARSE_CALLS stays flat. (The compile-time half —
    // parse_index(raw_bytes) does not type-check — needs no test.)
    #[test]
    fn parser_never_runs_on_failed_verify() {
        PARSE_CALLS.with(|c| c.set(0));
        let root = keypair(&ROOT_SEED);
        let body = full_index();
        let mut sig = root.sign(body.as_bytes()).as_ref().to_vec();
        sig[0] ^= 0x01; // tamper
        assert!(verify_index_with(&pubkey_b64(&root), body.into_bytes(), &sig).is_err());
        assert_eq!(
            PARSE_CALLS.with(std::cell::Cell::get),
            0,
            "no parse may run when verification fails"
        );
        // Positive control: a good signature verifies, and only then does the parser run.
        let vb = verified(&full_index());
        let _ = parse_index(&vb).unwrap();
        assert!(PARSE_CALLS.with(std::cell::Cell::get) >= 1);
    }

    /// Root-sign RAW bytes (which may be invalid UTF-8) into [`VerifiedBytes`], so the
    /// strict-`from_utf8` arm can be exercised over genuinely-verified, non-UTF-8 input.
    fn verified_raw(raw: Vec<u8>) -> VerifiedBytes {
        let root = keypair(&ROOT_SEED);
        let sig = root.sign(&raw).as_ref().to_vec();
        verify_index_with(&pubkey_b64(&root), raw, &sig).expect("root verifies raw bytes")
    }

    // STRICT, never-lossy UTF-8: a signed index containing an invalid-UTF-8 byte is
    // rejected (Malformed), NOT silently U+FFFD-substituted and reinterpreted. The byte
    // is part of the genuinely-verified bytes, so this exercises the parse-layer's
    // from_utf8 arm (not the signature).
    #[test]
    fn strict_utf8_rejects_invalid_bytes_in_signed_index() {
        let mut raw = b"schema = 1\nindex_build = 1\n".to_vec();
        raw.push(0xFF); // not valid UTF-8
        raw.extend_from_slice(b"\nvalid_until = \"2026-07-05T12:00:00Z\"\n");
        assert_eq!(
            parse_index(&verified_raw(raw)).unwrap_err(),
            Reject::Malformed
        );
    }

    // parse_pkg negative paths (symmetry with the index gate): a newer schema is refused,
    // and a missing required field fails closed.
    #[test]
    fn parse_pkg_rejects_newer_schema_and_missing_field() {
        let newer = "schema = 99\nprogram = \"ay\"\nbuild_number = 1\n";
        assert_eq!(parse_pkg(&verified(newer)).unwrap_err(), Reject::Schema);
        // Missing build_number (required) → Malformed, not a default-0 silent accept.
        let missing = "schema = 1\nprogram = \"ay\"\n";
        assert_eq!(
            parse_pkg(&verified(missing)).unwrap_err(),
            Reject::Malformed
        );
    }

    // The signed `program` field binds the manifest to a program (§4.2 anti-replay): a
    // pkg legitimately signed for "ay" must be refused when fetched as some other program.
    #[test]
    fn pkg_program_field_binds_identity() {
        let m = parse_pkg(&verified(
            "schema = 1\nprogram = \"ay\"\nbuild_number = 18\n",
        ))
        .unwrap();
        assert!(m.is_for("ay"));
        assert!(
            !m.is_for("trust"),
            "a pkg signed for ay must not pass as trust"
        );
    }

    // §17: `requires` is SIGNED metadata parsed from the verified bytes; absent ⇒ empty.
    #[test]
    fn parses_requires_field() {
        let with = parse_pkg(&verified(
            "schema = 1\nprogram = \"ay\"\nbuild_number = 18\nrequires = [\"ny\"]\n",
        ))
        .unwrap();
        assert_eq!(with.requires, vec!["ny".to_string()]);
        let without = parse_pkg(&verified(
            "schema = 1\nprogram = \"ay\"\nbuild_number = 18\n",
        ))
        .unwrap();
        assert!(
            without.requires.is_empty(),
            "absent requires defaults to empty"
        );
    }
}
