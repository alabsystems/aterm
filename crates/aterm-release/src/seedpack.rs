// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Toolchain-seed validation for the batteries-included bundle
//! (docs/TOOLCHAIN-PACKAGE-MANAGER.md §9.1): a cut seals the flat signed
//! registry `tools/atpkg-*.sh` emit (`index.toml`(`.sig`),
//! `aterm-machines.toml`(`.sig`), `pkg-*.toml`(`.sig`), artifact tarballs)
//! into `Contents/Resources/toolchain-seed.lproj`, where the client's bundled-seed
//! lane (crates/atpkg/src/bundled.rs) installs from it offline through
//! atpkg's ordinary signature gates.
//!
//! This module is the CUT-TIME quality gate, not the client trust anchor: it
//! refuses to seal a seed the shipped client would reject or ignore.
//! Resurrected (2026-08-17) from the lane deleted in `fc70a9db` with the ONE
//! correction that deletion demanded: the old validator re-implemented
//! verification under the retired `PKG_ROOT_PUBKEY`; this one runs the
//! CLIENT'S OWN chain — `atpkg::DirFetcher` over the seed dir, roster
//! admission and index selection under the same compiled paper-master anchor
//! the shipped binary bakes, per-pkg delegation via the trusted index — so
//! there is exactly one verification implementation and no second root to go
//! stale. On top of that chain sit the producer-only rules the client cannot
//! enforce: freshness must not already be lapsed, every pinned program must
//! carry a PRESENT artifact of exactly its signed size (a truncated copy must
//! not ship), and every file in the directory must be accounted for by the
//! signed manifests (nothing rides the code-signature seal unaudited).
//! Artifact bytes are NOT re-hashed here — every client re-verifies sha256 +
//! `tree_root` at install; the cut checks presence + exact size.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use atpkg::flow::Fetcher as _;

/// The PRODUCER-side staging directory under `dist/`, deliberately NOT
/// [`atpkg::SEED_DIR_NAME`].
///
/// The in-bundle name carries a `.lproj` suffix because codesign's optional-seal
/// rule keys off it; a plain directory in `dist/` is not inside any bundle and
/// nothing seals it, so borrowing the suffix here would only make an odd path
/// odder and imply a signing property this location does not have. The two names
/// are different because they answer to different systems — the cutter copies
/// `dist/toolchain-seed` INTO `Contents/Resources/toolchain-seed.lproj`.
pub const STAGED_DIR_NAME: &str = "toolchain-seed";

/// What a validated seed holds — feeds the bundle step's log line and the
/// provenance record.
#[derive(Debug, Clone)]
pub struct SeedStat {
    /// The validated registry directory (the copy source).
    pub dir: PathBuf,
    /// Regular files in the registry (all accounted for).
    pub files: usize,
    /// Total payload bytes.
    pub bytes: u64,
    /// The signed index's monotonic build.
    pub index_build: u64,
    /// The seed's EFFECTIVE freshness horizon (RFC3339, verbatim): the EARLIER
    /// of the index's own `valid_until` and the roster's, because the client
    /// checks the roster first and the seed dies with whichever lapses first.
    /// Recording the index's alone overstates the shelf life of the DMG.
    pub valid_until: String,
    /// The roster generation (`roster_seq`) that authorized the sealed index —
    /// recorded so a shipped DMG can be audited against a later revocation.
    /// Nothing at cut time can prove this generation is still current (that
    /// would be a network fact); what it can do is make the question
    /// answerable after the fact.
    pub roster_seq: u64,
    /// The channel-pinned `(program, build)` set the seed can install.
    pub programs: Vec<(String, u64)>,
    /// The target triples the seed's PRESENT artifacts serve — recorded from
    /// signed `[[artifact]]` rows whose tarball actually travelled, never from
    /// rows alone (a row naming a triple whose tarball is absent proves
    /// nothing). This is what tells the cut whether a per-arch DMG variant is
    /// PRODUCIBLE: `publish.rs` emits the `aterm-<v>-x86_64.dmg` pair exactly
    /// when this set contains `x86_64-apple-darwin`.
    pub targets: BTreeSet<String>,
    /// Non-fatal findings, for the CALLER to place.
    ///
    /// `validate` used to `println!` these itself, and a pure validator that prints
    /// cannot be placed: its three callers sit on two different grids and at different
    /// positions relative to their own label, so a function with no caller context got
    /// all three wrong. Worse, both cut call sites run `validate`, so one `cargo ship
    /// cut` printed the identical 600-column paragraph twice, ninety seconds apart —
    /// which is how a real warning teaches an operator to skip warnings.
    pub warnings: Vec<String>,
}

/// Resolve the seed directory for this cut: `ATERM_SEED_DIR` (explicit
/// operator override) beats the conventional `dist/toolchain-seed`; absence of
/// both means no seed dir exists — the CALLER decides whether that refuses the
/// cut (`step_build` does, batteries-included being the product default since
/// 2026-08-17) or ships seedless under an explicit `ATERM_SEEDLESS=1`.
pub fn resolve(dist: &Path) -> Option<PathBuf> {
    if let Ok(v) = std::env::var("ATERM_SEED_DIR") {
        let v = v.trim();
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    let conventional = dist.join(STAGED_DIR_NAME);
    conventional.is_dir().then_some(conventional)
}

/// What architecture surface a seed directory is being validated AS.
///
/// `Universal` is the cut's staging gate, unchanged: the full dual-arch seed a
/// universal .app seals, with the aarch64 hard-refusal and the x86_64 warning
/// (gate 2b). `Only(triple)` exists for the per-arch DMG restage: the filtered
/// `.lproj` must serve EXACTLY that one triple — an artifact of any other arch
/// in it means the filter leaked (the whole point of the split was not shipping
/// those bytes), and no artifact of the named arch means the filter gutted the
/// seed (batteries advertised, batteries absent). Both refuse the cut, at cut
/// time, before hdiutil ever runs — never a broken DMG in a user's hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchScope<'a> {
    /// The dual-arch seed sealed into the signed universal app (all existing
    /// callers, including tests/seedpack_real.rs).
    Universal,
    /// A per-arch DMG restage filtered to exactly this target triple.
    Only(&'a str),
}

/// Validate `dir` as a shippable seed by running the SHIPPED CLIENT'S first-run
/// resolution over it: `DirFetcher` candidates → paper-master roster admission →
/// index selection (no durable floors — exactly a fresh install's posture) →
/// per-pkg release delegation. Any failure is a hard error — a cut must never
/// seal a seed its own client would refuse.
pub fn validate(dir: &Path) -> Result<SeedStat, String> {
    validate_scoped(dir, ArchScope::Universal)
}

/// [`validate`] under an explicit [`ArchScope`] — the Only-scope caller is the
/// per-arch DMG restage (`dmg::create_arch_filtered`), which re-proves each
/// filtered `.lproj` through this same client chain before it is imaged.
pub fn validate_scoped(dir: &Path, scope: ArchScope<'_>) -> Result<SeedStat, String> {
    validate_inner(dir, scope).map(|(stat, _)| stat)
}

/// The triple → present-artifact-name map of a VALIDATED seed, derived from
/// the signed `[[artifact]]` target rows (never from filename conventions —
/// every byte-selection decision stays anchored in attested data). This is
/// what drives the per-arch DMG's keep-set: `dmg::create_arch_filtered` keeps
/// exactly `assets_by_triple(seed)[triple]` plus every signed manifest.
///
/// Runs the full Universal validation first: only a seed the shipped client
/// would accept may have its rows drive which bytes ship.
pub fn assets_by_triple(dir: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    validate_inner(dir, ArchScope::Universal).map(|(_, map)| map)
}

fn validate_inner(
    dir: &Path,
    scope: ArchScope<'_>,
) -> Result<(SeedStat, BTreeMap<String, BTreeSet<String>>), String> {
    // ---- 0. flat, regular, no surprises ---------------------------------
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut bytes: u64 = 0;
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("read seed dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read seed dir {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta =
            std::fs::symlink_metadata(entry.path()).map_err(|e| format!("stat {name}: {e}"))?;
        if !meta.is_file() {
            return Err(format!(
                "seed entry {name} is not a regular file (symlinks/subdirectories do not ship)"
            ));
        }
        // Finder litter is the single most likely reason a hand-inspected staging
        // directory fails this gate, and "unaccounted file(s): .DS_Store" with no
        // remedy is a cryptic way to stop a release. Name the fix here instead.
        if name == ".DS_Store" || name.starts_with("._") {
            return Err(format!(
                "seed dir {} contains Finder metadata ({name}). Every sealed byte must be \
                 named by a signed manifest, and this is not — remove it and re-run: \
                 `find {} -name '.DS_Store' -o -name '._*' | xargs rm -f`",
                dir.display(),
                dir.display()
            ));
        }
        bytes = bytes.saturating_add(meta.len());
        names.insert(name);
    }
    if names.is_empty() {
        return Err(format!("seed dir {} is empty", dir.display()));
    }

    // ---- 1. the client's own chain, first-run posture --------------------
    // An unarmed build cannot validate (or use) a seed at all — refusing here
    // keeps the "batteries included" label from out-running the proof.
    let anchor = atpkg::Anchor::pinned(0);
    if !anchor.is_armed() {
        return Err(
            "no paper master is pinned in this build (atpkg::PKG_TRUST_ANCHORS is empty) — \
             the shipped client would ignore the seed; arm the anchor or cut with \
             ATERM_SEEDLESS=1"
                .to_string(),
        );
    }
    let now = now_unix();
    let fetcher = atpkg::DirFetcher::new(dir.to_path_buf());
    let candidates = fetcher
        .index_candidates()
        .map_err(|e| format!("seed dir does not read as a registry: {e}"))?;
    if candidates.is_empty() {
        return Err(format!(
            "seed dir {} holds no complete signed quad (index.toml(.sig) + \
             aterm-machines.toml(.sig)) — the client's DirFetcher would yield no candidate",
            dir.display()
        ));
    }
    let selection = atpkg::select_index(&anchor, candidates, atpkg::BuildFloor::none(), now);
    let Some(selected) = selection.selected else {
        return Err(
            "the seed's signed quad does not survive the client's roster + index chain \
             (paper-master roster admission, machine authorization, signature over the raw \
             index bytes) — the shipped client would refuse this seed"
                .to_string(),
        );
    };
    let index = selected.index;

    // ---- 1a. freshness, with SHELF LIFE ---------------------------------
    // A DMG is not consumed the instant it is pressed: it sits on a download
    // page for months. The client refuses a lapsed index outright (flow.rs
    // `Stale`), so a seed that expires mid-shelf turns every later first run
    // into a silent network-only bootstrap — batteries advertised, batteries
    // absent. Refusing only an ALREADY-lapsed horizon (what this gate did
    // first, and what `atpkg_check_valid_until` still does on the shell side)
    // would let a cut ship one second of shelf life. So the gate is
    // now + MARGIN, not now.
    //
    // And the horizon that matters is the EARLIER of two: the client checks
    // the roster BEFORE the index (`select_index` pass 1), so a seed dies with
    // whichever lapses first. Recording only the index's — as this did —
    // overstates the shelf life of every DMG in the provenance record.
    let margin = min_shelf_secs()?;
    let index_horizon = i64::try_from(rfc3339_to_unix(&index.valid_until).ok_or_else(|| {
        format!(
            "index.toml: unparseable valid_until {:?}",
            index.valid_until
        )
    })?)
    .unwrap_or(i64::MAX);
    // Fail-closed on both unreadable ends, matching the client: an unparseable
    // `valid_until` is lapsed, and an unreadable clock reads `i64::MAX`, which
    // outlives every horizon — a machine that cannot tell the time seals
    // nothing rather than sealing something it cannot vouch for.
    let deadline = now.saturating_add(margin);
    if deadline >= index_horizon {
        return Err(freshness_err(
            "index.toml",
            &index.valid_until,
            margin,
            now >= index_horizon,
        ));
    }
    // The roster leg, gated through the client's OWN re-admission at the future
    // clock rather than a second parse of the file — `still_fresh` is exactly
    // the rule `admit_roster` applied at selection, asked about a later `now`.
    let roster_valid_until = read_roster_valid_until(dir)?;
    if index.roster().still_fresh(deadline).is_err() {
        let lapsed = index.roster().still_fresh(now).is_err();
        return Err(freshness_err(
            "aterm-machines.toml (the roster)",
            &roster_valid_until,
            margin,
            lapsed,
        ));
    }
    // What the DMG can honestly claim: the earlier of the two.
    let effective_valid_until = match rfc3339_to_unix(&roster_valid_until) {
        Some(r) if i64::try_from(r).unwrap_or(i64::MAX) < index_horizon => {
            roster_valid_until.clone()
        }
        _ => index.valid_until.clone(),
    };

    // The union of every channel's pin set — the installable surface.
    let mut pins: Vec<(String, u64)> = Vec::new();
    for ch in &index.channels {
        for (program, build) in &ch.pin {
            if !pins.iter().any(|(p, pb)| p == program && pb == build) {
                pins.push((program.clone(), *build));
            }
        }
    }
    if pins.is_empty() {
        return Err("index.toml pins no programs — an empty seed must not ship".to_string());
    }

    // ---- 2. every pinned program: signed pkg manifest + present artifact --
    let mut seen_targets: BTreeSet<String> = BTreeSet::new();
    let mut assets_by_target: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut accounted: BTreeSet<String> = [
        "index.toml",
        "index.toml.sig",
        "aterm-machines.toml",
        "aterm-machines.toml.sig",
    ]
    .map(str::to_string)
    .into();
    for (program, build) in &pins {
        let pkg_name = format!("pkg-{program}-{build}.toml");
        let sig_name = format!("{pkg_name}.sig");
        let repo = index
            .program(program)
            .map(|p| p.repo.clone())
            .unwrap_or_else(|| program.clone());
        let (pkg_bytes, pkg_sig) = fetcher
            .pkg_manifest(&repo, program, *build)
            .map_err(|e| format!("pinned {program}@{build}: {e}"))?;
        // The client's own delegation gate: release-key signature under the
        // trusted index's generation, verify-before-parse by construction.
        let verified = index.verify_pkg(pkg_bytes, &pkg_sig).map_err(|e| {
            format!("{pkg_name} does not verify under the index's delegated release key ({e:?})")
        })?;
        let pkg = atpkg::parse_pkg(&verified)
            .map_err(|e| format!("{pkg_name} parse (after verify): {e:?}"))?;
        accounted.insert(pkg_name.clone());
        accounted.insert(sig_name);
        // ANTI-REPLAY BIND, mirroring the client's own install-time check: a
        // release-key signature proves the OWNER made these bytes, never that
        // they are the bytes THIS pin names. Without this, a stale or
        // mis-named pack artifact copied into the registry verifies here and
        // is refused on every client.
        if !pkg.is_for(program) {
            return Err(format!(
                "{pkg_name} names program {:?}, not {program:?} — the client would reject \
                 this pin (anti-replay bind)",
                pkg.program
            ));
        }
        if pkg.build_number != *build {
            return Err(format!(
                "{pkg_name} carries build_number {}, not the pinned {build} — the client \
                 would reject this pin (anti-replay bind)",
                pkg.build_number
            ));
        }
        if pkg.artifacts.is_empty() {
            return Err(format!("{pkg_name}: no [[artifact]] rows"));
        }
        let mut present = 0usize;
        for row in &pkg.artifacts {
            if !names.contains(&row.asset) {
                // Other-triple artifact not carried by this seed — fine under
                // Universal scope (the published registry is bigger than any
                // one seal). Under Only scope ONE absence is NOT fine: a row
                // naming the scoped triple whose tarball is missing means the
                // per-arch filter dropped a byte the signed manifest promises
                // this arch, and the DMG would offer the program and then fail
                // offline — refuse the cut, not the user.
                if let ArchScope::Only(triple) = scope
                    && row.target == triple
                {
                    return Err(format!(
                        "{pkg_name}: the {triple}-filtered seed is missing {} — the \
                         per-arch filter dropped an artifact the signed manifest \
                         promises this architecture",
                        row.asset
                    ));
                }
                continue;
            }
            // Which target triples this seed can actually serve — the arch
            // coverage gate below reads it. Recorded from the artifacts that
            // are PRESENT, never from the manifest rows alone: a row naming a
            // triple whose tarball did not travel proves nothing.
            seen_targets.insert(row.target.clone());
            let actual = std::fs::metadata(dir.join(&row.asset))
                .map_err(|e| format!("stat {}: {e}", row.asset))?
                .len();
            if actual != row.size {
                return Err(format!(
                    "artifact {} is {actual} bytes but the signed manifest says {} — \
                     truncated or stale copy",
                    row.asset, row.size
                ));
            }
            accounted.insert(row.asset.clone());
            assets_by_target
                .entry(row.target.clone())
                .or_default()
                .insert(row.asset.clone());
            present += 1;
        }
        // Universal scope only: a pinned program with NO artifact at all is a
        // broken seal. Under Only scope, zero-present is legal exactly when the
        // manifest carries no row for the scoped triple (the client clean-skips
        // that pin on this arch — the loop above already refused the other
        // case, a promised row whose tarball the filter dropped).
        if present == 0 && scope == ArchScope::Universal {
            return Err(format!(
                "pinned {program}@{build} has no artifact present in the seed — it would be \
                 offered and then fail offline"
            ));
        }
    }

    // ---- 2b. arch coverage, the macOS twin of build.ps1's gate -----------
    // The .app is UNIVERSAL (arm64 + x86_64) and the registry is packed
    // per-triple, so a seed serves exactly the slices its artifacts name. Since
    // atpkg index build 12 the published registry has carried x86_64-apple-darwin
    // artifacts beside aarch64, and since index build 14 EVERY pinned program
    // does — the rustc coherence group included (pkg-trust-6808.toml carries an
    // x86_64-apple-darwin [[artifact]] row; the old "rustc_private is absent
    // cross-host" limit was cleared). Sealing an arm64-only stage into a
    // universal DMG ships hundreds of MB from which atpkg on an Intel Mac can
    // install exactly nothing: every pin clean-skips on triple
    // (`artifact_for`), so the batteries are silently absent on a machine that
    // was promised them. The Windows lane already refuses this; the mac lane
    // must SAY it at minimum, because "installs with all the binaries" is now
    // the product claim.
    //
    // The two slices are NOT symmetric, and treating them alike was a real bug.
    //
    //  * NO aarch64 artifacts is a BROKEN CUT. That is the slice essentially every
    //    Mac sold since 2020 runs, so a seal without it delivers batteries to
    //    almost nobody while still costing every downloader ~600 MB. It is a hard
    //    refusal, and deliberately NOT mutable by `ATERM_SEED_ARCH_ACK` — the mute
    //    button used to cover this case too, which meant one env var could ship the
    //    one seal that is never right.
    //  * NO x86_64 artifacts is a PARTIAL cut, not an impossible one. It used to be
    //    unavoidable; it no longer is for any of the toolset, so an arm64-only
    //    stage is now usually a stale `INDEX_BUILD` rather than a fact about the
    //    world. It warns, and the ack silences THAT and only that, because a
    //    deliberately arm64-only seal is the one an operator can honestly
    //    acknowledge. (An acknowledged arm64-only seal also cuts NO x86_64 DMG
    //    variant and omits the manifest's `dmg_x86_64` keys — Intel installs
    //    keep taking the lean zip, exactly the pre-pair behaviour.)
    //
    // The gate is also not sufficient on its own: `validate`'s present-artifact
    // check counts an artifact of ANY triple, so a seal could satisfy it while
    // serving no slice this app runs on. This is where that is caught.
    let covered: BTreeSet<&str> = seen_targets.iter().map(String::as_str).collect();
    let listed = || {
        if covered.is_empty() {
            "none".to_string()
        } else {
            covered.iter().copied().collect::<Vec<_>>().join(", ")
        }
    };
    let mut warnings = Vec::new();
    match scope {
        // A per-arch restage must serve EXACTLY its one triple: any other
        // covered triple means the filter leaked foreign-arch tarballs back
        // into a DMG whose whole reason to exist is not carrying them, and an
        // empty coverage means it filtered the seed into uselessness. Either
        // way the filter is wrong — refuse the cut, at cut time.
        ArchScope::Only(triple) => {
            if !(covered.len() == 1 && covered.contains(triple)) {
                return Err(format!(
                    "the {triple}-filtered seed covers [{}] — a per-arch DMG must carry \
                     exactly its own architecture's artifacts, no more (the filter leaked) \
                     and no less (the filter gutted the seed)",
                    listed()
                ));
            }
        }
        ArchScope::Universal => {
            if !covered.contains("aarch64-apple-darwin") {
                return Err(format!(
                    "the seed carries NO aarch64-apple-darwin artifacts (targets: {}) — that is the \
                     architecture of essentially every Mac this will install on, so the seal would \
                     cost every downloader ~600 MB and install nothing for almost all of them. \
                     Restage from a registry packed for this triple \
                     (`tools/atpkg-seed-from-published.sh`), or cut deliberately seedless with \
                     ATERM_SEEDLESS=1. This one is not acknowledgeable.",
                    listed()
                ));
            }
            // Headline, then the ACT, then the facts one per line. Every fact that was in the
            // 120-word run-on paragraph is still here: no x86 artifacts, which targets ARE
            // covered, that an Intel Mac installs nothing from the seal, the client's exact
            // marker, the upstream state, and the ack.
            //
            // What MOVED is the acknowledgement, from word 91 to line 2. Skimming stopped at "the
            // seed carries NO x86_64", which reads as a refusal — so the operator went looking for
            // what had failed, rather than for the one word that lets a warning proceed.
            if !covered.contains("x86_64-apple-darwin")
                && !std::env::var("ATERM_SEED_ARCH_ACK").is_ok_and(|v| v.trim() == "1")
            {
                warnings.push(format!(
                    "WARNING — no x86_64-apple-darwin artifacts in the seal (targets: {})\n\
                     since atpkg index build 14 this is a STALE STAGE, not an upstream \
                     limit — restage from a current index: INDEX_BUILD=<N> \
                     tools/atpkg-seed-from-published.sh (N >= 14); acknowledge a deliberately \
                     arm64-only seal with ATERM_SEED_ARCH_ACK=1 (a warning-mute, not a gate: \
                     the cut proceeds either way)\n\
                     · an Intel Mac installs NOTHING from this seal — the client says so and \
                     discards it (`seed-unusable: no build for this Mac's architecture`)\n\
                     · the published registry carries x86_64-apple-darwin artifacts for EVERY \
                     pinned program since index 14 (the rustc coherence group included — \
                     pkg-trust-6808.toml carries the row), so restaging puts that slice in \
                     the seal\n\
                     · an arm64-only seal also cuts no aterm-<v>-x86_64.dmg and omits the \
                     manifest's dmg_x86_64 keys — Intel installs quietly keep the lean zip",
                    listed()
                ));
            }
        }
    }

    // ---- 3. nothing unaccounted rides the seal ---------------------------
    let extras: Vec<&String> = names.difference(&accounted).collect();
    if !extras.is_empty() {
        return Err(format!(
            "unaccounted file(s) in the seed: {} — every sealed byte must be named by a \
             signed manifest",
            extras
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Ok((
        SeedStat {
            dir: dir.to_path_buf(),
            files: names.len(),
            bytes,
            index_build: index.index_build,
            valid_until: effective_valid_until,
            roster_seq: index.roster_seq(),
            programs: pins,
            targets: seen_targets,
            warnings,
        },
        assets_by_target,
    ))
}

/// The minimum shelf life a sealed seed must still have at cut time, in
/// seconds. Default 30 days; `ATERM_SEED_MIN_DAYS` overrides it (`0` restores
/// the old lapsed-only behaviour, for an emergency cut where a short-lived
/// seed genuinely beats none).
///
/// 30 days is not a proof, it is a floor: a DMG that expires sooner than the
/// next plausible release cadence is a DMG whose batteries go flat while it is
/// still the current download.
fn min_shelf_secs() -> Result<i64, String> {
    const DEFAULT_DAYS: i64 = 30;
    let days = match std::env::var("ATERM_SEED_MIN_DAYS") {
        Ok(v) => v
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("ATERM_SEED_MIN_DAYS={v:?} is not a whole number of days"))
            .and_then(|d| {
                (d >= 0)
                    .then_some(d)
                    .ok_or_else(|| format!("ATERM_SEED_MIN_DAYS={d} is negative"))
            })?,
        Err(_) => DEFAULT_DAYS,
    };
    Ok(days.saturating_mul(86_400))
}

/// One phrasing for both freshness legs, so the operator is told which horizon
/// is short, by how much it must move, and how to proceed anyway.
fn freshness_err(what: &str, valid_until: &str, margin: i64, already_lapsed: bool) -> String {
    let days = margin / 86_400;
    if already_lapsed {
        return format!(
            "{what} freshness LAPSED ({valid_until}) — the shipped client refuses it, so this \
             seed would be dead weight in every DMG.\n\
             restage:  tools/atpkg-seed-from-published.sh\n\
             \x20         (stages the NEWEST published index; set INDEX_BUILD=<build> only to \
             stage an older one deliberately — it seals that build into every DMG)"
        );
    }
    format!(
        "{what} expires {valid_until}, inside this cut's {days}-day minimum shelf life — the \
         DMG would outlive its own batteries and every later first run would silently fall \
         back to a network-only bootstrap. Republish the index with a longer horizon and \
         restage, or lower the floor deliberately with ATERM_SEED_MIN_DAYS=<days>."
    )
}

/// The roster's `valid_until`, for the RECORD (the gate itself runs through
/// `TrustedRoster::still_fresh`, never through this string).
///
/// Parsing after the fact is sound here and only here: these exact bytes
/// already passed the master signature inside `select_index`, so this re-reads
/// something proven rather than trusting something new. It is a display value —
/// a parse failure is reported, never silently defaulted.
fn read_roster_valid_until(dir: &Path) -> Result<String, String> {
    let path = dir.join("aterm-machines.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("aterm-machines.toml parse (after verify): {e}"))?;
    parsed
        .get("valid_until")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "aterm-machines.toml: no valid_until".to_string())
}

/// Wall clock as Unix seconds, `i64::MAX` when unreadable — the same
/// fail-closed sentinel the client's freshness gates use (everything reads
/// stale, nothing validates).
fn now_unix() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(_) => i64::MAX,
    }
}

/// Minimal strict RFC3339 `YYYY-MM-DDTHH:MM:SSZ` → Unix seconds (UTC only —
/// exactly the shape `tools/atpkg-index.sh` writes). Inverse of
/// [`crate::bundle::epoch_to_rfc3339`]'s civil math. `None` on any deviation.
pub fn rfc3339_to_unix(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    if b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> { s.get(r)?.parse().ok() };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // days_from_civil (Howard Hinnant), day 0 = 1970-01-01.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj / 400;
    let yoe = y_adj % 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe;
    days.checked_sub(719_468)
        .map(|days| days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_the_bundle_formatter() {
        for epoch in [0u64, 86_399, 86_400, 1_753_920_000, 4_102_444_800] {
            let s = crate::bundle::epoch_to_rfc3339(epoch);
            assert_eq!(rfc3339_to_unix(&s), Some(epoch), "epoch {epoch} via {s}");
        }
        assert_eq!(rfc3339_to_unix("2026-07-30"), None);
        assert_eq!(rfc3339_to_unix("2026-07-30T99:00:00Z"), None);
        assert_eq!(rfc3339_to_unix("2026-07-30T00:00:00+01:00"), None);
    }

    #[test]
    fn validate_refuses_junk_and_an_incomplete_quad() {
        let d = std::env::temp_dir().join(format!("seedpack-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // No index at all → refused before any crypto.
        std::fs::write(d.join("stray.bin"), b"x").unwrap();
        let err = validate(&d).unwrap_err();
        assert!(
            err.contains("signed quad") || err.contains("paper master"),
            "{err}"
        );
        // An index PAIR without the roster pair is still not a registry under
        // the one-root model.
        std::fs::write(d.join("index.toml"), b"schema = 2").unwrap();
        std::fs::write(d.join("index.toml.sig"), [0u8; 64]).unwrap();
        let err = validate(&d).unwrap_err();
        assert!(
            err.contains("signed quad") || err.contains("paper master"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
