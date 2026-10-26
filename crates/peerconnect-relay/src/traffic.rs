//! The relay's own traffic budget for groups: what it sends members each
//! month, and how fast, kept within limits by forwarding smaller video to
//! everyone (the video router's ceiling, GROUP.md §3.3) rather than by
//! turning anyone away. Voice, commands and chat are never limited.
//!
//! Counted are the bytes of the frames the relay hands its connections
//! (QUIC and IP headers add about a tenth on the wire); with a usage file,
//! the month's total survives restarts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use peerconnect::group::wire::VIDEO_LAYERS;

/// How often the budget is looked at.
const TICK: Duration = Duration::from_secs(10);
/// How often the month's usage is written to the usage file.
const SAVE_EVERY: u32 = 6;
/// How often the month's usage is logged.
const LOG_EVERY: u32 = 60;
/// A rate below this share of the limit for this long lets video grow again.
const CALM_SHARE: f64 = 0.7;
const CALM_TICKS: u32 = 3;

/// The budget, as configured.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    /// Bytes a month (calendar months, UTC). Video shrinks for everyone from
    /// 80% of it (layers up to 1), 95% (layer 0 only) and stops at 100%.
    pub monthly_bytes: Option<u64>,
    /// Bits per second at most, averaged over [`TICK`]: beyond it video
    /// shrinks a layer at a time for everyone, and grows back once well below.
    pub max_bits_per_second: Option<u64>,
    /// Where the month's usage is kept across restarts (`YYYY-MM bytes`).
    pub usage_file: Option<PathBuf>,
}

/// What the relay's connections share: bytes sent, and the video ceiling.
pub(crate) struct Traffic {
    sent: AtomicU64,
    /// Video layers every member may get now (`VIDEO_LAYERS`: all).
    ceiling: AtomicU8,
}

impl Traffic {
    pub(crate) fn new() -> Arc<Traffic> {
        Arc::new(Traffic { sent: AtomicU64::new(0), ceiling: AtomicU8::new(VIDEO_LAYERS) })
    }

    pub(crate) fn sent(&self, bytes: usize) {
        self.sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn ceiling(&self) -> u8 {
        self.ceiling.load(Ordering::Relaxed)
    }
}

/// Watches the traffic and sets the ceiling, until `stop`.
pub(crate) async fn watch(traffic: Arc<Traffic>, budget: Budget, mut stop: tokio::sync::watch::Receiver<bool>) {
    if budget.monthly_bytes.is_none() && budget.max_bits_per_second.is_none() && budget.usage_file.is_none() {
        return;
    }
    let mut state = State::new(&budget, now_month(), traffic.sent.load(Ordering::Relaxed));
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await;
    let mut ticks = 0u32;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.wait_for(|stop| *stop) => break,
        }
        ticks += 1;
        let ceiling = state.step(&budget, now_month(), traffic.sent.load(Ordering::Relaxed));
        if traffic.ceiling.swap(ceiling, Ordering::Relaxed) != ceiling {
            eprintln!("group video now limited to {} for everyone ({})", layers(ceiling), state.summary(&budget));
        }
        if ticks.is_multiple_of(SAVE_EVERY) {
            state.save(&budget);
        }
        if ticks.is_multiple_of(LOG_EVERY) {
            eprintln!("group traffic: {}", state.summary(&budget));
        }
    }
    state.save(&budget);
}

fn layers(ceiling: u8) -> String {
    match ceiling {
        0 => "no video".into(),
        c if c >= VIDEO_LAYERS => "all layers".into(),
        c => format!("layers up to {}", c - 1),
    }
}

/// The budget's arithmetic, free of clocks and files, for testing.
struct State {
    month: (i32, u32),
    /// Sent this month before the counter's present value.
    base: u64,
    /// The counter at the start of this month (or of the relay).
    counted_from: u64,
    last: u64,
    rate: f64,
    rate_ceiling: u8,
    calm: u32,
}

impl State {
    fn new(budget: &Budget, month: (i32, u32), counter: u64) -> State {
        let base = budget
            .usage_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|text| {
                let (m, bytes) = text.trim().split_once(' ')?;
                (m == format_month(month)).then(|| bytes.parse().ok()).flatten()
            })
            .unwrap_or(0);
        State { month, base, counted_from: counter, last: counter, rate: 0.0, rate_ceiling: VIDEO_LAYERS, calm: 0 }
    }

    fn used(&self) -> u64 {
        self.base + (self.last - self.counted_from)
    }

    /// One tick: the counter now, in `month`. Returns the ceiling.
    fn step(&mut self, budget: &Budget, month: (i32, u32), counter: u64) -> u8 {
        self.rate = (counter - self.last) as f64 * 8.0 / TICK.as_secs_f64();
        self.last = counter;
        if month != self.month {
            (self.month, self.base, self.counted_from) = (month, 0, counter);
        }
        let by_month = match budget.monthly_bytes {
            Some(max) if self.used() >= max => 0,
            Some(max) if self.used() as f64 >= max as f64 * 0.95 => 1,
            Some(max) if self.used() as f64 >= max as f64 * 0.8 => 2,
            _ => VIDEO_LAYERS,
        };
        if let Some(max) = budget.max_bits_per_second {
            if self.rate > max as f64 {
                self.rate_ceiling = self.rate_ceiling.saturating_sub(1).max(1);
                self.calm = 0;
            } else if self.rate < max as f64 * CALM_SHARE {
                self.calm += 1;
                if self.calm >= CALM_TICKS && self.rate_ceiling < VIDEO_LAYERS {
                    self.rate_ceiling += 1;
                    self.calm = 0;
                }
            } else {
                self.calm = 0;
            }
        }
        by_month.min(self.rate_ceiling)
    }

    fn summary(&self, budget: &Budget) -> String {
        let used = self.used() as f64 / 1e9;
        let month = match budget.monthly_bytes {
            Some(max) => {
                format!("{used:.1} GB of {:.0} GB this month ({:.0}%)", max as f64 / 1e9, used * 1e11 / max as f64)
            }
            None => format!("{used:.1} GB this month"),
        };
        format!("{month}, {:.1} Mbit/s now", self.rate / 1e6)
    }

    fn save(&self, budget: &Budget) {
        let Some(file) = &budget.usage_file else { return };
        let text = format!("{} {}\n", format_month(self.month), self.used());
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, file)).is_err() {
            eprintln!("cannot write {}", file.display());
        }
    }
}

fn format_month((year, month): (i32, u32)) -> String {
    format!("{year:04}-{month:02}")
}

/// The calendar month now, UTC.
fn now_month() -> (i32, u32) {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    month_of(days)
}

/// The year and month of a day counted from 1970-01-01 (proleptic Gregorian).
fn month_of(days: i64) -> (i32, u32) {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_from_days() {
        assert_eq!(month_of(0), (1970, 1));
        assert_eq!(month_of(31), (1970, 2));
        assert_eq!(month_of(20_752), (2026, 10), "2026-10-26");
        assert_eq!(month_of(11_016), (2000, 2), "2000-02-29");
    }

    #[test]
    fn video_shrinks_with_the_rate_and_grows_back() {
        let gb = 1_000_000_000u64;
        let budget = Budget { max_bits_per_second: Some(800_000_000), ..Budget::default() };
        let mut s = State::new(&budget, (2026, 10), 0);
        let mut tick = |counter: u64| s.step(&budget, (2026, 10), counter);
        assert_eq!(tick(gb), VIDEO_LAYERS, "1 GB in 10 s: 800 Mbit/s, not over");
        assert_eq!(tick(2 * gb + 1), VIDEO_LAYERS - 1, "just over: one layer less");
        assert_eq!(tick(3 * gb + 2), VIDEO_LAYERS - 2);
        // Calm again: a layer back after CALM_TICKS ticks.
        let mut counter = 3 * gb + 2;
        let mut last = 0;
        for _ in 0..CALM_TICKS {
            counter += 1;
            last = tick(counter);
        }
        assert_eq!(last, VIDEO_LAYERS - 1);
    }

    #[test]
    fn video_shrinks_with_the_month_and_comes_back_with_the_next() {
        let gb = 1_000_000_000u64;
        let budget = Budget { monthly_bytes: Some(100 * gb), ..Budget::default() };
        let mut s = State::new(&budget, (2026, 10), 0);
        assert_eq!(s.step(&budget, (2026, 10), 79 * gb), VIDEO_LAYERS);
        assert_eq!(s.step(&budget, (2026, 10), 80 * gb), 2, "80%: layers up to 1");
        assert_eq!(s.step(&budget, (2026, 10), 95 * gb), 1, "95%: layer 0");
        assert_eq!(s.step(&budget, (2026, 10), 100 * gb), 0, "spent: no video");
        assert_eq!(s.step(&budget, (2026, 11), 100 * gb + 1), VIDEO_LAYERS, "a new month starts afresh");
    }

    #[test]
    fn the_month_survives_a_restart() {
        let file = std::env::temp_dir().join(format!("peerconnect-usage-{}", std::process::id()));
        let budget = Budget { monthly_bytes: Some(1000), max_bits_per_second: None, usage_file: Some(file.clone()) };
        let mut s = State::new(&budget, (2026, 10), 0);
        s.step(&budget, (2026, 10), 600);
        s.save(&budget);
        let mut again = State::new(&budget, (2026, 10), 0);
        assert_eq!(again.used(), 600);
        assert_eq!(again.step(&budget, (2026, 10), 250), 2, "850 of 1000: past 80%");
        assert_eq!(State::new(&budget, (2026, 11), 0).used(), 0, "another month");
        let _ = std::fs::remove_file(file);
    }
}
