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
use super::resolve::{self, Candidate, Head, Resolution, Vouch};
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
}

impl Trust {
    /// The compiled anchors: Anthropic's key and `/usr/bin/codesign`.
    pub(crate) const PRODUCTION: Self = Self {
        claude_key: crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY,
        team_check: crate::install::codesign_team_check,
    };
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
    /// ([`crate::flow::DownloadTurns`]); only its payload's download waits for it.
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

    /// The exact download size, from the verified documents.
    #[must_use]
    pub(crate) const fn download_bytes(&self) -> u64 {
        self.candidate.artifact.artifact().size
    }

    /// What stays on disk once it lands, when the documents say exactly (a raw binary is its
    /// download; an archive's unpacked size is not published).
    #[must_use]
    pub(crate) fn disk_bytes(&self) -> Option<u64> {
        let a = self.candidate.artifact.artifact();
        (a.payload == "raw-binary").then_some(a.size)
    }

    /// What its landing's disk gate checks: the download and the tree it unpacks to, an archive's
    /// at its unpack bound.
    #[must_use]
    pub(crate) const fn landing_bytes(&self) -> crate::flow::LandingBytes {
        crate::flow::LandingBytes::of(self.candidate.artifact.artifact())
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
            let verified = resolve::verify(
                spec,
                triple,
                fetcher,
                *version,
                body,
                &trust.claude_key,
                now,
            );
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
                        &lane.trust.claude_key,
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
    let Some(digest) = c.vouch.digest() else {
        // No document names grok's digest: the first-seen log and the memo judge the bytes once
        // [`measure`] has them. An earlier refusal of this version holds without a download, since
        // nothing short of the bytes could tell a new cut from it.
        if pending.stamp.refused_at(c.version) {
            return fall_back(refuse(refused_unmeasured(spec.program, c.version)), true);
        }
        if let Err(e) = resolve::admit(&c.artifact, &c.vouch) {
            return fall_back(refuse(e.to_string()), true);
        }
        return Step::Land(Box::new(pending));
    };
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

fn refused_unmeasured(program: &str, version: Version) -> String {
    format!(
        "an earlier pass refused {version}'s bytes, and no document names their digest; they \
         are not fetched again until the vendor's head moves (`aterm pkg update {program}` \
         asks again)"
    )
}

fn recut(c: &Candidate, first: &AuthenticatedDigest) -> String {
    format!(
        "its {} differs from the one first seen for {} ({}) — a re-cut release is never an \
         update",
        c.anchor.digest_alg(),
        c.version,
        first.as_str()
    )
}

/// The measure step, for a candidate no document names a digest for (grok): its bytes are
/// downloaded to the staging path the landing reads, held to their exact size and hashed, and the
/// digest is held to the first-seen log and the refusal memo before anything is staged. [`land`]
/// then reuses the file ([`crate::flow::land_artifact`]) and logs the digest only once the darwin
/// team pin has passed those bytes.
///
/// # Errors
/// [`FlowError::Download`] for a staging directory that cannot be made, a download that failed
/// or came up short (nothing is memoized), a file that cannot be hashed or a first-seen log that
/// cannot be checked; [`FlowError::VendorRefused`] for a row [`resolve::admit`] refuses, a digest
/// other than the one first seen for the version (a re-cut, memoized), one an earlier pass
/// refused, or one the candidate's anchor does not take; the staging path's and the disk gate's
/// own errors. Every error from the download on removes the file.
fn measure(lane: &Lane<'_>, pending: &Pending) -> Result<Pending, FlowError> {
    let (spec, c) = (pending.spec, &pending.candidate);
    let art = c.artifact.artifact();
    resolve::admit(&c.artifact, &c.vouch)?;
    let dl = crate::flow::staged_download_path(lane.layout, spec.program, &art.asset)?;
    crate::flow::disk_gate(
        crate::flow::LandingBytes::of(art).need(),
        crate::freespace::available_bytes(&dl),
    )?;
    crate::flow::make_staging_dir(&dl).map_err(|e| FlowError::Download(e.to_string()))?;
    let _ = std::fs::remove_file(&dl);
    crate::flow::sweep_foreign_partials(&dl);
    let fail = |e: FlowError| {
        let _ = std::fs::remove_file(&dl);
        e
    };
    crate::progress::note_build(spec.program, c.version.build_id());
    if let Some(ticket) = lane.download_turn {
        ticket.wait();
    }
    let watch = crate::progress::watch_download(spec.program, &dl, art.size);
    let fetched = lane
        .fetcher
        .vendor_download(spec.program, &art.url, &dl, art.size);
    drop(watch);
    if let Some(ticket) = lane.download_turn {
        ticket.release();
    }
    if let Err(e) = fetched {
        crate::flow::note_fetch_fault();
        return Err(fail(FlowError::Download(e.to_string())));
    }
    let got = std::fs::symlink_metadata(&dl)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len());
    if got != Some(art.size) {
        return Err(fail(FlowError::Download(format!(
            "{} answered {} of its {} bytes",
            art.url,
            got.map_or_else(|| String::from("no file"), |n| n.to_string()),
            art.size
        ))));
    }
    let alg = c.anchor.digest_alg();
    let digest = crate::tree::file_digest(&dl, alg)
        .ok()
        .and_then(|hex| AuthenticatedDigest::measured(alg, &hex))
        .ok_or_else(|| {
            fail(FlowError::Download(format!(
                "{} could not be hashed",
                art.asset
            )))
        })?;
    match record_or_conflict(
        lane.layout,
        spec.program,
        c.version,
        lane.triple,
        &digest,
        false,
    ) {
        Ok(DigestSeen::Conflict { first }) => {
            let why = recut(c, &first);
            let v = Verdict::Refused {
                version: Some(c.version),
                why: why.clone(),
                keeping: pending.installed,
            };
            let _ = ProgramStamp::record_refusal(
                lane.layout,
                spec.program,
                &verdict_line(spec, &v),
                c.version,
                &digest,
            );
            return Err(fail(FlowError::VendorRefused(why)));
        }
        Ok(DigestSeen::New | DigestSeen::Same) => {}
        Err(e) => {
            return Err(fail(FlowError::Download(format!(
                "the first-seen digest log could not be checked: {e}"
            ))));
        }
    }
    if pending.stamp.is_refused(c.version, &digest) {
        return Err(fail(FlowError::VendorRefused(String::from(REFUSED_BYTES))));
    }
    let Some(candidate) = c.measured(digest) else {
        return Err(fail(FlowError::VendorRefused(String::from(
            "the measured digest is not the candidate's anchor's",
        ))));
    };
    resolve::admit(&candidate.artifact, &candidate.vouch).map_err(fail)?;
    Ok(Pending {
        candidate,
        ..pending.clone()
    })
}

/// Download, verify, stage (the darwin Developer ID gate inside), activate and shim a candidate
/// [`plan_one`] verified.
pub(crate) fn land_one(lane: &Lane<'_>, pending: &Pending) -> Outcome {
    let spec = pending.spec;
    let mut measured = None;
    let landed = match pending.candidate.vouch {
        Vouch::Measure => measure(lane, pending).and_then(|p| land(lane, measured.insert(p))),
        Vouch::Documents(_) | Vouch::Measured(_) => land(lane, pending),
    };
    let pending = measured.as_ref().unwrap_or(pending);
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
            if let (FlowError::Stage(StageError::SignerRefused(_)), Some(digest)) =
                (&e, c.vouch.digest())
            {
                let _ = ProgramStamp::record_refusal(
                    lane.layout,
                    spec.program,
                    &verdict_line(spec, &v),
                    c.version,
                    digest,
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
) -> Result<(Vec<String>, Vec<String>, String), FlowError> {
    let spec = pending.spec;
    let program = spec.program;
    let c = &pending.candidate;
    let art = c.artifact.artifact();
    resolve::admit(&c.artifact, &c.vouch)?;
    let Some(digest) = c.vouch.digest() else {
        return Err(FlowError::VendorRefused(String::from(
            "a payload no document vouches for is landed only once it is measured",
        )));
    };
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
    };
    record
        .record_bytes()
        .map_err(|e| FlowError::VendorRefused(e.to_string()))?;
    let final_root: RefCell<Option<String>> = RefCell::new(None);
    let gate =
        crate::install::developer_id_gate(spec.apple_team, spec.exposes, lane.trust.team_check);
    let pre_swap = |tree: &Path| -> Result<(), StageError> {
        let demoted = crate::macho::demote_foreign_executables(tree)?;
        gate(tree)?;
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
        let bytes = rec
            .record_bytes()
            .map_err(|e| StageError::Payload(e.to_string()))?;
        Ok(vec![(crate::store::VENDOR_SIDECAR_SUFFIX, bytes)])
    };
    let hooks = StageHooks {
        pre_swap: Some(&pre_swap),
        sidecars: Some(&sidecars),
        aliases: spec.aliases,
    };
    let exposes: Vec<String> = spec.exposes.iter().map(|t| (*t).to_string()).collect();
    let shim_env = spec.shim_env();
    let landed = crate::flow::land_artifact(
        lane.layout,
        &Landing {
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
        },
        &|dl| {
            lane.fetcher
                .vendor_download(program, &art.url, dl, art.size)
                .map_err(|e| e.to_string())
        },
    )?;
    let root = final_root.borrow().clone().unwrap_or(landed.root);
    Ok((landed.shimmed, landed.refused, root))
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
            "atpkg: {p} {version} deferred: its signature check did not finish ({why}); the \
             next pass checks it again — {}",
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

    /// The test key and a codesign stand-in that passes.
    pub(crate) fn trust() -> Trust {
        Trust {
            claude_key: crate::openpgp::testkit::test_key(),
            team_check: pass_check,
        }
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

    fn sha512_hex(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA512, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

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

    /// Publish grok `version` as xAI's stable pointer and its bucket payload; its URL. grok is
    /// served on darwin only, so elsewhere the landing half could not run.
    fn publish_grok(f: &Fake, version: &str, exe: &[u8]) -> String {
        let Some(platform) = resolve::grok_platform(world::triple()) else {
            aterm_uds::host::could_not_run(format!(
                "grok is not served for {}, so the landing half did not run",
                world::triple()
            ));
        };
        f.doc(grok().head_url, version.as_bytes());
        let url = format!(
            "https://storage.googleapis.com/grok-build-public-artifacts/cli/grok-{version}-{platform}"
        );
        f.payload(&url, exe);
        url
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

    #[test]
    fn a_fresh_grok_is_measured_once_and_logged_once_its_team_pin_passed() {
        let l = world::layout("fresh-grok");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.68");
        let url = publish_grok(&f, "1.0.68", &exe);
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
            std::slice::from_ref(&url),
            "the landing staged the measured file: no second request"
        );
        let build = world::build_dir(&l, "grok", "1.0.68");
        assert_eq!(std::fs::read(build.join("bin/grok")).unwrap(), exe);
        let record = complete_record(&build).expect("a complete record");
        assert_eq!(
            (record.sha256.as_str(), record.anchor),
            (
                world::sha(&exe).as_str(),
                super::super::Anchor::XaiDeveloperId
            )
        );
        assert_eq!(record.apple_team.as_deref(), Some("5Y6N3AJ54S"));
        let log = super::super::digests::digests_path(&l, "grok");
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains(&world::sha(&exe))
        );
        assert_eq!(
            crate::shim_env::read_sidecar(&build).spelled(),
            "GROK_DISABLE_AUTOUPDATER=1"
        );
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
    }

    #[test]
    fn a_grok_recut_is_refused_once_measured_and_never_fetched_again() {
        let l = world::layout("grok-recut");
        let f = Fake::default();
        publish_grok(&f, "1.0.68", &native_exe("first cut"));
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        crate::ops::uninstall(&l, "grok").unwrap();
        publish_grok(&f, "1.0.68", &native_exe("second cut"));
        let downloads = f.downloads.borrow().len();
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("first seen for 1.0.68"), "{}", o.line());
        assert!(
            !o.line().contains("signer refused"),
            "the measured digest refused it before anything was staged: {}",
            o.line()
        );
        assert_eq!(f.downloads.borrow().len(), downloads + 1, "measured once");
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        assert_eq!(shim_build(&l, "grok"), None, "nothing laid");
        assert!(!world::build_dir(&l, "grok", "1.0.68").exists());
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Refused { .. }), "{o:?}");
        assert!(o.line().contains("head moves"), "{}", o.line());
        assert_eq!(
            f.downloads.borrow().len(),
            downloads + 1,
            "not fetched again"
        );
    }

    #[test]
    fn a_short_grok_download_fails_unmemoized_and_lands_on_the_next_pass() {
        let l = world::layout("grok-short");
        let f = Fake::default();
        let exe = native_exe("grok 1.0.68");
        let url = publish_grok(&f, "1.0.68", &exe);
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
        f.payload(&url, &exe[..exe.len() - 1]);
        let o = land_one(&lane, &pending);
        assert!(matches!(o.verdict, Verdict::Failed { .. }), "{o:?}");
        assert!(o.line().contains("of its"), "{}", o.line());
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        assert_eq!(ProgramStamp::read(&l, "grok").unwrap().refused, None);
        assert!(!super::super::digests::digests_path(&l, "grok").exists());
        f.payload(&url, &exe);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    }

    /// The team pin, for each new row: a darwin Mach-O signed by another team is refused and
    /// memoized; grok's measured digest is never logged for it, and its version is not fetched
    /// again until a person asks; kimi's documented digest was logged before the download, as
    /// codex's is.
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
        publish_grok(&f, "1.0.68", &native_exe("not xAI's"));
        let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
        assert!(o.line().contains("signer refused"), "{}", o.line());
        let log = super::super::digests::digests_path(&l, "grok");
        assert!(!log.exists(), "nothing the pin refused is logged");
        assert!(staged(&l, "grok").is_empty(), "{:?}", staged(&l, "grok"));
        let downloads = f.downloads.borrow().len();
        let o = run_with(&l, &f, &Policy::default(), &refusing, grok(), false);
        assert!(o.line().contains("head moves"), "{}", o.line());
        assert_eq!(f.downloads.borrow().len(), downloads, "not fetched again");
        ProgramStamp::clear_refusal(&l, "grok").unwrap();
        let genuine = native_exe("xAI's");
        publish_grok(&f, "1.0.68", &genuine);
        let o = run(&l, &f, &Policy::default(), grok());
        assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains(&world::sha(&genuine))
        );
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
                measured: false,
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
                measured: false,
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
        for (spec, version) in [
            (claude(), "2.1.282"),
            (codex(), "0.157.0"),
            (agy(), "1.3.7"),
            (kimi(), "2.3.2"),
        ] {
            let publish = |f: &Fake, cut: &str| {
                let exe = native_exe(cut);
                match spec.program {
                    "claude" => f.publish_claude(version, &exe),
                    "codex" => f.publish_codex(version, &[("bin/codex", &exe, 0o755)]),
                    "kimi" => drop(publish_kimi(f, version, &exe)),
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
