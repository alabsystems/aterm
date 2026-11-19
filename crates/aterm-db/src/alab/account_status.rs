// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An account's standing (docs/DESIGN-shared-files-2026-11-11.md §10, "Account status and its
//! event log"): the one reading of `account.status` every gate asks, and the closed words of
//! `account-event`, the log that says what moved an account's status and why.
//!
//! A status word says where the account stands; an event row says what happened. A provider
//! that banned, suspended or broke an account, the owner's sheet that marks a provider's cell
//! `0`, or a person's note each leave one event row naming the account, the source that said so
//! (a closed word), the cause (a closed word, never a vendor's text) and the status it moved
//! from and to.

/// The account statuses, in the order the column's enum holds them: words are only ever
/// appended (design §3.8), so a build that knows an earlier list reads each word it knows.
pub const STATUS: &[&str] = &[
    "pending",
    "active",
    "paused",
    "broken",
    "banned",
    "suspended",
    "cancelled",
];

/// A status conflict's provisional word: the most final hold first, so two Macs that disagree
/// read the account as held rather than usable until the owner resolves it.
pub const SAFEST_FIRST: &[&str] = &[
    "banned",
    "suspended",
    "broken",
    "cancelled",
    "paused",
    "pending",
    "active",
];

/// Whether an account may be signed in to or gain a roster row unasked: its row is live and its
/// status is absent, `pending` or `active`. Every other word, including one this build does not
/// know, holds it.
pub fn usable(live: bool, status: Option<&str>) -> bool {
    live && status.is_none_or(|status| matches!(status, "pending" | "active"))
}

/// Who or what said an account event (`account-event.source`).
pub const SOURCES: &[&str] = &[
    "sheet",
    "fleet-db",
    "vendor-reply",
    "vendor-message",
    "person",
    "agent",
    "aterm",
    "history",
];

/// Why an account's status moved (`account-event.cause`): closed words, never a vendor's text.
pub const CAUSES: &[&str] = &[
    // The owner's sheet holds `0` in the provider's column.
    "sheet-zero",
    // A hold word in the sheet's notes for this vendor.
    "sheet-hold",
    // A fleet_db replica's status column.
    "fleet-db-status",
    // The owner set the status.
    "owner",
    // A verified sign-in made a pending account active.
    "verified-sign-in",
    // The provider refused the account as banned, suspended or broken.
    "vendor-banned",
    "vendor-suspended",
    "vendor-broken",
    // A note with no status move.
    "note",
];

/// The source and cause of the event the importer births beside a banned account it reads from
/// a sheet's `0`.
pub const SHEET_ZERO: (&str, &str) = ("sheet", "sheet-zero");

#[cfg(test)]
#[path = "account_status_tests.rs"]
mod tests;
