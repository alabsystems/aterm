// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `metric` control verb: a process in the aterm window streams a named numeric
//! sample into the app-fed store ([`crate::app_fed`]), shown by the app-fed HUD
//! panel. This is how an AI tool reports input/output token spend, a build reports
//! progress, etc. — `aterm-ctl metric <name> <value>`.
//!
//! Write-class (it mutates shared state); auth (same-uid peer + per-launch token,
//! and the Edge `write-input` scope) is handled by the dispatcher in `control.rs`.
//! On the FOCUSED window the HUD's ~3 fps refresh tick repaints the panel within
//! `HUD_INTERVAL` with no explicit nudge; an unfocused window (whose HUD tick is
//! disarmed) reflects a pushed metric on its next focus/output, which is acceptable
//! for an at-a-glance feed.

/// Longest accepted metric name (bounds the store's key memory).
const MAX_NAME: usize = 32;

/// Record one app-fed sample. Replies `OK <name> <value>` or an `ERR …` line.
pub(crate) fn cmd_metric(rest: &str) -> String {
    let mut it = rest.split_whitespace();
    let (Some(name), Some(valstr), None) = (it.next(), it.next(), it.next()) else {
        return "ERR usage: metric <name> <number>\n".to_string();
    };
    // Reject empties, over-long names, whitespace, AND control characters (ESC, other
    // C0/C1): the name is untrusted input that flows straight into HUD render cells, so
    // a control/escape byte must never reach the composed frame or misalign the band.
    if name.is_empty()
        || name.len() > MAX_NAME
        || name.contains(char::is_whitespace)
        || name.contains(char::is_control)
    {
        return "ERR bad name\n".to_string();
    }
    let Ok(value) = valstr.parse::<f64>() else {
        return "ERR bad value\n".to_string();
    };
    if !value.is_finite() {
        return "ERR bad value\n".to_string();
    }
    crate::app_fed::record(name, value, std::time::Instant::now());
    format!("OK {name} {value}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        // Shares the global app-fed store with `app_fed::tests`; serialize + clear.
        let _g = crate::app_fed::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::app_fed::test_clear();
        assert!(cmd_metric("tokens.in 1234").starts_with("OK tokens.in 1234"));
        assert!(cmd_metric("").starts_with("ERR usage"));
        assert!(cmd_metric("only-name").starts_with("ERR usage"));
        assert!(cmd_metric("a b c").starts_with("ERR usage")); // trailing garbage
        assert!(cmd_metric("name notanumber").starts_with("ERR bad value"));
        assert!(cmd_metric("name nan").starts_with("ERR bad value"));
        let long = "x".repeat(40);
        assert!(cmd_metric(&format!("{long} 1")).starts_with("ERR bad name"));
        // Control/escape bytes in the name are rejected (untrusted → render buffer).
        assert!(cmd_metric("tok\u{1b}[31m 1").starts_with("ERR bad name"));
        assert!(cmd_metric("a\u{7}b 1").starts_with("ERR bad name"));
    }
}
