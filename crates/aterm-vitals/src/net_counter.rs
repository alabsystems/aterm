// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Network byte counters and the windows over which they prove traffic.
//!
//! Darwin's routing socket gives non-platform binaries a 32-bit, 1024-byte-aligned
//! residue inside a 64-bit field. It is neither a lifetime total nor a counter whose
//! forward movement rules out a wrap. Keep that provenance with every reading.

/// The byte counter's source and effective representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ByteCounter {
    /// The reader did not establish the counter's representation.
    #[default]
    Unknown,
    /// A cumulative unsigned 64-bit byte counter.
    Cumulative64,
    /// Darwin `NET_RT_IFLIST2` for a non-platform binary: floor to 1024 bytes,
    /// then truncate to unsigned 32 bits. Packet counters remain unsigned 64-bit.
    DarwinModulo32,
}

impl ByteCounter {
    /// Source word used in diagnostic records.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Cumulative64 => "cumulative64",
            Self::DarwinModulo32 => "darwin_iflist2_modulo32",
        }
    }

    /// Effective counter width, where established.
    #[must_use]
    pub const fn bits(self) -> Option<u8> {
        match self {
            Self::Unknown => None,
            Self::Cumulative64 => Some(64),
            Self::DarwinModulo32 => Some(32),
        }
    }

    /// Byte quantum of the raw reading, where established.
    #[must_use]
    pub const fn quantum(self) -> Option<u64> {
        match self {
            Self::Unknown => None,
            Self::Cumulative64 => Some(1),
            Self::DarwinModulo32 => Some(1024),
        }
    }
}

/// Byte and packet counters of one interface, kept together for a rate window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteSample {
    /// Interface index: a renamed/replaced interface supplies no old baseline.
    pub index: u32,
    /// Effective byte-counter representation.
    pub counter: ByteCounter,
    /// Raw received bytes, possibly a modulo residue.
    pub rx: u64,
    /// Raw sent bytes, possibly a modulo residue.
    pub tx: u64,
    /// Cumulative received packets (64-bit, including on Darwin).
    pub rx_packets: u64,
    /// Cumulative sent packets (64-bit, including on Darwin).
    pub tx_packets: u64,
    /// Driver-reported link capacity in bits per second.
    pub rate_bps: Option<u64>,
}

/// Traffic represented by the two byte readings over one valid window.
/// Darwin values retain the input's 1024-byte quantization; no lifetime total is rebuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteDelta {
    /// Received byte delta.
    pub rx: u64,
    /// Sent byte delta.
    pub tx: u64,
}

/// Why two readings do not establish a byte rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaError {
    /// No comparable baseline has been taken yet.
    Warming,
    /// The interface or counter representation changed.
    SourceChanged,
    /// Counter width or provenance was not established.
    UnknownCounter,
    /// The elapsed interval is zero, too long, or otherwise invalid.
    InvalidInterval,
    /// A monotonic counter went backwards, or byte movement contradicts its guards.
    Discontinuity,
    /// Missing capacity or a long interval leaves the number of modulo wraps unknown.
    AmbiguousWrap,
}

impl DeltaError {
    /// Reason used for an unavailable rate in diagnostic records.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Warming => "warming",
            Self::SourceChanged => "counter_source_changed",
            Self::UnknownCounter => "counter_provenance_unknown",
            Self::InvalidInterval => "counter_interval_invalid",
            Self::Discontinuity => "counter_discontinuity",
            Self::AmbiguousWrap => "counter_wrap_ambiguous",
        }
    }
}

/// Longest supported rate window: capacity's retained looks are at most 90 seconds apart.
pub const MAX_WINDOW_NS: u64 = 90_000_000_000;

/// Derive traffic only when the source, interval and reset guards support one delta.
///
/// For a modulo counter, a link-capacity bound plus one quantization quantum must be
/// strictly less than 2^32, even when the residue increased: multiple wraps can otherwise
/// be invisible. Both endpoints must agree on a positive capacity. Monotonic packet
/// counters guard interface resets; a byte advance with no packet advance is also unknown.
/// This does not manufacture a wider cumulative counter or request greater OS privilege.
pub fn byte_delta(
    before: ByteSample,
    now: ByteSample,
    window_ns: u64,
) -> Result<ByteDelta, DeltaError> {
    if window_ns == 0 || window_ns > MAX_WINDOW_NS {
        return Err(DeltaError::InvalidInterval);
    }
    if before.index != now.index || before.counter != now.counter {
        return Err(DeltaError::SourceChanged);
    }
    let rx_packets = now
        .rx_packets
        .checked_sub(before.rx_packets)
        .ok_or(DeltaError::Discontinuity)?;
    let tx_packets = now
        .tx_packets
        .checked_sub(before.tx_packets)
        .ok_or(DeltaError::Discontinuity)?;
    match now.counter {
        ByteCounter::Unknown => Err(DeltaError::UnknownCounter),
        ByteCounter::Cumulative64 => Ok(ByteDelta {
            rx: now
                .rx
                .checked_sub(before.rx)
                .ok_or(DeltaError::Discontinuity)?,
            tx: now
                .tx
                .checked_sub(before.tx)
                .ok_or(DeltaError::Discontinuity)?,
        }),
        ByteCounter::DarwinModulo32 => {
            const MODULUS: u64 = 1 << 32;
            const QUANTUM: u64 = 1024;
            if [before.rx, before.tx, now.rx, now.tx]
                .iter()
                .any(|b| *b >= MODULUS || *b % QUANTUM != 0)
            {
                return Err(DeltaError::UnknownCounter);
            }
            // Unused interfaces often have no link capacity. Unchanged byte and full-width
            // packet counters carry no traffic to unwrap; they do not invalidate an active
            // interface's machine sum merely because its driver supplies no speed.
            if before.rx == now.rx && before.tx == now.tx && rx_packets == 0 && tx_packets == 0 {
                return Ok(ByteDelta { rx: 0, tx: 0 });
            }
            let bps = now
                .rate_bps
                .filter(|bps| *bps > 0 && before.rate_bps == Some(*bps))
                .ok_or(DeltaError::AmbiguousWrap)?;
            let bound = (u128::from(bps) * u128::from(window_ns)).div_ceil(8_000_000_000)
                + u128::from(QUANTUM - 1);
            if bound >= u128::from(MODULUS) {
                return Err(DeltaError::AmbiguousWrap);
            }
            let direction = |old, new, packets| {
                let delta = (new + MODULUS - old) % MODULUS;
                if u128::from(delta) > bound || (delta > 0 && packets == 0) {
                    Err(DeltaError::Discontinuity)
                } else {
                    Ok(delta)
                }
            };
            Ok(ByteDelta {
                rx: direction(before.rx, now.rx, rx_packets)?,
                tx: direction(before.tx, now.tx, tx_packets)?,
            })
        }
    }
}

#[cfg(test)]
#[path = "net_counter_tests.rs"]
mod tests;
