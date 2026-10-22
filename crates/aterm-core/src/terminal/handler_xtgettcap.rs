// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! XTGETTCAP: in-band termcap/terminfo queries. The answered capabilities
//! are `lookup_capability`'s table.
//!
//! Protocol:
//! - Request: `DCS + q Pt ST` (Pt = hex-encoded names separated by `;`)
//! - Success: `DCS 1 + r Pt ST` (Pt = hex-encoded name=value pairs)
//! - Invalid: `DCS 0 + r ST` (all names unknown or invalid hex encoding)

use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;

/// Maximum response bytes, against amplification: room for every supported
/// capability in one query, hex-encoded setaf/setab included.
const MAX_RESPONSE_SIZE: usize = 4096;

impl TerminalHandler<'_> {
    /// Answer the XTGETTCAP query accumulated in `dcs.data` (module docs).
    pub(super) fn handle_xtgettcap(&mut self, cap: &ResponseCapability) {
        let hex_names = &self.dcs.data;
        let mut response_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut total_size = 0;

        for hex_name in hex_names.split(|&b| b == b';') {
            if hex_name.is_empty() {
                continue;
            }

            let Some(name) = aterm_codec::hex::decode_vec(hex_name) else {
                self.xtgettcap_invalid_response(cap);
                return;
            };

            // Skip unknown names, as xterm does in multi-capability queries,
            // rather than aborting the whole query.
            let Some(value) = Self::lookup_capability(&name) else {
                continue;
            };

            let hex_name_enc = aterm_codec::hex::encode_upper(&name).into_bytes();
            let hex_value_enc = aterm_codec::hex::encode_upper(&value).into_bytes();

            // `name=value;`: stop before the reply passes MAX_RESPONSE_SIZE.
            let pair_size = hex_name_enc.len() + 1 + hex_value_enc.len() + 1;
            if total_size + pair_size > MAX_RESPONSE_SIZE {
                break;
            }
            total_size += pair_size;

            response_pairs.push((hex_name_enc, hex_value_enc));
        }

        if response_pairs.is_empty() {
            self.xtgettcap_invalid_response(cap);
            return;
        }

        // DCS 1 + r Pt ST
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

    /// The XTGETTCAP error reply, `DCS 0 + r ST`.
    #[inline]
    fn xtgettcap_invalid_response(&mut self, cap: &ResponseCapability) {
        self.send_response(cap, b"\x1bP0+r\x1b\\");
    }

    /// A capability's value by termcap or terminfo name; `None` if unknown.
    fn lookup_capability(name: &[u8]) -> Option<Vec<u8>> {
        match name {
            b"TN" | b"name" => Some(b"xterm-256color".to_vec()),

            b"Co" | b"colors" => Some(b"256".to_vec()),

            // ncurses direct color: 8 bits per channel.
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

            b"sgr0" | b"me" => Some(b"\x1b(B\x1b[m".to_vec()),

            b"bel" => Some(b"\x07".to_vec()),

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

            _ => None,
        }
    }
}
