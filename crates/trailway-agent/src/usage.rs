//! Aggregates heartbeats into one usage sample per interval.

use chrono::{DateTime, Utc};
use trailway_proto::{Heartbeat, Resource, UsageSample};

/// Collects the heartbeats of the current interval. Capacity `total` is the
/// latest seen, `used` the mean; KVM must have held for every heartbeat.
pub struct UsageWindow {
    start: DateTime<Utc>,
    count: u64,
    cpu_used_sum: u128,
    memory_used_sum: u128,
    cpu_total: u64,
    memory_total: u64,
    kvm: bool,
}

impl UsageWindow {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            start,
            count: 0,
            cpu_used_sum: 0,
            memory_used_sum: 0,
            cpu_total: 0,
            memory_total: 0,
            kvm: true,
        }
    }

    pub fn start(&self) -> DateTime<Utc> {
        self.start
    }

    pub fn push(&mut self, hb: &Heartbeat) {
        self.count += 1;
        self.cpu_used_sum += u128::from(hb.cpu.used);
        self.memory_used_sum += u128::from(hb.memory.used);
        self.cpu_total = hb.cpu.total;
        self.memory_total = hb.memory.total;
        self.kvm &= hb.kvm;
    }

    /// Closes the window at `now` and returns the next one. `None` when no
    /// heartbeat was seen or no time passed: nothing to report.
    pub fn flush(self, now: DateTime<Utc>) -> (Option<UsageSample>, Self) {
        let next = Self::new(now);
        if self.count == 0 || now <= self.start {
            return (None, next);
        }
        let n = u128::from(self.count);
        let mean = |sum: u128, total: u64| Resource {
            total,
            used: ((sum / n) as u64).min(total),
        };
        let sample = UsageSample {
            period_start: self.start,
            period_end: now,
            cpu: mean(self.cpu_used_sum, self.cpu_total),
            memory: mean(self.memory_used_sum, self.memory_total),
            kvm: self.kvm,
        };
        (Some(sample), next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn hb(cpu_used: u64, mem_used: u64, kvm: bool) -> Heartbeat {
        Heartbeat {
            agent_version: "0".into(),
            cpu: Resource {
                total: 4000,
                used: cpu_used,
            },
            memory: Resource {
                total: 8000,
                used: mem_used,
            },
            disk: Resource::default(),
            kvm,
            public_ip: None,
        }
    }

    #[test]
    fn averages_used_and_chains_windows() {
        let mut w = UsageWindow::new(at(0));
        w.push(&hb(1000, 2000, true));
        w.push(&hb(3000, 4000, true));
        let (s, next) = w.flush(at(60));
        let s = s.unwrap();
        assert_eq!((s.period_start, s.period_end), (at(0), at(60)));
        assert_eq!(
            s.cpu,
            Resource {
                total: 4000,
                used: 2000
            }
        );
        assert_eq!(
            s.memory,
            Resource {
                total: 8000,
                used: 3000
            }
        );
        assert!(s.kvm);
        // The next window starts exactly where this one ended: no gaps, no overlap.
        let mut w = next;
        w.push(&hb(0, 0, true));
        assert_eq!(w.flush(at(120)).0.unwrap().period_start, at(60));
    }

    #[test]
    fn kvm_must_hold_for_the_whole_window() {
        let mut w = UsageWindow::new(at(0));
        w.push(&hb(0, 0, true));
        w.push(&hb(0, 0, false));
        assert!(!w.flush(at(60)).0.unwrap().kvm);
    }

    #[test]
    fn empty_window_reports_nothing() {
        let (s, next) = UsageWindow::new(at(0)).flush(at(60));
        assert!(s.is_none());
        assert_eq!(next.start, at(60));
    }
}
