// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The credential readers' tests (plan packages SF-34 and SF-34b): each client's real reader
//! turns made-up login files into facts, and no byte of a value reaches them.

use super::*;
use crate::harness::accounts::{
    Account, AccountRecords, Auth, Binding, Logins, Roster, TokensHere, Vendor,
};
use crate::harness::accounts_tokens::{self, InVault, TokenRow, UsedOn, Verification};
use crate::harness::cli::shared_provider;
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

/// `rows` as a Grok Build, Kimi Code or agy store loads them ([`records`] with no credential
/// context: only a Codex store turns `contextual_auth` on, and each word's projection stays
/// `Legacy`), each renewed at `renewed` and last served at `served`.
fn file_records(rows: Vec<Row>, renewed: i64, served: i64) -> AccountRecords {
    let mut records = records(rows, renewed, served);
    records.state.contextual_auth = false;
    records
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

/// Writes `bytes` at `path`, its folder (and those above it) made private first.
fn put(path: &std::path::Path, bytes: &[u8]) {
    aterm_types::fs_restricted::ensure_private_dir(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A made-up access token shaped as a JWT whose claims name `user` and `email`, signed `sig`, as
/// Kimi Code's own token-subject reader takes one (`observe::kimi_named`).
fn claims_token(user: &str, email: &str, sig: &str) -> String {
    format!(
        "{}.{}.{sig}",
        b64url(br#"{"alg":"RS256","typ":"JWT"}"#),
        b64url(format!(r#"{{"user_id":"{user}","email":"{email}"}}"#).as_bytes())
    )
}

/// The lab key of `email`: each address its own.
fn key_of(email: &str) -> String {
    let hash = email.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("ac-{hash:016x}")
}

/// `label`'s Kimi Code row of the global account system, its login in `dir`.
fn kimi_row(label: &str, dir: &std::path::Path) -> Row {
    Row {
        vendor: Vendor {
            region: Some(Region::Global),
            flow: None,
        },
        ..row(label, dir)
    }
}

/// The lab key `row` is keyed by: its address's, else its label's.
fn row_key(row: &Row) -> Key {
    Key::new(&key_of(row.email.as_deref().unwrap_or(&row.label))).unwrap()
}

/// `label`'s row on its CLI's default login home: no directory of its own.
fn default_row(label: &str) -> Row {
    Row {
        label: label.into(),
        email: Some(format!("{label}@x.example")),
        ..Row::default()
    }
}

/// `cli`'s login rows of `records` (one read of `store`) at `NOW`, each with the facts [`read`]
/// states of its credential, as the login source makes them.
fn rows_of(
    cli: Cli,
    store: &Store,
    records: &AccountRecords,
) -> Vec<crate::harness::journal::LoginRow> {
    let judged = Judged::of(store, records);
    let none = SetupTokens::default();
    let token = |row: &Row, key: &Key| read(cli, row, key, &judged, store, &none, NOW).facts;
    crate::harness::journal::login_rows(
        cli,
        records,
        &|_, email| Some(key_of(email)),
        &|_| "linked",
        &token,
        NOW,
    )
}

/// GROK BUILD, KIMI CODE AND AGY STATE THE TOKEN BEHIND EACH SIGN-IN WITHOUT ITS VALUE (plan
/// package SF-34b), under the rule every client follows
/// (`each_clients_reader_states_its_credentials_facts_and_no_byte_of_its_value`): each an `oauth`
/// login in its `login-file`, the one in the row's own login home, issued at the renewal that
/// made it, last used at its last served turn, with no refresh time, since no field their
/// readers read states one; `valid` or `expiring` only where its login serves by its word on
/// record. Each store's records are as it loads them, with no credential context
/// ([`file_records`]).
///
/// Grok Build: gia's `auth.json` in her login home, and gdef's in the default home `~/.grok`,
/// each holding a made-up `key`, `valid` with no end, since its reader reads none; gio's, whose
/// record keeps its made-up tokens under names this reader does not read and states no `key`,
/// and gsi's, the harness's own stand-in login, `valid` too, since the harness counts a non-empty
/// file grok has not refused as a login grok starts on; gia's login in gus's place (its record
/// names her address) states nothing for gus; gil's, over which grok drew its sign-in (its
/// `auth.json.refused` newer than it), `refused`; gwen's, signed out here, `expired`; gem's,
/// whose record states an empty `key`, `expired`; gia's with no word on record, `unknown`, its
/// last use still its last served turn, as a store with no credential context keeps it. Kimi
/// Code: kai's credential file of her region's slot, with a refresh token and an access token
/// whose claims name her and whose end passed, `valid` with no end, since its CLI renews it;
/// kai's file in kay's place states nothing for kay; kit's, an opaque access token with no
/// refresh token, states its end, `expiring` 8 days before it, `valid` 60 days before it,
/// `expired` past it; kev's, a row with no region and so no address (as the roster requires), in
/// the file its connector names, `valid`; kim's, whose refresh grant the issuer refused (the
/// refresh's record beside it, both tokens left empty), `refused`, and with that record gone
/// `expired`, since the file holds no credential. agy: ana's grant with a refresh token, its
/// access token's `expiry` past, `valid` with no end; abe's with none, its `expiry` written in a
/// zone 8 hours behind UTC, states that end, `expiring`; amy's empty file, `expired`, since it
/// holds no credential; adef's in the person's own home, `valid`. Every reading is compared
/// whole, with the clock's turns, and the files each reader reads are named for a reader that
/// stamps them. No 8-byte window of any made-up value is in any reading, nor in the login rows
/// `login_rows` makes of them; the readers return no error text at all, only `Option`s of closed
/// words and seconds.
#[test]
fn grok_kimi_and_agy_state_the_token_behind_each_sign_in_without_its_value() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let at = |t: i64| Some(u64::try_from(t).unwrap());
    let json = |doc: &aterm_json::Value| doc.to_string().into_bytes();
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    aterm_types::fs_restricted::ensure_private_dir(&home).unwrap();
    let none = SetupTokens::default();
    let oauth = TokenFacts {
        kind: Kind::OAuth,
        kept: Some(Kept::LoginFile),
        issued_at: at(NOW - 20 * DAY),
        expires_at: None,
        refreshed_at: None,
        last_used: at(NOW - 900),
        state: State::Valid,
    };
    let reading = |facts: TokenFacts| Reading {
        facts: Some(facts),
        turns: None,
    };
    let mut said = String::new();

    // Grok Build.
    let grok_key = concat!("gk-", "Vb3nKq8wZt1mXr6hLc9jPf2dGs5aYe7uNo4iRx0Qw");
    let grok_doc = |user: &str, held: &str| {
        json(&aterm_json::json!({"https://accounts.x.ai/sign-in": {
            "key": held, "user_id": format!("grok-user-{user}"),
            "email": format!("{user}@x.example")}}))
    };
    let grok_dir = |label: &str| scratch.path().join(format!("grok-{label}"));
    put(
        &grok_dir("gia").join("auth.json"),
        &grok_doc("gia", grok_key),
    );
    put(
        &grok_dir("gus").join("auth.json"),
        &grok_doc("gia", grok_key),
    );
    put(
        &grok_dir("gil").join("auth.json"),
        &grok_doc("gil", grok_key),
    );
    put(
        &grok_dir("gwen").join("auth.json"),
        &grok_doc("gwen", grok_key),
    );
    put(&grok_dir("gem").join("auth.json"), &grok_doc("gem", ""));
    // gio's record keeps its tokens under names this reader does not read, and states no `key`.
    let grok_access = concat!("ga-", "Rt7wMq2xLz5tKb8nPc3hVf6dGs1aYe9uNo4iJx0Bm");
    let grok_refresh = concat!("gr-", "Wn4wHq9xKz2tKb5nTc8hRf3dGs6aYe1uLo7iPx0Cv");
    put(
        &grok_dir("gio").join("auth.json"),
        &json(&aterm_json::json!({"https://accounts.x.ai/sign-in": {
            "user_id": "grok-user-gio", "email": "gio@x.example",
            "access_token": grok_access, "refresh_token": grok_refresh}})),
    );
    // gsi's is the harness's own stand-in login (`grok_signin`'s tests sign in with it).
    put(&grok_dir("gsi").join("auth.json"), br#"{"stand-in":true}"#);
    put(
        &home.join(".grok").join("auth.json"),
        &grok_doc("gdef", grok_key),
    );
    // Grok drew its sign-in over gil's login after it was written.
    let gil_file = grok_dir("gil").join("auth.json");
    std::fs::File::options()
        .write(true)
        .open(&gil_file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
        .unwrap();
    crate::harness::grok_signin::mark_refused(&gil_file).unwrap();
    let grok = accounts::Store::at_for(&harness, Cli::Grok, &home);
    let gia = row("gia", &grok_dir("gia"));
    let gus = row("gus", &grok_dir("gus"));
    let gil = row("gil", &grok_dir("gil"));
    let gwen = row("gwen", &grok_dir("gwen"));
    let gem = row("gem", &grok_dir("gem"));
    let gio = row("gio", &grok_dir("gio"));
    let gsi = row("gsi", &grok_dir("gsi"));
    let gdef = default_row("gdef");
    let mut grok_records = file_records(
        vec![
            gia.clone(),
            gus.clone(),
            gil.clone(),
            gwen.clone(),
            gem.clone(),
            gio.clone(),
            gsi.clone(),
            gdef.clone(),
        ],
        NOW - 20 * DAY,
        NOW - 900,
    );
    grok_records.state.accounts.get_mut("gwen").unwrap().auth = Auth::SignedOut;
    let mut grok_read = |row: &Row, records: &AccountRecords| {
        let got = read(
            Cli::Grok,
            row,
            &row_key(row),
            &Judged::of(&grok, records),
            &grok,
            &none,
            NOW,
        );
        said.push_str(&format!(" {got:?}"));
        got
    };
    for (row, want, holds) in [
        (
            &gia,
            reading(oauth),
            "gia's login: no end, since its file states none",
        ),
        (
            &gdef,
            reading(oauth),
            "the default home's login, in ~/.grok",
        ),
        (
            &gus,
            Reading::default(),
            "gia's login in gus's place states nothing for gus",
        ),
        (
            &gil,
            reading(TokenFacts {
                state: State::Refused,
                ..oauth
            }),
            "a login grok refused",
        ),
        (
            &gwen,
            reading(TokenFacts {
                state: State::Expired,
                ..oauth
            }),
            "a login signed out here",
        ),
        (
            &gem,
            reading(TokenFacts {
                state: State::Expired,
                ..oauth
            }),
            "a login file whose record states an empty key",
        ),
        (
            &gio,
            reading(oauth),
            "a login file that states no key: held, as the harness counts it",
        ),
        (
            &gsi,
            reading(oauth),
            "the harness's own stand-in login: held, as the harness counts it",
        ),
    ] {
        assert_eq!(grok_read(row, &grok_records), want, "{holds}");
    }
    let mut unseen = grok_records.clone();
    unseen.state.accounts.get_mut("gia").unwrap().auth = Auth::Unseen;
    assert_eq!(
        grok_read(&gia, &unseen),
        reading(TokenFacts {
            state: State::Unknown,
            ..oauth
        }),
        "no word on record says the login serves; its last served turn stands, as a store with \
         no credential context keeps it"
    );
    assert_eq!(
        files(Cli::Grok, &gia, &grok),
        [
            grok_dir("gia").join("auth.json"),
            grok_dir("gia").join("auth.json.refused")
        ],
        "Grok's reader reads its auth.json and the refusal marker beside it"
    );

    // Kimi Code.
    let kimi_access = claims_token(
        "kimi-user-kai",
        "kai@x.example",
        "Mw8vQx3zRt6nKb1hLc4jPf9dGs2aYe7uWo5iNx0Hn",
    );
    let kimi_refresh = concat!("kr-", "Tz6wNq1xVm9tKb4nRc7hLf2dGs5aYe8uPo3iWx0Jk");
    let kimi_opaque = concat!("ko-", "Pq2wXz7mRv4tKb9nLc1hJf6dGs3aYe8uNo5iMx0Vt");
    let kimi_dir = |label: &str| scratch.path().join(format!("kimi-{label}"));
    let kimi_file =
        |label: &str| shared_provider::kimi_credential_path(&kimi_dir(label), Region::Global);
    let kai_doc = json(&aterm_json::json!({
        "access_token": kimi_access, "refresh_token": kimi_refresh, "token_type": "Bearer",
        "expires_at": NOW - 60, "expires_in": 900, "scope": "kimi-code"}));
    put(&kimi_file("kai"), &kai_doc);
    put(&kimi_file("kay"), &kai_doc);
    // kev's access token is no set of claims, so it names no account: the row's own home is
    // what makes it hers.
    put(
        &kimi_dir("kev").join("credentials").join("kimi-code.json"),
        &json(&aterm_json::json!({
            "access_token": kimi_opaque, "refresh_token": kimi_refresh, "token_type": "Bearer",
            "expires_at": NOW - 60, "expires_in": 900, "scope": "kimi-code"})),
    );
    // kim's refresh grant, refused by the issuer: the refresh records it and leaves both tokens
    // empty.
    drop(shared_provider::kimi_proof_fixture(
        &kimi_dir("kim"),
        Region::Global,
        "kim@x.example",
    ));
    shared_provider::expire_kimi_pending(&kimi_dir("kim"), Region::Global).unwrap();
    shared_provider::kimi_rejection_fixture(&kimi_dir("kim"), Region::Global, "kim@x.example");
    let kimi = accounts::Store::at_for(&harness, Cli::Kimi, &home);
    let kai = kimi_row("kai", &kimi_dir("kai"));
    let kay = kimi_row("kay", &kimi_dir("kay"));
    let kit = kimi_row("kit", &kimi_dir("kit"));
    let kim = kimi_row("kim", &kimi_dir("kim"));
    // A Kimi Code row with no region names no address, as the roster requires.
    let kev = Row {
        label: "kev".into(),
        dir: Some(kimi_dir("kev").to_string_lossy().into_owned()),
        ..Row::default()
    };
    let kimi_records = file_records(
        vec![
            kai.clone(),
            kay.clone(),
            kit.clone(),
            kim.clone(),
            kev.clone(),
        ],
        NOW - 20 * DAY,
        NOW - 900,
    );
    let mut kimi_read = |row: &Row, now: i64| {
        let got = read(
            Cli::Kimi,
            row,
            &row_key(row),
            &Judged::of(&kimi, &kimi_records),
            &kimi,
            &none,
            now,
        );
        said.push_str(&format!(" {got:?}"));
        got
    };
    assert_eq!(
        kimi_read(&kai, NOW),
        reading(oauth),
        "a grant its CLI renews states no end, though its access token's passed"
    );
    assert_eq!(
        kimi_read(&kay, NOW),
        Reading::default(),
        "kai's login in kay's place states nothing for kay"
    );
    assert_eq!(
        kimi_read(&kev, NOW),
        reading(oauth),
        "a row with no region, in the file its connector names"
    );
    for (holds, end, expires_at, state, turns) in [
        (
            "8 days before its end",
            NOW + 8 * DAY,
            at(NOW + 8 * DAY),
            State::Expiring,
            Some(NOW + 8 * DAY),
        ),
        (
            "60 days before its end",
            NOW + 60 * DAY,
            at(NOW + 60 * DAY),
            State::Valid,
            Some(NOW + 30 * DAY),
        ),
        (
            "past its end",
            NOW - DAY,
            at(NOW - DAY),
            State::Expired,
            None,
        ),
    ] {
        put(
            &kimi_file("kit"),
            &json(&aterm_json::json!({
                "access_token": kimi_opaque, "refresh_token": "", "token_type": "Bearer",
                "expires_at": end, "expires_in": 900, "scope": "kimi-code"})),
        );
        assert_eq!(
            kimi_read(&kit, NOW),
            Reading {
                facts: Some(TokenFacts {
                    expires_at,
                    state,
                    ..oauth
                }),
                turns,
            },
            "an access token held with no refresh token, {holds}"
        );
    }
    assert_eq!(
        kimi_read(&kim, NOW),
        reading(TokenFacts {
            state: State::Refused,
            ..oauth
        }),
        "a grant the issuer refused, its refusal recorded beside it"
    );
    let refusal = shared_provider::kimi_rejection_file(&kimi_dir("kim"), Region::Global);
    assert_eq!(
        files(Cli::Kimi, &kim, &kimi),
        [
            kimi_file("kim"),
            kimi_dir("kim").join("config.toml"),
            refusal.clone()
        ],
        "Kimi's reader reads its region's credential file, and its configuration and refusal \
         record for its refusal"
    );
    assert_eq!(
        files(Cli::Kimi, &kev, &kimi),
        [
            kimi_dir("kev").join("credentials").join("kimi-code.json"),
            kimi_dir("kev").join("credentials")
        ],
        "a row with no region: the file its connector names, and the folder the others are \
         written into"
    );
    std::fs::remove_file(&refusal).unwrap();
    assert_eq!(
        kimi_read(&kim, NOW),
        reading(TokenFacts {
            state: State::Expired,
            ..oauth
        }),
        "a file the refusal left with no credential, its record gone"
    );

    // agy.
    let agy_access = concat!("ya-", "Hn5wRq2xZm8tKb3nVc6hLf1dGs9aYe4uPo7iWx0Ds");
    let agy_refresh = concat!("yr-", "Kc9wTq4xNm1tKb6nZc3hLf8dGs2aYe5uPo7iRx0Fv");
    let agy_grant = |dir: &std::path::Path| {
        dir.join(".gemini")
            .join("antigravity-cli")
            .join("antigravity-oauth-token")
    };
    let agy_dir = |label: &str| scratch.path().join(format!("agy-{label}"));
    let held = |label: &str| {
        json(&aterm_json::json!({"auth_method": "consumer",
            "id_token_email": format!("{label}@x.example"), "token": {
            "access_token": agy_access, "token_type": "Bearer", "refresh_token": agy_refresh,
            "expiry": aterm_types::rfc3339::format_rfc3339(u64::try_from(NOW - 60).unwrap())}}))
    };
    put(&agy_grant(&agy_dir("ana")), &held("ana"));
    put(&agy_grant(&home), &held("adef"));
    // abe's grant holds no refresh token, its end written 8 hours behind UTC, as Go writes a
    // `time.Time` made in that zone.
    let behind =
        aterm_types::rfc3339::format_rfc3339(u64::try_from(NOW + 10 * DAY - 8 * 3600).unwrap())
            .replace('Z', ".123456789-08:00");
    put(
        &agy_grant(&agy_dir("abe")),
        &json(&aterm_json::json!({"auth_method": "consumer", "token": {
            "access_token": agy_access, "token_type": "Bearer", "expiry": behind}})),
    );
    put(&agy_grant(&agy_dir("amy")), b"");
    let agy = accounts::Store::at_for(&harness, Cli::Agy, &home);
    let ana = row("ana", &agy_dir("ana"));
    let abe = row("abe", &agy_dir("abe"));
    let amy = row("amy", &agy_dir("amy"));
    let adef = default_row("adef");
    let agy_records = file_records(
        vec![ana.clone(), abe.clone(), amy.clone(), adef.clone()],
        NOW - 20 * DAY,
        NOW - 900,
    );
    let mut agy_read = |row: &Row| {
        let got = read(
            Cli::Agy,
            row,
            &row_key(row),
            &Judged::of(&agy, &agy_records),
            &agy,
            &none,
            NOW,
        );
        said.push_str(&format!(" {got:?}"));
        got
    };
    assert_eq!(
        agy_read(&ana),
        reading(oauth),
        "a grant with a refresh token states no end, though its access token's passed"
    );
    assert_eq!(
        agy_read(&adef),
        reading(oauth),
        "the person's own home's grant"
    );
    assert_eq!(
        agy_read(&abe),
        Reading {
            facts: Some(TokenFacts {
                expires_at: at(NOW + 10 * DAY),
                state: State::Expiring,
                ..oauth
            }),
            turns: Some(NOW + 10 * DAY),
        },
        "a grant with no refresh token states its access token's end, read in its zone"
    );
    assert_eq!(
        agy_read(&amy),
        reading(TokenFacts {
            state: State::Expired,
            ..oauth
        }),
        "an empty file holds no credential"
    );
    assert_eq!(
        files(Cli::Agy, &ana, &agy),
        [agy_grant(&agy_dir("ana"))],
        "agy's reader reads its grant"
    );

    // No byte of a value in any reading, or in the rows made of them.
    let values = [
        grok_key,
        grok_access,
        grok_refresh,
        kimi_access.as_str(),
        kimi_refresh,
        kimi_opaque,
        agy_access,
        agy_refresh,
    ];
    assert_eq!(windows(&values, &said), Vec::<String>::new(), "{said}");
    let grok_rows = rows_of(Cli::Grok, &grok, &grok_records);
    let kimi_rows = rows_of(Cli::Kimi, &kimi, &kimi_records);
    let agy_rows = rows_of(Cli::Agy, &agy, &agy_records);
    let stated = |rows: &[crate::harness::journal::LoginRow], email: &str| {
        rows.iter()
            .find(|row| row.account.as_str() == key_of(email))
            .and_then(|row| row.token)
    };
    assert_eq!(stated(&grok_rows, "gia@x.example"), Some(oauth));
    assert_eq!(stated(&grok_rows, "gus@x.example"), None);
    assert_eq!(stated(&grok_rows, "gio@x.example"), Some(oauth));
    assert_eq!(
        stated(&kimi_rows, "kim@x.example").map(|facts| facts.state),
        Some(State::Expired)
    );
    assert_eq!(
        stated(&agy_rows, "abe@x.example").map(|facts| (facts.kind, facts.state)),
        Some((Kind::OAuth, State::Expiring))
    );
    let said = format!("{grok_rows:?} {kimi_rows:?} {agy_rows:?}");
    assert_eq!(windows(&values, &said), Vec::<String>::new(), "{said}");
}

/// A LOGIN FILE THESE READERS DO NOT READ STATES NOTHING AND NO BYTE OF IT: for each of Grok
/// Build, Kimi Code and agy, a file that is not there, one larger than the readers read
/// (`stored::MAX`), one cut off mid-value, one that is no JSON object, a link to a good one (the
/// readers open no final link), and an agy grant with no `token`, each read through the row's
/// reader as no facts at all; each file holds a made-up value, and no 8-byte window of it is in
/// what the readers return.
#[test]
fn a_login_file_these_readers_do_not_read_states_nothing_and_no_byte_of_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    aterm_types::fs_restricted::ensure_private_dir(&home).unwrap();
    let value = concat!("bad-", "Zr4wLq9xTm2tKb7nHc5hVf3dGs8aYe1uPo6iNx0Wq");
    let padded = format!(
        r#"{{"key":"{value}","access_token":"{value}","token":{{"access_token":"{value}"}},"pad":"{}"}}"#,
        "a".repeat(usize::try_from(stored::MAX).unwrap())
    );
    let good = format!(
        r#"{{"key":"{value}","access_token":"{value}","token":{{"access_token":"{value}"}}}}"#
    );
    let string = format!(r#""{value}""#);
    let mut said = String::new();
    for cli in [Cli::Grok, Cli::Kimi, Cli::Agy] {
        let store = accounts::Store::at_for(&harness, cli, &home);
        let name = cli.connector().stores[0];
        let mut rows = Vec::new();
        for (label, bytes) in [
            ("missing", None),
            ("oversized", Some(padded.as_bytes())),
            ("cut", Some(&good.as_bytes()[..good.len() / 2])),
            ("string", Some(string.as_bytes())),
            ("linked", None),
            (
                "tokenless",
                Some(br#"{"auth_method":"consumer"}"#.as_slice()),
            ),
        ] {
            // Only agy's grant needs a `token`. An object with no token is a Kimi file that holds
            // no credential (`expired`, as kim's emptied one in the exit test), and a Grok file
            // whose token its reader cannot read, held as the harness counts it (gio's there).
            if label == "tokenless" && cli != Cli::Agy {
                continue;
            }
            let dir = scratch.path().join(format!("{}-{label}", cli.id()));
            aterm_types::fs_restricted::ensure_private_dir(&dir).unwrap();
            let file = match cli {
                Cli::Kimi => shared_provider::kimi_credential_path(&dir, Region::Global),
                _ => dir.join(name),
            };
            if let Some(bytes) = bytes {
                put(&file, bytes);
            }
            if label == "linked" {
                let target = scratch.path().join(format!("{}-target.json", cli.id()));
                std::fs::write(&target, &good).unwrap();
                aterm_types::fs_restricted::ensure_private_dir(file.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&target, &file).unwrap();
            }
            let row = Row {
                vendor: Vendor {
                    region: (cli == Cli::Kimi).then_some(Region::Global),
                    flow: None,
                },
                ..row(label, &dir)
            };
            rows.push((label, row));
        }
        let all: Vec<Row> = rows.iter().map(|(_, row)| row.clone()).collect();
        let records = file_records(all, NOW - 20 * DAY, NOW - 900);
        let judged = Judged::of(&store, &records);
        for (label, row) in &rows {
            let read = read(
                cli,
                row,
                &row_key(row),
                &judged,
                &store,
                &SetupTokens::default(),
                NOW,
            );
            assert_eq!(read, Reading::default(), "{} {label}", cli.id());
            said.push_str(&format!(" {read:?}"));
        }
    }
    for read in [
        format!(
            "{:?}",
            stored::grok(&scratch.path().join("grok-cut").join("auth.json"), None)
        ),
        format!(
            "{:?}",
            stored::kimi(&scratch.path().join("grok-cut").join("auth.json"), None)
        ),
        format!(
            "{:?}",
            stored::agy(&scratch.path().join("grok-cut").join("auth.json"))
        ),
    ] {
        assert_eq!(read, "None");
        said.push_str(&read);
    }
    assert_eq!(windows(&[value], &said), Vec::<String>::new(), "{said}");
}

/// A LOGIN FILE IS READ AS LONG AS ITS LENGTH SAYS, OR NOT AT ALL: the readers read a file into
/// one buffer the length its open file's metadata gave, and a file that grew after that (more
/// bytes than it said) or shrank (fewer) is no file they read, since what they would parse is not
/// the file that length described and the buffer would have to grow; a read that fails reads
/// nothing, and one interrupted is made again.
#[test]
fn a_login_file_is_read_as_long_as_its_length_says_or_not_at_all() {
    let file = br#"{"key":"made-up"}"#.as_slice();
    let read = |len: usize| stored::whole(file, len).map(|bytes| bytes.expose().to_vec());
    assert_eq!(read(file.len()), Some(file.to_vec()), "as long as it says");
    assert_eq!(
        read(file.len() - 1),
        None,
        "it grew after its length was read"
    );
    assert_eq!(read(0), None, "it grew from nothing");
    assert_eq!(read(file.len() + 1), None, "it shrank");
    assert_eq!(
        stored::whole(b"".as_slice(), 0).map(|bytes| bytes.len()),
        Some(0),
        "an empty file"
    );

    /// A reader that is interrupted once, then reads `bytes` a byte at a time, then ends, or
    /// fails where `fails`.
    struct Torn<'a> {
        interrupted: bool,
        bytes: &'a [u8],
        fails: bool,
    }
    impl std::io::Read for Torn<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            match self.bytes.split_first() {
                Some((first, rest)) => {
                    buf[0] = *first;
                    self.bytes = rest;
                    Ok(1)
                }
                None if self.fails => Err(std::io::ErrorKind::Other.into()),
                None => Ok(0),
            }
        }
    }
    let torn = |fails: bool| {
        stored::whole(
            Torn {
                interrupted: false,
                bytes: file,
                fails,
            },
            file.len(),
        )
        .map(|bytes| bytes.expose().to_vec())
    };
    assert_eq!(
        torn(false),
        Some(file.to_vec()),
        "a read interrupted is made again"
    );
    assert_eq!(torn(true), None, "a read that fails reads nothing");
}

/// GO'S TIMES ARE READ IN ANY ZONE: agy's grant writes its end as Go's `time.Time` does, in the
/// zone it was made in, with nanoseconds where it has any, so `Z` and a `+HH:MM` or `-HH:MM`
/// offset each name the same second; a zone that is no offset, or out of range, names none, and
/// Go's zero time is no end.
#[test]
fn go_times_are_read_in_any_zone() {
    let utc = stored::rfc3339("2026-11-18T18:00:00Z").unwrap();
    for text in [
        "2026-11-18T18:00:00Z",
        "2026-11-18T10:00:00-08:00",
        "2026-11-18T10:00:00.987654321-08:00",
        "2026-11-18T23:30:00+05:30",
    ] {
        assert_eq!(stored::rfc3339(text), Some(utc), "{text}");
    }
    for text in [
        "2026-11-18T10:00:00+0800",
        "2026-11-18T10:00:00-24:00",
        "2026-11-18T10:00:00-08:60",
        "2026-11-18T10:00:00*08:00",
        "2026-11-18T10:00:00",
        "",
    ] {
        assert_eq!(stored::rfc3339(text), None, "{text}");
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let grant = scratch.path().join("antigravity-oauth-token");
    put(
        &grant,
        br#"{"auth_method":"consumer","token":{"access_token":"a","expiry":"0001-01-01T00:00:00Z"}}"#,
    );
    assert_eq!(
        stored::agy(&grant),
        Some(stored::Stored {
            held: true,
            refreshable: false,
            expires_at: None,
        }),
        "Go's zero time is no end"
    );
}
