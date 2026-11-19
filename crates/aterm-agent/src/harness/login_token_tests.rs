// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The credential readers' tests (plan package SF-34): each client's real reader turns made-up
//! login files into facts, and no byte of a value reaches them.

use super::*;
use crate::harness::accounts::{
    Account, AccountRecords, Auth, Binding, Logins, Roster, TokensHere,
};
use crate::harness::accounts_tokens::{self, InVault, TokenRow, UsedOn, Verification};
use crate::harness::codex_app_login::{self, RefusalReason};

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const ADA: &str = "ac-0123456789abcdef";
const BEA: &str = "ac-fedcba9876543210";
const CY: &str = "ac-00112233445566aa";

/// `bytes` in base64url with no padding, as a JWT's segments are.
fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    aterm_codec::base64::encode_url_into(&mut out, bytes).unwrap();
    out
}

/// A made-up JWT-shaped token for `email` that states its end `exp`, signed `sig`.
fn jwt(email: &str, exp: i64, sig: &str) -> String {
    format!(
        "{}.{}.{sig}",
        b64url(br#"{"alg":"RS256","typ":"JWT"}"#),
        b64url(format!(r#"{{"exp":{exp},"email":"{email}"}}"#).as_bytes())
    )
}

/// Each 8-byte window of `values` that is in `text`.
fn windows(values: &[&str], text: &str) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| (0..=value.len().saturating_sub(8)).map(move |at| &value[at..at + 8]))
        .filter(|window| text.contains(window))
        .map(str::to_owned)
        .collect()
}

fn row(label: &str, dir: &std::path::Path) -> Row {
    Row {
        label: label.into(),
        email: Some(format!("{label}@x.example")),
        dir: Some(dir.to_string_lossy().into_owned()),
        ..Row::default()
    }
}

/// `rows` signed in, each renewed at `renewed` and last served at `served`.
fn records(rows: Vec<Row>, renewed: i64, served: i64) -> AccountRecords {
    let mut state = accounts::State::default();
    state.contextual_auth = true;
    let mut logins = Logins::default();
    for row in &rows {
        state.accounts.insert(
            row.label.clone(),
            Account {
                auth: Auth::Ok,
                auth_at: Some(renewed),
                served_metadata: true,
                ..Account::default()
            },
        );
        logins.labels.insert(
            row.label.clone(),
            Binding {
                renewed_at: Some(renewed),
                served_at: Some(served),
                ..Binding::default()
            },
        );
    }
    let roster = Roster { rows };
    AccountRecords {
        raw_roster: roster.clone(),
        roster,
        roster_present: true,
        state,
        logins,
        notes: Vec::new(),
    }
}

/// Each of `records`' rows' word on record taken of the credentials its login file holds now, as
/// the reading that wrote it captured them (`Store::capture_auth_context`; none for a CLI with
/// no credential context).
fn captured(store: &Store, records: &mut AccountRecords) {
    for row in &records.roster.rows {
        if let Some(account) = records.state.accounts.get_mut(&row.label) {
            account.auth_context = store.capture_auth_context(row);
        }
    }
}

/// A token table row of `key` minted at `minted`, ending at `ends`, last served at `served`.
fn minted(key: &str, minted: i64, ends: i64, served: i64) -> TokenRow {
    let at = |t: i64| Some(u64::try_from(t).unwrap());
    TokenRow {
        key: key.to_owned(),
        account_id: None,
        label: Some("ada".into()),
        entry: format!("aterm-claude-token-{key}"),
        minted_at: at(minted),
        expires_at: at(ends),
        expiry_from: Some("printed".into()),
        minted_on: Some("m7".into()),
        signer: None,
        used_on: UsedOn {
            first_served_at: at(minted),
            last_served_at: at(served),
        },
        vault: InVault::InVault,
        verified: None,
        verification: Verification::default(),
        removed_at: None,
    }
}

/// Records Claude Code's refusal, on this Mac at `now`, of `key`'s token minted at `minted` and
/// ending at `ends`, as the harness records one (`claude_token::note_refused`).
fn refuse(paths: &aterm_secrets::client::Paths, key: &str, minted: i64, ends: i64, now: i64) {
    use aterm_secrets::claude_token::{self, Check, ExpiryFrom, Meta, Standing};
    let standing = Standing {
        key: claude_token::Key::parse(key).unwrap(),
        meta: Some(Meta {
            email: "ada@x.example".into(),
            account_id: "c-0123456789ab".into(),
            minted_at: u64::try_from(minted).unwrap(),
            expires_at: u64::try_from(ends).unwrap(),
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m7".into(),
            cli: "2.1.318".into(),
            minted_by: None,
            verified: None,
        }),
        check: Check::Readable,
        signer: None,
        minted_here: false,
        exposed: Vec::new(),
        refused_at: None,
    };
    let now = u64::try_from(now).unwrap();
    assert!(claude_token::note_refused(paths, &standing, 401, now).unwrap());
}

/// EACH CLIENT'S READER STATES ITS CREDENTIAL'S FACTS AND NO BYTE OF ITS VALUE, under one rule:
/// the end a row states is the credential's own where nothing renews it, and `valid` or
/// `expiring` only where the login serves by its word on record, scoped to the login the row
/// routes to now on the records with this Mac's runtime readings in them (`Judged::of`).
///
/// A Mac's login files hold made-up values, and each Codex word on record is of the credentials
/// its `auth.json` held when it was read (`captured`), as a reading records it. Codex: ada's
/// `auth.json` with JWT-shaped access and id tokens, a refresh token and its last refresh, read
/// as an `oauth` login in its `login-file`, issued at its renewal, with no end (its CLI renews the
/// access token), refreshed at its `last_refresh`, last used at its last served turn, `valid`;
/// ada's login in bob's place (his row expects his address) states nothing for bob; an API-key
/// login gives no credential context, so no word on record is scoped to it: `unknown`, with no
/// time, signed in or answering another account; ada's word on record of other credentials says
/// nothing of hers, nor of their use: `unknown`; an access token held with no refresh token
/// states its end, `expiring` 8 days before it, `valid` 60 days before it, `expired` past it, and
/// one held with a refresh token states none and is `valid`; a refusal of ada's refresh token
/// still pending leaves her `valid` and turns when it settles, and once settled she is
/// `refused`. Claude Code: ada on her vault token, a `setup-token` in the `secret-store` with the
/// mint and last use of the kept row of the token the reading names, `valid`, `expiring` from 30
/// days before its end, `refused` once this Mac recorded Claude Code's refusal of that very
/// token; with her stored login signed out and off her token, that token stated `expired` once
/// its end passed, or `refused` once refused here; bea's stored login in its `login-file` with a
/// refresh token, no end, `valid`; cy's with no refresh token, its access token's end and
/// `expiring`; and bea's earlier, signed-out login, whose file holds no token, `expired`. Every
/// reading is compared whole, and so are the clock's turns; the files each reader reads are
/// named for a reader that stamps them. No 8-byte window of any value is in any reading, nor in
/// the login rows `login_rows` makes of them, where bea's row states the facts of the login that
/// speaks for her and the wrong-account API-key row is not `valid`.
///
/// NEGATIVE CONTROLS, each run on macOS (one mutant per build of `login_token`, this test
/// alone): with `Judged::of` not hydrating the records, `` a signed-in API-key login `` fails;
/// with `by_end` treating an end within 30 days as `valid` (`<= 0` for `<= soon()`), `` an access
/// token 8 days before its end `` fails; with `stored_credential_in_home` given no expected
/// address, `` ada's login in bob's place `` fails; with Codex stating a refreshable access
/// token's end (`!stored.refreshable` dropped from its filter), `` ada's OAuth login `` fails;
/// with `setup_token` ignoring this Mac's refusal, `` a refused vault token the reading still
/// names `` fails; with `read` stating no withdrawn token (`withdrawn` answering `None`), `` her
/// stored login signed out, her token ended `` fails; with `SetupTokens::of_token` taking the
/// account's first row whatever its end (`.next()` for its `.find(..)`), `` the token the reading
/// names `` fails; with `codex_login` stating an API key by its presence
/// (`serves(..) || stored.api_key`), `` a signed-in API-key login `` fails; with Claude Code's
/// stored login never `expired` (its file's lapse and `signed_out_here` ignored), `` a login file
/// that holds no token `` fails.
#[test]
fn each_clients_reader_states_its_credentials_facts_and_no_byte_of_its_value() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let at = |t: i64| Some(u64::try_from(t).unwrap());
    let write = |dir: &std::path::Path, name: &str, doc: &aterm_json::Value| {
        aterm_types::fs_restricted::ensure_private_dir(dir).unwrap();
        std::fs::write(dir.join(name), doc.to_string()).unwrap();
    };

    // Codex: ada's OAuth login, an API-key one, ada's login in bob's place, and an access token
    // held with no refresh token.
    let access = jwt(
        "ada@x.example",
        NOW + 8 * DAY,
        "Fx9KqZ2mWv7tRb4nPc1hLd6gJs3aYe8uQo5iMx0Vk",
    );
    let id_token = jwt(
        "ada@x.example",
        NOW + 9 * DAY,
        "Lq3vNw8yXr1zMc5tGb9kHf2dJs6aPe4uWo7iRx0Bz",
    );
    let refresh = concat!("rt_", "Hp4nWq8xZm2vKt6bRc9jLf3dGs7aYe1uPo5iNx0Tz");
    let openai = concat!("sk-", "proj-", "Zt5wQm9xKv2rLb7nHc3jPf8dGs1aYe4uWo6iNx0Rq");
    let ada_doc = aterm_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": aterm_types::rfc3339::format_rfc3339(u64::try_from(NOW - 2 * DAY).unwrap()),
        "tokens": {"access_token": access, "id_token": id_token, "refresh_token": refresh},
    });
    let codex_dir = scratch.path().join("codex-ada");
    let bob_dir = scratch.path().join("codex-bob");
    let keyed_dir = scratch.path().join("codex-key");
    let wrong_dir = scratch.path().join("codex-wrong");
    let short_dir = scratch.path().join("codex-short");
    write(&codex_dir, codex_app_login::AUTH_FILE, &ada_doc);
    write(&bob_dir, codex_app_login::AUTH_FILE, &ada_doc);
    for dir in [&keyed_dir, &wrong_dir] {
        write(
            dir,
            codex_app_login::AUTH_FILE,
            &aterm_json::json!({"OPENAI_API_KEY": openai, "tokens": null}),
        );
    }
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let codex = accounts::Store::at_for(&harness, Cli::Codex, &home);
    let ada_codex = row("ada", &codex_dir);
    let bob_codex = row("bob", &bob_dir);
    let key_codex = row("key", &keyed_dir);
    let wrong_codex = row("wrong", &wrong_dir);
    let short = Row {
        email: Some("ada@x.example".into()),
        ..row("short", &short_dir)
    };
    let mut codex_records = records(
        vec![
            ada_codex.clone(),
            bob_codex.clone(),
            key_codex.clone(),
            wrong_codex.clone(),
            short.clone(),
        ],
        NOW - 20 * DAY,
        NOW - 900,
    );
    // An API-key row with no sign-in on record whose renewal since answered another account.
    codex_records.state.accounts.get_mut("wrong").unwrap().auth = Auth::Unseen;
    codex_records
        .logins
        .labels
        .get_mut("wrong")
        .unwrap()
        .wrong_at = Some(NOW - 100);
    captured(&codex, &mut codex_records);
    let codex_records = std::cell::RefCell::new(codex_records);
    let ada_key = Key::new(ADA).unwrap();
    let none = SetupTokens::default();
    let codex_read = |row: &Row, now: i64| {
        read(
            Cli::Codex,
            row,
            &ada_key,
            &Judged::of(&codex, &codex_records.borrow()),
            &codex,
            &none,
            now,
        )
    };
    let oauth = TokenFacts {
        kind: Kind::OAuth,
        kept: Some(Kept::LoginFile),
        issued_at: at(NOW - 20 * DAY),
        expires_at: None,
        refreshed_at: at(NOW - 2 * DAY),
        last_used: at(NOW - 900),
        state: State::Valid,
    };
    let ada = codex_read(&ada_codex, NOW);
    assert_eq!(
        ada,
        Reading {
            facts: Some(oauth),
            turns: None,
        },
        "ada's OAuth login"
    );
    assert_eq!(
        (
            oauth.kind.word(),
            oauth.kept.map(Kept::word),
            oauth.state.word()
        ),
        ("oauth", Some("login-file"), "valid"),
        "the words its row states"
    );
    assert_eq!(
        codex_read(&bob_codex, NOW),
        Reading::default(),
        "ada's login in bob's place states nothing for bob"
    );
    let api_key = TokenFacts {
        kind: Kind::ApiKey,
        kept: Some(Kept::LoginFile),
        issued_at: None,
        expires_at: None,
        refreshed_at: None,
        last_used: None,
        state: State::Unknown,
    };
    let keyed = codex_read(&key_codex, NOW);
    assert_eq!(
        keyed.facts,
        Some(api_key),
        "a signed-in API-key login, which no word on record is scoped to"
    );
    let wrong = codex_read(&wrong_codex, NOW);
    assert_eq!(
        wrong.facts,
        Some(api_key),
        "an API-key row that answered another account"
    );
    let mut other = codex_records.borrow().clone();
    other.state.accounts.get_mut("ada").unwrap().auth_context = None;
    let unscoped = read(
        Cli::Codex,
        &ada_codex,
        &ada_key,
        &Judged::of(&codex, &other),
        &codex,
        &none,
        NOW,
    );
    assert_eq!(
        unscoped,
        Reading {
            facts: Some(TokenFacts {
                last_used: None,
                state: State::Unknown,
                ..oauth
            }),
            turns: None,
        },
        "a word on record of other credentials says nothing of these, nor of their use"
    );
    let mut said = format!("{ada:?} {keyed:?} {wrong:?} {unscoped:?}");
    let mut held_tokens = Vec::new();
    // An access token held with no refresh token states its own end; one held with a refresh
    // token states none.
    for (holds, end, refresh_token, now, expires_at, state, turns) in [
        (
            "8 days before its end",
            NOW + 8 * DAY,
            None,
            NOW,
            at(NOW + 8 * DAY),
            State::Expiring,
            Some(NOW + 8 * DAY),
        ),
        (
            "60 days before its end",
            NOW + 60 * DAY,
            None,
            NOW,
            at(NOW + 60 * DAY),
            State::Valid,
            Some(NOW + 30 * DAY),
        ),
        (
            "past its end",
            NOW - DAY,
            None,
            NOW,
            at(NOW - DAY),
            State::Expired,
            None,
        ),
        (
            "a refresh token renews it",
            NOW - DAY,
            Some(refresh),
            NOW,
            None,
            State::Valid,
            None,
        ),
    ] {
        let held = jwt(
            "ada@x.example",
            end,
            "Qm7vXw2zRt9nKb4hLc8jPf1dGs6aYe3uWo5iNx0Bk",
        );
        write(
            &short_dir,
            codex_app_login::AUTH_FILE,
            &aterm_json::json!({"auth_mode": "chatgpt", "tokens": {
                "access_token": held, "refresh_token": refresh_token}}),
        );
        held_tokens.push(held);
        captured(&codex, &mut codex_records.borrow_mut());
        let reading = codex_read(&short, now);
        assert_eq!(
            reading,
            Reading {
                facts: Some(TokenFacts {
                    refreshed_at: None,
                    expires_at,
                    state,
                    ..oauth
                }),
                turns,
            },
            "an access token {holds}"
        );
        said.push_str(&format!(" {reading:?}"));
    }
    codex_app_login::record_refusal_for_test(&codex_dir, RefusalReason::Invalidated, NOW - 10)
        .unwrap();
    let pending = codex_read(&ada_codex, NOW);
    assert_eq!(
        pending,
        Reading {
            facts: Some(oauth),
            turns: Some(NOW + 50),
        },
        "a refusal still pending turns when it settles"
    );
    codex_app_login::record_refusal_for_test(&codex_dir, RefusalReason::Invalidated, NOW - DAY)
        .unwrap();
    let refused = codex_read(&ada_codex, NOW);
    assert_eq!(
        refused.facts,
        Some(TokenFacts {
            state: State::Refused,
            ..oauth
        }),
        "a settled refusal"
    );
    said.push_str(&format!(" {pending:?} {refused:?}"));
    let auth = codex_dir.join(codex_app_login::AUTH_FILE);
    assert_eq!(
        files(Cli::Codex, &ada_codex, &codex),
        [
            auth.clone(),
            auth.with_file_name("auth.json.refreshed"),
            codex_dir.join("config.toml"),
            home.join(".codex").join("config.toml"),
        ],
        "Codex's reader reads its auth.json, the refresh journal beside it and the configuration \
         its credential context reads"
    );

    // Claude Code: ada on her vault token, bea and cy on their stored logins.
    let claude_a = concat!("sk-ant-oat01-", "Wv4nQx8zKm1rTb6hLc3jPf9dGs2aYe5uNo7iRx0Mk");
    let claude_r = concat!("sk-ant-ort01-", "Jp2wZq7xVm4tKb9nRc6hLf1dGs8aYe3uPo5iWx0Qn");
    let bea_dir = scratch.path().join("claude-bea");
    write(
        &bea_dir,
        ".credentials.json",
        &aterm_json::json!({"claudeAiOauth": {"accessToken": claude_a,
            "refreshToken": claude_r, "expiresAt": (NOW + 3600) * 1000}}),
    );
    let cy_dir = scratch.path().join("claude-cy");
    write(
        &cy_dir,
        ".credentials.json",
        &aterm_json::json!({"claudeAiOauth": {"accessToken": claude_a,
            "expiresAt": (NOW + 10 * DAY) * 1000}}),
    );
    let claude = accounts::Store::at(&harness);
    accounts_tokens::update_table(&claude, |table| {
        table.rows.insert(
            ADA.into(),
            minted(ADA, NOW - 3 * DAY, NOW + 362 * DAY, NOW - 60),
        );
        // The token the vault still holds while this Mac's later write waits.
        table.replaced.insert(
            ADA.into(),
            minted(ADA, NOW - 200 * DAY, NOW + 100 * DAY, NOW - 600),
        );
    })
    .unwrap();
    let vault = aterm_secrets::client::Paths::locate_for_home(&home);
    let ada_claude = row("ada", &scratch.path().join("claude-ada"));
    let bea_claude = row("bea", &bea_dir);
    let cy_claude = row("cy", &cy_dir);
    // bea's earlier login, first in the roster and signed out, whose login file holds no token.
    let bea_out_dir = scratch.path().join("claude-bea-out");
    write(
        &bea_out_dir,
        ".credentials.json",
        &aterm_json::json!({"claudeAiOauth": {}}),
    );
    let bea_out = Row {
        email: Some("bea@x.example".into()),
        ..row("bea-out", &bea_out_dir)
    };
    let mut claude_records = records(
        vec![
            ada_claude.clone(),
            bea_out.clone(),
            bea_claude.clone(),
            cy_claude.clone(),
        ],
        NOW - 30 * DAY,
        NOW - 300,
    );
    claude_records
        .state
        .accounts
        .get_mut("bea-out")
        .unwrap()
        .auth = Auth::SignedOut;
    claude_records.state.tokens_here = Some(TokensHere {
        read_at: NOW - 60,
        ends: [("ada".to_owned(), NOW + 100 * DAY)].into(),
        vouched: false,
    });
    let claude_read =
        |row: &Row, key: &str, records: &AccountRecords, setup: &SetupTokens, now: i64| {
            read(
                Cli::Claude,
                row,
                &Key::new(key).unwrap(),
                &Judged::of(&claude, records),
                &claude,
                setup,
                now,
            )
        };
    let setup = SetupTokens::read(&claude, Some(&vault));
    let stored = TokenFacts {
        kind: Kind::OAuth,
        kept: Some(Kept::LoginFile),
        issued_at: at(NOW - 30 * DAY),
        expires_at: None,
        refreshed_at: None,
        last_used: at(NOW - 300),
        state: State::Valid,
    };
    let bea = claude_read(&bea_claude, BEA, &claude_records, &setup, NOW);
    assert_eq!(
        bea,
        Reading {
            facts: Some(stored),
            turns: None,
        },
        "bea's stored login, renewed by its refresh token"
    );
    let cy = claude_read(&cy_claude, CY, &claude_records, &setup, NOW);
    assert_eq!(
        cy,
        Reading {
            facts: Some(TokenFacts {
                expires_at: at(NOW + 10 * DAY),
                state: State::Expiring,
                ..stored
            }),
            turns: Some(NOW + 10 * DAY),
        },
        "cy's stored login, held with no refresh token"
    );
    let bea_out_read = claude_read(&bea_out, BEA, &claude_records, &setup, NOW);
    assert_eq!(
        bea_out_read.facts,
        Some(TokenFacts {
            state: State::Expired,
            ..stored
        }),
        "a login file that holds no token"
    );
    assert_eq!(
        files(Cli::Claude, &bea_claude, &claude),
        [bea_dir.join(".credentials.json")]
    );
    assert_eq!(
        SetupTokens::files(&claude, Some(&vault)),
        [
            claude.state_file().with_file_name("tokens.json"),
            claude.state_file().with_file_name("tokens-check.json"),
            aterm_secrets::claude_token::refusals_file(&vault),
        ],
        "the token facts' files"
    );
    said.push_str(&format!(" {bea:?} {cy:?} {bea_out_read:?}"));
    let token = TokenFacts {
        kind: Kind::SetupToken,
        kept: Some(Kept::SecretStore),
        issued_at: at(NOW - 200 * DAY),
        expires_at: at(NOW + 100 * DAY),
        refreshed_at: None,
        last_used: at(NOW - 600),
        state: State::Valid,
    };
    if crate::harness::token_route::LAUNCHES_ON_TOKEN {
        let on = claude_read(&ada_claude, ADA, &claude_records, &setup, NOW);
        assert_eq!(
            on,
            Reading {
                facts: Some(token),
                turns: Some(NOW + 70 * DAY),
            },
            "the token the reading names"
        );
        let late = NOW + 70 * DAY;
        let mut late_records = claude_records.clone();
        late_records.state.tokens_here = Some(TokensHere {
            read_at: late - 60,
            ..claude_records.state.tokens_here.clone().unwrap()
        });
        assert_eq!(
            claude_read(&ada_claude, ADA, &late_records, &setup, late),
            Reading {
                facts: Some(TokenFacts {
                    state: State::Expiring,
                    ..token
                }),
                turns: Some(NOW + 100 * DAY),
            },
            "30 days before its end"
        );
        assert_eq!(
            claude_read(&ada_claude, ADA, &claude_records, &setup, NOW + 100 * DAY)
                .facts
                .map(|facts| (facts.kind, facts.kept)),
            Some((Kind::OAuth, Some(Kept::Keychain))),
            "past her token's end she is on her stored login, which Claude Code keeps in the \
             keychain here"
        );
        said.push_str(&format!(" {on:?}"));
    }
    // ada's stored login signed out and the reading gone: off her token, the token this Mac saw
    // end is stated; once Claude Code refused it here, refused.
    let mut out_records = claude_records.clone();
    out_records.state.tokens_here = None;
    out_records.state.accounts.get_mut("ada").unwrap().auth = Auth::SignedOut;
    let ended = claude_read(&ada_claude, ADA, &out_records, &setup, NOW + 100 * DAY);
    assert_eq!(
        ended.facts,
        Some(TokenFacts {
            state: State::Expired,
            ..token
        }),
        "her stored login signed out, her token ended"
    );
    refuse(&vault, ADA, NOW - 200 * DAY, NOW + 100 * DAY, NOW - 30);
    let setup = SetupTokens::read(&claude, Some(&vault));
    let withdrawn = claude_read(&ada_claude, ADA, &out_records, &setup, NOW);
    assert_eq!(
        withdrawn.facts,
        Some(TokenFacts {
            state: State::Refused,
            ..token
        }),
        "her stored login signed out, her token refused here"
    );
    said.push_str(&format!(" {ended:?} {withdrawn:?}"));
    if crate::harness::token_route::LAUNCHES_ON_TOKEN {
        let on = claude_read(&ada_claude, ADA, &claude_records, &setup, NOW);
        assert_eq!(
            on.facts,
            Some(TokenFacts {
                state: State::Refused,
                ..token
            }),
            "a refused vault token the reading still names"
        );
        said.push_str(&format!(" {on:?}"));
    }

    // No byte of a value in any reading, or in the rows made of them.
    let values: Vec<&str> = [
        access.as_str(),
        id_token.as_str(),
        refresh,
        openai,
        claude_a,
        claude_r,
    ]
    .into_iter()
    .chain(held_tokens.iter().map(String::as_str))
    .collect();
    assert_eq!(windows(&values, &said), Vec::<String>::new(), "{said}");
    let keyer = |_: &str, email: &str| match email {
        "ada@x.example" => Some(ADA.to_owned()),
        "bea@x.example" => Some(BEA.to_owned()),
        "cy@x.example" => Some(CY.to_owned()),
        "wrong@x.example" => Some("ac-0a0b0c0d0e0f0a0b".to_owned()),
        _ => None,
    };
    let claude_judged = Judged::of(&claude, &claude_records);
    let token = |row: &Row, key: &Key| {
        read(Cli::Claude, row, key, &claude_judged, &claude, &setup, NOW).facts
    };
    let rows = crate::harness::journal::login_rows(
        Cli::Claude,
        &claude_records,
        &keyer,
        &|_| "linked",
        &token,
        NOW,
    );
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.token.is_some()), "{rows:?}");
    assert_eq!(
        rows.iter()
            .find(|row| row.account.as_str() == BEA)
            .and_then(|row| row.token),
        bea.facts,
        "bea's row states the facts of the login that speaks for her"
    );
    let codex_records = codex_records.into_inner();
    let codex_judged = Judged::of(&codex, &codex_records);
    let codex_token =
        |row: &Row, key: &Key| read(Cli::Codex, row, key, &codex_judged, &codex, &none, NOW).facts;
    let codex_rows = crate::harness::journal::login_rows(
        Cli::Codex,
        &codex_records,
        &keyer,
        &|_| "linked",
        &codex_token,
        NOW,
    );
    let wrong_row = codex_rows
        .iter()
        .find(|row| row.state == "wrong-account")
        .expect("the wrong-account API-key row");
    assert_eq!(
        wrong_row.token.map(|token| (token.kind, token.state)),
        Some((Kind::ApiKey, State::Unknown)),
        "a wrong-account API-key row is not valid"
    );
    let said = format!("{rows:?} {codex_rows:?}");
    assert_eq!(windows(&values, &said), Vec::<String>::new(), "{said}");
}
