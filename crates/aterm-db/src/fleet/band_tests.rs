// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The band projection's own arithmetic, held on the fixture's scenes.

use super::*;
use crate::fleet::fixture::{self, Scene};

/// A minute's start, so the scenes' minute rounding is easy to read.
const NOW_MS: u64 = 1_794_000_000_000;

/// `band` with its lanes counted again from its members, as a fold would state them.
fn recounted(mut band: Band) -> Band {
    let (mut machines, mut accounts, agents) = Band::lanes_of(&band.machines, &band.pool);
    machines.cores_x100 = band.machine_lane.cores_x100;
    machines.memory_mb = band.machine_lane.memory_mb;
    accounts.cannot_be_used = band.account_lane.cannot_be_used;
    accounts.next_refresh_ms = band.account_lane.next_refresh_ms;
    (band.machine_lane, band.account_lane, band.agent_lane) = (machines, accounts, agents);
    band
}

#[test]
fn the_design_scene_states_the_fleet_displays_counts_and_every_lane_closes() {
    let band = fixture::scene(Scene::Design, NOW_MS);
    assert_eq!(band.check(), Ok(()));
    let (m, a, g) = (band.machine_lane, band.account_lane, band.agent_lane);
    assert_eq!((m.up, m.members), (63, 72));
    assert_eq!((m.silent, m.unread, m.old_build, m.at_limit), (7, 2, 4, 5));
    assert_eq!((a.in_use, a.usable), (53, 127));
    assert_eq!((a.idle, a.at_limit, a.unread), (48, 21, 5));
    assert_eq!(a.cannot_be_used, Some(26));
    assert_eq!((g.running(), g.agents), (112, 180));
    assert_eq!(
        (g.output, g.job, g.idle, g.paused, g.error),
        (89, 23, 46, 11, 11)
    );
    assert_eq!(band.pool.len(), 31);
    assert_eq!(
        band.own.map(|own| band.machines[own].label.as_str()),
        Some("m3")
    );
    // The cores and memory of every machine that answers, in the rows' units.
    let cores = m
        .cores_x100
        .expect("the design's machines that answer were read");
    assert_eq!(cores.members, m.up);
    assert_eq!(cores.of % 100, 0, "whole cores, in hundredths: {cores:?}");
    assert_eq!(m.memory_mb.map(|memory| memory.members), Some(m.up));
    // The one duration printed is the refresh, 55 minutes 20 seconds away: 56 whole minutes,
    // which become 55 in 20 seconds.
    assert_eq!(band.next_change_ms, Some(NOW_MS + 20_000));

    // Each provider has its count, each account is drawn once (on one machine or in the pool),
    // and each is named by its realm's provider word, as a slug is.
    let drawn: Vec<&AccountMark> = band
        .machines
        .iter()
        .flat_map(|machine| &machine.accounts)
        .chain(&band.pool)
        .collect();
    let of = |provider| {
        drawn
            .iter()
            .filter(|account| account.provider == provider)
            .count()
    };
    assert_eq!(
        Provider::ALL.map(of),
        [69, 42, 11, 4, 1, 0],
        "the scene's accounts by provider"
    );
    let labels: std::collections::BTreeSet<&str> =
        drawn.iter().map(|account| account.label.as_str()).collect();
    assert_eq!(labels.len(), 127);
    for account in &drawn {
        let (word, _) = account.label.split_once('-').expect("a word and a count");
        assert_eq!(Provider::of(word), account.provider, "{}", account.label);
    }
    assert!(labels.contains("oai-01") && labels.contains("xai-01"));

    // What a mark says is true of its machine: an agent runs on an account in use there, and a
    // paused agent's machine holds an account at its limit.
    for machine in &band.machines {
        let has = |class| {
            machine
                .accounts
                .iter()
                .any(|account| account.class == class)
        };
        for agent in machine.agents.iter().flatten() {
            match agent.class {
                AgentClass::Output | AgentClass::Job => {
                    assert!(has(AccountClass::InUse), "{}", machine.label)
                }
                AgentClass::Paused => assert!(has(AccountClass::AtLimit), "{}", machine.label),
                AgentClass::Idle | AgentClass::Error => {}
            }
        }
        let runs = machine
            .agents
            .iter()
            .flatten()
            .any(|agent| matches!(agent.class, AgentClass::Output | AgentClass::Job));
        assert_eq!(has(AccountClass::InUse), runs, "{}", machine.label);
    }

    // Twice the data is twice every count, and still closes.
    let double = fixture::scene(Scene::Double, NOW_MS);
    assert_eq!(double.check(), Ok(()));
    assert_eq!(
        (double.machine_lane.up, double.machine_lane.members),
        (126, 144)
    );
    assert_eq!(
        (double.account_lane.in_use, double.account_lane.usable),
        (106, 254)
    );
    assert_eq!(
        (double.agent_lane.running(), double.agent_lane.agents),
        (224, 360)
    );

    // Control: a member counted twice. The lanes are counted again, so only the second drawing
    // of the account is wrong.
    let mut twice = band.clone();
    let again = twice.pool[0].clone();
    twice.machines[1].accounts.push(again.clone());
    assert_eq!(
        recounted(twice).check(),
        Err(Fault::AccountTwice(again.label))
    );

    // Control: a machine counted twice, numbered (its place repeats) and not (its label does).
    let mut twice = band.clone();
    twice.machines.insert(1, twice.machines[1].clone());
    assert_eq!(recounted(twice).check(), Err(Fault::Order));
    let mut twice = band.clone();
    let studio = MachineColumn {
        number: None,
        label: "studio".to_owned(),
        reach: Reach::Up,
        load_x100: Some(20),
        limits: Some(0),
        old_build: false,
        accounts: Vec::new(),
        agents: Some(vec![AgentMark {
            class: AgentClass::Idle,
            provider: None,
        }]),
    };
    twice.machines.push(studio.clone());
    assert_eq!(recounted(twice.clone()).check(), Ok(()));
    twice.machines.push(studio);
    assert_eq!(recounted(twice.clone()).check(), Err(Fault::Order));
    let mut renamed = twice;
    renamed.machines.pop();
    let last = renamed.machines.len() - 1;
    renamed.machines[last].label = renamed.machines[0].label.clone();
    assert_eq!(
        recounted(renamed).check(),
        Err(Fault::MachineTwice(band.machines[0].label.clone()))
    );

    // Control: machines out of number order, and an own column that names no machine.
    let mut swapped = band.clone();
    swapped.machines.swap(3, 4);
    assert_eq!(swapped.check(), Err(Fault::Order));
    let mut own = band.clone();
    own.own = Some(own.machines.len());
    assert_eq!(own.check(), Err(Fault::Own));

    // Control: a lane whose classes do not sum, in each lane.
    let mut open = band.clone();
    open.machine_lane.up -= 1;
    assert_eq!(open.check(), Err(Fault::Open(Lane::Machines)));
    let mut open = band.clone();
    open.account_lane.idle += 1;
    assert_eq!(open.check(), Err(Fault::Open(Lane::Accounts)));
    let mut open = band.clone();
    open.agent_lane.agents += 1;
    assert_eq!(open.check(), Err(Fault::Open(Lane::Agents)));

    // Control: a lane that sums and states a count its members do not give.
    let mut moved = band.clone();
    moved.machine_lane.up += 1;
    moved.machine_lane.unread -= 1;
    assert_eq!(moved.check(), Err(Fault::Miscount(Lane::Machines)));
    let mut moved = band.clone();
    moved.account_lane.in_use += 1;
    moved.account_lane.idle -= 1;
    assert_eq!(moved.check(), Err(Fault::Miscount(Lane::Accounts)));
    let mut moved = band.clone();
    moved.agent_lane.output += 1;
    moved.agent_lane.job -= 1;
    assert_eq!(moved.check(), Err(Fault::Miscount(Lane::Agents)));

    // Control: a lane's sum that states more used than there is, more members than answer, or
    // no member at all.
    let mut over = band.clone();
    over.machine_lane.cores_x100 = Some(Sum {
        used: cores.of + 1,
        ..cores
    });
    assert_eq!(over.check(), Err(Fault::Sum(Lane::Machines)));
    let mut over = band.clone();
    over.machine_lane.cores_x100 = Some(Sum {
        members: m.up + 1,
        ..cores
    });
    assert_eq!(over.check(), Err(Fault::Sum(Lane::Machines)));
    let mut over = band.clone();
    over.machine_lane.memory_mb = Some(Sum {
        used: 0,
        of: 0,
        members: 0,
    });
    assert_eq!(over.check(), Err(Fault::Sum(Lane::Machines)));

    // Control: an account with more than a whole share left.
    let mut whole = band.clone();
    let label = whole.pool[0].label.clone();
    whole.pool[0].left_x10 = Some(WHOLE_X10 + 1);
    assert_eq!(recounted(whole).check(), Err(Fault::OverWhole(label)));

    // Control: lane counts too large to add are a fault, never a panic.
    let mut huge = band;
    huge.machine_lane.up = u32::MAX;
    huge.machine_lane.silent = 1;
    assert_eq!(huge.check(), Err(Fault::Open(Lane::Machines)));
}

#[test]
fn a_scene_value_nobody_read_is_none_and_a_history_slot_nobody_sampled_is_none() {
    // A first day: no history, most machines never read, and nothing unread has a figure.
    let day_one = fixture::scene(Scene::DayOne, NOW_MS);
    assert_eq!(day_one.check(), Ok(()));
    assert!(day_one.history.slots.iter().all(Option::is_none));
    assert_eq!(day_one.next_change_ms, None, "it prints no duration");
    let m = day_one.machine_lane;
    assert!(m.unread > m.up, "most machines are not read: {m:?}");
    let own = day_one.own.map(|own| &day_one.machines[own]);
    assert_eq!(
        own.map(|machine| machine.reach),
        Some(Reach::Up),
        "a machine reads itself"
    );
    assert_eq!(m.cores_x100.map(|cores| cores.members), Some(m.up));
    assert_eq!(day_one.agent_lane.machines_unread, m.members - m.up);
    for machine in &day_one.machines {
        if machine.reach != Reach::Up {
            assert_eq!(
                (machine.load_x100, machine.limits, &machine.agents),
                (None, None, &None),
                "{}",
                machine.label
            );
        }
    }
    let a = day_one.account_lane;
    assert!(a.unread > a.usable / 2, "most accounts are not read: {a:?}");
    let pool_unread = day_one
        .pool
        .iter()
        .filter(|account| account.class == AccountClass::Unread)
        .count();
    assert!(pool_unread * 2 > day_one.pool.len(), "most of the pool");
    let left = a.left_x10.expect("some account's usage was read");
    assert!(left.members <= a.usable - a.unread);
    assert_eq!((a.cannot_be_used, a.next_refresh_ms), (None, None));
    assert_eq!(day_one.services.accounts, Service::Unread);
    // A provider no account of which was read has no sum, never a zero.
    assert_eq!(a.left_by[Provider::Other.index()], None);
    // A share is of what was read: nothing used is 0, and huge figures are still whole.
    assert_eq!(Sum::one(0, 8).share_x10(), Some(0));
    assert_eq!(Sum::one(3, 0).share_x10(), None);
    assert_eq!(
        Sum::one(u64::MAX, u64::MAX).share_x10(),
        Some(WHOLE_X10),
        "a share of huge figures is still whole"
    );

    // History with holes: the holes are None, the rest is sampled, the newest is now.
    let gappy = fixture::scene(Scene::Gappy, NOW_MS);
    assert_eq!(gappy.check(), Ok(()));
    for (i, slot) in gappy.history.slots.iter().enumerate() {
        let hole = i % 7 == 3 || (20..29).contains(&i);
        assert_eq!(slot.is_none(), hole, "slot {i}");
    }
    let newest = gappy.history.slots[SLOTS - 1]
        .as_ref()
        .expect("the newest slot is sampled");
    assert_eq!(newest.cores_x100, gappy.machine_lane.cores_x100);
    assert_eq!(newest.left_x10, gappy.account_lane.left_x10);
    assert_eq!(
        newest.agents.map(|agents| agents.used),
        Some(u64::from(gappy.agent_lane.running()))
    );
    let design = fixture::scene(Scene::Design, NOW_MS);
    assert!(design.history.slots.iter().all(Option::is_some));
    let build = |i: usize| {
        design.history.slots[i]
            .as_ref()
            .and_then(|slot| slot.build.clone())
    };
    assert_ne!(
        build(fixture::SCENE_BUILD_SLOT - 1),
        build(fixture::SCENE_BUILD_SLOT)
    );

    // Control: an unread value stated as 0, on a machine and on an account.
    let mut zero = day_one.clone();
    let at = zero
        .machines
        .iter()
        .position(|machine| machine.reach != Reach::Up)
        .expect("the first day has an unread machine");
    zero.machines[at].load_x100 = Some(0);
    assert_eq!(
        zero.check(),
        Err(Fault::Unread(zero.machines[at].label.clone()))
    );
    let mut zero = day_one.clone();
    let account = zero
        .pool
        .iter_mut()
        .find(|account| account.class == AccountClass::Unread)
        .expect("the first day's pool has an unread account");
    account.left_x10 = Some(0);
    let label = account.label.clone();
    assert_eq!(recounted(zero).check(), Err(Fault::UnreadAccount(label)));
    // A machine that is not up keeps the old-build mark its last read gave.
    for reach in [Reach::Silent, Reach::Unread] {
        let mut kept = day_one.clone();
        let at = kept
            .machines
            .iter()
            .position(|machine| machine.reach == reach)
            .expect("the first day has a machine of each reach");
        kept.machines[at].old_build = true;
        assert_eq!(recounted(kept).check(), Ok(()), "{reach:?}");
    }

    // Control: an unsampled slot stated with no sum.
    let mut empty = gappy.clone();
    empty.history.slots[3] = Some(Slot {
        cores_x100: None,
        left_x10: None,
        agents: None,
        build: None,
    });
    assert_eq!(empty.check(), Err(Fault::EmptySlot(3)));

    // Control: a slot that states more than it read, or a figure no member was read into.
    let mut over = gappy.clone();
    if let Some(agents) = over.history.slots[0]
        .as_mut()
        .and_then(|slot| slot.agents.as_mut())
    {
        agents.used = agents.of + 1;
    }
    assert_eq!(over.check(), Err(Fault::SlotSum(0)));
    let mut unread = gappy;
    if let Some(slot) = unread.history.slots[0].as_mut() {
        slot.cores_x100 = Some(Sum {
            used: 5000,
            of: 8000,
            members: 0,
        });
    }
    assert_eq!(unread.check(), Err(Fault::SlotSum(0)));
}

#[test]
fn a_word_names_its_provider_and_its_scene() {
    // The registry's own realm words, in any case; a provider with no mark is Other.
    for (word, provider) in [
        ("claude", Provider::Anthropic),
        ("ANTHROPIC", Provider::Anthropic),
        ("oai", Provider::OpenAi),
        ("codex", Provider::OpenAi),
        ("gemini", Provider::Google),
        ("agy", Provider::Google),
        ("meta", Provider::Meta),
        ("grok", Provider::Xai),
        ("x", Provider::Xai),
        ("kimi:global", Provider::Other),
        ("muse", Provider::Other),
    ] {
        assert_eq!(Provider::of(word), provider, "{word}");
    }
    for provider in Provider::ALL {
        assert_eq!(Provider::ALL[provider.index()], provider);
        if provider != Provider::Other {
            assert!(keys::PROVIDERS.contains(&provider.word()), "{provider:?}");
            assert_eq!(Provider::of(provider.word()), provider);
        }
    }
    // A minute count changes as it crosses each whole minute, wherever in a minute it starts.
    assert_eq!(next_minute_change(0, 55 * 60_000), Some(60_000));
    assert_eq!(
        next_minute_change(30_000, 30_000 + 55 * 60_000),
        Some(90_000)
    );
    assert_eq!(next_minute_change(0, 54 * 60_000 + 1), Some(1));
    assert_eq!(next_minute_change(5, 5), None);
    for scene in Scene::ALL {
        assert_eq!(Scene::of(scene.word()), scene);
    }
    assert_eq!(Scene::of("1"), Scene::Design);
}
