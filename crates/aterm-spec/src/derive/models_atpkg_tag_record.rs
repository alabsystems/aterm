// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `<build>.tracked-install` record against the macOS tag it explains: the WRITER
//! (an install whose tag could not be cleared) and the readers that must square with it
//! (the store heal that clears records, and `aterm pkg doctor`).

use super::*;

/// One build of the atpkg store. `tagged` is whether its files carry
/// `com.apple.provenance`, `record` whether a `<build>.tracked-install` record stands
/// beside it, `active` whether it is the build the shims run. `healed` is 1 right after
/// a store heal (`atpkg::provenance::heal_store`), whose last act squares the records
/// with what it could not clear; `said` is doctor's last word on the tag (0 not asked,
/// 1 silent, 2 warned). An install writes a record only when its heal failed; a later
/// retag by some tracked writer leaves none.
///
/// `Buggy=1` is the shape before 2026-09-23: nothing ever cleared a record — neither
/// beside a build whose files the heal made clean, nor beside a build no longer active —
/// and doctor warned from the record ("a stale record"), not from the files. Each
/// invariant has its own counterexample. Tier-1 (`atpkg::install`'s tests) drives the
/// real record writer, the real store heal and the real doctor over a fixture store, and
/// replays the never-cleared record as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_tag_record_model() -> Model {
    crate::ty_model! {
        AtpkgTagRecord {
            const Buggy = 0;
            var tagged = 0;
            var record = 0;
            var active = 1;
            var healed = 0;
            var said = 0;

            // An install whose own heal cleared what it laid: clean, no record.
            action StageHealed when (active == 1) {
                tagged = 0;
                record = 0;
                healed = 0;
                said = 0;
            }
            // An install whose heal failed: installed tagged, and recorded.
            action StageLeftTagged when (active == 1) {
                tagged = 1;
                record = 1;
                healed = 0;
                said = 0;
            }
            // A tracked writer touches the files afterwards; nothing records that.
            action Retag when (tagged == 0) {
                tagged = 1;
                healed = 0;
                said = 0;
            }
            // A newer build becomes the active one; this one stays for a rollback.
            action Supersede when (active == 1) {
                active = 0;
                healed = 0;
                said = 0;
            }
            // The store heal clears the active build, then squares its record.
            action HealClears when (active == 1) {
                tagged = 0;
                record = if Buggy == 1 { record } else { 0 };
                healed = 1;
                said = 0;
            }
            // The store heal could not clear the active build: the record stays with it.
            action HealFails when (active == 1 && tagged == 1) {
                healed = 1;
                said = 0;
            }
            // The store heal does not reach a build that is not active, and drops its
            // record: nothing would ever clear it otherwise.
            action HealPassesBy when (active == 0) {
                record = if Buggy == 1 { record } else { 0 };
                healed = 1;
                said = 0;
            }
            // Doctor reads the files the heal reaches.
            action Doctor when (active <= 1) {
                said = if (active == 1 && tagged == 1) || (Buggy == 1 && record == 1) {
                    2
                } else {
                    1
                };
            }
            invariant RecordOnlyBesideATag:
                healed == 0 || record == 0 || (tagged == 1 && active == 1);
            invariant DoctorSaysWhatIsOnDisk:
                said == 0 ||
                (said == 2 && tagged == 1 && active == 1) ||
                (said == 1 && (tagged == 0 || active == 0));
        }
    }
}
