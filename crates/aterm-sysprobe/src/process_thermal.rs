// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact process-wide Foundation thermal and low-power scalar properties.

/// Documented NSProcessInfo thermal severity; unknown raw values remain unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessThermalState {
    /// No thermal constraint.
    Nominal,
    /// Elevated temperature.
    Fair,
    /// Serious thermal constraint.
    Serious,
    /// Critical thermal constraint.
    Critical,
}
impl ProcessThermalState {
    /// Original documented Foundation scalar.
    #[must_use]
    pub const fn raw(self) -> isize {
        match self {
            Self::Nominal => 0,
            Self::Fair => 1,
            Self::Serious => 2,
            Self::Critical => 3,
        }
    }
}
/// Native exact thermal state and optional low-power property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessThermal {
    /// Exact documented thermal state.
    pub state: ProcessThermalState,
    /// None on systems predating the low-power selector.
    pub low_power: Option<bool>,
}
#[cfg(any(target_os = "macos", test))]
fn decode(raw: isize, low_power: Option<bool>) -> Option<ProcessThermal> {
    let state = match raw {
        0 => ProcessThermalState::Nominal,
        1 => ProcessThermalState::Fair,
        2 => ProcessThermalState::Serious,
        3 => ProcessThermalState::Critical,
        _ => return None,
    };
    Some(ProcessThermal { state, low_power })
}
/// Read thread-safe NSProcessInfo scalar properties without calling an application/window API.
/// This does not assert which transitive system images Foundation loads.
/// Missing selectors, unknown thermal values and unsupported platforms return None.
#[must_use]
pub fn process_thermal() -> Option<ProcessThermal> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{class, sel};
        aterm_objc::autoreleasepool(|_| {
            let class = class(c"NSProcessInfo");
            if class.as_id().is_null() {
                return None;
            }
            // SAFETY: NSProcessInfo is the Foundation class, +processInfo is an
            // immortal singleton, and these documented thread-safe getters have
            // scalar signatures. Availability is checked before either getter.
            unsafe {
                let info = aterm_objc::send::send_id(class.as_id(), sel!(processInfo));
                if info.is_null()
                    || !aterm_objc::send::send_bool_sel(
                        info,
                        sel!(respondsToSelector:),
                        sel!(thermalState),
                    )
                {
                    return None;
                }
                let low = aterm_objc::send::send_bool_sel(
                    info,
                    sel!(respondsToSelector:),
                    sel!(isLowPowerModeEnabled),
                )
                .then(|| aterm_objc::send::send_bool(info, sel!(isLowPowerModeEnabled)));
                decode(aterm_objc::send::send_isize(info, sel!(thermalState)), low)
            }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_states_are_exact_and_optional_low_power_stays_unknown() {
        for raw in 0..=3 {
            let reading = decode(raw, None).unwrap();
            assert_eq!(reading.state.raw(), raw);
            assert_eq!(reading.low_power, None);
        }
        assert_eq!(decode(-1, Some(false)), None);
        assert_eq!(decode(4, Some(true)), None);
        assert_eq!(decode(0, Some(true)).unwrap().low_power, Some(true));
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn native_scalar_getter_needs_no_gui() {
        let reading = process_thermal().expect("Foundation thermal state");
        assert!((0..=3).contains(&reading.state.raw()));
    }
}
