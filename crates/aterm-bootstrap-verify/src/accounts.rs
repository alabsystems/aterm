// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! User-private-group rule ([`user_private_group`]) over this machine's accounts.

use std::path::Path;

/// One passwd entry, as far as the rule reads it: the name and the primary group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: Vec<u8>,
    pub uid: u32,
    pub gid: u32,
}

/// One group entry: its name, gid and listed members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: Vec<u8>,
    pub gid: u32,
    pub members: Vec<Vec<u8>>,
}

/// Whether group `gid` can hold no account but `uid`.
#[must_use]
pub fn user_private_group(
    uid: u32,
    gid: u32,
    account: Option<&Account>,
    group: Option<&Group>,
    primaries: &[u32],
) -> bool {
    let (Some(account), Some(group)) = (account, group) else {
        return false;
    };
    uid != 0
        && account.gid == gid
        && group.name == account.name
        && group.members.iter().all(|member| *member == account.name)
        && primaries.iter().all(|other| *other == uid)
}

/// The directories systemd's userdb reads JSON user and group records from.
pub const USERDB_DIRS: [&str; 5] = [
    "/etc/userdb",
    "/run/userdb",
    "/run/host/userdb",
    "/usr/local/lib/userdb",
    "/usr/lib/userdb",
];

/// This machine's accounts, when the files are the whole answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accounts {
    pub passwd: Vec<Account>,
    pub groups: Vec<Group>,
}

fn sources<'a>(nsswitch: &'a str, database: &str) -> Option<Vec<&'a str>> {
    nsswitch.lines().find_map(|line| {
        let line = line.split('#').next().unwrap_or_default().trim();
        let (name, rest) = line.split_once(':')?;
        if name.trim() != database {
            return None;
        }
        let mut words = Vec::new();
        let mut action = false;
        for word in rest.split_whitespace() {
            if word.starts_with('[') {
                action = true;
            }
            if !action {
                words.push(word);
            }
            if word.ends_with(']') {
                action = false;
            }
        }
        Some(words)
    })
}

impl Accounts {
    /// Whether `nsswitch` leaves `passwd` and `group` to files and lists `systemd` beside them.
    #[must_use]
    pub fn files_answer(nsswitch: Option<&str>) -> Option<bool> {
        let mut systemd = false;
        for database in ["passwd", "group"] {
            let listed = nsswitch.and_then(|text| sources(text, database));
            for source in listed.unwrap_or_else(|| vec!["files"]) {
                match source {
                    "files" => {}
                    "systemd" => systemd = true,
                    _ => return None,
                }
            }
        }
        Some(systemd)
    }

    /// `/etc/passwd`'s entries, or `None` for one this reader cannot be sure of.
    #[must_use]
    pub fn parse_passwd(text: &[u8]) -> Option<Vec<Account>> {
        entries(text)
            .map(|fields| {
                let [name, _, uid, gid, ..] = fields.as_slice() else {
                    return None;
                };
                Some(Account {
                    name: name.to_vec(),
                    uid: number(uid)?,
                    gid: number(gid)?,
                })
            })
            .collect()
    }

    /// `/etc/group`'s entries, or `None` for one this reader cannot be sure of.
    #[must_use]
    pub fn parse_group(text: &[u8]) -> Option<Vec<Group>> {
        entries(text)
            .map(|fields| {
                let [name, _, gid, members, ..] = fields.as_slice() else {
                    return None;
                };
                Some(Group {
                    name: name.to_vec(),
                    gid: number(gid)?,
                    members: members
                        .split(|b| *b == b',')
                        .filter(|member| !member.is_empty())
                        .map(<[u8]>::to_vec)
                        .collect(),
                })
            })
            .collect()
    }

    /// This machine's accounts from `root`'s `etc` and `run` (`/` outside the tests).
    #[must_use]
    pub fn read_under(root: &Path) -> Option<Self> {
        let under = |path: &str| root.join(path.trim_start_matches('/'));
        let nsswitch = match std::fs::read(under("/etc/nsswitch.conf")) {
            Ok(bytes) => Some(String::from_utf8(bytes).ok()?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let systemd = Self::files_answer(nsswitch.as_deref())?;
        if systemd
            && USERDB_DIRS
                .iter()
                .any(|dir| match std::fs::read_dir(under(dir)) {
                    Ok(mut entries) => entries.next().is_some(),
                    Err(e) => e.kind() != std::io::ErrorKind::NotFound,
                })
        {
            return None;
        }
        Some(Self {
            passwd: Self::parse_passwd(&std::fs::read(under("/etc/passwd")).ok()?)?,
            groups: Self::parse_group(&std::fs::read(under("/etc/group")).ok()?)?,
        })
    }

    /// `getpwuid(uid)`: the first entry for it.
    #[must_use]
    pub fn account(&self, uid: u32) -> Option<&Account> {
        self.passwd.iter().find(|account| account.uid == uid)
    }

    /// `getgrgid(gid)`: the first entry for it.
    #[must_use]
    pub fn group(&self, gid: u32) -> Option<&Group> {
        self.groups.iter().find(|group| group.gid == gid)
    }

    /// The uids of every passwd entry whose primary group is `gid`, in file order.
    #[must_use]
    pub fn primary_members(&self, gid: u32) -> Vec<u32> {
        self.passwd
            .iter()
            .filter(|account| account.gid == gid)
            .map(|account| account.uid)
            .collect()
    }

    /// [`user_private_group`] of `gid` for `uid` over these accounts.
    #[must_use]
    pub fn private_group(&self, uid: u32, gid: u32) -> bool {
        user_private_group(
            uid,
            gid,
            self.account(uid),
            self.group(gid),
            &self.primary_members(gid),
        )
    }
}

fn entries(text: &[u8]) -> impl Iterator<Item = Vec<&[u8]>> {
    text.split(|b| *b == b'\n')
        .map(|line| line.trim_ascii_start())
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|line| {
            if line.starts_with(b"+") || line.starts_with(b"-") {
                Vec::new()
            } else {
                line.split(|b| *b == b':').collect()
            }
        })
}

fn number(field: &[u8]) -> Option<u32> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Whether `gid` is `uid`'s private group on this machine.
#[must_use]
pub fn private_group(uid: u32, gid: u32) -> bool {
    cfg!(target_os = "linux")
        && Accounts::read_under(Path::new("/"))
            .is_some_and(|accounts| accounts.private_group(uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_are_the_answer_only_when_nothing_else_is_asked() {
        assert_eq!(Accounts::files_answer(None), Some(false));
        let plain = "passwd: files\ngroup: files\nhosts: files dns\n";
        assert_eq!(Accounts::files_answer(Some(plain)), Some(false));
        let ubuntu = "# comment\npasswd:         files systemd\ngroup:          files [SUCCESS=merge] systemd\n";
        assert_eq!(Accounts::files_answer(Some(ubuntu)), Some(true));
        for elsewhere in [
            "passwd: files sss\ngroup: files\n",
            "passwd: files\ngroup: files ldap\n",
            "passwd: compat\n",
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
}
