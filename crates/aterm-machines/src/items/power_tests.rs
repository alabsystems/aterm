// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::Fake;

fn ctx() -> Ctx {
    Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

const VM_CUSTOM: &str = "AC Power:
 Sleep On Power Button 1
 standby              0
 powernap             1
 ttyskeepawake        1
 displaysleep         0
 sleep                0
 tcpkeepalive         1
 disksleep            0
";

const VM_CAP: &str = "Capabilities for AC Power:
 displaysleep
 disksleep
 sleep
 standby
 powernap
 ttyskeepawake
 tcpkeepalive
";

const LAPTOP_CUSTOM: &str = "Battery Power:
 Sleep On Power Button 1
 lowpowermode         0
 standby              1
 hibernatemode        3
 powernap             1
 hibernatefile        /var/vm/sleepimage
 ttyskeepawake        1
 womp                 0
 displaysleep         2
 networkoversleep     0
 sleep                1
 lessbright           1
 tcpkeepalive         1
 disksleep            10
AC Power:
 Sleep On Power Button 1
 lowpowermode         0
 standby              1
 hibernatemode        3
 powernap             1
 hibernatefile        /var/vm/sleepimage
 ttyskeepawake        1
 displaysleep         10
 womp                 1
 networkoversleep     0
 sleep                1
 tcpkeepalive         1
 disksleep            10
";

const LAPTOP_CAP: &str = "Capabilities for AC Power:
 displaysleep
 disksleep
 sleep
 womp
 standby
 powernap
 ttyskeepawake
 hibernatemode
 hibernatefile
 tcpkeepalive
 lowpowermode
";

fn starter() -> Table {
    Table {
        ac: Some(Profile {
            sleep: Some(0),
            disk_sleep: Some(0),
            display_sleep: Some(15),
            power_nap: Some(false),
            wake_on_lan: Some(true),
            restart_after_power_loss: Some(true),
            ..Profile::default()
        }),
        battery: Some(Profile {
            sleep: Some(10),
            ..Profile::default()
        }),
    }
}

fn outcomes(sys: &Fake, mode: Mode) -> Vec<(String, Outcome)> {
    let items = expand(&starter(), &ctx()).unwrap();
    engine::run(&items, mode, sys, &ctx(), &mut NoJournal)
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect()
}

#[test]
fn both_sections_of_a_laptop_and_the_one_of_the_vm_parse() {
    let laptop = parse_custom(LAPTOP_CUSTOM);
    assert_eq!(
        laptop.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["Battery Power", "AC Power"]
    );
    let ac = &laptop[1].1;
    assert!(ac.contains(&("displaysleep".into(), "10".into())));
    assert!(ac.contains(&("Sleep On Power Button".into(), "1".into())));
    assert!(ac.contains(&("hibernatefile".into(), "/var/vm/sleepimage".into())));
    let vm = parse_custom(VM_CUSTOM);
    assert_eq!(vm.len(), 1);
    assert_eq!(vm[0].0, "AC Power");
    assert!(parse_cap(VM_CAP).contains(&"powernap".to_owned()));
    assert!(!parse_cap(LAPTOP_CAP).contains(&"autorestart".to_owned()));
}

#[test]
fn in_the_vm_the_battery_profile_womp_and_autorestart_do_not_apply() {
    let sys = Fake::root();
    sys.on("/usr/bin/pmset -g custom", Ran::ok(VM_CUSTOM))
        .on("/usr/bin/pmset -g cap", Ran::ok(VM_CAP));
    let rows = outcomes(&sys, Mode::Plan);
    let get = |id: &str| rows.iter().find(|(i, _)| i == id).unwrap().1;
    assert_eq!(get("power.ac.sleep"), Outcome::Ok);
    assert_eq!(get("power.ac.disk-sleep"), Outcome::Ok);
    assert_eq!(get("power.ac.display-sleep"), Outcome::Planned);
    assert_eq!(get("power.ac.power-nap"), Outcome::Planned);
    assert_eq!(get("power.ac.wake-on-lan"), Outcome::NotApplicable);
    assert_eq!(
        get("power.ac.restart-after-power-loss"),
        Outcome::NotApplicable
    );
    assert_eq!(get("power.battery.sleep"), Outcome::NotApplicable);
}

fn with_row(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in text.lines() {
        if !l.starts_with(' ') {
            inside = l == format!("{section}:");
        } else if inside && l.split_whitespace().next() == Some(key) {
            out.push_str(&format!(" {key:<20} {value}\n"));
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn apply_on_laptop(id: &str, write: &str, after: &str) -> (crate::model::ItemReport, Vec<String>) {
    let sys = Fake::root();
    sys.on("/usr/bin/pmset -g custom", Ran::ok(LAPTOP_CUSTOM))
        .on("/usr/bin/pmset -g custom", Ran::ok(after))
        .on("/usr/bin/pmset -g cap", Ran::ok(LAPTOP_CAP))
        .on(write, Ran::ok(""));
    let items: Vec<Box<dyn Item>> = expand(&starter(), &ctx())
        .unwrap()
        .into_iter()
        .filter(|i| i.id() == id)
        .collect();
    let mut rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    (rows.remove(0), sys.calls())
}

#[test]
fn on_a_laptop_each_profile_is_written_with_its_own_flag_and_reverts_to_its_old_value() {
    let after = with_row(LAPTOP_CUSTOM, "AC Power", "sleep", "0");
    let (row, calls) = apply_on_laptop("power.ac.sleep", "/usr/bin/pmset -c sleep 0", &after);
    assert_eq!(row.outcome, Outcome::Changed, "{row:#?}");
    assert_eq!(row.revert.as_deref(), Some("/usr/bin/pmset -c sleep 1"));
    assert!(calls.contains(&"/usr/bin/pmset -c sleep 0".to_owned()));

    let after = with_row(LAPTOP_CUSTOM, "AC Power", "displaysleep", "15");
    let (row, _) = apply_on_laptop(
        "power.ac.display-sleep",
        "/usr/bin/pmset -c displaysleep 15",
        &after,
    );
    assert_eq!(row.outcome, Outcome::Changed);
    assert_eq!(
        row.revert.as_deref(),
        Some("/usr/bin/pmset -c displaysleep 10")
    );

    let after = with_row(LAPTOP_CUSTOM, "Battery Power", "sleep", "10");
    let (row, calls) = apply_on_laptop("power.battery.sleep", "/usr/bin/pmset -b sleep 10", &after);
    assert_eq!(row.outcome, Outcome::Changed, "{row:#?}");
    assert_eq!(row.revert.as_deref(), Some("/usr/bin/pmset -b sleep 1"));
    assert!(
        calls
            .iter()
            .all(|c| !c.contains(" -a ") && !c.contains(" -c "))
    );

    let (row, _) = apply_on_laptop("power.ac.wake-on-lan", "-", LAPTOP_CUSTOM);
    assert_eq!(row.outcome, Outcome::Ok);
    let (row, _) = apply_on_laptop("power.ac.restart-after-power-loss", "-", LAPTOP_CUSTOM);
    assert_eq!(row.outcome, Outcome::NotApplicable);
}

#[test]
fn a_write_to_one_section_is_not_mistaken_for_the_other() {
    let after = with_row(LAPTOP_CUSTOM, "AC Power", "sleep", "10");
    let (row, _) = apply_on_laptop("power.battery.sleep", "/usr/bin/pmset -b sleep 10", &after);
    assert_eq!(row.outcome, Outcome::Failed);
}

#[test]
fn a_write_pmset_accepted_but_did_not_keep_fails_its_read_back() {
    let sys = Fake::root();
    sys.on("/usr/bin/pmset -g custom", Ran::ok(VM_CUSTOM))
        .on("/usr/bin/pmset -c displaysleep 15", Ran::ok(""));
    let t = Table {
        ac: Some(Profile {
            display_sleep: Some(15),
            ..Profile::default()
        }),
        battery: None,
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("still reads never"),
        "{rows:#?}"
    );
}

#[test]
fn autorestart_applies_where_cap_lists_it_and_the_profile_prints_it() {
    let custom = format!("{VM_CUSTOM} autorestart          0\n");
    let cap = format!("{VM_CAP} autorestart\n");
    let sys = Fake::root();
    sys.on("/usr/bin/pmset -g custom", Ran::ok(&custom))
        .on("/usr/bin/pmset -g cap", Ran::ok(&cap));
    let t = Table {
        ac: Some(Profile {
            restart_after_power_loss: Some(true),
            ..Profile::default()
        }),
        battery: None,
    };
    let items = expand(&t, &ctx()).unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx()),
        Probe::Change {
            from: "off".into(),
            to: "on".into()
        }
    );
    assert_eq!(
        items[0].revert().as_deref(),
        Some("/usr/bin/pmset -c autorestart 0")
    );
}

#[test]
fn pmset_that_does_not_answer_is_unknown_not_not_applicable() {
    let rows = outcomes(&Fake::root(), Mode::Plan);
    assert!(
        rows.iter().all(|(_, o)| *o == Outcome::Unknown),
        "{rows:#?}"
    );
}
