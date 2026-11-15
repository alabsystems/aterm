// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed calls of `claude_token.rs` (DESIGN-claude-setup-token S4) on a scratch log vault with
//! three seated members: m22 (the admin), m7 and m40 (members with no admin key). Declared beside
//! the removal tests for their lab. No assertion prints a value: the fixture token appears in no
//! test output.

use super::*;
use crate::claude_token::{
    Check, ExpiryFrom, Key, Lookup, Meta, MintedBy, Mismatch, SOON_SECS, Standing, State, Verified,
};
use crate::membership_log::entries::OUTER_ONLY;

/// The fixture token: inert, in the setup-token grammar.
const TOKEN: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-0123456789").as_bytes();

/// A fixture replacement token, inert too.
const NEXT: &[u8] = concat!("sk-ant-", "oat01-inert_fixture-token-replaced").as_bytes();

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
        minted_by: None,
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

    /// m7, a member with no admin key, writes the fixture token, its meta naming no route (an
    /// earlier build's), and every Mac syncs.
    fn minted(self) -> Self {
        self.minted_by(None)
    }

    /// [`Self::minted`], the meta naming `by` as the route that made the token.
    fn minted_by(self, by: Option<MintedBy>) -> Self {
        assert!(matches!(can_administer(&self.macs.m7), Ok(false)));
        let written = crate::claude_token::write(
            &self.macs.m7,
            &key(),
            &SecretBytes::from(TOKEN),
            &Meta {
                minted_by: by,
                ..meta(a_year())
            },
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

/// Whether the Mac at `paths` has a file, or a row of what its apply records it reads by name,
/// of either of the token's names.
fn materialized(paths: &Paths) -> bool {
    let state = load_state(paths).unwrap();
    [key().entry(), key().meta_entry()].iter().any(|name| {
        paths.home.join(".secrets").join(name).exists()
            || state
                .held
                .as_ref()
                .is_some_and(|held| held.contains_key(name))
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

/// A NEW TOKEN IS `unverified` UNTIL VERIFIED, THEN `active`, `soon` AND `ended` BY THE CLOCK,
/// AND ITS ROUTE SAYS WHERE IT IS IN USE MEANWHILE: m7 made it in aterm's window, so it is in use
/// on every Mac from mint, m7 and m22 alike ([`Standing::serves_here`]). m7's `--replace` made at
/// a terminal is in use on m7 alone until m7 verifies it, in one draft that keeps its route; then
/// it is `active` on every Mac, and it turns `soon` within 30 days of its end and `ended` at it.
/// Another Mac's `verify` is refused. NEGATIVE CONTROL: a route that serves nothing beyond its
/// minting Mac (`MintedBy::serves_unverified` keyed on `minted_here` alone) fails `in use on every
/// Mac from mint`.
#[test]
fn a_token_is_unverified_until_verified_then_active_soon_and_ended_by_the_clock() {
    let lab = Lab::new().minted_by(Some(MintedBy::Window));
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let minting = standing_of(m7);
    assert!(minting.minted_here);
    assert_eq!(minting.signer.as_deref(), Some("m7"));
    assert_eq!(
        minting.state(now),
        State::Unverified {
            minted_here: true,
            minted_by: Some(MintedBy::Window),
            exposed: Vec::new(),
        }
    );
    let other = standing_of(m22);
    assert!(!other.minted_here);
    assert_eq!(
        other.signer.as_deref(),
        Some("m7"),
        "the signer, from the log"
    );
    assert_eq!(
        other.state(now).words(),
        "unverified (made in aterm's window on another Mac; in use on every Mac)"
    );
    assert!(
        minting.serves_here() && other.serves_here(),
        "in use on every Mac from mint"
    );
    assert!(
        minting.serves_elsewhere() && other.serves_elsewhere(),
        "so replacing it on either Mac replaces another Mac's token"
    );

    let made = meta(0).minted_at + 600;
    let replaced = crate::claude_token::write(
        m7,
        &key(),
        &SecretBytes::from(NEXT),
        &Meta {
            minted_at: made,
            minted_by: Some(MintedBy::Terminal),
            ..meta(a_year())
        },
        refuse(),
    )
    .unwrap();
    assert_eq!(replaced, MemberWritten::Posted);
    lab.sync_all();
    let (minting, other) = (standing_of(m7), standing_of(m22));
    assert!(minting.serves_here(), "a terminal's token serves m7");
    assert!(!other.serves_here(), "and no other Mac until verified");
    assert!(
        !minting.serves_elsewhere(),
        "m7 alone uses it: replacing it there replaces no other Mac's token"
    );
    assert!(
        other.serves_elsewhere(),
        "CONTROL: on m22 it is m7's token, in use there"
    );
    assert_eq!(
        other.state(now).words(),
        "unverified (made at a terminal on another Mac; not in use here until verified)"
    );
    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    assert!(matches!(
        crate::claude_token::verify(m22, &key(), made, evidence.clone(), refuse()).unwrap(),
        MemberWritten::Refused(_)
    ));
    assert_eq!(
        crate::claude_token::verify(m7, &key(), made, evidence.clone(), refuse()).unwrap(),
        MemberWritten::Posted
    );
    sync(m22, None).unwrap();
    let verified = standing_of(m22);
    assert_eq!(verified.check, Check::Readable, "verify writes one draft");
    let stamped = verified.meta.as_ref().expect("a meta");
    assert_eq!(stamped.verified, Some(evidence));
    assert_eq!(
        stamped.minted_by,
        Some(MintedBy::Terminal),
        "the route is kept"
    );
    assert!(verified.serves_here());
    assert_eq!(verified.state(now), State::Active);
    assert!(
        read(m22)
            .token()
            .is_some_and(|token| token.as_slice() == NEXT),
        "the same token"
    );
    let ends = stamped.expires_at;
    assert_eq!(verified.state(ends - SOON_SECS - 1), State::Active);
    assert_eq!(verified.state(ends - SOON_SECS), State::Soon);
    assert_eq!(verified.state(ends - 1), State::Soon);
    assert_eq!(verified.state(ends), State::Ended);
    lab.macs.server.stop();
}

/// AN EARLIER BUILD'S STAMP FROM A USAGE SAMPLE VERIFIES NOTHING, ON ANY MAC: 0.197.0 to 0.200.0
/// stamped `verified` by `windows` from the row's usage sample, which can hold another account's
/// readings. On a token whose route serves only its minting Mac before verification (a terminal's,
/// or a meta that names no route), that stamp leaves it `unverified` on every Mac: m22, which did
/// not mint it, starts no session on it, and m7, which did, reads it unverified, so its harness
/// compares it again ([`Standing::verified`] is what `note_served` and `note_basis` skip on). m7's
/// own stamp by [`crate::claude_token::BY_WINDOWS`] replaces it, and then every Mac serves the
/// token. NEGATIVE CONTROL: with [`Meta::verification`] counting any stamp, m22 reads the sample's
/// token `active` and serves it.
#[test]
fn an_earlier_builds_sample_stamp_verifies_nothing_on_any_mac() {
    for by in [None, Some(MintedBy::Terminal)] {
        let lab = Lab::new().minted_by(by);
        let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
        let now = aterm_time::unix_secs().unwrap();
        let sample = Verified {
            by: "windows".into(),
            ..verified_at(now)
        };
        assert_eq!(
            crate::claude_token::verify(m7, &key(), meta(0).minted_at, sample.clone(), refuse())
                .unwrap(),
            MemberWritten::Posted,
            "{by:?}: m7 writes the stamp an earlier build wrote"
        );
        lab.sync_all();
        let (minting, other) = (standing_of(m7), standing_of(m22));
        assert_eq!(
            other.meta.as_ref().and_then(|meta| meta.verified.clone()),
            Some(sample),
            "{by:?}: the vault holds the sample's stamp"
        );
        assert!(
            !other.verified() && !other.serves_here(),
            "{by:?}: m22 starts no session on it"
        );
        assert_eq!(
            other.state(now),
            State::Unverified {
                minted_here: false,
                minted_by: by,
                exposed: Vec::new(),
            },
            "{by:?}"
        );
        assert!(
            !minting.verified() && minting.serves_here(),
            "{by:?}: m7 serves it unverified, so its harness compares it again"
        );
        assert_eq!(
            crate::claude_token::verify(m7, &key(), meta(0).minted_at, verified_at(now), refuse())
                .unwrap(),
            MemberWritten::Posted,
            "{by:?}: m7's own comparison"
        );
        lab.sync_all();
        let other = standing_of(m22);
        assert!(other.verified() && other.serves_here(), "{by:?}");
        assert_eq!(other.state(now), State::Active, "{by:?}");
        lab.macs.server.stop();
    }
}

/// A META NAMES THE ROUTE THAT MADE ITS TOKEN, AND ONE THAT NAMES NONE STILL PARSES (§4.1): this
/// build writes `minted_by` and reads it back; a meta with no such field (an earlier build's, or
/// one an earlier build wrote again, which drops what it does not know), a null one, or a word
/// this build does not know parses with `minted_by` `None`, so its token serves its minting Mac
/// alone until verified, as it did before the field.
#[test]
fn a_meta_names_the_route_that_made_its_token_and_one_naming_none_still_parses() {
    for by in [MintedBy::Window, MintedBy::Headless, MintedBy::Terminal] {
        let written = Meta {
            minted_by: Some(by),
            ..meta(a_year())
        };
        assert_eq!(
            Meta::from_json(written.to_json().as_bytes()),
            Some(written.clone()),
            "{by:?}"
        );
        assert!(
            written
                .to_json()
                .contains(&format!(r#""minted_by":"{}""#, by.word())),
            "{by:?}"
        );
    }
    let written = Meta {
        minted_by: Some(MintedBy::Window),
        ..meta(a_year())
    };
    let mut value: aterm_json::Value = aterm_json::from_str(&written.to_json()).unwrap();
    let older = |value: &aterm_json::Value| {
        Meta::from_json(aterm_json::to_string(value).unwrap().as_bytes()).expect("it parses")
    };
    value.as_object_mut().unwrap().remove("minted_by");
    assert_eq!(older(&value).minted_by, None, "an earlier build's meta");
    value
        .as_object_mut()
        .unwrap()
        .insert("minted_by".into(), aterm_json::Value::Null);
    assert_eq!(older(&value).minted_by, None, "null");
    value.as_object_mut().unwrap().insert(
        "minted_by".into(),
        aterm_json::Value::from("carrier-pigeon"),
    );
    assert_eq!(
        older(&value).minted_by,
        None,
        "a word this build does not know"
    );
    assert!(MintedBy::serves_unverified(None, true));
    assert!(!MintedBy::serves_unverified(None, false));
    assert!(!MintedBy::serves_unverified(
        Some(MintedBy::Terminal),
        false
    ));
    assert!(MintedBy::serves_unverified(Some(MintedBy::Headless), false));
}

/// A VERIFICATION STAMPS ONLY THE TOKEN IT WAS READ ON: m7's evidence names the token it compared
/// by its `minted_at`; once m7 has replaced that token (another `minted_at`), the same `verify`
/// is refused and writes nothing, so the vault's token stays unverified on every Mac. CONTROL:
/// naming the token the vault holds now, it is written.
#[test]
fn a_verify_of_a_token_the_vault_no_longer_holds_writes_nothing() {
    let lab = Lab::new().minted();
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let compared = meta(0).minted_at;
    let replacement = Meta {
        minted_at: compared + 600,
        ..meta(a_year())
    };
    assert_eq!(
        crate::claude_token::write(
            m7,
            &key(),
            &SecretBytes::from(TOKEN),
            &replacement,
            refuse()
        )
        .unwrap(),
        MemberWritten::Posted
    );
    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    let refused = crate::claude_token::verify(m7, &key(), compared, evidence.clone(), refuse())
        .expect("a verify");
    assert!(
        matches!(&refused, MemberWritten::Refused(why) if why.contains("another replaced it")),
        "{refused:?}"
    );
    sync(m22, None).unwrap();
    for paths in [m7, m22] {
        let standing = standing_of(paths);
        assert_eq!(
            standing.meta.as_ref().map(|m| m.minted_at),
            Some(replacement.minted_at)
        );
        assert!(!standing.verified(), "nothing stamped the replacement");
    }
    assert_eq!(
        crate::claude_token::verify(
            m7,
            &key(),
            replacement.minted_at,
            evidence.clone(),
            refuse()
        )
        .unwrap(),
        MemberWritten::Posted,
        "CONTROL: the token the vault holds"
    );
    sync(m22, None).unwrap();
    assert_eq!(standing_of(m22).state(now), State::Active);
    lab.macs.server.stop();
}

/// A VERIFICATION NEVER WRITES OVER A REPLACEMENT THAT COMMITTED AFTER ITS READ: m7 minted the
/// token, and m7's `verify` reads it, still the vault's token, made at the `minted_at` it names;
/// before its draft (`BEFORE_DRAFT`) m7's `--replace` commits another token, as a vault pass
/// applying another Mac's would. The write stands on the pair it read
/// ([`crate::client::MemberEntry::read_as`]), which the log no longer holds: it is refused and
/// writes nothing, and the vault holds the replacement, its token and its meta, unverified, on m7
/// and on m22. NEGATIVE CONTROL (by hand, then restored): with `verify` standing on nothing
/// (`write_pair`'s `read` `None`), it signs the compared token's pair over the replacement,
/// putting that token back, stamped.
#[test]
fn a_verify_never_writes_over_a_replacement_committed_after_its_read() {
    let lab = Lab::new().minted();
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let compared = meta(0).minted_at;
    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    let replacement = Meta {
        minted_at: compared + 600,
        ..meta(a_year())
    };
    let (writer, written) = (m7.clone(), replacement.clone());
    log_entries::BEFORE_DRAFT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let replaced = crate::claude_token::write(
                &writer,
                &key(),
                &SecretBytes::from(NEXT),
                &written,
                refuse(),
            )
            .unwrap();
            assert_eq!(replaced, MemberWritten::Posted, "the replacement commits");
        }));
    });
    let restamped = crate::claude_token::verify(m7, &key(), compared, evidence, refuse()).unwrap();
    let ran = log_entries::BEFORE_DRAFT.with(|slot| slot.borrow_mut().take().is_none());
    assert!(
        ran,
        "the replacement committed between the read and the draft"
    );
    assert!(
        matches!(&restamped, MemberWritten::Refused(why)
            if why.contains("changed since this write read it")),
        "{restamped:?}"
    );
    sync(m22, None).unwrap();
    for paths in [m7, m22] {
        let read = read(paths);
        assert_eq!(read.standing.check, Check::Readable, "one draft");
        assert_eq!(
            read.standing.meta.as_ref(),
            Some(&replacement),
            "the replacement stands"
        );
        assert!(
            read.token().is_some_and(|token| token.as_slice() == NEXT),
            "the vault holds the replacement's token"
        );
    }
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
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: None,
        five_hour_resets_at: None,
    };
    assert_eq!(
        crate::claude_token::verify(m7, &key(), meta(0).minted_at, evidence, refuse()).unwrap(),
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
/// With no vault server answering, m7's token write, made in aterm's window, is signed into its
/// own log (`SignedHere`), and m7 reads the token and serves it at once; m22 and m40, whose syncs
/// reach no server, read no entry of the account. NEGATIVE CONTROL: with the server back on its
/// address and store, and before m7's `rejoin`, both sync and still read none, so it is m7's pass
/// that delivers the token and nothing else of the lab. After `rejoin` and a sync, both read the
/// same token, signed by m7 and unverified, and both serve it: its mint proved the account. After
/// m7's verifying write, which the server takes, and a sync, both read it `active`.
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
        &Meta {
            minted_by: Some(MintedBy::Window),
            ..meta(a_year())
        },
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
                minted_here: true,
                ..
            }
        ) && minted.serves_here(),
        "{rejoin:?}: {:?}",
        minted.state(now)
    );
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
            matches!(
                standing.state(now),
                State::Unverified {
                    minted_here: false,
                    minted_by: Some(MintedBy::Window),
                    ..
                }
            ) && standing.serves_here(),
            "{rejoin:?}: unverified, and in use here from mint"
        );
    }

    let evidence = Verified {
        on: "m7".into(),
        at: now,
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    };
    assert_eq!(
        crate::claude_token::verify(&m7, &key(), meta(0).minted_at, evidence, refuse()).unwrap(),
        MemberWritten::Posted,
        "{rejoin:?}: the server takes the verifying write"
    );
    for other in others {
        sync(other, None).expect("syncs");
        let standing = standing_of(other);
        assert_eq!(
            standing.state(now),
            State::Active,
            "{rejoin:?}: in use on the other Mac"
        );
        assert!(returns_token(other), "{rejoin:?}: the same token");
    }
    assert_eq!(
        standing_of(&m7).state(now),
        State::Active,
        "{rejoin:?}: and still on m7"
    );
    server.stop();
}

/// A token minted in aterm's window while no vault server answers is `SignedHere`, in use on the
/// Mac that minted it and absent on every other; once the server answers again, one records pass
/// of the minting Mac's, or the post a `signed here` row's reader retries, puts it in use on the
/// others, and the verifying write then labels it `active` there ([`off_the_lan_and_back`], which
/// holds the negative control).
#[test]
fn a_token_minted_with_no_server_is_in_use_here_and_on_another_mac_after_rejoin() {
    for rejoin in [Rejoin::RecordsPass, Rejoin::Post] {
        off_the_lan_and_back(rejoin);
    }
}

/// How a replacement token's meta compares with the meta of the token it replaces.
#[derive(Clone, Copy, Debug)]
enum Replacing {
    /// The same record, byte for byte (a replacement minted within the second of the token it
    /// replaces, its end assumed from the same mint time): the log already holds the meta as the
    /// replacement leaves it.
    SameMeta,
    /// A record of its own: a later `minted_at`.
    NewMeta,
}

/// Where the vault pass that signs a waiting replacement finds a vault server.
#[derive(Clone, Copy, Debug)]
enum Signing {
    /// The server answers: the minting Mac's sync runs its records pass, then its vault pass,
    /// which signs the waiting write and posts it.
    Online,
    /// No server answers: the minting Mac's vault pass signs the waiting write into its own log,
    /// and `Rejoin` carries it to the server once that answers again.
    Offline(Rejoin),
}

/// A `--replace` of a token that waits for the next vault pass: m7 minted the token, every Mac
/// synced, and m7 writes a replacement while a vault pass holds its claim (`OnBusy::Wait`, as
/// `store_patiently` writes once its patience ends), so both entries wait in `everyone.waiting`
/// (`Waiting(LockDeadline)`). The next vault pass, by `signing`, signs the pair over the account's
/// existing token; m7, which minted the replacement, then reads it as one draft, signed by m7 and
/// minted here, with its meta; m22 and m40 read the same after a sync, signed by m7 and not minted
/// there.
fn replaced_while_waiting(replacing: Replacing, signing: Signing) {
    let Lab { macs, m40 } = Lab::new().minted();
    let TwoMacs {
        root,
        server,
        m22,
        m7,
        ..
    } = macs;
    let case = format!("{replacing:?}, {signing:?}");
    let now = aterm_time::unix_secs().unwrap();
    let first = standing_of(&m7);
    assert_eq!(first.check, Check::Readable, "{case}: CONTROL");
    assert!(
        first.minted_here,
        "{case}: CONTROL: m7 minted the first token"
    );
    let earlier = first.meta.expect("the first token's meta");
    let meta = match replacing {
        Replacing::SameMeta => earlier.clone(),
        Replacing::NewMeta => Meta {
            minted_at: earlier.minted_at + 1,
            ..earlier.clone()
        },
    };
    let next = NEXT;
    let addr = server.addr;
    let online = match signing {
        Signing::Online => Some(server),
        Signing::Offline(_) => {
            server.stop();
            None
        }
    };
    let claim = log_entries::pass_claim(&m7)
        .unwrap()
        .expect("a pass's claim");
    let written = crate::claude_token::write(
        &m7,
        &key(),
        &SecretBytes::from(next),
        &meta,
        OnBusy::Wait(std::time::Instant::now()),
    )
    .unwrap();
    drop(claim);
    assert_eq!(
        written,
        MemberWritten::Waiting(WaitFor::LockDeadline),
        "{case}: the replacement waits for the next vault pass"
    );
    assert!(
        crate::claude_token::waiting_here(&m7, &key()).unwrap(),
        "{case}: it waits"
    );
    let server = match signing {
        Signing::Online => {
            sync(&m7, None).expect("m7's records pass and vault pass");
            online.expect("the server")
        }
        Signing::Offline(rejoin) => {
            let signed = vault_pass_at(&m7, aterm_time::unix_millis().unwrap()).unwrap();
            assert!(
                !signed.is_empty(),
                "{case}: the pass signs the waiting write"
            );
            let server = crate::client::log_entries_tests::server_again(root.path(), addr);
            match rejoin {
                Rejoin::RecordsPass => {
                    sync_records(&m7, None).expect("m7's records pass");
                }
                Rejoin::Post => {
                    let took = post_records_to_servers(&m7).expect("m7 posts its records");
                    assert_eq!(took, 1, "{case}: the one server takes them");
                }
            }
            server
        }
    };
    assert!(
        !crate::claude_token::waiting_here(&m7, &key()).unwrap(),
        "{case}: nothing waits once the pass signed it"
    );
    let here = read(&m7);
    assert_eq!(
        here.standing.check,
        Check::Readable,
        "{case}: the minting Mac reads its replacement as one draft"
    );
    assert!(
        here.token().is_some_and(|token| token.as_slice() == next),
        "{case}: the replacement"
    );
    assert_eq!(here.standing.meta.as_ref(), Some(&meta), "{case}: its meta");
    assert_eq!(here.standing.signer.as_deref(), Some("m7"), "{case}");
    assert!(here.standing.minted_here, "{case}: minted here");
    assert!(
        matches!(
            here.standing.state(now),
            State::Unverified {
                minted_here: true,
                ..
            }
        ),
        "{case}: {:?}",
        here.standing.state(now)
    );
    for other in [&m22, &m40] {
        sync(other, None).expect("syncs");
        let there = read(other);
        assert_eq!(there.standing.check, Check::Readable, "{case}");
        assert!(
            there.token().is_some_and(|token| token.as_slice() == next),
            "{case}: the replacement"
        );
        assert_eq!(
            there.standing.meta.as_ref(),
            Some(&meta),
            "{case}: its meta"
        );
        assert_eq!(there.standing.signer.as_deref(), Some("m7"), "{case}");
        assert!(!there.standing.minted_here, "{case}");
    }
    server.stop();
}

/// A replacement token that waits for the next vault pass reads, once that pass signs it, as one
/// draft on the Mac that minted it and on every other, whatever server the pass finds and whether
/// or not the log already holds its meta ([`replaced_while_waiting`]).
///
/// NEGATIVE CONTROL: with the pass judging each waiting write alone (`draft_verdicts` cut from
/// `sign_waiting_writes_by`), and again with each entry landed as a draft of its own (by hand,
/// then restored), `SameMeta` fails on the minting Mac with `Mismatched(Draft)`: the pass signs
/// the token and leaves the meta, which the log holds already. `NewMeta` passes either way.
#[test]
fn a_replacement_that_waited_for_the_vault_pass_reads_as_one_draft() {
    for replacing in [Replacing::SameMeta, Replacing::NewMeta] {
        for signing in [
            Signing::Online,
            Signing::Offline(Rejoin::RecordsPass),
            Signing::Offline(Rejoin::Post),
        ] {
            replaced_while_waiting(replacing, signing);
        }
    }
}

/// m22 removes m40, keeping the records to itself, and m7 is handed the `Remove` alone: the
/// removal's rotation is due on m7 (one-mesh M4), so m7's member writes wait for its next vault
/// pass. Returns m22's records since, the `Remove` first, which hand m7 the new key.
fn rotation_due_on_m7(lab: &Lab) -> Vec<Record> {
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let base = convergence::ids(m22);
    test_remove_unposted(m22, "m40").expect("m22 removes m40");
    let chain = convergence::signed_since(m22, &base);
    save_records(m7, &encode_records(&chain[..1])).unwrap();
    assert!(
        log_of(m7)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "CONTROL: m7 holds the Remove, not its rotation"
    );
    chain
}

/// A verification m7 wrote at `now` (unix seconds).
fn verified_at(now: u64) -> Verified {
    Verified {
        on: "m7".into(),
        at: now,
        by: crate::claude_token::BY_WINDOWS.into(),
        seven_day_resets_at: Some(now + 86_400),
        five_hour_resets_at: None,
    }
}

/// A verification that waits for a removal's rotation: m22 removes m40, and m7, which holds the
/// `Remove` and not yet its new key, verifies its token on a served reading
/// (`Waiting(RotationDue)`, as `accounts_tokens::verify_served` counts written). Once the rotation
/// reaches m7, its vault pass signs the verified meta and, beside it, the token the log already
/// holds, so the pair stays one draft. m7 reads the same token signed by m7, minted here and
/// verified; m22 reads it signed by m7 and not minted there. On both it reads `exposed (removed
/// m40)`, since m40 was handed the epoch that sealed it, and it is in use.
///
/// NEGATIVE CONTROL (by hand, then restored): with the pass judging each waiting write alone
/// (`draft_verdicts` cut from `sign_waiting_writes_by`), m7 reads `Mismatched(Draft)`.
#[test]
fn a_verification_that_waited_for_a_rotation_reads_as_one_draft() {
    let lab = Lab::new().minted();
    let chain = rotation_due_on_m7(&lab);
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    assert_eq!(
        crate::claude_token::verify(m7, &key(), meta(0).minted_at, verified_at(now), refuse())
            .unwrap(),
        MemberWritten::Waiting(WaitFor::RotationDue),
        "the verification waits for the rotation"
    );
    save_records(m7, &encode_records(&chain)).unwrap();
    sync(m7, None).expect("m7's records pass and vault pass");
    assert!(
        !crate::claude_token::waiting_here(m7, &key()).unwrap(),
        "nothing waits once the pass signed it"
    );
    let exposed = State::Exposed(vec!["m40".to_owned()]);
    let here = read(m7);
    assert_eq!(
        here.standing.check,
        Check::Readable,
        "the verified token reads as one draft"
    );
    assert!(returns_token(m7), "the same token");
    assert_eq!(
        here.standing.meta.as_ref().and_then(|m| m.verified.clone()),
        Some(verified_at(now)),
        "its verification"
    );
    assert_eq!(here.standing.signer.as_deref(), Some("m7"));
    assert!(here.standing.minted_here, "minted here");
    assert_eq!(here.standing.state(now), exposed);
    sync(m22, None).expect("m22 syncs");
    let there = standing_of(m22);
    assert_eq!(there.check, Check::Readable);
    assert!(returns_token(m22), "the same token");
    assert_eq!(there.signer.as_deref(), Some("m7"));
    assert!(!there.minted_here);
    assert!(there.verified(), "verified");
    assert_eq!(there.state(now), exposed, "in use on the other Mac");
    lab.macs.server.stop();
}

/// A verification never replaces a replacement that waits. m7 minted the token; with a removal's
/// rotation due on m7, its `--replace` waits (`Waiting(RotationDue)`), so sessions on m7 still
/// start on the first token, whose served reading verifies it. That verification is of the first
/// token, which the replacement is newer than: it is refused, and the replacement still waits.
/// Once the rotation reaches m7, its vault pass signs the replacement, and m7 reads it with its
/// own meta, unverified, signed by m7 and minted here; m22 reads the same, not minted there; and
/// nothing is journalled overtaken.
///
/// NEGATIVE CONTROL (by hand, then restored): with `land_member_writes` landing an entry that
/// stands on the value it read over a write of its name that leaves another value, the
/// verification answers `Waiting(PendingOfThisMac)`; with that assertion relaxed too, m7 reads
/// after the pass a token that is not the replacement, and reads it as one draft (`Readable`).
#[test]
fn a_verification_never_replaces_a_replacement_that_waits() {
    let lab = Lab::new().minted();
    let chain = rotation_due_on_m7(&lab);
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let first = standing_of(m7).meta.expect("the first token's meta");
    let compared = first.minted_at;
    let meta = Meta {
        minted_at: first.minted_at + 1,
        ..first
    };
    assert_eq!(
        crate::claude_token::write(m7, &key(), &SecretBytes::from(NEXT), &meta, refuse()).unwrap(),
        MemberWritten::Waiting(WaitFor::RotationDue),
        "the replacement waits for the rotation"
    );
    assert!(returns_token(m7), "CONTROL: m7 reads the first token yet");
    let verified =
        crate::claude_token::verify(m7, &key(), compared, verified_at(now), refuse()).unwrap();
    let MemberWritten::Refused(why) = &verified else {
        panic!("the verification of the first token: {verified:?}");
    };
    assert!(why.contains("changed since this write read it"), "{why}");
    assert!(
        crate::claude_token::waiting_here(m7, &key()).unwrap(),
        "the replacement still waits"
    );
    save_records(m7, &encode_records(&chain)).unwrap();
    sync(m7, None).expect("m7's records pass and vault pass");
    assert!(
        !crate::claude_token::waiting_here(m7, &key()).unwrap(),
        "nothing waits once the pass signed it"
    );
    let here = read(m7);
    assert_eq!(here.standing.check, Check::Readable);
    assert!(
        here.token().is_some_and(|token| token.as_slice() == NEXT),
        "the replacement stands"
    );
    assert_eq!(
        here.standing.meta.as_ref(),
        Some(&meta),
        "with its own meta"
    );
    assert_eq!(here.standing.signer.as_deref(), Some("m7"));
    assert!(here.standing.minted_here, "minted here");
    assert!(
        matches!(
            here.standing.state(now),
            State::Unverified {
                minted_here: true,
                ..
            }
        ),
        "{:?}",
        here.standing.state(now)
    );
    assert!(
        log_entries::read_overtaken(m7).unwrap().is_empty(),
        "nothing journalled overtaken"
    );
    sync(m22, None).expect("m22 syncs");
    let there = read(m22);
    assert_eq!(there.standing.check, Check::Readable);
    assert!(
        there.token().is_some_and(|token| token.as_slice() == NEXT),
        "the replacement"
    );
    assert_eq!(there.standing.meta.as_ref(), Some(&meta));
    assert_eq!(there.standing.signer.as_deref(), Some("m7"));
    assert!(!there.standing.minted_here);
    lab.macs.server.stop();
}

/// A removal takes back both entries of a replacement that waits, or neither. m7's `--replace`
/// waits for the vault pass (`Waiting(LockDeadline)`), and m7 removes the token while another
/// writer of m7's state takes the lock after the removal's first take of it
/// (`WAITING_LOCK_TAKES`). The take-back takes back both names under that one take, so no half of
/// the replacement's draft is left for the next pass to sign alone; the removal is posted, and
/// once m7's pass ran, no Mac reads an entry of the account.
///
/// NEGATIVE CONTROL (by hand, then restored): with the names taken back one at a time, a lock
/// take each, the second take fails, and the meta's write alone still waits ("both or neither").
#[test]
fn a_removal_takes_back_both_entries_of_a_waiting_replacement_or_neither() {
    let lab = Lab::new().minted();
    let m7 = &lab.macs.m7;
    let first = standing_of(m7).meta.expect("the first token's meta");
    let meta = Meta {
        minted_at: first.minted_at + 1,
        ..first
    };
    let claim = log_entries::pass_claim(m7)
        .unwrap()
        .expect("a pass's claim");
    let written = crate::claude_token::write(
        m7,
        &key(),
        &SecretBytes::from(NEXT),
        &meta,
        OnBusy::Wait(std::time::Instant::now()),
    )
    .unwrap();
    drop(claim);
    assert_eq!(written, MemberWritten::Waiting(WaitFor::LockDeadline));
    log_entries::WAITING_LOCK_TAKES.with(|takes| takes.set(Some(1)));
    let removed = crate::claude_token::remove(m7, &key(), soon());
    log_entries::WAITING_LOCK_TAKES.with(|takes| takes.set(None));
    let waits = [key().entry(), key().meta_entry()]
        .map(|name| crate::client::member_waits_here(m7, &name).unwrap());
    assert_eq!(
        waits[0], waits[1],
        "both entries of the replacement wait, or neither: {waits:?}"
    );
    assert_eq!(removed.unwrap(), MemberUnset::Posted);
    sync(m7, None).expect("m7's records pass and vault pass");
    for paths in lab.all() {
        sync(paths, None).expect("syncs");
        assert!(absent(paths), "no entry of the account");
    }
    lab.macs.server.stop();
}

/// How a `--replace` comes to wait for the vault pass.
#[derive(Clone, Copy, Debug)]
enum WaitsBy {
    /// A vault pass holds m7's draft claim past the write's patience (`OnBusy::Wait`, as
    /// `store_patiently` writes once its patience ends): no draft runs, and the write reads the
    /// log itself before it lands what waits (`Waiting(LockDeadline)`).
    Busy,
    /// A removal's rotation is due on m7, so the draft waits (`Waiting(RotationDue)`), and the
    /// rotation reaches m7 before the write lands.
    Rotation,
}

/// A verification of the token a `--replace` replaces, tried between the replacement's read of
/// the log and its landing (`BEFORE_LAND`, by `waits`): sessions on m7 still start on the first
/// token, whose served reading verifies it. It cannot come between them: it waits for the author
/// lock the replacement holds from its read to its landing, until its own patience ends (`Busy`),
/// and signs nothing. The replacement lands, the next pass signs it, and m7 reads it with its own
/// meta, unverified, signed by m7 and minted here; m22 reads the same; nothing is journalled
/// overtaken.
fn verified_while_a_replacement_lands(waits: WaitsBy) {
    let lab = Lab::new().minted();
    let chain = match waits {
        WaitsBy::Busy => Vec::new(),
        WaitsBy::Rotation => rotation_due_on_m7(&lab),
    };
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let now = aterm_time::unix_secs().unwrap();
    let first = standing_of(m7).meta.expect("the first token's meta");
    let compared = first.minted_at;
    let meta = Meta {
        minted_at: first.minted_at + 1,
        ..first
    };
    let claim = match waits {
        WaitsBy::Busy => Some(
            log_entries::pass_claim(m7)
                .unwrap()
                .expect("a pass's claim"),
        ),
        WaitsBy::Rotation => None,
    };
    let verified = std::rc::Rc::new(std::cell::RefCell::new(None));
    let hook = {
        let (m7, verified) = (m7.clone(), verified.clone());
        move || {
            drop(claim);
            if !chain.is_empty() {
                save_records(&m7, &encode_records(&chain)).unwrap();
            }
            // The served reading's patience: what it waits for is held by this very thread.
            let by = std::time::Instant::now() + std::time::Duration::from_millis(200);
            let written = crate::claude_token::verify(
                &m7,
                &key(),
                compared,
                verified_at(now),
                OnBusy::Refuse(by),
            );
            *verified.borrow_mut() = Some(written.unwrap());
        }
    };
    log_entries::BEFORE_LAND.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let (on_busy, want) = match waits {
        WaitsBy::Busy => (
            OnBusy::Wait(std::time::Instant::now()),
            WaitFor::LockDeadline,
        ),
        WaitsBy::Rotation => (refuse(), WaitFor::RotationDue),
    };
    let written = crate::claude_token::write(m7, &key(), &SecretBytes::from(NEXT), &meta, on_busy);
    log_entries::BEFORE_LAND.with(|slot| *slot.borrow_mut() = None);
    assert_eq!(
        written.unwrap(),
        MemberWritten::Waiting(want),
        "{waits:?}: the replacement waits"
    );
    sync(m7, None).expect("m7's records pass and vault pass");
    let here = read(m7);
    assert!(
        here.token().is_some_and(|token| token.as_slice() == NEXT),
        "{waits:?}: the replacement stands"
    );
    assert_eq!(here.standing.check, Check::Readable, "{waits:?}");
    assert_eq!(
        here.standing.meta.as_ref(),
        Some(&meta),
        "{waits:?}: with its own meta"
    );
    assert_eq!(here.standing.signer.as_deref(), Some("m7"), "{waits:?}");
    assert!(here.standing.minted_here, "{waits:?}: minted here");
    assert!(
        log_entries::read_overtaken(m7).unwrap().is_empty(),
        "{waits:?}: nothing journalled overtaken"
    );
    assert_eq!(
        verified.borrow().clone(),
        Some(MemberWritten::Busy),
        "{waits:?}: the verification signed nothing"
    );
    sync(m22, None).expect("m22 syncs");
    let there = read(m22);
    assert!(
        there.token().is_some_and(|token| token.as_slice() == NEXT),
        "{waits:?}: the replacement"
    );
    assert_eq!(there.standing.meta.as_ref(), Some(&meta), "{waits:?}");
    lab.macs.server.stop();
}

/// A verification never comes between a `--replace`'s read of the log and its landing, whichever
/// way the replacement waits ([`verified_while_a_replacement_lands`]).
///
/// NEGATIVE CONTROL (by hand, then restored): with `member_write` taking no author lock where a
/// busy vault left no draft, `Busy` fails: the verification signs the first token and its
/// verified meta between the read and the landing, and the pass drops the whole replacement as
/// overtaken, so m7 reads the first token. With the author lock let go before the landing,
/// `Rotation` fails the same way.
#[test]
fn a_verification_never_comes_between_a_replacements_read_and_its_landing() {
    for waits in [WaitsBy::Busy, WaitsBy::Rotation] {
        verified_while_a_replacement_lands(waits);
    }
}

/// A verification never replaces a waiting replacement that keeps the token and changes only its
/// meta. m7's `--replace` writes the same token with a later `minted_at` and end while a vault
/// pass holds its claim, so both entries wait (`Waiting(LockDeadline)`). A served reading then
/// verifies the token as m7's log holds it, with the first meta: its token entry is as the
/// replacement leaves it, but its meta is not, so the verification is refused and the
/// replacement waits as it was. The next pass signs it, and m7 reads the token with the
/// replacement's meta, unverified; nothing is journalled overtaken.
///
/// NEGATIVE CONTROL (by hand, then restored): with `verify` standing on the token it read alone
/// (the meta's `read_as` `None`), the verification answers `Waiting(PendingOfThisMac)`; with that
/// assertion relaxed too, m7 reads after the pass the first meta, verified.
#[test]
fn a_verification_never_replaces_a_waiting_replacement_that_keeps_the_token() {
    let lab = Lab::new().minted();
    let m7 = &lab.macs.m7;
    let now = aterm_time::unix_secs().unwrap();
    let first = standing_of(m7).meta.expect("the first token's meta");
    let compared = first.minted_at;
    let meta = Meta {
        minted_at: first.minted_at + 600,
        expires_at: first.expires_at + 600,
        ..first
    };
    let claim = log_entries::pass_claim(m7)
        .unwrap()
        .expect("a pass's claim");
    let written = crate::claude_token::write(
        m7,
        &key(),
        &SecretBytes::from(TOKEN),
        &meta,
        OnBusy::Wait(std::time::Instant::now()),
    )
    .unwrap();
    drop(claim);
    assert_eq!(written, MemberWritten::Waiting(WaitFor::LockDeadline));
    let waiting = std::fs::read(log_entries::waiting_file(m7)).unwrap();
    let verified =
        crate::claude_token::verify(m7, &key(), compared, verified_at(now), refuse()).unwrap();
    let MemberWritten::Refused(why) = &verified else {
        panic!("the verification of the first meta: {verified:?}");
    };
    assert!(why.contains("changed since this write read it"), "{why}");
    assert_eq!(
        std::fs::read(log_entries::waiting_file(m7)).unwrap(),
        waiting,
        "the replacement waits as it was"
    );
    sync(m7, None).expect("m7's records pass and vault pass");
    let here = read(m7);
    assert_eq!(here.standing.check, Check::Readable);
    assert!(returns_token(m7), "the same token");
    assert_eq!(
        here.standing.meta.as_ref(),
        Some(&meta),
        "the replacement's meta, unverified"
    );
    assert!(here.standing.minted_here, "minted here");
    assert!(
        log_entries::read_overtaken(m7).unwrap().is_empty(),
        "nothing journalled overtaken"
    );
    lab.macs.server.stop();
}

/// Whether `bytes` holds any run of sixteen bytes of the fixture token: longer than any run the
/// token shares with what a file may name it by (its entry names and `claude-token:<key>` share
/// `e-token-` and `0123456789` with it).
fn holds_token_value(bytes: &[u8]) -> bool {
    bytes
        .windows(16)
        .any(|window| TOKEN.windows(16).any(|run| run == window))
}

/// Every regular file under `dir`, at any depth.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

/// A need of the account's token (`aterm secrets need claude-token:<key>`) is answered once the
/// vault's log gives the Mac a verified token in use there, and the token reaches no file on any
/// Mac, the need ledger, `~/.secrets` and the vault's own state included. m7 writes it: while it
/// is `unverified` both needs wait, m7's too (whether the Mac that minted it serves it is the
/// harness's verdict there, which no vault record holds) and m40's, which reads the entry but not
/// a token it may use; once m7 verifies it, both end `not-needed`, m40's resting on the vault seq
/// m40 reads it at. Once Claude Code refused
/// that token on m40 ([`crate::claude_token::note_refused`]), m40's need raised again waits, the
/// token's standing there `rejected`, while m7's, which refused nothing, ends at once. NEGATIVE
/// CONTROLS: before the write both wait; m40's sight names the entry while it waits, so its name
/// alone answers nothing; m7's need is the refusal's; and the scan finds the token in a file that
/// holds it.
#[test]
fn a_need_of_a_token_ends_once_a_token_in_use_arrives_and_no_file_holds_it() {
    use crate::need::{self, Purpose, Reason, Want};
    let lab = Lab::new();
    for paths in lab.all() {
        need::test_alab_policy(paths);
    }
    let want = Want::ClaudeToken(key());
    let entry = key().entry();
    let ended = |paths: &Paths| {
        let needs = need::list(paths).unwrap();
        let [need] = needs.as_slice() else {
            panic!("one need: {needs:?}");
        };
        assert_eq!(need.want, want);
        need.state.ended().then(|| need.clone())
    };
    for paths in [&lab.macs.m7, &lab.m40] {
        let raised = need::raise(paths, want.clone(), Purpose::ByHand).unwrap();
        assert!(!raised.state.ended(), "{}", raised.line);
        assert!(ended(paths).is_none(), "it waits before the write");
    }

    let lab = lab.minted();
    let now = aterm_time::unix_secs().unwrap();
    let Lookup::Found(minted) = crate::claude_token::standing(&lab.macs.m7, &key()).unwrap() else {
        panic!("m7 reads the token's standing");
    };
    assert!(
        minted.minted_here && !minted.verified_in_use(now),
        "m7 minted it, unverified"
    );
    assert!(
        ended(&lab.macs.m7).is_none(),
        "an unverified token answers nothing, even where it was minted"
    );
    let elsewhere = need::facts(&lab.m40, now).unwrap();
    assert!(
        elsewhere
            .sight
            .as_ref()
            .is_some_and(|sight| sight.held.contains(&entry)),
        "m40 reads the entry"
    );
    let Lookup::Found(unverified) = crate::claude_token::standing(&lab.m40, &key()).unwrap() else {
        panic!("m40 reads the token's standing");
    };
    assert!(!unverified.verified_in_use(now), "and no token it may use");
    assert!(
        ended(&lab.m40).is_none(),
        "the entry's name alone answers nothing"
    );

    let minted_at = minted.meta.as_ref().expect("a meta").minted_at;
    assert_eq!(
        crate::claude_token::verify(&lab.macs.m7, &key(), minted_at, verified_at(now), refuse())
            .unwrap(),
        MemberWritten::Posted
    );
    lab.sync_all();
    let minted_here = ended(&lab.macs.m7).expect("verified, it is in use where it was minted");
    assert_eq!(
        minted_here.retired.map(|retired| retired.reason),
        Some(Reason::NotNeeded)
    );
    let answered = ended(&lab.m40).expect("a verified token is in use on every Mac");
    let retired = answered.retired.as_ref().unwrap();
    assert_eq!(retired.reason, Reason::NotNeeded, "{}", answered.line);
    assert!(retired.evidence.starts_with("vault seq "), "{retired:?}");

    let Lookup::Found(verified) = crate::claude_token::standing(&lab.m40, &key()).unwrap() else {
        panic!("m40 reads the token's standing");
    };
    assert!(crate::claude_token::note_refused(&lab.m40, &verified, now).unwrap());
    assert!(
        !crate::claude_token::note_refused(&lab.m40, &verified, now + 60).unwrap(),
        "the first refusal stands"
    );
    let Lookup::Found(refused) = crate::claude_token::standing(&lab.m40, &key()).unwrap() else {
        panic!("m40 reads the token's standing");
    };
    assert_eq!(
        (refused.state(now), refused.refused_at),
        (State::Rejected, Some(now))
    );
    let again = need::raise(&lab.m40, want.clone(), Purpose::ByHand).unwrap();
    assert!(!again.state.ended(), "{}", again.line);
    assert!(
        ended(&lab.m40).is_none(),
        "a token refused here answers nothing"
    );
    let control = need::raise(&lab.macs.m7, want.clone(), Purpose::ByHand).unwrap();
    assert_eq!(
        control.retired.map(|retired| retired.reason),
        Some(Reason::NotNeeded),
        "CONTROL: on a Mac that refused nothing it is in use: {}",
        control.line
    );

    let root = lab.macs.root.path();
    let ledger = lab.m40.dir.join(need::LEDGER);
    let files = files_under(root);
    assert!(files.contains(&ledger), "the scan reads the need ledger");
    for file in &files {
        assert!(
            !holds_token_value(&std::fs::read(file).unwrap()),
            "{} holds the token",
            file.display()
        );
    }
    assert!(
        std::fs::read_to_string(&ledger)
            .unwrap()
            .contains(&want.key()),
        "the ledger the scan read names the want"
    );
    let planted = root.join("control");
    std::fs::write(&planted, &TOKEN[TOKEN.len() - 16..]).unwrap();
    assert!(
        files_under(root)
            .iter()
            .any(|file| holds_token_value(&std::fs::read(file).unwrap())),
        "CONTROL: the scan finds a file that holds part of it"
    );
    lab.macs.server.stop();
}
