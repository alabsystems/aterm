// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};

struct Changing {
    outgoing: Vec<u8>,
    may_send: AtomicBool,
    withdraw_on_receive: bool,
    received: Mutex<Vec<Vec<u8>>>,
}

impl Changing {
    fn new(wire: &[u8], withdraw_on_receive: bool) -> Self {
        Self {
            outgoing: wire.to_vec(),
            may_send: AtomicBool::new(true),
            withdraw_on_receive,
            received: Mutex::new(Vec::new()),
        }
    }
}

impl Store for Changing {
    const MAX_WIRE: usize = 64;

    fn inventory(&self) -> Result<BTreeSet<Digest>> {
        Ok(BTreeSet::from([Sha256::digest(&self.outgoing)]))
    }

    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        Ok(
            if self.may_send.load(Ordering::Acquire)
                && ids.contains(&Sha256::digest(&self.outgoing))
            {
                vec![self.outgoing.clone()]
            } else {
                Vec::new()
            },
        )
    }

    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>> {
        self.received.lock().unwrap().extend_from_slice(wires);
        if self.withdraw_on_receive {
            self.may_send.store(false, Ordering::Release);
        }
        Ok(vec![true; wires.len()])
    }
}

#[test]
fn responder_fetches_current_payloads_after_receiving_updates() {
    for withdraw in [false, true] {
        let a = Changing::new(b"incoming update", false);
        let b = Changing::new(b"withdrawn outgoing record", withdraw);
        let (mut left, mut right) = UnixStream::pair().unwrap();
        for stream in [&left, &right] {
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
        }
        let ours: Vec<_> = a.inventory().unwrap().into_iter().collect();
        let theirs: Vec<_> = b.inventory().unwrap().into_iter().collect();
        let (sent, answered) = std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                exchange_batches(&mut right, &b, Side::Responder, &theirs, |_| {}).unwrap()
            });
            let sent = exchange_batches(&mut left, &a, Side::Initiator, &ours, |_| {}).unwrap();
            (sent, responder.join().unwrap())
        });
        let outgoing = usize::from(!withdraw);
        assert_eq!(sent, (1, outgoing, 0));
        assert_eq!(answered, (outgoing, 1, 0));
        assert_eq!(
            b.received.lock().unwrap().as_slice(),
            std::slice::from_ref(&a.outgoing)
        );
        assert_eq!(
            *a.received.lock().unwrap(),
            if withdraw {
                Vec::new()
            } else {
                vec![b.outgoing.clone()]
            },
            "a wire fetched before the update must never cross the send boundary"
        );
    }
}
