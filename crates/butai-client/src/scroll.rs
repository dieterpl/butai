//! Scrolling that speeds up the longer you keep at it, and a stage that waits
//! for the cursor to stop before it loads what is under it.
//!
//! Two separate problems with one cause: a list long enough to want speed is
//! also one long enough that "load each row as the cursor passes it" costs
//! something.
//!
//! - [`Momentum`] turns a run of same-direction steps into bigger steps. A
//!   terminal reports a held key as the same key press over and over (there is
//!   no release event without the kitty protocol, which this client does not
//!   ask for), and a fast wheel spin as notches arriving close together. So the
//!   only honest signal is *time*: steps closer together than the curve's
//!   `gap` belong to one run, and the run's age picks the step size. A pause
//!   or a change of direction starts over at the base step.
//! - [`Settle`] holds back a change of target until it has stopped changing.
//!   The first change after a quiet spell goes through at once, so a single
//!   `j` is as quick as it ever was; a run of them only loads where it ends.
//!
//! Every function takes `now` rather than reading the clock, so the tests below
//! can walk a curve in milliseconds without sleeping through any of them.

use std::time::{Duration, Instant};

/// How a run of steps turns into step sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Curve {
    /// The step for a lone event, and for every event until the run is
    /// `ramp_after` old.
    pub base: isize,
    /// Two events further apart than this are two runs, not one.
    pub gap: Duration,
    /// How long a run stays at `base` before it starts to speed up. This is
    /// what keeps short bursts precise: three quick taps of `j` move three rows.
    pub ramp_after: Duration,
    /// After `ramp_after`, the step grows by `base` every this long…
    pub ramp_every: Duration,
    /// …up to this, which is the fastest it goes.
    pub max: isize,
}

/// A held `j`/`k` or arrow key.
///
/// The gap is wider than every common auto-repeat interval (X's default is
/// 40ms, macOS's slowest repeat setting about 90ms) and narrower than the
/// initial repeat delay (250–600ms), so the first repeat begins the run and
/// deliberate tapping, which rarely beats 8 a second, mostly stays out of it.
/// At X's 25 repeats a second it is one row a step for the first quarter
/// second, then one more every 120ms, reaching eight a step a little over a
/// second in.
pub const KEY_CURVE: Curve = Curve {
    base: 1,
    gap: Duration::from_millis(120),
    ramp_after: Duration::from_millis(250),
    ramp_every: Duration::from_millis(120),
    max: 8,
};

/// The wheel over a list whose *cursor* it moves.
///
/// One notch is one row, because a rail of five agents has to be landable on
/// row two. A spin — notches under 80ms apart, which no one manages one at a
/// time — ramps fast, to six rows a notch within about a third of a second.
pub const WHEEL_CURVE: Curve = Curve {
    base: 1,
    gap: Duration::from_millis(80),
    ramp_after: Duration::from_millis(60),
    ramp_every: Duration::from_millis(50),
    max: 6,
};

/// Lines one wheel notch scrolls *text* by — a help page, a diff, an open file.
///
/// Was one, which made reading anything long with the wheel a chore: the
/// convention nearly every other reader follows is three, and the wheel's
/// momentum multiplies it from there.
pub const WHEEL_LINES: isize = 3;

/// How long BOOTH's fleet cursor must rest before the stage loads the agent
/// under it. `[ui] stage_settle_ms` overrides it.
///
/// Long enough to cover the gap between two auto-repeats with room to spare,
/// short enough that the preview of the row you stopped on still feels like it
/// arrived when you stopped.
pub const STAGE_SETTLE: Duration = Duration::from_millis(120);

impl Curve {
    /// The step size for a run that has been going for `held`.
    pub fn at(&self, held: Duration) -> isize {
        if held < self.ramp_after || self.ramp_every.is_zero() {
            return self.base;
        }
        let over = (held - self.ramp_after).as_millis() / self.ramp_every.as_millis();
        // Two at `ramp_after` itself: the ramp starts by doubling, not by
        // staying put for one more interval.
        let steps = 2 + over.min(self.max as u128) as isize;
        (self.base * steps).min(self.max).max(self.base)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    /// `-1` or `1`.
    dir: isize,
    start: Instant,
    last: Instant,
}

/// The run a list's scrolling is currently in, if any. One per input device —
/// a wheel spin and a held key are different gestures and do not share speed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Momentum {
    run: Option<Run>,
}

impl Momentum {
    /// One event towards `dir` (its sign is all that is read) at `now`; the
    /// signed distance it should move.
    pub fn step(&mut self, curve: &Curve, dir: isize, now: Instant) -> isize {
        let dir = dir.signum();
        if dir == 0 {
            return 0;
        }
        let run = match self.run {
            Some(r) if r.dir == dir && now.saturating_duration_since(r.last) <= curve.gap => {
                Run { last: now, ..r }
            }
            _ => Run { dir, start: now, last: now },
        };
        self.run = Some(run);
        dir * curve.at(now.saturating_duration_since(run.start))
    }

    /// Forget the run, so the next event starts at the base step whatever its
    /// timing.
    pub fn reset(&mut self) {
        self.run = None;
    }
}

/// A target that only moves once it has stopped moving.
///
/// `shown` is what is on screen; `offer` is told what *should* be, every
/// repaint, and decides whether the two may differ for now.
#[derive(Debug, Clone)]
pub struct Settle<T> {
    shown: T,
    /// A wanted target not yet shown, and since when it has been wanted.
    pending: Option<(T, Instant)>,
    /// When the wanted target last changed. A change this long after the last
    /// one is a fresh gesture and goes straight through.
    changed: Option<Instant>,
    delay: Duration,
}

impl<T: Clone + PartialEq> Settle<T> {
    pub fn new(shown: T, delay: Duration) -> Self {
        Self { shown, pending: None, changed: None, delay }
    }

    /// What to show at `now`, given that `want` is wanted.
    ///
    /// `now_please` skips the wait — for a target that something is about to
    /// *use* rather than look at, such as the pane the keyboard was just handed
    /// to. Typing into the preview of the row you left is the one failure this
    /// type could introduce, and that is what this rules out.
    pub fn offer(&mut self, want: T, now: Instant, now_please: bool) -> T {
        if want == self.shown {
            // Back where it started, or never left: nothing is waiting.
            if self.pending.take().is_some() {
                self.changed = Some(now);
            }
            return self.shown.clone();
        }
        let since = match &self.pending {
            Some((p, since)) if *p == want => *since,
            _ => {
                // A different target from the one waiting: the clock restarts,
                // unless this is the first change in a while, which goes
                // through as it always did.
                let quiet =
                    self.changed.is_none_or(|t| now.saturating_duration_since(t) >= self.delay);
                self.changed = Some(now);
                if quiet || now_please {
                    return self.show(want);
                }
                self.pending = Some((want.clone(), now));
                now
            }
        };
        if now_please || now.saturating_duration_since(since) >= self.delay {
            return self.show(want);
        }
        self.shown.clone()
    }

    /// When the target that is waiting will be due, so the loop knows to wake
    /// for it. `None` when nothing is waiting.
    pub fn due(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, since)| *since + self.delay)
    }

    fn show(&mut self, want: T) -> T {
        self.pending = None;
        self.shown = want.clone();
        want
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_curve_holds_base_then_ramps_to_its_cap() {
        let c = KEY_CURVE;
        assert_eq!(c.at(ms(0)), 1);
        assert_eq!(c.at(ms(249)), 1, "a quarter second of repeat is still one row");
        assert_eq!(c.at(ms(250)), 2);
        assert_eq!(c.at(ms(369)), 2);
        assert_eq!(c.at(ms(370)), 3);
        assert_eq!(c.at(ms(250 + 6 * 120)), 8);
        assert_eq!(c.at(ms(60_000)), 8, "and it never exceeds the cap");
        // Monotone: holding longer never moves slower.
        let mut last = 0;
        for t in (0..3000).step_by(7) {
            let s = c.at(ms(t));
            assert!(s >= last, "step fell from {last} to {s} at {t}ms");
            last = s;
        }
    }

    #[test]
    fn the_wheel_curve_ramps_faster_and_caps_lower() {
        let c = WHEEL_CURVE;
        assert_eq!(c.at(ms(0)), 1);
        assert_eq!(c.at(ms(60)), 2);
        assert_eq!(c.at(ms(60 + 4 * 50)), 6);
        assert_eq!(c.at(ms(5_000)), 6);
    }

    /// A held key at X's 25Hz, after the initial repeat delay.
    #[test]
    fn a_held_key_accelerates_and_a_pause_starts_over() {
        let t0 = Instant::now();
        let mut m = Momentum::default();
        // The press, then the first repeat 500ms later: two separate runs.
        assert_eq!(m.step(&KEY_CURVE, 1, t0), 1);
        let first = t0 + ms(500);
        let mut moved = Vec::new();
        for i in 0..40u64 {
            moved.push(m.step(&KEY_CURVE, 1, first + ms(40 * i)));
        }
        assert_eq!(&moved[..7], &[1; 7], "the first quarter second is row by row");
        assert!(moved.windows(2).all(|w| w[1] >= w[0]), "{moved:?}");
        assert_eq!(*moved.last().unwrap(), 8, "{moved:?}");
        // Let go for longer than the gap: the next press is one row again.
        let after = first + ms(40 * 39) + KEY_CURVE.gap + ms(1);
        assert_eq!(m.step(&KEY_CURVE, 1, after), 1);
    }

    #[test]
    fn turning_round_starts_a_new_run() {
        let t0 = Instant::now();
        let mut m = Momentum::default();
        for i in 0..30u64 {
            m.step(&KEY_CURVE, 1, t0 + ms(40 * i));
        }
        assert!(m.step(&KEY_CURVE, 1, t0 + ms(40 * 30)) > 1);
        assert_eq!(m.step(&KEY_CURVE, -1, t0 + ms(40 * 31)), -1, "the reverse is precise");
        m.reset();
        assert_eq!(m.step(&KEY_CURVE, -1, t0 + ms(40 * 32)), -1);
        assert_eq!(m.step(&KEY_CURVE, 0, t0 + ms(40 * 33)), 0);
    }

    #[test]
    fn slow_wheel_notches_move_one_row_each() {
        let t0 = Instant::now();
        let mut m = Momentum::default();
        for i in 0..10u64 {
            assert_eq!(m.step(&WHEEL_CURVE, -1, t0 + ms(150 * i)), -1);
        }
    }

    #[test]
    fn a_wheel_spin_goes_further_per_notch() {
        let t0 = Instant::now();
        let mut m = Momentum::default();
        let total: isize = (0..20u64).map(|i| m.step(&WHEEL_CURVE, 1, t0 + ms(20 * i))).sum();
        assert!(total > 60, "twenty quick notches went only {total} rows");
        assert_eq!(m.step(&WHEEL_CURVE, 1, t0 + ms(20 * 19 + 20)), 6);
    }

    fn settle() -> Settle<u32> {
        Settle::new(0, STAGE_SETTLE)
    }

    #[test]
    fn a_lone_change_goes_straight_through() {
        let t0 = Instant::now();
        let mut s = settle();
        assert_eq!(s.offer(1, t0, false), 1);
        assert_eq!(s.due(), None);
        // And so does the next one, once the first is long settled.
        assert_eq!(s.offer(2, t0 + ms(500), false), 2);
    }

    /// The reported case: holding `j` down BOOTH's fleet loaded every agent it
    /// passed. Now it loads the first and the last.
    #[test]
    fn a_fast_run_loads_only_where_it_starts_and_stops() {
        let t0 = Instant::now();
        let mut s = settle();
        let mut loaded = vec![];
        let mut shown = 0;
        for row in 1..=20u32 {
            let now = t0 + ms(40 * row as u64);
            let got = s.offer(row, now, false);
            if got != shown {
                loaded.push(got);
                shown = got;
            }
            // Every repaint in between re-offers the same row.
            assert_eq!(s.offer(row, now + ms(10), false), shown);
        }
        assert_eq!(loaded, vec![1], "the run loaded {loaded:?} on the way");
        let stopped = t0 + ms(40 * 20);
        assert_eq!(s.due(), Some(stopped + STAGE_SETTLE));
        assert_eq!(s.offer(20, stopped + STAGE_SETTLE - ms(1), false), 1, "not yet");
        assert_eq!(s.offer(20, stopped + STAGE_SETTLE, false), 20, "and now it has settled");
        assert_eq!(s.due(), None);
    }

    #[test]
    fn coming_back_to_what_is_shown_cancels_the_wait() {
        let t0 = Instant::now();
        let mut s = settle();
        s.offer(1, t0, false);
        assert_eq!(s.offer(2, t0 + ms(30), false), 1);
        assert!(s.due().is_some());
        assert_eq!(s.offer(1, t0 + ms(60), false), 1);
        assert_eq!(s.due(), None, "nothing left to load");
        // And that return counted as movement: the next change still waits.
        assert_eq!(s.offer(3, t0 + ms(90), false), 1);
    }

    #[test]
    fn now_please_skips_the_wait() {
        let t0 = Instant::now();
        let mut s = settle();
        s.offer(1, t0, false);
        assert_eq!(s.offer(2, t0 + ms(30), false), 1);
        assert_eq!(s.offer(2, t0 + ms(31), true), 2, "the keyboard went to the pane");
        assert_eq!(s.offer(3, t0 + ms(32), true), 3, "even mid-run");
    }
}
