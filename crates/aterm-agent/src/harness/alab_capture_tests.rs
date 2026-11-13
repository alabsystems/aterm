// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Host(u8);
impl SourceGuard for Host {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, _: &dyn SourceGuard) -> bool {
        false
    }
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        match self.0 {
            0 => save(),
            1 => Ok(()),
            2 => {
                let _ = save();
                Ok(())
            }
            _ => {
                save()?;
                Err("host failed after save".into())
            }
        }
    }
}

#[test]
fn guard_retains_the_full_callback_result_and_requires_a_call() {
    assert_eq!(ReceiptGuard::new(Host(0)).with_admitted(|| Ok(42)), Ok(42));
    assert!(ReceiptGuard::new(Host(1)).with_admitted(|| Ok(42)).is_err());
    assert_eq!(
        ReceiptGuard::new(Host(2)).with_admitted::<()>(|| Err("save failed".into())),
        Err("save failed".into())
    );
    let mut saved = false;
    let result = ReceiptGuard::new(Host(3)).with_admitted(|| {
        saved = true;
        Ok(42)
    });
    assert!(saved);
    assert_eq!(result, Err("host failed after save".into()));
}
