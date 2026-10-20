// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The gap to main, for a bundle whose pin is held to its repo's origin/main
//! ([`crate::programs::BundleDef::main_gap`]: trust). Trust is cut at an origin/main
//! commit, not from a signed release, so a pin ahead of the newest signed release can
//! still be far behind main. A report only: it holds no group and packs nothing. The cut
//! is a run of its own ([`trust_cut_commands`]), which the agent running it performs on a
//! release-credential machine, the release and the uploads included (Trust DECISION_QUEUE
//! D27, amended 2026-10-16).
//!
//! A build number counts a commit's whole ancestry, so `main's build - the pin's` is what
//! the pin lacks only when the pin is on main. When the clone has the pin's commit (its
//! trustc's `commit-hash`), the gap is counted on the graph (`<pin>..main` behind,
//! `main..<pin>` its own) and a pin off main is said to be off it; otherwise it falls back
//! to the build numbers, and says so.

use std::path::Path;

use crate::api::Build;
use crate::git;

/// The pin's own commit, as its installed compiler reports it (`-vV`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinCommit {
    /// The `commit-hash:` prefix (9 hex).
    pub commit: String,
    /// The `commit-date:` (`YYYY-MM-DD`), if reported.
    pub date: Option<String>,
}

/// Where the pin stands against main.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// Counted on the clone's graph.
    Graph {
        /// Commits main has that the pin lacks.
        behind: u64,
        /// Commits the pin has that main lacks.
        own: u64,
        /// Where they part (sha, build), when both are non-zero.
        fork: Option<(String, Build)>,
    },
    /// The pin's commit is not on the graph here: only the build numbers are known.
    ByNumber {
        /// Why the graph could not answer.
        why: String,
    },
}

/// One program's gap to its repo's main.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainGap {
    /// The program.
    pub program: String,
    /// Its published pin.
    pub pin: Build,
    /// What could be read.
    pub reading: Reading,
    /// The clone was not fetched this run (a dry run reads it as it stands).
    pub as_of_last_fetch: bool,
}

/// What the clone answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Nothing could be read; why.
    Unknown(String),
    /// Main and the pin, placed.
    Known {
        /// origin/main's commit.
        main_sha: String,
        /// Its build.
        main_build: Build,
        /// Its committer date.
        main_date: String,
        /// The pin's commit, when this host has its compiler.
        pin: Option<PinCommit>,
        /// Days between the pin's commit and main's.
        days: Option<i64>,
        /// Where the pin stands.
        place: Place,
    },
}

/// How much is owed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owed {
    /// Commits (or builds, by number) main has that the pin lacks.
    pub n: u64,
    /// The pin is off main (it has commits main lacks).
    pub off: bool,
}

/// Read the gap of `program` (pinned at `pin`) from `clone`, the program's scratch clone,
/// or `Err(why)` when there is none to read.
#[must_use]
pub fn read(
    program: &str,
    pin: Build,
    clone: std::result::Result<&Path, String>,
    pin_commit: Option<PinCommit>,
    as_of_last_fetch: bool,
) -> MainGap {
    let reading = match clone {
        Err(why) => Reading::Unknown(why),
        Ok(clone) => known(clone, pin_commit),
    };
    MainGap {
        program: program.to_string(),
        pin,
        reading,
        as_of_last_fetch,
    }
}

fn known(clone: &Path, pin_commit: Option<PinCommit>) -> Reading {
    const MAIN: &str = "refs/remotes/origin/main";
    let Ok(main_build) = git::rev_count(clone, MAIN) else {
        return Reading::Unknown(format!(
            "the clone {} has no origin/main to count",
            clone.display()
        ));
    };
    let main_sha = git::run(clone, &["rev-parse", MAIN]).unwrap_or_default();
    let main_date = git::run(clone, &["log", "-1", "--format=%cs", MAIN]).unwrap_or_default();
    let days = pin_commit
        .as_ref()
        .and_then(|p| p.date.as_deref())
        .and_then(|d| Some((day_unix(&main_date)? - day_unix(d)?) / 86_400));
    let full = pin_commit.as_ref().and_then(|p| {
        git::run(
            clone,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{}^{{commit}}", p.commit),
            ],
        )
        .ok()
        .filter(|s| !s.is_empty())
    });
    let place = match full {
        Some(full) => {
            let count = |range: String| {
                git::run(clone, &["rev-list", "--count", &range])
                    .ok()
                    .and_then(|n| n.parse::<u64>().ok())
            };
            match (
                count(format!("{full}..{main_sha}")),
                count(format!("{main_sha}..{full}")),
            ) {
                (Some(behind), Some(own)) => {
                    let fork = (behind > 0 && own > 0)
                        .then(|| git::run(clone, &["merge-base", &full, &main_sha]).ok())
                        .flatten()
                        .and_then(|f| Some((f.clone(), git::rev_count(clone, &f).ok()?)));
                    Place::Graph { behind, own, fork }
                }
                _ => Place::ByNumber {
                    why: format!(
                        "the clone lacks its commit {}",
                        pin_commit.as_ref().map_or("", |p| p.commit.as_str())
                    ),
                },
            }
        }
        None => Place::ByNumber {
            why: pin_commit.as_ref().map_or_else(
                || "no trustc of it here names its commit".to_string(),
                |p| format!("the clone lacks its commit {}", p.commit),
            ),
        },
    };
    Reading::Known {
        main_sha,
        main_build,
        main_date,
        pin: pin_commit,
        days,
        place,
    }
}

fn day_unix(day: &str) -> Option<i64> {
    aterm_types::rfc3339::parse_utc(&format!("{day}T00:00:00Z"))
}

fn short(sha: &str, n: usize) -> &str {
    &sha[..sha.len().min(n)]
}

impl MainGap {
    /// `TRUST-MAIN`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}-MAIN", self.program.to_uppercase())
    }

    /// Whether nothing could be read.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self.reading, Reading::Unknown(_))
    }

    /// What is owed, if a cut is.
    #[must_use]
    pub fn owed(&self) -> Option<Owed> {
        let Reading::Known {
            main_build, place, ..
        } = &self.reading
        else {
            return None;
        };
        match place {
            Place::Graph { behind, own, .. } if *behind > 0 => Some(Owed {
                n: *behind,
                off: *own > 0,
            }),
            Place::Graph { .. } => None,
            Place::ByNumber { .. } => (*main_build > self.pin).then(|| Owed {
                n: main_build - self.pin,
                off: false,
            }),
        }
    }

    /// Days between the pin's commit and main's, when both are known.
    #[must_use]
    pub fn days(&self) -> Option<i64> {
        match &self.reading {
            Reading::Known { days, .. } => *days,
            Reading::Unknown(_) => None,
        }
    }

    /// Main's commit, when read.
    #[must_use]
    pub fn main_sha(&self) -> Option<&str> {
        match &self.reading {
            Reading::Known { main_sha, .. } => Some(main_sha),
            Reading::Unknown(_) => None,
        }
    }

    /// Main's build, when read.
    #[must_use]
    pub fn main_build(&self) -> Option<Build> {
        match &self.reading {
            Reading::Known { main_build, .. } => Some(*main_build),
            Reading::Unknown(_) => None,
        }
    }

    /// `<n> builds behind`, or `off main, <n> commits behind`.
    #[must_use]
    pub fn words(&self) -> String {
        match self.owed() {
            Some(Owed { n, off: true }) => format!("off main, {n} commits behind"),
            Some(Owed { n, off: false }) => format!("{n} builds behind"),
            None => String::new(),
        }
    }

    /// The decision row's note: `; <words> <program> main <sha10> (<LABEL> below)`, or
    /// nothing when no cut is owed.
    #[must_use]
    pub fn row_note(&self) -> String {
        match (self.owed(), self.main_sha()) {
            (Some(_), Some(sha)) => format!(
                "; {} {} main {} ({} below)",
                self.words(),
                self.program,
                short(sha, 10),
                self.label()
            ),
            _ => String::new(),
        }
    }

    /// The once-a-day notice, when a cut is owed.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        let owed = self.owed()?;
        let days = self
            .days()
            .map(|d| format!(" ({d} days)"))
            .unwrap_or_default();
        let p = &self.program;
        Some(if owed.off {
            format!(
                "{p}: published build {} is off {p} main, {} commits{days} behind it — a cut is \
                 owed (ALAB-SYNC.md)",
                self.pin, owed.n
            )
        } else {
            format!(
                "{p}: published build {} is {} builds{days} behind {p} main — a cut is owed \
                 (ALAB-SYNC.md)",
                self.pin, owed.n
            )
        })
    }

    /// The `<LABEL>: …` line printed beside the decision table.
    #[must_use]
    pub fn line(&self) -> String {
        let label = self.label();
        let p = &self.program;
        let (main_sha, main_build, main_date, pin, days, place) = match &self.reading {
            Reading::Unknown(why) => return format!("{label}: unknown — {why}"),
            Reading::Known {
                main_sha,
                main_build,
                main_date,
                pin,
                days,
                place,
            } => (main_sha, main_build, main_date, pin, days, place),
        };
        let said = match pin {
            Some(PinCommit {
                commit,
                date: Some(d),
            }) => format!("the published {p} build {} ({commit}, {d})", self.pin),
            Some(PinCommit { commit, date: None }) => {
                format!("the published {p} build {} ({commit})", self.pin)
            }
            None => format!("the published {p} build {}", self.pin),
        };
        let owed = " — a cut is owed (ALAB-SYNC.md lists it)";
        let asof = if self.as_of_last_fetch {
            ", as of the last fetch of the clone"
        } else {
            ""
        };
        let days = days.map(|d| format!(", {d} days")).unwrap_or_default();
        let m10 = short(main_sha, 10);
        match place {
            Place::Graph { behind, own, fork } if *behind > 0 && *own > 0 => {
                let fork = fork
                    .as_ref()
                    .map(|(f, b)| format!(" (they part at {}, build {b})", short(f, 10)))
                    .unwrap_or_default();
                format!(
                    "{label}: {said} is OFF {p} main: main {m10} (build {main_build}, \
                     {main_date}) has {behind} commits it lacks, and it has {own} main does \
                     not{fork}{days}{owed}{asof}"
                )
            }
            Place::Graph { behind, .. } if *behind > 0 => format!(
                "{label}: {said} is {behind} builds behind {p} main {m10} (build {main_build}, \
                 {main_date}){days}{owed}{asof}"
            ),
            Place::Graph { own, .. } if *own > 0 => format!(
                "{label}: {said} is {own} commits past {p} main's build {main_build} ({m10}, \
                 {main_date}), which it contains — cut from a commit main does not hold yet; \
                 nothing owed{asof}"
            ),
            Place::Graph { .. } => format!("{label}: {said} is {p} main {m10} ({main_date}){asof}"),
            Place::ByNumber { why } if *main_build > self.pin => format!(
                "{label}: {said} is {} builds behind {p} main {m10} (build {main_build}, \
                 {main_date}){days}, by build number — {why}, so whether it is on main is not \
                 known{owed}{asof}",
                main_build - self.pin
            ),
            Place::ByNumber { why } if *main_build == self.pin => format!(
                "{label}: {said} has {p} main's build number ({m10}, {main_date}) — {why}, so \
                 whether it IS main is not known{asof}"
            ),
            Place::ByNumber { why } => format!(
                "{label}: {said} is past {p} main's build {main_build} ({m10}, {main_date}) by \
                 build number — {why}; nothing owed{asof}"
            ),
        }
    }
}

/// Trust main's cut, as trust's docs/CUTTING-A-TOOLCHAIN.md §3.9 runs it: mint, seal,
/// stage the trust-wp drivers into a copy of the seal (the directory that is packed), run
/// the staged pack's gates (`promote-toolchain.sh --gate-only`) and §3.9's first-step
/// conditions on that copy (the consumer check, and for P5a the seal-line audit), and only
/// then the archives' release, the re-pin push and the group publish. Placeholders: `<v>`
/// (the version the mint prints), `<rev>` (the seal `promote-toolchain.sh` names).
#[must_use]
pub fn trust_cut_commands(main_sha: &str) -> Vec<String> {
    let c12 = short(main_sha, 12);
    let arm = crate::triple::AARCH64_APPLE_DARWIN;
    let mint = format!("build/pack-mint-<v>-{arm}-{c12}");
    vec![
        "# on a release-credential machine (m2, m3), in a trust worktree no other lane builds in:".into(),
        "# §3.9's first step: this seal is cut only when DECISION_QUEUE.md's SEAL-1 holds, or for P5a SEAL-1-P5A's conditions, the seal-line audit and the consumer check among them, each checked on the staged pack below".into(),
        format!(
            "git -C $HOME/trust fetch origin && git -C $HOME/trust worktree add --detach $HOME/trust-cut-{c12} {main_sha} && cd $HOME/trust-cut-{c12}"
        ),
        "scripts/submodule_sync.sh".into(),
        "python3 scripts/recreate_bootstrap.py --force-config --no-atpkg --require-seed --no-build".into(),
        format!("ORACLE=~/.rustup/toolchains/1.97.1-{arm}   # the D16 stock oracle (§3.9)"),
        "scripts/mint_and_prove_seed.sh --check-only --ddc-seed \"$ORACLE\"".into(),
        "scripts/mint_and_prove_seed.sh --ddc-seed \"$ORACLE\"   # hours".into(),
        format!("RUSTUP_HOME=\"$PWD/{mint}/rustup\" scripts/promote-toolchain.sh {mint}/{arm}/stage2"),
        "# the pack: a copy of the seal with the five trust-wp drivers in its bin/ — the directory the publisher packs".into(),
        "SEAL=~/toolchains/trust-<rev> STAGE=~/toolchains/stage-trust-<rev>-wp".into(),
        "(cd first-party/trust-wp && RUSTC=\"$SEAL/bin/trustc\" \"$SEAL/bin/targo\" --unverified build --release --locked -p trust-wp-driver -p targo-trust-wp -p trust-wp-cli)".into(),
        "cp -ac \"$SEAL\" \"$STAGE\" && chmod u+w \"$STAGE/bin\" && (cd first-party/trust-wp/target/release && cp trust-wp trust-wp-driver trust-wp-rustc targo-trust-wp cargo-trust-wp \"$STAGE/bin/\")".into(),
        "# the staged pack's gates: promotion's UI and tool-start gates again, on this very directory; nothing below unless each passes".into(),
        "scripts/promote-toolchain.sh --gate-only \"$STAGE\"".into(),
        "# then the first step's conditions on \"$STAGE\", each result committed as its evidence; nothing below unless each holds:".into(),
        "# P5a's cut only (SEAL-1-P5A): the seal-line audit (SEAL-1 clause 4) of the deployed seal 321aaeda754's line, which its defaults name, so that commit must be in this clone (the local tag atpkg-trust-9192-seal)".into(),
        "python3 scripts/seal_line_audit.py   # exit 0 says the record covers the seal line; the condition holds only once each PORT and MEASURE row it lists is closed by a main commit".into(),
        "# the consumer check: aterm, ay, ty and clean, each at its origin/main, run the Rust steps of publish/config.sh's CHECK_CMD_DEFAULT with \"$STAGE\" as trust, from a scratch RUSTUP_HOME; each exits 0, or every red it shows is red under the published seal on the same tree".into(),
        format!("scripts/publish_seed_cut.sh --pack \"$STAGE\" --mint {mint}   # prints the archives' release and the re-pin push"),
        format!("scripts/publish_seed_cut.sh --pack \"$STAGE\" --mint {mint} --publish   # gh release create + git push origin seed/repin-{c12}"),
        "# once main contains the re-pin:".into(),
        "atpkg-publish publish-group rustc --seal \"$STAGE\" --dry-run".into(),
        "atpkg-publish publish-group rustc --seal \"$STAGE\" --upload".into(),
    ]
}
