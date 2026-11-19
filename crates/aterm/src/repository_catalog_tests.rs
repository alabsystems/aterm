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
    let bytes = catalog
        .refresh(
            "fixture-user",
            7,
            &[repository(2, "new-code")],
            RepositoryCatalogVisibility::TokenVisible,
        )
        .unwrap();
    Candidate {
        changes: catalog.changes(&bytes).unwrap(),
        bytes,
        binding: input.binding,
        visibility: RepositoryCatalogVisibility::TokenVisible,
        observed: 1,
        observed_at: Instant::now(),
    }
}

/// What `body` answers, and the `aterm_log` records at info or more severe it wrote on this
/// thread ([`crate::test_log::logged_on_this_thread`]).
fn logged_on_this_thread<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    crate::test_log::logged_on_this_thread(aterm_log::Level::Info, body)
}

fn sized(id: u64, name: &str, size: u64) -> Value {
    let mut repo = repository(id, name);
    repo.as_object_mut()
        .unwrap()
        .insert("size".into(), size.into());
    repo
}

/// What a look that signs nothing answers: no publication and no catalog line in the log.
const NOTHING: (usize, Vec<String>) = (0, Vec::new());

/// Keys GitHub's REST row carries that no code reads, as [`whole`] adds them.
const PROVIDER_SHAPED: [&str; 8] = [
    "branches_url",
    "commits_url",
    "homepage",
    "html_url",
    "issues_url",
    "license",
    "permissions",
    "pushed_at",
];

/// `repo` with part of what GitHub's whole REST row carries beside the fields a refresh takes
/// ([`PROVIDER_SHAPED`], and `avatar_url` in its owner), at values that differ with `mark`.
fn whole(mut repo: Value, mark: &str) -> Value {
    let api = format!("https://api.github.invalid/{mark}/repos/fixture-user/code");
    let fields = repo.as_object_mut().unwrap();
    for (key, value) in [
        (
            "branches_url",
            Value::from(format!("{api}/branches{{/branch}}")),
        ),
        ("commits_url", Value::from(format!("{api}/commits{{/sha}}"))),
        ("homepage", Value::Null),
        (
            "html_url",
            Value::from(format!("https://github.invalid/{mark}")),
        ),
        (
            "issues_url",
            Value::from(format!("{api}/issues{{/number}}")),
        ),
        ("license", Value::Null),
        (
            "permissions",
            json!({"admin": true, "pull": true, "push": mark == "saved"}),
        ),
        ("pushed_at", Value::from(format!("{mark}-pushed"))),
    ] {
        fields.insert(key.into(), value);
    }
    fields
        .get_mut("owner")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "avatar_url".into(),
            Value::from(format!("https://avatars.invalid/{mark}")),
        );
    repo
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

/// A gate a scripted provider waits on until the test opens it.
type Gate = Arc<(Mutex<bool>, Condvar)>;

fn gate() -> Gate {
    Arc::new((Mutex::new(false), Condvar::new()))
}

fn open(gate: &Gate) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

fn wait(gate: &Gate) {
    let mut go = gate.0.lock().unwrap();
    while !*go {
        go = gate.1.wait(go).unwrap();
    }
}

/// Opens its gates when dropped. Declared after the upkeep, it drops first, so an assertion's
/// unwind lets a provider waiting on a gate return before the upkeep's Drop joins its worker.
struct Release(Vec<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.iter().for_each(open);
    }
}

/// Spends what is left of the wait for the next look, as its running out does: the next step
/// with no read in flight asks the provider for one.
fn spend_the_wait(upkeep: &mut Upkeep) {
    upkeep.due = Some(Instant::now());
}

/// The reads the upkeep holds, from one locked look at its mailbox: queued, being made, or made
/// and waiting for a step. A read no pause retired is in one of the three until a step drains
/// it, so the look needs no absence timeout.
fn jobs(upkeep: &Upkeep) -> usize {
    let mail = upkeep.mailbox.0.lock().unwrap();
    [
        mail.pending.is_some(),
        mail.active.is_some(),
        mail.result.is_some(),
    ]
    .into_iter()
    .filter(|held| *held)
    .count()
}

/// A law about a pause starts with the wait for the next look running: while `due` is empty a
/// step asks the provider for a read whatever a pause kept or forgot, so the law would pass
/// under its own control.
fn assert_armed(upkeep: &Upkeep, law: &str) {
    assert!(
        upkeep.due.is_some_and(|due| due > Instant::now()),
        "{law} does not start with the wait for the next look running"
    );
}

/// Called by the existing shared-login automatic-upkeep owner, never a second test registration.
pub(in crate::shared_login) fn exercise_owned_upkeep() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let source = scratch.path().join("private.vault");
    let paths = Paths::for_home(&scratch.path().join("admin"));
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    // The saved repository holds keys of GitHub's whole row, as a hand save of it leaves them.
    let mut kept = whole(repository(1, "kept-code"), "saved");
    kept.as_object_mut()
        .unwrap()
        .insert("required_submodules".into(), json!(["vendor/required"]));
    // A second saved repository, whole and with policy, that this upkeep's provider never
    // lists: its token does not see it.
    let mut unlisted = whole(repository(5, "unlisted-code"), "saved");
    unlisted
        .as_object_mut()
        .unwrap()
        .insert("future_policy".into(), json!({"keep": ["exact", 7]}));
    let initial = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": [kept.clone(), unlisted.clone()]}))
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
    let answered = Arc::new(Mutex::new(None::<Candidate>));
    let (counted, answer) = (Arc::clone(&fetches), Arc::clone(&answered));
    // This upkeep's provider lists the first saved repository whole, as GitHub does, beside
    // one new repository, and never the second saved one. The first one's size moves at each
    // look (2, 102, 202, ... KiB) and is always in the class of the saved one, at most 1 MiB.
    upkeep.fetch = Arc::new(move |input, stopped| {
        let look = counted.fetch_add(1, Ordering::SeqCst) as u64;
        assert!(!stopped.load(Ordering::Acquire));
        let rows = [
            whole(sized(1, "kept-code", 2 + 100 * look), "github"),
            repository(2, "new-code"),
        ];
        let candidate = super::protocol_tests::provider(
            input.token,
            input.binding,
            &input.catalog,
            &rows,
            &stopped,
        )?;
        *answer.lock().unwrap() = Some(candidate.clone());
        Ok(candidate)
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
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().find(|row| row["id"].as_u64() == Some(5)),
        Some(&unlisted),
        "a saved repository GitHub did not list is not published as it was saved"
    );
    let retained = rows
        .iter()
        .find(|row| row["id"].as_u64() == Some(1))
        .unwrap();
    assert_eq!(
        retained["required_submodules"][0].as_str(),
        Some("vendor/required")
    );
    // The published row holds what GitHub owns and the policy, not the rest of GitHub's row.
    for key in PROVIDER_SHAPED {
        assert!(
            kept.get(key).is_some() && retained.get(key).is_none(),
            "the published repository still holds GitHub's {key}"
        );
    }
    assert!(
        retained["owner"].get("avatar_url").is_none(),
        "the published owner still holds GitHub's avatar_url"
    );
    assert_eq!(
        retained["size"].as_u64(),
        Some(1),
        "the saved size, in the class of GitHub's, stands"
    );
    assert!(
        catalog.value.as_slice().len() < initial.len(),
        "two lean repositories beside the unlisted one ({} bytes) are no smaller than one saved \
         whole beside it ({} bytes)",
        catalog.value.as_slice().len(),
        initial.len()
    );
    assert_eq!(
        refreshed_line(answered.lock().unwrap().as_ref().unwrap()),
        "private repository catalog refreshed: mode=TokenVisible, observed_owned=2, \
         other_fields=0, size_class=0, added=1, removed=0, pruned=1, \
         account=false, order=false"
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
        Author::Person,
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
    // One look, as in the size-class law: the wait for it is spent, so the next step reads the
    // provider once, and a second step drains that read into `publish`.
    let mut look = || -> usize {
        spend_the_wait(&mut upkeep);
        let admin = client::admin_open(&paths).unwrap();
        let far = Instant::now() + Duration::from_secs(60);
        assert_eq!(upkeep.step(&paths, &admin, far).unwrap(), 0);
        wait_for_result(&upkeep.mailbox);
        upkeep.step(&paths, &admin, far).unwrap()
    };
    // Three more looks over the lean catalog, GitHub's row whole and its size moved inside its
    // class each time.
    let signed = look() + look() + look();
    assert_eq!(
        signed, 0,
        "{signed} of 3 further looks at a lean catalog published it again"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 5);
    // A person saves the whole rows again. The looks after it find the same two repositories
    // and a size inside its class: the keys of GitHub's row the saved one holds are no part of
    // the catalog's meaning, so they publish nothing, and the person's save stands until a
    // change of meaning publishes it lean.
    let resaved = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": [kept.clone(), repository(2, "new-code"), unlisted.clone()]}))
    .unwrap();
    let current = client::admin_open(&paths).unwrap();
    let mut doc = current.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &resaved).unwrap();
    client::admin_publish_conditional_until(
        &paths,
        &current,
        &doc,
        Instant::now() + Duration::from_secs(60),
        Author::Person,
    )
    .unwrap();
    let signed = look() + look();
    assert_eq!(
        signed, 0,
        "{signed} of 2 looks whose only finding is a repository saved whole published it"
    );
    assert_eq!(
        client::admin_open(&paths)
            .unwrap()
            .published_entry(&Audience::Everyone, NAME)
            .unwrap()
            .value
            .as_slice(),
        resaved.as_slice(),
        "the person's whole save stands"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 7);
    drop(upkeep); // The actual owned idle worker is joined here.

    // Pause while a real owned fetch is active: the next step asks for a new read at once, and
    // the late answer, once released, is not kept for any step to publish (law 4).
    for change_source in [false, true] {
        let current = client::admin_open(&paths).unwrap();
        let late = refreshed(Input::capture(&paths, &current).unwrap());
        let mut upkeep = Upkeep::default();
        // The first read waits on `first`, every later one on `later`.
        let (first, later) = (gate(), gate());
        let release = Release(vec![Arc::clone(&first), Arc::clone(&later)]);
        let (entered, receiving) = std::sync::mpsc::channel();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (held, then) = (Arc::clone(&first), Arc::clone(&later));
        upkeep.fetch = Arc::new(move |input, stopped| {
            entered.send(Arc::clone(&stopped)).unwrap();
            wait(if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                &held
            } else {
                &then
            });
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
        let newer = if change_source {
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
                Author::Person,
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
            Some(newer)
        } else {
            None
        };
        // The administrator opened after the last publication: its binding is the kept one.
        let fresh = newer.as_ref().unwrap_or(&current);
        let before = std::fs::read(&source).unwrap();
        let idle_timer = upkeep.due.is_none();
        upkeep.pause();
        let was_stopped = stopped.load(Ordering::Acquire);
        let retired = {
            let mail = upkeep.mailbox.0.lock().unwrap();
            mail.pending.is_none() && mail.result.is_none()
        };
        // Law 4: the read the pause stopped is still being made (its provider waits on the
        // gate), and the next step asks for its replacement at once: the pause left `due`
        // empty, and a read told to stop holds back no new one.
        assert_eq!(
            upkeep
                .step(&paths, fresh, Instant::now() + Duration::from_secs(60))
                .unwrap(),
            0
        );
        let replaced = upkeep.mailbox.0.lock().unwrap().pending.is_some();
        assert!(was_stopped && retired);
        assert!(idle_timer, "a read in flight left the timer armed");
        assert!(
            replaced,
            "law 4 (source changed first: {change_source}): the read a pause stopped was not \
             replaced at once"
        );
        // The stopped read answers. Its replacement reaching the provider shows the worker has
        // settled that late answer: the one worker takes the replacement only after it.
        open(&first);
        let replacement = receiving
            .recv_timeout(Duration::from_secs(60))
            .expect("the replacement never reached the provider");
        let kept = {
            let mail = upkeep.mailbox.0.lock().unwrap();
            mail.result.is_some() || mail.pending.is_some()
        };
        let replacement_stopped = replacement.load(Ordering::Acquire);
        drop(release);
        drop(upkeep); // Releases, cancels and joins.
        assert!(
            !kept,
            "law 4 (source changed first: {change_source}): the late answer of a read a pause \
             stopped was kept for a step to publish"
        );
        assert!(
            !replacement_stopped,
            "the replacement was stopped before the upkeep was dropped"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            before,
            "a pause and the step after it publish nothing"
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
        Author::Person,
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

/// Called by the same automatic-upkeep owner as [`exercise_owned_upkeep`]. Looks that find
/// GitHub's sizes moved inside their size classes sign nothing and log nothing; a size
/// past its class, and another field, each sign the entry once, with GitHub's class for a size
/// that left its class and the saved size of each that stayed in its own, and log one line that
/// says which. Each look is the production read ([`observe`]) of a scripted provider, and each
/// line is what [`publish`] wrote to the log.
pub(in crate::shared_login) fn exercise_size_class() {
    /// In the class 1,572,865 to 1,835,008 KiB.
    const LARGE: u64 = 1_600_000;
    /// In the class 1,793 to 2,048 KiB.
    const SMALL: u64 = 2_000;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let source = scratch.path().join("private.vault");
    let paths = Paths::for_home(&scratch.path().join("admin"));
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let mut kept = sized(1, "large-code", LARGE);
    kept.as_object_mut()
        .unwrap()
        .insert("required_submodules".into(), json!(["vendor/required"]));
    let initial = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": [kept, sized(2, "small-code", SMALL)]}))
    .unwrap();
    let opened = client::admin_open(&paths).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &initial).unwrap();
    // A classic token: with the provider's scope and counters it proves the whole account.
    doc.set_entry(&Audience::Everyone, TOKEN, b"ghp_fixture_classic_only_test")
        .unwrap();
    client::admin_publish(&paths, &opened, &doc).unwrap();

    let provider = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut upkeep = Upkeep::default();
    let (listed, counted) = (Arc::clone(&provider), Arc::clone(&fetches));
    upkeep.fetch = Arc::new(move |input, stopped| {
        counted.fetch_add(1, Ordering::SeqCst);
        let rows = listed.lock().unwrap().clone();
        super::protocol_tests::provider(input.token, input.binding, &input.catalog, &rows, &stopped)
    });
    let far = || Instant::now() + Duration::from_secs(60);
    // One look: the 15 minute wait for it is spent, so the next step reads the provider
    // exactly once, and a second step with the same freshly opened administrator drains that
    // read into `publish`. The answer is how many publications that made and the catalog
    // lines it logged.
    let mut look = |large: u64, small: u64, branch: &str| -> (usize, Vec<String>) {
        let mut small = sized(2, "small-code", small);
        small
            .as_object_mut()
            .unwrap()
            .insert("default_branch".into(), branch.into());
        *provider.lock().unwrap() = vec![sized(1, "large-code", large), small];
        spend_the_wait(&mut upkeep);
        let admin = client::admin_open(&paths).unwrap();
        let before = fetches.load(Ordering::SeqCst);
        assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
        wait_for_result(&upkeep.mailbox);
        assert_eq!(fetches.load(Ordering::SeqCst), before + 1);
        let (signed, logged) =
            logged_on_this_thread(|| upkeep.step(&paths, &admin, far()).unwrap());
        let lines = logged
            .into_iter()
            .filter(|line| line.starts_with("INFO private repository catalog"))
            .collect();
        (signed, lines)
    };
    let published = || {
        let admin = client::admin_open(&paths).unwrap();
        let entry = admin.published_entry(&Audience::Everyone, NAME).unwrap();
        aterm_json::from_slice::<Value>(entry.value.as_slice()).unwrap()
    };
    let sizes = |catalog: &Value| {
        let rows = catalog["repositories"].as_array().unwrap();
        [rows[0]["size"].as_u64(), rows[1]["size"].as_u64()]
    };

    // Eight looks, each with both sizes moved from the look before and inside the classes of
    // the published ones, edges included.
    let before = std::fs::read(&source).unwrap();
    let inside: [(u64, u64); 8] = [
        (1_572_865, 1_793),
        (1_835_008, 2_048),
        (LARGE + 1, SMALL + 1),
        (1_700_000, 1_900),
        (1_800_000, 2_047),
        (LARGE - 27_000, 1_800),
        (LARGE + 200_000, 1_850),
        (LARGE + 7, SMALL + 7),
    ];
    let looks: Vec<_> = inside
        .iter()
        .map(|(large, small)| look(*large, *small, "main"))
        .collect();
    let signed: usize = looks.iter().map(|(signed, _)| signed).sum();
    assert_eq!(
        signed,
        0,
        "{signed} of {} looks that found only sizes inside their classes published the catalog",
        inside.len()
    );
    assert!(
        looks.iter().all(|(_, lines)| lines.is_empty()),
        "a look that publishes nothing logs no catalog line: {looks:?}"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        before,
        "a look that publishes nothing leaves the vault as it was"
    );
    assert_eq!(
        published(),
        aterm_json::from_slice::<Value>(&initial).unwrap()
    );

    // One KiB past the small repository's class: one publication, which holds its new class
    // (2,048, 2,560] KiB as 2,560 KiB, and the saved size of the large one, whose move stayed in
    // its class.
    assert_eq!(
        look(LARGE + 9, 2_049, "main"),
        (
            1,
            vec![
                "INFO private repository catalog refreshed: mode=AccountComplete, \
                 observed_owned=2, other_fields=0, size_class=1, added=0, removed=0, \
                 pruned=0, account=false, order=false"
                    .to_owned()
            ]
        )
    );
    let catalog = published();
    assert_eq!(sizes(&catalog), [Some(LARGE), Some(2_560)]);
    assert_eq!(
        catalog["repositories"][0]["required_submodules"][0].as_str(),
        Some("vendor/required")
    );
    // The classes are now those of the sizes that publication holds.
    let before = std::fs::read(&source).unwrap();
    assert_eq!(look(LARGE + 200_000, 2_560, "main"), NOTHING);
    assert_eq!(look(1_572_865, 2_049, "main"), NOTHING);
    assert_eq!(std::fs::read(&source).unwrap(), before);

    // Another field, with both sizes inside their classes: one publication, holding the saved
    // sizes.
    assert_eq!(
        look(LARGE + 100, 2_300, "trunk"),
        (
            1,
            vec![
                "INFO private repository catalog refreshed: mode=AccountComplete, \
                 observed_owned=2, other_fields=1, size_class=0, added=0, removed=0, \
                 pruned=0, account=false, order=false"
                    .to_owned()
            ]
        ),
        "a changed default branch is published"
    );
    let catalog = published();
    assert_eq!(
        catalog["repositories"][1]["default_branch"].as_str(),
        Some("trunk")
    );
    assert_eq!(sizes(&catalog), [Some(LARGE), Some(2_560)]);
    let before = std::fs::read(&source).unwrap();
    assert_eq!(look(LARGE + 100, 2_300, "trunk"), NOTHING);
    assert_eq!(look(LARGE + 1100, 2_100, "trunk"), NOTHING);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(fetches.load(Ordering::SeqCst), 14);
}

/// Called by the same automatic-upkeep owner as [`exercise_owned_upkeep`]. A pause, the
/// approver's call on each pass that cannot read its admin authority that instant, retires the
/// read in flight and nothing else: a pause and then a step ask the provider only for what the
/// schedule owes at once anyway (a changed entry, a changed member set, a read the pause
/// dropped), never for the pause itself. Every step uses an administrator opened after the
/// last publication, so its binding differs from the kept one only where an entry changed.
/// Laws 1, 2, 3 and 5 start with the wait for the next look running; law 4 is in
/// [`exercise_owned_upkeep`], where a read is held in flight. Law 7 holds what a pause relies
/// on when a read finishes inside a step: the wait is armed only with no read in flight.
pub(in crate::shared_login) fn exercise_pause_keeps_the_schedule() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let source = scratch.path().join("private.vault");
    let paths = Paths::for_home(&scratch.path().join("admin"));
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let catalog = |rows: &[Value]| {
        aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
            "repositories": rows}))
        .unwrap()
    };
    let opened = || client::admin_open(&paths).unwrap();
    let set = |name: &str, value: &[u8]| {
        let current = opened();
        let mut doc = current.doc.clone();
        doc.set_entry(&Audience::Everyone, name, value).unwrap();
        client::admin_publish(&paths, &current, &doc).unwrap();
    };
    let current = opened();
    let mut doc = current.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        NAME,
        &catalog(&[repository(1, "kept-code")]),
    )
    .unwrap();
    doc.set_entry(&Audience::Everyone, TOKEN, b"github_pat_fixture_only")
        .unwrap();
    client::admin_publish(&paths, &current, &doc).unwrap();

    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failing = Arc::new(AtomicBool::new(false));
    let mut upkeep = Upkeep::default();
    let (counted, fails) = (Arc::clone(&fetches), Arc::clone(&failing));
    upkeep.fetch = Arc::new(move |input, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        if fails.load(Ordering::SeqCst) {
            return Err("the scripted provider read failed".into());
        }
        Ok(refreshed(input))
    });
    let reads = || fetches.load(Ordering::SeqCst);
    let far = || Instant::now() + Duration::from_secs(60);

    // The first step asks for a read and the second publishes it (it adds a repository), which
    // arms the 15 minute wait and keeps the binding that publication predicts.
    let admin = opened();
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    wait_for_result(&upkeep.mailbox);
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 1);
    assert_eq!(reads(), 1);

    // Law 1: the upkeep's own publication drained and the wait running. A pause, then an
    // unrelated publication (a version whose apply met the approver), then a step: no read.
    assert_armed(&upkeep, "law 1");
    upkeep.pause();
    set("unrelated", b"published while the upkeep was paused");
    assert_eq!(upkeep.step(&paths, &opened(), far()).unwrap(), 0);
    assert_eq!(
        jobs(&upkeep),
        0,
        "law 1: a pause beside an unrelated version asked the provider again"
    );
    assert_eq!(reads(), 1, "law 1: one read in all");

    // Law 2: a pause, then a member is published, then a step: the changed member set is owed
    // exactly one read, at once, across the pause.
    assert_armed(&upkeep, "law 2");
    upkeep.pause();
    let recipient = aterm_secrets::age::Identity::from_secret_bytes([62; 32])
        .recipient()
        .to_bech32();
    let current = opened();
    let mut doc = current.doc.clone();
    doc.add_machine("fixture-admitted", &recipient).unwrap();
    client::admin_publish_approved(&paths, &current, &doc, &[recipient]).unwrap();
    let admin = opened();
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    assert_eq!(
        jobs(&upkeep),
        1,
        "law 2: a member published beside a pause was not read at once"
    );
    wait_for_result(&upkeep.mailbox);
    upkeep.step(&paths, &admin, far()).unwrap();
    assert_eq!(jobs(&upkeep), 0, "law 2: the member's read was asked twice");
    assert_eq!(reads(), 2);

    // Law 3: a pause, then another publication changes the inventory, or the token, then a
    // step: the changed entry is read at once, as it is with no pause.
    let rows = [
        repository(1, "kept-code"),
        repository(2, "new-code"),
        repository(3, "manual-code"),
    ];
    for (law, name, value) in [
        ("law 3, the inventory", NAME, catalog(&rows)),
        (
            "law 3, the token",
            TOKEN,
            b"github_pat_rotated_only".to_vec(),
        ),
    ] {
        assert_armed(&upkeep, law);
        let before = reads();
        upkeep.pause();
        set(name, &value);
        let admin = opened();
        assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
        assert_eq!(
            jobs(&upkeep),
            1,
            "{law}: an entry changed beside a pause was not read at once"
        );
        wait_for_result(&upkeep.mailbox);
        upkeep.step(&paths, &admin, far()).unwrap();
        assert_eq!(jobs(&upkeep), 0);
        assert_eq!(reads(), before + 1);
    }

    // Law 5: a failed read arms the minute's retry. A pause keeps that instant, and a step
    // inside the minute asks for nothing.
    failing.store(true, Ordering::SeqCst);
    spend_the_wait(&mut upkeep);
    let admin = opened();
    let before = reads();
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    wait_for_result(&upkeep.mailbox);
    assert_eq!(
        upkeep.step(&paths, &admin, far()).unwrap_err(),
        "the scripted provider read failed"
    );
    failing.store(false, Ordering::SeqCst);
    assert_armed(&upkeep, "law 5");
    let retry = upkeep.due;
    upkeep.pause();
    assert_eq!(
        upkeep.due, retry,
        "law 5: a pause moved a failed read's retry"
    );
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    assert_eq!(
        jobs(&upkeep),
        0,
        "law 5: a pause inside a failed read's minute asked the provider again"
    );
    assert_eq!(reads(), before + 1);

    // Law 6: a finished read waiting for its step when a pause comes is dropped, and the next
    // step asks for exactly one new read at once: a dropped result is never left behind an
    // armed wait. While a read waits the wait is not running, so this law starts without it.
    spend_the_wait(&mut upkeep);
    let admin = opened();
    let before = reads();
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    wait_for_result(&upkeep.mailbox);
    assert!(
        upkeep.due.is_none(),
        "law 6: a read waiting for its step left the wait running"
    );
    upkeep.pause();
    assert_eq!(
        jobs(&upkeep),
        0,
        "law 6: a pause left the finished read waiting"
    );
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    assert_eq!(
        jobs(&upkeep),
        1,
        "law 6: the read a pause dropped was not asked again at once"
    );
    wait_for_result(&upkeep.mailbox);
    upkeep.step(&paths, &admin, far()).unwrap();
    assert_eq!(jobs(&upkeep), 0);
    assert_eq!(reads(), before + 2);
    assert!(
        upkeep.due.is_some_and(|due| due > Instant::now()),
        "law 6: the drained new read did not start the wait for the next look"
    );
    drop(upkeep);

    // Law 7: a read that finishes after a step took the last answer, and before it chose to
    // ask for another, answers that step. No second read is asked for beside it, so the drain
    // of the first arms the wait with nothing in flight, as a pause relies on.
    let mut upkeep = Upkeep::default();
    let (first, later) = (gate(), gate());
    let release = Release(vec![Arc::clone(&first), Arc::clone(&later)]);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (counted, held, then) = (Arc::clone(&calls), Arc::clone(&first), Arc::clone(&later));
    let (entered, receiving) = std::sync::mpsc::channel();
    upkeep.fetch = Arc::new(move |input, _| {
        entered.send(()).unwrap();
        wait(if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            &held
        } else {
            &then
        });
        Ok(refreshed(input))
    });
    let admin = opened();
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    receiving
        .recv_timeout(Duration::from_secs(60))
        .expect("the provider never entered the first read");
    let (mailbox, finishing) = (Arc::clone(&upkeep.mailbox), Arc::clone(&first));
    upkeep.between = Some(Box::new(move || {
        open(&finishing);
        wait_for_result(&mailbox);
    }));
    assert_eq!(upkeep.step(&paths, &admin, far()).unwrap(), 0);
    upkeep.between = None;
    let answered = {
        let mail = upkeep.mailbox.0.lock().unwrap();
        mail.result.is_some() && mail.pending.is_none() && mail.active.is_none()
    };
    assert!(
        answered,
        "law 7: a read that finished inside a step was asked for again beside it"
    );
    upkeep.step(&paths, &admin, far()).unwrap();
    assert_eq!(
        jobs(&upkeep),
        0,
        "law 7: a read is in flight after the drain"
    );
    assert!(
        upkeep.due.is_some_and(|due| due > Instant::now()),
        "law 7: the drain did not start the wait for the next look"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(release);
}

/// The catalog a provider that lists `rows` refreshes `input` to.
fn listed(input: Input, rows: &[Value]) -> Candidate {
    let catalog = RepositoryCatalog::parse(&input.catalog).unwrap();
    let bytes = catalog
        .refresh(
            "fixture-user",
            7,
            rows,
            RepositoryCatalogVisibility::TokenVisible,
        )
        .unwrap();
    Candidate {
        changes: catalog.changes(&bytes).unwrap(),
        bytes,
        binding: input.binding,
        visibility: RepositoryCatalogVisibility::TokenVisible,
        observed: rows.len(),
        observed_at: Instant::now(),
    }
}

/// The rows of a provider that lists `repositories` repositories, each name 24 characters as a
/// lab's longer ones are, and one more at each `look`: the lean rows a refresh keeps, a little
/// under 300 bytes each.
fn lean(repositories: u64, look: usize) -> Vec<Value> {
    (0..repositories)
        .map(|n| repository(100 + n, &format!("code-{n:04}-{}", "x".repeat(15))))
        .chain([repository(5_000 + look as u64, &format!("look-{look}"))])
        .collect()
}

/// The rows of a provider that lists 800 repositories with long names and one more at each
/// `look`: a catalog of some 380 KiB.
fn large(look: usize) -> Vec<Value> {
    (0..800_u64)
        .map(|n| repository(100 + n, &format!("code-{n:04}-{}", "x".repeat(80))))
        .chain([repository(5_000 + look as u64, &format!("look-{look}"))])
        .collect()
}

/// A vault this Mac administers that publishes a one-repository catalog and its token, and what
/// its membership log holds.
fn published_lab() -> aterm_db::table::fixture::Lab {
    use aterm_secrets::commons::roster::Role;
    let lab = aterm_db::table::fixture::Lab::with_foundation(&[Role::Writer]);
    let paths = &lab.paths[0];
    client::seed_vault_genesis(paths, &lab._home.path().join("genesis.v2")).unwrap();
    let opened = client::admin_open(paths).unwrap();
    let mut doc = opened.doc.clone();
    let initial = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": [repository(1, "kept-code")]}))
    .unwrap();
    doc.set_entry(&Audience::Everyone, NAME, &initial).unwrap();
    doc.set_entry(&Audience::Everyone, TOKEN, b"github_pat_fixture_only")
        .unwrap();
    client::admin_publish(paths, &opened, &doc).unwrap();
    assert!(allowed(paths));
    lab
}

/// The bytes of the complete records `paths`' membership log holds.
fn held(paths: &Paths) -> usize {
    client::load_records(paths)
        .unwrap()
        .log()
        .expect("CONTROL: the vault writes a membership log")
        .fill(None)
        .bytes
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// One look: this Mac reads the version it published, as the approver's pass does before each
/// step, and the wait for the look is spent; then a step asks the provider and the next drains
/// the read into `publish`.
fn look(upkeep: &mut Upkeep, paths: &Paths) -> Result<usize, String> {
    client::sync_reader_until(paths, far()).unwrap();
    spend_the_wait(upkeep);
    let admin = client::admin_open(paths).unwrap();
    assert_eq!(upkeep.step(paths, &admin, far()), Ok(0));
    wait_for_result(&upkeep.mailbox);
    upkeep.step(paths, &admin, far())
}

/// A refused catalog is retired and its next look waits the whole 15 minutes, never the 60 s
/// of a retry: no read is held, and the wait runs past a retry's.
fn assert_waits_a_look(upkeep: &Upkeep) {
    assert_eq!(jobs(upkeep), 0, "the refused candidate is retired");
    assert!(
        upkeep.due.is_some_and(|due| due > Instant::now() + RETRY),
        "a refusal waits a look, not a retry"
    );
}

/// The upkeep is an automatic writer of the vault's membership log (FL-145): once this Mac's
/// seat signed 1 MiB of `Put`s in a day, the log's fill refuses the next catalog. The upkeep
/// then signs and publishes nothing, retires that candidate instead of offering it at every
/// step, waits the whole 15 minutes for its next look, and says the refusal once, in the fill's
/// words; a later look that is refused too says nothing. A person's publish of the same entry
/// from the same seat lands.
///
/// CONTROL, in the test: the first look, within the budget, publishes.
#[test]
fn vault_fill_refusal_retires_the_repository_catalog_and_is_said_once() {
    let lab = published_lab();
    let paths = &lab.paths[0];
    let mut upkeep = Upkeep::default();
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    upkeep.fetch = Arc::new(move |input, _| {
        Ok(listed(
            input,
            &lean(60, counted.fetch_add(1, Ordering::SeqCst)),
        ))
    });
    assert_eq!(
        look(&mut upkeep, paths),
        Ok(1),
        "CONTROL: the first catalog"
    );

    // The seat's day is spent by what a person signs from it too: four 300 KiB values of a
    // name that is no configuration's.
    for version in 0..4_u8 {
        let admin = client::admin_open(paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            "lab_archive",
            &vec![b'a' + version; 300 * 1024],
        )
        .unwrap();
        client::admin_publish(paths, &admin, &doc).expect("a person's write is never held to it");
    }

    let (seq, bytes) = (client::admin_open(paths).unwrap().seq(), held(paths));
    let refusal = look(&mut upkeep, paths).expect_err("a catalog past the seat's day");
    assert!(
        refusal.starts_with(
            "private repository catalog was not refreshed: the repository inventory upkeep's \
             write was not signed: this Mac's seat signed "
        ) && refusal.contains(" would pass the 1 MiB an automatic writer may sign in a day")
            && refusal.ends_with("; it is looked at again every 15 minutes"),
        "{refusal}"
    );
    assert!(
        !refusal.contains("code-"),
        "no repository's name: {refusal}"
    );
    assert_eq!(held(paths), bytes, "nothing is signed");
    assert_eq!(
        client::admin_open(paths).unwrap().seq(),
        seq,
        "and no version published"
    );
    assert_waits_a_look(&upkeep);
    // A step before the next look asks nothing and says nothing.
    let admin = client::admin_open(paths).unwrap();
    assert_eq!(upkeep.step(paths, &admin, far()), Ok(0));
    assert_eq!((jobs(&upkeep), fetches.load(Ordering::SeqCst)), (0, 2));
    // The next look is refused too: said once, not again.
    assert_eq!(
        look(&mut upkeep, paths),
        Ok(0),
        "a second refusal is not said again"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 3);
    assert_eq!(held(paths), bytes, "nothing is signed");
    assert_waits_a_look(&upkeep);

    // A person's publish of that catalog, from the same seat, is never held to the day.
    let admin = client::admin_open(paths).unwrap();
    let input = Input::capture(paths, &admin).unwrap();
    let catalog = listed(input, &lean(60, 9)).bytes;
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &catalog).unwrap();
    client::admin_publish(paths, &admin, &doc).expect("a person's write");
    assert!(held(paths) > bytes + catalog.len(), "it is signed");
}

/// A refusal is said once while it stands, and a later one is said again once something ended
/// it: a look whose catalog the vault already holds (the provider lists again what was
/// published), or a change of the entries the upkeep is kept for (a person's new token). Each
/// refusal after that is a new one, said once.
///
/// CONTROL, in the test: between the ends, a refusal is not said again.
#[test]
fn vault_fill_upkeep_says_a_refusal_again_once_it_has_ended() {
    let lab = published_lab();
    let paths = &lab.paths[0];
    let mut upkeep = Upkeep::default();
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    // The provider lists what was published at the first and the third look, and something new
    // at every other.
    upkeep.fetch = Arc::new(move |input, _| {
        let at = counted.fetch_add(1, Ordering::SeqCst);
        let rows = if at == 2 { lean(60, 0) } else { lean(60, at) };
        Ok(listed(input, &rows))
    });
    assert_eq!(
        look(&mut upkeep, paths),
        Ok(1),
        "CONTROL: the first catalog"
    );
    for version in 0..4_u8 {
        let admin = client::admin_open(paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            "lab_archive",
            &vec![b'a' + version; 300 * 1024],
        )
        .unwrap();
        client::admin_publish(paths, &admin, &doc).expect("a person's write");
    }
    let said = |result: Result<usize, String>| match result {
        Err(refusal) => {
            assert!(
                refusal.starts_with("private repository catalog was not refreshed: ")
                    && refusal.contains(" would pass the 1 MiB an automatic writer may sign"),
                "{refusal}"
            );
            true
        }
        Ok(0) => false,
        other => panic!("nothing is published past the seat's day: {other:?}"),
    };
    assert!(said(look(&mut upkeep, paths)), "the refusal is said");
    // The provider lists what the vault holds: nothing to publish, and the refusal has ended.
    assert_eq!(
        look(&mut upkeep, paths),
        Ok(0),
        "the vault holds the catalog"
    );
    assert!(
        said(look(&mut upkeep, paths)),
        "a refusal after the vault held the catalog is said again"
    );
    assert!(
        !said(look(&mut upkeep, paths)),
        "CONTROL: while it stands it is not said again"
    );
    // A person's new token changes what the upkeep is kept for.
    let admin = client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, TOKEN, b"github_pat_fixture_again")
        .unwrap();
    client::admin_publish(paths, &admin, &doc).expect("a person's new token");
    assert!(
        said(look(&mut upkeep, paths)),
        "a refusal after the entries changed is said again"
    );
    assert!(
        !said(look(&mut upkeep, paths)),
        "CONTROL: while it stands it is not said again"
    );
}

/// The sizes (KiB) of the repositories, ids 1 to 6, that a person saved for a day of looks
/// ([`a_day_of_looks_at_an_unchanged_account_publishes_nothing`]), each GitHub's own as an
/// earlier build wrote it.
const DAY_SAVED: [u64; 6] = [1_500, 3_000, 1_600_000, 300, 6_000_000, 40_000];

/// The size class of each of [`DAY_SAVED`], from its least size to its greatest (the class
/// itself), in KiB: the least number at or above a size with at most three significant bits, at
/// least 1 MiB.
const DAY_CLASSES: [(u64, u64); 6] = [
    (1_281, 1_536),
    (2_561, 3_072),
    (1_572_865, 1_835_008),
    (1, 1_024),
    (5_242_881, 6_291_456),
    (32_769, 40_960),
];

/// The size GitHub gives repository `index` of [`DAY_SAVED`] at look `look`: in the class of its
/// saved size ([`DAY_CLASSES`]), the least of the class at every fourth look, the greatest at
/// the look after and between them at the others, except that the last repository's is `0`
/// (GitHub has not sized it again) at every third look.
fn day_size(index: usize, look: u64) -> u64 {
    if index == 5 && look % 3 == 2 {
        return 0;
    }
    let (least, class) = DAY_CLASSES[index];
    let width = class - least + 1;
    match look % 4 {
        0 => least,
        1 => class,
        _ => least + (look * 7_919 + index as u64 * 104_729) % width,
    }
}

/// GitHub's rows for the account at look `look`, whole, with repository 2 renamed where
/// `renamed`.
fn day_rows(look: u64, renamed: bool) -> Vec<Value> {
    (0..DAY_SAVED.len())
        .map(|index| {
            let id = index as u64 + 1;
            let name = if renamed && id == 2 {
                "renamed-code".to_owned()
            } else {
                format!("code-{id}")
            };
            whole(sized(id, &name, day_size(index, look)), "github")
        })
        .collect()
}

/// A day of looks, 96 of them 15 minutes apart, at an account GitHub lists unchanged but for
/// each repository's size, which moves at every look and stays inside its size class,
/// signs nothing: the vault's membership log holds the bytes it held, and the entry the bytes
/// the person saved. The saved catalog is one a person saved by hand: its rows in another order
/// than a refresh writes, one of them holding keys of GitHub's whole row, and each size GitHub's
/// own, as an earlier build wrote sizes. Each look is the production read ([`observe`]) of a
/// scripted provider that lists GitHub's whole rows.
///
/// CONTROL, in the test: the next look, at which GitHub lists one of them renamed, publishes
/// once, and its line says the rename drove it and what was published beside it.
#[test]
fn a_day_of_looks_at_an_unchanged_account_publishes_nothing() {
    const LOOKS: u64 = 96;
    let lab = published_lab();
    let paths = &lab.paths[0];
    let mut rows: Vec<Value> = DAY_SAVED
        .iter()
        .zip(1..)
        .map(|(size, id)| sized(id, &format!("code-{id}"), *size))
        .collect();
    rows[0] = whole(rows[0].clone(), "saved");
    rows[0]
        .as_object_mut()
        .unwrap()
        .insert("required_submodules".into(), json!(["vendor/required"]));
    rows.reverse();
    let saved = aterm_json::to_vec(&json!({"schema": 2, "account": "fixture-user",
        "repositories": rows}))
    .unwrap();
    let admin = client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, NAME, &saved).unwrap();
    client::admin_publish(paths, &admin, &doc).expect("a person's save");

    let mut upkeep = Upkeep::default();
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let renamed = Arc::new(AtomicBool::new(false));
    let (counted, rename) = (Arc::clone(&fetches), Arc::clone(&renamed));
    upkeep.fetch = Arc::new(move |input, stopped| {
        let look = counted.fetch_add(1, Ordering::SeqCst) as u64;
        let rows = day_rows(look, rename.load(Ordering::SeqCst));
        super::protocol_tests::provider(input.token, input.binding, &input.catalog, &rows, &stopped)
    });
    let entry = || {
        client::admin_open(paths)
            .unwrap()
            .published_entry(&Audience::Everyone, NAME)
            .unwrap()
            .value
            .as_slice()
            .to_vec()
    };
    let (seq, bytes) = (client::admin_open(paths).unwrap().seq(), held(paths));
    let looks: Vec<_> = (0..LOOKS).map(|_| look(&mut upkeep, paths)).collect();
    let signed = looks.iter().filter(|look| **look != Ok(0)).count();
    assert_eq!(
        signed, 0,
        "{signed} of {LOOKS} looks at an account whose sizes moved inside their classes signed \
         or failed: {looks:?}"
    );
    assert_eq!(held(paths), bytes, "nothing is signed in a day");
    assert_eq!(
        client::admin_open(paths).unwrap().seq(),
        seq,
        "and no version published"
    );
    assert_eq!(entry(), saved, "the entry is the person's save");
    assert_eq!(fetches.load(Ordering::SeqCst), LOOKS as usize);

    // CONTROL: a rename publishes, once.
    renamed.store(true, Ordering::SeqCst);
    let (published, logged) = logged_on_this_thread(|| look(&mut upkeep, paths));
    assert_eq!(
        published,
        Ok(1),
        "CONTROL: a renamed repository is published"
    );
    let lines: Vec<_> = logged
        .into_iter()
        .filter(|line| line.starts_with("INFO private repository catalog"))
        .collect();
    assert_eq!(
        lines,
        [
            "INFO private repository catalog refreshed: mode=TokenVisible, observed_owned=6, \
          other_fields=1, size_class=0, added=0, removed=0, pruned=1, account=false, order=true"
        ]
    );
    assert!(held(paths) > bytes, "CONTROL: it is signed");
    let value: Value = aterm_json::from_slice(&entry()).unwrap();
    let rows = value["repositories"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row["id"].as_u64().unwrap(), row["size"].as_u64().unwrap()))
            .collect::<Vec<_>>(),
        DAY_SAVED
            .iter()
            .zip(1..)
            .map(|(size, id)| (id, *size))
            .collect::<Vec<_>>(),
        "published by id, each saved size inside its class standing"
    );
    assert_eq!(rows[1]["name"].as_str(), Some("renamed-code"));
    assert!(
        PROVIDER_SHAPED.iter().all(|key| rows[0].get(key).is_none()),
        "the row saved whole is published lean beside the rename: {:?}",
        rows[0]
    );
    assert_eq!(
        rows[0]["required_submodules"][0].as_str(),
        Some("vendor/required")
    );
}

/// The upkeep's catalog is held to 64 KiB at any fill. A catalog over it is refused by the
/// vault text's door, in words that name the entry, its size and the limit, in a log that is
/// all but empty; one that the door takes and whose `Put` record is over 64 KiB is refused by
/// the log's fill, in words that name the writer, the size and the limit. Either way the upkeep
/// signs and publishes nothing, retires the candidate, waits a look (15 minutes, not the 60 s
/// of a retry) and says the refusal once. When the provider's listing is lean again the catalog
/// is published and the log says so.
///
/// CONTROL, in the test: a lean catalog of 80 repositories, more than the lab lists, is under
/// 32 KiB and is published by the same upkeep.
#[test]
fn vault_fill_size_limit_retires_a_repository_catalog_over_64_kib_and_waits_a_look() {
    const LIMIT: usize = 64 * 1024;
    let lab = published_lab();
    let paths = &lab.paths[0];
    let mut upkeep = Upkeep::default();
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    upkeep.fetch = Arc::new(move |input: Input, _| {
        let at = counted.fetch_add(1, Ordering::SeqCst);
        Ok(match at {
            0 | 1 => listed(input, &large(at)),
            2 => {
                // A catalog a few bytes under the limit: the door takes its value, and the
                // `Put` record that seals it, a few hundred bytes more, is over the limit.
                let size = |rows: &[Value]| {
                    let again = Input {
                        binding: input.binding.clone(),
                        catalog: input.catalog.clone(),
                        token: SecretBytes::from(b"unused".as_slice()),
                    };
                    listed(again, rows).bytes.len()
                };
                let with = |repositories: u64, name: &str| {
                    let mut rows = lean(repositories, at);
                    rows.push(repository(9_000, name));
                    rows
                };
                // As many lean rows as fit, then one whose name fills what is left: each
                // character of a name is in its row three times.
                let row = size(&with(201, "p")) - size(&with(200, "p"));
                let fit = 200 + (LIMIT - 40 - size(&with(200, "p"))) / row;
                let fit = u64::try_from(fit).unwrap();
                let left = LIMIT - 40 - size(&with(fit, "p"));
                let rows = with(fit, &"p".repeat(1 + left / 3));
                listed(input, &rows)
            }
            _ => listed(input, &lean(80, at)),
        })
    });

    let (seq, bytes) = (client::admin_open(paths).unwrap().seq(), held(paths));
    assert!(bytes < LIMIT, "CONTROL: the log is all but empty: {bytes}");
    let refusal = look(&mut upkeep, paths).expect_err("a 380 KiB catalog");
    assert!(
        refusal.starts_with(
            "private repository catalog was not refreshed: `github-repositories.json` was not \
             set: the value it would take is 3"
        ) && refusal
            .contains(" KiB, and a fleet configuration value over 64 KiB is not set or changed")
            && refusal.ends_with("; it is looked at again every 15 minutes"),
        "{refusal}"
    );
    assert!(
        !refusal.contains("code-"),
        "no repository's name: {refusal}"
    );
    assert_eq!(held(paths), bytes, "nothing is signed");
    assert_eq!(
        client::admin_open(paths).unwrap().seq(),
        seq,
        "and no version published"
    );
    assert_waits_a_look(&upkeep);
    assert_eq!(
        look(&mut upkeep, paths),
        Ok(0),
        "a second refusal is not said again"
    );
    assert_waits_a_look(&upkeep);

    // A value the door takes whose record is over the limit: the fill's refusal, by the
    // writer's name. A publication did not end the first refusal, so the step says nothing;
    // `publish` says it.
    let admin = client::admin_open(paths).unwrap();
    let input = Input::capture(paths, &admin).unwrap();
    let near = (upkeep.fetch)(input, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(
        near.bytes.len() <= LIMIT && near.bytes.len() > LIMIT - 200,
        "CONTROL: the door takes the value: {}",
        near.bytes.len()
    );
    let Err(Unpublished::Refused(refusal)) = publish(paths, &admin, near, far()) else {
        panic!("a Put over the limit is refused by the fill");
    };
    assert!(
        refusal.starts_with(
            "the repository inventory upkeep's write was not signed: one Put of it is 64"
        ) && refusal.ends_with(
            " KiB, and an automatic writer signs no Put over 64 KiB, at any fill of the vault log"
        ),
        "{refusal}"
    );
    assert_eq!(held(paths), bytes, "nothing is signed");

    // The listing is lean again: 80 repositories, published, and the log says the refusal
    // ended.
    let (published, logged) = logged_on_this_thread(|| look(&mut upkeep, paths));
    assert_eq!(published, Ok(1), "CONTROL: a lean catalog is published");
    assert!(
        logged.iter().any(|line| line
            .ends_with("private repository catalog is refreshed again: the vault takes it")),
        "{logged:?}"
    );
    let catalog = client::admin_open(paths)
        .unwrap()
        .published_entry(&Audience::Everyone, NAME)
        .unwrap()
        .value
        .as_slice()
        .len();
    assert!(
        (16 * 1024..32 * 1024).contains(&catalog),
        "CONTROL: a lean catalog of 80 repositories is far under the limit: {catalog}"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 4);
}
