// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn discovery_and_rejected_browser_spawn_reach_a_hostless_outcome() {
    use std::os::unix::fs::PermissionsExt as _;

    note_met(MetWord::BotCheck, Some("old.example"));
    assert!(browser_for_start(None).is_err());
    assert_eq!(
        take_met(),
        Some(super::super::open_url::Met::new(
            MetWord::BrowserStart,
            None
        ))
    );

    let tmp = aterm_tempfile::tempdir().unwrap();
    let executable = tmp.path().join("private-launch-detail");
    std::fs::write(&executable, b"not executable").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    let browser = browser_for_start(find_headless_browser_with(
        None,
        &|name| (name == BROWSER_BIN_ENV).then(|| executable.as_os_str().to_owned()),
        false,
    ))
    .unwrap();
    note_met(MetWord::BlockPage, Some("old.example"));
    let cmd = headless_browser_command(&browser, tmp.path(), &|_| None);
    let (cmd_read, _cmd_write) = aterm_uds::pipe().unwrap();
    let (_out_read, out_write) = aterm_uds::pipe().unwrap();
    let error = start_browser(cmd, cmd_read.into(), out_write.into()).unwrap_err();
    assert!(!error.contains("private"));
    let expected = super::super::open_url::Met::new(MetWord::BrowserStart, None);
    let produced = take_met().unwrap();
    assert_eq!(produced, expected);
    let path = tmp.path().join("outcome");
    super::super::open_url::write_outcome(&path, &produced);
    let retained = super::super::open_url::read_outcome(&path).unwrap();
    assert_eq!(retained, expected);
    let history = super::super::sign_in_ladder::how_it_ended(
        super::super::cli::Renewal::Failed,
        None,
        Some(&retained),
    );
    assert!(history.ends_with(&expected.words()));
    assert!(!history.contains("private") && !history.contains("old.example"));
    assert_eq!(take_met(), None);
}

#[test]
fn a_new_headless_flow_clears_an_old_outcome_before_url_validation() {
    note_met(MetWord::BotCheck, Some("old.example"));
    let result = authorize_with_headless_browser_config_and_credential(
        "not-an-authorization-url",
        None,
        None,
        None,
        Duration::ZERO,
        None,
    );
    assert!(result.is_err());
    assert_eq!(take_met(), None);
}
