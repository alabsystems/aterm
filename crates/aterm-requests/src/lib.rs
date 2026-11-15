// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-Mac request journal (ATERM-KI-1109-3, the requests design's §4 and step 2 of its
//! build order): one record per thing something other than the person at this Mac asked it to
//! do, the unit a person later reviews and cleans up.
//!
//! The store is a folder in the state home ([`DIR`]) holding two newline-framed logs of `k=v`
//! lines in the session log's line shape (each at most [`aterm_slog::MAX_RECORD_BYTES`], values
//! percent-encoded): [`JOURNAL`], the wire and host tiers every reader may read, and [`SAID`],
//! the content tier (a why taken from a brief, goal or asked line, and the requester's own goal
//! and asked lines). Every append takes the log's lock ([`aterm_uds::line_log::hold`]) and is
//! finished by an ordered flush, so a seamless update's old and new process, or two instances on
//! one state home, mint distinct ids and fold the same requests; a power loss may cut only the
//! rows appended last. Nothing here reads a clock: every write takes its time.
//!
//! A request's footprint outside its tabs (step 4) is kept the same way: the folders its spawns
//! opened in (`root`), the folders it claims (`claim`, [`claim`]), a person's later work in one
//! (`touched`) and what each tab's process tree spent (`spent`). Nothing here deletes.
//!
//! A person's review of a request (step 5) is kept the same way: `review` (who, when, a note),
//! `kept` (its tabs and folders are a person's) and `reply` (a mail to its requester, its text in
//! the content tier); and its cleanup: `clean` (who began it), `archived` (the archive written
//! first), `gone` (what became of each claimed folder), and `cleaned` or `uncleaned`. Nothing here
//! deletes: [`claim::clean_verdict`] judges which claimed folders a cleanup may delete.

pub mod claim;
pub mod id;
pub mod record;
pub mod store;
pub mod why;

pub use claim::{Claim, ClaimSrc, HostFacts, Keep, Measured, TouchWhy, clean_verdict};
pub use record::{
    Brief, Did, EndWhy, Form, Out, Rec, Said, SaidBody, Spent, TaskEnd, Unclean, Verb, Via, WhySrc,
    archive_name,
};
pub use store::{
    ActRefusal, Answered, Archive, Born, ClaimAsk, ClaimRefusal, Claimed, CleanBegan, CleanEnd,
    CleanPlan, CleanStart, CleanedAs, Ended, From, Gone, GoneClaim, InspectionStatus, Intake,
    KeptRequest, LEASE_IDLE_MS, Left, Look, MAX_REPLIES, Ran, Refusal, Reply, Request, Review,
    Reviewed, SaidOf, Stage, State, Store, Tab, TabNow, Touch, lines_of,
};
pub use why::{Why, why_of};

/// The store's folder in the state home.
pub const DIR: &str = "requests";

/// The wire- and host-tier log in [`DIR`].
pub const JOURNAL: &str = "journal";

/// The content-tier log in [`DIR`].
pub const SAID: &str = "said";
