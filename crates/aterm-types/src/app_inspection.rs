// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared grammar for native tab-app control and semantic inspection (`app/v1`); unknown versions fail closed.

use std::fmt;

/// Frozen version token for native app inspection and actions.
pub const APP_INSPECTION_V1: &str = "app/v1";

const MAX_VIEW_ID_BYTES: usize = 64;
const MAX_UI_KEY_BYTES: usize = 256;
const MAX_ACTION_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 16 * 1024;
const MAX_URI_BYTES: usize = 16 * 1024;

/// Opaque, stable view identifier on the control wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireViewId<'a>(&'a str);

impl<'a> WireViewId<'a> {
    /// Validate and construct an opaque view ID for a response envelope.
    pub fn new(value: &'a str) -> Result<Self, ParseError> {
        Ok(Self(valid_token(value, MAX_VIEW_ID_BYTES, "view id")?))
    }

    /// Return the unmodified wire token.
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

/// Semantic projection requested for one native app view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionProjection {
    /// Semantic reading-order text.
    Text,
    /// Human-readable controls and current values.
    Controls,
    /// The structured semantic node tree.
    Tree,
    /// Renderer-aware text-fit audit for the exact compiled viewport.
    Audit,
}

/// A parsed `inspect app/v1 ...` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectRequest<'a> {
    /// Enumerate windows, tabs, split topology, and every leaf view.
    Tabs,
    /// Read the retained GUI network projection, without refreshing its producers.
    Network,
    /// Inspect one stable native app view.
    View {
        /// Stable target view; never inferred from current focus.
        view: WireViewId<'a>,
        /// Semantic projection to serialize.
        projection: InspectionProjection,
    },
}

/// Subject named by a versioned semantic response envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionSubject<'a> {
    /// Window/tab/split topology enumeration.
    Tabs,
    /// Sanitized metadata from the retained GUI network projection.
    Network,
    /// One stable view and semantic projection.
    View {
        /// Stable target view.
        view: WireViewId<'a>,
        /// Projection serialized by the following response lines.
        projection: InspectionProjection,
    },
}

/// First-line header metadata for a successful `app/v1` semantic response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectionEnvelope<'a> {
    /// Monotone host inspection revision.
    pub revision: u64,
    /// Exact subject represented by the following response body.
    pub subject: InspectionSubject<'a>,
}

impl InspectionEnvelope<'_> {
    /// Serialize the pinned first line of an `app/v1` response.
    #[must_use]
    pub fn header_line(self) -> String {
        match self.subject {
            InspectionSubject::Tabs => format!(
                "app-inspection version={APP_INSPECTION_V1} revision={} subject=tabs",
                self.revision
            ),
            InspectionSubject::Network => format!(
                "app-inspection version={APP_INSPECTION_V1} revision={} subject=network",
                self.revision
            ),
            InspectionSubject::View { view, projection } => format!(
                "app-inspection version={APP_INSPECTION_V1} revision={} subject=view view={} projection={}",
                self.revision,
                view.as_str(),
                projection.keyword(),
            ),
        }
    }
}

/// Validate a successful inspection response against its exact request.
///
/// Only the bounded first-line envelope is parsed here; callers validate the
/// requested body separately. Noncanonical, duplicate or unknown fields fail closed.
///
/// # Errors
/// The bounded header is malformed or does not match the requested version or subject.
pub fn validate_inspection_response<'a>(
    response: &str,
    request: InspectRequest<'a>,
) -> Result<InspectionEnvelope<'a>, ParseError> {
    const MAX_HEADER_BYTES: usize = 512;
    let end = response
        .as_bytes()
        .iter()
        .take(MAX_HEADER_BYTES + 1)
        .position(|byte| *byte == b'\n')
        .unwrap_or(response.len().min(MAX_HEADER_BYTES + 1));
    if end > MAX_HEADER_BYTES {
        return Err(ParseError::InvalidPayload("inspection header"));
    }
    let header = response
        .get(..end)
        .ok_or(ParseError::InvalidPayload("inspection header"))?;
    let revision = header
        .strip_prefix("app-inspection version=app/v1 revision=")
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(revision, _)| revision.parse::<u64>().ok())
        .ok_or(ParseError::InvalidPayload("inspection header"))?;
    let subject = match request {
        InspectRequest::Tabs => InspectionSubject::Tabs,
        InspectRequest::Network => InspectionSubject::Network,
        InspectRequest::View { view, projection } => InspectionSubject::View { view, projection },
    };
    let envelope = InspectionEnvelope { revision, subject };
    if header != envelope.header_line() {
        return Err(ParseError::InvalidPayload("inspection header"));
    }
    Ok(envelope)
}

impl InspectionProjection {
    /// Return the frozen lowercase wire keyword.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Controls => "controls",
            Self::Tree => "tree",
            Self::Audit => "audit",
        }
    }
}

/// A parsed `act app/v1 view ...` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActRequest<'a> {
    /// Stable target view; never inferred from current focus.
    pub view: WireViewId<'a>,
    /// Stable semantic key within the target view.
    pub ui_key: &'a str,
    /// Controller action token.
    pub action: &'a str,
    /// Optional action value, with surrounding whitespace trimmed.
    pub value: Option<&'a str>,
}

/// A parsed `open app ...` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAppRequest<'a> {
    /// Open read-only Allocation, optionally beside the focused pane.
    Allocation {
        /// Prefer a left/right split; a narrow or shared pane opens a tab.
        split: bool,
    },
    /// Focus or create the native Mail view.
    Mail,
    /// Focus or create the dedicated About native view.
    About,
    /// Focus or create the System State native view.
    SystemState,
    /// Show the window's Workers list, where it has one.
    Workers,
    /// Focus or create the Settings view at `route`.
    Settings {
        /// Absolute Settings route; `/` is the default landing route.
        route: &'a str,
    },
    /// Open or focus the canonical Markdown document requested by `uri`.
    Markdown {
        /// Owner-supplied URI; the host must still canonicalize and authorize it.
        uri: &'a str,
    },
    /// Open or focus the canonical editable document requested by `uri`.
    Editor {
        /// Owner-supplied URI; the host must still canonicalize and authorize it.
        uri: &'a str,
    },
}

impl OpenAppRequest<'_> {}

/// Fail-closed parse error for native app protocol requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The request does not match the frozen grammar.
    Usage(&'static str),
    /// The caller requested a protocol version this build does not implement.
    UnsupportedVersion,
    /// An opaque identity, key, or action token was empty, oversized, or contained whitespace or control bytes.
    InvalidToken(&'static str),
    /// The requested semantic projection is not part of `app/v1`.
    UnknownProjection,
    /// The requested first-party app is not part of the closed v1 set.
    UnknownApp,
    /// A Settings route is not absolute.
    InvalidRoute,
    /// A URI/value payload was empty, oversized, or contained a line break.
    InvalidPayload(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(usage) => write!(f, "usage: {usage}"),
            Self::UnsupportedVersion => f.write_str("unsupported app inspection version"),
            Self::InvalidToken(name) => write!(f, "invalid {name}"),
            Self::UnknownProjection => f.write_str("unknown app inspection projection"),
            Self::UnknownApp => f.write_str("unknown first-party app"),
            Self::InvalidRoute => f.write_str("invalid Settings route"),
            Self::InvalidPayload(name) => write!(f, "invalid {name}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse `inspect` arguments (`app/v1 tabs`, `app/v1 network`, or a view projection).
pub fn parse_inspect(rest: &str) -> Result<InspectRequest<'_>, ParseError> {
    reject_line_break(rest, "inspect payload")?;
    let mut input = rest;
    require_version(take_token(&mut input))?;
    match take_token(&mut input) {
        Some("tabs") if remainder(input).is_empty() => Ok(InspectRequest::Tabs),
        Some("network") if remainder(input).is_empty() => Ok(InspectRequest::Network),
        Some("view") => {
            let raw_view = take_token(&mut input).ok_or(ParseError::Usage(
                "inspect app/v1 view <view-id> <text|controls|tree|audit>",
            ))?;
            let view = WireViewId::new(raw_view)?;
            let projection = match take_token(&mut input) {
                Some("text") => InspectionProjection::Text,
                Some("controls") => InspectionProjection::Controls,
                Some("tree") => InspectionProjection::Tree,
                Some("audit") => InspectionProjection::Audit,
                Some(_) => return Err(ParseError::UnknownProjection),
                None => {
                    return Err(ParseError::Usage(
                        "inspect app/v1 view <view-id> <text|controls|tree|audit>",
                    ));
                }
            };
            if !remainder(input).is_empty() {
                return Err(ParseError::Usage(
                    "inspect app/v1 view <view-id> <text|controls|tree|audit>",
                ));
            }
            Ok(InspectRequest::View { view, projection })
        }
        _ => Err(ParseError::Usage(
            "inspect app/v1 tabs | inspect app/v1 network | inspect app/v1 view <view-id> <text|controls|tree|audit>",
        )),
    }
}

/// Parse `act` arguments (`app/v1 view <view-id> <ui-key> <action> [value]`).
pub fn parse_act(rest: &str) -> Result<ActRequest<'_>, ParseError> {
    reject_line_break(rest, "action payload")?;
    let mut input = rest;
    require_version(take_token(&mut input))?;
    if take_token(&mut input) != Some("view") {
        return Err(ParseError::Usage(
            "act app/v1 view <view-id> <ui-key> <action> [value]",
        ));
    }
    let view = WireViewId::new(required_token(
        &mut input,
        "act app/v1 view <view-id> <ui-key> <action> [value]",
    )?)?;
    let ui_key = valid_token(
        required_token(
            &mut input,
            "act app/v1 view <view-id> <ui-key> <action> [value]",
        )?,
        MAX_UI_KEY_BYTES,
        "ui key",
    )?;
    let action = valid_token(
        required_token(
            &mut input,
            "act app/v1 view <view-id> <ui-key> <action> [value]",
        )?,
        MAX_ACTION_BYTES,
        "action",
    )?;
    let tail = remainder(input);
    let value = if tail.is_empty() {
        None
    } else {
        validate_payload(tail, MAX_VALUE_BYTES, "action value")?;
        Some(tail)
    };
    Ok(ActRequest {
        view,
        ui_key,
        action,
        value,
    })
}

/// Parse `open app` arguments (`about`, `system-state`, `settings [route]`, `markdown <uri>`, or `editor <uri>`).
pub fn parse_open_app(rest: &str) -> Result<OpenAppRequest<'_>, ParseError> {
    reject_line_break(rest, "open payload")?;
    let mut input = rest;
    if take_token(&mut input) != Some("app") {
        return Err(ParseError::Usage(
            "open app <allocation [split]|mail|about|system-state|workers|settings [route]|markdown <uri>|editor <uri>>",
        ));
    }
    match take_token(&mut input) {
        Some("allocation") if matches!(remainder(input), "" | "split") => {
            Ok(OpenAppRequest::Allocation {
                split: remainder(input) == "split",
            })
        }
        Some("about") if remainder(input).is_empty() => Ok(OpenAppRequest::About),
        Some("mail") if remainder(input).is_empty() => Ok(OpenAppRequest::Mail),
        Some("system-state") if remainder(input).is_empty() => Ok(OpenAppRequest::SystemState),
        Some("workers") if remainder(input).is_empty() => Ok(OpenAppRequest::Workers),
        Some("settings") => {
            let route = remainder(input);
            let route = if route.is_empty() { "/" } else { route };
            if route.len() > MAX_UI_KEY_BYTES
                || !route.starts_with('/')
                || route.chars().any(char::is_whitespace)
            {
                return Err(ParseError::InvalidRoute);
            }
            valid_token(route, MAX_UI_KEY_BYTES, "Settings route")?;
            Ok(OpenAppRequest::Settings { route })
        }
        Some("markdown") => {
            let uri = remainder(input);
            validate_payload(uri, MAX_URI_BYTES, "document URI")?;
            Ok(OpenAppRequest::Markdown { uri })
        }
        Some("editor") => {
            let uri = remainder(input);
            validate_payload(uri, MAX_URI_BYTES, "document URI")?;
            Ok(OpenAppRequest::Editor { uri })
        }
        Some(_) => Err(ParseError::UnknownApp),
        None => Err(ParseError::Usage(
            "open app <mail|about|system-state|settings [route]|markdown <uri>|editor <uri>>",
        )),
    }
}

fn require_version(version: Option<&str>) -> Result<(), ParseError> {
    match version {
        Some(APP_INSPECTION_V1) => Ok(()),
        Some(_) => Err(ParseError::UnsupportedVersion),
        None => Err(ParseError::Usage("<verb> app/v1 ...")),
    }
}

fn required_token<'a>(input: &mut &'a str, usage: &'static str) -> Result<&'a str, ParseError> {
    take_token(input).ok_or(ParseError::Usage(usage))
}

fn take_token<'a>(input: &mut &'a str) -> Option<&'a str> {
    *input = input.trim_start_matches(char::is_whitespace);
    if input.is_empty() {
        return None;
    }
    match input.find(char::is_whitespace) {
        Some(end) => {
            let token = &input[..end];
            *input = &input[end..];
            Some(token)
        }
        None => {
            let token = *input;
            *input = "";
            Some(token)
        }
    }
}

fn remainder(input: &str) -> &str {
    input.trim_matches(char::is_whitespace)
}

fn valid_token<'a>(
    token: &'a str,
    max_bytes: usize,
    name: &'static str,
) -> Result<&'a str, ParseError> {
    if token.is_empty()
        || token.len() > max_bytes
        || token
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(ParseError::InvalidToken(name));
    }
    Ok(token)
}

fn validate_payload(payload: &str, max_bytes: usize, name: &'static str) -> Result<(), ParseError> {
    if payload.is_empty()
        || payload.len() > max_bytes
        || payload.contains(['\r', '\n'])
        || payload.contains('\0')
    {
        return Err(ParseError::InvalidPayload(name));
    }
    Ok(())
}

fn reject_line_break(payload: &str, name: &'static str) -> Result<(), ParseError> {
    if payload.contains(['\r', '\n']) || payload.contains('\0') {
        Err(ParseError::InvalidPayload(name))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "app_inspection_tests.rs"]
mod tests;
