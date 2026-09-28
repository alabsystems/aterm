// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SOUND CUES — the key-time click ledger and the cues a move or a key
//! mints for the host's synth.

use super::*;

/// One recorded sound trigger — the aural twin of a visual spawn, drained by
/// the host after [`CursorGlow::tick`] and mapped onto a
/// [`crate::trail_sound::SoundEvent`] (the host owns style, focus/motion
/// gating, volume, and pan normalization; the engine owns the WHEN and the
/// classification, which it already computed for the light).
#[derive(Clone, Copy, Debug)]
pub struct SoundCue {
    pub kind: crate::trail_sound::SoundKind,
    /// The arrival column (grid cells) — the host maps it to stereo pan.
    pub col: u16,
    /// The blaze 0..1 at the moment of the move (typing heat / jump flare).
    pub heat: f32,
    /// The live sweep hue 0..1 (phaser's laid-hue band; other styles ignore).
    pub hue: f32,
    /// CURSOR-MOVEMENT direction: +1 for a rightward / upward move, -1 for
    /// left / down, 0 for gestures with no travel direction (typed, jump,
    /// backspace, kill). For a Glide/Sweep the SAME sign also rides INSIDE
    /// `kind` (its enum payload), so the host's generic drain forwards the
    /// direction to the synth with zero mapping — this field is the explicit,
    /// host-facing twin (and what the classification proofs assert on).
    pub dir: i8,
    /// THE GLYPH WAS SHIFTED — a capital or a shifted symbol, forwarded to
    /// [`crate::trail_sound::SoundEvent::shifted`] so the keystroke sings an
    /// octave up (owner ask, 2026-08-30).
    ///
    /// ONLY THE KEY-TIME SEAM CAN SET IT ([`CursorGlow::cue_keystroke_shifted`]),
    /// because shiftedness lives in the key event's modifier flags. Every cue
    /// this engine mints from OBSERVED CURSOR MOTION after PTY output —
    /// `cue_move_sound`'s Typed/Jump/Backspace/Glide/Sweep, the landing
    /// starburst, the poof family — carries `false`, and must: an echo has no
    /// key behind it and inventing one would put a capital's octave on
    /// program output. `false` is the exact pre-flag identity.
    pub shifted: bool,
}

impl CursorGlow {
    /// Sound-cue backlog cap. Cues are drained every host tick; the cap only
    /// matters on a stalled host (minimized window), where dropping the
    /// overflow is exactly right — replaying a burst of stale keystroke
    /// sounds on un-minimize would be noise, not feedback.
    pub(super) const MAX_SOUND_CUES: usize = 8;

    /// Record one sound cue (drops when the backlog is full). Non-cursor
    /// gestures carry no travel direction (`dir = 0`).
    pub(super) fn cue_sound(&mut self, kind: crate::trail_sound::SoundKind, col: u16) {
        self.cue_sound_at(kind, col, self.blaze());
    }

    /// [`Self::cue_sound`] with the blaze supplied rather than sampled — the
    /// seam the KEY-TIME click needs. Every echo-born cue passes `blaze()` and
    /// is byte-identical; only [`Self::cue_keystroke`] passes something else
    /// (the heat its own keystroke is about to produce, which `self.heat` does
    /// not yet carry because the echo that charges it has not arrived).
    pub(super) fn cue_sound_at(
        &mut self,
        kind: crate::trail_sound::SoundKind,
        col: u16,
        heat: f32,
    ) {
        self.cue_sound_shifted(kind, col, heat, false);
    }

    /// [`Self::cue_sound_at`] with SHIFTEDNESS supplied — the key-time seam's
    /// own arm. Every other caller goes through `cue_sound_at` and gets
    /// `false`, which is what keeps every echo-born cue byte-identical: an
    /// echo has no key event behind it and cannot know.
    pub(super) fn cue_sound_shifted(
        &mut self,
        kind: crate::trail_sound::SoundKind,
        col: u16,
        heat: f32,
        shifted: bool,
    ) {
        if self.sound_cues.len() < Self::MAX_SOUND_CUES {
            self.sound_cues.push(SoundCue {
                kind,
                col,
                heat,
                hue: self.hue,
                dir: 0,
                shifted,
            });
        }
    }

    /// How long a KEY-TIME click credit stays spendable, measured from the
    /// NEWEST key. It must cover the whole key→PTY→shell→PTY→observed-echo
    /// round trip, or the echo arrives with no credit left and the character
    /// clicks twice; it must stay SHORT, or a key an app swallowed banks
    /// silence that eats a later output-driven click. 0.75 s covers a local
    /// echo (µs), a loaded flood frame (tens of ms), and an ordinary
    /// intercontinental ssh hop (~300 ms) with margin, while a swallowed key
    /// can mute at most this much of the output-only stream.
    pub(super) const KEYED_CLICK_FRESH: f32 = 0.75;

    /// Ceiling on unspent key-time click credits. Same bound as the cue
    /// backlog: no echo run can be longer than the cues it could have
    /// produced, so anything past this is a key that never echoed.
    pub(super) const MAX_KEYED_CLICKS: u8 = Self::MAX_SOUND_CUES as u8;

    /// HOST KEY-SEAM: click ONE typed glyph at the PHYSICAL keypress instead
    /// of at its echo. Returns whether a cue was recorded (the host uses it to
    /// ask for the redraw that drains it — see below).
    ///
    /// WHY NOT AT THE SPAWN EDGE: a cue born there arrives after key → PTY →
    /// shell → PTY → parse → the next presented frame. On a flooded session (the
    /// reader holds the term lock through whole bursts) or any remote link, that
    /// is tens to hundreds of milliseconds AFTER the finger moved — comfortably
    /// past the ~20 ms at which a click stops feeling attached to the key, on top
    /// of the ~21 ms of already-enqueued audio the output queue carries. Cueing
    /// here removes the echo round trip AND the frame quantization from the audio
    /// path: the only remaining delay is one drain (the host requests a redraw
    /// on `true`; the drain runs ahead of the present early-out, so a
    /// pixel-identical frame still delivers the click and still presents
    /// nothing).
    ///
    /// SILENCE LAW: a cue born at the key cannot inherit the spawn edge's
    /// "unreachable while dark" proof, so it is gated on [`Self::sound_live`] —
    /// the master switch, real geometry, and the host's `audible` verdict
    /// (unfocus only — reduced motion and load shed dim the light without
    /// closing the seam) as of the last tick. A host that never ticks records
    /// nothing at all. Everything downstream (focus, the sound knob, volume,
    /// the resize quiet window) is host policy applied at the drain, exactly as
    /// for an echo cue.
    ///
    /// The cue's column is the last KNOWN cursor cell — the pre-echo one, so
    /// its stereo pan sits one cell left of where the echo cue would have put
    /// it. Sub-cell panning is inaudible; being early is not.
    ///
    /// TIMBRE — the cue carries the heat this keystroke is ABOUT to produce,
    /// not the heat that preceded it. An echo-born cue is recorded AFTER
    /// `spawn`'s heat ramp, so it rides the freshly-charged blaze; sampling
    /// `blaze()` here instead would put the sound one whole keystroke behind the
    /// light — the same "one round trip late" quality the seam exists to delete,
    /// moved from the click's TIMING into its TONE. It
    /// is audible, not academic: heat scales the synth's level as
    /// `0.55 + 0.45·heat` and its ember layer as `0.1 + 0.28·heat`, so one
    /// missing [`Self::HEAT_GAIN`] step measures 0.81-1.49 dB on the RENDERED
    /// click (+3.2 dB on the ember layer taken alone) — at the loudness JND,
    /// in the same direction, on every click of the cold→hot ramp of every
    /// burst. Pinned by `one_keystroke_of_heat_lag_is_audible` in
    /// `trail_sound`, which is where the numbers come from.
    ///
    /// So reconstruct what `spawn` will compute: cool the standing heat/flare
    /// by the gap since the last tick's lazy decay (`heat_at`), then charge the
    /// SAME cadence credit the echo will, off the KEY clock rather than the
    /// echo clock — under a flood the key intervals are the true typing rhythm
    /// and the echo intervals are frame quantization. `self.heat` itself is
    /// deliberately NOT written: the light must keep charging at the echo,
    /// where the classification (forward / deletion / navigation) is actually
    /// known, so this prediction cannot alter a single pixel.
    ///
    /// `hue` needs no such treatment — it is a free-running sweep phase with no
    /// keystroke term, and the key-time sample is the phase the ear hears the
    /// click AT, which is the more correct one of the two.
    pub fn cue_keystroke(&mut self, now: Instant) -> bool {
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Typed)
    }

    /// [`Self::cue_keystroke`] with the gesture SPELLED by the host — the
    /// spacebar's comma ([`trail_sound::SoundKind::Space`]) rides here, since
    /// only the host knows the pressed key's identity and the engine only
    /// ever sees the echo's cursor move. Everything else — silence law,
    /// backlog bound, column, timbre prediction, the echo-muting credit — is
    /// the one shared construction.
    pub fn cue_keystroke_kind(
        &mut self,
        now: Instant,
        kind: crate::trail_sound::SoundKind,
    ) -> bool {
        self.cue_keystroke_shifted(now, kind, false)
    }

    /// [`Self::cue_keystroke_kind`] with SHIFTEDNESS spelled by the host — the
    /// capital's octave lift ([`SoundCue::shifted`]).
    ///
    /// The host prices it because only the host has the modifier flags; this
    /// engine sees echoes, and an echo cannot tell `A` from `a`. Nothing else
    /// about the cue changes, so an unshifted call is the exact identity.
    pub fn cue_keystroke_shifted(
        &mut self,
        now: Instant,
        kind: crate::trail_sound::SoundKind,
        shifted: bool,
    ) -> bool {
        // Dark ⇒ silent, by the same law the spawn edge gets for free.
        if !self.sound_live {
            return false;
        }
        // Backlog full ⇒ the host is not draining (minimized, wedged). Record
        // NOTHING — including no credit, so the echo path is unchanged — rather
        // than bank a click that could only play stale.
        if self.sound_cues.len() >= Self::MAX_SOUND_CUES {
            return false;
        }
        let col = self
            .last
            .or_else(|| self.last_visible.map(|(c, _)| c))
            .map_or(0, |(_, c)| c);
        let heat = self.keyed_blaze(now);
        self.cue_sound_shifted(kind, col, heat, shifted);
        self.key_cue_at = Some(now);
        self.keyed_clicks = self
            .keyed_clicks
            .saturating_add(1)
            .min(Self::MAX_KEYED_CLICKS);
        self.keyed_click_at = Some(now);
        true
    }

    /// The BARE-SHIFT lift cue ([`trail_sound::SoundKind::Shift`]) — the
    /// key-time seam for a press that has NO echo: a modifier moves no
    /// cursor, so nothing here banks an echo-muting credit or stamps the
    /// typing-cadence clock ([`Self::key_cue_at`] belongs to keystrokes; a
    /// shift is not typing rhythm, it is the hand shaping the next beat).
    /// Same silence law and backlog bound as [`Self::cue_keystroke`]; the
    /// heat is the standing blaze cooled to `now` WITHOUT the cadence charge
    /// no echo will ever earn. Take it back with [`Self::take_key_cue`]
    /// immediately after a `true`, exactly like the typed click.
    pub fn cue_modifier(&mut self, now: Instant) -> bool {
        if !self.sound_live {
            return false;
        }
        if self.sound_cues.len() >= Self::MAX_SOUND_CUES {
            return false;
        }
        let col = self
            .last
            .or_else(|| self.last_visible.map(|(c, _)| c))
            .map_or(0, |(_, c)| c);
        let (heat, flare) = self.cooled_blaze(now);
        self.cue_sound_at(
            crate::trail_sound::SoundKind::Shift,
            col,
            heat.max(flare).clamp(0.0, 1.0),
        );
        true
    }

    /// HOST PASTE-SEAM (RAINBOW-KITTY-V2.md §28, the even hand): a paste
    /// landed, cue its ONE up-strum ([`trail_sound::SoundKind::Strum`]).
    /// Keyed off the event KIND at `App::input_paste` — a human's Cmd-V and
    /// an agent's `paste` / `turn` remainder cue it alike; the seam never
    /// learns who pasted. Returns whether a cue was recorded.
    ///
    /// THE MUSIC BOX'S ALONE: under any of the nine other styles this
    /// records nothing and banks nothing, so their paste — the echo's own
    /// Jump, as it always was — is byte-identical. Under the rainbow kitty it
    /// is the keystroke construction verbatim ([`Self::cue_keystroke_kind`]:
    /// silence law, backlog bound, column, timbre prediction) INCLUDING the
    /// echo credit: the paste's own echo spends it and stays silent, so the
    /// strum is the one sound of the gesture rather than a strum with a
    /// brrrring on top.
    pub fn cue_paste(&mut self, now: Instant) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Strum)
    }

    /// The blaze a key-time click carries: the standing heat/flare cooled to
    /// `now` exactly as the next tick's lazy decay will cool them, plus the
    /// cadence credit this keystroke's own echo is about to add. Pure read —
    /// see the TIMBRE contract on [`Self::cue_keystroke`].
    ///
    /// The τ/gain come from the last drawing tick's snapshot, so a Fire session
    /// predicts on `FIRE_HEAT_GAIN`/`FIRE_HEAT_TAU` and a Trail Pack on its own
    /// overrides. `heat_tau_live == 0` is unreachable behind `sound_live`, but
    /// a zero τ would divide to `-inf`; fall back to the standing heat rather
    /// than emit a NaN into the synth.
    pub(super) fn keyed_blaze(&self, now: Instant) -> f32 {
        let (heat, flare) = self.cooled_blaze(now);
        let gap = self
            .key_cue_at
            .map_or(f32::MAX, |t| now.saturating_duration_since(t).as_secs_f32());
        let charged = (heat + Self::heat_cadence(gap) * self.heat_gain_live).min(1.0);
        charged.max(flare).clamp(0.0, 1.0)
    }

    /// The standing heat/flare cooled to `now` exactly as the next tick's
    /// lazy decay will cool them — [`Self::keyed_blaze`] without the cadence
    /// charge, which is the honest blaze for a cue whose press earns no echo
    /// (the shift lift). Pure read.
    pub(super) fn cooled_blaze(&self, now: Instant) -> (f32, f32) {
        let dt = self
            .heat_at
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        if dt > 0.0 && self.heat_tau_live > 0.0 {
            (
                self.heat * (-dt / self.heat_tau_live).exp(),
                self.flare * (-dt / Self::FLARE_DECAY_TAU).exp(),
            )
        } else {
            (self.heat, self.flare)
        }
    }

    /// Spend one key-time click credit if one is fresh — `true` means this
    /// echo's click ALREADY played at the key and the echo must stay silent.
    /// Stale credits are dropped wholesale (see [`Self::KEYED_CLICK_FRESH`]),
    /// so an un-echoed burst cannot mute the stream indefinitely.
    pub(super) fn take_keyed_click(&mut self, now: Instant) -> bool {
        if self.keyed_clicks == 0 {
            return false;
        }
        let fresh = self.keyed_click_at.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::KEYED_CLICK_FRESH
        });
        if !fresh {
            self.keyed_clicks = 0;
            self.keyed_click_at = None;
            return false;
        }
        self.keyed_clicks -= 1;
        if self.keyed_clicks == 0 {
            self.keyed_click_at = None;
        }
        true
    }

    /// Drop every unspent key-time click credit — the disable/reset twin of
    /// clearing [`Self::sound_cues`]: a credit that outlived its cue would mute
    /// the first echo click after the aurora comes back.
    pub(super) fn clear_keyed_clicks(&mut self) {
        self.keyed_clicks = 0;
        self.keyed_click_at = None;
    }

    /// A CURSOR-MOVEMENT threshold (chebyshev cells): an already-admitted move
    /// of at least this distance is a fast/coalesced Sweep; a shorter one is a
    /// Glide. The universal candidate gate runs first for every style and cue.
    pub(super) const SWEEP_MIN_DIST: f32 = 2.0;

    /// Record one CURSOR-MOVEMENT cue (Glide/Sweep) carrying the travel
    /// direction, so the synth can play it IN the current melody/key. The
    /// direction rides both inside `kind` (the synth's carrier) and on the
    /// explicit `dir` field (the host-facing twin the proofs read).
    pub(super) fn cue_cursor(&mut self, kind: crate::trail_sound::SoundKind, col: u16, dir: i8) {
        if self.sound_cues.len() < Self::MAX_SOUND_CUES {
            self.sound_cues.push(SoundCue {
                kind,
                col,
                heat: self.blaze(),
                hue: self.hue,
                dir,
                // A cursor motion is not a glyph: nothing was typed, so
                // nothing was shifted.
                shifted: false,
            });
        }
    }

    /// Drain the sound cues recorded since the last call (the host feeds them
    /// to the trail synth after [`Self::tick`]). Empty — and free — whenever
    /// the aurora is disabled, so a muted/reduced-motion session does zero
    /// sound work by construction.
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        // SEAM POINT 11 (§17.2, D14): v2 flushes every cue it mints into
        // THIS backlog inside `tick` (`rk::Frame::cues`), so its own drain is
        // empty between ticks by construction; appending it here is what lets
        // a non-ticking path prove nothing minted is ever left behind.
        // `take_key_cue` is untouched on purpose — it pops the cue
        // `cue_keystroke` just recorded, and v2 mints nothing on a key edge.
        if self.v2.engaged() {
            self.sound_cues.extend(self.v2.drain_sound_cues());
        }
        self.sound_cues.drain(..)
    }

    /// TAKE BACK the cue [`Self::cue_keystroke`] just recorded, so the host can
    /// hand it to the synth AT THE KEY instead of waiting for the next render
    /// tick's [`Self::drain_sound_cues`].
    ///
    /// WHY THE KEY SEAM STILL RECORDS AND THEN TAKES, rather than the engine
    /// simply handing the cue back: the record is what runs the silence law
    /// ([`Self::sound_live`]), the backlog bound, the column sample, and the
    /// timbre prediction. Splitting that into a second construction path is how
    /// a key-time click and an echo-time click drift apart. So the cue is built
    /// exactly once, by the one function that knows how, and this only changes
    /// WHO carries it to the speaker.
    ///
    /// Valid ONLY immediately after a `true` from `cue_keystroke` — it pops the
    /// most recent cue, which is that one. Anything older stays queued for the
    /// frame drain, so a host that never calls this is byte-identical.
    pub fn take_key_cue(&mut self) -> Option<SoundCue> {
        self.sound_cues.pop()
    }

    /// Drop the SOUND ledger: the pending cues and the keyed-click credits
    /// that account for them. Split out of [`Self::clear_transient_state`]
    /// for the same reason [`Self::clear_thermals`] is a sibling rather than
    /// a body — the callers genuinely differ, so the difference is stated
    /// ONCE at each call site instead of hidden inside a shared teardown.
    ///
    /// WHO CALLS IT: [`Self::reset`] and the master-off dark return, both
    /// unconditionally (a credit banked before the switch, or in a coordinate
    /// space that just died, must not mute the first click of the next one);
    /// and [`Self::settle_key_seam`] — the zero-amplitude return, a
    /// degenerate grid, the classic wake — ONLY when the tick is inaudible. A shed
    /// or motion-reduced frame is dark but still heard, so its banked credits
    /// must survive — the in-flight echoes of keys typed during the shed
    /// arrive after the latch flaps back to lit, find their credits, and stay
    /// silent. Wiping them per dark frame would double-click the whole burst.
    ///
    /// NOTHING VISUAL BELONGS HERE. The audio half must never resurrect
    /// visual state: geometry, the candidate cohort, the held park, the erase
    /// licences and the type-press ring all still clear on every dark tick.
    pub(super) fn clear_sound_ledger(&mut self) {
        self.sound_cues.clear();
        self.clear_keyed_clicks();
    }

    /// Snapshot the key-time click's TIMBRE inputs ([`Self::heat_gain_live`],
    /// [`Self::heat_tau_live`]) off this tick's config. Both are pure
    /// functions of `cfg` that never read `intensity`. Every tick that may
    /// OPEN the key seam calls this first, so the seam never opens on a
    /// stale or `Default` thermal snapshot.
    pub(super) fn store_key_timbre(&mut self, cfg: &GlowConfig) {
        self.heat_gain_live = Self::heat_gain(cfg, matches!(cfg.style, GlowStyle::Fire));
        self.heat_tau_live = Self::heat_tau(cfg);
    }
}
