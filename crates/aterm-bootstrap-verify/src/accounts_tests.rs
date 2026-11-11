// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_files_are_the_answer_only_when_nothing_else_is_asked() {
    assert_eq!(Accounts::files_answer(None), Some(false));
    let plain = "passwd: files\ngroup: files\nhosts: files dns\n";
    assert_eq!(Accounts::files_answer(Some(plain)), Some(false));
    let ubuntu =
        "# comment\npasswd:         files systemd\ngroup:          files [SUCCESS=merge] systemd\n";
    assert_eq!(Accounts::files_answer(Some(ubuntu)), Some(true));
    let multi_criteria = "passwd: files[NOTFOUND=continue UNAVAIL=return] systemd\ngroup: files\n";
    assert_eq!(Accounts::files_answer(Some(multi_criteria)), Some(true));
    for elsewhere in [
        "passwd: files sss\ngroup: files\n",
        "passwd: files\ngroup: files ldap\n",
        "passwd: compat\n",
        "passwd: files [NOTFOUND=continue]ldap\ngroup: files\n",
        "passwd: files [NOTFOUND=continue sss\ngroup: files\n",
        "passwd: [NOTFOUND=return] files\ngroup: files\n",
        "passwd: files]\ngroup: files\n",
        "passwd:\ngroup: files\n",
        "passwd: systemd\ngroup: files\n",
        "passwd: files\npasswd: files ldap\ngroup: files\n",
    ] {
        assert_eq!(Accounts::files_answer(Some(elsewhere)), None, "{elsewhere}");
    }
}

#[test]
fn a_line_this_reader_is_unsure_of_admits_no_group() {
    let passwd =
        b"# users\nroot:x:0:0:root:/root:/bin/bash\n\nann:x:1000:1000::/home/ann:/bin/bash\n";
    let group = b"root:x:0:\nann:x:1000:\nshared:x:1001:ann,bob\n";
    let accounts = Accounts {
        passwd: Accounts::parse_passwd(passwd).expect("parses"),
        groups: Accounts::parse_group(group).expect("parses"),
    };
    assert!(accounts.private_group(1000, 1000));
    assert!(
        !accounts.private_group(1000, 1001),
        "a group with another member"
    );
    assert!(
        !accounts.private_group(0, 0),
        "root's group is no one's private one"
    );
    let duplicate_gid = Accounts {
        passwd: Accounts::parse_passwd(passwd).expect("parses"),
        groups: Accounts::parse_group(b"ann:x:1000:\nbuilders:x:1000:bob\n").expect("parses"),
    };
    assert!(
        !duplicate_gid.private_group(1000, 1000),
        "a second group line sharing gid 1000 grants supplementary membership"
    );
    let duplicate_uid = Accounts {
        passwd: Accounts::parse_passwd(
            b"ann:x:1000:1000::/home/ann:/bin/bash\nalias:x:1000:1000::/home/ann:/bin/bash\n",
        )
        .expect("parses"),
        groups: Accounts::parse_group(b"ann:x:1000:\n").expect("parses"),
    };
    assert!(
        !duplicate_uid.private_group(1000, 1000),
        "divergent passwd entries for uid 1000 are not unambiguous"
    );
    assert_eq!(accounts.primary_members(1000), [1000]);
    assert_eq!(Accounts::parse_passwd(b"+@netgroup::::::\n"), None);
    assert_eq!(Accounts::parse_passwd(b"ann:x:1000\n"), None);
    assert_eq!(Accounts::parse_group(b"ann:x:ten:\n"), None);
}

#[test]
fn a_userdb_record_beside_systemd_ends_the_enumeration() {
    let root = aterm_tempfile::Builder::new().tempdir().expect("scratch");
    let at = |rel: &str| root.path().join(rel);
    std::fs::create_dir_all(at("etc")).expect("mkdir");
    std::fs::write(
        at("etc/nsswitch.conf"),
        "passwd: files systemd\ngroup: files systemd\n",
    )
    .expect("write");
    std::fs::write(at("etc/passwd"), "ann:x:1000:1000::/home/ann:/bin/sh\n").expect("write");
    std::fs::write(at("etc/group"), "ann:x:1000:\n").expect("write");
    assert!(
        Accounts::read_under(root.path()).is_some_and(|a| a.private_group(1000, 1000)),
        "the files, read whole"
    );
    std::fs::create_dir_all(at("run/userdb")).expect("mkdir");
    std::fs::write(at("run/userdb/bob.user"), "{}").expect("write");
    assert_eq!(
        Accounts::read_under(root.path()),
        None,
        "a record only systemd serves"
    );
}
