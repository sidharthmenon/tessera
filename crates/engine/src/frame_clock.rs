use std::time::{Duration, Instant};

/// Vsync-locked time for the render loop (R19). A wall clock sampled once per loop iteration lands
/// on frame boundaries with jitter, which aliases 60p content on a 60 Hz output into bursts of
/// dropped + repeated frames; counting presented vsyncs does not.
pub struct FrameClock {
    period: Duration,
    anchor: Instant,
    n: u32,
}

impl FrameClock {
    pub fn new(period: Duration, now: Instant) -> FrameClock {
        FrameClock { period, anchor: now, n: 0 }
    }

    pub fn period(&self) -> Duration {
        self.period
    }

    /// Frame time for this loop iteration. `paced`: the previous iteration presented on a Fifo
    /// surface (exactly one vsync later), so time advances one period. Otherwise, or when that
    /// drifts more than a period from `real` (missed vsyncs, refresh not as nominal), re-anchor.
    // ponytail: nominal period + hard re-anchor (one skipped/repeated frame per re-anchor); slew the
    // anchor toward `real` (a small PLL) if a display's real rate drifts enough to re-anchor often.
    pub fn tick(&mut self, real: Instant, paced: bool) -> Instant {
        self.n = if paced { self.n.saturating_add(1) } else { 0 };
        let t = self.anchor + self.period * self.n;
        if !paced || t.max(real) - t.min(real) > self.period {
            (self.anchor, self.n) = (real, 0);
            return real;
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_to_period_and_reanchors_on_drift() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut clock = FrameClock::new(ms(10), t0);
        assert_eq!(clock.tick(t0 + ms(3), false), t0 + ms(3), "unpaced: real time");
        // Paced: one period per iteration whatever the sampling jitter.
        assert_eq!(clock.tick(t0 + ms(16), true), t0 + ms(13));
        assert_eq!(clock.tick(t0 + ms(21), true), t0 + ms(23));
        assert_eq!(clock.tick(t0 + ms(33), true), t0 + ms(33));
        // Two vsyncs missed: 20 ms behind real, more than a period → re-anchor, then lock again.
        assert_eq!(clock.tick(t0 + ms(63), true), t0 + ms(63));
        assert_eq!(clock.tick(t0 + ms(74), true), t0 + ms(73));
        // Real time running slow by more than a period also re-anchors.
        assert_eq!(clock.tick(t0 + ms(70), true), t0 + ms(70));
    }
}
