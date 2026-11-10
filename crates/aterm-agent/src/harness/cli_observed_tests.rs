// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts observed` (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1):
//! each login place's file beside the row the roster believes is there.

use super::*;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";
const STEGO: &str = "stegosaurus.m9@proton.example";

fn home() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-observed-report-")
        .tempdir()
        .expect("temp dir")
}

fn profile(email: &str) -> String {
    format!(
        r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"11111111-2222-3333-4444-555555555555"}}}}"#
    )
}

/// THE LIVE BUG, as the verb shows it: the default row `m22` expects brontosaurus, and the
/// default home's profile names unicorn: the row is marked as contradicted, the file's address is
/// shown, and the verb parses with `--json` alone of its flags.
/// NEGATIVE CONTROL: with `agrees` taken from the row's label rather than its address, the place
/// agrees: `` left: Some(true) right: Some(false) ``.
#[test]
fn the_default_place_names_the_observed_login_beside_the_belief() {
    let home = home();
    std::fs::write(home.path().join(".claude.json"), profile(UNICORN)).unwrap();
    let roster = Roster::parse_for(
        Cli::Claude,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap();
    let mut state = accounts::State::default();
    accounts::note_session(&mut state, "s-41a2", "m22", None, 1_790_000_000);
    let rows = places_seen(&roster, &state, home.path(), 1_790_000_000);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.place, "~/.claude.json");
    assert_eq!(row.observed.as_deref(), Some(UNICORN));
    assert_eq!(row.believed.as_deref(), Some("m22"));
    assert_eq!(row.agrees, Some(false));
    assert_eq!(row.tabs, vec!["s-41a2".to_owned()]);
    assert_eq!(row.src, "profile");
    // With no default row, the default home is still a place, believed by none.
    let empty = places_seen(&Roster::default(), &state, home.path(), 1_790_000_000);
    assert_eq!(empty[0].believed, None);
    assert_eq!(empty[0].observed.as_deref(), Some(UNICORN));
    assert!(matches!(
        parsed("accounts observed --json"),
        Ok(Cmd::Accounts(AccountsCmd::Observed { json: true }))
    ));
    assert!(parsed("accounts observed --cli codex").is_err());
}

/// An unsigned stand-in for a ChatGPT token whose claims name `email`: it authenticates nothing.
fn claims_token(email: &str) -> String {
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let claims = format!(
        r#"{{"email":"{email}","https://api.openai.com/auth":{{"chatgpt_account_id":"acct-fixture","chatgpt_user_id":"user-fixture"}}}}"#
    );
    format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(claims.as_bytes())
    )
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Step 2's rows: Codex's default home beside its roster's default row, a row's own login
/// directory, agy's log and Muse Code's auth file, each by its own observer; a CLI with nothing
/// to read and no row has no row here.
/// NEGATIVE CONTROL: with `pool_places_seen` reading every place through the default home, the
/// row's own directory names the default's account: `` left: Some("brontosaurus…") right:
/// Some("unicorn…") ``.
#[test]
fn codex_agy_and_muse_places_are_observed_too() {
    let home = home();
    let token = claims_token(BRONTO);
    write(
        &home.path().join(".codex/auth.json"),
        &format!(r#"{{"tokens":{{"id_token":"{token}","access_token":"{token}"}}}}"#),
    );
    let own = home.path().join("logins/work-2");
    let token = claims_token(UNICORN);
    write(
        &own.join("auth.json"),
        &format!(r#"{{"tokens":{{"id_token":"{token}","access_token":"{token}"}}}}"#),
    );
    let roster = Roster::parse_for(
        Cli::Codex,
        &format!(
            "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{STEGO}\"\n\n\
             [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\nemail = \"{UNICORN}\"\n\
             dir = \"{}\"\n",
            own.display()
        ),
    )
    .unwrap();
    let state = accounts::State::default();
    let rows = pool_places_seen(Cli::Codex, &roster, &state, home.path(), 1_790_000_000);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].cli, "codex");
    assert_eq!(rows[0].place, "~/.codex/auth.json");
    assert_eq!(rows[0].observed.as_deref(), Some(BRONTO));
    assert_eq!(rows[0].believed.as_deref(), Some("m22"));
    assert_eq!(rows[0].agrees, Some(false));
    assert_eq!(rows[0].src, "auth-file");
    assert_eq!(rows[1].place, "~/logins/work-2/auth.json");
    assert_eq!(rows[1].observed.as_deref(), Some(UNICORN));
    assert_eq!(rows[1].agrees, Some(true));
    // No row and nothing read: no place. A row whose place holds nothing is shown unread.
    let bare = self::home();
    assert!(
        pool_places_seen(
            Cli::Codex,
            &Roster::default(),
            &state,
            bare.path(),
            1_790_000_000
        )
        .is_empty()
    );
    let listed = pool_places_seen(Cli::Codex, &roster, &state, bare.path(), 1_790_000_000);
    assert_eq!(listed[0].observed, None);
    assert_eq!(listed[0].gap, Some("unread"));
    assert_eq!(listed[0].place, "~/.codex");
    // agy's default home, by its CLI log; Muse Code's auth file, believed by no row.
    write(
        &home.path().join(".gemini/antigravity-cli/cli.log"),
        &format!("applyAuthResult: email={UNICORN}, authMethod=consumer\n"),
    );
    let agy = pool_places_seen(
        Cli::Agy,
        &Roster::default(),
        &state,
        home.path(),
        1_790_000_000,
    );
    assert_eq!(agy.len(), 1, "{agy:?}");
    assert_eq!(
        (agy[0].cli, agy[0].observed.as_deref(), agy[0].src),
        ("agy", Some(UNICORN), "cli-log")
    );
    assert_eq!(agy[0].believed, None);
    assert_eq!(muse_place_seen(home.path(), 1_790_000_000), None);
    write(
        &home.path().join(".config/muse/auth.json"),
        &format!(r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{BRONTO}"}}}}}}"#),
    );
    let muse = muse_place_seen(home.path(), 1_790_000_000).unwrap();
    assert_eq!(
        (muse.cli, muse.place.as_str(), muse.observed.as_deref()),
        ("muse", "~/.config/muse/auth.json", Some(BRONTO))
    );
    assert_eq!(muse.src, "auth-file");
}

/// Step 3's rows: Grok Build's default home beside its roster's default row, by the account its
/// `auth.json` record names; and a Kimi Code home whose token names a user and no address, shown
/// as a key-only place that names no account.
/// NEGATIVE CONTROL: with `pool_places_seen` returning no place for Grok Build and Kimi Code,
/// as before step 3: `` left: 0 right: 1 ``.
#[test]
fn grok_and_kimi_places_are_observed_too() {
    let home = home();
    write(
        &home.path().join(".grok/auth.json"),
        &format!(
            r#"{{"https://accounts.x.ai/sign-in":{{"key":"fixture-not-a-token","user_id":"grok-user-fixture","email":"{UNICORN}"}}}}"#
        ),
    );
    let roster = Roster::parse_for(
        Cli::Grok,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap();
    let state = accounts::State::default();
    let rows = pool_places_seen(Cli::Grok, &roster, &state, home.path(), 1_790_000_000);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].cli, rows[0].place.as_str(), rows[0].src),
        ("grok", "~/.grok/auth.json", "auth-file")
    );
    assert_eq!(rows[0].observed.as_deref(), Some(UNICORN));
    assert_eq!(rows[0].believed.as_deref(), Some("m22"));
    assert_eq!(rows[0].agrees, Some(false));
    assert_eq!(rows[0].gap, None);

    // An unsigned stand-in for a token whose claims name a user and no address.
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let token = format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(br#"{"user_id":"kimi-user-fixture"}"#)
    );
    write(
        &home.path().join(".kimi-code/config.toml"),
        "default_model = \"model\"\n\
         [providers.\"managed:kimi-code\"]\ntype = \"kimi\"\n\
         base_url = \"https://api.kimi.com/coding/v1\"\n\
         [providers.\"managed:kimi-code\".oauth]\nstorage = \"file\"\nkey = \"oauth/kimi-code\"\n\
         [models.model]\nprovider = \"managed:kimi-code\"\n",
    );
    write(
        &home.path().join(".kimi-code/credentials/kimi-code.json"),
        &format!(r#"{{"access_token":"{token}","token_type":"Bearer","expires_at":1}}"#),
    );
    let kimi = pool_places_seen(
        Cli::Kimi,
        &Roster::default(),
        &state,
        home.path(),
        1_790_000_000,
    );
    assert_eq!(kimi.len(), 1, "{kimi:?}");
    assert_eq!(
        (kimi[0].cli, kimi[0].place.as_str(), kimi[0].gap),
        (
            "kimi",
            "~/.kimi-code/credentials/kimi-code.json",
            Some("key-only")
        )
    );
    assert_eq!(kimi[0].observed, None);
    assert_eq!(kimi[0].src, "auth-file");
}
