// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-broker federation (`ROADMAP.md` & `DESIGN-aterm-fabric` Federation across brokers):
//! durable consumer-group-backed subtree mirroring ([`FederationBridge`]) with deterministic
//! `(producer_id, src_offset)` exactly-once deduplication, [`LogIdentity`] lineage continuity
//! checks, loop-preventing [`SubjectRewrite`] rules, and Chandy-Lamport consistent cuts
//! ([`FederatedCut`]) across independent per-host broker logs.

use crate::client::{connect, AnyClient, Closer, Event, Pipelined, Transport, ACK_SEQ_BASE};
use crate::logid::{Lineage, LogIdentity};
use crate::store::is_hidden_subject;
use astream_wire::{CausalCut, CausalEdge, CutViolation, Filter, Offset as WireOffset, Subject};

/// Log offset (`u64`) on a federated source or destination broker.
pub type Offset = u64;
use std::fmt;
use std::io;
#[cfg(feature = "identity")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(clean_islands)]
#[path = "federation_island.rs"]
mod federation_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{federation_cursor_advance, federation_seq_clamp};
#[cfg(clean_islands)]
pub use federation_island::{federation_cursor_advance, federation_seq_clamp};

contract_fn! {
    /// Verified check that a federated record's hop count lies strictly below `max_hops`
    /// (`hops < max_hops`), linked via `clean-kernel-defeq` to `federation_hop_allowed_isl`.
    #[must_use]
    pub const fn federation_hop_allowed(hops: u8, max_hops: u8) -> bool
    clauses [ensures result == (hops < max_hops)]
    {
        hops < max_hops
    }
}

/// Maximum burst of records pipelined to the destination broker in one round trip.
const FEDERATION_BATCH_CAP: usize = 256;

/// Wire transport for dialing a source or destination broker in [`FederationEndpoint`].
#[derive(Clone)]
pub enum FederationWire {
    /// Any standard client [`Transport`] (`Unix`, `Tcp`, `Sealed`, or `Handshake`).
    Client(Transport),
    /// Mutual Ed25519 signed-DH (`Broker::serve_tcp_identity`) with a pinned server key.
    #[cfg(feature = "identity")]
    Identity {
        /// Bridge's Ed25519 client identity keypair.
        identity: Arc<astream_aead::IdentityKeypair>,
        /// Pinned 32-byte Ed25519 public key expected from the remote broker.
        expected_server: [u8; astream_aead::ID_PUB_LEN],
    },
}

impl fmt::Debug for FederationWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FederationWire::Client(t) => write!(f, "FederationWire::Client({t:?})"),
            #[cfg(feature = "identity")]
            FederationWire::Identity { .. } => f.write_str("FederationWire::Identity(<pinned>)"),
        }
    }
}

/// Connection specification (address, transport, timeouts, and optional capability keyring)
/// for one side of a [`FederationBridge`].
#[derive(Clone, Debug)]
pub struct FederationEndpoint {
    /// Socket path (`Transport::Unix`) or `host:port` TCP address.
    pub endpoint: String,
    /// Wire transport and authentication credentials.
    pub wire: FederationWire,
    /// Optional TCP/handshake dial deadline.
    pub connect_timeout: Option<Duration>,
    /// Optional per-request socket read/write deadline once connected.
    pub io_timeout: Option<Duration>,
    /// Capabilities attached immediately after connecting on guarded brokers.
    #[cfg(feature = "cap")]
    pub caps: Vec<astream_cap::Capability>,
}

impl FederationEndpoint {
    /// Create an endpoint over a standard [`Transport`] (`Unix`, `Tcp`, `Sealed`, `Handshake`).
    #[must_use]
    pub fn new(endpoint: &str, transport: Transport) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            wire: FederationWire::Client(transport),
            connect_timeout: Some(Duration::from_secs(5)),
            io_timeout: Some(Duration::from_secs(10)),
            #[cfg(feature = "cap")]
            caps: Vec::new(),
        }
    }

    /// Create an endpoint over mutual Ed25519 signed-DH (`Broker::serve_tcp_identity`).
    #[cfg(feature = "identity")]
    #[must_use]
    pub fn identity(
        endpoint: &str,
        identity: Arc<astream_aead::IdentityKeypair>,
        expected_server: [u8; astream_aead::ID_PUB_LEN],
    ) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            wire: FederationWire::Identity {
                identity,
                expected_server,
            },
            connect_timeout: Some(Duration::from_secs(5)),
            io_timeout: Some(Duration::from_secs(10)),
            #[cfg(feature = "cap")]
            caps: Vec::new(),
        }
    }

    /// Attach a capability keyring presented on every dial/reconnect.
    #[cfg(feature = "cap")]
    #[must_use]
    pub fn with_caps(mut self, caps: Vec<astream_cap::Capability>) -> Self {
        self.caps = caps;
        self
    }

    /// Override `(connect_timeout, io_timeout)`.
    #[must_use]
    pub const fn with_timeouts(
        mut self,
        connect_timeout: Option<Duration>,
        io_timeout: Option<Duration>,
    ) -> Self {
        self.connect_timeout = connect_timeout;
        self.io_timeout = io_timeout;
        self
    }

    fn dial(&self) -> io::Result<(AnyClient, Closer)> {
        #[cfg_attr(not(feature = "cap"), allow(unused_mut))]
        let (mut client, closer) = match &self.wire {
            FederationWire::Client(t) => connect(t, &self.endpoint, self.connect_timeout)?,
            #[cfg(feature = "identity")]
            FederationWire::Identity {
                identity,
                expected_server,
            } => crate::client::connect_identity(
                &self.endpoint,
                identity,
                expected_server,
                self.connect_timeout,
            )?,
        };
        if let Some(io_to) = self.io_timeout {
            closer.set_read_timeout(Some(io_to))?;
            closer.set_write_timeout(Some(io_to))?;
        }
        #[cfg(feature = "cap")]
        if !self.caps.is_empty() {
            let mut ring: Vec<(&str, &[u8])> = Vec::with_capacity(self.caps.len().min(64));
            for cap in &self.caps {
                ring.push((cap.filter.as_str(), cap.tag.as_slice()));
            }
            client.attach_all(ring)?;
        }
        Ok((client, closer))
    }
}

/// Subject prefix transformation and loop-exclusion policy for [`FederationBridge`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubjectRewrite {
    /// Leading segment prefix stripped from matched source subjects (`""` = strip nothing).
    pub strip_prefix: String,
    /// Leading segment prefix prepended on the destination broker (`""` = add nothing).
    pub add_prefix: String,
    /// Segment prefixes skipped as peer-federated echo traffic so bidirectional bridges
    /// (`Host A <-> Host B`) never re-mirror records back to their origin.
    pub exclude_prefixes: Vec<String>,
    /// Current federation hop index (`0` for direct origin -> peer mirroring).
    pub hops: u8,
    /// Maximum allowed hop count (`1` by default; `0` blocks all forwarding).
    pub max_hops: u8,
}

impl SubjectRewrite {
    /// Identity pass-through (`max_hops = 1`, no prefix rewrite, no exclusions).
    #[must_use]
    pub fn passthrough() -> Self {
        Self {
            strip_prefix: String::new(),
            add_prefix: String::new(),
            exclude_prefixes: Vec::new(),
            hops: 0,
            max_hops: 1,
        }
    }

    /// Map `<strip_prefix>/...` on the source broker to `<add_prefix>/...` on the
    /// destination broker (`max_hops = 1`).
    pub fn prefix_map(strip_prefix: &str, add_prefix: &str) -> Result<Self, String> {
        let rw = Self {
            strip_prefix: strip_prefix.to_string(),
            add_prefix: add_prefix.to_string(),
            exclude_prefixes: Vec::new(),
            hops: 0,
            max_hops: 1,
        };
        rw.validate()?;
        Ok(rw)
    }

    /// Add a segment prefix excluded from mirroring (bidirectional loop prevention).
    pub fn with_excluded_prefix(mut self, prefix: &str) -> Result<Self, String> {
        validate_prefix_shape(prefix, "exclude_prefix")?;
        self.exclude_prefixes.push(prefix.to_string());
        Ok(self)
    }

    /// Set `(hops, max_hops)` checked via [`federation_hop_allowed`].
    #[must_use]
    pub const fn with_hop_budget(mut self, hops: u8, max_hops: u8) -> Self {
        self.hops = hops;
        self.max_hops = max_hops;
        self
    }

    /// Validate all configured prefixes against `astream-wire`'s subject grammar.
    pub fn validate(&self) -> Result<(), String> {
        if !self.strip_prefix.is_empty() {
            validate_prefix_shape(&self.strip_prefix, "strip_prefix")?;
        }
        if !self.add_prefix.is_empty() {
            validate_prefix_shape(&self.add_prefix, "add_prefix")?;
        }
        for ex in &self.exclude_prefixes {
            validate_prefix_shape(ex, "exclude_prefix")?;
        }
        Ok(())
    }

    /// Rewrite `src_subject`, returning `Ok(None)` if excluded by loop-prevention rules or
    /// hop budget, `Ok(Some(dst_subject))` when eligible, or `Err` if resulting subject is invalid.
    pub fn apply(&self, src_subject: &str) -> Result<Option<String>, String> {
        if !federation_hop_allowed(self.hops, self.max_hops) {
            return Ok(None);
        }
        for ex in &self.exclude_prefixes {
            if matches_segment_prefix(src_subject, ex) {
                return Ok(None);
            }
        }
        if !self.add_prefix.is_empty() && matches_segment_prefix(src_subject, &self.add_prefix) {
            // Already carries the destination's federated prefix: never double-wrap or loop.
            return Ok(None);
        }
        let tail = if self.strip_prefix.is_empty() {
            src_subject
        } else if src_subject == self.strip_prefix {
            return Err(format!(
                "source subject {src_subject:?} equals strip_prefix {:?} with no trailing segment",
                self.strip_prefix
            ));
        } else {
            match src_subject.strip_prefix(&self.strip_prefix) {
                Some(rest) if rest.starts_with('/') => rest,
                _ => return Ok(None),
            }
        };
        let mapped = if self.add_prefix.is_empty() {
            tail.to_string()
        } else {
            format!("{}{tail}", self.add_prefix)
        };
        if Subject::new(&mapped).is_err() {
            return Err(format!(
                "rewritten subject {mapped:?} is not a valid Subject"
            ));
        }
        Ok(Some(mapped))
    }
}

fn validate_prefix_shape(prefix: &str, field: &str) -> Result<(), String> {
    if prefix.is_empty() || !prefix.starts_with('/') || prefix.ends_with('/') {
        return Err(format!(
            "{field} {prefix:?} must start with '/' and not end with '/'"
        ));
    }
    if Subject::new(prefix).is_err() {
        return Err(format!("{field} {prefix:?} is not a valid Subject prefix"));
    }
    Ok(())
}

fn matches_segment_prefix(subject: &str, prefix: &str) -> bool {
    if subject == prefix {
        return true;
    }
    match subject.strip_prefix(prefix) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

/// Configuration for opening a [`FederationBridge`].
#[derive(Clone, Debug)]
pub struct FederationConfig {
    /// Source broker endpoint to mirror from.
    pub source: FederationEndpoint,
    /// Destination broker endpoint to publish onto.
    pub dest: FederationEndpoint,
    /// Durable consumer-group name on `source` (must be a valid `Subject`, e.g. `"/g/fed/host-b"`).
    pub group: String,
    /// Subscription [`Filter`] selecting records on `source` (e.g. `"/f/fleet/pub/host-a/>"`).
    pub filter: String,
    /// Stable `producer_id` used on `dest` (on a guarded destination broker with a bound
    /// `rw,p=<principal>:` grant, set to `astream_cap::producer_id_of(principal)`).
    pub producer_id: u64,
    /// Subject prefix mapping and loop-prevention policy.
    pub rewrite: SubjectRewrite,
}

/// Cross-broker provenance receipt emitted for each mirrored record, directly convertible
/// into a Chandy-Lamport [`CausalEdge<LogIdentity>`] for [`FederatedCut`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FederationLink {
    /// Source broker's [`LogIdentity`] at the time the record was read.
    pub src_log: LogIdentity,
    /// Source broker offset of the mirrored record.
    pub src_offset: Offset,
    /// Destination broker's [`LogIdentity`] at the time the record was published.
    pub dst_log: LogIdentity,
    /// Destination broker offset assigned (or deduplicated) for the mirrored record.
    pub dst_offset: Offset,
    /// `true` when `dest` already held `(producer_id, src_offset)` from an earlier attempt.
    pub deduped: bool,
    /// Rewritten subject published onto `dest`.
    pub dst_subject: String,
}

impl FederationLink {
    /// View this cross-broker mirror receipt as a directed [`CausalEdge<LogIdentity>`]
    /// (`cause: (src_log, src_offset) -> effect: (dst_log, dst_offset)`).
    #[must_use]
    pub const fn causal_edge(&self) -> CausalEdge<LogIdentity> {
        CausalEdge {
            cause: (self.src_log, WireOffset(self.src_offset)),
            effect: (self.dst_log, WireOffset(self.dst_offset)),
        }
    }
}

/// Summary returned by [`FederationBridge::sync_once`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FederationReport {
    /// Matching records fetched from `source` in this step.
    pub fetched: usize,
    /// Records published (or deduplicated) onto `dest`.
    pub mirrored: usize,
    /// Subset of `mirrored` that deduplicated against an existing record on `dest`.
    pub deduped: usize,
    /// Records skipped by loop-exclusion / hop-limit / prefix policy.
    pub skipped: usize,
    /// Highest `source` offset durably committed for `group` at the end of this step, if any.
    pub committed_upto: Option<Offset>,
    /// Bridge's next `source` fetch cursor after this step.
    pub next_cursor: Offset,
    /// Provenance links (`src_log@src_offset -> dst_log@dst_offset`) produced in this step.
    pub links: Vec<FederationLink>,
}

/// Failure modes of [`FederationBridge`].
#[derive(Debug)]
pub enum FederationError {
    /// Invalid configuration (bad filter, bad group subject, bad rewrite, or self-loop).
    Config(String),
    /// Remote broker did not advertise a [`LogIdentity`] in `Hello`.
    MissingLogIdentity {
        /// `"source"` or `"dest"`.
        role: &'static str,
    },
    /// Source broker's [`LogIdentity`] changed without continuing across `cursor`
    /// (truncated below `cursor`, restored from an older backup, or replaced).
    SourceLogDiverged {
        /// Previously pinned source identity.
        expected: LogIdentity,
        /// Newly observed source identity.
        found: LogIdentity,
        /// Bridge cursor that is no longer vouched for by `found.continues(&expected)`.
        cursor: Offset,
    },
    /// Destination broker's [`LogIdentity`] changed to an incompatible log/generation.
    DestLogChanged {
        /// Previously pinned destination identity.
        expected: LogIdentity,
        /// Newly observed destination identity.
        found: LogIdentity,
    },
    /// Source offset reached the reserved `ACK_SEQ_BASE` half (`>= 2^63`).
    OffsetInReservedHalf(Offset),
    /// Rewritten subject failed validation.
    Rewrite(String),
    /// Underlying broker or socket I/O error.
    Io(io::Error),
}

impl fmt::Display for FederationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FederationError::Config(msg) => write!(f, "federation config error: {msg}"),
            FederationError::MissingLogIdentity { role } => {
                write!(f, "federation {role} broker did not report a LogIdentity")
            }
            FederationError::SourceLogDiverged {
                expected,
                found,
                cursor,
            } => write!(
                f,
                "federation source log diverged at cursor {cursor}: expected {expected}, found {found}"
            ),
            FederationError::DestLogChanged { expected, found } => write!(
                f,
                "federation destination log identity changed: expected {expected}, found {found}"
            ),
            FederationError::OffsetInReservedHalf(off) => write!(
                f,
                "federation source offset {off} enters reserved ACK_SEQ_BASE half"
            ),
            FederationError::Rewrite(msg) => write!(f, "federation subject rewrite error: {msg}"),
            FederationError::Io(e) => write!(f, "federation I/O error: {e}"),
        }
    }
}

impl std::error::Error for FederationError {}

/// Durable, loop-free, exactly-once cross-broker subtree bridge (`source -> dest`).
pub struct FederationBridge {
    cfg: FederationConfig,
    src_ctl: AnyClient,
    src_closer: Closer,
    dst: AnyClient,
    dst_closer: Closer,
    src_log: LogIdentity,
    dst_log: LogIdentity,
    cursor: Offset,
}

impl FederationBridge {
    /// Validate `cfg`, dial `source` and `dest`, pin both [`LogIdentity`] values, and
    /// bootstrap `cursor` from `cfg.group`'s durable committed position on `source`.
    pub fn open(cfg: FederationConfig) -> Result<Self, FederationError> {
        if Subject::new(&cfg.group).is_err() || is_hidden_subject(&cfg.group) {
            return Err(FederationError::Config(format!(
                "group {:?} must be a valid non-reserved Subject",
                cfg.group
            )));
        }
        if Filter::new(&cfg.filter).is_err() {
            return Err(FederationError::Config(format!(
                "filter {:?} is not a valid Filter",
                cfg.filter
            )));
        }
        if let Err(msg) = cfg.rewrite.validate() {
            return Err(FederationError::Config(msg));
        }

        let (mut src_ctl, src_closer) = match cfg.source.dial() {
            Ok(pair) => pair,
            Err(e) => return Err(FederationError::Io(e)),
        };
        let src_log = read_client_identity(&mut src_ctl, "source")?;

        let (mut dst, dst_closer) = match cfg.dest.dial() {
            Ok(pair) => pair,
            Err(e) => return Err(FederationError::Io(e)),
        };
        let dst_log = read_client_identity(&mut dst, "dest")?;

        if src_log.id == dst_log.id && cfg.rewrite.add_prefix.is_empty() {
            return Err(FederationError::Config(
                "refusing self-federation onto the same broker LogIdentity without a loop-guarded add_prefix".to_string(),
            ));
        }

        let cursor = bootstrap_group_cursor(&cfg.source, &cfg.group, &cfg.filter)?;
        Ok(Self {
            cfg,
            src_ctl,
            src_closer,
            dst,
            dst_closer,
            src_log,
            dst_log,
            cursor,
        })
    }

    /// Pinned [`LogIdentity`] of the source broker.
    #[must_use]
    pub const fn source_identity(&self) -> LogIdentity {
        self.src_log
    }

    /// Pinned [`LogIdentity`] of the destination broker.
    #[must_use]
    pub const fn dest_identity(&self) -> LogIdentity {
        self.dst_log
    }

    /// Current source fetch cursor (`>= group_start` at open).
    #[must_use]
    pub const fn cursor(&self) -> Offset {
        self.cursor
    }

    /// Re-dial `source` and `dest` after a transient network error, verifying that both
    /// brokers' [`LogIdentity`] values remain consistent with `self.cursor`.
    pub fn reconnect(&mut self) -> Result<(), FederationError> {
        self.src_closer.close();
        self.dst_closer.close();

        let (mut new_src, new_src_closer) = match self.cfg.source.dial() {
            Ok(pair) => pair,
            Err(e) => return Err(FederationError::Io(e)),
        };
        let found_src = read_client_identity(&mut new_src, "source")?;
        let lineage = new_src.log_lineage().cloned();
        self.verify_or_adopt_source_identity(found_src, lineage.as_ref())?;

        let (mut new_dst, new_dst_closer) = match self.cfg.dest.dial() {
            Ok(pair) => pair,
            Err(e) => return Err(FederationError::Io(e)),
        };
        let found_dst = read_client_identity(&mut new_dst, "dest")?;
        if found_dst != self.dst_log {
            return Err(FederationError::DestLogChanged {
                expected: self.dst_log,
                found: found_dst,
            });
        }

        self.src_ctl = new_src;
        self.src_closer = new_src_closer;
        self.dst = new_dst;
        self.dst_closer = new_dst_closer;
        Ok(())
    }

    fn verify_or_adopt_source_identity(
        &mut self,
        found: LogIdentity,
        lineage: Option<&Lineage>,
    ) -> Result<(), FederationError> {
        if found == self.src_log {
            return Ok(());
        }
        if let Some(lin) = lineage {
            if let Some(common) = found.continues(self.src_log, lin) {
                if self.cursor <= common {
                    self.src_log = found;
                    return Ok(());
                }
            }
        }
        Err(FederationError::SourceLogDiverged {
            expected: self.src_log,
            found,
            cursor: self.cursor,
        })
    }

    /// Fetch up to `max_records` (`1..=256`) matching records from `source`, pipeline
    /// eligible rewritten publishes onto `dest` under deterministic `(producer_id, src_offset)`
    /// dedup keys, and durably commit `cfg.group` on `source` only after `dest` acknowledges
    /// the entire batch.
    pub fn sync_once(&mut self, max_records: usize) -> Result<FederationReport, FederationError> {
        let bounded_max = max_records.clamp(1, FEDERATION_BATCH_CAP) as u32;
        let ((records, (page_next, _head)), _gap) =
            match self
                .src_ctl
                .fetch_told(self.cursor, &self.cfg.filter, bounded_max)
            {
                Ok(res) => res,
                Err(e) => return Err(FederationError::Io(e)),
            };

        let fetched = records.len();
        let max_seq = ACK_SEQ_BASE.saturating_sub(1);
        let mut staged: Vec<(Offset, u64, String, Vec<u8>)> =
            Vec::with_capacity(fetched.min(FEDERATION_BATCH_CAP));
        let mut highest_src_off: Option<Offset> = None;
        let mut skipped = 0usize;

        for (src_off, src_subj, body) in records {
            if src_off >= ACK_SEQ_BASE {
                return Err(FederationError::OffsetInReservedHalf(src_off));
            }
            highest_src_off = Some(match highest_src_off {
                Some(prev) => federation_cursor_advance(prev, src_off),
                None => src_off,
            });
            if is_hidden_subject(&src_subj) {
                skipped = skipped.saturating_add(1);
                continue;
            }
            match self.cfg.rewrite.apply(&src_subj) {
                Ok(Some(dst_subj)) => {
                    let seq = federation_seq_clamp(src_off, max_seq);
                    staged.push((src_off, seq, dst_subj, body));
                }
                Ok(None) => {
                    skipped = skipped.saturating_add(1);
                }
                Err(msg) => return Err(FederationError::Rewrite(msg)),
            }
        }

        let mut links: Vec<FederationLink> =
            Vec::with_capacity(staged.len().min(FEDERATION_BATCH_CAP));
        let mut deduped_count = 0usize;

        if !staged.is_empty() {
            let mut pipe_ops: Vec<Pipelined<'_>> =
                Vec::with_capacity(staged.len().min(FEDERATION_BATCH_CAP));
            for (_src_off, seq, dst_subj, body) in &staged {
                pipe_ops.push(Pipelined::Publish {
                    producer_id: self.cfg.producer_id,
                    producer_seq: *seq,
                    subject: dst_subj.as_str(),
                    body: body.as_slice(),
                });
            }
            if let Err(e) = self.dst.send_pipelined(&pipe_ops) {
                return Err(FederationError::Io(e));
            }
            for (src_off, _seq, dst_subj, _body) in staged {
                let (dst_off, deduped) = match self.dst.recv_publish_ack() {
                    Ok(ack) => ack,
                    Err(e) => return Err(FederationError::Io(e)),
                };
                if deduped {
                    deduped_count = deduped_count.saturating_add(1);
                }
                links.push(FederationLink {
                    src_log: self.src_log,
                    src_offset: src_off,
                    dst_log: self.dst_log,
                    dst_offset: dst_off,
                    deduped,
                    dst_subject: dst_subj,
                });
            }
        }

        let committed_upto = match highest_src_off {
            Some(upto) => {
                if let Err(e) = self.src_ctl.commit(&self.cfg.group, upto) {
                    return Err(FederationError::Io(e));
                }
                Some(upto)
            }
            None => None,
        };

        let next_cur = match highest_src_off {
            Some(upto) => federation_cursor_advance(page_next, upto.saturating_add(1)),
            None => page_next,
        };
        self.cursor = federation_cursor_advance(self.cursor, next_cur);

        Ok(FederationReport {
            fetched,
            mirrored: links.len(),
            deduped: deduped_count,
            skipped,
            committed_upto,
            next_cursor: self.cursor,
            links,
        })
    }
}

impl Drop for FederationBridge {
    fn drop(&mut self) {
        self.src_closer.close();
        self.dst_closer.close();
    }
}

fn read_client_identity(
    client: &mut AnyClient,
    role: &'static str,
) -> Result<LogIdentity, FederationError> {
    if let Some(id) = client.seen_log_identity() {
        return Ok(id);
    }
    match client.log_identity() {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(FederationError::MissingLogIdentity { role }),
        Err(e) => Err(FederationError::Io(e)),
    }
}

fn bootstrap_group_cursor(
    source: &FederationEndpoint,
    group: &str,
    filter: &str,
) -> Result<Offset, FederationError> {
    let (client, closer) = match source.dial() {
        Ok(pair) => pair,
        Err(e) => return Err(FederationError::Io(e)),
    };
    let mut sub = match client.subscribe_group(group, filter) {
        Ok(s) => s,
        Err(e) => {
            closer.close();
            return Err(FederationError::Io(e));
        }
    };
    let mut resolved: Option<Offset> = None;
    let mut rounds = 0usize;
    while rounds < 16 {
        rounds = rounds.saturating_add(1);
        match sub.recv_event() {
            Ok(Some(Event::Delivery { offset, .. })) => {
                resolved = Some(offset);
                break;
            }
            Ok(Some(Event::Mark { head, .. })) => {
                resolved = Some(head);
                break;
            }
            Ok(Some(Event::Gap(_))) => {}
            Ok(None) => {
                closer.close();
                return Err(FederationError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source broker closed subscription before catch-up mark",
                )));
            }
            Err(e) => {
                closer.close();
                return Err(FederationError::Io(e));
            }
        }
    }
    closer.close();
    match resolved {
        Some(off) => Ok(off),
        None => Err(FederationError::Io(io::Error::other(
            "exceeded gap-notice rounds while bootstrapping federation group cursor",
        ))),
    }
}

/// Cross-broker Chandy-Lamport consistent cut mapping each broker [`LogIdentity`] in a
/// federated fleet to its frontier [`Offset`].
///
/// Wraps [`astream_wire::CausalCut<LogIdentity>`] and evaluates causal consistency,
/// violation witnesses, minimal causal closure (`heal`), and lattice `join`/`meet` over
/// [`FederationLink`] receipts, plus canonical `<id>.<gen>=<offset>` text persistence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FederatedCut {
    inner: CausalCut<LogIdentity>,
}

impl FederatedCut {
    /// An empty cross-broker cut admitting no broker logs.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: CausalCut::new(),
        }
    }

    /// Number of broker logs represented in this cut.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cut tracks zero broker logs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Query the frontier offset recorded for `log` (`None` if `log` is absent).
    #[must_use]
    pub fn frontier(&self, log: &LogIdentity) -> Option<Offset> {
        match self.inner.offset_of(log) {
            Some(off) => Some(off.0),
            None => None,
        }
    }

    /// Set `log`'s frontier to `offset`.
    pub fn insert(&mut self, log: LogIdentity, offset: Offset) {
        self.inner.set(log, WireOffset(offset));
    }

    /// Monotonically advance `log`'s frontier to `max(current, offset)`.
    pub fn advance(&mut self, log: LogIdentity, offset: Offset) {
        self.inner.advance(log, WireOffset(offset));
    }

    /// Borrow the underlying [`CausalCut<LogIdentity>`].
    #[must_use]
    pub const fn as_causal_cut(&self) -> &CausalCut<LogIdentity> {
        &self.inner
    }

    /// Decide Chandy-Lamport consistency across `links`: for every mirrored record whose
    /// `dst_offset` lies within `self.frontier(&dst_log)`, `self.frontier(&src_log)` must
    /// exist and be `>= src_offset`.
    #[must_use]
    pub fn is_consistent(&self, links: &[FederationLink]) -> bool {
        let edges = collect_edges(links);
        self.inner.is_consistent(&edges)
    }

    /// Return the first cross-broker [`CutViolation<LogIdentity>`] witnessed by `links`,
    /// or `None` if this cut is causally consistent.
    #[must_use]
    pub fn first_violation(&self, links: &[FederationLink]) -> Option<CutViolation<LogIdentity>> {
        let edges = collect_edges(links);
        self.inner.first_violation(&edges)
    }

    /// Advance lagging source broker frontiers to the minimal Chandy-Lamport consistent
    /// closure containing `self` with respect to `links`, returning the number of frontier
    /// updates performed.
    pub fn heal(&mut self, links: &[FederationLink]) -> usize {
        let edges = collect_edges(links);
        self.inner.heal(&edges)
    }

    /// Pointwise least upper bound (`max`) of two federated cuts.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            inner: self.inner.join(&other.inner),
        }
    }

    /// Pointwise greatest lower bound (`min` over shared logs) of two federated cuts.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Self {
        Self {
            inner: self.inner.meet(&other.inner),
        }
    }

    /// Whether `self` includes every broker log in `other` at an equal or greater frontier.
    #[must_use]
    pub fn dominates(&self, other: &Self) -> bool {
        self.inner.dominates(&other.inner)
    }

    /// Canonical deterministic `<32hex>.<generation>=<offset>\n` text encoding (sorted by
    /// [`LogIdentity`]), suitable for fabric control messages or sidecar persistence.
    #[must_use]
    pub fn encode_text(&self) -> String {
        let mut out = String::new();
        for (log, off) in self.inner.iter() {
            let off_val = off.0;
            out.push_str(&format!("{log}={off_val}\n"));
        }
        out
    }

    /// Decode a canonical `<32hex>.<generation>=<offset>` text cut, rejecting malformed
    /// identities, duplicate log entries, or non-integer offsets fail-closed.
    pub fn decode_text(text: &str) -> Result<Self, String> {
        const MAX_CUT_ENTRIES: usize = 4096;
        let mut cut = Self::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if cut.len() >= MAX_CUT_ENTRIES {
                return Err(format!(
                    "federated cut exceeds MAX_CUT_ENTRIES ({MAX_CUT_ENTRIES})"
                ));
            }
            let Some((id_str, off_str)) = line.split_once('=') else {
                return Err(format!(
                    "malformed federated cut line {line:?}: missing '='"
                ));
            };
            let Some(log) = LogIdentity::parse(id_str.trim()) else {
                return Err(format!(
                    "invalid LogIdentity {:?} in federated cut",
                    id_str.trim()
                ));
            };
            let off = match off_str.trim().parse::<u64>() {
                Ok(v) => v,
                Err(_) => {
                    return Err(format!(
                        "invalid offset {:?} for {log} in federated cut",
                        off_str.trim()
                    ));
                }
            };
            if cut.frontier(&log).is_some() {
                return Err(format!("duplicate LogIdentity {log} in federated cut"));
            }
            cut.insert(log, off);
        }
        Ok(cut)
    }
}

fn collect_edges(links: &[FederationLink]) -> Vec<CausalEdge<LogIdentity>> {
    const MAX_LINKS: usize = 65_536;
    let cap = links.len().min(MAX_LINKS);
    let mut out = Vec::with_capacity(cap);
    for link in links {
        out.push(link.causal_edge());
    }
    out
}
