// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
ubuntu:x:1000:1000:Ubuntu:/home/ubuntu:/bin/bash
dana:x:501:1001:Dana:/home/dana.guest:/bin/bash
dana2:x:502:1002::/home/dana.guest:/bin/bash
broken:x:notanumber:1::/home/broken.guest:/bin/sh
plus_uid:x:+503:1003::/home/plus_uid.guest:/bin/sh
plus_gid:x:503:+1003::/home/plus_gid.guest:/bin/sh
extra:x:504:1004:gecos:/home/extra.guest:/bin/sh:trailing
short:x:3
";

const GROUP: &str = "\
root:x:0:
adm:x:4:syslog,dana
sudo:x:27:ubuntu, dana
dana:x:1001:
users:x:100:dana2
video:x:44:dana,dana
plus:x:+45:dana
extra:x:46:dana:trailing
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
    let commented = format!(
        "#commented:x:700:700::/home/#commented.guest:/bin/sh\n\
         :x:701:701::/home/.guest:/bin/sh\n\
         a/b:x:702:702::/home/a/b.guest:/bin/sh\n\
         {PASSWD}"
    );
    assert_eq!(lane_user(&commented).unwrap().name, "dana");
    let root_lane = lane_user("dana:x:0:1001:Dana:/home/dana.guest:/bin/bash\n").unwrap_err();
    assert!(root_lane.contains("uid 0"), "{root_lane}");
    let root_gid_lane = lane_user("dana:x:501:0:Dana:/home/dana.guest:/bin/bash\n").unwrap_err();
    assert!(root_gid_lane.contains("gid 0"), "{root_gid_lane}");
}

#[test]
fn groups_are_the_primary_first_then_each_listing_by_exact_name_once() {
    let user = lane_user(PASSWD).unwrap();
    let group = format!("#wheel:x:99:dana\n:x:98:dana\n{GROUP}");
    assert_eq!(groups(&group, &user), [1001, 4, 27, 44]);
    assert_eq!(
        identity(PASSWD, GROUP).unwrap(),
        Identity::Switch {
            uid: 501,
            gid: 1001,
            groups: vec![1001, 4, 27, 44],
        }
    );
}
