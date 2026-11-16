// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::rc::Rc;

use super::resources::{Release, ThreadList};
use super::{MAX_THREADS, Operation, ReadError};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Released {
    Thread(u32),
    Array(usize, usize),
}

#[derive(Clone)]
struct Releaser {
    log: Rc<RefCell<Vec<Released>>>,
    fail_thread: Option<u32>,
    fail_array: bool,
}

impl Release for Releaser {
    fn thread(&self, port: u32) -> Result<(), i32> {
        self.log.borrow_mut().push(Released::Thread(port));
        if self.fail_thread == Some(port) {
            Err(19)
        } else {
            Ok(())
        }
    }

    fn array(&self, address: usize, bytes: usize) -> Result<(), i32> {
        self.log.borrow_mut().push(Released::Array(address, bytes));
        if self.fail_array { Err(23) } else { Ok(()) }
    }
}

fn expected(ports: &[u32]) -> Vec<Released> {
    ports
        .iter()
        .copied()
        .map(Released::Thread)
        .chain([Released::Array(
            ports.as_ptr() as usize,
            std::mem::size_of_val(ports),
        )])
        .collect()
}

#[test]
fn named_self_cpu_cleanup_releases_rights_beyond_the_inspection_cap_once() {
    let mut ports: Vec<_> = (1..=u32::try_from(MAX_THREADS + 3).unwrap()).collect();
    let log = Rc::new(RefCell::new(Vec::new()));
    let release = Releaser {
        log: Rc::clone(&log),
        fail_thread: None,
        fail_array: false,
    };
    // SAFETY: ports stays initialized and live until the fake releaser records its array return.
    let list = unsafe { ThreadList::new(ports.as_mut_ptr(), ports.len(), release) };
    assert_eq!(list.ports().iter().take(MAX_THREADS).count(), MAX_THREADS);
    assert_eq!(list.close(), Ok(()));
    assert_eq!(*log.borrow(), expected(&ports));
}

#[test]
fn named_self_cpu_early_return_and_unwinding_return_every_right_and_the_array() {
    let mut ports = vec![11, 17, 29];
    for unwind in [false, true] {
        let log = Rc::new(RefCell::new(Vec::new()));
        let release = Releaser {
            log: Rc::clone(&log),
            fail_thread: None,
            fail_array: false,
        };
        let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: the fixture remains live through the guard's Drop, including unwinding.
            let _list = unsafe { ThreadList::new(ports.as_mut_ptr(), ports.len(), release) };
            if unwind {
                panic!("injected thread-read unwind");
            }
            Err::<(), _>(ReadError::Unexpected {
                operation: Operation::ThreadCpu,
            })
        }));
        assert_eq!(exit.is_err(), unwind);
        assert_eq!(*log.borrow(), expected(&ports));
    }
}

#[test]
fn named_self_cpu_cleanup_failure_never_abandons_later_rights_or_retries_them() {
    let mut ports = vec![11, 17, 29];
    for fail_thread in [Some(11), None] {
        let log = Rc::new(RefCell::new(Vec::new()));
        let release = Releaser {
            log: Rc::clone(&log),
            fail_thread,
            fail_array: true,
        };
        // SAFETY: the fake release never frees the live fixture; it records each ownership return.
        let list = unsafe { ThreadList::new(ports.as_mut_ptr(), ports.len(), release) };
        assert_eq!(
            list.close(),
            Err(ReadError::Native {
                operation: if fail_thread.is_some() {
                    Operation::ReleaseThread
                } else {
                    Operation::ReleaseList
                },
                code: if fail_thread.is_some() { 19 } else { 23 },
            })
        );
        assert_eq!(*log.borrow(), expected(&ports));
    }
}
