// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Codex row signed out here says why, where the issuer's refusal of the refresh token its
//! login holds is on record beside that login: `signed-out (token expired)` in
//! `accounts --cli codex`, its table and `accounts observed`, and `signed_out_reason` in their
//! JSON. The reason annotates the condition and changes none: a row signed out with nothing on
//! record, or with a refusal not yet settled, reads plain `signed-out`, and a refusal on record
//! beside a row not signed out here makes it neither signed out nor annotated.

use super::*;
use crate::harness::codex_app_login::{self, RefusalReason};

struct NoProvider;

impl AccountsHost for NoProvider {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider IO")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
}

/// A Codex pool of `lost` (signed out here, its refresh token refused as expired on record and
/// settled), `gone` (signed out here, nothing on record of why), `fresh` (signed out here, its
/// refusal just made and not yet settled) and `live` (a settled refusal on record, and never
/// signed out here), each on a login directory of its own, and `base`, the row on the default
/// login (`base@example.test`; signed out here, the refusal of the token at the default home on
/// record and settled), whose default home holds a login of `default_names`. A source table
/// names none of them, so `accounts table` lists all of them as this Mac's own.
fn pool(tag: &str, default_names: &str) -> (Tmp, Env) {
    let tmp = tmp_dir(tag);
    let env = env_at(&tmp);
    let home = env.home.clone().expect("a home");
    std::fs::create_dir_all(home.join(".secrets")).expect("mkdir");
    aterm_uds::atomic_file::write(
        env.config.as_ref().expect("a config"),
        b"[fleet]\nmachine = \"m16\"\n",
        0o600,
    )
    .expect("config");
    std::fs::write(
        home.join(".secrets/ai-accounts.csv"),
        "email,computer,codex\nelsewhere@example.test,m22,1\n",
    )
    .expect("source table");
    let login = |dir: &Path, names: &str, refresh: &str| {
        let access = codex_app_login::tests::make_chatgpt_jwt(Some(names), "pro", env.now + 3600);
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(
            dir.join(codex_app_login::AUTH_FILE),
            aterm_json::to_string(&aterm_json::json!({"auth_mode":"chatgpt", "tokens":{
                "id_token":access, "access_token":access,
                "refresh_token":refresh, "account_id":"acct_fixture"
            }}))
            .expect("JSON"),
        )
        .expect("login");
    };
    let mut rows = Vec::new();
    let mut dirs = Vec::new();
    for label in ["lost", "gone", "fresh", "live"] {
        let dir = home.join("l").join(label);
        login(
            &dir,
            &format!("{label}@example.test"),
            &format!("{label}-refresh"),
        );
        rows.push(accounts::Row {
            label: label.into(),
            dir: Some(dir.to_str().expect("UTF-8").to_owned()),
            ..accounts::Row::default()
        });
        dirs.push(dir);
    }
    let default_home = home.join(Cli::Codex.accounts().default_home);
    login(&default_home, default_names, "base-refresh");
    rows.push(accounts::Row {
        label: "base".into(),
        email: Some("base@example.test".into()),
        ..accounts::Row::default()
    });
    dirs.push(default_home);
    let roster = env.accounts_roster_for(Cli::Codex).expect("a roster");
    std::fs::create_dir_all(roster.parent().expect("a folder")).expect("mkdir");
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Codex, "", &rows).expect("the rows"),
    )
    .expect("roster");
    let store = env.accounts_store_for(Cli::Codex).expect("a store");
    for row in [&rows[0], &rows[1], &rows[2], &rows[4]] {
        store
            .record_lapse_for_lab(row, env.now)
            .expect("signed out here");
    }
    let settled = env.now - i64::try_from(codex_app_login::ROTATION_HANG.as_secs()).expect("s");
    for (dir, at) in [
        (&dirs[0], settled),
        (&dirs[2], env.now),
        (&dirs[3], settled),
        (&dirs[4], settled),
    ] {
        codex_app_login::record_refusal_for_test(dir, RefusalReason::Expired, at)
            .expect("the refusal");
    }
    (tmp, env)
}

fn run(form: AccountsCmd, env: &Env) -> String {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let of = AccountsOf {
        cli: Cli::Codex,
        form,
        vendor: accounts::Vendor::default(),
    };
    let code = run_accounts_of(&of, env, &NoProvider, &mut out, &mut err);
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    String::from_utf8(out).expect("UTF-8")
}

/// The line of `text` that names `label` first.
fn line_of<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|line| line.split_whitespace().any(|word| word == label))
        .unwrap_or_else(|| panic!("no {label} in {text}"))
}

/// `field` of the row `label` names in `rows`, as the report's words.
fn field(rows: &Value, label: &str, key: &str) -> Value {
    rows.as_array()
        .expect("rows")
        .iter()
        .find(|row| row["label"] == Value::from(label) || row["believed"] == Value::from(label))
        .unwrap_or_else(|| panic!("no {label} in {rows}"))[key]
        .clone()
}

/// The rows signed out here that say why: one on a login directory of its own, and the row on
/// the default login, whose refusal is read at the default home the accounts store names.
const SAYS: [&str; 2] = ["lost", "base"];

/// The rows signed out here that say no reason: nothing is on record of why, or the refusal on
/// record is not yet settled.
const PLAIN: [&str; 2] = ["gone", "fresh"];

/// THE LIST SAYS WHY: `accounts --cli codex` writes `signed-out (token expired)` for the row whose
/// settled refusal is on record and `signed_out_reason` in its JSON; a row signed out with nothing
/// settled on record stays plain `signed-out`, and the row not signed out keeps its own condition.
#[test]
fn the_codex_list_says_why_a_row_is_signed_out() {
    let (_tmp, env) = pool("signed-out-reason-list", "base@example.test");
    let ((text, json), reads) = crate::harness::provider_read::captured(|| {
        (
            run(AccountsCmd::Report { json: false }, &env),
            run(AccountsCmd::Report { json: true }, &env),
        )
    });
    assert_eq!(reads, [], "a local read asks no provider");
    for label in SAYS {
        assert!(
            line_of(&text, label).contains(" signed-out (token expired)"),
            "{label}: {text}"
        );
    }
    for label in PLAIN {
        assert!(line_of(&text, label).contains(" signed-out "), "{text}");
        assert!(!line_of(&text, label).contains(" signed-out ("), "{text}");
    }
    assert!(!line_of(&text, "live").contains(" signed-out"), "{text}");
    let doc: Value = aterm_json::from_str(&json).expect("JSON");
    let rows = &doc["accounts"];
    for label in SAYS {
        assert_eq!(field(rows, label, "condition"), Value::from("signed-out"));
        assert_eq!(
            field(rows, label, "signed_out_reason"),
            Value::from("refresh_token_expired"),
            "{label}"
        );
    }
    for label in PLAIN {
        assert_eq!(field(rows, label, "condition"), Value::from("signed-out"));
        assert_eq!(field(rows, label, "signed_out_reason"), Value::Null);
    }
    assert_ne!(field(rows, "live", "condition"), Value::from("signed-out"));
    assert_eq!(field(rows, "live", "signed_out_reason"), Value::Null);
}

/// THE TABLE SAYS WHY, as the list does: `signed-out (token expired)` in its readiness text,
/// the readiness word itself unchanged in its JSON beside `signed_out_reason`.
#[test]
fn the_codex_table_says_why_a_row_is_signed_out() {
    let (_tmp, env) = pool("signed-out-reason-table", "base@example.test");
    let text = run(AccountsCmd::Table { json: false }, &env);
    for label in SAYS {
        assert!(
            line_of(&text, label).ends_with("signed-out (token expired)"),
            "{label}: {text}"
        );
    }
    for label in PLAIN {
        assert!(line_of(&text, label).ends_with("signed-out"), "{text}");
    }
    let json = run(AccountsCmd::Table { json: true }, &env);
    let doc: Value = aterm_json::from_str(&json).expect("JSON");
    let rows = &doc["pools"][0]["accounts"];
    for label in SAYS {
        assert_eq!(field(rows, label, "readiness"), Value::from("signed-out"));
        assert_eq!(
            field(rows, label, "signed_out_reason"),
            Value::from("refresh_token_expired"),
            "{label}"
        );
    }
    for label in PLAIN {
        assert_eq!(field(rows, label, "readiness"), Value::from("signed-out"));
        assert_eq!(field(rows, label, "signed_out_reason"), Value::Null);
    }
    assert_ne!(field(rows, "live", "readiness"), Value::from("signed-out"));
    assert_eq!(field(rows, "live", "signed_out_reason"), Value::Null);
}

/// THE OBSERVED PLACES SAY WHY: a Codex row signed out here reads `signed-out` in AUTH, with its
/// reason where a settled one is on record; a row not signed out here says nothing in AUTH.
#[test]
fn the_observed_codex_places_say_why_a_row_is_signed_out() {
    let (_tmp, env) = pool("signed-out-reason-observed", "base@example.test");
    let (mut text, mut json) = (Vec::new(), Vec::new());
    assert_eq!(
        run_accounts_observed(&env, false, &mut text),
        ExitCode::SUCCESS
    );
    assert_eq!(
        run_accounts_observed(&env, true, &mut json),
        ExitCode::SUCCESS
    );
    let text = String::from_utf8(text).expect("UTF-8");
    for label in SAYS {
        assert!(
            line_of(&text, label).contains(" signed-out (token expired)"),
            "{label}: {text}"
        );
    }
    for label in PLAIN {
        assert!(line_of(&text, label).contains(" signed-out "), "{text}");
        assert!(!line_of(&text, label).contains(" signed-out ("), "{text}");
    }
    assert!(!line_of(&text, "live").contains(" signed-out"), "{text}");
    let rows: Value = aterm_json::from_slice(&json).expect("JSON");
    for label in SAYS {
        assert_eq!(field(&rows, label, "auth"), Value::from("signed-out"));
        assert_eq!(
            field(&rows, label, "signed_out_reason"),
            Value::from("refresh_token_expired"),
            "{label}"
        );
    }
    for label in PLAIN {
        assert_eq!(field(&rows, label, "auth"), Value::from("signed-out"));
        assert_eq!(field(&rows, label, "signed_out_reason"), Value::Null);
    }
    assert_eq!(field(&rows, "live", "auth"), Value::Null);
    assert_eq!(field(&rows, "live", "signed_out_reason"), Value::Null);
}

/// A REASON IS THE ROW'S OWN ACCOUNT'S: the refusal on record is of the login file in a place,
/// so where the default home holds another account's login (the vendor's own sign-in moved the
/// default login off the row's), its refusal is not the row's, and the row signed out here
/// reads plain `signed-out` in the list, the table and the observed places. CONTROL: the same
/// pool, its default login the row's own account, says why.
#[test]
fn a_row_says_no_reason_for_another_accounts_login_in_its_place() {
    for (default_names, shown) in [
        ("other@example.test", None),
        ("base@example.test", Some("refresh_token_expired")),
    ] {
        let (_tmp, env) = pool("signed-out-reason-displaced", default_names);
        let says = |text: &str| {
            let line = line_of(text, "base");
            assert!(line.contains(" signed-out"), "{default_names}: {text}");
            line.contains(" signed-out (token expired)")
        };
        let list = run(AccountsCmd::Report { json: false }, &env);
        assert_eq!(says(&list), shown.is_some(), "{default_names}: {list}");
        let doc: Value =
            aterm_json::from_str(&run(AccountsCmd::Report { json: true }, &env)).expect("JSON");
        assert_eq!(
            field(&doc["accounts"], "base", "signed_out_reason"),
            Value::from(shown),
            "{default_names}"
        );
        let table = run(AccountsCmd::Table { json: false }, &env);
        assert_eq!(says(&table), shown.is_some(), "{default_names}: {table}");
        let doc: Value =
            aterm_json::from_str(&run(AccountsCmd::Table { json: true }, &env)).expect("JSON");
        assert_eq!(
            field(&doc["pools"][0]["accounts"], "base", "signed_out_reason"),
            Value::from(shown),
            "{default_names}"
        );
        let (mut text, mut json) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_observed(&env, false, &mut text),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run_accounts_observed(&env, true, &mut json),
            ExitCode::SUCCESS
        );
        let observed = String::from_utf8(text).expect("UTF-8");
        assert_eq!(
            says(&observed),
            shown.is_some(),
            "{default_names}: {observed}"
        );
        let rows: Value = aterm_json::from_slice(&json).expect("JSON");
        assert_eq!(
            field(&rows, "base", "signed_out_reason"),
            Value::from(shown),
            "{default_names}"
        );
    }
}

/// THE OBSERVED COLUMNS STAY ALIGNED BESIDE A REASON: AUTH is as wide as its longest word, so a
/// row that says `signed-out (token expired)` starts WEEK where the header and every other row
/// do.
#[test]
fn the_observed_columns_stay_aligned_beside_a_reason() {
    let (_tmp, env) = pool("signed-out-reason-columns", "base@example.test");
    let mut text = Vec::new();
    assert_eq!(
        run_accounts_observed(&env, false, &mut text),
        ExitCode::SUCCESS
    );
    let text = String::from_utf8(text).expect("UTF-8");
    let header = text.lines().next().expect("a header");
    let week = header.find("WEEK").expect("WEEK");
    let week = header[..week].chars().count();
    for label in SAYS.into_iter().chain(PLAIN).chain(["live"]) {
        let line = line_of(&text, label);
        let last = line.trim_end().rfind(' ').expect("columns") + 1;
        assert_eq!(line[..last].chars().count(), week, "{label}:\n{text}");
    }
}
