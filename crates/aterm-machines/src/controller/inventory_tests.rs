// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Stat};

const ROOT: &str = "/ctl/machines";
const UUID_7: &str = "0A1B2C3D-0000-4000-8000-000000000007";
const UUID_8: &str = "0A1B2C3D-0000-4000-8000-000000000008";

fn paths() -> Paths {
    Paths {
        root: PathBuf::from(ROOT),
    }
}

#[test]
fn a_relative_controller_dir_is_made_absolute_since_children_run_in_root() {
    let p = Paths::at(Path::new("lab/ctl"));
    assert!(p.root.is_absolute(), "{p:?}");
    assert!(p.known_hosts().ends_with("lab/ctl/known_hosts"), "{p:?}");
    assert_eq!(Paths::at(Path::new(ROOT)), paths());
}

fn host(name: &str, uuid: &str, address: &str) -> Host {
    Host {
        name: name.into(),
        uuid: uuid.into(),
        address: address.into(),
        user: "labadmin".into(),
        baseline: None,
        users: Vec::new(),
    }
}

/// An inventory the owner has annotated by hand.
const HAND_EDITED: &str = "\
# The lab, by bench.
schema = 1

[controller]
parallel = 4 # the switch drops more

# Bench A
[[host]]
name = \"lab-07\"
uuid = \"0A1B2C3D-0000-4000-8000-000000000007\"
address = \"10.0.0.7\"  # wired port 7
user = \"labadmin\"
baseline = \"lab\"

# Bench B, the one by the window
[[host]]
name = \"lab-08\"
uuid = \"0A1B2C3D-0000-4000-8000-000000000008\"
address = \"lab-08.local\"
user = \"labadmin\"

# end of hosts
";

fn fake_with(text: &str) -> Fake {
    let fake = Fake::root();
    fake.file(&format!("{ROOT}/machines.toml"), text.as_bytes());
    fake
}

fn written_inventory(fake: &Fake) -> String {
    String::from_utf8(fake.written(&format!("{ROOT}/machines.toml")).unwrap()).unwrap()
}

#[test]
fn a_missing_inventory_says_to_run_init() {
    let err = load(&Fake::root(), &paths()).unwrap_err();
    assert!(err.contains("run `aterm machines init`"), "{err}");
    assert!(err.contains("/ctl/machines/machines.toml"), "{err}");
}

#[test]
fn a_hand_edited_inventory_loads() {
    let inv = load(&fake_with(HAND_EDITED), &paths()).unwrap();
    assert_eq!(inv.parallel(), 4);
    assert_eq!(inv.hosts.len(), 2);
    assert_eq!(inv.hosts[0].baseline.as_deref(), Some("lab"));
    assert_eq!(inv.hosts[1].address, "lab-08.local");
}

#[test]
fn an_unknown_key_or_another_schema_is_refused() {
    let typo = HAND_EDITED.replace("baseline = \"lab\"", "baselin = \"lab\"");
    assert!(parse(&typo).is_err());
    let err = parse(&HAND_EDITED.replace("schema = 1", "schema = 2")).unwrap_err();
    assert!(err.contains("schema = 2"), "{err}");
}

#[test]
fn hosts_that_share_a_name_or_a_uuid_are_refused() {
    let twin = HAND_EDITED.replace("name = \"lab-08\"", "name = \"LAB-07\"");
    assert!(parse(&twin).unwrap_err().contains("two hosts are named"));
    let clone = HAND_EDITED.replace(UUID_8, &UUID_7.to_ascii_lowercase());
    assert!(parse(&clone).unwrap_err().contains("same uuid"));
}

#[test]
fn a_word_ssh_or_a_path_could_misread_is_refused() {
    let mut h = host("lab-07", UUID_7, "-oProxyCommand=x");
    assert!(check_host(&h).unwrap_err().contains("address"));
    h.address = "admin@10.0.0.7".into();
    assert!(check_host(&h).is_err());
    h.address = "fe80::1%en0".into();
    assert!(check_host(&h).is_ok());
    h.baseline = Some("../../etc/x".into());
    assert!(check_host(&h).unwrap_err().contains("baseline"));
    h.baseline = None;
    h.user = "-l".into();
    assert!(check_host(&h).unwrap_err().contains("user"));
    h.user = "labadmin".into();
    h.uuid = "not-a-uuid".into();
    assert!(check_host(&h).unwrap_err().contains("uuid"));
}

/// A selector means one host, even when another host uses that word in
/// a different field.
#[test]
fn a_selector_must_not_expand_an_ambiguous_word_into_other_hosts() {
    let unique = parse(HAND_EDITED).expect("ordinary inventory");
    for word in ["lab-07", "10.0.0.7", UUID_7] {
        let got = select(&unique, &[word.to_ascii_lowercase()], false).expect("unique host");
        assert_eq!(
            got.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["lab-07"]
        );
    }
    let repeated = select(
        &unique,
        &["lab-08".into(), UUID_7.into(), "10.0.0.7".into()],
        false,
    )
    .expect("explicit unique hosts, with a repeated identity");
    assert_eq!(
        repeated.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
        ["lab-07", "lab-08"]
    );
    assert_eq!(select(&unique, &[], true).expect("explicit all").len(), 2);
    assert!(select(&unique, &["missing".into()], false).is_err());
    assert!(select(&unique, &["lab-07".into()], true).is_err());

    let cases = [
        (
            "name/address",
            HAND_EDITED.replace("address = \"lab-08.local\"", "address = \"lab-07\""),
            "LAB-07".to_owned(),
        ),
        (
            "uuid/name",
            HAND_EDITED.replace("name = \"lab-08\"", &format!("name = \"{UUID_7}\"")),
            UUID_7.to_ascii_lowercase(),
        ),
        (
            "shared address",
            HAND_EDITED.replace("address = \"lab-08.local\"", "address = \"10.0.0.7\""),
            "10.0.0.7".to_owned(),
        ),
    ];
    let observed: Vec<_> = cases
        .into_iter()
        .map(|(kind, text, word)| {
            let inv = parse(&text).expect("valid inventory with distinct names and UUIDs");
            (kind, select(&inv, &[word], false).is_err())
        })
        .collect();
    assert!(
        observed.iter().all(|(_, refused)| *refused),
        "an ambiguous selector must be refused before choosing hosts: {observed:?}"
    );
}

#[test]
fn upserting_a_new_host_appends_it_and_keeps_every_comment() {
    let fake = fake_with(HAND_EDITED);
    let new = host("lab-09", "0A1B2C3D-0000-4000-8000-000000000009", "10.0.0.9");
    upsert_host(&fake, &paths(), &new).unwrap();
    let out = written_inventory(&fake);
    let kept = out.replace(
        "\n[[host]]\nname = \"lab-09\"\nuuid = \"0A1B2C3D-0000-4000-8000-000000000009\"\naddress = \"10.0.0.9\"\nuser = \"labadmin\"\n",
        "",
    );
    assert_eq!(kept, HAND_EDITED, "{out}");
    assert!(out.find("lab-09").unwrap() < out.find("# end of hosts").unwrap());
    let inv = parse(&out).unwrap();
    assert_eq!(inv.hosts.len(), 3);
    assert_eq!(inv.hosts[2], new);
    assert!(
        fake.calls()
            .contains(&"write /ctl/machines/machines.toml 600".to_string())
    );
}

#[test]
fn upserting_a_known_uuid_rewrites_only_what_changed_in_place() {
    let fake = fake_with(HAND_EDITED);
    let moved = host("lab-07", &UUID_7.to_ascii_lowercase(), "10.0.1.7");
    upsert_host(&fake, &paths(), &moved).unwrap();
    let out = written_inventory(&fake);
    assert_eq!(
        out,
        HAND_EDITED.replace(
            "address = \"10.0.0.7\"  # wired port 7",
            "address = \"10.0.1.7\"  # wired port 7"
        )
    );
    let inv = parse(&out).unwrap();
    assert_eq!(inv.hosts[0].baseline.as_deref(), Some("lab"));
    assert_eq!(inv.hosts[0].uuid, UUID_7);
}

#[test]
fn upserting_an_unchanged_host_writes_the_file_back_unchanged() {
    let fake = fake_with(HAND_EDITED);
    let mut same = host("lab-08", UUID_8, "lab-08.local");
    same.users = Vec::new();
    upsert_host(&fake, &paths(), &same).unwrap();
    assert_eq!(written_inventory(&fake), HAND_EDITED);
}

#[test]
fn upserting_writes_users_and_a_baseline_when_the_host_carries_them() {
    let fake = fake_with(HAND_EDITED);
    let mut h = host("lab-08", UUID_8, "lab-08.local");
    h.baseline = Some("kiosk".into());
    h.users = vec!["labadmin".into(), "student".into()];
    upsert_host(&fake, &paths(), &h).unwrap();
    let inv = parse(&written_inventory(&fake)).unwrap();
    assert_eq!(inv.hosts[1], h);
    assert_eq!(inv.hosts[0].baseline.as_deref(), Some("lab"));
}

#[test]
fn a_name_another_host_holds_is_refused() {
    let fake = fake_with(HAND_EDITED);
    let thief = host("lab-08", "0A1B2C3D-0000-4000-8000-000000000009", "10.0.0.9");
    let err = upsert_host(&fake, &paths(), &thief).unwrap_err();
    assert!(err.contains("taken"), "{err}");
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("write")),
        "{:?}",
        fake.calls()
    );
}

#[test]
fn a_new_host_lands_after_the_last_host_even_with_a_table_below_it() {
    let text = "schema = 1\n\n[[host]]\nname = \"lab-07\"\n# the one with the loud fan\nuuid = \"0A1B2C3D-0000-4000-8000-000000000007\"\naddress = \"10.0.0.7\"\nuser = \"labadmin\"\n\n# The machine user.\n[github]\naccount = \"lab-bot\"\n";
    let fake = fake_with(text);
    upsert_host(
        &fake,
        &paths(),
        &host("lab-09", "0A1B2C3D-0000-4000-8000-000000000009", "10.0.0.9"),
    )
    .unwrap();
    let out = written_inventory(&fake);
    assert!(
        out.find("lab-09").unwrap() < out.find("[github]").unwrap(),
        "{out}"
    );
    assert!(out.contains("# the one with the loud fan\n"), "{out}");
    assert!(out.contains("# The machine user.\n[github]"), "{out}");
    let inv = parse(&out).unwrap();
    assert_eq!(inv.hosts.len(), 2);
    assert_eq!(inv.github.unwrap().account, "lab-bot");
    let fake = fake_with(text);
    upsert_host(&fake, &paths(), &host("lab-07", UUID_7, "10.0.9.7")).unwrap();
    assert_eq!(
        written_inventory(&fake),
        text.replace("10.0.0.7", "10.0.9.7")
    );
}

#[test]
fn the_first_host_goes_into_an_inventory_that_has_none() {
    let fake = fake_with("schema = 1\n\n[controller]\n");
    upsert_host(&fake, &paths(), &host("lab-01", UUID_7, "10.0.0.1")).unwrap();
    let out = written_inventory(&fake);
    assert!(out.starts_with("schema = 1\n\n[controller]\n"), "{out}");
    assert_eq!(parse(&out).unwrap().hosts.len(), 1);
}

#[test]
fn retiring_a_host_comments_its_block_out_and_keeps_the_rest() {
    let fake = fake_with(HAND_EDITED);
    fake.file(&format!("{ROOT}/state/{UUID_7}.json"), b"{}");
    fake.file(
        &format!("{ROOT}/known_hosts"),
        format!(
            "{} ssh-ed25519 AAAAC3Nza7\n{UUID_8} ssh-ed25519 AAAAC3Nza8\n",
            UUID_7.to_ascii_lowercase()
        )
        .as_bytes(),
    );
    retire_host(&fake, &paths(), UUID_7).unwrap();
    let out = written_inventory(&fake);
    let expected = HAND_EDITED.replace(
        "[[host]]\nname = \"lab-07\"\nuuid = \"0A1B2C3D-0000-4000-8000-000000000007\"\naddress = \"10.0.0.7\"  # wired port 7\nuser = \"labadmin\"\nbaseline = \"lab\"\n",
        "# Retired by `aterm machines unenroll` (unix time 1800000000): lab-07 0A1B2C3D-0000-4000-8000-000000000007\n\
         # [[host]]\n# name = \"lab-07\"\n# uuid = \"0A1B2C3D-0000-4000-8000-000000000007\"\n\
         # address = \"10.0.0.7\"  # wired port 7\n# user = \"labadmin\"\n# baseline = \"lab\"\n",
    );
    assert_eq!(out, expected);
    let inv = parse(&out).unwrap();
    assert_eq!(inv.hosts.len(), 1);
    assert_eq!(inv.hosts[0].name, "lab-08");
    assert!(fake.calls().contains(&format!(
        "rename {ROOT}/state/{UUID_7}.json {ROOT}/state/retired/{UUID_7}.json"
    )));
    assert_eq!(
        fake.written(&format!("{ROOT}/known_hosts")).unwrap(),
        format!("{UUID_8} ssh-ed25519 AAAAC3Nza8\n").as_bytes()
    );
}

#[test]
fn retiring_the_last_host_of_a_file_without_a_final_newline_still_parses() {
    let text = "schema = 1\n\n[[host]]\nname = \"a\"\nuuid = \"0A1B2C3D-0000-4000-8000-000000000007\"\naddress = \"a.local\"\nuser = \"u\"";
    let fake = fake_with(text);
    retire_host(&fake, &paths(), UUID_7).unwrap();
    let out = written_inventory(&fake);
    assert!(parse(&out).unwrap().hosts.is_empty(), "{out}");
    assert!(out.ends_with("# user = \"u\"\n"), "{out}");
}

#[test]
fn retiring_an_unknown_uuid_changes_nothing() {
    let fake = fake_with(HAND_EDITED);
    let err = retire_host(&fake, &paths(), "0A1B2C3D-0000-4000-8000-0000000000FF").unwrap_err();
    assert!(err.contains("no host with uuid"), "{err}");
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

fn inventory() -> Inventory {
    parse(HAND_EDITED).unwrap()
}

#[test]
fn hosts_are_selected_by_name_uuid_or_address_in_inventory_order() {
    let inv = inventory();
    let names: Vec<String> = vec!["LAB-08.local".into(), UUID_7.to_ascii_lowercase()];
    let picked = select(&inv, &names, false).unwrap();
    assert_eq!(
        picked.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
        ["lab-07", "lab-08"]
    );
    let twice = select(&inv, &["lab-07".into(), "10.0.0.7".into()], false).unwrap();
    assert_eq!(twice.len(), 1);
    assert_eq!(select(&inv, &[], true).unwrap().len(), 2);
}

#[test]
fn unknown_names_are_listed_with_the_hosts_there_are() {
    let inv = inventory();
    let err = select(
        &inv,
        &["lab-99".into(), "lab-07".into(), "lab-98".into()],
        false,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "no host lab-99, lab-98 in machines.toml (hosts: lab-07, lab-08)"
    );
    assert!(select(&inv, &[], false).unwrap_err().contains("--all"));
    assert!(select(&inv, &["lab-07".into()], true).is_err());
}

#[test]
fn the_serial_is_stored_before_it_is_returned_and_never_goes_back() {
    let fake = Fake::root();
    let p = paths();
    assert_eq!(next_serial(&fake, &p, 0).unwrap(), 1);
    assert_eq!(fake.written("/ctl/machines/serial").unwrap(), b"1\n");
    assert_eq!(next_serial(&fake, &p, 0).unwrap(), 2);
    assert_eq!(next_serial(&fake, &p, 40).unwrap(), 41);
    assert_eq!(next_serial(&fake, &p, 3).unwrap(), 42);
    assert_eq!(fake.written("/ctl/machines/serial").unwrap(), b"42\n");
    assert!(
        fake.calls()
            .contains(&"write /ctl/machines/serial 600".to_string())
    );
}

#[test]
fn a_corrupt_serial_is_refused_rather_than_reset() {
    let fake = Fake::root();
    fake.file("/ctl/machines/serial", b"forty\n");
    assert!(
        next_serial(&fake, &paths(), 0)
            .unwrap_err()
            .contains("not a serial")
    );
    fake.file("/ctl/machines/serial", format!("{}\n", u64::MAX).as_bytes());
    assert!(next_serial(&fake, &paths(), 0).is_err());
}

#[test]
fn state_round_trips_and_a_missing_file_is_the_default() {
    let fake = Fake::root();
    let p = paths();
    assert_eq!(load_state(&fake, &p, UUID_7).unwrap(), HostState::default());
    let state = HostState {
        hello: Some("{\"schema\":1}".into()),
        last_seen: 1_800_000_000,
        unreachable_since: Some(1_800_000_100),
        report: None,
        applied_build: Some("26A428".into()),
        github_keys: vec![("labadmin".into(), 123_456_789)],
        github_key_text: vec![(123_456_789, "ssh-ed25519 AAAA".into())],
        github_revoked: vec!["ssh-ed25519 BBBB".into()],
        contact_error: Some((1_800_000_050, "its host key changed".into())),
        last_refusal: Some((1_800_000_060, "busy run=7".into())),
        signers_sha256: Some("ab".repeat(32)),
        only_sent: vec![(9, vec!["names".into()])],
        only: Some(Only {
            tables: vec!["names".into()],
            rest: Some(1_799_000_000),
        }),
    };
    save_state(&fake, &p, UUID_7, &state).unwrap();
    assert!(fake.calls().contains(&format!("mkdir {ROOT}/state 700")));
    assert!(
        fake.calls()
            .contains(&format!("write {ROOT}/state/{UUID_7}.json 600"))
    );
    assert_eq!(load_state(&fake, &p, UUID_7).unwrap(), state);
}

#[test]
fn state_for_a_word_that_is_not_a_uuid_is_refused_before_any_path_is_built() {
    let fake = Fake::root();
    assert!(load_state(&fake, &paths(), "../../x").is_err());
    assert!(save_state(&fake, &paths(), "../../x", &HostState::default()).is_err());
    assert!(fake.calls().is_empty());
}

#[test]
fn a_state_file_that_does_not_parse_is_an_error_not_the_default() {
    let fake = Fake::root();
    fake.file(&format!("{ROOT}/state/{UUID_7}.json"), b"{not json");
    assert!(load_state(&fake, &paths(), UUID_7).is_err());
}

#[test]
fn a_control_path_is_eight_hex_digits_under_the_root_when_that_fits() {
    let p = paths();
    let path = p.control_path("lab-07.local");
    assert_eq!(path.parent(), Some(Path::new("/ctl/machines/keys/ctl")));
    let name = path.file_name().unwrap().to_str().unwrap();
    assert_eq!(name.len(), 8);
    assert!(name.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(p.control_path("lab-07.local"), path, "stable per host");
    assert_ne!(p.control_path("lab-08.local"), path);
}

#[test]
fn a_deep_root_puts_control_sockets_under_private_tmp() {
    let deep = Paths {
        root: PathBuf::from(format!("/Users//someone/{}/machines", "x".repeat(60))),
    };
    let path = deep.control_path("lab-07.local");
    assert_eq!(
        path.parent().unwrap(),
        Path::new(&format!(
            "/private/tmp/aterm-machines-{}",
            aterm_uds::peer::our_uid()
        ))
    );
    assert!(path.as_os_str().len() <= CONTROL_PATH_MAX);
    let fits = "/".to_string() + &"r".repeat(CONTROL_PATH_MAX - "/keys/ctl/".len() - 8 - 1);
    let edge = Paths {
        root: PathBuf::from(&fits),
    };
    assert_eq!(edge.control_path("h").as_os_str().len(), CONTROL_PATH_MAX);
    assert!(edge.control_path("h").starts_with(&fits));
}

#[test]
fn a_control_dir_another_account_owns_or_can_enter_is_refused() {
    let p = paths();
    let dir = p.control_dir();
    let fake = Fake::root();
    let mut st = Stat {
        uid: 0,
        gid: 0,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    };
    fake.stat_of(dir.to_str().unwrap(), st);
    assert_eq!(p.ensure_control_dir(&fake).unwrap(), dir);
    st.uid = 502;
    fake.stat_of(dir.to_str().unwrap(), st);
    assert!(p.ensure_control_dir(&fake).unwrap_err().contains("uid 502"));
    st.uid = 0;
    st.mode = 0o755;
    fake.stat_of(dir.to_str().unwrap(), st);
    assert!(
        p.ensure_control_dir(&fake)
            .unwrap_err()
            .contains("chmod 700")
    );
    st.mode = 0o700;
    st.symlink = true;
    fake.stat_of(dir.to_str().unwrap(), st);
    assert!(p.ensure_control_dir(&fake).is_err());
}

#[test]
fn ensure_makes_the_root_and_its_directories_private() {
    let fake = Fake::root();
    paths().ensure(&fake).unwrap();
    assert_eq!(
        fake.calls(),
        [
            "mkdir /ctl/machines 700",
            "mkdir /ctl/machines/keys 700",
            "mkdir /ctl/machines/baselines 700",
            "mkdir /ctl/machines/state 700",
            "mkdir /ctl/machines/cache 700",
        ]
    );
}

#[test]
fn a_path_that_climbs_out_of_the_root_is_not_inside_it() {
    let root = Path::new("/ctl/machines/baselines");
    assert!(inside(root, Path::new("/ctl/machines/baselines/lab.toml")));
    assert!(!inside(
        root,
        Path::new("/ctl/machines/baselines/../keys/signing")
    ));
    assert!(!inside(root, Path::new("/ctl/machines/other.toml")));
}

#[test]
fn a_uuid_is_five_groups_of_hex_digits() {
    assert!(is_uuid("0A1B2C3D-0000-4000-8000-000000000007"));
    assert!(is_uuid("0a1b2c3d-0000-4000-8000-000000000007"));
    assert!(!is_uuid("0A1B2C3D-0000-4000-8000-00000000000"));
    assert!(!is_uuid("0A1B2C3D-0000-4000-8000-00000000000G"));
    assert!(!is_uuid("../0000-4000-8000-000000000007"));
}

/// mDNS reads `.local` without regard to case, so `M14.LOCAL.` is the host `m14.local`
/// names. NEGATIVE CONTROL: a name that only ends in `local` keeps it.
#[test]
fn a_dot_local_suffix_in_any_case_names_the_same_host() {
    let empty = std::collections::BTreeMap::new();
    for spelled in ["M14.LOCAL.", "m14.Local", "m14.local", "m14"] {
        assert_eq!(host_tag_key(spelled), "m14", "{spelled}");
    }
    assert_eq!(
        canonical_host_name("M1-Air-M2-24GB-1TB.LOCAL.", &empty),
        canonical_host_name("m1-Air-M2-24GB-1TB", &empty)
    );
    assert_eq!(
        canonical_host_name("spark-49f7-SSH.Local", &empty),
        "spark-49f7"
    );
    assert_eq!(host_tag_key("buildlocal"), "buildlocal");
    assert_eq!(host_tag_key(".LOCAL"), "");
}

#[test]
fn canonical_host_name_normalizes_inconsistent_bonjour_names_and_honors_custom_names() {
    let empty = std::collections::BTreeMap::new();
    assert_eq!(
        canonical_host_name("m1 Air M2 24GB 1TB", &empty),
        "m1-MacBook-Air-M2-24GB-1TB"
    );
    assert_eq!(
        canonical_host_name("m4 MacBook Pro 3.1GHz i7 16GB 1TB", &empty),
        "m4-MacBook-Pro-3.1GHz-i7-16GB-1TB"
    );
    assert_eq!(
        canonical_host_name("m8 Macbook Pro M4 Max 36GB 1TB", &empty),
        "m8-MacBook-Pro-M4-Max-36GB-1TB"
    );
    assert_eq!(
        canonical_host_name("m9 MacBook Pro  M2 32GB 8TB", &empty),
        "m9-MacBook-Pro-M2-32GB-8TB"
    );
    assert_eq!(
        canonical_host_name("m10 MacBook M4 Max 36GB 1TB", &empty),
        "m10-MacBook-Pro-M4-Max-36GB-1TB"
    );
    assert_eq!(
        canonical_host_name("m14-Air-M5-32GB-4TB-prime", &empty),
        "m14-MacBook-Air-M5-32GB-4TB-prime"
    );
    assert_eq!(
        canonical_host_name("m100 macbook pro m4 pro 48gb 500gb", &empty),
        "m100-MacBook-Pro-M4-Pro-48GB-500GB"
    );
    assert_eq!(canonical_host_name("spark-49f7-SSH", &empty), "spark-49f7");
    assert_eq!(canonical_host_name("gb tb ghz", &empty), "gb-tb-ghz");
    assert_eq!(canonical_host_name("m Air", &empty), "m-Air");

    let fake = fake_with(HAND_EDITED);
    set_host_name(&fake, &paths(), "m14", "m14-prime").unwrap();
    set_host_name(&fake, &paths(), "lab-07", "bench-a-07").unwrap();
    let inv = load(&fake, &paths()).unwrap();
    assert_eq!(inv.hosts[0].name, "bench-a-07");
    assert_eq!(
        canonical_host_name("m14 Air M5 32GB 4TB prime", &inv.names),
        "m14-prime"
    );
}
