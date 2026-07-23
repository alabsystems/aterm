// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layer 2 — the **binding**: a data-driven map from abstract [`Drive`]s (the verbs a
//! scene understands — energy, crowd, arrivals, …) onto signal [`Source`]s. Because the
//! binding is *data*, a user can re-point `energy` from `sys.cpu` to `net.rx` (or their
//! own `app.render.queue`) in the manifest without touching scene code — which is exactly
//! "the animation adapts to the *types* of statistics in that panel."
//!
//! [`Binding::resolve`] folds a [`SignalSet`] into [`Drives`], honoring source
//! availability: an absent signal yields a `0` drive **and** records `present=false`, so
//! a scene can choose a true neutral (cat keeps sleeping) rather than misreading a
//! missing counter as activity.

use crate::clampf;
use crate::signal::{SignalKey, SignalSet};

/// An abstract behaviour input a scene consumes. Scenes use the subset they care about
/// (the Meadow uses `Energy`/`Crowd`/`Arrivals`/`Departures`/`Butterflies`/`Weather`/
/// `Daylight`; Cosmos uses `Traffic`/`Arrivals`; Pulse uses `Value`/`Second`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Drive {
    /// How lively the protagonist is (sleep → play).
    Energy,
    /// Ambient population / occupancy.
    Crowd,
    /// Rate of newcomers entering.
    Arrivals,
    /// Rate of departures.
    Departures,
    /// Rate of "delight" spawns (butterflies / sparks).
    Butterflies,
    /// Adversity / weather (0 = clear, 1 = storm).
    Weather,
    /// Time of day (0 = night, 1 = day); a scene may also self-drive this.
    Daylight,
    /// Aggregate throughput (Cosmos nebula / orbiter density).
    Traffic,
    /// A scene's primary scalar (Pulse headline).
    Value,
    /// A scene's secondary scalar (Pulse second metric).
    Second,
    /// Generic "busyness" for scenes that want one knob.
    Activity,
}

impl Drive {
    /// Every drive, in a stable order.
    pub const ALL: [Drive; 11] = [
        Drive::Energy,
        Drive::Crowd,
        Drive::Arrivals,
        Drive::Departures,
        Drive::Butterflies,
        Drive::Weather,
        Drive::Daylight,
        Drive::Traffic,
        Drive::Value,
        Drive::Second,
        Drive::Activity,
    ];

    /// The number of distinct drives ([`Drives`] backing-array length).
    pub const COUNT: usize = Self::ALL.len();

    /// Dense index into the [`Drives`] backing arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Canonical manifest name (`energy`, `arrivals`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Drive::Energy => "energy",
            Drive::Crowd => "crowd",
            Drive::Arrivals => "arrivals",
            Drive::Departures => "departures",
            Drive::Butterflies => "butterflies",
            Drive::Weather => "weather",
            Drive::Daylight => "daylight",
            Drive::Traffic => "traffic",
            Drive::Value => "value",
            Drive::Second => "second",
            Drive::Activity => "activity",
        }
    }

    /// Parse a manifest drive name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Drive> {
        Drive::ALL.into_iter().find(|d| d.name() == s)
    }
}

/// Where a [`Drive`] gets its value.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A built-in system/engine signal.
    Sys(SignalKey),
    /// An app-fed named stream (`aterm-ctl metric <name> …`).
    App(String),
    /// A fixed constant (always present) — for pinning a drive in a manifest.
    Const(f32),
}

/// The resolved behaviour inputs for one frame: a normalized value per [`Drive`] plus a
/// `present` flag (mirroring [`crate::signal::Sig::present`]) so a scene can distinguish
/// "0 because idle" from "0 because the source is unavailable".
#[derive(Clone, Copy, Debug)]
pub struct Drives {
    v: [f32; Drive::COUNT],
    present: [bool; Drive::COUNT],
}

impl Default for Drives {
    fn default() -> Self {
        Self {
            v: [0.0; Drive::COUNT],
            present: [false; Drive::COUNT],
        }
    }
}

impl Drives {
    /// The normalized value `[0,1]` of a drive (0 if unbound/absent).
    #[must_use]
    pub fn get(&self, d: Drive) -> f32 {
        // `d.index()` is the dense discriminant, always `< COUNT`; the checked
        // lookup discharges the bounds obligation (the fallback is unreachable).
        self.v.get(d.index()).copied().unwrap_or(0.0)
    }

    /// Whether the drive had a real source this frame.
    #[must_use]
    pub fn present(&self, d: Drive) -> bool {
        // Same dense-discriminant invariant as `get`; the fallback is unreachable.
        self.present.get(d.index()).copied().unwrap_or(false)
    }

    // Ergonomic named accessors for the common drives.
    /// Protagonist liveliness.
    #[must_use]
    pub fn energy(&self) -> f32 {
        self.get(Drive::Energy)
    }
    /// Ambient population.
    #[must_use]
    pub fn crowd(&self) -> f32 {
        self.get(Drive::Crowd)
    }
    /// Newcomer rate.
    #[must_use]
    pub fn arrivals(&self) -> f32 {
        self.get(Drive::Arrivals)
    }
    /// Departure rate.
    #[must_use]
    pub fn departures(&self) -> f32 {
        self.get(Drive::Departures)
    }
    /// Delight-spawn rate.
    #[must_use]
    pub fn butterflies(&self) -> f32 {
        self.get(Drive::Butterflies)
    }
    /// Adversity / weather.
    #[must_use]
    pub fn weather(&self) -> f32 {
        self.get(Drive::Weather)
    }
    /// Aggregate throughput.
    #[must_use]
    pub fn traffic(&self) -> f32 {
        self.get(Drive::Traffic)
    }

    /// Daylight, if the binding supplied one (else the scene self-drives it).
    #[must_use]
    pub fn daylight(&self) -> Option<f32> {
        self.present(Drive::Daylight)
            .then(|| self.get(Drive::Daylight))
    }
}

/// A data-driven map `Drive → Source` (+ optional per-drive gain). Build it with the
/// `bind`/`gain` builder, or from a manifest. Resolving against a [`SignalSet`] produces
/// [`Drives`].
#[derive(Clone, Debug, Default)]
pub struct Binding {
    sources: Vec<(Drive, Source)>,
    gains: Vec<(Drive, f32)>,
}

impl Binding {
    /// An empty binding (every drive resolves absent/0).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind (or rebind) `drive` to `source` (builder style).
    #[must_use]
    pub fn bind(mut self, drive: Drive, source: Source) -> Self {
        match self.sources.iter_mut().find(|(d, _)| *d == drive) {
            Some(slot) => slot.1 = source,
            None => self.sources.push((drive, source)),
        }
        self
    }

    /// Set a per-drive gain multiplier (default 1.0). Lets a manifest say e.g. "tokens
    /// drive butterflies, but 3× as eagerly".
    #[must_use]
    pub fn gain(mut self, drive: Drive, gain: f32) -> Self {
        match self.gains.iter_mut().find(|(d, _)| *d == drive) {
            Some(slot) => slot.1 = gain,
            None => self.gains.push((drive, gain)),
        }
        self
    }

    /// The source bound to a drive, if any (for the `controls scenes` dump).
    #[must_use]
    pub fn source(&self, drive: Drive) -> Option<&Source> {
        self.sources
            .iter()
            .find(|(d, _)| *d == drive)
            .map(|(_, s)| s)
    }

    fn gain_of(&self, drive: Drive) -> f32 {
        self.gains
            .iter()
            .find(|(d, _)| *d == drive)
            .map_or(1.0, |(_, g)| *g)
    }

    /// Fold a [`SignalSet`] into [`Drives`]. Each bound drive reads its source's
    /// normalized value × gain (clamped to `[0,1]`); an absent source yields `0` with
    /// `present=false`. Unbound drives stay absent/0.
    #[must_use]
    pub fn resolve(&self, set: &SignalSet) -> Drives {
        let mut out = Drives::default();
        for &(drive, ref source) in &self.sources {
            let (norm, present) = match source {
                Source::Sys(key) => {
                    let s = set.get(*key);
                    (s.norm, s.present)
                }
                Source::App(name) => {
                    let s = set.app(name);
                    (s.norm, s.present)
                }
                Source::Const(c) => (*c, true),
            };
            let i = drive.index();
            // `drive.index()` is the dense discriminant, always `< COUNT`; the
            // checked lookups discharge the bounds obligations (the `None` arms
            // are unreachable).
            if let Some(slot) = out.v.get_mut(i) {
                *slot = clampf(norm * self.gain_of(drive), 0.0, 1.0);
            }
            if let Some(slot) = out.present.get_mut(i) {
                *slot = present;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::Sig;

    fn set_with(cpu: Option<f32>, tokens: Option<f32>) -> SignalSet {
        let mut s = SignalSet::new();
        if let Some(c) = cpu {
            s.set(SignalKey::Cpu, Sig::norm(c, 0.0, 0.0));
        }
        if let Some(t) = tokens {
            s.set_app("ai.tokens", Sig::norm(t, 0.0, 0.0));
        }
        s
    }

    #[test]
    fn resolves_bound_drives_from_signals() {
        let b = Binding::new()
            .bind(Drive::Energy, Source::Sys(SignalKey::Cpu))
            .bind(Drive::Butterflies, Source::App("ai.tokens".into()));
        let d = b.resolve(&set_with(Some(0.7), Some(0.4)));
        assert!((d.energy() - 0.7).abs() < 1e-6);
        assert!((d.butterflies() - 0.4).abs() < 1e-6);
        assert!(d.present(Drive::Energy) && d.present(Drive::Butterflies));
    }

    #[test]
    fn absent_source_is_zero_and_marked_absent() {
        let b = Binding::new().bind(Drive::Energy, Source::Sys(SignalKey::Cpu));
        // CPU not set in the bus → absent.
        let d = b.resolve(&set_with(None, None));
        assert_eq!(d.energy(), 0.0);
        assert!(
            !d.present(Drive::Energy),
            "absent source must be marked not-present (honesty), not a fake 0"
        );
    }

    #[test]
    fn unbound_drive_is_neutral() {
        let b = Binding::new();
        let d = b.resolve(&set_with(Some(0.9), None));
        assert_eq!(d.crowd(), 0.0);
        assert!(!d.present(Drive::Crowd));
        assert!(
            d.daylight().is_none(),
            "unbound daylight ⇒ scene self-drives"
        );
    }

    #[test]
    fn gain_scales_and_clamps() {
        let b = Binding::new()
            .bind(Drive::Butterflies, Source::App("ai.tokens".into()))
            .gain(Drive::Butterflies, 3.0);
        let d = b.resolve(&set_with(None, Some(0.5)));
        assert_eq!(d.butterflies(), 1.0, "0.5×3 clamps to 1.0");
    }

    #[test]
    fn const_source_is_always_present() {
        let b = Binding::new().bind(Drive::Daylight, Source::Const(0.25));
        let d = b.resolve(&SignalSet::new());
        assert_eq!(d.daylight(), Some(0.25));
    }

    #[test]
    fn rebinding_replaces_not_duplicates() {
        let b = Binding::new()
            .bind(Drive::Energy, Source::Sys(SignalKey::Cpu))
            .bind(Drive::Energy, Source::Sys(SignalKey::NetRx));
        assert_eq!(
            b.source(Drive::Energy),
            Some(&Source::Sys(SignalKey::NetRx))
        );
    }
}
