// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab alab-roll`: the registry roll's three-Mac partition run (docs/DESIGN-alab-lab-model-
//! 2026-11-01.md §3.8, §11 step 13) on three scratch Commons custodies in this process.
//!
//! This is no run on three real Macs. The three Macs are `aterm_db::table::fixture::Lab`'s: each
//! its own vault custody under one temporary home, enrolled in one test vault and repaired over
//! loopback exactly as a runtime's repair exchange runs, with the production roll, seal,
//! restatement and storage release. Nothing reads or writes this Mac's vault, registry or
//! journal, and the home goes when the run ends. Every value is synthetic (`example.test`).
//!
//! The run: m1 seeds the registry and every Mac writes its facts row. m1's first roll restates
//! but does not seal, since m2 and m3 have shown no build that reads the second generation; their
//! release pass (a claims write's housekeeping) declares it. In the cut m1 rolls alone
//! (restates, seals, releases its storage of the first generation) and edits a provider's site in
//! the second; m2 and m3, which have not heard the seal, read the first, and m2 edits the same
//! cell there, which m3 alone hears. After the heal every Mac reads the second generation, and
//! neither m2 nor m3 may release the first, since no cover carries m2's late record; m3's roll
//! carries it, beside m1's edit as a conflict; then every Mac releases the first and all three
//! read alike.

use aterm_db::{
    alab::{
        Registry, View,
        registry::{MACHINE_FACTS, PROVIDER},
    },
    att,
    catalog::{Directory, release::Kept},
    database::Change,
    table::fixture::{Lab, repair_commons, repair_commons_within},
    typed::{
        self,
        cell::{self, CellKey, CellValue, Field},
        keys,
    },
};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

use crate::{Fail, Res};

/// Repair passes per exchange. One pass repairs one table with each peer, m1 with m2 and then with
/// m3, so a record from m3 reaches m2 only through m1, on a later pass. 3 passes cover the calls
/// with 3 common tables (the catalog and two generations), which rely on no such crossing, and the
/// 2-table calls after the seal, where m3's cover must cross to m2: one more table common to m1
/// there already needs this raised. Below what a call needs, which table a pass picks follows
/// digests that differ per lab, so the run turns flaky (2 passes passed 2 of 24 runs) rather than
/// steadily red. This also sets what `lab alab-roll` runs.
const ROUNDS: usize = 3;

/// What the run saw, as [`judge`] reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Whether m1's first roll waited, unsealed, on exactly m2 and m3.
    pub staged_waited: bool,
    /// Whether m1's roll sealed the first generation, and released its own storage of it.
    pub sealed: bool,
    pub roller_released: bool,
    /// The generation m2 read in the cut, after m1 sealed.
    pub cut_generation: u32,
    /// The generation each Mac read after the heal.
    pub healed: Vec<u32>,
    /// Why m2 and m3 kept the first generation after the heal (`Kept::word`), none if released.
    pub kept_after_heal: Vec<Option<String>>,
    /// Whether m3's roll carried a register, wrote a cover row and released its storage.
    pub late_carried: bool,
    pub late_released: bool,
    /// Whether m2 released once m3's cover reached it.
    pub last_released: bool,
    /// The site values each Mac reads, sorted: a conflict holds both edits.
    pub site: Vec<Vec<String>>,
    /// Whether all three read the same registry, and how many facts rows.
    pub alike: bool,
    pub facts: Vec<usize>,
    /// Which Macs still store the first generation at the end.
    pub stored: Vec<bool>,
}

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(format!("alab-roll: {error}"))
}

fn now_ms() -> Result<u64, Fail> {
    aterm_time::unix_millis().ok_or_else(|| Fail::die("alab-roll: no system clock"))
}

/// An owner's edit of provider `anthropic`'s site.
fn site(url: &str) -> Result<Vec<Change>, Fail> {
    Ok(vec![
        cell::change(
            &PROVIDER,
            CellKey {
                pk: "anthropic",
                col: "site",
                elem: None,
            },
            &CellValue::owner(1, json!(url)),
        )
        .map_err(fail)?,
    ])
}

/// Mac `n`'s facts row.
fn facts(lab: &Lab, n: usize) -> Vec<Change> {
    vec![Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&lab.machine(n)),
        value: Some(
            json!({"r": 1, "n": 1, "mac": "mc-0000000000000000", "label": format!("m{}", n + 1)}),
        ),
    }]
}

/// Whether Mac `n` stores the registry's first generation.
fn stores_first(lab: &Lab, n: usize) -> Result<bool, Fail> {
    let trust = lab.bootstrap(n).trust();
    let path = Directory::new(&lab.paths[n], trust)
        .path(att::REGISTRY.descriptor(trust).map_err(fail)?.scope)
        .map_err(fail)?;
    Ok(path.exists())
}

/// A view's whole reading: every cells collection's projection and owned collection's rows.
fn image(registry: &Registry, view: &View) -> String {
    let table = registry.generation().table;
    let mut out = String::new();
    for collection in table.collections {
        match collection.kind {
            typed::Kind::Cells => out.push_str(&format!("{:?}", view.project(collection))),
            typed::Kind::Owned => out.push_str(&format!("{:?}", view.owned.get(collection.name))),
            typed::Kind::Log => {}
        }
    }
    out
}

fn site_values(view: &View) -> Vec<String> {
    let mut values: Vec<String> = match view
        .project(&PROVIDER)
        .rows
        .get("anthropic")
        .and_then(|row| row.fields.get("site"))
    {
        Some(Field::Conflict { candidates, .. }) => candidates
            .iter()
            .filter_map(|c| c.value.x.as_str().map(str::to_owned))
            .collect(),
        Some(Field::Value(candidate)) => candidate
            .value
            .x
            .as_str()
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    values.sort();
    values
}

/// Run the partition on a fresh scratch lab.
///
/// # Errors
/// A step the production code refused.
pub fn run() -> Result<Evidence, Fail> {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let macs: Vec<Registry> = lab.paths.iter().cloned().map(Registry::at).collect();
    let first = att::REGISTRY
        .descriptor(lab.bootstrap(0).trust())
        .map_err(fail)?
        .scope
        .digest();
    let mut ev = Evidence::default();

    macs[0].open().map_err(fail)?;
    macs[0].seed().map_err(fail)?;
    macs[0].write(vec![facts(&lab, 0)]).map_err(fail)?;
    repair_commons(&lab, ROUNDS);
    for (n, registry) in macs.iter().enumerate().skip(1) {
        registry.write(vec![facts(&lab, n)]).map_err(fail)?;
    }
    repair_commons(&lab, ROUNDS);

    // m1 restates; the seal waits until m2 and m3 declare the second generation.
    let staged = macs[0].roll(true, now_ms()?).map_err(fail)?;
    let mut waiting = staged.waiting.clone();
    waiting.sort_unstable();
    let mut others = vec![lab.machine(1), lab.machine(2)];
    others.sort_unstable();
    ev.staged_waited = !staged.sealed && waiting == others;
    repair_commons(&lab, ROUNDS);
    for registry in &macs[1..] {
        registry.release(now_ms()?).map_err(fail)?;
    }
    repair_commons(&lab, ROUNDS);

    // The cut: m1 alone.
    let rolled = macs[0].roll(true, now_ms()?).map_err(fail)?;
    ev.sealed = rolled.sealed;
    ev.roller_released = rolled.released.released == vec![first];
    macs[0]
        .write(vec![site("https://one.example.test")?])
        .map_err(fail)?;
    ev.cut_generation = macs[1].generation().generation;
    macs[1]
        .write(vec![site("https://two.example.test")?])
        .map_err(fail)?;
    repair_commons_within(&lab, ROUNDS, &[1, 2]);

    // The heal.
    repair_commons(&lab, ROUNDS);
    ev.healed = macs.iter().map(|r| r.generation().generation).collect();
    for registry in &macs[1..] {
        let released = registry.release(now_ms()?).map_err(fail)?;
        ev.kept_after_heal.push(
            released
                .kept
                .iter()
                .find(|(id, _)| *id == first)
                .map(|(_, why)| why.word().to_owned()),
        );
    }

    // m3 holds m2's late record: its roll carries it.
    let late = macs[2].roll(true, now_ms()?).map_err(fail)?;
    ev.late_carried = late.registers > 0 && late.cover && !late.sealed;
    ev.late_released = late.released.released == vec![first];
    repair_commons(&lab, ROUNDS);
    ev.last_released = macs[1].release(now_ms()?).map_err(fail)?.released == vec![first];
    repair_commons(&lab, ROUNDS);

    let views: Vec<View> = macs
        .iter()
        .map(Registry::view)
        .collect::<Result<_, _>>()
        .map_err(fail)?;
    ev.site = views.iter().map(site_values).collect();
    let images: Vec<String> = macs.iter().zip(&views).map(|(r, v)| image(r, v)).collect();
    ev.alike = images.windows(2).all(|pair| pair[0] == pair[1]);
    ev.facts = views
        .iter()
        .map(|view| {
            view.owned
                .get(MACHINE_FACTS.name)
                .map_or(0, |read| read.rows.len())
        })
        .collect();
    ev.stored = (0..3)
        .map(|n| stores_first(&lab, n))
        .collect::<Result<_, _>>()?;
    Ok(ev)
}

/// One named check and whether it held.
pub fn judge(ev: &Evidence) -> Vec<(&'static str, bool)> {
    let not_carried = Some(Kept::NotCarried.word().to_owned());
    let both = vec![
        "https://one.example.test".to_owned(),
        "https://two.example.test".to_owned(),
    ];
    vec![
        (
            "the seal waits for every Writer to declare the next generation",
            ev.staged_waited,
        ),
        (
            "the roller seals and releases its own storage",
            ev.sealed && ev.roller_released,
        ),
        (
            "a Mac that has not heard the seal reads the first generation",
            ev.cut_generation == 1,
        ),
        (
            "after the heal every Mac reads the second generation",
            ev.healed == [2, 2, 2],
        ),
        (
            "no Mac holding an uncarried late record releases the first generation",
            ev.kept_after_heal == [not_carried.clone(), not_carried],
        ),
        (
            "the late Mac's roll carries the late edit and releases",
            ev.late_carried && ev.late_released,
        ),
        (
            "the last Mac releases once the late cover reaches it",
            ev.last_released,
        ),
        (
            "both edits stand as one conflict on every Mac",
            ev.site.iter().all(|values| *values == both),
        ),
        (
            "every Mac reads the same registry, every facts row kept",
            ev.alike && ev.facts == [3, 3, 3],
        ),
        (
            "no Mac stores the first generation at the end",
            ev.stored == [false, false, false],
        ),
    ]
}

/// `lab alab-roll [--json]`.
///
/// # Errors
/// An unknown option, or a step the production code refused.
pub fn cmd(args: &[String]) -> Res {
    let as_json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(Fail::usage("alab-roll takes no operands but --json")),
    };
    let ev = run()?;
    let checks = judge(&ev);
    let pass = checks.iter().all(|(_, held)| *held);
    let report: Value = json!({
        "lane": "alab-roll",
        "real_macs": false,
        "macs": "three scratch Commons custodies in this process, repaired over loopback; no run on three real Macs happened",
        "pass": pass,
        "checks": checks.iter().map(|(name, held)| json!({"check": name, "pass": held})).collect::<Vec<_>>(),
    });
    let text = if as_json {
        format!("{}\n", aterm_json::to_string(&report).map_err(fail)?)
    } else {
        let mut text = String::from(
            "alab-roll: three scratch Commons custodies in this process (no run on three real Macs)\n",
        );
        for (name, held) in &checks {
            text.push_str(&format!(
                "  {}  {name}\n",
                if *held { "PASS" } else { "FAIL" }
            ));
        }
        text.push_str(if pass { "PASS\n" } else { "FAIL\n" });
        text
    };
    print!("{text}");
    Ok(i32::from(!pass))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> Evidence {
        Evidence {
            staged_waited: true,
            sealed: true,
            roller_released: true,
            cut_generation: 1,
            healed: vec![2, 2, 2],
            kept_after_heal: vec![Some("not-carried".into()), Some("not-carried".into())],
            late_carried: true,
            late_released: true,
            last_released: true,
            site: vec![
                vec![
                    "https://one.example.test".into(),
                    "https://two.example.test".into()
                ];
                3
            ],
            alike: true,
            facts: vec![3, 3, 3],
            stored: vec![false, false, false],
        }
    }

    /// The run itself, on three scratch custodies: every check holds.
    #[test]
    fn the_partition_run_passes() {
        aterm_spec::fs_lane::join();
        let _lease = aterm_spec::subprocess_lane::acquire().unwrap_or_else(|e| {
            aterm_spec::host::could_not_run(format!("subprocess admission refused ({e})"))
        });
        let ev = run().unwrap();
        let failed: Vec<&str> = judge(&ev)
            .into_iter()
            .filter(|(_, held)| !held)
            .map(|(name, _)| name)
            .collect();
        assert!(failed.is_empty(), "{failed:?} {ev:?}");
    }

    /// The judgement of a run that went as designed passes every check. NEGATIVE CONTROL: a run
    /// in which m2 released before m3's roll carried its late edit (the release this step must
    /// never make) fails exactly that check.
    #[test]
    fn an_early_release_fails_its_check() {
        assert!(judge(&passing()).iter().all(|(_, held)| *held));
        let mut early = passing();
        early.kept_after_heal[0] = None;
        let failed: Vec<&str> = judge(&early)
            .into_iter()
            .filter(|(_, held)| !held)
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            failed,
            ["no Mac holding an uncarried late record releases the first generation"]
        );
    }
}
