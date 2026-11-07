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

/// The same backoff for a shell remedy: waits for the pull request's checks
/// (`$1`) from 15 s up to 2 min, for at most an hour, with one `gh pr checks`
/// per look. Exits 0 once every required check passed, 1 when one failed or
/// the hour ran out, saying which in a plain line.
pub const WAIT_FOR_CHECKS: &str = r#"wait=15; spent=0
while :; do
  out=$(gh pr checks "$1" --required 2>&1); code=$?
  [ $code -eq 0 ] && { printf '%s\n' "$out"; echo "Every required check passed."; break; }
  if [ $code -ne 8 ]; then
    printf '%s\n' "$out"
    echo
    echo "A required check failed, so #$1 was not merged. Its name is marked fail above."
    exit 1
  fi
  if [ $spent -ge 3600 ]; then
    printf '%s\n' "$out"
    echo "Still running after an hour; stopped waiting. Run this again to keep waiting."
    exit 1
  fi
  sleep $wait; spent=$((spent + wait))
  wait=$((wait + wait / 2)); [ $wait -gt 120 ] && wait=120
done"#;

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
