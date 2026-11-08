// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed origin policy observations; no Git argv parser or transport substitute.

use crate::ledger::{ControlRefChange, ControlRefTransaction, Origin, RefWrite};
use std::collections::VecDeque;
use std::sync::Mutex;

pub enum Call {
    Get(&'static str, std::result::Result<Option<String>, String>),
    Import(String),
    Peel(String, Option<String>),
    Control(
        String,
        Vec<ControlRefChange>,
        std::result::Result<RefWrite, String>,
    ),
}

impl Call {
    pub fn get(reference: &'static str, value: Option<&str>) -> Self {
        Self::Get(reference, Ok(value.map(str::to_owned)))
    }
    pub fn control(
        owner: &str,
        reference: &str,
        before: Option<&str>,
        after: Option<&str>,
        result: std::result::Result<RefWrite, String>,
    ) -> Self {
        Self::Control(
            owner.into(),
            vec![ControlRefChange {
                reference: reference.into(),
                before: before.map(str::to_owned),
                after: after.map(str::to_owned),
            }],
            result,
        )
    }
}

pub struct Script(Mutex<VecDeque<Call>>);

impl Script {
    pub fn new(calls: Vec<Call>) -> Self {
        Self(Mutex::new(calls.into()))
    }
    fn next(&self) -> Call {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected origin call")
    }
    pub fn done(&self) {
        assert!(self.0.lock().unwrap().is_empty(), "unconsumed origin calls");
    }
}

impl Origin for Script {
    fn refresh_main(&self) -> std::result::Result<String, String> {
        panic!("unexpected main refresh")
    }
    fn get(&self, reference: &str) -> std::result::Result<Option<String>, String> {
        let Call::Get(expected, result) = self.next() else {
            panic!("expected a typed ref read")
        };
        assert_eq!(reference, expected);
        result
    }
    fn import(&self, oid: &str) -> std::result::Result<(), String> {
        let Call::Import(expected) = self.next() else {
            panic!("expected an exact object import")
        };
        assert_eq!(oid, expected);
        Ok(())
    }
    fn peel(&self, oid: &str) -> std::result::Result<Option<String>, String> {
        let Call::Peel(expected, result) = self.next() else {
            panic!("expected an immutable tag peel")
        };
        assert_eq!(oid, expected);
        Ok(result)
    }
    fn tag_object(&self, _: &str) -> std::result::Result<Vec<u8>, String> {
        panic!("unexpected tag body read")
    }
    fn write(&self, _: &str, _: Option<&str>, _: &str) -> std::result::Result<RefWrite, String> {
        panic!("unexpected ordinary immutable ref write")
    }
    fn delete(&self, _: &str, _: &str) -> std::result::Result<RefWrite, String> {
        panic!("unexpected ordinary immutable ref delete")
    }
    fn control_transaction(
        &self,
        transaction: ControlRefTransaction,
    ) -> std::result::Result<RefWrite, String> {
        let Call::Control(owner, changes, result) = self.next() else {
            panic!("expected a typed control transaction")
        };
        assert_eq!(transaction.owner_commit(), owner);
        assert_eq!(transaction.changes(), changes.as_slice());
        result
    }
}
