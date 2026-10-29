// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor-direct lane (design §1.3–§1.5): one program's head, its decision and its landing,
//! run ahead of and apart from the index lane.

use std::cell::RefCell;
use std::path::Path;

use super::decide::{
    Decision, HeadView, InstallReason, Installed, InstalledView, KeepReason, decide,
};
use super::digests::{AuthenticatedDigest, DigestSeen, record_or_conflict};
use super::policy::Policy;
use super::record::{RECORD_SCHEMA, VendorRecord, complete_record};
use super::resolve::{self, Candidate, Head, Resolution};
use super::stamp::ProgramStamp;
use super::table::VendorSpec;
use super::version::{Version, is_vendor_build};
use crate::flow::{Fetcher, FlowError, InstallReport, Landing, VendorReport};
use crate::install::{StageError, StageHooks, TeamCheck};
use crate::openpgp::PinnedRsaKey;
use crate::sig::TrustedIndex;
use crate::store::Layout;

/// The anchors the lane verifies under.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Trust {
    /// Anthropic's release key: claude's digest anchor.
    pub claude_key: PinnedRsaKey,
    /// How one darwin Mach-O is judged against the program's Developer ID team.
    pub team_check: TeamCheck,
    /// npm's registry keys: grok's digest anchor.
    pub npm_keys: &'static [super::npm::NpmKey],
}

impl Trust {
    /// The compiled anchors: Anthropic's key, npm's registry keys and `/usr/bin/codesign`.
    pub(crate) const PRODUCTION: Self = Self {
        claude_key: crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY,
        team_check: crate::install::codesign_team_check,
        npm_keys: super::npm::NPM_REGISTRY_KEYS,
    };

    /// The keys [`resolve::verify`] holds documents to.
    pub(crate) const fn keys(&self) -> resolve::Keys<'_> {
        resolve::Keys {
            claude: &self.claude_key,
            npm: self.npm_keys,
        }
    }
}

/// Everything the lane reads besides the program.
pub(crate) struct Lane<'a> {
    /// The store.
    pub layout: &'a Layout,
    /// Every vendor fetch goes through its `vendor_*` methods.
    pub fetcher: &'a dyn Fetcher,
    /// The client's own triple: it, never a document, picks the platform.
    pub triple: &'a str,
    /// The signed yanks ([`Policy::current`]).
    pub policy: &'a Policy,
    /// The anchors.
    pub trust: &'a Trust,
    /// A verified index to read a legacy build's version from, when the caller holds one.
    pub legacy_index: Option<&'a TrustedIndex>,
    /// A person asked for the program by name (`update <program>`, `install <program>`,
    /// `claude update`), not a pass or the head watch.
    pub asked: bool,
    /// The clock (Unix seconds).
    pub now: i64,
    /// This landing's place in line for the link among lanes landing side by side
    /// ([`crate::flow::DownloadTurns`]); only its downloads wait for it (grok's bucket copy and
    /// its payload, in one turn), and the landing gives it up when it ends ([`land_one`]).
    pub download_turn: Option<&'a crate::flow::DownloadTicket<'a>>,
}

/// What the lane came to for one program.
#[derive(Debug, Clone)]
pub(crate) enum Verdict {
    /// A build was verified, staged, activated and shimmed.
    Installed {
        /// What was active before.
        from: Installed,
        /// The version now active.
        to: Version,
        /// Why it moved.
        reason: InstallReason,
        /// The tools shimmed.
        shimmed: Vec<String>,
        /// Exposed names refused a shim.
        refused_shims: Vec<String>,
        /// The root of the tree as staged (the `.vendor` record's).
        tree_root: String,
    },
    /// The installed version is the vendor's latest.
    Current(Version),
    /// Nothing moved, for `reason`.
    Kept {
        /// Why.
        reason: KeepReason,
        /// What the head named.
        head: Option<Version>,
        /// What stays active.
        keeping: Installed,
    },
    /// The vendor's release channel could not be reached, or did not serve a document; nothing
    /// moved and nothing was judged.
    Unreachable {
        /// What stays active.
        keeping: Installed,
        /// What the host answered instead, when it answered (a status, a TLS refusal).
        why: Option<String>,
    },
    /// A document or the payload was refused; nothing moved.
    Refused {
        /// The version refused, when the head named one.
        version: Option<Version>,
        /// Why, in one line.
        why: String,
        /// What stays active.
        keeping: Installed,
    },
    /// The installed version is yanked and the retained `to` was re-activated.
    RolledBack {
        /// The yanked build.
        from: Installed,
        /// The version now active.
        to: Version,
    },
    /// The installed version is yanked and nothing admissible exists: its commands fail.
    Tombstoned {
        /// The yanked build.
        from: Installed,
    },
    /// The verified build's platform signature check did not finish in time
    /// ([`StageError::VerifyDeferred`]): no verdict on the bytes and no fault, so nothing moved,
    /// nothing is memoized, its archive is kept, and the next pass checks it again.
    Deferred {
        /// The version whose check did not finish.
        version: Version,
        /// Which file, and codesign's words.
        why: String,
        /// What stays active.
        keeping: Installed,
    },
    /// Landing a verified build failed on this machine (a download, the disk, a shim).
    Failed {
        /// The version that did not land.
        version: Version,
        /// Why.
        error: String,
        /// What stays active.
        keeping: Installed,
    },
    /// The program is dev-linked: managed from its checkout.
    Linked,
    /// The active build is ALab's own ([`crate::alab::build_is_alab`]), the channel signs no pin
    /// this machine may install, and the signed policy revokes nothing of it: kept, and never
    /// decided against or replaced by the vendor's head, asked or not ([`crate::alab`]'s source
    /// rule).
    AlabKept {
        /// How a line names the active build ([`crate::alab::build_words`]).
        what: String,
    },
    /// The kept ALab build's missing commands could not be restored on this machine.
    AlabRestoreFailed {
        /// The retained build whose commands were not laid.
        what: String,
        /// The restoration I/O error.
        error: String,
    },
    /// The active ALab build is revoked (a signed yank names it, or it is below the signed floor)
    /// and the newest retained ALab build below it that nothing revokes is active now.
    AlabRolledBack {
        /// How a line names the revoked build.
        from: String,
        /// The build now active.
        to_build: u64,
        /// How a line names it.
        to: String,
    },
    /// The active ALab build is revoked and no retained ALab build is admissible: its commands
    /// are disabled, as the index lane disables a revoked build ([`crate::gate`]), until the
    /// channel pins one this machine may install or a person chooses the vendor's build
    /// (`aterm pkg update <program> --vendor`).
    AlabTombstoned {
        /// How a line names the revoked build.
        what: String,
        /// Why it is revoked.
        why: Revoked,
    },
    /// The vendor publishes no build the lane can verify for this machine
    /// ([`VendorSpec::serves`]): nothing was asked of any host.
    Unavailable {
        /// The canonical `unavailable on <target>: <hint>` state.
        state: String,
    },
}

/// One program's lane result.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    /// The program.
    pub spec: &'static VendorSpec,
    /// What happened.
    pub verdict: Verdict,
    /// The per-version staging name of a payload the lane resolved, so a pass-end gc spares its
    /// `.part`.
    pub asset: Option<String>,
}

/// Why the signed policy revokes an ALab build ([`Revoked::of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Revoked {
    /// A signed yank names it.
    Yanked,
    /// It is below the channel's signed build floor.
    BelowFloor,
}

impl Revoked {
    fn of(policy: &Policy, program: &str, build: u64) -> Option<Self> {
        if policy.is_legacy_yanked(program, build) {
            Some(Self::Yanked)
        } else if build < policy.alab_floor(program) {
            Some(Self::BelowFloor)
        } else {
            None
        }
    }

    const fn words(self) -> &'static str {
        match self {
            Self::Yanked => "is yanked",
            Self::BelowFloor => "is below the ALab channel's build floor",
        }
    }
}

impl Outcome {
    /// The one line the lane owes the terminal, naming versions, never a store build id.
    #[must_use]
    pub(crate) fn line(&self) -> String {
        verdict_line(self.spec, &self.verdict)
    }

    /// Why the vendor's head is not what runs, for a row that keeps the active build (a hold, a
    /// yank, a newer install, a rollback, an unreached channel, an unfinished signature check, a
    /// kept ALab build): no fault, and never a claim to be the latest.
    #[must_use]
    pub(crate) fn kept_clause(&self) -> Option<String> {
        let vendor = self.spec.vendor;
        let latest = format!("{vendor}'s latest");
        Some(match &self.verdict {
            Verdict::Kept { reason, head, .. } => {
                let h = head.map_or_else(|| String::from("?"), |v| v.to_string());
                match reason {
                    KeepReason::Held => String::from("held by local pin"),
                    KeepReason::HeadOlder => format!("newer than {latest} ({h})"),
                    KeepReason::HeadYanked => format!("{latest} ({h}) is yanked"),
                    KeepReason::BelowFloor => {
                        format!("{latest} ({h}) is older than this aterm's floor")
                    }
                    KeepReason::BelowHighWater => {
                        format!("{latest} ({h}) is older than a version this machine ran")
                    }
                    KeepReason::MajorJump => {
                        format!("{latest} ({h}) is more than one major version ahead")
                    }
                    KeepReason::BuildDateRegressed => format!("{latest} ({h}) was built before it"),
                    KeepReason::LegacyUnread => format!(
                        "its version could not be read; `aterm pkg update {}` moves it to \
                         {latest} ({h})",
                        self.spec.program
                    ),
                    KeepReason::UpToDate | KeepReason::NoHead => return None,
                }
            }
            Verdict::RolledBack { from, .. } => {
                format!("rolled back from yanked {}", installed_words(*from))
            }
            Verdict::Unreachable { .. } => format!("{vendor}'s release channel was not reached"),
            Verdict::Deferred { version, .. } => {
                format!("{latest} ({version}) waits for its signature check to finish")
            }
            Verdict::AlabKept { what } => {
                format!("{what}; the ALab channel pins no build this machine may install")
            }
            Verdict::AlabRolledBack { from, to, .. } => {
                format!("{to}; rolled back from {from}, which the ALab channel revoked")
            }
            _ => return None,
        })
    }

    /// Whether this is a fault the caller counts (a refusal, a failed landing, a tombstone).
    #[must_use]
    pub(crate) const fn is_failure(&self) -> bool {
        matches!(
            self.verdict,
            Verdict::Refused { .. }
                | Verdict::Failed { .. }
                | Verdict::Tombstoned { .. }
                | Verdict::AlabTombstoned { .. }
                | Verdict::AlabRestoreFailed { .. }
        )
    }

    /// Convert this outcome into an [`InstallReport`] or [`FlowError`].
    ///
    /// # Errors
    /// The lane installed nothing it could stand behind: its line says why.
    pub(crate) fn into_install(self, layout: &Layout) -> Result<InstallReport, FlowError> {
        let program = self.spec.program;
        let line = self.line();
        let report =
            |build: u64, already_current: bool, shimmed, refused_shims, tree_root| InstallReport {
                program: program.to_string(),
                build,
                index_build: 0,
                roster_seq: 0,
                already_current,
                shimmed,
                refused_shims,
                tree_root,
                dependencies: Vec::new(),
                vendor: Some(VendorReport {
                    version: self.active_version().map(|v| v.to_string()),
                    vendor: self.spec.vendor.to_string(),
                    line: line.clone(),
                }),
            };
        let active = crate::ops::active_builds(layout).get(program).copied();
        match &self.verdict {
            Verdict::Installed {
                to,
                shimmed,
                refused_shims,
                tree_root,
                ..
            } => Ok(report(
                to.build_id(),
                false,
                shimmed.clone(),
                refused_shims.clone(),
                tree_root.clone(),
            )),
            Verdict::RolledBack { to, .. } => Ok(report(
                to.build_id(),
                false,
                Vec::new(),
                Vec::new(),
                String::new(),
            )),
            Verdict::AlabRolledBack { to_build, .. } => Ok(report(
                *to_build,
                false,
                Vec::new(),
                Vec::new(),
                String::new(),
            )),
            Verdict::Current(_) | Verdict::Kept { .. } | Verdict::AlabKept { .. }
                if active.is_some() =>
            {
                Ok(report(
                    active.unwrap_or_default(),
                    true,
                    Vec::new(),
                    Vec::new(),
                    String::new(),
                ))
            }
            Verdict::Linked => Err(FlowError::Linked(program.to_string())),
            Verdict::Unreachable { .. } => Err(FlowError::VendorUnreachable(
                line.strip_prefix("atpkg: ").unwrap_or(&line).to_string(),
            )),
            Verdict::Unavailable { state } => Err(FlowError::VendorUnavailable(state.clone())),
            _ => Err(FlowError::Vendor(
                line.strip_prefix("atpkg: ").unwrap_or(&line).to_string(),
            )),
        }
    }

    /// The version active when the lane finished, when one is known.
    #[must_use]
    pub(crate) const fn active_version(&self) -> Option<Version> {
        match &self.verdict {
            Verdict::Installed { to, .. } | Verdict::RolledBack { to, .. } => Some(*to),
            Verdict::Current(v) => Some(*v),
            Verdict::Kept { keeping, .. }
            | Verdict::Unreachable { keeping, .. }
            | Verdict::Deferred { keeping, .. }
            | Verdict::Refused { keeping, .. }
            | Verdict::Failed { keeping, .. } => keeping.version(),
            Verdict::Tombstoned { .. }
            | Verdict::Linked
            | Verdict::Unavailable { .. }
            | Verdict::AlabKept { .. }
            | Verdict::AlabRolledBack { .. }
            | Verdict::AlabTombstoned { .. }
            | Verdict::AlabRestoreFailed { .. } => None,
        }
    }
}

/// A verified candidate waiting for its bytes ([`land_one`]).
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    spec: &'static VendorSpec,
    candidate: Candidate,
    reason: InstallReason,
    installed: Installed,
    previous: Option<InstalledView>,
    active: Option<u64>,
    held: bool,
    high_water: Option<Version>,
    stamp: ProgramStamp,
}

impl Pending {
    /// The program.
    #[must_use]
    pub(crate) const fn program(&self) -> &'static str {
        self.spec.program
    }

    /// The exact download size, from the verified documents: the payload, and the vendor's own
    /// copy it is checked against (grok's bucket `.zst`).
    #[must_use]
    pub(crate) fn download_bytes(&self) -> u64 {
        let companion = self.candidate.companion.as_ref().map_or(0, |c| c.size);
        self.candidate
            .artifact
            .artifact()
            .size
            .saturating_add(companion)
    }

    /// What stays on disk once it lands, when the vendor says exactly (a raw binary is its
    /// download, an npm binary package's binary the size its vendor's own copy has; an archive's
    /// unpacked size is not published).
    #[must_use]
    pub(crate) fn disk_bytes(&self) -> Option<u64> {
        let a = self.candidate.artifact.artifact();
        match a.payload.as_str() {
            "raw-binary" => Some(a.size),
            crate::vendor::NPM_BROTLI_BINARY => Some(a.cost.disk_installed),
            _ => None,
        }
    }

    /// What its landing's disk gate checks: the downloads and the tree it unpacks to, an
    /// archive's at its unpack bound.
    #[must_use]
    pub(crate) fn landing_bytes(&self) -> crate::flow::LandingBytes {
        let companion = crate::flow::LandingBytes {
            asset: self.candidate.companion.as_ref().map_or(0, |c| c.size),
            tree: 0,
        };
        crate::flow::LandingBytes::of(self.candidate.artifact.artifact()).and(companion)
    }
}

/// [`plan_one`]'s answer.
#[derive(Debug, Clone)]
pub(crate) enum Step {
    /// Nothing to fetch; the lane is done.
    Done(Outcome),
    /// A verified candidate to land.
    Land(Box<Pending>),
}

/// [`plan_one`] then, when it names a candidate, [`land_one`].
pub(crate) fn install_one(lane: &Lane<'_>, spec: &'static VendorSpec, held: bool) -> Outcome {
    match plan_one(lane, spec, held) {
        Step::Done(outcome) => outcome,
        Step::Land(pending) => land_one(lane, &pending),
    }
}

/// What the lane read of one program before its pass's index landed ([`read_ahead`]): the head
/// and, when it named a version other than the installed one, that version's verified documents.
#[derive(Debug, Clone)]
pub(crate) struct ReadAhead {
    head: Head,
    verified: Option<(Version, Resolution)>,
}

/// The network half of [`plan_one`], which needs no index: `spec`'s head, conditional on its
/// stamp's validator, and, when it names a version other than the installed one of a program no
/// local pin holds, that version's documents, verified ([`resolve::verify`] writes nothing).
/// `None` for a dev-linked program, a kept ALab build, or a triple the vendor does not serve.
pub(crate) fn read_ahead(
    layout: &Layout,
    spec: &'static VendorSpec,
    fetcher: &dyn Fetcher,
    triple: &str,
    trust: &Trust,
    now: i64,
) -> Option<ReadAhead> {
    let program = spec.program;
    if crate::linkmode::is_linked(layout, program)
        || (!crate::alab::vendor_chosen(layout, program)
            && crate::alab::active_alab_build(layout, program, triple).is_some())
        || !spec.serves(triple)
    {
        return None;
    }
    let active = crate::ops::active_builds(layout).get(program).copied();
    let stamp = ProgramStamp::read(layout, program).unwrap_or_default();
    let etag = stamp.last_head.and(stamp.etag.clone());
    let head = resolve::read_head(spec, fetcher, etag.as_deref());
    let verified = match &head {
        Head::Named { version, body, .. }
            if installed_of(layout, program, active).version() != Some(*version)
                && !crate::pin::is_pinned(layout, program) =>
        {
            let verified =
                resolve::verify(spec, triple, fetcher, *version, body, trust.keys(), now);
            Some((*version, verified))
        }
        _ => None,
    };
    Some(ReadAhead { head, verified })
}

/// The reads ahead of a pass's vendor plans, one slot per program of the table, each filled by a
/// thread of its own ([`Reads::read`]) while the pass resolves its index, and taken by that
/// program's plan ([`Reads::take_read`]).
pub(crate) struct Reads {
    answers: std::sync::Mutex<Vec<ReadSlot>>,
    changed: std::sync::Condvar,
}

enum ReadSlot {
    /// Out on the network.
    Reading,
    /// In: what was read (`None`, nothing to read, or a thread that never ran it).
    Read(Option<ReadAhead>),
    /// Its plan has it.
    Taken,
}

/// A read its plan stopped waiting for ([`Reads::take_read`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StillReading;

impl Reads {
    /// Every program of the table still reading.
    pub(crate) fn new() -> Self {
        Self {
            answers: std::sync::Mutex::new(
                super::VENDORS.iter().map(|_| ReadSlot::Reading).collect(),
            ),
            changed: std::sync::Condvar::new(),
        }
    }

    /// Fill `program`'s slot with `read`'s answer, on the thread that reads it.
    pub(crate) fn read(&self, program: &str, read: impl FnOnce() -> Option<ReadAhead>) {
        struct Unread<'a>(&'a Reads, Option<usize>);
        impl Drop for Unread<'_> {
            fn drop(&mut self) {
                if let Some(i) = self.1 {
                    self.0.fill(i, None);
                }
            }
        }
        let Some(i) = super::VENDORS.iter().position(|s| s.program == program) else {
            return;
        };
        let mut unread = Unread(self, Some(i));
        let answer = read();
        unread.1 = None;
        self.fill(i, answer);
    }

    fn fill(&self, i: usize, answer: Option<ReadAhead>) {
        if let Ok(mut slots) = self.answers.lock()
            && matches!(slots[i], ReadSlot::Reading)
        {
            slots[i] = ReadSlot::Read(answer);
        }
        self.changed.notify_all();
    }

    /// `program`'s read, waited for until `until` (with `None`, for as long as it takes): what was
    /// read, or [`StillReading`] at `until`.
    pub(crate) fn take_read(
        &self,
        program: &str,
        until: Option<std::time::Instant>,
    ) -> Result<Option<ReadAhead>, StillReading> {
        let Some(i) = super::VENDORS.iter().position(|s| s.program == program) else {
            return Ok(None);
        };
        let Ok(mut slots) = self.answers.lock() else {
            return Ok(None);
        };
        loop {
            match std::mem::replace(&mut slots[i], ReadSlot::Taken) {
                ReadSlot::Read(read) => return Ok(read),
                ReadSlot::Taken => return Ok(None),
                ReadSlot::Reading => slots[i] = ReadSlot::Reading,
            }
            slots = match until {
                None => self
                    .changed
                    .wait(slots)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Some(until) => {
                    let left = until.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Err(StillReading);
                    }
                    self.changed
                        .wait_timeout(slots, left)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0
                }
            };
        }
    }
}

/// The lane for `spec` short of payload bytes: the installed build, the head (a 304 when it has
/// not moved), the decision, and for an install the verified candidate.
pub(crate) fn plan_one(lane: &Lane<'_>, spec: &'static VendorSpec, held: bool) -> Step {
    plan_one_with(lane, spec, held, None)
}

/// [`plan_one`] over what [`read_ahead`] read of `spec` before the pass's index landed: its head
/// instead of asking again, and its verified documents when the decision installs the version
/// they were verified for. The decision itself is made here, under the yanks the fresh index
/// binds.
pub(crate) fn plan_one_with(
    lane: &Lane<'_>,
    spec: &'static VendorSpec,
    held: bool,
    ahead: Option<ReadAhead>,
) -> Step {
    let layout = lane.layout;
    let program = spec.program;
    let done = |verdict: Verdict| {
        Step::Done(finish(
            lane,
            Outcome {
                spec,
                verdict,
                asset: None,
            },
        ))
    };
    if crate::linkmode::is_linked(layout, program) {
        return Step::Done(Outcome {
            spec,
            verdict: Verdict::Linked,
            asset: None,
        });
    }
    if !crate::alab::vendor_chosen(layout, program)
        && let Some(build) = crate::alab::active_alab_build(layout, program, lane.triple)
    {
        return done(keep_alab(lane, spec, build));
    }
    if !spec.serves(lane.triple) {
        let hint = format!(
            "{} publishes no {} build aterm can verify for this target",
            spec.vendor, spec.product
        );
        return Step::Done(Outcome {
            spec,
            verdict: Verdict::Unavailable {
                state: crate::state::unavailable(lane.triple, &hint),
            },
            asset: None,
        });
    }
    let active = crate::ops::active_builds(layout).get(program).copied();
    let stamp = ProgramStamp::read(layout, program).unwrap_or_default();
    let mut installed = installed_of(layout, program, active);
    if let Installed::Legacy {
        build,
        version: None,
    } = installed
    {
        installed = Installed::Legacy {
            build,
            version: stamp.legacy_version_of(build),
        };
    }
    let previous = previous_of(layout, lane.policy, program, installed);
    let mut legacy_asked = false;
    if let Some(build) = legacy_root_owed(layout, program, active) {
        let facts = legacy_facts(lane, program, build);
        if let Some((version, root)) = &facts {
            keep_legacy_facts(layout, program, build, *version, root.as_deref());
        }
        if let Installed::Legacy {
            build: active_build,
            version: None,
        } = installed
            && active_build == build
        {
            legacy_asked = true;
            if let Some((version, _)) = facts {
                installed = Installed::Legacy {
                    build,
                    version: Some(version),
                };
            }
        }
    }
    let etag = stamp.last_head.and(stamp.etag.clone());
    let (mut head, mut verified) = match ahead {
        Some(read) => (read.head, read.verified),
        None => (
            resolve::read_head(spec, lane.fetcher, etag.as_deref()),
            None,
        ),
    };
    let mut refetched = false;
    loop {
        let (head_version, body) = match &head {
            Head::Named {
                version,
                etag,
                body,
            } => {
                let _ = ProgramStamp::record_check(
                    layout,
                    program,
                    lane.now,
                    Some(*version),
                    etag.as_deref(),
                );
                (Some(*version), Some(body.clone()))
            }
            Head::Unchanged { etag } => {
                let _ = ProgramStamp::record_check(
                    layout,
                    program,
                    lane.now,
                    stamp.last_head,
                    Some(etag),
                );
                (stamp.last_head, None)
            }
            Head::Unreachable(_) | Head::Unserved(_) | Head::Refused(_) => (None, None),
        };
        let mut inputs = super::decide::Inputs {
            program,
            installed,
            previous,
            head: head_version.map(|version| HeadView {
                version,
                build_date: None,
            }),
            policy: lane.policy,
            floor: spec.floor,
            high_water: stamp.high_water,
            held,
            legacy_ceiling: spec.legacy_ceiling,
            asked: lane.asked,
        };
        let mut decision = decide(&inputs);
        if let Installed::Legacy {
            build,
            version: None,
        } = installed
            && !legacy_asked
            && (matches!(
                decision,
                Decision::Install {
                    reason: InstallReason::ReplacesLegacy,
                    ..
                } | Decision::Keep(KeepReason::LegacyUnread)
            ) || !lane.policy.yanked_of(program).is_empty())
        {
            legacy_asked = true;
            if let Some((version, root)) = legacy_facts(lane, program, build) {
                keep_legacy_facts(layout, program, build, version, root.as_deref());
                installed = Installed::Legacy {
                    build,
                    version: Some(version),
                };
                inputs.installed = installed;
                decision = decide(&inputs);
            }
        }
        match decision {
            Decision::Keep(KeepReason::NoHead) => return done(unread_head(&head, installed)),
            Decision::Keep(KeepReason::UpToDate) => {
                return done(match installed.version() {
                    Some(v) => Verdict::Current(v),
                    None => Verdict::Unreachable {
                        keeping: installed,
                        why: None,
                    },
                });
            }
            Decision::Keep(reason) => {
                return done(Verdict::Kept {
                    reason,
                    head: head_version,
                    keeping: installed,
                });
            }
            Decision::RollbackTo(to) => {
                return done(roll_back(lane, program, installed, active, previous, to));
            }
            Decision::Tombstone if head_version.is_none() => {
                return done(unread_head(&head, installed));
            }
            Decision::Tombstone => {
                crate::flow::install_tombstone_shims(layout, program, active);
                return done(Verdict::Tombstoned { from: installed });
            }
            Decision::Install { version, reason } => {
                let Some(body) = body else {
                    if refetched {
                        return done(Verdict::Unreachable {
                            keeping: installed,
                            why: None,
                        });
                    }
                    refetched = true;
                    head = resolve::reread_head(spec, lane.fetcher);
                    continue;
                };
                let resolution = match verified.take() {
                    Some((read, resolution)) if read == version => resolution,
                    _ => resolve::verify(
                        spec,
                        lane.triple,
                        lane.fetcher,
                        version,
                        &body,
                        lane.trust.keys(),
                        lane.now,
                    ),
                };
                let pending = Pending {
                    spec,
                    candidate: match resolution {
                        Resolution::Candidate(c) => *c,
                        Resolution::Unreachable => {
                            let v = Verdict::Unreachable {
                                keeping: installed,
                                why: None,
                            };
                            return done(yank_fallback(lane, &inputs, reason, active, v, false));
                        }
                        Resolution::Unserved(why) => {
                            let v = Verdict::Unreachable {
                                keeping: installed,
                                why: Some(why),
                            };
                            return done(yank_fallback(lane, &inputs, reason, active, v, false));
                        }
                        Resolution::Refused {
                            version,
                            reason: why,
                        } => {
                            let v = Verdict::Refused {
                                version,
                                why,
                                keeping: installed,
                            };
                            return done(yank_fallback(lane, &inputs, reason, active, v, true));
                        }
                    },
                    reason,
                    installed,
                    previous,
                    active,
                    held,
                    high_water: stamp.high_water,
                    stamp: stamp.clone(),
                };
                return admit_candidate(lane, pending, &inputs);
            }
        }
    }
}

fn admit_candidate(lane: &Lane<'_>, pending: Pending, inputs: &super::decide::Inputs<'_>) -> Step {
    let spec = pending.spec;
    let c = &pending.candidate;
    let asset = Some(c.artifact.artifact().asset.clone());
    let done = |verdict: Verdict| {
        Step::Done(finish(
            lane,
            Outcome {
                spec,
                verdict,
                asset: asset.clone(),
            },
        ))
    };
    let dated = super::decide::Inputs {
        head: Some(HeadView {
            version: c.version,
            build_date: c.build_date,
        }),
        ..*inputs
    };
    match decide(&dated) {
        Decision::Install { version, .. } if version == c.version => {}
        Decision::Keep(reason) => {
            return done(Verdict::Kept {
                reason,
                head: Some(c.version),
                keeping: pending.installed,
            });
        }
        _ => {
            return done(Verdict::Refused {
                version: Some(c.version),
                why: String::from("the verified build no longer decides as an install"),
                keeping: pending.installed,
            });
        }
    }
    let fall_back = |v: Verdict, head_refused: bool| {
        done(yank_fallback(
            lane,
            inputs,
            pending.reason,
            pending.active,
            v,
            head_refused,
        ))
    };
    let refuse = |why: String| Verdict::Refused {
        version: Some(c.version),
        why,
        keeping: pending.installed,
    };
    let digest = c.vouch.digest();
    let record = c.anchor.vouches_before_download();
    match record_or_conflict(
        lane.layout,
        spec.program,
        c.version,
        lane.triple,
        digest,
        record,
    ) {
        Ok(DigestSeen::Conflict { first }) => {
            let v = refuse(recut(c, &first));
            let _ = ProgramStamp::record_refusal(
                lane.layout,
                spec.program,
                &verdict_line(spec, &v),
                c.version,
                digest,
            );
            return fall_back(v, true);
        }
        Ok(DigestSeen::New | DigestSeen::Same) => {}
        Err(e) => {
            let v = Verdict::Failed {
                version: c.version,
                error: format!("the first-seen digest log could not be checked: {e}"),
                keeping: pending.installed,
            };
            return fall_back(v, false);
        }
    }
    if pending.stamp.is_refused(c.version, digest) {
        return fall_back(refuse(String::from(REFUSED_BYTES)), true);
    }
    if let Err(e) = resolve::admit(&c.artifact, &c.vouch) {
        return fall_back(refuse(e.to_string()), true);
    }
    Step::Land(Box::new(pending))
}

const REFUSED_BYTES: &str = "an earlier pass refused these exact bytes; they are not fetched \
                             again until the vendor's head or digest changes";

fn recut(c: &Candidate, first: &AuthenticatedDigest) -> String {
    format!(
        "its {} differs from the one first seen for {} ({}) — a re-cut release is never an \
         update",
        c.anchor.digest_alg(),
        c.version,
        first.as_str()
    )
}

/// What xAI's bucket copy of a build decoded to ([`agree`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Agreement {
    /// The bucket `.zst` it decoded from.
    url: String,
    /// The decoded binary's sha256, lowercase hex.
    sha256: String,
    /// Its length.
    len: u64,
}

/// The agreement step, for a candidate with a [`resolve::Companion`] (grok): the vendor's own
/// `.zst` of the binary is downloaded at its exact size, decoded as one checksummed zstd frame
/// declaring the binary's size, hashed and deleted, before the signed payload is fetched; the
/// binary staged from the payload must then be these bytes ([`land`]'s pre-swap).
///
/// # Errors
/// [`FlowError::StageRefused`] while a refusal of the payload's signed digests still binds
/// (the landing would refuse it before its download, so the copy is not fetched either); the
/// staging path's own errors; [`FlowError::Download`] for a staging directory that cannot
/// be made, a companion download that failed or came up short, or a frame that does not decode
/// to the binary's size and checksum (nothing is memoized: a torn or half-published copy heals
/// on a later pass); the disk gate's own, which holds room for the companion, the payload and
/// the binary at once. The downloaded copy is removed once it is decoded or refused.
fn agree(lane: &Lane<'_>, pending: &Pending) -> Result<Option<Agreement>, FlowError> {
    let (spec, c) = (pending.spec, &pending.candidate);
    let Some(companion) = c.companion.as_ref() else {
        return Ok(None);
    };
    let art = c.artifact.artifact();
    if let Some(note) = crate::flow::digest_refusal_note(
        lane.layout,
        spec.program,
        c.version.build_id(),
        art,
        false,
    ) {
        return Err(FlowError::StageRefused(note));
    }
    let asset = format!("{}-{}-{}.zst", spec.program, c.version, lane.triple);
    let dl = crate::flow::staged_download_path(lane.layout, spec.program, &asset)?;
    crate::flow::disk_gate(
        pending.landing_bytes().need(),
        crate::freespace::available_bytes(&dl),
    )?;
    crate::flow::make_staging_dir(&dl).map_err(|e| FlowError::Download(e.to_string()))?;
    let _ = std::fs::remove_file(&dl);
    crate::progress::note_build(spec.program, c.version.build_id());
    if let Some(ticket) = lane.download_turn {
        ticket.wait();
    }
    let watch = crate::progress::watch_download(spec.program, &dl, companion.size);
    let fetched = lane
        .fetcher
        .vendor_download(spec.program, &companion.url, &dl, companion.size);
    drop(watch);
    // The turn stays this landing's: the payload's download follows on it ([`land_one`]).
    let decoded = fetched
        .map_err(|e| {
            crate::flow::note_fetch_fault();
            e.to_string()
        })
        .and_then(|()| decode_companion(&dl, companion));
    let _ = std::fs::remove_file(&dl);
    let (sha256, len) = decoded.map_err(|why| {
        FlowError::Download(format!(
            "xAI's own copy of {} {} at {} could not be checked: {why}",
            spec.program, c.version, companion.url
        ))
    })?;
    Ok(Some(Agreement {
        url: companion.url.clone(),
        sha256,
        len,
    }))
}

/// The sha256 and length of the binary the companion at `dl` decodes to: exactly its size on
/// disk, one zstd frame that declares the binary's size and carries its XXH64 checksum, and
/// nothing else (no skippable frame, no frame after it).
fn decode_companion(dl: &Path, companion: &resolve::Companion) -> Result<(String, u64), String> {
    use std::io::Read as _;
    let got = std::fs::symlink_metadata(dl)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len());
    if got != Some(companion.size) {
        return Err(format!(
            "it answered {} of its {} bytes",
            got.map_or_else(|| String::from("no file"), |n| n.to_string()),
            companion.size
        ));
    }
    let open = || std::fs::File::open(dl).map_err(|e| format!("open the download: {e}"));
    let declared = aterm_zstd::declared_content_size(open()?).map_err(|e| e.to_string())?;
    if declared != Some(companion.binary_size) {
        return Err(format!(
            "its zstd frame declares {}, not the binary's {} bytes",
            declared.map_or_else(|| String::from("no size"), |n| format!("{n} bytes")),
            companion.binary_size
        ));
    }
    let mut decoder = aterm_zstd::Decoder::new(open()?)
        .require_checksum()
        .one_frame();
    let mut sha = crate::tree::BulkSha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut len: u64 = 0;
    loop {
        let n = decoder.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        len = len.saturating_add(n as u64);
        if len > companion.binary_size {
            return Err(format!(
                "it decodes past the binary's {} bytes",
                companion.binary_size
            ));
        }
        sha.update(&buf[..n]);
    }
    if len != companion.binary_size {
        return Err(format!(
            "it decodes to {len} bytes, not the binary's {}",
            companion.binary_size
        ));
    }
    Ok((sha.finalize_hex(), len))
}

/// Whether the binary staged in `tree` is the one `vendor`'s own copy decoded to.
///
/// # Errors
/// [`StageError::SignerRefused`] naming both digests when it is not (memoized for the
/// candidate's version and digest, its line short enough for the memo to keep both whole);
/// [`StageError::Io`] when the staged binary does not read.
fn hold_to_agreement(
    tree: &Path,
    exe: &str,
    vendor: &str,
    agreed: &Agreement,
) -> Result<(), StageError> {
    let path = tree.join("bin").join(exe);
    let len = std::fs::symlink_metadata(&path)
        .map_err(StageError::Io)?
        .len();
    let sha256 = crate::tree::file_digest(&path, crate::manifest::DigestAlg::Sha256)
        .map_err(StageError::Io)?;
    if sha256 == agreed.sha256 && len == agreed.len {
        return Ok(());
    }
    Err(StageError::SignerRefused(format!(
        "bin/{exe} from npm's signed tarball, sha256 {sha256} ({len} bytes), is not \
         {vendor}'s own build, sha256 {} ({} bytes)",
        agreed.sha256, agreed.len
    )))
}

/// Download, verify, stage (the darwin Developer ID gate inside), activate and shim a candidate
/// [`plan_one`] verified.
pub(crate) fn land_one(lane: &Lane<'_>, pending: &Pending) -> Outcome {
    let spec = pending.spec;
    let landed = agree(lane, pending).and_then(|agreed| land(lane, pending, agreed.as_ref()));
    // One turn on the link spans the bucket copy and the payload; it ends with the landing,
    // landed or not, so the next ticket's transfer may start.
    if let Some(ticket) = lane.download_turn {
        ticket.release();
    }
    let c = &pending.candidate;
    let asset = Some(c.artifact.artifact().asset.clone());
    let verdict = match landed {
        Ok((shimmed, refused_shims, tree_root)) => Verdict::Installed {
            from: pending.installed,
            to: c.version,
            reason: pending.reason,
            shimmed,
            refused_shims,
            tree_root,
        },
        Err(e) => {
            let refused = matches!(
                &e,
                FlowError::Stage(StageError::DigestMismatch { .. } | StageError::SignerRefused(_))
                    | FlowError::StageRefused(_)
                    | FlowError::VendorRefused(_)
            );
            let v = if let FlowError::Stage(StageError::VerifyDeferred(why)) = &e {
                Verdict::Deferred {
                    version: c.version,
                    why: why.clone(),
                    keeping: pending.installed,
                }
            } else if refused {
                Verdict::Refused {
                    version: Some(c.version),
                    why: e.to_string(),
                    keeping: pending.installed,
                }
            } else {
                Verdict::Failed {
                    version: c.version,
                    error: e.to_string(),
                    keeping: pending.installed,
                }
            };
            if let FlowError::Stage(StageError::SignerRefused(_)) = &e {
                let _ = ProgramStamp::record_refusal(
                    lane.layout,
                    spec.program,
                    &verdict_line(spec, &v),
                    c.version,
                    c.vouch.digest(),
                );
            }
            let inputs = super::decide::Inputs {
                program: spec.program,
                installed: pending.installed,
                previous: pending.previous,
                head: None,
                policy: lane.policy,
                floor: spec.floor,
                high_water: pending.high_water,
                held: pending.held,
                legacy_ceiling: spec.legacy_ceiling,
                asked: lane.asked,
            };
            yank_fallback(lane, &inputs, pending.reason, pending.active, v, refused)
        }
    };
    finish(
        lane,
        Outcome {
            spec,
            verdict,
            asset,
        },
    )
}

fn land(
    lane: &Lane<'_>,
    pending: &Pending,
    agreed: Option<&Agreement>,
) -> Result<(Vec<String>, Vec<String>, String), FlowError> {
    let spec = pending.spec;
    let program = spec.program;
    let c = &pending.candidate;
    let art = c.artifact.artifact();
    resolve::admit(&c.artifact, &c.vouch)?;
    let digest = c.vouch.digest();
    if c.companion.is_some() != agreed.is_some() {
        return Err(FlowError::VendorRefused(String::from(
            "a build the vendor's own copy vouches for is landed only once that copy agreed",
        )));
    }
    let exe = crate::store::ToolName::new(&art.entry)
        .map(|t| t.exe_file())
        .unwrap_or_default();
    let record = VendorRecord {
        schema: RECORD_SCHEMA,
        program: program.to_string(),
        version: c.version,
        vendor: spec.vendor.to_string(),
        source_url: art.url.clone(),
        sha256: art.sha256.clone(),
        sha512: art.sha512.clone(),
        size: art.size,
        tree_root: "0".repeat(64),
        apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
        anchor: c.anchor,
        build_date: c.build_date,
        verified_at: lane.now,
        measured_sha256: agreed.map(|a| a.sha256.clone()),
        agreed_with: agreed.map(|a| a.url.clone()),
        tree_from: None,
    };
    record
        .record_bytes()
        .map_err(|e| FlowError::VendorRefused(e.to_string()))?;
    let final_root: RefCell<Option<String>> = RefCell::new(None);
    let tree_from: std::cell::Cell<Option<&'static str>> = std::cell::Cell::new(None);
    let gate =
        crate::install::developer_id_gate(spec.apple_team, spec.exposes, lane.trust.team_check);
    let pre_swap = |tree: &Path| -> Result<(), StageError> {
        let demoted = crate::macho::demote_foreign_executables(tree)?;
        gate(tree)?;
        if let Some(agreed) = agreed {
            hold_to_agreement(tree, &exe, spec.vendor, agreed)?;
        }
        if !c.anchor.vouches_before_download() {
            let seen =
                record_or_conflict(lane.layout, program, c.version, lane.triple, digest, true)
                    .map_err(StageError::Io)?;
            if let DigestSeen::Conflict { first } = seen {
                return Err(StageError::SignerRefused(recut(c, &first)));
            }
        }
        if demoted && spec.aliases.is_empty() {
            *final_root.borrow_mut() = Some(crate::tree::tree_root(tree).map_err(StageError::Io)?);
        }
        Ok(())
    };
    let sidecars = |root: &str| -> Result<Vec<(&'static str, Vec<u8>)>, StageError> {
        let mut rec = record.clone();
        rec.tree_root = final_root
            .borrow()
            .clone()
            .unwrap_or_else(|| root.to_string());
        rec.tree_from = tree_from.get().map(str::to_string);
        let bytes = rec
            .record_bytes()
            .map_err(|e| StageError::Payload(e.to_string()))?;
        Ok(vec![(crate::store::VENDOR_SIDECAR_SUFFIX, bytes)])
    };
    let npm_package = |bytes: &[u8]| -> Result<(), StageError> {
        let Some(companion) = c.companion.as_ref() else {
            return Err(StageError::Payload(String::from(
                "an npm binary package is staged only for a build with its vendor's own copy",
            )));
        };
        let package = companion.platform.package();
        let tarball = companion.platform.tarball(c.version);
        let want = companion.platform.expected(&package, c.version, &tarball);
        super::npm::check_package_json(bytes, &want).map_err(StageError::Payload)
    };
    let hooks = StageHooks {
        pre_swap: Some(&pre_swap),
        sidecars: Some(&sidecars),
        aliases: spec.aliases,
        npm_package: Some(&npm_package),
    };
    let exposes: Vec<String> = spec.exposes.iter().map(|t| (*t).to_string()).collect();
    let shim_env = spec.shim_env();
    let landing = Landing {
        program,
        build: c.version.build_id(),
        artifact: art,
        exposes: &exposes,
        shim_env: &shim_env,
        aliases: crate::activate::Aliases::Off,
        installed: pending.active,
        strategy: crate::dispatch::ApplyStrategy::Shim,
        hooks: &hooks,
        download_turn: lane.download_turn,
    };
    let (mut copied, mut turn) = (None, lane.download_turn);
    if let Some(sibling) = sibling_tree(lane.layout, spec, c) {
        tree_from.set(Some(sibling.program));
        let source = crate::flow::SourceTree {
            program: sibling.program,
            dir: &sibling.dir,
            root: &sibling.root,
        };
        match crate::flow::land_from_tree(lane.layout, &landing, &source) {
            Ok(landed) => copied = landed,
            // The copy's stage failed on this machine, not on its bytes: no verdict, so the row
            // downloads its own. The copy was laid, so its turn on the link is given up, and
            // waiting on it would never end.
            Err(FlowError::Stage(e @ (StageError::Io(_) | StageError::VerifyDeferred(_)))) => {
                println!(
                    "atpkg: {program}: {}'s build of the same bytes did not stage as a copy \
                     ({e}); downloading its own",
                    sibling.program
                );
                turn = None;
            }
            Err(e) => return Err(e),
        }
        if copied.is_none() {
            tree_from.set(None);
        }
    }
    let landed = match copied {
        Some(landed) => landed,
        None => crate::flow::land_artifact(
            lane.layout,
            &Landing {
                download_turn: turn,
                ..landing
            },
            &|dl| {
                lane.fetcher
                    .vendor_download(program, &art.url, dl, art.size)
                    .map_err(|e| e.to_string())
            },
        )?,
    };
    let root = final_root.borrow().clone().unwrap_or(landed.root);
    Ok((landed.shimmed, landed.refused, root))
}

/// Another row's complete build of exactly a candidate's signed bytes ([`sibling_tree`]),
/// held from gc while its tree is copied.
struct SiblingTree {
    /// The row whose build it is.
    program: &'static str,
    /// Its build directory.
    dir: std::path::PathBuf,
    /// The `tree_root` its record names.
    root: String,
    /// Keeps gc from reclaiming the build until the copy is laid.
    _lease: crate::lease::Lease,
}

/// The build whose tree `spec`'s landing of `c` copies in place of a download: one of a row that
/// [lends its tree](VendorSpec::lends_tree_to) to `spec`, at the build `c`'s version names (not
/// that row's active one), complete, its record naming `c`'s version, anchor, URL, digest and
/// size and no vendor's copy agreed. A candidate with a vendor's copy to agree with is never
/// copied, and off Unix nothing is ([`crate::install::stage_from_tree`] lays no clone there).
/// The record is read again once the build's lease is held, which must be had at once: `None`
/// when no build qualifies, or gc is reclaiming it.
fn sibling_tree(layout: &Layout, spec: &'static VendorSpec, c: &Candidate) -> Option<SiblingTree> {
    if !cfg!(unix) || c.companion.is_some() {
        return None;
    }
    let art = c.artifact.artifact();
    let build = c.version.build_id();
    let names_c = |r: &VendorRecord| {
        r.version == c.version
            && r.anchor == c.anchor
            && r.source_url == art.url
            && r.sha256 == art.sha256
            && r.sha512 == art.sha512
            && r.size == art.size
            && r.measured_sha256.is_none()
            && r.agreed_with.is_none()
    };
    super::VENDORS
        .iter()
        .filter(|lender| lender.lends_tree_to(spec))
        .find_map(|lender| {
            let dir = layout.build_dir(lender.program, build);
            complete_record(&dir).filter(|r| names_c(r))?;
            let who = format!(
                "atpkg (pid {}) \u{2014} {} copies this build's tree",
                std::process::id(),
                spec.program
            );
            let subject = crate::lease::Subject::build(lender.program, build)?;
            let lease =
                crate::lease::take(&layout.prefix, &subject, &who, std::time::Duration::ZERO)
                    .ok()?;
            let record = complete_record(&dir).filter(|r| names_c(r))?;
            Some(SiblingTree {
                program: lender.program,
                dir,
                root: record.tree_root,
                _lease: lease,
            })
        })
}

fn yank_fallback(
    lane: &Lane<'_>,
    inputs: &super::decide::Inputs<'_>,
    reason: InstallReason,
    active: Option<u64>,
    verdict: Verdict,
    head_refused: bool,
) -> Verdict {
    if reason != InstallReason::ReplacesYanked {
        return verdict;
    }
    let headless = super::decide::Inputs {
        head: None,
        ..*inputs
    };
    match decide(&headless) {
        Decision::RollbackTo(to) => roll_back(
            lane,
            inputs.program,
            inputs.installed,
            active,
            inputs.previous,
            to,
        ),
        Decision::Tombstone if head_refused => {
            crate::flow::install_tombstone_shims(lane.layout, inputs.program, active);
            Verdict::Tombstoned {
                from: inputs.installed,
            }
        }
        _ => verdict,
    }
}

fn roll_back(
    lane: &Lane<'_>,
    program: &str,
    from: Installed,
    active: Option<u64>,
    previous: Option<InstalledView>,
    to: u64,
) -> Verdict {
    let (Some(_), Some(target)) = (active, previous.filter(|p| p.build == to)) else {
        return Verdict::Refused {
            version: None,
            why: String::from("no retained build to roll back to"),
            keeping: from,
        };
    };
    let exposes: Vec<String> = super::spec(program)
        .map(|s| s.exposes.iter().map(|t| (*t).to_string()).collect())
        .unwrap_or_default();
    match crate::flow::roll_back_to(lane.layout, program, to, &exposes) {
        Ok(()) => Verdict::RolledBack {
            from,
            to: target.version,
        },
        Err(e) => Verdict::Failed {
            version: target.version,
            error: format!("the rollback could not be laid: {e}"),
            keeping: from,
        },
    }
}

fn keep_alab(lane: &Lane<'_>, spec: &'static VendorSpec, build: u64) -> Verdict {
    let (layout, program) = (lane.layout, spec.program);
    let what = crate::alab::build_words(layout, program, build);
    let exposes: Vec<String> = spec.exposes.iter().map(|t| (*t).to_string()).collect();
    let shimmed = crate::ops::active_builds(layout).get(program).copied() == Some(build);
    let Some(why) = Revoked::of(lane.policy, program, build) else {
        if !shimmed
            && crate::store::build_is_complete(&layout.build_dir(program, build))
            && let Err(e) = crate::flow::roll_back_to(layout, program, build, &exposes)
        {
            return Verdict::AlabRestoreFailed {
                what,
                error: e.to_string(),
            };
        }
        return Verdict::AlabKept { what };
    };
    let to = crate::alab::retained_alab_below(layout, program, build)
        .into_iter()
        .find(|b| Revoked::of(lane.policy, program, *b).is_none());
    if let Some(to) = to
        && crate::flow::roll_back_to(layout, program, to, &exposes).is_ok()
    {
        return Verdict::AlabRolledBack {
            from: what,
            to_build: to,
            to: crate::alab::build_words(layout, program, to),
        };
    }
    if shimmed {
        crate::flow::install_tombstone_shims(layout, program, Some(build));
    }
    Verdict::AlabTombstoned { what, why }
}

fn unread_head(head: &Head, keeping: Installed) -> Verdict {
    match head {
        Head::Refused(why) => Verdict::Refused {
            version: None,
            why: why.clone(),
            keeping,
        },
        Head::Unserved(why) => Verdict::Unreachable {
            keeping,
            why: Some(why.clone()),
        },
        _ => Verdict::Unreachable { keeping, why: None },
    }
}

fn finish(lane: &Lane<'_>, outcome: Outcome) -> Outcome {
    let line = outcome.line();
    let verdict = line.strip_prefix("atpkg: ").unwrap_or(&line);
    let yank_move = match &outcome.verdict {
        Verdict::Installed {
            to,
            reason: InstallReason::ReplacesYanked,
            ..
        }
        | Verdict::RolledBack { to, .. } => Some(*to),
        _ => None,
    };
    let _ = match yank_move {
        Some(to) => ProgramStamp::record_yank_move(lane.layout, outcome.spec.program, verdict, to),
        None if matches!(outcome.verdict, Verdict::Linked) => Ok(()),
        None => ProgramStamp::record_outcome(
            lane.layout,
            outcome.spec.program,
            verdict,
            outcome.active_version(),
        ),
    };
    outcome
}

fn installed_of(layout: &Layout, program: &str, active: Option<u64>) -> Installed {
    let Some(build) = active else {
        return Installed::Nothing;
    };
    let dir = layout.build_dir(program, build);
    if is_vendor_build(build) {
        return complete_record(&dir).map_or(Installed::Nothing, |r| {
            Installed::Vendor(r.installed_view())
        });
    }
    if crate::store::build_is_complete(&dir) {
        Installed::Legacy {
            build,
            version: None,
        }
    } else {
        Installed::Nothing
    }
}

fn previous_of(
    layout: &Layout,
    policy: &Policy,
    program: &str,
    installed: Installed,
) -> Option<InstalledView> {
    let Installed::Vendor(current) = installed else {
        return None;
    };
    retained_below(layout, program, current.build)
        .into_iter()
        .find(|v| rollback_lands_on(layout, policy, program, v.build))
}

fn retained_below(layout: &Layout, program: &str, below: u64) -> Vec<InstalledView> {
    let mut views: Vec<InstalledView> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, b)| p == program && is_vendor_build(*b) && *b < below)
        .filter(|(_, b)| layout.build_dir(program, *b).join("bin").is_dir())
        .filter_map(|(_, b)| complete_record(&layout.build_dir(program, b)))
        .map(|r| r.installed_view())
        .collect();
    views.sort_by_key(|v| std::cmp::Reverse(v.version));
    views
}

fn retained_legacy_below(layout: &Layout, program: &str, below: u64) -> Vec<u64> {
    let mut builds: Vec<u64> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, b)| p == program && !is_vendor_build(*b) && *b < below)
        .map(|(_, b)| b)
        .filter(|b| {
            let dir = layout.build_dir(program, *b);
            dir.join("bin").is_dir() && crate::store::build_is_complete(&dir)
        })
        .collect();
    builds.sort_unstable_by(|a, b| b.cmp(a));
    builds
}

/// Whether [`roll_back_by_hand`] may land `program` on its retained `build`: complete as
/// [`installed_of`] reads one (a vendor build by its record, a legacy index build by its marker),
/// with a `bin/` to run, and revoked by nothing in `policy` (a yank, or an ALab build's floor).
pub(crate) fn rollback_lands_on(
    layout: &Layout,
    policy: &Policy,
    program: &str,
    build: u64,
) -> bool {
    layout.build_dir(program, build).join("bin").is_dir()
        && match installed_of(layout, program, Some(build)) {
            Installed::Vendor(view) => !policy.is_yanked(program, view.version),
            Installed::Legacy { build, .. }
                if crate::alab::build_is_alab(layout, program, build) =>
            {
                Revoked::of(policy, program, build).is_none()
            }
            Installed::Legacy { build, .. } => !policy.is_legacy_yanked(program, build),
            Installed::Nothing => false,
        }
}

/// What a by-hand rollback moved, each build as a line names it: a version, or a legacy index
/// build's `build <n>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandRollback {
    /// The build it left.
    pub(crate) from: String,
    /// The build it re-activated.
    pub(crate) to: String,
    /// That build's store id.
    pub(crate) to_build: u64,
}

/// Re-activate the newest retained admissible build below the active one
/// (`atpkg rollback <program>`).
///
/// # Errors
/// Nothing complete is active, no admissible build is retained, or the flip could not be laid.
pub(crate) fn roll_back_by_hand(
    layout: &Layout,
    policy: &Policy,
    spec: &'static VendorSpec,
) -> Result<HandRollback, String> {
    let program = spec.program;
    let active = crate::ops::active_builds(layout).get(program).copied();
    let alab = active.filter(|b| crate::alab::build_is_alab(layout, program, *b));
    let (from, below) = match installed_of(layout, program, active) {
        Installed::Vendor(current) => (current.version.to_string(), current.build),
        Installed::Legacy { build, .. } if alab.is_some() => {
            (crate::alab::build_words(layout, program, build), build)
        }
        Installed::Legacy { build, .. } => (super::build_words(build), build),
        Installed::Nothing => {
            return Err(match active {
                Some(b) => format!(
                    "{} is not a complete build — `aterm pkg update {program}` re-stages it",
                    super::display_build(program, b)
                ),
                None => format!(
                    "{program} is not installed — nothing to roll back (aterm pkg list shows \
                     what is)"
                ),
            });
        }
    };
    let retained: Vec<(u64, String)> = if alab.is_some() {
        crate::alab::retained_alab_below(layout, program, below)
            .into_iter()
            .map(|b| (b, crate::alab::build_words(layout, program, b)))
            .collect()
    } else {
        retained_below(layout, program, below)
            .into_iter()
            .map(|v| (v.build, v.version.to_string()))
            .chain(
                retained_legacy_below(layout, program, below)
                    .into_iter()
                    .map(|b| (b, super::build_words(b))),
            )
            .collect()
    };
    let Some((to_build, to)) = retained
        .iter()
        .find(|(b, _)| rollback_lands_on(layout, policy, program, *b))
        .cloned()
    else {
        let yanked: Vec<&str> = retained.iter().map(|(_, label)| label.as_str()).collect();
        return Err(match yanked.len() {
            0 => format!("no older {program} build is kept — {program} {from} stays"),
            n => format!(
                "{program} {} {} yanked and no older build is kept — {program} {from} stays",
                yanked.join(", "),
                if n == 1 { "is" } else { "are" }
            ),
        });
    };
    if alab.is_none() {
        crate::shim_env::write_sidecar(&layout.build_dir(program, to_build), &spec.shim_env())
            .map_err(|e| format!("the rollback could not be laid: {e}"))?;
    }
    let exposes: Vec<String> = spec.exposes.iter().map(|t| (*t).to_string()).collect();
    crate::flow::roll_back_to(layout, program, to_build, &exposes)
        .map_err(|e| format!("the rollback could not be laid: {e}"))?;
    let verdict = format!("{program} rolled back from {from} to {to} by hand");
    let _ =
        ProgramStamp::record_outcome(layout, program, &verdict, Version::from_build_id(to_build));
    Ok(HandRollback { from, to, to_build })
}

fn legacy_facts(lane: &Lane<'_>, program: &str, build: u64) -> Option<(Version, Option<String>)> {
    let index = lane.legacy_index?;
    let repo = index
        .program(program)
        .map(|p| p.repo.as_str())
        .filter(|repo| !crate::alab::agent(program).is_some_and(|a| a.repo == *repo))
        .unwrap_or(crate::manifest::INDEX_REPO);
    let (version, root) =
        crate::flow::legacy_facts_of_build(lane.fetcher, index, repo, program, build, lane.triple)?;
    Some((Version::parse(&version)?, root))
}

fn keep_legacy_facts(
    layout: &Layout,
    program: &str,
    build: u64,
    version: Version,
    root: Option<&str>,
) {
    let _ = ProgramStamp::record_legacy_version(layout, program, build, version);
    if let Some(root) = root {
        let _ = ProgramStamp::record_legacy_root(layout, program, build, root);
    }
}

/// The legacy index build a rollback of `program` would land on (the `active` one, or the newest
/// retained below the active vendor build) when no signed root is held for it: none in its stamp
/// and no recorded row for that build carrying one (an older client could drop it).
pub(crate) fn legacy_root_owed(layout: &Layout, program: &str, active: Option<u64>) -> Option<u64> {
    let build = match installed_of(layout, program, active) {
        Installed::Legacy { build, .. } => build,
        Installed::Vendor(current) => {
            *retained_legacy_below(layout, program, current.build).first()?
        }
        Installed::Nothing => return None,
    };
    let kept =
        ProgramStamp::read(layout, program).is_some_and(|s| s.legacy_root_of(build).is_some());
    let recorded = crate::status::read(layout)
        .and_then(|s| s.programs.get(program).cloned())
        .is_some_and(|row| row.installed_build == Some(build) && !row.tree_root.is_empty());
    (!kept && !recorded).then_some(build)
}

fn installed_words(i: Installed) -> String {
    match i {
        Installed::Nothing => String::from("nothing"),
        Installed::Legacy {
            version: Some(v), ..
        } => v.to_string(),
        Installed::Legacy {
            build,
            version: None,
        } => format!("build {build}"),
        Installed::Vendor(view) => view.version.to_string(),
    }
}

fn keeping(i: Installed) -> String {
    match i {
        Installed::Nothing => String::from("nothing is installed yet"),
        other => format!("keeping {}", installed_words(other)),
    }
}

fn verdict_line(spec: &VendorSpec, verdict: &Verdict) -> String {
    let (p, vendor) = (spec.program, spec.vendor);
    let latest = format!("{vendor}'s latest");
    match verdict {
        Verdict::Installed {
            from, to, reason, ..
        } => match (reason, from) {
            (_, Installed::Nothing) => format!("atpkg: {p} {to} installed ({vendor} latest)"),
            (InstallReason::ReplacesYanked, from) => format!(
                "atpkg: {p} {} → {to} ({} is yanked; {vendor} latest)",
                installed_words(*from),
                installed_words(*from)
            ),
            (_, from) => format!(
                "atpkg: {p} {} → {to} ({vendor} latest)",
                installed_words(*from)
            ),
        },
        Verdict::Current(v) => format!("atpkg: {p} {v} is {latest}"),
        Verdict::Kept {
            reason,
            head,
            keeping: k,
        } => {
            let h = head.map_or_else(|| String::from("?"), |v| v.to_string());
            let kept = keeping(*k);
            match reason {
                KeepReason::HeadOlder => format!(
                    "atpkg: {p} {} is newer than {latest} ({h}); {kept}",
                    installed_words(*k)
                ),
                KeepReason::HeadYanked => format!("atpkg: {p} — {latest} ({h}) is yanked; {kept}"),
                KeepReason::BelowFloor => format!(
                    "atpkg: {p} — {latest} ({h}) is older than this aterm's floor ({}); {kept}",
                    spec.floor
                ),
                KeepReason::BelowHighWater => format!(
                    "atpkg: {p} — {latest} ({h}) is older than a version this machine already \
                     ran; {kept}"
                ),
                KeepReason::MajorJump => format!(
                    "atpkg: {p} — {latest} ({h}) is more than one major version past {}; {kept}",
                    installed_words(*k)
                ),
                KeepReason::BuildDateRegressed => format!(
                    "atpkg: {p} — {latest} ({h}) was built before {}; {kept}",
                    installed_words(*k)
                ),
                KeepReason::Held => format!(
                    "atpkg: {p} not updated — you pinned it at {} (`aterm pkg unpin {p}` lets it \
                     update)",
                    installed_words(*k)
                ),
                KeepReason::LegacyUnread => format!(
                    "atpkg: {p} {} is a legacy index build newer than this aterm, and its \
                     version could not be read; {kept} until it can be — `aterm pkg update \
                     {p}` moves it to {latest} ({h})",
                    installed_words(*k)
                ),
                KeepReason::UpToDate | KeepReason::NoHead => {
                    format!("atpkg: {p} — nothing to do; {kept}")
                }
            }
        }
        Verdict::Unreachable {
            keeping: k,
            why: None,
        } => format!(
            "atpkg: {p} — {vendor}'s release channel is unreachable; {}",
            keeping(*k)
        ),
        Verdict::Unreachable {
            keeping: k,
            why: Some(why),
        } => format!(
            "atpkg: {p} — {vendor}'s release channel did not serve it ({why}); {}",
            keeping(*k)
        ),
        Verdict::Refused {
            version,
            why,
            keeping: k,
        } => match version {
            Some(v) => format!("atpkg: {p} {v} refused: {why} — {}", keeping(*k)),
            None => format!("atpkg: {p} refused: {why} — {}", keeping(*k)),
        },
        Verdict::RolledBack { from, to } => format!(
            "atpkg: {p} {} is yanked — rolled back to {to}",
            installed_words(*from)
        ),
        Verdict::Tombstoned { from } => format!(
            "atpkg: {p} {} is yanked and no admissible build exists — its commands are disabled \
             until one does",
            installed_words(*from)
        ),
        Verdict::Failed {
            version,
            error,
            keeping: k,
        } => format!(
            "atpkg: {p} {version} could not be installed: {error} — {}",
            keeping(*k)
        ),
        Verdict::Deferred {
            version,
            why,
            keeping: k,
        } => format!(
            "atpkg: {p} {version} deferred: it waits for its {} ({why}); the next pass checks \
             it again — {}",
            crate::cli::SIGNATURE_WAIT_CLAUSE,
            keeping(*k)
        ),
        Verdict::Linked => format!("atpkg: {p} dev-linked — skipped"),
        Verdict::Unavailable { state } => format!("atpkg: {p}: {state}"),
        Verdict::AlabKept { what } => format!(
            "atpkg: {p} {what} kept — the ALab channel pins no build this machine may install, \
             and an ALab build is never replaced by {latest} (`aterm pkg update {p} --vendor` \
             chooses it)"
        ),
        Verdict::AlabRestoreFailed { what, error } => {
            format!("atpkg: {p} {what}'s commands could not be restored: {error}")
        }
        Verdict::AlabTombstoned { what, why } => format!(
            "atpkg: {p} {what} {} and no older ALab build is admissible — its commands are \
             disabled until the ALab channel pins one this machine may install (`aterm pkg \
             update {p} --vendor` chooses {latest})",
            why.words()
        ),
        Verdict::AlabRolledBack { from, to, .. } => {
            format!("atpkg: {p} {from} was revoked by the ALab channel — rolled back to {to}")
        }
    }
}

/// A vendor world for tests: a fake fetcher serving synthetic claude releases signed by the
/// committed test key and synthetic codex releases (release JSON, `SHA256SUMS`, tarball), with
/// every fetch recorded, and a codesign stand-in that passes every Mach-O.
#[cfg(test)]
pub(crate) mod world {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::super::npm::{NpmKey, keyid_of};
    use super::super::resolve::{self, GrokPlatform};
    use super::super::version::Version;
    use super::Trust;
    use crate::flow::{Fetcher, VendorFetchError, VendorGet};
    use crate::store::Layout;

    /// 2026-09-22T00:00:00Z.
    pub(crate) const NOW: i64 = 1_790_035_200;
    const SIGNED_AT: u32 = 1_790_000_000;
    /// The claude documents' base.
    pub(crate) const CLAUDE: &str = "https://downloads.claude.ai/claude-code-releases/";
    /// The codex head.
    pub(crate) const CODEX_HEAD: &str = "https://releases.openai.com/codex/channels/latest";
    /// Where codex's `SHA256SUMS` and packages live.
    pub(crate) const CODEX_DL: &str = "https://github.com/openai/codex/releases/download/";

    /// The triple the tests resolve for: the real host's OS and architecture, so
    /// the CLI's native selection and the fixture's signed artifacts agree.
    pub(crate) fn triple() -> &'static str {
        match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
            (true, true) => "aarch64-apple-darwin",
            (true, false) => "x86_64-apple-darwin",
            (false, true) => "aarch64-unknown-linux-gnu",
            (false, false) => "x86_64-unknown-linux-gnu",
        }
    }

    fn platform() -> &'static str {
        match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
            (true, true) => "darwin-arm64",
            (true, false) => "darwin-x64",
            (false, true) => "linux-arm64",
            (false, false) => "linux-x64",
        }
    }

    /// `bin/<name>` as this host's shim runs it: `bin/<name>.exe` on Windows, where a
    /// payload lands under the executable spelling (`ToolName::exe_file`).
    pub(crate) fn bin(name: &str) -> String {
        format!("bin/{name}{}", crate::platform::EXE_SUFFIX)
    }

    /// A "binary" in this platform's native executable format, distinct per `tag`.
    pub(crate) fn native_exe(tag: &str) -> Vec<u8> {
        let mut bytes = if cfg!(target_os = "macos") {
            0xCFFA_EDFEu32.to_be_bytes().to_vec()
        } else {
            b"\x7fELF".to_vec()
        };
        bytes.extend_from_slice(&[0u8; 60]);
        bytes.extend_from_slice(tag.as_bytes());
        bytes
    }

    fn pass_check(
        _: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        Ok(())
    }

    fn refuse_check(
        _: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        Err(aterm_update_core::codesign::CodesignError::Refused {
            code: Some(3),
            stderr: "test-requirement: code failed to satisfy specified code requirement(s)\n"
                .into(),
        })
    }

    fn timeout_check(
        _: &Path,
        _: &str,
        _: std::time::Instant,
    ) -> Result<(), aterm_update_core::codesign::CodesignError> {
        Err(aterm_update_core::codesign::CodesignError::TimedOut)
    }

    /// [`trust`] with a codesign stand-in that never finishes by its deadline.
    pub(crate) fn trust_timing_out() -> Trust {
        Trust {
            team_check: timeout_check,
            ..trust()
        }
    }

    /// The test keys and a codesign stand-in that passes.
    pub(crate) fn trust() -> Trust {
        Trust {
            claude_key: crate::openpgp::testkit::test_key(),
            team_check: pass_check,
            npm_keys: npm_test_keys(),
        }
    }

    /// The signing half of the test npm registry key: `npm-test-key.p8`, a P-256 PKCS#8 key
    /// made for these tests (`openssl genpkey -algorithm EC -pkeyopt
    /// ec_paramgen_curve:P-256 -pkeyopt ec_param_enc:named_curve`, then `openssl pkcs8
    /// -topk8 -nocrypt -outform DER`).
    fn npm_signer() -> ring::signature::EcdsaKeyPair {
        ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            include_bytes!("fixtures/npm-test-key.p8"),
            &ring::rand::SystemRandom::new(),
        )
        .unwrap()
    }

    /// The test npm registry key, as [`Trust::npm_keys`] holds it.
    pub(crate) fn npm_test_keys() -> &'static [NpmKey] {
        static KEYS: std::sync::OnceLock<[NpmKey; 1]> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            use ring::signature::KeyPair as _;
            let point: [u8; 65] = npm_signer().public_key().as_ref().try_into().unwrap();
            [NpmKey {
                keyid: Box::leak(keyid_of(&point).into_boxed_str()),
                point,
                not_after: None,
            }]
        })
    }

    /// The test registry key's signature over `message`, base64 as npm serves it.
    pub(crate) fn npm_sign(message: &str) -> String {
        let sig = npm_signer()
            .sign(&ring::rand::SystemRandom::new(), message.as_bytes())
            .unwrap();
        aterm_codec::base64::encode(sig.as_ref()).unwrap()
    }

    /// grok's npm and bucket names for [`triple`]; `None` where xAI serves no build
    /// (Windows), so a test's landing half could not run.
    pub(crate) fn grok_platform() -> Option<GrokPlatform> {
        resolve::grok_platform(triple())
    }

    /// What [`Fake::publish_grok`] served.
    pub(crate) struct GrokServed {
        /// npm's tarball URL.
        pub tarball: String,
        /// xAI's bucket `.zst` of the binary.
        pub zst: String,
        /// xAI's bucket binary, which is only ever sized.
        pub binary: String,
        /// The tarball's sha512, lowercase hex.
        pub sha512: String,
    }

    /// `exe` as a Brotli stream ([`aterm_brotli::write_uncompressed`]).
    pub(crate) fn brotli(exe: &[u8]) -> Vec<u8> {
        let mut br = Vec::new();
        aterm_brotli::write_uncompressed(&mut br, exe).unwrap();
        br
    }

    /// `exe` as one checksummed zstd frame, as xAI's bucket serves its `.zst`.
    pub(crate) fn zstd(exe: &[u8]) -> Vec<u8> {
        let mut zst = Vec::new();
        aterm_zstd::frame::write_raw(&mut zst, exe, aterm_zstd::frame::Checksum::Xxh64).unwrap();
        zst
    }

    /// The `package/package.json` npm's package of grok `version` for this host carries.
    pub(crate) fn grok_package_json(platform: GrokPlatform, version: &str) -> String {
        format!(
            "{{\n  \"name\": \"{}\",\n  \"version\": \"{version}\",\n  \"license\": \
             \"Apache-2.0\",\n  \"os\": [\"{}\"],\n  \"cpu\": [\"{}\"]\n}}\n",
            platform.package(),
            platform.os,
            platform.cpu
        )
    }

    /// A gzip'd tar of `members` (`(path, type flag, link, bytes, mode)`) and nothing else:
    /// no directory entries, as npm packs a package.
    pub(crate) fn npm_tgz(members: &[(&str, u8, &str, &[u8], u32)]) -> Vec<u8> {
        crate::extract::fixtures::gzip_bytes(&crate::extract::fixtures::tar_bytes(members))
    }

    /// npm's package of grok `version` for this host as xAI packs it: `package/bin/grok.br`
    /// (`exe`), `package/package.json`, `package/README.md` and the third-party notices.
    pub(crate) fn grok_tgz(platform: GrokPlatform, version: &str, exe: &[u8]) -> Vec<u8> {
        let json = grok_package_json(platform, version);
        npm_tgz(&[
            ("package/bin/grok.br", b'0', "", &brotli(exe), 0o644),
            ("package/package.json", b'0', "", json.as_bytes(), 0o644),
            ("package/README.md", b'0', "", b"# Grok Build\n", 0o644),
            (
                "package/THIRD_PARTY_NOTICES.md",
                b'0',
                "",
                &[b'n'; 7995],
                0o644,
            ),
        ])
    }

    pub(crate) fn sha512_hex(bytes: &[u8]) -> String {
        aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA512, bytes).as_ref())
    }

    /// [`trust`] with a codesign stand-in that refuses every Mach-O.
    pub(crate) fn trust_refusing_signers() -> Trust {
        Trust {
            team_check: refuse_check,
            ..trust()
        }
    }

    pub(crate) fn sha(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// One served document.
    #[derive(Clone)]
    struct Doc {
        body: Vec<u8>,
        effective: Option<String>,
    }

    type LegacyPkgKey = (String, u64);

    type SignedDoc = (Vec<u8>, Vec<u8>);

    /// One of [`Fake`]'s fields: a lock, since a pass works its fetcher from several threads
    /// at once ([`Fetcher::shared`]), with `RefCell`'s and `Cell`'s calls so the tests read
    /// it like either.
    #[derive(Default)]
    pub(crate) struct Shared<T>(std::sync::RwLock<T>);

    impl<T> Shared<T> {
        pub(crate) fn borrow(&self) -> std::sync::RwLockReadGuard<'_, T> {
            self.0
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        pub(crate) fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<'_, T> {
            self.0
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    impl<T: Copy> Shared<T> {
        pub(crate) fn get(&self) -> T {
            *self.borrow()
        }

        pub(crate) fn set(&self, value: T) {
            *self.borrow_mut() = value;
        }
    }

    /// The fake fetcher.
    #[derive(Default)]
    pub(crate) struct Fake {
        docs: Shared<BTreeMap<String, Doc>>,
        payloads: Shared<BTreeMap<String, Vec<u8>>>,
        /// Payloads only ever sized, at a size no test holds in memory.
        sized: Shared<BTreeMap<String, u64>>,
        /// Every `vendor_get`, `(url, if-none-match)`.
        pub gets: Shared<Vec<(String, Option<String>)>>,
        /// Every payload download's URL.
        pub downloads: Shared<Vec<String>>,
        /// Every vendor host unreachable.
        pub offline: Shared<bool>,
        /// Payload downloads unreachable (the documents still answer).
        pub downloads_fail: Shared<bool>,
        /// A signed index (bytes, signature) the index lane may resolve.
        pub index: Shared<Option<(Vec<u8>, Vec<u8>)>>,
        /// With no index published, its host answers with a refusal (a 403) rather than not
        /// at all: a rate-limited or revoked-token machine, never an offline one.
        pub index_refused: Shared<bool>,
        /// How many times the index was asked for.
        pub index_reads: Shared<u32>,
        /// Every head read as a hint (`vendor_head`), by URL; each is also in `gets`.
        pub head_reads: Shared<Vec<String>>,
        /// Signed legacy pkg manifests.
        pkgs: Shared<BTreeMap<LegacyPkgKey, SignedDoc>>,
        /// The repo of every pkg manifest asked for.
        pub pkg_reads: Shared<Vec<String>>,
    }

    impl Fake {
        /// Serve `url` with `body`.
        pub(crate) fn doc(&self, url: &str, body: &[u8]) {
            self.docs.borrow_mut().insert(
                url.to_string(),
                Doc {
                    body: body.to_vec(),
                    effective: None,
                },
            );
        }

        /// Answer `url` as though redirected to `effective`.
        pub(crate) fn redirect(&self, url: &str, effective: &str) {
            if let Some(d) = self.docs.borrow_mut().get_mut(url) {
                d.effective = Some(effective.to_string());
            }
        }

        /// Serve `url`'s payload.
        pub(crate) fn payload(&self, url: &str, bytes: &[u8]) {
            self.payloads
                .borrow_mut()
                .insert(url.to_string(), bytes.to_vec());
        }

        /// Stop serving `url`: it answers 404.
        pub(crate) fn forget(&self, url: &str) {
            self.docs.borrow_mut().remove(url);
        }

        /// Stop serving the payload at `url`: it answers 404.
        pub(crate) fn forget_payload(&self, url: &str) {
            self.payloads.borrow_mut().remove(url);
            self.sized.borrow_mut().remove(url);
        }

        /// Answer `url`'s size as `size`, for a payload a test only sizes (a download of it
        /// fails).
        pub(crate) fn payload_sized(&self, url: &str, size: u64) {
            self.sized.borrow_mut().insert(url.to_string(), size);
        }

        /// The document served at `url`.
        pub(crate) fn body(&self, url: &str) -> Vec<u8> {
            self.docs.borrow()[url].body.clone()
        }

        /// Publish the signed pkg manifest of `program`'s legacy index build `build`,
        /// naming vendor `version`.
        pub(crate) fn publish_legacy_pkg(&self, program: &str, build: u64, version: &str) {
            let body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
                 build_number = {build}\nexposes = [\"{program}\"]\n"
            );
            let sig =
                crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
            self.pkgs
                .borrow_mut()
                .insert((program.to_string(), build), (body.into_bytes(), sig));
        }

        /// [`Fake::publish_legacy_pkg`] with an artifact for this triple, as a legacy index
        /// build shipped one — what an index lane could fetch if it ever planned a vendor
        /// program (its `download` panics, so a test sees the attempt).
        #[cfg(unix)]
        pub(crate) fn publish_legacy_pkg_with_artifact(
            &self,
            program: &str,
            build: u64,
            version: &str,
        ) {
            let body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
                 build_number = {build}\nexposes = [\"{program}\"]\n\
                 [[artifact]]\ntarget = \"{}\"\nkind = \"binary\"\n\
                 asset = \"{program}-{build}.tar.zst\"\nsha256 = \"{}\"\n\
                 tree_root = \"{}\"\nsize = 100\n[artifact.cost]\ndisk_installed = 1\n",
                triple(),
                "0".repeat(64),
                "1".repeat(64)
            );
            let sig =
                crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
            self.pkgs
                .borrow_mut()
                .insert((program.to_string(), build), (body.into_bytes(), sig));
        }

        /// [`Fake::publish_legacy_pkg`], its one artifact for `target` carrying the signed
        /// `tree_root` `root` — what the lane keeps for a rollback to that build.
        pub(crate) fn publish_legacy_pkg_rooted(
            &self,
            program: &str,
            build: u64,
            version: &str,
            target: &str,
            root: &str,
        ) {
            self.publish_pkg_with(program, build, version, target, root, "binary");
        }

        /// Publish the signed pkg manifest of index program `program` at `build`, its one
        /// artifact for `target` carrying the signed `tree_root` `root` — what an index
        /// pass's root recovery reads.
        #[cfg(unix)]
        pub(crate) fn publish_pkg_root(&self, program: &str, build: u64, target: &str, root: &str) {
            self.publish_pkg_kind(program, build, target, root, "binary");
        }

        /// [`Fake::publish_pkg_root`] with the artifact's `kind` as given.
        #[cfg(unix)]
        pub(crate) fn publish_pkg_kind(
            &self,
            program: &str,
            build: u64,
            target: &str,
            root: &str,
            kind: &str,
        ) {
            self.publish_pkg_with(program, build, "0.1", target, root, kind);
        }

        fn publish_pkg_with(
            &self,
            program: &str,
            build: u64,
            version: &str,
            target: &str,
            root: &str,
            kind: &str,
        ) {
            let body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
                 build_number = {build}\nexposes = [\"{program}\"]\n\
                 [[artifact]]\ntarget = \"{target}\"\nkind = \"{kind}\"\n\
                 asset = \"{program}-{build}.tar.zst\"\nsha256 = \"{}\"\n\
                 tree_root = \"{root}\"\nsize = 100\n[artifact.cost]\ndisk_installed = 1\n",
                "0".repeat(64)
            );
            let sig =
                crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
            self.pkgs
                .borrow_mut()
                .insert((program.to_string(), build), (body.into_bytes(), sig));
        }

        /// Stop serving every legacy pkg manifest.
        pub(crate) fn forget_legacy_pkgs(&self) {
            self.pkgs.borrow_mut().clear();
        }

        /// The claude payload URL for `version`.
        pub(crate) fn claude_payload_url(version: &str) -> String {
            format!("{CLAUDE}{version}/{}/claude", platform())
        }

        /// Publish claude `version` as Anthropic's latest: the head, a manifest signed by
        /// the test key naming `payload`'s digest and size, and the payload.
        pub(crate) fn publish_claude(&self, version: &str, payload: &[u8]) {
            self.publish_claude_dated(version, payload, "2026-09-21T20:55:27Z");
        }

        /// [`Fake::publish_claude`] with an explicit signed `buildDate`.
        pub(crate) fn publish_claude_dated(&self, version: &str, payload: &[u8], date: &str) {
            let manifest = format!(
                "{{\"version\":\"{version}\",\"buildDate\":\"{date}\",\"platforms\":{{\"{}\":\
                 {{\"binary\":\"claude\",\"checksum\":\"{}\",\"size\":{}}}}}}}",
                platform(),
                sha(payload),
                payload.len()
            );
            self.publish_claude_manifest(version, manifest.as_bytes());
            self.payload(&Self::claude_payload_url(version), payload);
        }

        /// Serve `manifest`, signed by the test key, as claude `version` (and make it the head).
        pub(crate) fn publish_claude_manifest(&self, version: &str, manifest: &[u8]) {
            let url = format!("{CLAUDE}{version}/manifest.json");
            self.doc(&url, manifest);
            let sig = crate::openpgp::testkit::sign_detached(manifest, SIGNED_AT);
            self.doc(&format!("{url}.sig"), &sig);
            self.doc(
                &format!("{CLAUDE}latest"),
                format!("{version}\n").as_bytes(),
            );
        }

        /// Publish grok `version` as xAI's stable pointer names it, npm's registry signs it
        /// (under the test key) and xAI's bucket serves it: npm's version document and its
        /// tarball of `exe` ([`grok_tgz`]), and the bucket's raw `exe` and `.zst` of it. `None`
        /// where xAI serves no build for this host.
        pub(crate) fn publish_grok(&self, version: &str, exe: &[u8]) -> Option<GrokServed> {
            let platform = grok_platform()?;
            let served = self.publish_grok_tarball(version, &grok_tgz(platform, version, exe));
            self.publish_grok_bucket(version, exe, exe);
            Some(served)
        }

        /// Serve `tgz` as npm's tarball of grok `version` for this host, and its version
        /// document signed by the test key over `tgz`'s sha512; the head names `version`.
        pub(crate) fn publish_grok_tarball(&self, version: &str, tgz: &[u8]) -> GrokServed {
            let platform = grok_platform().unwrap();
            let v = Version::parse(version).unwrap();
            let digest = ring::digest::digest(&ring::digest::SHA512, tgz);
            let sha512 = aterm_codec::hex::encode_lower(digest.as_ref());
            let integrity = format!(
                "sha512-{}",
                aterm_codec::base64::encode(digest.as_ref()).unwrap()
            );
            let name = platform.package();
            let tarball = platform.tarball(v);
            let sig = npm_sign(&format!("{name}@{version}:{integrity}"));
            let doc = format!(
                "{{\"name\":\"{name}\",\"version\":\"{version}\",\"os\":[\"{}\"],\
                 \"cpu\":[\"{}\"],\"dist\":{{\"integrity\":\"{integrity}\",\"tarball\":\
                 \"{tarball}\",\"signatures\":[{{\"keyid\":\"{}\",\"sig\":\"{sig}\"}}]}}}}",
                platform.os,
                platform.cpu,
                npm_test_keys()[0].keyid
            );
            self.doc(
                super::super::spec("grok").unwrap().head_url,
                version.as_bytes(),
            );
            self.doc(&platform.version_doc(v), doc.as_bytes());
            self.payload(&tarball, tgz);
            let binary = platform.binary(v);
            GrokServed {
                tarball,
                zst: format!("{binary}.zst"),
                binary,
                sha512,
            }
        }

        /// Serve xAI's bucket copy of grok `version` for this host: a raw object as long as
        /// `raw`, and the `.zst` of `zst`.
        pub(crate) fn publish_grok_bucket(&self, version: &str, raw: &[u8], zst: &[u8]) {
            let binary = grok_platform()
                .unwrap()
                .binary(Version::parse(version).unwrap());
            self.payload(&binary, raw);
            self.payload(&format!("{binary}.zst"), &zstd(zst));
        }

        /// The codex package name for [`triple`].
        pub(crate) fn codex_asset() -> String {
            format!("codex-package-{}.tar.gz", triple())
        }

        /// The codex payload URL for `version`.
        pub(crate) fn codex_payload_url(version: &str) -> String {
            format!("{CODEX_DL}rust-v{version}/{}", Self::codex_asset())
        }

        /// Publish codex `version`: a tarball of `files` (`(path, bytes, mode)`), its
        /// `SHA256SUMS` on GitHub and the release JSON (the head) agreeing on its digest.
        pub(crate) fn publish_codex(&self, version: &str, files: &[(&str, &[u8], u32)]) {
            let tarball = tarball(files);
            let digest = sha(&tarball);
            let asset = Self::codex_asset();
            let release = format!(
                "{{\"tag_name\":\"rust-v{version}\",\"assets\":[{{\"name\":\"{asset}\",\
                 \"digest\":\"sha256:{digest}\",\"browser_download_url\":\
                 \"https://releases.openai.com/codex/releases/{version}/{asset}\"}}]}}"
            );
            self.doc(CODEX_HEAD, release.as_bytes());
            let sums = format!(
                "{}  codex-app-server-package-{}.tar.gz\n{digest}  {asset}\n",
                "0".repeat(64),
                triple()
            );
            self.doc(
                &format!("{CODEX_DL}rust-v{version}/codex-package_SHA256SUMS"),
                sums.as_bytes(),
            );
            self.payload(&Self::codex_payload_url(version), &tarball);
        }

        /// The codex tree a genuine package lays: the entry, a helper, and data — one
        /// executable-mode data file among it, as 0.156.0 ships.
        pub(crate) fn publish_codex_default(&self, version: &str) {
            let codex = native_exe(&format!("codex {version}"));
            let helper = native_exe("rg");
            self.publish_codex(
                version,
                &[
                    (&bin("codex"), &codex, 0o755),
                    ("codex-path/rg", &helper, 0o755),
                    ("codex-resources/voice/runtime.json", b"{\"a\": 1}\n", 0o755),
                    ("codex-resources/voice/NOTICE.md", b"# Notices\n", 0o644),
                ],
            );
        }
    }

    /// A gzip'd tar of `files`, with their directories.
    pub(crate) fn tarball(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut dirs: Vec<String> = Vec::new();
        for (path, _, _) in files {
            let mut acc = String::new();
            for part in path.split('/').collect::<Vec<_>>().split_last().unwrap().1 {
                acc.push_str(part);
                acc.push('/');
                if !dirs.contains(&acc) {
                    dirs.push(acc.clone());
                }
            }
        }
        let mut entries: Vec<(&str, u8, &str, &[u8], u32)> = dirs
            .iter()
            .map(|d| (d.as_str(), b'5', "", &[][..], 0o755))
            .collect();
        entries.extend(
            files
                .iter()
                .map(|&(path, bytes, mode)| (path, b'0', "", bytes, mode)),
        );
        let tar = crate::extract::fixtures::tar_bytes(&entries);
        crate::extract::fixtures::gzip_bytes(&tar)
    }

    fn unreachable<T>(what: &str) -> Result<T, VendorFetchError> {
        Err(VendorFetchError::Unreachable(format!("offline: {what}")))
    }

    impl Fetcher for Fake {
        fn index_candidates(&self) -> Result<Vec<crate::select::Candidate>, String> {
            *self.index_reads.borrow_mut() += 1;
            let Some((index_bytes, sig)) = self.index.borrow().clone() else {
                return Err(String::from(if self.index_refused.get() {
                    "HTTP 403 for the index listing: API rate limit exceeded"
                } else {
                    "no index is published"
                }));
            };
            let (roster_bytes, roster_sig) = crate::sig::testkit::published_roster();
            Ok(vec![crate::select::Candidate {
                label: "v0".into(),
                index_bytes,
                sig,
                roster_bytes,
                roster_sig,
            }])
        }
        fn index_link_down(&self) -> bool {
            !self.index_refused.get()
        }
        fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
            Some(self)
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.pkg_reads.borrow_mut().push(repo.to_string());
            self.pkgs
                .borrow()
                .get(&(program.to_string(), build))
                .cloned()
                .ok_or_else(|| String::from("no manifest"))
        }
        fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
            panic!("the vendor lane never reaches the release lane (asked for {asset})")
        }
        fn download_url(&self, url: &str, _: &Path, _: u64) -> Result<(), String> {
            panic!("the vendor lane never reaches the index's https lane (asked for {url})")
        }
        fn vendor_get(
            &self,
            program: &str,
            url: &str,
            cap: u64,
            if_none_match: Option<&str>,
        ) -> Result<VendorGet, VendorFetchError> {
            assert!(
                crate::vendor::vendor_direct_url_allowed(program, url),
                "{program} asked for an unpinned {url}"
            );
            self.gets
                .borrow_mut()
                .push((url.to_string(), if_none_match.map(str::to_string)));
            if self.offline.get() {
                return unreachable(url);
            }
            let Some(doc) = self.docs.borrow().get(url).cloned() else {
                return Err(VendorFetchError::Refused(format!("404 for {url}")));
            };
            let etag = format!("\"{}\"", &sha(&doc.body)[..12]);
            if if_none_match == Some(etag.as_str()) {
                return Ok(VendorGet::NotModified { etag });
            }
            if doc.body.len() as u64 > cap {
                return Err(VendorFetchError::Refused(format!("{url} is over its cap")));
            }
            Ok(VendorGet::Body {
                bytes: doc.body,
                etag: Some(etag),
                effective_url: doc.effective.unwrap_or_else(|| url.to_string()),
            })
        }
        fn vendor_head(
            &self,
            program: &str,
            url: &str,
            cap: u64,
            if_none_match: Option<&str>,
        ) -> Result<VendorGet, VendorFetchError> {
            self.head_reads.borrow_mut().push(url.to_string());
            self.vendor_get(program, url, cap, if_none_match)
        }
        fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
            assert!(
                crate::vendor::vendor_direct_url_allowed(program, url),
                "{url}"
            );
            if self.offline.get() {
                return unreachable(url);
            }
            if let Some(size) = self.sized.borrow().get(url) {
                return Ok(*size);
            }
            self.payloads
                .borrow()
                .get(url)
                .map(|b| b.len() as u64)
                .ok_or_else(|| VendorFetchError::Refused(format!("404 for {url}")))
        }
        fn vendor_download(
            &self,
            program: &str,
            url: &str,
            dest: &Path,
            cap: u64,
        ) -> Result<(), VendorFetchError> {
            assert!(
                crate::vendor::vendor_direct_url_allowed(program, url),
                "{url}"
            );
            self.downloads.borrow_mut().push(url.to_string());
            if self.offline.get() || self.downloads_fail.get() {
                return unreachable(url);
            }
            let bytes = self
                .payloads
                .borrow()
                .get(url)
                .cloned()
                .ok_or_else(|| VendorFetchError::Refused(format!("404 for {url}")))?;
            if bytes.len() as u64 > cap {
                return Err(VendorFetchError::Refused(format!("{url} is over its cap")));
            }
            std::fs::write(dest, bytes).map_err(|e| VendorFetchError::Refused(e.to_string()))
        }
    }

    /// A signed index (bytes, machine signature) valid until `valid_until`, naming `ay` (an
    /// ALab program with no published build), `claude` and `codex`, its stable channel
    /// pinning all three with `yanked` as given.
    pub(crate) fn signed_index(
        index_build: u64,
        valid_until: &str,
        yanked: &[&str],
    ) -> (Vec<u8>, Vec<u8>) {
        signed_index_naming(index_build, valid_until, yanked, &["claude", "codex"])
    }

    /// [`signed_index`], with `vendors` the only vendor programs it names and pins.
    pub(crate) fn signed_index_naming(
        index_build: u64,
        valid_until: &str,
        yanked: &[&str],
        vendors: &[&str],
    ) -> (Vec<u8>, Vec<u8>) {
        let yanked: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
        let blocks: String = vendors
            .iter()
            .map(|p| format!("[programs.{p}]\nrepo = \"aterm\"\n"))
            .collect();
        let pins: String = vendors
            .iter()
            .map(|p| format!(", {p} = 2026091901"))
            .collect();
        let body = format!(
            "schema = 2\nindex_build = {index_build}\nvalid_until = \"{valid_until}\"\n\
             machine_id = \"{}\"\nroster_seq = {}\n\
             [programs.ay]\nrepo = \"ay\"\n{blocks}\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [{}]\n\
             pin = {{ ay = 18{pins} }}\n",
            crate::sig::testkit::MACHINE_ID,
            crate::sig::testkit::SEQ,
            yanked.join(", ")
        );
        let sig = crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
        (body.into_bytes(), sig)
    }

    /// A fresh 0700 prefix under the temp dir, removed when the test ends.
    pub(crate) fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-vlane-{label}-"))
    }

    /// The store directory of `program`'s vendor `version`.
    pub(crate) fn build_dir(layout: &Layout, program: &str, version: &str) -> PathBuf {
        let v = crate::vendor_direct::Version::parse(version).unwrap();
        layout.build_dir(program, v.build_id())
    }
}

#[cfg(test)]
#[path = "lane_alab_tests.rs"]
mod alab_tests;

#[cfg(test)]
mod tests {
    use super::world::{self, CLAUDE, CODEX_HEAD, Fake, NOW, native_exe};
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn claude() -> &'static VendorSpec {
        super::super::spec("claude").unwrap()
    }

    fn codex() -> &'static VendorSpec {
        super::super::spec("codex").unwrap()
    }

    fn codex_old() -> &'static VendorSpec {
        super::super::spec("codex-old").unwrap()
    }

    fn run(layout: &Layout, fetcher: &Fake, policy: &Policy, spec: &'static VendorSpec) -> Outcome {
        run_with(layout, fetcher, policy, &world::trust(), spec, false)
    }

    fn run_with(
        layout: &Layout,
        fetcher: &Fake,
        policy: &Policy,
        trust: &Trust,
        spec: &'static VendorSpec,
        held: bool,
    ) -> Outcome {
        run_indexed(layout, fetcher, policy, trust, spec, held, None)
    }

    fn run_indexed(
        layout: &Layout,
        fetcher: &Fake,
        policy: &Policy,
        trust: &Trust,
        spec: &'static VendorSpec,
        held: bool,
        legacy_index: Option<&TrustedIndex>,
    ) -> Outcome {
        let lane = Lane {
            layout,
            fetcher,
            triple: world::triple(),
            policy,
            trust,
            legacy_index,
            asked: false,
            now: NOW,
            download_turn: None,
        };
        install_one(&lane, spec, held)
    }

    fn run_asked(layout: &Layout, fetcher: &Fake, spec: &'static VendorSpec) -> Outcome {
        let lane = Lane {
            layout,
            fetcher,
            triple: world::triple(),
            policy: &Policy::default(),
            trust: &world::trust(),
            legacy_index: None,
            asked: true,
            now: NOW,
            download_turn: None,
        };
        install_one(&lane, spec, false)
    }

    fn legacy_claude(l: &Layout, build: u64) {
        let legacy = l.build_dir("claude", build);
        std::fs::create_dir_all(legacy.join("bin")).unwrap();
        std::fs::write(legacy.join(world::bin("claude")), native_exe("legacy")).unwrap();
        crate::activate::install_shims(
            l,
            &legacy,
            &["claude".to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        crate::store::mark_build_ready(&legacy).unwrap();
    }

    fn shim_build(layout: &Layout, program: &str) -> Option<u64> {
        crate::ops::active_builds(layout).get(program).copied()
    }

    fn assert_no_build_id(line: &str) {
        let digits: Vec<&str> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|w| w.len() >= 19)
            .collect();
        assert!(digits.is_empty(), "a vendor build id on a line: {line}");
    }

    #[test]
    fn a_fresh_claude_is_verified_staged_recorded_and_shimmed_with_its_compiled_env() {
        let l = world::layout("fresh-claude");
        let f = Fake::default();
        let payload = native_exe("claude 2.1.281");
        f.publish_claude("2.1.281", &payload);
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        let dir = world::build_dir(&l, "claude", "2.1.281");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        assert_eq!(
            std::fs::read(dir.join(world::bin("claude"))).unwrap(),
            payload
        );
        let record = complete_record(&dir).expect("a complete record");
        assert_eq!(record.version, v("2.1.281"));
        let modes = crate::store::declared_modes(&dir).unwrap();
        assert_eq!(
            record.tree_root,
            crate::tree::tree_root_declared(&dir, &modes).unwrap()
        );
        assert_eq!(record.sha256, world::sha(&payload));
        assert_eq!(
            record.apple_team.as_deref(),
            cfg!(target_os = "macos").then_some("Q6L2SF6YDW")
        );
        assert!(record.build_date.is_some(), "claude's signed buildDate");
        let env = crate::shim_env::read_sidecar(&dir);
        assert_eq!(
            env.entries(),
            [
                ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
                (
                    "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY".to_string(),
                    "1".to_string()
                ),
            ]
        );
        let shim = std::fs::read_to_string(l.shim(&crate::store::ToolName::new("claude").unwrap()))
            .unwrap();
        assert!(shim.contains("DISABLE_AUTOUPDATER"), "{shim}");
        assert_eq!(
            crate::store::recorded_exposes(&dir),
            Some(vec![crate::store::ToolName::new("claude").unwrap()]),
            "the vendor lane records the commands its build puts on PATH, as every landing does"
        );
        let stamp = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(stamp.last_head, Some(v("2.1.281")));
        assert_eq!(stamp.high_water, Some(v("2.1.281")));
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [Fake::claude_payload_url("2.1.281")]
        );
    }

    use world::sha512_hex;

    fn agy() -> &'static VendorSpec {
        super::super::spec("agy").unwrap()
    }

    fn muse() -> &'static VendorSpec {
        super::super::spec("muse").unwrap()
    }

    fn publish_agy(f: &Fake, version: &str, exe: &[u8]) -> String {
        let tarball = world::tarball(&[("antigravity", exe, 0o755)]);
        let (_, dir, file) = resolve::agy_platform(world::triple()).unwrap();
        let release = format!(
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/{version}-42/{dir}/"
        );
        let url = format!("{release}{file}");
        let manifest = format!(
            "{{\"version\":\"{version}\",\"url\":\"{url}\",\"sha512\":\"{}\"}}",
            sha512_hex(&tarball)
        );
        f.doc(agy().head_url, manifest.as_bytes());
        f.doc(&format!("{release}manifest.json"), manifest.as_bytes());
        f.payload(&url, &tarball);
        url
    }

    fn publish_muse(f: &Fake, version: &str, exe: &[u8]) {
        let Some((key, file)) = resolve::muse_platform(world::triple()) else {
            aterm_uds::host::could_not_run(format!(
                "muse is not served for {}, so the landing half did not run",
                world::triple()
            ));
        };
        let version = v(version);
        let url = resolve::muse_url(version, file);
        let manifest_url = resolve::muse_url(version, "manifest.json");
        let channel = format!(
            "{{\"channel\":\"muse-stable\",\"version\":\"{version}\",\"manifest_url\":\
             \"{manifest_url}\",\"urgency\":\"none\",\"min_version\":null}}"
        );
        let manifest = format!(
            "{{\"version\":\"{version}\",\"checksum_algorithm\":\"sha256\",\"artifacts\":\
             {{\"{key}\":{{\"url\":\"{}\",\"checksum\":\"{}\",\"size\":{}}}}}}}",
            url.replace('/', "\\/"),
            world::sha(exe),
            exe.len()
        );
        f.doc(muse().head_url, channel.as_bytes());
        f.doc(&manifest_url, manifest.as_bytes());
        f.payload(&url, exe);
    }

    fn kimi() -> &'static VendorSpec {
        super::super::spec("kimi").unwrap()
    }

    fn grok() -> &'static VendorSpec {
        super::super::spec("grok").unwrap()
    }

    /// Publish kimi `version` as Moonshot's latest: the head, the CDN manifest and the
    /// GitHub release manifest (answered through GitHub's storage) naming `exe`'s sha256,
    /// and the CDN payload; its URL.
    fn publish_kimi(f: &Fake, version: &str, exe: &[u8]) -> String {
        let (key, file) = resolve::kimi_platform(world::triple()).unwrap();
        let tag = format!("@moonshot-ai/kimi-code@{version}");
        let manifest = format!(
            "{{\"version\":\"{version}\",\"tag\":\"{tag}\",\"platforms\":{{\"{key}\":\
             {{\"filename\":\"{file}\",\"checksum\":\"{}\"}}}}}}",
            world::sha(exe)
        );
        f.doc(kimi().head_url, format!("{version}\n").as_bytes());
        let cdn = format!("https://cdn.kimi.com/kimi-code/binaries/{version}/");
        f.doc(&format!("{cdn}manifest.json"), manifest.as_bytes());
        let github = format!(
            "https://github.com/MoonshotAI/kimi-code/releases/download/{tag}/manifest.json"
        );
        f.doc(&github, manifest.as_bytes());
        f.redirect(
            &github,
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/2",
        );
        let url = format!("{cdn}{file}");
        f.payload(&url, exe);
        url
    }

    /// Publish grok `version` of `exe`: npm's signed tarball and xAI's bucket copy
    /// ([`Fake::publish_grok`]). Where xAI serves no build for this host (Windows) the landing
    /// half could not run.
    fn publish_grok(f: &Fake, version: &str, exe: &[u8]) -> world::GrokServed {
        f.publish_grok(version, exe).unwrap_or_else(|| {
            aterm_uds::host::could_not_run(format!(
                "grok is not served for {}, so the landing half did not run",
                world::triple()
            ))
        })
    }

    fn staged(l: &Layout, program: &str) -> Vec<String> {
        std::fs::read_dir(l.staging_dir(program))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_fresh_kimi_lands_the_binary_both_moonshot_manifests_name() {
        let l = world::layout("fresh-kimi");
        let f = Fake::default();
        let exe = native_exe("kimi 2.3.2");
        let url = publish_kimi(&f, "2.3.2", &exe);
        let o = run(&l, &f, &Policy::default(), kimi());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        let build = world::build_dir(&l, "kimi", "2.3.2");
        assert_eq!(std::fs::read(build.join(world::bin("kimi"))).unwrap(), exe);
        let record = complete_record(&build).expect("a complete record");
        assert_eq!(
            (record.sha256.as_str(), record.anchor),
            (
                world::sha(&exe).as_str(),
                super::super::Anchor::MoonshotTwoHost
            )
        );
        assert_eq!(record.source_url, url);
        let env = crate::shim_env::read_sidecar(&build);
        assert_eq!(env.spelled(), "KIMI_CODE_NO_AUTO_UPDATE=1");
        let shim =
            std::fs::read_to_string(l.shim(&crate::store::ToolName::new("kimi").unwrap())).unwrap();
        assert!(shim.contains("KIMI_CODE_NO_AUTO_UPDATE"), "{shim}");
        assert_eq!(f.downloads.borrow().as_slice(), [url]);
        let log = super::super::digests::digests_path(&l, "kimi");
        assert!(
            log.ends_with("vendor/digests-kimi.toml"),
            "{}",
            log.display()
        );
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains(&world::sha(&exe))
        );
    }

    /// A fresh grok: npm's registry-signed tarball (its sha512 logged before a byte moved)
    /// lands its decoded `bin/grok` once xAI's bucket `.zst` decoded to the same bytes; the
    /// bucket's raw binary is only sized, and nothing is left in staging.
    #[test]
    fn a_fresh_grok_lands_npms_signed_binary_once_xais_bucket_agrees() {
        let l = world::layout("fresh-grok");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [served.zst.clone(), served.tarball.clone()],
            "the bucket's .zst, then npm's tarball; never {}",
            served.binary
        );
        let build = world::build_dir(&l, "grok", "1.0.70");
        assert_eq!(std::fs::read(build.join("bin/grok")).unwrap(), exe);
        let bin: Vec<_> = std::fs::read_dir(build.join("bin"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(bin, ["grok"], "the .br is decoded, nothing else is laid");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(build.join("bin/grok"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "{mode:o}");
            let sealed = std::fs::metadata(build.join("bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(
                sealed,
                crate::store::SEALED_BIN_MODE,
                "a one-binary bin/: {sealed:o}"
            );
        }
        let record = complete_record(&build).expect("a complete record");
        assert_eq!(
            (
                record.sha512.as_str(),
                record.sha256.as_str(),
                record.anchor
            ),
            (
                served.sha512.as_str(),
                "",
                super::super::Anchor::XaiNpmRegistry
            )
        );
        assert_eq!(record.source_url, served.tarball);
        assert_eq!(
            record.measured_sha256.as_deref(),
            Some(world::sha(&exe).as_str())
        );
        assert_eq!(record.agreed_with.as_deref(), Some(served.zst.as_str()));
        assert_eq!(
            record.apple_team.as_deref(),
            cfg!(target_os = "macos").then_some("5Y6N3AJ54S")
        );
        let log = std::fs::read_to_string(super::super::digests::digests_path(&l, "grok")).unwrap();
        assert!(log.contains(&served.sha512), "{log}");
        assert_eq!(
            crate::shim_env::read_sidecar(&build).spelled(),
            "GROK_DISABLE_AUTOUPDATER=1"
        );
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    }

    /// A build xAI's bucket does not serve: the binary npm's signed tarball decodes to is
    /// refused when the bucket's copy (as long, other bytes) is not it, and memoized for that
    /// tarball's sha512, which was logged before the download; a person clearing the memo
    /// lands it once the bucket agrees.
    #[test]
    fn a_binary_the_bucket_does_not_agree_with_is_refused_and_memoized() {
        let l = world::layout("grok-disagree");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let mut other = exe.clone();
        *other.last_mut().unwrap() ^= 1;
        f.publish_grok_bucket("1.0.70", &exe, &other);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(
            o.line().contains(&format!(
                "is not xAI's own build, sha256 {} ({} bytes)",
                world::sha(&other),
                other.len()
            )),
            "{}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), 2, "the .zst and the tarball");
        let stamp = ProgramStamp::read(&l, "grok").unwrap();
        assert_eq!(
            stamp.refused.as_ref().map(|(v, d)| (*v, d.as_str())),
            Some((v("1.0.70"), served.sha512.as_str()))
        );
        let log = std::fs::read_to_string(super::super::digests::digests_path(&l, "grok")).unwrap();
        assert!(log.contains(&served.sha512), "first seen at plan: {log}");
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        assert_eq!(shim_build(&l, "grok"), None, "nothing laid");
        assert!(!crate::store::build_is_complete(&world::build_dir(
            &l, "grok", "1.0.70"
        )));
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(
            o.line().contains("refused these exact bytes"),
            "{}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), 2, "not fetched again");
        let memo = crate::store::stage_refusal(&world::build_dir(&l, "grok", "1.0.70"))
            .expect("the store's memo of the refusal");
        assert!(
            memo.why.contains(&world::sha(&other)) && memo.why.contains(&world::sha(&exe)),
            "the memo keeps both digests whole: {}",
            memo.why
        );
        ProgramStamp::clear_refusal(&l, "grok").unwrap();
        assert_eq!(
            crate::store::clear_stage_refusals(&l, "grok"),
            1,
            "a person's retry"
        );
        f.publish_grok_bucket("1.0.70", &exe, &exe);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }

    /// npm's tarball must be the bytes its signed document names: one flipped byte, at the
    /// size the registry answered, is a digest mismatch, refused before anything is staged.
    /// As for every vendor the first retry is free; once two passes saw the same bytes the
    /// refusal binds, and the bucket's copy is not fetched for a tarball the landing would
    /// refuse before its download.
    #[test]
    fn a_tarball_other_than_the_signed_one_is_refused_before_staging() {
        let l = world::layout("grok-tampered");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let mut tgz = world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe);
        assert_eq!(
            world::sha512_hex(&tgz),
            served.sha512,
            "the signed tarball, rebuilt"
        );
        let middle = tgz.len() / 2;
        tgz[middle] ^= 1;
        f.payload(&served.tarball, &tgz);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("sha512"), "{}", o.line());
        assert_eq!(f.downloads.borrow().len(), 2, "the .zst and the tarball");
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        assert_eq!(shim_build(&l, "grok"), None, "nothing laid");
        assert!(!crate::store::build_is_complete(&world::build_dir(
            &l, "grok", "1.0.70"
        )));
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert_eq!(f.downloads.borrow().len(), 4, "the first retry is free");
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("not downloaded again"), "{}", o.line());
        assert_eq!(
            f.downloads.borrow().len(),
            4,
            "two identical verdicts: neither download again"
        );
    }

    /// A bucket copy that is not one frame decoding to the binary's declared size and checksum
    /// fails the pass before npm's tarball is fetched, memoizing nothing (a torn or
    /// half-published copy heals), and the next pass lands once it is.
    #[test]
    fn a_bucket_copy_that_does_not_decode_fails_unmemoized() {
        let exe = native_exe("grok 1.0.70");
        let mut longer = exe.clone();
        longer.push(b'!');
        let checksum_flipped = {
            let mut zst = world::zstd(&exe);
            *zst.last_mut().unwrap() ^= 1;
            zst
        };
        let unchecked = {
            let mut zst = Vec::new();
            aterm_zstd::frame::write_raw(&mut zst, &exe, aterm_zstd::frame::Checksum::Omitted)
                .unwrap();
            zst
        };
        let one_frame = "more than the one zstd frame";
        let trailing_empty = [world::zstd(&exe), world::zstd(b"")].concat();
        let skippable = [0x184D_2A50u32.to_le_bytes(), 7u32.to_le_bytes()].concat();
        let trailing_skippable = [&world::zstd(&exe)[..], &skippable, b"padding"].concat();
        let leading_skippable = [&skippable[..], b"padding", &world::zstd(&exe)].concat();
        for (label, zst, why) in [
            ("declares", world::zstd(&longer), "its zstd frame declares"),
            ("checksum", checksum_flipped, "could not be checked"),
            ("unchecked", unchecked, "could not be checked"),
            ("empty", Vec::new(), "no content length"),
            ("trailing-empty", trailing_empty, one_frame),
            ("trailing-skippable", trailing_skippable, one_frame),
            ("leading-skippable", leading_skippable, "not a frame"),
        ] {
            let l = world::layout(&format!("grok-zst-{label}"));
            let f = Fake::default();
            let served = publish_grok(&f, "1.0.70", &exe);
            f.payload(&served.zst, &zst);
            let o = run(&l, &f, &Policy::default(), grok());
            assert!(
                !matches!(o.verdict, Verdict::Installed { .. }),
                "{label}: {o:?}"
            );
            assert!(o.line().contains(why), "{label}: {}", o.line());
            assert!(
                !f.downloads.borrow().contains(&served.tarball),
                "{label}: npm's tarball is not fetched"
            );
            assert_eq!(
                ProgramStamp::read(&l, "grok").and_then(|s| s.refused),
                None,
                "{label}: nothing memoized"
            );
            assert!(
                staged(&l, "grok").is_empty(),
                "{label}: {:?}",
                staged(&l, "grok")
            );
            f.publish_grok_bucket("1.0.70", &exe, &exe);
            let o = run(&l, &f, &Policy::default(), grok());
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "{label}: {o:?}"
            );
        }
    }

    /// A grok landing's disk gate holds room for npm's tarball, the bucket's `.zst` and the
    /// binary at once; planning it downloads nothing.
    #[test]
    fn a_grok_landing_needs_room_for_the_tarball_the_bucket_copy_and_the_binary() {
        let l = world::layout("grok-room");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let lane = Lane {
            layout: &l,
            fetcher: &f,
            triple: world::triple(),
            policy: &Policy::default(),
            trust: &world::trust(),
            legacy_index: None,
            asked: false,
            now: NOW,
            download_turn: None,
        };
        let Step::Land(pending) = plan_one(&lane, grok(), false) else {
            panic!("a grok to land");
        };
        let tgz = world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe).len() as u64;
        let zst = world::zstd(&exe).len() as u64;
        let need = pending.landing_bytes().need();
        assert_eq!(need, tgz + zst + exe.len() as u64);
        assert_eq!(pending.download_bytes(), tgz + zst);
        assert_eq!(pending.disk_bytes(), Some(exe.len() as u64));
        assert!(f.downloads.borrow().is_empty(), "{:?}", served.tarball);
    }

    /// The agreement step downloads the bucket's `.zst` alone, decodes it to the binary's
    /// sha256 and length, and deletes it before npm's tarball is requested.
    #[test]
    fn the_bucket_copy_is_decoded_and_deleted_before_the_tarball_is_fetched() {
        let l = world::layout("grok-agree");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let lane = Lane {
            layout: &l,
            fetcher: &f,
            triple: world::triple(),
            policy: &Policy::default(),
            trust: &world::trust(),
            legacy_index: None,
            asked: false,
            now: NOW,
            download_turn: None,
        };
        let Step::Land(pending) = plan_one(&lane, grok(), false) else {
            panic!("a grok to land");
        };
        let agreed = agree(&lane, &pending)
            .unwrap()
            .expect("grok has a bucket copy");
        assert_eq!(
            agreed,
            Agreement {
                url: served.zst.clone(),
                sha256: world::sha(&exe),
                len: exe.len() as u64,
            }
        );
        assert_eq!(f.downloads.borrow().as_slice(), [served.zst]);
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    }

    /// A grok landing's bucket copy and npm's tarball are one turn on the link: it keeps the
    /// turn it took for the copy, so the tarball's download, which waits on that same ticket,
    /// starts at once, and the next ticket's transfer starts only once the landing ends,
    /// landed or failed.
    #[test]
    fn a_grok_landing_holds_its_download_turn_from_the_bucket_copy_to_the_tarball() {
        let turns = crate::flow::DownloadTurns::default();
        let (first, second, third) = (turns.ticket(), turns.ticket(), turns.ticket());
        let (policy, trust) = (Policy::default(), world::trust());
        let lane = |l, f, ticket| Lane {
            layout: l,
            fetcher: f,
            triple: world::triple(),
            policy: &policy,
            trust: &trust,
            legacy_index: None,
            asked: false,
            now: NOW,
            download_turn: Some(ticket),
        };
        let l = world::layout("grok-turn");
        let f = Fake::default();
        publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
        let landing = lane(&l, &f, &first);
        let Step::Land(pending) = plan_one(&landing, grok(), false) else {
            panic!("a grok to land");
        };
        agree(&landing, &pending)
            .unwrap()
            .expect("grok has a bucket copy");
        assert!(
            first.holds_the_link() && !second.holds_the_link(),
            "the bucket copy gave up the turn npm's tarball still needs"
        );
        let o = land_one(&landing, &pending);
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert!(
            second.holds_the_link(),
            "the landing ended: the next transfer may start"
        );
        let l = world::layout("grok-turn-failed");
        let f = Fake::default();
        publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
        f.downloads_fail.set(true);
        let failing = lane(&l, &f, &second);
        let Step::Land(pending) = plan_one(&failing, grok(), false) else {
            panic!("a grok to land");
        };
        let o = land_one(&failing, &pending);
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        assert!(third.holds_the_link(), "a failed landing gave up its turn");
    }

    /// A short bucket download (fewer bytes than the bucket sized) fails unmemoized and lands on
    /// the next pass.
    #[test]
    fn a_short_bucket_download_fails_unmemoized_and_lands_on_the_next_pass() {
        let l = world::layout("grok-short");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.70");
        let served = publish_grok(&f, "1.0.70", &exe);
        let lane = Lane {
            layout: &l,
            fetcher: &f,
            triple: world::triple(),
            policy: &Policy::default(),
            trust: &world::trust(),
            legacy_index: None,
            asked: false,
            now: NOW,
            download_turn: None,
        };
        let Step::Land(pending) = plan_one(&lane, grok(), false) else {
            panic!("a grok to land");
        };
        let zst = world::zstd(&exe);
        f.payload(&served.zst, &zst[..zst.len() - 1]);
        let o = land_one(&lane, &pending);
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        assert!(o.line().contains("of its"), "{}", o.line());
        assert_eq!(
            f.downloads.borrow().as_slice(),
            std::slice::from_ref(&served.zst)
        );
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        assert_eq!(ProgramStamp::read(&l, "grok").unwrap().refused, None);
        f.payload(&served.zst, &zst);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }

    /// A document or copy not served (npm's version document, the bucket's binary or its
    /// `.zst`) is no verdict: nothing is downloaded or memoized, and the build lands once it
    /// is served.
    #[test]
    fn an_unserved_npm_document_or_bucket_copy_is_no_verdict() {
        let exe = native_exe("grok 1.0.70");
        for which in ["npm", "binary", "zst", "tarball"] {
            let l = world::layout(&format!("grok-unserved-{which}"));
            let f = Fake::default();
            let served = publish_grok(&f, "1.0.70", &exe);
            let version_doc = world::grok_platform().unwrap().version_doc(v("1.0.70"));
            let doc = f.body(&version_doc);
            match which {
                "npm" => f.forget(&version_doc),
                "binary" => f.forget_payload(&served.binary),
                "zst" => f.forget_payload(&served.zst),
                _ => f.forget_payload(&served.tarball),
            }
            let o = run(&l, &f, &Policy::default(), grok());
            assert!(
                matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
                "{which}: {o:?}"
            );
            assert!(!o.is_failure(), "{which}");
            assert!(f.downloads.borrow().is_empty(), "{which}: nothing fetched");
            assert_eq!(
                ProgramStamp::read(&l, "grok").and_then(|s| s.refused),
                None,
                "{which}"
            );
            f.doc(&version_doc, &doc);
            f.publish_grok_bucket("1.0.70", &exe, &exe);
            f.payload(
                &served.tarball,
                &world::grok_tgz(world::grok_platform().unwrap(), "1.0.70", &exe),
            );
            let o = run(&l, &f, &Policy::default(), grok());
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "{which}: {o:?}"
            );
        }
    }

    /// npm's version document signed by a key aterm does not ship (the test key, against the
    /// compiled registry key) is refused before a byte moves, naming the key.
    #[test]
    fn a_version_document_under_a_key_aterm_does_not_ship_is_refused_before_any_download() {
        let l = world::layout("grok-unknown-key");
        let f = Fake::default();
        publish_grok(&f, "1.0.70", &native_exe("grok 1.0.70"));
        let production = Trust {
            npm_keys: super::super::npm::NPM_REGISTRY_KEYS,
            ..world::trust()
        };
        let o = run_with(&l, &f, &Policy::default(), &production, grok(), false);
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(
            o.line().contains(&format!(
                "aterm does not ship ({})",
                world::npm_test_keys()[0].keyid
            )),
            "{}",
            o.line()
        );
        assert!(f.downloads.borrow().is_empty(), "nothing fetched");
        assert!(!super::super::digests::digests_path(&l, "grok").exists());
    }

    /// A tarball npm signed but the stager refuses — a member out of place or of another kind,
    /// a binary decoding to other than the bucket's size, a package.json naming another
    /// version — lays nothing.
    #[test]
    fn a_signed_tarball_the_stager_refuses_lays_nothing() {
        let Some(platform) = world::grok_platform() else {
            aterm_uds::host::could_not_run("grok is not served for this host");
        };
        let exe = native_exe("grok 1.0.70");
        let mut longer = exe.clone();
        longer.push(b'!');
        let json = world::grok_package_json(platform, "1.0.70");
        let other_version = world::grok_package_json(platform, "1.0.69");
        let br = world::brotli(&exe);
        let mut trailing = br.clone();
        trailing.push(0);
        let cases: Vec<(&str, Vec<u8>, &[u8], &str)> = vec![
            (
                "symlink",
                world::npm_tgz(&[
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'2', "../../evil", b"", 0o777),
                ]),
                &exe,
                "an npm binary package holds only regular files",
            ),
            (
                "directory",
                world::npm_tgz(&[
                    ("package/", b'5', "", b"", 0o755),
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                ]),
                &exe,
                "an npm binary package holds only regular files",
            ),
            (
                "beside-in-bin",
                world::npm_tgz(&[
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                    ("package/bin/helper", b'0', "", b"x", 0o755),
                ]),
                &exe,
                "\"package/bin/helper\"",
            ),
            (
                "deeper",
                world::npm_tgz(&[
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                    ("package/lib/x.js", b'0', "", b"x", 0o644),
                ]),
                &exe,
                "\"package/lib/x.js\"",
            ),
            (
                "second-br",
                world::npm_tgz(&[
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                ]),
                &exe,
                "or a second one",
            ),
            (
                "no-manifest",
                world::npm_tgz(&[("package/bin/grok.br", b'0', "", &br, 0o644)]),
                &exe,
                "holds no package/package.json",
            ),
            (
                "trailing",
                world::npm_tgz(&[
                    ("package/package.json", b'0', "", json.as_bytes(), 0o644),
                    ("package/bin/grok.br", b'0', "", &trailing, 0o644),
                ]),
                &exe,
                "bytes follow the stream's last meta-block",
            ),
            (
                "shorter",
                world::grok_tgz(platform, "1.0.70", &exe),
                &longer,
                "decodes to a length other than the binary's",
            ),
            (
                "longer",
                world::grok_tgz(platform, "1.0.70", &longer),
                &exe,
                "decodes to a length other than the binary's",
            ),
            (
                "manifest",
                world::npm_tgz(&[
                    (
                        "package/package.json",
                        b'0',
                        "",
                        other_version.as_bytes(),
                        0o644,
                    ),
                    ("package/bin/grok.br", b'0', "", &br, 0o644),
                ]),
                &exe,
                "is version \"1.0.69\", not 1.0.70",
            ),
        ];
        for (label, tgz, bucket, why) in cases {
            let l = world::layout(&format!("grok-tar-{label}"));
            let f = Fake::default();
            f.publish_grok_tarball("1.0.70", &tgz);
            f.publish_grok_bucket("1.0.70", bucket, bucket);
            let o = run(&l, &f, &Policy::default(), grok());
            assert!(
                !matches!(o.verdict, Verdict::Installed { .. }),
                "{label}: {o:?}"
            );
            assert!(
                o.line().contains(why),
                "{label}: want {why:?} in {}",
                o.line()
            );
            assert_eq!(shim_build(&l, "grok"), None, "{label}");
            assert!(
                !crate::store::build_is_complete(&world::build_dir(&l, "grok", "1.0.70")),
                "{label}"
            );
        }
    }

    /// The team pin, for each new row: a darwin Mach-O signed by another team is refused and
    /// memoized, and its version is not fetched again until a person asks; kimi's and grok's
    /// documented digests were logged before the download, as codex's is.
    #[test]
    fn a_build_signed_by_another_team_is_refused_and_memoized_for_kimi_and_grok() {
        let refusing = world::trust_refusing_signers();
        let l = world::layout("kimi-team");
        let f = Fake::default();
        publish_kimi(&f, "2.3.2", &native_exe("not Moonshot's"));
        let o = run_with(&l, &f, &Policy::default(), &refusing, kimi(), false);
        if !cfg!(target_os = "macos") {
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "no gate off darwin: {o:?}"
            );
            return;
        }
        assert!(o.line().contains("signer refused"), "{}", o.line());
        let downloads = f.downloads.borrow().len();
        run_with(&l, &f, &Policy::default(), &refusing, kimi(), false);
        assert_eq!(f.downloads.borrow().len(), downloads, "memoized");
        assert_eq!(shim_build(&l, "kimi"), None);

        let l = world::layout("grok-team");
        let f = Fake::default();
        let served = publish_grok(&f, "1.0.70", &native_exe("not xAI's"));
        let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
        assert!(o.line().contains("signer refused"), "{}", o.line());
        let log = super::super::digests::digests_path(&l, "grok");
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains(&served.sha512),
            "npm's signed digest was logged before the download"
        );
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        let downloads = f.downloads.borrow().len();
        let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
        assert!(
            o.line().contains("refused these exact bytes"),
            "{}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), downloads, "not fetched again");
        ProgramStamp::clear_refusal(&l, "grok").unwrap();
        assert_eq!(
            crate::store::clear_stage_refusals(&l, "grok"),
            1,
            "a person's retry"
        );
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }

    #[test]
    fn a_fresh_agy_lands_its_one_file_under_the_sha512_both_google_hosts_name() {
        let l = world::layout("fresh-agy");
        let f = Fake::default();
        let exe = native_exe("agy 1.3.7");
        let url = publish_agy(&f, "1.3.7", &exe);
        let sha512 = sha512_hex(&world::tarball(&[("antigravity", &exe, 0o755)]));
        let o = run(&l, &f, &Policy::default(), agy());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        let build = world::build_dir(&l, "agy", "1.3.7");
        assert_eq!(std::fs::read(build.join(world::bin("agy"))).unwrap(), exe);
        let record = complete_record(&build).expect("a complete record");
        assert_eq!(
            (record.sha256.as_str(), record.sha512.as_str()),
            ("", sha512.as_str())
        );
        assert_eq!(record.source_url, url);
        let env = crate::shim_env::read_sidecar(&build);
        assert_eq!(env.spelled(), "AGY_CLI_DISABLE_AUTO_UPDATE=true");
        let shim =
            std::fs::read_to_string(l.shim(&crate::store::ToolName::new("agy").unwrap())).unwrap();
        assert!(shim.contains("AGY_CLI_DISABLE_AUTO_UPDATE"), "{shim}");
        assert_eq!(f.downloads.borrow().as_slice(), [url]);
    }

    #[test]
    fn muse_lands_where_it_is_served_and_is_unavailable_elsewhere() {
        let elsewhere = "x86_64-unknown-linux-gnu";
        let l = world::layout("muse");
        let f = Fake::default();
        let lane = Lane {
            layout: &l,
            fetcher: &f,
            triple: elsewhere,
            policy: &Policy::default(),
            trust: &world::trust(),
            legacy_index: None,
            asked: true,
            now: NOW,
            download_turn: None,
        };
        let o = install_one(&lane, muse(), false);
        assert!(matches!(o.verdict, Verdict::Unavailable { .. }));
        assert!(!o.is_failure());
        assert!(f.gets.borrow().is_empty(), "no host was asked");
        assert!(ProgramStamp::read(&l, "muse").is_none(), "nothing recorded");
        let exe = native_exe("muse 1.4.9-R6298");
        publish_muse(&f, "1.4.9-R6298", &exe);
        let o = run(&l, &f, &Policy::default(), muse());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let build = world::build_dir(&l, "muse", "1.4.9-R6298");
        assert_eq!(std::fs::read(build.join("bin/muse")).unwrap(), exe);
        assert_eq!(complete_record(&build).unwrap().sha256, world::sha(&exe));
        assert!(crate::shim_env::read_sidecar(&build).is_empty());
    }

    #[test]
    fn an_upgrade_moves_and_an_unchanged_head_costs_one_conditional_get() {
        let l = world::layout("upgrade");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.282", &native_exe("b"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
        let gets_before = f.gets.borrow().len();
        let downloads_before = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
        let gets = f.gets.borrow()[gets_before..].to_vec();
        assert_eq!(gets.len(), 1, "{gets:?}");
        assert_eq!(gets[0].0, format!("{CLAUDE}latest"));
        assert!(gets[0].1.is_some(), "the check is conditional");
        assert_eq!(f.downloads.borrow().len(), downloads_before);
    }

    #[test]
    fn a_regressed_head_is_kept_at_and_an_unreachable_one_is_kept_quietly() {
        let l = world::layout("regress");
        let f = Fake::default();
        f.publish_claude("2.1.286", &native_exe("new"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.283", &native_exe("old"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::HeadOlder,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.286").build_id()));
        f.offline.set(true);
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(!o.is_failure());
    }

    #[test]
    fn a_refused_document_stages_nothing_and_downloads_nothing() {
        type Publish = Box<dyn Fn(&Fake)>;
        let cases: Vec<(&str, &'static VendorSpec, Publish)> = vec![
            (
                "bad-signature",
                claude(),
                Box::new(|f: &Fake| {
                    f.publish_claude("2.1.282", &native_exe("x"));
                    let other =
                        crate::openpgp::testkit::sign_detached(b"another document", 1_790_000_000);
                    f.doc(&format!("{CLAUDE}2.1.282/manifest.json.sig"), &other);
                }),
            ),
            (
                "manifest-off-host",
                claude(),
                Box::new(|f: &Fake| {
                    f.publish_claude("2.1.282", &native_exe("x"));
                    let url = format!("{CLAUDE}2.1.282/manifest.json");
                    f.redirect(&url, "https://evil.example/manifest.json");
                }),
            ),
            (
                "sums-disagree",
                codex(),
                Box::new(|f: &Fake| {
                    f.publish_codex_default("0.157.0");
                    let url = format!("{}rust-v0.157.0/codex-package_SHA256SUMS", world::CODEX_DL);
                    let sums = format!("{}  {}\n", "a".repeat(64), Fake::codex_asset());
                    f.doc(&url, sums.as_bytes());
                }),
            ),
            (
                "sums-off-host",
                codex(),
                Box::new(|f: &Fake| {
                    f.publish_codex_default("0.157.0");
                    let url = format!("{}rust-v0.157.0/codex-package_SHA256SUMS", world::CODEX_DL);
                    f.redirect(&url, "https://raw.githubusercontent.com/openai/codex/x");
                }),
            ),
            (
                "tag-mismatch",
                codex(),
                Box::new(|f: &Fake| {
                    f.publish_codex_default("0.157.0");
                    let body = String::from_utf8(f.body(CODEX_HEAD)).unwrap();
                    f.doc(
                        CODEX_HEAD,
                        body.replace("rust-v0.157.0", "v0.157.0").as_bytes(),
                    );
                }),
            ),
            (
                "non-canonical-head",
                claude(),
                Box::new(|f: &Fake| {
                    f.publish_claude("2.1.282", &native_exe("x"));
                    f.doc(&format!("{CLAUDE}latest"), b"2.1.0281\n");
                }),
            ),
            (
                "kimi-manifests-disagree",
                kimi(),
                Box::new(|f: &Fake| {
                    publish_kimi(f, "2.3.2", &native_exe("x"));
                    let url = "https://cdn.kimi.com/kimi-code/binaries/2.3.2/manifest.json";
                    let body = String::from_utf8(f.body(url)).unwrap();
                    let other = body.replace(&world::sha(&native_exe("x")), &"a".repeat(64));
                    f.doc(url, other.as_bytes());
                }),
            ),
            (
                "kimi-github-off-host",
                kimi(),
                Box::new(|f: &Fake| {
                    publish_kimi(f, "2.3.2", &native_exe("x"));
                    f.redirect(
                        "https://github.com/MoonshotAI/kimi-code/releases/download/\
                         @moonshot-ai/kimi-code@2.3.2/manifest.json",
                        "https://raw.githubusercontent.com/MoonshotAI/kimi-code/x",
                    );
                }),
            ),
            (
                "duplicate-key",
                codex(),
                Box::new(|f: &Fake| {
                    f.publish_codex_default("0.157.0");
                    let body = String::from_utf8(f.body(CODEX_HEAD)).unwrap();
                    let twice = body.replacen('{', "{\"tag_name\":\"rust-v0.157.0\",", 1);
                    f.doc(CODEX_HEAD, twice.as_bytes());
                }),
            ),
        ];
        for (label, spec, publish) in cases {
            let l = world::layout(&format!("refuse-{label}"));
            let f = Fake::default();
            publish(&f);
            for pass in 0..2 {
                let o = run(&l, &f, &Policy::default(), spec);
                assert!(
                    matches!(o.verdict, Verdict::Refused { .. }),
                    "{label} pass {pass}: {o:?}"
                );
                assert!(o.is_failure(), "{label}");
                assert!(
                    f.downloads.borrow().is_empty(),
                    "{label}: nothing downloaded"
                );
                assert_eq!(
                    shim_build(&l, spec.program),
                    None,
                    "{label}: nothing shimmed"
                );
                assert!(
                    crate::ops::list_installed(&l).is_empty(),
                    "{label}: nothing staged"
                );
            }
        }
    }

    #[test]
    fn a_digest_mismatch_is_refused_memoized_and_never_downloaded_again() {
        let l = world::layout("digest");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("installed"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.282", &native_exe("signed bytes!"));
        f.payload(
            &Fake::claude_payload_url("2.1.282"),
            &native_exe("swapped bytes"),
        );
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        assert!(!crate::store::build_is_complete(&world::build_dir(
            &l, "claude", "2.1.282"
        )));
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().len(),
            downloads + 1,
            "the first retry is free"
        );
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().len(),
            downloads,
            "two identical verdicts: no re-download"
        );
        assert_no_build_id(&o.line());
    }

    #[test]
    fn a_digest_mismatch_heals_when_the_bytes_do() {
        let l = world::layout("digest-heal");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("installed"));
        run(&l, &f, &Policy::default(), claude());
        let signed = native_exe("signed bytes!");
        f.publish_claude("2.1.282", &signed);
        let url = Fake::claude_payload_url("2.1.282");
        f.payload(&url, &native_exe("torn bytes!!!"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        f.payload(&url, &signed);
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    }

    #[test]
    fn a_document_the_host_does_not_serve_is_no_verdict() {
        let l = world::layout("unserved");
        let f = Fake::default();
        f.publish_codex_default("0.157.0");
        run(&l, &f, &Policy::default(), codex());
        f.publish_codex_default("0.157.1");
        f.forget(&format!(
            "{}rust-v0.157.1/codex-package_SHA256SUMS",
            world::CODEX_DL
        ));
        let yanked = Policy::from_entries(["codex@0.157.0"]);
        let o = run(&l, &f, &yanked, codex());
        assert!(
            matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
            "{o:?}"
        );
        assert!(!o.is_failure());
        assert!(
            o.line().contains("did not serve it") && o.line().contains("404"),
            "{}",
            o.line()
        );
        assert_eq!(
            shim_build(&l, "codex"),
            Some(v("0.157.0").build_id()),
            "not tombstoned"
        );
        f.forget(CODEX_HEAD);
        let o = run(&l, &f, &yanked, codex());
        assert!(
            matches!(o.verdict, Verdict::Unreachable { why: Some(_), .. }),
            "{o:?}"
        );
        assert!(!o.is_failure());
        assert_eq!(
            shim_build(&l, "codex"),
            Some(v("0.157.0").build_id()),
            "not tombstoned"
        );
    }

    #[test]
    fn an_unreadable_digest_log_fetches_nothing() {
        let l = world::layout("digest-log");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        let log = super::super::digests::digests_path(&l, "claude");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "not toml {{{").unwrap();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        assert!(o.line().contains("digest log"), "{}", o.line());
        assert!(f.downloads.borrow().is_empty(), "nothing fetched");
        assert_eq!(shim_build(&l, "claude"), None);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "not toml {{{");
    }

    /// A signed-yank rollback lays the table's exposes only — never a shim for another
    /// executable the retained build's `bin/` happens to hold.
    #[cfg(unix)]
    #[test]
    fn a_rollback_shims_the_tables_exposes_only() {
        let l = world::layout("rollback-exposes");
        let f = Fake::default();
        let publish = |version: &str| {
            let codex = native_exe(&format!("codex {version}"));
            let host = native_exe("code-mode-host");
            f.publish_codex(
                version,
                &[
                    ("bin/codex", &codex, 0o755),
                    ("bin/codex-code-mode-host", &host, 0o755),
                ],
            );
        };
        publish("0.157.0");
        run(&l, &f, &Policy::default(), codex());
        publish("0.157.1");
        run(&l, &f, &Policy::default(), codex());
        let host = crate::store::ToolName::new("codex-code-mode-host").unwrap();
        assert!(
            !l.shim(&host).exists(),
            "PRECONDITION: a landing shims the exposes only"
        );
        let o = run(&l, &f, &Policy::from_entries(["codex@0.157.1"]), codex());
        assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "codex"), Some(v("0.157.0").build_id()));
        assert!(
            !l.shim(&host).exists(),
            "a rollback lays no shim the table does not expose"
        );
    }

    #[test]
    fn a_signer_refusal_is_memoized_like_a_digest_refusal() {
        let l = world::layout("signer");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("x"));
        let trust = world::trust_refusing_signers();
        let o = run_with(&l, &f, &Policy::default(), &trust, claude(), false);
        if cfg!(target_os = "macos") {
            assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
            assert!(o.line().contains("signer refused"), "{}", o.line());
            let downloads = f.downloads.borrow().len();
            run_with(&l, &f, &Policy::default(), &trust, claude(), false);
            assert_eq!(f.downloads.borrow().len(), downloads);
            assert_eq!(shim_build(&l, "claude"), None);
        } else {
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "no Apple anchor off macOS"
            );
        }
    }

    #[test]
    fn a_signature_check_that_did_not_finish_defers_and_the_next_pass_lands_it() {
        let l = world::layout("deferred");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.282", &native_exe("b"));
        let slow = world::trust_timing_out();
        let o = run_with(&l, &f, &Policy::default(), &slow, claude(), false);
        if !cfg!(target_os = "macos") {
            assert!(
                matches!(o.verdict, Verdict::Installed { .. }),
                "no Apple anchor off macOS"
            );
            return;
        }
        assert!(matches!(o.verdict, Verdict::Deferred { .. }), "{o:?}");
        assert!(!o.is_failure(), "a wait is never a failure");
        assert_eq!(
            crate::cli::left_for_next_pass(&format!("atpkg: codex current\n{}\n", o.line())),
            [o.line()],
            "the pass's line leaves claude for the next pass"
        );
        assert_eq!(o.active_version(), Some(v("2.1.281")));
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
        assert_eq!(
            f.downloads.borrow().len(),
            downloads,
            "the verified archive was kept for the retry"
        );
    }

    #[test]
    fn a_yanked_head_is_skipped_and_a_hold_keeps_the_build() {
        let l = world::layout("yank-head");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.282", &native_exe("b"));
        let yanked = Policy::from_entries(["claude@2.1.282"]);
        let o = run(&l, &f, &yanked, claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::HeadYanked,
                    ..
                }
            ),
            "{o:?}"
        );
        assert!(
            f.downloads.borrow().len() == 1,
            "nothing fetched for a yanked head"
        );
        let o = run_with(&l, &f, &Policy::default(), &world::trust(), claude(), true);
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::Held,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
    }

    #[test]
    fn a_yanked_installed_version_moves_to_the_head_or_rolls_back() {
        let l = world::layout("yank-installed");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude("2.1.282", &native_exe("b"));
        run(&l, &f, &Policy::default(), claude());
        let yanked = Policy::from_entries(["claude@2.1.282"]);
        let o = run(&l, &f, &yanked, claude());
        assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        assert_eq!(
            ProgramStamp::read(&l, "claude").unwrap().high_water,
            Some(v("2.1.281"))
        );
        f.publish_claude("2.1.283", &native_exe("c"));
        let o = run(&l, &f, &yanked, claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Newer,
                    ..
                }
            ),
            "{o:?}"
        );
        f.publish_claude("2.1.284", &native_exe("d"));
        let o = run(&l, &f, &Policy::from_entries(["claude@2.1.283"]), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::ReplacesYanked,
                    ..
                }
            ),
            "{o:?}"
        );
        let o = run(
            &l,
            &f,
            &Policy::from_entries(["claude@2.1.283", "claude@2.1.284"]),
            claude(),
        );
        assert!(matches!(o.verdict, Verdict::RolledBack { .. }), "{o:?}");
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    }

    #[test]
    fn a_yanked_version_with_nothing_admissible_is_tombstoned() {
        let l = world::layout("tombstone");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        run(&l, &f, &Policy::default(), claude());
        let yanked = Policy::from_entries(["claude@2.1.281"]);
        f.offline.set(true);
        let o = run(&l, &f, &yanked, claude());
        assert!(
            matches!(o.verdict, Verdict::Unreachable { .. }),
            "an unread head proves nothing inadmissible: {o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        f.offline.set(false);
        let o = run(&l, &f, &yanked, claude());
        assert!(matches!(o.verdict, Verdict::Tombstoned { .. }), "{o:?}");
        assert!(o.is_failure());
    }

    /// The genuine codex shape: an executable-mode data file is demoted before the gate,
    /// the record's root is the root of the tree as it stands, and the entry keeps its bits.
    #[cfg(unix)]
    #[test]
    fn a_codex_tree_is_demoted_gated_and_recorded_as_it_stands() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = world::layout("codex");
        let f = Fake::default();
        f.publish_codex_default("0.157.0");
        let o = run(&l, &f, &Policy::default(), codex());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let dir = world::build_dir(&l, "codex", "0.157.0");
        let mode = |rel: &str| {
            std::fs::metadata(dir.join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("codex-resources/voice/runtime.json"), 0o644);
        assert_eq!(mode("bin/codex"), 0o755);
        assert_eq!(mode("codex-path/rg"), 0o755);
        let record = complete_record(&dir).unwrap();
        assert_eq!(record.tree_root, crate::tree::tree_root(&dir).unwrap());
        assert!(crate::shim_env::read_sidecar(&dir).entries().is_empty());
        assert_eq!(
            crate::verify::verify_program(&l, "codex"),
            crate::verify::VerifyOutcome::VendorRoot {
                build: v("0.157.0").build_id(),
                vendor: "OpenAI".into(),
                staged_from: crate::verify::StagedFrom::Documents,
                drift: None,
            }
        );
        assert_eq!(f.downloads.borrow().len(), 1);
    }

    /// An interpreter script in a vendor tree never runs: the darwin gate refuses the
    /// tree; elsewhere it lands without its execute bits.
    #[cfg(unix)]
    #[test]
    fn a_script_in_a_codex_tree_is_refused_or_demoted() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = world::layout("codex-script");
        let f = Fake::default();
        let codex_bin = native_exe("codex");
        f.publish_codex(
            "0.157.0",
            &[
                ("bin/codex", &codex_bin, 0o755),
                ("codex-path/rg", b"#!/bin/sh\nexec evil\n", 0o755),
            ],
        );
        let o = run(&l, &f, &Policy::default(), codex());
        if cfg!(target_os = "macos") {
            assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
            assert!(o.line().contains("interpreter script"), "{}", o.line());
            assert_eq!(shim_build(&l, "codex"), None);
        } else {
            assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
            let rg = world::build_dir(&l, "codex", "0.157.0").join("codex-path/rg");
            assert_eq!(
                std::fs::metadata(rg).unwrap().permissions().mode() & 0o111,
                0
            );
        }
    }

    #[test]
    fn codex_old_lands_openais_build_under_its_own_name() {
        let l = world::layout("codex-old");
        let f = Fake::default();
        f.publish_codex_default("0.163.0");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        let (entry, alias) = (
            dir.join(world::bin("codex")),
            dir.join(world::bin("codex-old")),
        );
        let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
        assert!(
            meta(&alias).file_type().is_file(),
            "a regular file, never a link"
        );
        assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
        assert_eq!(std::fs::read(&entry).unwrap(), native_exe("codex 0.163.0"));
        assert_eq!(meta(&alias).permissions(), meta(&entry).permissions());
        let tool = |name: &str| crate::store::ToolName::new(name).unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&tool("codex-old"))),
            Some(alias.clone())
        );
        assert_eq!(
            crate::activate::twin_target(&l, "codex-old"),
            Some(alias.clone())
        );
        assert!(std::fs::symlink_metadata(l.shim(&tool("codex"))).is_err());
        assert_eq!(shim_build(&l, "codex-old"), Some(v("0.163.0").build_id()));
        assert_eq!(shim_build(&l, "codex"), None);
        let record = complete_record(&dir).unwrap();
        assert_eq!(record.program, "codex-old");
        let modes = crate::store::declared_modes(&dir).unwrap();
        assert_eq!(
            record.tree_root,
            crate::tree::tree_root_declared(&dir, &modes).unwrap()
        );
        assert_eq!(
            crate::verify::verify_program(&l, "codex-old"),
            crate::verify::VerifyOutcome::VendorRoot {
                build: v("0.163.0").build_id(),
                vendor: "OpenAI".into(),
                staged_from: crate::verify::StagedFrom::Documents,
                drift: None,
            }
        );
        assert!(
            f.gets
                .borrow()
                .iter()
                .all(|(url, _)| crate::vendor::vendor_direct_url_allowed("codex", url)),
            "only OpenAI's documents, under codex's pins"
        );
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [Fake::codex_payload_url("0.163.0")]
        );
    }

    /// The gate judges a clone as a Mach-O of its own: a codesign verdict refusing
    /// `bin/codex-old` alone refuses the landing (nothing swapped in, nothing shimmed), though
    /// OpenAI's own entry passes.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_gate_judges_codex_olds_clone_as_a_mach_o_of_its_own() {
        fn refuse_the_alias(
            path: &Path,
            _: &str,
            _: std::time::Instant,
        ) -> Result<(), aterm_update_core::codesign::CodesignError> {
            if path.ends_with("bin/codex-old") {
                Err(aterm_update_core::codesign::CodesignError::Refused {
                    code: Some(3),
                    stderr: "test-requirement: code failed to satisfy specified code \
                             requirement(s)\n"
                        .into(),
                })
            } else {
                Ok(())
            }
        }
        let f = Fake::default();
        f.publish_codex_default("0.163.0");
        let refusing = Trust {
            team_check: refuse_the_alias,
            ..world::trust()
        };
        let l = world::layout("codex-old-gate");
        let o = run_with(&l, &f, &Policy::default(), &refusing, codex_old(), false);
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("bin/codex-old"), "{}", o.line());
        assert_eq!(shim_build(&l, "codex-old"), None);
        assert!(!crate::store::build_is_complete(&world::build_dir(
            &l,
            "codex-old",
            "0.163.0"
        )));
        let control = world::layout("codex-old-gate-control");
        let o = run(&control, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }

    #[test]
    fn a_codex_package_without_its_entry_lands_no_codex_old() {
        let f = Fake::default();
        f.publish_codex("0.163.0", &[("codex-path/rg", &native_exe("rg"), 0o755)]);
        let l = world::layout("codex-old-no-entry");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(o.is_failure(), "{o:?}");
        let missing_entry = format!("{} is not a regular file", world::bin("codex"));
        assert!(o.line().contains(&missing_entry), "{}", o.line());
        assert_eq!(shim_build(&l, "codex-old"), None);
        assert!(!crate::store::build_is_complete(&world::build_dir(
            &l,
            "codex-old",
            "0.163.0"
        )));
    }

    /// The files [`Fake::publish_codex_default`]'s package lays.
    fn codex_default_files() -> [String; 4] {
        [
            world::bin("codex"),
            "codex-path/rg".into(),
            "codex-resources/voice/runtime.json".into(),
            "codex-resources/voice/NOTICE.md".into(),
        ]
    }

    /// Assert `pkg verify` passes `program`'s `version`, its tree copied from `lender`'s build or
    /// (`None`) downloaded.
    fn verified(l: &Layout, program: &str, version: &str, lender: Option<&str>) {
        assert_eq!(
            crate::verify::verify_program(l, program),
            crate::verify::VerifyOutcome::VendorRoot {
                build: v(version).build_id(),
                vendor: "OpenAI".into(),
                staged_from: lender.map_or(crate::verify::StagedFrom::Documents, |p| {
                    crate::verify::StagedFrom::Sibling { program: p.into() }
                }),
                drift: None,
            },
            "{program}"
        );
    }

    /// A store with codex `0.163.0` landed from OpenAI's package.
    fn codex_landed(label: &str) -> (crate::testdir::ScratchLayout, Fake) {
        let l = world::layout(label);
        let f = Fake::default();
        f.publish_codex_default("0.163.0");
        let o = run(&l, &f, &Policy::default(), codex());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        (l, f)
    }

    /// Where codex is OpenAI's build, codex-old copies codex's verified build of the same
    /// release instead of fetching it again: one download for both rows, a tree of its own
    /// (each file a clone on its own inode, nothing linked), its alias, shim and twin in its own
    /// build, a record naming the lender, and otherwise exactly the tree, record and modes a
    /// download of its own lays.
    #[test]
    fn codex_old_takes_codexs_verified_tree_and_downloads_nothing() {
        let (l, f) = codex_landed("codex-old-taken");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [Fake::codex_payload_url("0.163.0")],
            "one download lands both rows"
        );
        let (from, dir) = (
            world::build_dir(&l, "codex", "0.163.0"),
            world::build_dir(&l, "codex-old", "0.163.0"),
        );
        let record = complete_record(&dir).unwrap();
        assert_eq!(record.tree_from.as_deref(), Some("codex"));
        verified(&l, "codex-old", "0.163.0", Some("codex"));
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "a tree of its own, never a link to codex's"
        );
        for rel in codex_default_files() {
            let (lent, own) = (from.join(&rel), dir.join(&rel));
            assert!(std::fs::symlink_metadata(&own).unwrap().is_file(), "{rel}");
            assert!(
                crate::clone::is_clone_of(&lent, &own),
                "{rel}: a clone on an inode of its own"
            );
        }
        let alias = dir.join(world::bin("codex-old"));
        assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
        assert!(
            std::fs::symlink_metadata(from.join(world::bin("codex-old"))).is_err(),
            "codex's tree gained nothing"
        );
        let tool = |name: &str| crate::store::ToolName::new(name).unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&tool("codex-old"))),
            Some(alias.clone())
        );
        assert_eq!(crate::activate::twin_target(&l, "codex-old"), Some(alias));
        verified(&l, "codex", "0.163.0", None);

        let control = world::layout("codex-old-taken-control");
        let o = run(&control, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let alone = world::build_dir(&control, "codex-old", "0.163.0");
        assert_eq!(
            complete_record(&alone),
            Some(VendorRecord {
                tree_from: None,
                ..record
            }),
            "a download of its own records the same tree, only no lender"
        );
        assert_eq!(
            crate::store::declared_modes(&dir).unwrap(),
            crate::store::declared_modes(&alone).unwrap()
        );
    }

    /// A codex build whose tree no longer folds to its record's root lends nothing: codex-old
    /// downloads its own, lays OpenAI's bytes, and nothing is memoized against it.
    #[test]
    fn a_drifted_codex_tree_is_downloaded_not_shared() {
        let (l, f) = codex_landed("codex-old-drift");
        let from = world::build_dir(&l, "codex", "0.163.0");
        std::fs::write(from.join("codex-path/rg"), native_exe("not rg")).unwrap();
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [
                Fake::codex_payload_url("0.163.0"),
                Fake::codex_payload_url("0.163.0")
            ]
        );
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        assert_eq!(
            std::fs::read(dir.join("codex-path/rg")).unwrap(),
            native_exe("rg")
        );
        assert_eq!(complete_record(&dir).unwrap().tree_from, None);
        verified(&l, "codex-old", "0.163.0", None);
        assert!(
            crate::store::stage_refusal(&dir).is_none(),
            "a drifted lender is no verdict on the bytes"
        );
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
    }

    /// Only a codex build of exactly codex-old's candidate is copied: a record naming another
    /// digest or URL, or no codex build of the version codex-old lands, means a download.
    #[test]
    fn only_the_same_bytes_are_shared() {
        let landed = |label: &str, edit: &dyn Fn(&mut VendorRecord)| {
            let (l, f) = codex_landed(label);
            let from = world::build_dir(&l, "codex", "0.163.0");
            let mut record = complete_record(&from).unwrap();
            edit(&mut record);
            super::super::durable::write_durable(
                &super::super::record_path(&from).unwrap(),
                &record.record_bytes().unwrap(),
            )
            .unwrap();
            assert_eq!(
                complete_record(&from),
                Some(record),
                "PRECONDITION: a record codex's own lane reads"
            );
            let o = run(&l, &f, &Policy::default(), codex_old());
            assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
            let dir = world::build_dir(&l, "codex-old", "0.163.0");
            let downloads = f.downloads.borrow().len();
            (downloads, complete_record(&dir).unwrap().tree_from)
        };
        assert_eq!(
            landed("same-bytes", &|_| {}),
            (1, Some("codex".into())),
            "the control"
        );
        assert_eq!(
            landed("other-digest", &|r| r.sha256 = "f".repeat(64)),
            (2, None)
        );
        let other_url = format!(
            "{}rust-v0.163.0/codex-package-other.tar.gz",
            world::CODEX_DL
        );
        assert_eq!(
            landed("other-url", &|r| r.source_url.clone_from(&other_url)),
            (2, None)
        );

        let (l, f) = codex_landed("other-version");
        f.publish_codex_default("0.164.0");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [
                Fake::codex_payload_url("0.163.0"),
                Fake::codex_payload_url("0.164.0")
            ]
        );
        let dir = world::build_dir(&l, "codex-old", "0.164.0");
        assert_eq!(complete_record(&dir).unwrap().tree_from, None);
    }

    /// A copied codex-old owns its tree outright: gc reclaiming the codex build it was copied
    /// from, then codex's uninstall, leave its bytes, shim and verify as they were.
    #[test]
    fn codex_old_outlives_codex() {
        let (l, f) = codex_landed("codex-old-outlives");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        assert_eq!(
            complete_record(&dir).unwrap().tree_from.as_deref(),
            Some("codex"),
            "PRECONDITION: copied"
        );
        for version in ["0.164.0", "0.165.0"] {
            f.publish_codex_default(version);
            let o = run(&l, &f, &Policy::default(), codex());
            assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        }
        let report = crate::gc::run(&l, None);
        assert!(
            std::fs::symlink_metadata(world::build_dir(&l, "codex", "0.163.0")).is_err(),
            "PRECONDITION: gc reclaimed the build codex-old was copied from: {:?}",
            report.reclaimed
        );
        let alias = dir.join(world::bin("codex-old"));
        let tool = crate::store::ToolName::new("codex-old").unwrap();
        let intact = |when: &str| {
            verified(&l, "codex-old", "0.163.0", Some("codex"));
            assert_eq!(
                std::fs::read(&alias).ok(),
                Some(native_exe("codex 0.163.0")),
                "{when}"
            );
            for rel in codex_default_files() {
                assert!(
                    std::fs::symlink_metadata(dir.join(&rel)).is_ok_and(|m| m.is_file()),
                    "{when}: {rel}"
                );
            }
            assert_eq!(
                crate::platform::resolve_shim(&l.shim(&tool)),
                Some(alias.clone()),
                "{when}"
            );
        };
        intact("after gc");
        crate::ops::uninstall(&l, "codex").unwrap();
        assert!(
            std::fs::symlink_metadata(l.prefix.join("store/codex")).is_err(),
            "PRECONDITION: codex is gone"
        );
        intact("after codex's uninstall");
    }

    /// A codex-old the lane would not download is not copied either: a refusal of these exact
    /// bytes, or a first-seen digest the candidate's differs from, refuses it before any codex
    /// build is looked at.
    #[test]
    fn a_refused_or_conflicting_codex_old_is_never_shared() {
        let (l, f) = codex_landed("codex-old-memo");
        let sha256 = complete_record(&world::build_dir(&l, "codex", "0.163.0"))
            .unwrap()
            .sha256;
        let digest =
            AuthenticatedDigest::authenticated(crate::manifest::DigestAlg::Sha256, &sha256)
                .unwrap();
        ProgramStamp::record_refusal(
            &l,
            "codex-old",
            "atpkg: codex-old 0.163.0 was refused",
            v("0.163.0"),
            &digest,
        )
        .unwrap();
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(
            o.line()
                .contains("an earlier pass refused these exact bytes"),
            "{}",
            o.line()
        );
        assert!(std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err());
        assert_eq!(shim_build(&l, "codex-old"), None);
        assert_eq!(f.downloads.borrow().len(), 1);

        let (l, f) = codex_landed("codex-old-recut");
        let first =
            AuthenticatedDigest::authenticated(crate::manifest::DigestAlg::Sha256, &"e".repeat(64))
                .unwrap();
        record_or_conflict(&l, "codex-old", v("0.163.0"), world::triple(), &first, true).unwrap();
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("first seen for 0.163.0"), "{}", o.line());
        assert!(std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err());
        assert_eq!(shim_build(&l, "codex-old"), None);
        assert_eq!(f.downloads.borrow().len(), 1);
    }

    /// codex-old follows codex release by release, copying each: two releases, two downloads.
    #[test]
    fn codex_old_follows_codex_forward() {
        let l = world::layout("codex-old-forward");
        let f = Fake::default();
        for version in ["0.163.0", "0.164.0"] {
            f.publish_codex_default(version);
            for spec in [codex(), codex_old()] {
                let o = run(&l, &f, &Policy::default(), spec);
                assert!(
                    matches!(o.verdict, Verdict::Installed { .. }),
                    "{version} {}: {o:?}",
                    spec.program
                );
            }
            let dir = world::build_dir(&l, "codex-old", version);
            assert_eq!(
                complete_record(&dir).unwrap().tree_from.as_deref(),
                Some("codex"),
                "{version}"
            );
        }
        assert_eq!(
            f.downloads.borrow().as_slice(),
            [
                Fake::codex_payload_url("0.163.0"),
                Fake::codex_payload_url("0.164.0")
            ]
        );
        assert_eq!(shim_build(&l, "codex-old"), Some(v("0.164.0").build_id()));
        verified(&l, "codex-old", "0.164.0", Some("codex"));
    }

    /// The gate judges a copied tree where codex-old's own stage lays it, as it judges a
    /// download: a team check refusing what sits under codex-old's store refuses the copy and
    /// swaps nothing in, and codex's build, shim and stamp are as they were.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_gate_judges_a_taken_tree_at_codex_olds_paths() {
        fn refuse_under_codex_old(
            path: &Path,
            _: &str,
            _: std::time::Instant,
        ) -> Result<(), aterm_update_core::codesign::CodesignError> {
            if path.components().any(|c| c.as_os_str() == "codex-old") {
                Err(aterm_update_core::codesign::CodesignError::Refused {
                    code: Some(3),
                    stderr: "test-requirement: code failed to satisfy specified code \
                             requirement(s)\n"
                        .into(),
                })
            } else {
                Ok(())
            }
        }
        let refusing = Trust {
            team_check: refuse_under_codex_old,
            ..world::trust()
        };
        let l = world::layout("taken-gate");
        let f = Fake::default();
        f.publish_codex_default("0.163.0");
        let o = run_with(&l, &f, &Policy::default(), &refusing, codex(), false);
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let o = run_with(&l, &f, &Policy::default(), &refusing, codex_old(), false);
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("signer refused"), "{}", o.line());
        assert_eq!(
            f.downloads.borrow().len(),
            1,
            "the copy was judged, no download"
        );
        assert!(
            std::fs::symlink_metadata(world::build_dir(&l, "codex-old", "0.163.0")).is_err(),
            "nothing swapped in"
        );
        assert_eq!(shim_build(&l, "codex-old"), None);
        assert_eq!(shim_build(&l, "codex"), Some(v("0.163.0").build_id()));
        verified(&l, "codex", "0.163.0", None);
        let from = world::build_dir(&l, "codex", "0.163.0");
        assert!(crate::store::stage_refusal(&from).is_none());
        assert_eq!(ProgramStamp::read(&l, "codex").unwrap().refused, None);
    }

    /// The scratch a stage left beside `program`'s store builds: a copy or a swap not cleaned up.
    fn scratch_left(l: &Layout, program: &str) -> Vec<String> {
        let build = world::build_dir(l, program, "0.163.0");
        let Ok(entries) = std::fs::read_dir(build.parent().unwrap()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.contains(".incoming-") || n.contains(".superseded-"))
            .collect()
    }

    /// Set the extended attribute `name` on `path` to `value`.
    #[cfg(target_os = "macos")]
    fn set_xattr(path: &Path, name: &str, value: &[u8]) {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: `path` and `name` are NUL-terminated C strings and `value` a readable slice of
        // the stated length, all outliving the call, which only reads them.
        let rc = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast::<libc::c_void>(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(rc, 0, "setxattr: {}", std::io::Error::last_os_error());
    }

    /// Finder information on a file of codex's build survives a clone and fails the Developer ID
    /// gate's strict check, though the tree's root does not fold it: that build lends codex-old
    /// nothing, and codex-old downloads its own, which carries none, leaving no scratch.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_finder_tagged_codex_tree_is_downloaded_not_copied() {
        let (l, f) = codex_landed("finder-tagged");
        let from = world::build_dir(&l, "codex", "0.163.0").join(world::bin("codex"));
        // Finder keeps only information that is not all zero: here, the file's hidden bit.
        let mut info = [0u8; 32];
        info[8] = 0x40;
        set_xattr(&from, "com.apple.FinderInfo", &info);
        let finder_info = |path: &Path| {
            crate::platform::xattr_names(path)
                .unwrap()
                .iter()
                .any(|name| name == b"com.apple.FinderInfo")
        };
        assert!(finder_info(&from), "the control");
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().len(),
            2,
            "codex-old downloaded its own"
        );
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        assert_eq!(complete_record(&dir).unwrap().tree_from, None);
        verified(&l, "codex-old", "0.163.0", None);
        assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
        assert!(!finder_info(&dir.join(world::bin("codex"))));
    }

    /// Clears every owner-binding flag under a tree when it drops, so a scratch layout a test
    /// locked a file in can be removed, whether the test passed or not.
    #[cfg(target_os = "macos")]
    struct Unlock(std::path::PathBuf);

    #[cfg(target_os = "macos")]
    impl Drop for Unlock {
        fn drop(&mut self) {
            fn walk(dir: &Path) {
                use std::os::macos::fs::MetadataExt as _;
                let Ok(entries) = std::fs::read_dir(dir) else {
                    return;
                };
                for path in entries.flatten().map(|e| e.path()) {
                    let Ok(meta) = std::fs::symlink_metadata(&path) else {
                        continue;
                    };
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    let bound = meta.st_flags() & crate::platform::OWNER_BINDING_FLAGS;
                    if bound != 0 {
                        let _ = crate::platform::set_file_flags(&path, meta.st_flags() & !bound);
                    }
                    if meta.is_dir() {
                        walk(&path);
                    }
                }
            }
            walk(&self.0);
        }
    }

    /// A clone keeps a file's BSD flags, so a locked file in codex's build would lay a copy no
    /// cleanup can remove, and every later stage of codex-old would stop on that scratch: that
    /// build lends nothing, nothing locked is laid, and codex-old downloads its own, leaving no
    /// scratch.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_locked_codex_file_is_downloaded_not_copied() {
        use std::os::macos::fs::MetadataExt as _;
        let (l, f) = codex_landed("locked");
        let _unlock = Unlock(l.prefix.clone());
        let notice = "codex-resources/voice/NOTICE.md";
        let from = world::build_dir(&l, "codex", "0.163.0").join(notice);
        crate::platform::set_file_flags(&from, libc::UF_IMMUTABLE).unwrap();
        let o = run(&l, &f, &Policy::default(), codex_old());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            f.downloads.borrow().len(),
            2,
            "codex-old downloaded its own"
        );
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        assert_eq!(complete_record(&dir).unwrap().tree_from, None);
        assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
        let laid = std::fs::symlink_metadata(dir.join(notice)).unwrap();
        assert_eq!(
            laid.st_flags() & crate::platform::OWNER_BINDING_FLAGS,
            0,
            "nothing locked is laid"
        );
    }

    /// A copy holds the room its tree takes, as a download's second disk gate does: on a volume
    /// that cannot clone it is a full copy. Short of room, codex-old's copy refuses as its
    /// download does (the same verdict and sentence; only the room named differs, for a copy
    /// needs none for the archive) and lays, fetches and memoizes nothing.
    #[test]
    fn a_copy_short_of_room_refuses_as_a_download_does() {
        let (l, f) = codex_landed("copy-short");
        let _full = crate::freespace::pinned::pin(&l.prefix, 0);
        let copy = run(&l, &f, &Policy::default(), codex_old());
        assert_eq!(f.downloads.borrow().len(), 1, "nothing fetched");
        let dir = world::build_dir(&l, "codex-old", "0.163.0");
        assert!(std::fs::symlink_metadata(&dir).is_err(), "nothing laid");
        assert_eq!(scratch_left(&l, "codex-old"), Vec::<String>::new());
        assert!(
            crate::store::stage_refusal(&dir).is_none(),
            "nothing memoized"
        );
        assert_eq!(shim_build(&l, "codex-old"), None);

        let alone = world::layout("download-short");
        let g = Fake::default();
        g.publish_codex_default("0.163.0");
        let _alone_full = crate::freespace::pinned::pin(&alone.prefix, 0);
        let download = run(&alone, &g, &Policy::default(), codex_old());
        assert!(g.downloads.borrow().is_empty(), "nothing fetched");
        let refusal = |o: &Outcome| {
            let Verdict::Failed { error, .. } = &o.verdict else {
                panic!("not a disk refusal: {o:?}");
            };
            assert!(error.starts_with(crate::flow::DISK_REFUSAL_HEAD), "{error}");
            let line = o.line();
            line.split(": needs ").next().unwrap().to_string()
        };
        assert_eq!(refusal(&copy), refusal(&download));
    }

    /// When the copy's stage fails on this machine rather than on its bytes (the Developer ID
    /// gate could not judge it), codex-old downloads its own instead of failing the pass, and
    /// that download does not wait on the turn on the link the copy gave up once it was laid.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_copy_the_gate_cannot_judge_is_downloaded_instead() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static JUDGED: AtomicBool = AtomicBool::new(false);
        fn cannot_judge_the_first(
            path: &Path,
            _: &str,
            _: std::time::Instant,
        ) -> Result<(), aterm_update_core::codesign::CodesignError> {
            if path.components().any(|c| c.as_os_str() == "codex-old")
                && !JUDGED.swap(true, Ordering::SeqCst)
            {
                return Err(aterm_update_core::codesign::CodesignError::Refused {
                    code: Some(1),
                    stderr: format!(
                        "{}: resource fork, Finder information, or similar detritus not \
                         allowed\n",
                        path.display()
                    ),
                });
            }
            Ok(())
        }
        let (l, f) = codex_landed("copy-unjudged");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let trust = Trust {
                team_check: cannot_judge_the_first,
                ..world::trust()
            };
            let (policy, turns) = (Policy::default(), crate::flow::DownloadTurns::default());
            let ticket = turns.ticket();
            let lane = Lane {
                layout: &l,
                fetcher: &f,
                triple: world::triple(),
                policy: &policy,
                trust: &trust,
                legacy_index: None,
                asked: false,
                now: NOW,
                download_turn: Some(&ticket),
            };
            let o = install_one(&lane, codex_old(), false);
            let dir = world::build_dir(&l, "codex-old", "0.163.0");
            let _ = tx.send((
                format!("{o:?}"),
                matches!(o.verdict, Verdict::Installed { .. }),
                f.downloads.borrow().len(),
                complete_record(&dir).map(|r| r.tree_from),
                scratch_left(&l, "codex-old"),
            ));
        });
        let (outcome, installed, downloads, tree_from, scratch) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the landing hung: its download waited on the turn its copy gave up");
        assert!(JUDGED.load(Ordering::SeqCst), "the copy reached the gate");
        assert!(installed, "{outcome}");
        assert_eq!(downloads, 2, "codex-old downloaded its own");
        assert_eq!(tree_from, Some(None));
        assert_eq!(scratch, Vec::<String>::new());
    }

    #[test]
    fn a_stale_part_of_another_version_is_not_resumed() {
        let l = world::layout("part");
        let f = Fake::default();
        let payload = native_exe("claude 2.1.282");
        f.publish_claude("2.1.282", &payload);
        let staging = l.staging_dir("claude");
        std::fs::create_dir_all(&staging).unwrap();
        let stale = staging.join(format!("claude-2.1.281-{}.part", world::triple()));
        std::fs::write(&stale, b"half of another version").unwrap();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            o.asset.as_deref(),
            Some(format!("claude-2.1.282-{}", world::triple()).as_str())
        );
        assert!(!stale.exists(), "the other version's partial was reclaimed");
        let dir = world::build_dir(&l, "claude", "2.1.282");
        assert_eq!(
            std::fs::read(dir.join(world::bin("claude"))).unwrap(),
            payload
        );
    }

    #[test]
    fn a_legacy_index_build_is_replaced_with_no_index_and_no_line_names_a_vendor_id() {
        let l = world::layout("legacy");
        legacy_claude(&l, 2_026_091_901);
        let f = Fake::default();
        f.publish_claude("2.1.280", &native_exe("below the floor"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::BelowFloor,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(2_026_091_901));
        f.publish_claude("2.1.281", &native_exe("vendor"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::ReplacesLegacy,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.281").build_id()));
        for line in [
            o.line(),
            crate::vendor_direct::display_build("claude", v("2.1.281").build_id()),
        ] {
            assert_no_build_id(&line);
        }
    }

    #[test]
    fn a_read_legacy_version_is_kept_and_no_later_pass_needs_the_index() {
        let l = world::layout("legacy-kept");
        legacy_claude(&l, 2_026_092_201);
        let f = Fake::default();
        *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
        f.publish_legacy_pkg("claude", 2_026_092_201, "2.1.281");
        let index = crate::resolve_verified_index(
            &f,
            &l,
            &crate::sig::testkit::anchor(),
            crate::sig::BuildFloor {
                index_build: 0,
                roster_seq: crate::sig::testkit::SEQ,
            },
            NOW,
        )
        .expect("the index verifies");
        f.publish_claude("2.1.281", &native_exe("vendor"));
        let o = run_indexed(
            &l,
            &f,
            &Policy::default(),
            &world::trust(),
            claude(),
            false,
            Some(&index),
        );
        assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
        assert_eq!(
            ProgramStamp::read(&l, "claude")
                .unwrap()
                .legacy_version_of(2_026_092_201),
            Some(v("2.1.281"))
        );
        *f.index.borrow_mut() = None;
        f.forget_legacy_pkgs();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Current(_)), "{o:?}");
        assert!(f.downloads.borrow().is_empty(), "nothing fetched");
        f.publish_claude("2.1.282", &native_exe("next"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Newer,
                    ..
                }
            ),
            "{o:?}"
        );
    }

    #[test]
    fn a_legacy_root_nothing_holds_is_read_from_its_signed_manifest_once() {
        let l = world::layout("legacy-root-owed");
        let legacy = 2_026_092_201;
        legacy_claude(&l, legacy);
        let root = crate::tree::tree_root(&l.build_dir("claude", legacy)).unwrap();
        let f = Fake::default();
        f.publish_claude("2.1.282", &native_exe("vendor"));
        ProgramStamp::record_legacy_version(&l, "claude", legacy, v("2.1.281")).unwrap();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let vendor = Some(v("2.1.282").build_id());
        assert_eq!(legacy_root_owed(&l, "claude", vendor), Some(legacy));
        assert_eq!(f.pkg_reads.borrow().len(), 0, "no index, no read");
        *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
        f.publish_legacy_pkg_rooted("claude", legacy, "2.1.281", world::triple(), &root);
        let index = crate::resolve_verified_index(
            &f,
            &l,
            &crate::sig::testkit::anchor(),
            crate::sig::BuildFloor {
                index_build: 0,
                roster_seq: crate::sig::testkit::SEQ,
            },
            NOW,
        )
        .expect("the index verifies");
        let indexed = || {
            run_indexed(
                &l,
                &f,
                &Policy::default(),
                &world::trust(),
                claude(),
                false,
                Some(&index),
            )
        };
        assert!(matches!(indexed().verdict, Verdict::Current(_)));
        let kept = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(kept.legacy_root_of(legacy), Some(root.as_str()));
        assert_eq!(f.pkg_reads.borrow().len(), 1);
        assert_eq!(legacy_root_owed(&l, "claude", vendor), None);
        indexed();
        assert_eq!(f.pkg_reads.borrow().len(), 1, "kept: no pass asks again");
        let path = crate::vendor_direct::stamp_path(&l, "claude").unwrap();
        let older: String = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| {
                !line.starts_with("legacy_root_build") && !line.starts_with("legacy_tree_root")
            })
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, older).unwrap();
        assert_eq!(legacy_root_owed(&l, "claude", vendor), Some(legacy));
        indexed();
        assert_eq!(f.pkg_reads.borrow().len(), 2);
        assert_eq!(
            ProgramStamp::read(&l, "claude")
                .unwrap()
                .legacy_root_of(legacy),
            Some(root.as_str())
        );
    }

    #[test]
    fn an_unread_legacy_build_above_the_ceiling_waits_for_its_version_or_a_person() {
        let above = claude().legacy_ceiling + 1;
        let l = world::layout("legacy-above");
        legacy_claude(&l, above);
        let f = Fake::default();
        f.publish_claude("2.1.282", &native_exe("vendor"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::LegacyUnread,
                    ..
                }
            ),
            "{o:?}"
        );
        assert!(!o.is_failure());
        assert_eq!(shim_build(&l, "claude"), Some(above));
        assert!(f.downloads.borrow().is_empty(), "nothing fetched");
        *f.index.borrow_mut() = Some(world::signed_index(45, "2099-01-01T00:00:00Z", &[]));
        f.publish_legacy_pkg("claude", above, "2.1.283");
        let index = crate::resolve_verified_index(
            &f,
            &l,
            &crate::sig::testkit::anchor(),
            crate::sig::BuildFloor {
                index_build: 0,
                roster_seq: crate::sig::testkit::SEQ,
            },
            NOW,
        )
        .expect("the index verifies");
        let o = run_indexed(
            &l,
            &f,
            &Policy::default(),
            &world::trust(),
            claude(),
            false,
            Some(&index),
        );
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::HeadOlder,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(above));
        let l = world::layout("legacy-above-asked");
        legacy_claude(&l, above);
        let o = run_asked(&l, &f, claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::ReplacesLegacy,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    }

    #[test]
    fn a_legacy_build_is_read_under_an_index_that_no_longer_names_its_program() {
        let above = claude().legacy_ceiling + 1;
        let l = world::layout("legacy-unnamed");
        legacy_claude(&l, above);
        let f = Fake::default();
        *f.index.borrow_mut() = Some(world::signed_index_naming(
            45,
            "2099-01-01T00:00:00Z",
            &[],
            &[],
        ));
        f.publish_legacy_pkg("claude", above, "2.1.281");
        f.publish_claude("2.1.282", &native_exe("vendor"));
        let index = crate::resolve_verified_index(
            &f,
            &l,
            &crate::sig::testkit::anchor(),
            crate::sig::BuildFloor {
                index_build: 0,
                roster_seq: crate::sig::testkit::SEQ,
            },
            NOW,
        )
        .expect("the index verifies");
        assert!(index.program("claude").is_none());
        let o = run_indexed(
            &l,
            &f,
            &Policy::default(),
            &world::trust(),
            claude(),
            false,
            Some(&index),
        );
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Newer,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(
            o.line(),
            "atpkg: claude 2.1.281 → 2.1.282 (Anthropic latest)"
        );
        assert_eq!(*f.pkg_reads.borrow(), [crate::manifest::INDEX_REPO]);
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.282").build_id()));
    }

    #[test]
    fn the_head_is_read_short_and_the_documents_patiently() {
        let l = world::layout("head-bounds");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let head = format!("{CLAUDE}latest");
        assert_eq!(
            f.head_reads.borrow().as_slice(),
            std::slice::from_ref(&head)
        );
        let patient: Vec<String> = f.gets.borrow().iter().map(|(u, _)| u.clone()).collect();
        assert!(
            patient.iter().any(|u| u.ends_with("2.1.281/manifest.json")),
            "{patient:?}"
        );
        f.publish_claude("2.1.282", &native_exe("b"));
        f.downloads_fail.set(true);
        run(&l, &f, &Policy::default(), claude());
        f.downloads_fail.set(false);
        f.head_reads.borrow_mut().clear();
        f.gets.borrow_mut().clear();
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert_eq!(
            f.head_reads.borrow().as_slice(),
            std::slice::from_ref(&head)
        );
        let unconditional: Vec<String> = f
            .gets
            .borrow()
            .iter()
            .filter(|(u, etag)| *u == head && etag.is_none())
            .map(|(u, _)| u.clone())
            .collect();
        assert_eq!(unconditional, [head], "the head's document, patiently");
    }

    #[test]
    fn a_fresh_install_is_bounded_by_the_floor_not_the_high_water() {
        let l = world::layout("fresh-hw");
        ProgramStamp::record_outcome(&l, "claude", "uninstalled", Some(v("2.1.291"))).unwrap();
        let f = Fake::default();
        f.publish_claude("2.1.290", &native_exe("pulled back"));
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Installed {
                    reason: InstallReason::Fresh,
                    ..
                }
            ),
            "{o:?}"
        );
        assert_eq!(shim_build(&l, "claude"), Some(v("2.1.290").build_id()));
    }

    #[test]
    fn an_unchanged_head_that_never_landed_is_fetched_again() {
        let l = world::layout("retry");
        let f = Fake::default();
        f.publish_claude("2.1.281", &native_exe("a"));
        f.downloads_fail.set(true);
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        f.downloads_fail.set(false);
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let conditional = f
            .gets
            .borrow()
            .iter()
            .filter(|(_, etag)| etag.is_some())
            .count();
        assert_eq!(
            conditional, 1,
            "the second pass's head was a 304, then asked again"
        );
    }

    #[test]
    fn a_same_version_digest_change_is_refused() {
        let mut rows = vec![
            (claude(), "2.1.282"),
            (codex(), "0.157.0"),
            (agy(), "1.3.7"),
            (kimi(), "2.3.2"),
        ];
        if world::grok_platform().is_some() {
            rows.push((grok(), "1.0.70"));
        }
        for (spec, version) in rows {
            let publish = |f: &Fake, cut: &str| {
                let exe = native_exe(cut);
                match spec.program {
                    "claude" => f.publish_claude(version, &exe),
                    "codex" => f.publish_codex(version, &[("bin/codex", &exe, 0o755)]),
                    "kimi" => drop(publish_kimi(f, version, &exe)),
                    "grok" => drop(publish_grok(f, version, &exe)),
                    _ => drop(publish_agy(f, version, &exe)),
                }
            };
            let l = world::layout(&format!("recut-{}", spec.program));
            let f = Fake::default();
            publish(&f, "first cut");
            f.downloads_fail.set(true);
            let o = run(&l, &f, &Policy::default(), spec);
            assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
            f.downloads_fail.set(false);
            publish(&f, "second cut");
            let downloads = f.downloads.borrow().len();
            let o = run(&l, &f, &Policy::default(), spec);
            assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
            assert!(
                o.line().contains(&format!("first seen for {version}")),
                "{}",
                o.line()
            );
            assert_eq!(f.downloads.borrow().len(), downloads, "nothing downloaded");
            assert_eq!(shim_build(&l, spec.program), None);
        }
    }

    #[test]
    fn a_muse_digest_is_logged_only_once_the_team_pin_passed_its_bytes() {
        let l = world::layout("muse-pin");
        let f = Fake::default();
        publish_muse(&f, "1.4.9-R6298", &native_exe("not Meta's"));
        let refusing = world::trust_refusing_signers();
        let o = run_with(&l, &f, &Policy::default(), &refusing, muse(), false);
        assert!(o.line().contains("signer refused"), "{}", o.line());
        let log = super::super::digests::digests_path(&l, "muse");
        assert!(!log.exists(), "nothing the pin refused is logged");
        let genuine = native_exe("Meta's");
        publish_muse(&f, "1.4.9-R6298", &genuine);
        let o = run(&l, &f, &Policy::default(), muse());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains(&world::sha(&genuine)), "{logged}");
        crate::ops::uninstall(&l, "muse").unwrap();
        publish_muse(&f, "1.4.9-R6298", &native_exe("re-cut"));
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), muse());
        assert!(
            o.line().contains("first seen for 1.4.9-R6298"),
            "{}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), downloads, "nothing downloaded");
    }

    #[test]
    fn a_build_date_regression_is_kept_at() {
        let l = world::layout("date");
        let f = Fake::default();
        f.publish_claude_dated("2.1.281", &native_exe("a"), "2026-09-21T20:55:27Z");
        run(&l, &f, &Policy::default(), claude());
        f.publish_claude_dated("2.1.282", &native_exe("b"), "2026-09-01T00:00:00Z");
        let o = run(&l, &f, &Policy::default(), claude());
        assert!(
            matches!(
                o.verdict,
                Verdict::Kept {
                    reason: KeepReason::BuildDateRegressed,
                    ..
                }
            ),
            "{o:?}"
        );
        assert!(f.downloads.borrow().len() == 1);
    }
}
