//! The time of one tick of `makibad`'s loop (Issue 624). The loop runs each of its passes and the
//! heartbeat through `Clock::time`; a tick of `SLOW_TICK` or longer is said on standard error
//! (`makibad.log`) in one line naming its slowest pass and recorded as one `tick_slow` event with
//! the total and the passes of `SLOW_PASS` or longer. A quicker tick leaves nothing.
//!
//! The clock lives for one tick and keeps nothing across ticks or a restart: a tick that never
//! ends is never said here (the outside watch of Issue 563 sees it).

use crate::db::writer::{NewEvent, StateWriter};
use serde_json::json;
use std::time::{Duration, Instant};

/// A tick at least this long is said and recorded.
pub const SLOW_TICK: Duration = Duration::from_secs(5);
/// A pass at least this long is named in the `tick_slow` event.
pub const SLOW_PASS: Duration = Duration::from_secs(1);

pub struct Clock {
    start: Instant,
    passes: Vec<(&'static str, Duration)>,
}

impl Clock {
    pub fn start() -> Clock {
        Clock {
            start: Instant::now(),
            passes: Vec::new(),
        }
    }

    /// Run the pass `name` and keep its time; a test's hold before it (`pass:<name>`) is counted.
    pub fn time<T>(&mut self, name: &'static str, pass: impl FnOnce() -> T) -> T {
        let at = Instant::now();
        super::crash::hold_pass(name);
        let out = pass();
        self.passes.push((name, at.elapsed()));
        out
    }

    /// End the tick: a slow one is said and recorded once; a refused record is said, not retried.
    pub fn finish(self, w: &StateWriter) {
        let total = self.start.elapsed();
        if total < SLOW_TICK {
            return;
        }
        let slowest = self
            .passes
            .iter()
            .copied()
            .max_by_key(|(_, d)| *d)
            .unwrap_or(("none", Duration::ZERO));
        eprintln!(
            "makibad: a tick took {:.1} s; slowest: {} {:.1} s",
            total.as_secs_f64(),
            slowest.0,
            slowest.1.as_secs_f64()
        );
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let passes: Vec<_> = self
            .passes
            .iter()
            .filter(|(_, d)| *d >= SLOW_PASS)
            .map(|(p, d)| json!({"pass": p, "ms": ms(*d)}))
            .collect();
        let payload = json!({"total_ms": ms(total),
                             "slowest": {"pass": slowest.0, "ms": ms(slowest.1)},
                             "passes": passes});
        if let Err(e) = w.append_event(&NewEvent {
            kind: "tick_slow",
            actor: Some("makibad"),
            payload: Some(&payload),
            ..Default::default()
        }) {
            eprintln!("makibad: cannot record the slow tick: {e}");
        }
    }
}
