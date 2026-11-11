// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn tags_and_glyphs_are_closed_sets() {
    for tag in tags::ALL {
        let again = Tag::try_new(tag.as_str()).expect("a vocabulary word validates");
        assert_eq!(&again, *tag);
    }
    assert_eq!(Tag::try_new(""), Err(TagError::Empty));
    assert_eq!(Tag::try_new("Config"), Err(TagError::BadChar('C')));
    assert_eq!(Tag::try_new("a b"), Err(TagError::BadChar(' ')));
    assert_eq!(Tag::try_new(&"x".repeat(25)), Err(TagError::TooLong));
    assert!(Tag::try_new(&"x".repeat(24)).is_ok());

    assert!(
        Glyph::new('\u{2715}').is_some(),
        "✕ paints in cells already"
    );
    assert!(Glyph::new('\u{2699}').is_none(), "⚙ is blank headless");
    assert!(Glyph::new('\u{1f680}').is_none(), "no emoji");
    assert_eq!(Glyph::or_fallback('\u{2699}'), Glyph::FALLBACK);
    assert_eq!(Glyph::FALLBACK.ch(), '!');
    for sev in Severity::ALL {
        assert!(Glyph::ALLOWED.contains(&sev.default_glyph().ch()));
        assert_eq!(Severity::parse(sev.as_str()), Some(sev));
    }
    assert!(Severity::Error > Severity::Warn && Severity::Warn > Severity::Info);
    assert!(Severity::Info > Severity::Success);
}

#[test]
fn labels_are_one_table() {
    for intent in every_intent() {
        let label = intent.label();
        let short = intent.short();
        assert!(
            !label.is_empty() && label.chars().count() <= 16,
            "{intent:?}: {label:?}"
        );
        assert!(
            !short.is_empty() && short.chars().count() <= 8,
            "{intent:?}: {short:?}"
        );
        assert_eq!(
            intent.closes_row(),
            matches!(intent, Intent::NotNow { .. }),
            "{intent:?}"
        );
        assert_eq!(
            intent.is_ask(),
            matches!(intent, Intent::NotNow { .. }),
            "{intent:?}"
        );
        assert!(
            !(intent.is_consequential() && intent.closes_row()),
            "{intent:?}: a consequential press never closes its row"
        );
        assert!(
            !(intent.is_decline() && intent.is_consequential()),
            "{intent:?}"
        );
    }
    let page = Intent::OpenSettings {
        route: "/security".into(),
    };
    let pane = Intent::OpenSystemPane {
        pane: "developer-tools".into(),
    };
    assert_eq!((page.label(), page.short()), ("Security", "Security"));
    assert_ne!(page.label(), pane.label());
    assert_ne!(page.short(), pane.short());
    assert!(Intent::ApplyUpdate { build: 1 }.is_consequential());
    assert!(
        !Intent::OpenSettings {
            route: "/packages".into()
        }
        .is_consequential()
            && !Intent::Details.is_consequential(),
        "navigations are the quiet chip"
    );
}

#[test]
fn intents_round_trip_and_junk_is_dropped() {
    for intent in every_intent() {
        let enc = intent.encode();
        assert_eq!(Intent::decode(&enc).as_ref(), Some(&intent), "{enc}");
        assert!(!enc.contains('\u{1f}') && !enc.contains('\t'), "{enc}");
    }
    let hostile = Intent::OpenPath {
        path: "/a b/%\t\n\u{1f}c".into(),
    };
    let enc = hostile.encode();
    assert!(
        !enc.contains('\t') && !enc.contains('\n') && !enc.contains('\u{1f}'),
        "{enc}"
    );
    assert_eq!(Intent::decode(&enc), Some(hostile));
    for junk in [
        "",
        "details:x",
        "open-config-editor:abc",
        "apply-update:-1",
        "not-now:maybe",
        "teleport:home",
        "new-window:1",
        "review-vault-sync:1",
        "show-session:0:1:s-x",
        "show-session:1:no:s-x",
        "show-session:1:1:",
        "show-session:1:1",
        "show-session:65536:1:s-x",
        "restart-service:",
        "restart-service:a b",
        "restart-service:../x",
        "restart-service:seventeen-letters",
        "details:",
        "new-window:",
        "end-recovered:",
        "review-vault-sync:",
        "open-settings",
        "open-path",
        "open-system-pane",
        "not-now:file-access:",
    ] {
        assert_eq!(Intent::decode(junk), None, "{junk:?}");
    }
    let restart = Intent::RestartService {
        service: "fseventsd".into(),
    };
    assert_eq!(restart.encode(), "restart-service:fseventsd");
    assert!(restart.is_consequential() && !restart.ends_with_row() && !restart.is_snooze());
    assert_eq!(
        Intent::decode("open-config-editor:-"),
        Some(Intent::OpenConfigEditor { line: None })
    );
    assert_eq!(
        Intent::decode("show-tab:2:7"),
        Some(Intent::ShowTab { tab: 2, window: 7 })
    );
    assert_eq!(
        Intent::decode("review-vault-sync"),
        Some(Intent::ReviewVaultSync),
        "records already written name it so"
    );
    let stable = Intent::ShowSession {
        tab: 2,
        window: 7,
        session: "s-a:b%\t".into(),
    };
    assert_eq!(Intent::decode(&stable.encode()), Some(stable));
    for retired in ["not-now:admin-step:clt,brew", "install-elevated:clt,brew"] {
        assert_eq!(Intent::decode(retired), None, "{retired:?}");
    }
    for hold in [
        Hold::Default,
        Hold::For(Duration::from_secs(9)),
        Hold::Live {
            stale_after: Duration::from_secs(30),
        },
        Hold::STANDING,
        Hold::asking(Duration::from_secs(600)),
        Hold::LogOnly,
    ] {
        assert_eq!(Hold::decode(&hold.encode()), Some(hold));
    }
    assert_eq!(Hold::decode("forever"), None);
    assert_eq!(Hold::decode("for:x"), None);
    assert_eq!(Hold::decode("default:"), None);
    assert_eq!(Hold::decode("standing:"), None);
    assert_eq!(Hold::decode("log-only:"), None);
    assert_eq!(
        Meter {
            fill_permille: Some(1000),
            stats: format!("{0} of {0} items", u64::MAX),
            ..Meter::default()
        }
        .normalized()
        .fill_permille,
        Some(1000)
    );
}

#[test]
fn an_open_link_decodes_only_an_https_page() {
    for url in [
        "https://chatgpt.com",
        "https://chatgpt.com/",
        "https://auth.openai.com/setup?step=2&x=a:b#mfa",
        "https://xn--bcher-kva.example/%7Euser",
    ] {
        let link = Intent::OpenLink { url: url.into() };
        assert_eq!(Intent::decode(&link.encode()), Some(link), "{url}");
    }
    assert_eq!(
        link_host("https://Auth.OpenAI.com/x"),
        Some("Auth.OpenAI.com")
    );
    let long = format!("https://example.com/{}", "a".repeat(LINK_URL_CAP));
    let refused = [
        "http://chatgpt.com/",
        "HTTPS://chatgpt.com/",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "mailto:someone@example.com",
        "https:chatgpt.com",
        "https:///path",
        "https://chatgpt.com:443/",
        "https://user@chatgpt.com/",
        "https://user:pw@chatgpt.com/",
        "https://good.com\\@evil.com/",
        "https://[::1]/",
        "https://localhost/",
        "https://chatgpt.com./",
        "https://.chatgpt.com/",
        "https://chat..gpt.com/",
        "https://-chatgpt.com/",
        "https://chat_gpt.com/",
        "https://chatgpt.com/a b",
        "https://chatgpt.com/\u{7f}",
        "https://chatgpt.com/\u{e9}",
        "https://chatgpt.com/\"x",
        long.as_str(),
    ];
    for url in refused {
        assert_eq!(link_host(url), None, "{url:?}");
        let forged = format!("open-link:{}", escape_payload(url));
        assert_eq!(Intent::decode(&forged), None, "{forged:?}");
    }
    assert_eq!(Intent::decode("open-link:"), None);
    assert_eq!(Intent::decode("open-link"), None);
    let link = Intent::OpenLink {
        url: "https://chatgpt.com/".into(),
    };
    assert_eq!((link.label(), link.short()), ("Open in browser", "Open"));
    assert!(!link.is_consequential() && !link.is_snooze() && !link.ends_with_row());
}

#[test]
fn a_busy_meter_never_carries_a_fill() {
    let m = Meter::busy("45 MB");
    assert!(m.busy && m.fill_permille.is_none());
    assert_eq!(m.stats, "45 MB");
    let filled = Meter {
        fill_permille: Some(420),
        busy: true,
        ..Meter::default()
    }
    .normalized();
    assert!(!filled.busy, "a fill is a fraction: not busy");
    let counted = Meter {
        amount: Some(Amount {
            series: 1,
            done: 5,
            total: 10,
            unit: Unit::Bytes,
        }),
        busy: true,
        ..Meter::default()
    }
    .normalized();
    assert_eq!((counted.fill_permille, counted.busy), (Some(500), false));
    let long = Meter::busy("x".repeat(STATS_CAP + 10));
    assert_eq!(long.stats.chars().count(), STATS_CAP);
    let posted = Message::new(tags::UPDATE, Severity::Info, "t").meter(Meter {
        fill_permille: Some(1),
        busy: true,
        ..Meter::default()
    });
    assert!(!posted.meter.unwrap().busy);
}

#[test]
fn in_flight_declares_busy_and_normalized_invents_nothing() {
    let live = Hold::Live {
        stale_after: Duration::from_secs(30),
    };
    let m = Message::new(tags::UPDATE, Severity::Info, "t").in_flight();
    assert_eq!(m.meter, Some(Meter::busy("")));
    let m = Message::new(tags::UPDATE, Severity::Info, "t")
        .meter(Meter {
            stats: "45 MB".into(),
            ..Meter::default()
        })
        .in_flight();
    assert_eq!(m.meter, Some(Meter::busy("45 MB")));
    let m = Message::new(tags::UPDATE, Severity::Info, "t")
        .meter(Meter {
            fill_permille: Some(300),
            ..Meter::default()
        })
        .in_flight();
    let meter = m.meter.unwrap();
    assert_eq!((meter.fill_permille, meter.busy), (Some(300), false));
    let still = Message::new(tags::UPDATE, Severity::Warn, "t")
        .hold(live)
        .normalized();
    assert_eq!(still.meter, None, "a Live row blocked on the person");
}

#[test]
fn the_builder_and_normalized_apply_every_cap() {
    let long = "x".repeat(TITLE_CAP + 40);
    let m = Message::new(tags::SYSTEM, Severity::Info, &long)
        .lines(
            (0..DETAIL_LINES_CAP + 5).map(|i| format!("line {i} {}", "y".repeat(DETAIL_LINE_CAP))),
        )
        .action(Intent::Details)
        .action(Intent::NewWindow)
        .action(Intent::NewWindow)
        .action(Intent::NewWindow)
        .key(&"k".repeat(KEY_CAP + 3))
        .meter(Meter {
            fill_permille: Some(4000),
            stats: "s".repeat(STATS_CAP + 5),
            ..Meter::default()
        });
    assert_eq!(m.title.chars().count(), TITLE_CAP);
    assert!(m.title.ends_with('\u{2026}'));
    assert_eq!(m.detail.len(), DETAIL_LINES_CAP);
    assert!(
        m.detail
            .iter()
            .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
    );
    let blank = Message::new(tags::SYSTEM, Severity::Info, "t")
        .line("")
        .line("\u{7}\u{1b}")
        .line("kept")
        .line("");
    assert_eq!(blank.detail, vec!["kept".to_string()]);
    let literal_blank = Message {
        detail: vec![String::new(), "a".into(), "\u{7}".into(), "b".into()],
        ..blank.clone()
    }
    .normalized();
    assert_eq!(literal_blank.detail, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(m.actions, vec![Intent::NewWindow, Intent::NewWindow]);
    assert_eq!(m.key.as_ref().map(|k| k.chars().count()), Some(KEY_CAP));
    let meter = m.meter.clone().unwrap();
    assert_eq!(meter.fill_permille, Some(1000));
    assert_eq!(meter.stats.chars().count(), STATS_CAP);

    let literal = Message {
        tag: tags::CONFIG,
        severity: Severity::Warn,
        glyph: Glyph::FALLBACK,
        title: "a\u{1b}[31mb\u{7}".into(),
        detail: vec!["ok".into()],
        actions: vec![
            Intent::Details,
            Intent::NewWindow,
            Intent::NewWindow,
            Intent::NewWindow,
        ],
        hold: Hold::Default,
        meter: None,
        key: Some(String::new()),
        origin: Origin::Host,
        reveal_after: None,
        finished: Some("\u{1b}Done".into()),
        loads: Loads::NONE,
        primary: None,
        need: Need::Unclassified,
    }
    .normalized();
    assert_eq!(literal.finished.as_deref(), Some("Done"));
    assert_eq!(
        literal.title, "a[31mb",
        "the ESC and BEL go; the printable bytes stay"
    );
    assert_eq!(literal.actions.len(), MAX_ACTIONS);
    assert_eq!(literal.key, None);
    assert!(
        Message::new(tags::SYSTEM, Severity::Info, "t")
            .key("")
            .key
            .is_none()
    );
    assert_eq!(MessageId::from_raw(0), None);
    assert_eq!(MessageId::from_raw(7).map(MessageId::raw), Some(7));
    assert_eq!(MessageId::FIRST.next().raw(), 2);
}
