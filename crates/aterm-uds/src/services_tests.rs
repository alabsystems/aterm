// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The live table reads services with footprints, never the kernel's coalition (launchd's,
/// which kernel_task shares), and the fseventsd running now is the one
/// [`aterm_vitals::services::FSEVENTSD`] restarts (its name, uid and image), which a look
/// at it alone finds.
#[test]
fn the_live_services_read_and_fseventsd_is_the_one_restarted() {
    let reading = read().expect("the process table and memory read");
    assert!(reading.total > 0);
    assert!(!reading.services.is_empty(), "{reading:?}");
    let kernels = aterm_sysprobe::coalitions::coalition_of(1).expect("launchd's coalition");
    assert!(
        reading
            .services
            .iter()
            .all(|s| s.coalition != kernels && s.pid > 1),
        "the kernel's coalition is no service: {reading:?}"
    );
    assert!(reading.pressure_warn.is_some(), "the pressure level reads");
    assert!(
        reading
            .services
            .windows(2)
            .all(|w| w[0].footprint >= w[1].footprint),
        "largest first"
    );
    let Some(fseventsd) = aterm_sysprobe::kproc::records()
        .expect("the process table reads")
        .into_iter()
        .find(|r| r.comm == "fseventsd" && !r.zombie)
    else {
        return;
    };
    let restart = aterm_vitals::services::FSEVENTSD;
    assert_eq!(fseventsd.uid, restart.uid);
    assert_eq!(
        aterm_sysprobe::process_path(fseventsd.pid).as_deref(),
        Some(std::path::Path::new(restart.path))
    );
    let Look::Running(service, total) = look(&restart) else {
        panic!("the fseventsd the table holds is looked at");
    };
    assert_eq!((service.pid, total), (fseventsd.pid, reading.total));
    assert!(service.footprint > 0, "{service:?}");
}
