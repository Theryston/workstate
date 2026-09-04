use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
    fn monotonic_now(&self) -> Instant;

    fn elapsed_since(&self, start: Instant) -> Duration {
        self.monotonic_now().saturating_duration_since(start)
    }
}
