// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS's own services' footprints, read without root, for [`aterm_vitals::services`].

pub use aterm_vitals::services::{Member, Reading, Restart, Service};

/// What one look at a service aterm restarts found ([`look`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// It runs: its service as read, and the machine's memory, bytes.
    Running(Service, u64),
    /// None runs (launchd starts one at its next client).
    NoneRunning,
    /// Nothing could be read: off macOS, or the process table, a candidate's image or
    /// coalition, or the machine's memory could not be read.
    Unread,
}

/// One look at `restart`'s service alone: the process of its name, uid and image, its
/// coalition's footprint, and the live members that footprint is of.
#[must_use]
pub fn look(restart: &Restart) -> Look {
    #[cfg(target_os = "macos")]
    {
        use aterm_sysprobe::coalitions;
        let Some(records) = aterm_sysprobe::kproc::records() else {
            return Look::Unread;
        };
        let mut unread = false;
        for r in records
            .iter()
            .filter(|r| r.uid == restart.uid && r.comm == restart.name && !r.zombie)
        {
            match aterm_sysprobe::process_path(r.pid) {
                Some(path) if path == std::path::Path::new(restart.path) => {
                    let read = coalitions::coalition_of(r.pid).and_then(|id| {
                        let footprint = coalitions::footprint(id)?;
                        let total = aterm_sysprobe::physical_memory_mib()?.saturating_mul(1 << 20);
                        Some((id, footprint, total))
                    });
                    let Some((coalition, footprint, total)) = read else {
                        return Look::Unread;
                    };
                    let live = records
                        .iter()
                        .filter(|m| !m.zombie && coalitions::coalition_of(m.pid) == Some(coalition))
                        .count();
                    return Look::Running(
                        Service {
                            coalition,
                            pid: r.pid,
                            start_s: r.start_s,
                            start_us: r.start_us,
                            uid: r.uid,
                            name: r.comm.clone(),
                            path: Some(restart.path.to_owned()),
                            live: u32::try_from(live).unwrap_or(u32::MAX),
                            footprint,
                        },
                        total,
                    );
                }
                Some(_) => {}
                None => unread = true,
            }
        }
        if unread {
            Look::Unread
        } else {
            Look::NoneRunning
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = restart;
        Look::Unread
    }
}

/// One read of every service: the process table, each live process's resource coalition, the
/// footprint of each coalition of system accounts alone ([`aterm_vitals::services::fold`]),
/// and the memory strain (the kernel's pressure level and the swap counter); `None` where the
/// table or the machine's memory cannot be read, and off macOS. `kernel_task` (pid 0) has no
/// coalition to read and is left out; the kernel's own coalition is known by launchd (pid 1),
/// whom the kernel places in it ([`aterm_vitals::services::kernel_member`]). A process of a
/// system account whose coalition did not read is one of [`Reading::unread`] (coalition 0), so
/// one failed read of a service's process is no end of its episode.
#[must_use]
pub fn read() -> Option<Reading> {
    #[cfg(target_os = "macos")]
    {
        use aterm_sysprobe::coalitions;
        use aterm_vitals::services::{kernel_member, system_uid};
        let total = aterm_sysprobe::physical_memory_mib()?.saturating_mul(1 << 20);
        let mut members: Vec<Member> = Vec::new();
        let mut lost: Vec<Member> = Vec::new();
        for r in aterm_sysprobe::kproc::records()?
            .into_iter()
            .filter(|r| !r.zombie)
        {
            let coalition = coalitions::coalition_of(r.pid);
            let member = Member {
                coalition: coalition.unwrap_or(0),
                pid: r.pid,
                uid: r.uid,
                name: r.comm,
                start_s: r.start_s,
                start_us: r.start_us,
            };
            match coalition {
                Some(_) => members.push(member),
                None if !kernel_member(member.pid) && system_uid(member.uid) => lost.push(member),
                None => {}
            }
        }
        let mut reading =
            aterm_vitals::services::fold(&members, total, coalitions::footprint, |pid| {
                aterm_sysprobe::process_path(pid)
                    .and_then(|p| p.into_os_string().into_string().ok())
            });
        reading.unread.extend(lost);
        reading.unread.sort_by_key(|m| (m.coalition, m.pid));
        reading.pressure_warn = aterm_sysprobe::memory_pressure()
            .map(|level| level != aterm_sysprobe::MemoryLevel::Normal);
        reading.swapped_kib = aterm_sysprobe::swapped_kib();
        Some(reading)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
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
}
