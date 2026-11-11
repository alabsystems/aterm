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

fn sources<'a>(nsswitch: &'a str, database: &str) -> Result<Option<Vec<&'a str>>, ()> {
    let mut matched = None;
    for line in nsswitch.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != database {
            continue;
        }
        if matched.is_some() {
            return Err(());
        }
        let mut words = Vec::new();
        let mut in_action = false;
        let mut token_start: Option<usize> = None;
        for (idx, ch) in rest.char_indices() {
            if in_action {
                match ch {
                    '[' => return Err(()),
                    ']' => in_action = false,
                    _ => {}
                }
            } else {
                match ch {
                    '[' => {
                        if let Some(start) = token_start.take() {
                            words.push(&rest[start..idx]);
                        } else if words.is_empty() {
                            return Err(());
                        }
                        in_action = true;
                    }
                    ']' => return Err(()),
                    c if c.is_whitespace() => {
                        if let Some(start) = token_start.take() {
                            words.push(&rest[start..idx]);
                        }
                    }
                    _ => {
                        if token_start.is_none() {
                            token_start = Some(idx);
                        }
                    }
                }
            }
        }
        if in_action {
            return Err(());
        }
        if let Some(start) = token_start {
            words.push(&rest[start..]);
        }
        matched = Some(words);
    }
    Ok(matched)
}

impl Accounts {
    /// Whether `nsswitch` leaves `passwd` and `group` to files, and whether it lists `systemd`
    /// beside them.
    #[must_use]
    pub fn files_answer(nsswitch: Option<&str>) -> Option<bool> {
        let mut systemd = false;
        for database in ["passwd", "group"] {
            let listed = match nsswitch {
                Some(text) => sources(text, database).ok()?,
                None => None,
            };
            let listed = listed.unwrap_or_else(|| vec!["files"]);
            if !listed.contains(&"files") {
                return None;
            }
            for source in listed {
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
            .map(|line| {
                let mut fields = line?.split(|b| *b == b':');
                let (Some(name), Some(_), Some(uid), Some(gid)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
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
            .map(|line| {
                let mut fields = line?.split(|b| *b == b':');
                let (Some(name), Some(_), Some(gid), Some(members)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
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
        const ACCOUNT_FILE_MAX: u64 = 16 * 1024 * 1024;
        let under = |path: &str| root.join(path.trim_start_matches('/'));
        let nsswitch = match read_regular_bounded(&under("/etc/nsswitch.conf"), ACCOUNT_FILE_MAX) {
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
            passwd: Self::parse_passwd(
                &read_regular_bounded(&under("/etc/passwd"), ACCOUNT_FILE_MAX).ok()?,
            )?,
            groups: Self::parse_group(
                &read_regular_bounded(&under("/etc/group"), ACCOUNT_FILE_MAX).ok()?,
            )?,
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
        let primaries = self.primary_members(gid);
        let mut saw_account = false;
        for account in self.passwd.iter().filter(|account| account.uid == uid) {
            saw_account = true;
            let mut saw_group = false;
            for group in self.groups.iter().filter(|group| group.gid == gid) {
                saw_group = true;
                if !user_private_group(uid, gid, Some(account), Some(group), &primaries) {
                    return false;
                }
            }
            if !saw_group {
                return false;
            }
        }
        saw_account
    }
}

fn read_regular_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "account file grew past cap",
        ));
    }
    Ok(bytes)
}

fn entries(text: &[u8]) -> impl Iterator<Item = Option<&[u8]>> {
    text.split(|b| *b == b'\n')
        .map(<[u8]>::trim_ascii_start)
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|line| (!line.starts_with(b"+") && !line.starts_with(b"-")).then_some(line))
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
#[path = "accounts_tests.rs"]
mod tests;
