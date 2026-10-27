// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

const OLD_TOTP: &str =
    "otpauth://totp/Test:mailbox%40example.org?secret=JBSWY3DPEHPK3PXP&issuer=Test";

fn credential(kind: usize, tier: i64) -> String {
    match kind {
        0 => format!("password-{tier}"),
        1 => format!("second-{tier}"),
        2 => match tier {
            1 => "KRSXG5A".to_owned(),
            2 => OLD_TOTP.to_owned(),
            3 => "MZXW6YTBOI".to_owned(),
            4 => "GEZDGNBVGY".to_owned(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn configuration(label: &str, tier: i64, exact: Option<i64>) -> String {
    let mut text = String::new();
    for (kind, section) in ["passwords", "mailbox_passwords", "totp_secrets"]
        .into_iter()
        .enumerate()
    {
        text.push_str(&format!(
            "[{section}]\n'{label}' = '{}'\n",
            credential(kind, tier)
        ));
        if let Some(exact) = exact {
            text.push_str(&format!(
                "'MailBox@Example.ORG' = '{}'\n",
                credential(kind, exact)
            ));
        }
    }
    text
}

fn fixture(source: i64, case: i64, spelling: &str) -> MailboxConfig {
    let home = aterm_tempfile::tempdir().unwrap();
    let low = configuration("m27-2", 2, (case == 2).then_some(4));
    let high = configuration(spelling, 1, (case == 1).then_some(3));
    let roster = "[[account]]\nlabel='m27-2'\nclass='dedicated'\nemail='mailbox@example.org'\n";
    let files = [
        (".secrets/ai-mailbox.toml", low.as_str()),
        ("store/mailbox.toml", high.as_str()),
        (".config/aterm/accounts.toml", roster),
    ];
    for (name, text) in files {
        let path = home.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let store = home.path().join("store");
    let cfg = match source {
        0 => discover_mailbox_config_files_raw(Some(home.path()), Some(&store), None),
        1 => discover_mailbox_config_with_env(Some(home.path()), None, |name| {
            (name == MAILBOX_ENV).then(|| store.join("mailbox.toml").into_os_string())
        })
        .unwrap(),
        _ => unreachable!(),
    };
    for (name, text) in files {
        assert_eq!(
            std::fs::read_to_string(home.path().join(name)).unwrap(),
            text
        );
    }
    cfg
}

pub(crate) fn precedence_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "AnotherSource" | "AnotherCase" => {
            mailbox_precedence_environment();
            let key = if action == "AnotherSource" {
                "source"
            } else {
                "case"
            };
            next.insert(key, state[key] + 1);
        }
        "Resolve" => {
            let mut selected = None;
            for spelling in ["m27-2", "m27_2", "m27.2"] {
                let cfg = fixture(state["source"], state["case"], spelling);
                let values = [
                    cfg.password_for(Some("mailbox@example.org")),
                    cfg.mailbox_password_for(Some("mailbox@example.org")),
                    cfg.totp_secret_for(Some("mailbox@example.org")),
                ];
                for (kind, value) in values.into_iter().enumerate() {
                    let value = value.expect("fixture has a credential");
                    let tier = (1..=4)
                        .find(|tier| credential(kind, *tier) == value)
                        .unwrap();
                    if let Some(prior) = selected {
                        assert_eq!(tier, prior, "credential kinds and label spellings agree");
                    }
                    selected = Some(tier);
                }
            }
            next.insert("phase", 1);
            next.insert("selected", selected.unwrap());
        }
        _ => panic!("unknown precedence action"),
    }
    next
}

#[test]
fn mailbox_source_precedence_conforms_with_negative_controls() {
    let model = aterm_models_harness::account_mailbox_precedence_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 12);
    let mut witnessed = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = precedence_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                if action.name == "Resolve" && state["source"] == 0 {
                    witnessed.insert(state["case"], (state.clone(), next));
                }
            }
        }
    }
    for (case, wrong, law) in [
        (0, 2, "WinningLabel"),
        (1, 1, "PrimaryExplicit"),
        (2, 1, "SecondaryExplicit"),
    ] {
        let (before, after) = &witnessed[&case];
        let mut mutant = after.clone();
        mutant.insert("selected", wrong);
        assert!(!model.check_invariant(law, &mutant));
        assert!(!model.successors("Resolve", before).contains(&mutant));
    }
}

#[test]
fn mailbox_superseded_totp_uri_does_not_retain_its_old_email_alias() {
    let home = aterm_tempfile::tempdir().unwrap();
    let old = configuration("m27", 2, None).replace("mailbox%40", "old-inbox%40");
    let new_uri = OLD_TOTP.replace("mailbox%40", "new-inbox%40");
    let new = configuration("m27", 1, None).replace("KRSXG5A", &new_uri);
    for (name, text) in [
        (".secrets/ai-mailbox.toml", old),
        ("store/mailbox.toml", new),
    ] {
        let path = home.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let cfg = discover_mailbox_config_files_raw(
        Some(home.path()),
        Some(&home.path().join("store")),
        None,
    );
    assert!(cfg.password_for(Some("old-inbox@example.org")).is_none());
    assert!(
        cfg.mailbox_password_for(Some("old-inbox@example.org"))
            .is_none()
    );
    assert!(cfg.totp_secret_for(Some("old-inbox@example.org")).is_none());
    assert_eq!(
        cfg.password_for(Some("new-inbox@example.org")).as_deref(),
        Some("password-1")
    );
    assert_eq!(
        cfg.totp_secret_for(Some("new-inbox@example.org"))
            .as_deref(),
        Some(new_uri.as_str())
    );
}

#[test]
fn mailbox_raw_named_secrets_obey_contested_label_isolation() {
    let home = aterm_tempfile::tempdir().unwrap();
    for (name, text) in [
        (".secrets/m27", format!("password-2\nsecond-2\n{OLD_TOTP}\n")),
        (".config/aterm/accounts.toml", "[[account]]\nlabel='m27'\nclass='dedicated'\nemail='owner@example.org'\n".to_owned()),
        (".config/aterm/accounts-kimi.toml", "[[account]]\nlabel='m27'\nclass='dedicated'\nemail='other@example.org'\nregion='global'\n".to_owned()),
    ] {
        let path = home.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let cfg = discover_mailbox_config_files_raw(Some(home.path()), None, None);
    assert!(cfg.contested_labels.contains("m27"));
    for email in ["mailbox@example.org", "other@example.org"] {
        assert!(cfg.password_for(Some(email)).is_none());
        assert!(cfg.mailbox_password_for(Some(email)).is_none());
    }
    assert_eq!(
        cfg.password_for(Some("owner@example.org")).as_deref(),
        Some("password-2")
    );
    assert_eq!(
        cfg.totp_secret_for(Some("mailbox@example.org")).as_deref(),
        Some(OLD_TOTP)
    );
}

#[test]
fn mailbox_named_files_cannot_override_equivalent_store_keys() {
    for raw in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let high = configuration("m27.2", 1, None);
        let roster = "[[account]]\nlabel='m27-2'\nclass='dedicated'\nemail='mailbox@example.org'\n";
        for (name, text) in [
            ("store/mailbox.toml", high.as_str()),
            (".config/aterm/accounts.toml", roster),
        ] {
            let path = home.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let secrets = home.path().join(".secrets");
        std::fs::create_dir(&secrets).unwrap();
        if raw {
            std::fs::write(
                secrets.join("m27_2"),
                format!("password-2\nsecond-2\n{OLD_TOTP}\n"),
            )
            .unwrap();
        } else {
            for (name, value) in [
                ("m27_2_password", credential(0, 2)),
                ("m27_2_mailbox_password", credential(1, 2)),
                ("m27_2_totp", credential(2, 2)),
            ] {
                std::fs::write(secrets.join(name), value).unwrap();
            }
        }
        let cfg = discover_mailbox_config_files_raw(
            Some(home.path()),
            Some(&home.path().join("store")),
            None,
        );
        for (kind, actual) in [
            cfg.password_for(Some("mailbox@example.org")),
            cfg.mailbox_password_for(Some("mailbox@example.org")),
            cfg.totp_secret_for(Some("mailbox@example.org")),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(actual, Some(credential(kind, 1)), "raw={raw}");
        }
    }
}

fn explicit_email_fixture(source: i64) -> MailboxConfig {
    let owner = "owner_m27@example.org";
    let home = aterm_tempfile::tempdir().unwrap();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).unwrap();
    let values = [credential(0, 1), credential(1, 1), credential(2, 1)];
    let fields = ["password", "mailbox_password", "totp_secret"];
    let body: String = fields
        .iter()
        .zip(&values)
        .map(|(key, value)| format!("{key} = '{value}'\n"))
        .collect();
    match source {
        0 => std::fs::write(secrets.join(owner), &body).unwrap(),
        1 => {
            for (field, value) in fields.iter().zip(&values) {
                std::fs::write(secrets.join(format!("{owner}_{field}")), value).unwrap();
            }
        }
        2 => std::fs::write(
            secrets.join("ai-mailbox.toml"),
            format!("[accounts.'{owner}']\n{body}"),
        )
        .unwrap(),
        3 => {
            let text: String = fields
                .iter()
                .zip(&values)
                .map(|(key, value)| format!("'{owner}_{key}' = '{value}'\n"))
                .collect();
            std::fs::write(secrets.join("ai-mailbox.toml"), text).unwrap();
        }
        4 => {
            let text: String = ["passwords", "mailbox_passwords", "totp_secrets"]
                .iter()
                .zip(&values)
                .map(|(section, value)| format!("[{section}]\n'{owner}' = '{value}'\n"))
                .collect();
            std::fs::write(secrets.join("ai-mailbox.toml"), text).unwrap();
        }
        5 => {
            let mut text = String::new();
            for (field, value) in fields.iter().zip(&values) {
                let path = secrets.join(format!("value-{field}"));
                std::fs::write(&path, value).unwrap();
                text.push_str(&format!("'{owner}_{field}_file' = '{}'\n", path.display()));
            }
            std::fs::write(secrets.join("ai-mailbox.toml"), text).unwrap();
        }
        6 => {
            let text = format!(
                "[accounts]\n'{owner}' = '{}'\n\
                 [mailbox_passwords]\n'{owner}' = '{}'\n\
                 [totp_secrets]\n'{owner}' = '{}'\n",
                values[0], values[1], values[2]
            );
            std::fs::write(secrets.join("ai-mailbox.toml"), text).unwrap();
        }
        _ => unreachable!(),
    }
    discover_mailbox_config_files_raw(Some(home.path()), None, None)
}

pub(crate) fn scope_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    if action == "AnotherSource" {
        mailbox_scope_environment();
        next.insert("source", state["source"] + 1);
        return next;
    }
    assert_eq!(action, "Resolve");
    let cfg = explicit_email_fixture(state["source"]);
    let mut selected = BTreeMap::new();
    for (key, target) in [
        ("own", "owner_m27@example.org"),
        ("spelling", "owner-m27@example.org"),
        ("machine", "other.m27@example.org"),
        ("machine", "m27"),
    ] {
        for (kind, actual) in [
            cfg.password_for(Some(target)),
            cfg.mailbox_password_for(Some(target)),
            cfg.totp_secret_for(Some(target)),
        ]
        .into_iter()
        .enumerate()
        {
            let value = match actual {
                None => 0,
                Some(value) => {
                    assert_eq!(value, credential(kind, 1));
                    1
                }
            };
            if let Some(prior) = selected.insert(key, value) {
                assert_eq!(
                    prior, value,
                    "all credential kinds and machine lookups agree"
                );
            }
            next.insert(key, value);
        }
    }
    next.insert("phase", 1);
    next
}

#[test]
fn explicit_email_secrets_do_not_create_credentials_for_other_mailboxes() {
    let model = aterm_models_harness::account_mailbox_scope_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 14);
    let mut witness = None;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = scope_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                if action.name == "Resolve" {
                    witness = Some((state.clone(), next));
                }
            }
        }
    }
    let (before, after) = witness.unwrap();
    for (key, wrong, law) in [
        ("own", 0, "OwnerWorks"),
        ("spelling", 1, "ExactSpelling"),
        ("machine", 1, "NoMachineWidening"),
    ] {
        let mut mutant = after.clone();
        mutant.insert(key, wrong);
        assert!(!model.check_invariant(law, &mutant));
        assert!(!model.successors("Resolve", &before).contains(&mutant));
    }
}
