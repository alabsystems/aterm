// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed calls of `claude_token.rs` (DESIGN-claude-setup-token S4) on a scratch log vault with
//! three seated members: m22 (the admin), m7 and m40 (members with no admin key). Declared beside
//! the removal tests for their lab. No assertion prints a value: the fixture token appears in no
//! test output.

use super::*;
use crate::claude_token::{
    Check, ExpiryFrom, Key, Lookup, Meta, Mismatch, SOON_SECS, Standing, State, Verified,
};
use crate::membership_log::entries::OUTER_ONLY;

/// The fixture token: inert, in the setup-token grammar.
const TOKEN: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-0123456789").as_bytes();

fn key() -> Key {
    Key::parse("ac-0123456789abcdef").unwrap()
}

/// A person's patience: the hang detector, never a latency budget.
fn soon() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60)
}

fn refuse() -> OnBusy {
    OnBusy::Refuse(soon())
}

fn meta(expires_at: u64) -> Meta {
    Meta {
        email: "inert@example.invalid".into(),
        account_id: "c-0123456789ab".into(),
        minted_at: 1_000,
        expires_at,
        expiry_from: ExpiryFrom::Assumed,
        minted_on: "m7".into(),
        cli: "2.1.318".into(),
        verified: None,
    }
}

/// A year from now.
fn a_year() -> u64 {
    aterm_time::unix_secs().unwrap() + 31_536_000
}

/// Whether `bytes` holds any run of eight bytes of the fixture token.
fn holds_token(bytes: &[u8]) -> bool {
    bytes
        .windows(8)
        .any(|window| TOKEN.windows(8).any(|run| run == window))
}

/// Three seated members, `[everyone] shared` held: m22, m7, m40.
struct Lab {
    macs: TwoMacs,
    m40: Paths,
}

impl Lab {
    fn new() -> Self {
        let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
        let (m40, _, _) = let_in(&macs, "m40", "m40");
        sync(&macs.m22, None).expect("m22 syncs");
        sync(&macs.m7, None).expect("m7 syncs");
        Self { macs, m40 }
    }

    fn all(&self) -> [&Paths; 3] {
        [&self.macs.m22, &self.macs.m7, &self.m40]
    }

    fn sync_all(&self) {
        for paths in self.all() {
            sync(paths, None).expect("syncs");
        }
    }

    /// m7, a member with no admin key, writes the fixture token, and every Mac syncs.
    fn minted(self) -> Self {
        assert!(matches!(can_administer(&self.macs.m7), Ok(false)));
        let written = crate::claude_token::write(
            &self.macs.m7,
            &key(),
            &SecretBytes::from(TOKEN),
            &meta(a_year()),
            refuse(),
        )
        .unwrap();
        assert_eq!(written, MemberWritten::Posted);
        self.sync_all();
        self
    }
}

fn read(paths: &Paths) -> crate::claude_token::TokenRead {
    match crate::claude_token::read(paths, &key(), std::time::Duration::from_secs(60)).unwrap() {
        Lookup::Found(read) => read,
        Lookup::Absent => panic!("absent"),
        Lookup::Unknown => panic!("unknown"),
    }
}

fn standing_of(paths: &Paths) -> Standing {
    read(paths).standing
}

/// Whether the read on `paths` returns exactly the fixture token.
fn returns_token(paths: &Paths) -> bool {
    read(paths)
        .token()
        .is_some_and(|token| token.as_slice() == TOKEN)
}

/// Whether the Mac at `paths` has a file, or a manifest row, of either of the token's names.
fn materialized(paths: &Paths) -> bool {
    let state = load_state(paths).unwrap();
    [key().entry(), key().meta_entry()].iter().any(|name| {
        paths.home.join(".secrets").join(name).exists() || state.manifest.files.contains_key(name)
    })
}

/// `write` then `read` returns the value on each of three members, the standing reads one draft,
/// and neither name ever becomes a file. A write of a value outside the grammar is refused.
#[test]
fn write_then_read_returns_the_value_on_each_member_and_never_materializes() {
    let lab = Lab::new().minted();
    for paths in lab.all() {
        assert!(returns_token(paths), "each member reads the token");
        assert_eq!(standing_of(paths).check, Check::Readable);
        assert!(!materialized(paths), "the class never materializes");
    }
    let refused = crate::claude_token::write(
        &lab.macs.m7,
        &key(),
        &SecretBytes::from(b"sk-ant-oat01-x\ny".as_slice()),
        &meta(a_year()),
        refuse(),
    )
    .unwrap();
    assert!(matches!(refused, MemberWritten::Refused(_)));
    // Writing again replaces both in place, in one draft.
    let mut later = meta(a_year());
    later.minted_at = 2_000;
    let next: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-replaced").as_bytes();
    assert_eq!(
        crate::claude_token::write(
            &lab.macs.m7,
            &key(),
            &SecretBytes::from(next),
            &later,
            refuse()
        )
        .unwrap(),
        MemberWritten::Posted
    );
    sync(&lab.macs.m22, None).unwrap();
    let replaced = read(&lab.macs.m22);
    assert!(replaced.token().is_some_and(|t| t.as_slice() == next));
    assert_eq!(replaced.standing.meta.unwrap().minted_at, 2_000);
    lab.macs.server.stop();
}

/// What a build before S4 sees: decoding only the outer layer, the entries hold no byte of the
/// fixture token, and so neither does the file its materializer would write (the value, at the
/// entry's name), nor the vault's records. CONTROL: a current build opens it.
#[test]
fn an_outer_layer_only_decode_holds_no_byte_of_the_token() {
    let lab = Lab::new().minted();
    let m22 = &lab.macs.m22;
    let identity = read_identity(m22).unwrap().unwrap();
    let log = log_of(m22);
    OUTER_ONLY.with(|on| on.set(true));
    let outer = log.open_audience_entries(&identity);
    OUTER_ONLY.with(|on| on.set(false));
    let outer = outer.unwrap().unwrap();
    let everyone = &outer.by_audience[&Audience::Everyone];
    for name in [key().entry(), key().meta_entry()] {
        let value = everyone[&name].value.as_slice();
        assert!(
            value.starts_with(&crate::membership_log::entries::inner::MAGIC),
            "{name}: the inner seal"
        );
        assert!(!holds_token(value), "{name}: no byte of the token");
    }
    assert!(
        !holds_token(&load_records(m22).unwrap().encode()),
        "the records hold no byte of the token"
    );
    let current = log.open_audience_entries(&identity).unwrap().unwrap();
    assert!(
        current.by_audience[&Audience::Everyone][&key().entry()]
            .value
            .as_slice()
            == TOKEN,
        "CONTROL: a current build opens it"
    );
    lab.macs.server.stop();
}

/// An older build's re-`Put` of every `[everyone]` entry under a fresh epoch wrapped to the
/// current members: it carries the inner ciphertext verbatim, so the seal still names the mint's
/// epoch.
fn older_build_reseals(paths: &Paths) {
    let identity = read_identity(paths).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(paths);
    let mut log = log_of(paths);
    OUTER_ONLY.with(|on| on.set(true));
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let generation = opened.max_generation[&EpochAudience::Everyone] + 1;
    let recipients: BTreeSet<Recipient> = log
        .membership()
        .members()
        .values()
        .map(|member| member.binding.recipient())
        .collect();
    let entries: Vec<Entry> = opened.by_audience[&Audience::Everyone]
        .values()
        .cloned()
        .collect();
    let records = bootstrap
        .device()
        .with_seed(|seed| -> crate::Result<Vec<Record>> {
            let authored = aterm_time::unix_millis().unwrap_or(0);
            let (draft, epoch_key) =
                log.draft_epoch(&actor, authored, EpochAudience::Everyone, generation)?;
            let epoch = draft.sign(seed, incarnation)?;
            let id = *epoch.id();
            log.insert(epoch.clone())?;
            let mut records = vec![epoch];
            for recipient in recipients {
                let wrap = log
                    .draft_wrap(&actor, authored, id, &epoch_key, recipient)?
                    .sign(seed, incarnation)?;
                log.insert(wrap.clone())?;
                records.push(wrap);
            }
            for entry in &entries {
                let put = log
                    .draft_put(&actor, authored, id, &epoch_key, entry)?
                    .sign(seed, incarnation)?;
                log.insert(put.clone())?;
                records.push(put);
            }
            Ok(records)
        });
    OUTER_ONLY.with(|on| on.set(false));
    save_records(paths, &encode_records(&records.unwrap())).unwrap();
    post_records_to_servers(paths).expect("posts the reseal");
}

/// A re-`Put` an older build signs under a new epoch carries the inner ciphertext: it reads on
/// every Mac that opens the sealing epoch (m7, m40), and `unreadable here` on m41, admitted
/// afterwards and handed only the new epoch. CONTROL: once a current build seals every entry
/// again under a fresh epoch (m7's member unset of another name), the seal moves forward and m41
/// reads it.
#[test]
fn a_re_put_carrying_the_inner_seal_reads_only_where_its_sealing_epoch_opens() {
    let lab = Lab::new().minted();
    test_put_everyone(&lab.macs.m22, "other", b"o1").unwrap();
    lab.sync_all();
    older_build_reseals(&lab.macs.m22);
    lab.sync_all();
    for paths in [&lab.macs.m7, &lab.m40] {
        assert!(returns_token(paths), "the sealing epoch opens here");
        assert_eq!(standing_of(paths).check, Check::Readable);
    }
    let (m41, _, _) = let_in(&lab.macs, "m41", "m41");
    let unread = read(&m41);
    assert_eq!(unread.standing.check, Check::Unreadable);
    assert_eq!(unread.standing.state(0), State::Unreadable);
    assert!(unread.token().is_none());
    assert_eq!(
        unread.standing.state(0).words(),
        "unreadable here",
        "the words"
    );
    sync(&lab.macs.m7, None).unwrap();
    assert_eq!(
        member_unset(&lab.macs.m7, "other", soon()).unwrap(),
        MemberUnset::Posted
    );
    sync(&m41, None).unwrap();
    assert!(returns_token(&m41), "CONTROL: the seal moved forward");
    lab.macs.server.stop();
}

/// A value with a CR, an LF, a second line or another prefix, put under the class name in one
/// draft with its meta, reads `mismatched (value)` and is never returned; so does a plain value
/// an older admin put with no inner seal. A meta put in another draft reads `mismatched (draft)`.
#[test]
fn a_value_outside_the_grammar_or_the_draft_is_mismatched() {
    let lab = Lab::new();
    let m7 = &lab.macs.m7;
    let json = meta(a_year()).to_json();
    let (entry, meta_entry) = (key().entry(), key().meta_entry());
    for bad in [
        b"sk-ant-oat01-inert\r".as_slice(),
        b"sk-ant-oat01-inert\n",
        b"sk-ant-oat01-inert\nsecond-line",
        b"sk-ant-api03-inert",
    ] {
        test_put_everyone_draft(m7, &[(&entry, bad), (&meta_entry, json.as_bytes())]).unwrap();
        sync(m7, None).unwrap();
        let read = read(m7);
        assert_eq!(read.standing.check, Check::Mismatched(Mismatch::Value));
        assert_eq!(read.standing.state(0).words(), "mismatched (value)");
        assert!(read.token().is_none(), "never returned");
    }
    OUTER_ONLY.with(|on| on.set(true));
    let plain = test_put_everyone_draft(m7, &[(&entry, TOKEN), (&meta_entry, json.as_bytes())]);
    OUTER_ONLY.with(|on| on.set(false));
    plain.unwrap();
    sync(m7, None).unwrap();
    let read_plain = read(m7);
    assert_eq!(
        read_plain.standing.check,
        Check::Mismatched(Mismatch::Value),
        "a plain value under a class name"
    );
    assert!(read_plain.token().is_none());
    // CONTROL: the same pair, sealed and in one draft, reads.
    test_put_everyone_draft(m7, &[(&entry, TOKEN), (&meta_entry, json.as_bytes())]).unwrap();
    sync(m7, None).unwrap();
    assert_eq!(standing_of(m7).check, Check::Readable, "CONTROL");
    // m22 writes the meta again, in a draft of its own.
    let m22 = &lab.macs.m22;
    sync(m22, None).unwrap();
    test_put_everyone_draft(m22, &[(&meta_entry, json.as_bytes())]).unwrap();
    sync(m7, None).unwrap();
    let split = read(m7);
    assert_eq!(split.standing.check, Check::Mismatched(Mismatch::Draft));
    assert_eq!(split.standing.state(0).words(), "mismatched (draft)");
    assert!(split.token().is_none());
    lab.macs.server.stop();
}

/// A new token is `unverified` and served only by its minting seat (m7); after m7 verifies it,
/// in one draft, it is `active` on every Mac, and it turns `soon` within 30 days of its end and
/// `ended` at it. Another Mac's `verify` is refused.
#[test]
fn a_token_is_unverified_until_verified_then_active_soon_and_ended_by_the_clock() {
    let lab = Lab::new().minted();
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let minting = standing_of(m7);
    assert!(minting.minted_here);
    assert_eq!(minting.signer.as_deref(), Some("m7"));
    assert!(matches!(
        minting.state(now),
        State::Unverified {
            served_here: true,
            ..
        }
    ));
    assert!(minting.in_use(now));
    let other = standing_of(m22);
    assert!(!other.minted_here);
    assert_eq!(
        other.signer.as_deref(),
        Some("m7"),
        "the signer, from the log"
    );
    assert!(matches!(
        other.state(now),
        State::Unverified {
            served_here: false,
            ..
        }
    ));
    assert!(!other.in_use(now), "served only by the minting seat");
    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: "windows".into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    assert!(matches!(
        crate::claude_token::verify(m22, &key(), evidence.clone(), refuse()).unwrap(),
        MemberWritten::Refused(_)
    ));
    assert_eq!(
        crate::claude_token::verify(m7, &key(), evidence.clone(), refuse()).unwrap(),
        MemberWritten::Posted
    );
    sync(m22, None).unwrap();
    let verified = standing_of(m22);
    assert_eq!(verified.check, Check::Readable, "verify writes one draft");
    assert_eq!(
        verified.meta.as_ref().and_then(|m| m.verified.clone()),
        Some(evidence)
    );
    assert_eq!(verified.state(now), State::Active);
    assert!(verified.in_use(now));
    assert!(returns_token(m22), "the same token");
    let ends = verified.meta.as_ref().unwrap().expires_at;
    assert_eq!(verified.state(ends - SOON_SECS - 1), State::Active);
    assert_eq!(verified.state(ends - SOON_SECS), State::Soon);
    assert_eq!(verified.state(ends - 1), State::Soon);
    assert_eq!(verified.state(ends), State::Ended);
    assert!(!verified.in_use(ends));
    lab.macs.server.stop();
}

/// After m22 removes m40, which was handed the epoch that sealed the token, the token reads
/// `exposed (removed m40)`, and stays in use.
#[test]
fn a_token_a_removed_mac_was_handed_is_exposed() {
    let lab = Lab::new().minted();
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: "windows".into(),
        seven_day_resets_at: None,
        five_hour_resets_at: None,
    };
    assert_eq!(
        crate::claude_token::verify(m7, &key(), evidence, refuse()).unwrap(),
        MemberWritten::Posted
    );
    sync(m22, None).unwrap();
    assert_eq!(standing_of(m22).state(now), State::Active, "CONTROL");
    test_remove_unposted(m22, "m40").expect("m22 removes m40");
    sync(m22, None).unwrap();
    let exposed = standing_of(m22);
    assert_eq!(exposed.exposed, vec!["m40".to_owned()]);
    assert_eq!(exposed.state(now), State::Exposed(vec!["m40".to_owned()]));
    assert_eq!(exposed.state(now).words(), "exposed (removed m40)");
    assert!(exposed.in_use(now));
    lab.macs.server.stop();
}

/// After m7 removes the token, both names in one draft under a fresh epoch, m41, admitted
/// afterwards and handed every record, opens neither: no name of the class, and no epoch key
/// that sealed the token. NEGATIVE CONTROL: removed by plain `Delete`s under the old epoch
/// (`UNSET_UNDER_OLD_EPOCH`), m41 is handed the key that opens it.
#[test]
fn after_remove_a_later_admit_opens_neither_name() {
    for old_epoch in [false, true] {
        let lab = Lab::new().minted();
        let m7 = &lab.macs.m7;
        let sealing = {
            let log = log_of(m7);
            let opened = log
                .open_audience_entries(&read_identity(m7).unwrap().unwrap())
                .unwrap()
                .unwrap();
            let put = opened.winning_puts[&(Audience::Everyone, key().entry())];
            match log.get(&put).map(Record::body) {
                Some(Body::Put { epoch, .. }) => *epoch,
                _ => panic!("no Put"),
            }
        };
        log_entries::UNSET_UNDER_OLD_EPOCH.with(|on| on.set(old_epoch));
        let removed = crate::claude_token::remove(m7, &key(), soon());
        log_entries::UNSET_UNDER_OLD_EPOCH.with(|on| on.set(false));
        assert_eq!(removed.unwrap(), MemberUnset::Posted);
        sync(m7, None).unwrap();
        let (m41, _, _) = let_in_by(&lab.macs, m7, "m41", "m41");
        let opened = log_of(&m41)
            .open_audience_entries(&read_identity(&m41).unwrap().unwrap())
            .unwrap()
            .unwrap();
        let holds = opened.unwrapped_keys.contains_key(&sealing)
            || opened.keyed_keys.contains_key(&sealing);
        if old_epoch {
            assert!(holds, "NEGATIVE CONTROL: m41 is handed the sealing key");
        } else {
            assert!(!holds, "m41 holds no key that sealed the token");
        }
        assert!(
            !opened
                .by_audience
                .values()
                .flat_map(BTreeMap::keys)
                .any(|name| crate::entry_class::claude_token(name)),
            "m41 opens neither name"
        );
        assert!(matches!(
            crate::claude_token::read(m7, &key(), std::time::Duration::from_secs(60)).unwrap(),
            Lookup::Absent
        ));
        lab.macs.server.stop();
    }
}

/// Whether the Mac at `paths` reads no entry of the account at all.
fn absent(paths: &Paths) -> bool {
    matches!(
        crate::claude_token::read(paths, &key(), std::time::Duration::from_secs(60)).unwrap(),
        Lookup::Absent
    )
}

/// How a Mac back on the LAN gets what it signed off it to a vault server.
#[derive(Clone, Copy, Debug)]
enum Rejoin {
    /// One records pass ([`sync_records`]), which a subscriber's pass and every `sync` of a vault
    /// at a server's address run before their vault pass: the server says it lacks this Mac's
    /// frontier, and the pass posts it.
    RecordsPass,
    /// [`post_records_to_servers`]: the post the harness retries once a minute for a row that
    /// reads `signed here` (`accounts_token_view::note_posted`). The vault pass is no third route:
    /// it posts only where it signed something in that pass, and a write already signed leaves
    /// it nothing to sign.
    Post,
}

/// Off the LAN and back (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §5, slice S6), with
/// `rejoin` carrying the token to the server that returned. Three seated Macs: m7 and m40,
/// members with no admin key, and m22.
///
/// With no vault server answering, m7's token write is signed into its own log (`SignedHere`),
/// and m7 reads the token and serves it at once, as the Mac that minted it; m22 and m40, whose
/// syncs reach no server, read no entry of the account. NEGATIVE CONTROL: with the server back
/// on its address and store, and before m7's `rejoin`, both sync and still read none, so it is
/// m7's pass that delivers the token and nothing else of the lab. After `rejoin` and a sync, both
/// read the same token, signed by m7 and unverified, so neither serves it; after m7's verifying
/// write, which the server takes, and a sync, both serve it.
///
/// With m7's `rejoin` step left out, m40's read after its sync fails.
fn off_the_lan_and_back(rejoin: Rejoin) {
    let Lab { macs, m40 } = Lab::new();
    let TwoMacs {
        root,
        server,
        m22,
        m7,
        ..
    } = macs;
    // m40 first: it reads before m22, the admin, has synced.
    let others = [&m40, &m22];
    assert!(
        matches!(can_administer(&m7), Ok(false)) && !matches!(can_administer(&m40), Ok(true)),
        "CONTROL: neither m7 nor m40 opens an admin locker"
    );
    let now = aterm_time::unix_secs().unwrap();
    let addr = server.addr;
    server.stop();

    let written = crate::claude_token::write(
        &m7,
        &key(),
        &SecretBytes::from(TOKEN),
        &meta(a_year()),
        refuse(),
    )
    .unwrap();
    assert_eq!(
        written,
        MemberWritten::SignedHere,
        "{rejoin:?}: no server took it"
    );
    assert!(
        !crate::claude_token::waiting_here(&m7, &key()).unwrap(),
        "{rejoin:?}: it is signed, not waiting"
    );
    assert!(returns_token(&m7), "{rejoin:?}: m7 reads its token at once");
    let minted = standing_of(&m7);
    assert!(minted.minted_here, "{rejoin:?}");
    assert!(
        matches!(
            minted.state(now),
            State::Unverified {
                served_here: true,
                ..
            }
        ),
        "{rejoin:?}: {:?}",
        minted.state(now)
    );
    assert!(minted.in_use(now), "{rejoin:?}: in use where it was minted");
    for other in others {
        assert!(
            sync(other, None).is_err(),
            "{rejoin:?}: CONTROL: no server answers"
        );
        assert!(absent(other), "{rejoin:?}: absent on every other Mac");
    }

    let server = crate::client::log_entries_tests::server_again(root.path(), addr);
    for other in others {
        sync(other, None).expect("the server answers again");
        assert!(
            absent(other),
            "{rejoin:?}: NEGATIVE CONTROL: the server is back and m7 has not passed"
        );
    }
    match rejoin {
        Rejoin::RecordsPass => {
            sync_records(&m7, None).expect("m7's records pass");
        }
        Rejoin::Post => {
            let took = post_records_to_servers(&m7).expect("m7 posts its records");
            assert_eq!(took, 1, "the one server takes them");
        }
    }
    for other in others {
        sync(other, None).expect("syncs");
        assert!(returns_token(other), "{rejoin:?}: the same token");
        let standing = standing_of(other);
        assert_eq!(standing.check, Check::Readable, "{rejoin:?}");
        assert_eq!(standing.signer.as_deref(), Some("m7"), "{rejoin:?}");
        assert!(!standing.minted_here, "{rejoin:?}");
        assert!(
            !standing.in_use(now),
            "{rejoin:?}: unverified, it is served only where it was minted"
        );
    }

    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: "windows".into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    assert_eq!(
        crate::claude_token::verify(&m7, &key(), evidence, refuse()).unwrap(),
        MemberWritten::Posted,
        "{rejoin:?}: the server takes the verifying write"
    );
    for other in others {
        sync(other, None).expect("syncs");
        let standing = standing_of(other);
        assert_eq!(standing.state(now), State::Active, "{rejoin:?}");
        assert!(standing.in_use(now), "{rejoin:?}: in use on the other Mac");
        assert!(returns_token(other), "{rejoin:?}: the same token");
    }
    assert!(standing_of(&m7).in_use(now), "{rejoin:?}: and still on m7");
    server.stop();
}

/// A token minted while no vault server answers is `SignedHere`, in use on the Mac that minted it
/// and absent on every other; once the server answers again, one records pass of the minting
/// Mac's, or the post a `signed here` row's reader retries, then the verifying write, put it in
/// use on the others ([`off_the_lan_and_back`], which holds the negative control).
#[test]
fn a_token_minted_with_no_server_is_in_use_here_and_on_another_mac_after_rejoin() {
    for rejoin in [Rejoin::RecordsPass, Rejoin::Post] {
        off_the_lan_and_back(rejoin);
    }
}
