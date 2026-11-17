// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A whole fleet at the lab's real scale, made of nothing real: [`inputs`] gives the registry,
//! the journal window, the network observation and one fleet read of a lab of [`MACHINES`]
//! machines, [`ACCOUNTS`] AI accounts and [`AGENTS`] agent sessions, so a lab instance and the
//! tests judge the tables at the size the owner's lab has without reading the owner's lab.
//!
//! It is deterministic: the same instant gives the same inputs, and every time in them is that
//! instant less a fixed span. Its machines are numbered with gaps (m1 to m296), most have no
//! Commons binding and most were never read, as in the lab: an unread cell is the common case
//! here, not the exception. Its addresses are under `fixture.example`, which no one holds.
//!
//! Nothing in a release reads it: this process's folder (`super::folder`) and
//! `aterm db fleet` take it only under the dev seam [`SEAM`] ([`under_seam`]).

use super::fold::{Inputs, Source};
use super::read::{
    Access, AccountWord, AgentSeen, Answer, Answered, Harness, Latest, Limits, Load, Look,
    MachineSeen, OutSrc, Pressure, Read, State, Tally, Task, TurnFacts, WaitTally, Work, WorkTally,
};
use crate::alab::account_names::Names;
use crate::alab::journal::event::{Event, SIGNIN_ENDED, SIGNIN_IN};
use crate::alab::journal::seen::{Claimed, Seen, SignedIn};
use crate::alab::sighted::{Obs, ObsState};
use crate::alab::view::{Account, LabView, Machine, MachineNickname, Preference};
use crate::typed::keys;
use aterm_digest::Sha256;
use aterm_json::json;
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkObservation, NetworkStateSnapshot, PeerObservation,
    Reachability,
};
use std::sync::Arc;

/// The registry's machines.
pub const MACHINES: usize = 216;
/// Of them, the ones the registry says are in use; the rest are planned.
pub const MACHINES_ACTIVE: usize = 164;
/// Of those, the ones a Commons facts row binds.
pub const MACHINES_BOUND: usize = 69;
/// Of those, the ones that answer the fleet read (this Mac among them).
pub const MACHINES_ANSWERING: usize = 61;
/// The registry's AI accounts.
pub const ACCOUNTS: usize = 147;
/// Of them, the active ones; two more are paused and the rest pending.
pub const ACCOUNTS_ACTIVE: usize = 36;
/// The accounts the journal holds a usage reading of.
pub const ACCOUNTS_READ: usize = 47;
/// The registry's preference rows.
pub const PREFERENCES: usize = 127;
/// The agent sessions the answering machines list.
pub const AGENTS: usize = 150;
/// The agent sessions whose account word names no registry account.
pub const AGENTS_UNRESOLVED: usize = 6;
/// The label of the one machine that answers and that the registry does not name.
pub const UNREGISTERED_LABEL: &str = "m300";
/// The Commons uuid of a Mac no machine row binds (a retired one), whose first 8 digits, the
/// name a fact gives such a Mac, are all decimal: a name that reads as a number.
pub const RETIRED_UUID: &str = "13000000a1b2c3d4e5f60718293a4b5c";

/// The dev seam that holds the fleet tables to this fixture's lab in place of this Mac's: debug
/// and dev-seams builds only, for `lab instance` and the galleries, which hold no registry and
/// no fleet.
pub const SEAM: &str = "ATERM_DEV_FLEET_FIXTURE";

/// The fixture's [`inputs`] as of `now_ms` where the dev seam [`SEAM`] is set, in a build that
/// compiles seams; `None` in every shipped binary.
pub fn under_seam(now_ms: u64) -> Option<Inputs> {
    aterm_types::dev_seam!(SEAM).map(|_| inputs(now_ms))
}

/// How many accounts each realm has; they sum to [`ACCOUNTS`].
const REALMS: [(&str, usize); 6] = [
    ("claude", 70),
    ("codex", 45),
    ("google", 15),
    ("grok", 8),
    ("kimi:global", 6),
    ("meta", 3),
];

/// Whether item `i` of `of` is one of `take` spread evenly over them.
const fn spread(i: usize, take: usize, of: usize) -> bool {
    i * take / of != (i + 1) * take / of
}

/// The harness that runs an account of `realm`, and the model word its sessions name.
fn harness_of(realm: &str, turn: usize) -> (Harness, &'static str) {
    match realm {
        "claude" => (Harness::Claude, "fable-5.2"),
        "codex" if turn % 2 == 0 => (Harness::Codex, "gpt-5.5-codex"),
        "codex" => (Harness::CodexAlab, "gpt-5.5-codex"),
        "google" => (Harness::Agy, "gemini-3-pro"),
        "grok" => (Harness::Grok, "grok-5"),
        "meta" => (Harness::Muse, "muse-2"),
        _ => (Harness::Kimi, "kimi-k3"),
    }
}

/// A machine's Commons uuid in the fixture: 16 bytes of a digest of its label.
fn uuid_of(label: &str) -> [u8; 16] {
    let mut sha = Sha256::new();
    sha.update(b"aterm/fleet/fixture/uuid\0");
    sha.update(label.as_bytes());
    let mut uuid = [0; 16];
    uuid.copy_from_slice(&sha.finalize()[..16]);
    uuid
}

/// One fixture account: its key, its realm, its slug and the address its sessions name.
struct Pooled {
    pk: String,
    realm: &'static str,
    slug: String,
    address: String,
}

/// One bound fixture machine: its key, its label and its uuid.
struct Bound {
    pk: String,
    label: String,
    uuid: [u8; 16],
    hex: String,
}

/// What agent `g` in `state` says of its turn; `older` for an aterm before the turn, which says
/// nothing of it but its agent's own word: `busy` for a busy or stalled agent, a box (`prompt`)
/// for every other waiting one.
fn turn_of(g: usize, state: State, older: bool) -> TurnFacts {
    let word_turn = matches!(state, State::Busy | State::Waiting | State::Stuck);
    if older {
        return TurnFacts {
            word_turn,
            ..TurnFacts::OLDER
        };
    }
    let open = word_turn;
    let ms = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let transcript = !matches!(g % 8, 6 | 7);
    TurnFacts {
        turn_open: Some(open),
        turn_age_ms: open.then(|| ms(g % 17) * 41_000),
        turn_end_age_ms: (g % 9 != 0).then(|| ms(g % 23) * 97_000 + 5_000),
        step_age_ms: (open && g % 3 == 0).then(|| ms(g % 29) * 61_000),
        out_age_ms: (g % 11 != 5).then(|| ms(g % 19) * 53_000),
        out_src: Some(if transcript {
            OutSrc::Transcript
        } else {
            OutSrc::Screen
        }),
        bg: Some(u32::try_from(g % 3).unwrap_or(u32::MAX)),
        older: false,
        word_turn,
    }
}

/// The look the fixture's `b`th bound machine sends, answering at about `now_ms`: a Mac or a
/// Linux box of 8 to 24 cores, some of them at a limit, a few with figures their host does not
/// read (no GPU, no link rate, no rate on a first look).
fn look_of(b: u64, now_ms: u64) -> Look {
    let linux = b % 9 == 4;
    let cores = 8 + (b % 3) * 8;
    let mem_total_mb = 16_384 << (b % 3);
    let mem_used_mb = mem_total_mb * (40 + b * 7 % 55) / 100;
    let busy = b * 13 % 100;
    let rated = b % 11 != 10;
    let up_for_ms = 3_600_000 * (b % 40 + 1);
    Look {
        age_ms: Some(b * 37 % 9_000),
        cores: Some(cores),
        load1_x100: Some(cores * busy * 6 / 5),
        cpu_busy_x100: rated.then_some(cores * busy),
        rate_win_ms: rated.then_some(60_000 + b * 101 % 9_000),
        mem_total_mb: Some(mem_total_mb),
        mem_used_mb: Some(mem_used_mb),
        mem_avail_mb: Some(mem_total_mb - mem_used_mb),
        // A Linux box here allocates no swap, so it has none in use.
        swap_used_mb: Some(if linux { 0 } else { b * 97 % 2_048 }),
        swap_total_mb: Some(if linux { 0 } else { 4_096 }),
        disk_total_mb: Some(476_000 << (b % 2)),
        gpu_cores: (!linux).then_some(10 + (b % 3) * 10),
        gpu_busy_pct: (!linux).then_some(b * 29 % 101),
        net_link_kbps: (b % 7 != 6).then_some(if b % 2 == 0 { 1_000_000 } else { 10_000_000 }),
        net_rx_kbps: rated.then_some(b * 3_109 % 400_000),
        net_tx_kbps: rated.then_some(b * 1_777 % 200_000),
        net_other_kbps: rated.then_some(b * 31 % 900),
        mem_pressure: Some(match b % 10 {
            8 => Pressure::Warn,
            9 => Pressure::Critical,
            _ => Pressure::Normal,
        }),
        limits: Limits {
            cpu: rated.then_some(busy >= 95 && cores * busy * 6 / 5 > cores * 100),
            mem: Some(b % 10 >= 8),
            disk: Some(b % 23 == 4),
            gpu: (!linux).then_some(b * 29 % 101 >= 95),
        },
        // The dial's level is the busier engine, lifted by pressure; its over here is the
        // floor, where the CPU is past its cores (at most 1.2 of them) or memory is critical.
        load: rated.then(|| Load {
            level_x100: busy
                .max(if linux { 0 } else { b * 29 % 101 })
                .max(match b % 10 {
                    8 => 88,
                    9 => 100,
                    _ => 0,
                }),
            over_x100: Some(
                if (busy >= 95 && cores * busy * 6 / 5 > cores * 100) || b % 10 == 9 {
                    12
                } else {
                    0
                },
            ),
        }),
        up_since_own_ms: now_ms.checked_sub(up_for_ms),
        up_for_ms: Some(up_for_ms),
        os: Some(if linux { "linux" } else { "macos" }),
        target: Some(
            if linux {
                "x86_64-unknown-linux-gnu"
            } else {
                "aarch64-apple-darwin"
            }
            .to_owned(),
        ),
    }
}

/// Each account of `lab`, a view built by hand, named as `alab::account_names` names a registry
/// row whose label the owner never wrote: its slug from its realm and login address, and its
/// provider word and address.
pub fn name_accounts(lab: &mut LabView) {
    let rows: Vec<keys::Slugged<'_>> = lab
        .accounts
        .values()
        .map(|account| keys::Slugged {
            pk: &account.pk,
            realm: account.realm.as_deref().unwrap_or_default(),
            handle: None,
            claims_email: account.identity.as_deref().or(account.login.as_deref()),
            live: account.live,
        })
        .collect();
    let mut slugs = keys::account_slugs(&rows);
    lab.names = lab
        .accounts
        .values()
        .map(|account| {
            let email = account.identity.clone().or_else(|| account.login.clone());
            let names = Names {
                slug: slugs.remove(&account.pk).unwrap_or_default(),
                name: account
                    .realm
                    .as_deref()
                    .zip(email.as_deref())
                    .and_then(|(realm, email)| keys::account_name(realm, email)),
                email,
                login: account.login.clone(),
                live: account.live,
            };
            (account.pk.clone(), names)
        })
        .collect();
    for account in lab.accounts.values_mut() {
        if let Some(names) = lab.names.get(&account.pk) {
            account.slug.clone_from(&names.slug);
            account.name.clone_from(&names.name);
        }
    }
}

/// The fixture's inputs as of `now_ms` (Unix milliseconds).
pub fn inputs(now_ms: u64) -> Inputs {
    let now_s = now_ms / 1_000;
    let span = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let mut lab = LabView::default();

    // Machines: m1 to m296 with ten of every 37 numbers unused.
    let mut bound: Vec<Bound> = Vec::new();
    let mut active = 0;
    for (i, n) in (1..=296_u32).filter(|n| n % 37 >= 10).enumerate() {
        let label = format!("m{n}");
        let pk = keys::machine_pk(&label);
        let planned = i % 4 == 3 && i < 208;
        if !planned {
            if spread(active, MACHINES_BOUND, MACHINES_ACTIVE) {
                let uuid = uuid_of(&label);
                bound.push(Bound {
                    pk: pk.clone(),
                    label: label.clone(),
                    uuid,
                    hex: aterm_codec::hex::encode_lower(&uuid),
                });
            }
            active += 1;
        }
        let (os, chip, ram_gb) = match i % 5 {
            _ if planned => (None, None, None),
            0 => (Some("macOS 27.0"), Some("M4 Max"), Some(128)),
            1 => (Some("macOS 27.0"), Some("M4 Pro"), Some(64)),
            2 => (Some("macOS 26.4"), Some("M3 Ultra"), Some(256)),
            3 => (Some("macOS 27.0"), Some("M5"), Some(32)),
            _ => (Some("macOS 26.4"), Some("M2 Max"), None),
        };
        lab.machines.insert(
            pk.clone(),
            Machine {
                pk,
                label: Some(label),
                nickname: MachineNickname::Unset,
                aliases: Vec::new(),
                status: Some(if planned { "planned" } else { "active" }.to_owned()),
                os: os.map(str::to_owned),
                chip: chip.map(str::to_owned),
                ram_gb,
                live: true,
            },
        );
    }
    for machine in &bound {
        lab.bound.insert(machine.hex.clone(), machine.pk.clone());
    }
    lab.me = bound.first().map(|me| me.pk.clone());

    // Accounts: each realm's share, a quarter of them active.
    let mut pool: Vec<Pooled> = Vec::new();
    let mut read: Vec<usize> = Vec::new();
    let mut in_use: Vec<usize> = Vec::new();
    let mut idle = 0;
    let realms = REALMS
        .iter()
        .flat_map(|(realm, count)| std::iter::repeat_n(*realm, *count));
    for (i, realm) in realms.enumerate() {
        let address = format!("pool{i:03}@fixture.example");
        let Ok(pk) = keys::account_pk(realm, &address) else {
            continue;
        };
        let status = if spread(i, ACCOUNTS_ACTIVE, ACCOUNTS) {
            read.push(i);
            in_use.push(i);
            "active"
        } else {
            idle += 1;
            match idle {
                21 | 71 => "paused",
                n if n <= ACCOUNTS_READ - ACCOUNTS_ACTIVE => {
                    read.push(i);
                    "pending"
                }
                _ => "pending",
            }
        };
        lab.accounts.insert(
            pk.clone(),
            Account {
                pk: pk.clone(),
                provider: keys::provider_of_realm(realm).map(str::to_owned),
                realm: Some(realm.to_owned()),
                slug: String::new(),
                name: None,
                legacy_label: None,
                login: Some(address.clone()),
                identity: None,
                status: Some(status.to_owned()),
                // The registry states a plan, by its plan row's key, for the active accounts
                // alone, as a person kept it.
                plan: (status == "active")
                    .then(|| keys::provider_of_realm(realm))
                    .flatten()
                    .map(|provider| {
                        format!("{provider}-{}", if i % 3 == 0 { "max" } else { "pro" })
                    }),
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
        pool.push(Pooled {
            pk,
            realm,
            slug: String::new(),
            address,
        });
    }
    name_accounts(&mut lab);
    for pooled in &mut pool {
        pooled.slug = lab.slug(&pooled.pk).unwrap_or_default().to_owned();
    }

    // Preferences: the accounts with readings, spread over the bound machines.
    for p in 0..PREFERENCES {
        lab.preferences.push(Preference {
            machine: bound[p % bound.len()].pk.clone(),
            account: pool[read[(p * 5) % read.len()]].pk.clone(),
            rank: Some(span(p / bound.len() + 1)),
        });
    }
    lab.preferences
        .sort_by(|a, b| (&a.machine, a.rank, &a.account).cmp(&(&b.machine, b.rank, &b.account)));

    // The journal: a reading of each window of 47 accounts, some past their reset, some walled.
    let mut events: Vec<(String, Event)> = Vec::new();
    let mut seen_rows: Vec<(usize, usize)> = Vec::new();
    for (r, i) in read.iter().copied().enumerate() {
        let account = &pool[i];
        let by = &bound[r % bound.len()];
        let cli = harness_of(account.realm, 0).0.word();
        let t = now_s.saturating_sub(600 + span(r) * 45);
        let reset_5h = if r % 9 == 4 {
            now_s.saturating_sub(120)
        } else {
            now_s + 600 + span(r) * 300
        };
        let mut log = |value: aterm_json::Value| {
            if let Some(event) = Event::from_value(&value) {
                events.push((by.hex.clone(), event));
            }
        };
        log(json!({
            "a": account.pk.as_str(), "c": cli, "k": "quota", "src": "statusline", "t": t,
            "w": "5h", "p": span(r * 53 % 1000), "r": reset_5h, "rr": "minute",
        }));
        if r % 12 != 5 {
            log(json!({
                "a": account.pk.as_str(), "c": cli, "k": "quota", "src": "statusline", "t": t,
                "w": "7d", "p": span(r * 31 % 1000), "r": now_s + 86_400 + span(r) * 3_600,
                "rr": "minute",
            }));
        }
        if r % 16 == 3 {
            log(json!({
                "a": account.pk.as_str(), "c": cli, "k": "wall", "src": "cli-output", "t": t,
                "w": "7d", "u": now_s + 7_200,
            }));
        }
        // Two readings in five carry the plan the CLI reported: for some accounts the registry
        // states none, for some the one it states, for some another.
        if r % 5 < 2 {
            log(json!({
                "a": account.pk.as_str(), "c": cli, "k": "plan-seen", "src": "cli-output",
                "t": t, "plan": if r % 2 == 0 { "max" } else { "pro" },
            }));
        }
        // One account in three runs in a tab on its Mac, sighted twenty minutes ago; for some
        // of them no Mac states a sign-in word, so the sighting is all that was observed.
        if r % 3 == 0 {
            log(json!({
                "a": account.pk.as_str(), "c": cli, "k": "seen", "src": "profile",
                "t": now_s.saturating_sub(1_200 + span(r) * 30), "x": format!("{r:08x}"),
            }));
        }
        // One in seven was found by another Mac's sweep days ago: a login place and no tab.
        if r % 7 == 3 {
            let other = &bound[(r + 1) % bound.len()];
            if let Some(event) = Event::from_value(&json!({
                "a": account.pk.as_str(), "c": cli, "k": "seen", "src": "profile",
                "t": now_s.saturating_sub(3 * 86_400 + span(r) * 600),
            })) {
                events.push((other.hex.clone(), event));
            }
        }
        // One in eleven was found, nine days ago, by the sweep of a Mac retired since.
        if r % 11 == 5
            && let Some(event) = Event::from_value(&json!({
                "a": account.pk.as_str(), "c": cli, "k": "seen", "src": "profile",
                "t": now_s.saturating_sub(9 * 86_400 + span(r) * 600),
            }))
        {
            events.push((RETIRED_UUID.to_owned(), event));
        }
        // One in ten has a registry row from a third Mac that says two tabs run it.
        if r % 10 == 7 {
            lab.account_obs.push(Obs {
                account: account.pk.clone(),
                uuid: bound[(r + 2) % bound.len()].hex.clone(),
                at_ms: now_ms.saturating_sub(900_000),
                state: ObsState::Running,
                tabs: 2,
            });
        }
        seen_rows.push((r, i));
    }
    let mut seen = Seen::of_events(&events, now_s);
    for (r, i) in seen_rows {
        let account = &pool[i];
        let by = &bound[r % bound.len()];
        seen.last_use.insert(
            account.pk.clone(),
            (by.hex.clone(), now_s.saturating_sub(span(r) * 60)),
        );
        if r % 4 == 0 {
            seen.signins.entry(account.pk.clone()).or_default().insert(
                by.hex.clone(),
                SignedIn {
                    state: SIGNIN_IN,
                    t: now_s.saturating_sub(86_400),
                    cli: harness_of(account.realm, 0).0.word().to_owned(),
                },
            );
        }
        // One account in eight was signed in on its Mac and is not now: its login ended.
        if r % 8 == 2 {
            seen.signins.entry(account.pk.clone()).or_default().insert(
                by.hex.clone(),
                SignedIn {
                    state: SIGNIN_ENDED,
                    t: now_s.saturating_sub(43_200),
                    cli: harness_of(account.realm, 0).0.word().to_owned(),
                },
            );
        }
        if r % 6 == 1 {
            seen.claims
                .entry(account.pk.clone())
                .or_default()
                .push(Claimed {
                    uuid: by.hex.clone(),
                    kind: "home".to_owned(),
                    since: now_s.saturating_sub(3_600),
                    until: now_s + 3_600,
                });
        }
    }
    for (b, machine) in bound.iter().enumerate() {
        seen.sums.insert(
            machine.hex.clone(),
            json!({
                "at": now_s.saturating_sub(3_600 * (span(b) % 30 + 1)),
                "aterm": if b % 3 == 0 { "0.141.0" } else { "0.140.0" },
                "label": machine.label.as_str(),
            }),
        );
    }

    // One fleet read: this Mac's own answer, 60 peers that answer, four that were asked and
    // were silent, four the book does not name, and one answer the registry does not name.
    let read_at_ms = now_ms.saturating_sub(20_000);
    let actives: Vec<&Pooled> = in_use.iter().map(|i| &pool[*i]).collect();
    let mut machines: Vec<MachineSeen> = Vec::new();
    let mut peers: Vec<PeerObservation> = Vec::new();
    let mut session = 0;
    let mut answering = 0;
    for (b, machine) in bound.iter().enumerate() {
        let answers = b % 8 != 7;
        if b > 0 {
            peers.push(PeerObservation {
                slot: span(b),
                identity: MachineIdentity {
                    machine_id: machine.uuid,
                    incarnation: [0; 16],
                    label: None,
                },
                first_seen_ms: now_ms.saturating_sub(86_400_000 * (span(b) + 1)),
                last_seen_ms: if answers {
                    now_ms.saturating_sub(5_000)
                } else {
                    now_ms.saturating_sub(3_600_000 * span(b))
                },
                last_checked_ms: answers.then(|| now_ms.saturating_sub(5_000)),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: if answers {
                    Reachability::Reachable
                } else {
                    Reachability::LastSeen
                },
                lease_ms: None,
                instances: None,
                samples: Vec::new(),
            });
        }
        let dial = format!("{}-fixture.local", machine.label);
        let short = format!("{}-fixture", machine.label);
        // One peer in five runs an aterm whose header names no label: its host name does.
        let header = (b % 5 != 3).then_some(machine.label.as_str());
        if !answers {
            if b % 16 == 7 {
                machines.push(MachineSeen::peer(
                    &dial,
                    &short,
                    None,
                    Answer::Silent {
                        why: Some("nothing-listens".to_owned()),
                    },
                ));
            }
            continue;
        }
        let count = if answering < AGENTS - 2 * MACHINES_ANSWERING {
            3
        } else {
            2
        };
        answering += 1;
        let mut tally = Tally::default();
        let mut work = WorkTally::default();
        let mut agents: Vec<AgentSeen> = Vec::new();
        for _ in 0..count {
            let g = session;
            session += 1;
            let account = actives[g % actives.len()];
            let (harness, model) = harness_of(account.realm, g);
            let state = match g % 7 {
                0..=2 => State::Busy,
                3 => State::Idle,
                4 => State::Waiting,
                5 => State::Walled,
                _ if g % 14 == 6 => State::Stuck,
                _ => State::Unknown,
            };
            tally.add(state);
            let turn = turn_of(g, state, b % 6 == 5);
            work.add(Work::of(state, &turn));
            let percent = |of: usize| u8::try_from(of % 101).ok();
            // Counters only where its row sends a whole conversation's: one agent in five, none.
            let tokens = (!turn.older && g % 5 != 2).then(|| span(g) * 41_000);
            agents.push(AgentSeen {
                sid: format!("s{:06x}", 0x51_0000 + g * 0x1f3),
                harness,
                state,
                state_why: (state == State::Walled).then(|| "usage-weekly".to_owned()),
                state_age_ms: (g % 13 != 0).then(|| span(g) * 13_000),
                task: match state {
                    State::Busy => Some(Task::Working),
                    State::Stuck => Some(Task::Stalled),
                    State::Idle if g % 2 == 0 => Some(Task::Done),
                    _ => None,
                },
                ctx_left_pct: (g % 11 != 0).then(|| percent(g * 7)).flatten(),
                usage_5h_pct: (g % 4 != 3).then(|| percent(g * 3)).flatten(),
                usage_7d_pct: (g % 4 != 3).then(|| percent(g * 5)).flatten(),
                account: if g % 10 == 9 {
                    None
                } else if g % 25 == 7 {
                    AccountWord::new(&format!("stranger{g}@fixture.example"))
                } else if g % 3 == 0 {
                    AccountWord::new(&account.slug)
                } else {
                    AccountWord::new(&account.address)
                },
                model: Some(model.to_owned()),
                // One agent in nine is pinned; on an aterm before the pin nobody says.
                pinned: (b % 6 != 5).then_some(g % 9 == 4),
                turn,
                tok_in: tokens,
                tok_out: tokens.map(|tokens| tokens / 23),
                tok_for_ms: tokens.map(|_| span(g) * 60_000),
                turns: (!turn.older).then(|| u32::try_from(g % 40).unwrap_or(u32::MAX)),
                tools: (!turn.older && harness != Harness::Agy && harness != Harness::Muse)
                    .then(|| u32::try_from(g * 3 % 400).unwrap_or(u32::MAX)),
                work: Work::of(state, &turn),
                answer_at_ms: (b % 6 != 5).then(|| now_ms - 1_000 - span(b) * 7),
                rate: None,
            });
        }
        // One machine in six runs an aterm before the look: it says nothing of its resources
        // and nothing of its fleet access.
        let older = b % 6 == 5;
        let answer = Answer::Answered(Box::new(Answered {
            build: Some(if b % 3 == 0 { "0.141.0" } else { "0.140.0" }.to_owned()),
            answer_at_ms: (!older).then(|| now_ms - 1_000 - span(b) * 7),
            home_free_mb: (!older).then(|| 40_000 + span(b) * 9_000),
            look: if older {
                Look::UNREAD
            } else {
                look_of(span(b), now_ms)
            },
            access: (!older).then_some(match b % 4 {
                0 => Access::Read,
                1 => Access::Write,
                _ => Access::Full,
            }),
            // This Mac's own answer reads no grant (`Answered::drive`).
            drive: (!older && b != 0).then_some(b % 4 != 0),
            sessions: u32::try_from(count + b % 3).ok(),
            tally: (b % 10 != 9).then_some(tally),
            work: (b % 10 != 9).then_some(work),
            // One machine in seven has a build waiting for memory; one in fourteen of those
            // did not read its claim.
            wait: (!older).then(|| {
                let mut wait = WaitTally::NONE;
                if b % 7 == 2 {
                    wait.add((b % 14 != 2).then(|| 6_144 + span(b) * 512));
                }
                wait
            }),
            agents,
            agents_more: 0,
        }));
        machines.push(if b == 0 {
            MachineSeen::own(header, answer)
        } else {
            MachineSeen::peer(&dial, &short, header, answer)
        });
    }
    machines.push(MachineSeen::peer(
        "m300-spare.local",
        "m300-spare",
        Some(UNREGISTERED_LABEL),
        Answer::Answered(Box::new(Answered {
            build: Some("0.139.0".to_owned()),
            answer_at_ms: None,
            home_free_mb: None,
            look: Look::UNREAD,
            access: None,
            drive: None,
            sessions: Some(1),
            tally: Some(Tally::default()),
            work: Some(WorkTally::default()),
            wait: None,
            agents: Vec::new(),
            agents_more: 0,
        })),
    ));
    let latest = Latest::default().with(Read {
        read_at_ms,
        machines,
        fleet: true,
    });

    let network = NetworkStateSnapshot {
        revision: 1,
        network: NetworkObservation {
            revision: 1,
            observed_at_ms: now_ms,
            history: HistoryState::Ready,
            peers,
            ..NetworkObservation::default()
        },
        ..NetworkStateSnapshot::default()
    };
    Inputs {
        lab: Source::Live {
            value: Arc::new(lab),
            at_ms: now_ms,
        },
        journal: Source::Live {
            value: Arc::new(seen),
            at_ms: now_ms,
        },
        network: Source::Live {
            value: Arc::new(network),
            at_ms: now_ms,
        },
        read: Source::Live {
            value: Arc::new(latest),
            at_ms: read_at_ms,
        },
    }
}
