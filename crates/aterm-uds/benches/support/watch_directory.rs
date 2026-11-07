// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared manual workload, also usable with exact-source comparison backends.

use std::hint::black_box;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

pub(super) trait Arms {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()>;
    fn disarm(&mut self, key: u64);
    fn wait(&mut self, touched: &mut Vec<u64>) -> io::Result<()>;
}

type Factory = fn() -> io::Result<Box<dyn Arms>>;

macro_rules! backend {
    ($name:ident, $make:ident, $($module:ident)::+) => {
        struct $name(Box<dyn $($module)::+::FileEvents>);
        impl $crate::work::Arms for $name {
            fn arm(&mut self, key: u64, path: &std::path::Path) -> std::io::Result<()> {
                self.0.arm_directory(key, path)
            }
            fn disarm(&mut self, key: u64) {
                self.0.disarm(key);
            }
            fn wait(&mut self, touched: &mut Vec<u64>) -> std::io::Result<()> {
                self.0.wait(Some(std::time::Duration::from_secs(60)), touched)
            }
        }
        fn $make() -> std::io::Result<Box<dyn $crate::work::Arms>> {
            $($module)::+::platform_events()
                .map(|events| Box::new($name(events)) as Box<dyn $crate::work::Arms>)
        }
    };
}
pub(super) use backend;

/// One fixture and alternating execution order for paired backends; identical factories form a
/// null comparison. Both backends must hear a real invalidation before any work is measured.
pub(super) fn paired(
    before: Factory,
    after: Factory,
    before_name: &str,
    after_name: &str,
) -> io::Result<()> {
    let fixture = Fixture::new()?;
    verify(before, &fixture)?;
    verify(after, &fixture)?;
    println!("watch_directory_probe: both_native_backends_heard_publication");
    for sample_index in 0..9 {
        for (index, case) in CASES.into_iter().enumerate() {
            let order = if (sample_index + index) % 2 == 0 {
                [(before, before_name), (after, after_name)]
            } else {
                [(after, after_name), (before, before_name)]
            };
            for (position, (make, name)) in order.into_iter().enumerate() {
                sample(make, &fixture, case, 512)?.emit(name, sample_index, position);
            }
        }
    }
    Ok(())
}

pub(super) struct Fixture {
    root: Rc<aterm_tempfile::TempDir>,
    shared: PathBuf,
    separate: Vec<PathBuf>,
}

impl Fixture {
    pub(super) fn new() -> io::Result<Self> {
        let root = Rc::new(
            aterm_tempfile::Builder::new()
                .prefix("aterm-watch-directory-")
                .tempdir()?,
        );
        let base = root.path().to_path_buf();
        Self::at(root, &base)
    }

    fn at(root: Rc<aterm_tempfile::TempDir>, base: &Path) -> io::Result<Self> {
        let shared = base.join("shared");
        std::fs::create_dir_all(&shared)?;
        let separate = (0..64)
            .map(|key| {
                let path = base.join(format!("separate-{key}"));
                std::fs::create_dir_all(&path)?;
                Ok(path)
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            root,
            shared,
            separate,
        })
    }

    fn describe(&self, name: &str) -> io::Result<()> {
        identifier(name);
        let lexical = self
            .shared
            .strip_prefix(self.root.path())
            .map_err(io::Error::other)?;
        let resolved = std::fs::canonicalize(&self.shared)?;
        let root = std::fs::canonicalize(self.root.path())?;
        let suffix = resolved.strip_prefix(&root).map_err(io::Error::other)?;
        println!(
            "watch_directory_layout: {{\"name\":\"{name}\",\"lexical_suffix_depth\":{},\"resolved_suffix_depth\":{},\"root_lexical_depth\":{},\"root_resolved_depth\":{}}}",
            lexical.components().count(),
            suffix.components().count(),
            self.root.path().components().count(),
            root.components().count()
        );
        Ok(())
    }
}

/// Compare lookup layouts with one backend and a shared temporary root. The relative alias
/// names the shallow physical directories; it adds one link to an otherwise equal lookup.
pub(super) fn layout_matrix(make: Factory) -> io::Result<()> {
    let root = Rc::new(
        aterm_tempfile::Builder::new()
            .prefix("aterm-watch-layout-")
            .tempdir()?,
    );
    let shallow = Fixture::at(Rc::clone(&root), &root.path().join("shallow"))?;
    let mut deep_path = root.path().join("depth");
    for depth in 0..8 {
        deep_path.push(format!("d{depth}"));
    }
    let deep = Fixture::at(Rc::clone(&root), &deep_path)?;
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink("shallow", &alias)?;
    let linked = Fixture::at(Rc::clone(&root), &alias)?;
    for (left, right) in std::iter::once((&shallow.shared, &linked.shared))
        .chain(shallow.separate.iter().zip(&linked.separate))
    {
        if std::fs::canonicalize(left)? != std::fs::canonicalize(right)? {
            return Err(io::Error::other(
                "the alias does not name the shallow fixture",
            ));
        }
    }
    for (name, fixture) in [("shallow", &shallow), ("deep", &deep), ("linked", &linked)] {
        fixture.describe(name)?;
        verify(make, fixture)?;
    }
    println!("watch_directory_probe: all_layouts_heard_publication_alias_targets_equal");
    for sample_index in 0..9 {
        for (index, case) in CASES.into_iter().enumerate() {
            for (comparison, (other, before_name, after_name)) in [
                (&deep, "shallow_depth", "deep"),
                (&linked, "shallow_link", "linked"),
            ]
            .into_iter()
            .enumerate()
            {
                let order = if (sample_index + index + comparison) % 2 == 0 {
                    [(&shallow, before_name), (other, after_name)]
                } else {
                    [(other, after_name), (&shallow, before_name)]
                };
                for (position, (fixture, name)) in order.into_iter().enumerate() {
                    sample(make, fixture, case, 512)?.emit(name, sample_index, position);
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Case {
    Cold,
    One,
    Shared,
    Separate,
}

pub(super) const CASES: [Case; 4] = [Case::Cold, Case::One, Case::Shared, Case::Separate];

impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::One => "one",
            Self::Shared => "shared64",
            Self::Separate => "separate64",
        }
    }
    fn keys(self) -> u64 {
        match self {
            Self::Cold | Self::One => 1,
            Self::Shared | Self::Separate => 64,
        }
    }
    fn path(self, fixture: &Fixture, key: u64) -> &Path {
        if self == Self::Separate {
            &fixture.separate[key as usize]
        } else {
            &fixture.shared
        }
    }
}

/// Outside the measured window, require a real directory invalidation under the armed key.
pub(super) fn verify(make: Factory, fixture: &Fixture) -> io::Result<()> {
    let mut events = make()?;
    events.arm(7, &fixture.shared)?;
    let probe = fixture.shared.join("probe");
    std::fs::write(&probe, "native publication")?;
    let mut touched = Vec::new();
    events.wait(&mut touched)?;
    if !touched.contains(&7) {
        return Err(io::Error::other(
            "the native directory arm heard no publication",
        ));
    }
    events.disarm(7);
    std::fs::remove_file(probe)
}

struct Counter {
    instructions: u64,
    user: u64,
    system: u64,
}

impl Counter {
    fn read() -> io::Result<Self> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
        // SAFETY: this flavor writes the complete buffer, read only on success; this process
        // is live for the call and no pointer into its address space is passed to another task.
        let result = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: success initialized this flavor's entire buffer.
        let info = unsafe { info.assume_init() };
        if info.ri_instructions == 0 {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(Self {
            instructions: info.ri_instructions,
            user: info.ri_user_time,
            system: info.ri_system_time,
        })
    }
}

pub(super) struct Sample {
    case: Case,
    operations: u64,
    instructions: u64,
    user_ns: u128,
    system_ns: u128,
    wall_ns: u128,
}

impl Sample {
    pub(super) fn emit(&self, variant: &str, sample: usize, position: usize) {
        identifier(variant);
        println!(
            "watch_directory_sample: {{\"variant\":\"{variant}\",\"sample\":{sample},\"position\":{position},\"case\":\"{}\",\"operations\":{},\"instructions\":{},\"user_ns\":{},\"system_ns\":{},\"wall_ns\":{}}}",
            self.case.name(),
            self.operations,
            self.instructions,
            self.user_ns,
            self.system_ns,
            self.wall_ns
        );
    }
}

fn identifier(name: &str) {
    assert!(
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "a sample label is an ASCII identifier"
    );
}

pub(super) fn sample(
    make: Factory,
    fixture: &Fixture,
    case: Case,
    operations: u64,
) -> io::Result<Sample> {
    if operations == 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: one writable timebase record, live for the call.
    let status = unsafe { libc::mach_timebase_info(&mut timebase) };
    if status != 0 || timebase.denom == 0 {
        return Err(io::Error::other("the process CPU timebase is unavailable"));
    }
    let mut retained = if case == Case::Cold {
        None
    } else {
        let mut events = make()?;
        for key in 0..case.keys() {
            events.arm(key, case.path(fixture, key))?;
        }
        Some(events)
    };
    let mut run = |count| -> io::Result<()> {
        for turn in 0..count {
            let key = black_box(turn % case.keys());
            let path = black_box(case.path(fixture, key));
            if let Some(events) = &mut retained {
                events.arm(key, path)?;
            } else {
                let mut events = make()?;
                events.arm(key, path)?;
                events.disarm(key);
            }
        }
        Ok(())
    };
    run(64)?;
    let before = Counter::read()?;
    let clock = Instant::now();
    run(operations)?;
    let wall_ns = clock.elapsed().as_nanos();
    let after = Counter::read()?;
    let difference = |a: u64, b: u64| {
        a.checked_sub(b)
            .ok_or_else(|| io::Error::other("a process work counter decreased"))
    };
    let ns =
        |ticks: u64| u128::from(ticks) * u128::from(timebase.numer) / u128::from(timebase.denom);
    Ok(Sample {
        case,
        operations,
        instructions: difference(after.instructions, before.instructions)?,
        user_ns: ns(difference(after.user, before.user)?),
        system_ns: ns(difference(after.system, before.system)?),
        wall_ns,
    })
}
