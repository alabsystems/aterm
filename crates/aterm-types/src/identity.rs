// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHO MAKES aterm — the origin every user-facing surface prints under its title
//! (`aterm --help`, `aterm --version`, the manual's front page and agent brief,
//! `atpkg help`, Settings ▸ About), so a person who meets the program cold knows
//! where it comes from. Owner's ruling, 2026-09-14: "I want alab.systems printed
//! somewhere in the about string and help text so that people know where this
//! comes from — and by Andrew Yates." ONE definition, so no surface can drift.

/// The author.
pub const AUTHOR: &str = "Andrew Yates";

/// The company.
pub const COMPANY: &str = "ALab";

/// The project site, as printed (no scheme).
pub const SITE: &str = "alab.systems";

/// The project site as an absolute URL — what About's "Open Project Site" opens.
pub const SITE_URL: &str = "https://alab.systems";

/// The ONE origin line every text surface prints under its title.
pub const ORIGIN_LINE: &str = "by Andrew Yates \u{00b7} ALab \u{00b7} alab.systems";

#[cfg(test)]
mod tests {
    use super::*;

    /// The line is the parts, so editing one without the other is caught here.
    #[test]
    fn the_origin_line_is_assembled_from_the_parts() {
        assert_eq!(
            ORIGIN_LINE,
            format!("by {AUTHOR} \u{00b7} {COMPANY} \u{00b7} {SITE}")
        );
        assert_eq!(SITE_URL, format!("https://{SITE}"));
        assert!(
            !SITE.contains("://"),
            "SITE is printed bare; SITE_URL carries the scheme"
        );
    }
}
