// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The public release channel: the GitHub owner every ALab public release is published
//! under. One value, read by aterm-update-core (`DEFAULT_OWNER`, `ATPKG_INDEX_OWNER`) and
//! by agh's release floor (`agh::config::RELEASE_FLOOR_OWNERS`), which can only raise an
//! owner to release tier.

/// The GitHub owner of the public release channel.
pub const PUBLIC_OWNER: &str = "alabsystems";
