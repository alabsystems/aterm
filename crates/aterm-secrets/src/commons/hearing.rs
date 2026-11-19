// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a roster writer heard from Commons members, and the gate it keeps (C26,
//! `docs/DESIGN-alab-swarm-2026-10-27.md` §4.2). 0.145 and earlier refuse a control node with
//! more than [`LEGACY_MAX_FACTS`] facts, so the first roster past 63 members is written only once
//! every current member heard within [`RECENT`] reports aterm [`LIFTED`] or later; a member
//! silent for [`SILENT`] leaves the roster (never the vault) and returns when it is heard again.
//!
//! A member's version is a [`report`]: 80 bytes signed by its Commons writer key, sent after a
//! completed repair round, which a 0.145 peer neither sends nor reads. The [`Ledger`] is this
//! device's own record of the authenticated contacts and reports it saw, and of how long it has
//! watched without a gap while hearing peers: a member counts as silent only once that watch is
//! long enough to have heard it, and only [`SILENT`] after it was last heard or first seen on the
//! roster. The local writer's version comes from this running build, without counting it as
//! a peer contact or report.
//!
//! The same reports keep a second gate, the registry's ([`in_place`], tracker LT-04B): no
//! registry secret goes into the vault until every current member reports, within [`RECENT`], a
//! release that writes no vault entry to a file ([`NO_SECRET_FILE`]), since a build that still
//! writes them would put the value in plain text under `~/.secrets`. That gate counts only what
//! it heard: a member unheard for [`RECENT`] holds it however long this device watched, for a
//! Mac that comes back on an older build would write the file; only a member gone from the
//! roster stops counting, and the vault's own list of readers still counts it
//! (`crate::client::fleet_in_place`).
//!
//! A member this device never meets can be heard through another member (tracker LT-04C): a
//! member signs its version with the time it signed ([`report_at`], 136 bytes), a device keeps
//! the newest such report of every roster member to hand to its peers ([`Reports`]), and
//! [`Ledger::relay`] counts one for the gates above from the time its member signed it. A
//! relayed report is no contact: it neither starts nor extends the watch. The 80-byte
//! [`report`] and the `heard` file keep their formats.

use super::{
    binding::Binding,
    bootstrap::Bootstrap,
    constraints::{LEGACY_MAX_FACTS, MAX_BINDINGS},
    journal::Trust,
    roster::{LEGACY_MAX_MEMBERS, MAX_MEMBERS},
};
use crate::{Error, Result, fsx};
use astream_wire::sync::Actor;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::BTreeMap;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// A member heard within this must report [`LIFTED`] before a roster crosses the 0.145 caps.
pub const RECENT: u64 = 7 * DAY_MS;
/// A member unheard for this leaves the Commons roster.
pub const SILENT: u64 = 30 * DAY_MS;
/// A watch interrupted for longer than this starts again: a member could have come and gone
/// unheard in the gap.
pub const GAP: u64 = 6 * 60 * 60 * 1000;
/// How recently another admin's device must have reported [`LIFTED`] or later for this one to
/// leave a roster refresh to it ([`Ledger::defers`]): of those admins, the device with the
/// smallest writer acts, so two stewards do not fork the history with the same change.
pub const DEFER: u64 = 15 * 60 * 1000;
/// The first release that verifies nodes past the 0.145 caps.
pub const LIFTED: Version = Version(0, 146, 0);
/// The first release that writes no vault entry to a file under `~/.secrets` and reads each in
/// place instead (tracker LT-04A). The release named here must be one every build of which
/// writes no file: a dev build stamped with the release before it reports that release
/// ([`Version::own`]), so the first release cut after the last file write is gone is the least
/// that qualifies. The workspace stood at 0.202.0 when the apply stopped writing files, so a
/// 0.202.0 build may still write them, and 0.203.0 is the first whose every build writes none.
pub const NO_SECRET_FILE: Option<Version> = Some(Version(0, 203, 0));
/// The first release that reads a table checkpoint (`ATCKP1`, aterm
/// docs/DESIGN-table-checkpoint-2026-11-15.md §5, tracker LT-B10B) whose state is in format 3,
/// the state a read folds from with its claims (tracker LT-B10D3). An older build reads a
/// checkpoint as a command it cannot decode (before 0.203.0) or computes its state in an earlier
/// format and seconds no share of format 3, so no Mac signs one until every member runs a build
/// that reads it. The workspace stood at 0.208.0 when format 3 landed, so a 0.208.0 build may
/// compute format 2, and 0.209.0 is the first whose every build computes format 3 (the reasoning
/// of [`NO_SECRET_FILE`]).
pub const READS_CHECKPOINTS: Version = Version(0, 209, 0);
/// Exact wire size of a signed [`report`].
pub const REPORT_LEN: usize = 4 + 12 + 64;
const REPORT_DOMAIN: &[u8] = b"aterm/commons/version-report/1\0";
/// Exact wire size of a time-signed report ([`report_at`]).
pub const REPORT2_LEN: usize = 4 + 48 + 12 + 8 + 64;
const REPORT2_DOMAIN: &[u8] = b"aterm/commons/version-report/2\0";
/// How far past this device's clock a time-signed report's time may be: one signed further
/// ahead is refused, so a member's fast clock makes its report count at most this much longer
/// than [`RECENT`]. A report's time is trusted no closer than this: one stamped within it after
/// the report a ledger holds does not replace that one ([`Ledger::relay`]).
pub const AHEAD: u64 = 10 * 60 * 1000;
/// A member signs its version again this long after its last time-signed report.
pub const REFRESH: u64 = DAY_MS;
/// The least time between two of one member's time-signed reports that a device keeps: a
/// member moves what every device keeps of it at most 24 times a day.
pub const STEP: u64 = 60 * 60 * 1000;
/// The least time between two writes of the kept reports' file (`<commons>/reports`).
pub const PERSIST: u64 = 60 * 60 * 1000;
/// The most reports one side offers a peer in one round; the rest follow in later rounds.
pub const RELAY_BATCH: usize = 256;
/// The least release a peer must report in a repair round ([`report`]) for time-signed reports
/// to be traded with it after that round, so an older peer receives the bytes it always did.
/// A build stamped with this release from before the trade existed does not answer one: the
/// trade is best effort, after everything else in the round.
pub const RELAYS: Version = Version(0, 207, 0);
const MAGIC: &str = "aterm-commons-heard 1";
const MAX_FILE: usize = 1 << 20;

/// A release number, `MAJOR.MINOR.PATCH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);
impl Version {
    /// The `MAJOR.MINOR.PATCH` that `text` starts with: `0.146.0+g1a2b3c` reads as `0.146.0`.
    pub fn parse(text: &str) -> Option<Self> {
        let core = text.split(['+', '-']).next()?;
        let mut parts = core.split('.').map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
    /// The release of the code that runs this check: what this device reports. The running
    /// binary's installed version (`aterm_stamp`), never below [`LIFTED`]: this code is
    /// [`LIFTED`]'s, so a process that installed no identity (a library's test binary) or one
    /// whose version does not read as a release still runs at least that release.
    pub fn own() -> Self {
        Self::parse(aterm_stamp::app_version()).map_or(LIFTED, |own| own.max(LIFTED))
    }
    fn bytes(self) -> [u8; 12] {
        let mut out = [0; 12];
        out[..4].copy_from_slice(&self.0.to_le_bytes());
        out[4..8].copy_from_slice(&self.1.to_le_bytes());
        out[8..].copy_from_slice(&self.2.to_le_bytes());
        out
    }
}
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn signed(network: [u8; 16], actor: &Actor, version: Version) -> Vec<u8> {
    let mut message = REPORT_DOMAIN.to_vec();
    message.extend_from_slice(&network);
    message.extend_from_slice(actor);
    message.extend_from_slice(&version.bytes());
    message
}

/// Sign `version` as `actor`, whose writer key `seed` must hold.
pub fn report(
    seed: &[u8; 32],
    network: [u8; 16],
    actor: &Actor,
    version: Version,
) -> Result<Vec<u8>> {
    let key = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid Commons writer key"))?;
    if key.public_key().as_ref() != &actor[..32] {
        return Err(Error::new("version report key is not this writer's"));
    }
    let mut wire = b"ACV1".to_vec();
    wire.extend_from_slice(&version.bytes());
    wire.extend_from_slice(key.sign(&signed(network, actor, version)).as_ref());
    Ok(wire)
}

/// The version `actor` signed in `wire` for `network`.
pub fn reported(wire: &[u8], network: [u8; 16], actor: &Actor) -> Result<Version> {
    if wire.len() != REPORT_LEN || &wire[..4] != b"ACV1" {
        return Err(Error::new("invalid Commons version report"));
    }
    let word = |at: usize| u32::from_le_bytes(wire[at..at + 4].try_into().unwrap());
    let version = Version(word(4), word(8), word(12));
    if !crate::ed25519::verifies(&actor[..32], &signed(network, actor, version), &wire[16..]) {
        return Err(Error::new(
            "Commons version report signature did not verify",
        ));
    }
    Ok(version)
}

fn signed_at(network: [u8; 16], actor: &Actor, version: Version, signed_ms: u64) -> Vec<u8> {
    let mut message = REPORT2_DOMAIN.to_vec();
    message.extend_from_slice(&network);
    message.extend_from_slice(actor);
    message.extend_from_slice(&version.bytes());
    message.extend_from_slice(&signed_ms.to_le_bytes());
    message
}

/// Sign `version` as `actor`, whose writer key `seed` must hold, with the time of signing
/// (Unix milliseconds on the signer's clock): `"ACV2"`, the actor, the version, the time and the
/// signature. Its signing domain is not [`report`]'s, so neither signature stands for the other.
pub fn report_at(
    seed: &[u8; 32],
    network: [u8; 16],
    actor: &Actor,
    version: Version,
    signed_ms: u64,
) -> Result<[u8; REPORT2_LEN]> {
    let key = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid Commons writer key"))?;
    if key.public_key().as_ref() != &actor[..32] {
        return Err(Error::new("version report key is not this writer's"));
    }
    let mut wire = [0; REPORT2_LEN];
    wire[..4].copy_from_slice(b"ACV2");
    wire[4..52].copy_from_slice(actor);
    wire[52..64].copy_from_slice(&version.bytes());
    wire[64..72].copy_from_slice(&signed_ms.to_le_bytes());
    wire[72..].copy_from_slice(
        key.sign(&signed_at(network, actor, version, signed_ms))
            .as_ref(),
    );
    Ok(wire)
}

/// The actor, version and signed time a time-signed report's bytes name; nothing is verified.
fn fields(wire: &[u8]) -> Option<(Actor, Version, u64)> {
    if wire.len() != REPORT2_LEN || &wire[..4] != b"ACV2" {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(wire[at..at + 4].try_into().unwrap());
    Some((
        wire[4..52].try_into().unwrap(),
        Version(word(52), word(56), word(60)),
        u64::from_le_bytes(wire[64..72].try_into().unwrap()),
    ))
}

/// Whether a report signed at `signed_ms` counts at `now_ms`: signed within [`RECENT`], and no
/// more than [`AHEAD`] past this clock.
fn timely(signed_ms: u64, now_ms: u64) -> bool {
    signed_ms.saturating_add(RECENT) > now_ms && signed_ms <= now_ms.saturating_add(AHEAD)
}

/// A time-signed report whose signature verified under a member's binding ([`relayed`], its
/// only constructor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relayed {
    network: [u8; 16],
    authority: [u8; 32],
    actor: Actor,
    version: Version,
    signed_ms: u64,
    wire: [u8; REPORT2_LEN],
}

impl Relayed {
    /// The member that signed it.
    pub fn actor(&self) -> Actor {
        self.actor
    }
    /// The version it signed.
    pub fn version(&self) -> Version {
        self.version
    }
    /// When it signed, by its own clock (Unix milliseconds).
    pub fn signed_ms(&self) -> u64 {
        self.signed_ms
    }
    /// The bytes it signed and sent, as [`report_at`] made them.
    pub fn wire(&self) -> &[u8; REPORT2_LEN] {
        &self.wire
    }
}

/// The time-signed report `member` made in `wire`, whoever handed it over. It is refused unless
/// it is [`REPORT2_LEN`] bytes of `"ACV2"`, names `member`'s own actor, was signed within
/// [`RECENT`] of `now_ms` and no more than [`AHEAD`] past it, and, checked last, carries the
/// actor's signature for `member`'s network.
pub fn relayed(wire: &[u8], member: &Binding, now_ms: u64) -> Result<Relayed> {
    let Some((actor, version, signed_ms)) = fields(wire) else {
        return Err(Error::new("invalid Commons relayed version report"));
    };
    if actor != member.actor() {
        return Err(Error::new(
            "Commons relayed version report is not this member's",
        ));
    }
    if !timely(signed_ms, now_ms) {
        return Err(Error::new(
            "Commons relayed version report was not signed within the last 7 days",
        ));
    }
    let network = *member.network();
    if !crate::ed25519::verifies(
        &actor[..32],
        &signed_at(network, &actor, version, signed_ms),
        &wire[72..],
    ) {
        return Err(Error::new(
            "Commons relayed version report signature did not verify",
        ));
    }
    Ok(Relayed {
        network,
        authority: *member.authority(),
        actor,
        version,
        signed_ms,
        wire: wire.try_into().unwrap(),
    })
}

/// What one contact said about the peer's version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Said {
    /// A verified report.
    Version(Version),
    /// A completed round with no valid report: an aterm older than reports, or a report lost.
    Nothing,
    /// The round ended before reports were traded; keep what was known.
    #[default]
    Unknown,
}

/// One member's latest contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// When it was last authenticated, or, never heard, when this device first saw it on the
    /// roster ([`Ledger::note`]) (Unix milliseconds).
    pub at_ms: u64,
    /// What its last completed round reported; `None` when none did or that round said nothing.
    pub version: Option<Version>,
    /// When that round completed; 0 when none did. A report older than [`RECENT`] no longer
    /// counts: the member may have gone back to an older aterm since.
    pub said_ms: u64,
}

/// Why the gate holds for one member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Heard within [`RECENT`] on an older aterm, or without a report from that time.
    Older(Option<Version>),
    /// Not heard within [`RECENT`], and not watched that long either.
    Unwatched,
    /// Not heard within [`RECENT`], under a floor that counts only a member it heard
    /// ([`Floor::unheard_passes`] false), however long this device watched.
    Unheard,
}

/// What a gate holds a member to: the release it must report, and whether a member this device
/// has not heard within [`RECENT`] through a watch at least that long passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floor {
    /// The least release a member must report within [`RECENT`]; `None` where no release
    /// passes, so every member holds the gate.
    pub version: Option<Version>,
    /// Whether a member unheard for [`RECENT`] through a watch that long passes: so for the 0.145
    /// caps ([`CAPS`]), since such a member stopped where its build stopped and a node it cannot
    /// read changes nothing for it; never for the registry ([`in_place`]), where a Mac that
    /// returns on an older build would write what it reads to a file.
    pub unheard_passes: bool,
}

impl Floor {
    /// Whether a member reporting `version` (`None`: no report that still counts) is below this
    /// floor.
    fn below(self, version: Option<Version>) -> bool {
        self.version
            .is_none_or(|floor| version.is_none_or(|version| version < floor))
    }
}

/// The gate a roster past the 0.145 caps waits on (C26).
pub const CAPS: Floor = Floor {
    version: Some(LIFTED),
    unheard_passes: true,
};

/// The first release that follows a vault into format 2: it reads format-2 records and stores
/// the format mark that lets it exchange with a format-2 node. None does yet, so no release
/// passes [`FENCE`]. The format fence itself (the version-2 bound, tracker FL-147) is not
/// enough: a node with it and no format-2 reader states format 1 for good, and a format-2 node
/// exchanges nothing with it ([`crate::crossing::RecordsBound::can_exchange`]), so a floor at
/// the fence's release would let the first format-2 write cut off every Mac but its writer.
/// Tracker FL-148 names its own release here, one whose every build reads format 2 (the
/// reasoning of [`NO_SECRET_FILE`]).
pub const READS_FORMAT: Option<Version> = None;

/// The floor the first format-2 write waits on (`docs/DESIGN-vault-log-2026-11-14.md` step 3;
/// the write, tracker FL-148, waits on it holding for 14 days on both admin seats): every
/// current member heard within [`RECENT`] reports [`READS_FORMAT`] or later, so while none is
/// named, every member heard within [`RECENT`] holds it. A member unheard for [`RECENT`] through
/// a watch that long passes, as for [`CAPS`]: a floor that waited on every seat would never open
/// while most seats are silent, and a Mac that returns below the floor is fenced, not misled:
/// its bound and a format-2 node's cannot exchange
/// ([`crate::crossing::RecordsBound::can_exchange`]), so it takes and serves nothing across the
/// line and stays on its consistent prefix.
pub const FENCE: Floor = Floor {
    version: READS_FORMAT,
    unheard_passes: true,
};

/// How long [`FENCE`] holds on a Mac without a break before that Mac signs its vault's first
/// format-2 record (`docs/DESIGN-vault-log-2026-11-14.md` step 4, FL-D25): any seated member may
/// sign it, an admin seat or not, once its own judgment of the floor held this long
/// ([`FenceWatch`]).
pub const FIRST_WRITE: u64 = 14 * DAY_MS;

const FENCE_MAGIC: &str = "aterm-commons-fence 1";

/// The third line of a fence file that records a break ([`FenceWatch::write`] of `None`).
const FENCE_BROKEN: &str = "broken";

#[cfg(test)]
thread_local! {
    /// MUTANT: a break of the fence watch unlinks its file, unflushed, and records nothing.
    pub(crate) static UNLINK_BREAK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// How long this device judged [`FENCE`] to hold without a break: from `since` to its latest
/// judgment, `through`, each judgment that held within [`GAP`] of the one before, as the
/// [`Ledger`]'s watch is kept. A judgment that does not hold, a gap past [`GAP`] or a clock that
/// went back starts it again. Kept beside the ledger ([`Bootstrap::fence_file`]) under its trust
/// context; no file is a watch that has not begun. Only a Mac whose build reads format 2 judges
/// the floor (`crate::client::format_two`), so no running build keeps the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FenceWatch {
    /// When the unbroken run of judgments that held began (Unix milliseconds).
    pub since: u64,
    /// Its latest judgment (Unix milliseconds).
    pub through: u64,
}

impl FenceWatch {
    /// The watch after a judgment at `now_ms` that the floor `holds`, following `before`.
    #[must_use]
    pub fn observe(before: Option<Self>, holds: bool, now_ms: u64) -> Option<Self> {
        if !holds {
            return None;
        }
        Some(match before {
            Some(watch) if watch.through <= now_ms && now_ms - watch.through <= GAP => Self {
                since: watch.since,
                through: now_ms,
            },
            _ => Self {
                since: now_ms,
                through: now_ms,
            },
        })
    }

    /// Whether the floor has held for [`FIRST_WRITE`] without a break, as judged at `now_ms`:
    /// the watch's latest judgment is within [`GAP`] of `now_ms`, and not after it.
    #[must_use]
    pub fn held(&self, now_ms: u64) -> bool {
        self.through <= now_ms
            && now_ms - self.through <= GAP
            && self.since.saturating_add(FIRST_WRITE) <= self.through
    }

    /// The watch kept beside `bootstrap`'s journal: `None` for no file, a break
    /// ([`FENCE_BROKEN`]), or one that is unsafe, unreadable, malformed or of another trust
    /// context.
    #[must_use]
    pub fn read(bootstrap: &Bootstrap) -> Option<Self> {
        let bytes = fsx::read_private(&bootstrap.fence_file(), 1024).ok()??;
        let text = std::str::from_utf8(bytes.as_slice()).ok()?;
        let mut lines = text.lines();
        if lines.next()? != FENCE_MAGIC || lines.next()? != pins(bootstrap.trust()) {
            return None;
        }
        let (since, through) = lines.next()?.split_once(' ')?;
        let watch = Self {
            since: since.parse().ok()?,
            through: through.parse().ok()?,
        };
        (lines.next().is_none() && watch.since <= watch.through).then_some(watch)
    }

    /// Keep `watch` beside `bootstrap`'s journal, durably: a watch as its two times, and `None`,
    /// a break, as a third line of [`FENCE_BROKEN`], which [`FenceWatch::read`] reads as no
    /// watch. A break is written, not unlinked: a power cut can take back an unlink whose
    /// directory was not flushed, and the watch it ended would then count across the break,
    /// while [`fsx::write_atomic`] flushes the file and its directory before it returns.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn write(watch: Option<Self>, bootstrap: &Bootstrap) -> Result<()> {
        #[cfg(test)]
        if watch.is_none() && UNLINK_BREAK.get() {
            return match std::fs::remove_file(bootstrap.fence_file()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(Error::new(e.to_string()))
                }
                _ => Ok(()),
            };
        }
        let times = watch.map_or_else(
            || FENCE_BROKEN.to_owned(),
            |watch| format!("{} {}", watch.since, watch.through),
        );
        fsx::write_atomic(
            &bootstrap.fence_file(),
            format!("{FENCE_MAGIC}\n{}\n{times}\n", pins(bootstrap.trust())).as_bytes(),
            0o600,
        )
    }
}

/// A trust context as the `heard` ledger's second line names it.
fn pins(trust: Trust) -> String {
    let hex = aterm_codec::hex::encode_lower;
    format!(
        "{} {} {}",
        hex(&trust.network),
        hex(&trust.authority),
        hex(&trust.genesis)
    )
}

/// The gate a registry secret's put into the vault waits on (tracker LT-04B): every current
/// member heard within [`RECENT`] reporting `release` or later, where `release` is the first
/// that writes no vault entry to a file ([`NO_SECRET_FILE`] outside a test); with none named,
/// every member holds it.
#[must_use]
pub const fn in_place(release: Option<Version>) -> Floor {
    Floor {
        version: release,
        unheard_passes: false,
    }
}

/// The gate a table checkpoint's signing waits on (tracker LT-B10B): every current member heard
/// within [`RECENT`] reporting [`READS_CHECKPOINTS`] or later. A member unheard for [`RECENT`]
/// holds it however long this device watched, for a Mac that comes back on an older build could
/// not read the table from the checkpoint; only a member gone from the roster stops counting.
pub const CHECKPOINTS: Floor = in_place(Some(READS_CHECKPOINTS));

/// The gate one Mac passes once it writes no vault entry to a file ([`NO_SECRET_FILE`],
/// [`Ledger::writes_no_secret_file`]).
pub const NO_FILE: Floor = in_place(NO_SECRET_FILE);

/// This device's record of Commons contacts for one trust context (`<commons>/heard`).
#[derive(Clone, Debug)]
pub struct Ledger {
    trust: Trust,
    /// Start of the current unbroken watch, and its latest moment.
    watch: Option<(u64, u64)>,
    heard: BTreeMap<Actor, Entry>,
    /// The last contact this process recorded. Never kept: after a restart the watch waits for
    /// a contact before it grows.
    contact_ms: Option<u64>,
    /// This process's validated local writer. Its running version is known without a peer
    /// report; neither that identity nor that knowledge is persisted as contact evidence.
    local: Option<Actor>,
}

impl Ledger {
    /// An empty ledger: nothing heard, nothing watched.
    pub fn new(trust: Trust) -> Self {
        Self {
            trust,
            watch: None,
            heard: BTreeMap::new(),
            contact_ms: None,
            local: None,
        }
    }

    /// A ledger whose unbroken watch ran from `since` to `through`, for tests.
    #[cfg(test)]
    pub(crate) fn watched(trust: Trust, since: u64, through: u64) -> Self {
        Self {
            watch: Some((since, through)),
            ..Self::new(trust)
        }
    }

    /// The ledger kept beside `bootstrap`'s journal. A missing, unreadable, unsafe or foreign
    /// file supplies no peer evidence and retires nobody. The bootstrap identifies this
    /// process's local writer, whose running version needs no peer report.
    pub fn read(bootstrap: &Bootstrap) -> Self {
        let trust = bootstrap.trust();
        let mut ledger = fsx::read_private(&bootstrap.heard_file(), MAX_FILE)
            .ok()
            .flatten()
            .and_then(|bytes| Self::parse(std::str::from_utf8(bytes.as_slice()).ok()?, trust))
            .unwrap_or_else(|| Self::new(trust));
        if let Ok(request) = bootstrap.request() {
            ledger.know_local(request.binding());
        }
        ledger
    }

    /// Identify this process from its bootstrap or validated local roster access. This is
    /// running-code knowledge only: it cannot extend a watch, refresh a report or defer to an
    /// admin as though a peer had been heard.
    pub(crate) fn know_local(&mut self, binding: &Binding) {
        self.local = (*binding.network() == self.trust.network
            && *binding.authority() == self.trust.authority)
            .then(|| binding.actor());
    }

    /// Keep this ledger beside `bootstrap`'s journal.
    pub fn write(&self, bootstrap: &Bootstrap) -> Result<()> {
        if bootstrap.trust() != self.trust {
            return Err(Error::new("Commons ledger has another trust context"));
        }
        fsx::write_atomic(&bootstrap.heard_file(), self.text().as_bytes(), 0o600)
    }

    fn text(&self) -> String {
        let hex = aterm_codec::hex::encode_lower;
        let (since, through) = self.watch.unwrap_or((0, 0));
        let mut out = format!(
            "{MAGIC}\n{} {} {}\n{since} {through}\n",
            hex(&self.trust.network),
            hex(&self.trust.authority),
            hex(&self.trust.genesis)
        );
        for (actor, entry) in &self.heard {
            let version = entry
                .version
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            out.push_str(&format!(
                "{} {} {version} {}\n",
                hex(actor),
                entry.at_ms,
                entry.said_ms
            ));
        }
        out
    }

    fn parse(text: &str, trust: Trust) -> Option<Self> {
        fn bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
            let mut out = [0; N];
            aterm_codec::hex::decode_into(text.as_bytes(), &mut out).then_some(out)
        }
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut pins = lines.next()?.split(' ');
        let pinned = Trust {
            network: bytes(pins.next()?)?,
            authority: bytes(pins.next()?)?,
            genesis: bytes(pins.next()?)?,
        };
        if pinned != trust || pins.next().is_some() {
            return None;
        }
        let uint = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        let (since, through) = lines.next()?.split_once(' ')?;
        let (since, through) = (uint(since)?, uint(through)?);
        let mut ledger = Self::new(trust);
        ledger.watch = (since > 0 && since <= through).then_some((since, through));
        for line in lines {
            let mut words = line.split(' ');
            let actor = bytes(words.next()?)?;
            let at_ms = uint(words.next()?)?;
            let version = match words.next()? {
                "-" => None,
                word => Some(Version::parse(word)?),
            };
            let said_ms = uint(words.next()?)?;
            if words.next().is_some()
                || ledger.heard.len() >= MAX_BINDINGS
                || ledger
                    .heard
                    .insert(
                        actor,
                        Entry {
                            at_ms,
                            version,
                            said_ms,
                        },
                    )
                    .is_some()
            {
                return None;
            }
        }
        Some(ledger)
    }

    /// Extend the watch to `now_ms` while a peer was heard within [`GAP`]: a device that hears
    /// nobody vouches for nobody's silence. A gap longer than [`GAP`], or a clock that went
    /// back, starts it again. Whether this changed anything a reader relies on.
    pub fn watch(&mut self, now_ms: u64) -> bool {
        if !self
            .contact_ms
            .is_some_and(|at| at <= now_ms && now_ms - at <= GAP)
        {
            return false;
        }
        let next = match self.watch {
            Some((since, through)) if through <= now_ms && now_ms - through <= GAP => {
                (since, now_ms)
            }
            _ => (now_ms, now_ms),
        };
        let restarted = self.watch.is_none_or(|(since, _)| since != next.0);
        self.watch = Some(next);
        restarted
    }

    /// Record an authenticated contact with `actor` at `at_ms` and what its round said. Whether
    /// this changed a version or added a member.
    pub fn record(&mut self, actor: Actor, at_ms: u64, said: Said) -> bool {
        self.contact_ms = Some(at_ms);
        let added = !self.heard.contains_key(&actor);
        if added && self.heard.len() >= MAX_BINDINGS {
            return false;
        }
        let entry = self.heard.entry(actor).or_insert(Entry {
            at_ms,
            version: None,
            said_ms: 0,
        });
        let before = entry.version;
        entry.at_ms = entry.at_ms.max(at_ms);
        let version = match said {
            Said::Version(version) => Some(version),
            Said::Nothing => None,
            Said::Unknown => return added,
        };
        if at_ms >= entry.said_ms {
            entry.version = version;
            entry.said_ms = at_ms;
        }
        added || before != entry.version
    }

    /// Count a member's time-signed report that reached this device through any member. It
    /// counts only for this ledger's network and authority, never for the local writer, and only
    /// while it is signed within [`RECENT`] of `now_ms` and no more than [`AHEAD`] past it. The
    /// member is heard from the time it signed, or from `now_ms` where it signed later, so the
    /// ledger holds no time ahead of this clock and a later round with the member still
    /// replaces what this wrote ([`Self::record`]). Its version replaces the one held only when
    /// that time is more than [`AHEAD`] later than the held report's. So an older report never
    /// replaces a newer one, relayed or heard directly, and neither does one stamped within
    /// [`AHEAD`] after a round with the member: its signer's clock may run that fast, so it may
    /// have been signed before the round. Two reports a device keeps of one member are [`STEP`]
    /// apart, so the later of them always counts. This is no contact: the watch neither starts
    /// nor grows by it. Whether this changed a version or added a member.
    pub fn relay(&mut self, report: &Relayed, now_ms: u64) -> bool {
        if report.network != self.trust.network
            || report.authority != self.trust.authority
            || self.local == Some(report.actor)
            || !timely(report.signed_ms, now_ms)
        {
            return false;
        }
        let at_ms = report.signed_ms.min(now_ms);
        let added = !self.heard.contains_key(&report.actor);
        if added && self.heard.len() >= MAX_BINDINGS {
            return false;
        }
        let entry = self.heard.entry(report.actor).or_insert(Entry {
            at_ms,
            version: None,
            said_ms: 0,
        });
        let before = entry.version;
        entry.at_ms = entry.at_ms.max(at_ms);
        if at_ms > entry.said_ms.saturating_add(AHEAD) {
            entry.version = Some(report.version);
            entry.said_ms = at_ms;
        }
        added || before != entry.version
    }

    /// Start the clock on a member of the current roster this device never heard: it is silent
    /// only [`SILENT`] after this, and until it is heard it holds the gate for [`RECENT`] as a
    /// member heard without a report. Whether it was new.
    pub fn note(&mut self, actor: Actor, now_ms: u64) -> bool {
        if self.heard.contains_key(&actor) || self.heard.len() >= MAX_BINDINGS {
            return false;
        }
        self.heard.insert(
            actor,
            Entry {
                at_ms: now_ms,
                version: None,
                said_ms: 0,
            },
        );
        true
    }

    /// The latest contact with `actor`.
    pub fn entry(&self, actor: &Actor) -> Option<&Entry> {
        self.heard.get(actor)
    }

    /// Since when the watch has run unbroken up to `now_ms`, if it is current.
    fn watched_since(&self, now_ms: u64) -> Option<u64> {
        self.watch
            .filter(|(_, through)| *through <= now_ms && now_ms - through <= GAP)
            .map(|(since, _)| since)
    }

    /// Whether `actor` was authenticated, or first noted, within `window` of `now_ms` (or after
    /// it, by a clock that went back).
    pub fn heard_within(&self, actor: &Actor, now_ms: u64, window: u64) -> bool {
        self.heard
            .get(actor)
            .is_some_and(|e| e.at_ms.saturating_add(window) > now_ms)
    }

    /// Whether `actor` reported [`LIFTED`] or later in a round completed within `window` of
    /// `now_ms`.
    pub fn lifted_within(&self, actor: &Actor, now_ms: u64, window: u64) -> bool {
        self.heard.get(actor).is_some_and(|e| {
            e.version >= Some(LIFTED) && e.said_ms > 0 && e.said_ms.saturating_add(window) > now_ms
        })
    }

    /// Whether this device, writing as `local`, leaves a roster refresh to one of the `admins`'
    /// devices: one with a smaller writer that reported [`LIFTED`] or later within [`DEFER`],
    /// so it runs the refresh too.
    pub fn defers(
        &self,
        local: &Actor,
        admins: impl IntoIterator<Item = Actor>,
        now_ms: u64,
    ) -> bool {
        admins
            .into_iter()
            .any(|admin| admin < *local && self.lifted_within(&admin, now_ms, DEFER))
    }

    /// The most members a roster read by `readers` may hold: [`MAX_MEMBERS`] once the history
    /// `crossed` the 0.145 caps or none of them holds the gate ([`Self::holding`]), else
    /// [`LEGACY_MAX_MEMBERS`].
    pub fn room<'a>(
        &self,
        crossed: bool,
        readers: impl IntoIterator<Item = &'a Binding>,
        now_ms: u64,
    ) -> usize {
        if crossed || self.holding(readers, now_ms).is_empty() {
            MAX_MEMBERS
        } else {
            LEGACY_MAX_MEMBERS
        }
    }

    /// Whether `actor` has been unheard for [`SILENT`] through a watch at least that long.
    pub fn silent(&self, actor: &Actor, now_ms: u64) -> bool {
        self.watched_since(now_ms)
            .is_some_and(|since| since.saturating_add(SILENT) <= now_ms)
            && !self.heard_within(actor, now_ms, SILENT)
    }

    /// The gate: each of `members` that holds a roster past the 0.145 caps back, and why
    /// ([`CAPS`]).
    pub fn holding<'a>(
        &self,
        members: impl IntoIterator<Item = &'a Binding>,
        now_ms: u64,
    ) -> Vec<(Actor, Hold)> {
        self.holding_below(members, now_ms, CAPS)
    }

    /// Whether `member`, one device, writes no vault entry to a file under `~/.secrets`: within
    /// [`RECENT`] of `now_ms` it reported, signed by its Commons writer key, [`NO_SECRET_FILE`]
    /// or later; for this device's own writer, this running build is that release or later
    /// ([`Version::own`]). It fails closed: a device unheard within [`RECENT`] however long this
    /// one watched, one whose last round reported nothing, one whose report is older than
    /// [`RECENT`] and one on an older release each write files as far as this device can tell.
    /// The per-device predicate of [`NO_FILE`], which a registry value's put (tracker LT-04B)
    /// asks of every Mac that reads the vault.
    #[must_use]
    pub fn writes_no_secret_file(&self, member: &Binding, now_ms: u64) -> bool {
        self.holding_below([member], now_ms, NO_FILE).is_empty()
    }

    /// Each of `members` that holds back what waits on `floor`, and why: the local writer where
    /// this running build is below it ([`Version::own`]); a member heard within [`RECENT`] whose
    /// report from that time is below it or missing; and a member not heard within [`RECENT`],
    /// unless the floor lets one pass ([`Floor::unheard_passes`]) and this device watched that
    /// long.
    pub fn holding_below<'a>(
        &self,
        members: impl IntoIterator<Item = &'a Binding>,
        now_ms: u64,
        floor: Floor,
    ) -> Vec<(Actor, Hold)> {
        let watched = self
            .watched_since(now_ms)
            .is_some_and(|since| since.saturating_add(RECENT) <= now_ms);
        members
            .into_iter()
            .filter_map(|member| {
                let actor = member.actor();
                if self.local == Some(actor) {
                    let version = Version::own();
                    return floor
                        .below(Some(version))
                        .then_some((actor, Hold::Older(Some(version))));
                }
                if self.heard_within(&actor, now_ms, RECENT) {
                    let entry = self.heard[&actor];
                    let version = entry
                        .version
                        .filter(|_| entry.said_ms.saturating_add(RECENT) > now_ms);
                    floor
                        .below(version)
                        .then_some((actor, Hold::Older(version)))
                } else if !floor.unheard_passes {
                    Some((actor, Hold::Unheard))
                } else {
                    (!watched).then_some((actor, Hold::Unwatched))
                }
            })
            .collect()
    }
}

/// Evidence for writing a control node past the 0.145 caps: the writer's ledger and its clock.
pub struct Gate<'a> {
    pub ledger: &'a Ledger,
    pub now_ms: u64,
}

impl Gate<'_> {
    /// Refuse a node 0.145 cannot read unless the control history already holds one (every
    /// 0.145 device stopped there, so another changes nothing for it) or every member of the
    /// current roster heard within [`RECENT`] reports [`LIFTED`] or later.
    pub(crate) fn admit<'b>(
        gate: Option<&Self>,
        facts: usize,
        crossed: bool,
        current: impl IntoIterator<Item = &'b Binding>,
    ) -> Result<()> {
        if facts <= LEGACY_MAX_FACTS || crossed {
            return Ok(());
        }
        let Some(gate) = gate else {
            return Err(Error::new(
                "a Commons roster past 63 members needs this Mac's record of which members run aterm 0.146",
            ));
        };
        let held = gate.ledger.holding(current, gate.now_ms);
        if held.is_empty() {
            return Ok(());
        }
        let older = held
            .iter()
            .filter(|(_, hold)| matches!(hold, Hold::Older(_)))
            .count();
        Err(Error::new(format!(
            "the Commons roster stays within 63 members until every member heard in the last 7 days runs aterm {LIFTED} or later: {older} heard recently run an older aterm or have not reported a version, {} not yet watched for 7 days",
            held.len() - older
        )))
    }
}

mod relay;
pub use relay::{
    ANSWER_MAX, GIVE_MAX, LAST_MAX, Mark, OFFER_MAX, Offer, Reports, Tag, answer_flight,
    give_flight, last_flight, offer_flight, read_answer, read_give, read_last, read_offer, tag,
    taken,
};

#[cfg(test)]
mod tests;
