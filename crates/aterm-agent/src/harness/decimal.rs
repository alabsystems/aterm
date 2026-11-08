// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact decimal scaling for the vendor figures whose lower bounds the harness says.

/// A non-negative JSON decimal multiplied by `10^places`: its integer floor and whether any
/// positive fraction remains. `max` is both the admitted ceiling and the largest returned floor.
///
/// The caller supplies an already validated JSON number lexeme. This routine refuses negative
/// (other than negative zero) and over-ceiling input rather than letting a floating-point
/// conversion move it across a boundary; JSON grammar validation remains the parser's job.
pub(super) fn scaled_floor(text: &str, places: u32, max: u32) -> Option<(u32, bool)> {
    let bytes = text.as_bytes();
    let (negative, start) = match bytes.first() {
        Some(b'-') => (true, 1),
        Some(_) => (false, 0),
        None => return None,
    };
    let exponent_at = bytes[start..]
        .iter()
        .position(|b| matches!(b, b'e' | b'E'))
        .map_or(bytes.len(), |at| start + at);
    let mantissa = bytes.get(start..exponent_at)?;
    let mut whole_digits = 0i64;
    let mut digit_count = 0i64;
    let mut saw_dot = false;
    let mut any_nonzero = false;
    for &byte in mantissa {
        match byte {
            b'0'..=b'9' => {
                if !saw_dot {
                    whole_digits = whole_digits.saturating_add(1);
                }
                digit_count = digit_count.saturating_add(1);
                any_nonzero |= byte != b'0';
            }
            b'.' if !saw_dot => saw_dot = true,
            _ => return None,
        }
    }
    if digit_count == 0 || (negative && any_nonzero) {
        return None;
    }

    let exponent = if exponent_at == bytes.len() {
        0
    } else {
        let rest = bytes.get(exponent_at + 1..)?;
        let (minus, digits) = match rest.first() {
            Some(b'+') => (false, rest.get(1..)?),
            Some(b'-') => (true, rest.get(1..)?),
            Some(_) => (false, rest),
            None => return None,
        };
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let magnitude = digits.iter().fold(0i64, |value, digit| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(*digit - b'0'))
        });
        if minus {
            magnitude.saturating_neg()
        } else {
            magnitude
        }
    };
    let cut = whole_digits
        .saturating_add(exponent)
        .saturating_add(i64::from(places));

    let mut floor = 0u32;
    let mut digit_index = 0i64;
    let mut remainder = false;
    for &byte in mantissa {
        let b'0'..=b'9' = byte else { continue };
        let digit = u32::from(byte - b'0');
        if digit_index < cut {
            floor = floor.checked_mul(10)?.checked_add(digit)?;
            if floor > max {
                return None;
            }
        } else {
            remainder |= digit != 0;
        }
        digit_index = digit_index.saturating_add(1);
    }
    let mut zeros = cut.saturating_sub(digit_count);
    while zeros > 0 && floor != 0 {
        floor = floor.checked_mul(10)?;
        if floor > max {
            return None;
        }
        zeros -= 1;
    }
    if floor == max && remainder {
        return None;
    }
    Some((floor, remainder))
}

/// A figure in tenths of a percent as the whole percent the harness prints: half up, except
/// that no reading under 100 (`x10 < 1000`) is said as `100`, so 99.96 (999 tenths) is `99`
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.1: 100% is printed only when a reading
/// says 100). At 1000 tenths and above it is the plain half-up figure.
pub(super) const fn whole_used(x10: u32) -> u32 {
    let half_up = x10.saturating_add(5) / 10;
    if x10 < 1000 && half_up > 99 {
        99
    } else {
        half_up
    }
}
