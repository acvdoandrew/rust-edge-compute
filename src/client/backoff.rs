use rand::Rng;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ExponentialBackoff {
    base: Duration,
    cap: Duration,
    jitter_ratio: f64,
    attempt: u32,
}

impl ExponentialBackoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self::with_jitter(base, cap, 0.20)
    }

    pub fn with_jitter(base: Duration, cap: Duration, jitter_ratio: f64) -> Self {
        let normalized_base = if base.is_zero() {
            Duration::from_millis(1)
        } else {
            base
        };

        let normalized_cap = if cap < normalized_base {
            normalized_base
        } else {
            cap
        };

        Self {
            base: normalized_base,
            cap: normalized_cap,
            jitter_ratio: jitter_ratio.max(0.0),
            attempt: 0,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let current = self.current_delay_without_jitter();
        self.attempt = self.attempt.saturating_add(1);

        if self.jitter_ratio <= f64::EPSILON {
            return current;
        }

        let min = (1.0 - self.jitter_ratio).max(0.0);
        let max = 1.0 + self.jitter_ratio;
        let multiplier = rand::thread_rng().gen_range(min..=max);

        scale_duration(current, multiplier)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn current_delay_without_jitter(&self) -> Duration {
        let base_ms = self.base.as_millis();
        let cap_ms = self.cap.as_millis();

        let growth_pow = self.attempt.min(63);
        let growth_factor = 1u128 << growth_pow;

        let scaled_ms = base_ms.saturating_mul(growth_factor);
        let bounded_ms = scaled_ms.min(cap_ms);

        Duration::from_millis(bounded_ms as u64)
    }
}

fn scale_duration(duration: Duration, multiplier: f64) -> Duration {
    let scaled_ms = (duration.as_secs_f64() * 1000.0 * multiplier).round();
    let millis = scaled_ms.max(1.0).min(u64::MAX as f64) as u64;
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_until_cap() {
        let mut backoff =
            ExponentialBackoff::with_jitter(Duration::from_secs(1), Duration::from_secs(8), 0.0);

        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
    }

    #[test]
    fn backoff_cap_is_hard_upper_bound() {
        let mut backoff = ExponentialBackoff::with_jitter(
            Duration::from_millis(250),
            Duration::from_millis(700),
            0.0,
        );

        assert_eq!(backoff.next_delay(), Duration::from_millis(250));
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
        assert_eq!(backoff.next_delay(), Duration::from_millis(700));
        assert_eq!(backoff.next_delay(), Duration::from_millis(700));
    }

    #[test]
    fn backoff_reset_restores_initial_delay() {
        let mut backoff =
            ExponentialBackoff::with_jitter(Duration::from_secs(2), Duration::from_secs(30), 0.0);

        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));

        backoff.reset();

        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
    }
}
