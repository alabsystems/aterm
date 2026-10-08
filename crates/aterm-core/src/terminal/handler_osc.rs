// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC (Operating System Command) handlers: OSC 0/1/2 titles, 7 (cwd),
//! 8 (hyperlinks), 52 (clipboard), 60/61/62 (xterm feature reporting) and the
//! dispatcher for the rest. OSC 1337 lives in `handler_osc_1337.rs`.

use aterm_codec::base64;

use super::ClipboardSelection;
use super::handler::TerminalHandler;
use super::{
    MAX_CWD_PATH_BYTES, MAX_HYPERLINK_URL_BYTES, MAX_OSC52_QUERY_RESPONSE_BYTES, MAX_TITLE_BYTES,
};

impl TerminalHandler<'_> {
    /// Route an OSC command by number to its handler. Called from
    /// `ActionSink::osc_dispatch` in handler.rs.
    pub(super) fn osc_dispatch_inner(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.is_empty() {
            return;
        }

        let cmd = match std::str::from_utf8(params[0])
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            Some(n) => n,
            None => return,
        };

        match cmd {
            0 => self.set_title(params, true, true),
            1 => self.set_title(params, true, false),
            2 => self.set_title(params, false, true),
            4 => self.handle_osc_4(cap, params),
            19 => self.handle_osc_19(cap, params),
            7 => self.handle_osc_7(params),
            8 => self.handle_osc_8(params),
            // OSC 9: simple desktop notification (Terminal / ConEmu style).
            // Gated by host notification authorization (handler_osc_notify.rs).
            9 => self.handle_osc_9(params),
            10 => self.handle_osc_10_11_12(cap, params, 0),
            11 => self.handle_osc_10_11_12(cap, params, 1),
            12 => self.handle_osc_10_11_12(cap, params, 2),
            // OSC 13-16, 18: mouse and Tektronix colors, ignored.
            13 | 14 | 15 | 16 | 18 => {}
            17 => self.handle_osc_17(cap, params),
            21 => self.handle_osc_21(cap, params),
            52 => self.handle_osc_52(cap, params),
            60..=62 => self.handle_osc_feature_reporting(cap, cmd),
            // OSC 66: kitty text sizing. Accepted and discarded — no host
            // consumes it, and the engine does not render scaled text.
            66 => {}
            // OSC 99: kitty desktop-notification protocol.
            // Gated by host notification authorization (handler_osc_notify.rs).
            99 => self.handle_osc_99(params),
            104 => self.handle_osc_104(params),
            110 => self.reset_dynamic_color(0),
            111 => self.reset_dynamic_color(1),
            112 => self.reset_dynamic_color(2),
            // OSC 113-116, 118: reset mouse/Tektronix colors (unused).
            113 | 114 | 115 | 116 | 118 => {}
            117 => self.reset_selection_background(),
            119 => self.reset_selection_foreground(),
            133 => self.handle_osc_133(params),
            633 => self.handle_osc_633(params),
            // OSC 777: rxvt-unicode `notify` desktop notification.
            // Gated by host notification authorization (handler_osc_notify.rs).
            777 => self.handle_osc_777(params),
            // OSC 1337 (iTerm2): the handler acts only on `File=` inline images;
            // other sub-commands (SetUserVar, …) go through the shell_api/KVP layer.
            1337 => self.handle_osc_1337(params),
            30001 => self.handle_osc_30001(),
            30101 => self.handle_osc_30101(),
            _ => {} // Unknown OSC
        }
    }

    /// Set window title and/or icon name from an OSC title param: OSC 0 sets
    /// both, OSC 1 the icon, OSC 2 the window. A real window-title change bumps
    /// the title epoch hosts poll. Titles are capped at [`MAX_TITLE_BYTES`] and
    /// stripped of control characters (see [`sanitize_title`]).
    pub(super) fn set_title(&mut self, params: &[&[u8]], icon: bool, window: bool) {
        // The parser splits on `;`, so a title containing semicolons arrives
        // across params[1..]: rejoin with ";" (as OSC 7 and OSC 8 do for URIs).
        if params.len() < 2 {
            return;
        }
        let title_bytes = params[1..].join(&b';');
        let text_utf8 = String::from_utf8_lossy(&title_bytes);
        let text = &text_utf8[..text_utf8.floor_char_boundary(MAX_TITLE_BYTES)];
        let sanitized = sanitize_title(text);
        let text = &sanitized;
        if icon {
            self.title.icon = text.as_str().into();
        }
        if window {
            // Bump the title epoch only when the stored value changes, so hosts
            // detect a background tab's title drift with a lock-free load.
            if &*self.title.window != text.as_str() {
                self.title
                    .epoch
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            self.title.window = text.as_str().into();
        }
    }

    /// Store a shell-reported working directory (or clear it). The title epoch
    /// ([`Terminal::title_epoch`](super::Terminal::title_epoch)) bumps only when
    /// the stored value changes: hosts label titleless tabs with the cwd, and a
    /// shell re-reporting the same directory each prompt must not thrash it.
    ///
    /// A path [`reported_cwd_is_storable`] refuses (over-long or containing NUL)
    /// is ignored: the prior cwd stays and nothing is signalled. This is the one
    /// place OSC 7 and OSC 633 `P;Cwd=` store a cwd.
    pub(super) fn store_reported_cwd(&mut self, path: Option<&str>) {
        if path.is_some_and(|path| !reported_cwd_is_storable(path)) {
            return;
        }
        if self.current_working_directory.as_deref() != path {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        *self.current_working_directory = path.map(String::from);
    }

    /// Handle OSC 7 current working directory (`OSC 7 ; file://host/path ST`):
    /// extract and percent-decode the path.
    pub(super) fn handle_osc_7(&mut self, params: &[&[u8]]) {
        // params[0] = "7"; params[1..] = the URI. URIs may contain literal
        // semicolons (RFC 3986 §3.3), which the parser splits: rejoin with ";".
        // A missing, empty or non-UTF-8 URI clears the cwd.
        let uri_bytes = params.get(1..).unwrap_or_default().join(&b';');
        let uri = std::str::from_utf8(&uri_bytes).unwrap_or_default();
        if uri.is_empty() {
            self.store_reported_cwd(None);
            return;
        }

        if let Some((host, path)) = Self::parse_file_uri(uri) {
            // A non-local RFC 8089 host (Windows UNC `file://server/share/dir`,
            // an SSH remote) must reach the embedder: queue it as `//host/path`,
            // which Windows accepts natively. An empty host or "localhost" keeps
            // the bare decoded path. The engine cannot know its own hostname
            // (wasm has none), so the consumer decides whether a named host is
            // local.
            let event_payload = match &host {
                Some(h) => format!("//{h}{path}"),
                None => path.clone(),
            };
            // Reject absurd paths before they reach the osc_events queue and
            // current_working_directory: the parser admits up to MAX_OSC_DATA
            // (8 MiB). 4 KiB (PATH_MAX) covers any real directory, UNC host
            // included. A decoded NUL is refused on the same terms (see
            // `reported_cwd_is_storable`), and not queued.
            if !reported_cwd_is_storable(&event_payload) {
                return;
            }
            // Queue the real parsed cwd (host-preserving) for poll-based hosts.
            self.queue_osc_event(7, event_payload);
            // current_working_directory and command marks keep the plain decoded
            // path: shells often report their own hostname for a local cwd, and
            // local-path consumers (spawn-in-cwd, the `cwd` verb) need a real
            // local directory.
            self.store_reported_cwd(Some(&path));
        }
        // A non-file:// URI leaves the cwd unchanged.
    }

    /// Parse a file:// URI into (host, path). The path is percent-decoded; the
    /// host is returned verbatim (decoding could inject a `/`), normalized to
    /// `None` for an empty host or `localhost`. `None` if not a file:// URI.
    fn parse_file_uri(uri: &str) -> Option<(Option<String>, String)> {
        // file://hostname/path or file:///path: the path starts at the first '/'.
        let rest = uri.strip_prefix("file://")?;
        let path_start = rest.find('/')?;
        let host = &rest[..path_start];
        let host = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            None
        } else {
            Some(host.to_string())
        };
        let encoded_path = &rest[path_start..];

        // Total decoder: a malformed `%` escape passes through verbatim, invalid
        // UTF-8 decodes to U+FFFD.
        Some((host, aterm_codec::pct::decode(encoded_path)))
    }

    /// Handle OSC 8 hyperlinks (`OSC 8 ; params ; URI ST`). `params` are
    /// optional `:`-separated key=value pairs (e.g. `id=foo`); an empty or
    /// refused URI ends the hyperlink.
    // Accept: a URI whose scheme is host-minted is admitted iff the mint is live;
    // bound by conformance_hyperlink_scheme_cap's action_enabled checks.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "Accept",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub(super) fn handle_osc_8(&mut self, params: &[&[u8]]) {
        // params[0] = "8", params[1] = key=value params (may be empty),
        // params[2] = URI (may be empty or absent when clearing).

        // The URI is params[2..] rejoined with ";": URIs may contain literal
        // semicolons (RFC 3986 §3.3), which the parser splits.
        let uri_bytes = params.get(2..).unwrap_or_default().join(&b';');
        let uri = std::str::from_utf8(&uri_bytes).unwrap_or("");

        // Accept the URL only if it passes all of, in this order (the O(1)
        // length reject and early-exiting scheme check first, so an oversized
        // URI short-circuits before the O(n) scans):
        // - length and scheme allowlist;
        // - no control chars (tab allowed);
        // - no BiDi directional overrides (U+202A-E, U+2066-9; CVE-2021-42574),
        //   which can reorder a displayed URL to spoof its host. Rejected, not
        //   stripped: a sanitized URL is not the one the sender requested.
        //
        // These predicates are the whole acceptance decision. Tagging a cell is
        // not following a link: what a click may open is decided in the host at
        // press time.
        let admitted = !uri.is_empty()
            && uri.len() <= MAX_HYPERLINK_URL_BYTES
            && is_allowed_scheme(uri, self.hyperlink_auth.extra_schemes())
            && uri.chars().all(|c| !c.is_control() || c == '\t')
            && !uri
                .chars()
                .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'));

        if admitted {
            // Parse id from the params field (`id=mylink` or `id=mylink:foo=bar`).
            let id = params
                .get(1)
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|param_str| {
                    param_str.split(':').find_map(|kv| {
                        kv.strip_prefix("id=")
                            .filter(|v| !v.is_empty() && v.len() <= 256)
                    })
                });
            self.transient.current_hyperlink = Some(std::sync::Arc::from(uri));
            self.transient.current_hyperlink_id = id.map(std::sync::Arc::from);
        } else {
            // An empty URI ends the link, and so does a refused one (as VTE
            // does for an over-long URI): a URI names a new link, so keeping
            // the previous one would tag the next text with a destination its
            // program gave other text.
            self.transient.current_hyperlink = None;
            self.transient.current_hyperlink_id = None;
        }
        self.transient.update_has_transient_extras();
    }

    /// Handle OSC 52 clipboard operations.
    pub(super) fn handle_osc_52(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.len() < 2 {
            return;
        }

        // Pc: selection characters such as "c", "p", "cp".
        let selection_str = match std::str::from_utf8(params[1]) {
            Ok(s) => s,
            Err(_) => return,
        };

        // Parse selection characters; empty defaults to clipboard ('c') per
        // xterm. Capped and de-duplicated against a long Pc parameter.
        let mut selections: Vec<ClipboardSelection> = Vec::with_capacity(4);
        if selection_str.is_empty() {
            selections.push(ClipboardSelection::Clipboard);
        } else {
            for c in selection_str.chars() {
                let Some(sel) = ClipboardSelection::from_char(c) else {
                    continue;
                };
                if !selections.contains(&sel) {
                    selections.push(sel);
                    if selections.len() == 12 {
                        break;
                    }
                }
            }
        }

        if selections.is_empty() {
            return;
        }

        let selection_param: String = selections.iter().map(|s| s.to_char()).collect();

        // Pd: "?" queries, empty clears, anything else is base64 to set.
        let data = params.get(2).copied().unwrap_or(&[]);
        if data == b"?" {
            self.handle_osc_52_query(cap, &selections, &selection_param);
        } else if data.is_empty() {
            self.handle_osc_52_clear(&selections);
        } else {
            self.handle_osc_52_set(&selections, data);
        }
    }

    /// Handle OSC 52 clipboard query (Pd = "?").
    ///
    /// Gated by both `super::clipboard_auth::ClipboardAuth::try_mint_query_capability`
    /// and the `ResponseCapability`; without either the callback is not invoked
    /// or no response can be sent. Both tokens are unforgeable outside their
    /// modules (private `_seal: ()`).
    fn handle_osc_52_query(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        selections: &[ClipboardSelection],
        selection_param: &str,
    ) {
        // `None` when the host has not authorized query access (the default):
        // no callback, no response. An installed policy's decision wins over
        // the `authorize_query` bool.
        let Some(token) = self
            .clipboard_auth
            .try_mint_query_capability_with_engine(self.policy.gates())
        else {
            return;
        };
        let Some(content) =
            super::clipboard_auth::invoke_query(&mut self.clipboard.callback, token, selections)
        else {
            return; // Callback returned None or is unwired — don't respond.
        };
        // Cap on decoded bytes; see MAX_OSC52_QUERY_RESPONSE_BYTES doc for wire-size notes.
        if content.len() > MAX_OSC52_QUERY_RESPONSE_BYTES {
            return;
        }
        // Response: `OSC 52 ; Pc ; <base64> <terminator>`, with the request's
        // terminator (BEL vs ST) for programs that only recognize BEL. Encode
        // fails only above aterm_codec::MAX_INPUT_LEN (64 MiB), far past the
        // content cap; fail closed by skipping the response.
        let Ok(encoded) = base64::encode(content.as_bytes()) else {
            return;
        };
        let terminator = if self.transient.last_osc_bel_terminated {
            "\x07"
        } else {
            "\x1b\\"
        };
        let response = format!("\x1b]52;{selection_param};{encoded}{terminator}");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle OSC 52 clipboard clear (empty Pd). Gated by the same
    /// [`super::clipboard_auth::ClipboardWriteCapability`] as set (see
    /// [`super::clipboard_auth::invoke_clear`]): clear is a strictly less
    /// dangerous subset of set.
    fn handle_osc_52_clear(&mut self, selections: &[ClipboardSelection]) {
        // An installed policy's OSC 52 set rule gates clear too.
        let Some(token) = self
            .clipboard_auth
            .try_mint_write_capability_with_engine(self.policy.gates())
        else {
            return;
        };
        super::clipboard_auth::invoke_clear(&mut self.clipboard.callback, token, selections);
    }

    /// Handle OSC 52 clipboard set (Pd = base64 data). Gated by
    /// `super::clipboard_auth::ClipboardAuth::try_mint_write_capability`:
    /// without a host-minted [`super::clipboard_auth::ClipboardWriteCapability`]
    /// (unforgeable outside `clipboard_auth.rs`) no PTY bytes reach the host
    /// clipboard.
    fn handle_osc_52_set(&mut self, selections: &[ClipboardSelection], data: &[u8]) {
        // Mint first, so unauthorized OSC 52 floods skip the base64 decode. An
        // installed policy's decision wins over the legacy bool.
        let Some(token) = self
            .clipboard_auth
            .try_mint_write_capability_with_engine(self.policy.gates())
        else {
            return;
        };

        let Ok(data_str) = std::str::from_utf8(data) else {
            return;
        };
        // O(1) bound on the encoded length before decoding: the parser admits up
        // to 8 MiB. The decoder accepts no whitespace, so
        // `MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4` is exactly the encoded
        // length of a cap-sized payload; `>` drops only payloads that cannot
        // decode within the cap. The post-decode check below is the exact bound.
        if data.len() > MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4 {
            return;
        }
        let Ok(decoded) = base64::decode(data_str) else {
            return;
        };
        if decoded.len() > MAX_OSC52_QUERY_RESPONSE_BYTES {
            return;
        }

        let Ok(content) = String::from_utf8(decoded) else {
            return;
        };

        // Queue the real decoded clipboard string for poll-based hosts (the
        // callback still receives `content` by value below).
        self.queue_osc_event(52, content.clone());

        super::clipboard_auth::invoke_set(&mut self.clipboard.callback, token, selections, content);
    }

    /// Handle OSC 60/61/62 xterm feature reporting (xterm-401).
    ///
    /// - OSC 60: reserved, no response.
    /// - OSC 61: allowWindowOps query; answers `ESC ] 61 ; true ST` since window
    ///   control belongs to the UI layer.
    /// - OSC 62: feature list, `ESC ] 62 ; f1 ; f2 ; ... ST`, from
    ///   `TerminalCapabilities::aterm_capabilities()`.
    ///
    /// See <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>.
    pub(super) fn handle_osc_feature_reporting(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        cmd: u32,
    ) {
        match cmd {
            // OSC 60 is reserved: xterm sends no response.
            60 => {}
            61 => {
                // OSC 61: window operations are the UI layer's, so report all as
                // allowed ("true" rather than xterm's bitmask). Match the request
                // terminator.
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!("\x1b]61;true{st}");
                self.send_response(cap, response.as_bytes());
            }
            62 => {
                // OSC 62: semicolon-separated features from TerminalCapabilities,
                // named after xterm where possible. Match the request terminator.
                use super::types::TerminalCapabilities;
                let features = TerminalCapabilities::aterm_capabilities().feature_list_string();
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!("\x1b]62;{features}{st}");
                self.send_response(cap, response.as_bytes());
            }
            _ => {}
        }
    }
}

/// Whether a shell-reported working directory may be stored and announced.
/// Both bounds mean "ignore the report", not "clear the cwd":
/// - at most [`MAX_CWD_PATH_BYTES`] (PATH_MAX, 4 KiB), so one report cannot pin
///   megabytes in the count-capped cwd/event fields;
/// - no NUL byte: no directory name contains one (OSC 7's decode turns `%00`
///   into `\0`), and the seamless-update wire refuses a cwd holding one.
fn reported_cwd_is_storable(path: &str) -> bool {
    path.len() <= MAX_CWD_PATH_BYTES && !path.contains('\0')
}

/// Strip from a title string: C0 controls except tab, C1 controls, and BiDi
/// override codepoints (U+202A-U+202E, U+2066-U+2069).
///
/// Control characters would cause rendering artifacts and embedded-ESC
/// attacks; BiDi overrides (CVE-2021-42574, "Trojan Source") can reorder a
/// title to spoof a hostname. Titles do not pass through the grid's
/// `BidiSecurity::Strict` filter, so the strip is unconditional, like the one
/// for notifications in `handler_osc_notify.rs`.
pub(super) fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|&c| {
            // Tab stays; other C0 (0x00-0x1F) and all C1 (0x80-0x9F) go.
            if c == '\t' {
                return true;
            }
            let code = c as u32;
            if code <= 0x1F || (0x80..=0x9F).contains(&code) {
                return false;
            }
            // BiDi directional overrides (CVE-2021-42574): LRE, RLE, PDF, LRO,
            // RLO (U+202A-E) and LRI, RLI, FSI, PDI (U+2066-9).
            if matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                return false;
            }
            true
        })
        .collect()
}

/// Whether a URI's scheme is allowed for OSC 8 hyperlinks (case-insensitive).
///
/// An allowlist: `http`, `https`, `mailto`, `sftp`, `tel`. Everything else is
/// refused at parse time, including `ssh:`/`git:` (CVE-2023-51385 class),
/// custom app URL handlers (`slack:`, `vscode:`, …) that could launch native
/// apps, and `javascript:`, `data:`, `file:`, `ftp:`.
///
/// `extra` is the host-minted extension of the allowlist, authorized via
/// `Terminal::authorize_hyperlink_scheme` (e.g. `orca`), stored lowercased and
/// validated at mint time
/// ([`super::hyperlink_auth::HyperlinkAuth::authorize_scheme`]). Empty by
/// default.
#[must_use]
fn is_allowed_scheme(uri: &str, extra: &[Box<str>]) -> bool {
    /// RFC 3986 safe scheme allowlist for OSC 8 hyperlinks.
    const SAFE_SCHEMES: &[&str] = &["http", "https", "mailto", "sftp", "tel"];

    // The RFC 3986 scheme is the run before the first ':'. The shape walk is
    // shared with mint-time validation, so a smuggled spelling (`orca\t`,
    // `ORCA%3A`) can be neither minted nor matched.
    let Some(colon) = uri.find(':') else {
        return false;
    };
    let scheme = &uri[..colon];
    if !super::hyperlink_auth::is_rfc3986_scheme_shape(scheme) {
        return false;
    }
    SAFE_SCHEMES
        .iter()
        .any(|safe| scheme.eq_ignore_ascii_case(safe))
        || extra.iter().any(|e| scheme.eq_ignore_ascii_case(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;
    use aterm_policy::{engine::PolicyEngine, profiles};

    // ---- sanitize_title --------------------------------------

    #[test]
    fn sanitize_title_passes_legitimate_text_through() {
        for text in [
            "Hello World",
            // Tab (0x09) is explicitly allowed.
            "Col1\tCol2",
            // CJK, Arabic and Hebrew: only the nine override codepoints go, and
            // pure-RTL scripts without them are safe.
            "\u{65E5}\u{672C}\u{8A9E}",
            "\u{0627}\u{0644}\u{0639}",
            "\u{05D0}\u{05D1}\u{05D2}",
            // One below / above the U+202A..U+202E and U+2066..U+2069 ranges.
            "a\u{2029}b",
            "a\u{202F}b",
            "a\u{2065}b",
            "a\u{206A}b",
        ] {
            assert_eq!(sanitize_title(text), text, "{text:?}");
        }
    }

    #[test]
    fn sanitize_title_strips_c0_controls() {
        assert_eq!(sanitize_title("a\x00b\x01c"), "abc");
        assert_eq!(sanitize_title("a\x1bESCb"), "aESCb");
        assert_eq!(sanitize_title("a\x0Ab"), "ab"); // LF
        assert_eq!(sanitize_title("a\x0Db"), "ab"); // CR
    }

    #[test]
    fn sanitize_title_strips_c1_controls() {
        assert_eq!(sanitize_title("a\u{0080}b"), "ab");
        assert_eq!(sanitize_title("a\u{009B}31mb"), "a31mb"); // C1 CSI
        assert_eq!(sanitize_title("a\u{009F}b"), "ab");
    }

    #[test]
    fn sanitize_title_strips_all_nine_bidi_overrides_concatenated() {
        // CVE-2021-42574 (Trojan Source): all nine override codepoints.
        let payload = "X\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}Y";
        assert_eq!(sanitize_title(payload), "XY");
    }

    // ---- OSC 0/1/2 end-to-end (Terminal::process) --------------------------

    #[test]
    fn osc_0_title_strips_all_nine_bidi_overrides() {
        let mut term = Terminal::new(24, 80);
        // All 9 codepoints concatenated with surrounding benign text.
        let payload = "X\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}Y";
        let seq = format!("\x1b]0;{payload}\x07");
        term.process(seq.as_bytes());
        assert_eq!(term.title(), "XY", "all 9 BiDi overrides must be stripped");
        assert_eq!(
            term.icon_name(),
            "XY",
            "OSC 0 icon name must also be sanitized"
        );
    }

    #[test]
    fn title_epoch_bumps_on_real_change_not_on_noop() {
        // The title epoch advances on an actual window-title change and stays
        // put on a no-op re-set.
        let mut term = Terminal::new(24, 80);
        let start = term.title_epoch();

        // OSC 2 sets a fresh window title -> epoch advances.
        term.process(b"\x1b]2;first\x07");
        assert_eq!(term.title(), "first");
        let after_first = term.title_epoch();
        assert!(
            after_first > start,
            "title_epoch must increase on a real title change ({start} -> {after_first})"
        );

        // Re-setting the same title is a no-op -> epoch must not move.
        term.process(b"\x1b]2;first\x07");
        assert_eq!(
            term.title_epoch(),
            after_first,
            "title_epoch must NOT advance when the title value is unchanged"
        );

        // A different title (e.g. a `cd` updating the cwd) -> epoch advances.
        term.process(b"\x1b]2;second\x07");
        assert_eq!(term.title(), "second");
        assert!(
            term.title_epoch() > after_first,
            "title_epoch must increase when the title value actually changes"
        );

        // The public set_title API shares the same change-detection semantics.
        let before_api = term.title_epoch();
        term.set_title("second"); // same value -> no bump
        assert_eq!(term.title_epoch(), before_api);
        term.set_title("third"); // new value -> bump
        assert!(term.title_epoch() > before_api);
    }

    #[test]
    fn title_epoch_bumps_on_real_cwd_change_not_on_reprompt() {
        // A titleless tab is labeled with the OSC 7 cwd, so a cwd change bumps
        // the epoch like a title change, while re-reporting the same directory
        // does not.
        let mut term = Terminal::new(24, 80);
        let start = term.title_epoch();

        // First cwd report -> a real label change -> epoch advances.
        term.process(b"\x1b]7;file://localhost/tmp/one\x07");
        assert_eq!(term.current_working_directory(), Some("/tmp/one"));
        let after_one = term.title_epoch();
        assert!(
            after_one > start,
            "title_epoch must increase on a real cwd change ({start} -> {after_one})"
        );

        // Same directory re-reported (every prompt does this) -> no bump.
        term.process(b"\x1b]7;file://localhost/tmp/one\x07");
        assert_eq!(
            term.title_epoch(),
            after_one,
            "title_epoch must NOT advance when the reported cwd is unchanged"
        );

        // `cd` to a different directory -> epoch advances.
        term.process(b"\x1b]7;file://localhost/tmp/two\x07");
        assert_eq!(term.current_working_directory(), Some("/tmp/two"));
        let after_two = term.title_epoch();
        assert!(
            after_two > after_one,
            "title_epoch must increase when the reported cwd actually changes"
        );

        // Clearing the cwd (empty OSC 7) is a label change too: the strip
        // falls back from the cwd to the presentation title / "aterm".
        term.process(b"\x1b]7;\x07");
        assert_eq!(term.current_working_directory(), None);
        assert!(
            term.title_epoch() > after_two,
            "title_epoch must increase when the cwd is cleared"
        );
    }

    #[test]
    fn osc_1_icon_strips_bidi_override() {
        let mut term = Terminal::new(24, 80);
        let payload = "safe\u{2066}evil";
        let seq = format!("\x1b]1;{payload}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.icon_name(),
            "safeevil",
            "U+2066 LRI must be stripped from OSC 1 icon name"
        );
    }

    #[test]
    fn osc_0_invalid_utf8_title_is_lossy_decoded() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]0;Title\x9cMore\x07");
        assert_eq!(
            term.title(),
            "Title\u{FFFD}More",
            "invalid UTF-8 title payload must be lossily decoded"
        );
        assert_eq!(
            term.icon_name(),
            "Title\u{FFFD}More",
            "invalid UTF-8 icon payload must be lossily decoded"
        );

        term.process(b"\x1b]0;Recovery\x07");
        assert_eq!(term.title(), "Recovery");
        assert_eq!(term.icon_name(), "Recovery");
    }

    // ---- OSC 8 URL bidi-override rejection --------------------------

    #[test]
    fn osc_8_url_with_each_of_nine_bidi_overrides_rejected() {
        // CVE-2021-42574: an override can reorder a displayed URL to spoof its
        // host, so each of the nine is rejected outright.
        let codepoints = [
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}',
        ];
        for cp in codepoints {
            let mut term = Terminal::new(24, 80);
            let url = format!("https://example.com/{cp}path");
            let seq = format!("\x1b]8;;{url}\x07");
            term.process(seq.as_bytes());
            assert!(
                term.current_hyperlink().is_none(),
                "OSC 8 URL containing U+{:04X} must be rejected",
                cp as u32
            );
        }
    }

    #[test]
    fn osc_8_refused_uri_ends_the_open_link() {
        // A new URI replaces the open link. One aterm refuses (a scheme off the
        // list, a BiDi override, over the byte cap) ends it as an empty URI
        // does, so the next text never carries the previous destination.
        let big = format!("https://a.example/{}", "a".repeat(MAX_HYPERLINK_URL_BYTES));
        for refused in [
            "file:///etc/passwd",
            "http://attacker\u{202E}example.com",
            &big,
        ] {
            let mut term = Terminal::new(24, 80);
            let seq = format!("\x1b]8;;https://a.example/\x1b\\A\x1b]8;;{refused}\x1b\\B");
            term.process(seq.as_bytes());
            assert_eq!(
                term.hyperlink_at_visible(0, 0).as_deref(),
                Some("https://a.example/")
            );
            assert_eq!(term.hyperlink_at_visible(0, 1), None, "{refused:.40}");
        }
    }

    // ---- OSC 7 / OSC 633-P cwd byte cap ----------------------

    #[test]
    fn osc_7_oversized_cwd_path_not_queued() {
        // A decoded OSC 7 path beyond MAX_CWD_PATH_BYTES (PATH_MAX, 4 KiB) is
        // rejected before it is stored or queued.
        let mut term = Terminal::new(24, 80);

        // Baseline: a normal sub-PATH_MAX directory is accepted and queued.
        term.process(b"\x1b]7;file:///home/user\x07");
        assert_eq!(term.current_working_directory(), Some("/home/user"));
        assert_eq!(
            term.take_osc_event(),
            Some((7, "/home/user".to_string())),
            "a legitimate OSC 7 cwd must still be queued for poll-based hosts"
        );

        // Oversized: decoded path = "/" + big, longer than MAX_CWD_PATH_BYTES.
        // It must leave the prior cwd untouched and queue nothing.
        let big = "a".repeat(MAX_CWD_PATH_BYTES + 1);
        let seq = format!("\x1b]7;file:///{big}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.current_working_directory(),
            Some("/home/user"),
            "oversized OSC 7 path must not overwrite the prior cwd"
        );
        assert!(
            term.take_osc_event().is_none(),
            "oversized OSC 7 path must not be queued"
        );
    }

    #[test]
    fn osc_633_p_oversized_cwd_rejected() {
        // OSC 633 'P' Cwd has the same MAX_CWD_PATH_BYTES cap before storing.
        let mut term = Terminal::new(24, 80);

        // Baseline: a normal cwd is accepted.
        term.process(b"\x1b]633;P;Cwd=/home/user\x07");
        assert_eq!(term.current_working_directory(), Some("/home/user"));

        // Oversized value (> MAX_CWD_PATH_BYTES) leaves the prior cwd untouched.
        let big = "a".repeat(MAX_CWD_PATH_BYTES + 1);
        let seq = format!("\x1b]633;P;Cwd={big}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.current_working_directory(),
            Some("/home/user"),
            "oversized OSC 633 P Cwd must not overwrite the prior cwd"
        );
    }

    /// A percent-encoded NUL (`%00`) decodes to `\0`, which no directory can
    /// contain and the seamless-update wire refuses. Such a report is ignored
    /// like an over-long one: the prior cwd stays and nothing is queued.
    #[test]
    fn osc_7_cwd_with_a_nul_byte_is_ignored() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7;file:///home/user\x07");
        assert_eq!(term.current_working_directory(), Some("/home/user"));
        assert!(
            term.take_osc_event().is_some(),
            "the baseline report queues"
        );
        let epoch = term.title_epoch();

        term.process(b"\x1b]7;file:///tmp/a%00b\x07");
        assert_eq!(
            term.current_working_directory(),
            Some("/home/user"),
            "a cwd containing NUL must not overwrite the prior cwd"
        );
        assert!(
            term.take_osc_event().is_none(),
            "a cwd the engine refuses to store is not announced either"
        );
        assert_eq!(
            term.title_epoch(),
            epoch,
            "an ignored report is not a tab-label change"
        );

        // With no prior cwd, it stays unset rather than becoming "a\0b".
        let mut fresh = Terminal::new(24, 80);
        fresh.process(b"\x1b]7;file:///tmp/a%00b\x07");
        assert_eq!(fresh.current_working_directory(), None);
    }

    /// The NUL refusal lives in `store_reported_cwd`, the one place OSC 7 and
    /// OSC 633 `P;Cwd=` store a cwd, so it holds for any caller. Pinned directly:
    /// the parser already drops C0 NUL inside an OSC string, so OSC 633 cannot
    /// carry one.
    #[test]
    fn the_cwd_store_ignores_a_path_with_a_nul_byte() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]633;P;Cwd=/home/user\x07");
        assert_eq!(term.current_working_directory(), Some("/home/user"));
        let epoch = term.title_epoch();
        {
            let (_parser, mut handler) = term.split_for_process();
            handler.store_reported_cwd(Some("/tmp/a\0b"));
        }
        assert_eq!(
            term.current_working_directory(),
            Some("/home/user"),
            "a cwd containing NUL must not overwrite the prior cwd"
        );
        assert_eq!(term.title_epoch(), epoch);
        // Clearing (`None`) is still honoured — only an unstorable path is ignored.
        {
            let (_parser, mut handler) = term.split_for_process();
            handler.store_reported_cwd(None);
        }
        assert_eq!(term.current_working_directory(), None);
    }

    #[test]
    fn osc_633_e_oversized_commandline_rejected() {
        // OSC 633 'E' (explicit command text) is retained on the mark and block,
        // so it is capped at MAX_COMMANDLINE_BYTES before unescaping/storing.
        let mut term = Terminal::new(24, 80);

        // Prompt-start (A) creates a current block for the commandline to land on.
        term.process(b"\x1b]633;A\x07");

        // Baseline: a normal command line is accepted and stored on the block.
        term.process(b"\x1b]633;E;ls -la\x07");
        assert_eq!(
            term.current_block().and_then(|b| b.commandline.as_deref()),
            Some("ls -la"),
            "a normal OSC 633 E command line is stored on the block"
        );

        // Oversized (> MAX_COMMANDLINE_BYTES) is rejected before storage and
        // leaves the prior command line untouched.
        let big = "a".repeat(crate::terminal::MAX_COMMANDLINE_BYTES + 1);
        let seq = format!("\x1b]633;E;{big}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.current_block().and_then(|b| b.commandline.as_deref()),
            Some("ls -la"),
            "oversized OSC 633 E command line must not overwrite the prior one"
        );
    }

    // ---- OSC 8 length and control-char refusal ---------

    #[test]
    fn osc_8_refuses_oversized_and_control_char_uris() {
        // Valid hyperlinks accepted; control chars and oversized URIs rejected.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com/path\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://example.com/path"),
            "a valid OSC 8 hyperlink is accepted"
        );

        // An oversized URI (beyond MAX_HYPERLINK_URL_BYTES) is rejected.
        let big = "a".repeat(MAX_HYPERLINK_URL_BYTES + 1);
        let seq = format!("\x1b]8;;https://example.com/{big}\x07");
        term.process(seq.as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "oversized OSC 8 URI must be rejected"
        );

        // A control char (DEL, U+007F — is_control(), tab excepted) is rejected.
        let mut term = Terminal::new(24, 80);
        term.process("\x1b]8;;https://example.com/\u{7f}x\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "OSC 8 URI containing a control char must be rejected"
        );
    }

    // ---- OSC 8 host-minted extra schemes --------

    #[test]
    fn osc_8_custom_scheme_rejected_without_host_mint() {
        // A custom app scheme is refused by default, though RFC 3986-valid.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;orca://focus/w1\x07");
        assert!(
            term.current_hyperlink().is_none(),
            "an unminted custom scheme must be rejected at parse time"
        );
    }

    #[test]
    fn osc_8_custom_scheme_accepted_after_authorize_hyperlink_scheme() {
        let mut term = Terminal::new(24, 80);
        assert!(term.authorize_hyperlink_scheme("orca"));
        term.process(b"\x1b]8;;orca://focus/w1\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("orca://focus/w1"),
            "a minted scheme must be accepted"
        );

        // RFC 3986 §3.1: scheme matching is case-insensitive, like the safe list.
        term.process(b"\x1b]8;;\x07");
        term.process(b"\x1b]8;;ORCA://focus/w2\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("ORCA://focus/w2"),
            "extra-scheme matching must be ASCII-case-insensitive"
        );

        // Literal-semicolon URI reconstruction composes with the mint:
        // the OSC parser splits on ';' and handle_osc_8 rejoins params[2..].
        term.process(b"\x1b]8;;\x07");
        term.process(b"\x1b]8;;orca://run/w1?cmd=a;b\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("orca://run/w1?cmd=a;b"),
            "semicolon reconstruction must apply to extra-scheme URIs too"
        );

        // Revoking the mint is the only way back: a minted scheme is admitted
        // for exactly as long as the host keeps it minted.
        term.process(b"\x1b]8;;\x07");
        term.revoke_hyperlink_scheme("orca");
        term.process(b"\x1b]8;;orca://focus/w3\x07");
        assert!(
            term.current_hyperlink().is_none(),
            "a revoked scheme must be refused again"
        );
    }

    #[test]
    fn authorize_hyperlink_scheme_refuses_javascript_data_file() {
        let mut term = Terminal::new(24, 80);
        for s in ["javascript", "data", "file", "vbscript", "about", "blob"] {
            assert!(
                !term.authorize_hyperlink_scheme(s),
                "never-allow scheme {s:?} must be refused even when the host asks"
            );
        }
        // Case / trailing-charset evasions refuse identically.
        assert!(!term.authorize_hyperlink_scheme("JavaScript"));
        assert!(!term.authorize_hyperlink_scheme("javascript."));
        // And the gate never saw any of them: a javascript: URI stays refused.
        term.process(b"\x1b]8;;javascript:alert(1)\x07");
        assert!(term.current_hyperlink().is_none());
    }

    #[test]
    fn authorize_hyperlink_scheme_refuses_malformed_scheme_shapes() {
        let mut term = Terminal::new(24, 80);
        // Smuggling shapes: whitespace, percent-encoded colon, empty, digit /
        // `+`/`.`/`-` lead, embedded colon — none may mint.
        for s in [
            "orca\t", "orca ", "ORCA%3A", "", "1orca", "+orca", ".orca", "-orca", "orc:a",
        ] {
            assert!(
                !term.authorize_hyperlink_scheme(s),
                "malformed scheme {s:?} must be refused"
            );
        }
        assert_eq!(term.hyperlink_extra_scheme_count(), 0);
        // A smuggled spelling cannot match at the gate: minting `orca` does not
        // admit a URI whose scheme is `orca%3A`. (C0 chars like `\t` never reach
        // the gate: the parser admits only 0x20-0xFF into OSC strings.)
        assert!(term.authorize_hyperlink_scheme("orca"));
        term.process(b"\x1b]8;;orca%3A://x\x07");
        assert!(
            term.current_hyperlink().is_none(),
            "a scheme with smuggled chars must fail the shared RFC 3986 shape walk"
        );
    }

    #[test]
    fn revoke_hyperlink_scheme_restores_default_allowlist() {
        let mut term = Terminal::new(24, 80);
        assert!(term.authorize_hyperlink_scheme("orca"));
        term.process(b"\x1b]8;;orca://focus/w1\x07");
        assert!(term.current_hyperlink().is_some());
        term.process(b"\x1b]8;;\x07");

        term.revoke_hyperlink_scheme("ORCA"); // case-insensitive removal
        term.process(b"\x1b]8;;orca://focus/w1\x07");
        assert!(
            term.current_hyperlink().is_none(),
            "a revoked extra scheme must be refused again"
        );
        // The hardcoded safe allowlist is untouched throughout.
        term.process(b"\x1b]8;;https://example.com/a\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn osc_8_extra_scheme_still_rejects_bidi_and_control_chars() {
        // The orthogonal URI guards run unchanged for minted schemes: minting
        // `orca` widens only the scheme comparison, never the char filters.
        let mut term = Terminal::new(24, 80);
        assert!(term.authorize_hyperlink_scheme("orca"));
        term.process("\x1b]8;;orca://safe\u{202E}live\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "BiDi override in an extra-scheme URI must still be rejected"
        );
        term.process("\x1b]8;;orca://x/\u{7f}y\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "control char in an extra-scheme URI must still be rejected"
        );
    }

    #[test]
    fn osc_8_extra_scheme_still_capped_by_max_url_bytes() {
        let mut term = Terminal::new(24, 80);
        assert!(term.authorize_hyperlink_scheme("orca"));
        let big = "a".repeat(MAX_HYPERLINK_URL_BYTES + 1);
        let seq = format!("\x1b]8;;orca://x/{big}\x07");
        term.process(seq.as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "the byte cap must apply to extra-scheme URIs unchanged"
        );
    }

    #[test]
    fn reset_clears_extra_schemes_is_a_host_choice() {
        // Extra schemes are host state, like the OSC 8/52/DCS authorizations: an
        // app-triggered DECSTR or RIS does not strip them (that would let an app
        // downgrade host policy). The host clears them with
        // revoke_hyperlink_scheme; a new Terminal starts empty.
        let mut term = Terminal::new(24, 80);
        assert!(term.authorize_hyperlink_scheme("orca"));
        term.process(b"\x1b[!p"); // DECSTR soft reset
        term.process(b"\x1bc"); // RIS full reset
        assert!(
            term.is_hyperlink_scheme_authorized("orca"),
            "extra schemes survive app-triggered resets (host-owned state)"
        );
        term.process(b"\x1b]8;;orca://focus/w1\x07");
        assert!(
            term.current_hyperlink().is_some(),
            "the minted scheme still accepts after reset"
        );
        // A fresh instance (the reattach/rebuild path) has no extra schemes.
        let fresh = Terminal::new(24, 80);
        assert_eq!(fresh.hyperlink_extra_scheme_count(), 0);
    }

    #[test]
    fn osc_52_standard_policy_wildcard_does_not_overgrant_revoked_set() {
        use crate::terminal::ClipboardOperation;
        use std::sync::{Arc, Mutex};

        let mut term = Terminal::new(24, 80);
        let captured = Arc::new(Mutex::new(None::<String>));
        let captured_clone = Arc::clone(&captured);
        term.set_clipboard_callback(move |op| {
            if let ClipboardOperation::Set { content, .. } = op {
                *captured_clone.lock().expect("poisoned") = Some(content);
            }
            None
        });
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

        term.process(b"\x1b]52;c;SGVsbG8=\x07");

        assert_eq!(*captured.lock().expect("poisoned"), None);
    }

    /// Every clipboard the handler admits is answered. The handler caps the
    /// decoded clipboard at `MAX_OSC52_QUERY_RESPONSE_BYTES`, and the base64
    /// response is ~1.34x that, so the response budgets must be sized on the
    /// encoded length or large admitted clipboards produce no reply at all.
    ///
    /// Checked against aterm-policy's `standard()` profile (the one `spawn.rs`
    /// installs) at sizes below, at and above the decoded-size boundary.
    #[test]
    fn osc52_query_answers_every_clipboard_it_admits() {
        for len in [
            1_024,
            49_144,
            49_145,
            64 * 1024 - 1,
            super::super::MAX_OSC52_QUERY_RESPONSE_BYTES,
        ] {
            let mut term = Terminal::new(24, 80);
            term.authorize_clipboard_access(
                crate::terminal::clipboard_auth::ClipboardAccess::Query,
            );
            term.set_clipboard_callback(move |op| match op {
                crate::terminal::ClipboardOperation::Query { .. } => Some("x".repeat(len)),
                _ => None,
            });
            term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

            term.process(b"\x1b]52;c;?\x07");

            let response = term.take_response().unwrap_or_else(|| {
                panic!(
                    "a {len}-byte clipboard is within the handler's own \
                     {}-byte cap, so it must be ANSWERED, not silently dropped",
                    super::super::MAX_OSC52_QUERY_RESPONSE_BYTES
                )
            });
            assert!(
                response.starts_with(b"\x1b]52;c;"),
                "the answer is an OSC 52 reply"
            );
        }
    }

    /// A clipboard above the cap is still refused, so the test above cannot pass
    /// for a handler that answers anything.
    #[test]
    fn osc52_query_still_refuses_a_clipboard_over_the_cap() {
        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(crate::terminal::clipboard_auth::ClipboardAccess::Query);
        let len = super::super::MAX_OSC52_QUERY_RESPONSE_BYTES + 1;
        term.set_clipboard_callback(move |op| match op {
            crate::terminal::ClipboardOperation::Query { .. } => Some("x".repeat(len)),
            _ => None,
        });
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

        term.process(b"\x1b]52;c;?\x07");

        assert!(
            term.take_response().is_none(),
            "a clipboard past the cap is refused"
        );
    }

    #[test]
    fn osc_52_standard_policy_wildcard_does_not_overgrant_revoked_query() {
        let mut term = Terminal::new(24, 80);
        term.set_clipboard_callback(|op| match op {
            crate::terminal::ClipboardOperation::Query { .. } => Some("secret".to_string()),
            _ => None,
        });
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

        term.process(b"\x1b]52;c;?\x07");

        assert!(
            term.take_response().is_none(),
            "standard wildcard Execute must not reopen OSC 52 query"
        );
    }

    #[test]
    fn osc_52_set_pre_decode_bound_rejects_oversized_without_decoding() {
        // OSC 52 set bounds the encoded length before base64::decode, so an
        // authorized oversized payload is not decoded only to be dropped.
        use crate::terminal::ClipboardAccess;
        use crate::terminal::ClipboardOperation;
        use std::sync::{Arc, Mutex};

        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(ClipboardAccess::Write);
        let captured = Arc::new(Mutex::new(None::<String>));
        let captured_clone = Arc::clone(&captured);
        term.set_clipboard_callback(move |op| {
            if let ClipboardOperation::Set { content, .. } = op {
                *captured_clone.lock().expect("poisoned") = Some(content);
            }
            None
        });

        // `threshold` is exactly the encoded length of a cap-sized payload.
        let threshold = MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4;
        // Valid base64 one full group over the threshold: it would decode (to
        // > 64 KiB of NULs), so this tests the early reject, not a decode error.
        let oversized = "A".repeat(threshold + 4);
        assert!(oversized.len() > threshold);
        assert_eq!(oversized.len() % 4, 0, "valid base64 length (no padding)");

        let seq = format!("\x1b]52;c;{oversized}\x07");
        term.process(seq.as_bytes());

        assert_eq!(
            *captured.lock().expect("poisoned"),
            None,
            "over-cap encoded payload must be rejected pre-decode"
        );
        assert!(
            term.take_osc_event().is_none(),
            "over-cap OSC 52 set must not queue a decoded clipboard event"
        );
    }

    #[test]
    fn osc_52_set_pre_decode_bound_passes_exact_cap_payload() {
        // The threshold is `>`, not `>=`: a payload decoding to exactly
        // MAX_OSC52_QUERY_RESPONSE_BYTES passes both the pre-decode bound and the
        // post-decode cap.
        use crate::terminal::ClipboardAccess;

        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(ClipboardAccess::Write);

        // Exactly-cap content of NUL bytes (valid UTF-8). 65536 = 21845*3 + 1,
        // so the standard-alphabet encoding is 21845*4 + 4 = 87384 chars =
        // MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4, i.e. the threshold; the
        // `>` comparison passes it.
        let content = vec![0u8; MAX_OSC52_QUERY_RESPONSE_BYTES];
        let encoded = base64::encode(&content).expect("encode cap-sized payload");
        assert_eq!(encoded.len(), MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4);

        let seq = format!("\x1b]52;c;{encoded}\x07");
        term.process(seq.as_bytes());

        let expected = String::from_utf8(content).expect("NUL bytes are valid UTF-8");
        assert_eq!(
            term.take_osc_event(),
            Some((52, expected)),
            "exactly-cap OSC 52 set must still reach the clipboard"
        );
    }
}
