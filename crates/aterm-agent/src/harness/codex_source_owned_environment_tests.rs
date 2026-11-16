// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStringExt as _;

fn starter(env: Vec<(String, String)>, unset: Vec<&'static str>) -> worker::Starter {
    worker::Starter {
        exe: "/not-executed".into(),
        args: Ok(Vec::new()),
        env,
        unset,
    }
}

fn entries<'a>(
    words: impl Iterator<Item = &'a [u8]>,
) -> Result<BTreeMap<OsString, OsString>, Error> {
    let settings = words
        .map(|word| {
            let split = word.iter().position(|b| *b == b'=').ok_or(Error::Denied)?;
            Ok((
                std::ffi::OsString::from_vec(word[..split].to_vec()),
                std::ffi::OsString::from_vec(word[split + 1..].to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    OwnedLaunchEnvironment::from_snapshot(&Process::observe(std::process::id()).unwrap(), settings)
        .map(|env| env.values)
}
#[test]
fn malformed_and_ambiguous_environment_entries_are_refused() {
    for words in [
        vec![b"=empty-name".as_slice()],
        vec![b"missing-equals".as_slice()],
        vec![b"A=first".as_slice(), b"A=second".as_slice()],
        vec![b"A=value\0suffix".as_slice()],
    ] {
        assert_eq!(entries(words.into_iter()), Err(Error::Denied));
    }
}

#[test]
fn sealed_native_arguments_reach_the_child_without_text_or_shell_conversion() {
    let process = Process::observe(std::process::id()).unwrap();
    let source = OwnedLaunchEnvironment::from_snapshot(&process, Vec::new()).unwrap();
    let mut starter = starter(Vec::new(), Vec::new());
    starter.exe = "/usr/bin/printf".into();
    let arguments = vec![
        "%s".into(),
        OsString::from_vec(b"path with spaces/\xff".to_vec()),
    ];
    let selected = source.admit_arguments(&starter, arguments.clone()).unwrap();
    let mut command = selected
        .command(
            &starter,
            "1",
            std::path::Path::new("/"),
            aterm_uds::spawnfd::Stdio::null(),
            false,
        )
        .unwrap();
    assert_eq!(
        command.command.get_args().collect::<Vec<_>>(),
        arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>()
    );
    let output = command.command.spawn().unwrap().wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"path with spaces/\xff");
    starter.args = Ok(vec!["changed-after-sealing".into()]);
    assert_eq!(selected.check(&starter), Err(Error::Denied));
    let source = OwnedLaunchEnvironment::from_snapshot(&process, Vec::new()).unwrap();
    assert!(matches!(
        source.admit_arguments(
            &starter,
            vec![OsString::from_vec(b"bad\0argument".to_vec())]
        ),
        Err(Error::Denied)
    ));
}

#[test]
fn backend_setting_comparison_excludes_only_the_replaced_native_constructor_nonce() {
    let process = Process::observe(std::process::id()).unwrap();
    let mut original = OwnedLaunchEnvironment::from_snapshot(&process, Vec::new()).unwrap();
    original.values =
        entries([b"HOME=/source".as_slice(), b"RAW=kept\xff".as_slice()].into_iter()).unwrap();
    let mut next = OwnedLaunchEnvironment {
        process: original.process.clone(),
        identity: original.identity,
        values: original.values.clone(),
    };
    original
        .values
        .insert(LAUNCH_ENV.into(), "original-native-nonce".into());
    next.values
        .insert(LAUNCH_ENV.into(), "different-native-nonce".into());
    assert!(original.same_settings(&next));
    next.values.insert("HOME".into(), "/other".into());
    assert!(!original.same_settings(&next));
    next.values.insert("HOME".into(), "/source".into());
    next.values.insert("EXTRA".into(), "value".into());
    assert!(!original.same_settings(&next));
    next.values.remove(std::ffi::OsStr::new("EXTRA"));
    next.values.insert(
        "RAW".into(),
        std::ffi::OsString::from_vec(b"changed\xff".to_vec()),
    );
    assert!(!original.same_settings(&next));
}

#[test]
fn native_settings_are_raw_data_and_every_utf8_setting_is_admitted() {
    let process = Process::observe(std::process::id()).unwrap();
    let mut environment = OwnedLaunchEnvironment::from_snapshot(&process, Vec::new()).unwrap();
    // Explicit startup DATA, not a claim of kernel environment observation.
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
    let mut environment = OwnedLaunchEnvironment::from_snapshot(&process, Vec::new()).unwrap();
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
