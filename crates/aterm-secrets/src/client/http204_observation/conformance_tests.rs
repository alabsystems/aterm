// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{derive::Model, interp::State as ModelState, verify};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    LegacyNoNewer,
    ReadClock,
    ReloadClock,
    IgnoreContext,
}

fn subscriber(paths: &Paths) -> Option<String> {
    read_identity(paths)
        .unwrap()
        .map(|identity| identity.recipient().to_bech32())
}

fn changed_recipient(paths: &Paths) -> Option<[u8; 32]> {
    let identity = Identity::from_secret_bytes([231; 32]);
    fsx::write_atomic(
        &paths.identity_file(),
        identity.to_bech32().as_slice(),
        0o600,
    )
    .unwrap();
    Some(identity.recipient().0)
}

fn transport_result(
    kind: i64,
    f: &super::tests::Fixture,
) -> (Option<transport::NoContentReply>, Option<Fetched>) {
    let Source::Url(url) = &f.config.source else {
        panic!("configured URL")
    };
    let subscriber = subscriber(&f.paths);
    if kind == 3 || kind == 4 {
        let failure = aterm_http::SendError {
            phase: if kind == 3 {
                aterm_http::Phase::Sent
            } else {
                aterm_http::Phase::Answered
            },
            error: aterm_http::Error::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        };
        let observed = transport::test_classify_get_response(
            Err(failure),
            url,
            Some(3),
            subscriber.as_deref(),
        );
        return match observed
            .and_then(|answer| answer.fetched().map_err(|why| transport::Unheard { why }))
        {
            Ok(observed) => (observed.http204, Some(observed.fetched)),
            Err(_) => (None, None),
        };
    }
    if (5..=8).contains(&kind) {
        let (status, reason) = match kind {
            5 => (200, "OK"),
            6 => (404, "Not Found"),
            7 => (302, "Found"),
            _ => (500, "Internal Server Error"),
        };
        let (url, server) = super::tests::stub(
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n\r\n").into_bytes(),
        );
        if kind == 5 || kind == 6 {
            let result = transport::fetch_within_as_observed(
                &url,
                Some(3),
                aterm_spec::wait::MUST_HAPPEN,
                subscriber.as_deref(),
            );
            server.join().unwrap().unwrap();
            let observed = result.unwrap();
            let expected = if kind == 5 {
                Fetched::Body(Vec::new())
            } else {
                Fetched::Missing
            };
            assert_eq!(observed.fetched, expected);
            assert!(observed.http204.is_none());
            return (observed.http204, Some(observed.fetched));
        }
        // Observe the completed status before classification: an unrelated lost request
        // must not satisfy a refusal case merely because neither outcome is an HTTP 204.
        // The public fetch flattens Foreign and Unheard into the same Error, so these two
        // cases exercise its production classifier before that information is discarded.
        let client = aterm_http::Client::new(
            aterm_http::Trust::PlatformVerifier,
            aterm_http::ProxyMode::Direct,
            aterm_spec::wait::MUST_HAPPEN,
        );
        let target = format!("{url}?after=3");
        let mut request = client.get(&target);
        if let Some(subscriber) = subscriber.as_deref() {
            request = request.header(transport::SUBSCRIBER_HEADER, subscriber);
        }
        let response = request.send_phased(&[]);
        server.join().unwrap().unwrap();
        let response = response.expect("the configured HTTP status");
        assert_eq!(response.status(), status);
        let result = transport::test_classify_get_response(
            Ok(response),
            &url,
            Some(3),
            subscriber.as_deref(),
        );
        return match kind {
            7 => {
                assert!(result.unwrap().fetched().is_err(), "a redirect is foreign");
                (None, None)
            }
            _ => {
                assert!(
                    result.unwrap_err().why.0.contains("500"),
                    "the server answered HTTP 500"
                );
                (None, None)
            }
        };
    }
    if kind == 9 || kind == 11 {
        let fetched = if kind == 9 {
            Fetched::NoNewer(None)
        } else {
            Fetched::Body(b"cached local bytes".to_vec())
        };
        let raw = transport::ObservedAnswer::from(transport::Answer::Fetched(fetched))
            .fetched()
            .unwrap();
        return (raw.http204, Some(raw.fetched));
    }
    if kind == 10 {
        let path = f.paths.home.join("scratch-cache");
        fsx::write_atomic(&path, b"local cached bytes", 0o600).unwrap();
        let source = Source::Path(path);
        let observed = transport::fetch_as_observed(
            &source,
            Some(3),
            Duration::from_secs(60),
            subscriber.as_deref(),
        )
        .unwrap();
        let mut local = f.config.clone();
        local.source = source;
        assert!(Request::start(&f.paths, &local, Some(3), subscriber.as_deref()).is_none());
        return (observed.http204, Some(observed.fetched));
    }
    (Some(f.reply), Some(Fetched::NoNewer(None)))
}

pub(crate) fn http204_transition(before: &ModelState, action: &str, kind: i64) -> ModelState {
    transition_with(before, action, kind, Fault::None)
}

fn transition_with(before: &ModelState, action: &str, kind: i64, fault: Fault) -> ModelState {
    let etag = match kind {
        1 => None,
        2 => Some("not-a-sequence"),
        _ => Some("\"3\""),
    };
    let f = super::tests::fixture(etag, kind != 21);
    assert_ne!(f.reply.responded_at_ms(), 1);
    let mut config = f.config.clone();
    let mut recipient = f.request.recipient;
    let mut state = load_state(&f.paths).unwrap();
    let mut previous_config = config.clone();
    if kind == 19 {
        previous_config.host.push_str("-previous-context");
    }
    state.http204_response = Some(Receipt {
        configuration: network_observation::configuration_binding(&previous_config),
        recipient,
        responded_at_ms: 1,
        persisted_at_ms: 1_000,
    });
    if kind <= 2 || kind > 20 {
        state.http204_response = None;
    }
    if kind == 22 {
        state.membership = Stored::Revoked;
        state.failed = true;
        state.pending.insert("unapplied".into(), "00".into());
    }
    let (reply, legacy) = transport_result(kind, &f);
    if (3..=11).contains(&kind) {
        assert!(reply.is_none(), "CONTROL: no actual204 evidence");
    }
    let mut request = f.request;
    let mut record_config = config.clone();
    let subscriber = subscriber(&f.paths);
    match kind {
        12 => {
            let Source::Url(url) = &config.source else {
                panic!("URL")
            };
            record_config.source = Source::Url(format!("{url}/other"));
            request =
                Request::start(&f.paths, &record_config, Some(3), subscriber.as_deref()).unwrap();
        }
        13 => request = Request::start(&f.paths, &config, Some(4), subscriber.as_deref()).unwrap(),
        14 => {
            request = Request::start(&f.paths, &config, Some(3), Some("other-subscriber")).unwrap()
        }
        15 => {
            config.host.push_str("-changed");
            record_config = config.clone();
        }
        16 => recipient = changed_recipient(&f.paths),
        _ => {}
    }
    let persisted_at_ms = match kind {
        17 => 60_999,
        19 => 1_001,
        20 => 999,
        _ => 61_000,
    };
    let mut wrote = reply.as_ref().is_some_and(|reply| {
        record(
            &mut state,
            &record_config,
            recipient,
            &request,
            reply,
            persisted_at_ms,
        )
    });
    if fault == Fault::LegacyNoNewer {
        // Unsafe former inference: a synthetic timeout or raw replay that says
        // NoNewer is mistaken for a newly received HTTP response.
        assert!(matches!(legacy, Some(Fetched::NoNewer(_))));
        state.http204_response = Some(Receipt {
            configuration: network_observation::configuration_binding(&config),
            recipient,
            responded_at_ms: f.reply.responded_at_ms(),
            persisted_at_ms,
        });
        wrote = true;
    }
    assert!(
        state.network_materialization.is_none(),
        "route evidence cannot mint a signed materialization receipt"
    );
    let mut shown = 0;
    let mut returned_at_ms = None;
    let phase = match action {
        "Receive" => 1,
        "Read" => 2,
        "Reload" => 3,
        _ => panic!("unknown action"),
    };
    if phase > 1 {
        if kind == 23 {
            config.host.push_str("-changed-after-record");
        }
        if kind == 24 {
            recipient = changed_recipient(&f.paths);
        }
        save_config(&f.paths, &config).unwrap();
        save_state(&f.paths, &state).unwrap();
        if phase == 3 {
            state = load_state(&f.paths).unwrap();
        }
        let bytes_before = std::fs::read(f.paths.state_file()).unwrap();
        let projection = if fault == Fault::IgnoreContext {
            state
                .http204_response
                .as_ref()
                .map(|receipt| VaultHttp204Response {
                    responded_at_ms: receipt.responded_at_ms,
                })
        } else {
            let projected = project(&config, recipient, &state);
            let observed = observe_http204_response(&f.paths, &config).unwrap();
            assert_eq!(observed, projected);
            if fault == Fault::ReadClock || (fault == Fault::ReloadClock && phase == 3) {
                // Unsafe projection: return a read clock without changing the
                // stored receipt. The old-history case distinguishes this
                // clock from the retained response without a timing wait.
                observed.map(|_| VaultHttp204Response {
                    responded_at_ms: aterm_time::unix_millis().unwrap(),
                })
            } else {
                observed
            }
        };
        shown = i64::from(projection.is_some());
        returned_at_ms = projection.map(VaultHttp204Response::responded_at_ms);
        assert_eq!(
            std::fs::read(f.paths.state_file()).unwrap(),
            bytes_before,
            "read must not write"
        );
    }
    let receipt = state.http204_response.as_ref().unwrap();
    let observed_at_ms = returned_at_ms.unwrap_or(receipt.responded_at_ms);
    let stamp = if observed_at_ms == 1 {
        1
    } else if observed_at_ms == f.reply.responded_at_ms() {
        2
    } else {
        3
    };
    let mut next = before.clone();
    next.insert("phase", phase);
    next.insert("wrote", i64::from(wrote));
    next.insert("stamp", stamp);
    next.insert("shown", shown);
    next
}

fn admits(model: &Model, kind: i64, before: &ModelState, after: &ModelState, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[("Kind", kind)],
        before,
        after,
        Some(action),
        "HTTP204 response conformance",
    );
    eprintln!("{} Kind={kind} {action}: {why}", model.name);
    accepted
}

#[test]
fn http204_configurations_conform_and_legacy_or_restamping_controls_are_refused() {
    let base = aterm_models_db::vault_http204_model();
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configurations| configurations[0].name == base.name)
        .unwrap();
    eprintln!(
        "{}: {} declared configurations",
        base.name,
        configurations.len()
    );
    let faults = verify::sweep_faults(&configurations);
    assert!(faults.is_empty(), "{faults:?}");
    for model in &configurations {
        let kind = model
            .consts
            .iter()
            .find(|(name, _)| *name == "Kind")
            .unwrap()
            .1;
        let coverage = verify::prove_and_catch_scalar(model, "HTTP204 response history");
        eprintln!("{} Kind={kind}: {coverage:?}", model.name);
        assert_eq!(
            aterm_spec::interp::fired_actions(model).len(),
            model.actions.len()
        );
        let mut before = model.init_state();
        for action in ["Receive", "Read", "Reload"] {
            let next = http204_transition(&before, action, kind);
            assert!(
                admits(&base, kind, &before, &next, action),
                "Kind={kind} {action}: {next:?}"
            );
            before = next;
        }
    }
    for kind in [3, 9] {
        let before = base.init_state();
        let unsafe_next = transition_with(&before, "Receive", kind, Fault::LegacyNoNewer);
        assert!(!admits(&base, kind, &before, &unsafe_next, "Receive"));
    }
    for (kind, action, fault) in [
        (3, "Read", Fault::ReadClock),
        (3, "Reload", Fault::ReloadClock),
        (23, "Read", Fault::IgnoreContext),
    ] {
        let model = aterm_spec::interp::with_consts(&base, &[("Kind", kind)]);
        let mut before = model.init_state();
        before = http204_transition(&before, "Receive", kind);
        if action == "Reload" {
            before = http204_transition(&before, "Read", kind);
        }
        let unsafe_next = transition_with(&before, action, kind, fault);
        if fault == Fault::ReadClock || fault == Fault::ReloadClock {
            assert!(!model.check_invariant("OriginalResponseTime", &unsafe_next));
        }
        assert!(!admits(&base, kind, &before, &unsafe_next, action));
    }
}
