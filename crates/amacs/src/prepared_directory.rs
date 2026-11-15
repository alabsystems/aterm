// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Base-bound directory refresh without moving live view or write authority to workers.

use crate::editor::{DirectoryCoordinates, PreparedDirectoryCoordinates};
use crate::prepared::{Evidence, Permit, PreparedText};
use crate::store::DirectoryListing;
use crate::text::TextRope;
use aterm_buffer::Seq;
use std::sync::Arc;

#[derive(Debug)]
struct BaseData {
    // Retain the old graph until replacement, stale rejection or cancellation retires it.
    _rope: TextRope,
    listing: Arc<DirectoryListing>,
}
/// Immutable captured directory base. Identity remains in the UI owner.
#[derive(Debug)]
pub struct DirectoryPreparationBase {
    pub(crate) seq: Seq,
    pub(crate) projection: Arc<str>,
    data: Option<BaseData>,
    permit: Option<Permit>,
    evidence: Evidence,
}
impl DirectoryPreparationBase {
    pub(crate) fn capture(
        seq: Seq,
        projection: Arc<str>,
        rope: TextRope,
        listing: Arc<DirectoryListing>,
        target: &PreparedText,
    ) -> Self {
        Self {
            seq,
            projection,
            data: Some(BaseData {
                _rope: rope,
                listing,
            }),
            permit: target.take_directory_retirement_node(),
            evidence: Evidence::default(),
        }
    }
    pub(crate) fn listing(&self) -> &Arc<DirectoryListing> {
        &self.data.as_ref().expect("captured directory").listing
    }
    /// Called on the preparation worker. Coordinates contain only scalar positions,
    /// never document/view identities, editors, capabilities or journal ownership.
    pub fn prepare(
        mut self,
        target: PreparedText,
        views: Vec<DirectoryCoordinates>,
    ) -> Result<PreparedDirectoryUpdate, &'static str> {
        if self.permit.is_none() {
            self.permit = target.take_directory_retirement_node();
        }
        let permit = self
            .permit
            .as_ref()
            .ok_or("directory preparation was already consumed")?;
        let _construction = permit.begin_construction();
        if views.iter().any(|view| !view.anchored_at(self.seq)) {
            return Err("directory view is not at the captured base");
        }
        let listing = target
            .directory_listing_on_worker()
            .ok_or("directory preparation was already consumed")?;
        let edits = crate::diff::minimal_edits(&self.projection, &target.projection());
        let changed = !edits.is_empty();
        let deltas: Vec<_> = edits
            .into_iter()
            .map(|edit| crate::store::EditDelta {
                old: edit.range,
                inserted_len: edit.insert.len(),
            })
            .collect();
        let coordinates = views
            .into_iter()
            .map(|view| {
                let target = self.listing().target_at(view.point());
                view.prepare(&deltas, listing.point_for(target))
            })
            .collect();
        Ok(PreparedDirectoryUpdate {
            base: self,
            target,
            coordinates,
            changed,
        })
    }
}
impl Drop for DirectoryPreparationBase {
    fn drop(&mut self) {
        if let Some(data) = self.data.take() {
            if let Some(permit) = self.permit.take() {
                permit.retire(self.evidence.clone(), move || drop(data));
            } else {
                drop(data);
            }
        }
    }
}
/// The worker's immutable refresh, still bound to its exact captured base.
#[derive(Debug)]
pub struct PreparedDirectoryUpdate {
    pub(crate) base: DirectoryPreparationBase,
    pub(crate) target: PreparedText,
    pub(crate) coordinates: Vec<PreparedDirectoryCoordinates>,
    pub(crate) changed: bool,
}
impl PreparedDirectoryUpdate {
    pub fn coordinates(&self) -> &[PreparedDirectoryCoordinates] {
        &self.coordinates
    }
}
