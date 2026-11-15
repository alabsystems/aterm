// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The fixture has the sheet's shape for any seed: [`ROWS`] rows, [`WITH_PASSWORD`] passwords
/// of 8 to 24 characters, none of them flag-shaped, a flag on each and on [`DECOYS`] more; the
/// same seed draws the same rows and another seed other ones.
#[test]
fn the_fixture_has_the_sheets_shape_and_one_seed_draws_one_table() {
    for seed in ["a", "e1-2026-11-05", "another"] {
        let rows = rows(seed);
        assert_eq!(rows.len(), ROWS);
        let with: Vec<&String> = rows
            .iter()
            .filter_map(|row| row.password.as_ref())
            .collect();
        assert_eq!(with.len(), WITH_PASSWORD);
        assert!(with.iter().all(|p| (8..=24).contains(&p.len())));
        assert!(with.iter().all(|p| !aterm_db::typed::secret::flag_like(
            &aterm_db::typed::secret::Secret::new(p.to_string())
        )));
        assert_eq!(
            rows.iter().filter(|row| row.flag).count(),
            WITH_PASSWORD + DECOYS
        );
        assert!(rows.iter().all(|row| row.password.is_none() || row.flag));
        let emails: BTreeSet<&String> = rows.iter().map(|row| &row.email).collect();
        assert_eq!(emails.len(), ROWS);
    }
    let same = |a: &[Row], b: &[Row]| {
        a.iter()
            .zip(b)
            .all(|(x, y)| x.email == y.email && x.password == y.password && x.flag == y.flag)
    };
    assert!(same(&rows("seed"), &rows("seed")));
    assert!(!same(&rows("seed"), &rows("other")));
}

/// The sheet carries a flag, never a password: no password of the fixture is in it.
#[test]
fn the_sheet_holds_flags_and_no_password() {
    let rows = rows("sheet");
    let text = sheet(&rows);
    assert_eq!(text.lines().count(), ROWS + 1);
    for row in &rows {
        if let Some(password) = &row.password {
            assert!(!text.contains(password.as_str()));
        }
    }
    assert_eq!(
        text.lines()
            .skip(1)
            .filter(|line| line.contains(",,1,"))
            .count(),
        WITH_PASSWORD + DECOYS
    );
}

/// A table's digest is of the rows its view shows, with their count and how many show a
/// password: two views of the same rows read one digest, any shown cell that differs moves it,
/// and a view that did not read the registry (`never`, no rows) is no table.
/// NEGATIVE CONTROL: with `listing` digesting a `never` view as an empty table, the last
/// assertion fails: `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn a_tables_digest_is_of_the_rows_its_view_shows() {
    let view = |password: Value, status: &str| {
        json!({"complete": true, "state": "live", "table": "emails", "emails": [
            {"ad": "ad-0123456789abcdef", "addr": "a@example.invalid", "password": password,
             "status": status},
            {"ad": "ad-fedcba9876543210", "addr": "b@example.invalid", "password": null,
             "status": "active"},
        ]})
    };
    let read = listing(&view(json!("••••••"), "active")).unwrap();
    assert_eq!(read["rows"], json!(2));
    assert_eq!(read["with_password"], json!(1));
    assert_eq!(read["complete"], json!(true));
    assert_eq!(read["digest"].as_str().map(str::len), Some(64));
    assert_eq!(listing(&view(json!("••••••"), "active")).unwrap(), read);
    for moved in [
        view(json!("••••••"), "disabled"),
        view(json!("address.password.ad-0123456789abcdef.1"), "active"),
        view(Value::Null, "active"),
    ] {
        assert_ne!(listing(&moved).unwrap()["digest"], read["digest"]);
    }
    let never = json!({"complete": false, "state": "never", "why": "busy", "table": "emails",
        "emails": []});
    assert!(listing(&never).unwrap_err().contains("did not read"));
    assert!(
        listing(&json!({"usage": "…"}))
            .unwrap_err()
            .contains("lists no rows")
    );
}
