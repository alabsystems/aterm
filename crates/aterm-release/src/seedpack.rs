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

use std::collections::BTreeSet;
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

/// Validate `dir` as a shippable seed by running the SHIPPED CLIENT'S first-run
/// resolution over it: `DirFetcher` candidates → paper-master roster admission →
/// index selection (no durable floors — exactly a fresh install's posture) →
/// per-pkg release delegation. Any failure is a hard error — a cut must never
/// seal a seed its own client would refuse.
pub fn validate(dir: &Path) -> Result<SeedStat, String> {
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
        return Err(freshness_err("index.toml", &index.valid_until, margin, now >= index_horizon));
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
        Some(r) if i64::try_from(r).unwrap_or(i64::MAX) < index_horizon => roster_valid_until.clone(),
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
                continue; // other-triple artifact not carried by this seed — fine
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
            present += 1;
        }
        if present == 0 {
            return Err(format!(
                "pinned {program}@{build} has no artifact present in the seed — it would be \
                 offered and then fail offline"
            ));
        }
    }

    // ---- 2b. arch coverage, the macOS twin of build.ps1's gate -----------
    // The .app is UNIVERSAL (arm64 + x86_64), but the registry is packed
    // host-triple-only — every published artifact today is
    // aarch64-apple-darwin. Sealing that into a universal DMG ships hundreds of
    // MB from which atpkg on an Intel Mac can install exactly nothing: every
    // pin clean-skips on triple (`artifact_for`), so the batteries are
    // silently absent on a machine that was promised them. The Windows lane
    // already refuses this; the mac lane must SAY it at minimum, because
    // "installs with all the binaries" is now the product claim.
    //
    // The two slices are NOT symmetric, and treating them alike was a real bug.
    //
    //  * NO aarch64 artifacts is a BROKEN CUT. That is the slice essentially every
    //    Mac sold since 2020 runs, so a seal without it delivers batteries to
    //    almost nobody while still costing every downloader ~600 MB. It is a hard
    //    refusal, and deliberately NOT mutable by `ATERM_SEED_ARCH_ACK` — the mute
    //    button used to cover this case too, which meant one env var could ship the
    //    one seal that is never right.
    //  * NO x86_64 artifacts is a known, currently-unavoidable state: nothing
    //    publishes an x86_64 lane yet. It warns, and the ack silences THAT and only
    //    that, because it is the only one an operator can honestly acknowledge.
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
    if !covered.contains("aarch64-apple-darwin") {
        return Err(format!(
            "the seed carries NO aarch64-apple-darwin artifacts (targets: {}) — that is the \
             architecture of essentially every Mac this will install on, so the seal would \
             cost every downloader ~600 MB and install nothing for almost all of them. \
             Restage from a registry packed for this triple \
             (`INDEX_BUILD=<N> tools/atpkg-seed-from-published.sh`), or cut deliberately \
             seedless with ATERM_SEEDLESS=1. This one is not acknowledgeable.",
            listed()
        ));
    }
    if !covered.contains("x86_64-apple-darwin")
        && !std::env::var("ATERM_SEED_ARCH_ACK").is_ok_and(|v| v.trim() == "1")
    {
        println!(
            "    WARNING: the seed carries NO x86_64-apple-darwin artifacts (targets: {}) — \
             an Intel Mac installs NOTHING from the seal. It does NOT fall back to a network \
             install either: the published index carries no x86_64 packages at all, so on \
             Intel there is no ALab toolchain by any route, and the client says so \
             (`seed-unusable: no build for this Mac's architecture`). The blocker is \
             upstream, not packaging: Trust has no x86_64-apple-darwin std, so the `trust` \
             and `trust-mc` sysroot bundles cannot be produced for that triple at all \
             (see buildplan.rs, the compat-slice comment). Acknowledge with \
             ATERM_SEED_ARCH_ACK=1.",
            listed()
        );
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

    Ok(SeedStat {
        dir: dir.to_path_buf(),
        files: names.len(),
        bytes,
        index_build: index.index_build,
        valid_until: effective_valid_until,
        roster_seq: index.roster_seq(),
        programs: pins,
    })
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
             seed would be dead weight in every DMG. Restage from a freshly published index \
             (`INDEX_BUILD=<N> tools/atpkg-seed-from-published.sh`)."
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
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
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
