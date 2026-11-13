// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The existing automatic-upkeep owner exercises an actual private signed-vault publication.

use super::*;
use aterm_json::{Value, json};

fn repository(id: u64, name: &str) -> Value {
    json!({"id": id, "name": name, "owner": {"id": 7, "login": "fixture-user"},
        "full_name": format!("fixture-user/{name}"), "private": true,
        "clone_url": format!("https://github.com/fixture-user/{name}.git"),
        "default_branch": "main", "size": 1, "fork": false, "archived": false})
}

fn refreshed(input: Input) -> Candidate {
    let catalog = RepositoryCatalog::parse(&input.catalog).unwrap();
    Candidate {
        bytes: catalog
            .refresh(
                "fixture-user",
                7,
                &[repository(2, "new-code")],
                RepositoryCatalogVisibility::TokenVisible,
            )
            .unwrap(),
        binding: input.binding,
        visibility: RepositoryCatalogVisibility::TokenVisible,
        observed: 1,
        observed_at: Instant::now(),
    }
}

fn wait_for_result(mailbox: &Arc<(Mutex<Mailbox>, Condvar)>) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if mailbox.0.lock().unwrap().result.is_some() {
            return;
        }
        assert!(
            Instant::now() < until,
            "the owned fetch never supplied its result"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Called by the existing shared-login automatic-upkeep owner, never a second test registration.
pub(in crate::shared_login) fn exercise_owned_upkeep() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let source = scratch.path().join("private.vault");
    let paths = Paths::for_home(&scratch.path().join("admin"));
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let mut kept = repository(1, "kept-code");
    kept.as_object_mut()
        .unwrap()
        .insert("required_submodules".into(), json!(["vendor/required"]));
    let initial = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": [kept.clone()]}))
    .unwrap();
    let opened = client::admin_open(&paths).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &initial).unwrap();
    doc.set_entry(&Audience::Everyone, TOKEN, b"github_pat_fixture_only")
        .unwrap();
    client::admin_publish(&paths, &opened, &doc).unwrap();
    let opened = client::admin_open(&paths).unwrap();
    assert!(allowed(&paths));
    let captured = Input::capture(&paths, &opened).unwrap();
    assert!(matches!(
        TokenKind::of(captured.token.as_slice()),
        Ok(TokenKind::FineGrained)
    ));
    super::protocol_tests::exercise(&captured);

    let mut upkeep = Upkeep::default();
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    upkeep.fetch = Arc::new(move |input, stopped| {
        counted.fetch_add(1, Ordering::SeqCst);
        assert!(!stopped.load(Ordering::Acquire));
        Ok(refreshed(input))
    });
    assert_eq!(
        upkeep
            .step(&paths, &opened, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        0
    );
    wait_for_result(&upkeep.mailbox);
    let before = std::fs::read(&source).unwrap();
    assert_eq!(upkeep.step(&paths, &opened, Instant::now()).unwrap(), 0);
    {
        let mail = upkeep.mailbox.0.lock().unwrap();
        assert!(
            matches!(&mail.result, Some(Ok(_))),
            "a spent callback retains its completed provider observation"
        );
        assert!(mail.pending.is_none() && mail.active.is_none());
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(&source).unwrap(),
        before,
        "a spent callback cannot publish"
    );
    // A publication unrelated to the catalog must survive the fresh reopen and exact-parent write.
    let current = client::admin_open(&paths).unwrap();
    let mut doc = current.doc.clone();
    doc.set_entry(&Audience::Everyone, "unrelated", b"retain this value")
        .unwrap();
    client::admin_publish(&paths, &current, &doc).unwrap();
    let current = client::admin_open(&paths).unwrap();
    assert_eq!(
        upkeep
            .step(&paths, &current, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        1
    );
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "fresh authority publishes the retained candidate without another provider fetch"
    );
    let published = client::admin_open(&paths).unwrap();
    let catalog = published
        .published_entry(&Audience::Everyone, NAME)
        .unwrap();
    let value: Value = aterm_json::from_slice(catalog.value.as_slice()).unwrap();
    let rows = value["repositories"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let retained = rows
        .iter()
        .find(|row| row["id"].as_u64() == Some(1))
        .unwrap();
    assert_eq!(
        retained["required_submodules"][0].as_str(),
        Some("vendor/required")
    );
    assert!(rows.iter().any(|row| row["id"].as_u64() == Some(2)));
    assert_eq!(
        published
            .published_entry(&Audience::Everyone, "unrelated")
            .unwrap()
            .value
            .as_slice(),
        b"retain this value"
    );
    assert_eq!(
        published
            .published_entry(&Audience::Everyone, TOKEN)
            .unwrap()
            .value
            .as_slice(),
        b"github_pat_fixture_only"
    );
    assert_eq!(
        upkeep
            .step(&paths, &published, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        0
    );
    {
        // Every admitted job remains pending, active or ready until a later drain. One locked
        // snapshot therefore proves no second fetch was queued, without an absence timeout.
        let mail = upkeep.mailbox.0.lock().unwrap();
        assert!(
            mail.pending.is_none() && mail.active.is_none() && mail.result.is_none(),
            "the catalog's own confirmed publication retains its ordinary refresh interval"
        );
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    let mut doc = published.doc.clone();
    doc.set_entry(&Audience::Everyone, "unrelated", b"newer unrelated value")
        .unwrap();
    client::admin_publish_conditional_until(
        &paths,
        &published,
        &doc,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let published = client::admin_open(&paths).unwrap();
    assert_eq!(
        upkeep
            .step(&paths, &published, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        0
    );
    {
        let mail = upkeep.mailbox.0.lock().unwrap();
        assert!(
            mail.pending.is_none() && mail.active.is_none() && mail.result.is_none(),
            "an unrelated vault version does not ask the provider again"
        );
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    // An actually published member, unlike an unrelated entry, owes an immediate read even
    // while the prior successful catalog's ordinary LOOK remains in the future.
    let recipient = aterm_secrets::age::Identity::from_secret_bytes([61; 32])
        .recipient()
        .to_bech32();
    let mut doc = published.doc.clone();
    doc.add_machine("fixture-new", &recipient).unwrap();
    client::admin_publish_approved(&paths, &published, &doc, &[recipient]).unwrap();
    let published = client::admin_open(&paths).unwrap();
    assert_eq!(
        upkeep
            .step(&paths, &published, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        0
    );
    wait_for_result(&upkeep.mailbox);
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert_eq!(
        upkeep
            .step(&paths, &published, Instant::now() + Duration::from_secs(60))
            .unwrap(),
        0
    );
    {
        let mail = upkeep.mailbox.0.lock().unwrap();
        assert!(
            mail.pending.is_none() && mail.active.is_none() && mail.result.is_none(),
            "a drained member-triggered read does not create a third fetch"
        );
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    drop(upkeep); // The actual owned idle worker is joined here.

    // Pause while a real owned fetch is active; a released late answer cannot publish or requeue.
    for change_source in [false, true] {
        let current = client::admin_open(&paths).unwrap();
        let late = refreshed(Input::capture(&paths, &current).unwrap());
        let mut upkeep = Upkeep::default();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        struct Release(Arc<(Mutex<bool>, Condvar)>);
        impl Drop for Release {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap() = true;
                self.0.1.notify_all();
            }
        }
        // Declared after upkeep so every assertion unwind releases the provider before Drop joins.
        let release = Release(Arc::clone(&gate));
        let (entered, receiving) = std::sync::mpsc::channel();
        upkeep.fetch = Arc::new(move |input, stopped| {
            entered.send(Arc::clone(&stopped)).unwrap();
            let mut go = gate.0.lock().unwrap();
            while !*go {
                go = gate.1.wait(go).unwrap();
            }
            Ok(refreshed(input))
        });
        assert_eq!(
            upkeep
                .step(&paths, &current, Instant::now() + Duration::from_secs(60))
                .unwrap(),
            0
        );
        let stopped = receiving
            .recv_timeout(Duration::from_secs(60))
            .expect("the provider never entered its owned fetch");
        if change_source {
            let mut newer_kept = kept.clone();
            newer_kept
                .as_object_mut()
                .unwrap()
                .insert("required_submodules".into(), json!(["vendor/new-policy"]));
            let newer = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
                "repositories": [newer_kept, repository(2, "new-code"), repository(3, "manual-code")]})).unwrap();
            let mut doc = current.doc.clone();
            doc.set_entry(&Audience::Everyone, NAME, &newer).unwrap();
            doc.set_entry(&Audience::Everyone, TOKEN, b"github_pat_newer_fixture_only")
                .unwrap();
            client::admin_publish_conditional_until(
                &paths,
                &current,
                &doc,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            let newer = client::admin_open(&paths).unwrap();
            assert_eq!(
                upkeep
                    .step(&paths, &newer, Instant::now() + Duration::from_secs(60))
                    .unwrap(),
                0
            );
            assert!(
                stopped.load(Ordering::Acquire),
                "changed source retires the active fetch"
            );
        }
        let before = std::fs::read(&source).unwrap();
        upkeep.pause();
        let was_stopped = stopped.load(Ordering::Acquire);
        let retired = {
            let mail = upkeep.mailbox.0.lock().unwrap();
            mail.pending.is_none() && mail.result.is_none() && upkeep.basis.is_none()
        };
        drop(release);
        drop(upkeep); // Releases, cancels and joins before examining the late answer's effects.
        assert!(was_stopped && retired);
        assert_eq!(
            std::fs::read(&source).unwrap(),
            before,
            "a paused late result publishes nothing"
        );
        if change_source {
            assert!(
                publish(
                    &paths,
                    &current,
                    late,
                    Instant::now() + Duration::from_secs(60)
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(&source).unwrap(),
                before,
                "stale capture cannot overwrite new policy or token"
            );
            let newer = client::admin_open(&paths).unwrap();
            assert_eq!(
                newer
                    .published_entry(&Audience::Everyone, TOKEN)
                    .unwrap()
                    .value
                    .as_slice(),
                b"github_pat_newer_fixture_only"
            );
            let value: Value = aterm_json::from_slice(
                newer
                    .published_entry(&Audience::Everyone, NAME)
                    .unwrap()
                    .value
                    .as_slice(),
            )
            .unwrap();
            let rows = value["repositories"].as_array().unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows.iter()
                    .find(|row| row["id"].as_u64() == Some(1))
                    .unwrap()["required_submodules"][0]
                    .as_str(),
                Some("vendor/new-policy")
            );
            assert!(rows.iter().any(|row| row["id"].as_u64() == Some(3)));
        }
    }

    // An administrator may deliberately publish an empty, account-bound catalog. A new
    // publisher instance must acquire its first visible repository without an old owner ID.
    let current = client::admin_open(&paths).unwrap();
    let empty = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": []}))
    .unwrap();
    let mut doc = current.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &empty).unwrap();
    client::admin_publish_conditional_until(
        &paths,
        &current,
        &doc,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let current = client::admin_open(&paths).unwrap();
    assert_eq!(RepositoryCatalog::parse(&empty).unwrap().owner_id(), None);
    let mut fresh = Upkeep::default();
    fresh.fetch = Arc::new(|input, stopped| {
        assert!(!stopped.load(Ordering::Acquire));
        Ok(refreshed(input))
    });
    let mailbox = Arc::clone(&fresh.mailbox);
    let mut application = crate::shared_login::recovery::with_catalog(fresh);
    // Exercise the actual Main composition, including offering and draining, so removing the
    // catalog call from the registered upkeep cannot leave this integration owner green.
    // This synthetic vault has no account-app catalog; those independent results are not this
    // repository proof. Its local signed repository publication below is decisive.
    let _ = application.step(&paths, &current, Instant::now() + Duration::from_secs(60));
    {
        let mail = mailbox.0.lock().unwrap();
        assert!(
            mail.pending.is_some() || mail.active.is_some() || mail.result.is_some(),
            "the application upkeep must offer its catalog work"
        );
    }
    wait_for_result(&mailbox);
    let _ = application.step(&paths, &current, Instant::now() + Duration::from_secs(60));
    let published = client::admin_open(&paths).unwrap();
    let value: Value = aterm_json::from_slice(
        published
            .published_entry(&Audience::Everyone, NAME)
            .unwrap()
            .value
            .as_slice(),
    )
    .unwrap();
    let rows = value["repositories"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"].as_u64(), Some(2));
    assert_eq!(rows[0]["owner"]["id"].as_u64(), Some(7));
    drop(application);
}
