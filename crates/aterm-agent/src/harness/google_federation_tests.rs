// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GOOGLE: &str = "https://accounts.google.com/o/oauth2/v2/auth?client_id=vendor-client&redirect_uri=https%3A%2F%2Fwww.kimi.ai%2Fgoogle-callback&scope=email%20profile&state=inner-state";
const HOSTS: &[&str] = &["auth.kimi.ai", "www.kimi.ai"];

#[test]
fn native_binding_checks_both_login_alias_and_canonical_identity() {
    let row = super::super::accounts::Row {
        email: Some("login-alias@example.com".into()),
        identity_email: Some("native-owner@example.com".into()),
        ..Default::default()
    };
    assert!(native_identity_matches(
        &row,
        Some("LOGIN-ALIAS@example.com"),
        "native-owner@example.com"
    ));
    assert!(!native_identity_matches(
        &row,
        Some("another-login@example.com"),
        "native-owner@example.com"
    ));
    assert!(!native_identity_matches(
        &row,
        Some("login-alias@example.com"),
        "another-owner@example.com"
    ));
    assert!(!native_identity_matches(
        &row,
        None,
        "native-owner@example.com"
    ));
}

#[test]
fn canonical_only_native_binding_requires_absent_alias_and_exact_identity() {
    let row = super::super::accounts::Row {
        identity_email: Some("native-owner@example.com".into()),
        ..Default::default()
    };
    assert!(native_identity_matches(
        &row,
        None,
        "NATIVE-OWNER@example.com"
    ));
    assert!(!native_identity_matches(
        &row,
        Some("native-owner@example.com"),
        "native-owner@example.com"
    ));
    assert!(!native_identity_matches(&row, None, "another@example.com"));
    assert!(!native_identity_matches(
        &super::super::accounts::Row::default(),
        None,
        "native-owner@example.com"
    ));
}

#[test]
fn google_federation_uses_one_harness_identity_root() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let store = super::super::accounts::Store::at_for(root.path(), Cli::Kimi, &home);
    let mut approver = VendorApprover::with_mailbox(
        Cli::Kimi,
        "one",
        &store.shared_dir(),
        &home,
        oauth::MailboxConfig::default(),
    );
    assert_eq!(approver.shared_dir, root.path().join("accounts"));
    assert_eq!(approver.google_state, root.path());
    let row = super::super::accounts::Row {
        label: "one".into(),
        ..Default::default()
    };
    let route = Cli::Kimi
        .accounts()
        .signin
        .as_ref()
        .unwrap()
        .route(None)
        .unwrap();
    let request = login_drive::Request {
        publication: None,
        enrollment: None,
        recovery: None,
        admission: None,
        cli: Cli::Kimi,
        row: &row,
        word: None,
        staging: &home,
        store: &store,
        route,
        hosts: HOSTS,
    };
    login_drive::Approver::begin(&mut approver, &request).unwrap();
    assert_eq!(approver.google_state, root.path());
    assert_eq!(
        approver.native_attempt.as_ref().unwrap().state(),
        root.path()
    );
}

#[test]
fn kimi_cookie_completion_requires_current_native_attempt_and_provider_proof() {
    use super::super::cli::shared_provider::kimi_proof_fixture;
    use super::super::{accounts, google_browser, google_browser_store};
    const EMAIL: &str = "person@example.com";
    const OLD: &[u8] = br#"[{"name":"fixture","value":"old-cookie","domain":".google.com","path":"/","secure":true}]"#;
    const NEW: &[u8] = br#"[{"name":"fixture","value":"new-cookie","domain":".google.com","path":"/","secure":true}]"#;
    for scenario in 0..12 {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let home = root.path().join("home");
        let staging = root.path().join("staging");
        for path in [&state, &home, &staging] {
            aterm_secrets::fsx::ensure_private_dir(path).unwrap();
        }
        let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        aterm_secrets::fsx::write_atomic(&state.join(REGISTRY_FILE), text.as_bytes(), 0o600)
            .unwrap();
        let registry = Registry::parse(text).unwrap();
        let identity = registry.resolve("kimi-global", "one").unwrap().0.clone();
        let store = accounts::Store::at_for(&state, Cli::Kimi, &home);
        let row = accounts::Row {
            label: "one".into(),
            email: Some(EMAIL.into()),
            vendor: accounts::Vendor {
                region: Some(Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        let route = Cli::Kimi
            .accounts()
            .signin
            .as_ref()
            .unwrap()
            .route(Some(crate::ai::connector::Flow::Device))
            .unwrap();
        let request = login_drive::Request {
            publication: None,
            enrollment: None,
            recovery: Some(login_drive::RecoveryKey([1; 32])),
            admission: None,
            cli: Cli::Kimi,
            row: &row,
            word: None,
            staging: &staging,
            store: &store,
            route,
            hosts: HOSTS,
        };
        let scope = FederationScope {
            attempt: NativeAttempt::capture(&request),
            provider: "kimi-global".into(),
            identity: identity.clone(),
            expected: EMAIL.into(),
            revision: registry.binding_revision("kimi-global", "one").unwrap(),
            region: Region::Global,
            work: None,
        };
        let claim = identity.claim(&state).unwrap();
        let snapshot = claim.profile().join("session.age");
        google_browser_store::save(&home, &identity, &claim, OLD).unwrap();
        let previous = std::fs::read(&snapshot).unwrap();
        drop(claim);
        let candidate_identity = if scenario == 8 {
            Registry::parse("[[identities]]\nid='other'\nemail='person@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='other'\nexpected_email='person@example.com'\n").unwrap().resolve("kimi-global", "one").unwrap().0.clone()
        } else {
            identity.clone()
        };
        let pending = PendingFederation {
            scope,
            native: None,
            session: Some(google_browser::pending_fixture(
                &candidate_identity,
                &state,
                &home,
                NEW,
            )),
        };
        let proof_staging = if scenario == 10 {
            root.path().join("another-native-home")
        } else {
            staging.clone()
        };
        let proof = kimi_proof_fixture(
            &proof_staging,
            if scenario == 9 {
                Region::MainlandCn
            } else {
                Region::Global
            },
            if scenario == 7 {
                "another@example.com"
            } else {
                EMAIL
            },
        );
        let changed = accounts::Row {
            label: "another".into(),
            ..row.clone()
        };
        let request = login_drive::Request {
            recovery: if scenario == 11 {
                Some(login_drive::RecoveryKey([11; 32]))
            } else {
                request.recovery
            },
            row: if scenario == 2 { &changed } else { &row },
            ..request
        };
        let outcome = if scenario == 1 {
            login_drive::Outcome::Failed
        } else {
            login_drive::Outcome::SignedIn {
                email: Some(EMAIL.into()),
                page: false,
            }
        };
        let deadline = if scenario == 3 {
            login_drive::Deadline::after(Duration::ZERO)
        } else {
            login_drive::Deadline::after(Duration::from_secs(60))
        };
        let result = pending.complete_using(&request, &outcome, deadline, |_| {
            assert!(
                !matches!(scenario, 1..=3 | 11),
                "failed, changed or expired attempt contacted Kimi"
            );
            assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
            assert!(
                candidate_identity.claim(&state).is_err(),
                "pending identity was released before provider proof"
            );
            if scenario == 4 {
                return Err("provider unavailable".into());
            }
            if scenario == 5 {
                aterm_secrets::fsx::write_atomic(&state.join(REGISTRY_FILE), b"changed", 0o600)
                    .unwrap();
            }
            if scenario == 6 {
                aterm_secrets::fsx::write_atomic(&staging.join("config.toml"), b"changed", 0o600)
                    .unwrap();
            }
            Ok(proof)
        });
        if scenario == 0 {
            result.unwrap();
            assert_ne!(std::fs::read(&snapshot).unwrap(), previous);
            let claim = identity.claim(&state).unwrap();
            assert_eq!(
                google_browser_store::load(&home, &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                NEW
            );
        } else {
            if scenario == 1 {
                result.unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
            assert!(candidate_identity.claim(&state).is_ok());
            if scenario == 8 {
                let claim = candidate_identity.claim(&state).unwrap();
                assert!(!claim.profile().join("session.age").exists());
            }
        }
    }
}

#[test]
fn inner_google_flow_is_distinct_and_refuses_other_clients_callbacks_and_scopes() {
    let flow = InnerFlow::admit(GOOGLE, HOSTS).unwrap();
    assert!(flow.admits_google("https://accounts.google.com/v3/signin/identifier", HOSTS));
    assert!(flow.is_callback("https://www.kimi.ai/google-callback?code=value&state=inner-state"));
    for url in [
        GOOGLE.replace("vendor-client", "other-client"),
        GOOGLE.replace("inner-state", "outer-state"),
        GOOGLE.replace(
            "scope=email%20profile",
            "scope=email%20profile%20https%3A%2F%2Fmail.google.com%2F",
        ),
        GOOGLE.replace("www.kimi.ai", "www.kimi.com"),
        GOOGLE.replace("accounts.google.com", "accounts.google.com.evil.test"),
    ] {
        assert!(!flow.admits_google(&url, HOSTS));
    }
    // Kimi's observed Google request omits state. Retain it when supplied;
    // an absent state still requires this owned flow and the native account proof.
    let stateless = InnerFlow::admit(&GOOGLE.replace("&state=inner-state", ""), HOSTS).unwrap();
    assert!(stateless.is_callback("https://www.kimi.ai/google-callback?code=value"));
    for url in [
        "https://www.kimi.ai/google-callback?state=inner-state",
        "https://www.kimi.ai/google-callback?code=&state=inner-state",
        "https://www.kimi.ai/google-callback?code=a&code=b&state=inner-state",
        "https://www.kimi.ai/google-callback?code=value&state=outer-state",
        "https://www.kimi.ai/google-callback?code=value",
        "https://www.kimi.ai/google-callback?error=denied&state=inner-state",
        "https://www.kimi.ai/elsewhere?code=value&state=inner-state",
    ] {
        assert!(!flow.is_callback(url));
    }
    for url in [
        GOOGLE.replace(
            "redirect_uri=",
            "redirect_uri=https%3A%2F%2Fevil.test%2F&unused=",
        ),
        GOOGLE.replace(
            "scope=email%20profile",
            "scope=https%3A%2F%2Fmail.google.com%2F",
        ),
        format!("{GOOGLE}&client_id=other-client"),
    ] {
        assert!(InnerFlow::admit(&url, HOSTS).is_none());
    }
    // The observed Google SDK flow can omit state. Its context and opener remain mandatory.
    assert!(InnerFlow::admit(&GOOGLE.replace("&state=inner-state", ""), HOSTS).is_some());
    let mut pending = InnerFlow::admit(GOOGLE, HOSTS);
    assert!(
        bind_target(
            &mut pending,
            &GOOGLE.replace("vendor-client", "swapped-client"),
            HOSTS
        )
        .is_none()
    );
    assert!(
        pending.is_some(),
        "a mismatched target must not consume the observed binding"
    );
    assert!(
        bind_target(
            &mut pending,
            "https://accounts.google.com/v3/signin/identifier",
            HOSTS
        )
        .is_some()
    );
    assert!(pending.is_none());
}

#[test]
fn configured_binding_is_exact_and_an_expired_handoff_does_not_launch_a_browser() {
    use crate::ai::connector::Completion;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut approver = VendorApprover::with_mailbox(
        super::super::accounts::Cli::Kimi,
        "work",
        root.path(),
        root.path(),
        oauth::MailboxConfig::default(),
    );
    let store = super::super::accounts::Store::at_for(root.path(), Cli::Kimi, root.path());
    let row = super::super::accounts::Row {
        label: "work".into(),
        email: Some("person@example.com".into()),
        vendor: super::super::accounts::Vendor {
            region: Some(Region::Global),
            ..Default::default()
        },
        ..Default::default()
    };
    let route = Cli::Kimi
        .accounts()
        .signin
        .as_ref()
        .unwrap()
        .route(None)
        .unwrap();
    approver.native_attempt = Some(NativeAttempt::capture(&login_drive::Request {
        publication: None,
        enrollment: None,
        recovery: None,
        admission: None,
        cli: Cli::Kimi,
        row: &row,
        word: None,
        route,
        staging: root.path(),
        store: &store,
        hosts: HOSTS,
    }));
    let handoff = Handoff {
        url: "https://www.kimi.ai/login?user_code=ABCD",
        code: Some("ABCD"),
        completion: Completion::Device,
        hosts: HOSTS,
        email: Some("person@example.com"),
        finished: &|| false,
        deadline: login_drive::Deadline::after(Duration::ZERO),
    };
    assert!(configured(&mut approver, &handoff).is_none());
    let text = "[[identities]]\nid='one'\nemail='person@example.com'\ncredential_ref='missing-google-entry'\n[[bindings]]\nprovider='kimi-global'\naccount='work'\nidentity='one'\nexpected_email='person@example.com'\n";
    aterm_uds::private_file::write_private(root.path(), REGISTRY_FILE, text.as_bytes()).unwrap();
    assert!(matches!(
        login_drive::Approver::approve(&mut approver, &handoff),
        Approval::NotRun(_)
    ));
    assert!(
        !root.path().join("google-identities").exists(),
        "an expired handoff must not start a browser"
    );
    let wrong = Handoff {
        email: Some("other@example.com"),
        ..handoff
    };
    assert!(matches!(
        configured(&mut approver, &wrong),
        Some(Approval::Refused(_))
    ));
    approver.google_account.1 = "other".into();
    assert!(configured(&mut approver, &handoff).is_none());
    assert_eq!(
        provider_key("kimi", &["www.kimi.com"]),
        Some("kimi-mainland-cn")
    );
    assert_eq!(provider_key("kimi", &["www.kimi.com", "www.kimi.ai"]), None);
}

/// THE GOOGLE SCRIPTS, RUN IN THE PINNED BROWSER ON FIXTURE PAGES (about:blank, no network):
/// each acts only on its own origin and identity and submits once; a check (a widget shown or
/// hidden, its button enabled or not; "Verify it's you") is answered as one with nothing ticked,
/// filled or pressed; the vendor card with no check is ticked, then pressed once, in the
/// evaluation that read it; a layer over the button takes the press point and nothing is pressed.
/// NEGATIVE CONTROL: with `select_google`'s `elementFromPoint` test removed, the press goes
/// through the layer: `` a press went through a layer over the button ``.
#[test]
#[ignore = "runs script behavior in the pinned full headless browser"]
fn browser_actions_enforce_origin_identity_single_submission_and_leave_challenges_alone() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = Registry::parse("[[identities]]\nid='fixture'\nemail='person@example.com'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='fixture'\nexpected_email='person@example.com'\n").unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(Duration::from_secs(60)),
    )
    .unwrap();
    let page = browser.open("about:blank").unwrap();
    for (field, path) in [
        (
            "<input type=password><button id=passwordNext>Next</button>",
            "/v3/signin/challenge/pwd",
        ),
        (
            "<input id=totpPin><button id=totpNext>Next</button>",
            "/v3/signin/challenge/totp",
        ),
    ] {
        browser.evaluate(&page, &format!("document.body.innerHTML='<span id=profileIdentifier>person@example.com</span>{field}';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"), false).unwrap();
        let action = google_action(
            "person@example.com",
            None,
            Authenticator::Unavailable,
            true,
            false,
        );
        let script = format!(
            "((location)=>{action})({{origin:'https://accounts.google.com',pathname:{}}})",
            json!(path)
        );
        assert_eq!(
            browser.evaluate(&page, &script, true).unwrap(),
            json!("credentials")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
        assert_eq!(
            browser
                .evaluate(&page, "document.querySelector('input').value", false)
                .unwrap(),
            json!("")
        );
    }
    browser.evaluate(&page,"document.body.innerHTML='<span id=profileIdentifier>person@example.com</span><input id=totpPin><button type=button id=totpNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true",false).unwrap();
    let submitted = google_action(
        "person@example.com",
        Some("fixture-only"),
        Authenticator::Submitted,
        true,
        true,
    );
    let submitted = format!(
        "((location)=>{submitted})({{origin:'https://accounts.google.com',pathname:'/v3/signin/challenge/totp'}})"
    );
    assert_eq!(
        browser.evaluate(&page, &submitted, true).unwrap(),
        json!("wait")
    );
    browser
        .evaluate(
            &page,
            "document.querySelector('#totpPin').setAttribute('aria-invalid','true');true",
            false,
        )
        .unwrap();
    assert_eq!(
        browser.evaluate(&page, &submitted, true).unwrap(),
        json!("totp_rejected")
    );
    assert_eq!(
        browser.evaluate(&page, "window.actions", false).unwrap(),
        json!(0)
    );
    for (origin, account, expected, actions) in [
        (
            "https://accounts.google.com",
            "person@example.com",
            "consent",
            1,
        ),
        (
            "https://accounts.google.com",
            "other@example.com",
            "wait",
            0,
        ),
        (
            "https://accounts.google.com.evil.test",
            "person@example.com",
            "refused",
            0,
        ),
    ] {
        browser.evaluate(&page, &format!("document.body.innerHTML='<span id=profileIdentifier>{account}</span><button id=submit_approve_access>Continue</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"), false).unwrap();
        let action = google_action(
            "person@example.com",
            None,
            Authenticator::Unavailable,
            true,
            true,
        );
        let script = format!(
            "((location)=>{action})({{origin:{},pathname:'/signin/oauth/consent'}})",
            json!(origin)
        );
        assert_eq!(
            browser.evaluate(&page, &script, true).unwrap(),
            json!(expected)
        );
        if expected == "consent" {
            assert_eq!(
                browser.evaluate(&page, &script, true).unwrap(),
                json!("wait")
            );
        }
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(actions)
        );
    }
    for (origin, identity, sent, state, actions) in [
        (
            "https://accounts.google.com",
            "person@example.com",
            false,
            "password",
            1,
        ),
        (
            "https://accounts.google.com",
            "person@example.com",
            true,
            "wait",
            0,
        ),
        (
            "https://accounts.google.com",
            "other@example.com",
            false,
            "refused",
            0,
        ),
        (
            "https://accounts.google.com.evil.test",
            "person@example.com",
            false,
            "refused",
            0,
        ),
    ] {
        let fixture = format!(
            "document.body.innerHTML='<span id=profileIdentifier>{identity}</span><input type=password><button id=passwordNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"
        );
        browser.evaluate(&page, &fixture, false).unwrap();
        let script = google_action(
            "person@example.com",
            Some("fake-fixture-only"),
            Authenticator::Unavailable,
            false,
            sent,
        );
        let scoped = format!(
            "((location)=>{script})({{origin:{},pathname:'/v3/signin/challenge/pwd'}})",
            json!(origin)
        );
        assert_eq!(
            browser.evaluate(&page, &scoped, true).unwrap(),
            json!(state)
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(actions)
        );
    }
    // Google's "Verify it's you" page, its mailed code's field with no such words around it
    // included, is a check: it is answered as one, and nothing on it is filled or pressed.
    for (account, text) in [
        (
            "person@example.com",
            "A verification code was sent by email to r***@example.com",
        ),
        ("other@example.com", "Email sent to rescue@example.com"),
        ("person@example.com", "Verify it&#39;s you"),
    ] {
        let fixture = format!(
            "document.body.innerHTML='<span id=profileIdentifier>{account}</span><form><p>{text}</p><input id=idvPin><button type=button id=idvPreregisteredEmailNext>Next</button></form>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"
        );
        browser.evaluate(&page, &fixture, false).unwrap();
        let script = google_action(
            "person@example.com",
            Some("fake-fixture-only"),
            Authenticator::Unavailable,
            true,
            true,
        );
        let scoped = format!(
            "((location)=>{script})({{origin:'https://accounts.google.com',pathname:'/v3/signin/challenge/ipe'}})"
        );
        assert_eq!(
            browser.evaluate(&page, &scoped, true).unwrap(),
            json!("check:block_page:Google")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
    }
    // The pages below carry no check: the last "Verify it's you" page is taken down first, or
    // it, not the path, would answer.
    browser
        .evaluate(
            &page,
            "document.body.innerHTML='<span id=profileIdentifier>person@example.com</span>';window.actions=0;true",
            false,
        )
        .unwrap();
    for path in [
        "/signin/recovery",
        "/signup",
        "/v3/signin/challenge/sms",
        "/v3/signin/challenge/ipp",
    ] {
        let script = google_action(
            "person@example.com",
            Some("fake-fixture-only"),
            Authenticator::Unavailable,
            true,
            true,
        );
        let scoped = format!(
            "((location)=>{script})({{origin:'https://accounts.google.com',pathname:{}}})",
            json!(path)
        );
        assert_eq!(
            browser.evaluate(&page, &scoped, true).unwrap(),
            json!("challenge")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
    }
    browser.evaluate(&page,"document.body.innerHTML='<input type=password autocomplete=new-password><button id=passwordNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true",false).unwrap();
    let script = google_action(
        "person@example.com",
        Some("fake-fixture-only"),
        Authenticator::Unavailable,
        false,
        false,
    );
    let scoped = format!(
        "((location)=>{script})({{origin:'https://accounts.google.com',pathname:'/signin'}})"
    );
    assert_eq!(
        browser.evaluate(&page, &scoped, true).unwrap(),
        json!("challenge")
    );
    assert_eq!(
        browser.evaluate(&page, "window.actions", false).unwrap(),
        json!(0)
    );
    // A vendor card with a human check, shown or not, its button enabled or not: nothing is
    // ticked or pressed, and the check is named.
    for widget in [
        "<label><input type=checkbox id=captcha>I am human</label>",
        "<iframe src=\\'https://newassets.hcaptcha.com/captcha/v1/x\\'></iframe>",
        "<textarea name=g-recaptcha-response style=display:none></textarea>",
        "<div class=cf-turnstile data-sitekey=fixture></div>",
        "<div hidden><iframe src=\\'https://client-api.arkoselabs.com/fc/x\\'></iframe></div>",
    ] {
        for button in [
            "<button>Continue with Google</button>",
            "<button disabled>Continue with Google</button>",
        ] {
            browser.evaluate(&page, &format!("document.body.innerHTML='<label><input type=checkbox id=terms>I agree to the terms and privacy policy</label>{widget}{button}';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"), false).unwrap();
            assert!(
                matches!(
                    choose_google(&mut browser, &page, "about:blank").unwrap(),
                    Chosen::Check(_)
                ),
                "{widget} {button}"
            );
            assert_eq!(
                browser
                    .evaluate(
                        &page,
                        "({terms:document.querySelector('#terms').checked,actions:window.actions})",
                        false
                    )
                    .unwrap(),
                json!({"terms":false,"actions":0}),
                "{widget} {button}"
            );
        }
    }
    // CONTROL: the same card with no check is ticked, then pressed once.
    browser.evaluate(&page, "document.body.innerHTML='<label><input type=checkbox id=terms>I agree to the terms and privacy policy</label><button>Continue with Google</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true", false).unwrap();
    assert_eq!(
        choose_google(&mut browser, &page, "about:blank").unwrap(),
        Chosen::Waiting
    );
    assert_eq!(
        choose_google(&mut browser, &page, "about:blank").unwrap(),
        Chosen::Pressed
    );
    assert_eq!(
        browser
            .evaluate(
                &page,
                "({terms:document.querySelector('#terms').checked,actions:window.actions})",
                false
            )
            .unwrap(),
        json!({"terms":true,"actions":1})
    );
    // A layer over the button takes the press point: nothing is pressed under it.
    browser.evaluate(&page, "document.body.innerHTML='<button>Continue with Google</button><div style=\"position:fixed;left:0;top:0;width:100vw;height:100vh;z-index:9\"></div>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true", false).unwrap();
    assert_eq!(
        choose_google(&mut browser, &page, "about:blank").unwrap(),
        Chosen::Waiting,
        "a press went through a layer over the button"
    );
    assert_eq!(
        browser.evaluate(&page, "window.actions", false).unwrap(),
        json!(0)
    );
}

#[test]
#[ignore = "contacts Kimi anonymously through the pinned headless browser"]
fn live_kimi_google_transition_preserves_context_and_vendor_callback() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = Registry::parse("[[identities]]\nid='anonymous-probe'\nemail='probe@example.invalid'\n[[bindings]]\nprovider='kimi-global'\naccount='probe'\nidentity='anonymous-probe'\nexpected_email='probe@example.invalid'\n").unwrap();
    let identity = registry.resolve("kimi-global", "probe").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(deadline.saturating_duration_since(Instant::now())),
    )
    .unwrap();
    let page = browser.open("https://www.kimi.ai/login").unwrap();
    let mut last_select = Instant::now() - Duration::from_secs(2);
    let mut clicks = 0;
    let mut pending: Option<InnerFlow> = None;
    let mut observed = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "anonymous Google transition did not complete after {clicks} pointer clicks; pages={observed:?}"
        );
        for url in browser.window_opens(&page).unwrap() {
            if login_drive::https_host(&url).as_deref() == Some("accounts.google.com") {
                println!(
                    "anonymous Kimi Google request: state_present={} callback_present={}",
                    !oauth::query_values(&url, "state").is_empty(),
                    oauth::query_values(&url, "redirect_uri").len() == 1
                );
            }
            if let Some(flow) = InnerFlow::admit(&url, HOSTS) {
                pending = Some(flow);
            }
        }
        observed.clear();
        for info in browser.pages().unwrap() {
            observed.push((
                login_drive::https_host(info.url()),
                info.opener() == Some(page.target()),
            ));
            if info.target() != page.target() && info.opener() != Some(page.target()) {
                continue;
            }
            let inner = bind_target(&mut pending, info.url(), HOSTS);
            if let Some(inner) = inner {
                assert_eq!(inner.callback, "https://www.kimi.ai/google-callback");
                return;
            }
        }
        if last_select.elapsed() >= Duration::from_secs(2) {
            let view = read_page(&mut browser, &page).unwrap();
            assert!(
                !oauth::bot_check(&view),
                "anonymous vendor page requires a security check"
            );
            if login_drive::pinned(&view.url, HOSTS)
                && choose_google(&mut browser, &page, &view.url).unwrap() == Chosen::Pressed
            {
                clicks += 1;
            }
            last_select = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
struct NativeClaudeProbe(aterm_uds::spawnfd::Child);

#[cfg(target_os = "macos")]
impl Drop for NativeClaudeProbe {
    fn drop(&mut self) {
        if let Ok(pid) = i32::try_from(self.0.id()) {
            let _ = aterm_uds::process::kill_group_until_empty(pid);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "macos")]
fn native_claude_probe(
    program: &std::path::Path,
    home: &std::path::Path,
    deadline: Instant,
) -> (NativeClaudeProbe, aterm_secrets::SecretBytes) {
    use std::io::Read;
    use std::os::fd::AsFd;
    aterm_secrets::fsx::ensure_private_dir(home).unwrap();
    let (mut reader, writer) = aterm_uds::pipe().unwrap();
    let mut command = aterm_uds::spawnfd::Command::new(login_drive::SANDBOX_EXEC);
    command
        .args(["-p", login_drive::NO_DESK])
        .arg(program)
        .args([
            "auth",
            "login",
            "--claudeai",
            "--email",
            "probe@example.invalid",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home.join("config"))
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", home.join("secure"))
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("BROWSER", "/usr/bin/false")
        .env("TERM", "dumb")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(writer.try_clone().unwrap())
        .stderr(writer);
    let child = NativeClaudeProbe(command.spawn_in_dedicated_process_group().unwrap());
    drop(command);
    let mut output = aterm_secrets::SecretBytes::from(Vec::new());
    loop {
        aterm_uds::ready::until(reader.as_fd(), aterm_uds::ready::Interest::Read, deadline)
            .expect("native Claude printed no authorization request");
        let mut chunk = [0; 4096];
        // A ready pipe read has one reader; the child cannot consume its bytes.
        let count = reader.read(&mut chunk).unwrap();
        assert!(
            count != 0,
            "native Claude closed before an authorization request"
        );
        let mut bytes = output.as_slice().to_vec();
        bytes.extend_from_slice(&chunk[..count]);
        assert!(
            bytes.len() <= 64 * 1024,
            "native Claude exceeded the probe output bound"
        );
        output = aterm_secrets::SecretBytes::from(bytes);
        let text = String::from_utf8_lossy(output.as_slice());
        let super::super::open_url::Pin::Endpoints(endpoints) = super::super::open_url::CLAUDE_PIN
        else {
            unreachable!()
        };
        for (endpoint, _) in endpoints {
            if let Some(start) = text.find(&format!("{endpoint}?")) {
                let tail = &text[start..];
                if let Some(end) = tail.find(|c: char| c.is_whitespace() || c.is_control()) {
                    let url = &tail[..end];
                    assert!(
                        FlowBound::of(url).is_ok(),
                        "native Claude request has no admitted outer binding"
                    );
                    println!(
                        "native Claude request captured on host={:?}",
                        login_drive::https_host(url)
                    );
                    return (child, aterm_secrets::SecretBytes::from(url.to_owned()));
                }
            }
        }
    }
}

#[test]
#[ignore = "contacts Claude anonymously through the pinned headless browser"]
fn live_claude_google_transition_observes_vendor_callback() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = Registry::parse("[[identities]]\nid='anonymous-probe'\nemail='probe@example.invalid'\n[[bindings]]\nprovider='claude'\naccount='probe'\nidentity='anonymous-probe'\nexpected_email='probe@example.invalid'\n").unwrap();
    let identity = registry.resolve("claude", "probe").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = Instant::now() + Duration::from_secs(60);
    #[cfg(target_os = "macos")]
    let native = std::env::var_os("ATERM_TEST_CLAUDE").map(|program| {
        native_claude_probe(
            std::path::Path::new(&program),
            &root.path().join("native-home"),
            deadline,
        )
    });
    #[cfg(target_os = "macos")]
    let start = native
        .as_ref()
        .map(|(_, url)| url.as_str().unwrap())
        .unwrap_or("https://claude.ai/login");
    #[cfg(not(target_os = "macos"))]
    let start = "https://claude.ai/login";
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(deadline.saturating_duration_since(Instant::now())),
    )
    .unwrap();
    let page = browser.open(start).unwrap();
    let mut last_select = Instant::now() - Duration::from_secs(2);
    let mut clicks = 0;
    let mut observed = Vec::new();
    let mut challenge_seen = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "anonymous Claude transition did not complete after {clicks} clicks; challenge_seen={challenge_seen}; hosts={observed:?}"
        );
        let mut urls = browser.window_opens(&page).unwrap();
        observed.clear();
        for info in browser.pages().unwrap() {
            observed.push(login_drive::https_host(info.url()));
            if info.target() == page.target() || info.opener() == Some(page.target()) {
                urls.push(info.url().to_owned());
            }
        }
        for url in urls {
            if login_drive::https_host(&url).as_deref() != Some("accounts.google.com") {
                continue;
            }
            let callback = oauth::query_values(&url, "redirect_uri");
            if callback.len() == 1 {
                let callback = callback[0].split(['?', '#']).next().unwrap();
                println!(
                    "anonymous Claude Google callback={callback} client_present={} state_present={}",
                    oauth::query_values(&url, "client_id").len() == 1,
                    oauth::query_values(&url, "state").len() == 1
                );
                assert!(callback.starts_with("https://"));
                return;
            }
        }
        if last_select.elapsed() >= Duration::from_secs(2) {
            let view = read_page(&mut browser, &page).unwrap();
            if oauth::bot_check(&view) {
                challenge_seen = true;
                // Ordinary page loading may clear a challenge by itself. Touch
                // nothing while it is present; never press a verification widget.
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if super::super::open_url::CLAUDE_PIN.admits(&view.url)
                && choose_google(&mut browser, &page, &view.url).unwrap() == Chosen::Pressed
            {
                clicks += 1;
            }
            last_select = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// EVERY GOOGLE SCRIPT READS THE PAGE FOR A CHECK BEFORE IT TOUCHES ANYTHING: the account
/// chooser, the identifier, password and authenticator fills, the consent press and the
/// vendor's `Continue with Google` all follow the shared predicate in the same evaluation, and
/// a check's answer names it. Google's own captcha and its "Verify it's you" field are checks.
/// The vendor's button is pressed in that evaluation, after the point at its centre was found
/// to be the button itself; an answer of where it is presses nothing later.
/// NEGATIVE CONTROL: with `select_google` answering the button's centre for a press sent by a
/// later command (the branch's first form), `` a press left the evaluation that read the
/// check ``.
#[test]
fn every_google_script_reads_the_check_before_it_touches_anything() {
    let action = google_action(
        "ada@x.example",
        Some("fixture-only"),
        Authenticator::Ready("123456"),
        false,
        false,
    );
    let select = select_google("https://www.kimi.ai/login");
    for (script, met) in [
        (&action, "if(met) return 'check:'+met.kind+':'+met.name;"),
        (&select, "if(met) return {check: met};"),
    ] {
        assert!(script.contains(oauth::CHECK_PAGE_JS));
        let at = script.find(met).expect("the check's answer");
        for (touch, _) in script.match_indices(".click(") {
            assert!(at < touch, "a press before the check");
        }
        for (touch, _) in script.match_indices("fill(") {
            assert!(at < touch, "a fill before the check");
        }
    }
    let press = select
        .find("button.click();")
        .expect("a press left the evaluation that read the check");
    let hit = select
        .find("document.elementFromPoint(x,y)")
        .expect("the hit test");
    assert!(hit < press, "the press comes before its hit test");
    assert!(!select.contains("return {x,y}"), "{select}");
    assert_eq!(chosen_of(&json!({"pressed": true})), Chosen::Pressed);
    for answer in [
        json!({"x": 10.0, "y": 20.0}),
        json!(false),
        json!({"pressed": false}),
    ] {
        assert_eq!(chosen_of(&answer), Chosen::Waiting, "{answer}");
    }
    let captcha = action
        .find("return 'check:human_check:captcha';")
        .expect("Google's captcha");
    let verify = action
        .find("return 'check:block_page:Google';")
        .expect("Verify it's you");
    let chooser = action.find("choice.click()").expect("the chooser's press");
    assert!(captcha < chooser && verify < chooser);
    assert!(
        !action.contains("idvPinNext"),
        "the mailed code is never typed"
    );
    assert_eq!(
        action_check("check:human_check:hCaptcha"),
        Some(oauth::CheckMet::HumanCheck("hCaptcha".into()))
    );
    assert_eq!(
        action_check("check:block_page:Google"),
        Some(oauth::CheckMet::BlockPage)
    );
    assert_eq!(
        action_check("check:made-up"),
        Some(oauth::CheckMet::HumanCheck("captcha".into()))
    );
    assert_eq!(action_check("challenge"), None);
}
