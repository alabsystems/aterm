// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::floor::{GIB, Knobs};
use aterm_sysprobe::{LinkByteCounter, LinkCounters, LinkKind};

/// The wall clock of a test machine's first look, Unix ms.
const T0: u64 = 1_800_000_000_000;

/// A scripted machine: the test sets what each reader gives and when the clocks stand, and
/// reads back how often the machine was looked at.
struct Machine {
    base: Instant,
    at: Duration,
    /// How far the wall clock ran ahead of the monotonic one while the machine slept: the
    /// monotonic clock stops in sleep, the wall clock does not.
    slept: Duration,
    clock_read: bool,
    look: Look,
    ticks: Option<Vec<CoreTicks>>,
    links: Option<Vec<LinkReading>>,
    psi: Option<Psi>,
    swapped_kib: Option<u64>,
    lines: Option<Lines>,
    looks: usize,
    lines_asked: Vec<(String, u64)>,
}

impl Machine {
    /// A machine no reader reads anything of, its clocks at the start.
    fn unread() -> Machine {
        Machine {
            base: Instant::now(),
            at: Duration::ZERO,
            slept: Duration::ZERO,
            clock_read: true,
            look: Look::default(),
            ticks: None,
            links: None,
            psi: None,
            swapped_kib: None,
            lines: None,
            looks: 0,
            lines_asked: Vec::new(),
        }
    }

    /// One look by `keeper` with the clocks `secs` after the start, for no home.
    fn look_at(&mut self, keeper: &mut Keeper, secs: f64) -> MachineLook {
        self.at = Duration::from_secs_f64(secs);
        keeper.look(self, None)
    }
}

impl Probe for Machine {
    fn now(&mut self) -> Clock {
        Clock {
            at: self.base + self.at,
            unix_ms: self
                .clock_read
                .then(|| T0 + u64::try_from((self.at + self.slept).as_millis()).unwrap()),
        }
    }
    fn look(&mut self, _home: Option<&Path>) -> Look {
        self.looks += 1;
        self.look.clone()
    }
    fn core_ticks(&mut self) -> Option<Vec<CoreTicks>> {
        self.ticks.clone()
    }
    fn net_links(&mut self) -> Option<Vec<LinkReading>> {
        self.links.clone()
    }
    fn psi(&mut self) -> Option<Psi> {
        self.psi
    }
    fn swapped_kib(&mut self) -> Option<u64> {
        self.swapped_kib
    }
    fn disk_lines(&mut self, key: &str, total: u64) -> Option<Lines> {
        self.lines_asked.push((key.to_owned(), total));
        self.lines
    }
}

/// `cores` cores, each with `busy` busy and `idle` idle ticks since boot.
fn ticks(cores: usize, busy: u64, idle: u64) -> Vec<CoreTicks> {
    vec![
        CoreTicks {
            user: busy,
            idle,
            ..CoreTicks::default()
        };
        cores
    ]
}

/// An active physical link with a driver rate and its byte counters.
fn link(name: &str, index: u32, rate_bps: Option<u64>, rx: u64, tx: u64) -> LinkReading {
    LinkReading {
        name: name.to_owned(),
        index,
        kind: LinkKind::Wired,
        up: true,
        loopback: false,
        physical: Some(true),
        byte_counter: LinkByteCounter::Cumulative64,
        rate_bps,
        rx: LinkCounters {
            bytes: rx,
            ..LinkCounters::default()
        },
        tx: LinkCounters {
            bytes: tx,
            ..LinkCounters::default()
        },
    }
}

const VOLUME_TOTAL: u64 = 1000 * GIB;

/// One look at a 48 GiB, 14-core machine: every reading present, each figure distinct from
/// every other so one read from the wrong class shows; no swap in use and an idle GPU, two
/// measured zeros.
fn full_look() -> Look {
    Look {
        cores: Some(14),
        load: Some([2.5, 3.0, 4.004]),
        memory: Some(aterm_sysprobe::MemoryDetail {
            total: 48 * GIB,
            available: 7 * GIB,
            free: GIB,
            wired: 4 * GIB,
            compressed: Some(3 * GIB),
            app: 18 * GIB,
            shared: Some(GIB),
            swap_used: Some(0),
            swap_total: Some(2 * GIB),
            pressure: Some(MemoryLevel::Normal),
            ..aterm_sysprobe::MemoryDetail::default()
        }),
        gpu: Some(aterm_sysprobe::GpuReading {
            device_pct: Some(0),
            cores: Some(40),
            ..aterm_sysprobe::GpuReading::default()
        }),
        volume: Some(aterm_sysprobe::volumes::Reading {
            mount: "/System/Volumes/Data".into(),
            key: aterm_sysprobe::volumes::VolumeKey::Container("disk3".into()),
            total: VOLUME_TOTAL,
            free: 200 * GIB,
            files_free: None,
            dev: 1,
            read_only: false,
        }),
    }
}

/// The lines the watch holds a volume of [`VOLUME_TOTAL`] to under the default knobs.
fn default_lines() -> Lines {
    crate::floor::lines(
        Knobs::default(),
        Some(VOLUME_TOTAL),
        Some(crate::floor::room_bounds(VOLUME_TOTAL).0),
    )
}

/// A look with every input of every verdict read and each resource at its limit.
fn at_every_limit() -> MachineLook {
    MachineLook {
        cores: Some(8),
        load_x100: Some([1600, 900, 400]),
        cpu_busy_x100: Some(800),
        mem_total_mb: Some(10_000),
        mem_avail_mb: Some(500),
        mem_pressure: Some(MemoryLevel::Critical),
        disk_free_mb: Some(1024),
        gpu_busy_pct: Some(100),
        ..MachineLook::UNREAD
    }
}

/// LAW: a figure no reader read is `None` in the look, and so is every verdict; none is ever a
/// zero. CONTROL: the same keeper over a machine read whole gives every figure, each from the
/// reading that owns it and in its unit, with the measured zeros (no swap in use, an idle GPU,
/// a link that sent nothing) kept as zeros.
#[test]
fn an_unread_figure_is_none_and_never_zero() {
    let mut blind = Machine::unread();
    blind.clock_read = false;
    let mut keeper = Keeper::new();
    assert_eq!(blind.look_at(&mut keeper, 0.0), MachineLook::UNREAD);
    assert_eq!(
        blind.look_at(&mut keeper, 60.0),
        MachineLook::UNREAD,
        "a minute later there is still no counter to take a rate from"
    );
    assert_eq!(blind.looks, 2);
    assert!(blind.lines_asked.is_empty(), "no volume, no lines to ask");

    let mut whole = Machine::unread();
    whole.look = full_look();
    whole.ticks = Some(ticks(14, 1000, 9000));
    whole.links = Some(vec![link(
        "en0",
        4,
        Some(1_000_000_000),
        1_000_000,
        500_000,
    )]);
    whole.psi = Some(Psi {
        some_avg10: 0.25,
        some_avg60: 11.5,
        full_avg10: 0.0,
    });
    whole.lines = Some(default_lines());
    let mut keeper = Keeper::new();
    let first = whole.look_at(&mut keeper, 0.0);
    whole.ticks = Some(ticks(14, 1000 + 1500, 9000 + 4500));
    whole.links = Some(vec![link(
        "en0",
        4,
        Some(1_000_000_000),
        8_500_000,
        500_000,
    )]);
    let second = whole.look_at(&mut keeper, 60.0);
    let expected = MachineLook {
        at_unix_ms: Some(T0 + 60_000),
        cores: Some(14),
        load_x100: Some([250, 300, 400]),
        cpu_busy_x100: Some(350),
        rate_win_ms: Some(60_000),
        mem_total_mb: Some(48 * 1024),
        mem_used_mb: Some(26 * 1024),
        mem_avail_mb: Some(7 * 1024),
        mem_pressure: Some(MemoryLevel::Normal),
        mem_psi60_x100: Some(1150),
        swap_used_mb: Some(0),
        swap_total_mb: Some(2 * 1024),
        disk_total_mb: Some(1000 * 1024),
        disk_free_mb: Some(200 * 1024),
        gpu_cores: Some(40),
        gpu_busy_pct: Some(0),
        net_link_kbps: Some(1_000_000),
        net_rx_kbps: Some(1000),
        net_tx_kbps: Some(0),
        net_other_kbps: Some(0),
        limits: Limits {
            cpu: Some(false),
            mem: Some(false),
            disk: Some(false),
            gpu: Some(false),
        },
        // 3.5 of 14 cores busy, an idle GPU, no pressure and the load under the cores; the
        // swap counter is unread, so the over is too.
        load: Some(LoadScore {
            level_x100: 25,
            over_x100: None,
        }),
    };
    assert_eq!(second, expected);
    assert_eq!(
        first,
        MachineLook {
            at_unix_ms: Some(T0),
            cpu_busy_x100: None,
            rate_win_ms: None,
            net_rx_kbps: None,
            net_tx_kbps: None,
            net_other_kbps: None,
            limits: Limits {
                cpu: None,
                ..expected.limits
            },
            load: None,
            ..expected.clone()
        },
        "the first look differs only by what needs two"
    );
    assert_eq!(
        whole.lines_asked,
        vec![("container:disk3".to_owned(), VOLUME_TOTAL); 2],
        "the lines asked are the look's own volume's"
    );

    let mut partial = Machine::unread();
    partial.look = Look {
        cores: Some(0),
        load: Some([f64::NAN, 1.0, 1.0]),
        memory: Some(aterm_sysprobe::MemoryDetail {
            total: 0,
            pressure: None,
            ..full_look().memory.unwrap()
        }),
        gpu: Some(aterm_sysprobe::GpuReading::default()),
        volume: None,
    };
    let look = partial.look_at(&mut Keeper::new(), 0.0);
    assert_eq!(
        (look.cores, look.load_x100, look.mem_total_mb),
        (None, None, None),
        "no core, a load that is not a number and no memory size say nothing"
    );
    assert_eq!(
        (look.mem_pressure, look.gpu_cores, look.gpu_busy_pct),
        (None, None, None)
    );
    assert_eq!((look.disk_total_mb, look.disk_free_mb), (None, None));
    assert_eq!(look.mem_avail_mb, Some(7 * 1024));
    assert_eq!(look.limits, Limits::UNREAD);
}

#[test]
fn unknown_swap_stays_absent_while_measured_zero_survives_the_machine_and_fleet_views() {
    for (used, total) in [
        (None, None),
        (Some(0), Some(0)),
        (None, Some(2 * GIB)),
        (Some(3 * MIB / 2), Some(5 * MIB / 2)),
    ] {
        let mut machine = Machine::unread();
        machine.look = full_look();
        let memory = machine.look.memory.as_mut().unwrap();
        memory.swap_used = used;
        memory.swap_total = total;
        let look = machine.look_at(&mut Keeper::new(), 0.0);
        assert_eq!(
            (look.swap_used_mb, look.swap_total_mb),
            (used.map(mib), total.map(mib))
        );
        assert_eq!(
            look.mem_total_mb,
            Some(48 * 1024),
            "other memory figures remain measured"
        );

        let fleet = crate::host_vitals::HostVitals::of(&machine.look, None);
        assert_eq!(fleet.swap_used_bytes, used);
        let wire = aterm_json::to_value(&fleet).unwrap();
        assert_eq!(wire.get("swap_used_bytes").and_then(|v| v.as_u64()), used);
        assert_eq!(
            wire.get("swap_used_bytes").is_none(),
            used.is_none(),
            "unknown swap is omitted, and an observed zero remains on the wire"
        );
    }
}

/// LAW: CPU busy and the network rates are means between two looks, so a machine's first look
/// has none, and the look a minute later has the mean over that minute. CONTROLS: the counters
/// since boot read 10% busy and megabytes moved at the first look, so a rate made from one look
/// (against zero, or busy over uptime) gives another figure both times.
#[test]
fn the_first_look_has_no_rate_and_one_a_minute_later_has_the_mean_between_them() {
    let mut m = Machine::unread();
    m.look.cores = Some(4);
    m.ticks = Some(ticks(4, 1000, 9000));
    m.links = Some(vec![link(
        "en0",
        4,
        Some(1_000_000_000),
        1_000_000,
        500_000,
    )]);
    let mut keeper = Keeper::new();
    let first = m.look_at(&mut keeper, 0.0);
    assert_eq!(
        (first.cpu_busy_x100, first.rate_win_ms),
        (None, None),
        "{first:?}"
    );
    assert_eq!(
        (first.net_rx_kbps, first.net_tx_kbps, first.net_other_kbps),
        (None, None, None)
    );
    assert_eq!(
        first.net_link_kbps,
        Some(1_000_000),
        "the link's rate is a level: one look reads it"
    );

    // Over the minute each core is busy half the time: two of the four cores' worth. The
    // link receives 7.5 MB (1,000 kbit/s) and sends 750 kB (100 kbit/s).
    m.ticks = Some(ticks(4, 1000 + 3000, 9000 + 3000));
    m.links = Some(vec![link(
        "en0",
        4,
        Some(1_000_000_000),
        1_000_000 + 7_500_000,
        500_000 + 750_000,
    )]);
    let second = m.look_at(&mut keeper, 60.0);
    assert_eq!(second.rate_win_ms, Some(60_000));
    assert_eq!(
        second.cpu_busy_x100,
        Some(200),
        "since boot it is a quarter busy: one core's worth"
    );
    assert_eq!(
        (
            second.net_rx_kbps,
            second.net_tx_kbps,
            second.net_other_kbps
        ),
        (Some(1000), Some(100), Some(0))
    );
}

/// LAW: with looks every ten seconds (many askers), a rate is still the mean over the minute:
/// it is taken against the look nearest 60 s back, never the previous one. Each ten seconds is
/// busier than the last, so the previous look gives another figure at every step. Before any
/// look is 45 s back there is no rate; the first is over the 50 s there are; of two looks
/// equally near a minute back, the older.
#[test]
fn a_rate_is_taken_against_the_look_nearest_sixty_seconds_back() {
    // Ten seconds `i` adds 60 × i busy ticks of 1,000 on the one core, and 75,000 × i bytes.
    let (busy, bytes) = (|i: u64| 60 * i, |i: u64| 75_000 * i);
    let mut m = Machine::unread();
    m.look.cores = Some(1);
    let mut keeper = Keeper::new();
    let (mut user, mut idle, mut rx) = (5000, 5000, 9_000_000);
    for k in 0..=12u64 {
        if k > 0 {
            user += busy(k);
            idle += 1000 - busy(k);
            rx += bytes(k);
        }
        m.ticks = Some(ticks(1, user, idle));
        m.links = Some(vec![link("en0", 4, Some(1_000_000_000), rx, 0)]);
        let look = m.look_at(&mut keeper, 10.0 * k as f64);
        let figures = (look.rate_win_ms, look.cpu_busy_x100, look.net_rx_kbps);
        let expected = match k {
            0..=4 => (None, None, None),
            // Against the first look, 50 s back: ten-second spans 1 to 5.
            5 => (Some(50_000), Some(18), Some(180)),
            // Against the look 60 s back: spans k-5 to k, 60 × (6k − 15) busy of 6,000.
            _ => (Some(60_000), Some(6 * k - 15), Some(60 * k - 150)),
        };
        assert_eq!(figures, expected, "the look at {} s", 10 * k);
        assert_ne!(
            look.cpu_busy_x100,
            Some(6 * k),
            "CONTROL: the previous look's ten seconds at {} s",
            10 * k
        );
    }
    assert_eq!(m.looks, 13, "each look ten seconds on is a new one");

    let mut m = Machine::unread();
    m.ticks = Some(ticks(1, 0, 0));
    let mut keeper = Keeper::new();
    m.look_at(&mut keeper, 0.0);
    m.ticks = Some(ticks(1, 100, 900));
    m.look_at(&mut keeper, 10.0);
    m.ticks = Some(ticks(1, 650, 5850));
    let look = m.look_at(&mut keeper, 65.0);
    assert_eq!(
        (look.rate_win_ms, look.cpu_busy_x100),
        (Some(65_000), Some(10)),
        "65 s and 55 s back are equally near: the older, the whole 65 s"
    );
}

/// LAW: a rate needs a look 45 s to 90 s back, both ends included; with none there is no rate
/// and no window, never a rate over whatever interval there is. CONTROLS: one millisecond
/// inside each bound there is a rate, and the look after a gap is rated again a minute on.
#[test]
fn a_rate_with_no_look_45_to_90_seconds_back_is_unread() {
    let window_after = |gap: f64| {
        let mut m = Machine::unread();
        m.ticks = Some(ticks(2, 100, 900));
        m.links = Some(vec![link("en0", 4, None, 0, 0)]);
        let mut keeper = Keeper::new();
        m.look_at(&mut keeper, 0.0);
        m.ticks = Some(ticks(2, 600, 1400));
        m.links = Some(vec![link("en0", 4, None, 1000, 1000)]);
        let look = m.look_at(&mut keeper, gap);
        assert_eq!(
            look.rate_win_ms.is_some(),
            look.cpu_busy_x100.is_some(),
            "{gap}"
        );
        assert_eq!(
            look.rate_win_ms.is_some(),
            look.net_rx_kbps.is_some(),
            "{gap}"
        );
        look.rate_win_ms
    };
    assert_eq!(window_after(10.0), None);
    assert_eq!(window_after(44.999), None);
    assert_eq!(window_after(45.0), Some(45_000));
    assert_eq!(window_after(90.0), Some(90_000));
    assert_eq!(window_after(90.001), None);
    assert_eq!(window_after(600.0), None);

    let mut m = Machine::unread();
    m.ticks = Some(ticks(2, 100, 900));
    let mut keeper = Keeper::new();
    m.look_at(&mut keeper, 0.0);
    m.look_at(&mut keeper, 30.0);
    assert_eq!(
        m.look_at(&mut keeper, 40.0).rate_win_ms,
        None,
        "looks 40 s and 10 s back are both too near"
    );
    let mut keeper = Keeper::new();
    m.look_at(&mut keeper, 100.0);
    assert_eq!(m.look_at(&mut keeper, 191.0).rate_win_ms, None);
    m.ticks = Some(ticks(2, 400, 1600));
    let look = m.look_at(&mut keeper, 251.0);
    assert_eq!(
        (look.rate_win_ms, look.cpu_busy_x100),
        (Some(60_000), Some(60)),
        "against the look that had no rate itself"
    );
}

/// LAW: a cumulative 64-bit counter that went back, or a core count
/// that changed gives no rate: not a zero and not the wrapped difference. Each rate fails by
/// itself, and the window stands while the other formed. CONTROL: the same counters going
/// forward give both.
#[test]
fn a_counter_that_went_back_gives_no_rate() {
    /// The look a minute after one that read two cores at (1000, 9000) with a counted `en0` at
    /// (5000, 5000) and `en1` at (700, 700).
    fn after(cores: Vec<CoreTicks>, en0: (u64, u64), en1: (u64, u64)) -> MachineLook {
        let mut m = Machine::unread();
        m.ticks = Some(ticks(2, 1000, 9000));
        m.links = Some(vec![
            link("en0", 4, Some(1_000_000_000), 5000, 5000),
            link("en1", 5, Some(100_000_000), 700, 700),
        ]);
        let mut keeper = Keeper::new();
        m.look_at(&mut keeper, 0.0);
        m.ticks = Some(cores);
        m.links = Some(vec![
            link("en0", 4, Some(1_000_000_000), en0.0, en0.1),
            link("en1", 5, Some(100_000_000), en1.0, en1.1),
        ]);
        m.look_at(&mut keeper, 60.0)
    }
    let rates = |look: &MachineLook| {
        (
            look.rate_win_ms,
            look.cpu_busy_x100,
            look.net_rx_kbps,
            look.net_tx_kbps,
            look.net_other_kbps,
        )
    };
    let forward = ticks(2, 4000, 12_000);
    let moved = (5000 + 7_500_000, 5000 + 750_000);
    let other = (700 + 75_000, 700 + 75_000);
    assert_eq!(
        rates(&after(forward.clone(), moved, other)),
        (Some(60_000), Some(100), Some(1000), Some(100), Some(20)),
        "CONTROL"
    );

    let mut one_back = ticks(2, 9000, 12_000);
    one_back[1].user = 999;
    assert_eq!(
        rates(&after(one_back, moved, other)),
        (Some(60_000), None, Some(1000), Some(100), Some(20)),
        "one core's counter went back while the other's grew"
    );
    assert_eq!(
        rates(&after(ticks(3, 4000, 12_000), moved, other)),
        (Some(60_000), None, Some(1000), Some(100), Some(20)),
        "a core more"
    );
    assert_eq!(
        rates(&after(forward.clone(), (4999, moved.1), other)),
        (Some(60_000), Some(100), None, None, Some(20)),
        "the link's receive counter went back: the link was made again, so neither side is read"
    );
    assert_eq!(
        rates(&after(forward.clone(), (moved.0, 4999), other)),
        (Some(60_000), Some(100), None, None, Some(20))
    );
    assert_eq!(
        rates(&after(forward, moved, (699, other.1))),
        (Some(60_000), Some(100), Some(1000), Some(100), None),
        "another link's counter went back: only what is summed from it is unread"
    );
    assert_eq!(
        rates(&after(ticks(2, 999, 12_000), (4999, 4999), (699, 699))),
        (None, None, None, None, None),
        "nothing formed, so there is no window either"
    );
}

/// LAW: the CPU is at its limit only with the 1-minute load over the cores AND at least 95% of
/// the cores busy over the window (the deck's one over rule). CONTROLS: a load exactly at the
/// cores with every core busy is at capacity, not past it; a load twice the cores with the
/// cores under 95% busy (tasks blocked on I/O) is not CPU at its limit.
#[test]
fn cpu_is_at_its_limit_only_with_load_over_the_cores_and_the_cores_busy() {
    let cpu = |load1_x100: u32, busy_x100: u64| {
        let look = MachineLook {
            cores: Some(8),
            load_x100: Some([load1_x100, 0, 0]),
            cpu_busy_x100: Some(busy_x100),
            ..MachineLook::UNREAD
        };
        Limits::of(&look, None, None).cpu
    };
    assert_eq!(cpu(1600, 800), Some(true));
    assert_eq!(cpu(801, 760), Some(true), "just over the cores, 95% busy");
    assert_eq!(cpu(800, 800), Some(false), "the load is at the cores");
    assert_eq!(
        cpu(1600, 759),
        Some(false),
        "under 95% of the cores busy, whatever the load"
    );
    assert_eq!(cpu(100, 800), Some(false));
    assert_eq!(
        Limits::of(
            &MachineLook {
                load_x100: Some([0, 0, 3200]),
                ..at_every_limit()
            },
            None,
            None
        )
        .cpu,
        Some(false),
        "the 1-minute load decides, not the 15-minute one"
    );

    // Through the keeper: the verdict follows the look's own figures, and waits for the rate.
    let mut m = Machine::unread();
    m.look.cores = Some(8);
    m.look.load = Some([16.0, 9.0, 4.0]);
    m.ticks = Some(ticks(8, 1000, 9000));
    let mut keeper = Keeper::new();
    assert_eq!(m.look_at(&mut keeper, 0.0).limits.cpu, None);
    m.ticks = Some(ticks(8, 7000, 9000));
    let look = m.look_at(&mut keeper, 60.0);
    assert_eq!(
        (look.cpu_busy_x100, look.limits.cpu),
        (Some(800), Some(true))
    );
}

/// LAW: where no PSI is read (macOS, Windows), memory is at its limit at the pressure word
/// `warn` or `critical`. CONTROLS: `normal` is not, however much swap is in use; and swap in
/// use beside an unread word leaves the verdict unread, never at the limit.
#[test]
fn memory_is_at_its_limit_at_warn_or_critical_and_swap_alone_is_not() {
    let mem = |pressure: Option<MemoryLevel>| {
        let look = MachineLook {
            mem_total_mb: Some(48 * 1024),
            mem_avail_mb: Some(100),
            swap_used_mb: Some(9 * 1024),
            swap_total_mb: Some(10 * 1024),
            mem_pressure: pressure,
            ..MachineLook::UNREAD
        };
        Limits::of(&look, None, None).mem
    };
    assert_eq!(mem(Some(MemoryLevel::Warn)), Some(true));
    assert_eq!(mem(Some(MemoryLevel::Critical)), Some(true));
    assert_eq!(mem(Some(MemoryLevel::Normal)), Some(false));
    assert_eq!(mem(None), None);
}

/// LAW: where the memory PSI is read (Linux), the minute's stall decides with the room rule
/// over the look's own memory figures, and the pressure word, a ten-second level, does not.
/// The look carries the minute's figure. CONTROLS: a `critical` word under a quiet minute is
/// not at the limit; a stalled minute with exactly a tenth available is not; the same minute
/// with the memory figures unread is unread.
#[test]
fn with_psi_read_the_minutes_stall_decides_memory_and_the_pressure_word_does_not() {
    let psi = |some_avg60: f32| Psi {
        some_avg10: 0.0,
        some_avg60,
        full_avg10: 0.0,
    };
    let look = |avail_mb: Option<u64>, word: MemoryLevel| MachineLook {
        mem_total_mb: Some(10_000),
        mem_avail_mb: avail_mb,
        mem_pressure: Some(word),
        ..MachineLook::UNREAD
    };
    let mem =
        |avail_mb, word, avg60| Limits::of(&look(avail_mb, word), None, Some(&psi(avg60))).mem;
    assert_eq!(mem(Some(999), MemoryLevel::Normal, 10.0), Some(true));
    assert_eq!(mem(Some(999), MemoryLevel::Critical, 9.99), Some(false));
    assert_eq!(mem(Some(1000), MemoryLevel::Critical, 40.0), Some(false));
    assert_eq!(mem(None, MemoryLevel::Critical, 40.0), None);
    assert_eq!(
        Limits::of(
            &MachineLook {
                mem_total_mb: None,
                ..look(Some(999), MemoryLevel::Critical)
            },
            None,
            Some(&psi(40.0))
        )
        .mem,
        None
    );

    let mut m = Machine::unread();
    m.look = full_look();
    assert_eq!(m.look_at(&mut Keeper::new(), 0.0).mem_psi60_x100, None);
    m.psi = Some(psi(10.25));
    let read = m.look_at(&mut Keeper::new(), 0.0);
    assert_eq!(read.mem_psi60_x100, Some(1025));
    assert_eq!(
        read.limits.mem,
        Some(false),
        "7 of 48 GiB available is room"
    );
    if let Some(memory) = m.look.memory.as_mut() {
        memory.available = 4 * GIB;
    }
    assert_eq!(m.look_at(&mut Keeper::new(), 0.0).limits.mem, Some(true));
}

/// LAW: network capacity is the fastest counted link's driver rate, and received and sent are
/// that one link's, so use is never one link's rate against every link's traffic; the other
/// counted links' bytes are the "other" figure. A link that does not count toward the machine
/// (loopback, a tunnel, one that is down) is in none of them, whatever its rate or traffic.
/// With no driver rate there is no capacity and the two are sums over the counted links.
/// CONTROLS: each excluded link moved far more than the counted ones.
#[test]
fn the_network_pair_is_the_fastest_links_and_never_passes_its_rate_by_summing_links() {
    const GBIT: u64 = 1_000_000_000;
    /// The links at a look: `en0` (10 Gbit/s unless `rates` is off), `en1` (1 Gbit/s), and
    /// three that do not count, each counter `step` times its own per-minute traffic.
    fn links(step: u64, rates: bool) -> Vec<LinkReading> {
        let rate = |bps: u64| rates.then_some(bps);
        vec![
            LinkReading {
                loopback: true,
                physical: Some(false),
                ..link("lo0", 1, None, step * 9_000_000_000, step * 9_000_000_000)
            },
            link("en1", 6, rate(GBIT), step * 750_000, step * 75_000),
            link(
                "en0",
                4,
                rate(10 * GBIT),
                step * 75_000_000,
                step * 7_500_000,
            ),
            LinkReading {
                physical: None,
                ..link("utun3", 9, None, step * 8_000_000_000, step * 8_000_000_000)
            },
            LinkReading {
                up: false,
                ..link("en7", 7, rate(100 * GBIT), step * 7_000_000_000, 0)
            },
        ]
    }
    let net = |look: &MachineLook| {
        (
            look.net_link_kbps,
            look.net_rx_kbps,
            look.net_tx_kbps,
            look.net_other_kbps,
        )
    };
    let a_minute = |first: Option<Vec<LinkReading>>, second: Option<Vec<LinkReading>>| {
        let mut m = Machine::unread();
        m.links = first;
        let mut keeper = Keeper::new();
        m.look_at(&mut keeper, 0.0);
        m.links = second;
        net(&m.look_at(&mut keeper, 60.0))
    };

    assert_eq!(
        a_minute(Some(links(1, true)), Some(links(2, true))),
        (Some(10_000_000), Some(10_000), Some(1000), Some(110)),
        "en0's rate and en0's traffic; en1's 100 + 10 kbit/s apart"
    );
    assert_eq!(
        a_minute(Some(links(1, false)), Some(links(2, false))),
        (None, Some(10_100), Some(1010), Some(0)),
        "no link reports a rate: sums over the counted links, and nothing is left out"
    );

    let mut tied = links(2, true);
    tied[2].rate_bps = Some(GBIT);
    assert_eq!(
        a_minute(Some(links(1, true)), Some(tied)),
        (Some(1_000_000), Some(10_000), Some(1000), Some(110)),
        "of two equally fast, the lower interface index: en0"
    );

    let en0_only = |step| Some(vec![links(step, true).swap_remove(2)]);
    assert_eq!(
        a_minute(en0_only(1), Some(links(2, true))),
        (Some(10_000_000), Some(10_000), Some(1000), None),
        "en1 did not count a minute ago: what it moved since is unread, not zero"
    );
    let mut unrated = links(2, false);
    unrated.truncate(3);
    let mut then = links(1, false);
    then.remove(1);
    assert_eq!(
        a_minute(Some(then), Some(unrated)),
        (None, None, None, None),
        "a sum with one link unread is unread"
    );
    let mut none_counted = links(2, true);
    none_counted.retain(|l| !matches!(l.name.as_str(), "en0" | "en1"));
    assert_eq!(
        a_minute(Some(links(1, true)), Some(none_counted)),
        (None, None, None, None),
        "no counted link"
    );
    assert_eq!(
        a_minute(Some(links(1, true)), None),
        (None, None, None, None),
        "the links were not read"
    );
    assert_eq!(
        a_minute(None, Some(links(2, true))),
        (Some(10_000_000), None, None, None),
        "the links were not read a minute ago"
    );
}

/// Routing residues can cross zero in a short, capacity-bounded window. The same raw
/// values are not a valid delta after an interface replacement, source change or reset.
#[test]
fn network_rates_preserve_modulo_source_and_interface_identity() {
    const MODULUS: u64 = 1 << 32;
    let mut before = link(
        "en0",
        8,
        Some(100_000_000),
        MODULUS - 1_024_000,
        MODULUS - 2_048_000,
    );
    before.byte_counter = LinkByteCounter::DarwinModulo32;
    before.rx.packets = 1000;
    before.tx.packets = 2000;
    let mut now = before.clone();
    now.rx.bytes = 2_048_000;
    now.tx.bytes = 1_024_000;
    now.rx.packets += 3000;
    now.tx.packets += 3000;
    let read = |before: &LinkReading, now: &LinkReading, seconds| {
        let retained = [LinkBytes {
            name: before.name.clone(),
            sample: link_byte_sample(before),
        }];
        let net = Net::of(
            Some(&[now]),
            Some((&retained, Duration::from_secs(seconds))),
        );
        (net.link_kbps, net.rx_kbps, net.tx_kbps, net.other_kbps)
    };
    assert_eq!(
        read(&before, &now, 1),
        (Some(100_000), Some(24_576), Some(24_576), Some(0))
    );
    assert_eq!(
        read(&before, &now, 60),
        (Some(100_000), Some(409), Some(409), Some(0))
    );

    let unread = (Some(100_000), None, None, Some(0));
    let mut changed = now.clone();
    changed.index += 1;
    assert_eq!(
        read(&before, &changed, 1),
        unread,
        "the same name on another interface is no baseline"
    );
    changed = now.clone();
    changed.byte_counter = LinkByteCounter::Cumulative64;
    assert_eq!(
        read(&before, &changed, 1),
        unread,
        "counter sources must agree"
    );
    changed = now.clone();
    changed.rx.packets = before.rx.packets - 1;
    assert_eq!(
        read(&before, &changed, 1),
        unread,
        "a packet reset is not a byte wrap"
    );
    changed = now.clone();
    changed.rate_bps = Some(10_000_000);
    assert_eq!(
        read(&before, &changed, 1),
        (Some(10_000), None, None, Some(0)),
        "a changed capacity does not establish the past window's bound"
    );

    before.byte_counter = LinkByteCounter::Cumulative64;
    before.rx.bytes = 70 * GIB;
    before.tx.bytes = 80 * GIB;
    now = before.clone();
    now.rx.bytes += 3_072_000;
    now.tx.bytes += 3_072_000;
    assert_eq!(
        read(&before, &now, 1),
        (Some(100_000), Some(24_576), Some(24_576), Some(0)),
        "ordinary full-width cumulative counters retain their real totals"
    );
}

/// A small forward residue on a fast link cannot identify how many 4 GiB wraps occurred
/// during the keeper's 45–90 second window. Capacity remains known while use stays unknown.
#[test]
fn long_network_looks_do_not_present_a_forward_modulo_residue_as_measured_traffic() {
    for gap in [45, 60, 90] {
        let mut m = Machine::unread();
        let mut before = link("en0", 8, Some(10_000_000_000), 1_024_000, 2_048_000);
        before.byte_counter = LinkByteCounter::DarwinModulo32;
        before.rx.packets = 1000;
        before.tx.packets = 2000;
        m.links = Some(vec![before.clone()]);
        let mut keeper = Keeper::new();
        m.look_at(&mut keeper, 0.0);
        before.rx.bytes += 1_024_000;
        before.tx.bytes += 1_024_000;
        before.rx.packets += 1000;
        before.tx.packets += 1000;
        m.links = Some(vec![before]);
        let look = m.look_at(&mut keeper, gap as f64);
        assert_eq!(look.rate_win_ms, Some(gap * 1000));
        assert_eq!(
            (
                look.net_link_kbps,
                look.net_rx_kbps,
                look.net_tx_kbps,
                look.net_other_kbps
            ),
            (Some(10_000_000), None, None, Some(0)),
            "{gap} seconds permit multiple wraps even though both residues increased"
        );
    }
}

/// A moving link without a byte bound makes only its own share unavailable. An idle
/// interface with unchanged packets and bytes does not erase another link's measured use.
#[test]
fn unknown_modulo_traffic_does_not_become_zero_in_network_capacity_totals() {
    let mut main_before = link("en0", 8, Some(10_000_000_000), 70 * GIB, 80 * GIB);
    main_before.rx.packets = 1000;
    main_before.tx.packets = 2000;
    let mut main_now = main_before.clone();
    main_now.rx.bytes += 7_500_000;
    main_now.tx.bytes += 750_000;
    let mut other_before = link("en1", 9, None, 1_024_000, 2_048_000);
    other_before.byte_counter = LinkByteCounter::DarwinModulo32;
    other_before.rx.packets = 500;
    other_before.tx.packets = 600;
    let mut other_now = other_before.clone();
    let retained = [
        LinkBytes {
            name: main_before.name.clone(),
            sample: link_byte_sample(&main_before),
        },
        LinkBytes {
            name: other_before.name.clone(),
            sample: link_byte_sample(&other_before),
        },
    ];
    let read = |main: &LinkReading, other: &LinkReading| {
        let net = Net::of(
            Some(&[main, other]),
            Some((&retained, Duration::from_secs(60))),
        );
        (net.rx_kbps, net.tx_kbps, net.other_kbps)
    };
    assert_eq!(
        read(&main_now, &other_now),
        (Some(1000), Some(100), Some(0)),
        "unchanged unused interface"
    );
    other_now.rx.bytes += 1024;
    other_now.rx.packets += 1;
    assert_eq!(
        read(&main_now, &other_now),
        (Some(1000), Some(100), None),
        "moving bytes with no capacity have no proven wrap count"
    );
}

/// LAW: the disk is at its limit with free space below the floor the disk watch holds the
/// volume to, the line at which aterm starts reclaiming; with the floor turned off, below the
/// emergency line. CONTROLS: free space between the floor and the warning line is within the
/// limit (the warning is not the limit); a floor of 0 is "off", not "never at its limit".
#[test]
fn disk_is_at_its_limit_below_the_watchs_floor_and_below_the_emergency_line_with_no_floor() {
    let disk = |free_bytes: u64, lines: &Lines| {
        let look = MachineLook {
            disk_free_mb: Some(free_bytes / MIB),
            ..MachineLook::UNREAD
        };
        Limits::of(&look, Some(lines), None).disk
    };
    let kept = default_lines();
    assert!(
        kept.emergency + GIB < kept.floor && kept.floor + GIB < kept.warn,
        "the fixture tells the three lines apart: {kept:?}"
    );
    assert_eq!(disk_limit_line(&kept), kept.floor);
    assert_eq!(disk(kept.floor - MIB, &kept), Some(true));
    assert_eq!(disk(kept.floor, &kept), Some(false));
    assert_eq!(
        disk(kept.floor + GIB, &kept),
        Some(false),
        "under the warning line, over the floor"
    );
    assert_eq!(disk(kept.emergency, &kept), Some(true));

    let mut knobs = Knobs::default();
    assert!(knobs.read("auto_free_gib", 0));
    let off = crate::floor::lines(knobs, Some(VOLUME_TOTAL), None);
    assert_eq!((off.floor, off.emergency), (0, 8 * GIB));
    assert_eq!(disk_limit_line(&off), off.emergency);
    assert_eq!(disk(off.emergency - MIB, &off), Some(true));
    assert_eq!(disk(off.emergency, &off), Some(false));
    assert_eq!(disk(0, &off), Some(true));

    // Through the keeper: the home volume's free space against the lines the probe gives.
    let mut m = Machine::unread();
    m.look = full_look();
    m.lines = Some(kept);
    assert_eq!(m.look_at(&mut Keeper::new(), 0.0).limits.disk, Some(false));
    if let Some(volume) = m.look.volume.as_mut() {
        volume.free = kept.floor - 1;
    }
    assert_eq!(m.look_at(&mut Keeper::new(), 0.0).limits.disk, Some(true));
}

/// LAW: a verdict is `None` where any input of its rule is unread, never "within the limit".
/// CONTROL: with every input read each of the four is at its limit, and taking one input away
/// changes that verdict alone.
#[test]
fn a_verdict_with_an_unread_input_is_unread() {
    let all = at_every_limit();
    let lines = default_lines();
    let stalled = Psi {
        some_avg10: 50.0,
        some_avg60: 50.0,
        full_avg10: 50.0,
    };
    let at_limit = Limits {
        cpu: Some(true),
        mem: Some(true),
        disk: Some(true),
        gpu: Some(true),
    };
    assert_eq!(Limits::of(&all, Some(&lines), None), at_limit);
    assert_eq!(Limits::of(&all, Some(&lines), Some(&stalled)), at_limit);

    let without = |edit: fn(&mut MachineLook)| {
        let mut look = all.clone();
        edit(&mut look);
        look
    };
    for (what, look) in [
        ("cores", without(|l| l.cores = None)),
        ("load", without(|l| l.load_x100 = None)),
        ("busy", without(|l| l.cpu_busy_x100 = None)),
    ] {
        assert_eq!(
            Limits::of(&look, Some(&lines), None),
            Limits {
                cpu: None,
                ..at_limit
            },
            "{what} unread"
        );
    }
    assert_eq!(
        Limits::of(&without(|l| l.mem_pressure = None), Some(&lines), None),
        Limits {
            mem: None,
            ..at_limit
        }
    );
    for look in [
        without(|l| l.mem_total_mb = None),
        without(|l| l.mem_avail_mb = None),
    ] {
        assert_eq!(
            Limits::of(&look, Some(&lines), Some(&stalled)),
            Limits {
                mem: None,
                ..at_limit
            }
        );
    }
    assert_eq!(
        Limits::of(&without(|l| l.disk_free_mb = None), Some(&lines), None),
        Limits {
            disk: None,
            ..at_limit
        }
    );
    assert_eq!(
        Limits::of(&all, None, None),
        Limits {
            disk: None,
            ..at_limit
        },
        "the watch's lines unread"
    );
    assert_eq!(
        Limits::of(&without(|l| l.gpu_busy_pct = None), Some(&lines), None),
        Limits {
            gpu: None,
            ..at_limit
        }
    );
    assert_eq!(
        Limits::of(&without(|l| l.gpu_busy_pct = Some(94)), Some(&lines), None).gpu,
        Some(false)
    );
    assert_eq!(
        Limits::of(&without(|l| l.gpu_busy_pct = Some(95)), Some(&lines), None).gpu,
        Some(true)
    );
    assert_eq!(Limits::of(&MachineLook::UNREAD, None, None), Limits::UNREAD);
}

/// LAW: one look is kept ten seconds: asked again inside them, the keeper reads nothing and
/// answers the same look, its time included; at ten seconds it reads the machine again.
/// CONTROL: the machine changed between the looks, so a keeper that always read would show it
/// early and one that never read again would not show it at all.
#[test]
fn a_look_is_kept_ten_seconds_and_read_again_after() {
    let mut m = Machine::unread();
    m.look.cores = Some(8);
    m.look.load = Some([1.0, 1.0, 1.0]);
    let mut keeper = Keeper::new();
    let first = m.look_at(&mut keeper, 0.0);
    assert_eq!((m.looks, first.at_unix_ms), (1, Some(T0)));

    m.look.load = Some([7.0, 1.0, 1.0]);
    for secs in [0.001, 5.0, 9.999] {
        assert_eq!(m.look_at(&mut keeper, secs), first, "{secs} s on");
    }
    assert_eq!(m.looks, 1, "nothing was read inside the ten seconds");

    let again = m.look_at(&mut keeper, 10.0);
    assert_eq!(m.looks, 2);
    assert_eq!(
        (again.at_unix_ms, again.load_x100),
        (Some(T0 + 10_000), Some([700, 100, 100]))
    );
    assert_eq!(m.look_at(&mut keeper, 19.999), again);
    assert_eq!(m.looks, 2, "the ten seconds count from the look that read");
    m.look_at(&mut keeper, 20.0);
    assert_eq!(m.looks, 3);
}

/// LAW: a kept look answers only for the home it read the volume of: asked for another home
/// inside the ten seconds, the keeper reads again. That look adds no counters to the ring,
/// which holds at most one look a ten seconds. CONTROL: 65 s after the first look the rate is
/// over the 65 s; had the look at 5 s been kept as counters, it would be over that look's 60 s.
#[test]
fn a_look_for_another_home_is_read_again_and_adds_no_counters_inside_the_keep() {
    let mut m = Machine::unread();
    m.ticks = Some(ticks(1, 0, 1000));
    let mut keeper = Keeper::new();
    let (here, there) = (Path::new("/home/a"), Path::new("/home/b"));
    keeper.look(&mut m, Some(here));
    m.at = Duration::from_secs(5);
    keeper.look(&mut m, Some(here));
    assert_eq!(m.looks, 1);
    keeper.look(&mut m, Some(there));
    assert_eq!(m.looks, 2, "another home's volume was never read");
    keeper.look(&mut m, Some(there));
    assert_eq!(m.looks, 2);

    m.ticks = Some(ticks(1, 650, 6850));
    m.at = Duration::from_secs(65);
    let look = keeper.look(&mut m, Some(there));
    assert_eq!(
        (look.rate_win_ms, look.cpu_busy_x100),
        (Some(65_000), Some(10))
    );
}

/// LAW: a look is kept only while both clocks say it is young. The monotonic clock stops while
/// the machine sleeps, so a look taken just before an eight-hour sleep is five seconds old on it
/// and eight hours old on the wall clock: the keeper reads the machine again, and that look's
/// time is the wall clock's now. Nor is a rate taken across the sleep: the counters of the look
/// before it went with it, so the look after has no rate, and one a minute later has a rate over
/// that minute. CONTROLS: with no sleep the same five seconds keep the look, and the same
/// minute rates it; a wall clock that reads before the epoch leaves the keep to the monotonic
/// clock alone.
#[test]
fn a_look_from_before_a_sleep_is_neither_kept_nor_rated_against() {
    const SLEPT: Duration = Duration::from_secs(8 * 3600);
    let across = |slept: Duration| {
        let mut m = Machine::unread();
        m.look.cores = Some(2);
        m.ticks = Some(ticks(2, 100, 900));
        let mut keeper = Keeper::new();
        let first = m.look_at(&mut keeper, 0.0);
        m.ticks = Some(ticks(2, 600, 1400));
        m.look_at(&mut keeper, 50.0);
        m.slept = slept;
        m.look.cores = Some(4);
        let after = m.look_at(&mut keeper, 55.0);
        (first, after, m.looks)
    };

    let (_, kept, looks) = across(Duration::ZERO);
    assert_eq!(
        looks, 2,
        "CONTROL: five seconds on, with no sleep, the look is kept"
    );
    assert_eq!(kept.cores, Some(2));

    let (_, woke, looks) = across(SLEPT);
    assert_eq!(
        looks, 3,
        "the look from before the sleep was answered as now"
    );
    let wall = T0 + 55_000 + u64::try_from(SLEPT.as_millis()).unwrap();
    assert_eq!(
        (woke.at_unix_ms, woke.cores),
        (Some(wall), Some(4)),
        "{woke:?}"
    );
    assert_eq!(
        (woke.rate_win_ms, woke.cpu_busy_x100),
        (None, None),
        "the look 55 s back on the monotonic clock is eight hours back: {woke:?}"
    );

    let rate_after = |slept: Duration| {
        let mut m = Machine::unread();
        m.ticks = Some(ticks(1, 0, 1000));
        let mut keeper = Keeper::new();
        m.look_at(&mut keeper, 0.0);
        m.slept = slept;
        m.ticks = Some(ticks(1, 300, 6700));
        let look = m.look_at(&mut keeper, 60.0);
        (look.rate_win_ms, look.cpu_busy_x100)
    };
    assert_eq!(
        rate_after(Duration::ZERO),
        (Some(60_000), Some(5)),
        "CONTROL: with no sleep the minute is a rate"
    );
    assert_eq!(
        rate_after(SLEPT),
        (None, None),
        "no rate is taken across a sleep"
    );
    assert_eq!(
        rate_after(LOOK_KEEP),
        (Some(60_000), Some(5)),
        "a wall clock ahead by no more than the keep (a slew, a short nap) still rates"
    );

    let mut m = Machine::unread();
    m.clock_read = false;
    let mut keeper = Keeper::new();
    m.look_at(&mut keeper, 0.0);
    m.look_at(&mut keeper, 5.0);
    assert_eq!(m.looks, 1, "CONTROL: an unread wall clock keeps the look");
}

/// The process's own look reads this machine in process and is kept: asked twice in a row it
/// is one look, with its time and this machine's cores, and the home's volume no smaller than
/// what is free on it. It is this process's first look, so it has no rate yet.
#[test]
fn the_latest_look_reads_this_machine_and_is_kept() {
    let home = aterm_tempfile::tempdir().unwrap();
    let look = latest(Some(home.path()));
    assert!(look.at_unix_ms.is_some(), "{look:?}");
    assert!(look.cores.is_some_and(|n| n > 0), "{look:?}");
    let (total, free) = (look.disk_total_mb, look.disk_free_mb);
    assert!(
        total.zip(free).is_some_and(|(total, free)| total >= free),
        "{look:?}"
    );
    assert_eq!((look.cpu_busy_x100, look.rate_win_ms), (None, None));
    assert_eq!(latest(Some(home.path())), look);
}

/// The deck's dial over the readings its snapshot would hold of the same machine, built here as
/// [`aterm_vitals::strain::Now::of`] builds them from the deck's rows, in hundredths.
fn dial(now: &Now) -> Option<LoadScore> {
    let read = aterm_vitals::strain::strain(now, []);
    let x100 = |v: f32| (f64::from(v) * 100.0).round() as u8;
    Some(LoadScore {
        level_x100: x100(read.level?),
        over_x100: Some(x100(read.over)),
    })
}

/// LAW: the load score is the resource deck's own dial read over the look's figures: the
/// busier of the CPU's and the GPU's busy shares, lifted by memory pressure, and the depth past
/// capacity by the deck's one over rule (the 1-minute load as its queue) and its swap rule.
/// Each case is checked against `aterm_vitals::strain` over the readings the deck's snapshot
/// holds of the same machine and against its figure written out. CONTROLS: a GPU busier than
/// the CPU raises the level; pegged cores with nothing waiting are full, not over; pressure
/// `warn` lifts the level and moves no over; `critical` is the over floor; swap at 64 MiB/s
/// pins the over with the CPU calm.
#[test]
fn the_load_score_is_the_decks_own_dial_over_the_looks_figures() {
    let look = |busy_x100: u64, load1_x100: u32, gpu: Option<u8>, word: MemoryLevel| MachineLook {
        cores: Some(8),
        load_x100: Some([load1_x100, 100, 100]),
        cpu_busy_x100: Some(busy_x100),
        gpu_busy_pct: gpu,
        mem_pressure: Some(word),
        ..MachineLook::UNREAD
    };
    let deck = |busy: f64, queue: f64, gpu: Option<f64>, pressure: Pressure, swap_bps| Now {
        cpu: Some(busy / 8.0),
        gpu,
        cores: Some(8.0),
        queue: Some(queue),
        swap_bps,
        pressure,
    };
    const MIB_S: f64 = 1_048_576.0;
    let cases = [
        // Two of eight cores busy, the GPU idle, the load under the cores.
        (
            look(200, 150, Some(0), MemoryLevel::Normal),
            Some(0.0),
            deck(2.0, 1.5, Some(0.0), Pressure::Normal, Some(0.0)),
            (25, 0),
        ),
        // The GPU at 97% is the busier engine.
        (
            look(200, 150, Some(97), MemoryLevel::Normal),
            Some(0.0),
            deck(2.0, 1.5, Some(0.97), Pressure::Normal, Some(0.0)),
            (97, 0),
        ),
        // Every core busy and twice the cores waiting: a third of the way past capacity.
        (
            look(800, 1600, Some(0), MemoryLevel::Normal),
            Some(0.0),
            deck(8.0, 16.0, Some(0.0), Pressure::Normal, Some(0.0)),
            (100, 33),
        ),
        // Every core busy and nothing waiting: full, not over.
        (
            look(800, 800, None, MemoryLevel::Normal),
            Some(0.0),
            deck(8.0, 8.0, None, Pressure::Normal, Some(0.0)),
            (100, 0),
        ),
        // The kernel reclaiming lifts a calm machine to the warn level and no further.
        (
            look(80, 50, None, MemoryLevel::Warn),
            Some(0.0),
            deck(0.8, 0.5, None, Pressure::Warn, Some(0.0)),
            (88, 0),
        ),
        // The kernel killing: full, at the over floor.
        (
            look(80, 50, None, MemoryLevel::Critical),
            Some(0.0),
            deck(0.8, 0.5, None, Pressure::Critical, Some(0.0)),
            (100, 12),
        ),
        // Swapping 64 MiB a second with the CPU calm: the over is pinned, the level is not.
        (
            look(80, 50, Some(3), MemoryLevel::Warn),
            Some(64.0 * MIB_S),
            deck(0.8, 0.5, Some(0.03), Pressure::Warn, Some(64.0 * MIB_S)),
            (88, 100),
        ),
    ];
    for (at, (look, swap_bps, now, (level_x100, over_x100))) in cases.into_iter().enumerate() {
        let score = LoadScore::of(&look, swap_bps);
        assert_eq!(score, dial(&now), "case {at}: the deck's dial");
        assert_eq!(
            score,
            Some(LoadScore {
                level_x100,
                over_x100: Some(over_x100),
            }),
            "case {at}"
        );
    }
}

/// LAW: the load score is unread where the CPU's busy share is (no look a minute back, no
/// cores), whatever the GPU and the memory say, and never a score of 0. CONTROL: the same look
/// with the share read scores, a measured idle machine as a read 0.
#[test]
fn the_load_score_is_unread_without_the_cpus_busy_share_and_never_zero() {
    let rated = MachineLook {
        cores: Some(4),
        load_x100: Some([0, 0, 0]),
        cpu_busy_x100: Some(0),
        gpu_busy_pct: Some(97),
        mem_pressure: Some(MemoryLevel::Critical),
        ..MachineLook::UNREAD
    };
    assert_eq!(
        LoadScore::of(
            &MachineLook {
                gpu_busy_pct: Some(0),
                mem_pressure: Some(MemoryLevel::Normal),
                ..rated.clone()
            },
            Some(0.0)
        ),
        Some(LoadScore {
            level_x100: 0,
            over_x100: Some(0)
        }),
        "an idle machine read is a read 0"
    );
    assert_eq!(LoadScore::of(&rated, None).map(|s| s.level_x100), Some(100));
    for unrated in [
        MachineLook {
            cpu_busy_x100: None,
            ..rated.clone()
        },
        MachineLook {
            cores: None,
            ..rated.clone()
        },
        MachineLook::UNREAD,
    ] {
        assert_eq!(LoadScore::of(&unrated, Some(1e9)), None, "{unrated:?}");
    }
}

/// LAW: THE OVER IS UNREAD WHERE THE DIAL COULD NOT JUDGE IT, never a 0 that says within
/// capacity: with the CPU's busy share read (the level reads) and the run queue unread (no load
/// average: Windows, or a failed `getloadavg`), the swap traffic unread (no counter a window
/// back, one that went back), or, where neither says over, the memory pressure unread, the
/// score keeps its level and says no over. A term that is read and over still reads with the
/// pressure unread: `critical` only lifts to the floor, which an over term already passes.
/// CONTROL: the same pegged machine with every input read and nothing waiting is a read 0.
#[test]
fn the_over_is_unread_where_its_inputs_are_never_zero() {
    let pegged = MachineLook {
        cores: Some(4),
        load_x100: Some([400, 400, 400]),
        cpu_busy_x100: Some(400),
        mem_pressure: Some(MemoryLevel::Normal),
        ..MachineLook::UNREAD
    };
    let over = |look: &MachineLook, swap_bps: Option<f64>| {
        let score = LoadScore::of(look, swap_bps).expect("the busy share is read");
        assert_eq!(score.level_x100, 100, "the level reads: {look:?}");
        score.over_x100
    };
    assert_eq!(
        over(&pegged, Some(0.0)),
        Some(0),
        "CONTROL: full, nothing waits"
    );
    assert_eq!(
        over(
            &MachineLook {
                load_x100: None,
                ..pegged.clone()
            },
            Some(0.0)
        ),
        None,
        "the run queue unread"
    );
    assert_eq!(over(&pegged, None), None, "the swap traffic unread");
    let pressure_unread = MachineLook {
        mem_pressure: None,
        ..pegged.clone()
    };
    assert_eq!(
        over(&pressure_unread, Some(0.0)),
        None,
        "the pressure unread"
    );
    assert_eq!(
        over(
            &MachineLook {
                load_x100: Some([1600, 400, 400]),
                ..pressure_unread
            },
            Some(0.0)
        ),
        Some(67),
        "four times the cores waiting reads over with the pressure unread"
    );
}

/// LAW: through the keeper, the load score waits for the CPU's rate, and its swap term is the
/// swap counter's traffic over the same window as the rates. CONTROLS: the same minute with the
/// swap counter unread at either end, or one that went back, gives no swap rate, so its over is
/// unread beside its level, never a calm 0.
#[test]
fn the_keepers_load_score_reads_the_swap_traffic_over_the_rate_window() {
    let minute = |swap: (Option<u64>, Option<u64>)| {
        let mut m = Machine::unread();
        m.look.cores = Some(4);
        m.look.load = Some([0.5, 0.5, 0.5]);
        m.look.memory = Some(aterm_sysprobe::MemoryDetail {
            pressure: Some(MemoryLevel::Normal),
            ..aterm_sysprobe::MemoryDetail::default()
        });
        m.ticks = Some(ticks(4, 1000, 9000));
        m.swapped_kib = swap.0;
        let mut keeper = Keeper::new();
        let first = m.look_at(&mut keeper, 0.0).load;
        // One core's worth busy over the minute.
        m.ticks = Some(ticks(4, 1000 + 1500, 9000 + 4500));
        m.swapped_kib = swap.1;
        (first, m.look_at(&mut keeper, 60.0).load)
    };
    let calm = Some(LoadScore {
        level_x100: 25,
        over_x100: Some(0),
    });
    let unjudged = Some(LoadScore {
        level_x100: 25,
        over_x100: None,
    });
    // 64 MiB a second for the minute: 64 × 1024 KiB × 60 s.
    let (first, swapping) = minute((Some(1_000), Some(1_000 + 64 * 1024 * 60)));
    assert_eq!(first, None, "the first look has no CPU rate to score");
    assert_eq!(
        swapping,
        Some(LoadScore {
            level_x100: 25,
            over_x100: Some(100),
        })
    );
    assert_eq!(
        minute((Some(1_000), Some(1_000 + 4 * 1024 * 60))).1,
        Some(LoadScore {
            level_x100: 25,
            over_x100: Some(12),
        }),
        "4 MiB/s is the line: over, at the floor"
    );
    assert_eq!(
        minute((Some(1_000), Some(1_000 + 2 * 1024 * 60))).1,
        calm,
        "under the line"
    );
    assert_eq!(minute((None, Some(64 * 1024 * 60))).1, unjudged);
    assert_eq!(minute((Some(64 * 1024 * 60), None)).1, unjudged);
    assert_eq!(
        minute((Some(64 * 1024 * 60), Some(0))).1,
        unjudged,
        "a counter that went back"
    );
}
