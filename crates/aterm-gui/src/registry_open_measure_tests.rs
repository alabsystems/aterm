// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::alab::Registry;
use aterm_db::table::fixture::Lab;
use aterm_secrets::commons::roster::Role;

#[test]
fn an_empty_registered_inventory_and_an_unconfigured_home_have_distinct_outcomes() {
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let report = measure(lab.paths[0].clone()).unwrap();
    assert!(report.starts_with("status=empty machines=0 "));

    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::locate_for_home(dir.path());
    assert_eq!(measure(paths).unwrap_err(), "status=unavailable");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
