// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC (Operating System Command) sequence handlers for the terminal.
//!
//! This module contains handlers for various OSC sequences:
//! - OSC 7: Current working directory
//! - OSC 8: Hyperlinks
//! - OSC 52: Clipboard operations
//! - OSC 60/61/62: xterm-401 feature reporting queries
//! - OSC 66: Text sizing (Kitty protocol)
//!
//! OSC 1337 (Terminal) handlers are in `handler_osc_1337.rs`.
//! Extracted from handler.rs as part of #485 (large files refactor).

use aterm_codec::base64;

use super::ClipboardSelection;
use super::handler::TerminalHandler;
use super::{
    MAX_CWD_PATH_BYTES, MAX_HYPERLINK_URL_BYTES, MAX_OSC52_QUERY_RESPONSE_BYTES, MAX_TITLE_BYTES,
};

impl TerminalHandler<'_> {
    /// Inner dispatcher for OSC sequences.
    ///
    /// Routes OSC commands by number to the appropriate handler method.
    /// Called from the `ActionSink::osc_dispatch` trait impl in handler.rs.
    pub(super) fn osc_dispatch_inner(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        if params.is_empty() {
            return;
        }

        // Parse the OSC command number
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
            19 => self.handle_osc_19(params),
            7 => self.handle_osc_7(params),
            8 => self.handle_osc_8(params),
            // OSC 9: simple desktop notification (Terminal / ConEmu style).
            // Gated by host notification authorization (handler_osc_notify.rs).
            9 => self.handle_osc_9(params),
            10 => self.handle_osc_10_11_12(cap, params, 0),
            11 => self.handle_osc_10_11_12(cap, params, 1),
            12 => self.handle_osc_10_11_12(cap, params, 2),
            // OSC 13-16, 18: mouse foreground/background and Tektronix colors.
            // These are defined in xterm but not relevant to modern terminals.
            // Silently ignored (#7555).
            13 | 14 | 15 | 16 | 18 => {}
            17 => self.handle_osc_17(cap, params),
            21 => self.handle_osc_21(cap, params),
            52 => self.handle_osc_52(cap, params),
            60..=62 => self.handle_osc_feature_reporting(cap, cmd),
            66 => self.handle_osc_66(params),
            // OSC 99: kitty desktop-notification protocol.
            // Gated by host notification authorization (handler_osc_notify.rs).
            99 => self.handle_osc_99(params),
            104 => self.handle_osc_104(params),
            110 => self.reset_dynamic_color(0),
            111 => self.reset_dynamic_color(1),
            112 => self.reset_dynamic_color(2),
            // OSC 113-116, 118: reset mouse/Tektronix colors (unused, #7555).
            113 | 114 | 115 | 116 | 118 => {}
            117 => self.reset_selection_background(),
            119 => self.reset_selection_foreground(),
            133 => self.handle_osc_133(params),
            633 => self.handle_osc_633(params),
            // OSC 777: rxvt-unicode `notify` desktop notification.
            // Gated by host notification authorization (handler_osc_notify.rs).
            777 => self.handle_osc_777(params),
            // OSC 1337 (iTerm2 proprietary): the `File=` inline-image sub-command
            // is wired here (handler_osc_1337.rs); every other sub-command
            // (SetUserVar, …) flows through the shell_api/KVP layer, and the
            // handler ignores anything that is not `File=`.
            1337 => self.handle_osc_1337(params),
            30001 => self.handle_osc_30001(),
            30101 => self.handle_osc_30101(),
            _ => {} // Unknown OSC
        }
    }

    /// Set window title and/or icon name from an OSC title param.
    ///
    /// OSC 0 sets both icon and window. OSC 1 sets icon only. OSC 2 sets window only.
    /// The legacy v2 callback fires whenever the window title changes.
    /// The v3 event callback fires for all title changes with the title type.
    /// Titles are capped at [`MAX_TITLE_BYTES`] to prevent unbounded memory growth.
    ///
    /// Control characters (C0: 0x00-0x1F except tab, C1: 0x80-0x9F) are stripped
    /// from title strings before storage to prevent rendering artifacts and
    /// potential security issues (#7588).
    pub(super) fn set_title(&mut self, params: &[&[u8]], icon: bool, window: bool) {
        // Title text starts at params[1]. The VTE parser splits on `;`, so
        // a title containing literal semicolons (e.g. "user@host: /foo;bar")
        // will be split across params[1..]. Reconstruct by joining with ";",
        // matching how OSC 7 and OSC 8 handle URIs with semicolons (#7681).
        let title_bytes: Vec<u8> = if params.len() > 2 {
            let mut combined = params[1].to_vec();
            for extra in &params[2..] {
                combined.push(b';');
                combined.extend_from_slice(extra);
            }
            combined
        } else if let Some(&p) = params.get(1) {
            p.to_vec()
        } else {
            return;
        };
        let text_utf8 = String::from_utf8_lossy(&title_bytes);
        let text = &text_utf8[..text_utf8.floor_char_boundary(MAX_TITLE_BYTES)];
        let sanitized = sanitize_title(text);
        let text = &sanitized;
        if icon {
            self.title.icon = text.as_str().into();
        }
        if window {
            // Bump the title-change epoch only when the stored value actually
            // changes, so a background tab's title/cwd drift is observable by
            // the host UI via a lock-free `title_epoch()` load (no bump on a
            // no-op re-set of the same title).
            if &*self.title.window != text.as_str() {
                self.title
                    .epoch
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            self.title.window = text.as_str().into();
            if let Some(ref mut callback) = self.title.callback {
                callback(text);
            }
        }
        // v3 event callback fires for all title types (icon, window, or both).
        if let Some(ref mut callback) = self.title.event_callback {
            let title_type = match (icon, window) {
                (true, true) => aterm_types::TitleType::WindowAndIcon,
                (true, false) => aterm_types::TitleType::IconOnly,
                (false, true) => aterm_types::TitleType::WindowOnly,
                // Unreachable: at least one of icon/window is always true
                // when set_title is called from OSC dispatch.
                (false, false) => return,
            };
            callback(title_type, text);
        }
    }

    /// Store a shell-reported working directory (or clear it) and keep the
    /// host-visible change signal honest: the title epoch — the lock-free
    /// tab-label drift signal, see [`Terminal::title_epoch`](super::Terminal::title_epoch)
    /// — bumps only when the stored VALUE actually changes, mirroring
    /// `set_title`'s no-op guard. A host UI that labels a titleless tab with
    /// this cwd needs a cwd change to be observable exactly like a title
    /// change, while a shell re-reporting the same directory every prompt
    /// (the OSC 7 steady state) must not thrash the signal. The
    /// `DirectoryChanged` shell callback still fires unconditionally — the
    /// pre-existing contract is that consumers see every report, changed or
    /// not.
    pub(super) fn store_reported_cwd(&mut self, path: Option<&str>) {
        if self.current_working_directory.as_deref() != path {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        *self.current_working_directory = path.map(String::from);
        self.shell_directory_changed(path);
    }

    /// Handle OSC 7 current working directory.
    ///
    /// OSC 7 format: `OSC 7 ; file://hostname/path/to/dir ST`
    ///
    /// The URI is a file:// URL pointing to the current working directory.
    /// We extract and decode the path portion for use by the terminal.
    pub(super) fn handle_osc_7(&mut self, params: &[&[u8]]) {
        // OSC 7 format: OSC 7 ; URI ST
        // params[0] = "7" (the command number, already parsed)
        // params[1..] = URI (file://hostname/path/to/dir)
        // URIs can contain literal semicolons (RFC 3986 §3.3), which the OSC
        // parser splits into separate params. Reconstruct by joining with ";".
        let uri_bytes: Vec<u8> = if params.len() > 2 {
            let mut combined = params[1].to_vec();
            for extra in &params[2..] {
                combined.push(b';');
                combined.extend_from_slice(extra);
            }
            combined
        } else if let Some(&p) = params.get(1) {
            p.to_vec()
        } else {
            // No URI provided - clear CWD
            self.store_reported_cwd(None);
            return;
        };

        let Some(uri) = std::str::from_utf8(&uri_bytes).ok() else {
            // No URI provided - clear CWD
            self.store_reported_cwd(None);
            return;
        };

        if uri.is_empty() {
            // Empty URI - clear CWD
            self.store_reported_cwd(None);
            return;
        }

        // Parse the file:// URI
        if let Some((host, path)) = Self::parse_file_uri(uri) {
            // The RFC 8089 host field is semantically part of the URI: a
            // non-local host (a Windows UNC `file://server/share/dir`, an SSH
            // remote reporting its hostname) must reach the embedder, not be
            // silently discarded. Queue it in the UNC-style `//host/path` form
            // Windows accepts natively (POSIX reserves a leading `//` for
            // network paths too). The RFC 8089 local forms — an empty host and
            // "localhost" — keep the bare decoded path, byte-identical to the
            // historical payload. The engine cannot know its own hostname
            // (wasm has none), so deciding whether a NAMED host is actually
            // local is the consumer's job.
            let event_payload = match &host {
                Some(h) => format!("//{h}{path}"),
                None => path.clone(),
            };
            // Reject absurd paths before they reach the count-capped (but not
            // byte-capped) osc_events queue / current_working_directory, where a
            // callback-based host can retain them indefinitely. The parser admits
            // up to MAX_OSC_DATA (8 MiB) and percent-decoding never shrinks the
            // path, so without this a single OSC 7 could pin megabytes of cwd.
            // 4 KiB (PATH_MAX) is generous for any real directory — a UNC host
            // counts toward the same bound, as it does in a real UNC path. (#7172)
            if event_payload.len() > MAX_CWD_PATH_BYTES {
                return;
            }
            // Queue the REAL parsed cwd (host-preserving) for poll-based hosts.
            self.queue_osc_event(7, event_payload);
            // current_working_directory / the shell callback / command marks keep
            // the PLAIN decoded path: shells commonly report their machine's
            // hostname for a LOCAL cwd (aterm's own integration scripts emit
            // `file://$(hostname)…`), and these local-path consumers (GUI
            // spawn-in-cwd, the control socket's `cwd` verb) must not regress
            // to a `//host/…` string that names no local directory.
            self.store_reported_cwd(Some(&path));
        }
        // If not a valid file:// URI, we leave CWD unchanged
    }

    /// Parse a file:// URI into its (host, path) parts.
    ///
    /// Handles percent-encoding in the path. The host is returned verbatim
    /// (never percent-decoded — decoding could inject a `/` and corrupt the
    /// host/path split; real hostnames are plain reg-names), normalized to
    /// `None` for the RFC 8089 local forms: an empty host (`file:///path`)
    /// and the case-insensitive `localhost`. Returns `None` if not a valid
    /// file:// URI.
    fn parse_file_uri(uri: &str) -> Option<(Option<String>, String)> {
        // Check for file:// prefix
        let rest = uri.strip_prefix("file://")?;

        // The format is file://hostname/path or file:///path (empty hostname)
        // Find the start of the path (first / after hostname)
        let path_start = rest.find('/')?;
        let host = &rest[..path_start];
        let host = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            None
        } else {
            Some(host.to_string())
        };
        let encoded_path = &rest[path_start..];

        // Decode percent-encoding
        Some((host, Self::percent_decode(encoded_path)))
    }

    /// Decode percent-encoded characters in a string.
    ///
    /// Percent-encoded bytes are decoded and interpreted as UTF-8.
    /// Invalid UTF-8 sequences are replaced with the Unicode replacement character.
    fn percent_decode(s: &str) -> String {
        let mut bytes = Vec::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '%' {
                // Try to read two hex digits
                let mut hex = String::with_capacity(2);
                for _ in 0..2 {
                    if let Some(&next) = chars.peek() {
                        if next.is_ascii_hexdigit() {
                            if let Some(hex_digit) = chars.next() {
                                hex.push(hex_digit);
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                if hex.len() == 2 {
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        bytes.push(byte);
                        continue;
                    }
                }
                // Invalid encoding, keep as-is
                bytes.push(b'%');
                bytes.extend(hex.as_bytes());
            } else if c.is_ascii() {
                // ASCII characters go directly as bytes
                bytes.push(c as u8);
            } else {
                // Non-ASCII char already in URL - encode as UTF-8 bytes
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                bytes.extend(encoded.as_bytes());
            }
        }

        // Interpret collected bytes as UTF-8
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Handle OSC 8 hyperlinks.
    ///
    /// OSC 8 format: `OSC 8 ; params ; URI ST`
    /// - params: Optional key=value pairs separated by `:` (e.g., `id=foo:line=42`)
    /// - URI: The hyperlink URL (empty to end hyperlink)
    ///
    /// The params are parsed but currently only stored for potential future use.
    /// The primary function is to set/clear the current hyperlink URL.
    // Accept ⟵ the extra-scheme acceptance decision (deep-links §7): a URI whose
    // scheme is host-minted is admitted IFF the mint is live — bound in Tier-1
    // by conformance_hyperlink_scheme_cap's action_enabled checks.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "Accept",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub(super) fn handle_osc_8(&mut self, params: &[&[u8]]) {
        // OSC 8 format: OSC 8 ; params ; URI ST
        // params[0] = "8" (the command number, already parsed)
        // params[1] = params field (may be empty, contains key=value pairs like id=xxx)
        // params[2] = URI (may be empty to clear hyperlink)
        //
        // Note: Some terminals only send 2 params when clearing (OSC 8 ; ; ST)
        // because the URI is empty. We handle both cases.

        // Get the URI (third+ parameters). URIs can contain literal
        // semicolons (RFC 3986 §3.3), which the OSC parser splits into
        // separate params. Reconstruct by joining params[2..] with ";". (#7412)
        let uri_bytes: Vec<u8> = if params.len() > 2 {
            let mut combined = params[2].to_vec();
            for extra in &params[3..] {
                combined.push(b';');
                combined.extend_from_slice(extra);
            }
            combined
        } else {
            Vec::new()
        };
        let uri = std::str::from_utf8(&uri_bytes).unwrap_or("");

        if uri.is_empty() {
            // Clear hyperlink
            self.transient.current_hyperlink = None;
            self.transient.current_hyperlink_id = None;
            self.transient.update_has_transient_extras();
        } else {
            // Set hyperlink - validate it's a reasonable URL. We don't strictly
            // validate the URL format, but we do ensure it doesn't contain
            // control characters that could cause issues and reject Trojan
            // Source BiDi overrides.
            //
            // Order matters: the O(1) length reject and the early-exiting
            // scheme check come FIRST so an oversized `OSC 8 ; ; <huge>` URI
            // (the parser admits up to MAX_OSC_DATA, 8 MiB) short-circuits
            // before the two O(n) char scans run. AND is commutative and every
            // predicate is side-effect free, so this is behavior-preserving.
            //
            // - `is_control` scan: reject control chars (tab allowed).
            // - BiDi scan (#7958, CVE-2021-42574): reject OSC 8 URLs containing
            //   BiDi directional overrides (U+202A-E, U+2066-9). A URL like
            //   "http://safe.example.com\u{202E}moc.live" visually reorders in
            //   status bars / previews to spoof the destination hostname.
            //   Legitimate URLs never contain these codepoints; reject outright
            //   rather than silently strip (a sanitized URL is not the URL the
            //   sender requested, and dereffing it would be misleading).
            if uri.len() <= MAX_HYPERLINK_URL_BYTES
                && is_allowed_scheme(uri, self.hyperlink_auth.extra_schemes())
                && uri.chars().all(|c| !c.is_control() || c == '\t')
                && !uri
                    .chars()
                    .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
            {
                // CF-014: route through HyperlinkCapability. The capability
                // gate is orthogonal to the scheme-allowlist / BiDi / length
                // checks above — those validate the URI shape; this mints
                // a capability iff the host has authorized OSC 8 at all.
                if let Some(token) = self.hyperlink_auth.try_mint_capability() {
                    // Parse id from params field (key=value pairs separated by ':')
                    // e.g. "id=mylink" or "id=mylink:foo=bar"
                    let id = params
                        .get(1)
                        .and_then(|p| std::str::from_utf8(p).ok())
                        .and_then(|param_str| {
                            param_str.split(':').find_map(|kv| {
                                kv.strip_prefix("id=")
                                    .filter(|v| !v.is_empty() && v.len() <= 256)
                            })
                        });
                    super::hyperlink_auth::invoke_set_hyperlink(
                        &mut *self.transient,
                        token,
                        uri,
                        id,
                    );
                }
            }
            // Invalid URLs are silently ignored (consistent with other terminals)
        }
    }

    /// Handle OSC 52 clipboard operations.
    pub(super) fn handle_osc_52(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[&[u8]],
    ) {
        // OSC 52 requires at least 2 params: the selection target and the data
        if params.len() < 2 {
            return;
        }

        // Parse selection targets (Pc parameter)
        // This is a string of characters like "c", "p", "cp", etc.
        let selection_str = match std::str::from_utf8(params[1]) {
            Ok(s) => s,
            Err(_) => return,
        };

        // Parse selection characters into ClipboardSelection variants
        // Empty selection defaults to clipboard ('c') per xterm.
        //
        // Security: cap + de-dupe selections to avoid unbounded allocation from a maliciously
        // long Pc parameter.
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

        // Get the data parameter (Pd)
        let data = params.get(2).copied().unwrap_or(&[]);

        // Determine the operation based on data content
        if data == b"?" {
            // Query operation - request clipboard content
            self.handle_osc_52_query(cap, &selections, &selection_param);
        } else if data.is_empty() {
            // Clear operation - empty data means clear
            self.handle_osc_52_clear(&selections);
        } else {
            // Set operation - decode base64 and set clipboard
            self.handle_osc_52_set(&selections, data);
        }
    }

    /// Handle OSC 52 clipboard query (Pd = "?").
    ///
    /// **Security (CF-003 + CF-005):** this path is gated by both
    /// [`super::clipboard_auth::ClipboardAuth::try_mint_query_capability`]
    /// (query authorization) and the `ResponseCapability` (response channel
    /// authorization). Without a host-minted `ClipboardQueryCapability`, the
    /// callback is never invoked and no response is emitted. Without a
    /// `ResponseCapability`, the response bytes cannot be sent. Both tokens
    /// are unforgeable outside their respective modules (private `_seal: ()`),
    /// so the parser path has no structural way to bypass either gate.
    fn handle_osc_52_query(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        selections: &[ClipboardSelection],
        selection_param: &str,
    ) {
        // Structural capability check. Returns `None` when the host has
        // not authorized query access (default posture) — the callback
        // is not reached and no PTY response is emitted. Engine-consulting
        // variant (#7994): when a policy is installed, its rule decision
        // wins over the legacy `authorize_query` bool per design §6.3.
        let Some(token) = self.clipboard_auth.try_mint_query_capability_with_engine(
            self.policy_engine.as_ref(),
            aterm_policy::OriginTag::Pty,
        ) else {
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
        // Encode the clipboard content and send response.
        // Response format: OSC 52 ; Pc ; <base64> <terminator>
        // Use the same terminator (BEL vs ST) as the request for compatibility
        // with programs that only recognize BEL-terminated responses (#7548).
        // Encode is fallible only on inputs over aterm_codec::MAX_INPUT_LEN
        // (64 MiB); content is already capped at MAX_OSC52_QUERY_RESPONSE_BYTES
        // above, so this only fires on a misconfigured cap. Fail closed: skip
        // the clipboard response rather than panic on oversized input.
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

    /// Handle OSC 52 clipboard clear (empty Pd).
    ///
    /// **Security (CF-004):** clear is gated by the same
    /// [`super::clipboard_auth::ClipboardWriteCapability`] as *set*. The
    /// policy choice is documented on [`super::clipboard_auth::invoke_clear`]:
    /// clear is a strictly-less-dangerous subset of set (an attacker can
    /// only empty the clipboard, not inject arbitrary content), and
    /// distinguishing the two tokens would make host configuration more
    /// confusing without adding meaningful defense in depth.
    fn handle_osc_52_clear(&mut self, selections: &[ClipboardSelection]) {
        // Engine-consulting variant (#7994): the OSC 52 *set* rule gates
        // *clear* too (per invoke_clear's doc, clear is strictly-less-
        // dangerous than set and shares the write capability).
        let Some(token) = self.clipboard_auth.try_mint_write_capability_with_engine(
            self.policy_engine.as_ref(),
            aterm_policy::OriginTag::Pty,
        ) else {
            return;
        };
        super::clipboard_auth::invoke_clear(&mut self.clipboard.callback, token, selections);
    }

    /// Handle OSC 52 clipboard set (Pd = base64-encoded data).
    ///
    /// **Security (CF-004):** gated by
    /// [`super::clipboard_auth::ClipboardAuth::try_mint_write_capability`].
    /// Without a host-minted [`super::clipboard_auth::ClipboardWriteCapability`],
    /// the callback is never invoked and no PTY-origin bytes reach the
    /// host clipboard delegate. The capability is unforgeable outside
    /// `clipboard_auth.rs` (private `_seal: ()` field), so the parser
    /// path has no structural way to bypass this gate.
    fn handle_osc_52_set(&mut self, selections: &[ClipboardSelection], data: &[u8]) {
        // Mint the capability early. If the host hasn't authorized
        // clipboard write, we skip the expensive base64 decode as well —
        // an attacker blasting ungated OSC 52 ; c ; <huge base64> at the
        // terminal should not burn CPU on decode we'll throw away.
        // Engine-consulting variant (#7994): policy decision wins over
        // the legacy bool per design §6.3.
        let Some(token) = self.clipboard_auth.try_mint_write_capability_with_engine(
            self.policy_engine.as_ref(),
            aterm_policy::OriginTag::Pty,
        ) else {
            return;
        };

        // Decode base64 data
        let data_str = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return, // Invalid UTF-8 in base64, ignore
        };
        // O(1) pre-decode bound on the *encoded* length, mirroring the
        // skip-decode optimization documented above for the unauthorized
        // case. The OSC parser admits up to MAX_OSC_DATA (8 MiB) of payload,
        // so without this check an authorized-yet-oversized blast would
        // allocate ~6 MiB only to be dropped by the post-decode cap below.
        // The decoder rejects any non-alphabet byte (no whitespace
        // tolerance), so encoded length is tightly ~4/3 × decoded length:
        // `MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4` is exactly the encoded
        // length of a MAX_OSC52_QUERY_RESPONSE_BYTES payload, so this `>`
        // drops only payloads that provably cannot decode within the cap and
        // passes every legitimate <=cap clipboard set. The precise
        // post-decode `decoded.len() > MAX_OSC52_QUERY_RESPONSE_BYTES` check
        // below stays as the exact bound.
        if data.len() > MAX_OSC52_QUERY_RESPONSE_BYTES / 3 * 4 + 4 {
            return;
        }
        let decoded = match base64::decode(data_str) {
            Ok(bytes) => bytes,
            Err(_) => return, // Invalid base64, ignore
        };
        if decoded.len() > MAX_OSC52_QUERY_RESPONSE_BYTES {
            return;
        }

        // Convert to UTF-8 string
        let content = match String::from_utf8(decoded) {
            Ok(s) => s,
            Err(_) => return, // Invalid UTF-8, ignore
        };

        // Queue the REAL decoded clipboard string for poll-based hosts (the
        // callback still receives `content` by value below).
        self.queue_osc_event(52, content.clone());

        super::clipboard_auth::invoke_set(&mut self.clipboard.callback, token, selections, content);
    }

    /// Handle OSC 66 - Text sizing (Kitty protocol).
    ///
    /// Format: `OSC 66 ; metadata ; text ST`
    ///
    /// The metadata is a colon-separated list of key=value pairs controlling
    /// text rendering dimensions and alignment.
    ///
    /// # Reference
    ///
    /// <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>
    pub(super) fn handle_osc_66(&mut self, params: &[&[u8]]) {
        // Need at least: OSC code, metadata, text
        if params.len() < 3 {
            return;
        }

        // Parse metadata (second parameter)
        let metadata = match std::str::from_utf8(params[1]) {
            Ok(s) => s,
            Err(_) => return,
        };

        // Collect text (may span multiple params if semicolons in content)
        let text = if params.len() == 3 {
            match std::str::from_utf8(params[2]) {
                Ok(s) => s.to_string(),
                Err(_) => return,
            }
        } else {
            // Reconstruct text with embedded semicolons
            let mut text = String::new();
            for (idx, param) in params[2..].iter().enumerate() {
                if idx > 0 {
                    text.push(';');
                }
                match std::str::from_utf8(param) {
                    Ok(s) => text.push_str(s),
                    Err(_) => return,
                }
            }
            text
        };

        // Parse into operation and invoke callback
        let operation = super::types::TextSizingOperation::parse(metadata, &text);
        if let Some(callback) = self.text_sizing_callback {
            callback(operation);
        }
    }

    /// Handle OSC 60/61/62 - xterm feature reporting (xterm-401).
    ///
    /// These sequences allow applications to query which features the terminal
    /// supports. Introduced in xterm-401 (2025-07-02).
    ///
    /// # OSC Numbers
    ///
    /// - **OSC 60**: Obsolete/reserved, no response sent
    /// - **OSC 61**: Query allowWindowOps - which window manipulation operations are enabled
    /// - **OSC 62**: Query feature list - which terminal features are enabled
    ///
    /// # Response Format
    ///
    /// - OSC 61: `ESC ] 61 ; <value> ST` where value is "true" (all ops allowed)
    /// - OSC 62: `ESC ] 62 ; feature1 ; feature2 ; ... ST`
    ///
    /// # aterm-core Implementation
    ///
    /// Since aterm-core is a library (not a full terminal emulator), we report:
    /// - OSC 61: All window ops allowed (value "true") - actual control is UI layer
    /// - OSC 62: Features from `TerminalCapabilities::aterm_capabilities()`
    ///
    /// # Reference
    ///
    /// See: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
    pub(super) fn handle_osc_feature_reporting(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        cmd: u32,
    ) {
        match cmd {
            60 => {
                // OSC 60 is obsolete/reserved - no response per xterm behavior
            }
            61 => {
                // OSC 61 - allowWindowOps query
                // aterm-core doesn't control window operations (that's UI layer),
                // so we report all operations as allowed.
                //
                // xterm uses numeric bitmask, but "true" is simpler and compatible.
                // Match request terminator per #7548.
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!("\x1b]61;true{st}");
                self.send_response(cap, response.as_bytes());
            }
            62 => {
                // OSC 62 - Feature list query
                // Report features from TerminalCapabilities as semicolon-separated list.
                //
                // Feature names follow xterm conventions where possible.
                // Match request terminator per #7548.
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
            _ => {
                // Unreachable - only called for 60/61/62
            }
        }
    }
}

/// Strip control characters from a title string (#7588, #7958).
///
/// Removes:
/// - C0 controls (0x00-0x1F) except tab (0x09)
/// - C1 controls (0x80-0x9F)
/// - Unicode bidirectional override codepoints (U+202A-U+202E, U+2066-U+2069)
///
/// The control-character strip (#7588) prevents rendering artifacts, line breaks
/// in title bars, and embedded-ESC attacks. The BiDi override strip (#7958,
/// CVE-2021-42574 / "Trojan Source") prevents window-title spoofing where a
/// title like `"OSC]2;safe\u{202E}livemaster.com\u{202C}.ru\x07"` visually
/// reorders to a different apparent hostname in the title bar.
///
/// Title surfaces do not flow through the grid's `BidiSecurity::Strict`
/// filter (handler_write.rs, #7913), so the strip is unconditional here —
/// matching the sibling unconditional strip in `handler_osc_notify.rs` for
/// OSC 9 / Terminal notifications.
fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|&c| {
            // Allow tab (0x09), reject other C0 (0x00-0x1F) and all C1 (0x80-0x9F)
            if c == '\t' {
                return true;
            }
            let code = c as u32;
            // C0 range: 0x00-0x1F
            if code <= 0x1F {
                return false;
            }
            // C1 range: 0x80-0x9F
            if (0x80..=0x9F).contains(&code) {
                return false;
            }
            // BiDi directional overrides (CVE-2021-42574 / Trojan Source).
            // U+202A LRE, U+202B RLE, U+202C PDF, U+202D LRO, U+202E RLO
            // U+2066 LRI, U+2067 RLI, U+2068 FSI, U+2069 PDI
            if matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                return false;
            }
            true
        })
        .collect()
}

/// Check if a URI has a scheme that is allowed for OSC 8 hyperlinks.
///
/// This is an **allowlist** check: only URIs with schemes in the default
/// safe list (`http`, `https`, `mailto`, `sftp`, `tel`) are accepted.
/// Everything else — including `ssh:`/`git:` (rejected since #7989, see
/// below), attacker-registered macOS URL handlers (`slack:`, `zoom:`,
/// `vscode:`, `ms-word:`, `applefeedback:`, arbitrary custom schemes) and
/// dangerous schemes (`javascript:`, `data:`, `file:`, `ftp:`, etc.) — is
/// refused at parse time. Case-insensitive.
///
/// Converted from blocklist (`has_dangerous_scheme`) to allowlist in #7919
/// after F01-4 (HN-P1) demonstrated that attacker-registered URL handlers
/// could slip past the blocklist and launch native apps via `NSWorkspace.open`.
///
/// Since #7989 (CVE-2023-51385 class) `ssh` and `git` are rejected by default;
/// `file`/`ftp` and all custom app schemes are likewise refused.
/// (#7413, #7495, #7700, #7919, #7989)
///
/// `extra` is the HOST-MINTED extension of the allowlist (deep-links §7): the
/// bounded, never-allow-filtered scheme set the embedding host authorized via
/// `Terminal::authorize_hyperlink_scheme` (e.g. `orca`). Entries are stored
/// lowercased and validated at mint time
/// ([`super::hyperlink_auth::HyperlinkAuth::authorize_scheme`]); comparison
/// stays `eq_ignore_ascii_case` like the safe list. An empty slice — the
/// default — is byte-for-byte the pre-extension gate.
#[must_use]
fn is_allowed_scheme(uri: &str, extra: &[Box<str>]) -> bool {
    /// RFC 3986 safe scheme allowlist for OSC 8 hyperlinks.
    const SAFE_SCHEMES: &[&str] = &["http", "https", "mailto", "sftp", "tel"];

    // Extract the RFC 3986 scheme: the run of characters before the first
    // ':'. Anything without the RFC shape (missing colon, empty scheme,
    // digit/space lead, illegal character) is not a valid scheme; the shape
    // walk is SHARED with mint-time validation so a smuggled spelling
    // (`orca\t`, `ORCA%3A`, …) can neither be minted nor matched.
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

    // ---- sanitize_title (#7588, #7958) --------------------------------------

    #[test]
    fn sanitize_title_plain_ascii_unchanged() {
        assert_eq!(sanitize_title("Hello World"), "Hello World");
    }

    #[test]
    fn sanitize_title_tab_preserved() {
        // Tab (0x09) is explicitly allowed.
        assert_eq!(sanitize_title("Col1\tCol2"), "Col1\tCol2");
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
    fn sanitize_title_strips_bidi_overrides_202a_202e() {
        // CVE-2021-42574 / Trojan Source — U+202A..U+202E (LRE/RLE/PDF/LRO/RLO).
        assert_eq!(sanitize_title("safe\u{202A}evil"), "safeevil"); // LRE
        assert_eq!(sanitize_title("safe\u{202B}evil"), "safeevil"); // RLE
        assert_eq!(sanitize_title("safe\u{202C}evil"), "safeevil"); // PDF
        assert_eq!(sanitize_title("safe\u{202D}evil"), "safeevil"); // LRO
        assert_eq!(sanitize_title("safe\u{202E}evil"), "safeevil"); // RLO
    }

    #[test]
    fn sanitize_title_strips_bidi_isolates_2066_2069() {
        // CVE-2021-42574 — U+2066..U+2069 (LRI/RLI/FSI/PDI).
        assert_eq!(sanitize_title("safe\u{2066}evil"), "safeevil"); // LRI
        assert_eq!(sanitize_title("safe\u{2067}evil"), "safeevil"); // RLI
        assert_eq!(sanitize_title("safe\u{2068}evil"), "safeevil"); // FSI
        assert_eq!(sanitize_title("safe\u{2069}evil"), "safeevil"); // PDI
    }

    #[test]
    fn sanitize_title_strips_all_nine_bidi_overrides_concatenated() {
        // Full Trojan Source payload — all 9 override codepoints in one string.
        let payload = "X\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{2066}\u{2067}\u{2068}\u{2069}Y";
        assert_eq!(sanitize_title(payload), "XY");
    }

    #[test]
    fn sanitize_title_preserves_legitimate_unicode() {
        // CJK, Arabic, Hebrew, and other non-override Unicode must pass
        // through — only the 9 explicit-override codepoints are stripped.
        assert_eq!(
            sanitize_title("\u{65E5}\u{672C}\u{8A9E}"),
            "\u{65E5}\u{672C}\u{8A9E}"
        );
        let arabic = "\u{0627}\u{0644}\u{0639}"; // alef lam ain
        assert_eq!(sanitize_title(arabic), arabic);
        // Pure-RTL scripts (without override codepoints) are safe.
        let hebrew = "\u{05D0}\u{05D1}\u{05D2}"; // aleph bet gimel
        assert_eq!(sanitize_title(hebrew), hebrew);
    }

    #[test]
    fn sanitize_title_boundary_codepoints_below_and_above_override_ranges() {
        // U+2029 is one below U+202A — must pass through.
        assert_eq!(sanitize_title("a\u{2029}b"), "a\u{2029}b");
        // U+202F (NARROW NO-BREAK SPACE) is one above U+202E — must pass through.
        assert_eq!(sanitize_title("a\u{202F}b"), "a\u{202F}b");
        // U+2065 is one below U+2066 — must pass through.
        assert_eq!(sanitize_title("a\u{2065}b"), "a\u{2065}b");
        // U+206A is one above U+2069 — must pass through.
        assert_eq!(sanitize_title("a\u{206A}b"), "a\u{206A}b");
    }

    // ---- OSC 0/1/2 end-to-end (Terminal::process) --------------------------

    #[test]
    fn osc_2_title_with_rlo_bidi_override_is_sanitized() {
        // CVE-2021-42574 repro — OSC 2 sets the window title. A title like
        // `"safe\u{202E}moc.livemaster\u{202C}.ru"` visually reorders in the
        // title bar to spoof `safesemaster.com.ru` style displays.
        let mut term = Terminal::new(24, 80);
        let payload = "safe\u{202E}evil.example";
        let seq = format!("\x1b]2;{payload}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.title(),
            "safeevil.example",
            "U+202E must be stripped from OSC 2 window title"
        );
    }

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
        // Regression for the background-tab stale-title gap (#bug19 provider):
        // the per-Terminal title epoch must advance on an actual window-title
        // change and stay put on a no-op re-set, so a host UI can detect a
        // background tab's title/cwd drift with a lock-free epoch compare.
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

        // Re-setting the SAME title is a no-op -> epoch must not move.
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
        // The epoch is the TAB-LABEL drift signal, and a titleless tab is
        // labeled with the OSC 7 cwd — so a cwd change must be observable
        // exactly like a title change, while the shell RE-REPORTING the same
        // directory on every prompt (the OSC 7 steady state) must not thrash
        // the signal into a per-prompt strip refresh.
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

    // ---- OSC 8 URL bidi-override rejection (#7958) --------------------------

    #[test]
    fn osc_8_url_with_rlo_bidi_override_rejected() {
        // CVE-2021-42574 — a crafted URL like
        // "http://safe.example\u{202E}moc.live" reorders in status bars and
        // link previews to spoof a different hostname. Reject outright.
        let mut term = Terminal::new(24, 80);
        term.process("\x1b]8;;http://safe.example\u{202E}moc.live\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "OSC 8 URL containing U+202E must be rejected outright (#7958)"
        );
    }

    #[test]
    fn osc_8_url_with_each_of_nine_bidi_overrides_rejected() {
        // Verify each of the 9 codepoints individually triggers rejection.
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
    fn osc_8_clean_url_still_accepted_after_bidi_filter() {
        // Regression guard: the bidi-override rejection must not break
        // legitimate https:// URLs (which have no overrides).
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com/path\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://example.com/path"),
            "clean URL must remain accepted"
        );
    }

    #[test]
    fn osc_8_url_with_bidi_override_does_not_disturb_prior_hyperlink() {
        // If a valid hyperlink is already set and then an override-carrying
        // URL arrives, the attacker must not be able to clear or replace the
        // prior hyperlink. The invalid URL is silently ignored (matching the
        // existing is_allowed_scheme rejection path) and the prior URL stays.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://safe.example/a\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://safe.example/a")
        );
        term.process("\x1b]8;;http://attacker\u{202E}example.com\x07".as_bytes());
        // Prior hyperlink is preserved — the attacker's URL was invalid and
        // silently dropped, same as an unknown-scheme URL.
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://safe.example/a"),
            "prior valid hyperlink must be preserved when new URL contains BiDi override"
        );
    }

    // ---- OSC 7 / OSC 633-P cwd byte cap (idx12, #7172) ----------------------

    #[test]
    fn osc_7_oversized_cwd_path_not_queued() {
        // OSC 7's percent-decoded path flows into the count-capped (but not
        // byte-capped) osc_events queue and current_working_directory, where a
        // callback-based host can pin it indefinitely. A path beyond
        // MAX_CWD_PATH_BYTES (PATH_MAX, 4 KiB) is rejected before it is stored
        // or queued, so the queue's documented memory bound holds in bytes.
        let mut term = Terminal::new(24, 80);

        // Baseline: a normal sub-PATH_MAX directory is accepted AND queued.
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
            "oversized OSC 7 path must not be queued (it would defeat the \
             osc_events queue's byte bound)"
        );
    }

    #[test]
    fn osc_633_p_oversized_cwd_rejected() {
        // OSC 633 'P' Cwd shares the OSC 7 gap — its value lands in the
        // count-capped current_working_directory / mark / block fields. The
        // same MAX_CWD_PATH_BYTES cap applies before storing it.
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

    #[test]
    fn osc_633_e_oversized_commandline_rejected() {
        // OSC 633 'E' (explicit command text) is RETAINED on the current mark
        // and block (`commandline`), which the host can hold indefinitely. Its
        // sibling 'P' Cwd path already caps at MAX_CWD_PATH_BYTES; 'E' must cap
        // at MAX_COMMANDLINE_BYTES before unescaping/storing so a crafted
        // multi-MiB command line cannot pin memory per retained block (#7172).
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

    // ---- OSC 8 validation order: length/scheme before scans (idx13) ---------

    #[test]
    fn osc_8_length_and_scheme_checked_before_scans() {
        // The O(1) length reject and the early-exiting scheme check now run
        // FIRST in the && chain so an oversized `OSC 8 ; ; <huge>` URI
        // short-circuits before the control-char / BiDi char scans. AND is
        // commutative, so behavior is unchanged: a valid hyperlink is accepted;
        // control chars, BiDi overrides, and oversized URIs are all rejected.

        // A valid hyperlink is still accepted.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com/path\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://example.com/path"),
            "a valid OSC 8 hyperlink must still be accepted"
        );

        // An oversized URI (beyond MAX_HYPERLINK_URL_BYTES) is rejected and
        // leaves the prior hyperlink untouched.
        let big = "a".repeat(MAX_HYPERLINK_URL_BYTES + 1);
        let seq = format!("\x1b]8;;https://example.com/{big}\x07");
        term.process(seq.as_bytes());
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("https://example.com/path"),
            "oversized OSC 8 URI must be rejected (short-circuited before any scan)"
        );

        // A control char (DEL, U+007F — is_control(), tab excepted) is rejected.
        let mut term = Terminal::new(24, 80);
        term.process("\x1b]8;;https://example.com/\u{7f}x\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "OSC 8 URI containing a control char must be rejected"
        );

        // A BiDi override (U+202E RLO) is still rejected (Trojan-Source defense).
        let mut term = Terminal::new(24, 80);
        term.process("\x1b]8;;https://example.com/\u{202E}x\x07".as_bytes());
        assert!(
            term.current_hyperlink().is_none(),
            "OSC 8 URI containing a BiDi override must be rejected"
        );
    }

    // ---- OSC 8 host-minted extra schemes (orca deep-links §7, #4384) --------

    #[test]
    fn osc_8_custom_scheme_rejected_without_host_mint() {
        // The F01-4/#7919 posture holds by default: a custom app scheme is
        // refused at parse time even though it is RFC 3986-valid.
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

        // Literal-semicolon URI reconstruction (#7412) composes with the mint:
        // the OSC parser splits on ';' and handle_osc_8 rejoins params[2..].
        term.process(b"\x1b]8;;\x07");
        term.process(b"\x1b]8;;orca://run/w1?cmd=a;b\x07");
        assert_eq!(
            term.current_hyperlink().map(|s| s.as_ref()),
            Some("orca://run/w1?cmd=a;b"),
            "semicolon reconstruction must apply to extra-scheme URIs too"
        );

        // The CF-014 capability gate still layers on top: revoking OSC 8
        // acceptance wholesale kills minted-scheme links too.
        term.process(b"\x1b]8;;\x07");
        term.revoke_hyperlinks();
        term.process(b"\x1b]8;;orca://focus/w3\x07");
        assert!(
            term.current_hyperlink().is_none(),
            "revoke_hyperlinks must gate extra-scheme URIs as well"
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
        // A smuggled spelling can't match at the gate either: minting `orca`
        // does not admit a URI whose extracted scheme is `orca%3A`. (C0 chars
        // like `\t` never even reach the gate — the OSC parser admits only
        // 0x20-0xFF into OSC strings, so a tab-smuggled scheme dies earlier.)
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
        // The orthogonal URI guards run UNCHANGED for minted schemes: minting
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
        // CHOSEN SEMANTICS (deep-links §7.2): extra schemes are
        // terminal-instance HOST state, like the OSC 8/52/DCS authorizations —
        // an application-triggered soft reset (DECSTR) or full reset (RIS)
        // must NOT strip a host-minted scheme (an app could otherwise silently
        // downgrade host policy). The HOST clears them via
        // revoke_hyperlink_scheme; a rebuilt Terminal starts empty.
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
        // Perf hardening (idx 7): OSC 52 set bounds the *encoded* length
        // before base64::decode, so an authorized-yet-oversized blast cannot
        // allocate ~6 MiB only to be dropped by the post-decode 64 KiB cap.
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
        // A valid-base64 ('A' = 0) payload one full group over the threshold;
        // it WOULD decode (to > 64 KiB of NUL bytes) absent the pre-decode
        // bound, so this guards the early reject rather than a base64 error.
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
        // The threshold is `> cap_encoded_len`, not `>=`, so a payload that
        // decodes to exactly MAX_OSC52_QUERY_RESPONSE_BYTES (the largest
        // legitimate clipboard set) must pass the pre-decode bound AND the
        // post-decode cap. Guards the off-by-one in the threshold formula.
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
