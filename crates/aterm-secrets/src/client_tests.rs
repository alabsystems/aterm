// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[test]
fn offline_materialization_keeps_convergence_due_for_the_returning_source() {
    use crate::converge::{Due, Last};
    for prior_failure in [false, true] {
        let mut due = Due::default();
        assert!(due.at_start(1, [None; 3], Last::None));
        if prior_failure {
            due.failed();
        }
        let applied = SyncOutcome::Applied {
            seq: 4,
            report: Report::default(),
        };
        assert!(converge_after_pass(
            &mut due,
            &applied,
            2,
            [None; 3],
            Last::Ended,
            SourceSaid::Nothing,
        ));
        let unchanged = SyncOutcome::Unchanged { seq: 4 };
        assert!(!converge_after_pass(
            &mut due,
            &unchanged,
            3,
            [None; 3],
            Last::Ended,
            SourceSaid::Nothing,
        ));
        assert!(converge_after_pass(
            &mut due,
            &unchanged,
            4,
            [None; 3],
            Last::Ended,
            SourceSaid::AtOnce,
        ));
        assert!(!converge_after_pass(
            &mut due,
            &unchanged,
            5,
            [None; 3],
            Last::Ended,
            SourceSaid::AtOnce,
        ));

        // The old direct call consumed failure on the local apply, so an
        // unchanged network recovery on the same routes started nothing.
        let mut old = Due::default();
        assert!(old.after_pass(&applied, 2, [None; 3], Last::Ended));
        assert!(!old.after_pass(&unchanged, 3, [None; 3], Last::Ended));
    }
}

#[test]
fn config_debug_omits_nested_url_credentials() {
    let query = "synthetic-vault-query-credential";
    let fragment = "synthetic-vault-fragment-credential";
    for url in [
        format!("https://vault.example/v1/alab?token={query}#{fragment}"),
        format!("https://{query}@vault.example/v1/alab#{fragment}"),
    ] {
        let config = Config {
            source: Source::Url(url.clone()),
            mirrors: vec![url],
            name: "alab".to_owned(),
            signer: None,
            followed: Vec::new(),
            host: "test-machine".to_owned(),
        };
        for shown in [format!("{config:?}"), format!("{config:#?}")] {
            assert!(!shown.contains(query));
            assert!(!shown.contains(fragment));
        }
    }
}

/// A wait after an error ends at the change the kernel tells of, not at a later look: the
/// kernel's word that the routes changed 7 s in ends a minute's wait there. A message that
/// leaves the routes as they were (a neighbour's entry) waits on to the end.
#[test]
fn a_wait_after_an_error_ends_once_the_network_changes() {
    // Each wait: the kernel's next message, this long after the last.
    let waits = |messages: Vec<u64>| {
        let mut messages = messages.into_iter();
        move |most: Duration| {
            messages
                .next()
                .map_or(most, |after| Duration::from_secs(after).min(most))
        }
    };
    let mut looks = 0;
    let moved = nap_unless_moved(
        LONGEST_ERROR_RETRY,
        &mut || {
            looks += 1;
            looks == 2
        },
        &mut waits(vec![3, 4]),
    );
    assert!(moved);
    assert_eq!(looks, 2, "a look at each message, none between them");

    let mut slept = Duration::ZERO;
    let mut kernel = waits(vec![3, 4]);
    assert!(!nap_unless_moved(
        LONGEST_ERROR_RETRY,
        &mut || false,
        &mut |most| {
            let waited = kernel(most);
            slept += waited;
            waited
        }
    ));
    assert_eq!(slept, LONGEST_ERROR_RETRY);
}

#[test]
fn a_pass_that_errs_is_tried_again_within_a_minute() {
    let mut backoff = Duration::from_secs(5);
    let waits: Vec<u64> = (0..8)
        .map(|_| {
            let wait = backoff;
            backoff = retry_after(backoff, false, false);
            wait.as_secs()
        })
        .collect();
    assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60, 60]);
    assert_eq!(retry_after(backoff, true, false), Duration::from_secs(5));
}

/// Away from the ALab LAN a pass that failed is tried again a quarter hour on, never every
/// minute, and a move of this Mac's network (the way home) still asks again within seconds.
/// NEGATIVE CONTROL: `retry_after` ignoring `away` backs off to the minute, and "away: a
/// quarter hour" fails.
#[test]
fn away_a_pass_that_errs_waits_a_quarter_hour_and_a_move_ends_it() {
    let backoff = Duration::from_secs(5);
    assert_eq!(
        retry_after(backoff, false, true),
        AWAY_ERROR_RETRY,
        "away: a quarter hour"
    );
    assert_eq!(
        retry_after(AWAY_ERROR_RETRY, false, false),
        LONGEST_ERROR_RETRY,
        "back home: the minute again"
    );
    assert_eq!(retry_after(AWAY_ERROR_RETRY, true, true), backoff);
    assert_ne!(
        failed_line("the vault did not answer", true),
        failed_line("the vault did not answer", false),
        "away changes the words"
    );
}

#[test]
fn a_look_tells_an_answering_vault_from_none_and_makes_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let reader = Paths::for_home(&root.path().join("reader"));

    let plain = root.path().join("team.secrets");
    let missing = Source::Path(root.path().join("unplugged").join("team.vault"));
    assert!(
        matches!(look(&missing, HANG_DETECTOR, Duration::ZERO, &|_| {}), Look::NoVault(why) if why.ends_with("team.vault: no vault there"))
    );
    fsx::write_atomic(&plain, b"token=inert-token\n", 0o600).expect("plain vault");
    assert_eq!(
        look(&Source::Path(plain), HANG_DETECTOR, Duration::ZERO, &|_| {}),
        Look::Answers
    );

    let running = serve(&root.path().join("store"), &bound());
    let url = format!("http://{}/v1/alab", running.addr);
    let source = Source::Url(url.clone());
    assert_eq!(
        look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}),
        Look::NoVault(format!("{url}: no vault there"))
    );
    let admin = Paths::for_home(&root.path().join("ma"));
    create(&admin, &url, None, Some("ma")).expect("create");
    assert_eq!(
        look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}),
        Look::Answers
    );
    running.stop();
    assert!(
        matches!(look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}), Look::Unreached(why) if why.starts_with(&format!("fetching {url}"))),
        "{:?}",
        look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {})
    );
    assert!(!reader.dir.exists(), "a look makes nothing");
}

struct ScriptedTries {
    brokers: std::collections::VecDeque<Result<(), String>>,
    answers: std::collections::VecDeque<Result<transport::Answer, transport::Unheard>>,
    now: Instant,
    took: Duration,
    gets: usize,
    pauses: Vec<Duration>,
}

impl ScriptedTries {
    /// Each try takes `took`; the broker answers as `brokers` say (then answers), and each
    /// GET as `answers` say.
    fn new(
        took: Duration,
        brokers: impl IntoIterator<Item = Result<(), String>>,
        answers: impl IntoIterator<Item = Result<transport::Answer, transport::Unheard>>,
    ) -> Self {
        Self {
            brokers: brokers.into_iter().collect(),
            answers: answers.into_iter().collect(),
            now: Instant::now(),
            took,
            gets: 0,
            pauses: Vec::new(),
        }
    }
}

impl Tries for ScriptedTries {
    fn broker(&mut self, url: &str) -> Result<(), String> {
        assert_eq!(url, "http://alab-secrets.local:7484/v1/alab");
        self.brokers.pop_front().unwrap_or(Ok(()))
    }

    fn get(&mut self, url: &str, wait: Duration) -> Result<transport::Answer, transport::Unheard> {
        assert_eq!(url, "http://alab-secrets.local:7484/v1/alab");
        assert_eq!(
            wait, HANG_DETECTOR,
            "every try waits as long as the look's wait"
        );
        self.gets += 1;
        self.now += self.took;
        self.answers
            .pop_front()
            .expect("a GET the script has no answer for")
    }

    fn now(&mut self) -> Instant {
        self.now
    }

    fn pause(&mut self, pause: Duration) {
        self.pauses.push(pause);
        self.now += pause;
    }
}

fn unheard(why: &str) -> Result<transport::Answer, transport::Unheard> {
    Err(transport::Unheard {
        why: Error::new(why),
    })
}

fn served_vault() -> Result<transport::Answer, transport::Unheard> {
    Ok(transport::Answer::Fetched(Fetched::Body(
        envelope::MAGIC.as_bytes().to_vec(),
    )))
}

fn alab() -> Source {
    Source::Url("http://alab-secrets.local:7484/v1/alab".into())
}

/// The broker's words while it is not installed.
fn not_installed() -> Result<(), String> {
    Err(aterm_uds::lan::Unavailable::NotInstalled.to_string())
}

/// A look waits for the LAN broker while it is unavailable, saying its words once, and runs
/// its one try as soon as the broker answers; a broker still unavailable once the patience
/// is spent is the look's answer, in its words, and no GET runs. A failed try is unreached
/// at once: nothing waits on a person.
/// NEGATIVE CONTROL: a look that tries the vault before the broker answers takes the
/// script's GET while the broker is still unavailable, and `gets` is 1 in the second case.
#[test]
fn a_look_waits_for_the_lan_broker_and_then_tries_once() {
    let said = std::cell::RefCell::new(Vec::new());
    let say = |why: &str| said.borrow_mut().push(why.to_owned());
    let patience = Duration::from_secs(180);
    let mut tries = ScriptedTries::new(
        transport::LOOK_CONNECT,
        [not_installed(), not_installed()],
        [served_vault()],
    );
    assert_eq!(
        look_with(&alab(), HANG_DETECTOR, patience, &say, &mut tries),
        Look::Answers
    );
    assert_eq!(tries.gets, 1);
    assert_eq!(tries.pauses, [LOOK_PAUSE, LOOK_PAUSE]);
    assert_eq!(*said.borrow(), [not_installed().unwrap_err()]);

    said.borrow_mut().clear();
    let mut tries = ScriptedTries::new(
        transport::LOOK_CONNECT,
        std::iter::repeat_with(not_installed).take(100),
        [served_vault()],
    );
    let Look::Unreached(why) = look_with(
        &alab(),
        HANG_DETECTOR,
        Duration::from_secs(10),
        &say,
        &mut tries,
    ) else {
        panic!("a broker still unavailable is unreached");
    };
    assert_eq!(why, not_installed().unwrap_err());
    assert_eq!((tries.gets, tries.pauses.len()), (0, 4));

    said.borrow_mut().clear();
    let mut tries =
        ScriptedTries::new(transport::LOOK_CONNECT, [not_installed()], [served_vault()]);
    assert!(matches!(
        look_with(&alab(), HANG_DETECTOR, Duration::ZERO, &say, &mut tries),
        Look::Unreached(_)
    ));
    assert!(
        said.borrow().is_empty(),
        "a look that gives up says nothing held"
    );

    let mut tries = ScriptedTries::new(
        transport::LOOK_CONNECT,
        [],
        [unheard("dns: no answer within 8000ms"), served_vault()],
    );
    assert_eq!(
        look_with(&alab(), HANG_DETECTOR, patience, &say, &mut tries),
        Look::Unreached("dns: no answer within 8000ms".into()),
        "a try that fails is the answer"
    );
    assert_eq!((tries.gets, tries.pauses.len()), (1, 0));

    let mut tries = ScriptedTries::new(
        transport::LOOK_CONNECT,
        [],
        [Ok(transport::Answer::Foreign("a login page".into()))],
    );
    assert_eq!(
        look_with(&alab(), HANG_DETECTOR, patience, &|_| {}, &mut tries),
        Look::NoVault("a login page".into())
    );
}

#[test]
fn a_kept_config_copied_back_reads_the_vault_this_machine_read_before() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let first = root.path().join("first.secrets");
    let second = root.path().join("second.secrets");
    fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("first vault");
    fsx::write_atomic(&second, b"token=second-token\n", 0o600).expect("second vault");
    let reader = Paths::for_home(&root.path().join("reader"));
    join_pinned(&reader, first.to_str().unwrap(), "reader");
    let kept = reader.dir.join("before-join-1800000000");
    keep_config(&reader, &kept).expect("kept");
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&kept), 0o700);
    assert_eq!(mode(&kept.join("source.toml")), 0o600);
    assert_eq!(mode(&kept.join("state.toml")), 0o600);
    let kept_source = std::fs::read(kept.join("source.toml")).unwrap();
    let kept_state = std::fs::read(kept.join("state.toml")).unwrap();

    join_pinned(&reader, second.to_str().unwrap(), "reader");
    let token = materialize::target_path(&reader.home, "token").unwrap();
    assert_eq!(std::fs::read(&token).unwrap(), b"second-token");
    assert_eq!(
        pinned(&reader).name,
        "second",
        "the control: still the other"
    );

    keep_config(&reader, &kept).expect_err("the earlier snapshot owns this name");
    assert_eq!(
        std::fs::read(kept.join("source.toml")).unwrap(),
        kept_source
    );
    assert_eq!(std::fs::read(kept.join("state.toml")).unwrap(), kept_state);

    let partial = root.path().join("partial-kept");
    fsx::ensure_private_dir(&partial).unwrap();
    fsx::write_atomic(&partial.join("source.toml"), b"partial copy", 0o600).unwrap();
    keep_config(&reader, &partial).expect_err("a partial snapshot stays as it is");
    assert_eq!(
        std::fs::read(partial.join("source.toml")).unwrap(),
        b"partial copy"
    );
    assert!(!partial.join("state.toml").exists());

    let busy = root.path().join("busy-kept");
    {
        let _writer = StateLock::take(&reader).expect("the writer owns the state");
        keep_config(&reader, &busy).expect_err("the state is being changed");
        assert!(!busy.exists(), "no snapshot was admitted");
    }
    keep_config(&reader, &busy).expect("the coherent current state can be kept");
    assert_eq!(
        std::fs::read(busy.join("source.toml")).unwrap(),
        std::fs::read(reader.source_file()).unwrap()
    );
    assert_eq!(
        std::fs::read(busy.join("state.toml")).unwrap(),
        std::fs::read(reader.state_file()).unwrap()
    );

    let unlocked = Paths::for_home(&root.path().join("unlocked"));
    fsx::ensure_private_dir(&unlocked.dir).unwrap();
    fsx::write_atomic(&unlocked.source_file(), &kept_source, 0o600).unwrap();
    fsx::write_atomic(&unlocked.state_file(), &kept_state, 0o600).unwrap();
    let unavailable = root.path().join("unavailable-kept");
    keep_config(&unlocked, &unavailable).expect_err("existing state needs its lock");
    assert!(!unavailable.exists());
    assert_eq!(std::fs::read(unlocked.source_file()).unwrap(), kept_source);
    assert_eq!(std::fs::read(unlocked.state_file()).unwrap(), kept_state);

    for name in ["source.toml", "state.toml"] {
        std::fs::copy(kept.join(name), reader.dir.join(name)).expect("copied back");
    }
    sync(&reader, None).expect("sync");
    assert_eq!(pinned(&reader).name, "first");
    assert_eq!(std::fs::read(&token).unwrap(), b"first-token");
    assert!(materialization_ready(&reader).unwrap());

    let fresh = Paths::for_home(&root.path().join("fresh"));
    let into = root.path().join("fresh-kept");
    keep_config(&fresh, &into).expect("nothing to keep");
    assert_eq!(std::fs::read_dir(&into).unwrap().count(), 0);
    assert!(!fresh.dir.exists());

    // A state.toml past source.toml's 4 MiB, within the MAX_STATE its own reader keeps, is
    // kept whole. Control: one past MAX_STATE is refused, and no copy is made.
    let large = Paths::for_home(&root.path().join("large"));
    join_pinned(&large, first.to_str().unwrap(), "large");
    let state = std::fs::read_to_string(large.state_file()).unwrap();
    for (padding, kept) in [((4 << 20) + 1024, true), (MAX_STATE, false)] {
        let big = format!("# {}\n{state}", "x".repeat(padding));
        fsx::write_atomic(&large.state_file(), big.as_bytes(), 0o600).unwrap();
        let into = root.path().join(format!("large-kept-{padding}"));
        if kept {
            keep_config(&large, &into).expect("a state within MAX_STATE is kept");
            assert_eq!(
                std::fs::read(into.join("state.toml")).unwrap(),
                big.as_bytes()
            );
        } else {
            keep_config(&large, &into).expect_err("a state past MAX_STATE");
            assert!(!into.exists());
        }
    }

    let signed = root.path().join("signed.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    let created = create(&admin, signed.to_str().unwrap(), None, Some("admin")).unwrap();
    let targets = [
        first.to_string_lossy().into_owned(),
        format!("{}#{}", signed.display(), created.signer),
    ];
    let config_bytes = |dir: &Path| {
        ["source.toml", "state.toml"].map(|name| std::fs::read(dir.join(name)).unwrap())
    };
    for (kind, target) in targets.into_iter().enumerate() {
        for had_config in [false, true] {
            let reader = Paths::for_home(&root.path().join(format!("raced-{kind}-{had_config}")));
            if had_config {
                join_pinned(&reader, first.to_str().unwrap(), "reader");
            }
            let expected = config(&reader).unwrap();
            let kept = root.path().join(format!("raced-kept-{kind}-{had_config}"));
            if had_config {
                keep_config_if_unchanged(&reader, &kept, expected.as_ref()).unwrap();
            }
            let kept_before = had_config.then(|| config_bytes(&kept));
            let options = JoinOptions {
                source: target.clone(),
                host: Some("reader".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            };
            if !had_config {
                let pristine = Paths::for_home(&root.path().join(format!("pristine-{kind}")));
                join_if_unchanged(&pristine, &options, None).unwrap();
                assert!(pristine.identity_file().is_file());
                assert_eq!(
                    config(&pristine).unwrap().unwrap().source,
                    transport::parse_source(&target, &pristine.home).unwrap().0
                );
            }
            let bytes = || {
                [
                    reader.source_file(),
                    reader.state_file(),
                    reader.identity_file(),
                    materialize::target_path(&reader.home, "token").unwrap(),
                ]
                .map(|path| std::fs::read(path).unwrap())
            };
            let terminal = std::cell::RefCell::new(None);
            let refused = join_with(
                &reader,
                &options,
                ExpectedConfig::Read(expected.as_ref()),
                |source| {
                    let fetched = transport::fetch(source, None, Duration::from_secs(30))?;
                    join_pinned(&reader, second.to_str().unwrap(), "reader");
                    *terminal.borrow_mut() = Some(bytes());
                    Ok(fetched)
                },
            );
            assert!(
                refused.is_err(),
                "the terminal's newer vault owns the state"
            );
            assert_eq!(bytes(), terminal.into_inner().unwrap());
            assert_eq!(config(&reader).unwrap().unwrap().name, "second");

            let missed = root.path().join(format!("missed-{kind}-{had_config}"));
            keep_config_if_unchanged(&reader, &missed, expected.as_ref())
                .expect_err("the admitted vault cannot name a snapshot of the newer one");
            assert!(!missed.exists());
            if let Some(before) = kept_before {
                assert_eq!(config_bytes(&kept), before);
            }

            let current = config(&reader).unwrap();
            let admitted = root.path().join(format!("admitted-{kind}-{had_config}"));
            keep_config_if_unchanged(&reader, &admitted, current.as_ref()).unwrap();
            assert_eq!(config_bytes(&admitted), config_bytes(&reader.dir));
            join_if_unchanged(&reader, &options, current.as_ref()).unwrap();
            let wanted = config(&reader).unwrap().unwrap();
            assert_eq!(
                wanted.source,
                transport::parse_source(&target, &reader.home).unwrap().0
            );

            join_with(&reader, &options, ExpectedConfig::Any, |source| {
                let fetched = transport::fetch(source, None, Duration::from_secs(30))?;
                join_pinned(&reader, second.to_str().unwrap(), "reader");
                Ok(fetched)
            })
            .unwrap();
            assert_eq!(config(&reader).unwrap().unwrap(), wanted);
        }
    }
}

#[test]
fn a_vault_too_large_to_seal_is_refused_saying_why() {
    let key = Identity::generate().expect("a key").recipient().to_bech32();
    let mut doc = Document::new_vault("m1", &key);
    let value = vec![b'x'; text::MAX_VALUE];
    for n in 1..=5 {
        doc.set_entry(&Audience::Everyone, &format!("a{n}"), &value)
            .expect("set");
    }
    seal("alab", 1, &[7u8; 32], &[8u8; 32], false, &doc, &[]).expect("five values of 1 MiB fit");
    doc.set_entry(&Audience::Everyone, "a6", &value)
        .expect("the text holds a sixth");
    let error = seal("alab", 1, &[7u8; 32], &[8u8; 32], false, &doc, &[]).expect_err("too large");
    assert!(
        error.0.contains("too large to publish") && error.0.contains("entries fit"),
        "{error}"
    );
}

/// A pass the server answered at once, leaving this Mac's seq as it was.
const ANSWERED_AT_ONCE: Pass = Pass {
    source: SourceSaid::AtOnce,
    moved: false,
};

/// A pass that moved this Mac's seq (a version that let another Mac in, leaving this one
/// out) or that the server held is followed by the next long-poll at once, so an approval
/// that comes next is heard as it lands. Negative control: one answered at once with
/// nothing new pauses, as before (30 s after a NotIn, 10 s after nothing newer).
#[test]
fn a_pass_that_moved_the_seq_or_was_held_is_followed_at_once() {
    let url = Source::Url("http://127.0.0.1:7484/v1/alab".to_string());
    let mut failing = FIRST_RETRY;
    let moved = Pass {
        source: SourceSaid::AtOnce,
        moved: true,
    };
    let held = Pass {
        source: SourceSaid::Held,
        moved: false,
    };
    let unchanged = SyncOutcome::Unchanged { seq: 2 };
    for (outcome, pass) in [
        (&SyncOutcome::NotIn, moved),
        (&SyncOutcome::NotIn, held),
        (&unchanged, held),
    ] {
        assert_eq!(
            pause_after(&url, outcome, &mut failing, pass),
            Duration::ZERO,
            "{outcome:?} {pass:?}"
        );
    }
    let at_once = |outcome| {
        let mut failing = FIRST_RETRY;
        pause_after(&url, outcome, &mut failing, ANSWERED_AT_ONCE)
    };
    assert_eq!(at_once(&SyncOutcome::NotIn).as_secs(), 30);
    assert_eq!(at_once(&unchanged).as_secs(), 10);
}

/// A resting approver (longer than [`WATCHED_NAP`]: no admin here, as on every member Mac,
/// or after a failure) reads nothing of this Mac's state while it naps; the short wait for a
/// version this Mac applies reads the seq it waits past. Negative control: a nap that
/// watches the seq whatever its length reads the state of every member Mac ten times a
/// second.
#[test]
fn a_resting_approver_reads_no_state_while_it_naps() {
    let stopped = aterm_uds::stop::Stop::new();
    stopped.set();
    let reads = std::cell::Cell::new(0);
    let seq = || {
        reads.set(reads.get() + 1);
        Some(7)
    };
    approver_nap(&stopped, Duration::from_secs(30), &seq);
    assert_eq!(reads.get(), 0, "a resting approver read this Mac's state");
    approver_nap(&stopped, WATCHED_NAP, &seq);
    assert_eq!(
        reads.get(),
        1,
        "the wait for a version reads the seq it waits past"
    );
}

/// A RESTING APPROVER SLEEPS ON THE STOP: an hour's rest ([`approver_nap`]), and an hour's
/// watched wait ([`nap_unless`]), each asleep on the stop, end when the subscriber beside
/// them ends and sets it, not when their time is up. The stop is set only once the nap sleeps
/// on it ([`aterm_uds::stop::Stop::entered`]). Negative control (2026-11-10): a rest, or a
/// watched wait's look, that sleeps without the stop never sleeps on it, which trips the
/// one-minute hang detector.
#[test]
fn a_resting_approver_ends_its_nap_when_the_stop_is_set() {
    let naps: [fn(&aterm_uds::stop::Stop); 2] = [
        |stop| approver_nap(stop, Duration::from_secs(3600), &|| Some(7)),
        |stop| nap_unless(stop, Duration::from_secs(3600), NAP_LOOK, &|| false),
    ];
    for nap in naps {
        let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
        let (done, ended) = std::sync::mpsc::channel();
        let napping = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            nap(&napping);
            done.send(()).expect("the test waits");
        });
        let hang = Instant::now() + Duration::from_secs(60);
        while stop.entered() == 0 {
            assert!(Instant::now() < hang, "the nap never slept on the stop");
            std::thread::sleep(Duration::from_millis(1));
        }
        stop.set();
        ended
            .recv_timeout(Duration::from_secs(60))
            .expect("the nap ends when the stop is set");
        thread.join().expect("the nap ends");
    }
}

/// THE APPROVER ENDS HOWEVER THE SUBSCRIBER ENDS: a subscriber that panics sets the stop as
/// it unwinds, so the approver asleep on it ends and the panic reaches `watch`'s caller,
/// rather than the scope waiting on the approver for good. Negative control (2026-11-10):
/// with the stop set only after the subscriber returns, the approver's hour-long sleep trips
/// the one-minute hang detector.
#[test]
fn a_subscriber_that_panics_ends_the_approver_beside_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct OwnedUpkeep {
        stop: Arc<aterm_uds::stop::Stop>,
        worker: Option<std::thread::JoinHandle<()>>,
        dropped: Arc<AtomicBool>,
        paused: Arc<AtomicUsize>,
    }
    impl AccountUpkeep for OwnedUpkeep {
        fn step(&mut self, _: &Paths, _: &Admin, _: Instant) -> crate::Result<usize> {
            panic!("teardown grants no admin authority")
        }
        fn pause(&mut self) {
            self.paused.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Drop for OwnedUpkeep {
        fn drop(&mut self) {
            self.stop.set();
            self.worker
                .take()
                .unwrap()
                .join()
                .expect("the owned worker ends");
            self.dropped.store(true, Ordering::Release);
        }
    }

    for panics in [false, true] {
        let (done, ended) = std::sync::mpsc::channel();
        let watch = std::thread::spawn(move || {
            let joined = Arc::new(AtomicBool::new(false));
            let dropped = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicUsize::new(0));
            let outcome = std::panic::catch_unwind(|| {
                beside_the_approver(
                    |stop| {
                        let worker_stop = Arc::new(aterm_uds::stop::Stop::new());
                        let ending = Arc::clone(&worker_stop);
                        let joined = Arc::clone(&joined);
                        let worker = std::thread::spawn(move || {
                            ending.sleep(Duration::from_secs(3600));
                            joined.store(true, Ordering::Release);
                        });
                        let mut approver = Approver {
                            account_upkeep: Some(Box::new(OwnedUpkeep {
                                stop: worker_stop,
                                worker: Some(worker),
                                dropped: Arc::clone(&dropped),
                                paused: Arc::clone(&paused),
                            })),
                            account_upkeep_at: Some(Instant::now()),
                            ..Approver::default()
                        };
                        approver.pause_account_upkeep();
                        assert!(approver.account_upkeep_at.is_none());
                        assert_eq!(paused.load(Ordering::SeqCst), 1);
                        assert!(
                            !dropped.load(Ordering::Acquire),
                            "pause never drops or joins"
                        );
                        stop.sleep(Duration::from_secs(3600));
                    },
                    || {
                        if panics {
                            panic!("the subscriber panics");
                        }
                    },
                )
            });
            done.send((
                outcome.is_err(),
                joined.load(Ordering::Acquire),
                dropped.load(Ordering::Acquire),
            ))
            .expect("the test waits");
        });
        let (panicked, joined, dropped) = ended
            .recv_timeout(Duration::from_secs(60))
            .expect("the approver and its worker end with the subscriber");
        assert_eq!(
            panicked, panics,
            "the subscriber's outcome reaches the caller"
        );
        assert!(
            joined && dropped,
            "owned upkeep is joined before watch returns or re-raises"
        );
        watch.join().expect("the watch ends");
    }
}

/// A pass that writes N files flushes each in order before its rename and its directory
/// once, not each file and its directory durably: 20 entries cost the [pending] record's 2
/// durable flushes, 20 ordered ones and 1 for `~/.secrets`, where they cost 42 durable
/// ones. Negative control: `write_atomic` in `place` brings back 2 durable flushes a file.
#[test]
fn a_pass_of_many_entries_flushes_each_in_order_and_its_directory_once() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).expect("state dir");
    let entries: Vec<Entry> = (0..20)
        .map(|n| Entry {
            name: format!("token{n:02}"),
            value: SecretBytes::from(format!("inert-{n}").as_bytes()),
            audience: crate::text::Audience::Everyone,
            if_absent: false,
            anchor: None,
        })
        .collect();
    let mut state = State::default();
    let recording = aterm_durable::seam::Recording::start();
    let report = apply_entries(&paths, &mut state, &entries).expect("applied");
    let flushes = recording.take();
    assert_eq!(report.written.len(), 20, "{report:?}");
    let durable = flushes.iter().filter(|flush| flush.is_durable()).count();
    let ordered = flushes.iter().filter(|flush| flush.is_ordered()).count();
    assert_eq!((durable, ordered), (2 + 1, 20), "{flushes:?}");
    // What a cut-short pass resumes from is on disk before the first file is written.
    assert!(state.pending.is_empty(), "every write landed");
    for entry in &entries {
        let target = materialize::target_path(&paths.home, &entry.name).expect("target");
        assert_eq!(
            std::fs::read(target).expect("written"),
            entry.value.as_slice()
        );
    }
}

#[test]
fn a_pass_that_leaves_an_entry_failed_backs_off_and_a_clean_one_starts_again() {
    let url = Source::Url("http://127.0.0.1:7484/v1/alab".to_string());
    let applied = |failed: usize| SyncOutcome::Applied {
        seq: 2,
        report: crate::materialize::Report {
            failed: (0..failed)
                .map(|n| (format!("entry{n}"), "is a symlink".to_string()))
                .collect(),
            ..Default::default()
        },
    };
    let mut failing = FIRST_RETRY;
    let pauses: Vec<u64> = (0..8)
        .map(|_| pause_after(&url, &applied(1), &mut failing, ANSWERED_AT_ONCE).as_secs())
        .collect();
    assert_eq!(pauses, [5, 10, 20, 40, 80, 160, 300, 300]);
    assert_eq!(
        pause_after(&url, &applied(0), &mut failing, ANSWERED_AT_ONCE),
        Duration::ZERO
    );
    assert_eq!(
        pause_after(&url, &applied(2), &mut failing, ANSWERED_AT_ONCE),
        FIRST_RETRY
    );
    pause_after(&url, &applied(2), &mut failing, ANSWERED_AT_ONCE);
    let unchanged = SyncOutcome::Unchanged { seq: 2 };
    assert_eq!(
        pause_after(&url, &unchanged, &mut failing, ANSWERED_AT_ONCE).as_secs(),
        10
    );
    assert_eq!(
        pause_after(&url, &applied(1), &mut failing, ANSWERED_AT_ONCE),
        FIRST_RETRY
    );
    let path = Source::Path(PathBuf::from("/tmp/vault"));
    assert_eq!(
        pause_after(&path, &applied(1), &mut failing, ANSWERED_AT_ONCE).as_secs(),
        60
    );
}

fn bound() -> std::net::TcpListener {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind")
}

fn serve(store: &Path, socket: &std::net::TcpListener) -> crate::server::Running {
    crate::server::start(
        socket.try_clone().expect("a copy of the socket"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("server starts")
}

struct Unserved {
    stop: std::sync::Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    closer: std::thread::JoinHandle<()>,
}

impl Unserved {
    fn at(socket: &std::net::TcpListener) -> Unserved {
        let listener = socket.try_clone().expect("a copy of the socket");
        let addr = listener.local_addr().expect("its address");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&stop);
        let closer = std::thread::spawn(move || {
            loop {
                let stream = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .map(|(stream, _)| stream);
                drop(stream);
                if seen.load(Ordering::SeqCst) {
                    return;
                }
            }
        });
        Unserved { stop, addr, closer }
    }

    fn end(self) {
        self.stop.store(true, Ordering::SeqCst);
        std::net::TcpStream::connect(self.addr).expect("the closer is woken");
        self.closer.join().expect("the closer ends");
    }
}

fn retiring<T>(keys: Vec<[u8; 32]>, current: [u8; 32], flow: impl FnOnce() -> T) -> T {
    let set = |retired: Option<Retired>| {
        envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = retired);
    };
    set(Some(Retired {
        keys,
        current: Some(current),
    }));
    let out = flow();
    set(None);
    out
}

fn join_pinned(paths: &Paths, source: &str, host: &str) -> JoinOutcome {
    join(
        paths,
        &JoinOptions {
            source: source.to_string(),
            host: Some(host.to_string()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .expect("join")
}

#[test]
fn materialization_observation_explains_each_guard_without_reading_or_mutating_inputs() {
    fn blocked(paths: &Paths, config: &Config, expected: MaterializationBlocker) {
        let before_state = std::fs::read(paths.state_file()).ok();
        let before_config = std::fs::read(paths.source_file()).ok();
        let called = std::cell::Cell::new(false);
        let observed = observe_materialization(paths, config, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(
            matches!(observed, MaterializationObservation::Blocked(reason) if reason == expected)
        );
        assert!(!called.get());
        // A reader that gates work waits a writer out; one that outlasts the wait is said
        // as that, never as a vault that is not ready.
        let (ready, gated) = with_current_wait(Duration::from_millis(20), || {
            (
                materialization_ready_for(paths, config),
                with_materialization_ready::<()>(paths, config, || {
                    panic!("a blocked legacy reader must not run")
                }),
            )
        });
        match expected {
            MaterializationBlocker::WriterBusy => {
                assert_eq!(
                    ready.err().map(|e| e.kind()),
                    Some(crate::ErrorKind::VaultBusy)
                );
                assert_eq!(
                    gated.err().map(|e| e.kind()),
                    Some(crate::ErrorKind::VaultBusy)
                );
            }
            _ => {
                assert!(!ready.unwrap());
                assert!(gated.unwrap().is_none());
            }
        }
        assert_eq!(std::fs::read(paths.state_file()).ok(), before_state);
        assert_eq!(std::fs::read(paths.source_file()).ok(), before_config);
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.secrets");
    fsx::write_atomic(&source, b"token=inert-token\n", 0o600).unwrap();
    let paths = Paths::for_home(&root.path().join("reader"));
    let initial = Config {
        source: Source::Path(source.clone()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "reader".into(),
    };
    blocked(&paths, &initial, MaterializationBlocker::LockMissing);
    assert!(!paths.home.exists());
    drop(StateLock::take(&paths).unwrap());
    blocked(&paths, &initial, MaterializationBlocker::Unconfigured);
    join_pinned(&paths, source.to_str().unwrap(), "reader");
    let configured = config(&paths).unwrap().unwrap();
    let writer = StateLock::take(&paths).unwrap();
    blocked(&paths, &configured, MaterializationBlocker::WriterBusy);
    drop(writer);
    let mut changed = configured.clone();
    changed.name.push_str("-other");
    blocked(&paths, &changed, MaterializationBlocker::ConfigChanged);
    let original = load_state(&paths).unwrap();
    for (membership, expected) in [
        (Stored::Pending, MaterializationBlocker::NotAdmitted),
        (Stored::Revoked, MaterializationBlocker::Revoked),
    ] {
        let mut state = original.clone();
        state.membership = membership;
        save_state(&paths, &state).unwrap();
        blocked(&paths, &configured, expected);
    }
    let mut state = original.clone();
    state.source_identity = None;
    save_state(&paths, &state).unwrap();
    blocked(&paths, &configured, MaterializationBlocker::SourceChanged);
    state = original.clone();
    state.failed = true;
    save_state(&paths, &state).unwrap();
    blocked(
        &paths,
        &configured,
        MaterializationBlocker::LocalFilesIncomplete,
    );
    save_state(&paths, &original).unwrap();
    std::fs::remove_file(materialize::target_path(&paths.home, "token").unwrap()).unwrap();
    blocked(
        &paths,
        &configured,
        MaterializationBlocker::LocalFilesIncomplete,
    );

    let admin = Paths::for_home(&root.path().join("admin"));
    let signed_source = root.path().join("signed.vault");
    create(&admin, signed_source.to_str().unwrap(), None, Some("admin")).unwrap();
    let configured = config(&admin).unwrap().unwrap();
    let old = configured.signer.unwrap();
    let current = envelope::signer_public(&[238; 32]).unwrap();
    retiring(vec![old], current, || {
        blocked(&admin, &configured, MaterializationBlocker::UntrustedPin);
    });
}

#[test]
fn materialization_observation_reads_only_while_ready_and_holding_the_shared_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.secrets");
    fsx::write_atomic(&source, b"token=inert-token\n", 0o600).unwrap();
    let paths = Paths::for_home(&root.path().join("reader"));
    join_pinned(&paths, source.to_str().unwrap(), "reader");
    let configured = config(&paths).unwrap().unwrap();
    let before_state = std::fs::read(paths.state_file()).unwrap();
    let before_config = std::fs::read(paths.source_file()).unwrap();
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.lock_file())
        .unwrap();
    let calls = std::cell::Cell::new(0);
    let observation = observe_materialization(&paths, &configured, || {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test checks the shared observation lease without blocking"
        )]
        let attempt = writer.try_lock();
        assert!(matches!(attempt, Err(std::fs::TryLockError::WouldBlock)));
        calls.set(calls.get() + 1);
        Ok(42)
    })
    .unwrap();
    assert!(matches!(observation, MaterializationObservation::Ready(value) if value.inputs == 42));
    assert_eq!(calls.get(), 1);
    assert!(materialization_ready_for(&paths, &configured).unwrap());
    assert!(
        with_materialization_ready(&paths, &configured, || Ok(42))
            .unwrap()
            .is_some()
    );
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), before_state);
    assert_eq!(std::fs::read(paths.source_file()).unwrap(), before_config);
}

#[test]
fn readiness_observation_creates_no_state_or_lock_and_never_changes_directory_mode() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let reader = Paths::for_home(&root.path().join("absent"));
    let expected = Config {
        source: Source::Path(root.path().join("team.secrets")),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "reader".into(),
    };
    assert!(!materialization_ready(&reader).unwrap());
    assert!(!materialization_ready_for(&reader, &expected).unwrap());
    assert!(!reader.home.exists());

    std::fs::create_dir_all(&reader.dir).unwrap();
    std::fs::set_permissions(&reader.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!materialization_ready(&reader).unwrap());
    assert!(!reader.lock_file().exists());
    assert_eq!(
        std::fs::metadata(&reader.dir).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let linked_reader = Paths::for_home(&root.path().join("linked"));
    std::fs::create_dir_all(linked_reader.dir.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&reader.dir, &linked_reader.dir).unwrap();
    assert!(StateLock::observe(&linked_reader).is_err());

    fsx::ensure_private_dir(&reader.dir).unwrap();
    let _lock = StateLock::take(&reader).unwrap();
    drop(_lock);
    std::fs::set_permissions(reader.lock_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(StateLock::observe(&reader).is_err());
    std::fs::set_permissions(reader.lock_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let hard_lock = reader.dir.join("state.lock.hard");
    std::fs::hard_link(reader.lock_file(), &hard_lock).unwrap();
    assert!(StateLock::observe(&reader).is_err());
    std::fs::remove_file(&hard_lock).unwrap();
    assert!(StateLock::observe(&reader).unwrap().is_some());
}

#[test]
fn readiness_observation_yields_to_writers_and_coexists_with_shared_readers() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.secrets");
    fsx::write_atomic(&source, b"token=inert-token\n", 0o600).unwrap();
    let reader = Paths::for_home(&root.path().join("reader"));
    join_pinned(&reader, source.to_str().unwrap(), "reader");
    let configured = config(&reader).unwrap().unwrap();
    let writer = StateLock::take(&reader).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let observing = reader.clone();
    let worker = std::thread::spawn(move || {
        send.send(observe_materialization(&observing, &configured, || Ok(())))
            .unwrap();
    });
    let status = receive.recv_timeout(Duration::from_secs(60));
    // A readiness check waits the writer out and reads the vault once it lets go: the
    // writer lets go only from inside that wait.
    let mut writer = Some(writer);
    let (ready, tries) = test_while_state_lock_held(
        move || drop(writer.take()),
        || materialization_ready(&reader),
    );
    worker.join().unwrap();
    assert!(matches!(
        status
            .expect("a status read must not wait for the writer")
            .unwrap(),
        MaterializationObservation::Blocked(MaterializationBlocker::WriterBusy)
    ));
    assert!(ready.unwrap());
    assert!(tries >= 1, "the readiness check met the writer and waited");
    assert!(materialization_ready(&reader).unwrap());

    let shared = StateLock::observe(&reader).unwrap().expect("first reader");
    assert!(materialization_ready(&reader).unwrap());
    drop(shared);
    let writer = StateLock::take(&reader).unwrap();
    drop(writer);
    assert!(materialization_ready(&reader).unwrap());
}

/// A file the vault wrote whose directory then became a symlink (a dotfiles setup's
/// `~/.config/gh`) is held without a look, so readiness does not judge it through the link,
/// while the same file missing from a real directory is incomplete.
#[test]
fn readiness_does_not_judge_a_held_file_behind_a_symlinked_directory() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.secrets");
    let text = b"~/.config/gh/hosts.yml<<END if-absent\ngithub.com: {}\nEND\n";
    fsx::write_atomic(&source, text, 0o600).unwrap();
    let reader = Paths::for_home(&root.path().join("reader"));
    join_pinned(&reader, source.to_str().unwrap(), "reader");
    assert!(materialization_ready(&reader).unwrap(), "filled");
    let gh = reader.home.join(".config/gh");
    let dotfiles = root.path().join("dotfiles");
    std::fs::create_dir(&dotfiles).unwrap();
    std::fs::remove_dir_all(&gh).unwrap();
    std::os::unix::fs::symlink(&dotfiles, &gh).unwrap();
    sync(&reader, None).expect("synced");
    assert!(
        materialization_ready(&reader).unwrap(),
        "held behind the link, not judged through it"
    );
    std::fs::remove_file(&gh).unwrap();
    std::fs::create_dir(&gh).unwrap();
    assert!(
        !materialization_ready(&reader).unwrap(),
        "missing from a real directory"
    );
}

#[test]
fn readiness_refuses_a_changed_source_before_its_file_digests() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let first = root.path().join("first.secrets");
    let second = root.path().join("second.secrets");
    fsx::write_atomic(&first, b"token=first-token\n", 0o600).unwrap();
    fsx::write_atomic(&second, b"token=second-token\n", 0o600).unwrap();
    let reader = Paths::for_home(&root.path().join("reader"));
    join_pinned(&reader, first.to_str().unwrap(), "reader");
    let expected = status(&reader).unwrap().config.unwrap();
    assert!(materialization_ready_for(&reader, &expected).unwrap());
    join_pinned(&reader, second.to_str().unwrap(), "reader");
    let digests_read = std::cell::Cell::new(false);
    assert!(
        !materialization_ready_with(&reader, Some(&expected), |paths, state| {
            digests_read.set(true);
            files_intact(paths, state)
        })
        .unwrap()
    );
    assert!(!digests_read.get());
    assert!(!materialization_ready_for(&reader, &expected).unwrap());
    assert!(materialization_ready(&reader).unwrap());
    assert!(materialization_ready_for(&reader, &pinned(&reader)).unwrap());

    join_pinned(&reader, first.to_str().unwrap(), "reader");
    assert!(materialization_ready_for(&reader, &expected).unwrap());
}

#[test]
fn readiness_lock_admission_refuses_symlinks_and_nonregular_files() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let reader = Paths::for_home(&root.path().join("reader"));
    fsx::ensure_private_dir(&reader.dir).unwrap();
    let target = root.path().join("owned-target");
    fsx::write_atomic(&target, b"inert target", 0o600).unwrap();
    std::os::unix::fs::symlink(&target, reader.lock_file()).unwrap();
    assert!(StateLock::observe(&reader).is_err());
    assert!(StateLock::take(&reader).is_err());
    assert!(StateLock::try_take(&reader).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"inert target");
    std::fs::remove_file(reader.lock_file()).unwrap();
    std::fs::create_dir(reader.lock_file()).unwrap();
    assert!(StateLock::observe(&reader).is_err());
    assert!(StateLock::take(&reader).is_err());
    assert!(StateLock::try_take(&reader).is_err());
}

#[test]
fn a_store_holds_the_alab_vault_only_under_a_key_this_build_names_for_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    // A vault named `alab`, signed by a scratch key standing in for the ALab key.
    let admin = Paths::for_home(&root.path().join("admin"));
    let source = store.join("alab.vault");
    create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let signer = pinned(&admin).signer.expect("pin");
    let other = envelope::signer_public(&[239; 32]).expect("public key");
    assert!(
        !crate::server::holds_alab(&store),
        "a vault named alab under a key this build does not name is anyone's"
    );
    assert!(retiring(Vec::new(), signer, || crate::server::holds_alab(
        &store
    )));
    assert!(
        retiring(vec![signer], other, || crate::server::holds_alab(&store)),
        "a retired key's vault is still the one the store holds until its successor comes"
    );
    // Another vault's bytes in the ALab vault's file, or bytes that do not verify, are not it.
    let bytes = std::fs::read(&source).unwrap();
    let elsewhere = Paths::for_home(&root.path().join("elsewhere"));
    let home = root.path().join("home.vault");
    create(&elsewhere, home.to_str().unwrap(), None, Some("elsewhere")).unwrap();
    let home_signer = pinned(&elsewhere).signer.expect("pin");
    std::fs::copy(&home, &source).unwrap();
    assert!(
        !retiring(Vec::new(), home_signer, || crate::server::holds_alab(
            &store
        )),
        "the vault `home` is not the ALab vault"
    );
    let mut forged = bytes;
    let at = forged.len() / 2;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    std::fs::write(&source, &forged).unwrap();
    assert!(!retiring(Vec::new(), signer, || crate::server::holds_alab(
        &store
    )));
}

#[test]
fn pin_admission_follows_verified_rotation_and_compiled_replacement() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
        .expect("create scratch vault");
    let original = pinned(&admin).signer.expect("original pin");
    rotate(&admin);
    let successor = pinned(&admin).signer.expect("successor pin");
    rotate(&admin);
    let config = pinned(&admin);
    let current = config.signer.expect("current pin");
    let unrelated = envelope::signer_public(&[239; 32]).expect("unrelated public key");
    for pin in [original, successor, current] {
        assert!(pin_admitted(&config, Some(pin)));
    }
    assert!(!pin_admitted(&config, Some(unrelated)));

    let mut fork = config.clone();
    fork.signer = Some(unrelated);
    fork.followed = vec![original];
    let admitted = retiring(vec![original], successor, || {
        [
            pin_admitted(&config, Some(original)),
            pin_admitted(&config, Some(successor)),
            pin_admitted(&config, Some(current)),
            pin_admitted(&config, Some(unrelated)),
            pin_admitted(&fork, None),
            pin_admitted(&fork, Some(unrelated)),
        ]
    });
    assert_eq!(admitted, [true, true, true, false, false, false]);

    let mut unsigned = config;
    unsigned.signer = None;
    unsigned.followed.clear();
    assert!(pin_admitted(&unsigned, None));
    assert!(!pin_admitted(&unsigned, Some(current)));
    unsigned.source = Source::Url("https://vault.example/v1/team".into());
    assert!(!pin_admitted(&unsigned, None));
}

#[test]
fn a_join_of_a_signed_path_that_now_holds_plain_text_keeps_the_pin_and_writes_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    let created = create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let before = pinned(&admin);
    assert!(before.signer.is_some());
    // Someone who can write the file puts plain text in the vault's place.
    fsx::write_atomic(&source, b"~/.config/gh/hosts.yml=inert-planted\n", 0o600).unwrap();
    let planted = |paths: &Paths| {
        materialize::target_path(&paths.home, "~/.config/gh/hosts.yml")
            .unwrap()
            .exists()
    };
    let join_typed = |paths: &Paths, typed: String| {
        join(
            paths,
            &JoinOptions {
                source: typed,
                host: Some("reader".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
    };

    // The Mac that pins it, joining again by the bare path.
    let refused = join_typed(&admin, source.display().to_string()).expect_err("refused");
    assert!(
        refused.to_string().contains("holds plain text"),
        "{refused}"
    );
    assert_eq!(pinned(&admin), before, "the pin stands");
    assert!(!planted(&admin));
    // A Mac that names the signer it expects.
    let fresh = Paths::for_home(&root.path().join("fresh"));
    join_typed(&fresh, format!("{}#{}", source.display(), created.signer)).expect_err("refused");
    assert!(config(&fresh).unwrap().is_none(), "nothing configured");
    assert!(!planted(&fresh));
    // A Mac that pins nothing there may read a plain file.
    let plain = Paths::for_home(&root.path().join("plain"));
    join_typed(&plain, source.display().to_string()).expect("a plain file");
    assert!(planted(&plain));
}

#[test]
fn only_a_pin_this_build_names_for_alab_or_one_reached_from_it_is_the_alab_vault() {
    let key = |seed: u8| envelope::signer_public(&[seed; 32]).expect("public key");
    let (retired, current, successor, other) = (key(1), key(2), key(3), key(4));
    let config = |signer: Option<[u8; 32]>, followed: Vec<[u8; 32]>| Config {
        source: Source::Url("https://anywhere.example/v1/alab".into()),
        mirrors: Vec::new(),
        name: "alab".into(),
        signer,
        followed,
        host: "mac".into(),
    };
    let judged = retiring(vec![retired], current, || {
        [
            // The key this build names, at any address.
            alab_vault(&config(Some(current), Vec::new())),
            // A rotation this build does not know yet, reached from the current key.
            alab_vault(&config(Some(successor), vec![current])),
            // Another vault, and a plain file.
            alab_vault(&config(Some(other), Vec::new())),
            alab_vault(&config(None, Vec::new())),
            // A retired pin, and a key a retired one endorsed that this build does not
            // know: both give way to the current key, so neither is the vault this build trusts.
            alab_vault(&config(Some(retired), Vec::new())),
            alab_vault(&config(Some(other), vec![retired])),
            // A retired key's endorsement of the current one is the ALab vault's own handoff.
            alab_vault(&config(Some(current), vec![retired])),
        ]
    });
    assert_eq!(judged, [true, true, false, false, false, false, true]);
}

#[test]
fn a_key_the_chain_names_before_a_link_that_does_not_verify_is_no_predecessor() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
        .expect("create scratch vault");
    let opened = admin_open(&admin).expect("open");
    let signer = envelope::signer_public(&opened.seed).expect("public");
    let named = [239; 32];
    let key = envelope::signer_public(&named).expect("public");
    let signed = |seq: u64, handoff: Vec<envelope::Link>, seed: &[u8; 32]| {
        let unsigned = Unsigned {
            name: "team".to_string(),
            seq,
            time: 0,
            handoff,
            parents: Vec::new(),
            lockers: Vec::new(),
        };
        envelope::sign_unchecked(&unsigned, seed).expect("signed")
    };
    let cached = |endorsement: [u8; 64]| {
        let bytes = signed(opened.seq() + 1, vec![(key, endorsement)], &opened.seed);
        // As every writer of the held vault does: a rename over it, so the read after it
        // meets a new file's stamp ([`chain_cache::held`]).
        crate::fsx::write_atomic(&admin.cache_file(), &bytes, 0o600).expect("cached");
        pinned(&admin)
    };
    let config = cached([7; 64]);
    assert_eq!((config.signer, config.followed.len()), (Some(signer), 0));
    assert!(!pin_admitted(&config, Some(key)));
    let config = cached(envelope::endorse(&named, "team", &signer).expect("endorsed"));
    assert_eq!(config.followed, vec![key]);
    assert!(pin_admitted(&config, Some(key)));

    let next_seed = [241; 32];
    let next = envelope::signer_public(&next_seed).expect("public");
    let handoff = vec![
        (key, [7; 64]),
        (
            signer,
            envelope::endorse(&opened.seed, "team", &next).expect("endorsed"),
        ),
    ];
    std::fs::write(&source, signed(opened.seq() + 2, handoff, &next_seed)).expect("written");
    sync(&admin, None).expect("synced");
    let config = pinned(&admin);
    assert_eq!(
        (config.signer, config.followed.clone()),
        (Some(next), vec![signer])
    );
    assert!(!pin_admitted(&config, Some(key)));
    assert!(pin_admitted(&config, Some(signer)));
    let reader = Paths::for_home(&root.path().join("reader"));
    let pin = crate::b64::encode_padded(&signer);
    join_pinned(&reader, &format!("{}#{pin}", source.display()), "reader");
    let config = pinned(&reader);
    assert_eq!(
        (config.signer, config.followed.clone()),
        (Some(next), vec![signer])
    );
    assert!(!pin_admitted(&config, Some(key)));
}

#[test]
fn materialization_readiness_rejects_intact_files_under_retired_authority() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
        .expect("create scratch vault");
    let opened = admin_open(&admin).expect("open scratch vault");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .expect("add test credential");
    admin_publish(&admin, &opened, &doc).expect("publish test credential");
    sync(&admin, None).expect("materialize test credential");
    assert!(materialization_ready(&admin).expect("ready before retirement"));
    let old = pinned(&admin).signer.expect("held pin");
    let current = envelope::signer_public(&[238; 32]).expect("replacement public key");
    let state_before = std::fs::read(admin.state_file()).expect("state before check");
    let source_before = std::fs::read(admin.source_file()).expect("source before check");
    let ready = retiring(vec![old], current, || materialization_ready(&admin));
    assert!(!ready.expect("retired authority refused"));
    assert_eq!(std::fs::read(admin.state_file()).unwrap(), state_before);
    assert_eq!(std::fs::read(admin.source_file()).unwrap(), source_before);
    assert!(materialization_ready(&admin).expect("test authority restored"));
}

#[test]
fn plain_source_changes_never_admit_the_previous_sources_intact_files() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let first = root.path().join("first.secrets");
    let second = root.path().join("second.secrets");
    fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("first vault");
    fsx::write_atomic(&second, b"token=second-token\n", 0o600).expect("second vault");
    let reader = Paths::for_home(&root.path().join("reader"));
    join_pinned(&reader, first.to_str().unwrap(), "reader");
    assert!(materialization_ready(&reader).unwrap());
    let original = pinned(&reader);
    let mut changed = original.clone();
    changed.source = Source::Path(second.clone());
    changed.name = "second".into();

    save_config(&reader, &changed).unwrap();
    assert!(!materialization_ready(&reader).unwrap());
    let mut legacy = load_state(&reader).unwrap();
    legacy.source_identity = None;
    legacy.failed = false;
    save_state(&reader, &legacy).unwrap();
    assert!(!materialization_ready(&reader).unwrap());
    sync(&reader, None).expect("migrate the old record");
    assert!(materialization_ready(&reader).unwrap());
    let token = materialize::target_path(&reader.home, "token").unwrap();
    assert_eq!(std::fs::read(&token).unwrap(), b"second-token");

    let mut state = load_state(&reader).unwrap();
    state.seq = 0;
    state.digest = None;
    state.membership = Stored::Plain;
    prepare_materialization(&reader, &mut state).unwrap();
    assert!(!materialization_ready(&reader).unwrap());
    save_config(&reader, &original).unwrap();
    std::fs::remove_file(&first).expect("remove only scratch vault");
    assert!(sync(&reader, None).is_err());
    assert!(!materialization_ready(&reader).unwrap());
    assert_eq!(std::fs::read(&token).unwrap(), b"second-token");
    fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("restore scratch vault");
    sync(&reader, None).expect("finish the interrupted join");
    assert!(materialization_ready(&reader).unwrap());
    assert_eq!(std::fs::read(token).unwrap(), b"first-token");
}

#[test]
fn signed_source_binding_and_applied_digest_are_required_until_sync_repairs_them() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"intact-token")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    sync(&admin, None).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    for missing_digest in [false, true] {
        let mut legacy = load_state(&admin).unwrap();
        if missing_digest {
            legacy.digest = None;
        } else {
            legacy.source_identity = None;
        }
        legacy.failed = false;
        save_state(&admin, &legacy).unwrap();
        assert!(!materialization_ready(&admin).unwrap());
        assert!(matches!(
            sync(&admin, None).unwrap(),
            SyncOutcome::Applied { .. }
        ));
        assert!(materialization_ready(&admin).unwrap());
    }

    let old_config = pinned(&admin);
    let old_state = load_state(&admin).unwrap();
    rotate(&admin);
    let new_config = pinned(&admin);
    assert_ne!(old_config.signer, new_config.signer);
    assert!(
        materialization_ready(&admin).unwrap(),
        "one sync binds the new signer"
    );
    save_state(&admin, &old_state).unwrap();
    assert!(
        !materialization_ready(&admin).unwrap(),
        "old signer binding cannot admit new config"
    );

    for replaced in [false, true] {
        save_config(&admin, &old_config).unwrap();
        let mut interrupted = old_state.clone();
        if replaced {
            interrupted.seq = 0;
            interrupted.digest = None;
        }
        prepare_materialization(&admin, &mut interrupted).unwrap();
        save_config(&admin, &new_config).unwrap();
        assert!(!materialization_ready(&admin).unwrap());
        sync(&admin, None).expect("resume after rotated or replaced config publication");
        assert!(materialization_ready(&admin).unwrap());
        assert_eq!(
            load_state(&admin).unwrap().source_identity,
            Some(source_identity(&new_config))
        );
    }
}

#[test]
fn a_path_subscriber_puts_back_a_rewritten_file_once_per_version() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let replaced = |paths: &Paths| {
        std::fs::read_dir(paths.home.join(".secrets").join(materialize::REPLACED_DIR))
            .map_or(0, Iterator::count)
    };
    // A signed vault at a path, read by its admin.
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    // A plain file at a path.
    let text = root.path().join("home.secrets");
    fsx::write_atomic(&text, b"token=inert-token\n", 0o600).unwrap();
    let plain = Paths::for_home(&root.path().join("plain"));
    join_pinned(&plain, text.to_str().unwrap(), "plain");

    let (mut on_admin, mut on_plain) = (None, None);
    for (paths, repaired) in [(&admin, &mut on_admin), (&plain, &mut on_plain)] {
        let token = materialize::target_path(&paths.home, "token").unwrap();
        let mut pass = || subscriber_pass(paths, repaired, &FOUND_NONE).expect("a pass");
        pass();
        assert_eq!(std::fs::read(&token).unwrap(), b"inert-token");
        assert_eq!(replaced(paths), 0);
        // A program on this Mac rewrites the file: put back once...
        fsx::write_atomic(&token, b"program-1", 0o600).unwrap();
        pass();
        assert_eq!(std::fs::read(&token).unwrap(), b"inert-token");
        assert_eq!(replaced(paths), 1);
        pass();
        // ...not every minute it rewrites it again.
        fsx::write_atomic(&token, b"program-2", 0o600).unwrap();
        pass();
        pass();
        assert_eq!(std::fs::read(&token).unwrap(), b"program-2");
        assert_eq!(replaced(paths), 1, "one copy moved aside, not one a minute");
    }
    // A new version of the plain file is written whatever was put back before.
    fsx::write_atomic(&text, b"token=inert-token\nnote=inert-note\n", 0o600).unwrap();
    subscriber_pass(&plain, &mut on_plain, &FOUND_NONE).expect("a pass");
    let read = |name: &str| std::fs::read(materialize::target_path(&plain.home, name).unwrap());
    assert_eq!(read("note").unwrap(), b"inert-note");
    assert_eq!(read("token").unwrap(), b"inert-token");
}

#[test]
fn the_subscriber_repairs_a_changed_file_once_per_version_so_readiness_returns() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .unwrap();
    doc.set_entry_with(
        &Audience::Everyone,
        "~/.config/git/config",
        b"[tool]\n",
        true,
        None,
    )
    .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    sync(&admin, None).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    let mut repaired = None;
    assert!(!repair_due(&admin, &mut repaired), "nothing to repair");

    let config = materialize::target_path(&admin.home, "~/.config/git/config").unwrap();
    fsx::write_atomic(&config, b"[tool]\nhelper = local\n", 0o600).unwrap();
    let token = materialize::target_path(&admin.home, "token").unwrap();
    std::fs::remove_file(&token).unwrap();
    assert!(!materialization_ready(&admin).unwrap());
    assert!(repair_due(&admin, &mut repaired));
    assert!(
        !repair_due(&admin, &mut repaired),
        "once per version, whatever the files hold"
    );
    sync(&admin, None).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    assert!(!repair_due(&admin, &mut repaired), "all as written");
    // Within the same version, a file that changes after the repair is put back again,
    // once: the same set changing again is not repaired every pass.
    std::fs::remove_file(&token).unwrap();
    assert!(repair_due(&admin, &mut repaired), "another set of files");
    assert!(!repair_due(&admin, &mut repaired), "the same set once");
    sync(&admin, None).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    assert_eq!(std::fs::read(&token).unwrap(), b"inert-token");
    assert_eq!(
        std::fs::read(&config).unwrap(),
        b"[tool]\nhelper = local\n",
        "the rewritten if-absent file is the machine's own"
    );

    let first = load_state(&admin).unwrap();
    let other = root.path().join("other.vault");
    let admin = Paths::for_home(&root.path().join("admin-other"));
    let token = materialize::target_path(&admin.home, "token").unwrap();
    create(&admin, other.to_str().unwrap(), None, Some("admin")).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-other")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    let second = load_state(&admin).unwrap();
    assert_eq!(first.seq, 2);
    assert_eq!(second.seq, first.seq);
    assert_ne!(second.source_identity, first.source_identity);
    assert!(materialization_ready(&admin).unwrap());
    assert!(!repair_due(&admin, &mut repaired));
    std::fs::remove_file(&token).unwrap();
    assert!(repair_due(&admin, &mut repaired));
    assert!(
        !repair_due(&admin, &mut repaired),
        "once for this source too"
    );
    sync(&admin, None).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    assert_eq!(std::fs::read(&token).unwrap(), b"inert-other");

    std::fs::remove_file(&token).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "note", b"inert-note")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    std::fs::remove_file(&token).unwrap();
    assert!(repair_due(&admin, &mut repaired));

    let store = root.path().join("store");
    let socket = bound();
    let server = serve(&store, &socket);
    let url = format!("http://{}/v1/served", server.addr);
    let admin = Paths::for_home(&root.path().join("admin-served"));
    let token = materialize::target_path(&admin.home, "token").unwrap();
    create(&admin, &url, None, Some("admin")).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-served")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    assert!(materialization_ready(&admin).unwrap());
    std::fs::remove_file(&token).unwrap();
    // With no admissible local cache, a failed fetch must leave the repair
    // due for when the source returns. An intact cache repairs offline.
    fsx::write_atomic(&admin.cache_file(), b"damaged cache", 0o600).unwrap();
    server.stop();
    let gone = Unserved::at(&socket);
    assert!(subscriber_pass(&admin, &mut repaired, &FOUND_NONE).is_err());
    gone.end();
    let server = serve(&store, &socket);
    let (_, outcome) =
        subscriber_pass(&admin, &mut repaired, &FOUND_NONE).expect("the server is back");
    assert!(
        matches!(outcome, SyncOutcome::Applied { .. }),
        "{outcome:?}"
    );
    assert!(materialization_ready(&admin).unwrap());
    assert_eq!(std::fs::read(&token).unwrap(), b"inert-served");
    server.stop();
}

#[test]
fn materialization_capture_holds_local_reads_and_fences_the_version() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.secrets");
    fsx::write_atomic(&source, b"token=inert-first-token\n", 0o600).unwrap();
    let paths = Paths::for_home(&root.path().join("reader"));
    join_pinned(&paths, source.to_str().unwrap(), "reader");
    let configured = config(&paths).unwrap().unwrap();
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.lock_file())
        .unwrap();
    let try_writer = || {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test observes the materialization reader lease without blocking"
        )]
        writer.try_lock()
    };
    let captured = with_materialization_ready(&paths, &configured, || {
        assert!(matches!(
            try_writer(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        Ok(fsx::read_private(&paths.home.join(".secrets/token"), 1024)?.unwrap())
    })
    .unwrap()
    .unwrap();
    assert_eq!(captured.inputs.as_slice(), b"inert-first-token");
    assert!(captured.snapshot.is_current(&paths).unwrap());
    try_writer().unwrap();
    writer.unlock().unwrap();

    let mut state = load_state(&paths).unwrap();
    state.heard += 1;
    state.requested += 1;
    state.proof = Some("inert-proof".into());
    save_state(&paths, &state).unwrap();
    assert!(
        captured.snapshot.is_current(&paths).unwrap(),
        "not a new version"
    );

    fsx::write_atomic(&source, b"token=inert-next-token\n", 0o600).unwrap();
    sync(&paths, None).unwrap();
    assert!(materialization_ready(&paths).unwrap());
    assert!(!captured.snapshot.is_current(&paths).unwrap());
    let current = config(&paths).unwrap().unwrap();
    let mut mismatch = current.clone();
    mismatch.name = "another".into();
    assert!(
        with_materialization_ready::<()>(&paths, &mismatch, || {
            panic!("another source must not read inputs")
        })
        .unwrap()
        .is_none()
    );
    let writer = StateLock::take(&paths).unwrap();
    let busy = with_current_wait(Duration::from_millis(20), || {
        with_materialization_ready::<()>(&paths, &current, || {
            panic!("an active sync must not read inputs")
        })
    });
    assert_eq!(
        busy.err().map(|error| error.kind()),
        Some(crate::ErrorKind::VaultBusy)
    );
    drop(writer);
    assert!(
        with_materialization_ready(&paths, &current, || {
            Err::<(), _>(Error::new("injected local read failure"))
        })
        .is_err()
    );
    assert!(
        StateLock::observe(&paths).unwrap().is_some(),
        "failure releases the lease"
    );
}

#[test]
fn a_version_that_writes_no_file_leaves_a_snapshot_current() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let publish = |change: &dyn Fn(&mut Document)| {
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        change(&mut doc);
        admin_publish(&admin, &opened, &doc).unwrap();
        sync(&admin, None).unwrap();
    };
    publish(&|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .unwrap();
    });
    let configured = config(&admin).unwrap().unwrap();
    let captured = with_materialization_ready(&admin, &configured, || Ok(()))
        .unwrap()
        .unwrap();
    // A machine let in (what every open-vault join publishes): a new version, no file.
    publish(&|doc| {
        doc.add_machine("m2", &transport::test_recipient(2))
            .unwrap();
    });
    assert!(load_state(&admin).unwrap().seq > captured.snapshot.seq);
    assert!(
        captured.snapshot.is_current(&admin).unwrap(),
        "no file it read changed"
    );
    // CONTROL: a version that changes a file it wrote is not current.
    publish(&|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"inert-next")
            .unwrap();
    });
    assert!(!captured.snapshot.is_current(&admin).unwrap());
}

#[test]
fn materialization_marks_are_local_redacted_and_unknown_while_a_writer_runs() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&scratch.path().join("home"));
    assert!(materialization_mark(&paths).unwrap().is_none());
    assert!(
        !paths.home.exists(),
        "an observation creates no local state"
    );
    let source = scratch.path().join("scratch.secrets");
    fsx::write_atomic(&source, b"token=inert-mark-test-value\n", 0o600).unwrap();
    join_pinned(&paths, source.to_str().unwrap(), "scratch");
    let known = materialization_mark(&paths).unwrap().unwrap();
    assert_eq!(
        MaterializationMark::from_cache_key(&known.cache_key()),
        Some(known)
    );
    let debug = format!("{known:?}");
    assert!(!debug.contains(&known.cache_key()));
    assert!(!debug.contains("inert-mark-test-value"));
    let mut heartbeat = load_state(&paths).unwrap();
    heartbeat.heard += 1;
    heartbeat.requested += 1;
    heartbeat.proof = Some("inert-join-proof".into());
    save_state(&paths, &heartbeat).unwrap();
    let state = std::fs::read(paths.state_file()).unwrap();
    assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
    {
        let _writer = StateLock::take(&paths).unwrap();
        assert!(materialization_mark(&paths).unwrap().is_none());
    }
    assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), state);

    fsx::write_atomic(&paths.state_file(), b"invalid = [", 0o600).unwrap();
    assert!(materialization_mark(&paths).is_err());
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), b"invalid = [");
    fsx::write_atomic(&paths.state_file(), &state, 0o600).unwrap();
    assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
}

#[test]
fn materialization_readiness_tracks_failed_resumed_and_missing_files() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.secrets");
    fsx::write_atomic(
        &source,
        b"token=inert-token\n~/.config/gh/hosts.yml=inert-value\n",
        0o600,
    )
    .expect("plain vault");
    let reader = Paths::for_home(&root.path().join("reader"));
    assert!(!materialization_ready(&reader).expect("unconfigured"));
    assert!(!reader.dir.exists(), "a readiness check creates no state");
    let blocked = materialize::target_path(&reader.home, "~/.config/gh/hosts.yml").expect("target");
    std::fs::create_dir_all(&blocked).expect("directory blocks the file");
    assert!(matches!(
        join_pinned(&reader, source.to_str().expect("utf-8"), "reader"),
        JoinOutcome::Plain { .. }
    ));
    assert_eq!(
        status(&reader).expect("status").membership,
        Membership::Plain
    );
    assert!(load_state(&reader).expect("state").failed);
    assert!(!materialization_ready(&reader).expect("partial sync"));

    std::fs::remove_dir(&blocked).expect("remove only the test's empty directory");
    sync(&reader, None).expect("retry same source");
    assert!(materialization_ready(&reader).expect("complete sync"));

    let token = materialize::target_path(&reader.home, "token").expect("token path");
    std::fs::remove_file(&token).expect("remove test token");
    let state_before = std::fs::read(reader.state_file()).expect("state before check");
    assert!(!materialization_ready(&reader).expect("missing file"));
    assert!(!token.exists(), "readiness must not repair the file");
    assert_eq!(std::fs::read(reader.state_file()).unwrap(), state_before);
    sync(&reader, None).expect("repair missing file");
    assert!(materialization_ready(&reader).expect("repaired"));

    fsx::write_atomic(&token, b"inert-local-edit", 0o600).expect("edit test token");
    assert!(!materialization_ready(&reader).expect("changed file"));
}

fn approve(admin: &Paths) {
    let opened = admin_open(admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    for request in requests(admin, Some(&doc)).expect("requests") {
        let host = free_name(&doc, &request.host);
        doc.add_machine(&host, &request.recipient).expect("add");
    }
    admin_publish(admin, &opened, &doc).expect("publish");
}

fn rotate(admin: &Paths) -> Published {
    let opened = admin_open(admin).expect("admin opens");
    admin_rotate(admin, &opened, &opened.doc.clone()).expect("rotate")
}

fn pinned(paths: &Paths) -> Config {
    config(paths).expect("config").expect("configured")
}

#[test]
fn a_concurrent_machine_key_wins_without_replacing_any_existing_bytes() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).expect("state directory");
    let first = Identity::from_secret_bytes([7; 32]);
    let expected = first.recipient();
    let first = install_identity(&paths, first, None).expect("first key wins");
    assert_eq!(first.recipient(), expected);
    let held = fsx::read_private(&paths.identity_file(), 16 << 10)
        .expect("read")
        .expect("key");
    let later = install_identity(&paths, Identity::from_secret_bytes([9; 32]), None)
        .expect("return the winning key");
    assert_eq!(later.recipient(), expected);
    assert_eq!(
        fsx::read_private(&paths.identity_file(), 16 << 10)
            .expect("read")
            .expect("key"),
        held,
        "a creator never replaces the first private key"
    );
    assert_eq!(
        machine_identity(&paths).expect("existing key").recipient(),
        expected
    );

    fsx::write_atomic(&paths.identity_file(), b"unrecognized existing work", 0o600)
        .expect("owned sentinel");
    assert!(install_identity(&paths, Identity::from_secret_bytes([11; 32]), None).is_err());
    assert_eq!(
        fsx::read_private(&paths.identity_file(), 16 << 10)
            .expect("read")
            .expect("sentinel")
            .as_slice(),
        b"unrecognized existing work"
    );
}

/// A12: an identity this test binary makes is a lab identity, its mark written into its file
/// with the key. Every build reads the key past the mark (the last line that is no comment),
/// the mark reads back, and the Commons device opened from the file carries it.
#[test]
fn a_new_identity_here_is_marked_in_its_file_and_its_key_reads_as_before() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    let identity = machine_identity(&paths).expect("made");
    let file = fsx::read_private(&paths.identity_file(), 16 << 10)
        .expect("read")
        .expect("the file");
    let text = file.as_str().expect("UTF-8");
    assert!(
        text.lines().any(|line| line
            == "# aterm lab identity: test-binary (it founds its own lab and never joins the \
                live ALab)"),
        "the mark is in the file"
    );
    let key = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .expect("a key line");
    assert_eq!(
        Identity::parse(key).expect("a key").recipient(),
        identity.recipient(),
        "a build before the mark reads the same key"
    );
    assert_eq!(
        lab_mark(&paths).expect("read"),
        Some(crate::LabMark::TestBinary)
    );
    let device = crate::commons::bootstrap::Device::open(&paths)
        .expect("device")
        .expect("an identity");
    assert_eq!(device.lab(), Some(crate::LabMark::TestBinary));
}

/// A12: an identity a release build made on a Mac's own hardware stays unmarked under this
/// test binary, so a developer's own Mac keeps its seat; one a build inside a virtual machine
/// made is marked, whatever binary reads it. Making it again keeps the first file and its mark.
#[test]
fn a_release_identity_stays_unmarked_here_and_a_virtual_machines_is_marked() {
    let in_vm = crate::Making {
        virtual_machine: true,
        ..crate::Making::default()
    };
    for (making, mark) in [
        (crate::Making::default(), None),
        (in_vm, Some(crate::LabMark::VirtualMachine)),
    ] {
        let home = aterm_tempfile::tempdir().expect("owned home");
        let paths = Paths::for_home(home.path());
        let made = machine_identity_as(&paths, making).expect("made");
        assert_eq!(lab_mark(&paths).expect("read"), mark);
        assert_eq!(refuse_lab_identity(&paths).is_ok(), mark.is_none());
        let again = machine_identity(&paths).expect("kept");
        assert_eq!(again.recipient(), made.recipient());
        assert_eq!(lab_mark(&paths).expect("read"), mark, "{making:?}");
    }
}

/// A12: `aterm secrets join` with no URL, or the default one, from a lab identity is refused
/// before anything is dialed and configures nothing; a fresh Mac in a build that makes lab
/// identities is refused before its key is made. The control: a URL a person typed is dialed
/// (the loopback listener there hears it).
#[test]
fn a_lab_identitys_join_of_the_live_alab_is_refused_before_it_dials() {
    let options = |source: &str| JoinOptions {
        source: source.to_owned(),
        host: Some("m9".to_owned()),
        interactive: false,
        wait: Duration::ZERO,
        confirm: &|_| false,
        password: JoinPassword::Given(None),
        fetched: None,
        requested: &|| {},
        say: &|_| {},
    };
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    let in_vm = crate::Making {
        virtual_machine: true,
        ..crate::Making::default()
    };
    machine_identity_as(&paths, in_vm).expect("made in a virtual machine");
    for source in ["", crate::ALAB_DEFAULT_URL] {
        let refused = join(&paths, &options(source)).expect_err("refused");
        assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
        assert_eq!(
            refused.to_string(),
            format!(
                "this Mac's vault identity is a lab identity (virtual-machine): \
                 {LAB_IDENTITY_REFUSAL}"
            )
        );
        // The bounded join a writer's handover runs (`aterm-db`'s generation) refuses as
        // well.
        let bounded = join_if_unchanged_until(
            &paths,
            &options(source),
            None,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect_err("refused");
        assert_eq!(bounded.kind(), crate::ErrorKind::LabIdentity, "{bounded}");
        assert!(
            config(&paths).expect("read").is_none(),
            "nothing configured"
        );
    }

    let fresh = aterm_tempfile::tempdir().expect("owned home");
    let fresh = Paths::for_home(fresh.path());
    let refused = join(&fresh, &options("")).expect_err("refused");
    assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
    assert!(
        refused
            .to_string()
            .starts_with("this build makes lab identities (test-binary)"),
        "{refused}"
    );
    assert!(
        read_identity(&fresh).expect("read").is_none(),
        "no key made"
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("port").port();
    let (heard, hears) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = heard.send(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .map(drop)
            .is_ok(),
        );
    });
    let typed = join(&paths, &options(&format!("http://127.0.0.1:{port}/v1/lab")));
    assert!(
        typed.is_err_and(|error| error.kind() != crate::ErrorKind::LabIdentity),
        "a typed URL is not refused as the live ALab"
    );
    assert_eq!(
        hears.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(true),
        "the typed URL was dialed"
    );
}

/// A12: a browse for the members on this network is refused to a lab identity where it would
/// find ALab's (`_alab._tcp`), and to a value that names no lab service; a browse of a lab's
/// own service goes, and so does a release identity's browse.
#[test]
fn a_member_browse_is_refused_to_a_lab_identity_only_where_it_finds_alab() {
    let root = aterm_tempfile::tempdir().expect("owned home");
    let lab = Paths::for_home(&root.path().join("lab"));
    machine_identity(&lab).expect("made");
    let release = Paths::for_home(&root.path().join("release"));
    machine_identity_as(&release, crate::Making::default()).expect("made");
    let browse = |paths: &Paths, service: Option<&str>| {
        refuse_lab_member_browse_as(
            paths,
            service.map(std::ffi::OsStr::new),
            crate::Making::now(),
        )
    };
    assert_eq!(
        browse(&lab, None).expect_err("ALab's members").kind(),
        crate::ErrorKind::LabIdentity
    );
    assert!(browse(&lab, Some("_alab._tcp")).is_err());
    browse(&lab, Some("_alab-lab-a12x._tcp")).expect("a lab's own members");
    browse(&release, None).expect("a release identity browses ALab's");
}

/// A12: a proof over the fleet names no vault: a lab identity's is refused where this Mac
/// follows ALab's vault, or none yet, and goes where it follows a lab's; a release identity's
/// goes either way.
#[test]
fn a_lab_identitys_proof_over_the_fleet_goes_only_to_a_lab_it_follows() {
    let root = aterm_tempfile::tempdir().expect("owned home");
    let lab = Paths::for_home(&root.path().join("lab"));
    machine_identity(&lab).expect("made");
    let follows = |signer: [u8; 32]| Config {
        source: Source::Url("http://127.0.0.1:9/v1/lab".to_owned()),
        mirrors: Vec::new(),
        name: "lab".to_owned(),
        signer: Some(signer),
        followed: Vec::new(),
        host: "m9".to_owned(),
    };
    assert!(refuse_lab_identity_for_fleet(&lab).is_err(), "no vault yet");
    let alab = decode_key32(crate::ALAB_SIGNER).expect("the ALab key");
    save_config(&lab, &follows(alab)).expect("configured");
    assert!(refuse_lab_identity_for_fleet(&lab).is_err(), "ALab's vault");
    let own = envelope::signer_public(&[5; 32]).expect("a lab's key");
    save_config(&lab, &follows(own)).expect("configured");
    refuse_lab_identity_for_fleet(&lab).expect("its own lab's members");

    let release = Paths::for_home(&root.path().join("release"));
    machine_identity_as(&release, crate::Making::default()).expect("made");
    refuse_lab_identity_for_fleet(&release).expect("a release identity proves to ALab");
}

/// A12: an identity a release build made is no lab identity under a debug build or a test, so
/// a developer's own Mac keeps its seat; in a virtual machine it is one all the same (a
/// legacy-upgrade guest's identity predates the mark), and the refusal says why.
///
/// NEGATIVE CONTROL: `lab_standing` reading an unmarked held identity as `None` whatever
/// `making` is: the VM refusal fails.
#[test]
fn an_unmarked_identity_is_a_lab_identity_in_a_virtual_machine_and_under_no_debug_build() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    machine_identity_as(&paths, crate::Making::default()).expect("made");
    let debug = crate::Making {
        test_binary: true,
        debug_build: true,
        lab_lane: true,
        virtual_machine: false,
        foreign_home: true,
    };
    refuse_lab_identity_as(&paths, debug).expect("a release identity keeps its seat");
    let in_vm = crate::Making {
        virtual_machine: true,
        ..crate::Making::default()
    };
    let refused = refuse_lab_identity_as(&paths, in_vm).expect_err("refused in a VM");
    assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
    assert_eq!(
        refused.to_string(),
        format!(
            "this Mac runs in a virtual machine, so its vault identity is a lab identity \
             (virtual-machine): {LAB_IDENTITY_REFUSAL}"
        )
    );
    assert!(refuse_lab_member_browse_as(&paths, None, in_vm).is_err());
}

/// A12: either lab knob set, whatever its value, marks the process a lab lane's, so the
/// identity it makes is a lab identity and a live-vault join is refused; neither set marks
/// nothing.
///
/// NEGATIVE CONTROL: `lab_lane_of` reading `LAB_MEMBER_SERVICE_ENV` alone fails at the vault
/// knob.
#[test]
fn either_lab_knob_marks_the_process_a_lab_lanes() {
    for knob in [
        crate::discover::LAB_MEMBER_SERVICE_ENV,
        crate::discover::LAB_VAULT_SERVICE_ENV,
    ] {
        assert!(crate::Making::lab_lane_of(|set| set == knob), "{knob}");
    }
    assert!(!crate::Making::lab_lane_of(|_| false));
}

/// A12: a release build on a Mac's own hardware that holds an identity a debug build or a
/// test made is refused as any build is, and told how the Mac rejoins with it: a new identity
/// once the vault folder is set aside (A16's start does that to this user's own folder). A
/// build that makes lab identities is told nothing of the kind.
///
/// NEGATIVE CONTROL: `refused_as_lab` leaving `rejoin` empty: the release build's refusal names
/// no way back.
#[test]
fn a_release_build_holding_a_debug_made_identity_is_told_how_the_mac_rejoins() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    machine_identity(&paths).expect("made by this test");
    let release = refuse_lab_identity_as(&paths, crate::Making::default())
        .expect_err("a held lab identity is refused under a release build too");
    assert_eq!(
        release.to_string(),
        format!(
            "this Mac's vault identity is a lab identity (test-binary): {LAB_IDENTITY_REFUSAL}; \
             this build makes no lab identity, so with this Mac's vault folder ({}) set \
             aside (a release app's start does that to its own) it makes a new one that can \
             join ALab",
            paths.dir.display()
        )
    );
    let debug = crate::Making {
        debug_build: true,
        ..crate::Making::default()
    };
    let lab = refuse_lab_identity_as(&paths, debug).expect_err("refused");
    assert_eq!(
        lab.to_string(),
        format!(
            "this Mac's vault identity is a lab identity (test-binary): {LAB_IDENTITY_REFUSAL}"
        )
    );
}

/// A12: the virtual-machine probe decides `Making::now`'s fact outside a test binary, and a
/// guest's unmarked identity is then a lab identity, refused; a probe that reads 0 or does not
/// answer is hardware, and a test binary reads no probe at all, so no test hangs on its host.
///
/// NEGATIVE CONTROL: `Making::reading` ignoring `virtual_machine`: the guest's identity is
/// not refused.
#[test]
fn the_virtual_machine_probe_marks_a_guest_outside_a_test_binary_only() {
    let guest = crate::Making::reading(false, || Some(true), || None);
    assert!(guest.virtual_machine, "{guest:?}");
    for hardware in [
        crate::Making::reading(false, || Some(false), || None),
        crate::Making::reading(false, || None, || None),
        crate::Making::reading(true, || Some(true), || None),
    ] {
        assert!(!hardware.virtual_machine, "{hardware:?}");
    }
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    machine_identity_as(&paths, crate::Making::default()).expect("made by a release build");
    let refused = refuse_lab_identity_as(&paths, guest).expect_err("a guest");
    assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
    refuse_lab_identity_as(&paths, crate::Making::reading(false, || None, || None))
        .expect("hardware keeps its seat");
}

/// A release app handed a scratch `HOME` on a Mac's own hardware makes a lab identity: its vault
/// key, fleet node key and node id are all new there, so a member on the home LAN would seat it
/// as one more Mac under this Mac's name (m11 held 25 seats on 2026-11-12). The mark is the
/// file's, so the identity is refused whatever process reads it later, and a process in a
/// foreign home that holds no identity yet is refused before it makes one. An identity made in
/// the login's own home stays unmarked, and a process in a foreign home that reads it (a root
/// shell that kept the person's `HOME`) keeps its seat.
///
/// NEGATIVE CONTROL: `lab_identity` not reading `foreign_home`: the process that holds no key
/// yet is not refused (`before a key is made`).
#[test]
fn an_identity_made_in_a_foreign_home_is_a_lab_identity() {
    let foreign = crate::Making {
        foreign_home: true,
        ..crate::Making::default()
    };
    let scratch = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(scratch.path());
    let refused = refuse_lab_identity_as(&paths, foreign).expect_err("before a key is made");
    assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
    assert!(
        refused
            .to_string()
            .starts_with("this process's HOME is not its login's own home"),
        "{refused}"
    );
    machine_identity_as(&paths, foreign).expect("made");
    let mark = |paths: &Paths, making| {
        lab_standing(paths, making)
            .expect("read")
            .map(|(mark, _)| mark)
    };
    assert_eq!(
        mark(&paths, crate::Making::default()),
        Some(crate::LabMark::ForeignHome)
    );
    let refused = refuse_lab_identity_as(&paths, crate::Making::default())
        .expect_err("refused whatever process reads it");
    assert_eq!(
        refused.to_string(),
        format!(
            "this Mac's vault identity is a lab identity (foreign-home): {LAB_IDENTITY_REFUSAL}; \
             this build makes no lab identity, so with this Mac's vault folder ({}) set aside (a \
             release app's start does that to its own) it makes a new one that can join ALab",
            paths.dir.display()
        )
    );
    assert!(refuse_lab_member_browse_as(&paths, None, foreign).is_err());

    let own = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(own.path());
    machine_identity_as(&paths, crate::Making::default()).expect("made");
    assert_eq!(mark(&paths, foreign), None);
    refuse_lab_identity_as(&paths, foreign).expect("an identity of the login's own home");
}

/// The home probe decides `Making::now`'s fact outside a test binary: a home that is another
/// folder than the account database's is foreign, the same folder by another spelling is not,
/// and an unresolved home on either side is not, so no Mac makes a lab identity because a probe
/// failed. A test binary reads no probe.
///
/// NEGATIVE CONTROL: `foreign_home_of` comparing the two paths as text: the symlinked home
/// reads foreign (`left: Some(true) right: Some(false)`). `Making::reading` ignoring
/// `foreign_home`: the first `assert!` fails.
#[test]
fn the_foreign_home_probe_reads_folders_not_spellings() {
    let scratch = crate::Making::reading(false, || None, || Some(true));
    assert!(scratch.foreign_home, "{scratch:?}");
    assert_eq!(
        crate::lab_identity(crate::Making {
            foreign_home: scratch.foreign_home,
            ..crate::Making::default()
        }),
        Some(crate::LabMark::ForeignHome)
    );
    for own in [
        crate::Making::reading(false, || None, || Some(false)),
        crate::Making::reading(false, || None, || None),
        crate::Making::reading(true, || None, || Some(true)),
    ] {
        assert!(!own.foreign_home, "{own:?}");
    }

    let root = aterm_tempfile::tempdir().expect("owned root");
    let (account, other) = (root.path().join("account"), root.path().join("other"));
    std::fs::create_dir(&account).expect("account home");
    std::fs::create_dir(&other).expect("another home");
    let of = crate::Making::foreign_home_of;
    assert_eq!(of(Some(&account), Some(&account)), Some(false));
    assert_eq!(of(Some(&other), Some(&account)), Some(true));
    assert_eq!(
        of(Some(&root.path().join("not-made-yet")), Some(&account)),
        Some(true)
    );
    assert_eq!(of(None, Some(&account)), None);
    assert_eq!(of(Some(&other), None), None);
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&account, &link).expect("a symlinked home");
        assert_eq!(of(Some(&link), Some(&account)), Some(false));
    }
}

/// A12: a joiner's lines carry its identity file's mark, or, for an identity an older build
/// made unmarked, the virtual-machine mark in a guest, so a member on the live genesis refuses
/// such a guest whatever members it names (`--at`).
///
/// NEGATIVE CONTROL: `wire_mark` returning the file's mark alone: the guest's lines read
/// unmarked.
#[test]
fn a_guests_unmarked_identity_carries_the_virtual_machine_mark_on_the_wire() {
    let guest = crate::Making {
        virtual_machine: true,
        ..crate::Making::default()
    };
    assert_eq!(wire_mark(None, guest), Some(crate::LabMark::VirtualMachine));
    assert_eq!(wire_mark(None, crate::Making::default()), None);
    assert_eq!(
        wire_mark(Some(crate::LabMark::DebugBuild), guest),
        Some(crate::LabMark::DebugBuild),
        "the file's own mark stands"
    );
}

/// A12: an ask nobody typed (the subscriber's re-ask, the handover's) goes to ALab's vault
/// from no lab identity: refused before anything is posted, and the subscriber passes over it
/// in silence. The same re-ask to another vault goes. Each request carries a proof, which
/// `join_routing::post` sends to the configured URL alone (never a browse's ALab mirrors), so
/// even a broken refusal reaches only this test's loopback listener.
///
/// NEGATIVE CONTROL: drop the guard in `request_join_until` and the pass-over in
/// `ask_again_if_due`: the listener hears the lab identity's re-ask of ALab's vault.
#[test]
fn a_lab_identitys_automatic_reask_of_alabs_vault_is_refused_before_it_posts() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let at = listener.local_addr().expect("address");
    let (heard, hears) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .map(|(stream, _)| stream);
            drop(stream);
            if heard.send(()).is_err() {
                break;
            }
        }
    });
    let vault = |name: &str, signer: [u8; 32]| Config {
        source: Source::Url(format!("http://{at}/v1/{name}")),
        mirrors: Vec::new(),
        name: name.to_owned(),
        signer: Some(signer),
        followed: Vec::new(),
        host: "m9".to_owned(),
    };
    let alab = vault(
        "alab",
        decode_key32(crate::ALAB_SIGNER).expect("the ALab key"),
    );
    assert!(alab_vault(&alab));
    let home = aterm_tempfile::tempdir().expect("owned home");
    let lab = Paths::for_home(home.path());
    machine_identity(&lab).expect("made by this test");
    save_config(&lab, &alab).expect("source.toml");
    let proof = Some(crate::b64::encode_padded(&[1; 32]));
    save_state(
        &lab,
        &State {
            membership: Stored::Pending,
            requested: 0,
            proof: proof.clone(),
            ..State::default()
        },
    )
    .expect("state.toml");

    let refused = request_join(&lab, &alab, proof.clone(), Asker::Again).expect_err("refused");
    assert_eq!(refused.kind(), crate::ErrorKind::LabIdentity, "{refused}");
    let said = std::cell::RefCell::new(Vec::new());
    ask_again_if_due(&lab, &alab, &|line| said.borrow_mut().push(line.to_owned()));
    assert!(said.borrow().is_empty(), "passed over in silence: {said:?}");
    assert!(hears.try_recv().is_err(), "nothing was posted");

    let team = vault("team", [7; 32]);
    save_config(&lab, &team).expect("source.toml");
    let _ = request_join(&lab, &team, proof, Asker::Again);
    hears
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a re-ask of another vault is posted");
}

/// A12: a member whose vault config cannot be read is on the live genesis: a lab identity is
/// refused rather than let in on a failed read. One that follows a lab's vault is not.
///
/// NEGATIVE CONTROL: `on_live_genesis` reading a failed store as `false`: the unreadable
/// config reads as off it.
#[test]
fn a_member_whose_vault_cannot_be_read_is_on_the_live_genesis() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    save_config(
        &paths,
        &Config {
            source: Source::Url("http://127.0.0.1:9/v1/lab".to_owned()),
            mirrors: Vec::new(),
            name: "lab".to_owned(),
            signer: Some(envelope::signer_public(&[5; 32]).expect("a lab's key")),
            followed: Vec::new(),
            host: "m9".to_owned(),
        },
    )
    .expect("configured");
    assert!(!on_live_genesis(&paths), "a lab's vault");
    let set = |mode| {
        std::fs::set_permissions(paths.source_file(), std::fs::Permissions::from_mode(mode))
            .expect("chmod");
    };
    set(0o644);
    let unread = on_live_genesis(&paths);
    set(0o600);
    assert!(config(&paths).is_ok(), "readable again");
    assert!(unread, "a config no read takes is the live ALab's");
}

/// A12: only a store pinned to the ALab genesis this build compiles is the live ALab's. This
/// crate's unit tests compile none, so a pinned scratch store is not, until a test says so.
#[test]
fn only_a_store_pinned_to_the_compiled_genesis_reads_as_live() {
    let pinned = crate::membership_log::Store::with_compiled(Some([4; 32]));
    assert!(!live_genesis_store(&pinned));
    assert!(with_live_genesis(true, || live_genesis_store(&pinned)));
    assert!(!live_genesis_store(&pinned), "the seam ends with its run");
}

#[test]
fn the_alab_vault_is_known_by_its_keys_whatever_url_reaches_it() {
    let key = |n: u8| envelope::signer_public(&[n; 32]).expect("public");
    let b64 = |key: &[u8; 32]| crate::b64::encode_padded(key);
    let chain = vec![(key(1), [0u8; 64]), (key(2), [0u8; 64])];
    let knows = |keys: Vec<[u8; 32]>, current: [u8; 32]| Retired {
        keys,
        current: Some(current),
    };
    for retired in [
        knows(Vec::new(), key(1)),
        knows(vec![key(1)], key(2)),
        knows(vec![key(0)], key(2)),
    ] {
        let release = alab_release(&retired, &chain, &key(3)).expect("the ALab vault");
        assert_eq!(release.signer, b64(&key(3)));
        let mut expected: Vec<String> = retired.keys.iter().map(b64).collect();
        for (link, _) in &chain {
            if !expected.contains(&b64(link)) {
                expected.push(b64(link));
            }
        }
        assert_eq!(release.retired, expected);
    }
    assert_eq!(
        alab_release(&knows(vec![key(5)], key(6)), &chain, &key(3)),
        None
    );
    assert_eq!(alab_release(&Retired::default(), &chain, &key(3)), None);

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = serve(&root.path().join("store"), &bound());
    let admin = Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/alab", running.addr);
    create(&admin, &url, None, Some("ma")).expect("create");
    let first = pinned(&admin).signer.expect("pinned");
    let done = rotate(&admin);
    assert!(done.rotated);
    assert_eq!(done.alab_release, None);
    let second = pinned(&admin).signer.expect("pinned");
    let done = retiring(Vec::new(), first, || rotate(&admin));
    let third = pinned(&admin).signer.expect("pinned");
    let release = done.alab_release.expect("the ALab vault, by loopback");
    assert_eq!(release.signer, b64(&third));
    assert_eq!(release.retired, vec![b64(&first), b64(&second)]);
    running.stop();
}

#[test]
fn a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key() {
    std::thread::scope(|scope| {
        for (fork, rejoin, bare) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            scope.spawn(move || {
                let root = aterm_tempfile::tempdir().expect("tempdir");
                let store = root.path().join("store");
                let socket = bound();
                let running = serve(&store, &socket);
                let url = format!("http://{}/v1/alab", running.addr);
                let admin = Paths::for_home(&root.path().join("ma"));
                let created = create(&admin, &url, None, Some("ma")).expect("create");
                let reader = Paths::for_home(&root.path().join("mb"));
                join_pinned(&reader, &format!("{url}#{}", created.signer), "mb");
                approve(&admin);
                assert!(matches!(
                    sync(&reader, None).expect("sync"),
                    SyncOutcome::Applied { seq: 2, .. }
                ));
                let stale = std::fs::read(store.join("alab.vault")).expect("the store's copy");

                let old = admin_open(&admin).expect("open").seed;
                let old_key = envelope::signer_public(&old).expect("public");
                rotate(&admin);
                let new_key = pinned(&admin).signer.expect("pinned");

                running.stop();
                let spoof = serve(&root.path().join("spoof"), &socket);
                let mut signer = old;
                let mut fed = Unsigned {
                    name: "alab".to_string(),
                    seq: 1 << 40,
                    time: 0,
                    handoff: Vec::new(),
                    parents: Vec::new(),
                    lockers: Vec::new(),
                };
                if fork {
                    aterm_uds::rand::fill(&mut signer).expect("random");
                    let key = envelope::signer_public(&signer).expect("public");
                    let endorsement = envelope::endorse(&old, "alab", &key).expect("endorsed");
                    fed.handoff = vec![(old_key, endorsement)];
                }
                let fed = envelope::sign(&fed, &signer).expect("signed");
                transport::publish(&Source::Url(url.clone()), &fed).expect("the spoof takes it");
                assert_eq!(sync(&reader, None).expect("taken"), SyncOutcome::NotIn);
                assert_eq!(
                    pinned(&reader).signer,
                    Some(envelope::signer_public(&signer).expect("public"))
                );
                if bare {
                    let text = std::fs::read_to_string(reader.source_file()).expect("source.toml");
                    let kept: String = text
                        .lines()
                        .filter(|line| !line.contains("followed"))
                        .map(|line| format!("{line}\n"))
                        .collect();
                    assert_ne!(kept, text, "the fork's lineage was written");
                    std::fs::write(reader.source_file(), kept).expect("written");
                }
                spoof.stop();

                let lost = serve(&root.path().join("lost"), &socket);
                transport::publish(&Source::Url(url.clone()), &stale).expect("the stale copy");
                let poll = Some(Duration::from_millis(100));
                assert_eq!(
                    sync(&reader, poll).expect("polled"),
                    SyncOutcome::Unchanged { seq: 1 << 40 }
                );
                assert_eq!(
                    std::fs::read(root.path().join("lost").join("alab.vault")).ok(),
                    Some(stale),
                    "nothing put back"
                );
                lost.stop();

                let running = serve(&store, &socket);
                let error = sync(&reader, None).expect_err("kept off");
                let why = if fork {
                    "endorsed no handoff"
                } else {
                    "rollback"
                };
                assert!(error.0.contains(why), "{error}");
                assert_eq!(
                    sync(&reader, poll).expect("polled"),
                    SyncOutcome::Unchanged { seq: 1 << 40 }
                );
                assert_eq!(load_state(&reader).expect("state").seq, 1 << 40);
                retiring(vec![old_key], new_key, || {
                    if rejoin {
                        assert!(matches!(
                            join_pinned(&reader, &url, "mb"),
                            JoinOutcome::Joined { seq: 3, .. }
                        ));
                    } else {
                        assert!(matches!(
                            sync(&reader, poll).expect("sync"),
                            SyncOutcome::Applied { seq: 3, .. }
                        ));
                    }
                });
                let config = pinned(&reader);
                assert_eq!(config.signer, Some(new_key));
                assert_eq!(config.followed, vec![old_key]);
                running.stop();
            });
        }
    });
}

#[test]
fn an_admin_gives_the_vault_back_to_a_store_the_removed_admin_fed() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let socket = bound();
    let running = serve(&store, &socket);
    let url = format!("http://{}/v1/alab", running.addr);
    let admin = Paths::for_home(&root.path().join("ma"));
    test_create_unbound(&admin, &url, None, Some("ma")).expect("create");
    let stale = root.path().join("stale");
    std::fs::create_dir_all(&stale).expect("dir");
    std::fs::copy(store.join("alab.vault"), stale.join("alab.vault")).expect("copied");
    let old = admin_open(&admin).expect("open").seed;
    let old_key = envelope::signer_public(&old).expect("public");
    rotate(&admin);
    let new_key = pinned(&admin).signer.expect("pinned");
    running.stop();

    let fed = Unsigned {
        name: "alab".to_string(),
        seq: 1 << 40,
        time: 0,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    let fed = envelope::sign(&fed, &old).expect("signed");
    let moved = serve(&stale, &socket);
    transport::publish(&Source::Url(url.clone()), &fed).expect("taken");
    moved.stop();
    let opened = retiring(vec![old_key], new_key, || {
        let moved = serve(&stale, &socket);
        let opened = admin_open(&admin);
        moved.stop();
        opened
    });
    assert_eq!(opened.expect("opened, its copy put back").seq(), 2);
    let cached = std::fs::read(admin.cache_file()).expect("cache");
    assert_eq!(std::fs::read(stale.join("alab.vault")).ok(), Some(cached));
}

#[test]
fn a_pass_cut_short_after_its_writes_leaves_its_files_the_vaults() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let (key, toml, profile) = (
        "~/.aterm/machine.key",
        "~/.aterm/machine.toml",
        "~/.aterm/release-credentials.toml",
    );
    let ring = |value: &[u8]| {
        [(key, toml), (toml, profile), (profile, key)].map(|(name, anchor)| Entry {
            name: name.to_string(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: true,
            anchor: Some(anchor.to_string()),
        })
    };
    let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
    let mut first = State {
        seq: 1,
        ..State::default()
    };
    let report = apply_entries(&paths, &mut first, &ring(b"v1")).expect("v1");
    assert_eq!(report.written.len(), 3);
    drop(first);
    let mut state = load_state(&paths).expect("state");
    assert_eq!(
        state.seq, 0,
        "the version moves on only with the pass's own save"
    );
    let report = apply_entries(&paths, &mut state, &ring(b"v1")).expect("v1 again");
    assert_eq!(report.unchanged.len(), 3, "{report:?}");
    state.seq = 1;
    save_state(&paths, &state).expect("saved");
    let mut cut = load_state(&paths).expect("state");
    cut.seq = 2;
    apply_entries(&paths, &mut cut, &ring(b"v2")).expect("v2");
    drop(cut);
    assert_eq!(load_state(&paths).expect("state").seq, 1);
    let mut next = load_state(&paths).expect("state");
    let report = apply_entries(&paths, &mut next, &ring(b"v3")).expect("v3");
    assert_eq!(report.written.len(), 3, "{report:?}");
    assert!(
        report.left.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );
    for name in [key, toml, profile] {
        assert_eq!(std::fs::read(at(name)).expect("read"), b"v3", "{name}");
        assert_eq!(
            next.manifest.files[name],
            materialize::digest_hex(b"v3"),
            "{name}"
        );
    }
    assert!(next.pending.is_empty());
}

#[test]
fn a_pass_cut_short_leaves_no_entry_it_keeps_away_pending() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let (anchor, gap, changed) = (
        "~/.config/gh/hosts.yml",
        "~/.config/git/config",
        "~/.config/git/credentials",
    );
    let entry = |name: &str, anchor: Option<&str>| Entry {
        name: name.to_string(),
        value: SecretBytes::from(&b"vault-value"[..]),
        audience: Audience::Everyone,
        if_absent: true,
        anchor: anchor.map(str::to_string),
    };
    let entries = [
        entry(anchor, None),
        entry(gap, Some(anchor)),
        entry(changed, Some(anchor)),
    ];
    let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
    for (name, value) in [
        (anchor, &b"the machine's own"[..]),
        (changed, b"vault-value"),
    ] {
        std::fs::create_dir_all(at(name).parent().expect("parent")).expect("dir");
        std::fs::write(at(name), value).expect("written");
    }
    let mut state = State {
        seq: 1,
        ..State::default()
    };
    state.manifest.files.insert(
        changed.to_string(),
        materialize::digest_hex(b"vault-before"),
    );
    save_state(&paths, &state).expect("saved");
    let report = apply_entries(&paths, &mut state, &entries).expect("applied");
    assert!(report.written.is_empty(), "{report:?}");
    drop(state);
    let pending = load_state(&paths).expect("state").pending;
    assert!(pending.is_empty(), "{pending:?}");
    std::fs::write(at(gap), b"vault-value").expect("written");
    for name in [gap, changed] {
        std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o600))
            .expect("private");
    }
    let mut next = load_state(&paths).expect("state");
    let report = apply_entries(&paths, &mut next, &entries).expect("applied again");
    assert!(report.removed.is_empty(), "{report:?}");
    for name in [anchor, gap, changed] {
        assert!(at(name).exists(), "{name} is kept");
    }
}

/// `aterm secrets keep` (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.9, step A14; §10
/// "Release detach"): a kept file survives its entry's `Delete` with its digest unchanged,
/// a vault-written file that was not kept is removed (the control), a name keep cannot
/// take changes nothing, and keeping one file of an anchored set withdraws none of the
/// others while their entries stay.
#[test]
fn keep_detaches_each_file_by_digest_and_a_name_it_cannot_keep_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
    let names = |list: &[&str]| {
        list.iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    };
    // No vault state: nothing is kept, and no vault folder is made.
    let kept = keep(&paths, &names(&["signing"])).expect("kept");
    assert!(matches!(kept.as_slice(), [(_, Kept::Not(_))]), "{kept:?}");
    assert!(!paths.dir.exists(), "keep made {}", paths.dir.display());
    let entry = |name: &str, value: &[u8], anchor: Option<&str>| Entry {
        name: name.to_string(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent: anchor.is_some() || name.ends_with(".key"),
        anchor: anchor.map(str::to_string),
    };
    let (key, profile, signing, changed, missing, token) = (
        "~/.aterm/release-ring/machine.key",
        "~/.aterm/release-ring/release-credentials.toml",
        "signing",
        "notary",
        "roster",
        "token",
    );
    let release = [
        entry(key, b"ring key sentinel", None),
        entry(profile, b"profile sentinel", Some(key)),
        entry(signing, b"developer id sentinel", None),
    ];
    let rest = [
        entry(changed, b"notary sentinel", None),
        entry(missing, b"roster sentinel", None),
        entry(token, b"dev token sentinel", None),
    ];
    let entries: Vec<Entry> = release.iter().chain(&rest).cloned().collect();
    let mut state = State {
        seq: 1,
        ..State::default()
    };
    save_state(&paths, &state).expect("saved");
    let report = apply_entries(&paths, &mut state, &entries).expect("applied");
    assert_eq!(report.written.len(), entries.len(), "{report:?}");
    save_state(&paths, &state).expect("saved");
    std::fs::write(at(changed), b"the person's edit").expect("changed");
    std::fs::set_permissions(at(changed), std::fs::Permissions::from_mode(0o600)).expect("private");
    std::fs::remove_file(at(missing)).expect("removed");
    // Names keep cannot take leave the state byte for byte as it was.
    let saved = std::fs::read(paths.state_file()).expect("state");
    let refused = keep(
        &paths,
        &names(&[changed, missing, "never-written", crate::commons::ENTRY]),
    )
    .expect("looked");
    assert_eq!(refused.len(), 4, "{refused:?}");
    for (name, kept) in &refused {
        let Kept::Not(why) = kept else {
            panic!("{name} was kept");
        };
        assert!(why.contains(name.as_str()), "{why}");
    }
    assert_eq!(std::fs::read(paths.state_file()).expect("state"), saved);
    let digests = |list: &[&str]| {
        list.iter()
            .map(|name| materialize::digest_hex(&std::fs::read(at(name)).expect("read")))
            .collect::<Vec<_>>()
    };
    let before = digests(&[key, signing]);
    // The anchor alone of the key's set, and a name twice.
    let kept = keep(&paths, &names(&[key, signing, key])).expect("kept");
    assert_eq!(
        kept,
        [
            (key.to_string(), Kept::Kept),
            (signing.to_string(), Kept::Kept)
        ]
    );
    let shown = format!("{kept:?} {refused:?}");
    assert!(!shown.contains("sentinel"), "{shown}");
    let again = keep(&paths, &names(&[signing])).expect("looked");
    assert_eq!(again, [(signing.to_string(), Kept::Kept)]);
    // While the entries stay, a pass leaves every file of the set as the vault's.
    let mut state = load_state(&paths).expect("state");
    assert_eq!(
        state.kept,
        BTreeSet::from([key.to_string(), signing.to_string()])
    );
    let report = apply_entries(&paths, &mut state, &entries).expect("still in the vault");
    assert!(report.removed.is_empty(), "{report:?}");
    for name in [key, profile, signing] {
        assert!(
            state.manifest.files.contains_key(name),
            "{name}: {report:?}"
        );
        assert!(at(name).exists(), "{name}: {report:?}");
    }
    save_state(&paths, &state).expect("saved");
    // The `Delete`s: the kept files stay as they were, and the unkept ones go.
    let mut state = load_state(&paths).expect("state");
    let report = apply_entries(&paths, &mut state, &rest).expect("deleted");
    assert!(
        report.removed.iter().any(|name| name == profile),
        "{report:?}"
    );
    assert!(
        !at(profile).exists(),
        "the unkept file of the set is taken back"
    );
    save_state(&paths, &state).expect("saved");
    let mut state = load_state(&paths).expect("state");
    assert!(state.kept.is_empty(), "{:?}", state.kept);
    for name in [key, signing] {
        assert!(
            !state.manifest.files.contains_key(name),
            "{:?}",
            state.manifest
        );
    }
    let report = apply_entries(&paths, &mut state, &[]).expect("deleted");
    assert!(
        report.removed.iter().any(|name| name == token),
        "{report:?}"
    );
    assert!(!at(token).exists(), "the unkept file is taken back");
    assert_eq!(
        digests(&[key, signing]),
        before,
        "the kept files are unchanged"
    );
}

/// A pass cut short after writing a new value leaves the manifest at the old digest and the
/// new one pending: the file holds what the vault wrote all the same, and keep takes it.
#[test]
fn keep_takes_a_file_holding_a_cut_pass_pending_value() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let target = materialize::target_path(root.path(), "signing").expect("valid");
    let mut state = State {
        seq: 1,
        ..State::default()
    };
    state
        .manifest
        .files
        .insert("signing".into(), materialize::digest_hex(b"v1"));
    state
        .pending
        .insert("signing".into(), materialize::digest_hex(b"v2"));
    save_state(&paths, &state).expect("saved");
    std::fs::create_dir_all(target.parent().expect("parent")).expect("dir");
    std::fs::write(&target, b"v2").expect("written");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("private");
    let kept = keep(&paths, &["signing".to_string()]).expect("kept");
    assert_eq!(kept, [("signing".to_string(), Kept::Kept)]);
}

#[test]
fn a_cut_pass_resumes_as_the_vaults_and_writes_only_what_it_pended() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
    let private = |name: &str, value: &[u8]| {
        std::fs::create_dir_all(at(name).parent().expect("parent")).expect("dir");
        std::fs::write(at(name), value).expect("written");
        std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o600))
            .expect("private");
    };
    let entry = |name: &str, value: &[u8], if_absent: bool, anchor: Option<&str>| Entry {
        name: name.to_string(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent,
        anchor: anchor.map(str::to_string),
    };
    let resume = |pending: &[(&str, &[u8])], manifest: &[(&str, &[u8])]| {
        let mut state = State {
            seq: 1,
            ..State::default()
        };
        for (name, value) in pending {
            state
                .pending
                .insert(name.to_string(), materialize::digest_hex(value));
        }
        for (name, value) in manifest {
            state
                .manifest
                .files
                .insert(name.to_string(), materialize::digest_hex(value));
        }
        save_state(&paths, &state).expect("saved");
        load_state(&paths).expect("state")
    };
    let (anchor, gap) = ("~/.config/gh/hosts.yml", "~/.config/git/config");
    private(gap, b"vault-value");
    private(anchor, b"the machine's own");
    let mut state = resume(&[(gap, b"vault-value")], &[]);
    let entries = [
        entry(anchor, b"vault-value", true, None),
        entry(gap, b"vault-value", true, Some(anchor)),
    ];
    let report = apply_entries(&paths, &mut state, &entries).expect("applied");
    assert_eq!(report.removed, vec![gap.to_string()], "{report:?}");
    assert!(!at(gap).exists());
    assert!(at(anchor).exists(), "the machine's own anchor");
    let ring = [
        "~/.aterm/machine.key",
        "~/.aterm/machine.toml",
        "~/.aterm/release-credentials.toml",
    ];
    for name in ring {
        private(name, b"v2");
    }
    let v1: Vec<(&str, &[u8])> = ring.iter().map(|name| (*name, &b"v1"[..])).collect();
    let v2: Vec<(&str, &[u8])> = ring.iter().map(|name| (*name, &b"v2"[..])).collect();
    let mut state = resume(&v2, &v1);
    let report = apply_entries(&paths, &mut state, &[]).expect("applied");
    assert_eq!(report.removed.len(), 3, "{report:?}");
    for name in ring {
        assert!(!at(name).exists(), "{name}");
    }
    assert!(state.manifest.files.is_empty(), "{:?}", state.manifest);
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("dir");
    std::os::unix::fs::symlink(&elsewhere, root.path().join(".aterm/roster")).expect("linked");
    let linked = "~/.aterm/roster/aterm-machines.toml";
    private(linked, b"vault-value");
    let resumed: materialize::Pending =
        [(linked.to_string(), materialize::digest_hex(b"vault-value"))].into();
    let mut ahead = None;
    let (manifest, _) = materialize::apply_resuming(
        root.path(),
        &[entry(linked, b"vault-value", false, None)],
        &materialize::Manifest::default(),
        &resumed,
        now(),
        &mut |held, pending| {
            ahead = Some((held.clone(), pending.clone()));
            Ok(())
        },
    )
    .expect("applied");
    let (resumed_held, pending) = ahead.expect("told ahead");
    assert!(pending.is_empty(), "{pending:?}");
    assert!(resumed_held.files.is_empty(), "{resumed_held:?}");
    assert!(manifest.files.is_empty(), "{manifest:?}");
    private("~/.aterm/apple/devid-netrc.key", b"the machine's own");
    let own_anchor = "~/.aterm/apple/devid-own.key";
    private(own_anchor, b"the machine's own");
    let entries = [
        entry("plain_gap", b"vault-value", false, None),
        entry(
            "~/.aterm/apple/devid-netrc.key",
            b"vault-value",
            false,
            None,
        ),
        entry(
            "~/.aterm/apple/devid-filled.key",
            b"vault-value",
            true,
            None,
        ),
        entry(own_anchor, b"vault-value", true, None),
        entry(
            "~/.aterm/apple/devid-kept.key",
            b"vault-value",
            true,
            Some(own_anchor),
        ),
        entry(linked, b"vault-value", false, None),
    ];
    let mut told = materialize::Pending::new();
    let (_, report) = materialize::apply_resuming(
        root.path(),
        &entries,
        &materialize::Manifest::default(),
        &materialize::Pending::new(),
        now(),
        &mut |_, pending| {
            told = pending.clone();
            Ok(())
        },
    )
    .expect("applied");
    assert_eq!(report.written.len(), 3, "{report:?}");
    for name in &report.written {
        assert!(told.contains_key(name), "{name} written unpended: {told:?}");
    }
    assert!(
        !told.contains_key("~/.aterm/apple/devid-kept.key"),
        "{told:?}"
    );
    assert!(!told.contains_key(linked), "{told:?}");
}

#[test]
fn a_pass_cut_short_is_applied_again_at_the_same_version() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let path = shared.to_str().expect("utf-8");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, path, None, Some("ma")).expect("create");
    let reader = Paths::for_home(&root.path().join("mb"));
    join_pinned(&reader, &format!("{path}#{}", created.signer), "mb");
    let recipient = status(&reader)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    let (token, gap) = ("token", "~/.config/git/config");
    {
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("mb", &recipient).expect("add");
        doc.set_entry(&Audience::Everyone, token, b"vault-token")
            .expect("set");
        doc.set_entry_with(&Audience::Everyone, gap, b"vault-config\n", true, None)
            .expect("set");
        admin_publish(&admin, &opened, &doc).expect("publish");
    }
    let at = materialize::target_path(&reader.home, gap).expect("valid");
    std::fs::create_dir_all(at.parent().expect("parent")).expect("dir");
    std::fs::write(&at, b"the machine's own\n").expect("written");
    let first = sync(&reader, None).expect("sync");
    assert!(matches!(first, SyncOutcome::Applied { .. }), "{first:?}");
    assert_eq!(std::fs::read(&at).expect("kept"), b"the machine's own\n");
    std::fs::remove_file(&at).expect("removed");
    let mut state = load_state(&reader).expect("state");
    state
        .pending
        .insert(gap.to_string(), materialize::digest_hex(b"vault-config\n"));
    save_state(&reader, &state).expect("saved");
    assert!(!materialization_ready(&reader).expect("interrupted write"));
    assert!(matches!(
        sync(&reader, None).expect("sync again"),
        SyncOutcome::Applied { .. }
    ));
    assert_eq!(std::fs::read(&at).expect("filled"), b"vault-config\n");
    assert!(load_state(&reader).expect("state").pending.is_empty());
    assert!(materialization_ready(&reader).expect("resumed write completed"));
}

#[test]
fn a_target_that_changed_since_the_look_is_not_written_unannounced() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let name = "~/.config/gh/hosts.yml";
    let at = materialize::target_path(root.path(), name).expect("valid");
    std::fs::create_dir_all(&at).expect("a directory in the way");
    let entries = [Entry {
        name: name.to_string(),
        value: SecretBytes::from(&b"vault-value"[..]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }];
    let (manifest, report) = materialize::apply_resuming(
        root.path(),
        &entries,
        &materialize::Manifest::default(),
        &materialize::Pending::new(),
        now(),
        &mut |_, pending| {
            assert!(pending.is_empty(), "{pending:?}");
            std::fs::remove_dir(&at).expect("the directory goes");
            Ok(())
        },
    )
    .expect("applied");
    assert!(report.written.is_empty(), "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(!at.exists());
    assert!(manifest.files.is_empty(), "{manifest:?}");
}

#[test]
fn a_membership_with_no_digest_floors_on_its_cache_unapplied() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = Paths::for_home(&root.path().join("ma"));
    create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    let first = std::fs::read(admin.cache_file()).expect("cache");
    approve(&admin);
    let second = std::fs::read(admin.cache_file()).expect("cache");
    rotate(&admin);
    let config = pinned(&admin);
    let mut state = load_state(&admin).expect("state");
    assert!(floor(&admin, &state, &config.name, &config).is_some());
    state.seq = 0;
    state.digest = None;
    let below = floor(&admin, &state, &config.name, &config).expect("a floor below the cache");
    let cached = std::fs::read(admin.cache_file()).expect("cache");
    let judged = |bytes: &[u8]| {
        envelope::verify_for_reader(
            bytes,
            &config.name,
            &config.signer.expect("pin"),
            &config.followed,
            Some(&below),
        )
        .map(|(_, _, verdict)| verdict)
    };
    assert_eq!(judged(&cached).expect("the cache"), Verdict::New);
    for older in [&first, &second] {
        assert!(judged(older).is_err(), "an older version is refused");
    }
    let held = envelope::parse(&cached).expect("parsed");
    assert!(!held.handoff.is_empty(), "the rotation's link");
    let seed = admin_open(&admin).expect("open").seed;
    let signed = |seq: u64, handoff: Vec<envelope::Link>| {
        let unsigned = Unsigned {
            name: "team".to_string(),
            seq,
            time: 0,
            handoff,
            parents: Vec::new(),
            lockers: Vec::new(),
        };
        envelope::sign(&unsigned, &seed).expect("signed")
    };
    assert!(
        judged(&signed(held.seq, held.handoff.clone())).is_err(),
        "another at its seq"
    );
    assert_eq!(
        judged(&signed(held.seq + 1, held.handoff.clone())).expect("next"),
        Verdict::New
    );
    assert!(
        judged(&signed(held.seq + 1, Vec::new())).is_err(),
        "a next one dropping the chain"
    );
    std::fs::write(admin.cache_file(), b"alab-vault/1\ngarbage\n").expect("written");
    assert!(floor(&admin, &state, &config.name, &config).is_none());
    std::fs::write(admin.cache_file(), &cached).expect("written");
    state.membership = Stored::None;
    assert!(floor(&admin, &state, &config.name, &config).is_some());
}

#[test]
fn a_large_manifest_reads_back_and_one_past_the_bound_is_never_written() {
    let home = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(home.path());
    let digest = materialize::digest_hex(b"v");
    let names = |count: usize, len: usize| {
        (0..count)
            .map(|i| (format!("~/{i:0>len$}"), digest.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let mut state = State::default();
    state.manifest.files = names(25_000, 120);
    state.pending = names(25_000, 120);
    save_state(&paths, &state).expect("saved");
    let loaded = load_state(&paths).expect("read back");
    assert_eq!(loaded.manifest, state.manifest);
    assert_eq!(loaded.pending, state.pending);

    let mut past = State::default();
    past.manifest.files = names(MAX_STATE / 1000 + 1, 1000);
    let error = save_state(&paths, &past).expect_err("past the bound");
    assert!(error.0.contains("more entries"), "{error}");
    assert_eq!(
        load_state(&paths).expect("still reads").manifest,
        state.manifest
    );
}

#[test]
fn a_request_for_a_vault_this_machine_left_leaves_its_state_alone() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared");
    std::fs::create_dir_all(&shared).expect("dir");
    let vault = |admin: &str, name: &str| {
        let path = shared.join(format!("{name}.vault"));
        let paths = Paths::for_home(&root.path().join(admin));
        let created =
            create(&paths, path.to_str().expect("utf-8"), None, Some(admin)).expect("create");
        format!("{}#{}", path.display(), created.signer)
    };
    let (first, second) = (vault("ma", "team"), vault("mz", "other"));
    let reader = Paths::for_home(&root.path().join("mb"));
    join_pinned(&reader, &first, "mb");
    let left = pinned(&reader);
    join_pinned(&reader, &second, "mb");
    let held = || {
        let state = load_state(&reader).expect("state");
        (state.seq, state.membership, state.proof, state.requested)
    };
    let before = held();
    request_join(
        &reader,
        &left,
        Some("a-proof-for-the-vault-it-left".into()),
        Asker::Join,
    )
    .expect("asked");
    assert_eq!(held(), before);
    let current = pinned(&reader);
    request_join(&reader, &current, Some("its-proof".into()), Asker::Join).expect("asked");
    assert_eq!(
        load_state(&reader).expect("state").proof.as_deref(),
        Some("its-proof")
    );

    struct Stop(Option<crate::server::Running>);
    impl Drop for Stop {
        fn drop(&mut self) {
            if let Some(server) = self.0.take() {
                server.stop();
            }
        }
    }
    let server = Stop(Some(serve(&root.path().join("store"), &bound())));
    let addr = server.0.as_ref().expect("running").addr;
    let admin = Paths::for_home(&root.path().join("admin"));
    let recipient = machine_identity(&admin).unwrap().recipient().to_bech32();
    let mut sources = Vec::new();
    for (name, value) in [("first", b"inert-first"), ("second", b"inert-other")] {
        let creator = Paths::for_home(&root.path().join(format!("creator-{name}")));
        let url = format!("http://{addr}/v1/{name}");
        let created =
            test_create_with_password(&creator, &url, None, Some("creator")).expect("create");
        let opened = admin_open(&creator).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("admin", &recipient).unwrap();
        doc.add_admin("admin").unwrap();
        if name == "first" {
            doc.set_open(true).expect("open first vault");
        }
        doc.set_entry(&Audience::Everyone, "token", value)
            .expect("set token");
        admin_publish(&creator, &opened, &doc).expect("publish open vault");
        sources.push(format!("{url}#{}", created.signer));
    }
    join_pinned(&admin, &sources[1], "admin");
    let second_config = pinned(&admin);
    let from_a = Paths::for_home(&root.path().join("from-a"));
    let from_b = Paths::for_home(&root.path().join("from-b"));
    join_pinned(&from_a, &sources[0], "from-a");
    join_pinned(&from_b, &sources[1], "from-b");
    join_pinned(&admin, &sources[0], "admin");
    let opened = admin_open(&admin).expect("capture first vault");
    let mut checked = None;
    let (_, first_seq, first_approval) = approval_here(&admin, &mut checked)
        .expect("first policy")
        .expect("first approves");
    assert_eq!(first_approval, Approval::Open);
    {
        let _lock = StateLock::take(&admin).expect("join writer");
        let mut state = load_state(&admin).expect("first state");
        state.seq = 0;
        state.digest = None;
        state.membership = Stored::None;
        prepare_materialization(&admin, &mut state).expect("invalidate first source");
        save_config(&admin, &second_config).expect("publish second source");
    }
    assert!(approval_here(&admin, &mut checked).is_err());
    assert!(
        checked.is_none(),
        "an incomplete cache is not retained policy"
    );
    sync(&admin, None).expect("finish second-source join");
    let current = pinned(&admin);
    let (_, second_seq, second_approval) = approval_here(&admin, &mut checked)
        .expect("second policy")
        .expect("second approves");
    assert_eq!(second_seq, first_seq);
    assert_eq!(second_approval, Approval::Password);

    let token = materialize::target_path(&admin.home, "token").unwrap();
    std::fs::remove_file(&token).expect("remove owned token");
    std::fs::create_dir(&token).expect("block only the owned entry");
    let SyncOutcome::Applied { report, .. } = sync(&admin, None).expect("partial apply") else {
        panic!("the blocked entry must be retried");
    };
    assert!(!report.failed.is_empty());
    let partial = load_state(&admin).expect("failed materialization state");
    assert!(partial.failed && source_bound(&current, &partial));
    assert!(!materialization_ready(&admin).expect("failed files are not ready"));
    let mut cold = None;
    let (_, seq, approval) = approval_here(&admin, &mut cold)
        .expect("verified cache despite failed file")
        .expect("still an admin");
    assert_eq!((seq, approval), (second_seq, Approval::Password));
    std::fs::remove_dir(&token).expect("remove only the empty owned blocker");
    sync(&admin, None).expect("repair materialization");
    let queued = |config: &Config| {
        requests_for(&admin, config, None)
            .expect("queue")
            .into_iter()
            .map(|request| request.host)
            .collect::<Vec<_>>()
    };
    assert_eq!(queued(&opened.config), ["from-a"]);
    assert_eq!(queued(&current), ["from-b"]);
    let mut closed = opened.doc.clone();
    closed.set_open(false).expect("close clone");
    let (_, _, held) = code_only_for(&opened.config, &closed, &opened.doc)
        .expect("hold only the opened vault's waiting keys");
    assert_eq!(
        held.iter()
            .map(|request| request.host.as_str())
            .collect::<Vec<_>>(),
        ["from-a"]
    );
    let body = |source: &Source| match transport::fetch(source, None, Duration::from_secs(30))
        .expect("fetch")
    {
        Fetched::Body(bytes) => bytes,
        other => panic!("expected a vault, got {other:?}"),
    };
    let before_a = body(&opened.config.source);
    let before_b = body(&current.source);
    let state = std::fs::read(admin.state_file()).expect("current state");
    // It refuses, or lets nobody in (an open vault admits only a member's Allow); either way
    // neither vault nor this Mac's state is touched.
    assert!(
        auto_approve_opened(&admin, &opened, &mut false, &mut Default::default())
            .map_or(true, |done| done.approved.is_empty())
    );
    assert_eq!(body(&opened.config.source), before_a);
    assert_eq!(body(&current.source), before_b);
    assert_eq!(std::fs::read(admin.state_file()).unwrap(), state);
    for machine in [&from_a, &from_b] {
        assert_eq!(
            sync(machine, None).expect("not admitted"),
            SyncOutcome::NotIn
        );
        assert!(
            !materialize::target_path(&machine.home, "token")
                .unwrap()
                .exists()
        );
    }

    join_pinned(&admin, &sources[0], "admin");
    // The server it reads from here on is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    // The open vault admits only a member's Allow, so a person approves it by its key.
    let from_a_key = machine_identity(&from_a).unwrap().recipient().to_bech32();
    assert_eq!(
        approve_request(&admin, &from_a_key).expect("approve first"),
        "from-a"
    );
    sync(&from_a, None).expect("first machine syncs");
    assert_eq!(
        std::fs::read(materialize::target_path(&from_a.home, "token").unwrap()).unwrap(),
        b"inert-first"
    );
    assert_eq!(
        sync(&from_b, None).expect("still pending"),
        SyncOutcome::NotIn
    );
    assert!(
        !materialize::target_path(&from_b.home, "token")
            .unwrap()
            .exists()
    );
    assert_eq!(body(&current.source), before_b);

    let fresh = admin_open(&admin).expect("capture admitted first vault");
    join_pinned(&admin, &sources[1], "admin");
    let rested = auto_approve_opened(&admin, &fresh, &mut false, &mut Default::default())
        .expect("no publication needed");
    assert!(rested.approved.is_empty());
    assert_eq!(rested.seq, None);
    assert_eq!(
        rested
            .judged
            .iter()
            .map(|request| request.host.as_str())
            .collect::<Vec<_>>(),
        ["from-a"]
    );
    assert_eq!(body(&current.source), before_b);
}

#[test]
fn a_join_to_another_vault_forgets_the_proof_made_for_the_last() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve(&root.path().join("store"), &bound());
    let vault = |admin: &str, name: &str| {
        let paths = Paths::for_home(&root.path().join(admin));
        let url = format!("http://{}/v1/{name}", server.addr);
        let created = test_create_with_password(&paths, &url, None, Some(admin)).expect("create");
        (paths, format!("{url}#{}", created.signer))
    };
    let (team_admin, team) = vault("ma", "team");
    let (other_admin, other) = vault("mz", "other");
    let password = admin_open(&team_admin)
        .expect("admin opens")
        .doc
        .join_password()
        .expect("create makes a join password");
    let reader = Paths::for_home(&root.path().join("mb"));
    let joined = |source: &str, password: &dyn Fn() -> Option<SecretBytes>| {
        join(
            &reader,
            &JoinOptions {
                source: source.to_string(),
                host: Some("mb".to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Ask(password),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
    };
    let asked = joined(&team, &|| Some(password.clone()));
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let mut state = load_state(&reader).expect("state");
    assert!(state.proof.is_some(), "the request carried a proof");
    state.requested = state.requested.saturating_sub(90 * 24 * 60 * 60);
    save_state(&reader, &state).expect("state saved");

    let seen = std::cell::RefCell::new(None);
    let moved = joined(&other, &|| {
        ask_again_if_due(&reader, &pinned(&reader), &|_| {});
        let held = load_state(&reader).expect("state").proof.is_some();
        let queued = requests(&other_admin, None).expect("requests").len();
        *seen.borrow_mut() = Some((held, queued));
        None
    });
    let (held, queued) = seen.take().expect("asked for the password");
    assert!(!held, "the proof made for the vault it left is gone");
    assert_eq!(
        queued, 0,
        "nothing reached the other vault before its own request"
    );
    assert!(
        matches!(moved, Ok(JoinOutcome::Pending { .. })),
        "{moved:?}"
    );
    server.stop();
}

#[test]
fn a_join_cut_off_before_its_request_is_queued_is_asked_for_by_the_next_pass() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let socket = bound();
    let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
    let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let gone = std::cell::RefCell::new(None);
    // The join applies the version it verified without a second fetch, so a server stopped
    // at the confirm stays stopped through the password prompt, up to the request.
    let stop = || {
        if let Some(running) = server.borrow_mut().take() {
            running.stop();
            *gone.borrow_mut() = Some(Unserved::at(&socket));
        }
    };
    let cut_off = |host: &str, source: String, interactive: bool| {
        let paths = Paths::for_home(&root.path().join(host));
        let joined = join(
            &paths,
            &JoinOptions {
                source,
                host: Some(host.to_string()),
                interactive,
                wait: Duration::ZERO,
                confirm: &|_| {
                    stop();
                    true
                },
                password: JoinPassword::Ask(&|| {
                    stop();
                    None
                }),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        );
        assert!(joined.is_err(), "{host}: {joined:?}");
        gone.borrow_mut().take().expect("stopped").end();
        *server.borrow_mut() = Some(serve(&store, &socket));
        paths
    };
    let at_confirm = cut_off("mb", url.clone(), true);
    let at_request = cut_off("mc", format!("{url}#{}", created.signer), false);
    for (paths, host) in [(&at_confirm, "mb"), (&at_request, "mc")] {
        assert_eq!(
            status(paths).expect("status").membership,
            Membership::Pending
        );
        let (config, _) = subscriber_pass(paths, &mut None, &FOUND_NONE).expect("a pass");
        ask_again_if_due(paths, &config, &|_| {});
        let queued: Vec<String> = requests(&admin, None)
            .expect("requests")
            .into_iter()
            .map(|request| request.host)
            .collect();
        assert!(queued.iter().any(|queued| queued == host), "{queued:?}");
    }
    server.borrow_mut().take().expect("running").stop();
}

#[test]
fn a_reask_begun_on_the_vault_left_never_sends_the_new_ones_proof_there() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let team_server = serve(&root.path().join("team-store"), &bound());
    let team_url = format!("http://{}/v1/team", team_server.addr);
    let other_socket = bound();
    let other_server =
        std::cell::RefCell::new(Some(serve(&root.path().join("other-store"), &other_socket)));
    let other_url = format!(
        "http://{}/v1/other",
        other_socket.local_addr().expect("address")
    );
    let other_admin = Paths::for_home(&root.path().join("mz"));
    let other =
        test_create_with_password(&other_admin, &other_url, None, Some("mz")).expect("create");
    let password = admin_open(&other_admin)
        .expect("admin opens")
        .doc
        .join_password()
        .expect("create makes a join password");
    let mac = Paths::for_home(&root.path().join("ma"));
    create(&mac, &team_url, None, Some("ma")).expect("create");
    let begun = pinned(&mac);
    let gone = std::cell::RefCell::new(None);
    let moved = join(
        &mac,
        &JoinOptions {
            source: format!("{other_url}#{}", other.signer),
            host: Some("ma".to_string()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Ask(&|| {
                other_server.borrow_mut().take().expect("running").stop();
                *gone.borrow_mut() = Some(Unserved::at(&other_socket));
                Some(password.clone())
            }),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    );
    assert!(moved.is_err(), "{moved:?}");
    let state = load_state(&mac).expect("state");
    assert!(
        state.proof.is_some() && state.requested == 0,
        "due at once, with the other vault's proof"
    );
    ask_again_if_due(&mac, &begun, &|_| {});
    let queued = transport::list_joins(&team_url).expect("listed");
    assert!(queued.is_empty(), "{queued:?}");
    gone.borrow_mut().take().expect("stopped").end();
    team_server.stop();
}

#[test]
fn a_reask_asks_under_the_name_source_toml_holds_now() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let socket = bound();
    let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
    let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let mac = Paths::for_home(&root.path().join("mb"));
    let cut_off = |host: &str| {
        let gone = std::cell::RefCell::new(None);
        let joined = join(
            &mac,
            &JoinOptions {
                source: format!("{url}#{}", created.signer),
                host: Some(host.to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Ask(&|| {
                    server.borrow_mut().take().expect("running").stop();
                    *gone.borrow_mut() = Some(Unserved::at(&socket));
                    None
                }),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        );
        assert!(joined.is_err(), "{host}: {joined:?}");
        gone.borrow_mut().take().expect("stopped").end();
        *server.borrow_mut() = Some(serve(&store, &socket));
    };
    cut_off("mb");
    let begun = pinned(&mac);
    cut_off("renamed");
    ask_again_if_due(&mac, &begun, &|_| {});
    let queued: Vec<String> = requests(&admin, None)
        .expect("requests")
        .into_iter()
        .map(|request| request.host)
        .collect();
    assert_eq!(queued, ["renamed"]);
    server.borrow_mut().take().expect("running").stop();
}

/// A join cut off before posting its request is asked for by a running subscriber
/// [`JOIN_GRACE`] after it saved the vault: not sooner, and not an hour on.
#[test]
fn a_join_cut_off_before_its_request_is_asked_for_after_the_grace() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve(&root.path().join("store"), &bound());
    let url = format!("http://{}/v1/team", server.addr);
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let mac = Paths::for_home(&root.path().join("mb"));
    let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        join(
            &mac,
            &JoinOptions {
                source: format!("{url}#{}", created.signer),
                host: Some("mb".to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Ask(&|| panic!("the process ends at the password prompt")),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
    }));
    assert!(cut.is_err(), "the join ends at its prompt");
    let state = load_state(&mac).expect("state");
    assert!(
        state.membership == Stored::Pending && state.requested != 0,
        "waiting, with a request recorded that was never posted"
    );
    let hosts = || -> Vec<String> {
        requests(&admin, None)
            .expect("requests")
            .into_iter()
            .map(|request| request.host)
            .collect()
    };
    let config = pinned(&mac);
    ask_again_if_due(&mac, &config, &|_| {});
    assert!(
        hosts().is_empty(),
        "a pass leaves a join in flight to post its own request"
    );
    let mut state = load_state(&mac).expect("state");
    state.requested = state.requested.saturating_sub(JOIN_GRACE);
    save_state(&mac, &state).expect("state saved");
    ask_again_if_due(&mac, &config, &|_| {});
    assert_eq!(hosts(), ["mb"], "a pass after the grace asks");
    server.stop();
}

#[test]
fn a_join_under_another_name_drops_the_old_names_proof() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let socket = bound();
    let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
    let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = test_create_with_password(&admin, &url, None, Some("ma")).expect("create");
    let password = admin_open(&admin)
        .expect("admin opens")
        .doc
        .join_password()
        .expect("create makes a join password");
    let key = join::Key::derive("team", password.as_slice());
    let mac = Paths::for_home(&root.path().join("mb"));
    let asked = |host: &str, password: &dyn Fn() -> Option<SecretBytes>| {
        join(
            &mac,
            &JoinOptions {
                source: format!("{url}#{}", created.signer),
                host: Some(host.to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Ask(password),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
    };
    let gone = std::cell::RefCell::new(None);
    let first = asked("mb", &|| {
        server.borrow_mut().take().expect("running").stop();
        *gone.borrow_mut() = Some(Unserved::at(&socket));
        Some(password.clone())
    });
    assert!(first.is_err(), "{first:?}");
    gone.borrow_mut().take().expect("stopped").end();
    *server.borrow_mut() = Some(serve(&store, &socket));
    let held = load_state(&mac).expect("state");
    assert!(
        held.proof.is_some() && held.requested == 0,
        "due, with mb's proof"
    );

    let misproved = |queued: &[Request]| -> Vec<String> {
        queued
            .iter()
            .filter(|request| {
                request.proof.as_ref().is_some_and(|proof| {
                    !key.verify("team", &request.recipient, &request.host, proof)
                })
            })
            .map(|request| request.host.clone())
            .collect()
    };
    let seen = std::cell::RefCell::new(None);
    let renamed = asked("renamed", &|| {
        ask_again_if_due(&mac, &pinned(&mac), &|_| {});
        *seen.borrow_mut() = Some((
            load_state(&mac).expect("state").proof,
            requests(&admin, None).expect("requests"),
        ));
        Some(password.clone())
    });
    let (proof, queued) = seen.take().expect("asked for the password");
    assert!(misproved(&queued).is_empty(), "{:?}", misproved(&queued));
    assert!(
        proof.is_none(),
        "no proof made for mb beside the name renamed"
    );
    assert!(
        matches!(renamed, Ok(JoinOutcome::Pending { ref host, .. }) if host == "renamed"),
        "{renamed:?}"
    );
    let queued = requests(&admin, None).expect("requests");
    assert!(misproved(&queued).is_empty(), "{:?}", misproved(&queued));
    let held: Vec<(&str, bool)> = queued
        .iter()
        .map(|request| (request.host.as_str(), request.proof.is_some()))
        .collect();
    assert_eq!(held, [("renamed", true)], "with renamed's proof");
    server.borrow_mut().take().expect("running").stop();
}

#[test]
fn a_reask_never_puts_back_a_proof_a_join_replaced() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let gone = Unserved::at(&socket);
    let mac = Paths::for_home(&root.path().join("mb"));
    let config = Config {
        source: Source::Url(format!(
            "http://{}/v1/team",
            socket.local_addr().expect("address")
        )),
        mirrors: Vec::new(),
        name: "team".to_string(),
        signer: Some([7; 32]),
        followed: Vec::new(),
        host: "mb".to_string(),
    };
    save_config(&mac, &config).expect("source.toml");
    let read = Some(crate::b64::encode_padded(&[1; 32]));
    let typed = Some(crate::b64::encode_padded(&[2; 32]));
    let state = |proof: &Option<String>, requested: u64| State {
        membership: Stored::Pending,
        requested,
        proof: proof.clone(),
        ..State::default()
    };
    let held = || {
        let state = load_state(&mac).expect("state");
        (state.proof, state.requested, state.membership)
    };
    save_state(&mac, &state(&typed, 1_000)).expect("state.toml");
    request_join(&mac, &config, read.clone(), Asker::Again).expect_err("unanswered");
    assert_eq!(
        held(),
        (typed.clone(), 1_000, Stored::Pending),
        "left alone"
    );
    request_join(&mac, &config, typed.clone(), Asker::Again).expect_err("unanswered");
    assert_eq!(held(), (typed.clone(), 0, Stored::Pending));
    request_join(&mac, &config, read.clone(), Asker::Join).expect_err("unanswered");
    assert_eq!(held(), (read, 0, Stored::Pending));
    gone.end();
}

#[test]
fn a_request_the_server_lost_with_its_store_is_asked_again_once_the_vault_is_back() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let server = serve(&root.path().join("store"), &socket);
    let url = format!("http://{}/v1/team", server.addr);
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let mac = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mac,
        &JoinOptions {
            source: format!("{url}#{}", created.signer),
            host: Some("mb".to_string()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    server.stop();
    let gone = Unserved::at(&socket);
    assert!(
        subscriber_pass(&mac, &mut None, &FOUND_NONE).is_err(),
        "the server is gone"
    );
    gone.end();
    let server = serve(&root.path().join("moved"), &socket);
    let (_, restored) = subscriber_pass(&admin, &mut None, &FOUND_NONE).expect("the admin's pass");
    assert!(
        matches!(restored, SyncOutcome::PutBack { served: None, .. }),
        "the admin's subscriber put the vault back: {restored:?}"
    );
    let (config, outcome) = subscriber_pass(&mac, &mut None, &FOUND_NONE).expect("a pass");
    assert!(
        matches!(outcome, SyncOutcome::Unchanged { .. }),
        "{outcome:?}"
    );
    ask_again_if_due(&mac, &config, &|_| {});
    let queued: Vec<String> = requests(&admin, None)
        .expect("requests")
        .into_iter()
        .map(|request| request.host)
        .collect();
    assert_eq!(queued, ["mb"]);
    server.stop();
}

#[test]
fn an_admin_gives_a_path_its_copy_back_over_a_version_it_does_not_verify() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = Paths::for_home(&root.path().join("ma"));
    create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    let old = admin_open(&admin).expect("open").seed;
    rotate(&admin);
    let seq = admin_open(&admin).expect("open").seq();
    let written = |seq: u64, seed: &[u8; 32]| {
        let unsigned = Unsigned {
            name: "team".to_string(),
            seq,
            time: 0,
            handoff: Vec::new(),
            parents: Vec::new(),
            lockers: Vec::new(),
        };
        envelope::sign(&unsigned, seed).expect("signed")
    };
    let mut forged = written(seq, &old);
    let at = forged.len() - 10;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    for (what, bytes) in [
        ("the old key, far ahead", written(1 << 40, &old)),
        ("a signature that does not verify, at its seq", forged),
        ("no envelope at all", b"alab-vault/1\ngarbage\n".to_vec()),
    ] {
        std::fs::write(&shared, &bytes).expect("written");
        let opened = admin_open(&admin).unwrap_or_else(|error| panic!("{what}: {error}"));
        assert_eq!(opened.seq(), seq, "{what}");
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        assert_eq!(std::fs::read(&shared).ok(), Some(cached), "{what}");
    }
    let stranger = envelope::signer_public(&[9u8; 32]).expect("key");
    for seq in [1, 1 << 40] {
        let other = written(seq, &[9u8; 32]);
        std::fs::write(&shared, &other).expect("written");
        let error = admin_open(&admin).expect_err("refused");
        assert!(error.0.contains("move the file aside"), "{error}");
        assert!(
            error.0.contains(&envelope::fingerprint(&stranger)),
            "{error}"
        );
        assert!(
            !error.0.contains(&crate::b64::encode_padded(&stranger)),
            "{error}"
        );
        assert_eq!(std::fs::read(&shared).ok(), Some(other), "seq {seq}");
    }
    let pin = pinned(&admin).signer.expect("pinned");
    let forged_chain = Unsigned {
        name: "team".to_string(),
        seq: 1 << 40,
        time: 0,
        handoff: vec![(pin, [7u8; 64])],
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    std::fs::write(
        &shared,
        envelope::sign_unchecked(&forged_chain, &[9u8; 32]).expect("signed"),
    )
    .expect("written");
    let opened = admin_open(&admin).expect("put back over a forged handoff");
    assert_eq!(opened.seq(), seq);
    let cached = std::fs::read(admin.cache_file()).expect("cache");
    assert_eq!(std::fs::read(&shared).ok(), Some(cached));
    let elsewhere = Unsigned {
        name: "other".to_string(),
        seq: 1,
        time: 0,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    let elsewhere = envelope::sign(&elsewhere, &[9u8; 32]).expect("signed");
    std::fs::write(&shared, &elsewhere).expect("written");
    let error = admin_open(&admin).expect_err("refused");
    assert!(
        error.0.contains("holds the vault other, not team"),
        "{error}"
    );
    assert!(!error.0.contains("secrets join"), "{error}");
    assert_eq!(std::fs::read(&shared).ok(), Some(elsewhere));
    let old_key = envelope::signer_public(&old).expect("public");
    let fresh = [9u8; 32];
    let fork = Unsigned {
        name: "team".to_string(),
        seq: 1 << 40,
        time: 0,
        handoff: vec![(
            old_key,
            envelope::endorse(&old, "team", &envelope::signer_public(&fresh).expect("key"))
                .expect("endorsed"),
        )],
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    std::fs::write(&shared, envelope::sign(&fork, &fresh).expect("signed")).expect("written");
    assert!(admin_open(&admin).is_err(), "a fork no build names");
    let new_key = pinned(&admin).signer.expect("pinned");
    let opened = retiring(vec![old_key], new_key, || admin_open(&admin));
    assert_eq!(opened.expect("put back over the fork").seq(), seq);
    let cached = std::fs::read(admin.cache_file()).expect("cache");
    assert_eq!(std::fs::read(&shared).ok(), Some(cached));
}

#[test]
fn an_admin_puts_back_the_link_a_rotation_by_a_build_before_chains_dropped() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let path = shared.to_str().expect("utf-8");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, path, None, Some("ma")).expect("create");
    let first = format!("{path}#{}", created.signer);
    let reader = Paths::for_home(&root.path().join("mb"));
    join_pinned(&reader, &first, "mb");
    let recipient = status(&reader)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    {
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("mb", &recipient).expect("add");
        admin_publish(&admin, &opened, &doc).expect("publish");
    }
    assert!(!rotate(&admin).first_long_chain);
    assert!(matches!(
        sync(&reader, None).expect("sync"),
        SyncOutcome::Applied { seq: 3, .. }
    ));

    let opened = admin_open(&admin).expect("open");
    let mut seed = [0u8; 32];
    aterm_uds::rand::fill(&mut seed).expect("random");
    let next = envelope::signer_public(&seed).expect("public");
    let name = &opened.config.name;
    let link = (
        envelope::signer_public(&opened.seed).expect("public"),
        envelope::endorse(&opened.seed, name, &next).expect("endorsed"),
    );
    let cut = seal_with_admissions_and_parents(
        name,
        opened.seq + 1,
        &seed,
        &opened.commons_seed,
        opened.commons_bound,
        &opened.doc,
        &[link],
        &opened.opened_heads,
        &admission::Ledger::default()
            .retained(&opened.doc)
            .expect("retained"),
    )
    .expect("sealed");
    // A build before chains wrote a path vault judging only the sequence and the parents,
    // which the cut passes. This build's path judges the chain as a server does and refuses
    // the cut, so it is written here as that build wrote it.
    assert_eq!(
        transport::publish(&opened.config.source, &cut)
            .expect_err("this build's path refuses the cut")
            .kind(),
        crate::ErrorKind::Refused
    );
    fsx::write_atomic(&shared, &cut, 0o644).expect("written as a build before chains wrote it");

    let error = sync(&reader, None).expect_err("refused");
    assert!(
        error.0.contains("drops or changes handoff 1 of 1"),
        "{error}"
    );
    let healed = admin_open(&admin).expect("opened with the link put back");
    assert_eq!(healed.handoff.len(), 2);
    let done = admin_publish(&admin, &healed, &healed.doc.clone()).expect("publish");
    assert!(
        done.first_long_chain,
        "two pairs, which builds before chains cannot read"
    );
    assert!(matches!(
        sync(&reader, None).expect("sync"),
        SyncOutcome::Applied { seq: 5, .. }
    ));
    let late = Paths::for_home(&root.path().join("mc"));
    join_pinned(&late, &first, "mc");
    assert_eq!(pinned(&late).signer, Some(next));
}

#[test]
fn only_a_request_with_a_proof_was_asked_with_a_password() {
    let home = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(home.path());
    assert!(!asked_with_password(&paths), "no state");
    let mut state = State {
        membership: Stored::Pending,
        ..State::default()
    };
    save_state(&paths, &state).expect("saved");
    assert!(!asked_with_password(&paths), "asked by code alone");
    state.proof = Some("proof".to_string());
    save_state(&paths, &state).expect("saved");
    assert!(asked_with_password(&paths));
}

#[test]
fn the_approver_remembers_only_what_is_still_listed() {
    let key = crate::transport::test_recipient(1);
    let request = |n: u32| Request {
        recipient: key.clone(),
        host: "m9".to_string(),
        code: "abcd-efgh-ijkm-npqr".to_string(),
        at: None,
        allowed: None,
        proof: Some(format!("proof-{n}")),
    };
    let waiting = request(0);
    let config = Config {
        source: Source::Url("http://vault.invalid:7484/v1/team".to_string()),
        mirrors: Vec::new(),
        name: "team".to_string(),
        signer: Some([7; 32]),
        followed: Vec::new(),
        host: "ma".to_string(),
    };
    let mut settled = Settled::default();
    for n in 1..=100 {
        let listed = [waiting.clone(), request(n)];
        assert!(
            settled.due(&config, 1, &listed, Approval::Password),
            "proof {n}"
        );
        settled.settle(listed.iter().map(Judged::of).collect());
        assert!(
            !settled.due(&config, 1, &listed, Approval::Password),
            "proof {n}"
        );
    }
    assert_eq!(settled.judged.len(), 2);
    assert!(!settled.due(
        &config,
        1,
        std::slice::from_ref(&waiting),
        Approval::Password
    ));
    assert_eq!(settled.judged.len(), 1);
}

/// On the pinned ALab vault, open, an approver lets in no Mac by its asking alone: a plain
/// ask, the same ask as an aterm 0.148 or 0.149 server listed it with its ` home=` tag (read
/// past, not refused), and a `code-only` key all wait for a person. A Mac on the home ALab
/// LAN is let in by a member when it proves itself (`readmit`), never by the approver.
///
/// NEGATIVE CONTROLS: with `auto_approve_listed` letting an open ALab vault's plain request
/// in as `AdmissionMethod::Open` (the approver's former home-LAN branch with its `home=`
/// check dropped), "a plain ask" fails; with `parse_join_line` refusing a ` home=` token,
/// the older server's line is skipped and "an older server's tagged line" fails.
#[test]
fn the_alab_vaults_approver_lets_in_no_plain_ask() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = bound();
    let server = serve(&root.path().join("store"), &socket);
    let url = format!("http://{}/v1/alab", server.addr);
    let paths = Paths::for_home(&root.path().join("m1"));
    let created = create(&paths, &url, None, Some("m1")).unwrap();
    let newcomer = Paths::for_home(&root.path().join("m2"));
    join_pinned(&newcomer, &format!("{url}#{}", created.signer), "m2");
    let mut admin = admin_open(&paths).unwrap();
    assert_eq!(
        Approval::of(&admin.published_doc),
        Some(Approval::Open),
        "a vault is created open"
    );
    let listed = transport::list_joins(&url).unwrap();
    assert_eq!(listed.len(), 1);
    let tagged = requests_in(vec![format!("{} home=0123456789abcdef", listed[0])], &[]);
    assert_eq!(
        tagged,
        requests_in(listed, &[]),
        "an older server's tagged line"
    );
    let signer = admin.config.signer.unwrap();
    let judge = |admin: &Admin, requests: &[Request]| {
        auto_approve_listed(
            &paths,
            admin,
            requests,
            &mut false,
            &mut BTreeSet::new(),
            (&mut JoinKeys::default(), None),
        )
        .unwrap()
    };
    retiring(Vec::new(), signer, || {
        assert!(alab_vault(&admin.config), "the pinned ALab vault");
        let done = auto_approve(&paths).unwrap();
        assert!(done.approved.is_empty(), "a plain ask");
        assert_eq!(done.asking.len(), 1, "it waits for a person");
        let done = judge(&admin, &tagged);
        assert!(done.approved.is_empty(), "an older server's home= tag");
        assert_eq!(done.asking.len(), 1);
        admin
            .published_doc
            .add_code_only(&tagged[0].recipient)
            .unwrap();
        let done = judge(&admin, &tagged);
        assert!(done.approved.is_empty(), "a code-only key");
        assert_eq!(done.code_only, [tagged[0].recipient.clone()]);
    });
    // CONTROL: the request is there for a person, who approves it by its code.
    assert_eq!(approve_request(&paths, &tagged[0].recipient).unwrap(), "m2");
    server.stop();
}

fn serve_apart(
    store: &Path,
    socket: &std::net::TcpListener,
    pins: &[(String, [u8; 32])],
) -> crate::server::Running {
    crate::server::start_apart(
        socket.try_clone().expect("a copy of the socket"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: pins.to_vec(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("server starts")
}

fn a_vault_with_a_member(
    root: &Path,
    socket: &std::net::TcpListener,
) -> (crate::server::Running, String, Paths, Paths) {
    let server = serve(&root.join("store"), socket);
    let url = format!("http://{}/v1/team", server.addr);
    let admin = Paths::for_home(&root.join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let member = Paths::for_home(&root.join("mb"));
    join_pinned(&member, &format!("{url}#{}", created.signer), "mb");
    let key = status(&member)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &key).expect("add");
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .expect("set");
    admin_publish(&admin, &opened, &doc).expect("publish");
    assert!(matches!(
        sync(&member, None).expect("sync"),
        SyncOutcome::Applied { seq: 2, .. }
    ));
    (server, url, admin, member)
}

fn served_at(source: &Source) -> Option<Vec<u8>> {
    match transport::fetch(source, None, Duration::from_secs(30)) {
        Ok(Fetched::Body(bytes)) => Some(bytes),
        Ok(Fetched::Missing) => None,
        other => panic!("{other:?}"),
    }
}

fn pin_of(paths: &Paths) -> Vec<(String, [u8; 32])> {
    let config = pinned(paths);
    vec![(config.name, config.signer.expect("signed"))]
}

#[test]
fn a_machine_in_the_vault_puts_back_a_vault_its_server_lost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let source = Source::Url(url);
    let pin = pin_of(&admin);
    let lose = |server: crate::server::Running, store: &str| {
        server.stop();
        let empty = serve_apart(&root.path().join(store), &socket, &pin);
        assert!(served_at(&source).is_none(), "the server holds no vault");
        empty
    };
    let copy = std::fs::read(member.cache_file()).expect("the member's copy");

    let server = lose(server, "moved");
    let (config, outcome) = subscriber_pass(&member, &mut None, &FOUND_NONE).expect("a pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 2,
            served: None
        }
    );
    assert!(
        served_at(&source) == Some(copy.clone()),
        "the member's copy is served"
    );
    let mut failing = FIRST_RETRY;
    assert_eq!(
        pause_after(&config.source, &outcome, &mut failing, ANSWERED_AT_ONCE),
        FIRST_RETRY
    );

    let token = materialize::target_path(&member.home, "token").expect("target");
    std::fs::remove_file(&token).expect("the token removed");
    std::fs::create_dir(&token).expect("the entry blocked");
    let SyncOutcome::Applied { report, .. } = sync(&member, None).expect("a partial apply") else {
        panic!("the blocked entry is applied again");
    };
    assert!(!report.failed.is_empty());
    let server = lose(server, "again");
    let (_, outcome) = subscriber_pass(&member, &mut None, &FOUND_NONE).expect("a pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 2,
            served: None
        }
    );
    assert!(served_at(&source) == Some(copy), "the member's copy again");
    server.stop();
}

#[test]
fn a_put_back_the_source_keeps_no_more_backs_off_and_is_logged_once() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let config = Config {
        source: Source::Url("http://127.0.0.1:7484/v1/team".to_string()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: Some([1u8; 32]),
        followed: Vec::new(),
        host: "mb".into(),
    };
    let put_back = SyncOutcome::PutBack {
        seq: 2,
        served: None,
    };
    let (mut failing, mut repeated) = (FIRST_RETRY, String::new());
    let mut pass = |outcome: &SyncOutcome| {
        (
            pause_after(&config.source, outcome, &mut failing, ANSWERED_AT_ONCE).as_secs(),
            pass_line(&paths, &config, outcome, &mut repeated),
        )
    };
    let passes: Vec<(u64, bool)> = (0..8)
        .map(|_| {
            let (pause, line) = pass(&put_back);
            (pause, line.is_some())
        })
        .collect();
    assert_eq!(
        passes,
        [
            (5, true),
            (10, false),
            (20, false),
            (40, false),
            (80, false),
            (160, false),
            (300, false),
            (300, false)
        ]
    );
    assert_eq!(pass(&SyncOutcome::Unchanged { seq: 2 }), (10, None));
    let (pause, line) = pass(&put_back);
    assert_eq!(pause, 5);
    let line = line.expect("logged again");
    assert!(
        line.contains("held no vault named team: put this machine's copy back (seq 2)"),
        "{line}"
    );
    let failed = SyncOutcome::Applied {
        seq: 2,
        report: crate::materialize::Report {
            failed: vec![("token".to_string(), "is a symlink".to_string())],
            ..Default::default()
        },
    };
    assert!(pass(&failed).1.is_some());
    assert_eq!(pass(&failed).1, None);
}

#[test]
fn a_server_that_does_not_pin_the_vault_refuses_its_copy_and_the_pass_errs() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, _, member) = a_vault_with_a_member(root.path(), &socket);
    let source = Source::Url(url);
    server.stop();
    let server = serve_apart(&root.path().join("moved"), &socket, &[]);
    let first = subscriber_pass(&member, &mut None, &FOUND_NONE)
        .expect_err("refused")
        .to_string();
    assert!(
        first.contains("holds no vault named team, and putting this machine's copy back failed")
            && first.contains("403")
            && first.contains(
                "only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>"
            ),
        "{first}"
    );
    assert!(served_at(&source).is_none(), "nothing stored");
    let second = subscriber_pass(&member, &mut None, &FOUND_NONE)
        .expect_err("refused again")
        .to_string();
    assert_eq!(second, first, "one line in the log, then the backoff");
    assert!(served_at(&source).is_none(), "nothing stored");
    server.stop();
}

#[test]
fn a_machine_kept_out_or_without_a_verified_copy_puts_nothing_back() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let source = Source::Url(url.clone());
    let signer = crate::b64::encode_padded(&pinned(&admin).signer.expect("signed"));
    let waiting = Paths::for_home(&root.path().join("mc"));
    assert!(matches!(
        join_pinned(&waiting, &format!("{url}#{signer}"), "mc"),
        JoinOutcome::Pending { .. }
    ));
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "later", b"inert-later")
        .expect("set");
    admin_publish(&admin, &opened, &doc).expect("publish");
    let newer = served_at(&source).expect("seq 3");
    let copy = std::fs::read(member.cache_file()).expect("the member's copy");
    server.stop();
    let server = serve_apart(&root.path().join("moved"), &socket, &pin_of(&admin));
    let missing_here = |paths: &Paths| missing(&pinned(paths)).to_string();

    let error = subscriber_pass(&waiting, &mut None, &FOUND_NONE).expect_err("kept out");
    assert_eq!(error.to_string(), missing_here(&waiting));
    assert!(
        served_at(&source).is_none(),
        "a waiting machine puts nothing back"
    );
    assert_eq!(
        load_state(&waiting).expect("state").requested,
        0,
        "asked again at once"
    );

    let mut forged = copy.clone();
    let at = forged.len() - 10;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    let held = load_state(&member).expect("state").digest;
    let record = |digest: Option<[u8; 32]>| {
        let mut state = load_state(&member).expect("state");
        state.digest = digest;
        save_state(&member, &state).expect("state.toml");
    };
    for (what, cached, recorded) in [
        ("a cache that is not the version held", &newer, held),
        (
            "a cache that does not verify, though state.toml holds it",
            &forged,
            Some(sha256(&forged)),
        ),
    ] {
        record(recorded);
        fsx::write_atomic(&member.cache_file(), cached, 0o600).expect("cache");
        let error = subscriber_pass(&member, &mut None, &FOUND_NONE).expect_err(what);
        assert_eq!(error.to_string(), missing_here(&member), "{what}");
        assert!(served_at(&source).is_none(), "{what}: nothing put back");
    }
    record(held);
    fsx::write_atomic(&member.cache_file(), &copy, 0o600).expect("cache");
    let (_, outcome) = subscriber_pass(&member, &mut None, &FOUND_NONE).expect("a pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 2,
            served: None
        }
    );
    assert!(
        served_at(&source) == Some(copy),
        "the member's copy is served"
    );
    server.stop();
}

#[test]
fn a_machine_in_the_vault_writes_nothing_to_a_path_that_holds_no_vault() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let path = shared.to_str().expect("utf-8");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, path, None, Some("ma")).expect("create");
    let member = Paths::for_home(&root.path().join("mb"));
    join_pinned(&member, &format!("{path}#{}", created.signer), "mb");
    let key = status(&member)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &key).expect("add");
    admin_publish(&admin, &opened, &doc).expect("publish");
    assert!(matches!(
        sync(&member, None).expect("sync"),
        SyncOutcome::Applied { seq: 2, .. }
    ));
    std::fs::remove_file(&shared).expect("the file gone");

    let error = subscriber_pass(&member, &mut None, &FOUND_NONE).expect_err("missing");
    assert_eq!(error.to_string(), missing(&pinned(&member)).to_string());
    assert!(!shared.exists(), "nothing written at the path");
    admin_open(&admin).expect("open");
    assert_eq!(
        std::fs::read(&shared).ok(),
        std::fs::read(admin.cache_file()).ok(),
        "the admin's copy is back"
    );
}

#[test]
fn an_admin_of_a_vault_closed_to_joins_puts_back_a_vault_its_server_lost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let server = serve(&root.path().join("store"), &socket);
    let url = format!("http://{}/v1/team", server.addr);
    let source = Source::Url(url.clone());
    let admin = Paths::for_home(&root.path().join("ma"));
    create(&admin, &url, None, Some("ma")).expect("create");
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_open(false).expect("closed");
    admin_publish(&admin, &opened, &doc).expect("publish");
    assert_eq!(approving_here(&admin).expect("read"), None, "no approver");
    server.stop();
    let server = serve_apart(&root.path().join("moved"), &socket, &pin_of(&admin));
    let (_, outcome) = subscriber_pass(&admin, &mut None, &FOUND_NONE).expect("a pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 2,
            served: None
        }
    );
    let copy = std::fs::read(admin.cache_file()).expect("the admin's copy");
    assert!(
        served_at(&source) == Some(copy),
        "the admin's copy is served"
    );
    server.stop();
}

#[test]
fn a_machine_puts_its_newer_copy_back_over_an_older_one_a_server_took() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let server = serve(&root.path().join("store"), &socket);
    let url = format!("http://{}/v1/team", server.addr);
    let source = Source::Url(url.clone());
    let served = || match transport::fetch(&source, None, Duration::from_secs(30)) {
        Ok(Fetched::Body(bytes)) => bytes,
        other => panic!("{other:?}"),
    };
    let admin = Paths::for_home(&root.path().join("ma"));
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let member = Paths::for_home(&root.path().join("mb"));
    join_pinned(&member, &format!("{url}#{}", created.signer), "mb");
    let key = status(&member)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &key).expect("add");
    admin_publish(&admin, &opened, &doc).expect("publish");
    let stale = served();
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .expect("set");
    admin_publish(&admin, &opened, &doc).expect("publish");
    assert!(matches!(
        sync(&member, None).expect("sync"),
        SyncOutcome::Applied { seq: 3, .. }
    ));
    let newest = std::fs::read(member.cache_file()).expect("the member's copy");

    server.stop();
    let server = serve(&root.path().join("moved"), &socket);
    transport::publish(&source, &stale).expect("the stale copy is taken");

    let mut forged = newest.clone();
    let at = forged.len() - 10;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    for (what, cached) in [
        ("a cache that is not the version held", &stale),
        ("a cache that does not verify", &forged),
    ] {
        fsx::write_atomic(&member.cache_file(), cached, 0o600).expect("cache");
        assert!(
            subscriber_pass(&member, &mut None, &FOUND_NONE).is_err(),
            "{what}: a missing valid local copy cannot make the older served version healthy"
        );
        assert_eq!(
            load_state(&member).unwrap().seq,
            3,
            "the rollback floor remains intact"
        );
        assert_eq!(std::fs::read(member.cache_file()).unwrap(), *cached);
        assert!(served() == stale, "{what}: nothing published");
    }
    fsx::write_atomic(&member.cache_file(), &newest, 0o600).expect("cache");
    let exact_socket = bound();
    let exact = serve(&root.path().join("exact-peer"), &exact_socket);
    let exact_url = format!("http://{}/v1/team", exact.addr);
    transport::publish(&Source::Url(exact_url.clone()), &newest).expect("seed exact peer");
    let outcome = sync_with_candidates(&member, Some(Duration::ZERO), &[exact_url])
        .expect("a no-change pass with an exact peer");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 3,
            served: Some(2)
        }
    );
    assert!(served() == newest, "the newest copy is served");
    exact.stop();

    // A pass that does not long-poll (a repair is due) hears an older copy as a signed body,
    // not a `204` naming its seq: it puts the newest back all the same, and the repair it
    // was for runs the pass after.
    server.stop();
    let server = serve(&root.path().join("again"), &socket);
    transport::publish(&source, &stale).expect("the stale copy is taken again");
    let token = materialize::target_path(&member.home, "token").expect("target");
    std::fs::remove_file(&token).expect("the token gone");
    let mut repaired = None;
    let (_, outcome) = subscriber_pass(&member, &mut repaired, &FOUND_NONE).expect("a repair pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 3,
            served: Some(2)
        }
    );
    assert!(served() == newest, "the newest copy is served again");
    let (_, outcome) = subscriber_pass(&member, &mut repaired, &FOUND_NONE).expect("the repair");
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read(&token).expect("the token"), b"inert-token");

    // An older version signed before a rotation is put back over the same way: its seq is
    // read as a `204`'s is, and what goes back is this Mac's copy under its new pin.
    rotate(&admin);
    assert!(matches!(
        sync(&member, None).expect("the rotated version"),
        SyncOutcome::Applied { seq: 4, .. }
    ));
    let rotated = std::fs::read(member.cache_file()).expect("the member's copy");
    let peer_socket = bound();
    let peer = serve(&root.path().join("rotated-peer"), &peer_socket);
    let peer_url = format!("http://{}/v1/team", peer.addr);
    transport::publish(&Source::Url(peer_url.clone()), &rotated)
        .expect("an exact peer holds the rotated copy");
    server.stop();
    let server = serve(&root.path().join("rotated"), &socket);
    transport::publish(&source, &stale).expect("the stale copy, signed before the rotation");
    std::fs::remove_file(&token).expect("the token gone");
    let mut repaired = None;
    let outcome = sync_with_candidates(&member, None, &[peer_url]).expect("a repair pass");
    assert_eq!(
        outcome,
        SyncOutcome::PutBack {
            seq: 4,
            served: Some(2)
        }
    );
    assert!(served() == rotated, "the rotated copy is served");
    let (_, outcome) = subscriber_pass(&member, &mut repaired, &FOUND_NONE).expect("the repair");
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 4, .. }),
        "{outcome:?}"
    );
    peer.stop();
    server.stop();
}

#[test]
fn bounded_member_join_and_sync_keep_normal_admission_and_materialization() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, admin, _) = a_vault_with_a_member(root.path(), &socket);
    let member = Paths::for_home(&root.path().join("bounded"));
    let signer = crate::b64::encode_padded(&pinned(&admin).signer.expect("pin"));
    let options = JoinOptions {
        source: format!("{url}#{signer}"),
        host: Some("bounded".into()),
        interactive: false,
        wait: Duration::ZERO,
        confirm: &|_| false,
        password: JoinPassword::Given(None),
        fetched: None,
        requested: &|| {},
        say: &|_| {},
    };
    assert!(matches!(
        join_if_unchanged_until(&member, &options, None, Instant::now() + HANG_DETECTOR)
            .expect("join request"),
        JoinOutcome::Pending { .. }
    ));
    let recipient = read_identity(&member)
        .unwrap()
        .unwrap()
        .recipient()
        .to_bech32();
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.add_machine("bounded", &recipient).expect("admit");
    admin_publish(&admin, &opened, &doc).expect("publish approval");
    assert!(matches!(
        sync_reader_until(&member, Instant::now() + HANG_DETECTOR).expect("bounded sync"),
        SyncOutcome::Applied { .. }
    ));
    assert_eq!(
        std::fs::read(materialize::target_path(&member.home, "token").unwrap()).unwrap(),
        b"inert-token"
    );
    assert_eq!(load_state(&member).unwrap().membership, Stored::In);
    server.stop();
}

/// A Mac's mirrors (`source.toml`'s `mirrors`) serve it while its url does not answer, under
/// the one pin and floor its url's version is read under: a mirror with a newer version is
/// applied, a mirror with an older one never rolls the Mac back, and neither is written to.
///
/// NEGATIVE CONTROL: leave the mirrors out of the peers a pass reads (`purpose.vault_read()`
/// false in `collect_sync_peer_urls`): fails "the newer mirror serves the Mac".
#[test]
fn a_mirror_serves_a_member_whose_url_is_down_and_never_rolls_it_back() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let held_two = std::fs::read(member.cache_file()).expect("seq 2");
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"new-token")
        .unwrap();
    admin_publish(&admin, &opened, &doc).expect("seq 3");
    let three = served_at(&Source::Url(url.clone())).expect("seq 3 served");
    let mirror_at = |name: &str, bytes: &[u8]| {
        let socket = bound();
        let running = serve(&root.path().join(name), &socket);
        let mirror = format!("http://{}/v1/team", running.addr);
        transport::publish(&Source::Url(mirror.clone()), bytes).expect("seeded");
        (running, mirror)
    };
    let (newer, newer_url) = mirror_at("newer", &three);
    let (older, older_url) = mirror_at("older", &held_two);
    // The url refuses at once: its server and the listener it was handed are gone.
    server.stop();
    drop(socket);
    let token = || std::fs::read(materialize::target_path(&member.home, "token").unwrap());
    assert!(sync(&member, None).is_err(), "the url alone is down");
    assert_eq!(load_state(&member).unwrap().seq, 2);

    let config = configured(&member).unwrap();
    set_mirrors(&member, &config, &[older_url.clone(), newer_url.clone()]).expect("listed");
    let started = Instant::now();
    let outcome = sync(&member, None);
    println!(
        "mirror fail-over: url refused, seq 3 applied from a mirror in {:?}",
        started.elapsed()
    );
    assert!(
        matches!(outcome, Ok(SyncOutcome::Applied { seq: 3, .. })),
        "the newer mirror serves the Mac: {outcome:?}"
    );
    assert_eq!(token().unwrap(), b"new-token");
    assert_eq!(
        configured(&member).unwrap().mirrors,
        [older_url.clone(), newer_url],
        "a pass keeps the list"
    );

    newer.stop();
    let _ = sync(&member, None);
    assert_eq!(load_state(&member).unwrap().seq, 3, "never rolled back");
    assert_eq!(token().unwrap(), b"new-token");
    assert_eq!(
        served_at(&Source::Url(older_url)).expect("the older mirror"),
        held_two,
        "nothing is written to a mirror"
    );
    older.stop();
}

/// A Mac's mirrors are read for the vault, its envelope and its records, and never sent a
/// join: a join and its re-asks go to the url and the vault's servers alone.
///
/// NEGATIVE CONTROL: have `PeerUse::vault_read` hold for `Join`: fails "no join reaches a
/// mirror".
#[test]
fn a_mirror_is_read_for_the_vault_and_never_sent_a_join() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let url = "http://vault.local:7484/v1/team";
    let mirror = "http://192.168.1.40:7484/v1/team";
    let config = Config {
        source: Source::Url(url.to_owned()),
        mirrors: vec![mirror.to_owned()],
        name: "team".to_owned(),
        signer: Some([7; 32]),
        followed: Vec::new(),
        host: "m1".to_owned(),
    };
    let peers =
        |purpose| collect_sync_peer_urls(&paths, &config, url, &[], Some(&FOUND_NONE), purpose);
    for purpose in [PeerUse::Records, PeerUse::Envelope, PeerUse::Reader] {
        assert_eq!(peers(purpose), [mirror]);
    }
    assert!(peers(PeerUse::Join).is_empty(), "no join reaches a mirror");
}

/// A rejoin of the same vault (its url, name and pin) keeps the Mac's mirrors, as it keeps
/// the version it holds; a join that starts afresh (here at another url), which resets that
/// version, starts with none.
///
/// NEGATIVE CONTROLS: save every join with no mirrors (`mirrors = Vec::new()` in
/// `join_with_until`): fails "a rejoin keeps the mirrors"; keep them on every join (drop
/// `.filter(|_| goes_on)` there): fails "a join elsewhere starts with no mirrors".
#[test]
fn a_rejoin_of_the_same_vault_keeps_its_mirrors_and_a_join_elsewhere_starts_with_none() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, _admin, member) = a_vault_with_a_member(root.path(), &socket);
    let mirror = "http://192.168.1.40:7484/v1/team".to_owned();
    let config = configured(&member).unwrap();
    set_mirrors(&member, &config, std::slice::from_ref(&mirror)).expect("listed");
    let signer = crate::b64::encode_padded(&config.signer.expect("pin"));
    join_pinned(&member, &format!("{url}#{signer}"), "mb");
    assert_eq!(
        configured(&member).unwrap().mirrors,
        [mirror],
        "a rejoin keeps the mirrors"
    );
    assert_eq!(load_state(&member).unwrap().seq, 2);

    let elsewhere_socket = bound();
    let elsewhere = serve(&root.path().join("elsewhere"), &elsewhere_socket);
    let elsewhere_url = format!("http://{}/v1/team", elsewhere.addr);
    let held = std::fs::read(member.cache_file()).expect("seq 2");
    transport::publish(&Source::Url(elsewhere_url.clone()), &held).expect("seeded");
    join_pinned(&member, &format!("{elsewhere_url}#{signer}"), "mb");
    let joined = configured(&member).unwrap();
    assert_eq!(joined.source, Source::Url(elsewhere_url));
    assert!(
        joined.mirrors.is_empty(),
        "a join elsewhere starts with no mirrors: {:?}",
        joined.mirrors
    );
    elsewhere.stop();
    server.stop();
}

/// A mirror is read for the vault's records and never vouches that a pass read the newest
/// of them, nor hears who reads: nothing posts it records, so it falls behind by
/// construction, and `readmit`'s freshness read must fail closed while only a mirror
/// answers; a mirror can be a copy a third party keeps, so it is not sent this Mac's
/// recipient. A listed mirror the pass's own peers also name is a server, and does both.
///
/// NEGATIVE CONTROLS: count a mirror's records bound (drop `.filter(|(url, _, _)|
/// !read_only.holds(url))` from `answering_bounds` in `sync_records_until`): fails "a mirror
/// alone never vouches the pass fresh"; send it the subscriber (`subscriber_for` returns
/// `subscriber`): fails "a mirror is not told who reads".
#[test]
fn a_mirror_alone_never_vouches_a_records_pass_fresh_nor_hears_who_reads() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, _url, _admin, member) = a_vault_with_a_member(root.path(), &socket);
    let held_two = std::fs::read(member.cache_file()).expect("seq 2");
    // A server holding the vault's version and none of its records, listed as a mirror.
    let listed = |store: &str| {
        let store = root.path().join(store);
        let socket = bound();
        let running = serve(&store, &socket);
        let url = format!("http://{}/v1/team", running.addr);
        transport::publish(&Source::Url(url.clone()), &held_two).expect("seeded");
        let config = configured(&member).unwrap();
        set_mirrors(&member, &config, std::slice::from_ref(&url)).expect("listed");
        (running, store, url)
    };
    let recipient = read_identity(&member)
        .unwrap()
        .unwrap()
        .recipient()
        .to_bech32();
    // What a server stopped (which writes down every read it heard) heard of this Mac.
    let heard = |store: &Path| {
        crate::server::read_last_heard(store)
            .into_iter()
            .any(|(_, heard, _)| heard == recipient)
    };
    // The url refuses at once: its server and the listener it was handed are gone.
    server.stop();
    drop(socket);
    // `readmit::read_newest`'s pass: bounded by the freshness it asks for.
    let pass = |peers: &[String]| {
        let by = Instant::now() + PROBE_DEADLINE;
        sync_records_until(&member, None, peers, None, Some(by), None)
    };
    assert!(pass(&[]).is_err(), "the url alone is down");

    let (mirror, mirror_store, _) = listed("mirror");
    let read = pass(&[]).expect("the mirror answers");
    assert!(
        !read.fresh(),
        "a mirror alone never vouches the pass fresh: {read:?}"
    );
    let _ = sync(&member, None);
    mirror.stop();
    assert!(!heard(&mirror_store), "a mirror is not told who reads");

    let (named, named_store, named_url) = listed("named");
    let read = pass(std::slice::from_ref(&named_url)).expect("the server answers");
    assert!(
        read.fresh(),
        "a server named as the pass's own peer vouches for it: {read:?}"
    );
    named.stop();
    assert!(heard(&named_store), "a server is told who reads");
}

/// An admin's pass starts no heal from a mirror's head: nothing writes a mirror, so its
/// version lags by construction, and a head two versions behind is one `heads_diverged`
/// flags. Neither is anything written to it.
///
/// NEGATIVE CONTROL: count every peer's head (drop `.filter(|p| !read_only().holds(&p.url))`
/// from the heal's `observed` in `sync_repairing_inner`): fails "a mirror's head starts no
/// heal".
#[test]
fn an_admins_pass_starts_no_heal_from_a_lagging_mirror() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, _url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let held_two = std::fs::read(member.cache_file()).expect("seq 2");
    for value in [b"three".as_slice(), b"four"] {
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", value).unwrap();
        admin_publish(&admin, &opened, &doc).expect("published");
    }
    let mirror_socket = bound();
    let mirror = serve(&root.path().join("mirror"), &mirror_socket);
    let mirror_url = format!("http://{}/v1/team", mirror.addr);
    transport::publish(&Source::Url(mirror_url.clone()), &held_two).expect("seeded");
    let runs = || log_entries::MIRROR_RUNS.with(std::cell::Cell::get);
    sync(&admin, None).expect("the admin's pass");
    let settled = runs();
    sync(&admin, None).expect("the admin's pass");
    assert_eq!(runs(), settled, "a settled admin's pass starts no heal");

    let config = configured(&admin).unwrap();
    set_mirrors(&admin, &config, std::slice::from_ref(&mirror_url)).expect("listed");
    sync(&admin, None).expect("the admin's pass");
    assert_eq!(runs(), settled, "a mirror's head starts no heal");
    assert_eq!(
        served_at(&Source::Url(mirror_url)).expect("the mirror"),
        held_two,
        "nothing is written to a mirror"
    );
    mirror.stop();
    server.stop();
}

#[test]
fn bounded_member_sync_uses_a_verified_peer_when_the_primary_is_silent() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, peer_url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"new-token")
        .unwrap();
    admin_publish(&admin, &opened, &doc).unwrap();
    let silent = bound();
    let mut config = configured(&member).unwrap();
    config.source = Source::Url(format!("http://{}/v1/team", silent.local_addr().unwrap()));
    save_config(&member, &config).unwrap();
    assert!(matches!(
        bounded::sync_reader_with_candidates_until(
            &member,
            Instant::now() + HANG_DETECTOR,
            &[peer_url],
        )
        .expect("verified peer answers"),
        SyncOutcome::Applied { seq: 3, .. }
    ));
    assert_eq!(
        std::fs::read(materialize::target_path(&member.home, "token").unwrap()).unwrap(),
        b"new-token"
    );
    server.stop();
}

#[test]
fn bounded_local_write_failure_still_reads_a_newer_network_version() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = bound();
    let (server, _, admin, member) = a_vault_with_a_member(root.path(), &socket);
    let token = materialize::target_path(&member.home, "token").unwrap();
    std::fs::remove_file(&token).unwrap();
    std::fs::create_dir(&token).unwrap();
    let before = load_state(&member).unwrap();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "new-note", b"new-version")
        .unwrap();
    let published = admin_publish(&admin, &opened, &doc).unwrap();
    assert!(published.seq > before.seq);
    let outcome = sync_reader_until(&member, Instant::now() + HANG_DETECTOR).unwrap();
    assert!(matches!(outcome, SyncOutcome::Applied { seq, .. } if seq == published.seq));
    let after = load_state(&member).unwrap();
    assert_eq!(after.seq, published.seq);
    assert!(after.failed, "the local obstruction is still reported");
    assert_eq!(
        std::fs::read(materialize::target_path(&member.home, "new-note").unwrap()).unwrap(),
        b"new-version"
    );
    server.stop();
}

#[test]
fn bounded_member_operations_do_not_wait_forever_for_the_state_lock() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, _, member) = a_vault_with_a_member(root.path(), &socket);
    let config = configured(&member).unwrap();
    let bytes = std::fs::read(member.cache_file()).unwrap();
    let state = std::fs::read(member.state_file()).unwrap();
    let lock = StateLock::take(&member).expect("hold state lock");
    let started = Instant::now();
    let error = sync_reader_until(&member, Instant::now() + Duration::from_millis(200))
        .expect_err("held state lock");
    assert_eq!(error.kind(), crate::ErrorKind::TimedOut);
    let options = JoinOptions {
        source: url,
        host: Some(config.host.clone()),
        interactive: false,
        wait: Duration::ZERO,
        confirm: &|_| false,
        password: JoinPassword::Given(None),
        fetched: Some(&bytes),
        requested: &|| {},
        say: &|_| {},
    };
    let error = join_if_unchanged_until(
        &member,
        &options,
        Some(&config),
        Instant::now() + Duration::from_millis(200),
    )
    .expect_err("join respects the same state lock");
    assert_eq!(error.kind(), crate::ErrorKind::TimedOut);
    assert!(started.elapsed() < HANG_DETECTOR, "bounded operations hung");
    assert_eq!(std::fs::read(member.state_file()).unwrap(), state);
    drop(lock);
    server.stop();
}

#[test]
fn bounded_member_sync_defers_putting_back_a_missing_server_envelope() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
    server.stop();
    let server = serve_apart(&root.path().join("empty"), &socket, &pin_of(&admin));
    let source = Source::Url(url);
    assert!(served_at(&source).is_none());
    assert_eq!(
        sync_reader_until(&member, Instant::now() + HANG_DETECTOR).unwrap(),
        SyncOutcome::Unchanged { seq: 2 }
    );
    assert!(served_at(&source).is_none(), "reader did not publish");
    assert!(matches!(
        sync(&member, None).unwrap(),
        SyncOutcome::PutBack { .. }
    ));
    assert!(
        served_at(&source).is_some(),
        "standing sync still repairs servers"
    );
    server.stop();
}

/// An approval's bounded wait ends at its own time: the server holds a long-poll for its
/// own (here two hang detectors), which a try of [`sync_within`] does not outlast, and
/// running out of time is no news, not a failure. Negative control: a try on a fetch's
/// deadline (its total plus [`transport::FETCH_MARGIN`], under test two hang detectors),
/// as [`sync`] fetches, waits out the server's hold, past the detector.
#[test]
fn a_bounded_wait_ends_at_its_own_time_whatever_the_server_holds() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let socket = bound();
    let (server, _, _, member) = a_vault_with_a_member(root.path(), &socket);
    server.stop();
    let holding = crate::server::start(
        socket.try_clone().expect("a copy of the socket"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: 2 * HANG_DETECTOR,
            keep_awake: false,
        },
    )
    .expect("server starts");
    let started = Instant::now();
    let outcome = sync_within(&member, Duration::from_millis(300)).expect("no news");
    let took = started.elapsed();
    assert_eq!(outcome, SyncOutcome::Unchanged { seq: 2 });
    assert!(
        took < HANG_DETECTOR,
        "the wait ran past its own total toward the server's hold: {took:?}"
    );
    holding.stop();
}

/// A file an entry left this Mac's own (a plain value adopted, an ` if-absent` file left in
/// place) is out of the manifest; deleted, it is a gap the vault fills at the version the
/// Mac holds, by an explicit sync and by the subscriber's repair. A file an anchored entry
/// left (its anchor this Mac's own) is no gap, so readiness never waits on it.
#[test]
fn a_gone_file_an_entry_left_this_macs_own_is_filled_again_at_the_same_version() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().expect("utf-8"), None, Some("admin")).expect("create");
    let target = |name: &str| materialize::target_path(&admin.home, name).expect("target");
    let (adopted, hosts) = (target("adopted"), target("~/.config/gh/hosts.yml"));
    let (anchor, anchored) = (
        target("~/.config/git/config"),
        target("~/.config/git/credentials"),
    );
    let paired = target("~/.aterm/apple/devid-pair.key");
    fsx::ensure_private_dir(&admin.home.join(".secrets")).expect("~/.secrets");
    fsx::write_atomic(&adopted, b"inert-adopted", 0o600).expect("this Mac's value");
    std::fs::create_dir_all(hosts.parent().expect("parent")).expect("dir");
    fsx::write_atomic(&hosts, b"mine: {}\n", 0o600).expect("this Mac's hosts");
    std::fs::create_dir_all(anchor.parent().expect("parent")).expect("dir");
    fsx::write_atomic(&anchor, b"[mine]\n", 0o600).expect("this Mac's config");
    std::fs::create_dir_all(paired.parent().expect("parent")).expect("dir");
    fsx::write_atomic(&paired, b"mine\n", 0o600).expect("this Mac's half of a pair");
    let opened = admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "adopted", b"inert-adopted")
        .expect("set");
    for (name, value, anchored_to) in [
        ("~/.config/gh/hosts.yml", &b"github.com: {}\n"[..], None),
        ("~/.config/git/config", b"[tool]\n", None),
        (
            "~/.config/git/credentials",
            b"inert-key",
            Some("~/.config/git/config"),
        ),
        ("~/.aterm/apple/devid-pair.cer", b"inert-a", None),
        (
            "~/.aterm/apple/devid-pair.key",
            b"inert-b",
            Some("~/.aterm/apple/devid-pair.cer"),
        ),
    ] {
        doc.set_entry_with(&Audience::Everyone, name, value, true, anchored_to)
            .expect("set");
    }
    admin_publish(&admin, &opened, &doc).expect("publish");
    sync(&admin, None).expect("sync");
    let own = load_state(&admin).expect("state").own.expect("recorded");
    assert_eq!(
        own,
        [
            "adopted",
            "~/.config/gh/hosts.yml",
            "~/.aterm/apple/devid-pair.key",
            "~/.config/git/config"
        ]
        .map(String::from)
        .into(),
        "an entry anchored to this Mac's own file is not named; one anchored to the vault's is"
    );
    assert!(!anchored.exists(), "the anchored entry left no file");
    assert!(materialization_ready(&admin).expect("ready"));
    assert_eq!(
        sync(&admin, None).expect("sync"),
        SyncOutcome::Unchanged { seq: 2 }
    );
    let mut repaired = None;
    assert!(!repair_due(&admin, &mut repaired), "nothing to repair");

    std::fs::remove_file(&adopted).expect("gone");
    std::fs::remove_file(&hosts).expect("gone");
    std::fs::remove_file(&paired).expect("gone");
    assert!(!materialization_ready(&admin).expect("ready"));
    assert!(repair_due(&admin, &mut repaired), "the gaps are due");
    assert!(!repair_due(&admin, &mut repaired), "once per version");
    let SyncOutcome::Applied { seq: 2, report } = sync(&admin, None).expect("filled") else {
        panic!("the gaps are filled at the version held");
    };
    assert_eq!(
        report.written,
        [
            "adopted",
            "~/.aterm/apple/devid-pair.key",
            "~/.config/gh/hosts.yml"
        ]
        .map(String::from)
    );
    assert_eq!(std::fs::read(&adopted).expect("filled"), b"inert-adopted");
    assert_eq!(std::fs::read(&paired).expect("filled"), b"inert-b");
    assert_eq!(std::fs::read(&hosts).expect("filled"), b"github.com: {}\n");
    let state = load_state(&admin).expect("state");
    assert!(
        state.manifest.files.contains_key("adopted"),
        "the vault's now"
    );
    assert_eq!(
        state.own.expect("recorded"),
        ["~/.config/git/config".to_string()].into()
    );
    assert!(materialization_ready(&admin).expect("ready"));
    assert_eq!(
        sync(&admin, None).expect("sync"),
        SyncOutcome::Unchanged { seq: 2 }
    );

    // A state an older aterm wrote has no such record: readiness does not wait on one,
    // and the next sync, or the subscriber's next pass, applies once to write it.
    let mut state = load_state(&admin).expect("state");
    state.own = None;
    save_state(&admin, &state).expect("state.toml");
    assert!(materialization_ready(&admin).expect("ready"));
    assert!(repair_due(&admin, &mut None), "applied once");
    let SyncOutcome::Applied { seq: 2, report } = sync(&admin, None).expect("sync") else {
        panic!("applied once at the version held");
    };
    assert!(quiet(&report), "nothing to write");
    assert!(load_state(&admin).expect("state").own.is_some());
    assert_eq!(
        sync(&admin, None).expect("sync"),
        SyncOutcome::Unchanged { seq: 2 }
    );
}

/// A write that failed before it wrote anything keeps nothing pending ([`landed`]), so a file
/// a person writes with the same value later is theirs: adopted, and left by the vault's
/// unset.
#[test]
fn a_write_that_failed_before_it_wrote_leaves_a_later_identical_file_the_persons() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = root.path().join("team.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, source.to_str().expect("utf-8"), None, Some("admin")).expect("create");
    let token = materialize::target_path(&admin.home, "~/.config/gh/hosts.yml").expect("target");
    let dir = token.parent().expect("parent").to_path_buf();
    std::fs::create_dir_all(&dir).expect("dir");
    let mode = |mode| {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("mode");
    };
    let publish = |set: bool| {
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        if set {
            doc.set_entry(
                &Audience::Everyone,
                "~/.config/gh/hosts.yml",
                b"inert-token",
            )
            .expect("set");
        } else {
            doc.unset_entry(&Audience::Everyone, "~/.config/gh/hosts.yml")
                .expect("unset");
        }
        admin_publish(&admin, &opened, &doc).expect("publish");
    };

    mode(0o500);
    publish(true);
    let _ = sync(&admin, None);
    let state = load_state(&admin).expect("state");
    assert!(state.failed, "the write failed");
    assert!(state.pending.is_empty(), "{:?}", state.pending);
    mode(0o700);
    fsx::write_atomic(&token, b"inert-token", 0o600).expect("the person's own file");
    sync(&admin, None).expect("sync");
    let state = load_state(&admin).expect("state");
    assert!(!state.failed);
    assert!(
        !state.manifest.files.contains_key("~/.config/gh/hosts.yml"),
        "adopted"
    );
    publish(false);
    sync(&admin, None).expect("sync");
    assert!(token.exists(), "the vault's unset leaves the person's file");
}

/// Of the digests a pass pended, those of entries it failed whose file holds the value stay:
/// a write that landed and then failed its directory's flush. Negative controls: an entry
/// that did not fail, and a failed one whose file holds another value or none.
#[test]
fn a_failed_entry_stays_pending_only_while_its_file_holds_the_value() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path();
    let target = |name: &str| materialize::target_path(home, name).expect("target");
    fsx::ensure_private_dir(&home.join(".secrets")).expect("~/.secrets");
    let digest = materialize::digest_hex(b"inert-value");
    for (name, held) in [
        ("landed", Some(&b"inert-value"[..])),
        ("other", Some(b"inert-other")),
        ("absent", None),
        ("written", Some(b"inert-value")),
    ] {
        if let Some(held) = held {
            fsx::write_atomic(&target(name), held, 0o600).expect("file");
        }
    }
    let pended: materialize::Pending = ["landed", "other", "absent", "written"]
        .into_iter()
        .map(|name| (name.to_string(), digest.clone()))
        .collect();
    let failed: Vec<(String, String)> = ["landed", "other", "absent"]
        .into_iter()
        .map(|name| (name.to_string(), "a reason".to_string()))
        .collect();
    assert_eq!(
        landed(home, pended, &failed),
        [("landed".to_string(), digest)].into()
    );
}
