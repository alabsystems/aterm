// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compute module's layout, limit, ownership, synchronization and parity tests (design §12.8).

use std::collections::BTreeSet;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::time::{Duration, Instant};

use aterm_effects::int_bag_mlp::{
    self, Batch, BatchError, C_MAX, DOMAIN_BYTES_MAX, E_MAX, H_MAX, JOB_BYTES_MAX, JOB_WORDS,
    LAYOUT, MAX_ROW_IDS, PARAMS_BYTES_MAX, PARAMS_WORDS, ParamsError, ParamsView, ROWS_MAX, params,
};
use aterm_effects::tone_int;
use aterm_spec::host::could_not_run;

use super::job::{terminal_outcome, typed};
use super::pipeline::{ECHO, qualification_set};
use super::*;
use crate::metal::ffi::{self, MtlSize};
use crate::metal::loss::{self, CbOutcome, LossLatch};

/// The compute device, or COULD NOT RUN where the listing names no GPU or the GPU is not unified.
fn device() -> ComputeDevice {
    match ComputeDevice::open() {
        Ok(dev) => dev,
        Err(OpenError::NoDevice) => could_not_run("no Metal device in the device listing"),
        Err(OpenError::NotUnified { name }) => {
            could_not_run(format!("{name} has no unified memory"))
        }
        Err(e) => panic!("the compute device did not open: {e:?}"),
    }
}

/// tone-int v1's params on `dev` and their qualified pipeline.
fn tone(dev: &ComputeDevice) -> (Arc<Params>, Pipeline) {
    let params = Params::new(dev, v1()).expect("sealed params");
    let pipeline = Pipeline::build(dev, &params).expect("a qualified tone pipeline");
    (params, pipeline)
}

/// tone-int v1's blob.
fn v1() -> &'static [u8] {
    tone_int::v1().expect("tone-int v1")
}

/// The checked CPU reference's out words.
fn reference(p: &ParamsView<'_>, b: &Batch) -> Vec<u32> {
    let mut out = vec![0u32; b.out_words()];
    int_bag_mlp::forward(p, b, &mut out).expect("forward");
    out
}

/// A prepared job on `dev` with `b` loaded.
fn loaded(dev: &ComputeDevice, params: &Arc<Params>, b: &Batch) -> Job<Prepared> {
    let mut job = Job::new(dev, params);
    job.load(b).expect("loaded");
    job
}

/// Load, submit and wait one job, returning its out words.
fn run(dev: &ComputeDevice, params: &Arc<Params>, pipeline: &Pipeline, b: &Batch) -> Vec<u32> {
    loaded(dev, params, b)
        .submit(pipeline)
        .expect("submitted")
        .wait()
        .expect("completed")
        .out()
        .to_vec()
}

/// The workload's 371 texts repeated cyclically to `rows` rows.
fn cyclic(rows: usize) -> Vec<&'static str> {
    tone_int::texts().into_iter().cycle().take(rows).collect()
}

/// Every out word equal, or the first difference named.
fn assert_words_equal(gpu: &[u32], cpu: &[u32], what: &str) {
    assert_eq!(gpu.len(), cpu.len(), "{what}: out lengths");
    if let Some(i) = gpu.iter().zip(cpu).position(|(g, c)| g != c) {
        panic!(
            "{what}: out word {i} is {:#x} on the GPU and {:#x} in the reference",
            gpu[i], cpu[i]
        );
    }
}

/// `bytes` with header word `at` set to `value`.
fn patched(bytes: &[u8], at: usize, value: u32) -> Vec<u8> {
    let mut b = bytes.to_vec();
    b[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
    b
}

/// Header word `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at * 4..at * 4 + 4].try_into().expect("a word"))
}

/// The prelude text, pinned.
const PRELUDE_GOLDEN: &str = "\
constant constexpr uint JOB_MAGIC = 0;
constant constexpr uint JOB_LAYOUT_VERSION = 1;
constant constexpr uint JOB_ROWS = 2;
constant constexpr uint JOB_TOTAL_IDS = 3;
constant constexpr uint JOB_OUT_STRIDE = 4;
constant constexpr uint PARAMS_MAGIC = 0;
constant constexpr uint PARAMS_LAYOUT_VERSION = 1;
constant constexpr uint PARAMS_B = 2;
constant constexpr uint PARAMS_E = 3;
constant constexpr uint PARAMS_H = 4;
constant constexpr uint PARAMS_C = 5;
constant constexpr uint PARAMS_MIN_IDS = 6;
constant constexpr uint PARAMS_MULT = 7;
constant constexpr uint PARAMS_QMAX = 8;
constant constexpr uint PARAMS_G = 9;
constant constexpr uint PARAMS_T = 10;
constant constexpr uint PARAMS_EMB_OFF = 11;
constant constexpr uint PARAMS_EMB_LEN = 12;
constant constexpr uint PARAMS_W1_OFF = 13;
constant constexpr uint PARAMS_W1_LEN = 14;
constant constexpr uint PARAMS_W2_OFF = 15;
constant constexpr uint PARAMS_W2_LEN = 16;
constant constexpr uint PARAMS_LUT_OFF = 17;
constant constexpr uint PARAMS_LUT_LEN = 18;
constant constexpr uint PARAMS_B1_OFF = 19;
constant constexpr uint PARAMS_B1_LEN = 20;
constant constexpr uint PARAMS_B2_OFF = 21;
constant constexpr uint PARAMS_B2_LEN = 22;
";

/// tone-int v1's shape.
const TONE_SHAPE: Shape = Shape {
    embed: tone_int::DIMS.embed,
    hidden: tone_int::DIMS.hidden,
    classes: tone_int::DIMS.classes,
    min_ids: tone_int::MIN_IDS,
};

/// The identity of the pipeline specialised to [`TONE_SHAPE`], pinned.
const TONE_PIPELINE_ID: &str = "bc559cd082719a2661949c677af1c6d117c3615573f8dd6377300fd0c35a5211";

/// L1: the echo kernel reads every job and params header word by its generated name and writes them out in table order.
#[test]
fn the_kernel_reads_every_header_word_by_its_generated_name() {
    for (name, _) in LAYOUT.fields() {
        let reads = ECHO.matches(&format!("[{name}];")).count();
        assert_eq!(reads, 1, "the echo reads {name} {reads} times");
    }
    let dev = device();
    let mtl = dev.domain.device();
    let lib = mtl
        .new_library(&format!("{}{ECHO}", prelude()))
        .expect("the echo compiles under the prelude");
    let f = lib.function("k_layout_echo").expect("k_layout_echo");
    let pso = mtl.new_compute_pipeline(&f).expect("the echo pipeline");
    let sentinel = |base: u32, i: usize| base + i as u32 * 7 + 1;
    let words = |n: usize, base: u32| -> Vec<u8> {
        (0..n)
            .flat_map(|i| sentinel(base, i).to_le_bytes())
            .collect()
    };
    let total = JOB_WORDS + PARAMS_WORDS;
    let job = mtl.new_buffer(JOB_WORDS * 4).expect("job");
    let params = mtl.new_buffer(PARAMS_WORDS * 4).expect("params");
    let out = mtl.new_buffer((total + 1) * 4).expect("out");
    // SAFETY: each buffer was just made at exactly these lengths in shared storage, and no GPU work references any of them yet.
    unsafe {
        ffi::buffer_write(&job, &words(JOB_WORDS, 0x1000_0000));
        ffi::buffer_write(&params, &words(PARAMS_WORDS, 0x2000_0000));
        ffi::buffer_write(&out, &words(total + 1, 0xd000_0000));
    }
    let one = MtlSize {
        width: 1,
        height: 1,
        depth: 1,
    };
    ffi::dispatch_compute(dev.domain.queue(), &pso, &[&job, &params, &out], one)
        .expect("the echo completes");
    // SAFETY: `out` holds `total + 1` words, and `dispatch_compute` returned after its command buffer completed, so every GPU write landed.
    let got = unsafe { ffi::buffer_u32s(&out, total + 1) };
    let want: Vec<u32> = LAYOUT
        .job
        .iter()
        .map(|&(_, i)| sentinel(0x1000_0000, i))
        .chain(LAYOUT.params.iter().map(|&(_, i)| sentinel(0x2000_0000, i)))
        .chain([sentinel(0xd000_0000, total)])
        .collect();
    assert_eq!(
        got, want,
        "every header word echoed in table order, and no more"
    );
}

/// L2: the prelude names each layout field once at its index, its text and the tone pipeline's identity are pinned, and no salt enters the identity.
#[test]
fn the_prelude_and_the_pipeline_identity_are_pinned() {
    let text = prelude();
    for (name, index) in LAYOUT.fields() {
        let line = format!("constant constexpr uint {name} = {index};");
        assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line}");
        assert_eq!(text.matches(&format!(" {name} ")).count(), 1, "{name}");
    }
    assert_eq!(text.lines().count(), LAYOUT.fields().count());
    assert_eq!(text, PRELUDE_GOLDEN);
    assert_eq!(source(), format!("{PRELUDE_GOLDEN}{KERNEL}"));
    assert_eq!(identity(TONE_SHAPE).hex(), TONE_PIPELINE_ID);
    let other = Shape {
        min_ids: TONE_SHAPE.min_ids + 1,
        ..TONE_SHAPE
    };
    assert_ne!(identity(other), identity(TONE_SHAPE));

    let dev = device();
    let lib = Library::compile(&dev).expect("the library");
    let canonical = Pipeline::new(&lib, TONE_SHAPE).expect("the canonical pipeline");
    let salted_lib = Library::compile_salted(&dev, 0x5a17).expect("the salted library");
    let salted = Pipeline::new_salted(&salted_lib, TONE_SHAPE, RUN_SALT_MIN + 3)
        .expect("the salted pipeline");
    assert_eq!(canonical.identity(), identity(TONE_SHAPE));
    assert_eq!(salted.identity(), canonical.identity());
    assert_eq!((lib.salt(), canonical.run_salt()), (None, None));
    assert_eq!(
        (salted.library_salt(), salted.run_salt()),
        (Some(0x5a17), Some(RUN_SALT_MIN + 3))
    );
    salted
        .qualify()
        .expect("a salted pipeline computes the same function");
    assert_eq!(
        Pipeline::new_salted(&lib, TONE_SHAPE, RUN_SALT_MIN - 1).map(drop),
        Err(PipelineError::SaltTooSmall {
            salt: RUN_SALT_MIN - 1
        })
    );
    for wide in [
        Shape {
            embed: E_MAX + 1,
            ..TONE_SHAPE
        },
        Shape {
            classes: C_MAX + 1,
            ..TONE_SHAPE
        },
        Shape {
            min_ids: 0,
            ..TONE_SHAPE
        },
    ] {
        assert_eq!(
            Pipeline::new(&lib, wide).map(drop),
            Err(PipelineError::BadShape(wide))
        );
    }
    let width = canonical.threadgroup_width();
    assert!(width >= 1 && width <= canonical.max_total_threads_per_threadgroup());
    assert!(width <= canonical.thread_execution_width());
}

/// What a params refusal must match.
type ParamsExpect = fn(&ParamsError) -> bool;
/// What a batch refusal must match.
type BatchExpect = fn(&BatchError) -> bool;

/// L4: every params and batch refusal comes before any buffer exists, and an all-abstain batch with no ids is valid.
#[test]
fn a_refused_input_creates_no_buffer() {
    let dev = device();
    let v1 = v1();
    let at = |i: usize| word(v1, i);
    let mut int8_min = v1.to_vec();
    int8_min[at(params::EMB_OFF) as usize] = 0x80;
    let mut lossy = v1.to_vec();
    let b1 = at(params::B1_OFF) as usize;
    lossy[b1..b1 + 4].copy_from_slice(&2_047i32.to_le_bytes());
    let mut b2_big = v1.to_vec();
    let b2 = at(params::B2_OFF) as usize;
    b2_big[b2..b2 + 4].copy_from_slice(&(1i32 << 29).to_le_bytes());
    let mut trailing = v1.to_vec();
    trailing.extend_from_slice(&[0; 16]);
    let cases: Vec<(&str, Vec<u8>, ParamsExpect)> = vec![
        ("truncated", v1[..8].to_vec(), |e| {
            matches!(e, ParamsError::Truncated { .. })
        }),
        ("magic", patched(v1, params::MAGIC, 0), |e| {
            matches!(e, ParamsError::BadMagic { .. })
        }),
        ("version", patched(v1, params::LAYOUT_VERSION, 2), |e| {
            matches!(e, ParamsError::BadVersion { .. })
        }),
        ("dim", patched(v1, params::E, 0), |e| {
            matches!(e, ParamsError::DimOutOfBounds { .. })
        }),
        ("min ids", patched(v1, params::MIN_IDS, 0), |e| {
            matches!(e, ParamsError::MinIdsZero)
        }),
        ("shift", patched(v1, params::G, 0), |e| {
            matches!(e, ParamsError::ShiftOutOfRange { .. })
        }),
        ("t", patched(v1, params::T, u32::MAX), |e| {
            matches!(e, ParamsError::NegativeT { .. })
        }),
        ("mult", patched(v1, params::MULT, 0), |e| {
            matches!(e, ParamsError::MultOutOfRange { .. })
        }),
        (
            "qmax",
            patched(v1, params::QMAX, at(params::QMAX) + 1),
            |e| matches!(e, ParamsError::QmaxMismatch { .. }),
        ),
        (
            "length",
            patched(v1, params::EMB_LEN, at(params::EMB_LEN) - 1),
            |e| matches!(e, ParamsError::BadLength { .. }),
        ),
        ("outside", v1[..v1.len() - 4].to_vec(), |e| {
            matches!(e, ParamsError::OutsideBlob { .. })
        }),
        (
            "unaligned",
            patched(v1, params::W1_OFF, at(params::W1_OFF) + 1),
            |e| matches!(e, ParamsError::Unaligned { .. }),
        ),
        (
            "overlap",
            patched(v1, params::W1_OFF, at(params::EMB_OFF)),
            |e| matches!(e, ParamsError::Overlap { .. }),
        ),
        ("trailing", trailing, |e| {
            matches!(e, ParamsError::Trailing { .. })
        }),
        ("int8 min", int8_min, |e| {
            matches!(e, ParamsError::Int8Min { .. })
        }),
        ("lossy", lossy, |e| matches!(e, ParamsError::Lossy { .. })),
        ("b2", b2_big, |e| {
            matches!(e, ParamsError::B2OutOfRange { .. })
        }),
    ];
    let before = dev.buffers_created();
    for (what, blob, expected) in &cases {
        match Params::new(&dev, blob) {
            Err(ParamsRefusal::Check(e)) => assert!(expected(&e), "{what}: {e:?}"),
            other => panic!("{what}: {other:?}"),
        }
    }
    assert_eq!(dev.buffers_created(), before, "no params buffer");
    assert_eq!(dev.live_bytes(), 0);

    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let before = dev.buffers_created();
    let mut job = Job::new(&dev, &params);
    assert_eq!(dev.buffers_created(), before, "a new job has no buffers");
    let refusals: [(Vec<u32>, Vec<u16>, BatchExpect); 5] = [
        (vec![0], vec![], |e| matches!(e, BatchError::Empty)),
        (vec![0, 2, 1, 3], vec![0, 1, 2], |e| {
            matches!(e, BatchError::NotMonotone { .. })
        }),
        (vec![0, 2], vec![0, 1, 2], |e| {
            matches!(e, BatchError::BadTotal { .. })
        }),
        (vec![0, 1], vec![2_048], |e| {
            matches!(e, BatchError::IdOutOfRange { .. })
        }),
        (
            vec![0, MAX_ROW_IDS + 1],
            vec![1; MAX_ROW_IDS as usize + 1],
            |e| matches!(e, BatchError::RowTooLong { .. }),
        ),
    ];
    for (offsets, ids, expected) in refusals {
        let e = Batch::pack(&view, offsets, ids).expect_err("refused");
        assert!(expected(&e), "{e:?}");
    }
    let extreme = int_bag_mlp::extreme_params();
    let wide = int_bag_mlp::check(&extreme).expect("the extreme blob");
    let rows = ROWS_MAX as usize;
    assert!(matches!(
        Batch::pack(&wide, vec![0; rows + 2], vec![]),
        Err(BatchError::TooManyRows { .. })
    ));
    assert!(matches!(
        Batch::pack(&wide, vec![0; rows + 1], vec![]),
        Err(BatchError::TooLarge { .. })
    ));
    let foreign = tone_int::pack(&wide, &[tone_int::B1_TEXT]).expect("a batch at other dims");
    assert!(matches!(
        job.load(&foreign),
        Err(LoadRefusal::DimsMismatch { .. })
    ));
    assert_eq!(dev.buffers_created(), before, "no job buffer");

    let abstain = Batch::pack(&view, vec![0, 0, 0], vec![]).expect("all abstain, no ids");
    assert_eq!(abstain.total_ids(), 0);
    job.load(&abstain).expect("loaded");
    assert_eq!(dev.buffers_created(), before + 4, "four job buffers");
    let done = job
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    let c = tone_int::DIMS.classes;
    assert_eq!(done.out(), [0, 0, 0, 0, 0, c, 0, 0, 0, 0, 0, c]);
    assert_eq!(done.out(), reference(&view, &abstain));
}

/// L5: each byte budget is refused by name before allocation, the domain's with jobs in flight.
#[test]
fn each_byte_budget_is_refused_by_name_before_allocation() {
    let dev = device();
    let limits = dev.limits();
    assert!(limits.max_buffer_length > 0 && limits.working_set > 0);
    assert_eq!(
        limits.params,
        PARAMS_BYTES_MAX.min(limits.max_buffer_length)
    );
    assert_eq!(limits.job, JOB_BYTES_MAX.min(limits.max_buffer_length));
    assert_eq!(limits.domain, DOMAIN_BYTES_MAX.min(limits.working_set));
    assert_eq!(
        (limits.params, limits.job, limits.domain),
        (PARAMS_BYTES_MAX, JOB_BYTES_MAX, DOMAIN_BYTES_MAX),
        "the declared budgets bind on {}",
        dev.name()
    );
    let before = dev.buffers_created();
    let huge = vec![0u8; PARAMS_BYTES_MAX as usize + 1];
    assert_eq!(
        Params::new(&dev, &huge).map(drop),
        Err(ParamsRefusal::TooLarge(TooLarge {
            what: What::Params,
            bytes: PARAMS_BYTES_MAX + 1,
            limit: PARAMS_BYTES_MAX,
        }))
    );
    assert_eq!(dev.buffers_created(), before);

    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let (rows, per_row) = (ROWS_MAX as usize, 16usize);
    let offsets: Vec<u32> = (0..=rows).map(|r| (r * per_row) as u32).collect();
    let ids: Vec<u16> = (0..rows * per_row).map(|i| (i % 2_048) as u16).collect();
    let big = Batch::pack(&view, offsets, ids).expect("a batch near the job budget");
    let job_bytes = big.bytes().total();
    assert!(job_bytes > JOB_BYTES_MAX - (JOB_BYTES_MAX >> 3));
    let mut flying = Vec::new();
    while dev.live_bytes() + job_bytes <= DOMAIN_BYTES_MAX {
        flying.push(
            loaded(&dev, &params, &big)
                .submit(&pipeline)
                .expect("submitted"),
        );
    }
    assert_eq!(flying.len(), 4, "jobs in flight under the domain budget");
    let (live, created) = (dev.live_bytes(), dev.buffers_created());
    let mut job = Job::new(&dev, &params);
    assert_eq!(
        job.load(&big),
        Err(LoadRefusal::TooLarge(TooLarge {
            what: What::Domain,
            bytes: live + job_bytes,
            limit: DOMAIN_BYTES_MAX,
        }))
    );
    assert_eq!(
        (dev.buffers_created(), dev.live_bytes()),
        (created, live),
        "the refused load made no buffer"
    );
    for flight in flying {
        assert_eq!(flight.wait().expect("completed").rows(), ROWS_MAX);
    }
    job.load(&big).expect("fits once the others are done");
    drop(job);
    assert_eq!(dev.live_bytes(), params.bytes().len() as u64);
}

/// L5: an injected device limit below a budget is the limit a refusal names.
#[test]
fn an_injected_device_limit_is_the_one_refused() {
    let dev = device();
    dev.inject_device_limits(Some(65_536), None);
    assert_eq!(dev.limits().params, 65_536);
    let created = dev.buffers_created();
    assert_eq!(
        Params::new(&dev, v1()).map(drop),
        Err(ParamsRefusal::TooLarge(TooLarge {
            what: What::Params,
            bytes: v1().len() as u64,
            limit: 65_536,
        }))
    );
    assert_eq!(dev.buffers_created(), created);

    let dev = device();
    let params = Params::new(&dev, v1()).expect("params");
    let b1 = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("B1");
    let need = b1.bytes().total();
    dev.inject_device_limits(Some(need - 1), None);
    let created = dev.buffers_created();
    let mut job = Job::new(&dev, &params);
    assert_eq!(
        job.load(&b1),
        Err(LoadRefusal::TooLarge(TooLarge {
            what: What::Job,
            bytes: need,
            limit: need - 1,
        }))
    );
    assert_eq!(dev.buffers_created(), created);

    let dev = device();
    let params = Params::new(&dev, v1()).expect("params");
    let live = dev.live_bytes();
    dev.inject_device_limits(None, Some(live + need - 1));
    assert_eq!(dev.limits().working_set, live + need - 1);
    let created = dev.buffers_created();
    let mut job = Job::new(&dev, &params);
    assert_eq!(
        job.load(&b1),
        Err(LoadRefusal::TooLarge(TooLarge {
            what: What::Domain,
            bytes: live + need,
            limit: live + need - 1,
        }))
    );
    assert_eq!(dev.buffers_created(), created);
}

/// The device names itself, and Metal counts the bytes its buffers hold.
#[test]
fn the_device_reports_its_allocation() {
    let dev = device();
    assert!(!dev.name().is_empty());
    assert_ne!(dev.registry_id(), 0);
    let (params, _pipeline) = tone(&dev);
    assert_eq!(dev.live_bytes(), params.bytes().len() as u64);
    assert!(dev.current_allocated_size() >= dev.live_bytes());
    let fresh = current_allocated_size_fresh().expect("a fresh listing");
    assert!(fresh >= dev.live_bytes());
    assert_eq!(params.bytes(), v1(), "the sealed buffer holds the blob");
    assert_eq!(dev.leaks(), Leaks::default());
}

/// O2: an in-flight job dropped without a wait waits for its terminal status, then gives back every buffer and its params clone.
#[test]
fn a_dropped_job_in_flight_waits_then_releases_everything() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let base = dev.live_bytes();
    let batch = tone_int::pack(&params.view(), &tone_int::texts()).expect("B371");
    let drained = dev.drained_drops();
    #[cfg(debug_assertions)]
    let fed = dev.domain.latch().fed_count();

    let job = loaded(&dev, &params, &batch);
    assert_eq!(Arc::strong_count(&params), 2);
    assert_eq!(dev.live_bytes(), base + batch.bytes().total());
    drop(job);
    assert_eq!(
        (dev.live_bytes(), dev.drained_drops()),
        (base, drained),
        "a prepared job's drop does not wait"
    );

    let flight = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted");
    assert_eq!(Arc::strong_count(&params), 2);
    drop(flight);
    assert_eq!(dev.drained_drops(), drained + 1, "the drop waited");
    #[cfg(debug_assertions)]
    assert_eq!(dev.domain.latch().fed_count(), fed + 1, "and fed the latch");
    assert_eq!(Arc::strong_count(&params), 1);
    assert_eq!(dev.live_bytes(), base);
    assert_eq!(dev.leaks(), Leaks::default());

    let done = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    drop(done);
    assert_eq!(
        dev.drained_drops(),
        drained + 1,
        "a completed job's drop does not wait"
    );
    assert_eq!((Arc::strong_count(&params), dev.live_bytes()), (1, base));
    assert_eq!(dev.loss_reason(), None);
}

/// O3: two jobs in flight share one sealed params buffer and each gets its own batch's result.
#[test]
fn two_jobs_in_flight_share_one_params() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let texts = tone_int::texts();
    let (a, b) = texts.split_at(200);
    let ba = tone_int::pack(&view, a).expect("the first 200");
    let bb = tone_int::pack(&view, b).expect("the other 171");
    let created = dev.buffers_created();
    let fa = loaded(&dev, &params, &ba).submit(&pipeline).expect("a");
    let fb = loaded(&dev, &params, &bb).submit(&pipeline).expect("b");
    assert_eq!(dev.buffers_created(), created + 8, "no params copy");
    assert_eq!(Arc::strong_count(&params), 3);
    assert!(fa.serial() < fb.serial());
    let db = fb.wait().expect("b completed");
    let da = fa.wait().expect("a completed");
    assert_words_equal(da.out(), &reference(&view, &ba), "a");
    assert_words_equal(db.out(), &reference(&view, &bb), "b");
    assert_eq!(da.header(), ba.header());
    assert_eq!((da.offsets(), da.ids()), (ba.offsets(), ba.ids()));
    drop((da, db));
    assert_eq!(Arc::strong_count(&params), 1);
}

/// O4: a recycled job loads a smaller batch into the buffers it has and computes it exactly.
#[test]
fn a_recycled_job_reuses_its_buffers() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let big = tone_int::pack(&view, &cyclic(16_384)).expect("B16384");
    let small = tone_int::pack(&view, &tone_int::texts()).expect("B371");
    let done = loaded(&dev, &params, &big)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    assert_words_equal(done.out(), &reference(&view, &big), "B16384");
    let (created, live) = (dev.buffers_created(), dev.live_bytes());
    let mut job = done.recycle();
    job.load(&small).expect("reloaded");
    assert_eq!(
        (dev.buffers_created(), dev.live_bytes()),
        (created, live),
        "no new buffer"
    );
    let done = job
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    assert_eq!(done.rows(), 371);
    assert_words_equal(done.out(), &reference(&view, &small), "B371 after B16384");
    assert_eq!(
        int_bag_mlp::out_digest(done.out()).hex(),
        tone_int::B371_OUT_DIGEST
    );
}

/// O4: a recycled job grows into a larger batch by releasing each buffer it replaces before making the next, and the domain's pre-check counts exactly the bytes that frees and makes: it admits a load that leaves the live bytes at the limit and refuses one byte less by name, before allocation.
#[test]
fn a_recycled_job_grows_within_the_domain_exactly() {
    let recycled = || {
        let dev = device();
        let (params, pipeline) = tone(&dev);
        let view = params.view();
        let b1 = tone_int::pack(&view, &[tone_int::B1_TEXT]).expect("B1");
        let big = tone_int::pack(&view, &cyclic(16_384)).expect("B16384");
        let job = loaded(&dev, &params, &b1)
            .submit(&pipeline)
            .expect("submitted")
            .wait()
            .expect("completed")
            .recycle();
        (dev, params, pipeline, b1, big, job)
    };

    let (dev, params, pipeline, b1, big, mut job) = recycled();
    assert_eq!(b1.bytes().job, big.bytes().job, "the header slot is kept");
    let created = dev.buffers_created();
    job.load(&big).expect("grown");
    assert_eq!(dev.buffers_created(), created + 3);
    assert_eq!(
        dev.live_bytes(),
        params.bytes().len() as u64 + big.bytes().total()
    );
    let done = job
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    assert_words_equal(
        done.out(),
        &reference(&params.view(), &big),
        "B16384 after B1",
    );

    let (dev, _params, _pipeline, b1, big, mut job) = recycled();
    let after = dev.live_bytes() - b1.bytes().total() + big.bytes().total();
    dev.inject_device_limits(None, Some(after));
    job.load(&big)
        .expect("the domain holds exactly what the load leaves");
    assert_eq!(dev.live_bytes(), after);

    let (dev, _params, _pipeline, b1, big, mut job) = recycled();
    let live = dev.live_bytes();
    let after = live - b1.bytes().total() + big.bytes().total();
    dev.inject_device_limits(None, Some(after - 1));
    let created = dev.buffers_created();
    assert_eq!(
        job.load(&big),
        Err(LoadRefusal::TooLarge(TooLarge {
            what: What::Domain,
            bytes: after,
            limit: after - 1,
        }))
    );
    assert_eq!(
        (dev.buffers_created(), dev.live_bytes()),
        (created, live),
        "no slot replaced"
    );
}

/// O4: a recycled job never holds more than the job limit: a buffer larger than the new batch needs is remade at its need once keeping it would put the job's four over the limit, so it accepts what a fresh job accepts.
#[test]
fn a_recycled_job_never_holds_more_than_the_job_limit() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let (rows, per_row) = (8_000u32, MAX_ROW_IDS);
    let long = Batch::pack(
        &view,
        (0..=rows).map(|r| r * per_row).collect(),
        (0..rows * per_row).map(|i| (i % 2_048) as u16).collect(),
    )
    .expect("8,000 rows of 4,096 ids");
    let empty =
        Batch::pack(&view, vec![0; ROWS_MAX as usize + 1], Vec::new()).expect("2^20 empty rows");
    let limit = dev.limits().job;
    let (l, e) = (long.bytes(), empty.bytes());
    assert!(l.total() <= limit && e.total() <= limit);
    assert!(
        e.job + e.offsets + l.ids + e.out > limit,
        "keeping the long batch's ids would hold over the job limit"
    );
    let base = dev.live_bytes();
    let done = loaded(&dev, &params, &long)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    assert_eq!(done.rows(), rows);
    let created = dev.buffers_created();
    let mut job = done.recycle();
    job.load(&empty).expect("reloaded");
    assert_eq!(
        dev.live_bytes(),
        base + e.total(),
        "every buffer at the batch's need"
    );
    assert_eq!(
        dev.buffers_created(),
        created + 3,
        "offsets and out grown, ids shrunk, the header kept"
    );
    let done = job
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    assert_words_equal(done.out(), &reference(&view, &empty), "2^20 empty rows");
}

/// A recycled job that shrinks one buffer and grows others fits a domain limit set exactly at its new total.
#[test]
fn a_recycled_job_releases_what_it_remakes_before_it_allocates() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let (rows, per_row) = (8_000u32, MAX_ROW_IDS);
    let long = Batch::pack(
        &view,
        (0..=rows).map(|r| r * per_row).collect(),
        (0..rows * per_row).map(|i| (i % 2_048) as u16).collect(),
    )
    .expect("8,000 rows of 4,096 ids");
    let empty =
        Batch::pack(&view, vec![0; ROWS_MAX as usize + 1], Vec::new()).expect("2^20 empty rows");
    let base = dev.live_bytes();
    let mut job = loaded(&dev, &params, &long)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed")
        .recycle();
    let after = base + empty.bytes().total();
    dev.inject_device_limits(None, Some(after));
    job.load(&empty)
        .expect("the load fits once the remade buffers are released");
    assert_eq!(dev.live_bytes(), after, "every buffer at the batch's need");
}

/// A completed job's stamps are on one tick base and in order.
#[test]
fn a_completed_job_carries_ordered_stamps() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let batch = tone_int::pack(&params.view(), &tone_int::texts()).expect("B371");
    let serial = dev.next_serial();
    let before = ticks();
    let done = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .expect("completed");
    let t = done.times().expect("stamps");
    assert_eq!(done.serial(), serial);
    assert_eq!(t.timebase, Timebase::host());
    assert!(before <= t.before_encode && t.before_encode <= t.after_commit);
    assert!(t.after_commit <= t.woke && t.handler <= t.woke);
    if let Some(s) = t.scheduled {
        assert!(t.before_encode <= s && s <= t.handler);
    }
    assert!(t.gpu_start > 0.0 && t.gpu_end >= t.gpu_start);
    assert!(ticks() >= t.woke);
}

/// S1: one batch submitted 100 times on one recycled job gives the same words every time.
#[test]
fn a_repeated_job_gives_identical_words() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let batch = tone_int::pack(&params.view(), &tone_int::texts()).expect("B371");
    let want = reference(&params.view(), &batch);
    let mut job = Job::new(&dev, &params);
    for i in 0..100 {
        job.load(&batch).expect("loaded");
        let done = job
            .submit(&pipeline)
            .expect("submitted")
            .wait()
            .expect("completed");
        assert_words_equal(done.out(), &want, &format!("submission {i}"));
        job = done.recycle();
    }
    assert_eq!(dev.loss_reason(), None);
}

/// S2: an injected retryable failure releases the job's buffers and latches nothing, and a retry computes exactly.
#[cfg(debug_assertions)]
#[test]
fn an_injected_retryable_failure_latches_nothing() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("B1");
    let (base, fed) = (dev.live_bytes(), dev.domain.latch().fed_count());
    dev.arm_fault(Fault::Retryable, dev.next_serial());
    let failed = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .map(drop);
    assert_eq!(
        failed,
        Err(JobFailure::Retryable {
            code: 2,
            name: "injected compute timeout"
        })
    );
    assert_eq!(dev.domain.latch().fed_count(), fed + 1);
    assert_eq!((dev.loss_reason(), dev.live_bytes()), (None, base));
    assert_eq!(Arc::strong_count(&params), 1);
    let got = run(&dev, &params, &pipeline, &batch);
    assert_words_equal(&got, &reference(&params.view(), &batch), "the retry");
}

/// S2: an injected loss releases the job's buffers and latches the domain, so every later step refuses.
#[cfg(debug_assertions)]
#[test]
fn an_injected_loss_latches_the_domain() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("B1");
    let waiting = loaded(&dev, &params, &batch);
    let base = dev.live_bytes();
    dev.arm_fault(Fault::Lost, dev.next_serial());
    let failed = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted")
        .wait()
        .map(drop);
    let Err(JobFailure::Lost(reason)) = failed else {
        panic!("{failed:?}");
    };
    assert!(reason.contains("injected compute loss"), "{reason}");
    assert_eq!(dev.loss_reason().as_deref(), Some(reason.as_str()));
    assert_eq!((dev.live_bytes(), dev.leaks()), (base, Leaks::default()));
    match waiting.submit(&pipeline) {
        Err((job, SubmitRefusal::DomainLost(r))) => {
            assert_eq!(r, reason);
            drop(job);
        }
        other => panic!("{other:?}"),
    }
    let mut job = Job::new(&dev, &params);
    assert!(matches!(job.load(&batch), Err(LoadRefusal::DomainLost(_))));
    assert!(matches!(
        Params::new(&dev, v1()),
        Err(ParamsRefusal::DomainLost(_))
    ));
}

/// The hang detector a test shortens a domain's to, so an injected hang's timed wait expires quickly.
#[cfg(debug_assertions)]
const SHORT_DETECTOR: Duration = Duration::from_millis(200);

/// S2: an injected hang withholds the completion, so the wait's own timed detector expires; the job's buffers and params clone are leaked and counted, and the domain latched, whether a wait or a drop meets it.
#[cfg(debug_assertions)]
#[test]
fn an_injected_hang_leaks_counted_and_latches() {
    for by_drop in [false, true] {
        let dev = device();
        let (params, pipeline) = tone(&dev);
        let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("B1");
        let (base, drained) = (dev.live_bytes(), dev.drained_drops());
        let bytes = batch.bytes().total();
        dev.shorten_hang_detector(SHORT_DETECTOR);
        dev.arm_fault(Fault::Hang, dev.next_serial());
        let flight = loaded(&dev, &params, &batch)
            .submit(&pipeline)
            .expect("submitted");
        let started = Instant::now();
        if by_drop {
            drop(flight);
        } else {
            assert_eq!(flight.wait().map(drop), Err(JobFailure::Hung));
        }
        let took = started.elapsed();
        assert!(
            took >= SHORT_DETECTOR && took < HANG_DETECTOR,
            "{took:?}: the domain's detector, not the job's completion, ended the wait"
        );
        assert_eq!(dev.drained_drops(), drained, "a hung drop is not drained");
        assert_eq!(
            dev.leaks(),
            Leaks {
                jobs: 1,
                bytes,
                params_clones: 1
            }
        );
        assert_eq!(dev.live_bytes(), base + bytes, "the leak stays counted");
        assert_eq!(Arc::strong_count(&params), 2, "the leaked clone");
        let reason = dev.loss_reason().expect("latched");
        assert!(reason.contains("compute job hung"), "{reason}");
        let mut job = Job::new(&dev, &params);
        assert!(matches!(job.load(&batch), Err(LoadRefusal::DomainLost(_))));
    }
}

/// S2: two jobs in flight that fail differently each report their own cause, while the domain keeps the first.
#[cfg(debug_assertions)]
#[test]
fn each_failed_job_names_its_own_cause() {
    let dev = device();
    let (params, pipeline) = tone(&dev);
    let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("B1");
    dev.shorten_hang_detector(SHORT_DETECTOR);
    let first = dev.next_serial();
    dev.arm_fault(Fault::Hang, first);
    dev.arm_fault(Fault::Lost, first + 1);
    let hung = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted");
    let lost = loaded(&dev, &params, &batch)
        .submit(&pipeline)
        .expect("submitted");
    assert_eq!(hung.wait().map(drop), Err(JobFailure::Hung));
    dev.shorten_hang_detector(HANG_DETECTOR);
    let failed = lost.wait().map(drop);
    let Err(JobFailure::Lost(reason)) = failed else {
        panic!("{failed:?}");
    };
    assert!(reason.contains("injected compute loss"), "{reason}");
    let latched = dev.loss_reason().expect("latched");
    assert!(latched.contains("compute job hung"), "{latched}");
}

/// A status not yet terminal at the completed handler is a loss, which latches; every other status reads as `loss::classify` reads it.
#[test]
fn an_unfinished_status_at_the_handler_is_a_latched_loss() {
    for status in [
        loss::STATUS_NOT_ENQUEUED,
        loss::STATUS_ENQUEUED,
        loss::STATUS_COMMITTED,
        loss::STATUS_SCHEDULED,
    ] {
        let outcome = terminal_outcome(status, None);
        assert_eq!(
            outcome,
            CbOutcome::Lost {
                code: None,
                name: "compute job unfinished at its completed handler",
            }
        );
        let latch = LossLatch::new();
        latch.record(&outcome);
        assert!(latch.is_lost(), "status {status}");
    }
    for (status, code) in [
        (loss::STATUS_COMPLETED, None),
        (loss::STATUS_ERROR, Some(loss::ERROR_TIMEOUT)),
        (loss::STATUS_ERROR, Some(loss::ERROR_PAGE_FAULT)),
        (loss::STATUS_ERROR, None),
    ] {
        assert_eq!(terminal_outcome(status, code), loss::classify(status, code));
    }
}

/// A view refuses a null or misaligned contents pointer rather than reading through it.
#[test]
fn a_null_or_misaligned_contents_pointer_maps_nothing() {
    let mut words = [0u32; 2];
    let base = words.as_mut_ptr().cast::<u8>();
    assert_eq!(typed::<u32>(base), Some(base.cast::<u32>()));
    assert_eq!(
        typed::<u16>(base.wrapping_add(2)),
        Some(base.wrapping_add(2).cast::<u16>())
    );
    assert_eq!(
        typed::<u8>(base.wrapping_add(1)),
        Some(base.wrapping_add(1))
    );
    for off in 1..4 {
        assert_eq!(typed::<u32>(base.wrapping_add(off)), None, "{off}");
    }
    assert_eq!(typed::<u16>(base.wrapping_add(1)), None);
    assert_eq!(typed::<u32>(std::ptr::null_mut()), None);
}

/// What the parity runs exercised, so a pass cannot be vacuous.
#[derive(Debug, Default)]
struct Tally {
    rows: u64,
    classes: BTreeSet<u32>,
    abstained: u64,
    empty: u64,
    full: u64,
    tied: u64,
    clamped: u64,
    saturated: u64,
    interior: u64,
}

/// Whether `p`'s activation table is the sign function, under which no unit is interior in effect.
fn sign_lut(p: &ParamsView<'_>) -> bool {
    p.lut()
        .iter()
        .zip(-p.t()..)
        .all(|(&v, i)| i32::from(v as i8) == i.signum() * 127)
}

/// Run `b` and compare every word with the reference, counting what its rows exercised.
fn parity(
    dev: &ComputeDevice,
    params: &Arc<Params>,
    pipeline: &Pipeline,
    b: &Batch,
    tally: &mut Tally,
    what: &str,
) {
    let view = params.view();
    let got = run(dev, params, pipeline, b);
    assert_words_equal(&got, &reference(&view, b), what);
    let sign = sign_lut(&view);
    let c = view.dims().classes as usize;
    for (r, row) in (0..b.rows()).zip(got.chunks(c + 1)) {
        let ids = b.row(r).expect("a row");
        tally.rows += 1;
        tally.empty += u64::from(ids.is_empty());
        tally.full += u64::from(ids.len() == MAX_ROW_IDS as usize);
        match int_bag_mlp::trace(&view, b, r).expect("trace") {
            None => tally.abstained += 1,
            Some(t) => {
                tally.clamped += u64::from(t.clamped);
                tally.saturated += u64::from(t.saturated);
                tally.interior += if sign { 0 } else { u64::from(t.interior) };
                tally.classes.insert(row[c]);
                let logits = row[..c].iter().map(|&w| w as i32);
                let best = logits.clone().max().expect("a class");
                tally.tied += u64::from(logits.filter(|&l| l == best).count() > 1);
            }
        }
    }
}

/// S3: the GPU equals the checked CPU reference word for word on tone-int, 64 seeded blobs and the extreme blob, which reach both clamps, the row cap, ties and, outside tone-int, at least 10,000 interior units.
#[test]
fn every_word_equals_the_cpu_reference() {
    let dev = device();
    let mut tally = Tally::default();
    let (params, pipeline) = tone(&dev);
    let view = params.view();
    let b371 = tone_int::pack(&view, &tone_int::texts()).expect("B371");
    parity(&dev, &params, &pipeline, &b371, &mut tally, "B371");
    let got = run(&dev, &params, &pipeline, &b371);
    assert_eq!(
        int_bag_mlp::out_digest(&got).hex(),
        tone_int::B371_OUT_DIGEST
    );
    let b16384 = tone_int::pack(&view, &cyclic(16_384)).expect("B16384");
    parity(&dev, &params, &pipeline, &b16384, &mut tally, "B16384");
    let edges = tone_int::pack(&view, &["", "ok", tone_int::B1_TEXT, "a"]).expect("edge texts");
    parity(&dev, &params, &pipeline, &edges, &mut tally, "edge texts");
    let (tone_classes, tone_interior) = (tally.classes.len(), tally.interior);

    let lib = Library::compile(&dev).expect("the library");
    for seed in 0..64 {
        let (blob, offsets, ids) = int_bag_mlp::seeded(seed);
        let params = Params::new(&dev, &blob).expect("seeded params");
        let pipeline = Pipeline::new(&lib, Shape::of(&params)).expect("a seeded pipeline");
        pipeline.qualify().expect("qualified at the seeded shape");
        let batch = Batch::pack(&params.view(), offsets, ids).expect("seeded rows");
        let what = format!("seed {seed}");
        parity(&dev, &params, &pipeline, &batch, &mut tally, &what);
    }

    let extreme = int_bag_mlp::extreme_params();
    let params = Params::new(&dev, &extreme).expect("extreme params");
    let pipeline = Pipeline::build(&dev, &params).expect("the extreme pipeline");
    let (offsets, ids) = int_bag_mlp::extreme_rows();
    let batch = Batch::pack(&params.view(), offsets, ids).expect("extreme rows");
    parity(&dev, &params, &pipeline, &batch, &mut tally, "extreme");

    assert!(tone_classes >= 4, "tone-int's classes: {tally:?}");
    assert!(tally.abstained >= 1 && tally.empty >= 1, "{tally:?}");
    assert!(tally.full >= 1 && tally.tied >= 1, "{tally:?}");
    assert!(tally.clamped >= 1 && tally.saturated >= 1, "{tally:?}");
    assert!(
        tally.interior - tone_interior >= 10_000,
        "interior units outside tone-int: {tally:?}"
    );
    assert!(tally.rows > 16_384, "{tally:?}");
    assert_eq!(dev.loss_reason(), None);
}

/// S4: a pipeline whose qualification finds a mismatch stays refused, and no unqualified, foreign or misshapen pipeline runs a job.
#[test]
fn an_unqualified_or_mismatched_pipeline_refuses_the_job() {
    let dev = device();
    let (params, good) = tone(&dev);
    let view = params.view();
    let batch = tone_int::pack(&view, &[tone_int::B1_TEXT]).expect("B1");
    let lib = Library::compile(&dev).expect("the library");

    let bad = Pipeline::new(&lib, TONE_SHAPE).expect("a pipeline");
    let Err(PipelineError::Unqualified(m)) = bad.qualify_against_corrupted(5) else {
        panic!("a flipped reference word was not found");
    };
    assert_eq!((m.batch, m.word, m.gpu ^ m.cpu), (0, 5, 1));
    assert_eq!(
        bad.qualify(),
        Err(PipelineError::Unqualified(m)),
        "refused for good"
    );
    assert!(!bad.is_qualified());
    let job = match loaded(&dev, &params, &batch).submit(&bad) {
        Err((job, SubmitRefusal::Unqualified)) => job,
        other => panic!("{other:?}"),
    };
    let done = job
        .submit(&good)
        .expect("given back intact")
        .wait()
        .expect("completed");
    assert_words_equal(done.out(), &reference(&view, &batch), "after a refusal");

    let pending = Pipeline::new(&lib, TONE_SHAPE).expect("a pipeline");
    assert!(matches!(
        loaded(&dev, &params, &batch).submit(&pending),
        Err((_, SubmitRefusal::Unqualified))
    ));
    assert!(matches!(
        Job::new(&dev, &params).submit(&good),
        Err((_, SubmitRefusal::NotLoaded))
    ));

    let (blob, offsets, ids) = int_bag_mlp::seeded(3);
    let seeded = Params::new(&dev, &blob).expect("seeded params");
    let rows = Batch::pack(&seeded.view(), offsets, ids).expect("seeded rows");
    match loaded(&dev, &seeded, &rows).submit(&good) {
        Err((_, SubmitRefusal::DimsMismatch { pipeline, params })) => {
            assert_eq!((pipeline, params), (TONE_SHAPE, Shape::of(&seeded)));
        }
        other => panic!("{other:?}"),
    }
    let other_min = Params::new(&dev, &patched(v1(), params::MIN_IDS, tone_int::MIN_IDS + 1))
        .expect("tone-int at another MIN_IDS");
    assert_eq!(other_min.dims(), params.dims());
    assert!(matches!(
        loaded(&dev, &other_min, &batch).submit(&good),
        Err((_, SubmitRefusal::DimsMismatch { .. }))
    ));

    let elsewhere = device();
    let foreign = Params::new(&elsewhere, v1()).expect("params on another domain");
    assert!(matches!(
        Job::new(&dev, &foreign).load(&batch),
        Err(LoadRefusal::OtherDomain)
    ));
    assert!(matches!(
        loaded(&elsewhere, &foreign, &batch).submit(&good),
        Err((_, SubmitRefusal::OtherDomain))
    ));
}

/// A fault in the kernel's steps 4–6 that the extreme blob alone does not see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutant {
    /// The kernel as written.
    Faithful,
    /// Step 5 shifts the signed product, a floor where the profile truncates toward zero.
    FloorShift,
    /// Step 4 divides by n − 1.
    DivideByNMinus1,
    /// Step 6 reads the next entry, kept in bounds: `lut[min(clamp(p) + T + 1, 2T)]`.
    LutShift,
    /// Step 5 shifts by G − 1.
    ShiftByGMinus1,
}

/// The kernel's arithmetic on the CPU, one row as one GPU thread computes it, with `m` applied.
fn kernel_model(p: &ParamsView<'_>, b: &Batch, m: Mutant) -> Vec<u32> {
    let d = p.dims();
    let (e, h, c) = (d.embed as usize, d.hidden as usize, d.classes as usize);
    let i8s = |v: &[u8], i: usize| i64::from(v[i] as i8);
    let (t, g) = (
        i64::from(p.t()),
        p.g() - u32::from(m == Mutant::ShiftByGMinus1),
    );
    let mut out = Vec::with_capacity(b.out_words());
    for r in 0..b.rows() {
        let ids = b.row(r).expect("a row");
        let n = ids.len() as u64;
        if n < u64::from(p.min_ids()) {
            out.extend(std::iter::repeat_n(0, c).chain([d.classes]));
            continue;
        }
        let s: Vec<i64> = (0..e)
            .map(|j| {
                ids.iter()
                    .map(|&x| i8s(p.emb(), usize::from(x) * e + j))
                    .sum()
            })
            .collect();
        let hid: Vec<i64> = (0..h)
            .map(|k| {
                let a: i64 = (0..e).map(|j| s[j] * i8s(p.w1(), j * h + k)).sum();
                let n = if m == Mutant::DivideByNMinus1 {
                    (n - 1).max(1)
                } else {
                    n
                };
                let q = (a.unsigned_abs() / n).min(u64::from(p.qmax())) as i64;
                let mult = i64::from(p.mult());
                let pre = if m == Mutant::FloorShift {
                    (if a < 0 { -q } else { q } * mult) >> g
                } else {
                    let mag = (q * mult) >> g;
                    if a < 0 { -mag } else { mag }
                } + i64::from(p.b1()[k]);
                let mut at = pre.clamp(-t, t) + t;
                if m == Mutant::LutShift {
                    at = (at + 1).min(2 * t);
                }
                i8s(p.lut(), at as usize)
            })
            .collect();
        let logits: Vec<i64> = (0..c)
            .map(|class| {
                i64::from(p.b2()[class])
                    + (0..h)
                        .map(|k| hid[k] * i8s(p.w2(), k * c + class))
                        .sum::<i64>()
            })
            .collect();
        let best = (1..c).fold(0, |best, i| if logits[i] > logits[best] { i } else { best });
        out.extend(logits.iter().map(|&l| l as i32 as u32).chain([best as u32]));
    }
    out
}

/// Qualification's set (design §12.5) catches a floor shift in step 5, an n − 1 divisor in step 4, the in-bounds table shift in step 6 and a shift by G − 1 at tone-int's shape, the profile's smallest and largest, and the 64 seeded blobs' shapes; the CPU model of the faithful kernel equals the reference on all of it.
#[test]
fn the_qualification_set_catches_faults_in_steps_4_to_6() {
    let mut shapes = vec![
        TONE_SHAPE,
        Shape {
            embed: 1,
            hidden: 1,
            classes: 1,
            min_ids: 1,
        },
        Shape {
            embed: E_MAX,
            hidden: H_MAX,
            classes: C_MAX,
            min_ids: 2,
        },
    ];
    for seed in 0..64 {
        let (blob, _, _) = int_bag_mlp::seeded(seed);
        let p = int_bag_mlp::check(&blob).expect("a seeded blob");
        let d = p.dims();
        shapes.push(Shape {
            embed: d.embed,
            hidden: d.hidden,
            classes: d.classes,
            min_ids: p.min_ids(),
        });
    }
    let mutants = [
        Mutant::FloorShift,
        Mutant::DivideByNMinus1,
        Mutant::LutShift,
        Mutant::ShiftByGMinus1,
    ];
    for shape in shapes {
        let mut caught = [false; 4];
        for (blob, batches) in qualification_set(shape).expect("the qualification set") {
            let p = int_bag_mlp::check(&blob).expect("a qualification blob");
            for b in &batches {
                let want = reference(&p, b);
                assert_eq!(kernel_model(&p, b, Mutant::Faithful), want, "{shape:?}");
                for (seen, &m) in caught.iter_mut().zip(&mutants) {
                    *seen |= kernel_model(&p, b, m) != want;
                }
            }
        }
        for (seen, m) in caught.iter().zip(mutants) {
            assert!(seen, "{m:?} passes qualification at {shape:?}");
        }
    }
}

/// The kernel's step 5 as written: the magnitude shifted, then its sign.
const STEP5_EXACT: &str = "        const int mag = int((q * mult) >> g);
        const int p = (a < 0 ? -mag : mag) + b1[k];
";

/// Step 5 with the signed product shifted, a floor where the profile truncates.
const STEP5_FLOOR: &str = "        const int signed_q = a < 0 ? -int(q) : int(q);
        const int p = ((signed_q * int(mult)) >> g) + b1[k];
";

/// S4: a kernel whose step 5 floors where the profile truncates passes the extreme blob's batches and is refused Unqualified by the interior batch, for good.
#[test]
fn a_kernel_that_floors_in_step_5_is_refused() {
    assert_eq!(KERNEL.matches(STEP5_EXACT).count(), 1, "step 5 as written");
    let dev = device();
    let wrong = format!(
        "{}{}",
        prelude(),
        KERNEL.replacen(STEP5_EXACT, STEP5_FLOOR, 1)
    );
    let lib = Library::compile_source(&dev, &wrong).expect("the floor-shift kernel compiles");
    let pipeline = Pipeline::new(&lib, TONE_SHAPE).expect("its pipeline");
    let Err(PipelineError::Unqualified(m)) = pipeline.qualify() else {
        panic!("a floor in step 5 was not found");
    };
    assert_eq!(
        m.batch, 2,
        "the interior batch, after the extreme blob's two: {m:?}"
    );
    assert_eq!(pipeline.qualify(), Err(PipelineError::Unqualified(m)));
    assert!(!pipeline.is_qualified());
}
