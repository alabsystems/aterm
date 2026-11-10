// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional, read-only IOReport CPU subscription. ABI references: Apple's
//! IOPMLib.h/IOPM.h and macmon 39e98aa73e49256e6281b2a228f6ef3499ae3209,
//! src/sources.rs. No channel identifiers or descriptors are synthesized.

use std::ffi::{CStr, CString, c_void};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{Chain, Channel, Controls, Link, Pair, PowerLimits, Reading, State, interpret, nanos};
use crate::iokit::{self, CFDictionaryRef, CFMutableDictionaryRef, CFStringRef, CFTypeRef};
use crate::macos::{Cf, cf_bytes, cf_text, cf_u64};

type Subscription = CFTypeRef;

aterm_dl::table! {
    struct Api {
        copy_channels = c"IOReportCopyChannelsInGroup": unsafe extern "C" fn(CFStringRef, CFStringRef, u64, u64, u64) -> CFMutableDictionaryRef;
        subscribe = c"IOReportCreateSubscription": unsafe extern "C" fn(*const c_void, CFMutableDictionaryRef, *mut CFMutableDictionaryRef, u64, CFTypeRef) -> Subscription;
        sample = c"IOReportCreateSamples": unsafe extern "C" fn(Subscription, CFMutableDictionaryRef, CFTypeRef) -> CFDictionaryRef;
        delta = c"IOReportCreateSamplesDelta": unsafe extern "C" fn(CFDictionaryRef, CFDictionaryRef, CFTypeRef) -> CFDictionaryRef;
        name = c"IOReportChannelGetChannelName": unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef;
        subgroup = c"IOReportChannelGetSubGroup": unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef;
        unit = c"IOReportChannelGetUnitLabel": unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef;
        state_count = c"IOReportStateGetCount": unsafe extern "C" fn(CFDictionaryRef) -> i32;
        state_name = c"IOReportStateGetNameForIndex": unsafe extern "C" fn(CFDictionaryRef, i32) -> CFStringRef;
        residency = c"IOReportStateGetResidency": unsafe extern "C" fn(CFDictionaryRef, i32) -> i64;
    }
}

fn api() -> Reading<&'static Api> {
    static API: OnceLock<Reading<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let lib = aterm_dl::Library::open(&[c"/usr/lib/libIOReport.dylib"])
            .map_err(|_| "unsupported:ioreport_library".to_owned())?;
        // SAFETY: signatures above transcribe the IOReport C ABI; CF references
        // are opaque pointers. Library keeps its handle for the process lifetime.
        unsafe { Api::load(&lib) }.map_err(|_| "unsupported:ioreport_symbol".to_owned())
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub(super) struct Reader {
    api: &'static Api,
    subscription: Cf,
    channels: Cf,
    chain: Chain<Cf>,
    e_frequencies: Option<Vec<f64>>,
    p_frequencies: Option<Vec<f64>>,
}

impl Reader {
    pub(super) fn new() -> Reading<Self> {
        let api = api()?;
        let group = Cf::string(c"CPU Stats").ok_or("unavailable:ioreport_group")?;
        // SAFETY: live CFString, NULL optional subgroup and zero reserved fields.
        let channels =
            Cf::adopt(unsafe { (api.copy_channels)(group.0.cast(), null(), 0, 0, 0) }.cast())
                .ok_or("unavailable:ioreport_channels")?;
        let mut subscribed = null_mut();
        // SAFETY: live channels dictionary; writable output; default allocator
        // and no synthetic descriptors. Both returned references are +1.
        let subscription = Cf::adopt(unsafe {
            (api.subscribe)(
                null(),
                channels.0.cast_mut().cast(),
                &raw mut subscribed,
                0,
                null(),
            )
        });
        let subscribed = Cf::adopt(subscribed.cast());
        let (Some(subscription), Some(channels)) = (subscription, subscribed) else {
            return Err("refused:ioreport_subscription".into());
        };
        let (e_frequencies, p_frequencies) = frequency_tables();
        Ok(Self {
            api,
            subscription,
            channels,
            chain: Chain::default(),
            e_frequencies,
            p_frequencies,
        })
    }

    /// One snapshot, and its delta against the previous one within `gap` (the gap rule,
    /// [`Chain`]): the window between their returns, the two calls' summed span, and the rows.
    pub(super) fn sample(&mut self, gap: Duration) -> (u64, u64, Reading<Vec<Channel>>) {
        let before = Instant::now();
        // SAFETY: subscription and its actual subscribed dictionary remain live.
        let sample = Cf::adopt(
            unsafe {
                (self.api.sample)(
                    self.subscription.0,
                    self.channels.0.cast_mut().cast(),
                    null(),
                )
            }
            .cast(),
        );
        let after = Instant::now();
        let Some(sample) = sample else {
            self.chain.clear();
            return (0, 0, Err("refused:ioreport_sample".into()));
        };
        let call_ns = nanos(after.duration_since(before));
        let current = Link {
            snapshot: sample,
            after,
            call_ns,
        };
        let Some(Pair {
            previous,
            current,
            window_ns,
            span_ns,
        }) = self.chain.push(current, gap)
        else {
            return (0, call_ns, Err("warming".into()));
        };
        // SAFETY: both snapshots were returned under the same live subscription.
        let delta = Cf::adopt(
            unsafe { (self.api.delta)(previous.0.cast(), current.0.cast(), null()) }.cast(),
        );
        let rows = delta
            .ok_or_else(|| "refused:ioreport_delta".to_owned())
            .and_then(|delta| self.decode(&delta));
        (window_ns, span_ns, rows)
    }

    /// Whether a snapshot taken now would pair within `gap`.
    pub(super) fn warm(&self, gap: Duration) -> bool {
        self.chain.warm(Instant::now(), gap)
    }

    fn decode(&self, delta: &Cf) -> Reading<Vec<Channel>> {
        let key = Cf::string(c"IOReportChannels").ok_or("unavailable:cf_string")?;
        // SAFETY: dictionary type is checked before reading its borrowed array.
        let array = unsafe {
            if iokit::CFGetTypeID(delta.0) != iokit::CFDictionaryGetTypeID() {
                return Err("unread_format:ioreport_dictionary".into());
            }
            iokit::CFDictionaryGetValue(delta.0.cast(), key.0)
        };
        if array.is_null() {
            return Err("unread_format:ioreport_channels".into());
        }
        // SAFETY: array is a live CF object borrowed from delta, type checked.
        let count = unsafe {
            if iokit::CFGetTypeID(array) != iokit::CFArrayGetTypeID() {
                return Err("unread_format:ioreport_array".into());
            }
            iokit::CFArrayGetCount(array.cast())
        };
        if !(1..=512).contains(&count) {
            return Err("unread_format:channel_count".into());
        }
        let mut rows = Vec::new();
        let raw_key = Cf::string(c"RawElements").ok_or("unavailable:cf_string")?;
        for index in 0..count {
            // SAFETY: index is in the checked array; each child is type checked.
            let item = unsafe { iokit::CFArrayGetValueAtIndex(array.cast(), index) };
            if item.is_null() {
                return Err("unread_format:null_channel".into());
            }
            // SAFETY: live borrowed CF object, checked before channel getters.
            if unsafe { iokit::CFGetTypeID(item) } != unsafe { iokit::CFDictionaryGetTypeID() } {
                return Err("unread_format:channel_dictionary".into());
            }
            let item = item.cast();
            // SAFETY: item is a channel dictionary from this CPU subscription.
            let subgroup = cf_text(unsafe { (self.api.subgroup)(item) }.cast()).unwrap_or_default();
            if !matches!(
                subgroup.as_str(),
                "CPU Core Performance States" | "CPU Complex Performance States"
            ) {
                continue;
            }
            // SAFETY: live dictionary and key; RawElements must be CFData.
            let raw = unsafe { iokit::CFDictionaryGetValue(item, raw_key.0) };
            if raw.is_null() {
                return Err("unread_format:raw_elements".into());
            }
            // SAFETY: borrowed from delta, which lives throughout decoding.
            let raw = unsafe { cf_bytes(raw) }.ok_or("unread_format:raw_elements")?;
            if raw.is_empty() || raw.len() > 8192 {
                return Err("unread_format:raw_elements_size".into());
            }
            let raw_elements = raw.to_vec();
            // SAFETY: same live channel dictionary; strings are borrowed.
            let name = cf_text(unsafe { (self.api.name)(item) }.cast())
                .ok_or("unread_format:channel_name")?;
            let unit = cf_text(unsafe { (self.api.unit)(item) }.cast())
                .unwrap_or_else(|| "unavailable".into());
            let count = unsafe { (self.api.state_count)(item) };
            if !(1..=128).contains(&count) {
                return Err("unread_format:state_count".into());
            }
            let mut states = Vec::new();
            for i in 0..count {
                // SAFETY: i is within IOReport's own state count for this channel.
                let name = cf_text(unsafe { (self.api.state_name)(item, i) }.cast())
                    .ok_or("unread_format:state_name")?;
                let ticks = unsafe { (self.api.residency)(item, i) };
                states.push(State { name, ticks });
            }
            let frequencies = if name.contains("ECPU") {
                self.e_frequencies.as_deref()
            } else if name.contains("PCPU") {
                self.p_frequencies.as_deref()
            } else {
                None
            };
            let (active, active_mhz) = interpret(&states, frequencies);
            rows.push(Channel {
                raw_elements,
                name,
                subgroup,
                unit,
                states,
                active,
                active_mhz,
                frequency_source: if frequencies.is_some() {
                    "pmgr_voltage_states:ordinal"
                } else {
                    "unavailable:frequency_mapping"
                }
                .into(),
            });
        }
        if rows.is_empty() {
            Err("unavailable:cpu_residency_channels".into())
        } else {
            Ok(rows)
        }
    }
}

fn service(class: &CStr) -> Option<u32> {
    // SAFETY: NUL-terminated class, matching dictionary consumed by IOKit.
    unsafe {
        let matching = iokit::IOServiceMatching(class.as_ptr());
        if matching.is_null() {
            return None;
        }
        let service = iokit::IOServiceGetMatchingService(0, matching.cast_const());
        (service != 0).then_some(service)
    }
}

fn property(entry: u32, name: &CStr) -> Option<Cf> {
    Cf::property(entry, Cf::string(name)?.0)
}

/// Read the documented M1–M4 table layout. Unrecognized chips keep raw states
/// and activity but no guessed frequency. The ordinal association is labelled,
/// so a table out of order maps nothing.
fn frequency_tables() -> (Option<Vec<f64>>, Option<Vec<f64>>) {
    let brand = crate::machine_words().cpu.unwrap_or_default();
    let named = |prefix: &str| {
        brand
            .strip_prefix(prefix)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with(' '))
    };
    if !["Apple M1", "Apple M2", "Apple M3", "Apple M4"]
        .iter()
        .any(|p| named(p))
    {
        return (None, None);
    }
    let (e, p) = pmgr_tables();
    (e.and_then(ordinal), p.and_then(ordinal))
}

/// The chip's DVFS range: each table's lowest and highest state ([`super::DvfsRange`]). It
/// needs no order, so it reads chips the ordinal map refuses (M5's Super table repeats a
/// lower state: 4428 then 4416 MHz).
pub(super) fn dvfs_range() -> Result<super::DvfsRange, &'static str> {
    let (e, p) = pmgr_tables();
    Ok(super::DvfsRange {
        e_mhz: span(&e.ok_or("pmgr_e_table")?),
        p_mhz: span(&p.ok_or("pmgr_p_table")?),
    })
}

/// The power manager's DVFS tables, MHz, as written: Efficiency (`voltage-states1-sram`), then
/// Super (`voltage-states5-sram`); `None` for a table it does not give or that does not decode
/// ([`decode_table`]).
fn pmgr_tables() -> (Option<Vec<f64>>, Option<Vec<f64>>) {
    // SAFETY: an absolute NUL-terminated registry path, +1 result or zero.
    let entry = unsafe { iokit::IORegistryEntryFromPath(0, c"IODeviceTree:/arm-io/pmgr".as_ptr()) };
    if entry == 0 {
        return (None, None);
    }
    let read = |key| {
        property(entry, key).and_then(|v| {
            // SAFETY: v remains live until the bytes have been copied/decoded.
            decode_table(unsafe { cf_bytes(v.0) }?)
        })
    };
    let result = (read(c"voltage-states1-sram"), read(c"voltage-states5-sram"));
    // SAFETY: release this function's +1 registry reference once.
    unsafe { iokit::IOObjectRelease(entry) };
    result
}

/// A table's states, MHz: 8-byte rows whose first four bytes are the frequency, in Hz on M1–M3
/// and kHz on M4 and M5, told apart by magnitude (a top over 10^8 is Hz: no core clocks at
/// 100 GHz, nor under 100 MHz). `None` for an empty, misshapen or oversized table, or a zero row.
fn decode_table(bytes: &[u8]) -> Option<Vec<f64>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(8) || bytes.len() > 128 * 8 {
        return None;
    }
    let raw: Vec<_> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|v| f64::from(u32::from_le_bytes(v[..4].try_into().expect("four bytes"))))
        .collect();
    let top = raw.iter().copied().fold(0.0, f64::max);
    let scale = if top > 1e8 { 1e6 } else { 1e3 };
    raw.iter()
        .all(|v| *v > 0.0)
        .then(|| raw.iter().map(|v| v / scale).collect())
}

/// A table whose states rise with their ordinal, as the ordinal map needs.
fn ordinal(table: Vec<f64>) -> Option<Vec<f64>> {
    table.windows(2).all(|w| w[0] < w[1]).then_some(table)
}

/// A decoded table's lowest and highest state.
fn span(table: &[f64]) -> (f64, f64) {
    table
        .iter()
        .fold((f64::INFINITY, 0.0), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
}

fn sysctl(name: &CStr) -> Reading<u64> {
    crate::macos::sysctl_integer_result(name)
        .map_err(errno_word)
        .and_then(|v| u64::try_from(v).map_err(|_| "unread_format:negative_integer".into()))
}

fn errno_word(code: i32) -> String {
    match code {
        libc::EPERM | libc::EACCES => format!("refused:errno_{code}"),
        libc::ENOENT | libc::ENOTSUP => format!("unsupported:errno_{code}"),
        _ => format!("os_error:errno_{code}"),
    }
}

fn power_limits() -> Reading<PowerLimits> {
    let mut status = null();
    // SAFETY: writable dictionary output; read-only public IOPMLib function.
    let rc = unsafe { iokit::IOPMCopyCPUPowerStatus(&raw mut status) };
    let status = Cf::adopt(status.cast());
    if rc != 0 {
        return Err(format!("os_error:iopm_{:08x}", rc as u32));
    }
    let status = status.ok_or("unavailable:cpu_power_status")?;
    // SAFETY: status is live; only read as a dictionary after checking type.
    if unsafe { iokit::CFGetTypeID(status.0) } != unsafe { iokit::CFDictionaryGetTypeID() } {
        return Err("unread_format:cpu_power_status".into());
    }
    let field = |name| -> Reading<u64> {
        let key = Cf::string(name).ok_or("unavailable:cf_string")?;
        // SAFETY: dictionary and key are live; value is borrowed for conversion.
        cf_u64(unsafe { iokit::CFDictionaryGetValue(status.0.cast(), key.0) })
            .ok_or_else(|| format!("unavailable:{}", name.to_string_lossy()))
    };
    Ok(PowerLimits {
        speed_percent: field(c"CPU_Speed_Limit"),
        scheduler_percent: field(c"CPU_Scheduler_Limit"),
        available_cpus: field(c"CPU_Available_CPUs"),
    })
}

pub(super) fn controls() -> Controls {
    let cluster_masks = if let Some(entry) = service(c"AppleCLPC") {
        let result = (|| {
            let count = property(entry, c"#cpu-num-clusters")
                .and_then(|v| cf_u64(v.0))
                .filter(|n| (1..=64).contains(n))
                .ok_or("unread_format:clpc_cluster_count")?;
            (0..count)
                .map(|i| {
                    let name = CString::new(format!("#cpu-core-mask-cluster-{i}")).expect("digits");
                    property(entry, &name)
                        .and_then(|v| cf_u64(v.0))
                        .map(|mask| (i as u32, mask))
                        .ok_or_else(|| "unavailable:clpc_cluster_mask".to_owned())
                })
                .collect::<Reading<Vec<_>>>()
        })();
        // SAFETY: release this function's +1 reference exactly once.
        unsafe { iokit::IOObjectRelease(entry) };
        result
    } else {
        Err("unavailable:clpc".into())
    };
    let last_sleep_reason = if let Some(entry) = service(c"IOPMrootDomain") {
        let text = property(entry, c"Last Sleep Reason").and_then(|v| cf_text(v.0));
        // SAFETY: release this function's +1 reference exactly once.
        unsafe { iokit::IOObjectRelease(entry) };
        text.ok_or_else(|| "unavailable:last_sleep_reason".into())
    } else {
        Err("unavailable:root_domain".into())
    };
    Controls {
        os_version: crate::machine_words()
            .os
            .ok_or_else(|| "unavailable:os_version".into()),
        kernel_release: crate::macos::sysctl_text_in::<64>(c"kern.osrelease")
            .ok_or_else(|| "unavailable:kernel_release".into()),
        kernel_version: crate::macos::sysctl_text_in::<512>(c"kern.version")
            .ok_or_else(|| "unavailable:kernel_version".into()),
        online: crate::macos::online_processors(),
        active_cpus: sysctl(c"hw.activecpu"),
        requested_recommended_mask: crate::macos::sysctl_integer_result(
            c"kern.sched_recommended_cores",
        )
        .map(|v| v as u64)
        .map_err(errno_word),
        cluster_masks,
        power_limits: power_limits(),
        last_sleep_reason,
        last_sleep_ms: crate::macos::sysctl_timeval_ms(c"kern.sleeptime")
            .ok_or_else(|| "unavailable:sleeptime".into()),
        last_wake_ms: crate::macos::sysctl_timeval_ms(c"kern.waketime")
            .ok_or_else(|| "unavailable:waketime".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows(states: &[u32]) -> Vec<u8> {
        states
            .iter()
            .flat_map(|f| f.to_le_bytes().into_iter().chain([0; 4]))
            .collect()
    }

    #[test]
    fn dvfs_table_rejects_missing_partial_or_nonmonotonic_data() {
        assert_eq!(decode_table(&[]), None);
        assert_eq!(decode_table(&[0; 7]), None);
        assert_eq!(decode_table(&[0; 8]), None);
        let table = rows(&[1_020_000_000, 2_000_000_000]);
        assert_eq!(decode_table(&table), Some(vec![1020.0, 2000.0]));
        let mut wrong = table.clone();
        wrong.extend_from_slice(&table[..8]);
        let out_of_order = decode_table(&wrong).expect("decodes");
        assert_eq!(ordinal(out_of_order), None);
        assert_eq!(
            ordinal(decode_table(&table).expect("decodes")),
            Some(vec![1020.0, 2000.0])
        );
    }

    /// A table in Hz (M1–M3) and one in kHz (M4, M5) read the same MHz, and the range takes each
    /// table's lowest and highest state whatever their order: M5's Super table (N1) repeats a
    /// lower state near its top. CONTROL: its ordinal map refuses that table.
    #[test]
    fn the_range_reads_hz_or_khz_tables_in_any_order() {
        assert_eq!(
            decode_table(&rows(&[972_000, 3_048_000])),
            decode_table(&rows(&[972_000_000, 3_048_000_000]))
        );
        let m5_super = decode_table(&rows(&[
            1_308_000, 1_608_000, 1_932_000, 2_220_000, 2_508_000, 2_820_000, 3_072_000, 3_300_000,
            3_516_000, 3_720_000, 3_876_000, 4_020_000, 4_176_000, 4_248_000, 4_272_000, 4_332_000,
            4_428_000, 4_416_000, 4_464_000,
        ]))
        .expect("decodes");
        assert_eq!(span(&m5_super), (1308.0, 4464.0));
        assert_eq!(ordinal(m5_super), None, "control");
    }

    /// This Mac's power manager gives both tables, each a range a core clocks within.
    #[test]
    fn this_macs_dvfs_range_reads_or_says_why() {
        match dvfs_range() {
            Ok(r) => {
                for (lo, hi) in [r.e_mhz, r.p_mhz] {
                    assert!((100.0..hi).contains(&lo) && hi < 10_000.0, "{r:?}");
                }
            }
            // An Intel Mac has no pmgr tables.
            Err(why) => assert!(why.starts_with("pmgr"), "{why}"),
        }
    }
}
