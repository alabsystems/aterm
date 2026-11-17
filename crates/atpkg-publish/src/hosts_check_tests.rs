// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{HostCheck, render};
use crate::ctx::HostChoice;

#[test]
fn render_is_one_sorted_object() {
    assert_eq!(
        render(
            HostChoice::Agh,
            &[HostCheck {
                name: "config",
                ok: true,
                detail: "d".into(),
            }]
        ),
        r#"{"checks":[{"detail":"d","name":"config","ok":true}],"host":"agh","ok":true}"#
    );
}

/// RL-27: `--lane tip`'s `armed` row holds only when launchd lists both tip jobs. A lane whose
/// units are rendered and whose jobs launchd does not list fails it, naming when the units were
/// rendered and who stopped the jobs where aterm did (`lane disarm tip` records it), else that
/// they were removed outside aterm, and the next step. CONTROL: before RL-27 `hosts check
/// --lane tip` had no such row, so m86's every-check-ok answered for a lane that was not
/// running.
#[test]
fn a_tip_lane_that_is_installed_and_not_loaded_fails_the_armed_check() {
    use super::armed_check;
    use crate::ctx::Ctx;
    use crate::host::Hosts;
    use crate::host::fake::FakeHost;
    use crate::lane::units::{TIP_LABEL, TIP_READY_LABEL};

    const NOW: i64 = 1_792_022_400;
    let home = aterm_tempfile::tempdir().unwrap();
    let ctx = Ctx::new(
        home.path(),
        None,
        None,
        NOW,
        Hosts::single(std::sync::Arc::new(FakeHost::default())),
    );
    let mut none = |_: &str| Ok(false);

    let check = armed_check(&ctx, &mut none);
    assert!(!check.ok, "{check:?}");
    assert!(
        check.detail.contains("no tip lane is installed here"),
        "{check:?}"
    );

    let units = ctx.state_dir.join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join(format!("{TIP_LABEL}.plist")), "<plist/>").unwrap();
    let check = armed_check(&ctx, &mut none);
    assert!(!check.ok, "{check:?}");
    for words in [
        format!("launchd lists no {TIP_LABEL} and no {TIP_READY_LABEL}"),
        "the lane's units were rendered".to_string(),
        "removed outside aterm".to_string(),
        "next: atpkg-publish lane install tip --staged-only".to_string(),
    ] {
        assert!(check.detail.contains(&words), "{words}: {check:?}");
    }

    crate::lane::install::record_tip_stopped(&ctx, "lane disarm tip");
    let check = armed_check(&ctx, &mut none);
    assert!(!check.ok, "{check:?}");
    let when = aterm_types::rfc3339::format_rfc3339(u64::try_from(NOW).unwrap());
    assert!(
        check.detail.contains(&format!(
            "aterm stopped its jobs {when} with `atpkg-publish lane disarm tip`, run by "
        )),
        "{check:?}"
    );
    assert!(!check.detail.contains("outside aterm"), "{check:?}");

    let mut half = |label: &str| Ok(label == TIP_LABEL);
    let check = armed_check(&ctx, &mut half);
    assert!(!check.ok, "{check:?}");
    assert!(
        check
            .detail
            .starts_with(&format!("launchd lists no {TIP_READY_LABEL}")),
        "{check:?}"
    );

    let mut both = |_: &str| Ok(true);
    let check = armed_check(&ctx, &mut both);
    assert!(check.ok, "{check:?}");

    let mut unreadable = |_: &str| Err("no Aqua manager".to_string());
    let check = armed_check(&ctx, &mut unreadable);
    assert!(
        !check.ok && check.detail.contains("no Aqua manager"),
        "{check:?}"
    );
}
