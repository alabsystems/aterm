// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

#[path = "run_output_proof_tests.rs"]
mod output_proof;

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

use super::ConfigAccess;

const ACCESSES: [ConfigAccess; 3] = [
    ConfigAccess::Write,
    ConfigAccess::Read,
    ConfigAccess::ObjectSource,
];

/// RL-30: a repository agh only reads objects from may set exactly the keys a word on every
/// child makes inert, in any spelling; a clone it fetches into or writes may not, and every
/// other guarded key stays refused for an object source too.
#[test]
fn only_an_object_source_may_set_the_keys_every_child_overrides() {
    let verdict = |key: &str, access| -> aterm_json::Value {
        aterm_json::from_slice(&super::unix::project_config_names(
            format!("core.bare\0{key}\0").as_bytes(),
            access,
        ))
        .unwrap()
    };
    let allowed = aterm_json::json!({"Allowed": {"keys": 2}});
    for key in ["core.hooksPath", "CORE.FSMONITOR", "core.sshcommand"] {
        assert_eq!(verdict(key, ConfigAccess::ObjectSource), allowed, "{key}");
        for access in [ConfigAccess::Read, ConfigAccess::Write] {
            assert_ne!(verdict(key, access), allowed, "{key} {access:?}");
        }
    }
    for key in [
        "url.https://x/.insteadOf",
        "core.gitProxy",
        "core.alternateRefsCommand",
        "remote.origin.uploadpack",
        "remote.origin.promisor",
        "extensions.partialClone",
        "hook.x.command",
        "credential.helper",
        "include.path",
        "protocol.ext.allow",
    ] {
        assert_ne!(verdict(key, ConfigAccess::ObjectSource), allowed, "{key}");
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
            for access in ACCESSES {
                let project = |key: &str| {
                    super::unix::project_config_names(
                        format!("core.bare\0{key}\0").as_bytes(),
                        access,
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
            for access in ACCESSES {
                let verdict: aterm_json::Value = aterm_json::from_slice(
                    &super::unix::project_config_names(key.as_bytes(), access),
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

/// Each guarded ref is judged on its own. Main is an object witness: a move is
/// handed back for the caller's fast-forward proof, never refused as a byte
/// difference. NEGATIVE CONTROL: the whole-listing comparison refused every
/// row below that has a moved main, the tag push of the 0.204.0 cut among them.
#[test]
fn inventory_is_judged_per_ref_with_main_only_an_object_witness() {
    use crate::error::{Kind, SideEffect};
    use crate::git::{
        ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF,
    };
    use std::collections::BTreeMap;
    const MAIN: &str = "refs/heads/main";
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let (owner, old, new, other) = (
        "a".repeat(40),
        "b".repeat(40),
        "d".repeat(40),
        "c".repeat(40),
    );
    let tag = vec![crate::github::refs::RefUpdate {
        reference: "refs/tags/v1.0.0".into(),
        before: ZERO.into(),
        after: owner.clone(),
    }];
    let judge = |updates: &[crate::github::refs::RefUpdate],
                 control: Option<&ControlRefTransaction>,
                 listing: &[(&str, &str)]| {
        let current: BTreeMap<&str, &str> = listing.iter().copied().collect();
        super::unix::judge_inventory(updates, control, Some(&old), &current)
            .map(|moved| moved.map(|(before, after)| (before.to_owned(), after.to_owned())))
    };
    // An ordinary tag create: main as judged, or advanced, goes ahead.
    assert_eq!(judge(&tag, None, &[(MAIN, &old)]).unwrap(), None);
    assert_eq!(
        judge(&tag, None, &[(MAIN, &new)]).unwrap(),
        Some((old.clone(), new.clone()))
    );
    // A main that disappeared witnesses nothing.
    let error = judge(&tag, None, &[]).unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::CouldNotRun, SideEffect::None)
    );
    // A main that appeared was never a witness.
    let current = BTreeMap::from([(MAIN, new.as_str())]);
    assert_eq!(
        super::unix::judge_inventory(&tag, None, None, &current).unwrap(),
        None
    );
    // A requested ref that moved is the caller's conflict, main moved or not.
    for main in [&old, &new] {
        let error = judge(&tag, None, &[(MAIN, main), ("refs/tags/v1.0.0", &other)]).unwrap_err();
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::Conflict, SideEffect::None)
        );
        assert!(error.why.contains(&other), "{}", error.why);
    }
    // A single lease create: its unrequested fence sibling must stay absent.
    let lease = ControlRefTransaction::new(
        &owner,
        vec![ControlRefChange {
            reference: RELEASE_LEASE_REF.into(),
            before: None,
            after: Some(owner.clone()),
        }],
    )
    .unwrap();
    let rows = vec![crate::github::refs::RefUpdate {
        reference: RELEASE_LEASE_REF.into(),
        before: ZERO.into(),
        after: owner.clone(),
    }];
    assert_eq!(
        judge(&rows, Some(&lease), &[(MAIN, &new)]).unwrap(),
        Some((old.clone(), new.clone()))
    );
    let error = judge(
        &rows,
        Some(&lease),
        &[(MAIN, &old), (PUBLISHER_FENCE_REF, &other)],
    )
    .unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    let error = judge(
        &rows,
        Some(&lease),
        &[(MAIN, &new), (RELEASE_LEASE_REF, &owner)],
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::Conflict);
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
        let after = "a".repeat(40);
        let source = if matches!(&op, super::GitOp::PushRefs { .. }) {
            after.as_str()
        } else {
            "(delete)"
        };
        let sibling = if matches!(&op, super::GitOp::PushRefs { .. }) {
            format!("\n!\t{after}:refs/tags/other\t[remote rejected] (atomic push failure)")
        } else {
            String::new()
        };
        let line = format!(
            "!\t{source}:{reference}\t[remote rejected] (cannot lock ref '{reference}': is at {actual} but expected {before})"
        );
        let error = classify(&op, &format!("{line}{sibling}"));
        assert_eq!(
            (error.kind, error.side_effect, error.exit()),
            (Kind::Conflict, SideEffect::None, 9)
        );
        if !sibling.is_empty() {
            let incomplete = classify(&op, &line);
            assert_eq!(incomplete.side_effect, SideEffect::Unknown);
            assert_ne!(incomplete.kind, Kind::Conflict);
        }
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
            assert_ne!(
                classify(&op, &format!("{bad}{sibling}")).kind,
                Kind::Conflict,
                "{bad}"
            );
        }
        for (other, expected) in [
            ("protected branch hook declined", Kind::Refused),
            ("failed", Kind::CouldNotRun),
        ] {
            let other = format!("!\tcommit:refs/heads/other\t[remote rejected] ({other})");
            for rows in [format!("{line}\n{other}"), format!("{other}\n{line}")] {
                let error = classify(&op, &rows);
                if matches!(
                    &op,
                    super::GitOp::PushRefs { .. } | super::GitOp::PushControl { .. }
                ) {
                    assert_eq!(
                        (error.kind, error.side_effect),
                        (Kind::CouldNotRun, SideEffect::Unknown),
                        "inconsistent atomic report: {rows}"
                    );
                } else {
                    assert_eq!(error.kind, expected, "{rows}");
                }
            }
        }
    }
}

#[test]
fn atomic_receive_lock_conflicts_need_every_requested_rejection_and_exact_old_values() {
    use crate::error::{Kind, SideEffect};
    use crate::fx::child::ChildOut;
    use crate::github::refs::{AtomicRefBatch, RefUpdate};
    let zero = "0".repeat(40);
    let before = "b".repeat(40);
    let after = "a".repeat(40);
    let actual = "c".repeat(40);
    let redactor = crate::redact::Redactor::new();
    let slug = crate::slug::Slug::new("fixture-dev", "app").unwrap();
    for (op, reference, absent) in [
        (
            super::GitOp::PushRefs {
                dir: "/unused".into(),
                batch: AtomicRefBatch::new(vec![
                    RefUpdate {
                        reference: "refs/tags/new".into(),
                        before: zero.clone(),
                        after: after.clone(),
                    },
                    RefUpdate {
                        reference: "refs/heads/other".into(),
                        before: before.clone(),
                        after: after.clone(),
                    },
                ])
                .unwrap(),
                dry_run: false,
            },
            "refs/tags/new",
            true,
        ),
        (
            super::GitOp::PushControl {
                dir: "/unused".into(),
                transaction: crate::git::ControlRefTransaction::new(
                    &after,
                    vec![crate::git::ControlRefChange {
                        reference: crate::git::RELEASE_LEASE_REF.into(),
                        before: None,
                        after: Some(after.clone()),
                    }],
                )
                .unwrap(),
                dry_run: false,
            },
            crate::git::RELEASE_LEASE_REF,
            true,
        ),
        (
            super::GitOp::PushRefs {
                dir: "/unused".into(),
                batch: AtomicRefBatch::new(vec![RefUpdate {
                    reference: "refs/heads/update".into(),
                    before: before.clone(),
                    after: after.clone(),
                }])
                .unwrap(),
                dry_run: false,
            },
            "refs/heads/update",
            false,
        ),
    ] {
        let mut stdout = format!(
            "To https://github.com/fixture-dev/app.git\n!\t{after}:{reference}\t[remote rejected] (atomic transaction failed)\n"
        );
        if reference == "refs/tags/new" {
            stdout.push_str(&format!(
                "!\t{after}:refs/heads/other\t[remote rejected] (atomic transaction failed)\n"
            ));
        }
        stdout.push_str("Done\n");
        let classify = |out: &ChildOut| super::unix::failed(&op, out, &redactor, &slug, true);
        for reason in [
            "reference already exists".to_string(),
            format!("reference is missing but expected {before}"),
            format!("is at {actual} but expected {before}"),
        ] {
            let out = ChildOut {
                code: Some(1),
                stdout: stdout.as_bytes().to_vec(),
                stderr: format!("remote: error: cannot lock ref '{reference}': {reason}        \n")
                    .into_bytes(),
            };
            let applies = absent == (reason == "reference already exists");
            let error = classify(&out);
            if !applies {
                assert_ne!(error.kind, Kind::Conflict, "{reference}: {reason}");
                continue;
            }
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::Conflict, SideEffect::None),
                "{reference}: {reason}"
            );
            let mut negatives = Vec::new();
            for malformed in [
                stdout.replace("!\t", "*\t"),
                stdout.replace("atomic transaction failed", "failed"),
                stdout.replace(
                    &format!("{after}:{reference}"),
                    &format!("{actual}:{reference}"),
                ),
                stdout.lines().take(1).collect::<Vec<_>>().join("\n"),
                format!(
                    "{stdout}!\t{after}:{reference}\t[remote rejected] (atomic transaction failed)\n"
                ),
            ] {
                negatives.push(ChildOut {
                    stdout: malformed.into_bytes(),
                    ..out.clone()
                });
            }
            if reference == "refs/tags/new" {
                negatives.push(ChildOut {
                    stdout: stdout.replace(
                        &format!("!\t{after}:refs/heads/other\t[remote rejected] (atomic transaction failed)\n"),
                        "",
                    ).into_bytes(),
                    ..out.clone()
                });
            }
            for malformed in [
                String::from_utf8(out.stderr.clone())
                    .unwrap()
                    .replace(reference, "refs/heads/unrequested"),
                format!("remote: error: cannot lock ref '{reference}': {reason} extra\n"),
            ] {
                negatives.push(ChildOut {
                    stderr: malformed.into_bytes(),
                    ..out.clone()
                });
            }
            negatives.push(ChildOut {
                code: None,
                ..out.clone()
            });
            negatives.push(ChildOut {
                stdout: vec![b'\n'; ChildOut::OUTPUT_KEEP],
                ..out.clone()
            });
            for negative in negatives {
                let error = classify(&negative);
                assert_ne!(error.kind, Kind::Conflict, "{negative:?}");
            }
        }
    }
}

#[test]
fn atomic_stale_info_needs_complete_exact_request_rows() {
    use crate::error::{Kind, SideEffect};
    use crate::fx::child::ChildOut;
    use crate::github::refs::{AtomicRefBatch, RefUpdate};
    let before = "b".repeat(40);
    let after = "a".repeat(40);
    let op = super::GitOp::PushRefs {
        dir: "/unused".into(),
        batch: AtomicRefBatch::new(
            ["refs/heads/main", "refs/heads/other"]
                .into_iter()
                .map(|reference| RefUpdate {
                    reference: reference.into(),
                    before: before.clone(),
                    after: after.clone(),
                })
                .collect(),
        )
        .unwrap(),
        dry_run: false,
    };
    let first = format!("!\t{after}:refs/heads/main\t[rejected] (stale info)\n");
    let second = format!("!\t{after}:refs/heads/other\t[rejected] (atomic push failed)\n");
    let complete = format!("{first}{second}");
    let redactor = crate::redact::Redactor::new();
    let slug = crate::slug::Slug::new("fixture-dev", "app").unwrap();
    let classify = |stdout: &str| {
        super::unix::failed(
            &op,
            &ChildOut {
                code: Some(1),
                stdout: stdout.as_bytes().to_vec(),
                stderr: b"stale info\n".to_vec(),
            },
            &redactor,
            &slug,
            true,
        )
    };
    let known = classify(&complete);
    assert_eq!(
        (known.kind, known.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    for incomplete in [
        "stale info\n".into(),
        first.clone(),
        second,
        format!("{complete}{first}"),
        complete.replace(&after, &"c".repeat(40)),
        complete.replace("refs/heads/other", "refs/heads/unrequested"),
        format!("{}{complete}", "\n".repeat(ChildOut::OUTPUT_KEEP)),
    ] {
        let error = classify(&incomplete);
        assert_ne!(error.kind, Kind::Conflict);
        assert_eq!(error.side_effect, SideEffect::Unknown);
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
        peeled: false,
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
