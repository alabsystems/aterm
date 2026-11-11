// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional fleet browsing never holds the name writer. Pending requests coalesce to the
//! newest panel; the panel's press ID rejects results for an alert that has closed.

use std::sync::mpsc;
use std::time::Duration;

pub(super) fn start() -> std::io::Result<mpsc::Sender<u64>> {
    let (send, requests) = mpsc::channel();
    std::thread::Builder::new()
        .name("aterm-name-discovery".to_owned())
        .spawn(move || {
            run(requests, |press| {
                let available = aterm_secrets::discover::browse_members(Duration::from_secs(2))
                    .map(|members| !members.is_empty())
                    .map_err(|why| why.to_string());
                aterm_objc::run_on_main(move |_| {
                    crate::machine_rename_sheet::deliver_discovery(press, available);
                });
            });
        })?;
    Ok(send)
}

fn run(requests: mpsc::Receiver<u64>, mut browse: impl FnMut(u64)) {
    while let Ok(mut press) = requests.recv() {
        for newer in requests.try_iter() {
            press = newer;
        }
        browse(press);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_browse_keeps_only_the_newest_pending_panel() {
        let (send, requests) = mpsc::channel();
        let mut seen = Vec::new();
        send.send(1).unwrap();
        let mut sender = Some(send);
        run(requests, |press| {
            seen.push(press);
            if let Some(send) = sender.take() {
                send.send(2).unwrap();
                send.send(3).unwrap();
            }
        });
        assert_eq!(seen, [1, 3]);
    }
}
