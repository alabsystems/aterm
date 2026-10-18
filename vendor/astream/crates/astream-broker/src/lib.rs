#![forbid(unsafe_code)]
//! `astream-broker` — astream's **servable broker**: the message bus you actually
//! connect to.
//!
//! A thread-per-connection daemon that turns astream's invariants into a usable
//! system, over a local Unix-domain socket ([`Broker::serve`], same-machine), plain
//! TCP ([`Broker::serve_tcp`] — plaintext, trusted network only), or — with the
//! `aead` feature — an XChaCha20-Poly1305-sealed TCP transport
//! (`Broker::serve_tcp_sealed`, confidential + authenticated). With the `cap`
//! feature a broker opened by `Broker::open_guarded` enforces signed capabilities on
//! its accept path — presented as a proof of possession over a per-connection nonce
//! (`Hello` → `Nonce` → `Attach` → `Mark`), so the capability's tag never crosses the
//! wire, and held on a bounded per-connection keyring. The same Frame protocol rides
//! every transport, and a caller that picks the transport at runtime reaches all of
//! them through ONE function: [`connect`] takes a [`Transport`] and hands back a
//! transport-erased [`AnyClient`] plus the [`Closer`] that ends it from another thread
//! and bounds its reads and writes, with an optional connect timeout.
//!
//! - **PUBLISH** appends to a durable log with **exactly-once ingest** — a re-sent
//!   `(producer_id, producer_seq)` is not re-appended and returns its original
//!   offset (survives a broker restart). Writes are GROUP-COMMITTED: one writer
//!   thread batches every mutation and, at the Strict tier, fsyncs once per batch
//!   (`ack ⟹ fsync'd`); a single connection can pipeline many publishes.
//! - **SUBSCRIBE(filter, from_offset)** streams every matching record (the
//!   `astream_wire::Filter` grammar) in offset order, gaplessly, then tails live.
//! - **RESUME**: a consumer that crashes reconnects with `from_offset =
//!   last_committed + 1` and continues with no gaps and no dups.
//! - **REPLAY-SUBSCRIPTION**: `subscribe(filter, 0)` re-delivers full history
//!   deterministically — time-travel as a native operation.
//! - **DURABLE CONSUMER GROUPS**: `commit` / `subscribe_group` keep a group's
//!   committed offset ON the broker, and `process_and_produce` is the atomic
//!   read-process-write transaction (output + commit in one durable record) — true
//!   end-to-end exactly-once processing.
//! - **RETAINED STATE**: `Last{filter, after, max}` is the last record of every
//!   matching subject, paged in subject order and closed by a `Mark{next, head}` a
//!   `subscribe` on the same connection can tail on from. [`Client::last_walk`] and
//!   [`Client::last_all`] page it to the end on the resume cursor and keep the FIRST
//!   page's mark — the one splice point a paged answer has.
//! - **BOUNDED READS**: `Fetch{from, filter, max}` reads at most `max` matching
//!   records while scanning at most `FETCH_SCAN_MAX`, and — unlike every other
//!   streaming verb — leaves the connection usable.
//! - **A LAST WILL**: `Will{..}` is the record the broker appends when your
//!   connection ends, deduped by your own producer key and fenced by any later
//!   record of yours; every will the log still holds is re-fired on open. A TCP
//!   client that keeps alive ([`Client::keepalive_if_idle`]) is held to
//!   [`KEEPALIVE_DEADLINE`], so one that vanishes without a FIN ends — and its will
//!   fires — instead of holding its connection until the broker restarts.
//! - **LOG IDENTITY**: the answer to `Hello` names the log served — a
//!   [`LogIdentity`], a random id and a generation that moves whenever an offset may
//!   have come to name a different record (a repair's cut, an older copy restored, a
//!   Relaxed run's power loss) — so a client that persisted offsets checks them first
//!   ([`Client::log_identity`]); a replica reports its leader's. Beside it rides a
//!   [`Lineage`] — where generations still agree, so a reader resumes at
//!   [`LogIdentity::continues`] instead of forgetting — and the tier the broker serves
//!   ([`Client::log_lineage`], [`Client::served_durability`]).
//! - **FORK-DELIVERY and cognition routing** build on the same SUBSCRIBE primitive.
//! - **The durability dial**: Strict (a full flush per batch), Ordered (an ordering
//!   barrier per batch, `F_BARRIERFSYNC`: a power loss leaves a prefix of whole batches,
//!   the newest possibly lost and the loss announced; a full flush issued once a horizon
//!   has passed since the first ack none covers, so that ack is at most the horizon plus
//!   one batch and one full flush from stable media — and with no horizon, no age bounds
//!   the loss), Relaxed (page cache), and Replicated
//!   ([`Broker::open_replicated`]: an ack waits for a follower quorum). On Ordered and
//!   Relaxed an ESCALATION TABLE ([`Escalation`], in [`OpenOptions`]) names the records a
//!   person would miss: the broker holds them unseen and unacked until a full flush has
//!   made them durable, outside the log lock. Every `Mark` carries the durable head, and
//!   `Hello` the tier served ([`Broker::durable_head`], [`Client::seen_durable`],
//!   [`Client::served_durability`]).
//!   [`ShardedBroker`] runs N share-nothing partitions. Every batch is SEALED with its
//!   record count (its other records marked as continuing it) and every open records a
//!   durable floor, so after a power loss the log is a prefix of whole batches (Strict)
//!   or its longest intact prefix (Relaxed, with the loss announced by the generation)
//!   — no refusal for a torn write once a sealing build has opened it, but for the one
//!   residual [`Durability::Strict`] names. [`Broker::open_opts`] is the one open the
//!   other constructors are shorthands for.
//!
//! Everything is std-only (threads + `Mutex`/`Condvar` + sockets) over the
//! `astream_wire` codec; `forbid(unsafe)`. The default build has zero third-party
//! deps — the optional `cap` / `aead` features pull the vetted in-tree crypto crates.
//!
//! Honest boundary: delivery to the socket is ordered, gapless, and AT-LEAST-ONCE
//! (the consumer's cursor or a durable group commit makes it effectively exactly-once;
//! [`drain`] / [`take`] / [`ack`] are that inbox loop as library helpers).

// A contracted fn is written once, through `contract_fn!`: under a Trust compiler
// (`cfg(clean_islands)`, build.rs) it carries its `ensures` clause, which no stock rustc can
// parse, and every other compiler gets the plain fn.
#[macro_use]
#[path = "../../astream-wire/contract_fn.rs"]
mod contract_fn;

// Plain-Rust twins of the functions this crate's Clean islands compile, for a compiler
// that cannot lex the islands.
#[cfg(any(test, not(clean_islands)))]
mod twins;

pub mod brecord;
pub mod broker;
pub mod client;
#[cfg(test)]
mod crash_sim;
mod escalation;
pub mod logid;
pub mod proto;
mod recover;
/// cfg(unix): sharding binds one Unix-domain socket per shard, and std has no
/// Unix-domain sockets on Windows (where the broker serves TCP only).
#[cfg(unix)]
pub mod sharded;
mod storage;
pub mod store;

pub use brecord::{
    brecord_clamp_body_len, brecord_version_current, BrokerRecord, BREC_VERSION, MAX_RECORD_PAYLOAD,
};
pub use broker::{
    broker_batch_clamp, broker_wake_attempts_within_cap, identity_principal, Broker, BrokerHandle,
    BrokerStats, IDLE_MARK, IDLE_MARK_DEADLINE, KEEPALIVE_DEADLINE, KEEPALIVE_INTERVAL,
};
pub use client::{
    ack, broker_refusal, client_walk_page_advance, client_walk_within_limit, connect, drain,
    protocol_mismatch, take, AnyClient, AnyStream, AnySubscription, BrokerRefusal, Client, Closer,
    CreditWindow, Event, Page, ProtocolMismatch, Record, SubjectBudget, Subscription,
    SubscriptionCloser, Transport, Walk, ACK_SEQ_BASE, LAST_WALK_PAGES_MAX,
};

/// Capability minting and principal derivation (`cap` feature), re-exported so
/// callers of [`Broker::open_guarded`] / [`Client::attach`] can mint and derive
/// producer IDs without depending on `astream-cap` directly.
#[cfg(feature = "cap")]
pub use astream_cap::{mint, producer_id_of, Capability};

/// Static public-key identity types (Rung 8), re-exported so callers of
/// [`Broker::serve_tcp_identity`] / [`Client::connect_tcp_identity`] can build an
/// identity without depending on `astream-aead` directly.
#[cfg(feature = "identity")]
pub use astream_aead::{IdentityKeypair, ID_PUB_LEN};
pub use escalation::{Escalate, Escalation};
pub use logid::{logid_generation_advance, logid_lineage_within_cap, Lineage, LogIdentity};
pub use proto::{NonceExt, Request, Response, ServedInfo, ServedReason, Tier};
pub use recover::{recover_keep_within_len, recover_unit_start_advance};
#[cfg(unix)]
pub use sharded::{
    sharded_clamp_index, sharded_count_nonzero, ShardedBroker, ShardedClient, ShardedHandle,
    ShardedSubscription,
};
pub use storage::{storage_len_advance, storage_truncate_within_len};
pub use store::{
    store_ack_seq_in_reserved_half, store_head_advance, Boot, BrokerLog, Durability, FlushCounts,
    InjectedFaults, LogStats, OpenOptions, Prepared, Volume, ORDERED_HORIZON, RETIRE_SUBJECT,
};

#[cfg(test)]
mod test_scratch;
