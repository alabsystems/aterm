// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::SecretBytes;
use aterm_secrets::commons::journal::Trust;
use aterm_secrets::members::Bundle;
use aterm_secrets::members::bundle::Handed;
use aterm_secrets::members::desk::{self, Desk, Handing, Handout, Local};
use aterm_secrets::text::{self, Audience, Entry};
use std::net::IpAddr;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;

mod staged_discovery_tests;

const TRUST: Trust = Trust {
    network: [1; 16],
    authority: [2; 32],
    genesis: [3; 32],
};

/// A Mac with a synthetic machine key and Commons device under `root/name`. Its identity is
/// unmarked, as a release build makes one on a Mac: these joins ask members a browse finds,
/// which a lab identity never does ([`a_lab_identity_asks_no_member_a_browse_finds`]).
fn mac(root: &Path, name: &str) -> (Paths, Device) {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
    let device = Device::open(&paths).expect("device").expect("an identity");
    (paths, device)
}

/// The joining Mac `name` under `root`, with a synthetic node key.
fn me(root: &Path, name: &str) -> Me {
    let (paths, device) = mac(root, name);
    Me {
        paths,
        device,
        node: [7; 32],
        name: name.to_owned(),
        model: "Mac15,3".to_owned(),
        place: join::JoinPlace::Home,
        members: discover::MEMBER_SERVICE.to_owned(),
    }
}

/// The desk of the member `name` under `root`, which can allow.
fn member(root: &Path, name: &str) -> Mutex<Desk> {
    let (_, device) = mac(root, name);
    Mutex::new(Desk::new(
        Local::with(device, TRUST, true, name).expect("member"),
    ))
}

/// The bundle a member hands: F10's files, one synthetic `[everyone]` file, and a vault
/// server that refuses at once (nothing listens on port 1).
fn bundle(handing: &Handing<'_>) -> aterm_secrets::Result<Handout> {
    let bundle = Bundle {
        vault: "alab".into(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["http://127.0.0.1:1/v1/alab".into()],
        allowed: handing.allowed.clone(),
        handed: Handed::Files(text::render_everyone_locker(&[Entry {
            name: "gh_access_token".into(),
            value: SecretBytes::new(b"synthetic-gh-token".to_vec()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }])),
    }
    .encode()?;
    Ok(Handout {
        bundle,
        logged: false,
        note: None,
    })
}

/// Allow the one row on `desk`: the words its sheet shows.
fn allow(desk: &Mutex<Desk>) -> String {
    let mut desk = desk.lock().unwrap();
    let now = aterm_time::Instant::now();
    let row = desk.rows(now)[0].id;
    let phrase = desk.allow(row, now).expect("allowed");
    desk.words(phrase, now).expect("live").join(" ")
}

/// The member at [`M1`].
const M1: &str = "m1:7100";

/// Run `body` with a dial that reaches `desk` at [`M1`] (every other address refuses), the
/// desk serving each connection on its own thread with a one-minute hang detector on both
/// ends; and how many times it was dialed.
fn with_member<T>(desk: &Mutex<Desk>, body: impl FnOnce(&Dial<'_>) -> T) -> (T, usize) {
    let dials = std::sync::atomic::AtomicUsize::new(0);
    let said = std::thread::scope(|scope| {
        let dial = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            if at != M1 {
                return Err(aterm_secrets::Error::new("connection refused"));
            }
            dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (ours, theirs) = UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
            }
            scope.spawn(move || {
                let mut ours = ours;
                let _ = desk::serve(desk, &mut ours, IpAddr::from([192, 0, 2, 9]), &bundle);
            });
            Ok(Box::new(theirs))
        };
        body(&dial)
    });
    (said, dials.into_inner())
}

/// A vault no test reaches.
fn no_vault(_: &Paths) -> aterm_secrets::Result<()> {
    Err(aterm_secrets::Error::new("no vault in this test"))
}

/// `join_as` with `args` through `reach`: its exit code, and what it said on stdout and on
/// stderr.
fn joined_apart(me: &Me, args: &JoinArgs, reach: &Reach<'_>) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = join_as(me, args, reach, &mut out, &mut err);
    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    (code, out, err)
}

/// `join_as` with `args` through `reach`: its exit code, and what it said on both streams.
fn joined(me: &Me, args: &JoinArgs, reach: &Reach<'_>) -> (u8, String) {
    let (code, out, err) = joined_apart(me, args, reach);
    (code, out + &err)
}

/// Let `j` in through `m1`: it asks, a person allows the row, and `j` runs the phrase with
/// `follow` standing in for its subscriber's install. What the run said.
fn let_in(m1: &Mutex<Desk>, j: &Me, follow: &(dyn Fn(&Path) -> Follows + Sync)) -> String {
    let ((code, said), _) = with_member(m1, |dial| {
        let ats = [M1.to_owned()];
        let (heard, missed) = join::ask_all(dial, &j.joining().unwrap(), &ats);
        assert_eq!(heard.len(), 1, "{missed:?}");
        let words = allow(m1);
        let reach = Reach {
            browse: &|| Ok(Vec::new()),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow,
        };
        let args = JoinArgs {
            phrase: Some(SecretBytes::from(words)),
            at: ats.to_vec(),
            ..JoinArgs::default()
        };
        joined(j, &args, &reach)
    });
    assert_eq!(code, 0, "{said}");
    said
}

/// A Mac the phrase lets in on a lane with no window (SSH, `--no-app`, Linux) gets its vault
/// subscriber from the run, since nothing else there starts one, and hears that it follows
/// the vault with no one here only while that subscriber runs; `status` says the same.
///
/// NEGATIVE CONTROL: drop the `(reach.follow)(…)` call in `join_as`'s `Ok` arm and print the
/// two lines as before (`asks again every hour` whatever runs): no install is asked for, and
/// the run promises a subscriber that does not run.
#[test]
fn a_mac_the_phrase_lets_in_starts_its_vault_subscriber_and_says_it_follows_only_when_one_runs() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let follow = |home: &Path| {
        homes.lock().unwrap().push(home.to_path_buf());
        Follows::Failed("launchd refused it".to_owned())
    };
    let said = let_in(&m1, &j, &follow);
    assert!(
        client::config(&j.paths).expect("config").is_some(),
        "the bundle set up the vault: {said}"
    );
    assert_eq!(
        homes.lock().unwrap().as_slice(),
        std::slice::from_ref(&j.paths.home),
        "the run installs this Mac's subscriber: {said}"
    );
    assert!(
        !said.contains("needs no one here") && !said.contains("asks again every hour"),
        "{said}"
    );
    assert!(said.contains("aterm secrets agent on"), "{said}");
    // `status` there: it finishes by itself only while the subscriber runs.
    let absent = status_said(&j.paths, &|_| Follows::Absent, &|| join::JoinPlace::Home);
    assert!(absent.contains("aterm secrets agent on"), "{absent}");
    assert!(!absent.contains("finishes by itself"), "{absent}");
    let runs = status_said(&j.paths, &|_| Follows::Runs, &|| join::JoinPlace::Home);
    assert!(runs.contains("finishes by itself"), "{runs}");
}

/// Write `membership` into the vault state of the Mac at `paths`, as its sync stores it.
fn store_membership(paths: &Paths, membership: &str) {
    let at = paths.dir.join("state.toml");
    let mut state: aterm_toml::Table =
        aterm_toml::from_str(&std::fs::read_to_string(&at).expect("state")).expect("toml");
    state.insert(
        "membership".into(),
        aterm_toml::Value::String(membership.into()),
    );
    aterm_secrets::fsx::write_atomic(
        &at,
        aterm_toml::to_string(&state).expect("toml").as_bytes(),
        0o600,
    )
    .expect("state written");
}

/// A Mac a rotation dropped with no `Remove` (one-mesh E2) is told so, in `aterm secrets
/// status`'s words, with the way back (a new Mac), and not to run `aterm members join` here,
/// which every member that read the rotation refuses for its key.
/// `client::test_read_dropped` stands in for the judgment, which aterm-secrets'
/// `a_recipient_a_rotation_drops_with_no_remove_reads_out_and_says_dropped` holds to a real
/// rotation.
///
/// CONTROL: the same Mac, revoked and not dropped, reads `NotIn` and its old line.
#[test]
fn a_dropped_mac_says_how_it_comes_back_where_a_revoked_one_reads_not_in() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let _ = let_in(&m1, &j, &|_| Follows::Runs);
    store_membership(&j.paths, "revoked");
    let Ok(client::Membership::Revoked { seq }) =
        client::status(&j.paths).map(|status| status.membership)
    else {
        panic!("j reads itself out");
    };
    let read = || Standing::read(&j.paths, &|_| Follows::Runs, &|| join::JoinPlace::Home);
    assert!(!client::dropped(&j.paths), "no rotation dropped j");
    assert_eq!(read(), Standing::NotIn, "the control");
    assert!(read().line().contains("aterm members join"));

    let (standing, words) =
        client::test_read_dropped(|| (read(), client::out_words(&j.paths, seq)));
    assert_eq!(standing, Standing::Dropped(words.clone()));
    let line = standing.line();
    assert_eq!(line, words, "the words `aterm secrets status` says");
    assert!(line.starts_with("dropped"), "{line}");
    assert!(!line.contains("aterm members join"), "{line}");
}

/// IN A FLEET (`docs/DESIGN-machine-names-2026-11-04.md` §11.85): a vault that took this
/// Mac out outranks the fleet key it still holds, which is a secret it was let go from, not
/// a membership.
///
/// CONTROLS: the same Mac and key while it is in reads in, and the key alone, where no vault
/// is read ([`crate::machine_name::standing_vault`]), still reads in.
#[test]
fn a_vault_that_took_this_mac_out_outranks_its_fleet_key() {
    use crate::machine_name::InFleet;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let _ = let_in(&m1, &j, &|_| Follows::Runs);
    let state = root.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state).expect("state");
    let key = format!("{}\n", "ab".repeat(32));
    aterm_uds::private_file::write_private(&state, aterm_net::fleet::KEY_FILE, key.as_bytes())
        .expect("a fleet key");
    crate::fleet_vault::take_fixture(&state, Some(&j.paths)).unwrap();
    assert_eq!(
        InFleet::read(&state, Some(&j.paths)),
        InFleet::In,
        "the control: in, with a key"
    );
    store_membership(&j.paths, "revoked");
    assert_eq!(InFleet::read(&state, Some(&j.paths)), InFleet::Out);
    assert_eq!(InFleet::read(&state, None), InFleet::In, "the key alone");
}

/// A Mac a rotation dropped (one-mesh E2, f299) asks no member of that vault with its key:
/// where its browse finds the members of the vault that dropped it (here a lab's members
/// service, the lab's vault being this world's), `aterm members join` and the window's phrase
/// join say `aterm secrets status`'s words before a member is looked for, dialed or proved to,
/// or ALab's vault joined, a phrase that is not one included, and the Mac stays revoked.
/// `client::test_read_dropped` stands in for the judgment, which aterm-secrets'
/// `a_dropped_macs_join_sends_no_key_and_keeps_its_way_back` holds to a real rotation.
///
/// CONTROLS, still dropped: a browse of ALab's service finds ALab's members, which never
/// refused this key (a phrase moves the Mac to ALab afresh), and members a person names with
/// `--at` serve a vault nothing here knows, so both joins look for a member as before. Not
/// dropped, a phrase that is not one reads `Refused`.
/// NEGATIVE CONTROL: without `join_tried`'s dropped check the lab join looks for a member
/// (the count), and without `phrase_join_as`'s a phrase that is not one reads `Refused`
/// ("Press Allow again" on the page).
#[test]
fn a_dropped_mac_asks_no_member_of_its_vault_and_no_phrase_reads_spent() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let mut j = me(root.path(), "j");
    let _ = let_in(&m1, &j, &|_| Follows::Runs);
    store_membership(&j.paths, "revoked");
    let Ok(client::Membership::Revoked { seq }) =
        client::status(&j.paths).map(|status| status.membership)
    else {
        panic!("j reads itself out");
    };
    let config = client::config(&j.paths).expect("config").expect("a vault");
    assert!(
        !client::alab_vault(&config),
        "this world's vault is a lab's"
    );
    let lab = "_alab-lab-e2._tcp";
    assert!(aterm_secrets::discover::lab_service(lab));
    assert!(browse_serves(lab, &config));
    assert!(!browse_serves(discover::MEMBER_SERVICE, &config));
    j.members = lab.to_owned();

    let reached = AtomicUsize::new(0);
    let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
        reached.fetch_add(1, Ordering::SeqCst);
        Err(aterm_secrets::Error::new("no member here"))
    };
    let reach = Reach {
        browse: &|| {
            reached.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        },
        dial: &dial,
        prove: &|_, _, _, _| {
            reached.fetch_add(1, Ordering::SeqCst);
            Proved::No
        },
        join_vault: &|paths| {
            reached.fetch_add(1, Ordering::SeqCst);
            no_vault(paths)
        },
        follow: &|_| Follows::Runs,
    };
    let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
    let not_one = SecretBytes::from("not a phrase".to_owned());
    let joined = |me: &Me, args: &JoinArgs| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let tried = join_tried(me, args, &reach, &mut out, &mut err, &|| Ok(()));
        (tried, String::from_utf8_lossy(&err).into_owned())
    };
    let named = JoinArgs {
        at: vec!["127.0.0.1:1".to_owned()],
        ..JoinArgs::default()
    };
    let alab = Me {
        members: discover::MEMBER_SERVICE.to_owned(),
        ..me(root.path(), "j")
    };

    let (words, tried, tried_as, phrased, unread, at_alab, at_named) =
        client::test_read_dropped(|| {
            let words = client::out_words(&j.paths, seq);
            let tried = joined(&j, &JoinArgs::default());
            let tried_as = join_as(
                &j,
                &JoinArgs::default(),
                &reach,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            let phrased = phrase_join_as(&j, &phrase, &reach);
            let unread = phrase_join_as(&j, &not_one, &reach);
            let before = reached.load(Ordering::SeqCst);
            let at_alab = (
                joined(&alab, &JoinArgs::default()).0,
                reached.load(Ordering::SeqCst) > before,
            );
            let before = reached.load(Ordering::SeqCst);
            let at_named = (
                joined(&j, &named).0,
                reached.load(Ordering::SeqCst) > before,
            );
            (words, tried, tried_as, phrased, unread, at_alab, at_named)
        });
    assert!(words.starts_with("dropped"), "{words}");
    assert_eq!(
        tried,
        (Tried::Dropped, format!("aterm members join: {words}\n"))
    );
    assert_eq!(tried_as, 1);
    assert_eq!(phrased, PhraseTry::Dropped(words.clone()));
    assert_eq!(unread, PhraseTry::Dropped(words));
    assert!(
        at_alab.0 != Tried::Dropped && at_alab.1,
        "ALab's members never refused this key: {at_alab:?}"
    );
    assert!(
        at_named.0 != Tried::Dropped && at_named.1,
        "members named with --at are asked: {at_named:?}"
    );
    assert!(matches!(
        client::status(&j.paths).map(|status| status.membership),
        Ok(client::Membership::Revoked { .. })
    ));

    // CONTROL: not dropped, the same Mac reads a phrase that is not one as refused.
    let refused = phrase_join_as(&j, &not_one, &reach);
    assert!(matches!(refused, PhraseTry::Refused(_)), "{refused:?}");
}

/// System State's phrase field joins as `aterm members join <phrase>` does, with no
/// terminal: the phrase the member shows lets this Mac in, and one it does not show is
/// refused with why, which the window logs.
///
/// NEGATIVE CONTROL: have `phrase_join_as` send `join_as` its phrase's first word alone:
/// the phrase the member shows lets nobody in.
#[test]
fn the_windows_phrase_join_lets_the_mac_in_or_says_why_not() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let (wrong, _) = with_member(&m1, |dial| {
        let ats = [M1.to_owned()];
        let (heard, _) = join::ask_all(dial, &j.joining().unwrap(), &ats);
        assert_eq!(heard.len(), 1);
        let shown = allow(&m1);
        // A phrase of the list's words that the member does not show.
        let other =
            std::iter::repeat_with(|| Phrase::generate().expect("a phrase").words().join(" "))
                .find(|words| *words != shown)
                .expect("a phrase the member does not show");
        let reach = Reach {
            browse: &|| Ok(ats.to_vec()),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        phrase_join_as(&j, &SecretBytes::from(other), &reach)
    });
    let PhraseTry::Refused(why) = wrong else {
        panic!("a phrase the member does not show lets nobody in: {wrong:?}");
    };
    assert!(!why.is_empty());
    assert!(client::config(&j.paths).expect("config").is_none());
    let (right, _) = with_member(&m1, |dial| {
        let ats = [M1.to_owned()];
        let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
        let shown = allow(&m1);
        let reach = Reach {
            browse: &|| Ok(ats.to_vec()),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        phrase_join_as(&j, &SecretBytes::from(format!("  {shown}\n")), &reach)
    });
    let PhraseTry::Joined(said) = right else {
        panic!("the phrase the member shows lets this Mac in: {right:?}");
    };
    assert!(
        client::config(&j.paths).expect("config").is_some(),
        "{said:?}"
    );
}

/// A try with the phrase that reached no member (none answered on this network: the
/// member's Mac asleep or waking, or no LAN broker to browse through) asked
/// nothing of the phrase, so it is not a refused one: the window keeps the words, and the
/// same phrase lets this Mac in on the next try. A phrase that is not three of the list's
/// words is refused before any member is looked for.
///
/// NEGATIVE CONTROL: map `Tried::NoMember` to `PhraseTry::Refused` in `phrase_join_as`: the
/// first try reads as a spent phrase and the window clears the words it still holds good.
#[test]
fn a_phrase_try_that_reached_no_member_is_not_a_refused_phrase() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let ((first, second), _) = with_member(&m1, |dial| {
        let ats = [M1.to_owned()];
        let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
        let shown = SecretBytes::from(allow(&m1));
        let nobody = Reach {
            browse: &|| Ok(Vec::new()),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        let first = phrase_join_as(&j, &shown, &nobody);
        let reach = Reach {
            browse: &|| Ok(ats.to_vec()),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        (first, phrase_join_as(&j, &shown, &reach))
    });
    assert!(
        matches!(first, PhraseTry::NotReached(ref why) if why.contains("no ALab member")),
        "a try that reached no member spent no phrase: {first:?}"
    );
    assert!(
        matches!(second, PhraseTry::Joined(_)),
        "the same phrase lets the Mac in on the next try: {second:?}"
    );
    let unbrowsable = Reach {
        browse: &|| {
            Err(aterm_secrets::Error::new(
                aterm_uds::lan::Unavailable::NotInstalled.to_string(),
            ))
        },
        dial: &|_| panic!("no member is dialed"),
        prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
        join_vault: &no_vault,
        follow: &|_| Follows::Runs,
    };
    let other = me(root.path(), "k");
    let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
    assert!(
        matches!(
            phrase_join_as(&other, &phrase, &unbrowsable),
            PhraseTry::NotReached(_)
        ),
        "no member could be looked for: not reached"
    );
    assert!(
        matches!(
            phrase_join_as(
                &other,
                &SecretBytes::from("not a phrase".to_owned()),
                &unbrowsable
            ),
            PhraseTry::Refused(_)
        ),
        "not three of the list's words: refused before any member is looked for"
    );
}

/// A join whose browse named a member, and whose own lookup of that member's name got no
/// answer, says macOS's Local Network question holds it (E3, 2026-11-05: a guest's
/// `members join <phrase>` waited out its 5 s on the member's `.local` name while macOS asked,
/// and was told only that no member answers). The same lookup of a name a person gave with
/// `--at`, or a member the browse named that refused the connection, says no such thing: that
/// Mac may be asleep or elsewhere.
///
/// NEGATIVE CONTROL: drop the [`LOOKUP_HELD`] line from `say_missed`: the browsed join's
/// words name no Local Network question, as the guest's did.
#[test]
fn a_member_the_browse_named_that_aterm_could_not_look_up_is_macos_local_network_question() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // An identity a release build made: a lab identity browses no members (A12).
    let j = me(root.path(), "j");
    assert_eq!(client::lab_mark(&j.paths).expect("its mark"), None);
    let named = "lab-mini.local:7486".to_owned();
    let unanswered = aterm_net::fleet_member::Unreached::Lookup(
        "lab-mini.local".to_owned(),
        Duration::from_secs(5),
    )
    .to_string();
    let said = |args: &JoinArgs, why: &str| {
        let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            Err(aterm_secrets::Error::new(why))
        };
        let reach = Reach {
            browse: &|| Ok(vec![named.clone()]),
            dial: &dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let tried = join_tried(&j, args, &reach, &mut out, &mut err, &|| Ok(()));
        assert_eq!(tried, Tried::NoMember);
        String::from_utf8(err).unwrap()
    };
    let phrase = || {
        Some(SecretBytes::from(
            Phrase::generate().expect("a phrase").words().join(" "),
        ))
    };
    let browsed = JoinArgs {
        phrase: phrase(),
        ..JoinArgs::default()
    };
    let err = said(&browsed, &unanswered);
    assert!(err.contains(&unanswered), "{err}");
    assert!(err.contains(LOOKUP_HELD), "the browse named it: {err}");
    let given = JoinArgs {
        phrase: phrase(),
        at: vec![named.clone()],
        ..JoinArgs::default()
    };
    let err = said(&given, &unanswered);
    assert!(
        err.contains(&unanswered) && !err.contains(LOOKUP_HELD),
        "{err}"
    );
    let err = said(&browsed, "nothing listens on 192.168.64.17:7486");
    assert!(!err.contains(LOOKUP_HELD), "a refused connect: {err}");
}

/// A12: a lab identity (one this test binary makes, as a debug build, a lab lane or a VM
/// makes one) that runs `aterm members join` with no `--at`, or the window's phrase join,
/// looks for no member, dials none and proves itself to none: a loopback member records no
/// connection. The same join from an identity a release build made browses and dials it: the
/// pair shows the refusal bites.
///
/// NEGATIVE CONTROL: drop the `refuse_lab_member_browse` return in `join_tried`: the lab
/// identity browses, and the loopback member hears it.
#[test]
fn a_lab_identity_asks_no_member_a_browse_finds() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback member");
    let at = listener.local_addr().expect("its address").to_string();
    let (rang, rings) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .map(|(stream, _)| stream);
            drop(stream);
            if rang.send(()).is_err() {
                break;
            }
        }
    });
    let (browsed, proved, vaulted) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let dial = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
        let stream = std::net::TcpStream::connect(at)?;
        stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
        Ok(Box::new(stream))
    };
    let reach = Reach {
        browse: &|| {
            browsed.fetch_add(1, Ordering::SeqCst);
            Ok(vec![at.clone()])
        },
        dial: &dial,
        prove: &|_, _, _, _| {
            proved.fetch_add(1, Ordering::SeqCst);
            Proved::No
        },
        join_vault: &|paths| {
            vaulted.fetch_add(1, Ordering::SeqCst);
            no_vault(paths)
        },
        follow: &|_| Follows::Runs,
    };
    // The identity this Mac's process makes, which `me` keeps.
    client::machine_identity(&Paths::for_home(&root.path().join("lab"))).expect("identity");
    let lab = me(root.path(), "lab");
    assert_eq!(
        client::lab_mark(&lab.paths).expect("its mark"),
        Some(aterm_secrets::LabMark::TestBinary)
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let tried = join_tried(
        &lab,
        &JoinArgs::default(),
        &reach,
        &mut out,
        &mut err,
        &|| Ok(()),
    );
    let said = String::from_utf8_lossy(&err).into_owned();
    assert_eq!(tried, Tried::Ended(1), "{said}");
    assert!(said.contains(client::LAB_IDENTITY_REFUSAL), "{said}");
    let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
    assert!(
        matches!(
            phrase_join_as(&lab, &phrase, &reach),
            PhraseTry::Refused(ref why) if why.contains(client::LAB_IDENTITY_REFUSAL)
        ),
        "the window's phrase join refuses too"
    );
    assert_eq!(
        browsed.load(Ordering::SeqCst),
        0,
        "no member was looked for"
    );
    assert_eq!(proved.load(Ordering::SeqCst), 0, "no member was proved to");
    assert_eq!(
        vaulted.load(Ordering::SeqCst),
        0,
        "ALab's vault was not joined"
    );
    assert!(rings.try_recv().is_err(), "no member was dialed");

    // The members a person names with `--at` are asked; the live ALab's vault is neither
    // joined nor left for the subscriber to retry.
    let named = JoinArgs {
        at: vec![at.clone()],
        ..JoinArgs::default()
    };
    join_tried(&lab, &named, &reach, &mut out, &mut err, &|| Ok(()));
    rings
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the member named is dialed");
    assert_eq!(browsed.load(Ordering::SeqCst), 0, "none was looked for");
    assert_eq!(
        vaulted.load(Ordering::SeqCst),
        0,
        "ALab's vault was not joined"
    );
    assert!(
        !lab.paths.dir.join("alab-join-pending").exists(),
        "no intent to join ALab's vault"
    );

    let release = me(root.path(), "release");
    assert_eq!(client::lab_mark(&release.paths).expect("its mark"), None);
    let mut err = Vec::new();
    // The member hangs up on the ask, and the join waits at the vault, as any does.
    join_tried(
        &release,
        &JoinArgs::default(),
        &reach,
        &mut out,
        &mut err,
        &|| Ok(()),
    );
    let said = String::from_utf8_lossy(&err).into_owned();
    assert!(!said.contains(client::LAB_IDENTITY_REFUSAL), "{said}");
    assert_eq!(
        browsed.load(Ordering::SeqCst),
        1,
        "a release identity browses"
    );
    assert_eq!(vaulted.load(Ordering::SeqCst), 1, "and joins ALab's vault");
    rings
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("and dials the member it found");
}

/// A Mac a member let in that runs `aterm members join` again (one still waiting on a lane
/// with no window, or install.sh's `--no-wait` on its next lane) says it was allowed and
/// makes sure its subscriber runs. It asks no member and does not say to press Allow: the
/// member that let it in, and each it told, closed its row and shows nothing to Allow.
///
/// NEGATIVE CONTROL: drop the `holds_allowed` return in `join_as`: `--no-wait` asks m1 again,
/// which answers Closed, and the Mac says "press Allow on any Mac in ALab" again.
#[test]
fn a_mac_let_in_already_says_so_and_asks_no_member_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let_in(&m1, &j, &|_| Follows::Absent);
    let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let follow = |home: &Path| {
        homes.lock().unwrap().push(home.to_path_buf());
        Follows::Runs
    };
    for args in [
        JoinArgs {
            no_wait: true,
            ..JoinArgs::default()
        },
        JoinArgs::default(),
    ] {
        let ((code, said), dials) = with_member(&m1, |dial| {
            let reach = Reach {
                browse: &|| Ok(vec![M1.to_owned()]),
                dial,
                prove: &|_, _, _, _| panic!("a Mac let in proves nothing again"),
                join_vault: &no_vault,
                follow: &follow,
            };
            joined(&j, &args, &reach)
        });
        assert_eq!(code, 0, "{said}");
        assert_eq!(dials, 0, "no member is asked again: {said}");
        assert!(!said.contains(join::LET_IN), "{said}");
        assert!(said.contains("allowed:"), "{said}");
        assert!(said.contains("finishes by itself"), "{said}");
    }
    assert_eq!(
        homes.lock().unwrap().len(),
        2,
        "each run makes sure it follows"
    );
}

/// What `join --no-wait` says lets this Mac in follows where it is: its first line ends with
/// the words for that place (`join::let_in`: automatic admission on the ALab LAN only), and
/// the exit code is 0 everywhere: being away is words, not a failure.
/// NEGATIVE CONTROL: `said_no_wait` printing `join::LET_IN` whatever `at` says gives a Mac off
/// the LAN the ALab LAN's words, and this test fails at the first of them, "Proves".
#[test]
fn join_no_wait_says_what_lets_this_mac_in_where_it_is() {
    for at in [
        join::JoinPlace::Home,
        join::JoinPlace::Proves,
        join::JoinPlace::Away,
        join::JoinPlace::Unknown,
    ] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = said_no_wait(
            at,
            Asked {
                heard: Ok(&[]),
                missed: &[],
                browsed: true,
            },
            Ok(Follows::Runs),
            &mut out,
            &mut err,
        );
        let said = String::from_utf8(out).unwrap();
        assert_eq!(code, 0, "{at:?}: {said}");
        let first = said.lines().next().unwrap_or_default();
        assert!(first.ends_with(join::let_in(at)), "{at:?}: {said}");
        assert_eq!(
            first.ends_with(join::LET_IN),
            at == join::JoinPlace::Home,
            "{at:?}: {said}"
        );
    }
}

/// A first fetch failure must leave a subscriber able to retry, without claiming membership.
#[test]
fn a_first_vault_fetch_failure_keeps_headless_bootstrap_running() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    // A Mac a release build set up: a lab identity sets up no join of ALab's vault.
    client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
    let followed = std::sync::atomic::AtomicBool::new(false);
    let follow = |_: &Path| {
        followed.store(true, std::sync::atomic::Ordering::Relaxed);
        Follows::Runs
    };
    let reach = Reach {
        browse: &|| Ok(Vec::new()),
        dial: &|_| panic!("no member dial"),
        prove: &|_, _, _, _| panic!("the vault half proves nothing"),
        join_vault: &|_| Err(aterm_secrets::Error::new("first fetch unavailable")),
        follow: &follow,
    };
    let waiting = wait_at_vault(&paths, &reach);
    assert!(waiting.is_ok());
    assert!(followed.load(std::sync::atomic::Ordering::Relaxed));
    assert!(client::config(&paths).unwrap().is_none());
    assert_eq!(
        client::status(&paths).unwrap().membership,
        client::Membership::Unconfigured
    );
    assert!(!paths.home.join(".secrets").exists());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        said_no_wait(
            join::JoinPlace::Home,
            Asked {
                heard: Ok(&[]),
                missed: &[],
                browsed: true,
            },
            waiting,
            &mut out,
            &mut err
        ),
        0
    );
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("retries setup automatically"), "{out}");
    assert!(
        !out.contains(join::LET_IN),
        "no membership or queued vault request yet: {out}"
    );
    assert!(
        out.contains("no ALab member answers on this network yet"),
        "the members' half is said too: {out}"
    );
}

/// A vault setup that has not finished still says how the members' half went: a browse that
/// could not run names why (no LAN broker to browse through), and a member that heard is
/// said as such.
/// NEGATIVE CONTROL: the arm that printed only the vault's line (round 8's vault-phrase row
/// read it and could not tell a browse that could not run from an empty one) fails each
/// assertion.
#[test]
fn an_unfinished_vault_setup_still_says_how_the_members_ask_went() {
    let bootstrapping = || {
        Ok(Follows::Bootstrapping(
            Box::new(Follows::Runs),
            "first fetch unavailable".to_owned(),
        ))
    };
    let refused = format!(
        "browsing _alab._tcp: {}",
        aterm_uds::lan::Unavailable::NotInstalled
    );
    let said = |heard: Result<&[Heard], String>| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = said_no_wait(
            join::JoinPlace::Home,
            Asked {
                heard,
                missed: &[],
                browsed: true,
            },
            bootstrapping(),
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0);
        String::from_utf8(out).unwrap()
    };
    let out = said(Err(refused.clone()));
    assert!(
        out.starts_with("ALab vault setup has not finished"),
        "{out}"
    );
    assert!(
        out.contains(&format!("{refused}; no ALab member was asked")),
        "{out}"
    );
    let out = said(Ok(&[]));
    assert!(
        out.contains("no ALab member answers on this network yet"),
        "{out}"
    );
    assert!(!out.contains("LAN broker"), "{out}");
}

#[test]
fn a_headless_bootstrap_keeps_sharing_off_before_fetch_or_follow() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    std::fs::create_dir_all(root.path().join(".config/aterm")).unwrap();
    aterm_uds::atomic_file::write(
        &root.path().join(".config/aterm/aterm.toml"),
        b"[swarm]\nenabled = false\n",
        0o600,
    )
    .unwrap();
    let reach = Reach {
        browse: &|| panic!("no browse"),
        dial: &|_| panic!("no dial"),
        prove: &|_, _, _, _| panic!("the vault half proves nothing"),
        join_vault: &|_| panic!("sharing off: no fetch"),
        follow: &|_| panic!("sharing off: no follower"),
    };
    // A Mac a release build set up, so sharing alone stops it (a lab identity would be
    // refused first).
    client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
    let refused = wait_at_vault(&paths, &reach).expect_err("sharing off");
    assert!(refused.contains("sharing is off"), "{refused}");
    assert!(client::config(&paths).unwrap().is_none());
}

/// install.sh's windowless `members join --no-wait` sets the Mac up at the ALab vault (its
/// configuration, Pending, a queued request) and starts its vault subscriber, as System
/// State's Secrets stage does, whether or not a member answers. That subscriber asks ALab's
/// members again (about half a minute later, then after 1, 2, 4 and 8 minutes, then every
/// 15), so a
/// member asleep, away or
/// restarting its window when the lane asked shows the row too; and with no member
/// answering, the lane says so rather than nothing.
///
/// NEGATIVE CONTROL: never start `wait_at_vault` (`waits` always `None`): neither Mac joins
/// the vault or gets a subscriber, and the one no member answers fails with nothing on
/// stdout, the line install.sh shows.
#[test]
fn default_and_install_joins_wait_at_the_vault_without_a_phrase_prompt() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    for (name, members) in [("j", vec![M1.to_owned()]), ("k", Vec::new())] {
        let mac = me(root.path(), name);
        let joins: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let join_vault = |paths: &Paths| -> aterm_secrets::Result<()> {
            joins.lock().unwrap().push(paths.home.clone());
            Ok(())
        };
        let follow = |home: &Path| {
            homes.lock().unwrap().push(home.to_path_buf());
            Follows::Runs
        };
        let ((code, said), _) = with_member(&m1, |dial| {
            let reach = Reach {
                browse: &|| Ok(members.clone()),
                dial,
                prove: &|_, _, _, _| Proved::No,
                join_vault: &join_vault,
                follow: &follow,
            };
            let args = JoinArgs {
                no_wait: name == "k",
                ..JoinArgs::default()
            };
            joined(&mac, &args, &reach)
        });
        assert_eq!(code, 0, "{name}: {said}");
        let home = [mac.paths.home.clone()];
        assert_eq!(
            *joins.lock().unwrap(),
            home,
            "{name} joins the vault: {said}"
        );
        assert_eq!(*homes.lock().unwrap(), home, "{name} follows it: {said}");
        let first = said.lines().next().unwrap_or_default();
        assert!(first.ends_with(join::LET_IN), "{name}: {said}");
        if members.is_empty() {
            assert!(
                first.contains(
                    "asks them again (about half a minute later, then after 1, 2, 4 and 8 \
                     minutes, then every 15)"
                ),
                "{said}"
            );
        }
    }
    let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
    assert_eq!(rows.len(), 1, "j's request waits on m1");
}

/// A phraseless `join` (install.sh's `members join --no-wait` included) proves this Mac to
/// the members it found before it asks any plainly, as its subscriber does: where a member
/// let it in by its proof, or may have signed it in, no member is asked and no row waits for
/// a person's Allow; only where none did is each asked. The vault half runs either way, and
/// a line that waits says what lets this Mac in where it is (`join::let_in`), on the ALab
/// LAN and off it alike.
///
/// The proof dials no member after [`PROVE_WITHIN`] from its start, so install.sh's lane
/// ends within its own bound.
///
/// NEGATIVE CONTROLS: call `join::ask_all` whatever the proof came to: m1 shows a row for
/// the Mac a member let in ("j"). Prove before a phrase join too: the phrase tests' `prove`
/// panics ("a phrase join proves nothing"). Give the proof no deadline, or a later one: the
/// `until` assertion fails. Print `join::LET_IN` where a member may have signed this Mac in,
/// whatever its place: the Mac off the LAN that holds the fleet's book ("k-proves") is told
/// it waits for automatic admission, and its first line fails.
#[test]
fn a_phraseless_join_proves_first_and_asks_only_where_no_member_let_it_in() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    for ((place, at), (name, outcome, first, asks)) in [
        (join::JoinPlace::Home, "home"),
        (join::JoinPlace::Proves, "proves"),
    ]
    .into_iter()
    .flat_map(|(place, at)| {
        [
            ("j", Proved::In, PROVED_IN, false),
            ("k", Proved::SignedMaybe, join::let_in(place), false),
            ("l", Proved::No, join::let_in(place), true),
        ]
        .map(|case| ((place, at), case))
    }) {
        // A machine name (lower case, `aterm_secrets::text::valid_machine_name`): the place's
        // word after the case's.
        let name = format!("{name}-{at}");
        let m1 = member(root.path(), &format!("m1-{name}"));
        let mac = Me {
            place,
            ..me(root.path(), &name)
        };
        let proved_at: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
        let outcome = Mutex::new(Some(outcome));
        let before = std::time::Instant::now();
        let prove = |_: &Me, ats: &[String], until: std::time::Instant, _: &Current<'_>| {
            if ats.is_empty() {
                return Proved::No; // This fixture has no cached fleet route.
            }
            assert!(
                until >= before + PROVE_WITHIN && until <= std::time::Instant::now() + PROVE_WITHIN,
                "the proof dials no member after PROVE_WITHIN from its start"
            );
            proved_at.lock().unwrap().push(ats.to_vec());
            outcome.lock().unwrap().take().expect("one proof per join")
        };
        let ((code, said), dials) = with_member(&m1, |dial| {
            let reach = Reach {
                browse: &|| Ok(vec![M1.to_owned()]),
                dial,
                prove: &prove,
                join_vault: &|_| Ok(()),
                follow: &|_| Follows::Runs,
            };
            let args = JoinArgs {
                no_wait: true,
                ..JoinArgs::default()
            };
            joined(&mac, &args, &reach)
        });
        assert_eq!(code, 0, "{name}: {said}");
        assert_eq!(
            *proved_at.lock().unwrap(),
            [vec![M1.to_owned()]],
            "{name} proves itself to the member it found first: {said}"
        );
        assert_eq!(said.lines().next(), Some(first), "{name}: {said}");
        let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
        assert_eq!(rows.len(), usize::from(asks), "{name}: {said}");
        assert_eq!(dials, usize::from(asks), "{name}: {said}");
    }
}

/// A phraseless `join` on a Mac whose person turned sharing off (`[swarm] enabled = false`)
/// proves this Mac to no member, so no member's proof writes its vault or ~/.secrets, as the
/// vault half and the subscriber keep that switch too; it still asks the members plainly,
/// which writes nothing here.
///
/// NEGATIVE CONTROL: prove whatever `[swarm]` says (`proves` without its `enabled_in_home`
/// read): `prove` panics.
#[test]
fn a_phraseless_join_with_sharing_off_proves_nothing_and_writes_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let mac = me(root.path(), "j");
    let config = mac.paths.home.join(".config/aterm");
    std::fs::create_dir_all(&config).unwrap();
    crate::test_config::write(config.join("aterm.toml"), "[swarm]\nenabled = false\n").unwrap();
    let ((code, said), dials) = with_member(&m1, |dial| {
        let reach = Reach {
            browse: &|| Ok(vec![M1.to_owned()]),
            dial,
            prove: &|_, _, _, _| panic!("sharing off: no proof"),
            join_vault: &|_| panic!("sharing off: no fetch"),
            follow: &|_| panic!("sharing off: no follower"),
        };
        let args = JoinArgs {
            no_wait: true,
            ..JoinArgs::default()
        };
        joined(&mac, &args, &reach)
    });
    assert_eq!(code, 0, "{said}");
    assert_eq!(dials, 1, "the plain ask still goes: {said}");
    let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
    assert_eq!(rows.len(), 1, "{said}");
    assert!(client::config(&mac.paths).unwrap().is_none(), "{said}");
    assert!(!mac.paths.home.join(".secrets").exists(), "{said}");
}

/// A proof whose time ran out ([`crate::vault_join::dial_until`], as [`proved_back`] dials
/// by) dials no further member and reads as one no member let in ([`Proved::No`], never
/// [`Proved::SignedMaybe`]: nothing went to that member), so the plain ask follows; before
/// then each member is dialed within [`DIAL_WITHIN`] and what is left, and with no deadline
/// within [`DIAL_WITHIN`].
///
/// NEGATIVE CONTROL: let `dial_until` dial past `until`: the members are dialed after the
/// deadline, and the first assertion fails.
#[test]
fn a_proof_whose_time_ran_out_dials_no_further_member_and_leaves_the_plain_ask() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mac = me(root.path(), "j");
    let ats = ["m2:7100".to_owned(), M1.to_owned()];
    let sign_node = |_: &[u8]| [0u8; 64];
    let dialed: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());
    let reach = |at: &str, within: Duration| -> aterm_secrets::Result<Box<dyn Stream>> {
        dialed.lock().unwrap().push((at.to_owned(), within));
        Err(aterm_secrets::Error::new("connection refused"))
    };
    let proved = |until: Option<std::time::Instant>| {
        dialed.lock().unwrap().clear();
        let dial = crate::vault_join::dial_until(until, DIAL_WITHIN, &reach);
        let tried = join::prove_first(&mac.paths, &dial, &mac.joining().unwrap(), &sign_node, &ats);
        (proof_outcome(tried), dialed.lock().unwrap().clone())
    };

    let (outcome, dialed_then) = proved(Some(std::time::Instant::now()));
    assert!(dialed_then.is_empty(), "{dialed_then:?}");
    assert!(matches!(outcome, Proved::No), "{outcome:?}");

    let (outcome, dialed_then) = proved(Some(std::time::Instant::now() + Duration::from_secs(60)));
    assert!(matches!(outcome, Proved::No), "{outcome:?}");
    assert_eq!(
        dialed_then
            .iter()
            .map(|(at, _)| at.as_str())
            .collect::<Vec<_>>(),
        [M1, "m2:7100"],
        "one after another, in address order"
    );
    assert!(dialed_then.iter().all(|(_, within)| *within <= DIAL_WITHIN));

    let (_, dialed_then) = proved(None);
    assert!(
        dialed_then.iter().all(|(_, within)| *within == DIAL_WITHIN),
        "{dialed_then:?}"
    );
}

#[test]
fn a_pending_default_vault_requires_authority_after_restart_without_a_native_pin() {
    use std::io::{BufRead as _, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let mac = me(root.path(), "pending");
    let source = mac.paths.dir.join("source.toml");
    let saved = format!(
        "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'pending'\n",
        aterm_secrets::ALAB_DEFAULT_URL,
        aterm_secrets::ALAB_SIGNER,
    );
    aterm_secrets::fsx::write_atomic(&source, saved.as_bytes(), 0o600).unwrap();
    assert_eq!(
        client::status(&mac.paths).unwrap().membership,
        client::Membership::Pending
    );
    let paths = Paths::for_home(&mac.paths.home);
    assert!(!join::pending_proof(&paths).unwrap());
    let expected = client::config(&paths).unwrap();
    assert!(expected.as_ref().is_some_and(client::alab_vault));
    let device = Device::open(&paths).unwrap().unwrap();
    let joining = Joining {
        device: &device,
        records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
        node: mac.node,
        name: mac.name.clone(),
        model: mac.model.clone(),
    };
    let proving = join::ProofLock::take(&paths, None).unwrap().unwrap();
    let current = || Ok(());
    let scope = ProofScope::read(&paths, expected.as_ref(), true, &current, &proving).unwrap();
    let captured = Mutex::new(Vec::new());
    let tried = std::thread::scope(|threads| {
        let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            let (mut member, joiner) = UnixStream::pair()?;
            for stream in [&member, &joiner] {
                stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            }
            let captured = &captured;
            threads.spawn(move || {
                    // A legacy member advertises a syntactically valid greeting, then
                    // refuses this extension before issuing any possession challenge.
                    writeln!(member,
                        "ALAB member=1 key={} network={} authority={} genesis={} name=legacy allow=1 invite=0 challenge={}",
                        "09".repeat(32), "01".repeat(16), "02".repeat(32),
                        "03".repeat(32), "04".repeat(16),
                    ).unwrap();
                    let mut line = String::new();
                    std::io::BufReader::new(&mut member).read_line(&mut line).unwrap();
                    captured.lock().unwrap().push(line);
                    member.write_all(b"NO why=unsupported-authority\n").unwrap();
                });
            Ok(Box::new(joiner))
        };
        scope.direct(&paths, &dial, &joining, &|_| [0; 64], &[M1.to_owned()])
    });
    assert!(matches!(proof_outcome(tried), Proved::No));
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.starts_with("prove "));
    let nonce = request
        .split_ascii_whitespace()
        .find_map(|field| field.strip_prefix("authority="))
        .expect("the production selector must use the admitted witness path");
    assert_eq!(nonce.len(), 64);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(std::fs::read(&source).unwrap(), saved.as_bytes());
    assert!(!join::pending_proof(&paths).unwrap());
    assert_eq!(
        client::status(&paths).unwrap().membership,
        client::Membership::Pending
    );
}

/// A member whose vault state a pass holds (an admission publishing on the admin Mac, an
/// apply) waits for it inside the `ask` exchange, which the fleet listener's handshake
/// watchdog cuts at `HANDSHAKE_WITHIN`. The wait leaves most of that bound to the TLS
/// handshake, CPace and the possession proof, so the member's refusal ("Allow the new Mac
/// again once it ends") reaches the new Mac rather than a cut socket, and a relayed bundle is
/// never written into a socket the new Mac has left (the row closed and the `Allowed` posted
/// for a Mac that wrote nothing).
///
/// NEGATIVE CONTROL: put `HANDOVER_LOCK_WAIT` back at 60 s: the wait outlasts the bound.
#[test]
fn a_busy_members_handover_wait_ends_well_inside_the_ask_exchanges_bound() {
    let (wait, bound) = (
        client::HANDOVER_LOCK_WAIT,
        aterm_net::fleet::HANDSHAKE_WITHIN,
    );
    assert!(wait * 3 <= bound, "{wait:?} of the exchange's {bound:?}");
}

/// On a Mac that cannot browse for members (off macOS, where the browse says
/// `discover::NO_BROWSE`, or one with no LAN broker to browse through), install.sh's
/// `members join --no-wait` still sets the Mac up at the ALab vault with its subscriber, as
/// `secrets join --no-wait` did on that lane before F10, and says why it asked no member on
/// stdout, the line install.sh shows, rather than nothing; it does not promise that its
/// subscriber asks the members, which browses the same way.
///
/// NEGATIVE CONTROL: on a browse error return 1 with the error on stderr before
/// `said_no_wait`, as before: stdout is empty, so install.sh shows nothing.
#[test]
fn a_mac_that_cannot_browse_still_waits_at_the_vault_and_says_why_it_asked_no_member() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let k = me(root.path(), "k");
    let joins: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let join_vault = |paths: &Paths| -> aterm_secrets::Result<()> {
        joins.lock().unwrap().push(paths.home.clone());
        Ok(())
    };
    let follow = |home: &Path| {
        homes.lock().unwrap().push(home.to_path_buf());
        Follows::Runs
    };
    let unreached = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
        Err(aterm_secrets::Error::new("no member is dialed"))
    };
    let reach = Reach {
        browse: &|| {
            Err(aterm_secrets::Error::new(
                aterm_secrets::discover::NO_BROWSE,
            ))
        },
        dial: &unreached,
        prove: &|_, ats, _, current| {
            current().unwrap();
            assert!(ats.is_empty(), "this fixture has no cached fleet route");
            Proved::No
        },
        join_vault: &join_vault,
        follow: &follow,
    };
    let args = JoinArgs {
        no_wait: true,
        ..JoinArgs::default()
    };
    let (code, out, err) = joined_apart(&k, &args, &reach);
    assert_eq!(code, 0, "{out}{err}");
    let home = [k.paths.home.clone()];
    assert_eq!(
        *joins.lock().unwrap(),
        home,
        "it joins the vault: {out}{err}"
    );
    assert_eq!(*homes.lock().unwrap(), home, "it follows it: {out}{err}");
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.contains(aterm_secrets::discover::NO_BROWSE),
        "{out}{err}"
    );
    assert!(!out.contains("asks them again"), "{out}");
}

/// A new Mac's `members join` mints its node key whether install.sh left the state
/// directory open to others (0755, made with the package store under it) or there is none
/// yet, and leaves it 0700, as `fleet init` and `fleet join` do.
/// NEGATIVE CONTROL: drop `ensure_private_dir` from [`Me::at_state`] and both fail with
/// "this Mac's node key: fleet-peers's lock: the directory …".
#[cfg(unix)]
#[test]
fn a_new_macs_join_mints_its_node_key_where_install_sh_left_the_state_open_or_none() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let open = tmp.path().join("open");
    std::fs::create_dir(&open).expect("open state directory");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("0755");
    let none = tmp.path().join("none");
    for (home, state) in [("a", &open), ("b", &none)] {
        let home = tmp.path().join(home);
        std::fs::create_dir(&home).expect("home");
        let me = Me::at_state(Paths::for_home(&home), state).expect("the joining Mac");
        let kept = aterm_net::fleet_member::read_node_identity(state)
            .expect("read the node")
            .map(|node| *node.key().bytes());
        assert_eq!(kept, Some(me.node), "{}", state.display());
        let mode = std::fs::metadata(state)
            .expect("state directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{}", state.display());
    }
}

#[test]
fn join_takes_a_phrase_as_one_word_or_three_and_no_wait_alone() {
    let parse = |args: &[&str]| {
        JoinArgs::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    };
    assert_eq!(
        parse(&["maple", "river", "ocean"])
            .unwrap()
            .phrase
            .as_ref()
            .and_then(SecretBytes::as_str),
        Some("maple river ocean")
    );
    assert_eq!(
        parse(&["maple river ocean", "--member", "m7"]).unwrap(),
        JoinArgs {
            phrase: Some(SecretBytes::from("maple river ocean".to_owned())),
            member: Some("m7".into()),
            ..JoinArgs::default()
        }
    );
    assert_eq!(
        parse(&[
            "--no-wait",
            "--at",
            "m22.local",
            "--at",
            "10.0.0.2:7200",
            "--at",
            "::1",
            "--at",
            "[::1]:7200",
        ])
        .unwrap(),
        JoinArgs {
            no_wait: true,
            at: vec![
                "m22.local:7100".into(),
                "10.0.0.2:7200".into(),
                "[::1]:7100".into(),
                "[::1]:7200".into(),
            ],
            ..JoinArgs::default()
        }
    );
    assert!(parse(&["--at", "m22.local:0"]).is_err());
    assert!(parse(&["--at", "m22.local:abc"]).is_err());
    assert!(dial("127.0.0.1:+7100").is_err());
    assert!(dial("127.0.0.1:0").is_err());
    assert!(dial("[not-v6]:7100").is_err());
    assert!(parse(&["--no-wait", "maple", "river", "ocean"]).is_err());
    assert!(parse(&["--member"]).is_err());
    assert!(parse(&["--wait"]).is_err());
}

/// `members undo` signs nothing: it lists the Undo rows and says a person's press of one
/// signs this Mac's share; on a Mac no removal keeps out it says so rather than refusing as
/// a signing verb would (F16's review).
///
/// NEGATIVE CONTROL: have `undo_said` call `client::undo_removals(paths, who)` as before:
/// the Mac with no vault refuses ("…no membership log…") and the first assert fails.
#[test]
fn members_undo_lists_the_rows_and_signs_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (paths, _device) = mac(root.path(), "m9");
    assert_eq!(
        undo_said(&paths, Some("m7")),
        Ok(vec![
            "no removal by m7 keeps this Mac out with an Undo to sign".to_owned()
        ])
    );
    let offer = |label: &str, signed: bool| client::UndoOffer {
        remover: [7; 48],
        label: label.to_owned(),
        code: format!("{label}-code"),
        others: 4,
        signed,
        needed: 3,
    };
    let offers = [offer("m7", false), offer("m12", true)];
    let lines = undo_lines(&offers, None);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("Undo m7's removals (4 other Macs"),
        "{lines:?}"
    );
    assert!(
        lines[0].contains("press the row on this Mac's Machines view"),
        "{lines:?}"
    );
    assert!(lines[1].contains("this Mac signed its share"), "{lines:?}");
    assert_eq!(undo_lines(&offers, Some("M12-CODE")).len(), 1);
}

/// The kinds of the records `paths`' log holds that `before` does not, in order of kind.
fn signed_since(
    paths: &Paths,
    before: &std::collections::BTreeSet<aterm_secrets::membership_log::RecordId>,
) -> Vec<aterm_secrets::membership_log::Kind> {
    let store = client::load_records(paths).expect("records");
    let mut kinds: Vec<_> = store
        .log()
        .expect("a log")
        .records()
        .filter(|record| !before.contains(record.id()))
        .map(aterm_secrets::membership_log::Record::kind)
        .collect();
    kinds.sort_unstable();
    kinds
}

/// `aterm fleet remove` and `aterm members remove` are one verb (one-mesh §4.16): each front
/// parses to the same removal, which signs the same records into a like vault and says the
/// same lines, the log name and node id it acted on among them.
///
/// NEGATIVE CONTROL: in `parse_lab`, set `stolen` on the fleet front's `remove`: its removal
/// says the `NEEDS YOU:` line and the two no longer compare equal.
#[test]
fn fleet_remove_and_members_remove_sign_the_same_records() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let words = ["remove".to_owned(), "member".to_owned()];
    let mut removals = Vec::new();
    for (front, dir) in [(Front::Members, "members"), (Front::Fleet, "fleet")] {
        let Ok(LabVerb::Remove(remove)) = parse_lab(front, &words) else {
            panic!("{} remove member is a removal", front.name());
        };
        let fixture =
            client::test_commons_excluded(&root.path().join(dir)).expect("a vault and member");
        let before = client::load_records(&fixture.admin)
            .expect("records")
            .inventory();
        // `member` is no node id or registry label: the removal resolves it by word.
        let said =
            remove_said(&fixture.admin, &remove, &|_| Ok(None)).expect("the admin removes it");
        let signed = signed_since(&fixture.admin, &before);
        fixture.server.stop();
        removals.push((remove, said, signed));
    }
    assert_eq!(removals[0], removals[1]);
    let (_, said, signed) = &removals[0];
    assert!(
        signed.contains(&aterm_secrets::membership_log::Kind::Remove),
        "{signed:?}"
    );
    assert!(
        said.iter()
            .any(|line| line.ends_with("was member, node - (it bound no node key)")),
        "{said:?}"
    );
}

/// Both front ends read the same lab verbs; `aterm fleet` says `admit` where `aterm members`
/// says `allow`, since `aterm fleet allow` is this Mac's own drive grant.
#[test]
fn both_fronts_read_the_same_lab_verbs() {
    let words = |text: &str| -> Vec<String> { text.split(' ').map(str::to_owned).collect() };
    for (members, fleet) in [
        ("requests", "requests"),
        ("invite", "invite"),
        ("ignore m12", "ignore m12"),
        ("allow m12", "admit m12"),
        ("allow --again m12", "admit --again m12"),
        ("remove m12", "remove m12"),
        ("remove --stolen m12", "remove --stolen m12"),
    ] {
        let (m, f) = (
            parse_lab(Front::Members, &words(members)),
            parse_lab(Front::Fleet, &words(fleet)),
        );
        assert!(m.is_ok(), "{members}: {m:?}");
        assert_eq!(m, f, "{members} / {fleet}");
    }
    assert_eq!(
        parse_lab(Front::Fleet, &words("allow m12")),
        Err(LabRefusal::Usage)
    );
    assert_eq!(
        parse_lab(Front::Members, &words("admit m12")),
        Err(LabRefusal::Usage)
    );
    assert_eq!(
        parse_lab(Front::Fleet, &words("requests now")),
        Err(LabRefusal::Usage)
    );
    assert!(matches!(
        parse_lab(Front::Fleet, &words("remove")),
        Err(LabRefusal::Said(_))
    ));
}

/// `members remove` names one Mac, with `--stolen` anywhere; no Mac, two, or another flag
/// is refused before anything is read.
#[test]
fn remove_names_one_mac() {
    let parse = |args: &[&str]| {
        RemoveArgs::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    };
    assert_eq!(
        parse(&["m12"]),
        Ok(RemoveArgs {
            who: "m12".into(),
            stolen: false
        })
    );
    assert_eq!(
        parse(&["--stolen", "m12"]),
        Ok(RemoveArgs {
            who: "m12".into(),
            stolen: true
        })
    );
    assert!(parse(&[]).is_err());
    assert!(parse(&["m12", "m13"]).is_err());
    assert!(parse(&["m12", "--force"]).is_err());

    let req =
        |args: &[&str]| window_request(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(req(&["invite"]), Ok("members invite\n".to_owned()));
    assert_eq!(req(&["allow", "m12"]), Ok("members allow m12\n".to_owned()));
    assert_eq!(
        req(&["allow", "--again", "m12"]),
        Ok("members allow --again m12\n".to_owned())
    );
    assert_eq!(
        req(&["ignore", "m12"]),
        Ok("members ignore m12\n".to_owned())
    );
    for injected in [
        "",
        "-h",
        "--again",
        "--again victim",
        "m12\n@s-1 send bad",
        "m12\r@s-1 send bad",
        "m12 extra",
    ] {
        assert!(req(&["allow", injected]).is_err(), "{injected:?}");
        assert!(req(&["ignore", injected]).is_err(), "{injected:?}");
        assert!(
            req(&["allow", "--again", injected]).is_err(),
            "{injected:?}"
        );
        assert_eq!(
            main_entry(vec!["allow".into(), injected.into()]),
            ExitCode::from(2),
            "{injected:?}"
        );
    }
}

/// A typed phrase that no answering member shows (the member that showed it is asleep or on
/// another network, while another member of ALab answers here, its person having pressed no
/// Allow for this Mac) went to no member, so it is not spent: the page keeps the words and
/// says the phrase is still good, never that it let nobody in.
///
/// NEGATIVE CONTROL: map `Tried::Unsent` to `PhraseTry::Refused` in `phrase_join_as`.
#[test]
fn a_phrase_no_answering_member_shows_is_not_refused() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // m2 answers here, but its person pressed no Allow for this Mac.
    let m2 = member(root.path(), "m2");
    let j = me(root.path(), "j");
    // The words another member (asleep now) showed, which the person typed.
    let typed = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
    let (tried, _) = with_member(&m2, |dial| {
        let reach = Reach {
            browse: &|| Ok(vec![M1.to_owned()]),
            dial,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        phrase_join_as(&j, &typed, &reach)
    });
    assert!(
        matches!(tried, PhraseTry::NotReached(_)),
        "no member heard shows a phrase for this Mac, so nothing was asked of the typed one: \
         {tried:?}"
    );
}

/// A member that answered the ask and then could not be dialed for the run (its Mac went to
/// sleep, the network moved) was never sent the phrase: not a refused one.
///
/// NEGATIVE CONTROL: have `join::run_phrase` call a dial failure `PhraseFailed::Sent`.
#[test]
fn a_phrase_never_sent_to_the_member_is_not_refused() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m1 = member(root.path(), "m1");
    let j = me(root.path(), "j");
    let (tried, _) = with_member(&m1, |dial| {
        let ats = [M1.to_owned()];
        let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
        let shown = SecretBytes::from(allow(&m1));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        // The ask reaches the member; every dial after it finds the member gone.
        let flaky = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                dial(at)
            } else {
                Err(aterm_secrets::Error::new("host is down"))
            }
        };
        let reach = Reach {
            browse: &|| Ok(ats.to_vec()),
            dial: &flaky,
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        phrase_join_as(&j, &shown, &reach)
    });
    assert!(
        matches!(tried, PhraseTry::NotReached(_)),
        "the run never reached the member, so the phrase was never tried: {tried:?}"
    );
}

/// A proof that may have signed (its possession answer went to a member) ends the round: no
/// plain ask follows, so no member shows a "New Mac" entry for a Mac it may have just let in.
/// A proof no member took and none may have signed is followed by the plain ask.
///
/// NEGATIVE CONTROL: `proof_outcome` reading a missed proof that may have signed as
/// `Proved::No` (its `signed_maybe` arm cut): the plain ask runs and the first assert fails
/// (left `Ok(3)`, right `Ok(1)`).
#[test]
fn a_proof_that_may_have_signed_sends_no_plain_ask() {
    let missed = |signed_maybe| join::Missed {
        refused: vec![("m22:7".to_owned(), "the stream ended".to_owned())],
        signed_maybe,
    };
    let asked = std::cell::Cell::new(0);
    let ask = || {
        asked.set(asked.get() + 1);
        Ok(3)
    };
    assert_eq!(
        ask_after_proof(proof_outcome(Err(missed(true))), ask),
        Ok(1)
    );
    assert_eq!(asked.get(), 0, "no ask after a proof that may have signed");
    assert_eq!(
        ask_after_proof(proof_outcome(Err(missed(false))), ask),
        Ok(3)
    );
    assert_eq!(asked.get(), 1, "a plain ask after a proof nobody took");
}

#[test]
fn direct_admission_skips_bonjour_only_after_proof_and_continues_unproved_hints() {
    for outcome in [Proved::In, Proved::SignedMaybe] {
        let mut outcome = Some(outcome);
        assert_eq!(
            ask_discovered(
                vec!["192.0.2.1:7100".into()],
                std::time::Instant::now() + PROVE_WITHIN,
                |_, _| outcome.take().unwrap(),
                || panic!("a direct proof needs no browse"),
                &|| Ok(()),
                |_| panic!("an admission that may have signed sends no ask")
            ),
            Ok(1)
        );
    }
    let attempts = std::cell::RefCell::new(Vec::new());
    let deadlines = std::cell::RefCell::new(Vec::new());
    let until = std::time::Instant::now() + PROVE_WITHIN;
    assert_eq!(
        ask_discovered(
            vec!["192.0.2.1:7100".into()],
            until,
            |ats, deadline| {
                attempts.borrow_mut().push(ats.to_vec());
                deadlines.borrow_mut().push(deadline);
                if ats == ["member.local:7100"] {
                    Proved::In
                } else {
                    Proved::No
                }
            },
            || Ok(vec![
                "192.0.2.1:7100".into(),
                "member.local:7100".into(),
                "member.local:7100".into()
            ]),
            &|| Ok(()),
            |_| panic!("the continued discovery proved a member")
        ),
        Ok(1)
    );
    let deadlines = deadlines.borrow();
    assert!(deadlines[0] < deadlines[1]);
    assert_eq!(
        deadlines[1], until,
        "later hints retain only the original final deadline"
    );
    assert_eq!(
        *attempts.borrow(),
        [
            vec!["192.0.2.1:7100".to_owned()],
            vec!["member.local:7100".to_owned()]
        ]
    );
}

#[test]
fn an_elapsed_round_never_renews_the_retained_proof_deadline() {
    let until = std::time::Instant::now() - Duration::from_secs(1);
    let attempts = std::cell::Cell::new(0);
    let (outcome, found) = staged_member_proof(
        Vec::new(),
        until,
        |ats, deadline| {
            assert!(ats.is_empty());
            assert!(
                deadline <= until,
                "discovery cannot renew an elapsed proof round"
            );
            attempts.set(attempts.get() + 1);
            Proved::No
        },
        || Ok(Vec::new()),
        &|| Ok(()),
    )
    .unwrap();
    assert!(matches!(outcome, Proved::No));
    assert!(found.is_empty());
    assert_eq!(
        attempts.get(),
        1,
        "an empty DNS set never repeats retained proof"
    );
}

#[test]
fn failed_direct_proofs_keep_all_reachable_members_for_the_plain_request() {
    assert_eq!(
        ask_discovered(
            vec!["192.0.2.1:7100".into()],
            std::time::Instant::now() + PROVE_WITHIN,
            |_, _| Proved::No,
            || Ok(vec!["member.local:7100".into()]),
            &|| Ok(()),
            |ats| {
                assert_eq!(ats, ["192.0.2.1:7100", "member.local:7100"]);
                Ok(2)
            }
        ),
        Ok(2)
    );
}

/// A member's `list` line says who let it in, the UTC day its entry was signed, how the member
/// knew to and whether the log holds a fleet node key for its seat: together they tell a seat a
/// person allowed (`how=phrase`), one a member let in by itself with a node key (a proof from
/// the home ALab LAN, `how=manual node=yes`) and a writer successor (`how=manual node=no`) apart,
/// and date each. An entry whose record this Mac's log does not hold prints no day; a removed
/// Mac's line is as before.
///
/// NEGATIVE CONTROL: `signed_day` counting a day's first millisecond as the day before
/// (`(ms - 1) / 86 400 000`) prints `on=2026-11-12` for the second line's 00:00:00 stamp; `listed_line` not reading `node` prints `node=no` for the
/// first; reading `how` as `Phrase` for every seat fails the second.
#[test]
fn a_members_line_says_who_let_it_in_on_which_day_and_whether_it_holds_a_node_key() {
    let mac = |by: Option<&str>, ms: u64, how: Option<Evidence>, node: bool| client::ListedMac {
        label: "m391".to_owned(),
        name: "m11".to_owned(),
        admitted_by: by.map(str::to_owned),
        admitted_ms: ms,
        how,
        node,
        code: "code".to_owned(),
        removed: None,
    };
    // 2026-11-12T23:59:59Z and the second after it.
    assert_eq!(
        listed_line(&mac(
            Some("m3"),
            1_794_527_999_000,
            Some(Evidence::Phrase),
            true
        )),
        "m391 m11 code=code admitted by=m3 on=2026-11-12 how=phrase node=yes"
    );
    assert_eq!(
        listed_line(&mac(
            Some("m11"),
            1_794_528_000_000,
            Some(Evidence::Manual),
            false
        )),
        "m391 m11 code=code admitted by=m11 on=2026-11-13 how=manual node=no"
    );
    assert_eq!(
        listed_line(&mac(Some("m22"), 1, Some(Evidence::Listed), true)),
        "m391 m11 code=code admitted by=m22 on=1970-01-01 how=listed node=yes"
    );
    assert_eq!(
        listed_line(&mac(None, 0, None, false)),
        "m391 m11 code=code founder node=no"
    );
    let removed = client::ListedMac {
        removed: Some(client::RemovedNote {
            label: "m391".to_owned(),
            by: vec!["m22".to_owned(), "m3".to_owned()],
            on_ms: 1,
            quorum: true,
        }),
        ..mac(None, 0, None, true)
    };
    assert_eq!(
        listed_line(&removed),
        "m391 m11 code=code removed by=m22,m3 quorum"
    );
}
