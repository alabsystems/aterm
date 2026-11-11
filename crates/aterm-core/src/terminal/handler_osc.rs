// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OSC (Operating System Command) handlers.

use aterm_codec::base64;

use super::ClipboardSelection;
use super::handler::TerminalHandler;
use super::{
    MAX_CWD_PATH_BYTES, MAX_HYPERLINK_URL_BYTES, MAX_OSC52_QUERY_RESPONSE_BYTES, MAX_TITLE_BYTES,
};

impl TerminalHandler<'_> {
    /// Route an OSC command by number to its handler.
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
            9 => self.handle_osc_9(params),
            10 => self.handle_osc_10_11_12(cap, params, 0),
            11 => self.handle_osc_10_11_12(cap, params, 1),
            12 => self.handle_osc_10_11_12(cap, params, 2),
            13 | 14 | 15 | 16 | 18 => {}
            17 => self.handle_osc_17(cap, params),
            21 => self.handle_osc_21(cap, params),
            52 => self.handle_osc_52(cap, params),
            60..=62 => self.handle_osc_feature_reporting(cap, cmd),
            66 => {}
            99 => self.handle_osc_99(params),
            104 => self.handle_osc_104(params),
            110 => self.reset_dynamic_color(0),
            111 => self.reset_dynamic_color(1),
            112 => self.reset_dynamic_color(2),
            113 | 114 | 115 | 116 | 118 => {}
            117 => self.reset_selection_background(),
            119 => self.reset_selection_foreground(),
            133 => self.handle_osc_133(params),
            633 => self.handle_osc_633(params),
            777 => self.handle_osc_777(params),
            1337 => self.handle_osc_1337(params),
            7501 => self.handle_osc_7501(params),
            30001 => self.handle_osc_30001(),
            30101 => self.handle_osc_30101(),
            _ => {}
        }
    }

    /// OSC 7501 (`state=<word>:app=<codex|grok-build>[:kind=<k>]`, `state=clear`), the root record
    /// only ([`super::program_state`]), at the batch's injected wall clock.
    #[inline]
    fn handle_osc_7501(&mut self, params: &[&[u8]]) {
        let [_, body] = params else {
            return;
        };
        if let Ok(body) = std::str::from_utf8(body) {
            self.program_state
                .accept_at(body, self.transient.process_wall_ms);
        }
    }

    /// Set window title and/or icon name from an OSC title param.
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

    /// Store a shell-reported working directory (or clear it).
    pub(super) fn store_reported_cwd(&mut self, path: Option<&str>, host: Option<&str>) {
        if path.is_some_and(|path| !reported_cwd_is_storable(path)) {
            return;
        }
        let host = host.filter(|_| path.is_some());
        if self.current_working_directory.as_deref() != path
            || self.shell.cwd_host.as_deref() != host
        {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        *self.current_working_directory = path.map(String::from);
        self.shell.cwd_host = host.map(String::from);
        self.shell.cwd_reports = self.shell.cwd_reports.saturating_add(1);
        self.shell.cwd_carried = false;
    }

    /// Handle OSC 7 current working directory (`OSC 7 ; file://host/path ST`).
    #[inline]
    pub(super) fn handle_osc_7(&mut self, params: &[&[u8]]) {
        self.report_cwd(params.get(1..).unwrap_or_default(), false);
    }

    /// Apply a `file://` cwd report (`uri`), unless the report is unsigned and either unchanged
    /// under a signing shell or [`Self::shell_sits_at_prompt`] is true with no program running.
    pub(super) fn report_cwd(&mut self, uri: &[&[u8]], signed: bool) {
        if signed {
            self.shell.signs_cwd = true;
            self.shell.held_cwd = None;
            self.shell.cwd_before_job = None;
        }
        let Some(mut report) = Self::osc_7_report(uri) else {
            return;
        };
        let unchanged = report.path.as_deref() == self.current_working_directory.as_deref()
            && report.host == self.shell.cwd_host;
        if signed {
            // At every prompt, the folder unchanged: nothing to tell the host.
            if unchanged {
                report.event = None;
            }
            self.apply_cwd_report(report);
        } else if unchanged && self.shell.signs_cwd {
        } else if !self.shell_sits_at_prompt() {
            self.apply_cwd_report(report);
        } else if self.job_runs_past_prompt() {
            if self.shell.cwd_before_job.is_none() {
                self.shell.cwd_before_job = super::shell::CwdReport::of(
                    self.current_working_directory.as_deref(),
                    self.shell.cwd_host.as_deref(),
                );
            }
            self.apply_cwd_report(report);
        } else if !self.shell.signs_cwd {
            self.shell.held_cwd = Some(report);
        }
    }

    /// Whether a foreground or probed in-group job runs past an open prompt, recording any live `program_probe` transition.
    fn job_runs_past_prompt(&mut self) -> bool {
        if self.shell.foreground_job {
            self.shell.probe_job_active = false;
            true
        } else if self.shell.program_probe.is_some() {
            let running = self.shell.program_runs_since_prompt();
            if running != self.shell.probe_job_active {
                self.shell.probe_transitioned = true;
            }
            self.shell.probe_job_active = running;
            running
        } else {
            self.shell.probe_job_active
        }
    }

    /// What an OSC 7 whose URI is `uri` does to the reported cwd; `None` leaves it unchanged.
    fn osc_7_report(uri: &[&[u8]]) -> Option<super::shell::CwdReport> {
        let uri_bytes = uri.join(&b';');
        let uri = std::str::from_utf8(&uri_bytes).unwrap_or_default();
        if uri.is_empty() {
            return super::shell::CwdReport::of(None, None);
        }

        let (host, path) = Self::parse_file_uri(uri)?;
        super::shell::CwdReport::of(Some(&path), host.as_deref())
    }

    /// Apply an OSC 7 report.
    #[inline]
    pub(super) fn apply_cwd_report(&mut self, report: super::shell::CwdReport) {
        if let Some(event) = report.event {
            self.queue_osc_event(7, || event);
        }
        self.store_reported_cwd(report.path.as_deref(), report.host.as_deref());
    }

    /// Parse a file:// URI into (host, path).
    fn parse_file_uri(uri: &str) -> Option<(Option<String>, String)> {
        let rest = uri.strip_prefix("file://")?;
        let path_start = rest.find('/')?;
        let host = &rest[..path_start];
        let host = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            None
        } else {
            Some(host.to_string())
        };
        let encoded_path = &rest[path_start..];

        Some((host, aterm_codec::pct::decode(encoded_path)))
    }

    /// Handle OSC 8 hyperlinks (`OSC 8 ; params ; URI ST`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "Accept",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub(super) fn handle_osc_8(&mut self, params: &[&[u8]]) {
        let uri_bytes = params.get(2..).unwrap_or_default().join(&b';');
        let uri = std::str::from_utf8(&uri_bytes).unwrap_or("");

        let admitted = !uri.is_empty()
            && uri.len() <= MAX_HYPERLINK_URL_BYTES
            && is_allowed_scheme(uri, self.hyperlink_auth.extra_schemes())
            && uri.chars().all(|c| !c.is_control() || c == '\t')
            && !uri
                .chars()
                .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'));

        if admitted {
            let id = params
                .get(1)
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|param_str| {
                    param_str
                        .split(':')
                        .rev()
                        .find_map(|kv| kv.strip_prefix("id="))
                        .filter(|v| !v.is_empty() && v.len() <= 256)
                });
            self.transient.current_hyperlink =
                Some(std::sync::Arc::new(aterm_grid::HyperlinkData {
                    url: std::sync::Arc::from(uri),
                    id: id.map(std::sync::Arc::from),
                }));
        } else {
            self.transient.current_hyperlink = None;
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

        let selection_str = match std::str::from_utf8(params[1]) {
            Ok(s) => s,
            Err(_) => return,
        };

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
    fn handle_osc_52_query(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        selections: &[ClipboardSelection],
        selection_param: &str,
    ) {
        let Some(token) = self
            .clipboard_auth
            .try_mint_query_capability_with_engine(self.policy.gates())
        else {
            return;
        };
        let Some(content) =
            super::clipboard_auth::invoke_query(&mut self.clipboard.callback, token, selections)
        else {
            return;
        };
        if content.len() > MAX_OSC52_QUERY_RESPONSE_BYTES {
            return;
        }
        let Ok(encoded) = base64::encode(content.as_bytes()) else {
            return;
        };
        let terminator = if self.transient.last_osc_bel_terminated {
            "\x07"
        } else {
            "\x1b\\"
        };
        let response = format!("\x1b]52;{selection_param};{encoded}{terminator}");
        self.transient.host_io_ordinal = self.transient.host_io_ordinal.wrapping_add(1);
        self.send_response(cap, response.as_bytes());
    }

    /// Handle OSC 52 clipboard clear ([`super::clipboard_auth::ClipboardWriteCapability`],
    /// [`super::clipboard_auth::invoke_clear`]).
    fn handle_osc_52_clear(&mut self, selections: &[ClipboardSelection]) {
        let Some(token) = self
            .clipboard_auth
            .try_mint_write_capability_with_engine(self.policy.gates())
        else {
            return;
        };
        super::clipboard_auth::invoke_clear(&mut self.clipboard.callback, token, selections);
    }

    /// Handle OSC 52 clipboard set ([`super::clipboard_auth::ClipboardWriteCapability`]).
    fn handle_osc_52_set(&mut self, selections: &[ClipboardSelection], data: &[u8]) {
        let Some(token) = self
            .clipboard_auth
            .try_mint_write_capability_with_engine(self.policy.gates())
        else {
            return;
        };

        let Ok(data_str) = std::str::from_utf8(data) else {
            return;
        };
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

        self.queue_osc_event(52, || content.clone());

        super::clipboard_auth::invoke_set(&mut self.clipboard.callback, token, selections, content);
    }

    /// Handle OSC 60/61/62 xterm feature reporting, replying with the request's terminator.
    pub(super) fn handle_osc_feature_reporting(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        cmd: u32,
    ) {
        match cmd {
            60 => {}
            61 => {
                let st = if self.transient.last_osc_bel_terminated {
                    "\x07"
                } else {
                    "\x1b\\"
                };
                let response = format!("\x1b]61;true{st}");
                self.send_response(cap, response.as_bytes());
            }
            62 => {
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
#[inline]
pub(super) fn reported_cwd_is_storable(path: &str) -> bool {
    path.len() <= MAX_CWD_PATH_BYTES && !path.contains('\0')
}

/// Strip from a title string.
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
            if matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                return false;
            }
            true
        })
        .collect()
}

/// Whether a URI's scheme is allowed for OSC 8 hyperlinks (case-insensitive).
#[must_use]
fn is_allowed_scheme(uri: &str, extra: &[Box<str>]) -> bool {
    /// Schemes admitted without a host mint.
    const SAFE_SCHEMES: &[&str] = &["http", "https", "mailto", "sftp", "tel"];

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
#[path = "handler_osc_tests.rs"]
mod tests;
