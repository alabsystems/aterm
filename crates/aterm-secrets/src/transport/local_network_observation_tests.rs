// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Swapping the actual scratch config during the request cannot substitute a new endpoint
/// for the one this pass captured. The injected send performs no network or permission IO.
#[cfg(unix)]
#[test]
fn a_config_swap_cannot_substitute_the_completed_requests_scope() {
    let world = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(world.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    let file = paths.dir.join("source.toml");
    let remote = "url = 'https://vault.example/v1/alab'\nname = 'alab'\n";
    let local = "url = 'http://vault.local/v1/alab'\nname = 'alab'\n";
    let path = "path = '/synthetic/vault'\nname = 'alab'\n";
    for (before, after, expected) in [
        (remote, local, None),
        (path, local, None),
        (local, remote, Some(LocalNetworkHeard::Reached)),
    ] {
        crate::fsx::write_atomic(&file, before.as_bytes(), 0o600).unwrap();
        let original = crate::client::config(&paths).unwrap().unwrap();
        let mut requests = 0;
        let (sent, heard) = with_local_network_heard(&original.source, || {
            requests += 1;
            crate::fsx::write_atomic(&file, after.as_bytes(), 0o600).unwrap();
            Ok(())
        });
        sent.unwrap();
        assert_eq!(requests, 1);
        assert_eq!(heard, expected);
        let replacement = crate::client::config(&paths).unwrap().unwrap();
        assert_ne!(original.source, replacement.source);
        // The former post-sync reread selects the opposite scope on these same bytes.
        let (_, late) = with_local_network_heard(&replacement.source, || Ok(()));
        assert_ne!(late, heard);
        assert_eq!(std::fs::read(&file).unwrap(), after.as_bytes());
    }
}

fn failure(phase: aterm_http::Phase, error: aterm_http::Error) -> aterm_http::SendError {
    aterm_http::SendError { phase, error }
}

#[test]
fn only_an_actual_local_response_or_resolver_refusal_is_heard() {
    let local = Source::Url("https://VAULT.LOCAL./v1/alab".to_owned());
    // Any actual HTTP status is reachability, even when later vault classification fails.
    for status in [200, 204, 404, 503] {
        let (sent, heard) = with_local_network_heard(&local, || Ok(status));
        assert_eq!(sent.unwrap(), status);
        assert_eq!(heard, Some(LocalNetworkHeard::Reached));
    }
    let refused = || {
        failure(
            aterm_http::Phase::NotSent,
            aterm_http::Error::Io(std::io::Error::other(aterm_http::LOCAL_NETWORK_REFUSED)),
        )
    };
    assert_eq!(
        with_local_network_heard::<()>(&local, || Err(refused())).1,
        Some(LocalNetworkHeard::Refused)
    );
    for source in [
        Source::Url("https://vault.example/v1/alab".to_owned()),
        Source::Url("http://10.0.0.2/v1/alab".to_owned()),
        Source::Path("/synthetic/vault.local".into()),
    ] {
        assert_eq!(with_local_network_heard(&source, || Ok(())).1, None);
        assert_eq!(
            with_local_network_heard::<()>(&source, || Err(refused())).1,
            None
        );
    }
    for error in [
        aterm_http::Error::Io(std::io::ErrorKind::TimedOut.into()),
        aterm_http::Error::Io(std::io::ErrorKind::ConnectionRefused.into()),
        aterm_http::Error::Protocol(aterm_http::LOCAL_NETWORK_REFUSED.to_owned()),
        aterm_http::Error::Invalid(aterm_http::LOCAL_NETWORK_REFUSED.to_owned()),
    ] {
        assert_eq!(
            with_local_network_heard::<()>(&local, || {
                Err(failure(aterm_http::Phase::Sent, error))
            })
            .1,
            None
        );
    }
    assert_eq!(
        with_local_network_heard::<()>(&local, || {
            let mut refused_after_send = refused();
            refused_after_send.phase = aterm_http::Phase::Sent;
            Err(refused_after_send)
        })
        .1,
        None
    );
    // Body collection may fail after status and headers parsed. The actual Answered phase
    // still witnesses reachability, and takes precedence over any quoted refusal marker.
    for error in [
        aterm_http::Error::TooLarge { limit: 32 },
        aterm_http::Error::Io(std::io::ErrorKind::UnexpectedEof.into()),
        aterm_http::Error::Io(std::io::ErrorKind::TimedOut.into()),
        aterm_http::Error::Io(std::io::Error::other(aterm_http::LOCAL_NETWORK_REFUSED)),
    ] {
        let (sent, heard) = with_local_network_heard::<()>(&local, || {
            Err(failure(aterm_http::Phase::Answered, error))
        });
        assert_eq!(sent.unwrap_err().phase, aterm_http::Phase::Answered);
        assert_eq!(heard, Some(LocalNetworkHeard::Reached));
    }
}

#[test]
fn an_unanswered_long_poll_is_not_a_local_network_answer() {
    let url = "http://vault.local/v1/alab";
    let (sent, heard) = with_local_network_heard(&Source::Url(url.to_owned()), || {
        Err(failure(
            aterm_http::Phase::Sent,
            aterm_http::Error::Io(std::io::ErrorKind::TimedOut.into()),
        ))
    });
    let fetched = classify_get_response(sent, url, Some(9), None)
        .unwrap()
        .fetched()
        .unwrap();
    assert_eq!(fetched.fetched, Fetched::NoNewer(None));
    assert!(fetched.http204.is_none());
    assert_eq!(heard, None);
}
