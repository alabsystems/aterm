// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::sync::Arc;

use super::*;
use crate::FetchStage;
use crate::metal::encoder::EncodeSession;
use crate::metal::loss::LossLatch;
use crate::metal::test_support::device;
use crate::pipeline_table::ShaderLibrary;

/// Stages that each lose a byte the way a plausible twin would, for the controls.
const MUTANTS: &str = r"
// Stores the byte's Unorm value: right on the Unorm view, one gamma off through the sRGB view.
fragment float4 fs_fetch_probe_store_unorm(BgVsOut in [[stage_in]]) {
    return float4(float3(fetch_probe_bytes(in.pos)) / 255.0, 1.0);
}
// Takes the fetched linear value for the byte.
fragment float4 fs_fetch_probe_trip_linear(BgVsOut in [[stage_in]],
                                           float4 dst [[color(0)]]) {
    return float4(fetch_store(uint3(rint(dst.rgb * 255.0))), dst.a);
}
// Recovers every byte but 128, which comes back one higher.
fragment float4 fs_fetch_probe_trip_one_off(BgVsOut in [[stage_in]],
                                            float4 dst [[color(0)]]) {
    uint3 b = fetch_bytes(dst.rgb);
    return float4(fetch_store(b + uint3(b == uint3(128u))), dst.a);
}
// Reads every byte one higher than it is, up to 255.
fragment float4 fs_fetch_probe_trip_plus_one(BgVsOut in [[stage_in]],
                                             float4 dst [[color(0)]]) {
    return float4(fetch_store(min(fetch_bytes(dst.rgb) + 1u, uint3(255u))), dst.a);
}
";

struct Rig {
    mint: MetalResourceDevice,
    session: EncodeSession,
}

/// A rig on this machine's device, or `None` where the device has no framebuffer fetch.
fn rig() -> Option<Rig> {
    let dev = device();
    if !capable(&dev) {
        return None;
    }
    let latch = Arc::new(LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
    let mint = MetalResourceDevice::new(&dev, latch);
    Some(Rig { mint, session })
}

/// `cell.metal` as this device compiles it, with [`MUTANTS`] after it.
fn library_with_mutants(dev: &Device) -> Library {
    let src = format!(
        "{}\n{MUTANTS}",
        crate::metal::shaders::source_for(ShaderLibrary::Cell, true)
    );
    dev.new_library(&src)
        .unwrap_or_else(|e| panic!("cell.metal with the probe mutants failed to compile:\n{e}"))
}

#[test]
fn the_device_returns_every_byte_through_its_srgb_store_and_fetch() {
    let Some(rig) = rig() else {
        let dev = device();
        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = MetalResourceDevice::new(&dev, latch);
        let lib = crate::metal::pipelines::compile_library(&dev, ShaderLibrary::Cell)
            .expect("cell.metal compiles without the fetch twins");
        assert_eq!(
            verdict(&mint, &session, &lib),
            FetchProbe::Unsupported,
            "a device without framebuffer fetch is never probed"
        );
        return;
    };
    let lib = crate::metal::pipelines::compile_library(rig.mint.device(), ShaderLibrary::Cell)
        .expect("cell.metal compiles with the fetch twins");
    let got = probe(&rig.mint, &rig.session, &lib, SHIPPED);
    assert_eq!(
        got,
        FetchProbe::Passed,
        "{}: the sRGB store and fetch must return all 256 bytes of every channel",
        rig.mint.device().name()
    );
    assert_eq!(
        verdict(&rig.mint, &rig.session, &lib),
        FetchProbe::Passed,
        "the verdict is the shipped probe's"
    );
    assert_eq!(
        known(rig.mint.device(), &lib),
        Some(FetchProbe::Passed),
        "and cached for the next mint to know without probing"
    );
}

/// The in-pass rows are not vacuous: the blend stage leaves row 3 off its uploaded bytes on most pixels, so row 2's fetch meets what a fixed-function sRGB blend left in the tile, not the bytes a load gave it.
#[test]
fn the_blend_rows_hold_bytes_the_upload_did_not() {
    let Some(rig) = rig() else {
        aterm_spec::host::could_not_run("the device has no framebuffer fetch");
    };
    let lib = crate::metal::pipelines::compile_library(rig.mint.device(), ShaderLibrary::Cell)
        .expect("cell.metal compiles with the fetch twins");
    let got = run(&rig.mint, &rig.session, &lib, SHIPPED).expect("the probe runs");
    let up = upload_rows();
    let moved = (0..ROW as usize)
        .filter(|x| got[3 * ROW_LEN + x * 4..][..3] != up[3 * ROW_LEN + x * 4..][..3])
        .count();
    assert!(
        moved > ROW as usize * 3 / 4,
        "the blend moved {moved} of {ROW} pixels"
    );
}

#[test]
fn a_stage_that_loses_a_byte_fails_the_probe() {
    let Some(rig) = rig() else {
        aterm_spec::host::could_not_run("the device has no framebuffer fetch");
    };
    let lib = library_with_mutants(rig.mint.device());
    let run = |stages| probe(&rig.mint, &rig.session, &lib, stages);
    assert_eq!(
        run(SHIPPED),
        FetchProbe::Passed,
        "the shipped stages pass on the mutants' library"
    );
    assert!(
        matches!(
            run(Stages {
                store: "fs_fetch_probe_store_unorm",
                ..SHIPPED
            }),
            FetchProbe::Failed {
                stage: FetchStage::Store,
                ..
            }
        ),
        "a store that skips the sRGB decode loses bytes in the store stage"
    );
    assert!(
        matches!(
            run(Stages {
                trip: "fs_fetch_probe_trip_linear",
                in_pass: "fs_fetch_probe_trip_linear",
                ..SHIPPED
            }),
            FetchProbe::Failed {
                stage: FetchStage::RoundTrip,
                ..
            }
        ),
        "a trip that takes the linear value for the byte loses bytes"
    );
    assert_eq!(
        run(Stages {
            trip: "fs_fetch_probe_trip_one_off",
            in_pass: "fs_fetch_probe_trip_one_off",
            ..SHIPPED
        }),
        FetchProbe::Failed {
            stage: FetchStage::RoundTrip,
            channel: 1,
            want: 128,
            got: 129,
        },
        "one byte one off is found at its first pixel: green 128, at x = 127"
    );
    assert!(
        matches!(
            run(Stages {
                in_pass: "fs_fetch_probe_trip_plus_one",
                ..SHIPPED
            }),
            FetchProbe::Failed {
                stage: FetchStage::InPass,
                ..
            }
        ),
        "a fetch after the pass's own blend that reads another byte than the pass stores \
         fails in the in-pass stage, with the store and the round trip whole"
    );
}

/// A verdict is cached the first time a probe reaches one, and then read rather than probed again; a probe that could not run ([`FetchProbe::Refused`]) is not cached, so the next ask probes again.
#[test]
fn a_verdict_is_cached_once_unless_the_probe_could_not_run() {
    let runs = Cell::new(0u32);
    let ask = |id: u64, v: FetchProbe| {
        settle(id, || {
            runs.set(runs.get() + 1);
            v
        })
    };
    let failed = FetchProbe::Failed {
        stage: FetchStage::Store,
        channel: 0,
        want: 1,
        got: 2,
    };
    // Ids no device carries, so the real device's verdict never meets them.
    let (bad, busy) = (u64::MAX, u64::MAX - 1);
    assert_eq!(ask(bad, failed), (failed, true), "the first ask caches");
    assert_eq!(ask(bad, FetchProbe::Passed), (failed, false));
    assert_eq!(
        runs.get(),
        1,
        "a cached verdict is read, never probed again"
    );

    assert_eq!(ask(busy, FetchProbe::Refused), (FetchProbe::Refused, false));
    assert_eq!(ask(busy, FetchProbe::Refused), (FetchProbe::Refused, false));
    assert_eq!(runs.get(), 3, "a refused probe runs again on the next ask");
    assert_eq!(ask(busy, FetchProbe::Passed), (FetchProbe::Passed, true));
    assert_eq!(ask(busy, failed), (FetchProbe::Passed, false));
    assert_eq!(runs.get(), 4);
}

/// A `cell.metal` that does not compile with its framebuffer-fetch twins loses the twins alone: it compiles without them, the device reads as Unsupported, and the Metal arm still has its library.
#[test]
fn a_cell_library_whose_twins_do_not_compile_compiles_without_them() {
    let dev = device();
    if !capable(&dev) {
        aterm_spec::host::could_not_run("the device has no framebuffer fetch");
    }
    let opts = crate::metal::ffi::CompileOptions::aterm_default().expect("compile options");
    let refuse_twins = |src: &str| {
        if src.starts_with(crate::metal::shaders::FETCH_DEFINE) {
            return Err("injected twin compile failure".to_owned());
        }
        dev.new_library_with_options(src, &opts)
    };
    let lib =
        crate::metal::pipelines::compile_library_with(&dev, ShaderLibrary::Cell, refuse_twins)
            .expect("cell.metal compiles without its twins");
    assert!(lib.function("fs_bg").is_some(), "the frame rows are there");
    assert!(lib.function(SHIPPED.store).is_none(), "the twins are not");
    assert_eq!(known(&dev, &lib), Some(FetchProbe::Unsupported));
    let shipped = crate::metal::pipelines::compile_library(&dev, ShaderLibrary::Cell)
        .expect("cell.metal compiles with its twins");
    assert!(
        shipped.function(SHIPPED.store).is_some(),
        "the shipped compile keeps them"
    );
    assert!(
        crate::metal::pipelines::compile_library_with(&dev, ShaderLibrary::Blit, |_| {
            Err("injected blit failure".to_owned())
        })
        .is_err(),
        "another library's failure is the arm's"
    );
}

#[test]
fn the_judge_names_the_first_lost_byte_by_stage_and_channel() {
    let up = upload_rows();
    let want = expected_rows(&up);
    assert_eq!(judge(&want, &want), FetchProbe::Passed);
    let mut got = want.clone();
    got[5 * 4 + 1] ^= 1;
    assert_eq!(
        judge(&got, &want),
        FetchProbe::Failed {
            stage: FetchStage::Store,
            channel: 1,
            want: 250,
            got: 251,
        }
    );
    let mut got = want.clone();
    got[ROW_LEN + 7 * 4 + 3] = 0;
    assert_eq!(
        judge(&got, &want),
        FetchProbe::Failed {
            stage: FetchStage::RoundTrip,
            channel: 3,
            want: 248,
            got: 0,
        },
        "the round trip keeps its uploaded alpha"
    );
    let mut got = want.clone();
    got[2 * ROW_LEN + 9 * 4] ^= 1;
    assert_eq!(
        judge(&got, &want),
        FetchProbe::Failed {
            stage: FetchStage::InPass,
            channel: 0,
            want: want[2 * ROW_LEN + 9 * 4],
            got: want[2 * ROW_LEN + 9 * 4] ^ 1,
        },
        "row 2 is the in-pass stage"
    );
    let mut got = want.clone();
    got[3 * ROW_LEN + 9 * 4] ^= 1;
    let expect = expected_rows(&got);
    assert!(
        matches!(
            judge(&got, &expect),
            FetchProbe::Failed {
                stage: FetchStage::InPass,
                ..
            }
        ),
        "row 3 sets what row 2 must hold"
    );
}
