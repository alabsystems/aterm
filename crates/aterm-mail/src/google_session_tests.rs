// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn grant() -> Grant {
    Grant {
        email: "me@example.test".into(),
        client_id: "client".into(),
        client_secret: None,
        refresh: Secret::new("sensitive-refresh".into()),
    }
}

#[test]
fn codec_rejects_truncation_trailing_data_and_bad_accounts() {
    let bytes = encode(&grant()).unwrap();
    assert_eq!(
        decode(&bytes).unwrap().refresh.expose(),
        "sensitive-refresh"
    );
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.to_vec();
    extra.push(0);
    assert!(decode(&extra).is_err());
    assert!(email("a@b@c").is_err());
    assert_eq!(email(" Me@Example.Test ").unwrap(), "me@example.test");
}

#[cfg(unix)]
struct Home(PathBuf);
#[cfg(unix)]
impl Home {
    fn new() -> Self {
        let mut nonce = [0; 8];
        aterm_uds::rand::fill(&mut nonce).unwrap();
        let home = std::env::temp_dir().join(format!(
            "google-mail-{}-{:x}.noindex",
            std::process::id(),
            u64::from_le_bytes(nonce)
        ));
        Self(home)
    }
    fn session(&self) -> Session {
        save(&self.0, &grant()).unwrap();
        Session::load(&self.0, "me@example.test").unwrap().unwrap()
    }
}
#[cfg(unix)]
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn tokens() -> Tokens {
    Tokens {
        access_token: Secret::new("access".into()),
        refresh_token: Some(Secret::new("rotated".into())),
        expires_in: 3600,
    }
}

#[cfg(unix)]
#[test]
fn renewal_caches_access_and_persists_rotation_privately() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let mut session = home.session();
    assert!(session.renew_with(|_| Ok(tokens())).unwrap());
    assert!(
        !session
            .renew_with(|_| panic!("valid token must be reused"))
            .unwrap()
    );
    assert_eq!(session.generation(), 1);
    let path = location(&home.0, "me@example.test");
    assert_eq!(
        load_grant(&home.0, &path)
            .unwrap()
            .unwrap()
            .refresh
            .expose(),
        "rotated"
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(7).any(|part| part == b"rotated"));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(accounts(&home.0), vec!["me@example.test"]);
}

#[cfg(unix)]
#[test]
fn rejected_grants_stop_retries_but_transport_errors_do_not() {
    let home = Home::new();
    let mut session = home.session();
    assert!(
        session
            .renew_with(|_| Err(Error {
                kind: ErrorKind::Transport
            }))
            .is_err()
    );
    assert!(
        session
            .renew_with(|_| Err(Error {
                kind: ErrorKind::Rejected
            }))
            .is_err()
    );
    assert!(
        session
            .renew_with(|_| panic!("rejected grant must not retry"))
            .is_err()
    );
    assert!(session.authentication().is_none());

    // Re-registering a fresh refresh token on disk lifts the rejection for a live session.
    register(
        &home.0,
        "me@example.test",
        "client",
        None,
        Secret::new("fresh-refresh".into()),
    )
    .unwrap();
    assert!(
        session
            .renew_with(|latest| {
                assert_eq!(latest.refresh.expose(), "fresh-refresh");
                Ok(tokens())
            })
            .unwrap()
    );
    assert!(session.authentication().is_some());
}

#[cfg(unix)]
#[test]
fn failed_persistence_retries_without_rotating_again() {
    let home = Home::new();
    let mut session = home.session();
    let directory = home.0.join(".config/aterm/mail/google");
    let saved = directory.with_extension("saved");
    assert_eq!(
        session
            .renew_with(|_| {
                std::fs::rename(&directory, &saved).unwrap();
                std::fs::write(&directory, b"blocked").unwrap();
                Ok(tokens())
            })
            .unwrap_err()
            .kind,
        ErrorKind::Storage
    );
    assert!(session.authentication().is_none());
    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(saved, directory).unwrap();
    assert!(
        session
            .renew_with(|_| panic!("pending rotation must be saved first"))
            .unwrap()
    );
    assert_eq!(session.generation(), 1);
}

#[cfg(unix)]
#[test]
fn shared_session_and_independent_readers_only_lock_during_renewal() {
    let home = Home::new();
    save(&home.0, &grant()).unwrap();
    let first = shared(&home.0, "me@example.test").unwrap().unwrap();
    let second = shared(&home.0, "ME@example.test").unwrap().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    let mut independent = Session::load(&home.0, "me@example.test").unwrap().unwrap();
    let held = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
    assert_eq!(
        independent
            .renew_with(|_| panic!("lease is held"))
            .unwrap_err()
            .kind,
        ErrorKind::Storage
    );
    let started = Instant::now();
    assert_eq!(
        independent
            .renew_until(started + Duration::from_millis(5))
            .unwrap_err()
            .kind,
        ErrorKind::Transport
    );
    drop(held);
    first.lock().unwrap().renew_with(|_| Ok(tokens())).unwrap();
    independent
        .renew_with(|latest| {
            assert_eq!(latest.refresh.expose(), "rotated");
            Ok(tokens())
        })
        .unwrap();
    let released = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
    drop(released);
    let path = location(&home.0, "me@example.test");
    let wrong = location(&home.0, "other@example.test");
    std::fs::copy(&path, &wrong).unwrap();
    assert!(load_grant(&home.0, &wrong).is_err());
    drop((first, second));
    assert!(Session::load(&home.0, "me@example.test").unwrap().is_some());
}
/// A RENEWAL RETRIES A BUSY GRANT STORE FOR ITS OWN CAP AT MOST (KI-1105-14): handed a day
/// (what was left of a long sign-in) while another process holds the grant store, a renewal
/// gives up [`BUSY_RETRY`] after it began, and by a shorter end at that end, on a clock the
/// test moves. Once the store is free its request is handed the caller's end, which
/// `refresh_until` caps itself.
#[cfg(unix)]
#[test]
fn a_renewal_retries_a_busy_grant_store_for_at_most_its_own_cap() {
    let home = Home::new();
    let base = Instant::now();
    let clock = std::cell::Cell::new(base);
    let budget = base + Duration::from_secs(86_400);
    let short = base + Duration::from_secs(5);
    let mut busy = home.session();
    let held = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
    for (end, gives_up) in [(budget, base + BUSY_RETRY), (short, short)] {
        clock.set(base);
        let error = busy
            .renew_until_on(
                end,
                || clock.get(),
                |_, _| panic!("the grant store is held"),
                |pause| {
                    let next = clock.get() + pause;
                    assert!(next <= gives_up, "a busy grant store held the renewal on");
                    clock.set(next);
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Transport);
        assert_eq!(clock.get(), gives_up, "it retried until then");
    }
    drop(held);
    for end in [short, budget] {
        clock.set(base);
        let mut free = home.session();
        let mut handed = None;
        let renewed = free
            .renew_until_on(
                end,
                || clock.get(),
                |_, end| {
                    handed = Some(end);
                    Ok(tokens())
                },
                |_| panic!("a free grant store needs no pause"),
            )
            .unwrap();
        assert!(renewed);
        assert_eq!(handed, Some(end), "the request is handed its caller's end");
    }
}

#[cfg(unix)]
#[test]
fn renewal_refreshes_when_wall_clock_advances_across_sleep() {
    let home = Home::new();
    let mut session = home.session();
    let now = std::cell::Cell::new(1_800_000_000_u64);
    assert!(
        session
            .renew_with_clock(|| Some(now.get()), |_| Ok(tokens()))
            .unwrap()
    );
    assert_eq!(session.generation(), 1);

    // Before `3600 - 60 = 3540` wall-clock seconds, cached token is reused.
    now.set(1_800_003_539);
    assert!(
        !session
            .renew_with_clock(|| Some(now.get()), |_| panic!("must reuse before margin"))
            .unwrap()
    );
    assert_eq!(session.generation(), 1);

    // Once wall-clock time crosses the margin (for example across macOS sleep while Instant
    // barely moved), `renew_with_clock` refreshes the access token and bumps `generation`.
    now.set(1_800_003_540);
    assert!(
        session
            .renew_with_clock(
                || Some(now.get()),
                |_| Ok(Tokens {
                    access_token: Secret::new("access-2".into()),
                    refresh_token: None,
                    expires_in: 3600,
                })
            )
            .unwrap()
    );
    assert_eq!(session.generation(), 2);
}

#[cfg(unix)]
#[test]
fn renewal_keeps_rotation_across_missing_or_shifted_response_clocks() {
    for timestamp in [1_800_000_000, u64::MAX - 10] {
        for response_clock in [
            None,
            Some(timestamp - 100),
            Some(timestamp.saturating_add(100)),
        ] {
            let home = Home::new();
            let mut session = home.session();
            let clock = std::cell::Cell::new(Some(timestamp));
            assert!(
                session
                    .renew_with_clock(
                        || clock.get(),
                        |_| {
                            clock.set(response_clock);
                            Ok(tokens())
                        },
                    )
                    .unwrap()
            );
            assert_eq!(session.generation(), 1);
            let base = response_clock.map_or(timestamp, |observed| observed.min(timestamp));
            assert_eq!(session.expires, base.saturating_add(3540));
            assert_eq!(
                load_grant(&home.0, &location(&home.0, "me@example.test"))
                    .unwrap()
                    .unwrap()
                    .refresh
                    .expose(),
                "rotated"
            );
            let lease = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
            drop(lease);
            if response_clock.is_none() {
                assert_eq!(
                    session
                        .renew_with_clock(|| clock.get(), |_| panic!("no request without a clock"))
                        .unwrap_err()
                        .kind,
                    ErrorKind::Storage
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn explicit_reenrollment_releases_a_rejected_session_without_retrying_old_credentials() {
    let home = Home::new();
    let mut session = home.session();
    assert!(
        session
            .renew_with(|_| Err(Error {
                kind: ErrorKind::Rejected
            }))
            .is_err()
    );
    assert!(
        session
            .renew_with(|_| panic!("unchanged rejected grant must not retry"))
            .is_err()
    );
    register(
        &home.0,
        "me@example.test",
        "new-client",
        None,
        Secret::new("new-refresh".into()),
    )
    .unwrap();
    assert!(
        session
            .renew_with(|grant| {
                assert_eq!(grant.client_id, "new-client");
                assert_eq!(grant.refresh.expose(), "new-refresh");
                Ok(tokens())
            })
            .unwrap()
    );
}
