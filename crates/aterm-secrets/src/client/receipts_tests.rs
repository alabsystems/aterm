// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A member folder whose `state.toml` holds one matching receipt of each kind, bound to its
/// configuration and identity as the recorders bind them: a signed body admitted at sequence 3
/// sampled at 1 000 ms, a 204 response at 2 000 ms and a refresh that succeeded at 3 000 ms.
fn fixture() -> (aterm_tempfile::TempDir, Paths, Config) {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let recipient = hex(&machine_identity(&paths).unwrap().recipient().0);
    let config = Config {
        source: Source::Url("http://127.0.0.1:1/v1/team".into()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "member".into(),
    };
    save_config(&paths, &config).unwrap();
    let configuration = hex(&network_observation::configuration_binding(&config));
    let digest = [7; 32];
    let receipts: aterm_toml::Table = aterm_toml::from_str(&format!(
        "[network_materialization]\nversion = 1\nsource = \"configured-url\"\nsequence = 3\n\
         sampled_at_ms = 1000\nconfiguration = \"{configuration}\"\nsource_identity = \"{}\"\n\
         recipient = \"{recipient}\"\ndigest = \"{}\"\n\
         [http204_response]\nversion = 1\nconfiguration = \"{configuration}\"\n\
         recipient = \"{recipient}\"\nresponded_at_ms = 2000\npersisted_at_ms = 2000\n\
         [network_refresh]\nversion = 1\nconfiguration = \"{configuration}\"\n\
         recipient = \"{recipient}\"\ncompleted_at_ms = 3000\noutcome = \"succeeded\"\n",
        hex(&source_identity(&config)),
        hex(&digest),
    ))
    .unwrap();
    let state = State {
        seq: 3,
        digest: Some(digest),
        source_identity: Some(source_identity(&config)),
        network_materialization: network_observation::Receipt::parse(&receipts),
        http204_response: http204_observation::Receipt::parse(&receipts),
        network_refresh: network_refresh::Receipt::parse(&receipts),
        ..State::default()
    };
    assert!(
        state.network_materialization.is_some()
            && state.http204_response.is_some()
            && state.network_refresh.is_some(),
        "CONTROL: each receipt parses"
    );
    {
        let _writer = StateLock::take(&paths).unwrap();
        save_state(&paths, &state).unwrap();
    }
    (root, paths, config)
}

/// Opens so far of `state.toml`, of the identity and of `source.toml` under `paths` (this
/// test's alone), and observations of a state lock on this thread.
fn opens(paths: &Paths) -> [usize; 4] {
    [
        fsx::reads_of(&paths.state_file()),
        fsx::reads_of(&paths.identity_file()),
        fsx::reads_of(&paths.source_file()),
        STATE_OBSERVATIONS.get(),
    ]
}

/// What `read` gave, and the [`opens`] it made.
fn opened_by<T>(paths: &Paths, read: impl FnOnce() -> T) -> (T, [usize; 4]) {
    let before = opens(paths);
    let value = read();
    let after = opens(paths);
    (value, std::array::from_fn(|at| after[at] - before[at]))
}

/// The fixture's three receipts, each as recorded.
fn as_recorded(receipts: &Receipts) -> bool {
    receipts.materialization.is_some_and(|receipt| {
        receipt.source() == VaultNetworkSource::ConfiguredUrl
            && receipt.sequence() == 3
            && receipt.sampled_at_ms() == 1_000
    }) && receipts
        .http204
        .is_some_and(|receipt| receipt.responded_at_ms() == 2_000)
        && receipts.refresh.is_some_and(|receipt| {
            receipt.completed_at_ms() == 3_000
                && receipt.outcome() == VaultNetworkRefreshOutcome::Succeeded
        })
}

/// Each call gives all three receipts from one observation of the state lock and one open each
/// of `state.toml`, the identity and `source.toml`. A writer holding the lock gives `Busy` and
/// opens nothing, a folder with no lock gives `Absent` and makes nothing, and another vault
/// configured gives a read with no receipts after opening only `source.toml`.
/// NEGATIVE CONTROL (2026-11-14): with `observe_receipts` reading each receipt as main's three
/// readers did (an observation and a read of the configuration, the identity and `state.toml`
/// of its own), `call 1: state.toml, identity, source.toml, observations` fails with
/// `[3, 3, 3, 3]` against `[1, 1, 1, 1]`. With `Busy` read as `Absent` (the HTTP 204 reader's
/// former collapse), `a writer holds the lock` fails with `Absent` against `Busy`.
#[test]
fn one_read_gives_all_three_receipts_under_one_observation() {
    let (root, paths, config) = fixture();
    for call in 1..=3 {
        let (read, opened) = opened_by(&paths, || observe_receipts(&paths, &config).unwrap());
        assert_eq!(
            opened,
            [1, 1, 1, 1],
            "call {call}: state.toml, identity, source.toml, observations"
        );
        let Observed::Read(receipts) = read else {
            panic!("call {call}: {read:?}")
        };
        assert!(as_recorded(&receipts), "call {call}: {receipts:?}");
    }

    let writer = StateLock::take(&paths).unwrap();
    let (read, opened) = opened_by(&paths, || observe_receipts(&paths, &config).unwrap());
    assert_eq!(read, Observed::Busy, "a writer holds the lock");
    assert_eq!(opened, [0, 0, 0, 1], "a busy lock opens no file");
    drop(writer);
    let Observed::Read(receipts) = observe_receipts(&paths, &config).unwrap() else {
        panic!("the writer let go")
    };
    assert!(as_recorded(&receipts), "{receipts:?}");

    let absent = Paths::locate_for_home(&root.path().join("absent"));
    assert_eq!(
        observe_receipts(&absent, &config).unwrap(),
        Observed::Absent
    );
    assert!(!absent.dir.exists(), "an absent folder is not made");

    let other = Config {
        host: "other".into(),
        ..config.clone()
    };
    let (read, opened) = opened_by(&paths, || observe_receipts(&paths, &other).unwrap());
    assert_eq!(read, Observed::Read(Receipts::default()));
    assert_eq!(
        opened,
        [0, 0, 1, 1],
        "another vault's receipts are not read"
    );
}
