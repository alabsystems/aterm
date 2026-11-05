// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence (B7c, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Presence"): each Mac's one row
//! `/m/n-<id>/shared/now/presence` on `metrics.log`, written every 30 seconds with its build, its
//! interface addresses and the digest and count of its membership log and lab tables. A Mac is up
//! when its row advanced within the last 90 seconds of this Mac's clock, and the node service
//! persists, per seat, when it last saw that seat up, at most once per 10 minutes.
//!
//! Empty until B7c. It will publish through [`crate::Mesh::publish`]; a `now` subject keeps the
//! row with the highest sequence number (`aterm_metrics::rules`), so a restarted Mac's first row
//! wins at once.
