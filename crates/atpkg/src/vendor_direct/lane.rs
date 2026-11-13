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
        let active = crate::ops::active_build_of(layout, program);
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
    let active = crate::ops::active_build_of(layout, program);
    let stamp = ProgramStamp::read(layout, program).unwrap_or_default();
    let etag = stamp.last_head.and(stamp.etag.clone());
    let head = resolve::read_head(spec, fetcher, etag.as_deref());
    let verified = match &head {
        Head::Named { version, body, .. }
            if installed_of(layout, program, active).version() != Some(*version)
                && !crate::pin::holds_update(layout, program) =>
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
    let active = crate::ops::active_build_of(layout, program);
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
            asked: lane.asked
                || (crate::alab::vendor_chosen(layout, program)
                    && active.is_some_and(|b| crate::alab::build_is_alab(layout, program, b))),
        };
        let mut decision = decide(&inputs);
        if let Installed::Legacy {
            build,
            version: None,
        } = installed
            && !legacy_asked
            && !crate::alab::build_is_alab(layout, program, build)
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
        crate::flow::Retry::Program,
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
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = crate::metadata_io::open_regular_with_meta(dl)
        .map_err(|_| format!("it answered no file of its {} bytes", companion.size))?;
    if meta.len() != companion.size {
        return Err(format!(
            "it answered {} of its {} bytes",
            meta.len(),
            companion.size
        ));
    }
    let declared = aterm_zstd::declared_content_size(&mut file).map_err(|e| e.to_string())?;
    if declared != Some(companion.binary_size) {
        return Err(format!(
            "its zstd frame declares {}, not the binary's {} bytes",
            declared.map_or_else(|| String::from("no size"), |n| format!("{n} bytes")),
            companion.binary_size
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("open the download: {e}"))?;
    let mut decoder = aterm_zstd::Decoder::new(file)
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
    let (file, meta) = crate::metadata_io::open_regular_with_meta(&path).map_err(StageError::Io)?;
    let len = meta.len();
    let sha256 = crate::tree::sha256_opened(file).map_err(StageError::Io)?;
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
        version: "",
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
    let shimmed = crate::ops::active_build_of(layout, program) == Some(build);
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
    let mut views: Vec<InstalledView> = crate::ops::list_installed_for(layout, program)
        .into_iter()
        .filter(|b| is_vendor_build(*b) && *b < below)
        .filter(|b| layout.build_dir(program, *b).join("bin").is_dir())
        .filter_map(|b| complete_record(&layout.build_dir(program, b)))
        .map(|r| r.installed_view())
        .collect();
    views.sort_by_key(|v| std::cmp::Reverse(v.version));
    views
}

fn retained_legacy_below(layout: &Layout, program: &str, below: u64) -> Vec<u64> {
    let mut builds: Vec<u64> = crate::ops::list_installed_for(layout, program)
        .into_iter()
        .filter(|b| !is_vendor_build(*b) && *b < below)
        .filter(|b| layout.build_dir(program, *b).join("bin").is_dir())
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
    let active = crate::ops::active_build_of(layout, program);
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
                    super::display_have_build(layout, program, b)
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
                    .map(|b| {
                        let words = if crate::alab::build_is_alab(layout, program, b) {
                            crate::alab::build_words(layout, program, b)
                        } else {
                            super::build_words(b)
                        };
                        (b, words)
                    }),
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
    if !crate::alab::build_is_alab(layout, program, to_build) {
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
        Installed::Legacy { build, .. } if !crate::alab::build_is_alab(layout, program, build) => {
            build
        }
        Installed::Legacy { .. } => return None,
        Installed::Vendor(current) => retained_legacy_below(layout, program, current.build)
            .into_iter()
            .find(|b| !crate::alab::build_is_alab(layout, program, *b))?,
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
#[path = "lane_world_tests.rs"]
pub(crate) mod world;

#[cfg(test)]
#[path = "lane_alab_tests.rs"]
mod alab_tests;

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;
