use std::time::{Instant, SystemTime};

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
    fn monotonic_now(&self) -> Instant;
}
