// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable request to bootstrap the pinned default vault, without claiming membership.

use super::*;

const FILE: &str = "alab-join-pending";
const INTENT: &[u8] = b"ALAB-JOIN-1\n";
const NATIVE_ONLY: &[u8] = b"ALAB-JOIN-NATIVE-1\n";

fn intent(paths: &Paths) -> crate::Result<Option<bool>> {
    fsx::read_private(&paths.dir.join(FILE), NATIVE_ONLY.len())?
        .map(|bytes| match bytes.as_slice() {
            INTENT => Ok(false),
            NATIVE_ONLY => Ok(true),
            _ => Err(Error::new("unrecognized ALab bootstrap intent")),
        })
        .transpose()
}

pub(super) fn pending(paths: &Paths) -> bool {
    matches!(config(paths), Ok(None)) && intent(paths).is_ok_and(|intent| intent.is_some())
}

/// A native member proof's result. Only a verified log bundle grants membership;
/// an unanswered possession exchange remains ambiguous and must not queue another enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAdmission {
    /// No member admitted this Mac or may have signed an admission.
    None,
    /// A verified log bundle was committed locally. Materialization is read separately.
    Admitted,
    /// A member may have signed, or another process holds the proof lock.
    Uncertain,
}

/// The executable's native member transport, absent in library-only users. `current` is a
/// read-only authorization check; a proof rechecks it before publication under the vault lock.
pub type NativeBootstrap =
    fn(&Paths, &(dyn Fn() -> crate::Result<()> + Sync)) -> crate::Result<NativeAdmission>;

static NATIVE_BOOTSTRAP: std::sync::OnceLock<NativeBootstrap> = std::sync::OnceLock::new();
/// The executable's local setup-policy read; an unreadable policy holds background work.
pub type BootstrapAllowed = fn(&Paths) -> crate::Result<bool>;
static BOOTSTRAP_ALLOWED: std::sync::OnceLock<BootstrapAllowed> = std::sync::OnceLock::new();

/// Install the executable's read-only setup authority check for background bootstrap.
/// A later Stop, off choice or selected source holds continuation after a restart too.
pub fn on_bootstrap_allowed(allowed: BootstrapAllowed) {
    let _ = BOOTSTRAP_ALLOWED.set(allowed);
}

/// Check the current executable's background setup choices, without acquiring the vault lock.
/// # Errors
/// Setup was stopped, disabled, replaced, or could not be read.
pub fn check_bootstrap_allowed(paths: &Paths) -> crate::Result<()> {
    if crate::service::subscriber_turned_off(&paths.home) {
        return Err(Error::new(
            "the vault subscriber was turned off; bootstrap is held",
        ));
    }
    if BOOTSTRAP_ALLOWED
        .get()
        .is_some_and(|allowed| !allowed(paths).unwrap_or(false))
    {
        return Err(Error::new(
            "automatic vault bootstrap is held by setup's current choices",
        ));
    }
    Ok(())
}

pub(super) fn background_native(paths: &Paths) -> crate::Result<NativeAdmission> {
    native_bootstrap(paths, &|| {
        check_bootstrap_allowed(paths)?;
        if config(paths)?.is_some() {
            return Err(Error::new(
                "the vault source changed while native bootstrap ran",
            ));
        }
        Ok(())
    })
}

/// Install the executable's native-only, compiled-authority bootstrap transport once.
pub fn on_native_bootstrap(bootstrap: NativeBootstrap) {
    let _ = NATIVE_BOOTSTRAP.set(bootstrap);
}

/// Try native admission without vault HTTP or Bonjour discovery. The callback must admit
/// only the compiled default authority and commit only over an exactly unconfigured source.
/// # Errors
/// Sharing or the caller's authorization changed, this identity is isolated, or proof failed.
pub fn native_bootstrap(
    paths: &Paths,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<NativeAdmission> {
    remember_native(paths, native_attempt(paths, current)?)
}

fn native_attempt(
    paths: &Paths,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<NativeAdmission> {
    current()?;
    let Some(bootstrap) = NATIVE_BOOTSTRAP.get() else {
        return Ok(NativeAdmission::None);
    };
    refuse_lab_identity(paths)?;
    if !crate::swarm::enabled_in_home(&paths.home)? {
        return Err(Error::new("sharing is off; native bootstrap was held"));
    }
    bootstrap(paths, current)
}

fn remember_native(paths: &Paths, outcome: NativeAdmission) -> crate::Result<NativeAdmission> {
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        return Ok(outcome);
    }
    let previous = intent(paths)?;
    if outcome == NativeAdmission::Uncertain && previous.is_some() {
        fsx::write_atomic(&paths.dir.join(FILE), NATIVE_ONLY, 0o600)?;
    }
    Ok(
        if (previous == Some(true) || crate::members::join::pending_proof(paths)?)
            && outcome == NativeAdmission::None
        {
            NativeAdmission::Uncertain
        } else {
            outcome
        },
    )
}

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
    if intent(paths)?.is_some() {
        return Ok(());
    }
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
    resume_attempt(paths, join, native_attempt)
}

/// Resume a pending bootstrap of the default vault, the first step of every subscriber pass: by
/// native proof (`native`), else by the ordinary join (`join`).
///
/// Only a pending bootstrap reads [`check_bootstrap_allowed`]: an intent, no vault configured and
/// sharing on. The gate holds bootstrap work, never a pass over a vault this Mac already has, so
/// a Mac with no intent (every configured member) returns at once and takes no lock, and a
/// configured Mac's stale intent is wiped under the lock whatever the gate says. Configured Macs
/// the gate holds are ordinary: a founder made with `--no-agent` keeps a subscriber-off record,
/// and setup's choices hold automatic bootstrap where they hold Secrets or plan a vault other
/// than the default.
///
/// The look without the lock ends the pass's part here only on a clean absence. Whatever else it
/// reads, a wipe's zeroed bytes in flight among them, is read again under the lock.
///
/// # Errors
/// The intent cannot be read, the gate holds a pending bootstrap, or the bootstrap failed.
fn resume_attempt(
    paths: &Paths,
    join: impl FnOnce(&Paths, ExpectedConfig<'static>) -> crate::Result<()>,
    native: impl FnOnce(
        &Paths,
        &(dyn Fn() -> crate::Result<()> + Sync),
    ) -> crate::Result<NativeAdmission>,
) -> crate::Result<()> {
    if matches!(intent(paths), Ok(None)) {
        return Ok(());
    }
    {
        let _lock = StateLock::take(paths)?;
        if intent(paths)?.is_none() {
            return Ok(());
        }
        if config(paths)?.is_some() {
            return fsx::wipe_and_remove(&paths.dir.join(FILE));
        }
        if !crate::swarm::enabled_in_home(&paths.home)? {
            return Ok(());
        }
    }
    check_bootstrap_allowed(paths)?;
    let current = || {
        check_bootstrap_allowed(paths)?;
        if config(paths)?.is_some() {
            return Err(Error::new(
                "the vault source changed while native bootstrap ran",
            ));
        }
        if !crate::swarm::enabled_in_home(&paths.home)? {
            return Err(Error::new("sharing is off; native bootstrap was held"));
        }
        Ok(())
    };
    match remember_native(paths, native(paths, &current)?)? {
        NativeAdmission::Admitted => {
            let _lock = StateLock::take(paths)?;
            if config(paths)?.is_some() {
                fsx::wipe_and_remove(&paths.dir.join(FILE))?;
            }
            return Ok(());
        }
        NativeAdmission::Uncertain => {
            return Err(Error::new(
                "native admission is not confirmed; the subscriber retries the same proof",
            ));
        }
        NativeAdmission::None => {}
    }
    current()?;
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

    #[test]
    fn route_wake_can_attempt_native_bootstrap_with_only_durable_intent() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        assert!(!paths.state_file().exists());
        let mut asked = MembersAsked {
            network: || None,
            ..MembersAsked::default()
        };
        let attempts = std::cell::Cell::new(0);
        let native = |paths: &Paths| {
            assert!(config(paths)?.is_none());
            assert!(pending(paths));
            attempts.set(attempts.get() + 1);
            Ok(NativeAdmission::None)
        };
        ask_members_if_due_with_native(
            &paths,
            &mut asked,
            &|_| panic!("unconfigured bootstrap uses native only"),
            &native,
            &|_| {},
        );
        assert_eq!(attempts.get(), 1);
        // A real route wake resets this ordinary backoff; the proof's own durable pin/pause
        // remains authoritative before any possession, regardless of this reset.
        asked.at = None;
        asked.asks = 0;
        ask_members_if_due_with_native(
            &paths,
            &mut asked,
            &|_| panic!("no Bonjour ask"),
            &native,
            &|_| {},
        );
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn possession_retained_before_a_crash_holds_http_without_intent_conversion() {
        use crate::commons::bootstrap::Device;
        use crate::members::{join::ProofLock, joiner::Joining, proof_retry};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        machine_identity(&paths).unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let request = device.request([1; 16], [2; 32]).unwrap();
        let joining = Joining {
            device: &device,
            records_bound: records_bound(&paths, &load_records(&paths).unwrap()).unwrap(),
            node: [7; 32],
            name: "newcomer".into(),
            model: String::new(),
        };
        let proving = ProofLock::take(&paths, None).unwrap().unwrap();
        proof_retry::sending(
            &paths,
            &proving,
            &joining,
            "127.0.0.1:7",
            [3; 32],
            request.binding(),
            100,
        )
        .unwrap();
        assert_eq!(
            intent(&paths).unwrap(),
            Some(false),
            "crash before the caller records SignedMaybe"
        );
        assert!(
            resume_attempt(
                &paths,
                |_, _| panic!("retained possession must not fall through to HTTP"),
                |_, _| Ok(NativeAdmission::None),
            )
            .is_err()
        );
        let signer = crate::b64::decode_padded(crate::ALAB_SIGNER)
            .unwrap()
            .try_into()
            .unwrap();
        let config = Config {
            source: Source::Url(crate::ALAB_DEFAULT_URL.into()),
            mirrors: Vec::new(),
            name: crate::ALAB_VAULT.into(),
            signer: Some(signer),
            followed: Vec::new(),
            host: "newcomer".into(),
        };
        let _state = StateLock::take(&paths).unwrap();
        assert!(refuse_pending_native_enrollment(&paths, &config).is_err());
        proof_retry::clear(&paths, &proving).unwrap();
        assert!(refuse_pending_native_enrollment(&paths, &config).is_ok());
    }

    #[test]
    fn an_ambiguous_native_proof_never_falls_back_to_http_across_restarts() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        assert!(
            resume_attempt(
                &paths,
                |_, _| panic!("possession may have signed; no HTTP enrollment"),
                |_, current| {
                    current()?;
                    Ok(NativeAdmission::Uncertain)
                },
            )
            .is_err()
        );
        assert_eq!(intent(&paths).unwrap(), Some(true));
        prepare_alab_join_retry(&paths).unwrap();
        assert_eq!(
            intent(&paths).unwrap(),
            Some(true),
            "another setup pass cannot erase uncertainty"
        );
        let restarted = Paths::for_home(root.path());
        assert!(
            resume_attempt(
                &restarted,
                |_, _| panic!("a missing peer does not undo earlier ambiguity"),
                |_, current| {
                    current()?;
                    Ok(NativeAdmission::None)
                },
            )
            .is_err()
        );
        assert!(config(&restarted).unwrap().is_none());
        assert!(read_machine_identity(&restarted).unwrap().is_none());
    }

    #[test]
    fn native_bootstrap_is_attempted_before_any_default_vault_fetch() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        let selected = Config {
            source: Source::Url("http://127.0.0.1:7/v1/fixture".into()),
            mirrors: Vec::new(),
            name: "fixture".into(),
            signer: Some([7; 32]),
            followed: Vec::new(),
            host: "newcomer".into(),
        };
        resume_attempt(
            &paths,
            |_, _| panic!("authenticated native admission needed no vault fetch"),
            |paths, current| {
                current()?;
                // This injected proof stands for the guarded, compiled-authority writer;
                // admission/entry validity are exercised by the real protocol tests.
                save_config(paths, &selected)?;
                Ok(NativeAdmission::Admitted)
            },
        )
        .unwrap();
        assert_eq!(config(&paths).unwrap(), Some(selected));
        assert!(!pending(&paths));
        assert!(!paths.dir.join(FILE).exists());
    }

    #[test]
    fn a_later_source_choice_or_sharing_hold_prevents_native_retry_and_fetch() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        prepare_alab_join_retry(&paths).unwrap();
        let settings = paths.home.join(".config/aterm/aterm.toml");
        std::fs::write(&settings, "[swarm]\nenabled = false\n").unwrap();
        resume_attempt(
            &paths,
            |_, _| panic!("sharing off"),
            |_, _| panic!("sharing off"),
        )
        .unwrap();
        std::fs::write(&settings, "[swarm]\nenabled = true\n").unwrap();
        let selected = Config {
            source: Source::Path(root.path().join("personal.vault")),
            mirrors: Vec::new(),
            name: "personal".into(),
            signer: None,
            followed: Vec::new(),
            host: "newcomer".into(),
        };
        save_config(&paths, &selected).unwrap();
        resume_attempt(
            &paths,
            |_, _| panic!("the selected source wins"),
            |_, _| panic!("the selected source wins"),
        )
        .unwrap();
        assert_eq!(config(&paths).unwrap(), Some(selected));
        assert!(!paths.dir.join(FILE).exists());
    }

    /// The bootstrap gate holds a pending bootstrap alone, never a pass over a vault this Mac
    /// has, a stale intent beside that vault included. The churn lab's founder, made with
    /// `--no-agent`, keeps a subscriber-off record; with the gate read first, its subscriber
    /// failed every pass there and took no record in.
    #[test]
    fn the_bootstrap_gate_holds_a_pending_bootstrap_and_never_a_configured_pass() {
        let root = aterm_tempfile::tempdir().unwrap();
        let waiting = Paths::for_home(&root.path().join("waiting"));
        prepare_alab_join_retry(&waiting).unwrap();
        crate::service::record_subscriber_off(&waiting.home).unwrap();
        let held = resume_attempt(
            &waiting,
            |_, _| panic!("a held bootstrap fetches nothing"),
            |_, _| panic!("a held bootstrap proves nothing"),
        )
        .expect_err("the gate holds a pending bootstrap");
        assert!(held.to_string().contains("bootstrap is held"), "{held}");
        assert!(pending(&waiting), "the intent waits for the gate");

        let founder = Paths::for_home(&root.path().join("founder"));
        let source = root.path().join("team.vault");
        create(&founder, source.to_str().unwrap(), None, Some("founder")).unwrap();
        crate::service::record_subscriber_off(&founder.home).unwrap();
        assert!(
            check_bootstrap_allowed(&founder).is_err(),
            "the gate is held here too"
        );
        let opened = admin_open(&founder).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "note", b"inert-note")
            .unwrap();
        admin_publish(&founder, &opened, &doc).unwrap();
        let note = crate::materialize::target_path(&founder.home, "note").unwrap();
        // An intent setup wrote before this Mac had a vault, left beside the one it has now.
        fsx::write_atomic(&founder.dir.join(FILE), INTENT, 0o600).unwrap();
        for pass in ["with the stale intent", "with none"] {
            let _ = std::fs::remove_file(&note);
            subscriber_pass(&founder, &mut None, &FOUND_NONE).unwrap_or_else(|e| {
                panic!("a configured Mac's pass {pass} is not the bootstrap gate's: {e}")
            });
            assert_eq!(
                std::fs::read(&note).unwrap(),
                b"inert-note",
                "the pass {pass} read the vault"
            );
            assert!(!founder.dir.join(FILE).exists(), "the stale intent is gone");
        }
    }

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
            mirrors: Vec::new(),
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
