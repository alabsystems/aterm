// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{Asked, CredentialAnswer, CredentialRequest, admitted};
use crate::git::grant::MapGrants;
#[cfg(unix)]
use crate::identity::MapEnv;
use crate::policy::Router;

/// One dev profile with a real private token file, reading and writing for `fixture-dev`.
fn router() -> (aterm_tempfile::TempDir, Router, std::path::PathBuf) {
    let root = crate::test_fixtures::scratch_dir("credential-binding");
    let token = root.path().join("dev.token");
    #[cfg(unix)]
    crate::test_fixtures::setup_file(&token, agh_fake::token("dev").as_bytes());
    #[cfg(not(unix))]
    std::fs::write(&token, agh_fake::token("dev")).unwrap();
    let text = format!(
        "[profiles.dev]\ntoken_file = \"{}\"\ntier = \"dev\"\n\
                login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
        crate::test_fixtures::toml_path(&token)
    );
    let parsed = crate::config::parse(&text).unwrap_or_else(|f| panic!("{}: {}", f.rule, f.what));
    let validated = crate::config::validate(&parsed, std::path::Path::new("/"), &|_| None)
        .unwrap_or_else(|faults| panic!("{} faults", faults.len()));
    (root, Router::from_validated(validated), token)
}

/// `whoami --git` explains what the helper answers: off unix that is always a quit, as
/// `serve_with` answers there, even for a profile that is Present.
#[test]
fn the_helper_answers_only_on_unix() {
    let (_root, router, _token) = router();
    let req = CredentialRequest::parse(
        "get",
        "protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n",
    )
    .unwrap_or_else(|e| panic!("the request: {}", e.why));
    let admission = admitted(&req, &Asked::default(), &router, &MapGrants::new(), &[]);
    assert!(!admission.granted);
    assert_eq!(
        matches!(admission.answer, CredentialAnswer::Credential { .. }),
        cfg!(unix),
        "{:?}",
        admission.answer
    );
}

#[cfg(unix)]
#[test]
fn final_grant_admission_binds_the_exact_loaded_or_pinned_credential() {
    use std::sync::Arc;
    let (_root, router, token) = router();
    let load = |seed| {
        std::fs::write(&token, agh_fake::token(seed)).unwrap();
        Arc::new(crate::identity::load(router.profile("dev").unwrap()).unwrap())
    };
    let original = load("original");
    let replacement = load("replacement");
    let nonce = "ab".repeat(16);
    let grant = |binding| super::super::grant::Grant {
        nonce: nonce.clone(),
        slug: crate::Slug::parse("fixture-dev/app").unwrap(),
        profile: "dev".into(),
        credential_binding: binding,
        pid: std::process::id(),
        start: 1,
        expires_at: 100,
    };
    let mut last_byte_changed = original.git_grant_binding();
    last_byte_changed[31] ^= 1;
    for (binding, pins, seed, allowed) in [
        (original.git_grant_binding(), Vec::new(), "original", true),
        (
            original.git_grant_binding(),
            Vec::new(),
            "replacement",
            false,
        ),
        (
            original.git_grant_binding(),
            vec![original.clone()],
            "replacement",
            true,
        ),
        (
            replacement.git_grant_binding(),
            vec![original.clone()],
            "replacement",
            false,
        ),
        (last_byte_changed, vec![original.clone()], "original", false),
    ] {
        let grants = MapGrants::new().with(grant(binding), true);
        std::fs::write(&token, agh_fake::token(seed)).unwrap();
        let env = MapEnv::new().with(crate::git::GRANT_VAR, &nonce);
        let mut input =
            std::io::Cursor::new(b"protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n");
        let mut output = Vec::new();
        super::serve_with(
            "get",
            super::HelperIo {
                stdin: &mut input,
                stdin_is_terminal: false,
                stdout: &mut output,
            },
            &router,
            &grants,
            &env,
            1,
            &pins,
        )
        .unwrap();
        if allowed {
            assert_eq!(
                output,
                format!(
                    "username=x-access-token\npassword={}\n",
                    agh_fake::token("original")
                )
                .as_bytes()
            );
        } else {
            assert_eq!(output, b"quit=1\n");
        }
    }

    // A successful selection is no authority to emit a file replaced before admission.
    let grants = MapGrants::new().with(grant(original.git_grant_binding()), true);
    let req = CredentialRequest::parse(
        "get",
        "protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n",
    )
    .unwrap();
    let asked = Asked {
        nonce: Some(&nonce),
        now: 1,
        ..Asked::default()
    };
    std::fs::write(&token, agh_fake::token("original")).unwrap();
    let (selected, selected_grant) = super::select(&req, &asked, &router, &grants, &|name| {
        router.presence(name)
    });
    assert!(matches!(selected, CredentialAnswer::Credential { .. }));
    assert!(selected_grant.is_some());
    std::fs::write(&token, agh_fake::token("replacement")).unwrap();
    let admission = admitted(&req, &asked, &router, &grants, &[]);
    assert!(matches!(admission.answer, CredentialAnswer::Quit { .. }));
    assert!(admission.credential.is_none());
}

#[cfg(unix)]
#[test]
fn invalid_raw_operation_nonce_never_becomes_an_ordinary_read() {
    use std::os::unix::ffi::OsStringExt as _;
    let (_root, router, _token) = router();
    struct RawNonce {
        nonce: Option<std::ffi::OsString>,
    }
    impl crate::identity::EnvSource for RawNonce {
        fn var_os(&self, name: &str) -> Option<std::ffi::OsString> {
            if name == crate::git::GRANT_VAR {
                return self.nonce.clone();
            }
            None
        }
    }
    for nonce in [
        Some(std::ffi::OsString::from_vec(vec![0xff])),
        Some(std::ffi::OsString::new()),
        None,
    ] {
        let supplied = nonce.is_some();
        let env = RawNonce { nonce };
        let mut input =
            std::io::Cursor::new(b"protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n");
        let mut output = Vec::new();
        super::serve_with(
            "get",
            super::HelperIo {
                stdin: &mut input,
                stdin_is_terminal: false,
                stdout: &mut output,
            },
            &router,
            &MapGrants::new(),
            &env,
            1,
            &[],
        )
        .unwrap();
        if supplied {
            assert_eq!(output, b"quit=1\n");
        } else {
            assert_eq!(
                output,
                format!(
                    "username=x-access-token\npassword={}\n",
                    agh_fake::token("dev")
                )
                .as_bytes()
            );
        }
    }
}
