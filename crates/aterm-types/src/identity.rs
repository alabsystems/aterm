// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Who makes aterm: the origin every user-facing surface prints under its title
//! (`aterm --help`, `--version`, the manual, `atpkg help`, Settings ▸ About), defined once.

/// The author.
pub const AUTHOR: &str = "Andrew Yates";

/// The company.
pub const COMPANY: &str = "ALab";

/// The project site, as printed (no scheme).
pub const SITE: &str = "alab.systems";

/// The project site as a link: [`SITE`] behind `https://`.
pub const SITE_URL: &str = "https://alab.systems";

/// The origin line every text surface prints under its title.
pub const ORIGIN_LINE: &str = "by Andrew Yates \u{00b7} ALab \u{00b7} alab.systems";
