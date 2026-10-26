#![forbid(unsafe_code)]
//! # astream-wire
//!
//! The pure vocabulary of the astream message bus: the wire [`Frame`] codec,
//! the [`Subject`] / [`Filter`] address grammar, the single canonical
//! [`assign_partition`] function, [`Offset`] arithmetic, the Chandy-Lamport
//! [`CausalCut`] frontier lattice, the fabric receipts ([`V1Receipt`], [`FabricReceipt`]), and the
//! [`steps`] record — astream's generic dense, versioned, CRC-framed opaque
//! record — with its torn-tail-or-fault classifier.
//!
//! Design rules:
//!
//! * **No panics on hostile input.** Decoders return `Result`/`Option`; the
//!   encode path returns `Result` rather than panicking on an oversized payload.
//! * **One partitioner.** There is exactly one [`assign_partition`], so every
//!   router derives the same partition for the same message.
//! * **Names match behavior.** [`crate::hash::crc32_ieee`] is named for the
//!   polynomial it computes (IEEE, not Castagnoli/CRC-32C).
//! * **Zero dependencies.** This crate pulls in nothing, so it is trivially
//!   auditable and reproducible.

// A contracted fn is written once, through `contract_fn!`: under a Trust compiler
// (`cfg(clean_islands)`, build.rs) it carries its `ensures` clause, which no stock rustc can
// parse, and every other compiler gets the plain fn.
#[macro_use]
#[path = "../contract_fn.rs"]
mod contract_fn;

// Plain-Rust twins of the functions this crate's Clean islands compile, for a compiler
// that cannot lex the islands.
#[cfg(any(test, not(clean_islands)))]
mod twins;

pub mod cut;
pub mod frame;
pub mod hash;
pub mod offset;
pub mod partition;
pub mod receipt;
pub mod steps;
pub mod subject;
/// Transport-independent causal replication of independently authored records.
pub mod sync;

pub use cut::{CausalCut, CausalEdge, CutViolation};
pub use frame::{
    frame_clamp_payload_len, payload_within_cap, Decoded, Frame, FrameError, HEADER_SIZE,
    MAX_PAYLOAD_LEN,
};
pub use hash::{
    crc32_ieee, crc32_ieee_update, crc32_is_ieee_poly, crc32_poly_select, crc_table_index_clamp,
    crc_table_index_in_bounds, fnv1a_64, fnv1a_64_is_init, fnv1a_64_is_prime, fnv1a_64_mix_u64,
    fnv1a_64_mul_prime, fnv1a_64_state_matches, fnv1a_64_update, CRC32_IEEE_POLY, FNV1A_64_INIT,
    FNV1A_64_PRIME,
};
pub use offset::{offset_advance_monotone, offset_precedes_or_eq, Offset};
pub use partition::{
    assign_partition, partition_in_bounds, partition_modulus_clamp, time_bucket, PartitionKey,
    TIME_BUCKET_MS,
};
pub use receipt::{deadline_elapsed, FabricReceipt, ReceiptError, ReceiptVerdict, V1Receipt};
pub use steps::{step_prefix_advance, step_within_bounds};
pub use subject::{
    filter_depth_contains, segment_depth_advance, Filter, FilterError, Subject, SubjectError,
};
pub use sync::{sync_clamp_parent_count, sync_within_capacity};
