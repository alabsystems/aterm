// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{cmd_grant, log_actor};
use crate::control::{Principal, Scope, ServingPrincipal};
use crate::session_store::test_handle;
use aterm_session::{EdgeToken, Op, SessionId};
use aterm_slog::Actor;

#[test]
fn log_actor_matrix() {
    let caller = test_handle(1);
    let target = test_handle(2);
    assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Ctl);
    assert_eq!(
        log_actor(
            Scope::Fleet {
                content: true,
                drive: true
            },
            &target.ctx
        ),
        Actor::Nobody
    );
    {
        let _driver = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "authenticated-node".to_owned(),
            host: "m72 / remote=desk".to_owned(),
        });
        let expected = Actor::fleet_drive("authenticated-node", None);
        assert_eq!(log_actor(Scope::Owner, &target.ctx), expected);
        let _harness = ServingPrincipal::enter(Principal::Harness);
        assert_eq!(log_actor(Scope::Owner, &target.ctx), expected);
        assert_eq!(log_actor(Scope::Bridge, &target.ctx), expected);
        assert_eq!(
            log_actor(
                Scope::Fleet {
                    content: true,
                    drive: true
                },
                &target.ctx
            ),
            expected
        );
    }
    assert_eq!(
        log_actor(Scope::Owner, &target.ctx),
        Actor::Ctl,
        "driver context restored"
    );
    {
        let _harness = ServingPrincipal::enter(Principal::Harness);
        assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Harness);
        assert_eq!(
            log_actor(Scope::Bridge, &target.ctx),
            Actor::Bridge,
            "the principal labels an Owner connection only"
        );
    }
    assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Ctl, "restored");
    let grant = |src: &str| {
        target
            .ctx
            .edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .grant(
                SessionId::new(src),
                target.sid.clone(),
                Op::WriteInput,
                target.ctx.nonce,
            )
    };
    let minted = grant(caller.sid.as_str());
    assert_eq!(
        log_actor(Scope::Edge(minted), &target.ctx).word(),
        caller.sid.as_str()
    );
    for chosen in [
        "harness", "human", "ctl", "bridge", "aterm", "s-boss", "s-1",
    ] {
        let tok = grant(chosen);
        assert_eq!(
            log_actor(Scope::Edge(tok), &target.ctx),
            Actor::Nobody,
            "an edge granted as {chosen:?} is nobody"
        );
    }
    assert_eq!(
        log_actor(Scope::Edge(EdgeToken::generate()), &target.ctx),
        Actor::Nobody
    );
}

#[cfg(unix)]
#[test]
fn fleet_input_completion_keeps_admitted_actor_after_driver_context_ends() {
    use crate::control_input_ack::{InputMark, testing};
    use aterm_slog::{Conf, Key, Kind, Src, Val};
    let (handle, mut reader) = testing::owned_handle(1);
    let expected = Actor::fleet_drive("authenticated-node", None);
    let mark = {
        let _driver = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "authenticated-node".to_owned(),
            host: "m72 / remote=desk".to_owned(),
        });
        InputMark::at_epoch(
            &handle.ctx,
            1,
            log_actor(Scope::Owner, &handle.ctx),
            crate::session_log::InputVerb::Paste,
            None,
            false,
        )
        .typed_by(
            &handle.ctx,
            Some(crate::typed_by::Caller::new(crate::typed_by::Who::Word(
                "harness".to_owned(),
            ))),
        )
    };
    assert_eq!(log_actor(Scope::Owner, &handle.ctx), Actor::Ctl);
    assert_eq!(testing::input_count(&handle), 0, "admission is not a write");
    assert!(testing::paste(&handle, &mark, "by=harness"));
    assert_eq!(
        testing::byte(&mut reader),
        b'b',
        "actual accepted pipe byte"
    );
    let timeline = handle.ctx.timeline.lock().unwrap();
    let rows: Vec<_> = timeline
        .slog()
        .log()
        .window()
        .filter(|row| row.k == Kind::Input)
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].by, expected,
        "neither later TLS, text nor typist selects the actor"
    );
    assert_eq!(rows[0].src, Src::Ctl);
    assert_eq!(rows[0].conf, Conf::Strong);
    assert_eq!(rows[0].f.get(Key::Verb), Some(&Val::Word("paste")));
}

#[test]
fn grant_refuses_a_reserved_source() {
    let target = test_handle(3);
    for word in aterm_slog::RESERVED_ACTOR_WORDS {
        let reply = cmd_grant(&target.ctx, Scope::Owner, &format!("{word} write-input"));
        assert!(
            reply.starts_with(&format!("ERR reserved: src={word} ")),
            "{word}: {reply}"
        );
    }
    let ok = cmd_grant(
        &target.ctx,
        Scope::Owner,
        "s-0123456789abcdef0123 write-input",
    );
    assert!(ok.starts_with("OK "), "{ok}");
    let store = crate::session_store::new_store();
    store.write().unwrap().register(target);
    let conn = std::sync::Arc::new(crate::connections::ConnectionTable::default());
    let reply = super::cmd_connect_in(
        &conn,
        &store,
        Scope::Owner,
        &format!("dst={} src=harness", "s-x"),
    );
    assert!(reply.starts_with("ERR reserved: src=harness "), "{reply}");
}
