// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only macOS routing-notification census: `route_events [duration_ms]` (default 20000).
//! Counts message types and GET requester PIDs without recording addresses or changing routes.

#[cfg(any(target_os = "macos", test))]
fn requester(message: &[u8]) -> Option<i32> {
    // net/route.h's rt_msghdr: length/version/type at 0/2/3, requester pid at 16.
    // Other message types may have different headers and cannot name a requester here.
    if message.len() < 92 || message[2] != 5 || message[3] != 4 {
        return None;
    }
    let size = usize::from(u16::from_ne_bytes(message[..2].try_into().ok()?));
    if !(92..=message.len()).contains(&size) {
        return None;
    }
    let pid = i32::from_ne_bytes(message[16..20].try_into().ok()?);
    (pid > 0).then_some(pid)
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    observe()
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("route_events requires macOS".into())
}

#[cfg(target_os = "macos")]
fn observe() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::time::{Duration, Instant};

    let duration = std::env::args()
        .nth(1)
        .map_or(Ok(20_000), |arg| arg.parse::<u64>())?;
    let routes = aterm_uds::kept_out(|| -> std::io::Result<OwnedFd> {
        // SAFETY: socket takes no pointer and its returned descriptor is owned below.
        let raw = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw is the new socket's descriptor and no other owner exists.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: this changes only this live owned descriptor's close-on-exec flag.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(fd)
    })?;
    let mut events = [0_u64; 256];
    let mut requesters = std::collections::BTreeMap::<i32, u64>::new();
    let mut polls = 0_u64;
    let mut timeouts = 0_u64;
    let mut bytes = 0_u64;
    let mut short = 0_u64;
    let start = Instant::now();
    let deadline = start
        .checked_add(Duration::from_millis(duration))
        .ok_or("duration exceeds the monotonic clock range")?;
    let mut buffer = [0_u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fd = libc::pollfd {
            fd: routes.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = aterm_uds::deadline::poll_timeout_ms(left);
        polls += 1;
        // SAFETY: fd is one live pollfd and the count passed is one.
        let ready = unsafe { libc::poll(&raw mut fd, 1, millis) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if ready == 0 {
            timeouts += 1;
            continue;
        }
        if fd.revents & libc::POLLIN == 0 {
            return Err(format!("route socket poll flags: {}", fd.revents).into());
        }
        // SAFETY: buffer is live and writable for its whole length; fd remains borrowed.
        let read = unsafe {
            libc::recv(
                fd.fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if read < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error.into());
        }
        let read = usize::try_from(read)?;
        if read == 0 {
            return Err("route socket closed".into());
        }
        bytes += u64::try_from(read)?;
        if read >= 4 {
            events[usize::from(buffer[3])] += 1;
            if let Some(pid) = requester(&buffer[..read]) {
                *requesters.entry(pid).or_default() += 1;
            }
        } else {
            short += 1;
        }
    }
    println!(
        "route_events elapsed_us={} polls={polls} timeouts={timeouts} bytes={bytes} short={short}",
        start.elapsed().as_micros()
    );
    for (kind, count) in events.iter().enumerate().filter(|(_, count)| **count != 0) {
        println!("route_event type={kind:#x} count={count}");
    }
    for (pid, count) in requesters {
        println!("route_requester pid={pid} get_count={count}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::requester;

    #[test]
    fn only_complete_get_headers_attribute_a_positive_requester() {
        let mut message = [0_u8; 92];
        message[..2].copy_from_slice(&92_u16.to_ne_bytes());
        message[2] = 5;
        message[3] = 4;
        message[16..20].copy_from_slice(&1234_i32.to_ne_bytes());
        assert_eq!(requester(&message), Some(1234));
        assert_eq!(requester(&message[..19]), None);
        for pid in [0_i32, -1] {
            message[16..20].copy_from_slice(&pid.to_ne_bytes());
            assert_eq!(requester(&message), None);
        }
        message[16..20].copy_from_slice(&1234_i32.to_ne_bytes());
        message[3] = 7;
        assert_eq!(requester(&message), None);
        message[3] = 4;
        message[2] = 0;
        assert_eq!(requester(&message), None);
        message[2] = 5;
        message[..2].copy_from_slice(&93_u16.to_ne_bytes());
        assert_eq!(requester(&message), None);
    }
}
