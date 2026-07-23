// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared grammar for native tab-app control and semantic inspection.
//!
//! This module deliberately contains no GUI or terminal types. The control
//! dispatcher can therefore classify and parse an app request before attempting
//! to resolve a PTY session, which is required for native-only windows and an
//! empty session pool. The first protocol version is explicitly negotiated as
//! [`APP_INSPECTION_V1`]; unknown versions fail closed.

use std::fmt;

/// Frozen version token for native app inspection and actions.
pub const APP_INSPECTION_V1: &str = "app/v1";

const MAX_VIEW_ID_BYTES: usize = 64;
const MAX_UI_KEY_BYTES: usize = 256;
const MAX_ACTION_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 16 * 1024;
const MAX_URI_BYTES: usize = 16 * 1024;

/// An opaque, stable view identifier as represented on the control wire.
///
/// The host owns the concrete ID type. Keeping the parser opaque prevents this
/// shared protocol crate from coupling stable IDs to a process-local integer
/// representation.
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
    /// One stable view and semantic projection.
    View {
        /// Stable target view.
        view: WireViewId<'a>,
        /// Projection serialized by the following response lines.
        projection: InspectionProjection,
    },
}

/// Metadata that prefixes every successful `app/v1` semantic response.
///
/// The host's monotone `revision` lets a driver detect that topology or app
/// state changed between reads. The header is counted as the first line in the
/// control protocol's ordinary `Lines` framing.
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
            InspectionSubject::View { view, projection } => format!(
                "app-inspection version={APP_INSPECTION_V1} revision={} subject=view view={} projection={}",
                self.revision,
                view.as_str(),
                projection.keyword(),
            ),
        }
    }
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
    /// Optional action value. The remainder is preserved verbatim except for
    /// surrounding ASCII whitespace, so text values may contain spaces.
    pub value: Option<&'a str>,
}

/// First-party app kind accepted by `open app ...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAppKind {
    /// The singleton Settings app.
    Settings,
    /// A Markdown document view.
    Markdown,
    /// An editable document view.
    Editor,
}

/// A parsed `open app ...` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAppRequest<'a> {
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

impl OpenAppRequest<'_> {
    /// Return the request's first-party app kind.
    #[must_use]
    pub const fn kind(self) -> OpenAppKind {
        match self {
            Self::Settings { .. } => OpenAppKind::Settings,
            Self::Markdown { .. } => OpenAppKind::Markdown,
            Self::Editor { .. } => OpenAppKind::Editor,
        }
    }
}

/// Fail-closed parse error for native app protocol requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The request does not match the frozen grammar.
    Usage(&'static str),
    /// The caller requested a protocol version this build does not implement.
    UnsupportedVersion,
    /// An opaque identity/key/action token was empty, oversized, or contained
    /// whitespace/control bytes.
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

/// Parse the arguments following the `inspect` verb.
///
/// Accepted forms are `app/v1 tabs` and
/// `app/v1 view <view-id> <text|controls|tree|audit>`.
pub fn parse_inspect(rest: &str) -> Result<InspectRequest<'_>, ParseError> {
    reject_line_break(rest, "inspect payload")?;
    let mut input = rest;
    require_version(take_token(&mut input))?;
    match take_token(&mut input) {
        Some("tabs") if remainder(input).is_empty() => Ok(InspectRequest::Tabs),
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
            "inspect app/v1 tabs | inspect app/v1 view <view-id> <text|controls|tree|audit>",
        )),
    }
}

/// Parse the arguments following the `act` verb.
///
/// Accepted form: `app/v1 view <view-id> <ui-key> <action> [value]`.
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

/// Parse the arguments following the existing `open` verb when its first token
/// is `app`.
///
/// Accepted forms are `app settings [route]`, `app markdown <uri>`, and
/// `app editor <uri>`. Parsing never grants file authority: the returned URI is
/// still an untrusted request for the host to canonicalize and authorize.
pub fn parse_open_app(rest: &str) -> Result<OpenAppRequest<'_>, ParseError> {
    reject_line_break(rest, "open payload")?;
    let mut input = rest;
    if take_token(&mut input) != Some("app") {
        return Err(ParseError::Usage(
            "open app <settings [route]|markdown <uri>|editor <uri>>",
        ));
    }
    match take_token(&mut input) {
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
            "open app <settings [route]|markdown <uri>|editor <uri>>",
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
mod tests {
    use super::*;

    #[test]
    fn inspect_version_and_stable_view_are_explicit() {
        assert_eq!(parse_inspect("app/v1 tabs"), Ok(InspectRequest::Tabs));
        assert_eq!(
            parse_inspect(" app/v1   view view-42 controls "),
            Ok(InspectRequest::View {
                view: WireViewId("view-42"),
                projection: InspectionProjection::Controls,
            })
        );
        assert_eq!(
            parse_inspect("app/v1 view v tree"),
            Ok(InspectRequest::View {
                view: WireViewId("v"),
                projection: InspectionProjection::Tree,
            })
        );
        assert_eq!(
            parse_inspect("app/v1 view v audit"),
            Ok(InspectRequest::View {
                view: WireViewId("v"),
                projection: InspectionProjection::Audit,
            })
        );
        assert_eq!(
            parse_inspect("app/v2 tabs"),
            Err(ParseError::UnsupportedVersion)
        );
        assert_eq!(
            parse_inspect("app/v1 view v pixels"),
            Err(ParseError::UnknownProjection)
        );
        assert!(parse_inspect("app/v1 view v text extra").is_err());
    }

    #[test]
    fn semantic_response_envelope_pins_version_subject_and_revision() {
        assert_eq!(
            InspectionEnvelope {
                revision: 17,
                subject: InspectionSubject::Tabs,
            }
            .header_line(),
            "app-inspection version=app/v1 revision=17 subject=tabs"
        );
        assert_eq!(
            InspectionEnvelope {
                revision: 18,
                subject: InspectionSubject::View {
                    view: WireViewId::new("view-42").unwrap(),
                    projection: InspectionProjection::Controls,
                },
            }
            .header_line(),
            "app-inspection version=app/v1 revision=18 subject=view view=view-42 projection=controls"
        );
    }

    #[test]
    fn action_targets_view_and_semantic_key_not_focus() {
        let parsed = parse_act("app/v1 view view:9 settings/font_px set 14").unwrap();
        assert_eq!(parsed.view.as_str(), "view:9");
        assert_eq!(parsed.ui_key, "settings/font_px");
        assert_eq!(parsed.action, "set");
        assert_eq!(parsed.value, Some("14"));

        let with_spaces = parse_act("app/v1 view v editor/insert replace hello world").unwrap();
        assert_eq!(with_spaces.value, Some("hello world"));
        assert!(parse_act("app/v1 view v key").is_err());
        assert!(parse_act("app/v1 view v key set\nsecond-request").is_err());
    }

    #[test]
    fn open_request_is_parsed_but_not_canonicalized_or_authorized() {
        assert_eq!(
            parse_open_app("app settings"),
            Ok(OpenAppRequest::Settings { route: "/" })
        );
        assert_eq!(
            parse_open_app("app settings /updates"),
            Ok(OpenAppRequest::Settings { route: "/updates" })
        );
        assert_eq!(
            parse_open_app("app markdown file:///tmp/README.md"),
            Ok(OpenAppRequest::Markdown {
                uri: "file:///tmp/README.md"
            })
        );
        assert_eq!(
            parse_open_app("app editor untitled:Scratch Buffer"),
            Ok(OpenAppRequest::Editor {
                uri: "untitled:Scratch Buffer"
            })
        );
        assert_eq!(
            parse_open_app("app settings updates"),
            Err(ParseError::InvalidRoute)
        );
        assert_eq!(
            parse_open_app("app browser https://example.test"),
            Err(ParseError::UnknownApp)
        );
    }

    #[test]
    fn opaque_tokens_and_payloads_are_bounded_and_line_safe() {
        let long_view = "v".repeat(MAX_VIEW_ID_BYTES + 1);
        assert_eq!(
            parse_inspect(&format!("app/v1 view {long_view} text")),
            Err(ParseError::InvalidToken("view id"))
        );
        assert!(parse_open_app("app markdown file:///a\nact app/v1").is_err());
        assert!(parse_act("app/v1 view v key set \0").is_err());
    }
}
