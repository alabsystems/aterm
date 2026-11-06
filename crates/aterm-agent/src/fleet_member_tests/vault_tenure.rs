// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault a fleet state was taken under (`crate::fleet_vault`, ATERM-KI-1105-10): its drive
//! grants admit only while that vault is the one configured. Each vault here is a hand-written
//! `source.toml` beside the member's state, with no records: its log binds no key, so F17 judges
//! every key by the book and the grants, the shape a move to another vault leaves the Macs of the
//! vault left in.

use super::*;
use crate::fleet_vault::{Tenure, Vault, tenure};
use aterm_secrets::client::Paths;

/// The vault configured beside `world`'s state.
fn vault_of(world: &World) -> Paths {
    Paths::for_home(world.places.state.parent().unwrap())
}

/// Configure vault `name` beside `world`'s state, pinned at the key of `signer` bytes and reached
/// from those of `followed`, with no records: its log binds no key and its deny list names none.
fn in_vault(world: &World, name: &str, signer: u8, followed: &[u8]) {
    configure(&vault_of(world), name, signer, followed);
}

/// [`in_vault`] at the vault paths `paths`.
fn configure(paths: &Paths, name: &str, signer: u8, followed: &[u8]) {
    aterm_secrets::fsx::ensure_private_dir(&paths.dir).unwrap();
    let key = |byte: u8| format!("\"{}\"", aterm_secrets::b64::encode_padded(&[byte; 32]));
    let followed: Vec<String> = followed.iter().map(|byte| key(*byte)).collect();
    aterm_secrets::fsx::write_atomic(
        &paths.dir.join("source.toml"),
        format!(
            "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = {}\n\
             followed = [{}]\nhost = \"m22\"\n",
            key(signer),
            followed.join(", ")
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
}

/// Where `world`'s fleet state stands against the vault configured beside it.
fn tenure_of(world: &World) -> Tenure {
    tenure(&world.places.state, Some(&vault_of(world)))
}

/// The `name` line of the record of vault `name`.
fn named(name: &str) -> String {
    format!("name {}\n", aterm_codec::hex::encode_lower(name.as_bytes()))
}

/// Where a member of a loopback fleet listens: a port of its own.
fn here() -> Opts {
    Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    }
}

/// A fleet joined under vault "alab" records that vault in its join's take, and a grant made then
/// lets the issuer drive this Mac while "alab" is the vault configured: through a signer rotation,
/// and through the line re-pinned at the rotated key alone once a writer widened the record by it.
/// Under another vault, under another key line with the name of "alab" (a replacement with no
/// handoff chain), with no vault and with a record that cannot be read, no grant admits: the grant
/// stays in aterm.toml, `aterm fleet peers` says why, and `aterm fleet allow drive` grants
/// nothing. A join of the same fleet again under the vault this Mac is in takes the grant made
/// under the one it left out, as another fleet's join does, and a grant made after it drives.
///
/// NEGATIVE CONTROLS (2026-11-05): `fleet_vault::grants_in_effect` returning `grants` fails
/// "vault \"other\": the grant made under \"alab\" drives this Mac"; the join's take writing no
/// record (`fleet_vault::take` dropped) fails "the join records the vault its fleet is taken
/// under"; a record never widened (`kept` answering `None` for a record there) fails "the line
/// re-pinned at the rotated key alone"; the grant's refusal dropped from `set_level` fails
/// "vault \"other\": `aterm fleet allow drive` grants nothing"; the join's `Ungrant::OtherVault`
/// arm dropped fails on the join's report, which says no grant went; an unreadable record read
/// as none (`tenure` answering `Here`) fails "a record that names no key".
#[test]
fn a_fleet_taken_under_one_vault_drives_by_its_grants_under_it_alone() {
    let (issuer, code, _serving) = issuing("tenure-issuer");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let member = World::new("tenure-member");
    in_vault(&member, "alab", 1, &[]);
    ok(join(&member.places, &here(), &code));
    let record = member::read_vault(&member.places.state)
        .unwrap()
        .expect("the join records the vault its fleet is taken under");
    assert!(record.contains(&named("alab")), "{record}");
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let granted = fleet_table(&member.config()).unwrap();
    assert_eq!(granted.drive_from.len(), 1, "{granted:?}");
    let drives = || drives_here(&member.places, &issuer_node);
    assert_eq!(drives(), Some(true), "the grant made under \"alab\"");
    assert_eq!(void_grants(&member.places), None);

    in_vault(&member, "alab", 2, &[1]);
    assert_eq!(
        (tenure_of(&member), drives()),
        (Tenure::Here, Some(true)),
        "a signer rotation: a new signer reached from the old"
    );
    // A writer under the book's lock (a heartbeat round) widens the record by the rotated key.
    crate::fleet_vault::keep(&member.places.state, Some(&vault_of(&member)));
    in_vault(&member, "alab", 2, &[]);
    assert_eq!(
        (tenure_of(&member), drives()),
        (Tenure::Here, Some(true)),
        "the line re-pinned at the rotated key alone (a join of the vault at another place)"
    );

    for (vault, signer, now) in [
        ("other", 9, "this Mac is in vault \"other\" now"),
        (
            "alab",
            7,
            "the vault \"alab\" this Mac is in now pins another signer",
        ),
    ] {
        in_vault(&member, vault, signer, &[]);
        assert_eq!(
            drives(),
            Some(false),
            "vault \"{vault}\": the grant made under \"alab\" drives this Mac"
        );
        assert!(granted_here(&member.places).is_empty(), "vault \"{vault}\"");
        assert_eq!(
            fleet_table(&member.config()).unwrap(),
            granted,
            "vault \"{vault}\": the person's grant stays as they made it"
        );
        let said = void_grants(&member.places).expect("`aterm fleet peers` says why");
        assert!(
            said.contains("taken under vault \"alab\"")
                && said.contains(now)
                && said.contains("[fleet] drive_from keeps 1 grant as it is")
                && said.contains("`aterm fleet sync`"),
            "vault \"{vault}\": {said}"
        );
        let refused = match allow(&member.places, &args(&["drive", "--from", &issuer_node])) {
            Ok(report) => {
                panic!("vault \"{vault}\": `aterm fleet allow drive` grants nothing: {report}")
            }
            Err(failure) => failure_words(failure),
        };
        assert!(
            refused.contains(now) && refused.contains("nothing was granted"),
            "vault \"{vault}\": {refused}"
        );
        assert_eq!(fleet_table(&member.config()).unwrap(), granted);
    }

    let source = vault_of(&member).dir.join("source.toml");
    std::fs::remove_file(&source).unwrap();
    assert_eq!(
        (tenure_of(&member), drives()),
        (
            Tenure::Elsewhere {
                taken: "alab".to_owned(),
                now: None
            },
            Some(false)
        ),
        "no vault configured"
    );
    in_vault(&member, "alab", 2, &[]);
    let record = member.places.state.join(member::VAULT_FILE);
    let kept = std::fs::read(&record).unwrap();
    write_private(&member.places.state, member::VAULT_FILE, b"name 61\n").unwrap();
    assert!(
        matches!(tenure_of(&member), Tenure::Unread(_)) && drives() == Some(false),
        "a record that names no key"
    );
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&member.places.state, member::VAULT_FILE, &kept).unwrap();
    assert!(
        matches!(tenure_of(&member), Tenure::Unread(_)) && drives() == Some(false),
        "a vault configuration that cannot be read"
    );
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(drives(), Some(true), "CONTROL: both read again");

    in_vault(&member, "other", 9, &[]);
    let listed = ok(peers(&member.places, &args(&[])));
    assert!(
        listed.contains(
            "Drive: this Mac's fleet, its book and its drive grants were taken under vault \"alab\""
        ),
        "{listed}"
    );
    let again = ok(join(&member.places, &here(), &code));
    assert!(
        again.contains("made under another vault than the one this Mac is in now"),
        "{again}"
    );
    assert!(
        configured_grants(&member.places).is_empty(),
        "a join again under the vault this Mac is in takes the grant made under the vault left \
         out: {:?}",
        fleet_table(&member.config())
    );
    assert_eq!(tenure_of(&member), Tenure::Here);
    assert_eq!(drives(), Some(false));
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    assert_eq!(
        drives(),
        Some(true),
        "a grant made under the vault this Mac is in drives it"
    );
    assert_eq!(void_grants(&member.places), None);
}

/// A fleet with no vault drives by its grants as before, and its take records none. A record no
/// take wrote (a fleet an older build joined, or one taken under no vault) is read as the vault
/// configured now, and the first writer that reads one under the book's lock records it: a read
/// of the issuer's book (`aterm fleet peers`) here, a heartbeat round, a sync in the fleet. From
/// then that vault is the one its grants are of, and no writer under another moves it. A state
/// that holds no fleet records nothing, and a leave takes the record with the fleet.
///
/// NEGATIVE CONTROLS (2026-11-05): no adoption (`kept` answering `None` where no record is
/// there) fails "the first writer that reads a vault records it"; `keep_in` without its fleet
/// check fails "a state that holds no fleet records nothing".
#[test]
fn a_record_no_take_wrote_adopts_the_vault_the_first_writer_reads() {
    let (issuer, code, _serving) = issuing("adopt-issuer");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let member = World::new("adopt-member");
    ok(join(&member.places, &here(), &code));
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let state = &member.places.state;
    let drives = || drives_here(&member.places, &issuer_node);
    assert_eq!(
        member::read_vault(state).unwrap(),
        None,
        "a take under no vault records none"
    );
    assert_eq!(
        (tenure(state, None), drives()),
        (Tenure::Here, Some(true)),
        "a fleet with no vault drives by its grants, as before"
    );

    in_vault(&member, "alab", 1, &[]);
    assert_eq!(
        (tenure_of(&member), drives()),
        (Tenure::Here, Some(true)),
        "a vault configured since, which no writer has read yet: adopted"
    );
    ok(peers(&member.places, &args(&[])));
    assert!(
        member::read_vault(state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "the first writer that reads a vault records it"
    );
    in_vault(&member, "other", 9, &[]);
    assert_eq!(
        drives(),
        Some(false),
        "the vault adopted is the one the grants are of from then on"
    );
    crate::fleet_vault::keep(state, Some(&vault_of(&member)));
    ok(peers(&member.places, &args(&[])));
    assert!(
        member::read_vault(state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "a writer under another vault moves the record"
    );
    assert_eq!(drives(), Some(false));

    let bare = World::new("adopt-bare");
    in_vault(&bare, "alab", 1, &[]);
    member::ensure_private_dir(&bare.places.state).unwrap();
    crate::fleet_vault::keep(&bare.places.state, Some(&vault_of(&bare)));
    assert_eq!(
        member::read_vault(&bare.places.state).unwrap(),
        None,
        "a state that holds no fleet records nothing"
    );

    ok(leave(&member.places, &args(&[])));
    assert_eq!(
        member::read_vault(state).unwrap(),
        None,
        "a leave takes the record with the fleet"
    );

    // A Mac that moved before any writer recorded a vault adopts the one it moved to, and a join
    // of the same fleet again keeps the grants then (ATERM-KI-1105-10): what ends them is a
    // level below drive, a leave, or a join of another fleet.
    let moved = World::new("adopt-moved");
    ok(join(&moved.places, &here(), &code));
    ok(allow(
        &moved.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    in_vault(&moved, "other", 9, &[]);
    ok(peers(&moved.places, &args(&[])));
    assert!(
        member::read_vault(&moved.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("other")))
    );
    let drives = || drives_here(&moved.places, &issuer_node);
    assert_eq!(
        drives(),
        Some(true),
        "the grant made before the move, adopted"
    );
    ok(join(&moved.places, &here(), &code));
    assert_eq!(
        (configured_grants(&moved.places).len(), drives()),
        (1, Some(true)),
        "a join of the same fleet again keeps the grants of an adopted record"
    );
    ok(allow(&moved.places, &args(&["content"])));
    assert_eq!(
        (configured_grants(&moved.places).len(), drives()),
        (0, Some(false)),
        "a level below drive takes them out"
    );
}

/// The vault's default fleet, taken by a sync under vault "alab", with its default grant (`*`,
/// the ALab `--all` policy) letting every member drive, as before; a sync in that fleet adopts a
/// record no take wrote. Moved to vault "other", whose
/// default fleet code names the same fleet, a sync takes it again under "other", as a join does:
/// the grant made under "alab" goes and the default grant is made again under "other". An issuer
/// that took its fleet under another vault than the one whose default fleet is another cannot
/// join its own, and a sync says how it issues the fleet anew.
///
/// NEGATIVE CONTROLS (2026-11-05): `sync_plan` without its arms for a fleet taken elsewhere
/// fails on its plan for the member, which owes no `Join`; no adoption fails "an issuer that
/// took its fleet under another vault", whose record a heartbeat round adopted.
#[test]
fn a_sync_takes_the_default_fleet_again_under_the_vault_this_mac_moved_to() {
    let (issuer, code, _serving) = issuing("tenure-sync");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let member = World::new("tenure-sync-member");
    in_vault(&member, "alab", 1, &[]);
    assert!(matches!(
        converged(converge_on(&member.places, &code, &sync_here())),
        FleetSync::Joined(_)
    ));
    let parsed = JoinCode::parse(&code).unwrap();
    let key = FleetSecret::new(&parsed.secret).unwrap();
    let drives = || drives_here(&member.places, &issuer_node);
    assert_eq!(
        (fleet_table(&member.config()).unwrap().drive_from, drives()),
        (vec![aterm_net::fleet::ALL_MEMBERS.to_owned()], Some(true)),
        "the default grant under one vault, as before"
    );
    assert!(matches!(
        sync_plan(&member.places, &parsed, &key),
        Ok(SyncPlan::In)
    ));
    // A record no take wrote: a sync that finds this Mac in the vault's fleet adopts the vault.
    remove_private(&member.places.state, member::VAULT_FILE).unwrap();
    assert!(matches!(
        converged(converge_on(&member.places, &code, &sync_here())),
        FleetSync::Current
    ));
    assert!(
        member::read_vault(&member.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "a sync in the vault's fleet records the vault"
    );

    in_vault(&member, "other", 9, &[]);
    assert_eq!(drives(), Some(false));
    assert!(matches!(
        sync_plan(&member.places, &parsed, &key),
        Ok(SyncPlan::Join(..))
    ));
    assert!(
        matches!(
            converged(converge_on(&member.places, &code, &sync_here())),
            FleetSync::Joined(_)
        ),
        "a sync under the vault moved to takes the default fleet again"
    );
    assert_eq!(
        (
            tenure_of(&member),
            fleet_table(&member.config()).unwrap().drive_from,
            drives()
        ),
        (
            Tenure::Here,
            vec![aterm_net::fleet::ALL_MEMBERS.to_owned()],
            Some(true)
        ),
        "the default grant made again under the vault this Mac is in"
    );
    assert!(matches!(
        sync_plan(&member.places, &parsed, &key),
        Ok(SyncPlan::In)
    ));

    in_vault(&issuer, "alab", 1, &[]);
    crate::fleet_vault::keep(&issuer.places.state, Some(&vault_of(&issuer)));
    in_vault(&issuer, "other", 9, &[]);
    let another = JoinCode::parse(&a_code()).unwrap();
    let another_key = FleetSecret::new(&another.secret).unwrap();
    assert!(
        matches!(
            sync_plan(&issuer.places, &another, &another_key),
            Ok(SyncPlan::Blocked(why)) if why.contains("`aterm fleet init --rotate`")
        ),
        "an issuer that took its fleet under another vault, whose default fleet is another"
    );
}

/// The issuer of the vault's default fleet, which it took under another vault than the one
/// configured now (a move, or a vault replacement whose key line keeps no key of the one before),
/// takes it again in place at its next sync, as a member's sync takes it by a join: the grants
/// made beside the old record go, named in the report, the vault configured is recorded, and the
/// default grant (`"*"`, the vault's `--all` policy) is made again where it stood; a narrower
/// grant is not made again, and a grant made after it is that vault's. A record of the vault
/// that does not read is taken again the same way, said as that.
///
/// NEGATIVE CONTROL (2026-11-05): `sync_plan` without its `Retake` arm (the issuer of the
/// vault's default fleet read as blocked, as before) fails "the issuer of the vault's default
/// fleet takes it again in place".
#[test]
fn the_issuer_of_the_vaults_default_fleet_takes_it_again_under_the_vault_configured() {
    let (issuer, code, _serving) = issuing("retake-issuer");
    let member = World::new("retake-member");
    ok(join(&member.places, &here(), &code));
    let member_node = member::node_id(&member.places.state).unwrap();
    in_vault(&issuer, "alab", 1, &[]);
    crate::fleet_vault::keep(&issuer.places.state, Some(&vault_of(&issuer)));
    ok(allow(&issuer.places, &args(&["drive", "--all"])));
    let drives = || drives_here(&issuer.places, &member_node);
    let table = || fleet_table(&issuer.config()).unwrap();
    assert_eq!(
        (table().drive_from, drives()),
        (vec![aterm_net::fleet::ALL_MEMBERS.to_owned()], Some(true)),
        "PRECONDITION: the default grant under \"alab\""
    );

    in_vault(&issuer, "other", 9, &[]);
    assert_eq!(drives(), Some(false), "PRECONDITION: void under \"other\"");
    let parsed = JoinCode::parse(&code).unwrap();
    let key = FleetSecret::new(&parsed.secret).unwrap();
    assert!(
        matches!(
            sync_plan(&issuer.places, &parsed, &key),
            Ok(SyncPlan::Retake(_))
        ),
        "the issuer of the vault's default fleet takes it again in place"
    );
    assert!(matches!(
        checked(check_on(&issuer.places, &code)),
        FleetCheck::Due(why) if why.contains("taken under another vault")
    ));
    let report = match converged(converge_on(&issuer.places, &code, &sync_here())) {
        FleetSync::Retaken(report) => report,
        other => panic!("{other:?}"),
    };
    assert!(
        report.contains("made under another vault than the one this Mac is in now")
            && report.contains("every member this Mac's membership log admits"),
        "the grant made under \"alab\" out, and the default grant made again: {report}"
    );
    assert_eq!(
        (tenure_of(&issuer), table().drive_from, drives()),
        (
            Tenure::Here,
            vec![aterm_net::fleet::ALL_MEMBERS.to_owned()],
            Some(true)
        ),
        "the default grant made again under the vault this Mac is in"
    );
    assert!(
        member::read_vault(&issuer.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("other")))
    );
    assert_eq!(
        checked(check_on(&issuer.places, &code)),
        FleetCheck::Current
    );

    // A narrower grant, made under "other", goes and is not made again.
    ok(allow(&issuer.places, &args(&["content"])));
    ok(allow(
        &issuer.places,
        &args(&["drive", "--from", &member_node]),
    ));
    assert_eq!(drives(), Some(true));
    in_vault(&issuer, "third", 5, &[]);
    let report = match converged(converge_on(&issuer.places, &code, &sync_here())) {
        FleetSync::Retaken(report) => report,
        other => panic!("{other:?}"),
    };
    assert!(
        report.contains("its grant of [fleet] drive_from removed"),
        "{report}"
    );
    assert_eq!(
        (table().drive_from, table().level(), drives()),
        (
            Vec::<String>::new(),
            Some(FleetScope::Content.word()),
            Some(false)
        ),
        "a narrower grant is the person's to make again"
    );
    ok(allow(
        &issuer.places,
        &args(&["drive", "--from", &member_node]),
    ));
    assert_eq!(drives(), Some(true), "a grant made under \"third\" drives");

    // A record that does not read: taken again the same way, said as that.
    write_private(&issuer.places.state, member::VAULT_FILE, b"name 61\n").unwrap();
    assert!(matches!(tenure_of(&issuer), Tenure::Unread(_)));
    assert!(matches!(
        checked(check_on(&issuer.places, &code)),
        FleetCheck::Due(why) if why.contains("cannot read which vault")
    ));
    let report = match converged(converge_on(&issuer.places, &code, &sync_here())) {
        FleetSync::Retaken(report) => report,
        other => panic!("{other:?}"),
    };
    assert!(
        report.contains("the record of the vault it was made under does not read"),
        "{report}"
    );
    assert_eq!(tenure_of(&issuer), Tenure::Here);
}

/// A move to another vault while a take of the fleet is under way leaves the fleet the vault's
/// the take was decided under: a sync's join of the vault's default fleet that dials while this
/// Mac joins another vault (`aterm secrets join` beside the vault subscriber's pass) takes the
/// fleet under the vault it read the code under, so its default grant admits no Mac under the
/// vault moved to, and the join's report says so; a person's join, under the vault configured as
/// it began. A sync that finds this Mac in the fleet, or rekeys it, adopts for a record no take
/// wrote the vault it read the code under, not the one configured by the time it writes.
///
/// NEGATIVE CONTROLS (2026-11-05): the join's take of the vault configured as it writes
/// (`fleet_vault::take` of `vault_now(places)` in place of `under`) fails "the sync's join takes
/// the fleet under the vault it read the code under"; `in_fleet` adopting the vault configured
/// (`keep_as` of `vault_now(places)`) fails "a sync in the fleet adopts the vault it read the
/// code under"; `rekey` keeping the vault configured fails "a rekey adopts the vault it read the
/// code under".
#[test]
fn a_move_while_a_take_is_under_way_leaves_the_fleet_the_vaults_it_was_decided_under() {
    let (issuer, code, _serving) = issuing("decided-issuer");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let moved_at_lock = |world: &World| {
        let paths = vault_of(world);
        move |step: &str| {
            if step == "lock" {
                configure(&paths, "other", 9, &[]);
            }
        }
    };

    let member = World::new("decided-member");
    in_vault(&member, "alab", 1, &[]);
    let synced = with_step_hook(moved_at_lock(&member), || {
        sync_with(&member.places, false, publishes(&code), &sync_here())
    });
    let report = match converged(synced) {
        FleetSync::Joined(report) => report,
        other => panic!("{other:?}"),
    };
    assert!(
        member::read_vault(&member.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "the sync's join takes the fleet under the vault it read the code under"
    );
    assert_eq!(
        (
            tenure_of(&member),
            fleet_table(&member.config()).unwrap().drive_from,
            drives_here(&member.places, &issuer_node)
        ),
        (
            Tenure::Elsewhere {
                taken: "alab".to_owned(),
                now: Some("other".to_owned())
            },
            vec![aterm_net::fleet::ALL_MEMBERS.to_owned()],
            Some(false)
        ),
        "the default grant, made under \"alab\", admits no Mac under \"other\""
    );
    assert!(
        report.contains(
            "Drive: this Mac's fleet, its book and its drive grants were taken under vault \"alab\""
        ),
        "{report}"
    );

    let person = World::new("decided-person");
    in_vault(&person, "alab", 1, &[]);
    ok(with_step_hook(moved_at_lock(&person), || {
        join(&person.places, &here(), &code)
    }));
    assert!(
        member::read_vault(&person.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "a person's join takes the fleet under the vault configured as it began"
    );
    assert!(
        allow(&person.places, &args(&["drive", "--from", &issuer_node])).is_err(),
        "no grant is made beside a fleet the vault left holds"
    );

    let adopting = World::new("decided-adopting");
    in_vault(&adopting, "alab", 1, &[]);
    assert!(matches!(
        converged(converge_on(&adopting.places, &code, &sync_here())),
        FleetSync::Joined(_)
    ));
    let alab = Vault::configured(&vault_of(&adopting));
    assert!(alab.is_some());
    remove_private(&adopting.places.state, member::VAULT_FILE).unwrap();
    in_vault(&adopting, "other", 9, &[]);
    assert_eq!(
        converged(converge_under(
            &adopting.places,
            &code,
            alab.as_ref(),
            &sync_here()
        )),
        FleetSync::Current
    );
    assert!(
        member::read_vault(&adopting.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "a sync in the fleet adopts the vault it read the code under"
    );
    assert_eq!(drives_here(&adopting.places, &issuer_node), Some(false));

    remove_private(&adopting.places.state, member::VAULT_FILE).unwrap();
    let rotated = aterm_secrets::client::rotate_fleet_code_text(&code).unwrap();
    assert!(matches!(
        converged(converge_under(
            &adopting.places,
            &rotated,
            alab.as_ref(),
            &sync_here()
        )),
        FleetSync::Rekeyed(_)
    ));
    assert!(
        member::read_vault(&adopting.places.state)
            .unwrap()
            .is_some_and(|record| record.contains(&named("alab"))),
        "a rekey adopts the vault it read the code under"
    );
}

/// A record of the vault the fleet was taken under that does not read is said as that, never as
/// a move: a sync plans a join with it said, and the join takes the grants made beside the record
/// out as made beside a record that does not read, records the vault configured, and makes the
/// default grant again.
///
/// NEGATIVE CONTROLS (2026-11-05): the join's `Tenure::Unread` arm giving `Ungrant::OtherVault`
/// fails "the grants go as made beside a record that does not read"; `sync_plan` saying a move
/// for it fails "a sync says the record does not read".
#[test]
fn a_record_of_the_vault_that_does_not_read_is_said_so_and_taken_again() {
    let (issuer, code, _serving) = issuing("unread-record-issuer");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let member = World::new("unread-record-member");
    in_vault(&member, "alab", 1, &[]);
    assert!(matches!(
        converged(converge_on(&member.places, &code, &sync_here())),
        FleetSync::Joined(_)
    ));
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    write_private(&member.places.state, member::VAULT_FILE, b"name 61\n").unwrap();
    assert!(matches!(tenure_of(&member), Tenure::Unread(_)));
    assert_eq!(drives_here(&member.places, &issuer_node), Some(false));
    let parsed = JoinCode::parse(&code).unwrap();
    let key = FleetSecret::new(&parsed.secret).unwrap();
    assert!(
        matches!(
            sync_plan(&member.places, &parsed, &key),
            Ok(SyncPlan::Join(_, why)) if why.contains("cannot read which vault")
                && !why.contains("taken under another vault")
        ),
        "a sync says the record does not read"
    );
    let report = match converged(converge_on(&member.places, &code, &sync_here())) {
        FleetSync::Joined(report) => report,
        other => panic!("{other:?}"),
    };
    assert!(
        report.contains("the record of the vault they were made under does not read")
            && !report.contains("made under another vault"),
        "the grants go as made beside a record that does not read: {report}"
    );
    assert_eq!(
        (
            tenure_of(&member),
            fleet_table(&member.config()).unwrap().drive_from,
            drives_here(&member.places, &issuer_node)
        ),
        (
            Tenure::Here,
            vec![aterm_net::fleet::ALL_MEMBERS.to_owned()],
            Some(true)
        )
    );
}
