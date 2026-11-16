// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native receive failures are still failures; an intent handle is not a lifeline.

use super::*;

struct Owned(SupervisedChild);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.child_mut().kill();
        let _ = self.0.child_mut().wait();
    }
}

pub(crate) fn actual_mailbox_errors() {
    let mut child = Owned(SupervisedChild::Ordinary(
        aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .unwrap(),
    ));
    let mut events: JobEvents<'_> =
        &mut |_| panic!("a refused receive cannot authorize child control");
    let (handle, mut cancellation) = Cancellation::channel(Duration::ZERO).unwrap();
    handle.0.send(&[255]).unwrap();
    // A modeled foreign process cannot consume even an already queued packet.
    let original_owner = cancellation.owner;
    cancellation.owner = 0;
    assert_eq!(
        cancellation
            .service(&mut child.0, &mut events)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    cancellation.owner = original_owner;
    assert_eq!(
        cancellation
            .service(&mut child.0, &mut events)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!cancellation.requested());

    // Replace only this test receiver with a real native disconnected pair.
    // The original retained peer cannot repair another socket's actual error.
    let (peer, inbox) = UnixDatagram::pair().unwrap();
    inbox.set_nonblocking(true).unwrap();
    cancellation.inbox = inbox;
    drop(peer);
    assert_eq!(
        cancellation
            .service(&mut child.0, &mut events)
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionReset
    );
    assert!(!cancellation.requested());
}
