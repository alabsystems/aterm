// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_misc::log_rotation_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use aterm_spec::verify::validate_transition_tiered;

const LINE: u64 = 4;
const OVERRIDES: &[(&str, i64)] = &[("MaxLines", 14)];

fn budget() -> RotationBudget {
    RotationBudget {
        rotate_at: LINE,
        check_bytes: LINE,
        check_every: Duration::from_secs(30),
    }
}

fn no_note(_: &Path) -> String {
    String::new()
}

fn lines(path: &Path) -> i64 {
    std::fs::metadata(path).map_or(0, |m| (m.len() / LINE) as i64)
}

fn id_at(path: &Path) -> FileId {
    std::fs::metadata(path).ok().and_then(|m| file_id(&m))
}

struct Harness {
    model: Model,
    path: PathBuf,
    a: RotatingFile,
    b: RotatingFile,
    written: i64,
    aged: i64,
    er: i64,
    older: (FileId, i64),
    steps: usize,
}

impl Harness {
    fn handle(&self, w: &RotatingFile) -> i64 {
        if w.id.is_some() && w.id == id_at(&self.path) {
            0
        } else if w.id.is_some() && w.id == id_at(&rotated_path(&self.path)) {
            1
        } else {
            2
        }
    }

    fn project(&self) -> State {
        let due_by_bytes = |w: &RotatingFile| i64::from(w.since_check >= w.budget.check_bytes);
        let due_by_time =
            |w: &RotatingFile| i64::from(w.last_check.elapsed() >= w.budget.check_every);
        let mut s = self.model.init_state();
        s.insert("live", lines(&self.path));
        s.insert("old", lines(&rotated_path(&self.path)));
        s.insert("aged", self.aged);
        s.insert("written", self.written);
        s.insert("ga", self.handle(&self.a));
        s.insert("gb", self.handle(&self.b));
        s.insert("na", due_by_bytes(&self.a));
        s.insert("nb", due_by_bytes(&self.b));
        s.insert("ea", due_by_time(&self.a));
        s.insert("eb", due_by_time(&self.b));
        s.insert("er", self.er);
        s
    }

    fn step(&mut self, action: &'static str, run: impl FnOnce(&mut Self)) {
        let before = self.project();
        assert_eq!(
            self.a.check_due(),
            self.model.action_enabled("CheckA", &before),
            "A's look-due answer departs from the model at {before:?}"
        );
        assert_eq!(
            self.b.check_due(),
            self.model.action_enabled("CheckB", &before),
            "B's look-due answer departs from the model at {before:?}"
        );
        assert!(
            self.model.action_enabled(action, &before),
            "step {}: the script asks for {action}, which the model does not allow at {before:?}",
            self.steps
        );
        run(self);
        let older_now = id_at(&rotated_path(&self.path));
        if older_now != self.older.0 {
            self.aged += self.older.1;
            self.er = 0;
        }
        self.older = (older_now, lines(&rotated_path(&self.path)));
        let after = self.project();
        let label = format!("log rotation step {} ({action})", self.steps);
        let (conforms, evidence) = validate_transition_tiered(
            &self.model,
            OVERRIDES,
            &before,
            &after,
            Some(action),
            &label,
        );
        assert!(conforms, "{label}: {before:?} -> {after:?}\n{evidence}");
        for inv in &self.model.invariants {
            assert!(
                self.model.check_invariant(inv.name, &after),
                "{label}: violates {}: {after:?}",
                inv.name
            );
        }
        self.steps += 1;
    }

    fn write(&mut self, writer: char) {
        let (check, write) = if writer == 'A' {
            ("CheckA", "WriteA")
        } else {
            ("CheckB", "WriteB")
        };
        let due = if writer == 'A' {
            self.a.check_due()
        } else {
            self.b.check_due()
        };
        if due {
            self.step(check, |h| {
                if writer == 'A' {
                    h.a.check()
                } else {
                    h.b.check()
                }
            });
        }
        let line = format!("{writer}{:02}\n", self.written);
        assert_eq!(line.len() as u64, LINE);
        self.step(write, |h| {
            let w = if writer == 'A' { &mut h.a } else { &mut h.b };
            assert!(!w.check_due(), "the look was taken just above");
            w.append(line.as_bytes(), no_note);
            h.written += 1;
        });
    }

    fn tick(&mut self) {
        self.step("Tick", |h| {
            for w in [&mut h.a, &mut h.b] {
                w.last_check = w
                    .last_check
                    .checked_sub(w.budget.check_every)
                    .expect("the monotonic clock is older than 30 s");
            }
            h.er = 1;
        });
    }

    fn restart_b(&mut self) {
        self.step("RestartB", |h| {
            h.b = RotatingFile::open(h.path.clone(), budget()).unwrap();
        });
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-logconf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    crate::control_auth::ensure_private_dir(&dir).unwrap();
    dir
}

#[test]
fn two_real_writers_conform_through_rotation_follow_and_restart() {
    let dir = scratch("trace");
    let path = dir.join("aterm.log");
    let model = aterm_spec::interp::with_consts(&log_rotation_model(), OVERRIDES);
    let a = RotatingFile::open(path.clone(), budget()).unwrap();
    let b = RotatingFile::open(path.clone(), budget()).unwrap();
    let mut h = Harness {
        model,
        path: path.clone(),
        a,
        b,
        written: 0,
        aged: 0,
        er: 1,
        older: (None, 0),
        steps: 0,
    };
    h.write('B');
    h.write('B');
    h.write('A');
    h.write('B');
    assert_eq!(h.project()["ga"], 1);
    h.tick();
    h.write('A');
    h.restart_b();
    assert_eq!(h.aged, 3, "the first older copy was retired");
    h.tick();
    h.write('B');
    h.restart_b();
    h.write('A');
    h.write('A');
    assert_eq!(h.project()["gb"], 1);
    h.write('B');
    assert_eq!(h.project()["old"], 3, "a late line joins the older copy");
    h.tick();
    h.write('B');
    h.write('A');
    assert_eq!(h.project()["gb"], 1);
    h.tick();
    h.write('A');
    let aged = h.aged;
    h.write('B');
    assert_eq!((h.project()["gb"], h.project()["ga"]), (0, 1));
    assert!(h.aged > aged, "the look that followed also rotated");
    assert_eq!(h.written, 13);
    let s = h.project();
    assert_eq!(s["live"] + s["old"] + s["aged"], 13, "{s:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_truncating_start_this_replaced_is_refused() {
    let dir = scratch("neg");
    let path = dir.join("aterm.log");
    let model = aterm_spec::interp::with_consts(&log_rotation_model(), OVERRIDES);
    let a = RotatingFile::open(path.clone(), budget()).unwrap();
    let b = RotatingFile::open(path.clone(), budget()).unwrap();
    let mut h = Harness {
        model,
        path: path.clone(),
        a,
        b,
        written: 0,
        aged: 0,
        er: 1,
        older: (None, 0),
        steps: 0,
    };
    h.write('A');
    h.write('B');
    let before = h.project();
    assert!(before["live"] > 1, "the file is over the cap: {before:?}");
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap(),
    );
    h.b = RotatingFile::open(path.clone(), budget()).unwrap();
    let after = h.project();
    let (conforms, _) = validate_transition_tiered(
        &h.model,
        OVERRIDES,
        &before,
        &after,
        Some("RestartB"),
        "log rotation negative control",
    );
    assert!(
        !conforms,
        "a truncating start conformed: {before:?} -> {after:?}"
    );
    assert!(
        !h.model.check_invariant("EveryLineOnce", &after),
        "{after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
