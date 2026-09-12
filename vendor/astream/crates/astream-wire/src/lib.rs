#![forbid(unsafe_code)]
//! # astream-wire
//!
//! The pure vocabulary of the astream message bus: the wire [`Frame`] codec,
//! the [`Subject`] / [`Filter`] address grammar, the single canonical
//! [`assign_partition`] function, and [`Offset`] arithmetic.
//!
//! Design rules carried over as *lessons* from the kafka2 audit:
//!
//! * **No panics on hostile input.** Decoders return `Result`/`Option`; the
//!   encode path returns `Result` instead of `.expect()`-ing (kafka2 panicked
//!   in `frame::encode`).
//! * **One partitioner.** There is exactly one [`assign_partition`]; kafka2
//!   shipped a second, divergent round-robin partitioner.
//! * **Names match behavior.** [`crate::hash::crc32_ieee`] is named for the
//!   polynomial it computes (kafka2 called an IEEE CRC `compute_crc32c`).
//! * **Zero dependencies.** This crate pulls in nothing, so it is trivially
//!   auditable and reproducible.

pub mod frame;
pub mod hash;
pub mod offset;
pub mod partition;
pub mod subject;

pub use frame::{Decoded, Frame, FrameError, HEADER_SIZE, MAX_PAYLOAD_LEN};
pub use hash::{crc32_ieee, fnv1a_64};
pub use offset::Offset;
pub use partition::{assign_partition, time_bucket, PartitionKey, TIME_BUCKET_MS};
pub use subject::{Filter, FilterError, Subject, SubjectError};
