// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct LaunchFixture;
impl sealed::LaunchDoor for LaunchFixture {}

#[test]
fn the_fourth_launch_mint_is_refused_even_when_earlier_tokens_were_dropped() {
    let mut launch = Launch::launch_use(LaunchFixture);
    for index in 0..3 {
        assert!(
            Use::launch_read_use(
                &mut launch,
                Account::in_home(
                    Read::CodexEndpoint,
                    Path::new("isolated"),
                    Some(&format!("row-{index}"))
                )
            )
            .is_some()
        );
    }
    assert!(
        Use::launch_read_use(
            &mut launch,
            Account::in_home(Read::CodexEndpoint, Path::new("isolated"), Some("fourth"))
        )
        .is_none()
    );
}

#[test]
fn another_account_or_provider_cannot_spend_a_token_or_record_a_probe() {
    for account in [
        Account::in_home(Read::CodexEndpoint, Path::new("other-home"), Some("first")),
        Account::in_home(
            Read::CodexEndpoint,
            Path::new("first-home"),
            Some("other-row"),
        ),
        Account::in_home(Read::AgyUsage, Path::new("first-home"), Some("first")),
    ] {
        let (result, probes) = captured(|| {
            Use::fixture(Read::CodexEndpoint, Path::new("first-home"), Some("first"))
                .begin(account, 17, None)
        });
        assert!(result.is_err());
        assert!(probes.is_empty());
    }
}

#[test]
fn every_authorized_read_records_one_probe_with_its_own_reason() {
    for read in [
        Read::ClaudePanel,
        Read::KimiPanel,
        Read::CodexEndpoint,
        Read::AgyUsage,
        Read::MuseUsage,
    ] {
        let running = Running {
            account: Account::in_home(read, Path::new("isolated"), Some("row")),
        };
        let (result, probes) = captured(|| {
            Use::session_read_use(running).begin(
                Account::in_home(read, Path::new("isolated"), Some("row")),
                17,
                None,
            )
        });
        assert!(result.is_ok());
        assert_eq!(
            probes,
            vec![ProbeEvent {
                at: 17,
                read,
                account: Some("row".into()),
                login: None,
                why: InUse::LiveSession
            }]
        );
    }
}

#[test]
fn replay_claims_cannot_be_used_as_live_authority() {
    let (result, probes) = captured(|| {
        Use::replay_use().begin(
            Account::in_home(Read::ClaudePanel, Path::new(""), None),
            17,
            None,
        )
    });
    assert!(result.is_err());
    assert!(probes.is_empty());
}

#[test]
fn the_codex_read_refuses_another_home_before_recording_or_reading_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let (reading, probes) = captured(|| {
        super::super::codex_app_login::read_login_in_home(
            &second,
            root.path(),
            std::time::Duration::ZERO,
            17,
            None,
            Use::fixture(Read::CodexEndpoint, &first, Some("first")),
        )
    });
    assert_eq!(reading, super::super::codex_app_login::Reading::Unread);
    assert!(probes.is_empty());
    let (_, probes) = captured(|| {
        super::super::codex_app_login::read_login_in_home(
            &second,
            root.path(),
            std::time::Duration::ZERO,
            17,
            None,
            Use::fixture(Read::CodexEndpoint, &second, Some("second")),
        )
    });
    assert_eq!(
        probes.len(),
        1,
        "CONTROL: an authorized attempt records its probe even when credentials are absent"
    );
}

fn event() -> ProbeEvent {
    ProbeEvent {
        at: 1,
        read: Read::CodexEndpoint,
        account: Some("codex-a".to_owned()),
        login: None,
        why: InUse::Launching,
    }
}

/// CONTROL: a note outside `captured` is not in the next capture, so a capture sees only its
/// own body's reads.
#[test]
fn a_capture_holds_only_what_its_body_reported() {
    note(&event());
    let ((), seen) = captured(|| note(&event()));
    assert_eq!(seen, vec![event()]);
    let ((), none) = captured(|| ());
    assert!(none.is_empty());
}
