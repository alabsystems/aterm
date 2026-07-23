// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OFFLINE trainer for the tone-of-typing classifier
//! (`aterm_effects::tone`). Run MANUALLY when the corpus or architecture
//! changes — never part of any build:
//!
//! ```sh
//! cargo run -p aterm-effects --example tone_train --release
//! ```
//!
//! Reads `data/tone_corpus.tsv`, trains the fastText-style bag model with
//! plain SGD (deterministic seed — the same corpus always yields the same
//! bytes), quantizes through the SAME `encode_model`/`from_bytes` pair the
//! runtime uses, audits the QUANTIZED model against the held-out
//! expectations file, and only then overwrites `data/tone_model.atn`.
//! Floors here are stricter than the conformance tests' (train ≥ 0.95,
//! holdout ≥ 0.78 overall, ≥ 0.70 per focus language vs the tests' 0.70 /
//! 0.60) so the committed asset always carries margin over the pins.
//!
//! Featurization is `tone::for_each_ngram_bucket` — the exact runtime
//! function — so trained weights can never drift from the runtime's view of
//! the text.

use aterm_effects::tone::{self, BUCKETS, EMBED, HIDDEN, TONE_COUNT, Tone, ToneModel, ToneScratch};

const EMB_LEN: usize = BUCKETS * EMBED;
const W1_LEN: usize = EMBED * HIDDEN;
const W2_LEN: usize = HIDDEN * TONE_COUNT;

type LanguageAccuracy = std::collections::BTreeMap<String, (usize, usize)>;
type ConfusionMatrix = [[usize; TONE_COUNT]; TONE_COUNT];
type AccuracyReport = (f32, LanguageAccuracy, ConfusionMatrix);

const EPOCHS: usize = 300;
const LR0: f32 = 0.15;
const LR1: f32 = 0.01;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64* — deterministic, dependency-free.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

struct Example {
    label: usize,
    buckets: Vec<usize>,
}

fn load(path: &std::path::Path) -> Vec<Example> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let Some((tone, _lang, body)) = tone::parse_corpus_line(line) else {
            if !line.trim().is_empty() && !line.starts_with('#') {
                panic!(
                    "{}:{}: malformed corpus line {line:?}",
                    path.display(),
                    ln + 1
                );
            }
            continue;
        };
        let mut buckets = Vec::new();
        tone::for_each_ngram_bucket(body, |b| buckets.push(b));
        assert!(
            buckets.len() >= tone::MIN_NGRAMS,
            "{}:{}: line too short to ever classify: {body:?}",
            path.display(),
            ln + 1
        );
        out.push(Example {
            label: tone.index(),
            buckets,
        });
    }
    out
}

struct Net {
    emb: Vec<f32>,
    w1: Vec<f32>,
    b1: Vec<f32>,
    w2: Vec<f32>,
    b2: Vec<f32>,
}

impl Net {
    fn init(rng: &mut Rng) -> Net {
        Net {
            emb: (0..EMB_LEN).map(|_| rng.range(-0.08, 0.08)).collect(),
            w1: (0..W1_LEN).map(|_| rng.range(-0.30, 0.30)).collect(),
            b1: vec![0.0; HIDDEN],
            w2: (0..W2_LEN).map(|_| rng.range(-0.30, 0.30)).collect(),
            b2: vec![0.0; TONE_COUNT],
        }
    }

    /// Forward pass; returns (pooled embedding, hidden, softmax probs).
    fn forward(&self, buckets: &[usize]) -> ([f32; EMBED], [f32; HIDDEN], [f32; TONE_COUNT]) {
        let mut e = [0.0f32; EMBED];
        for &b in buckets {
            let row = &self.emb[b * EMBED..(b + 1) * EMBED];
            for (acc, &w) in e.iter_mut().zip(row) {
                *acc += w;
            }
        }
        let inv = 1.0 / buckets.len() as f32;
        for x in &mut e {
            *x *= inv;
        }
        let mut h = [0.0f32; HIDDEN];
        for (j, hj) in h.iter_mut().enumerate() {
            let mut acc = self.b1[j];
            for (i, &ei) in e.iter().enumerate() {
                acc += ei * self.w1[i * HIDDEN + j];
            }
            *hj = acc.tanh();
        }
        let mut logits = [0.0f32; TONE_COUNT];
        for (c, l) in logits.iter_mut().enumerate() {
            let mut acc = self.b2[c];
            for (j, &hj) in h.iter().enumerate() {
                acc += hj * self.w2[j * TONE_COUNT + c];
            }
            *l = acc;
        }
        let mx = logits.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let mut p = [0.0f32; TONE_COUNT];
        let mut z = 0.0;
        for (pc, &l) in p.iter_mut().zip(&logits) {
            *pc = (l - mx).exp();
            z += *pc;
        }
        for pc in &mut p {
            *pc /= z;
        }
        (e, h, p)
    }

    /// One SGD step of softmax cross-entropy on one example.
    fn step(&mut self, ex: &Example, lr: f32) {
        let (e, h, p) = self.forward(&ex.buckets);
        // dL/dlogit = p - onehot.
        let mut g = p;
        g[ex.label] -= 1.0;
        // Hidden gradient through w2.
        let mut dh = [0.0f32; HIDDEN];
        for j in 0..HIDDEN {
            let mut acc = 0.0;
            for (c, &gc) in g.iter().enumerate() {
                acc += self.w2[j * TONE_COUNT + c] * gc;
            }
            dh[j] = acc * (1.0 - h[j] * h[j]); // tanh'
        }
        // Output layer update.
        for (j, &hj) in h.iter().enumerate() {
            for (c, &gc) in g.iter().enumerate() {
                self.w2[j * TONE_COUNT + c] -= lr * hj * gc;
            }
        }
        for (c, &gc) in g.iter().enumerate() {
            self.b2[c] -= lr * gc;
        }
        // Pooled-embedding gradient through w1 (compute BEFORE updating w1).
        let mut de = [0.0f32; EMBED];
        for (i, dei) in de.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (j, &dhj) in dh.iter().enumerate() {
                acc += self.w1[i * HIDDEN + j] * dhj;
            }
            *dei = acc;
        }
        for (i, &ei) in e.iter().enumerate() {
            for (j, &dhj) in dh.iter().enumerate() {
                self.w1[i * HIDDEN + j] -= lr * ei * dhj;
            }
        }
        for (j, &dhj) in dh.iter().enumerate() {
            self.b1[j] -= lr * dhj;
        }
        // Embedding bag update: mean pool distributes 1/n to each occurrence.
        let inv = 1.0 / ex.buckets.len() as f32;
        for &b in &ex.buckets {
            let row = &mut self.emb[b * EMBED..(b + 1) * EMBED];
            for (w, &dei) in row.iter_mut().zip(&de) {
                *w -= lr * inv * dei;
            }
        }
    }
}

fn accuracy(model: &ToneModel, set: &[(Tone, String, String)]) -> AccuracyReport {
    let mut scratch = ToneScratch::default();
    let mut ok = 0usize;
    let mut by_lang: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    let mut confusion = [[0usize; TONE_COUNT]; TONE_COUNT];
    for (want, lang, text) in set {
        let got = model.classify(text, &mut scratch);
        confusion[want.index()][got.index()] += 1;
        let e = by_lang.entry(lang.clone()).or_default();
        e.1 += 1;
        if got == *want {
            ok += 1;
            e.0 += 1;
        }
    }
    (ok as f32 / set.len() as f32, by_lang, confusion)
}

fn read_labeled(path: &std::path::Path) -> Vec<(Tone, String, String)> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter_map(|l| {
            tone::parse_corpus_line(l)
                .map(|(t, lang, body)| (t, lang.to_string(), body.to_string()))
        })
        .collect()
}

fn main() {
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    let train = load(&data.join("tone_corpus.tsv"));
    let holdout = read_labeled(&data.join("tone_holdout.tsv"));
    let train_labeled = read_labeled(&data.join("tone_corpus.tsv"));
    println!(
        "train lines: {}  holdout lines: {}",
        train.len(),
        holdout.len()
    );
    assert!(train.len() >= 250, "corpus shrank suspiciously");
    assert!(holdout.len() >= 60, "holdout shrank suspiciously");

    let mut rng = Rng(0x5EED_701E_D00D_F00D);
    let mut net = Net::init(&mut rng);
    let mut order: Vec<usize> = (0..train.len()).collect();
    for epoch in 0..EPOCHS {
        let lr = LR0 + (LR1 - LR0) * epoch as f32 / (EPOCHS - 1) as f32;
        // Fisher–Yates with the deterministic rng.
        for i in (1..order.len()).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        for &k in &order {
            net.step(&train[k], lr);
        }
        if epoch % 50 == 49 {
            let bytes = tone::encode_model(&net.emb, &net.w1, &net.b1, &net.w2, &net.b2);
            let m = ToneModel::from_bytes(&bytes).expect("round trip");
            let (tr, _, _) = accuracy(&m, &train_labeled);
            let (ho, _, _) = accuracy(&m, &holdout);
            println!(
                "epoch {:3}  quantized train {tr:.3}  holdout {ho:.3}",
                epoch + 1
            );
        }
    }

    // Quantize through the runtime's exact encode/decode pair, then audit
    // THAT model — what ships is what was measured.
    let bytes = tone::encode_model(&net.emb, &net.w1, &net.b1, &net.w2, &net.b2);
    let model = ToneModel::from_bytes(&bytes).expect("round trip");
    let (train_acc, _, _) = accuracy(&model, &train_labeled);
    let (holdout_acc, by_lang, confusion) = accuracy(&model, &holdout);
    println!("\nQUANTIZED train accuracy   {train_acc:.3}");
    println!("QUANTIZED holdout accuracy {holdout_acc:.3}");
    println!("holdout by language:");
    for (lang, (ok, n)) in &by_lang {
        println!("  {lang:5} {ok:2}/{n:2}");
    }
    println!(
        "holdout confusion (rows = truth, cols = predicted, order {:?}):",
        Tone::ALL.map(Tone::label)
    );
    for (r, row) in confusion.iter().enumerate() {
        println!("  {:10} {row:?}", Tone::from_index(r).label());
    }

    // Refuse to write an asset that would not carry margin over the pins.
    assert!(
        train_acc >= 0.95,
        "train floor missed: {train_acc:.3} < 0.95"
    );
    assert!(
        holdout_acc >= 0.78,
        "holdout floor missed: {holdout_acc:.3} < 0.78"
    );
    for lang in ["zh", "ja", "ko", "en", "es"] {
        let &(ok, n) = by_lang.get(lang).unwrap_or(&(0, 0));
        assert!(n > 0, "holdout lost {lang}");
        let acc = ok as f32 / n as f32;
        assert!(
            acc >= 0.70,
            "{lang} floor missed: {ok}/{n} = {acc:.3} < 0.70"
        );
    }

    let out = data.join("tone_model.atn");
    std::fs::write(&out, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    println!("\nwrote {} ({} bytes)", out.display(), bytes.len());
}
