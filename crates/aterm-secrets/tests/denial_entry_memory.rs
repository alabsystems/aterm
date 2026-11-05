// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A reserved entry read does not retain unrelated decrypted values.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes, client,
    commons::bootstrap::Bootstrap,
    membership_log::{Draft, EpochAudience, Inserted, Log, encode_records},
    text::{Audience, Entry, MAX_VALUE},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every pointer and unchanged layout is forwarded to System. Constant TLS counters
// allocate nothing. The default realloc counts both blocks while growing an allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's valid layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = ACTIVE.try_with(|active| {
                if active.get() {
                    let _ = LIVE.try_with(|live| {
                        let next = live.get() + layout.size();
                        live.set(next);
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(next)));
                    });
                }
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = LIVE.try_with(|live| live.set(live.get() - layout.size()));
            }
        });
        // SAFETY: forwarding the matching pointer and layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measuring;

// The public reads share bounded process-wide caches, while this allocator counts only
// the current thread. Another fixture can evict a warmed snapshot during measurement,
// moving its replacement's ownership into that cache and its eventual drop to a different
// thread. Isolate whole scenarios so the count measures plaintext resolution, as intended.
static SCENARIO: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

const VALUES: usize = 24;
const VALUE_BYTES: usize = 512 * 1024;

fn fixture(paths: &client::Paths, vault: &std::path::Path, denied: &str) -> Log {
    client::create(paths, vault.to_str().unwrap(), None, Some("m1")).unwrap();
    let roster = format!("deny_nodes = [\"{denied}\"]\n");
    let initial_roster = format!("deny_nodes = [\"{}\"]\n", "e".repeat(32));
    let admin = client::admin_open(paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(
            &Audience::Everyone,
            aterm_secrets::SWARM_ROSTER_ENTRY,
            initial_roster.as_bytes(),
        )
        .unwrap();
    client::admin_publish(paths, &admin, &document).unwrap();
    client::sync(paths, None).unwrap();
    client::seed_vault_genesis(paths, &paths.home.join("genesis.alm")).unwrap();
    // Publish the small seeded document once more so its envelope names the log frontier.
    // The subsequent large values remain only in the log; no oversized document is published.
    let seeded = client::admin_open(paths).unwrap();
    client::admin_publish(paths, &seeded, &seeded.doc).unwrap();
    client::sync(paths, None).unwrap();
    let bootstrap = Bootstrap::required(paths).unwrap();
    let request = bootstrap.request().unwrap();
    let actor = request.binding().actor();
    let incarnation = *request.binding().incarnation();
    let mut log = client::load_records(paths).unwrap().log().unwrap().clone();
    assert!(log.membership().members().contains_key(&actor));
    let authored = log.records().map(|r| r.authored_ms()).max().unwrap() + 1;
    let generation = log
        .records()
        .filter_map(|record| match record.body() {
            aterm_secrets::membership_log::Body::Epoch {
                audience: EpochAudience::Everyone,
                generation,
                ..
            } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let sign = |draft: Draft| {
        bootstrap
            .device()
            .with_seed(|seed| draft.sign(seed, incarnation))
            .unwrap()
    };
    let (draft, key) = log
        .draft_epoch(&actor, authored, EpochAudience::Everyone, generation)
        .unwrap();
    let epoch = sign(draft);
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    let wrap = log
        .draft_wrap(
            &actor,
            authored + 1,
            epoch_id,
            &key,
            request.binding().recipient(),
        )
        .unwrap();
    assert_eq!(log.insert(sign(wrap)).unwrap(), Inserted::Held);
    for n in 0..=VALUES {
        let entry = if n == VALUES {
            Entry {
                name: aterm_secrets::SWARM_ROSTER_ENTRY.into(),
                value: SecretBytes::new(roster.as_bytes().to_vec()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            }
        } else {
            Entry {
                name: format!("unrelated-{n:02}"),
                value: SecretBytes::new(vec![n as u8; VALUE_BYTES]),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            }
        };
        let put = log
            .draft_put(&actor, authored + 2 + n as u64, epoch_id, &key, &entry)
            .unwrap();
        assert_eq!(log.insert(sign(put)).unwrap(), Inserted::Held);
    }
    let wire = encode_records(log.records());
    assert!(wire.len() > VALUES * VALUE_BYTES);
    client::save_records(paths, &wire).unwrap();
    assert_eq!(
        client::load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .held_count(),
        log.held_count()
    );
    let identity = client::read_machine_identity(paths).unwrap().unwrap();
    let opened = log.open_entries(&identity).unwrap().unwrap();
    let unrelated: Vec<_> = opened
        .iter()
        .filter(|entry| entry.name.starts_with("unrelated-"))
        .collect();
    assert_eq!(
        unrelated.len(),
        VALUES,
        "every large value is a live winner"
    );
    assert!(
        unrelated
            .iter()
            .all(|entry| entry.value.len() == VALUE_BYTES)
    );
    log
}

#[test]
fn public_denial_read_bounds_unrelated_plaintext_retention() {
    let _scenario = SCENARIO.lock().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&home.path().join("member"));
    let expected = vec!["f".repeat(32)];
    // Signed records and ciphertext remain owned outside the measured interval. This is
    // specifically the plaintext resolution cost, not a cold-Log or total-process bound.
    let original = fixture(&paths, &home.path().join("team.vault"), &expected[0]);
    // The envelope retains a different deny value: this result proves the public read chose
    // the new log-backed roster rather than bypassing the large log through the envelope.
    for _ in 0..2 {
        assert_eq!(
            client::swarm_denied_nodes(&paths).unwrap(),
            Some(expected.clone())
        );
    }

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let denied = client::swarm_denied_nodes(&paths).unwrap();
    assert_eq!(denied.as_ref(), Some(&expected));
    drop(denied);
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    let peak = PEAK.with(Cell::get);
    // One maximum-sized value and its decoding/copy scratch fit five MAX_VALUE buffers;
    // the small, warmed signed history and filesystem/roster metadata receive 1 MiB more.
    let bound = 5 * MAX_VALUE + 1024 * 1024;
    println!(
        "denial entries={VALUES} value_bytes={VALUE_BYTES} records={} peak={peak} bound={bound}",
        original.held_count()
    );
    assert!(
        peak <= bound,
        "denial plaintext peak {peak} exceeds {bound}"
    );
}

#[test]
fn public_local_roster_read_bounds_unrelated_plaintext_retention() {
    let _scenario = SCENARIO.lock().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&home.path().join("member"));
    let denied = "f".repeat(32);
    let host_denied = "a".repeat(32);
    let mut original = fixture(&paths, &home.path().join("team.vault"), &denied);
    let bootstrap = Bootstrap::required(&paths).unwrap();
    let request = bootstrap.request().unwrap();
    let actor = request.binding().actor();
    let member_name = original.membership().members()[&actor].name.clone();
    let hosts = vec![member_name];
    let incarnation = *request.binding().incarnation();
    let authored = original.records().map(|r| r.authored_ms()).max().unwrap() + 1;
    let sign = |draft: Draft| {
        bootstrap
            .device()
            .with_seed(|seed| draft.sign(seed, incarnation))
            .unwrap()
    };
    let (draft, key) = original
        .draft_epoch(&actor, authored, EpochAudience::Hosts(hosts.clone()), 1)
        .unwrap();
    let epoch = sign(draft);
    let epoch_id = *epoch.id();
    assert_eq!(original.insert(epoch).unwrap(), Inserted::Held);
    let wrap = original
        .draft_wrap(
            &actor,
            authored + 1,
            epoch_id,
            &key,
            request.binding().recipient(),
        )
        .unwrap();
    assert_eq!(original.insert(sign(wrap)).unwrap(), Inserted::Held);
    let host_roster = Entry {
        name: aterm_secrets::SWARM_ROSTER_ENTRY.into(),
        value: SecretBytes::new(format!("deny_nodes = [\"{host_denied}\"]\n").into_bytes()),
        audience: Audience::Hosts(hosts),
        if_absent: false,
        anchor: None,
    };
    let put = original
        .draft_put(&actor, authored + 2, epoch_id, &key, &host_roster)
        .unwrap();
    assert_eq!(original.insert(sign(put)).unwrap(), Inserted::Held);
    client::save_records(&paths, &encode_records(original.records())).unwrap();
    let membership = original.membership();
    assert_eq!(membership.members().len(), 1);
    assert!(membership.unproved().is_empty());
    let member = membership.members().values().next().unwrap();
    assert!(
        aterm_secrets::swarm_roster::leading_m_label(&member.label)
            .is_some_and(|(_, canonical)| canonical == member.label)
    );
    assert_eq!(member.entry, *original.network());
    assert!(member.admitted_by.is_none());
    // The base visible roster is Host-specific; the local helper also retains Everyone's
    // deny value. Neither uses the covered envelope's older value. The membership overlay
    // contributes the sole founder with its signed name/binding.
    let expected = aterm_secrets::swarm_roster::SwarmRoster {
        members: vec![aterm_secrets::swarm_roster::SwarmMember {
            recipient: member.binding.recipient().to_bech32(),
            vault_name: member.name.clone(),
            label: member.label.clone(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: original.genesis().authored_ms(),
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: member
                .node_key
                .filter(|key| *key != [0; 32])
                .map(|key| aterm_codec::hex::encode_lower(&key)),
        }],
        deny_labels: Vec::new(),
        deny_nodes: vec![host_denied.clone(), denied],
    };
    // Confirm the ordinary visible entry is the Host value, so the Everyone union below
    // would fail if this consumer accidentally used the merged-name resolver.
    assert_eq!(
        client::swarm_denied_nodes(&paths).unwrap(),
        Some(vec![host_denied])
    );
    // Warm signed-record and roster caches, while each public call still resolves entries.
    for _ in 0..2 {
        assert_eq!(
            client::local_swarm_roster_for_log(&paths, &original).unwrap(),
            Some(expected.clone())
        );
    }

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let roster = client::local_swarm_roster_for_log(&paths, &original).unwrap();
    assert_eq!(roster.as_ref(), Some(&expected));
    drop(roster);
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    let peak = PEAK.with(Cell::get);
    // Match the public denial budget: five maximum value buffers plus 1 MiB for
    // filesystem, signed-history and roster metadata, excluding the held ciphertext.
    let bound = 5 * MAX_VALUE + 1024 * 1024;
    println!(
        "local-roster entries={VALUES} value_bytes={VALUE_BYTES} records={} peak={peak} bound={bound}",
        original.held_count()
    );
    assert!(
        peak <= bound,
        "local roster plaintext peak {peak} exceeds {bound}"
    );
}
