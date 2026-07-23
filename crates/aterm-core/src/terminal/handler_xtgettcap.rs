// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! XTGETTCAP (xterm termcap/terminfo query) handler for the terminal.
//!
//! This module handles XTGETTCAP queries which allow applications to query
//! terminal capabilities in-band via DCS sequences.
//!
//! Supported capabilities:
//! - `TN` / `name`: Terminal name (returns "xterm-256color")
//! - `Co` / `colors`: Number of colors (returns 256)
//! - `RGB`: Direct color support (returns 8:8:8)
//! - `Tc`: True color support (empty value = supported)
//! - `Ms`: OSC 52 clipboard format string
//! - `Ss` / `Se`: Cursor style set/reset (DECSCUSR)
//! - `smkx` / `ks`: Enter keypad transmit mode
//! - `rmkx` / `ke`: Exit keypad transmit mode
//! - `op`: Reset foreground/background to defaults
//! - `AF` / `setaf`: Set ANSI foreground (256-color parameterized)
//! - `AB` / `setab`: Set ANSI background (256-color parameterized)
//! - `sgr0` / `me`: Reset all attributes
//! - `bel`: Bell character
//! - `cr`: Carriage return
//! - `smcup` / `ti`: Enter alt screen
//! - `rmcup` / `te`: Exit alt screen
//! - Keyboard keys: `kbs`, `kdch1`/`kD`, `kcuu1`/`ku`, `kcud1`/`kd`,
//!   `kcuf1`/`kr`, `kcub1`/`kl`, `khome`/`kh`, `kend`/`@7`,
//!   `kpp`/`kP`, `knp`/`kN`, `kich1`/`kI`
//! - Function keys: `kf1`-`kf12` / `k1`-`k;`,`F1`,`F2`
//!
//! Protocol:
//! - Request: `DCS + q Pt ST` (Pt = hex-encoded names separated by `;`)
//! - Success: `DCS 1 + r Pt ST` (Pt = hex-encoded name=value pairs)
//! - Invalid: `DCS 0 + r ST` (all names unknown or invalid hex encoding)

use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;

/// Maximum response size in bytes to prevent amplification attacks.
/// 4096 accommodates all supported capabilities in a single query,
/// including large terminfo string values like setaf/setab when hex-encoded.
const MAX_RESPONSE_SIZE: usize = 4096;

impl TerminalHandler<'_> {
    /// Handle XTGETTCAP (xterm termcap/terminfo query).
    ///
    /// Format: `DCS + q Pt ST`
    /// - Pt is hex-encoded list of capability names separated by `;`
    ///
    /// Response: `DCS <status> + r Pt ST`
    /// - status = 1 for valid request with known capabilities
    /// - status = 0 for invalid request (all names unknown or invalid hex)
    pub(super) fn handle_xtgettcap(&mut self, cap: &ResponseCapability) {
        // Decode hex-encoded names
        let hex_names = &self.dcs.data;

        // Split on `;` and decode each name
        let mut response_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut total_size = 0;

        for hex_name in hex_names.split(|&b| b == b';') {
            if hex_name.is_empty() {
                continue;
            }

            // Decode hex to get the capability name
            let Some(name) = hex_decode(hex_name) else {
                // Invalid hex encoding - send error response
                self.xtgettcap_invalid_response(cap);
                return;
            };

            // Look up the capability — skip unknown names per xterm behavior
            // (xterm responds with known caps and ignores unknown ones in
            // multi-capability queries, rather than aborting the entire query).
            let Some(value) = Self::lookup_capability(&name) else {
                continue;
            };

            // Hex-encode name and value for response
            let hex_name_enc = hex_encode(&name);
            let hex_value_enc = hex_encode(&value);

            // Check response size bound
            // Format: hex_name=hex_value (plus ; separator)
            let pair_size = hex_name_enc.len() + 1 + hex_value_enc.len() + 1;
            if total_size + pair_size > MAX_RESPONSE_SIZE {
                // Response would be too large - truncate
                break;
            }
            total_size += pair_size;

            response_pairs.push((hex_name_enc, hex_value_enc));
        }

        if response_pairs.is_empty() {
            // No valid capabilities requested
            self.xtgettcap_invalid_response(cap);
            return;
        }

        // Build success response: DCS 1 + r Pt ST
        let mut response = Vec::with_capacity(total_size + 8);
        response.extend_from_slice(b"\x1bP1+r");

        for (i, (name, value)) in response_pairs.iter().enumerate() {
            if i > 0 {
                response.push(b';');
            }
            response.extend_from_slice(name);
            response.push(b'=');
            response.extend_from_slice(value);
        }

        response.extend_from_slice(b"\x1b\\");
        self.send_response(cap, &response);
    }

    /// Send invalid/error response for XTGETTCAP.
    fn xtgettcap_invalid_response(&mut self, cap: &ResponseCapability) {
        // DCS 0 + r ST
        self.send_response(cap, b"\x1bP0+r\x1b\\");
    }

    /// Look up a capability by name (termcap or terminfo).
    ///
    /// Returns the value as bytes, or None if unknown.
    fn lookup_capability(name: &[u8]) -> Option<Vec<u8>> {
        match name {
            // Terminal name (termcap: TN, terminfo: name)
            b"TN" | b"name" => Some(b"xterm-256color".to_vec()),

            // Number of colors (termcap: Co, terminfo: colors)
            b"Co" | b"colors" => Some(b"256".to_vec()),

            // Direct color / RGB support (ncurses extension)
            // Value 8:8:8 indicates 8 bits per channel for R, G, B.
            // ncurses-based programs parse this format for truecolor detection (#7466).
            b"RGB" => Some(b"8:8:8".to_vec()),

            // True color support (tmux extension, empty = supported)
            b"Tc" => Some(Vec::new()),

            // OSC 52 clipboard format (Ms terminfo string)
            b"Ms" => Some(b"\x1b]52;%p1%s;%p2%s\x07".to_vec()),

            // Cursor style: set (DECSCUSR) and reset to default
            b"Ss" => Some(b"\x1b[%p1%d q".to_vec()),
            b"Se" => Some(b"\x1b[2 q".to_vec()),

            // Keypad transmit mode: enter (smkx/ks) and exit (rmkx/ke)
            b"smkx" | b"ks" => Some(b"\x1b[?1h\x1b=".to_vec()),
            b"rmkx" | b"ke" => Some(b"\x1b[?1l\x1b>".to_vec()),

            // Reset colors to default (original pair)
            b"op" => Some(b"\x1b[39;49m".to_vec()),

            // Set ANSI foreground color (256-color parameterized)
            b"AF" | b"setaf" => {
                Some(b"\x1b[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m".to_vec())
            }

            // Set ANSI background color (256-color parameterized)
            b"AB" | b"setab" => {
                Some(b"\x1b[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m".to_vec())
            }

            // Reset attributes (terminfo: sgr0)
            b"sgr0" | b"me" => Some(b"\x1b(B\x1b[m".to_vec()),

            // Bell (terminfo: bel)
            b"bel" => Some(b"\x07".to_vec()),

            // Carriage return (terminfo: cr)
            b"cr" => Some(b"\r".to_vec()),

            // Alt screen enter/exit (terminfo: smcup/rmcup)
            b"smcup" | b"ti" => Some(b"\x1b[?1049h\x1b[22;0;0t".to_vec()),
            b"rmcup" | b"te" => Some(b"\x1b[?1049l\x1b[23;0;0t".to_vec()),

            // Keyboard keys (commonly queried by ncurses, tmux, vim)
            b"kbs" => Some(b"\x7f".to_vec()), // backspace
            b"kdch1" | b"kD" => Some(b"\x1b[3~".to_vec()), // delete
            b"kcuu1" | b"ku" => Some(b"\x1bOA".to_vec()), // cursor up
            b"kcud1" | b"kd" => Some(b"\x1bOB".to_vec()), // cursor down
            b"kcuf1" | b"kr" => Some(b"\x1bOC".to_vec()), // cursor right
            b"kcub1" | b"kl" => Some(b"\x1bOD".to_vec()), // cursor left
            b"khome" | b"kh" => Some(b"\x1bOH".to_vec()), // home
            b"kend" | b"@7" => Some(b"\x1bOF".to_vec()), // end
            b"kpp" | b"kP" => Some(b"\x1b[5~".to_vec()), // page up
            b"knp" | b"kN" => Some(b"\x1b[6~".to_vec()), // page down
            b"kich1" | b"kI" => Some(b"\x1b[2~".to_vec()), // insert

            // Function keys (xterm encoding)
            b"kf1" | b"k1" => Some(b"\x1bOP".to_vec()),
            b"kf2" | b"k2" => Some(b"\x1bOQ".to_vec()),
            b"kf3" | b"k3" => Some(b"\x1bOR".to_vec()),
            b"kf4" | b"k4" => Some(b"\x1bOS".to_vec()),
            b"kf5" | b"k5" => Some(b"\x1b[15~".to_vec()),
            b"kf6" | b"k6" => Some(b"\x1b[17~".to_vec()),
            b"kf7" | b"k7" => Some(b"\x1b[18~".to_vec()),
            b"kf8" | b"k8" => Some(b"\x1b[19~".to_vec()),
            b"kf9" | b"k9" => Some(b"\x1b[20~".to_vec()),
            b"kf10" | b"k;" => Some(b"\x1b[21~".to_vec()),
            b"kf11" | b"F1" => Some(b"\x1b[23~".to_vec()),
            b"kf12" | b"F2" => Some(b"\x1b[24~".to_vec()),

            // Unknown capability
            _ => None,
        }
    }
}

/// Decode a hex-encoded byte sequence.
///
/// Returns None if the input contains invalid hex characters or has odd length.
fn hex_decode(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }

    let mut result = Vec::with_capacity(hex.len() / 2);

    for chunk in hex.chunks(2) {
        let high = hex_char_to_nibble(chunk[0])?;
        let low = hex_char_to_nibble(chunk[1])?;
        result.push((high << 4) | low);
    }

    Some(result)
}

/// Convert a hex character to its nibble value.
fn hex_char_to_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Encode bytes as uppercase hex.
fn hex_encode(data: &[u8]) -> Vec<u8> {
    const HEX_CHARS: &[u8] = b"0123456789ABCDEF";

    let mut result = Vec::with_capacity(data.len() * 2);
    for &byte in data {
        result.push(HEX_CHARS[(byte >> 4) as usize]);
        result.push(HEX_CHARS[(byte & 0x0F) as usize]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_decode_valid() {
        assert_eq!(hex_decode(b"544E"), Some(b"TN".to_vec()));
        assert_eq!(hex_decode(b"436F"), Some(b"Co".to_vec()));
        assert_eq!(hex_decode(b"524742"), Some(b"RGB".to_vec()));
    }

    #[test]
    fn test_hex_decode_lowercase() {
        assert_eq!(hex_decode(b"544e"), Some(b"TN".to_vec()));
        assert_eq!(hex_decode(b"436f"), Some(b"Co".to_vec()));
    }

    #[test]
    fn test_hex_decode_invalid() {
        assert_eq!(hex_decode(b"5"), None); // Odd length
        assert_eq!(hex_decode(b"GH"), None); // Invalid chars
        assert_eq!(hex_decode(b"5G"), None); // Invalid char
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(b"TN"), b"544E".to_vec());
        assert_eq!(hex_encode(b"Co"), b"436F".to_vec());
        assert_eq!(hex_encode(b"RGB"), b"524742".to_vec());
        assert_eq!(hex_encode(b""), Vec::<u8>::new());
    }

    #[test]
    fn test_hex_encode_decode_roundtrip() {
        let original = b"xterm-256color";
        let encoded = hex_encode(original);
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, original.to_vec());
    }
}
