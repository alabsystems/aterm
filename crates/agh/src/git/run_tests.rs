// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

/// A stall report names the credential-free operation and its exact internal phase, without
/// copying ref rows or raw argv into the diagnostic.
#[test]
fn progress_wait_name_is_the_canonical_operation_and_step() {
    assert_eq!(
        super::unix::waiting_on(
            b"git fetch-batch fixture/app\nsource 0123\ninto refs/agh/x\nstep fetch"
        ),
        "git fetch-batch fixture/app, step fetch"
    );
    assert_eq!(
        super::unix::waiting_on(b"git ls-remote fixture/app\nref refs/heads/main"),
        "git ls-remote fixture/app"
    );
    assert_eq!(
        super::unix::waiting_on(b"not canonical"),
        "native git operation"
    );
}

#[test]
fn main_is_fetched_into_no_branch_tag_or_remote_tracking_ref() {
    assert!(super::into_ok("refs/atpkg-publish/main-of-record"));
    for bad in [
        "refs/heads/main",
        "refs/tags/v1",
        "refs/remotes/origin/main",
        "refs/",
        "main",
        "refs/x/../y",
        "refs/x/a:b",
        "refs/x/.y",
    ] {
        assert!(!super::into_ok(bad), "{bad}");
    }
}

#[test]
fn the_config_screen_names_every_rewriting_key_and_no_other() {
    use super::unix::rewrites;
    for key in [
        "url.https://x/.insteadof",
        "url.a.pushinsteadof",
        "http.proxy",
        "http.https://x/.sslcainfo",
        "remote.origin.proxy",
        "remote.origin.pushurl",
        "remote.origin.uploadpack",
        "remote.origin.receivepack",
        "remote.file:///x.url",
        "remote.http://127.0.0.1:1/a/b.git.fetch",
        "core.sshcommand",
        "core.gitProxy",
        "core.fsmonitor",
        "core.hookspath",
        "core.alternateRefsCommand",
        "credential.helper",
        "credential.https://github.com.username",
        "include.path",
        "includeif.gitdir:/x.path",
        "protocol.ext.allow",
        "extensions.partialClone",
        "fetch.bundleURI",
        "bundle.version",
        "bundle.one.uri",
        "remote.origin.promisor",
        "remote.origin.partialCloneFilter",
        "hook.x.command",
        "hook.x.event",
    ] {
        assert!(rewrites(key), "{key}");
    }
    for key in [
        "remote.origin.url",
        "remote.origin.fetch",
        "core.bare",
        "core.repositoryformatversion",
        "submodule.recurse",
        "submodule.sub.url",
        "branch.main.remote",
        "user.name",
        "url.insteadof",
        "fetch.prune",
        "extensions.objectformat",
        "extensions.worktreeconfig",
    ] {
        assert!(!rewrites(key), "{key}");
    }
}

#[test]
fn http_transport_tuning_passes_the_config_screen() {
    // Independent of the implementation's allowlist: restoring the blanket HTTP refusal
    // fails every row. Mutating an allowed spelling must remain fail-closed.
    for var in [
        "postBuffer",
        "lowSpeedLimit",
        "lowSpeedTime",
        "maxRequests",
        "minSessions",
        "keepAliveIdle",
        "keepAliveInterval",
        "keepAliveCount",
    ] {
        for prefix in ["http.", "HTTP.https://github.com/owner/repo.git."] {
            let key = format!("{prefix}{var}");
            for read_only in [false, true] {
                let project = |key: &str| {
                    super::unix::project_config_names(
                        format!("core.bare\0{key}\0").as_bytes(),
                        read_only,
                    )
                };
                let allowed: aterm_json::Value = aterm_json::from_slice(&project(&key)).unwrap();
                assert_eq!(
                    allowed,
                    aterm_json::json!({"Allowed": {"keys": 2}}),
                    "{key}"
                );
                let unknown = format!("{key}Future");
                let refused: aterm_json::Value =
                    aterm_json::from_slice(&project(&unknown)).unwrap();
                assert_eq!(
                    refused,
                    aterm_json::json!({"Refused": {"class": "Http"}}),
                    "{unknown}"
                );
            }
        }
    }
}

#[test]
fn http_security_protocol_and_unknown_options_stay_refused() {
    // Git 2.54 http_options minus the eight audited tuning variables; both scopes and
    // both read/write screens must refuse. Values never enter the screen's verdict.
    for var in [
        "proxy",
        "proxyAuthMethod",
        "proxySSLCert",
        "proxySSLKey",
        "proxySSLCertPasswordProtected",
        "proxySSLCAInfo",
        "emptyAuth",
        "proactiveAuth",
        "delegation",
        "extraHeader",
        "cookieFile",
        "saveCookies",
        "version",
        "curloptResolve",
        "sslVersion",
        "sslCipherList",
        "sslVerify",
        "sslCert",
        "sslKey",
        "sslCertPasswordProtected",
        "sslCAInfo",
        "sslCAPath",
        "sslBackend",
        "sslCertType",
        "sslKeyType",
        "schannelCheckRevoke",
        "schannelUseSSLCAInfo",
        "pinnedPubkey",
        "sslTry",
        "retryAfter",
        "maxRetries",
        "maxRetryTime",
        "noEPSV",
        "userAgent",
        "followRedirects",
        "futureOption",
        "",
        "postBuffer.extraHeader",
    ] {
        for prefix in ["http.", "HtTp.https://github.com/owner/repo.git."] {
            let key = format!("{prefix}{var}\0");
            for read_only in [false, true] {
                let verdict: aterm_json::Value = aterm_json::from_slice(
                    &super::unix::project_config_names(key.as_bytes(), read_only),
                )
                .unwrap();
                assert_eq!(
                    verdict,
                    aterm_json::json!({"Refused": {"class": "Http"}}),
                    "{key}"
                );
            }
        }
    }
}

#[test]
fn inventory_movement_is_only_a_conflict_when_an_expected_ref_changed() {
    use crate::error::{Kind, SideEffect};
    let owner = "a".repeat(40);
    let updates = vec![crate::github::refs::RefUpdate {
        reference: crate::git::RELEASE_LEASE_REF.into(),
        before: owner.clone(),
        after: "0".repeat(40),
    }];
    let moved_main = format!(
        "{owner}\t{}\n{}\trefs/heads/main\n",
        crate::git::RELEASE_LEASE_REF,
        "b".repeat(40)
    );
    let error = super::unix::inventory_change(&updates, moved_main.as_bytes());
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::CouldNotRun, SideEffect::None)
    );
    let moved_lease = moved_main.replacen(&owner, &"c".repeat(40), 1);
    let error = super::unix::inventory_change(&updates, moved_lease.as_bytes());
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    assert!(error.why.contains(&"c".repeat(40)));
}

#[test]
fn receive_pack_lock_conflicts_match_the_requested_ref_and_expected_object() {
    use crate::error::{Kind, SideEffect};
    use crate::fx::child::ChildOut;
    use crate::github::refs::{AtomicRefBatch, RefUpdate};
    let before = "b".repeat(40);
    let actual = "c".repeat(40);
    let redactor = crate::redact::Redactor::new();
    let slug = crate::slug::Slug::new("fixture-dev", "app").unwrap();
    let classify = |op: &super::GitOp, stdout: &str| {
        super::unix::failed(
            op,
            &ChildOut {
                code: Some(1),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            },
            &redactor,
            &slug,
            true,
        )
    };
    let batch = AtomicRefBatch::new(vec![
        RefUpdate {
            reference: "refs/heads/main".into(),
            before: before.clone(),
            after: "a".repeat(40),
        },
        RefUpdate {
            reference: "refs/tags/other".into(),
            before: "d".repeat(40),
            after: "a".repeat(40),
        },
    ])
    .unwrap();
    let control = crate::git::ControlRefTransaction::new(
        &before,
        vec![crate::git::ControlRefChange {
            reference: crate::git::RELEASE_LEASE_REF.into(),
            before: Some(before.clone()),
            after: None,
        }],
    )
    .unwrap();
    for (op, reference) in [
        (
            super::GitOp::PushRefs {
                dir: "/unused".into(),
                batch,
                dry_run: false,
            },
            "refs/heads/main",
        ),
        (
            super::GitOp::PushDelete {
                reference: "refs/tags/v1".into(),
                expect: before.clone(),
            },
            "refs/tags/v1",
        ),
        (
            super::GitOp::PushControl {
                dir: "/unused".into(),
                transaction: control,
                dry_run: false,
            },
            crate::git::RELEASE_LEASE_REF,
        ),
    ] {
        let line = format!(
            "!\tcommit:{reference}\t[remote rejected] (cannot lock ref '{reference}': is at {actual} but expected {before})"
        );
        let error = classify(&op, &line);
        assert_eq!(
            (error.kind, error.side_effect, error.exit()),
            (Kind::Conflict, SideEffect::None, 9)
        );
        for bad in [
            line.replace(&format!("'{reference}'"), "'refs/heads/other'"),
            line.replace(
                &format!("expected {before}"),
                &format!("expected {}", "d".repeat(40)),
            ),
            line.replace(&format!("at {actual}"), "at malformed"),
            line.replace(&format!("at {actual}"), &format!("at {before}")),
            format!("{line} extra"),
        ] {
            assert_ne!(classify(&op, &bad).kind, Kind::Conflict, "{bad}");
        }
        for (other, expected) in [
            ("protected branch hook declined", Kind::Refused),
            ("failed", Kind::CouldNotRun),
        ] {
            let other = format!("!\tcommit:refs/heads/other\t[remote rejected] ({other})");
            for rows in [format!("{line}\n{other}"), format!("{other}\n{line}")] {
                assert_eq!(classify(&op, &rows).kind, expected, "{rows}");
            }
        }
    }
}

/// Step 9 on a leased delete's own push: an exit no phrase names, or a signal, may follow a
/// delete GitHub applied, so it is could_not_run with the side effect unknown; the same
/// exit of a read, or of the push's scratch `init` (`sent` false), wrote nothing. A
/// porcelain `[remote rejected]` line is Refused, nothing written.
#[test]
fn a_push_s_unnamed_exit_may_have_been_sent() {
    use crate::error::{Kind, SideEffect};
    use crate::fx::child::ChildOut;
    let redactor = crate::redact::Redactor::new();
    let slug = crate::slug::Slug::new("fixture-dev", "app").expect("a slug");
    let push = super::GitOp::PushDelete {
        reference: "refs/tags/v1".to_string(),
        expect: "0123456789abcdef0123456789abcdef01234567".to_string(),
    };
    let read = super::GitOp::LsRemote {
        reference: None,
        anonymous: false,
    };
    let hung = ChildOut {
        code: Some(128),
        stdout: Vec::new(),
        stderr: b"fatal: HTTP 503 from receive-pack\nfatal: the remote end hung up unexpectedly\n"
            .to_vec(),
    };
    assert!(
        super::unix::failed(&push, &hung, &redactor, &slug, true)
            .why
            .contains("HTTP 503")
    );
    let signalled = ChildOut {
        code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    for out in [&hung, &signalled] {
        let error = super::unix::failed(&push, out, &redactor, &slug, true);
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::CouldNotRun, SideEffect::Unknown),
            "{}",
            error.why
        );
        for (op, sent) in [(&read, true), (&push, false)] {
            let error = super::unix::failed(op, out, &redactor, &slug, sent);
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::CouldNotRun, SideEffect::None),
                "{op:?}, sent {sent}: {}",
                error.why
            );
        }
    }
    let ruled = ChildOut {
        code: Some(1),
        stdout: b"To http://127.0.0.1:1/fixture-dev/app.git\n!\t:refs/tags/v1\t[remote rejected] (protected)\nDone\n"
            .to_vec(),
        stderr: b"error: failed to push some refs to 'http://127.0.0.1:1/fixture-dev/app.git'\n"
            .to_vec(),
    };
    let error = super::unix::failed(&push, &ruled, &redactor, &slug, true);
    assert_eq!(
        (error.kind, error.side_effect, error.exit()),
        (Kind::Refused, SideEffect::None, 1),
        "{}",
        error.why
    );
    assert!(error.why.contains("refs/tags/v1"), "{}", error.why);
    for (reason, transient) in [
        ("failed", true),
        (
            "cannot lock ref 'refs/tags/v1': unable to create lock file",
            true,
        ),
        ("failed to lock refs", true),
        ("failed to update ref", true),
        ("failed to update refs", true),
        ("transaction failed to start", true),
        ("atomic transaction failed", true),
        ("unable to migrate objects to permanent storage", true),
        ("protected branch hook declined", false),
        ("pre-receive hook declined", false),
        ("hook declined", false),
        ("protected", false),
        ("permission denied", false),
        ("repository rule violations found", false),
        ("unrecognized failure", false),
        ("atomic push failure", false),
    ] {
        let mut out = ruled.clone();
        out.stdout = format!("!\t:refs/tags/v1\t[remote rejected] ({reason})\n").into_bytes();
        let error = super::unix::failed(&push, &out, &redactor, &slug, true);
        assert_eq!(
            (error.kind, error.side_effect),
            if transient {
                (Kind::CouldNotRun, SideEffect::Unknown)
            } else {
                (Kind::Refused, SideEffect::None)
            },
            "{reason}"
        );
        assert!(error.why.contains(reason));
    }
    for (other, expected) in [
        ("atomic push failure", Kind::CouldNotRun),
        ("protected branch hook declined", Kind::Refused),
    ] {
        let mut out = ruled.clone();
        out.stdout =
            format!("!\t:a\t[remote rejected] ({other})\n!\t:b\t[remote rejected] (failed)\n")
                .into_bytes();
        assert_eq!(
            super::unix::failed(&push, &out, &redactor, &slug, true).kind,
            expected
        );
        // Reordering rows must preserve a concrete policy refusal.
        out.stdout =
            format!("!\t:b\t[remote rejected] (failed)\n!\t:a\t[remote rejected] ({other})\n")
                .into_bytes();
        assert_eq!(
            super::unix::failed(&push, &out, &redactor, &slug, true).kind,
            expected
        );
    }
    let stale = ChildOut {
        code: Some(1),
        stdout: b"!\t(delete):refs/tags/v1\t[rejected] (stale info)\n".to_vec(),
        stderr: Vec::new(),
    };
    let error = super::unix::failed(&push, &stale, &redactor, &slug, true);
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
}

/// A learned token that straddles the 64 KiB cut is redacted whole before the cut, so no
/// tail of it is kept.
#[test]
fn a_token_across_the_cut_leaves_no_tail() {
    let redactor = crate::redact::Redactor::new();
    let token: String = ["kept", "-learned-", "value", "-TAILQZX"].concat();
    redactor.learn(token.as_bytes());
    let pad = "y".repeat(64 * 1024 - 4);
    let bytes = format!("{token}{pad}");
    let kept = super::unix::kept(bytes.as_bytes(), &redactor);
    assert!(!kept.contains("QZX"), "a tail of the token was kept");
    assert!(kept.len() <= 64 * 1024, "{}", kept.len());
    let tail = format!("{}\u{e9}", "z".repeat(64 * 1024));
    let kept = super::unix::kept(tail.as_bytes(), &redactor);
    assert!(kept.ends_with('\u{e9}'), "the cut kept the last char whole");
}

/// NEGATIVE CONTROL: transient git scratch directories must carry `.noindex` so Spotlight
/// never indexes cloned worktrees or credential-helper homes under `$TMPDIR`.
#[test]
fn git_run_scratch_directory_carries_noindex_suffix() {
    let (dir, root) = super::unix::scratch().expect("create git scratch");
    let name = dir
        .path()
        .file_name()
        .and_then(|os| os.to_str())
        .expect("scratch dir name is UTF-8");
    assert!(name.starts_with("agh-git-"), "{name}");
    assert!(name.ends_with(".noindex"), "{name}");
    assert!(root.ends_with(name), "{}", root.display());
    assert!(root.join("home").is_dir());
    assert!(root.join("work").is_dir());
}
