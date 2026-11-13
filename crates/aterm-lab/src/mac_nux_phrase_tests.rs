// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_missing_ask_is_the_local_network_question_only_when_it_held_the_ask_and_the_desk_answered() {
    let held = [(Some(10), 700), (Some(12), 812)];
    let sent = "Joined ALab: a member let this Mac in by its proof.";
    assert!(
        lan_cause(sent, true, Some(812), &held).starts_with("macOS's unanswered Local Network")
    );
    // Negative controls: no hold of the ask's own pid, an unreachable desk, an unread pid.
    assert!(lan_cause(sent, true, Some(900), &held).starts_with("not the Local Network"));
    assert!(lan_cause(sent, false, Some(812), &held).starts_with("not established: the desk"));
    assert!(lan_cause(sent, true, None, &held).starts_with("not established: the ask's pid"));
    // The ask's own words: a browse that heard no member is named as such only where the
    // desk was reachable.
    let none =
        "no ALab member answers on this network yet: x | this Mac's vault subscriber does not run";
    assert!(
        lan_cause(none, true, Some(900), &held).starts_with("the ask's browse heard no member")
    );
    assert!(lan_cause(none, false, Some(900), &held).starts_with("not established: the desk"));
    assert_eq!(said_lines("a\n\n  b  \n"), "a | b");
    let wrapped = with_pid("/usr/bin/env HOME=/h /a/aterm members join --no-wait");
    assert!(wrapped.starts_with("/bin/sh -c "), "{wrapped}");
    assert!(
        wrapped.contains("echo $$ > /var/tmp/aterm-nux/phrase-ask.pid; exec /usr/bin/env HOME=/h"),
        "{wrapped}"
    );
}

/// The run's lab members service in these tests.
const LAB: &str = "_alab-lab-k3x9q2._tcp";

/// A run that met the acceptance test: every file the seed's, In, the joiner's browse found
/// the member's lab advert alone and asked it at the guest's own address, the seed's port on
/// loopback and the member's at that address, the member's lab registration the guest's one,
/// no advert of a real service anywhere.
fn passing() -> Saw {
    Saw {
        allowed_by: Some(MEMBER_NAME.into()),
        files: CREDENTIALS
            .iter()
            .map(|c| Arrived {
                file: c.file.into(),
                at: Some(1_000),
                held: Held::Same,
            })
            .collect(),
        standing: Standing::In,
        table: TableSaw {
            seed: Ok(table_read()),
            joiner: Ok(table_read()),
        },
        service: LAB.into(),
        guest: ["127.0.0.1", "::1", "fe80::1", "192.168.64.5"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect(),
        member_at: "192.168.64.5:7486".into(),
        advertised: Some("alab-1a2b3c4d".into()),
        lab_adverts: Ok(vec![LabAdvert {
            instance: "alab-1a2b3c4d".into(),
            elsewhere: None,
        }]),
        joiner_from: Some("192.168.64.5".into()),
        asks_from: vec!["192.168.64.5".into()],
        listening: vec!["127.0.0.1:7485".into(), "192.168.64.5:7486".into()],
        registrations: vec![format!(
            "900 /usr/bin/dns-sd -R alab-1a2b3c4d {LAB} local 7486 v=1 role=member \
             fleet=7486 window=1"
        )],
        guest_adverts: SERVICES
            .iter()
            .map(|s| ((*s).to_owned(), Ok(Split::default())))
            .collect(),
        host_adverts: SERVICES
            .iter()
            .map(|s| ((*s).to_owned(), Ok(Split::default())))
            .collect(),
    }
}

/// The lab's one-row table as either identity reads it once the tables moved.
fn table_read() -> TableRead {
    TableRead {
        complete: true,
        epoch: "1a2b3c4d".into(),
        rows: 1,
        with_password: 1,
        digest: "9f".repeat(32),
    }
}

/// What `table_now` prints: the view (compact JSON, every password masked) and the digest.
const TABLE_OUT: &str = "{\"complete\":true,\"emails\":[{\"ad\":\"ad-0123456789abcdef\",\"password\":\"\u{2022}\u{2022}\"}],\"table\":\"emails\"}\n--nux-table--\n{\"digest\":\"9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f\",\"epoch\":\"1a2b3c4d\",\"rows\":1,\"table\":\"emails\",\"with_password\":1}\n";

#[test]
fn a_table_read_is_its_view_and_its_digest_value_free() {
    assert_eq!(read_table(TABLE_OUT), Ok(table_read()));
    // A digest refusal, a view that is not one, and a run that printed no mark each say why.
    let no_key = TABLE_OUT.replace(
        &TABLE_OUT[TABLE_OUT.find("--nux-table--\n").unwrap() + 14..],
        "aterm db: this Mac holds no [everyone] key, so the table cannot be digested\n",
    );
    let why = read_table(&no_key).unwrap_err();
    assert!(why.contains("no [everyone] key"), "{why}");
    let undeclared =
        "{\"next\":\"x\",\"registry\":\"not imported\",\"why\":\"y\"}\n--nux-table--\n{}";
    let why = read_table(undeclared).unwrap_err();
    assert!(why.starts_with("the emails view said"), "{why}");
    assert!(read_table("aterm: command not found").is_err());
}

#[test]
fn a_repair_line_reads_its_words_and_a_refusal_reads_none() {
    let line = "repaired 1793900000000 standing=enrolled-now rounds=8 control=5 rows=24 keys=2";
    let fields = read_repair(line).unwrap();
    assert_eq!(fields["standing"], "enrolled-now");
    assert_eq!(fields["rows"], "24");
    assert_eq!(fields["at"], "1793900000000");
    assert_eq!(
        read_repair("aterm-lab-guest: /j is pending in Commons"),
        None
    );
}

#[test]
fn a_run_whose_joiner_does_not_read_the_labs_table_fails() {
    assert_eq!(table_verdict(&passing().table), Vec::<String>::new());
    let with = |seed: Result<TableRead, String>, joiner: Result<TableRead, String>| {
        let mut saw = passing();
        saw.table = TableSaw { seed, joiner };
        verdict(&saw)
    };
    let t = table_read;
    // Another digest under the same epoch: the joiner holds another table.
    let other = TableRead {
        digest: "00".repeat(32),
        ..t()
    };
    assert_eq!(
        with(Ok(t()), Ok(other)),
        [
            "the joiner's table is not the seed's (1 row(s), 1 with a password, digest \
          00000000…; the seed's 1, 1, 9f9f9f9f…)"
        ]
    );
    // The same digest text under another epoch proves nothing: unproven, never a match.
    let moved = TableRead {
        epoch: "ffffffff".into(),
        ..t()
    };
    assert_eq!(
        with(Ok(t()), Ok(moved)),
        [
            "the joiner digested under epoch ffffffff, the seed under 1a2b3c4d, so the tables \
          are unproven"
        ]
    );
    let partial = TableRead {
        complete: false,
        ..t()
    };
    assert_eq!(
        with(Ok(t()), Ok(partial)),
        ["the joiner's table history is not complete"]
    );
    // A seed table with no password, or no row, cannot show the joiner opening one.
    let bare = TableRead {
        with_password: 0,
        ..t()
    };
    assert_eq!(
        with(Ok(bare.clone()), Ok(bare)),
        ["the seed's table holds no password, so the joiner opening one is unproven"]
    );
    assert_eq!(
        with(Ok(t()), Err("no [everyone] key".into())),
        ["the joiner's table did not read: no [everyone] key"]
    );
    assert_eq!(
        with(
            Err("the seed made no table: import".into()),
            Err("none".into())
        ),
        [
            "the seed's table did not read: the seed made no table: import",
            "the joiner's table did not read: none"
        ]
    );
    // NEGATIVE CONTROL (2026-11-05): with the epoch arm removed from `table_verdict`, the
    // `moved` case above passes (left: []), as a digest compared across epochs would.
}

#[test]
fn the_seeds_sheet_holds_no_password_and_its_password_never_reaches_argv() {
    let made = sheet("/n/phrase-seed");
    assert!(made.contains("umask 077"), "{made}");
    assert!(made.contains("/n/phrase-seed/sheet/emails.csv"), "{made}");
    assert!(
        !made.contains("/n/phrase-seed/seed/"),
        "never the published folder: {made}"
    );
    assert!(made.contains("Email Address,Status"), "{made}");
    assert!(
        !made.contains("Password"),
        "the sheet's flag column is left out: {made}"
    );
    let sealed = seal("/A/aterm", "/n/phrase-seed");
    assert!(
        sealed.contains("password --stdin < /n/phrase-seed/sheet/pw"),
        "the value comes on stdin from its file: {sealed}"
    );
    assert!(import_table("/A/aterm", "/n/phrase-seed").contains(
        "HOME=/n/phrase-seed /A/aterm db alab import --emails /n/phrase-seed/sheet/emails.csv --apply"
    ));
    assert_eq!(
        repair("/n/aterm-lab-guest", "/n/j", "/n/s"),
        "/usr/bin/env HOME=/n/j /n/aterm-lab-guest repair /n/j /n/s 8 /n/s 2>&1"
    );
}

#[test]
fn a_run_whose_joiner_got_no_file_fails() {
    assert_eq!(verdict(&passing()), Vec::<String>::new());
    let mut saw = passing();
    saw.files[1].held = Held::Absent;
    saw.files[1].at = None;
    assert_eq!(
        verdict(&saw),
        ["the joiner got no ~/.secrets/gh_access_token_alabsystems (publishing)"]
    );
    // A file the guest did not report at all is as missing as an absent one.
    let mut saw = passing();
    saw.files.retain(|a| a.file != "ai-accounts.csv");
    assert_eq!(
        verdict(&saw),
        ["the joiner got no ~/.secrets/ai-accounts.csv (AI accounts)"]
    );
    let mut saw = passing();
    saw.files[0].held = Held::Differs;
    assert_eq!(
        verdict(&saw),
        ["~/.secrets/gh_access_token (GitHub) is not the seed's value"]
    );
}

/// The acceptance test's "from any existing member": a phrase the vault's admin showed, or
/// one no desk record names, is not the member's Allow.
#[test]
fn a_run_whose_phrase_came_from_the_admin_fails() {
    let mut saw = passing();
    saw.allowed_by = Some("nux-seed".into());
    assert_eq!(
        verdict(&saw),
        ["the phrase came from nux-seed, not nux-member, the member that is no admin"]
    );
    let mut saw = passing();
    saw.allowed_by = None;
    assert_eq!(
        verdict(&saw),
        ["no desk record names the Mac that showed the phrase"]
    );
}

/// The member asks as itself, pinned to the vault's signer, the seed approves it, and it
/// enrols with the seed: each a guest command of its own.
#[test]
fn the_member_joins_by_the_seeds_approval_and_enrols_with_it() {
    let created = "Created vault nux-local at http://127.0.0.1:7485/v1/nux-local.\n\
                   On another Mac: aterm secrets join \
                   http://admins-virtual-machine.local:7485/v1/nux-local#age1sx2k9 --name <host>\n";
    assert_eq!(signer_of(created), Some("age1sx2k9"));
    assert_eq!(signer_of("aterm secrets join http://h:7485/v1/v\n"), None);
    assert_eq!(signer_of("aterm secrets join http://h:7485/v1/v#\n"), None);
    assert_eq!(signer_of(""), None);
    // The code `secrets join` and `secrets status` print (cli.rs's `waiting_line`,
    // `pending_membership`), approved by itself: `approve --all` needs a terminal.
    let waiting = "waiting for approval of nux-member (code abcd-efgh-ijkm-npqr): on an admin \
                   Mac run aterm secrets approve abcd-efgh-ijkm-npqr\n\
                   membership: waiting for approval (on an admin Mac: aterm secrets approve \
                   abcd-efgh-ijkm-npqr)\n";
    assert_eq!(approve_code(waiting), Some("abcd-efgh-ijkm-npqr"));
    assert_eq!(
        approve_code(
            "membership: waiting for approval (on an admin Mac: aterm secrets approve wxyz-2345)\n"
        ),
        Some("wxyz-2345")
    );
    assert_eq!(approve_code("aterm secrets approve <code>\n"), None);
    assert_eq!(
        approve_code("joined: seq 3, 3 secrets in ~/.secrets\n"),
        None
    );
    assert_eq!(
        approve("/A/aterm", "/n/seed", "abcd-efgh-ijkm-npqr"),
        "/usr/bin/env HOME=/n/seed /A/aterm secrets approve abcd-efgh-ijkm-npqr \
         > /n/seed/approve.out 2>&1 || { cat /n/seed/approve.out; exit 1; }"
    );
    assert!(!approve("/A/aterm", "/n/seed", "c").contains("--all"));
    let sync = sync_now("/A/aterm", "/n/member");
    assert!(
        sync.starts_with("/usr/bin/env HOME=/n/member /A/aterm secrets sync ")
            && sync.contains("HOME=/n/member /A/aterm members status"),
        "{sync}"
    );
    assert_eq!(
        enrol("/n/helper", "/n/member", "/n/seed"),
        "/usr/bin/env HOME=/n/member /n/helper enrol /n/member /n/seed"
    );
    assert_ne!(MEMBER_NAME, SEED_NAME);
}

#[test]
fn a_run_whose_joiner_stayed_pending_fails() {
    for still in [Standing::Allowed, Standing::Asking, Standing::NotIn] {
        let mut saw = passing();
        saw.standing = still.clone();
        assert_eq!(
            verdict(&saw),
            [format!("the joiner stayed {still}, never In")]
        );
    }
}

/// An advert that is not this Mac's own, heard in the guest or from a guest on this Mac,
/// fails the row.
#[test]
fn a_run_whose_servers_advertised_fails() {
    let another = || Split {
        others: vec!["nuxlab on bridge100 (nuxlab.local 192.168.64.3)".into()],
        this_macs: Vec::new(),
    };
    for (service, on_guest) in [(0, true), (1, true), (0, false), (1, false)] {
        let mut saw = passing();
        let adverts = if on_guest {
            &mut saw.guest_adverts
        } else {
            &mut saw.host_adverts
        };
        adverts[service].1 = Ok(another());
        assert_eq!(verdict(&saw).len(), 1, "{:?}", verdict(&saw));
    }
}

/// This Mac's own registration, heard over tart's bridge in the guest and on this Mac (the
/// ALab vault the lab Mac serves), is named and never fails the row; one beside it that is
/// not this Mac's still does.
#[test]
fn a_run_that_heard_only_this_macs_own_adverts_passes() {
    let own = || Split {
        others: Vec::new(),
        this_macs: vec!["alab-secrets heard over bridge100".into()],
    };
    let mut saw = passing();
    saw.guest_adverts[0].1 = Ok(own());
    saw.host_adverts[0].1 = Ok(own());
    assert!(verdict(&saw).is_empty(), "{:?}", verdict(&saw));
    // NEGATIVE CONTROL: a guest's advert beside this Mac's own fails, on either side.
    for on_guest in [true, false] {
        let mut saw = passing();
        let mut both = own();
        both.others
            .push("nuxlab on bridge100 (nuxlab.local 192.168.64.3)".into());
        if on_guest {
            saw.guest_adverts[0].1 = Ok(both);
        } else {
            saw.host_adverts[0].1 = Ok(both);
        }
        assert_eq!(verdict(&saw).len(), 1, "{:?}", verdict(&saw));
    }
}

/// A browse that never looked (a `dns-sd` that could not reach mDNSResponder, a transcript
/// `script` never flushed) proves no advert absent, on either side.
#[test]
fn a_run_whose_browse_did_not_run_fails() {
    let mut saw = passing();
    saw.guest_adverts[0].1 = Err("dns-sd failed: DNSService call failed -65563".into());
    assert_eq!(
        verdict(&saw),
        [
            "the guest's browse of _alab-vault._tcp did not run (dns-sd failed: DNSService \
             call failed -65563), so no advert is unproven"
        ]
    );
    let mut saw = passing();
    saw.guest_adverts.pop();
    assert_eq!(
        verdict(&saw),
        ["the guest's browse of _alab._tcp left no record, so no advert is unproven"]
    );
    let mut saw = passing();
    saw.host_adverts[1].1 = Err("its transcript is empty".into());
    assert_eq!(
        verdict(&saw),
        [
            "this Mac's browse of _alab._tcp did not run (its transcript is empty), so no \
             advert from a guest is unproven"
        ]
    );
    let mut saw = passing();
    saw.host_adverts.clear();
    assert_eq!(verdict(&saw).len(), 2, "{:?}", verdict(&saw));
}

/// A registration a guest process holds is the guest advertising, whatever a browse heard.
#[test]
fn a_run_whose_guest_registered_a_bonjour_service_fails() {
    // As `pgrep -lf dns-sd` lists them: aterm's scoped registration on a Mac on two
    // networks and its plain one count; a browse, the `script` it runs under and the shell
    // that ran `pgrep` do not.
    let pgrep = "\
        812 /usr/bin/dns-sd -i en0 -P nuxlab _alab-vault._tcp local 7485 nuxlab.local 192.168.64.5 path=/v1\n\
        813 dns-sd -R nuxlab _alab._tcp local 7100\n\
        820 /usr/bin/dns-sd -B _alab-vault._tcp local.\n\
        821 /usr/bin/script -q -t 0 /tmp/heard_alab._tcp.txt /usr/bin/dns-sd -B _alab._tcp local.\n\
        830 sh -c pgrep -lf dns-sd || true\n";
    assert_eq!(
        registrations(pgrep),
        [
            "812 /usr/bin/dns-sd -i en0 -P nuxlab _alab-vault._tcp local 7485 nuxlab.local \
             192.168.64.5 path=/v1",
            "813 dns-sd -R nuxlab _alab._tcp local 7100",
        ]
    );
    assert!(registrations("").is_empty());
    let mut saw = passing();
    saw.registrations = vec!["812 dns-sd -P nuxlab _alab-vault._tcp local 7485".into()];
    assert_eq!(
        verdict(&saw),
        ["a guest process registered a Bonjour service: \
             812 dns-sd -P nuxlab _alab-vault._tcp local 7485"]
    );
    // The member's registration of the run's lab service is the one the row expects; one of
    // the live fleet's service, or of another lab's, beside it still fails. NEGATIVE CONTROL
    // (2026-10-29): `registers` taking any `_alab-lab-` registration fails here at another
    // lab's: `left: []`.
    for beside in [
        "813 dns-sd -R nuxlab _alab._tcp local 7100",
        "814 dns-sd -R alab-00000000 _alab-lab-zzzzzz._tcp local 7486",
    ] {
        let mut saw = passing();
        saw.registrations.push(beside.into());
        assert_eq!(
            verdict(&saw),
            [format!(
                "a guest process registered a Bonjour service: {beside}"
            )]
        );
    }
}

#[test]
fn a_run_whose_servers_listened_beyond_loopback_fails() {
    let mut saw = passing();
    saw.listening = vec!["*:7485".into(), "127.0.0.1:7486".into()];
    assert_eq!(
        verdict(&saw),
        ["an aterm process listened beyond loopback: *:7485"]
    );
    let mut saw = passing();
    saw.listening.push("192.168.64.5:7100".into());
    assert_eq!(
        verdict(&saw),
        ["an aterm process listened beyond loopback: 192.168.64.5:7100"]
    );
    // Only the member's port at the guest's own address listens beyond loopback: the same
    // port on every interface, or at another address, does not pass for it. NEGATIVE CONTROL
    // (2026-10-29): `isolation_verdict` passing any listener on the member's port fails here
    // at `*:7486`: `left: []`.
    for wide in ["*:7486", "192.168.64.9:7486"] {
        let mut saw = passing();
        saw.listening.push(wide.into());
        assert_eq!(
            verdict(&saw),
            [format!("an aterm process listened beyond loopback: {wide}")]
        );
    }
    // A run that read no listener on a seed port has not shown where it listened.
    let mut saw = passing();
    saw.listening = vec!["127.0.0.1:7485".into()];
    assert_eq!(
        verdict(&saw),
        ["no listener on port 7486 read back, so where the seed listened is unproven"]
    );
    let mut saw = passing();
    saw.listening.push("[::1]:7487".into());
    assert_eq!(verdict(&saw), Vec::<String>::new());
}

/// The member's desk record as the guest helper writes it (`src/guest/member.rs`'s module
/// docs).
const RECORD: &str = "advertised 990 instance=alab-1a2b3c4d service=_alab-lab-k3x9q2._tcp port=7486\n\
                      member 1000 listen=192.168.64.5:7486 name=nux-member\n\
                      heard 1250 id=1 name=admins-virtual-machine code=K7Q2-9F3A from=192.168.64.5\n\
                      asked 1251 from=192.168.64.5\n\
                      phrase 1300 id=1 name=admins-virtual-machine maple river ocean\n\
                      let-in 4100 name=admins-virtual-machine\n\
                      posted 4180 ok\n";

/// A run whose bundle the new Mac never acknowledged (`src/guest/member.rs`'s
/// `sent-unacknowledged`) is read as sent, with why, never as a Mac let in.
///
/// NEGATIVE CONTROL: drop `read_desk`'s `sent-unacknowledged` arm: `left: None`, `right:
/// Some((4100, "admins-virtual-machine", "the other Mac closed the join exchange"))`.
#[test]
fn a_run_the_new_mac_never_acknowledged_is_read_as_no_mac_let_in() {
    let seed = read_desk(
        "sent-unacknowledged 4100 name=admins-virtual-machine the other Mac closed the join \
         exchange\nposted 4180 ok\n",
    );
    assert_eq!(seed.let_in, None);
    assert_eq!(
        seed.unacknowledged,
        Some((
            4100,
            "admins-virtual-machine".into(),
            "the other Mac closed the join exchange".into()
        ))
    );
}

#[test]
fn the_phrase_reader_types_only_the_one_phrase_shown_for_the_mac_that_asked() {
    let seed = read_desk(RECORD);
    assert_eq!(
        seed.advertised,
        Some(("alab-1a2b3c4d".into(), "_alab-lab-k3x9q2._tcp".into()))
    );
    assert_eq!(
        seed.serving,
        Some((1000, "192.168.64.5:7486".into(), "nux-member".into()))
    );
    assert_eq!(
        seed.heard,
        [Heard {
            at: 1250,
            name: "admins-virtual-machine".into(),
            code: "K7Q2-9F3A".into(),
            from: Some("192.168.64.5".into()),
        }]
    );
    assert_eq!(seed.asked, ["192.168.64.5"]);
    // Every address an ask came from, each once, an ended exchange's included.
    let more = format!("{RECORD}ask-ended 4200 from=192.168.64.1 eof\nasked 4300 from=10.0.0.7\n");
    assert_eq!(
        read_desk(&more).sources(),
        ["192.168.64.5", "10.0.0.7", "192.168.64.1"]
    );
    assert_eq!(seed.let_in, Some((4100, "admins-virtual-machine".into())));
    assert_eq!(seed.posted, Some(Ok(4180)));
    let shown = presented_phrase(&seed).unwrap().unwrap();
    assert_eq!(
        (shown.at, shown.name.as_str()),
        (1300, "admins-virtual-machine")
    );
    assert_eq!(shown.words, "maple river ocean");
    assert!(
        !format!("{shown:?}").contains("maple"),
        "a Debug names no word"
    );

    // Not yet: the seed heard nothing, or heard the Mac and shows no phrase.
    assert_eq!(
        presented_phrase(&read_desk("member 1 listen=127.0.0.1:7486 name=s\n")),
        Ok(None)
    );
    let asking: String = RECORD.lines().take(2).map(|l| format!("{l}\n")).collect();
    assert_eq!(presented_phrase(&read_desk(&asking)), Ok(None));
    // Never: a phrase for a Mac it never heard, words that are no phrase, two phrases, a
    // refusal, an Allow that failed.
    let other = RECORD.replace(
        "phrase 1300 id=1 name=admins-virtual-machine",
        "phrase 1300 id=1 name=mx",
    );
    assert_eq!(
        presented_phrase(&read_desk(&other)).unwrap_err(),
        "the member shows a phrase for mx, a Mac it never heard ask"
    );
    let garbled = RECORD.replace("maple river ocean", "maple river zzzz");
    let refused = presented_phrase(&read_desk(&garbled)).unwrap_err();
    assert!(
        refused.starts_with("the member's phrase for admins-virtual-machine:"),
        "{refused}"
    );
    assert!(
        !refused.contains("maple"),
        "an error names no word: {refused}"
    );
    let short = RECORD.replace("maple river ocean", "maple river");
    assert!(presented_phrase(&read_desk(&short)).is_err());
    let twice =
        format!("{RECORD}phrase 1400 id=1 name=admins-virtual-machine abandon ability able\n");
    assert_eq!(
        presented_phrase(&read_desk(&twice)).unwrap_err(),
        "the member shows 2 phrases, where the lane pressed Allow once"
    );
    assert_eq!(
        presented_phrase(&read_desk(
            "refused 9 this member holds no Commons control (60 s on)\n"
        ))
        .unwrap_err(),
        "the member's desk refused to serve: this member holds no Commons control (60 s on)"
    );
    let failed = format!("{asking}allow-failed 1290 this Mac cannot allow yet\n");
    assert_eq!(
        presented_phrase(&read_desk(&failed)).unwrap_err(),
        "the member's Allow failed: this Mac cannot allow yet"
    );
    assert_eq!(
        read_desk("posted 5 failed no vault server took it\nask-ended 6 from=127.0.0.1 eof\n"),
        Desk {
            posted: Some(Err("no vault server took it".into())),
            ended: vec!["from=127.0.0.1 eof".into()],
            ..Desk::default()
        }
    );
}

#[test]
fn the_guest_reads_say_each_file_where_the_joiner_stands_and_who_listens() {
    let arrivals = "gh_access_token 1761650000.250 same\n\
                    gh_access_token_alabsystems 1761650000.5 differs\n\
                    ai-accounts.csv none absent\n";
    assert_eq!(
        read_arrivals(arrivals),
        [
            Arrived {
                file: "gh_access_token".into(),
                at: Some(1_761_650_000_250),
                held: Held::Same
            },
            Arrived {
                file: "gh_access_token_alabsystems".into(),
                at: Some(1_761_650_000_500),
                held: Held::Differs
            },
            Arrived {
                file: "ai-accounts.csv".into(),
                at: None,
                held: Held::Absent
            },
        ]
    );
    assert_eq!(
        Standing::of("member as admins-virtual-machine\n"),
        Standing::In
    );
    assert_eq!(
        Standing::of("allowed: ~/.secrets is written, and this Mac follows vault changes"),
        Standing::Allowed
    );
    assert_eq!(Standing::of("asking: press Allow"), Standing::Asking);
    assert_eq!(
        Standing::of("not in ALab: run `aterm members join` here"),
        Standing::NotIn
    );
    assert_eq!(
        Standing::of("unknown: no home"),
        Standing::Unread("unknown: no home".into())
    );
    assert_eq!(
        read_status("member as admins-virtual-machine\n1761650004000\n"),
        (Standing::In, Some(1_761_650_004_000))
    );
    assert_eq!(
        read_status("allowed: ~/.secrets is written\n1761650003000\n"),
        (Standing::Allowed, Some(1_761_650_003_000))
    );
    // A status that printed nothing, or a clock that never came, reads as such.
    assert_eq!(
        read_status("1761650003000\n"),
        (Standing::Unread(String::new()), Some(1_761_650_003_000))
    );
    assert_eq!(read_status("member as m\n"), (Standing::In, None));
    assert_eq!(read_status(""), (Standing::Unread(String::new()), None));

    let lsof = "COMMAND     PID  USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME\n\
                aterm       812 admin    9u  IPv4 0x1c3a5e0f1d2b3c4d      0t0  TCP 127.0.0.1:7485 (LISTEN)\n\
                aterm-lab   830 admin    3u  IPv4 0x1c3a5e0f1d2b3c4e      0t0  TCP 127.0.0.1:7486 (LISTEN)\n\
                aterm       812 admin   10u  IPv6 0x1c3a5e0f1d2b3c4f      0t0  TCP *:7485 (LISTEN)\n\
                aterm       812 admin   11u  IPv4 0x1c3a5e0f1d2b3c50      0t0  TCP 127.0.0.1:7485 (LISTEN)\n";
    assert_eq!(
        listen_addresses(lsof),
        ["127.0.0.1:7485", "127.0.0.1:7486", "*:7485"],
        "a wildcard listener is read, so the verdict sees it"
    );
    assert!(listen_addresses("").is_empty());
    assert!(on_loopback("127.0.0.1:7485") && on_loopback("[::1]:7486"));
    assert!(!on_loopback("*:7485") && !on_loopback("[::]:7486") && !on_loopback("10.0.0.2:1"));
}

#[test]
fn vault_phrase_is_a_row_of_all_beside_vault_local_that_runs_the_app() {
    use super::super::{Scenario, parse, parse_scenarios};
    let all = parse_scenarios("all").unwrap();
    let at = all
        .iter()
        .position(|s| *s == Scenario::VaultPhrase)
        .expect("all runs vault-phrase");
    assert_eq!(all[at - 1], Scenario::VaultLocal);
    assert_eq!(Scenario::VaultPhrase.name(), "vault-phrase");
    let ask = parse(
        &[
            "--scenario".to_owned(),
            "vault-phrase,vault-local".to_owned(),
        ],
        "img".into(),
    )
    .unwrap();
    assert_eq!(ask.scenarios, [Scenario::VaultLocal, Scenario::VaultPhrase]);
    let s = Scenario::VaultPhrase;
    assert!(s.needs_app() && !s.grows_disk() && s.not_run().is_none());
}

/// The seed's server listens on 127.0.0.1 and names no mDNS service, the member's desk
/// listens at the guest's own address registered under the run's lab service, and the
/// driver types the phrase at the prompt `aterm members join` prints.
#[test]
fn the_seed_serves_on_loopback_and_the_member_at_the_guests_address() {
    let serve = serve("/A/aterm", "/n/seed");
    assert!(
        serve.starts_with("(/usr/bin/env HOME=/n/seed /A/aterm secrets serve "),
        "{serve}"
    );
    let words: Vec<&str> = serve.split_whitespace().collect();
    let after = |flag: &str| {
        words
            .iter()
            .position(|w| *w == flag)
            .map(|at| words[at + 1])
    };
    assert_eq!(after("--listen"), Some("127.0.0.1:7485"), "{serve}");
    assert!(words.contains(&"--no-advertise"), "{serve}");
    let member = desk("/n/helper", "/n/member", "192.168.64.5:7486", "/n/rec", LAB);
    assert!(
        member.contains(
            "HOME=/n/member /n/helper member /n/member 192.168.64.5:7486 /n/rec \
             --advertise _alab-lab-k3x9q2._tcp "
        ),
        "{member}"
    );
    let create = create("/A/aterm", "/n/seed");
    assert!(
        create.contains("secrets create 'http://127.0.0.1:7485/v1/")
            && create.contains("--import /n/seed/seed --no-agent --name nux-seed"),
        "{create}"
    );
    assert!(
        !create.contains("--advertise") && !create.contains("--serve"),
        "{create}"
    );
    let h = Homes {
        seed: "/n/seed".into(),
        member: "/n/member".into(),
        joiner: "/n/joiner".into(),
    };
    let homes = homes(&h, "/n/rec");
    assert!(
        homes.contains("mkdir -p /n/seed/seed /n/member /n/joiner;"),
        "{homes}"
    );
    for c in &CREDENTIALS {
        assert!(
            homes.contains(&format!("> /n/seed/seed/{};", c.file)),
            "{homes}"
        );
    }
    assert!(homes.contains("chmod 600 /n/seed/seed/*"), "{homes}");
    assert_eq!(
        as_home("/A/aterm", "/n/joiner"),
        "/usr/bin/env HOME=/n/joiner /A/aterm"
    );
    assert_eq!(
        arrivals("/n/helper", "/n/seed", "/n/joiner"),
        "/n/helper arrivals /n/seed/seed /n/joiner/.secrets gh_access_token \
         gh_access_token_alabsystems ai-accounts.csv"
    );

    // The seed founds the lab as the other labs do, into a new file beside its homes.
    assert_eq!(
        genesis("/n/helper", "/n/seed"),
        "/usr/bin/env HOME=/n/seed /n/helper genesis /n/seed /n/seed/genesis.v2"
    );
    assert_eq!(
        genesis_said("genesis 1793898071123 members=2 digest=3ba9f999\n"),
        "2 members, digest 3ba9f999"
    );
    assert!(genesis_said("x: genesis: no vault configured").starts_with("said "));
}

/// Every command of the joiner's (its ask, its join, its subscriber, its status) runs with
/// `ATERM_LAB_MEMBER_SERVICE` naming the run's lab service, so each that browses finds the
/// member by its own browse of that service, never of `_alab._tcp`; the seed's commands
/// carry neither. The join also discovers the member through the broker.
///
/// NEGATIVE CONTROL (2026-10-29): `as_joiner` without its `ATERM_LAB_MEMBER_SERVICE=` word
/// (the joiner then browses the live fleet's `_alab._tcp`) fails here at the ask.
#[test]
fn every_command_of_the_joiners_browses_the_runs_lab_service_alone() {
    let me = as_joiner("/A/aterm", "/n/joiner", LAB);
    let ask = ask_cmd(&me);
    let join = join_cmd(&me);
    let watching = watch(&me, "/n/joiner");
    let status = status_now(&me);
    for cmd in [&ask, &join, &watching, &status] {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        assert!(
            words.contains(&"ATERM_LAB_MEMBER_SERVICE=_alab-lab-k3x9q2._tcp")
                && words.contains(&"HOME=/n/joiner"),
            "{cmd}"
        );
        assert!(!words.contains(&"--at"), "{cmd}");
    }
    assert!(ask.ends_with(" members join --no-wait"), "{ask}");
    // The phrase reaches the join as its argument from the joiner's own shell, so the lane's
    // command line names the variable, never a phrase.
    assert!(join.starts_with("/bin/sh -c '"), "{join}");
    assert!(join.ends_with(" members join \"$NUX_PHRASE\"'"), "{join}");
    assert_eq!(
        watching,
        "(/usr/bin/env HOME=/n/joiner ATERM_LAB_MEMBER_SERVICE=_alab-lab-k3x9q2._tcp \
         /A/aterm secrets watch </dev/null >/n/joiner/watch.log 2>&1 &)"
    );
    let seed = watch(&as_home("/A/aterm", "/n/seed"), "/n/seed");
    assert!(!seed.contains(LAB_MEMBER_SERVICE_ENV), "{seed}");
    assert_eq!(
        names_knob("/g/aterm.app/Contents/MacOS/aterm"),
        "/usr/bin/grep -q -a -F ATERM_LAB_MEMBER_SERVICE \
         '/g/aterm.app/Contents/MacOS/aterm' && echo named || echo absent"
    );
}

/// The run's lab service is one `browse_members` takes, never the live fleet's, and each
/// run's differs with its random bytes.
///
/// NEGATIVE CONTROL (2026-10-29): `lab_service_of` with a seventh character after its nonce
/// (`_alab-lab-000000x._tcp`, past DNS-SD's 15) fails here at its first `lab_service`
/// assertion.
#[test]
fn the_runs_lab_service_is_a_lab_one() {
    for bytes in [[0u8; 6], [255; 6], [35, 36, 71, 72, 200, 9]] {
        let service = lab_service_of(bytes);
        assert!(aterm_secrets::discover::lab_service(&service), "{service}");
        assert_eq!(
            aterm_secrets::discover::member_service(Some(std::ffi::OsStr::new(&service))).unwrap(),
            service
        );
        assert_ne!(service, MEMBER_SERVICE);
    }
    assert_eq!(
        lab_service_of([0, 1, 10, 35, 36, 255]),
        "_alab-lab-01az03._tcp"
    );
    assert_ne!(lab_service_of([1; 6]), lab_service_of([2; 6]));
}

/// The guest's addresses as `ifconfig -a` prints them on a tart guest, and the one the member
/// listens at: its IPv4 address on tart's network, never loopback or link-local.
///
/// NEGATIVE CONTROL (2026-10-29): `lan_address` without its link-local filter fails here:
/// `left: Some(169.254.20.7)`, `right: Some(192.168.64.5)`.
#[test]
fn the_member_listens_at_the_guests_address_on_tarts_network() {
    let ifconfig = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\
        \toptions=1203<RXCSUM,TXCSUM,TXSTATUS,SW_TIMESTAMP>\n\
        \tinet 127.0.0.1 netmask 0xff000000\n\
        \tinet6 ::1 prefixlen 128 \n\
        \tinet6 fe80::1%lo0 prefixlen 64 scopeid 0x1 \n\
        en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
        \tether 7a:21:9c:40:11:02\n\
        \tinet6 fe80::1c2f:8ad3:6b1e:2a41%en0 prefixlen 64 secured scopeid 0x6 \n\
        \tinet 169.254.20.7 netmask 0xffff0000 broadcast 169.254.255.255\n\
        \tinet 192.168.64.5 netmask 0xffffff00 broadcast 192.168.64.255\n\
        \tinet 192.168.64.5 netmask 0xffffff00 broadcast 192.168.64.255\n";
    let addresses = inet_addresses(ifconfig);
    let expected: Vec<IpAddr> = [
        "127.0.0.1",
        "::1",
        "fe80::1",
        "fe80::1c2f:8ad3:6b1e:2a41",
        "169.254.20.7",
        "192.168.64.5",
    ]
    .iter()
    .map(|a| a.parse().unwrap())
    .collect();
    assert_eq!(addresses, expected);
    assert_eq!(
        lan_address(&addresses),
        Some(Ipv4Addr::new(192, 168, 64, 5))
    );
    assert_eq!(
        lan_address(&addresses[..5]),
        None,
        "loopback and link-local are not it"
    );
    assert!(inet_addresses("").is_empty());
}

/// The joiner's browse is proven to have found the member only when the member registered
/// under the run's lab service, the guest's browse heard that instance resolving to the guest,
/// and the member heard the joiner ask from a guest address beyond loopback.
///
/// NEGATIVE CONTROL (2026-10-29): `browse_verdict` with its `Ok(ip) if ip.is_loopback()` arm
/// removed (an ask over loopback passes for one the browse resolved) fails here:
/// `left: []`.
#[test]
fn a_run_whose_joiner_did_not_find_the_member_by_its_browse_fails() {
    assert_eq!(browse_verdict(&passing()), Vec::<String>::new());
    // The joiner dialed the member at loopback (as `--at 127.0.0.1:7486` would).
    let mut saw = passing();
    saw.joiner_from = Some("127.0.0.1".into());
    saw.asks_from = vec!["127.0.0.1".into()];
    assert_eq!(
        verdict(&saw),
        ["the joiner asked over loopback (127.0.0.1), not at the address its browse resolved"]
    );
    let mut saw = passing();
    saw.joiner_from = None;
    assert_eq!(
        verdict(&saw),
        ["the member heard the joiner ask from no address"]
    );
    // The member registered nothing, or its browse never heard the member's instance.
    let mut saw = passing();
    saw.advertised = None;
    assert_eq!(
        verdict(&saw),
        [format!(
            "the member's record names no {LAB} registration, so no browse could find it"
        )]
    );
    let mut saw = passing();
    saw.lab_adverts = Ok(Vec::new());
    assert_eq!(
        verdict(&saw),
        [format!(
            "the guest's browse of {LAB} never heard the member's alab-1a2b3c4d resolving \
             to the guest (it heard none)"
        )]
    );
    let mut saw = passing();
    saw.lab_adverts = Err("dns-sd failed: DNSService call failed -65563".into());
    assert_eq!(
        verdict(&saw),
        [format!(
            "the guest's browse of {LAB} did not run (dns-sd failed: DNSService call failed \
             -65563), so what the joiner's browse could find is unproven"
        )]
    );
    // The member's instance heard resolving elsewhere is not the member the joiner found.
    let mut saw = passing();
    saw.lab_adverts = Ok(vec![LabAdvert {
        instance: "alab-1a2b3c4d".into(),
        elsewhere: Some("admins-virtual-machine.local 192.168.64.1".into()),
    }]);
    assert_eq!(verdict(&saw).len(), 2, "{:?}", verdict(&saw));
    assert_eq!(browse_verdict(&saw).len(), 1, "{:?}", browse_verdict(&saw));
}

/// Anything that reached beyond the guest fails the row: an ask the member heard from an
/// address that is not the guest's (this Mac's end of tart's bridge, another Mac), or an
/// advert of the lab service the guest heard that is not its own.
///
/// NEGATIVE CONTROL (2026-10-29): `isolation_verdict` without its check of `asks_from`
/// against the guest's addresses (an ask from this Mac's bridge address passes) fails here:
/// `left: []`.
#[test]
fn a_run_that_reached_beyond_the_guest_fails() {
    assert_eq!(isolation_verdict(&passing()), Vec::<String>::new());
    for stranger in ["192.168.64.1", "192.168.1.242", "fe80::9", "no-address"] {
        let mut saw = passing();
        saw.asks_from.push(stranger.into());
        assert_eq!(
            verdict(&saw),
            [format!(
                "the member heard an ask from {stranger}, not one of the guest's own addresses"
            )]
        );
    }
    // The joiner's own ask from a stranger fails as isolation, not as a browse that missed.
    let mut saw = passing();
    saw.joiner_from = Some("192.168.64.1".into());
    saw.asks_from = vec!["192.168.64.1".into()];
    assert_eq!(browse_verdict(&saw), Vec::<String>::new());
    assert_eq!(
        isolation_verdict(&saw).len(),
        1,
        "{:?}",
        isolation_verdict(&saw)
    );
    // A lab advert beside the member's that resolved elsewhere is one the joiner could ask.
    let mut saw = passing();
    saw.lab_adverts.as_mut().unwrap().push(LabAdvert {
        instance: "alab-99999999".into(),
        elsewhere: Some("m22.local 192.168.1.242".into()),
    });
    assert_eq!(
        verdict(&saw),
        [format!(
            "the guest heard 1 {LAB} advert(s) not its own (alab-99999999 (m22.local \
             192.168.1.242)): the joiner's browse could have asked them"
        )]
    );
}
