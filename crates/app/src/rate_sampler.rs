//! View-side sliding-window transfer rate / ETA (phase 2).
//! Not part of core protocol — see design doc §2.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use macsftp_core::TransferId;

pub const WINDOW_SECS: f64 = 4.0;
pub const WARMUP_SECS: f64 = 0.5;
pub const STALL_SECS: f64 = 3.0;

#[derive(Debug, Clone, Copy)]
pub struct RateSnapshot {
    pub speed_bps: Option<f64>,
    pub stalled: bool,
    pub eta_secs: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AggregateRate {
    pub speed_bps: Option<f64>,
    pub eta_secs: Option<f64>,
}

#[derive(Debug, Clone)]
struct RateSample {
    at: Instant,
    bytes_done: u64,
}

#[derive(Debug, Default)]
struct RateSampler {
    samples: VecDeque<RateSample>,
    last_bytes_change_at: Option<Instant>,
    last_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct TransferRateBook {
    samplers: HashMap<TransferId, RateSampler>,
}

impl TransferRateBook {
    pub fn observe(&mut self, id: TransferId, bytes_done: u64, now: Instant) {
        let sampler = self.samplers.entry(id).or_default();
        if sampler.last_bytes.is_none_or(|b| bytes_done > b) {
            sampler.last_bytes_change_at = Some(now);
            sampler.last_bytes = Some(bytes_done);
        }
        sampler.samples.push_back(RateSample { at: now, bytes_done });
        let cutoff = now - std::time::Duration::from_secs_f64(WINDOW_SECS);
        while sampler
            .samples
            .front()
            .is_some_and(|s| s.at < cutoff && sampler.samples.len() > 2)
        {
            sampler.samples.pop_front();
        }
        // Also drop if only one sample older than window — keep last two max logic simple:
        while let Some(front) = sampler.samples.front() {
            if front.at < cutoff && sampler.samples.len() > 1 {
                sampler.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self, id: TransferId) {
        self.samplers.remove(&id);
    }

    pub fn snapshot(
        &self,
        id: TransferId,
        bytes_done: u64,
        bytes_total: Option<u64>,
        now: Instant,
    ) -> RateSnapshot {
        let Some(sampler) = self.samplers.get(&id) else {
            return RateSnapshot {
                speed_bps: None,
                stalled: false,
                eta_secs: None,
            };
        };
        let speed_bps = speed_from_samples(&sampler.samples, now);
        let stalled = is_stalled(sampler, speed_bps, now);
        // Guard applies to or-patterns only when all arms bind the same names;
        // split arms so `s` is only matched when bound.
        let eta_secs = match (stalled, speed_bps, bytes_total) {
            (true, _, _) => None,
            (_, None, _) => None,
            (_, Some(s), _) if s <= f64::EPSILON => None,
            (false, Some(speed), Some(total)) if total >= bytes_done => {
                Some((total - bytes_done) as f64 / speed)
            }
            _ => None,
        };
        RateSnapshot {
            speed_bps,
            stalled,
            eta_secs,
        }
    }

    pub fn aggregate(
        &self,
        running: &[(TransferId, u64 /* done */, Option<u64> /* total */)],
        now: Instant,
    ) -> AggregateRate {
        let mut sum_speed = 0.0;
        let mut any_speed = false;
        let mut remaining: u64 = 0;
        let mut any_remaining = false;
        for &(id, done, total) in running {
            let snap = self.snapshot(id, done, total, now);
            if let Some(s) = snap.speed_bps {
                if !snap.stalled {
                    sum_speed += s;
                    any_speed = true;
                }
            }
            if let Some(t) = total {
                remaining = remaining.saturating_add(t.saturating_sub(done));
                any_remaining = true;
            }
        }
        let speed_bps = any_speed.then_some(sum_speed);
        let eta_secs = match (speed_bps, any_remaining) {
            (Some(s), true) if s > f64::EPSILON => Some(remaining as f64 / s),
            _ => None,
        };
        AggregateRate { speed_bps, eta_secs }
    }
}

fn speed_from_samples(samples: &VecDeque<RateSample>, _now: Instant) -> Option<f64> {
    let first = samples.front()?;
    let last = samples.back()?;
    if samples.len() < 2 {
        return None;
    }
    let elapsed = last.at.duration_since(first.at).as_secs_f64();
    if elapsed < WARMUP_SECS {
        return None;
    }
    let delta = last.bytes_done.saturating_sub(first.bytes_done) as f64;
    Some(delta / elapsed)
}

fn is_stalled(sampler: &RateSampler, speed_bps: Option<f64>, now: Instant) -> bool {
    let Some(changed_at) = sampler.last_bytes_change_at else {
        return false;
    };
    let idle = now.duration_since(changed_at).as_secs_f64() >= STALL_SECS;
    if !idle {
        return false;
    }
    match speed_bps {
        None => true, // past stall threshold with no usable speed
        Some(s) => s <= f64::EPSILON,
    }
}

pub fn format_speed(bps: Option<f64>) -> String {
    match bps {
        None => "— MB/s".into(),
        Some(s) if s >= 1_000_000.0 => format!("{:.1} MB/s", s / 1_000_000.0),
        Some(s) => format!("{:.1} KB/s", s / 1000.0),
    }
}

pub fn format_eta(secs: Option<f64>) -> String {
    match secs {
        None => "—".into(),
        Some(s) if s < 60.0 => format!("{}s", s.ceil() as u64),
        Some(s) if s < 3600.0 => {
            let m = (s / 60.0).floor() as u64;
            let sec = (s % 60.0).ceil() as u64;
            format!("{m}m {sec}s")
        }
        Some(s) => {
            let h = (s / 3600.0).floor() as u64;
            let m = ((s % 3600.0) / 60.0).floor() as u64;
            format!("{h}h {m}m")
        }
    }
}

pub fn format_running_detail(done: u64, total: Option<u64>, snap: &RateSnapshot) -> String {
    use macsftp_ui::format_size;
    let done_s = format_size(Some(done)).to_string();
    if snap.stalled {
        return match total {
            Some(t) => format!("{} / {} · Stalled", done_s, format_size(Some(t))),
            None => format!("{done_s} · Stalled"),
        };
    }
    let speed_s = format_speed(snap.speed_bps);
    match total {
        Some(t) => format!(
            "{} / {} · {} · ETA {}",
            done_s,
            format_size(Some(t)),
            speed_s,
            format_eta(snap.eta_secs)
        ),
        None => format!("{done_s} · {speed_s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn id(n: u64) -> TransferId {
        TransferId(n)
    }

    #[test]
    fn speed_uses_window_endpoints() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 1_000_000, t0 + Duration::from_secs(1));
        let snap = book.snapshot(id(1), 1_000_000, Some(10_000_000), t0 + Duration::from_secs(1));
        assert!(snap.speed_bps.unwrap() > 900_000.0 && snap.speed_bps.unwrap() < 1_100_000.0);
        assert!(!snap.stalled);
        assert!(snap.eta_secs.unwrap() > 8.0 && snap.eta_secs.unwrap() < 12.0);
    }

    #[test]
    fn warmup_yields_no_speed() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 100, t0 + Duration::from_millis(100));
        let snap = book.snapshot(id(1), 100, Some(1000), t0 + Duration::from_millis(100));
        assert!(snap.speed_bps.is_none());
        assert!(!snap.stalled);
    }

    #[test]
    fn stalled_when_bytes_unchanged_past_threshold() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 500, t0);
        book.observe(id(1), 500, t0 + Duration::from_secs(1));
        book.observe(id(1), 500, t0 + Duration::from_secs(4));
        let snap = book.snapshot(id(1), 500, Some(1000), t0 + Duration::from_secs(4));
        assert!(snap.stalled);
        assert!(snap.eta_secs.is_none());
    }

    #[test]
    fn clear_removes_sampler() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.clear(id(1));
        let snap = book.snapshot(id(1), 0, Some(100), t0 + Duration::from_secs(2));
        assert!(snap.speed_bps.is_none());
    }

    #[test]
    fn format_running_detail_stalled_and_normal() {
        let stalled = RateSnapshot {
            speed_bps: Some(0.0),
            stalled: true,
            eta_secs: None,
        };
        let s = format_running_detail(1_000_000, Some(2_000_000), &stalled);
        assert!(s.contains("Stalled"), "{s}");
        assert!(!s.contains("— MB/s") || s.contains("Stalled"));

        let normal = RateSnapshot {
            speed_bps: Some(1_048_576.0),
            stalled: false,
            eta_secs: Some(10.0),
        };
        let s = format_running_detail(1_000_000, Some(2_000_000), &normal);
        assert!(s.contains("MB/s") || s.contains("KB/s"), "{s}");
        assert!(s.contains("ETA"), "{s}");
        assert!(!s.contains("— MB/s · ETA —"), "{s}");
    }

    #[test]
    fn aggregate_sums_running_speeds() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 2_000_000, t0 + Duration::from_secs(1));
        book.observe(id(2), 0, t0);
        book.observe(id(2), 2_000_000, t0 + Duration::from_secs(1));
        let now = t0 + Duration::from_secs(1);
        let agg = book.aggregate(
            &[
                (id(1), 2_000_000, Some(10_000_000)),
                (id(2), 2_000_000, Some(10_000_000)),
            ],
            now,
        );
        assert!(agg.speed_bps.unwrap() > 3_500_000.0);
        assert!(agg.eta_secs.is_some());
    }
}
