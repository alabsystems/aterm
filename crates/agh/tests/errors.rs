// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Kinds, exits, sub-kinds, the two error lines and GitHub's error body (the plan's §4.6.1).

use std::collections::BTreeSet;

use agh::error::{FieldError, Forbid, GithubBody, HttpRef, IdentityRef, Tried, exit_rows};
use agh::policy::RefusalCode;
use agh::rate::RateSnapshot;
use agh::redact::Redactor;
use agh::wire::WireKind;
use agh::{Error, Kind, Replay, SideEffect, Sub};
use astream_effects::{EffectRecord, ReplayDivergence};

#[test]
fn exits_are_one_to_fifteen_then_130() {
    let exits: Vec<u8> = Kind::ALL.iter().map(|k| k.exit()).collect();
    let mut expected: Vec<u8> = (1..=15).collect();
    expected.push(130);
    assert_eq!(exits, expected);
}

#[test]
fn every_kind_has_its_slug() {
    let slugs: Vec<&str> = Kind::ALL.iter().map(|k| k.slug()).collect();
    assert_eq!(
        slugs,
        [
            "refused",
            "usage",
            "could_not_run",
            "unauthenticated",
            "forbidden",
            "not_found",
            "not_visible",
            "pending",
            "conflict",
            "invalid",
            "rate_limited",
            "server",
            "outcome_unknown",
            "incomplete",
            "failed",
            "interrupted",
        ]
    );
    for kind in Kind::ALL {
        assert_eq!(kind.to_string(), kind.slug());
    }
}

#[test]
fn the_exit_table_is_ok_then_every_kind() {
    let rows = exit_rows();
    assert_eq!(rows.len(), 17);
    let first = rows[0];
    assert_eq!(
        (first.exit, first.slug, first.meaning, first.next),
        (0, "ok", "done", "continue")
    );
    for (row, kind) in rows[1..].iter().zip(Kind::ALL) {
        assert!(!kind.meaning().is_empty());
        assert!(!kind.next_action().is_empty());
        assert_eq!(
            (row.exit, row.slug, row.meaning, row.next),
            (kind.exit(), kind.slug(), kind.meaning(), kind.next_action())
        );
    }
}

#[test]
fn exactly_five_kinds_are_retriable() {
    let retriable: Vec<Kind> = Kind::ALL.into_iter().filter(|k| k.retriable()).collect();
    assert_eq!(
        retriable,
        [
            Kind::CouldNotRun,
            Kind::Pending,
            Kind::RateLimited,
            Kind::Server,
            Kind::OutcomeUnknown
        ]
    );
}

#[test]
fn the_gh_exit_table() {
    for kind in Kind::ALL {
        let (with, without) = match kind {
            Kind::Unauthenticated => (4, 4),
            Kind::Pending => (8, 1),
            Kind::Interrupted => (2, 2),
            _ => (1, 1),
        };
        assert_eq!(kind.gh_exit(true), with, "{kind}");
        assert_eq!(kind.gh_exit(false), without, "{kind}");
    }
}

#[test]
fn sub_slugs() {
    let cases: Vec<(Sub, Option<&str>)> = vec![
        (Sub::None, None),
        (
            Sub::Forbid(Forbid::Sso {
                url: Some("https://github.com/orgs/fixture-dev/sso".into()),
            }),
            Some("forbid.sso"),
        ),
        (
            Sub::Forbid(Forbid::DraftsUnproven),
            Some("forbid.drafts_unproven"),
        ),
        (
            Sub::Forbid(Forbid::Permission { accepted: None }),
            Some("forbid.permission"),
        ),
        (Sub::Forbid(Forbid::NoPush), Some("forbid.no_push")),
        (Sub::Forbid(Forbid::Blocked), Some("forbid.blocked")),
        (Sub::Forbid(Forbid::Other), Some("forbid.other")),
        (Sub::Primary, Some("primary")),
        (Sub::Secondary, Some("secondary")),
        (Sub::PreRefused, Some("pre_refused")),
        (Sub::AlreadyExists, Some("already_exists")),
        (Sub::Moved { to: "x".into() }, Some("moved")),
        (Sub::AmbiguousTag { ids: vec![1, 2] }, Some("ambiguous_tag")),
        (
            Sub::DigestMismatch {
                expected: "a".into(),
                got: "b".into(),
            },
            Some("digest_mismatch"),
        ),
        (
            Sub::SizeMismatch {
                expected: 1,
                got: 2,
            },
            Some("size_mismatch"),
        ),
        (
            Sub::NotUploaded {
                state: "open".into(),
            },
            Some("not_uploaded"),
        ),
        (Sub::Torn { page: 2 }, Some("torn")),
        (Sub::Capped { pages: 20 }, Some("capped")),
        (Sub::TruncatedTree, Some("truncated_tree")),
        (Sub::DirectoryCap, Some("directory_cap")),
        (Sub::CompareCap, Some("compare_cap")),
        (Sub::SearchCap, Some("search_cap")),
        (Sub::Wire(WireKind::Idle), Some("wire.idle")),
        (Sub::Protocol, Some("protocol")),
        (Sub::Local, Some("local")),
        (
            Sub::Policy(RefusalCode::WriterAbsent),
            Some("policy.writer_absent"),
        ),
        (Sub::Replay(Replay::RequestDiverged), Some("replay.request")),
    ];
    for (sub, slug) in cases {
        assert_eq!(sub.slug().as_deref(), slug, "{sub:?}");
    }
}

#[test]
fn replay_of_each_divergence() {
    let cases = [
        (
            ReplayDivergence::Exhausted { expected: "clock" },
            Replay::Exhausted,
        ),
        (
            ReplayDivergence::Unreached {
                position: 3,
                found: EffectRecord::Mono(1),
            },
            Replay::Unconsumed,
        ),
        (
            ReplayDivergence::KindMismatch {
                expected: "clock",
                found: EffectRecord::Mono(1),
            },
            Replay::KindMismatch,
        ),
        (
            ReplayDivergence::PathDiverged {
                recorded: "a".into(),
                asked: "b".into(),
            },
            Replay::PathDiverged,
        ),
        (
            ReplayDivergence::DoorDiverged {
                recorded: "gh".into(),
                asked: "sleep".into(),
            },
            Replay::DoorDiverged,
        ),
        (
            ReplayDivergence::RequestDiverged {
                door: "gh".into(),
                recorded: b"a".to_vec(),
                asked: b"b".to_vec(),
            },
            Replay::RequestDiverged,
        ),
        (
            ReplayDivergence::OutputDiverged {
                door: "stdout".into(),
                recorded: b"a".to_vec(),
                emitted: b"b".to_vec(),
            },
            Replay::OutputDiverged,
        ),
        (
            ReplayDivergence::Unrecorded {
                door: "gh".into(),
                request: b"a".to_vec(),
            },
            Replay::Unrecorded,
        ),
    ];
    for (divergence, replay) in cases {
        assert_eq!(Replay::of(&divergence), replay, "{divergence:?}");
    }
}

#[test]
fn replay_slugs_and_kinds() {
    let all = [
        (Replay::Exhausted, "exhausted"),
        (Replay::KindMismatch, "kind"),
        (Replay::PathDiverged, "path"),
        (Replay::DoorDiverged, "door"),
        (Replay::RequestDiverged, "request"),
        (Replay::OutputDiverged, "output"),
        (Replay::Unrecorded, "unrecorded"),
        (Replay::Argv, "argv"),
        (Replay::Unconsumed, "unconsumed"),
        (Replay::BodyNotTaped, "body_not_taped"),
        (Replay::StreamNotTaped, "stream_not_taped"),
        (Replay::Format, "format"),
    ];
    for (replay, slug) in all {
        assert_eq!(replay.slug(), slug);
        let error = Error::replay(replay, "the test's why");
        assert_eq!(error.sub, Sub::Replay(replay));
        if replay == Replay::Argv {
            assert_eq!((error.kind, error.exit()), (Kind::Usage, 2));
        } else {
            assert_eq!((error.kind, error.exit()), (Kind::CouldNotRun, 3));
        }
    }
}

/// A `GithubBody` holding all three permission headers.
fn permission_body() -> GithubBody {
    GithubBody::default().with_headers(&[
        (
            "X-Accepted-GitHub-Permissions".into(),
            "contents=read".into(),
        ),
        ("X-OAuth-Scopes".into(), "repo".into()),
        ("X-Accepted-OAuth-Scopes".into(), "admin:public_key".into()),
    ])
}

/// An error with every optional part set.
fn full_error() -> Error {
    Error::new(Kind::Forbidden, "the test's why")
        .with_sub(Sub::Forbid(Forbid::Sso {
            url: Some("https://github.com/orgs/fixture-dev/sso".into()),
        }))
        .with_next("the test's next")
        .with_repo("fixture-dev/packs")
        .with_identity(IdentityRef::Profile {
            name: "dev".into(),
            fingerprint: "0123456789ab".into(),
        })
        .with_http(HttpRef {
            status: 403,
            method: "GET",
            path: "/repos/fixture-dev/packs".into(),
            request_id: Some("FAKE:7".into()),
        })
        .with_reset(1_760_003_600)
        .with_retry_after(30)
        .with_github(permission_body())
}

#[test]
fn the_line_of_a_full_error() {
    let line = full_error().line("release.view", &Redactor::new());
    assert_eq!(
        line,
        "agh: forbidden exit=5 verb=release.view repo=fixture-dev/packs as=dev \
         http=\"403 GET /repos/fixture-dev/packs\" req=FAKE:7 sub=forbid.sso reset=1760003600 \
         retry_after=30 side_effect=none accepted=\"contents=read\" scopes=\"repo\" \
         accepted_scopes=\"admin:public_key\" why=\"the test's why\" next=\"the test's next\""
    );
}

#[test]
fn the_line_of_a_minimal_error() {
    let line = Error::usage("the test's why")
        .with_next("the test's next")
        .line("-", &Redactor::new());
    assert_eq!(
        line,
        "agh: usage exit=2 verb=- side_effect=none why=\"the test's why\" next=\"the test's next\""
    );
}

#[test]
fn the_line_escapes() {
    let line = Error::usage("a\"b\\c\nd\re\tf\u{1}g").line("-", &Redactor::new());
    assert!(line.contains(r#"why="a\"b\\c\nd\re\tf\x01g""#), "{line}");
}

#[test]
fn the_object_has_its_nineteen_keys() {
    let value = full_error().to_json(&Redactor::new());
    let object = value
        .get("error")
        .and_then(|e| e.as_object())
        .expect("an error object");
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "as",
        "documentation_url",
        "errors",
        "exit",
        "kind",
        "message",
        "next",
        "rate",
        "repo",
        "request_id",
        "retriable",
        "retry_after",
        "reset",
        "side_effect",
        "status",
        "sub",
        "tried",
        "v",
        "why",
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected);
    assert_eq!(object.get("v").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(object.get("exit").and_then(|v| v.as_u64()), Some(5));
    assert_eq!(
        object.get("sub").and_then(|v| v.as_str()),
        Some("forbid.sso")
    );
    assert_eq!(
        object.get("side_effect").and_then(|v| v.as_str()),
        Some(SideEffect::None.slug())
    );
}

#[test]
fn each_tried_form() {
    let error = Error::refused("the test's why").with_tried(vec![
        Tried::Sent {
            as_: "dev".into(),
            status: 404,
        },
        Tried::Probe {
            path: "repos/fixture-dev/packs".into(),
            status: 200,
        },
        Tried::Skipped {
            as_: "release".into(),
            why: "absent".into(),
        },
    ]);
    let value = error.to_json(&Redactor::new());
    let tried = value
        .get("error")
        .and_then(|e| e.get("tried"))
        .and_then(|t| t.as_array())
        .expect("tried");
    assert_eq!(tried.len(), 3);
    assert_eq!(tried[0].get("as").and_then(|v| v.as_str()), Some("dev"));
    assert_eq!(tried[0].get("status").and_then(|v| v.as_u64()), Some(404));
    assert_eq!(
        tried[1].get("probe").and_then(|v| v.as_str()),
        Some("repos/fixture-dev/packs")
    );
    assert_eq!(tried[1].get("status").and_then(|v| v.as_u64()), Some(200));
    assert_eq!(tried[2].get("as").and_then(|v| v.as_str()), Some("release"));
    assert_eq!(tried[2].get("why").and_then(|v| v.as_str()), Some("absent"));
}

#[test]
fn a_token_in_a_why_is_redacted_everywhere() {
    let token = agh_fake::token("e");
    let error = Error::could_not_run(format!("it held {token}"));
    let redactor = Redactor::new();
    let line = error.line("-", &redactor);
    let json = error.to_json(&redactor).to_string();
    let display = error.to_string();
    for text in [&line, &json, &display] {
        assert!(!text.contains(&token), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }
}

#[test]
fn github_body_parse() {
    assert_eq!(GithubBody::parse(b"<html>"), GithubBody::default());
    let long = format!("{} {}", "m".repeat(500), "m".repeat(99));
    let body = format!(
        r#"{{"message":"{long}","errors":["plain",{{"code":"already_exists","field":"tag_name","resource":"Release"}}]}}"#
    );
    let parsed = GithubBody::parse(body.as_bytes());
    assert_eq!(
        parsed.message.as_ref().map(|m| m.chars().count()),
        Some(500)
    );
    assert_eq!(
        parsed.errors[0],
        FieldError {
            message: Some("plain".into()),
            ..FieldError::default()
        }
    );
    assert_eq!(parsed.errors[1].code.as_deref(), Some("already_exists"));
}

/// A 40-character learned value with no token prefix, from two fake tokens' hex.
fn unprefixed(seed: &str) -> String {
    let head = agh_fake::token(seed);
    let tail = agh_fake::token(&format!("{seed}-tail"));
    format!(
        "{}{}",
        &head["ghp_".len()..],
        &tail["ghp_".len().."ghp_".len() + 4]
    )
}

#[test]
fn a_learned_value_across_the_message_cap_leaves_none_of_its_bytes() {
    let value = unprefixed("cap");
    assert_eq!(value.len(), 40);
    let message = format!("{} {value} tail", "x".repeat(479));
    let body = format!(r#"{{"message":"{message}"}}"#);
    let parsed = GithubBody::parse(body.as_bytes());
    let kept = parsed.message.clone().expect("a message");
    assert_eq!(kept, "x".repeat(479));
    let redactor = Redactor::new();
    redactor.learn(value.as_bytes());
    let json = Error::could_not_run("x")
        .with_github(parsed)
        .to_json(&redactor)
        .to_string();
    assert!(!json.contains(&value[..20]), "{json}");
}

#[test]
fn the_rate_resource_is_redacted_in_the_json() {
    let value = unprefixed("rate");
    let redactor = Redactor::new();
    redactor.learn(value.as_bytes());
    let rate = RateSnapshot {
        resource: value.clone(),
        limit: 60,
        remaining: 0,
        reset: 1_760_000_060,
        used: 60,
    };
    let json = Error::could_not_run("x").with_rate(rate).to_json(&redactor);
    let resource = json
        .get("error")
        .and_then(|e| e.get("rate"))
        .and_then(|r| r.get("resource"))
        .and_then(|r| r.as_str());
    assert_eq!(resource, Some("[redacted]"));
    assert!(!json.to_string().contains(&value), "{json}");
}

#[test]
fn github_body_with_headers() {
    let long = "p".repeat(300);
    let body = GithubBody::default().with_headers(&[
        (
            "x-accepted-github-permissions".into(),
            "contents=read".into(),
        ),
        (
            "X-ACCEPTED-GITHUB-PERMISSIONS".into(),
            "metadata=read".into(),
        ),
        ("X-OAuth-Scopes".into(), long),
    ]);
    assert_eq!(body.accepted_permissions.as_deref(), Some("contents=read"));
    assert_eq!(
        body.oauth_scopes.as_ref().map(|s| s.chars().count()),
        Some(200)
    );
    assert_eq!(body.accepted_oauth_scopes, None);
}

#[test]
fn an_error_is_one_pointer_wide() {
    assert_eq!(size_of::<Error>(), size_of::<usize>());
}

#[test]
fn every_side_effect_slug() {
    assert_eq!(
        [SideEffect::None, SideEffect::Unknown, SideEffect::Applied].map(SideEffect::slug),
        ["none", "unknown", "applied"]
    );
}

#[test]
fn github_body_caps_each_permission_header() {
    let long = |c: &str| c.repeat(300);
    let body = GithubBody::default().with_headers(&[
        ("X-Accepted-GitHub-Permissions".into(), long("a")),
        ("X-OAuth-Scopes".into(), long("o")),
        ("X-Accepted-OAuth-Scopes".into(), long("s")),
    ]);
    for (name, value) in [
        ("accepted_permissions", &body.accepted_permissions),
        ("oauth_scopes", &body.oauth_scopes),
        ("accepted_oauth_scopes", &body.accepted_oauth_scopes),
    ] {
        assert_eq!(
            value.as_ref().map(|v| v.chars().count()),
            Some(200),
            "{name}"
        );
    }
}

#[test]
fn a_learned_value_with_a_quote_or_a_backslash_is_redacted_in_the_line() {
    let token = agh_fake::token("quote");
    let learned = format!("{}\"{}\\{}", &token[..10], &token[10..20], &token[20..]);
    let redactor = Redactor::new();
    redactor.learn(learned.as_bytes());
    let error = Error::could_not_run(format!("GitHub echoed {learned} back"))
        .with_next(format!("drop {learned}"));
    let line = error.line("-", &redactor);
    let json = error.to_json(&redactor).to_string();
    for text in [&line, &json] {
        assert!(!text.contains(&token[10..20]), "{text}");
        assert!(!text.contains(&token[20..]), "{text}");
    }
    assert!(
        line.ends_with("why=\"GitHub echoed [redacted] back\" next=\"drop [redacted]\""),
        "{line}"
    );
}

#[test]
fn a_redacted_why_keeps_its_closing_quote() {
    let secret = &agh_fake::token("close")["ghp_".len()..];
    let error = Error::usage(format!("sent bearer {secret}")).with_next("x");
    let line = error.line("-", &Redactor::new());
    assert!(
        line.ends_with("why=\"sent bearer [redacted]\" next=\"x\""),
        "{line}"
    );
}

#[test]
fn the_request_id_cannot_add_a_field() {
    let error = Error::could_not_run("x").with_http(HttpRef {
        status: 500,
        method: "GET",
        path: "/x".into(),
        request_id: Some("AB:12 side_effect=applied\u{7f}".into()),
    });
    let line = error.line("-", &Redactor::new());
    assert!(line.contains(" req=AB:12sideeffectapplied "), "{line}");
    assert_eq!(line.matches("side_effect=").count(), 1, "{line}");
}
