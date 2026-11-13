// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn inspect_version_and_stable_view_are_explicit() {
    assert_eq!(parse_inspect("app/v1 tabs"), Ok(InspectRequest::Tabs));
    assert_eq!(
        parse_inspect(" app/v1  network "),
        Ok(InspectRequest::Network)
    );
    assert!(parse_inspect("app/v1 network extra").is_err());
    assert_eq!(
        parse_inspect("app/v2 network"),
        Err(ParseError::UnsupportedVersion)
    );
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
fn inspection_response_matches_the_exact_request() {
    for payload in [
        "app/v1 tabs",
        "app/v1 network",
        "app/v1 view view-42 tree",
        "app/v1 view view-42 audit",
    ] {
        let request = parse_inspect(payload).unwrap();
        let subject = match request {
            InspectRequest::Tabs => InspectionSubject::Tabs,
            InspectRequest::Network => InspectionSubject::Network,
            InspectRequest::View { view, projection } => {
                InspectionSubject::View { view, projection }
            }
        };
        let expected = InspectionEnvelope {
            revision: 17,
            subject,
        };
        let header = expected.header_line();
        assert_eq!(
            validate_inspection_response(&format!("{header}\nbody"), request),
            Ok(expected)
        );
        for bad in [
            "ERR refused".to_string(),
            header.replace("app/v1", "app/v2"),
            header.replace("revision=17", "revision=no"),
            header.replace("revision=17 ", ""),
            format!("{header} revision=17"),
            format!("{header} unknown=value"),
            header.replace("subject=", "subject=wrong"),
            "x".repeat(513),
        ] {
            assert!(
                validate_inspection_response(&bad, request).is_err(),
                "{bad}"
            );
        }
    }
    let request = parse_inspect("app/v1 view view-42 tree").unwrap();
    for payload in [
        "app/v1 tabs",
        "app/v1 network",
        "app/v1 view other tree",
        "app/v1 view view-42 audit",
    ] {
        let wrong = parse_inspect(payload).unwrap();
        let subject = match wrong {
            InspectRequest::Tabs => InspectionSubject::Tabs,
            InspectRequest::Network => InspectionSubject::Network,
            InspectRequest::View { view, projection } => {
                InspectionSubject::View { view, projection }
            }
        };
        assert!(
            validate_inspection_response(
                &InspectionEnvelope {
                    revision: 17,
                    subject
                }
                .header_line(),
                request
            )
            .is_err()
        );
    }
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
    assert_eq!(parse_open_app("app about"), Ok(OpenAppRequest::About));
    assert!(parse_open_app("app about /extra").is_err());
    assert!(parse_open_app("app about\nact app/v1").is_err());
    assert_eq!(
        parse_open_app("app system-state"),
        Ok(OpenAppRequest::SystemState)
    );
    assert!(parse_open_app("app system-state /extra").is_err());
    assert_eq!(parse_open_app("app tables"), Ok(OpenAppRequest::Tables));
    assert!(parse_open_app("app tables /extra").is_err());
    assert!(parse_open_app("app system-state\nact app/v1").is_err());
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
