// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn progress_maps_to_the_taskbar_flag_and_value() {
    assert_eq!(encode(None), (TBPF_NOPROGRESS, 0));
    assert_eq!(encode(Some(TaskbarProgress::Hidden)), (TBPF_NOPROGRESS, 0));
    assert_eq!(
        encode(Some(TaskbarProgress::Indeterminate)),
        (TBPF_INDETERMINATE, 0)
    );
    assert_eq!(encode(Some(TaskbarProgress::Normal(42))), (TBPF_NORMAL, 42));
    assert_eq!(encode(Some(TaskbarProgress::Error(7))), (TBPF_ERROR, 7));
    assert_eq!(encode(Some(TaskbarProgress::Paused(99))), (TBPF_PAUSED, 99));
}

#[test]
fn the_vtable_slots_are_the_headers() {
    let slot = |offset: usize| offset / std::mem::size_of::<usize>();
    assert_eq!(
        slot(std::mem::offset_of!(ITaskbarList3Vtbl, set_progress_value)),
        9
    );
    assert_eq!(
        slot(std::mem::offset_of!(ITaskbarList3Vtbl, set_progress_state)),
        10
    );
}
