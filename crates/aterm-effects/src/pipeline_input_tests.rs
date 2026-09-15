// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise the web host's public input seam against real terminal echoes.
//! This child module reads existing engine diagnostics without exposing a
//! production-only accessor just for tests.

use super::*;

/// The three glow streams after the shipping pipeline hands them to the host.
/// Equality includes every field, stream boundary and element order.
#[derive(Debug, PartialEq, Eq)]
struct GlowFrame<'a> {
    under: &'a [GlowQuad],
    over: &'a [GlowQuad],
    halos: &'a [RainHalo],
}

impl<'a> GlowFrame<'a> {
    fn read(input: &'a RenderInput) -> Self {
        Self {
            under: &input.glow_under,
            over: &input.cursor_glow_add,
            halos: &input.glow_halo,
        }
    }
}

struct Host {
    pipeline: EffectsPipeline,
    term: Terminal,
    input: RenderInput,
}

impl Host {
    fn new(epoch: Instant) -> Self {
        let mut pipeline = EffectsPipeline::new();
        pipeline.t0 = epoch;
        pipeline.set_cursor_glow(
            true,
            "rainbow kitty",
            None,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(8, 80);
        term.set_default_background(aterm_core::terminal::Rgb {
            r: 12,
            g: 13,
            b: 12,
        });
        term.set_default_foreground(aterm_core::terminal::Rgb {
            r: 230,
            g: 228,
            b: 221,
        });
        term.process(b"\x1b[3;5H");
        let input = term.cell_frame(8, 80);
        let mut host = Self {
            pipeline,
            term,
            input,
        };
        host.present();
        assert!(host.pipeline.glow.v2_status().is_some());
        host
    }

    fn present(&mut self) {
        self.pipeline.advance(40.0);
        self.term.cell_frame_into(&mut self.input, 8, 80);
        self.pipeline.apply(&mut self.term, &mut self.input, 10, 19);
    }

    fn echo(&mut self, ch: char) {
        self.term.process(ch.encode_utf8(&mut [0; 4]).as_bytes());
        self.present();
    }

    fn note(&mut self, ch: char) {
        assert!(
            self.pipeline
                .note_committed_char(&mut self.term, &self.input, ch)
        );
    }

    fn credits(&self) -> usize {
        self.pipeline.glow.in_flight_tally().credits
    }

    /// Compare emitted content in tests; production `Status::fp` remains the
    /// inexpensive idle/non-idle sentinel. Summary counts cannot distinguish
    /// equal-sized frames with different geometry, color or blend fields.
    fn glow_frame(&self) -> GlowFrame<'_> {
        GlowFrame::read(&self.input)
    }
}

#[test]
fn glow_frame_equality_rejects_equal_count_changed_content() {
    // Replay the count-only observer as the negative control, against a frame
    // genuinely emitted by the pipeline before corrupting one rendered field.
    fn count_only(host: &Host) -> [u64; 7] {
        let status = host.pipeline.glow.v2_status().unwrap();
        [
            u64::from(status.quads),
            u64::from(status.halos),
            u64::from(status.stars),
            u64::from(status.meteors),
            u64::from(status.cells),
            u64::from(status.bridged),
            status.fp,
        ]
    }

    let mut host = Host::new(Instant::now());
    for ch in "primer".chars() {
        host.note(ch);
        host.echo(ch);
    }
    let original = host.input.clone();
    let expected = GlowFrame::read(&original);
    assert!(
        !expected.under.is_empty(),
        "the real emitter must draw a ribbon"
    );
    assert_ne!(host.pipeline.glow.v2_status().unwrap().fp, 0);
    assert_eq!(host.glow_frame(), expected);
    let summary = count_only(&host);

    host.input.glow_under[0].color ^= 1;
    assert_eq!(host.input.glow_under.len(), expected.under.len());
    assert_eq!(host.input.cursor_glow_add.len(), expected.over.len());
    assert_eq!(host.input.glow_halo.len(), expected.halos.len());
    assert_eq!(
        count_only(&host),
        summary,
        "the old observer misses changed content"
    );
    assert_ne!(
        host.glow_frame(),
        expected,
        "the stream comparison must catch it"
    );
}

#[test]
fn web_glyph_classes_reach_the_real_rainbow_renderer() {
    // The web host probes only the caret row. First present the blank row
    // above through that real probe, then type beneath its observed sky.
    // This uses normal terminal snapshots, not invented glyph clearance.
    for (ch, class, shifted) in [
        (' ', TypedClass::Space, false),
        ('!', TypedClass::Bang, true),
        ('A', TypedClass::Capital, true),
        ('É', TypedClass::Capital, true),
        ('?', TypedClass::Glyph, true),
        ('a', TypedClass::Glyph, false),
        ('界', TypedClass::Glyph, false),
    ] {
        let epoch = Instant::now();
        let mut web = Host::new(epoch);
        let mut explicit = Host::new(epoch);
        let mut historical = Host::new(epoch);
        for host in [&mut web, &mut explicit, &mut historical] {
            host.term.process(b"\x1b[2;5H");
            host.present();
            host.term.process(b"\x1b[3;5H");
            host.present();
        }
        // Exercise each class within an established, ink-bearing typed row.
        for prefix in "primer".chars() {
            for host in [&mut web, &mut explicit, &mut historical] {
                host.note(prefix);
                host.echo(prefix);
            }
        }
        assert!(web.pipeline.glow.ribbon_segments() > 0);
        let mut differs_from_unclassified = false;
        for _ in 0..16 {
            web.note(ch);
            let width = aterm_grapheme::char_width(ch) as u16;
            explicit
                .pipeline
                .glow
                .note_typed_glyph(explicit.pipeline.now(), width, shifted, class);
            historical
                .pipeline
                .glow
                .note_typed_cells(historical.pipeline.now(), width);
            for host in [&mut web, &mut explicit, &mut historical] {
                host.echo(ch);
            }
            assert_eq!(
                web.glow_frame(),
                explicit.glow_frame(),
                "web {ch:?} must render like the explicitly classified engine event"
            );
            differs_from_unclassified |= web.glow_frame() != historical.glow_frame();
            for _ in 0..2 {
                for host in [&mut web, &mut explicit, &mut historical] {
                    host.present();
                }
                assert_eq!(web.glow_frame(), explicit.glow_frame());
                differs_from_unclassified |= web.glow_frame() != historical.glow_frame();
            }
        }
        // Space must rest the sky, and hero classes must earn their stars.
        // Both visibly differ from the old unclassified route.
        if matches!(
            class,
            TypedClass::Space | TypedClass::Bang | TypedClass::Capital
        ) {
            assert!(
                differs_from_unclassified,
                "negative control must distinguish the old plain-Glyph route for {ch:?}"
            );
        }
    }
}

#[test]
fn web_non_text_intents_preserve_or_retire_the_existing_credit_pool() {
    for (kind, remaining) in [
        (PetInputKind::Text, 3),
        (PetInputKind::Navigate, 3),
        (PetInputKind::Submit, 3),
        (PetInputKind::Paste, 3),
        (PetInputKind::Delete, 1),
        (PetInputKind::Kill, 0),
        (PetInputKind::KillForward, 0),
    ] {
        let mut host = Host::new(Instant::now());
        // Two accepted writes are still in flight: one narrow and one wide.
        // Deleting the newest must retire its entire two-cell credit while
        // preserving the earlier key; a line kill retires both.
        host.note('a');
        host.note('界');
        assert_eq!(host.credits(), 3);
        let seq = host.pipeline.cursor_pet_console_input_seq();
        host.pipeline.note_console_input(kind);
        assert_eq!(host.credits(), remaining, "intent {kind:?}");
        assert_eq!(host.pipeline.pending_keys, 0, "no deferred typed replay");
        assert_eq!(host.pipeline.cursor_pet_console_input_seq(), seq + 1);
        assert_eq!(host.pipeline.cursor_pet_console_input_kind(), Some(kind));
        // Cadence advancement must not quietly recreate credit for this key.
        host.pipeline.advance(16.0);
        assert_eq!(host.credits(), remaining, "after rAF: {kind:?}");
    }
}

#[test]
fn web_delete_dispatch_reaches_erase_without_a_typed_replay() {
    let mut host = Host::new(Instant::now());
    for ch in "abcdef".chars() {
        host.note(ch);
        host.echo(ch);
    }
    assert_eq!(host.credits(), 0);
    let before = host.pipeline.glow.erase_momentum(host.pipeline.now());
    host.pipeline.note_console_input(PetInputKind::Delete);
    assert!(host.pipeline.glow.erase_momentum(host.pipeline.now()) > before);
    host.term.process(b"\x08\x1b[K");
    host.present();
    assert_eq!(host.term.cursor().col, 9);
    assert_eq!(host.credits(), 0);
    assert_eq!(host.pipeline.pending_keys, 0);

    // The old web contract queued a plain typed press for the same Backspace.
    // Run that broken input through the genuine queue, not a forged tally.
    host.pipeline.note_keystroke();
    host.pipeline.advance(16.0);
    assert_eq!(host.credits(), 1, "historical route mints false credit");
}

#[test]
fn web_accepted_printables_conform_to_the_existing_input_credit_model() {
    let model = aterm_spec::derive::cursor_hint_license_model();
    let mut state = model.init_state();
    let mut host = Host::new(Instant::now());
    for ch in ['a', '!'] {
        host.note(ch);
        let expected = model.successors("PressArmsLicense", &state);
        assert_eq!(expected.len(), 1);
        let mut projected = expected[0].clone();
        // Bind the admission/budget portion of the existing model: no echo
        // has arrived, so all accepted one-cell presses remain unpaid.
        projected.insert(
            "arms",
            i64::try_from(host.pipeline.cursor_pet_console_input_seq()).unwrap(),
        );
        projected.insert("credit_arms", i64::try_from(host.credits()).unwrap());
        assert!(expected.contains(&projected));
        let mut duplicate = projected.clone();
        duplicate.insert("credit_arms", projected["credit_arms"] + 1);
        assert!(
            !expected.contains(&duplicate),
            "a second credit is not a second input"
        );
        state = projected;
    }
}
