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
