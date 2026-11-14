// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStrExt as _;

fn starter(env: Vec<(String, String)>, unset: Vec<&'static str>) -> worker::Starter {
    worker::Starter {
        exe: "/not-executed".into(),
        args: Ok(Vec::new()),
        env,
        unset,
    }
}

#[test]
fn malformed_and_ambiguous_environment_entries_are_refused() {
    for words in [
        vec![b"=empty-name".as_slice()],
        vec![b"missing-equals".as_slice()],
        vec![b"A=first".as_slice(), b"A=second".as_slice()],
        vec![b"A=value\0suffix".as_slice()],
    ] {
        assert_eq!(entries(words.into_iter()), Err(Error::Unavailable));
    }
}

#[test]
fn native_settings_are_raw_data_and_every_utf8_setting_is_admitted() {
    let process = Process::observe(std::process::id()).unwrap();
    let mut environment = OwnedLaunchEnvironment::capture(&process).unwrap();
    // Private fixture data only; public creation always reads the actual kernel.
    environment.values = entries(
        [
            b"NAME=source".as_slice(),
            b"REMOVE=old".as_slice(),
            b"RAW=kept\xff".as_slice(),
        ]
        .into_iter(),
    )
    .unwrap();
    let original = starter(vec![("NAME".into(), "override".into())], vec!["REMOVE"]);
    let admitted = environment.prepare(original).unwrap();
    assert_eq!(admitted.env, vec![("NAME".into(), "override".into())]);
    let mut command = aterm_uds::spawnfd::command("/not-executed");
    let selected_environment = OwnedLaunchEnvironment {
        process: environment.process.clone(),
        identity: environment.identity,
        values: environment.values.clone(),
    }
    .admit(&admitted)
    .unwrap();
    selected_environment
        .install(&mut command, &admitted)
        .unwrap();
    let mut changed = admitted.clone();
    changed.env.push(("HOME".into(), "/different-home".into()));
    assert_eq!(selected_environment.check(&changed), Err(Error::Denied));
    let mut armed = admitted.clone();
    armed
        .env
        .push((LAUNCH_ENV.into(), "native-new-constructor".into()));
    assert_eq!(selected_environment.check(&armed), Ok(()));
    let selected: BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_os_string(), value.to_os_string())))
        .collect();
    assert_eq!(
        selected[std::ffi::OsStr::new("RAW")].as_bytes(),
        b"kept\xff"
    );
    assert!(!selected.contains_key(std::ffi::OsStr::new("REMOVE")));
    assert_eq!(
        format!("{environment:?}"),
        "OwnedLaunchEnvironment(redacted)"
    );
    // A UTF-8 inherited selector cannot be hidden from the admission view.
    environment
        .values
        .insert("CODEX_AUTH_PATH".into(), "/unadmitted/auth.json".into());
    assert_eq!(environment.check(&admitted), Err(Error::Denied));
    // Existing String-based worker auth policy cannot inspect non-UTF-8 selectors.
    environment.values.insert(
        "CODEX_AUTH_PATH".into(),
        OsString::from_vec(b"/auth\xff".to_vec()),
    );
    assert!(matches!(
        environment.prepare(admitted),
        Err(Error::Unsupported)
    ));
}

#[test]
fn mismatched_source_identity_is_not_a_current_environment_producer() {
    let process = Process::observe(std::process::id()).unwrap();
    let mut environment = OwnedLaunchEnvironment::capture(&process).unwrap();
    environment.process.birth = environment.process.birth.wrapping_add(1);
    assert_eq!(environment.current(), Err(Error::Stale));
}

#[test]
fn prepared_worker_diagnostics_do_not_print_environment_or_argument_values() {
    let mut launch = starter(
        vec![(
            "PRIVATE_SETTING".into(),
            "environment-value-must-stay-private".into(),
        )],
        Vec::new(),
    );
    launch.args = Ok(vec!["argument-value-must-stay-private".into()]);
    let shown = format!("{:?}", worker::Admitted::assumed(launch));
    assert!(!shown.contains("environment-value-must-stay-private"));
    assert!(!shown.contains("argument-value-must-stay-private"));
    assert!(!shown.contains("PRIVATE_SETTING"));
}
