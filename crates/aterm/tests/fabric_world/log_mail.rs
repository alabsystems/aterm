// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end test that delivered mail writes a `mail dir=in` mark in the session log.

use super::harness::{FLEET, World, kv, off_of, until};

/// A mirroring world whose bridge accepts `a-orchestrator`'s tasks, ready.
fn world(tag: &str) -> World {
    let w = World::boot_flags_with(
        tag,
        &["--log", "turns", "--accept-from", "a-orchestrator"],
        &[],
    );
    w.wait_ready();
    w
}

/// `@<sid> log <args>`'s rows.
fn log_rows(w: &World, sid: &str, args: &str) -> Vec<String> {
    let r = w.verb(&format!("@{sid} log {args}"));
    assert!(r.ok(), "log {args}: {}", r.header());
    r.rows().to_vec()
}

/// The session's mail records.
fn mail(w: &World, sid: &str) -> Vec<String> {
    log_rows(w, sid, "kinds=mail,mail-seen,mail-landed")
}

/// The session's `mail dir=in` mark at broker offset `off`, once written.
fn mail_in_at(w: &World, sid: &str, off: u64) -> String {
    until(&format!("{sid}'s mail dir=in at off={off}"), || {
        mail(w, sid)
            .into_iter()
            .find(|l| l.contains(" mail dir=in ") && kv(l, "off") == Some(off.to_string().as_str()))
    })
}

/// Every body on `face`, oldest first.
fn bodies(w: &World, face: &str) -> Vec<String> {
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, face, 256)?;
            for (_, _, body) in rows {
                out.push(String::from_utf8_lossy(&body).into_owned());
            }
            if next >= head || next <= from {
                return Ok(out);
            }
            from = next;
        }
    })
    .unwrap_or_else(|e| panic!("read {face}: {e}"))
}

#[test]
fn delivered_mail_is_a_mail_mark_in_the_log_end_to_end() {
    let started = std::time::Instant::now();
    let w = world("lmail");
    let (a, b) = w.two_sessions();

    let mut god = w.god();
    let mut tasks = Vec::new();
    for (who, text) in [
        ("a-stranger", "SENTINEL-UNLISTED"),
        ("a-orchestrator", "SENTINEL-LISTED"),
    ] {
        let subject = format!("/f/{FLEET}/in/{}/{b}/{who}/task", w.node);
        let body = format!("v=1 t=1 text={text}");
        let (off, _) = god
            .publish(
                7_000 + u64::try_from(who.len()).unwrap_or(0),
                1,
                &subject,
                body.as_bytes(),
            )
            .expect("publish the task");
        tasks.push((who, off));
    }

    let reply = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=60000 SENTINEL-ASK"
    ));
    assert!(
        reply.ok(),
        "post: {} — log:\n{}",
        reply.header(),
        w.log_tail()
    );
    let r_off = off_of(reply.header());

    let ask = mail_in_at(&w, &b, r_off);
    assert!(
        ask.contains(" kind=ask ") && ask.contains(" by=bridge src=fabric conf=exact "),
        "{ask}"
    );
    assert!(ask.contains(&format!(" from={a}@{} ", w.node)), "{ask}");
    for (who, off) in &tasks {
        let row = mail_in_at(&w, &b, *off);
        let want = if *who == "a-orchestrator" {
            " kind=task "
        } else {
            " kind=note "
        };
        assert!(row.contains(want), "{who}: {row}");
        assert!(row.contains(" by=bridge src=fabric conf=exact "), "{row}");
    }

    let landed = until("A's mail-landed", || {
        mail(&w, &a)
            .into_iter()
            .find(|l| l.contains(" mail-landed "))
    });
    assert_eq!(
        kv(&landed, "off"),
        Some(r_off.to_string().as_str()),
        "{landed}"
    );

    let inbox = kv(&ask, "inbox").expect("the ask's inbox id").to_string();
    let seen = w.verb(&format!("@{b} inbox seen {inbox} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());
    let seen_mark = until("B's mail-seen", || {
        mail(&w, &b)
            .into_iter()
            .find(|l| l.contains(" mail-seen ") && kv(l, "inbox") == Some(inbox.as_str()))
    });
    assert!(seen_mark.contains(" word=handled "), "{seen_mark}");

    for sid in [&a, &b] {
        for row in log_rows(&w, sid, "tier=content") {
            assert!(!row.contains("SENTINEL"), "a body in {sid}'s log: {row}");
        }
    }

    assert!(w.verb(&format!("@{b} mark - need word=review")).ok());
    let need: u64 = log_rows(&w, &b, "kinds=need")
        .last()
        .and_then(|l| l.split_whitespace().next()?.parse().ok())
        .expect("the need's seq");
    let face = format!("/f/{FLEET}/pub/{}/{b}/ev", w.node);
    until("the need to land on B's ev face", || {
        bodies(&w, &face).iter().find_map(|body| {
            let d = aterm_slog::decode_bus(body).ok()?;
            (d.rec.seq == need && d.rec.k == aterm_slog::Kind::Need).then_some(())
        })
    });
    let on_face = bodies(&w, &face);
    for body in &on_face {
        if let Ok(d) = aterm_slog::decode_bus(body) {
            assert!(
                !matches!(
                    d.rec.k,
                    aterm_slog::Kind::Mail
                        | aterm_slog::Kind::MailSeen
                        | aterm_slog::Kind::MailLanded
                ),
                "a mail record on the ev face: {body}"
            );
        }
        for leak in ["SENTINEL", " dir=", " inbox=", " from="] {
            assert!(!body.contains(leak), "{leak} on the ev face: {body}");
        }
    }

    eprintln!(
        "MEASURED log_mail world: {} ms, {} ev bodies read",
        started.elapsed().as_millis(),
        on_face.len()
    );
}
