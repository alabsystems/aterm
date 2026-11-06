// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The size and label words a machine's description is built from, shared by
//! `aterm_machines::hardware` (the controller's names) and [`super::long_name`]
//! (the OS names), so the two never disagree about what a disk or a model is called.

/// Apple internal SSD capacities and minimum reported size ratios ([`disk_size`]).
const SOLD: [(u64, &str, u128); 8] = [
    (128_000_000_000, "128GB", 940),
    (256_000_000_000, "256GB", 940),
    (512_000_000_000, "512GB", 940),
    (1_000_000_000_000, "1TB", 995),
    (2_000_000_000_000, "2TB", 995),
    (4_000_000_000_000, "4TB", 995),
    (8_000_000_000_000, "8TB", 995),
    (16_000_000_000_000, "16TB", 995),
];

/// A disk's size as Apple sells it, from the bytes `ioreg` reports for the whole disk.
#[must_use]
pub fn disk_size(bytes: u64, rotational: bool) -> String {
    let sold = SOLD.iter().find(|(size, _, least)| {
        let (b, s) = (u128::from(bytes), u128::from(*size));
        b * 1000 >= s * least && b * 1000 <= s * 1001
    });
    if let (false, Some((_, sold, _))) = (rotational, sold) {
        return (*sold).to_owned();
    }
    if bytes < 999_500_000_000 {
        return format!("{}GB", rounded(bytes, 1_000_000_000));
    }
    let tenths = rounded(bytes, 100_000_000_000);
    if tenths.is_multiple_of(10) {
        format!("{}TB", tenths / 10)
    } else {
        format!("{}.{}TB", tenths / 10, tenths % 10)
    }
}

/// Memory as Apple sells it: whole gibibytes, written `GB` (`hw.memsize` 34359738368 is `32GB`).
#[must_use]
pub fn memory_size(bytes: u64) -> String {
    format!("{}GB", rounded(bytes, 1 << 30))
}

/// `bytes` in whole `unit`s (an even number), half a unit rounding up, for every `bytes`: no
/// half unit is added first, which a reading near `u64::MAX` has no room for.
fn rounded(bytes: u64, unit: u64) -> u64 {
    bytes / unit + u64::from(bytes % unit >= unit / 2)
}

/// `text` as macOS turns a Computer Name into a Local hostname.
#[must_use]
pub fn label_form(text: &str) -> String {
    label_form_keeping(text, "")
}

/// [`label_form`] with each `keep` in `text` (such as a naming template's placeholder) preserved.
#[must_use]
pub fn label_form_keeping(text: &str, keep: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if !keep.is_empty()
            && let Some(after) = rest.strip_prefix(keep)
        {
            out.push_str(keep);
            rest = after;
            continue;
        }
        rest = &rest[c.len_utf8()..];
        if matches!(c, '\'' | '\u{2019}') {
            continue;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::{disk_size, memory_size};

    /// The largest reading names the top size, half units rounding up as for every other.
    /// NEGATIVE CONTROL: the readings either side of a half unit round down and up.
    #[test]
    fn a_reading_near_the_top_of_u64_rounds_without_overflow() {
        assert_eq!(memory_size(u64::MAX), "17179869184GB");
        assert_eq!(disk_size(u64::MAX, true), "18446744.1TB");
        assert_eq!(disk_size(u64::MAX, false), "18446744.1TB");
        const GIB: u64 = 1 << 30;
        assert_eq!(memory_size(32 * GIB + GIB / 2 - 1), "32GB");
        assert_eq!(memory_size(32 * GIB + GIB / 2), "33GB");
        assert_eq!(disk_size(1_049_999_999_999, true), "1TB");
        assert_eq!(disk_size(1_050_000_000_000, true), "1.1TB");
        assert_eq!(disk_size(499_499_999_999, true), "499GB");
        assert_eq!(disk_size(499_500_000_000, true), "500GB");
    }
}
