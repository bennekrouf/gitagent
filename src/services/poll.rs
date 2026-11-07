//! How long to wait before asking GitHub again. Every look at a pull request's
//! checks or a workflow run is a call against the hourly API budget, and a
//! fixed short interval spent most of it on builds that take tens of minutes.
//! Waits start short, so a quick result is seen quickly, and stretch out.

use std::time::Duration;

/// A wait that grows by half each time, up to `max`.
#[derive(Clone, Debug, PartialEq)]
pub struct Backoff {
    next: Duration,
    max: Duration,
}

impl Backoff {
    pub fn new(first: Duration, max: Duration) -> Self {
        Self { next: first, max }
    }

    /// For a pull request's checks: from 15 s up to 2 min.
    pub fn checks() -> Self {
        Self::new(Duration::from_secs(15), Duration::from_secs(120))
    }

    /// For a release build: from 30 s up to 3 min.
    pub fn build() -> Self {
        Self::new(Duration::from_secs(30), Duration::from_secs(180))
    }

    /// The wait to use now, and lengthens the next one.
    pub fn step(&mut self) -> Duration {
        let now = self.next;
        self.next = (now + now / 2).min(self.max);
        now
    }

    pub async fn sleep(&mut self) {
        tokio::time::sleep(self.step()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_grow_by_half_and_stop_at_the_cap() {
        let mut b = Backoff::checks();
        let waits: Vec<u64> = (0..8).map(|_| b.step().as_secs()).collect();
        assert_eq!(waits[0], 15);
        assert!(waits.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(*waits.last().unwrap(), 120);
    }

    #[test]
    fn three_quarters_of_an_hour_of_checks_is_a_few_dozen_looks_not_ninety() {
        let mut b = Backoff::checks();
        let (mut spent, mut looks) = (0, 0);
        while spent < 45 * 60 {
            spent += b.step().as_secs();
            looks += 1;
        }
        assert!(looks <= 28, "{looks}");
    }
}
