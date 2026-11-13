// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{BODY_TAPE_MAX, request_credentialed, request_digest};
use crate::error::{Kind, Replay, Sub};
use crate::identity::Credential;
use crate::policy::Tier;
use crate::wire::door::AuthHeader;
use crate::wire::{Deadlines, Endpoints, Method, WireBody, WireRequest};

/// `method` to `url` on github.com's endpoints, as the profile `dev`.
fn as_dev<'a>(method: Method, url: &str) -> WireRequest<'a> {
    let mut req = WireRequest::new(method, url, Vec::new(), 1 << 20, Deadlines::JSON);
    req.as_profile = Some("dev");
    req
}

/// The credentialed canonical bytes, as text.
fn text(req: &WireRequest<'_>) -> String {
    let bytes = request_credentialed(req, &Endpoints::github_com()).expect("canonical");
    String::from_utf8(bytes).expect("UTF-8")
}

/// The digest is the SHA-256 test vector's, then the size.
#[test]
fn a_request_digest_is_sha256_then_the_size() {
    assert_eq!(
        request_digest(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad 3"
    );
}

/// The profile is named, the credential never: an `authorization` header among the
/// headers, and the `auth` the pipeline built, leave the bytes as they are without them.
#[test]
fn a_credentialed_request_names_its_profile_and_never_its_credential() {
    let e = Endpoints::github_com();
    let token = agh_fake::token("canon-credentialed");
    let cred = Credential::for_tests("dev", Tier::Dev, token.as_bytes());
    let header = AuthHeader::for_origin_for_tests(&cred, &e, e.api()).expect("a header");
    let plain = as_dev(Method::Get, "https://api.github.com/user");
    let mut carried = as_dev(Method::Get, "https://api.github.com/user");
    carried
        .headers
        .push(("Authorization".to_string(), format!("Bearer {token}")));
    carried.auth = Some(&header);
    assert_eq!(text(&plain), "GET /user\norigin api\nas dev\nbody none");
    assert_eq!(text(&carried), text(&plain));
    let mut anonymous = as_dev(Method::Get, "https://api.github.com/user");
    anonymous.as_profile = None;
    assert_eq!(
        text(&anonymous),
        "GET /user\norigin api\nas anonymous\nbody none"
    );
}

/// A JSON body of at most `BODY_TAPE_MAX` bytes is written out, one line per newline.
#[test]
fn a_small_json_body_is_written_line_by_line() {
    let body = b"{\"a\":1}\n{\"b\":2}";
    let mut req = as_dev(Method::Post, "https://api.github.com/repos/o/r/releases");
    req.body = WireBody::Bytes(body);
    assert_eq!(
        text(&req),
        "POST /repos/o/r/releases\norigin api\nas dev\nbody json 15\n{\"a\":1}\n{\"b\":2}"
    );
    let at_max = vec![b' '; BODY_TAPE_MAX];
    req.body = WireBody::Bytes(&at_max);
    assert!(
        text(&req).contains(&format!("\nbody json {BODY_TAPE_MAX}\n")),
        "a body of exactly BODY_TAPE_MAX bytes is written out"
    );
}

/// A body over `BODY_TAPE_MAX`, or one that is not UTF-8, is named by its digest and size.
#[test]
fn a_large_or_binary_body_is_named_by_its_digest() {
    let large = vec![b' '; BODY_TAPE_MAX + 1];
    let binary = [0xff, 0xfe, b'a'];
    for body in [&large[..], &binary[..]] {
        let mut req = as_dev(Method::Patch, "https://api.github.com/repos/o/r");
        req.body = WireBody::Bytes(body);
        assert_eq!(
            text(&req),
            format!(
                "PATCH /repos/o/r\norigin api\nas dev\nbody {}",
                request_digest(body)
            )
        );
    }
}

#[test]
fn exact_fetch_identity_keeps_refs_and_auth_mode_but_not_local_paths() {
    use crate::git::GitRun;
    use std::path::Path;

    let slug = crate::slug::Slug::new("owner", "repo").unwrap();
    let request = |path: &str, source: &str, into: &str, anonymous: bool| {
        super::git_request(&GitRun::fetch_ref(
            &slug,
            Path::new(path),
            source,
            into,
            anonymous,
            crate::git::FetchScope::NativeExact,
        ))
    };
    let expected = request("/first/checkout", "refs/heads/main", "refs/agh/exact", true);
    assert_eq!(
        expected,
        b"git fetch-ref owner/repo\nref refs/heads/main\ninto refs/agh/exact\nanonymous true"
    );
    assert_eq!(
        expected,
        request(
            "/another/checkout",
            "refs/heads/main",
            "refs/agh/exact",
            true
        )
    );
    for (source, into, anonymous) in [
        ("refs/tags/v1", "refs/agh/exact", true),
        ("refs/heads/main", "refs/agh/other", true),
        ("refs/heads/main", "refs/agh/exact", false),
    ] {
        assert_ne!(
            expected,
            request("/first/checkout", source, into, anonymous)
        );
    }
}

#[test]
fn exact_fetch_closure_scopes_cannot_share_a_replay_identity() {
    use crate::git::{FetchScope, GitRun};
    let slug = crate::slug::Slug::new("owner", "repo").unwrap();
    let root = std::path::Path::new("/not-opened");
    let scopes = [
        FetchScope::NativeExact,
        FetchScope::MetadataOnly,
        FetchScope::SelectedCheckout { cache: None },
        FetchScope::SelectedCheckout {
            cache: Some(root.into()),
        },
        FetchScope::FullSource { cache: root.into() },
    ];
    let keys: Vec<_> = scopes
        .into_iter()
        .map(|scope| {
            super::git_request(&GitRun::fetch_ref(
                &slug,
                root,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "refs/agh/exact",
                true,
                scope,
            ))
        })
        .collect();
    for (index, key) in keys.iter().enumerate() {
        assert!(keys[..index].iter().all(|other| other != key));
    }
}

#[test]
fn batch_fetch_identity_binds_the_snapshot_and_auth_mode_without_local_paths() {
    use crate::git::{FetchBatch, FetchRow, GitRun};
    let slug = crate::slug::Slug::new("owner", "repo").unwrap();
    let request = |path: &str, expected: &str, anonymous| {
        let batch = FetchBatch::new(vec![FetchRow {
            source: "refs/heads/main".into(),
            into: "refs/agh/main".into(),
            expected_oid: expected.into(),
        }])
        .unwrap();
        super::git_request(&GitRun::fetch_batch(
            &slug,
            std::path::Path::new(path),
            batch,
            anonymous,
        ))
    };
    let expected = "a".repeat(40);
    let first = request("/one", &expected, true);
    assert_eq!(first, request("/two", &expected, true));
    assert_ne!(first, request("/one", &"b".repeat(40), true));
    assert_ne!(first, request("/one", &expected, false));
    let import = super::git_request(&GitRun::fetch_batch(
        &slug,
        std::path::Path::new("/one"),
        FetchBatch::objects(vec![expected.clone()]).unwrap(),
        true,
    ));
    assert_ne!(
        first, import,
        "publishing refs and importing objects are distinct requests"
    );
    let other = super::git_request(&GitRun::fetch_batch(
        &slug,
        std::path::Path::new("/two"),
        FetchBatch::objects(vec!["b".repeat(40)]).unwrap(),
        true,
    ));
    assert_ne!(import, other);
}

#[test]
fn native_push_identity_binds_each_old_new_ref_and_dry_run_but_not_source_path() {
    use crate::git::GitRun;
    use crate::github::refs::{AtomicRefBatch, RefUpdate};
    let slug = crate::slug::Slug::new("owner", "repo").unwrap();
    let request = |dir: &str, reference: &str, before: char, after: char, dry| {
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: reference.into(),
            before: before.to_string().repeat(40),
            after: after.to_string().repeat(40),
        }])
        .unwrap();
        super::git_request(&GitRun::push_refs(
            &slug,
            std::path::Path::new(dir),
            batch,
            dry,
        ))
    };
    let expected = request("/first", "refs/heads/main", '0', 'a', false);
    assert_eq!(
        expected,
        request("/other", "refs/heads/main", '0', 'a', false)
    );
    for (reference, before, after, dry) in [
        ("refs/tags/v1", '0', 'a', false),
        ("refs/heads/main", 'b', 'a', false),
        ("refs/heads/main", '0', 'c', false),
        ("refs/heads/main", '0', 'a', true),
    ] {
        assert_ne!(expected, request("/first", reference, before, after, dry));
    }
}

/// `git_decode` reads back what `git_reply` wrote, and refuses what it did not.
#[test]
fn git_replies_round_trip() {
    use super::super::child::ChildOut;
    use super::{git_decode, git_reply};
    let out = |code: Option<i32>, stdout: &[u8], stderr: &[u8]| ChildOut {
        code,
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    };
    for answer in [
        out(Some(0), b"", b""),
        out(Some(0), b"abc\n", b""),
        out(Some(1), b"", b"one\ntwo\nthree"),
        out(None, b"x", b"\n"),
    ] {
        assert_eq!(
            git_decode(&git_reply(&answer)).expect("decodes"),
            answer,
            "{answer:?}"
        );
    }
    assert_eq!(
        git_reply(&out(Some(0), b"", b"")),
        b"exit 0\nstdout 1\n\nstderr 1\n"
    );
    let token = agh_fake::token("canon-git");
    let shaped = git_decode(&git_reply(&out(
        Some(128),
        b"",
        format!("fatal: {token}\n").as_bytes(),
    )))
    .expect("decodes");
    assert_eq!(shaped.stderr, b"fatal: [redacted]\n");
    let run = aterm_codec::base64::encode(b"agh:canon-git-1").expect("base64");
    assert_eq!(run.len(), 20);
    assert!(run.bytes().any(|b| !b.is_ascii_alphabetic()), "{run}");
    let basic = git_decode(&git_reply(&out(
        Some(128),
        b"",
        format!("remote: Basic {run}\nfatal: denied").as_bytes(),
    )))
    .expect("decodes");
    let stderr = String::from_utf8(basic.stderr).expect("UTF-8");
    assert_eq!(stderr, "remote: Basic [redacted]\nfatal: denied");
    for junk in [
        &b""[..],
        b"exit 0",
        b"exit x\nstdout 1\n\nstderr 1\n",
        b"exit 0\nstdout 2\n\nstderr 1\n",
        b"exit 0\nstdout 1\n\nstderr 1\n\nmore",
    ] {
        let refused = git_decode(junk).expect_err("not git_reply's");
        assert_eq!(refused.sub, Sub::Replay(Replay::Format), "{junk:?}");
    }
}

/// A git run that ended with no exit is taped as its error and read back as that error,
/// its side effect kept and its why shaped; any other `failed` bytes are `Replay::Format`.
#[test]
fn git_failures_round_trip() {
    use super::{git_answer, git_failure};
    use crate::error::SideEffect;

    let token = agh_fake::token("canon-git-failure");
    for effect in [SideEffect::None, SideEffect::Unknown, SideEffect::Applied] {
        let failed =
            crate::error::Error::local(format!("/usr/bin/git did not finish\nwith {token}"))
                .with_side_effect(effect);
        let back = git_answer(&git_failure(&failed))
            .expect("decodes")
            .expect_err("a failure");
        assert_eq!(back.kind, Kind::CouldNotRun);
        assert_eq!(back.sub, Sub::Local);
        assert_eq!(back.side_effect, effect);
        assert_eq!(back.why, "/usr/bin/git did not finish with [redacted]");
    }
    let ran = super::super::child::ChildOut {
        code: Some(0),
        stdout: b"ok".to_vec(),
        stderr: Vec::new(),
    };
    let answer = git_answer(&super::git_reply(&ran)).expect("decodes");
    assert_eq!(answer.expect("an exit"), ran);
    for junk in [
        &b"failed"[..],
        b"failed maybe\nwhy x",
        b"failed none",
        b"failed none\nwhy x\nmore",
        b"failed none\nhow x",
    ] {
        let refused = git_answer(junk).expect_err("not git_failure's");
        assert_eq!(refused.sub, Sub::Replay(Replay::Format), "{junk:?}");
    }
}

/// A file body is named by the digest the pipeline set; with none, it is not taped.
#[test]
fn a_file_body_is_named_by_its_digest_or_refused() {
    let mut bytes: &[u8] = b"abc";
    let mut req = as_dev(Method::Post, "https://uploads.github.com/repos/o/r/assets");
    req.body = WireBody::Reader {
        len: 3,
        reader: &mut bytes,
    };
    req.body_digest = Some("sha256:00ff");
    assert_eq!(
        text(&req),
        "POST /repos/o/r/assets\norigin uploads\nas dev\nbody sha256:00ff 3"
    );
    req.body_digest = None;
    let refused = request_credentialed(&req, &Endpoints::github_com()).expect_err("no digest");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Replay(Replay::StreamNotTaped))
    );
}

/// A redirect off the API keeps no query: codeload's `?token=` and a signed blob URL's
/// `?sig=` are bearer secrets. One on the API origin is kept as it is.
#[test]
fn a_location_off_the_api_keeps_no_query() {
    let e = Endpoints::github_com();
    let location = |url: &str| {
        let kept = super::keep(&e, vec![("Location".to_string(), url.to_string())]);
        kept.into_iter().map(|(_, value)| value).collect::<Vec<_>>()
    };
    assert_eq!(
        location("https://codeload.github.com/o/r/legacy.tar.gz/refs/heads/main?token=ABCDEF"),
        ["https://codeload.github.com/o/r/legacy.tar.gz/refs/heads/main?signed=redacted"]
    );
    assert_eq!(
        location("https://pipelines.example.net/logs/1.zip?sv=2025&se=9&sig=c2lnbmF0dXJl"),
        ["https://pipelines.example.net/logs/1.zip?signed=redacted"]
    );
    let api = "https://api.github.com/repositories/9/releases?page=2&per_page=100";
    assert_eq!(location(api), [api]);
    assert_eq!(
        location("https://codeload.github.com/o/r/tar.gz/main"),
        ["https://codeload.github.com/o/r/tar.gz/main"]
    );
}
