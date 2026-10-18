//! The demo programs — each written ONCE against [`EffectSeam`], run under record
//! and replay. [`run`] reads the three backend doors (clock, rng, file); its
//! digest folds the effects **non-commutatively** (so a replay bug that transposes
//! the two random words is observable), with the file bytes folded last.
//! [`converse`] goes through every door, including the three that carry a value
//! the program supplies (a reply, an output, a flag), and each has an
//! independent oracle over its tape.

use crate::seam::{EffectRecord, EffectSeam, EffectsLog};

const P: u64 = 0x0000_0100_0000_01b3;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "demo_island.rs"]
mod demo_island;
#[cfg(not(clean_islands))]
pub use crate::twins::demo_clamp_poll_index;
#[cfg(clean_islands)]
pub use demo_island::demo_clamp_poll_index;

contract_fn! {
    /// Verified check that a stop-poll index `idx` is strictly within `max_reads`,
    /// linked via `clean-kernel-defeq` to `demo_poll_in_range_isl`.
    #[must_use]
    pub fn demo_poll_in_range(idx: u64, max_reads: u64) -> bool
    clauses [ensures result == (idx < max_reads)]
    {
        idx < max_reads
    }
}

/// Fold the four effects into a digest. Non-commutative in `r0`/`r1`.
fn digest(c: u64, r0: u64, r1: u64, bytes: &[u8]) -> u64 {
    let mut d = c;
    d = d.wrapping_mul(P).wrapping_add(r0);
    d = d.wrapping_mul(P).wrapping_add(r1);
    for &b in bytes {
        d = d.wrapping_mul(P).wrapping_add(b as u64);
    }
    d
}

/// The program: read one clock, two random words, then one file — in that fixed
/// order — and fold them. Works against any [`EffectSeam`], so the *same code*
/// runs under record and replay; a seam's refusal (a diverged replay) stops the
/// program at the effect it could not answer and is returned unchanged.
pub fn run<S: EffectSeam>(seam: &mut S, path: &str) -> Result<u64, S::Error> {
    let c = seam.now_nanos()?;
    let r0 = seam.next_rand()?;
    let r1 = seam.next_rand()?;
    let bytes = seam.read_file(path)?;
    Ok(digest(c, r0, r1, &bytes))
}

/// A GENUINELY INDEPENDENT recomputation of the digest — the order-sensitive head
/// is spelled out as a **closed form**, written WITHOUT the `digest` loop, so a
/// copy-paste fold bug (e.g. an `r0`/`r1` transposition or a wrong multiplier in
/// `digest`) cannot also live here: the two would then disagree. Equal to `digest`
/// on correct inputs. So `run(replay) == oracle(recorded values)` catches both
/// replay-machinery bugs (value flows through the seam on one side, directly on the
/// other) AND a shared fold-algorithm bug — never `f(x) == f(x)`.
pub fn oracle(c: u64, r0: u64, r1: u64, bytes: &[u8]) -> u64 {
    let head = c
        .wrapping_mul(P)
        .wrapping_add(r0)
        .wrapping_mul(P)
        .wrapping_add(r1);
    bytes
        .iter()
        .fold(head, |d, &b| d.wrapping_mul(P).wrapping_add(b as u64))
}

/// Extract the `(clock, rand0, rand1, file-bytes)` a tape recorded, in order.
/// Returns `None` unless the tape is exactly `[Clock, Rand, Rand, File]`.
pub fn tape_values(log: &EffectsLog) -> Option<(u64, u64, u64, Vec<u8>)> {
    match log.0.as_slice() {
        [EffectRecord::Clock(c), EffectRecord::Rand(r0), EffectRecord::Rand(r1), EffectRecord::File { bytes, .. }] => {
            Some((*c, *r0, *r1, bytes.clone()))
        }
        _ => None,
    }
}

/// The door the [`converse`] demo sends its question through.
pub const PEER: &str = "peer";
/// The door the [`converse`] demo prints through.
pub const OUT: &str = "out";
/// The flag the [`converse`] demo polls.
pub const STOP: &str = "stop";
/// How many times [`converse`] polls [`STOP`], back to back.
pub const STOP_READS: usize = 4;

/// FNV-1a's offset basis: the [`converse`] digest's starting word, so an
/// all-zero run does not digest to zero.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The question [`converse`] asks: built from the random word and the file it
/// read, so a replay fed a wrong value asks a DIFFERENT question — which the
/// reply door refuses as a diverged request — instead of silently receiving
/// the recorded answer to a question it did not ask.
pub fn ask(rand: u64, file: &[u8]) -> Vec<u8> {
    format!("ask rand={rand:016x} len={}", file.len()).into_bytes()
}

/// What [`converse`] prints for a reply.
pub fn say(reply: &[u8]) -> Vec<u8> {
    let mut line = b"got ".to_vec();
    line.extend_from_slice(reply);
    line
}

/// The program's running digest: one multiply-add per word, a length word
/// before every byte string so adjacent strings cannot slide into each other.
struct Digest(u64);

impl Digest {
    fn word(&mut self, w: u64) {
        self.0 = self.0.wrapping_mul(P).wrapping_add(w);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        for &x in b {
            self.word(u64::from(x));
        }
    }
}

/// The doors demo: a program that goes through EVERY door of [`EffectSeam`],
/// in a fixed order, and folds everything it saw and said into one digest:
///
/// 1. a monotonic read, a wall-clock read, a random word, one file;
/// 2. one [`exchange`](EffectSeam::exchange) through [`PEER`] whose request
///    ([`ask`]) depends on the random word and the file;
/// 3. one [`emit`](EffectSeam::emit) through [`OUT`] of [`say`]`(reply)`;
/// 4. [`STOP_READS`] back-to-back [`raised`](EffectSeam::raised) reads of
///    [`STOP`], with nothing between them on the tape;
/// 5. a closing monotonic read.
///
/// `peer` and `stop` are the live world of the two closure doors. The same
/// code runs under every seam; a replay must never call either.
pub fn converse<S: EffectSeam>(
    seam: &mut S,
    path: &str,
    peer: &mut dyn FnMut(&[u8]) -> Vec<u8>,
    stop: &mut dyn FnMut() -> bool,
) -> Result<u64, S::Error> {
    let mono0 = seam.mono_nanos()?;
    let clock = seam.now_nanos()?;
    let rand = seam.next_rand()?;
    let file = seam.read_file(path)?;
    let request = ask(rand, &file);
    let reply = seam.exchange(PEER, &request, peer)?;
    let said = say(&reply);
    seam.emit(OUT, &said)?;
    let mut stops = [false; STOP_READS];
    for read in &mut stops {
        *read = seam.raised(STOP, stop)?;
    }
    let mono1 = seam.mono_nanos()?;

    let mut d = Digest(OFFSET);
    d.word(mono0);
    d.word(clock);
    d.word(rand);
    d.bytes(&file);
    d.bytes(&request);
    d.bytes(&reply);
    d.bytes(&said);
    // The flag reads as one word, bit i = read i.
    d.word(stops.iter().enumerate().fold(0, |w, (i, &up)| {
        w | u64::from(up).checked_shl(i as u32).unwrap_or(0)
    }));
    d.word(mono1);
    Ok(d.0)
}

/// Everything a [`converse`] tape recorded, read straight off its records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverseValues {
    /// The opening monotonic read.
    pub mono0: u64,
    /// The wall-clock read.
    pub clock: u64,
    /// The random word.
    pub rand: u64,
    /// The file's bytes.
    pub file: Vec<u8>,
    /// The request the program sent through [`PEER`], as taped.
    pub request: Vec<u8>,
    /// The reply the peer returned, as taped.
    pub reply: Vec<u8>,
    /// What the program printed through [`OUT`], as taped.
    pub said: Vec<u8>,
    /// The read of [`STOP`] that first answered true, if one did (`0` is the
    /// first read) — the taped `Raised` record's false-read count.
    pub raised_at: Option<u64>,
    /// The closing monotonic read.
    pub mono1: u64,
}

/// Read a [`converse`] tape's values. `None` unless the tape is exactly
/// `[Mono, Clock, Rand, File, Exchange(peer), Emit(out), (Raised(stop))?, Mono]`
/// with a `Raised` count inside the polls the program makes.
pub fn converse_values(log: &EffectsLog) -> Option<ConverseValues> {
    use EffectRecord as R;
    let (mono0, clock, rand, file, peer, request, reply, out, said, raised_at, mono1) = match log
        .0
        .as_slice()
    {
        [R::Mono(mono0), R::Clock(clock), R::Rand(rand), R::File { bytes: file, .. }, R::Exchange {
            door: peer,
            request,
            reply,
        }, R::Emit {
            door: out,
            bytes: said,
        }, R::Raised { door, false_reads }, R::Mono(mono1)]
            if door == STOP && demo_poll_in_range(*false_reads, STOP_READS as u64) =>
        {
            (
                mono0,
                clock,
                rand,
                file,
                peer,
                request,
                reply,
                out,
                said,
                Some(*false_reads),
                mono1,
            )
        }
        [R::Mono(mono0), R::Clock(clock), R::Rand(rand), R::File { bytes: file, .. }, R::Exchange {
            door: peer,
            request,
            reply,
        }, R::Emit {
            door: out,
            bytes: said,
        }, R::Mono(mono1)] => (
            mono0, clock, rand, file, peer, request, reply, out, said, None, mono1,
        ),
        _ => return None,
    };
    (peer == PEER && out == OUT).then(|| ConverseValues {
        mono0: *mono0,
        clock: *clock,
        rand: *rand,
        file: file.clone(),
        request: request.clone(),
        reply: reply.clone(),
        said: said.clone(),
        raised_at,
        mono1: *mono1,
    })
}

/// A GENUINELY INDEPENDENT recomputation of [`converse`]'s digest from the
/// values its tape holds. It does not call [`ask`] or [`say`] and does not run
/// the program's fold: it lays every word out in one flat list and folds that,
/// and it derives the four flag answers from the `Raised` count, not from
/// replaying the flag door. So `converse(replay) == converse_oracle(tape)`
/// says the replayed program asked exactly the taped request, printed exactly
/// the taped output and saw the flag rise at exactly the taped read — through
/// two different pieces of code.
pub fn converse_oracle(v: &ConverseValues) -> u64 {
    // Bits raised_at..STOP_READS: every read from the first true one on.
    let all = (1u64 << STOP_READS) - 1;
    let stop_bits = v.raised_at.map_or(0, |n| {
        #[expect(
            clippy::manual_unwrap_or,
            reason = "a812042 spelled `unwrap_or` as a match for the verified lane; keep its form"
        )]
        let shift = match u32::try_from(n) {
            Ok(k) => k,
            Err(_) => u32::MAX,
        };
        all.checked_shl(shift).unwrap_or(0) & all
    });
    let mut words = vec![v.mono0, v.clock, v.rand];
    for s in [&v.file, &v.request, &v.reply, &v.said] {
        words.push(s.len() as u64);
        for &b in s {
            words.push(b as u64);
        }
    }
    words.push(stop_bits);
    words.push(v.mono1);
    words
        .iter()
        .fold(OFFSET, |d, &w| d.wrapping_mul(P).wrapping_add(w))
}
