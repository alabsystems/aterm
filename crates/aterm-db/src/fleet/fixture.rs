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
    AccountWord, AgentSeen, Answer, Answered, Harness, Latest, MachineSeen, Read, State, Tally,
    Task,
};
use crate::alab::journal::event::{Event, SIGNIN_IN};
use crate::alab::journal::seen::{Claimed, Seen, SignedIn};
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

/// One fixture account: its key, its realm, its label and the address its sessions name.
struct Pooled {
    pk: String,
    realm: &'static str,
    label: String,
    address: String,
}

/// One bound fixture machine: its key, its label and its uuid.
struct Bound {
    pk: String,
    label: String,
    uuid: [u8; 16],
    hex: String,
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
        let label = keys::default_account_label(realm, &pk).unwrap_or_else(|_| format!("pool-{i}"));
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
                label: Some(label.clone()),
                legacy_label: None,
                login: Some(address.clone()),
                identity: None,
                status: Some(status.to_owned()),
                plan: (status == "active")
                    .then(|| if i % 3 == 0 { "max" } else { "pro" }.to_owned()),
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
        pool.push(Pooled {
            pk,
            realm,
            label,
            address,
        });
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
            let percent = |of: usize| u8::try_from(of % 101).ok();
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
                    AccountWord::new(&account.label)
                } else {
                    AccountWord::new(&account.address)
                },
                model: Some(model.to_owned()),
            });
        }
        let answer = Answer::Answered(Answered {
            build: Some(if b % 3 == 0 { "0.141.0" } else { "0.140.0" }.to_owned()),
            home_free_mb: (b % 6 != 5).then(|| 40_000 + span(b) * 9_000),
            sessions: u32::try_from(count + b % 3).ok(),
            tally: (b % 10 != 9).then_some(tally),
            agents,
            agents_more: 0,
        });
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
        Answer::Answered(Answered {
            build: Some("0.139.0".to_owned()),
            home_free_mb: None,
            sessions: Some(1),
            tally: Some(Tally::default()),
            agents: Vec::new(),
            agents_more: 0,
        }),
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
