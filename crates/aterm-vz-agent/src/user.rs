// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lane user, read from `/etc/passwd` and `/etc/group` before each fork.
//!
//! The bake makes the lane user with the host's name and uid and home
//! `/home/<name>.guest` (lima's convention, the design's §4 and §8), so the agent finds it
//! as the one account whose home is so named, and the unit it runs under names no user.

use crate::system::Identity;

/// One `/etc/passwd` account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// Its name.
    pub name: String,
    /// Its uid.
    pub uid: u32,
    /// Its primary gid.
    pub gid: u32,
    /// Its home directory.
    pub home: String,
}

fn parse_u32(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Every well-formed account in `passwd`'s text (`name:x:uid:gid:gecos:home:shell`).
fn accounts(passwd: &str) -> impl Iterator<Item = (&str, u32, u32, &str)> + '_ {
    passwd.lines().filter_map(|line| {
        let mut f = [""; 7];
        let mut count = 0;
        for part in line.split(':') {
            if count >= 7 {
                return None;
            }
            f[count] = part;
            count += 1;
        }
        if count != 7 || f[0].is_empty() || f[0].starts_with('#') {
            return None;
        }
        Some((f[0], parse_u32(f[2])?, parse_u32(f[3])?, f[5]))
    })
}

/// The lane user: the one non-root account whose home is `/home/<name>.guest`.
///
/// # Errors
/// None is, or more than one is, or its uid or gid is 0: which.
pub fn lane_user(passwd: &str) -> Result<User, String> {
    let mut matches = accounts(passwd).filter(|(name, _, _, home)| {
        !name.contains('/')
            && home
                .strip_prefix("/home/")
                .and_then(|s| s.strip_suffix(".guest"))
                == Some(*name)
    });
    let Some((name, uid, gid, home)) = matches.next() else {
        return Err("no lane user: no account in /etc/passwd has home /home/<name>.guest".into());
    };
    if let Some((second, _, _, _)) = matches.next() {
        let mut names = vec![name, second];
        names.extend(matches.map(|(n, _, _, _)| n));
        return Err(format!(
            "more than one lane user in /etc/passwd: {}",
            names.join(", ")
        ));
    }
    if uid == 0 {
        return Err(format!(
            "lane user '{name}' in /etc/passwd has uid 0 (root)"
        ));
    }
    if gid == 0 {
        return Err(format!(
            "lane user '{name}' in /etc/passwd has gid 0 (root)"
        ));
    }
    Ok(User {
        name: name.to_string(),
        uid,
        gid,
        home: home.to_string(),
    })
}

/// `user`'s groups from `group`'s text (`name:x:gid:member,member`): its primary gid
/// first, then each group that lists it by name, once each.
#[must_use]
pub fn groups(group: &str, user: &User) -> Vec<u32> {
    let mut out = vec![user.gid];
    for line in group.lines() {
        let mut f = [""; 4];
        let mut count = 0;
        for part in line.split(':') {
            if count >= 4 {
                count += 1;
                break;
            }
            f[count] = part;
            count += 1;
        }
        if count != 4 || f[0].is_empty() || f[0].starts_with('#') {
            continue;
        }
        let Some(gid) = parse_u32(f[2]) else {
            continue;
        };
        if !out.contains(&gid) && f[3].split(',').any(|m| m.trim() == user.name) {
            out.push(gid);
        }
    }
    out
}

/// The lane user's [`Identity::Switch`], from the two files' text.
///
/// # Errors
/// [`lane_user`]'s.
pub fn identity(passwd: &str, group: &str) -> Result<Identity, String> {
    let user = lane_user(passwd)?;
    let groups = groups(group, &user);
    Ok(Identity::Switch {
        uid: user.uid,
        gid: user.gid,
        groups,
    })
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
