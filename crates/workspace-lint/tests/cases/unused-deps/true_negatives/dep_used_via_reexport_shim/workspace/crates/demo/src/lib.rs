use web_time::{Duration, Instant};

pub fn measure() -> Duration {
    let start = Instant::now();
    start.elapsed()
}
