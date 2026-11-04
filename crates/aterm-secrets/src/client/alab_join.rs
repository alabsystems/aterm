// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable request to bootstrap the pinned default vault, without claiming membership.

use super::*;

const FILE: &str = "alab-join-pending";
const INTENT: &[u8] = b"ALAB-JOIN-1\n";

#[cfg(test)]
type Attempt = Box<dyn Fn(&Paths, ExpectedConfig<'static>) -> crate::Result<()>>;
#[cfg(test)]
thread_local! {
    static ATTEMPT: std::cell::RefCell<Option<Attempt>> = const { std::cell::RefCell::new(None) };
}

/// Keep the default ALab join retryable before its first authenticated fetch.
/// This writes no source, identity, membership or credentials. An existing source wins.
///
/// # Errors
/// Sharing is disabled, the slot is reserved for creation, or private state cannot be written.
pub fn prepare_alab_join_retry(paths: &Paths) -> crate::Result<()> {
    if !crate::swarm::enabled_in_home(&paths.home)? {
        return Err(Error::new("sharing is off; ALab bootstrap was not started"));
    }
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        return Ok(());
    }
    creation::permit_join(paths)?;
    fsx::write_atomic(&paths.dir.join(FILE), INTENT, 0o600)
}

pub(super) fn resume(paths: &Paths) -> crate::Result<()> {
    #[cfg(test)]
    if let Some(result) = ATTEMPT.with(|attempt| {
        attempt
            .borrow()
            .as_ref()
            .map(|join| resume_with(paths, join))
    }) {
        return result;
    }
    resume_with(paths, |paths, expected| {
        join_live(
            paths,
            &JoinOptions {
                source: String::new(),
                host: None,
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
            expected,
        )
        .map(drop)
    })
}

fn resume_with(
    paths: &Paths,
    join: impl FnOnce(&Paths, ExpectedConfig<'static>) -> crate::Result<()>,
) -> crate::Result<()> {
    if fsx::read_private(&paths.dir.join(FILE), INTENT.len())?.is_none() {
        return Ok(());
    }
    {
        let _lock = StateLock::take(paths)?;
        let Some(intent) = fsx::read_private(&paths.dir.join(FILE), INTENT.len())? else {
            return Ok(());
        };
        if intent.as_slice() != INTENT {
            return Err(Error::new("unrecognized ALab bootstrap intent"));
        }
        if config(paths)?.is_some() {
            return fsx::wipe_and_remove(&paths.dir.join(FILE));
        }
        if !crate::swarm::enabled_in_home(&paths.home)? {
            return Ok(());
        }
    }
    // The ordinary join verifies the pinned signature before configuring anything. Its mode
    // preserves another source chosen during the fetch and rechecks sharing before committing
    // the fetched configuration, applying it or posting a request. Work already in flight may
    // finish; a committed configuration remains for the subscriber when sharing resumes.
    let joined = join(paths, ExpectedConfig::WhileSharing(None));
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        fsx::wipe_and_remove(&paths.dir.join(FILE))?;
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscriber_attempt(
        paths: &Paths,
        attempt: impl Fn(&Paths, ExpectedConfig<'static>) -> crate::Result<()> + 'static,
    ) -> crate::Result<(Config, SyncOutcome)> {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                ATTEMPT.with(|attempt| *attempt.borrow_mut() = None);
            }
        }
        ATTEMPT.with(|kept| *kept.borrow_mut() = Some(Box::new(attempt)));
        let _clear = Clear;
        subscriber_pass(paths, &mut None, &FOUND_NONE)
    }

    #[test]
    fn bootstrap_intent_survives_first_fetch_and_authentication_failures_then_recovers() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("newcomer"));
        prepare_alab_join_retry(&paths).unwrap();
        let failed = subscriber_attempt(&paths, |_, _| Err(Error::new("network unavailable")));
        assert!(failed.is_err());
        assert!(config(&paths).unwrap().is_none());
        assert!(read_machine_identity(&paths).unwrap().is_none());
        assert_eq!(status(&paths).unwrap().membership, Membership::Unconfigured);

        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = crate::server::start(
            socket,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/team", server.addr);
        let admin = Paths::for_home(&root.path().join("admin"));
        let import = root.path().join("import");
        std::fs::create_dir(&import).unwrap();
        std::fs::write(import.join("bootstrap-test"), b"fixture-only").unwrap();
        let created = test_create_unbound(&admin, &url, Some(&import), Some("admin")).unwrap();
        let options = JoinOptions {
            source: format!("{url}#{}", created.signer),
            host: Some("newcomer".into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        };
        assert!(
            resume_with(&paths, |paths, expected| {
                join_with(paths, &options, expected, |_| {
                    Err(Error::new("first fetch unavailable"))
                })
                .map(drop)
            })
            .is_err()
        );
        assert!(config(&paths).unwrap().is_none());
        assert!(read_machine_identity(&paths).unwrap().is_none());
        let source = Source::Url(url);
        let Fetched::Body(bytes) =
            transport::fetch(&source, None, Duration::from_secs(60)).unwrap()
        else {
            panic!("signed envelope");
        };
        let mut invalid = bytes.clone();
        let signature = String::from_utf8(bytes.clone())
            .unwrap()
            .find("signature: ")
            .unwrap()
            + "signature: ".len();
        invalid[signature] = if invalid[signature] == b'A' {
            b'B'
        } else {
            b'A'
        };
        assert!(
            envelope::parse(&invalid)
                .unwrap()
                .verify_signature()
                .is_err()
        );
        let join_using = |bytes: Vec<u8>| {
            let source = options.source.clone();
            move |paths: &Paths, expected| {
                let options = JoinOptions {
                    source: source.clone(),
                    host: Some("newcomer".into()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                };
                join_with(paths, &options, expected, |_| {
                    Ok(Fetched::Body(bytes.clone()))
                })
                .map(drop)
            }
        };
        assert!(subscriber_attempt(&paths, join_using(invalid)).is_err());
        assert!(config(&paths).unwrap().is_none());
        assert!(read_machine_identity(&paths).unwrap().is_none());
        assert!(!paths.home.join(".secrets").exists());

        // A new process's paths, on an ordinary subscriber round, recover from the retained
        // intent. The injected transport serves a test vault with its own explicit pin;
        // production resume always uses the compiled default ALab authority.
        let restarted = Paths::for_home(&paths.home);
        subscriber_attempt(&restarted, join_using(bytes)).unwrap();
        assert_eq!(status(&restarted).unwrap().membership, Membership::Pending);
        assert!(!restarted.home.join(".secrets").exists());
        assert!(!restarted.dir.join(FILE).exists());
        resume_with(&restarted, |_, _| panic!("configured vault must win")).unwrap();
        let recipient = read_machine_identity(&restarted)
            .unwrap()
            .unwrap()
            .recipient()
            .to_bech32();
        // The server it reads from here on is its own on loopback, which answers.
        let _answering = ServersAnswering::set();
        approve_request(&admin, &recipient).unwrap();
        sync(&restarted, None).unwrap();
        assert!(matches!(
            status(&restarted).unwrap().membership,
            Membership::In { .. }
        ));
        assert_eq!(
            std::fs::read(restarted.home.join(".secrets/bootstrap-test")).unwrap(),
            b"fixture-only"
        );
    }

    #[test]
    fn bootstrap_retry_preserves_sharing_off_and_a_later_source_choice() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        let settings = paths.home.join(".config/aterm/aterm.toml");
        std::fs::write(&settings, "[swarm]\nenabled = false\n").unwrap();
        assert!(prepare_alab_join_retry(&paths).is_err());
        resume_with(&paths, |_, _| panic!("sharing off must hold the retry")).unwrap();
        assert!(paths.dir.join(FILE).exists());
        std::fs::write(&settings, "[swarm]\nenabled = true\n").unwrap();
        let chosen = Config {
            source: Source::Path(root.path().join("personal.vault")),
            name: "personal".into(),
            signer: None,
            followed: Vec::new(),
            host: "newcomer".into(),
        };
        save_config(&paths, &chosen).unwrap();
        resume_with(&paths, |_, _| panic!("a later source choice must win")).unwrap();
        assert_eq!(config(&paths).unwrap(), Some(chosen));
        assert!(!paths.dir.join(FILE).exists());
    }

    #[test]
    fn sharing_off_during_bootstrap_fetch_holds_the_join_until_enabled_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = crate::server::start(
            socket,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/team", server.addr);
        let admin = Paths::for_home(&root.path().join("admin"));
        let created = test_create_unbound(&admin, &url, None, Some("admin")).unwrap();
        let Fetched::Body(bytes) =
            transport::fetch(&Source::Url(url.clone()), None, Duration::from_secs(60)).unwrap()
        else {
            panic!("signed envelope");
        };
        let options = JoinOptions {
            source: format!("{url}#{}", created.signer),
            host: Some("newcomer".into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        };
        let paths = Paths::for_home(&root.path().join("newcomer"));
        prepare_alab_join_retry(&paths).unwrap();
        let settings = paths.home.join(".config/aterm/aterm.toml");
        let stopped = resume_with(&paths, |paths, expected| {
            join_with(paths, &options, expected, |_| {
                // The fetch began with sharing on, and finishes after the user turned it off.
                std::fs::write(&settings, "[swarm]\nenabled = false\n").unwrap();
                Ok(Fetched::Body(bytes.clone()))
            })
            .map(drop)
        });
        assert!(
            stopped.is_err(),
            "bootstrap continued after sharing was turned off"
        );
        assert!(config(&paths).unwrap().is_none());
        assert!(read_machine_identity(&paths).unwrap().is_none());
        assert!(paths.dir.join(FILE).exists());
        assert!(transport::list_joins(&url).unwrap().is_empty());

        // Turning sharing on retries the retained intent and queues the same ordinary join.
        std::fs::write(&settings, "[swarm]\nenabled = true\n").unwrap();
        resume_with(&paths, |paths, expected| {
            join_with(paths, &options, expected, |_| {
                Ok(Fetched::Body(bytes.clone()))
            })
            .map(drop)
        })
        .unwrap();
        assert_eq!(status(&paths).unwrap().membership, Membership::Pending);
        assert!(!paths.dir.join(FILE).exists());
        assert_eq!(transport::list_joins(&url).unwrap().len(), 1);

        // An explicit manual join keeps its authority even with automatic sharing off.
        let manual = Paths::for_home(&root.path().join("manual"));
        std::fs::create_dir_all(manual.home.join(".config/aterm")).unwrap();
        std::fs::write(
            manual.home.join(".config/aterm/aterm.toml"),
            "[swarm]\nenabled = false\n",
        )
        .unwrap();
        join_with(&manual, &options, ExpectedConfig::Read(None), |_| {
            Ok(Fetched::Body(bytes.clone()))
        })
        .unwrap();
        assert_eq!(status(&manual).unwrap().membership, Membership::Pending);
        assert_eq!(transport::list_joins(&url).unwrap().len(), 2);

        // If sharing turns off after configuration was committed, keep that partial join,
        // but start neither its apply nor its request. The subscriber can finish it later.
        let partial = Paths::for_home(&root.path().join("partial"));
        prepare_alab_join_retry(&partial).unwrap();
        let settings = partial.home.join(".config/aterm/aterm.toml");
        let say = |event: &JoinEvent<'_>| {
            if matches!(event, JoinEvent::Machine { .. }) {
                std::fs::write(&settings, "[swarm]\nenabled = false\n").unwrap();
            }
        };
        let options = JoinOptions {
            host: Some("partial".into()),
            say: &say,
            ..options
        };
        let stopped = resume_with(&partial, |paths, expected| {
            join_with(paths, &options, expected, |_| Ok(Fetched::Body(bytes))).map(drop)
        });
        assert!(stopped.is_err());
        let config = config(&partial).unwrap().expect("committed source is kept");
        assert!(read_machine_identity(&partial).unwrap().is_some());
        assert!(!partial.cache_file().exists(), "nothing was applied");
        assert!(!partial.dir.join(FILE).exists());
        assert_eq!(transport::list_joins(&url).unwrap().len(), 2);

        std::fs::write(&settings, "[swarm]\nenabled = true\n").unwrap();
        subscriber_pass(&partial, &mut None, &FOUND_NONE).unwrap();
        assert_eq!(load_state(&partial).unwrap().membership, Stored::Pending);
        // The normal join grace elapses: its ordinary subscriber re-ask finishes the request.
        change_request(&partial, &config, |state| {
            state.requested = 0;
            true
        })
        .unwrap();
        ask_again_if_due(&partial, &config, &|_| {});
        assert_eq!(transport::list_joins(&url).unwrap().len(), 3);
    }
}
