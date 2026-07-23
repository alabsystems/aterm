// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layer 1 — the **normalized telemetry bus**. The host samples real sources (the
//! `metrics_service` snapshot, the engine frame ring, and the `app_fed` named streams)
//! into a [`SignalSet`]; a scene only ever reads this bus, never hardware — so the art
//! is structurally decoupled from the data (and trivially testable with synthetic input).
//!
//! Honesty is built in: a source the OS cannot attribute (per-process GPU/net on macOS)
//! is [`Sig::ABSENT`], never a fabricated `0`. The binding layer ([`crate::bind`]) treats
//! an absent signal as "no drive," so a missing counter never reads as, say, an idle cat.

/// A canonical, host-independent signal a scene can bind to. Values are *normalized* by
/// the host into `[0,1]` (`norm`) for behaviour mapping, while `value`/`rate` keep the
/// natural units for any readout. App-fed streams are addressed by name (see
/// [`SignalSet::app`]) rather than by this enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SignalKey {
    /// System CPU busy fraction (0..1 of all cores).
    Cpu,
    /// System memory used fraction (0..1).
    Mem,
    /// System GPU utilization fraction (0..1).
    Gpu,
    /// System disk throughput, normalized against a rolling window.
    Disk,
    /// System network receive rate, normalized.
    NetRx,
    /// System network transmit rate, normalized.
    NetTx,
    /// This session's CPU fraction (aterm's process subtree).
    SesCpu,
    /// This session's memory fraction.
    SesMem,
    /// Render frames-per-second, normalized against 60.
    Fps,
    /// Frame render time, normalized (higher = slower = worse).
    FrameMs,
    /// Present latency, normalized.
    PresentMs,
    /// Slow-frame indicator (0 = smooth, 1 = a slow frame occurred recently).
    SlowFrames,
}

impl SignalKey {
    /// Every key, in a stable order (for iteration / the `controls scenes` dump).
    pub const ALL: [SignalKey; 12] = [
        SignalKey::Cpu,
        SignalKey::Mem,
        SignalKey::Gpu,
        SignalKey::Disk,
        SignalKey::NetRx,
        SignalKey::NetTx,
        SignalKey::SesCpu,
        SignalKey::SesMem,
        SignalKey::Fps,
        SignalKey::FrameMs,
        SignalKey::PresentMs,
        SignalKey::SlowFrames,
    ];

    /// The number of distinct keys (the [`SignalSet`] backing-array length).
    pub const COUNT: usize = Self::ALL.len();

    /// Dense index into the [`SignalSet`] backing array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The canonical dotted name used in manifests (`sys.cpu`, `engine.frame_ms`, …).
    #[must_use]
    pub const fn dotted(self) -> &'static str {
        match self {
            SignalKey::Cpu => "sys.cpu",
            SignalKey::Mem => "sys.mem",
            SignalKey::Gpu => "sys.gpu",
            SignalKey::Disk => "sys.disk",
            SignalKey::NetRx => "net.rx",
            SignalKey::NetTx => "net.tx",
            SignalKey::SesCpu => "ses.cpu",
            SignalKey::SesMem => "ses.mem",
            SignalKey::Fps => "engine.fps",
            SignalKey::FrameMs => "engine.frame_ms",
            SignalKey::PresentMs => "engine.present_ms",
            SignalKey::SlowFrames => "engine.slow_frames",
        }
    }

    /// Parse a dotted name back to a key (manifest loading). `None` for an unknown /
    /// app-fed name (those are looked up in [`SignalSet::app`] instead).
    #[must_use]
    pub fn parse(s: &str) -> Option<SignalKey> {
        SignalKey::ALL.into_iter().find(|k| k.dotted() == s)
    }
}

/// One sampled signal: normalized behaviour value `norm ∈ [0,1]`, plus the raw `value`
/// and derived `rate` for any readout. `present` is the honesty flag — `false` means the
/// source was unavailable this sample and `norm`/`value` are meaningless.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sig {
    /// Normalized behaviour value in `[0,1]` (host auto-scales rates/utilization).
    pub norm: f32,
    /// Raw value in natural units (fraction, bytes/s, ms, …) for readouts.
    pub value: f64,
    /// Derived per-second slope, for counters/streams (0 otherwise).
    pub rate: f64,
    /// `true` if the source produced a real sample this tick (else fabricated-free).
    pub present: bool,
}

impl Sig {
    /// An absent signal — the source was unavailable. Behaviour reads as neutral `0`.
    pub const ABSENT: Sig = Sig {
        norm: 0.0,
        value: 0.0,
        rate: 0.0,
        present: false,
    };

    /// A present signal with a normalized behaviour value (raw value/rate optional).
    #[must_use]
    pub fn norm(norm: f32, value: f64, rate: f64) -> Sig {
        Sig {
            norm: crate::clampf(norm, 0.0, 1.0),
            value,
            rate,
            present: true,
        }
    }
}

impl Default for Sig {
    fn default() -> Self {
        Sig::ABSENT
    }
}

/// One app-fed named stream (`aterm-ctl metric <name> <value>`) — the user-extensible
/// "seed input" channel. The host normalizes `norm` against a rolling window so a scene
/// can bind behaviour to it without knowing the stream's natural scale.
#[derive(Clone, Debug, PartialEq)]
pub struct AppSignal {
    /// The stream name (e.g. `ai.tokens`, `build.pct`).
    pub name: String,
    /// The sampled signal.
    pub sig: Sig,
}

/// The full bus for one frame: the dense system/engine signals plus any app-fed streams.
/// Cheap to build and clone; the host refills it each HUD tick and the scene reads it.
#[derive(Clone, Debug, Default)]
pub struct SignalSet {
    sys: [Sig; SignalKey::COUNT],
    /// App-fed named streams, sorted by name (stable for iteration/readout).
    pub app: Vec<AppSignal>,
}

impl SignalSet {
    /// An all-absent bus (the honest pre-sample state).
    #[must_use]
    pub fn new() -> Self {
        Self {
            sys: [Sig::ABSENT; SignalKey::COUNT],
            app: Vec::new(),
        }
    }

    /// Set a system/engine signal.
    pub fn set(&mut self, key: SignalKey, sig: Sig) {
        // `key.index()` is the dense discriminant, always `< COUNT`; the checked
        // lookup discharges the bounds obligation (the `None` arm is unreachable).
        if let Some(slot) = self.sys.get_mut(key.index()) {
            *slot = sig;
        }
    }

    /// Read a system/engine signal (absent if never set).
    #[must_use]
    pub fn get(&self, key: SignalKey) -> Sig {
        // Same dense-discriminant invariant as `set`; the fallback is unreachable.
        self.sys.get(key.index()).copied().unwrap_or(Sig::ABSENT)
    }

    /// Add or replace an app-fed stream by name, keeping `app` sorted.
    pub fn set_app(&mut self, name: &str, sig: Sig) {
        match self.app.binary_search_by(|a| a.name.as_str().cmp(name)) {
            Ok(i) => self.app[i].sig = sig,
            Err(i) => self.app.insert(
                i,
                AppSignal {
                    name: name.to_string(),
                    sig,
                },
            ),
        }
    }

    /// Look up an app-fed stream by exact name (absent if not present this frame).
    #[must_use]
    pub fn app(&self, name: &str) -> Sig {
        self.app
            .binary_search_by(|a| a.name.as_str().cmp(name))
            .map(|i| self.app[i].sig)
            .unwrap_or(Sig::ABSENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_the_default_and_honest() {
        let s = SignalSet::new();
        assert!(!s.get(SignalKey::Cpu).present, "unsampled ⇒ absent, not 0");
        assert_eq!(s.get(SignalKey::Cpu).norm, 0.0);
        assert!(!s.app("nope").present);
    }

    #[test]
    fn set_and_get_roundtrip_for_every_key() {
        let mut s = SignalSet::new();
        for (i, k) in SignalKey::ALL.into_iter().enumerate() {
            s.set(k, Sig::norm((i as f32) / 20.0, i as f64, 0.0));
        }
        for (i, k) in SignalKey::ALL.into_iter().enumerate() {
            let g = s.get(k);
            assert!(g.present);
            assert!((g.norm - (i as f32) / 20.0).abs() < 1e-6);
        }
    }

    #[test]
    fn app_streams_stay_sorted_and_lookup_works() {
        let mut s = SignalSet::new();
        s.set_app("zeta", Sig::norm(0.9, 9.0, 1.0));
        s.set_app("alpha", Sig::norm(0.1, 1.0, 0.0));
        s.set_app("mid", Sig::norm(0.5, 5.0, 0.0));
        let names: Vec<&str> = s.app.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["alpha", "mid", "zeta"], "kept sorted");
        assert_eq!(s.app("mid").value, 5.0);
        // replace keeps order + count
        s.set_app("mid", Sig::norm(0.6, 6.0, 0.0));
        assert_eq!(s.app.len(), 3);
        assert_eq!(s.app("mid").value, 6.0);
    }

    #[test]
    fn dotted_names_roundtrip() {
        for k in SignalKey::ALL {
            assert_eq!(SignalKey::parse(k.dotted()), Some(k));
        }
        assert_eq!(SignalKey::parse("app.ai.tokens"), None);
    }

    #[test]
    fn norm_is_clamped() {
        assert_eq!(Sig::norm(2.0, 0.0, 0.0).norm, 1.0);
        assert_eq!(Sig::norm(-1.0, 0.0, 0.0).norm, 0.0);
    }
}
