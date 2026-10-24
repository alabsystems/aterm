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

/// Every well-formed account in `passwd`'s text (`name:x:uid:gid:gecos:home:shell`).
fn accounts(passwd: &str) -> impl Iterator<Item = User> + '_ {
    passwd.lines().filter_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            return None;
        }
        Some(User {
            name: f[0].to_string(),
            uid: f[2].parse().ok()?,
            gid: f[3].parse().ok()?,
            home: f[5].to_string(),
        })
    })
}

/// The lane user: the one account whose home is `/home/<name>.guest`.
///
/// # Errors
/// None is, or more than one is: which.
pub fn lane_user(passwd: &str) -> Result<User, String> {
    let mut found: Vec<User> = accounts(passwd)
        .filter(|u| u.home == format!("/home/{}.guest", u.name))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("no lane user: no account in /etc/passwd has home /home/<name>.guest".into()),
        _ => Err(format!(
            "more than one lane user in /etc/passwd: {}",
            found
                .iter()
                .map(|u| u.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// `user`'s groups from `group`'s text (`name:x:gid:member,member`): its primary gid
/// first, then each group that lists it by name, once each.
#[must_use]
pub fn groups(group: &str, user: &User) -> Vec<u32> {
    let mut out = vec![user.gid];
    for line in group.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 4 {
            continue;
        }
        let Ok(gid) = f[2].parse::<u32>() else {
            continue;
        };
        if f[3].split(',').any(|m| m.trim() == user.name) && !out.contains(&gid) {
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
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
ubuntu:x:1000:1000:Ubuntu:/home/ubuntu:/bin/bash
dana:x:501:1001:Dana:/home/dana.guest:/bin/bash
dana2:x:502:1002::/home/dana.guest:/bin/bash
broken:x:notanumber:1::/home/broken.guest:/bin/sh
short:x:3
";

    const GROUP: &str = "\
root:x:0:
adm:x:4:syslog,dana
sudo:x:27:ubuntu, dana
dana:x:1001:
users:x:100:dana2
video:x:44:dana,dana
garbage
bad:x:nan:dana
";

    #[test]
    fn the_lane_user_is_the_one_whose_home_is_its_name_dot_guest() {
        let user = lane_user(PASSWD).unwrap();
        assert_eq!(
            user,
            User {
                name: "dana".into(),
                uid: 501,
                gid: 1001,
                home: "/home/dana.guest".into(),
            }
        );
        let none = lane_user("root:x:0:0:root:/root:/bin/bash\n").unwrap_err();
        assert!(none.contains("no lane user"), "{none}");
        let two = format!("{PASSWD}other:x:600:600::/home/other.guest:/bin/sh\n");
        let refused = lane_user(&two).unwrap_err();
        assert!(refused.contains("dana, other"), "{refused}");
    }

    #[test]
    fn groups_are_the_primary_first_then_each_listing_by_exact_name_once() {
        let user = lane_user(PASSWD).unwrap();
        assert_eq!(groups(GROUP, &user), [1001, 4, 27, 44]);
        assert_eq!(
            identity(PASSWD, GROUP).unwrap(),
            Identity::Switch {
                uid: 501,
                gid: 1001,
                groups: vec![1001, 4, 27, 44],
            }
        );
    }
}
