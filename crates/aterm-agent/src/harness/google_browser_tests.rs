// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Instant;

/// A GOOGLE BROWSER CALL ITS HOST SLEPT THROUGH ENDS AT ITS NEXT LOOK (KI-1105-14): a call
/// given an hour, to a browser that reads it and never answers, whose host slept two hours
/// once it was sent, ends then. NEGATIVE CONTROL: a call that took what was left of its
/// deadline as one span, on the clock that stops in sleep, waited the hour out, which the
/// one-minute hang detector reports.
#[test]
fn a_browser_call_its_host_slept_through_ends_at_its_next_look() {
    const HOUR: Duration = Duration::from_secs(3600);
    let (said, called) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let seam = super::super::wake_deadline::SleepSeam::hold();
        let sleeper = seam.sleeper();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (mut commands, to_browser) = std::io::pipe().unwrap();
        let (from_browser, answers) = std::io::pipe().unwrap();
        let browser = std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            // A command ends at its NUL.
            while std::io::Read::read(&mut commands, &mut byte).unwrap() == 1 && byte[0] != 0 {}
            sleeper.slept(2 * HOUR);
            // Never answers while the call runs.
            let _ = released.recv();
            drop(answers);
        });
        let mut cdp = CdpClient::new(to_browser, from_browser);
        let got = call_within(
            &mut cdp,
            super::super::login_drive::Deadline::waking(WakeDeadline::after(HOUR), &Instant::now),
            HOUR,
            None,
            "Runtime.evaluate",
            json!({"expression":"1"}),
        );
        drop(release);
        browser.join().expect("stand-in browser");
        let _ = said.send(got);
    });
    let got = called
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the call waited on the clock that stops in sleep, past the deadline its host slept through");
    assert_eq!(got.err(), Some(Error::Protocol));
}

#[test]
fn pending_google_cookies_retain_work_until_the_identity_claim_retires() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse(
        "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
    ).unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let lifetime = super::super::account_work::Lifetime::default();
    let mut pending = pending_fixture(identity, root.path(), root.path(), b"synthetic cookies");
    pending._work = lifetime.enter();
    let pause = lifetime.pause().unwrap().try_wait().unwrap_err();
    assert!(identity.claim(root.path()).is_err());
    drop(pending);
    let drained = pause.try_wait().unwrap();
    let claim = identity.claim(root.path()).unwrap();
    assert!(drained.resume().is_ok());
    drop(claim);
}

#[test]
fn pending_google_cookies_do_not_recheck_a_proof_after_work_closes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse(
        "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
    ).unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let lifetime = super::super::account_work::Lifetime::default();
    let mut pending = pending_fixture(identity, root.path(), root.path(), b"synthetic cookies");
    pending._work = lifetime.enter();
    assert_eq!(
        super::super::google_browser_store::fingerprint(&pending.claim).unwrap(),
        None
    );
    pending.previous = Some([42; 32]);
    let checks = std::cell::Cell::new(0);
    let pause = std::cell::RefCell::new(None);
    let result = pending.commit_using(|_| {
        checks.set(checks.get() + 1);
        if checks.get() == 1 {
            *pause.borrow_mut() = lifetime.pause();
        }
        Ok(())
    });
    assert_eq!(result, Err(Error::Session));
    assert_eq!(checks.get(), 1);
    assert!(pause.into_inner().unwrap().try_wait().is_ok());
}

#[test]
fn recovered_google_cookies_keep_work_and_refuse_a_closed_commit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse(
        "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
    ).unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let lifetime = super::super::account_work::Lifetime::default();
    let pending = PendingSession::recover_scoped(
        identity,
        root.path(),
        root.path(),
        aterm_secrets::SecretBytes::from(b"synthetic cookies".as_slice()),
        None,
        lifetime.enter(),
    )
    .unwrap();
    let pause = lifetime.pause().unwrap().try_wait().unwrap_err();
    let checks = std::cell::Cell::new(0);
    assert_eq!(
        pending.commit_using(|_| {
            checks.set(checks.get() + 1);
            Err(Error::Session)
        }),
        Err(Error::Session)
    );
    assert_eq!(checks.get(), 0);
    assert!(pause.try_wait().is_ok());
    assert!(identity.claim(root.path()).is_ok());
}

#[test]
fn only_the_owned_main_frame_can_supply_a_same_tab_handoff() {
    let event = |frame: &str, disposition: &str| json!({"params":{"frameId":frame,"disposition":disposition,"url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=fixture"}});
    assert!(navigation_request(&event("main", "currentTab"), "main").is_some());
    for (frame, disposition) in [
        ("child", "currentTab"),
        ("main", "newTab"),
        ("main", "newWindow"),
        ("main", "download"),
    ] {
        assert!(navigation_request(&event(frame, disposition), "main").is_none());
    }
    assert!(navigation_request(&event("", "currentTab"), "").is_none());
    assert!(navigation_request(&json!({"params":{"frameId":"main"}}), "main").is_none());
}

#[test]
#[ignore = "uses the pinned headless browser and a local callback that clears its URL"]
fn native_committed_callback_survives_immediate_query_cleanup() {
    use std::io::{BufRead as _, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let registry=super::super::google_identity::Registry::parse("[[identities]]\nid='callback-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='callback-fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(Duration::from_secs(60)),
    )
    .unwrap();
    let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(deadline),
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&stream);
        for _ in 0..32 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        let body = "<html><script>history.replaceState({},'', '/oauth-callback');document.title='cleared';</script></html>";
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    });
    let original = format!("http://{address}/oauth-callback?code=fixture-code&state=fixture-state");
    let page = browser.open(&original).unwrap();
    let mut observed = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "callback did not clear its query"
        );
        observed |= browser
            .navigation_commits(&page)
            .unwrap()
            .iter()
            .any(|url| url == &original);
        if browser
            .evaluate(
                &page,
                "document.title==='cleared'&&location.search===''",
                false,
            )
            .ok()
            == Some(json!(true))
            && observed
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
    assert!(browser.navigation_commits(&page).unwrap().is_empty());
}

#[test]
#[ignore = "runs a same-tab navigation in the pinned full headless browser"]
fn native_main_frame_navigation_is_observed_before_the_destination() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='navigation'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='navigation'\nexpected_email='fixture@example.invalid'\n").unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(Duration::from_secs(60)),
    )
    .unwrap();
    let page = browser.open("about:blank").unwrap();
    browser
        .evaluate(
            &page,
            "setTimeout(()=>location.assign('about:blank?same-tab'),0);true",
            true,
        )
        .unwrap();
    loop {
        assert!(
            Instant::now() < deadline,
            "same-tab navigation event did not arrive"
        );
        browser.pages().unwrap();
        let requests = browser.navigation_requests(&page).unwrap();
        if !requests.is_empty() {
            assert_eq!(requests, ["about:blank?same-tab"]);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(browser.window_opens(&page).unwrap().is_empty());
    assert!(browser.navigation_requests(&page).unwrap().is_empty());
}

#[test]
fn target_admission_excludes_default_and_other_identity_contexts() {
    let own = json!({"type":"page","targetId":"popup","openerId":"parent","url":"https://accounts.google.com/signin","browserContextId":"own"});
    let info = page_info(&own, "own").unwrap();
    assert_eq!(info.target(), "popup");
    assert_eq!(info.url(), "https://accounts.google.com/signin");
    assert_eq!(info.opener(), Some("parent"));
    assert!(page_info(&own, "other").is_none());
    assert!(
        page_info(
            &json!({"type":"page","targetId":"default","url":"https://accounts.google.com/signin"}),
            "own"
        )
        .is_none()
    );
    assert!(page_info(&json!({"type":"iframe","targetId":"frame","url":"https://accounts.google.com/","browserContextId":"own"}), "own").is_none());
}

#[test]
fn session_cookie_admission_excludes_other_domains_partitions_and_expired_values() {
    let cookie = |domain: &str| json!({"name":"fixture","value":"secret-fixture","domain":domain,"path":"/","secure":true,"httpOnly":true,"expires":-1});
    let mut expired = cookie(".google.com");
    expired
        .as_object_mut()
        .unwrap()
        .insert("expires".into(), json!(1));
    let mut partitioned = cookie(".google.com");
    partitioned.as_object_mut().unwrap().insert(
        "partitionKey".into(),
        json!({"topLevelSite":"https://vendor.example"}),
    );
    let input = json!([
        cookie(".google.com"),
        cookie("accounts.google.com"),
        cookie(".google.com.evil.test"),
        cookie("vendor.example"),
        expired,
        partitioned
    ]);
    let accepted = google_cookies(&input).unwrap();
    assert_eq!(accepted.0.as_array().unwrap().len(), 2);
    let mut malformed = cookie(".google.com");
    malformed
        .as_object_mut()
        .unwrap()
        .insert("secure".into(), json!("yes"));
    assert!(google_cookies(&json!([malformed])).is_err());
    assert!(google_cookies(&json!({})).is_err());
}

#[test]
#[ignore = "verifies encrypted cookie restart through the pinned full browser"]
fn native_encrypted_session_survives_restart_without_plaintext_profile_cookies() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store_home = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='encrypted-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='encrypted-fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let browser_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let mut browser =
        MemoryBrowser::start(identity, root.path(), browser_home.as_deref(), deadline).unwrap();
    assert!(!browser.restore_session(store_home.path()).unwrap());
    browser.call(None,"Storage.setCookies",json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"encrypted-cookie-fixture","domain":".google.com","path":"/","secure":true,"httpOnly":true},{"name":"vendor","value":"excluded-fixture","domain":"example.invalid","path":"/"}]})).unwrap();
    browser.save_session(store_home.path()).unwrap();
    let scratch = browser._profile.path().to_owned();
    drop(browser);
    assert!(!scratch.exists());
    let mut restarted =
        MemoryBrowser::start(identity, root.path(), browser_home.as_deref(), deadline).unwrap();
    assert!(restarted.restore_session(store_home.path()).unwrap());
    let cookies = SecretValue(
        restarted
            .call(
                None,
                "Storage.getCookies",
                json!({"browserContextId":restarted.context}),
            )
            .unwrap(),
    );
    let values = cookies.0["cookies"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["name"].as_str(), Some("fixture"));
    assert_eq!(
        values[0]["value"].as_str(),
        Some("encrypted-cookie-fixture")
    );
    let default = SecretValue(
        restarted
            .call(None, "Storage.getCookies", json!({}))
            .unwrap(),
    );
    assert!(default.0["cookies"].as_array().unwrap().is_empty());
}

#[test]
#[ignore = "verifies deferred cookie commit and restart through the pinned full browser; account proof is synthetic"]
fn native_pending_session_survives_browser_exit_and_commits_after_proof() {
    let state = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let profile = aterm_tempfile::tempdir().unwrap();
    let text = "[[identities]]\nid='pending'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='pending'\nexpected_email='fixture@example.invalid'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='pending'\nexpected_email='fixture@example.invalid'\n";
    aterm_secrets::fsx::write_atomic(
        &state.path().join("google-identities.toml"),
        text.as_bytes(),
        0o600,
    )
    .unwrap();
    let registry = super::super::google_identity::Registry::parse(text).unwrap();
    let identity = registry.resolve("agy", "one").unwrap().0;
    let browser_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let mut browser =
        MemoryBrowser::start(identity, state.path(), browser_home.as_deref(), deadline).unwrap();
    browser.call(None,"Storage.setCookies",json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"pending-cookie","domain":".google.com","path":"/","secure":true,"httpOnly":true},{"name":"vendor","value":"excluded-cookie","domain":"example.invalid","path":"/"}]})).unwrap();
    let scratch = browser._profile.path().to_owned();
    let pending = browser.into_pending_session(home.path()).unwrap();
    assert!(
        !scratch.exists(),
        "cookie candidate retained the native browser"
    );
    assert!(
        identity.claim(state.path()).is_err(),
        "browser exit released the pending identity"
    );
    let path = pending.claim.profile().join("session.age");
    assert!(
        !path.exists(),
        "cookies were persisted before account proof"
    );
    let proof = super::super::agy_signin::publication_fixture(
        state.path(),
        profile.path(),
        "one",
        "fixture@example.invalid",
    );
    pending.commit(&proof).unwrap();
    let saved = std::fs::read(&path).unwrap();
    assert!(
        !saved
            .windows(b"pending-cookie".len())
            .any(|part| part == b"pending-cookie")
    );
    let kimi_store = super::super::accounts::Store::at_for(
        state.path(),
        crate::ai::connector::Cli::Kimi,
        home.path(),
    );
    let approver = super::super::headless_oauth::VendorApprover::with_mailbox(
        crate::ai::connector::Cli::Kimi,
        "one",
        &kimi_store.shared_dir(),
        home.path(),
        super::super::headless_oauth::MailboxConfig::default(),
    );
    assert_eq!(identity, registry.resolve("kimi-global", "one").unwrap().0);
    let mut restarted = MemoryBrowser::start(
        identity,
        &approver.google_state,
        browser_home.as_deref(),
        deadline,
    )
    .unwrap();
    assert!(restarted.restore_session(home.path()).unwrap());
    let cookies = SecretValue(
        restarted
            .call(
                None,
                "Storage.getCookies",
                json!({"browserContextId":restarted.context}),
            )
            .unwrap(),
    );
    assert_eq!(cookies.0["cookies"].as_array().unwrap().len(), 1);
    assert_eq!(
        cookies.0["cookies"][0]["value"].as_str(),
        Some("pending-cookie")
    );
    restarted.call(None,"Storage.setCookies",json!({"browserContextId":restarted.context,"cookies":[{"name":"fixture","value":"discarded-cookie","domain":".google.com","path":"/","secure":true,"httpOnly":true}]})).unwrap();
    let discarded = restarted.into_pending_session(home.path()).unwrap();
    drop(discarded);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        saved,
        "discarding a candidate replaced the committed session"
    );
    assert!(identity.claim(state.path()).is_ok());
    let mut active =
        MemoryBrowser::start(identity, state.path(), browser_home.as_deref(), deadline).unwrap();
    assert!(active.restore_session(home.path()).unwrap());
    active.call(None,"Storage.setCookies",json!({"browserContextId":active.context,"cookies":[{"name":"fixture","value":"older-live-context","domain":".google.com","path":"/","secure":true,"httpOnly":true}]})).unwrap();
    let newer = br#"[{"name":"fixture","value":"newer-publication","domain":".google.com","path":"/","secure":true}]"#;
    super::super::google_browser_store::save(
        home.path(),
        identity,
        active._identity.as_ref().unwrap(),
        newer,
    )
    .unwrap();
    let ciphertext = std::fs::read(&path).unwrap();
    let older = active.into_pending_session(home.path()).unwrap();
    older.commit(&proof).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        ciphertext,
        "a live context rolled a later snapshot back during completion"
    );
    let claim = identity.claim(state.path()).unwrap();
    assert_eq!(
        super::super::google_browser_store::load(home.path(), identity, &claim)
            .unwrap()
            .unwrap()
            .as_slice(),
        newer
    );
}

#[test]
#[ignore = "provisions the pinned full browser and verifies native context isolation"]
fn native_bootstrap_keeps_cookies_only_in_its_memory_context() {
    let root = aterm_tempfile::tempdir().unwrap();
    let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
    let identity = registry.resolve("fixture", "one").unwrap().0;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut browser = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(Duration::from_secs(300)),
    )
    .unwrap();
    let page = browser.open("about:blank").unwrap();
    assert_eq!(browser.evaluate(&page, "1+1", false).unwrap(), json!(2));
    browser.call(None, "Storage.setCookies", json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"memory-only","domain":"example.invalid","path":"/"}]})).unwrap();
    let cookies = browser
        .call(
            None,
            "Storage.getCookies",
            json!({"browserContextId":browser.context}),
        )
        .unwrap();
    assert_eq!(cookies["cookies"].as_array().unwrap().len(), 1);
    let default_cookies = browser.call(None, "Storage.getCookies", json!({})).unwrap();
    assert!(default_cookies["cookies"].as_array().unwrap().is_empty());
    let targets = browser.pages().unwrap();
    assert!(targets.iter().any(|target| target.target() == page.target));
    assert!(
        targets
            .iter()
            .all(|target| target.context == browser.context)
    );
    let scratch = browser._profile.path().to_owned();
    drop(browser);
    assert!(!scratch.exists());
    let mut restarted = MemoryBrowser::start(
        identity,
        root.path(),
        home.as_deref(),
        WakeDeadline::after(Duration::from_secs(60)),
    )
    .unwrap();
    let cookies = restarted
        .call(
            None,
            "Storage.getCookies",
            json!({"browserContextId":restarted.context}),
        )
        .unwrap();
    assert!(cookies["cookies"].as_array().unwrap().is_empty());
    let other = PageInfo {
        target: "forged".into(),
        opener: None,
        url: "https://accounts.google.com/".into(),
        context: "other".into(),
    };
    assert!(matches!(restarted.attach(&other), Err(Error::WrongContext)));
}
