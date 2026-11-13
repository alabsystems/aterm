// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::{Session, ctx, session_of, user};
use crate::model::{Mode, Outcome};

/// A `[consumer]` row and a `[[defaults]]` row that name one restart share it across
/// their tables: `expand` hands both one registry, so the process is relaunched once,
/// after the later write, under one journal line that names both rows. Negative
/// control: the `[[defaults]]` rows given a registry of their own make a second
/// relaunch of the same id, which the engine runs in place of the first, so its line
/// names the `[[defaults]]` row alone and the catalog row's write is never relaunched
/// for.
#[test]
fn a_catalog_row_and_a_defaults_row_of_one_reader_relaunch_it_once() {
    let admin = user("admin", 501);
    let text = format!(
        "schema = {}\n\n[consumer]\nairdrop = false\n\n[[defaults]]\n\
         domain = \"com.apple.sharingd\"\nkey = \"SuppressAirDropNotifications\"\n\
         value = true\nscope = \"user\"\nrestart = \"sharingd\"\n",
        crate::SCHEMA
    );
    let c = ctx(std::slice::from_ref(&admin));
    let items = expand(&crate::baseline::parse(&text).unwrap(), &c).unwrap();
    assert_eq!(
        items.len(),
        2,
        "{:?}",
        items.iter().map(|i| i.id()).collect::<Vec<_>>()
    );
    let (sys, session) = session_of(Session::running(&["sharingd"]));
    let mut journal = Recorded::default();
    let rows = crate::engine::run(&items, Mode::Apply, &sys, &c, &mut journal);
    assert!(
        rows.iter().all(|row| row.outcome == Outcome::Changed),
        "{rows:?}"
    );
    let session = session.lock().unwrap();
    assert_eq!(session.kills("sharingd"), 1, "{:?}", session.events);
    assert_eq!(
        session.events.last().map(String::as_str),
        Some("kill sharingd"),
        "{:?}",
        session.events
    );
    let relaunches: Vec<&(String, String)> = journal
        .0
        .iter()
        .filter(|(id, _)| id == "relaunch.sharingd@admin")
        .collect();
    let [(_, to)] = relaunches.as_slice() else {
        panic!("{:?}", journal.0);
    };
    assert!(
        items.iter().all(|item| to.contains(&item.id())),
        "{to}: {:?}",
        items.iter().map(|item| item.id()).collect::<Vec<_>>()
    );
}

/// Each line a journal was handed: its id and `to`.
#[derive(Default)]
struct Recorded(Vec<(String, String)>);

impl aterm_converge::Journal for Recorded {
    fn before_change(
        &mut self,
        id: &str,
        _from: &str,
        to: &str,
        _revert: Option<&str>,
    ) -> Result<(), String> {
        self.0.push((id.into(), to.into()));
        Ok(())
    }
}

#[test]
fn every_item_is_of_the_family_its_baseline_table_names() {
    let text = crate::baseline::fill(
        crate::controller::templates::DEFAULT_BASELINE,
        &[
            ("name", "lab-07"),
            ("tag", "lab-07"),
            ("serial", "ZNYVNTRJN5"),
            ("uuid", "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"),
        ],
    )
    .unwrap();
    let baseline = crate::baseline::parse(&text).unwrap();
    let items = expand(&baseline, &ctx(&[user("admin", 501)])).unwrap();
    let tables = crate::baseline::tables();
    let mut seen: Vec<&str> = Vec::new();
    for item in &items {
        assert!(
            tables.contains(&item.family()),
            "{} is of family {}, which no table is named",
            item.id(),
            item.family()
        );
        if !seen.contains(&item.family()) {
            seen.push(item.family());
        }
    }
    assert!(seen.len() >= 10, "the starter baseline's tables: {seen:?}");
    assert!(
        !seen.contains(&"homebrew") && !seen.contains(&"app"),
        "the starter installs no Homebrew and no app: {seen:?}"
    );
    // The families stay for an owner's own baseline.
    let owned = format!(
        "{text}\n[homebrew]\npkg = {{ url = \"https://example.com/Homebrew.pkg\", sha256 = \
         \"{zero}\", team_id = \"927JGANW46\" }}\nformulae = [\"jq\"]\n\n[[app]]\n\
         name = \"Caffeine\"\nkind = \"dmg-app\"\nurl = \"https://example.com/Caffeine.dmg\"\n\
         sha256 = \"{zero}\"\nteam_id = \"YD6LEYT6WZ\"\nbundle = \"Caffeine.app\"\n\
         version = \"1.1.4\"\n",
        zero = "0".repeat(64)
    );
    let items = expand(
        &crate::baseline::parse(&owned).unwrap(),
        &ctx(&[user("admin", 501)]),
    )
    .unwrap();
    for family in ["homebrew", "app"] {
        assert!(
            items.iter().any(|item| item.family() == family),
            "an owner's [{family}] expands"
        );
    }
}
