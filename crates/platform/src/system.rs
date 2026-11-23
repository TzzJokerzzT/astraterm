//! System information for the status bar: CPU load, memory usage and the clock.
//!
//! Everything comes from `/proc`, which costs one small read per sample. The
//! application samples at most once per second and only while the status bar is
//! visible, so the monitor never turns into background polling.

use std::time::{Duration, Instant};

/// Aggregate CPU times from the `cpu` line of `/proc/stat`, in clock ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    /// Time spent idle (including I/O wait).
    pub idle: u64,
    /// Total time across every state.
    pub total: u64,
}

/// Parses the aggregate `cpu` line of `/proc/stat`.
///
/// The values are `user nice system idle iowait irq softirq steal guest guest_nice`.
/// Guest time is already included in `user`/`nice`, so it is not added again.
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<_, _>>()
        .ok()?;
    if fields.len() < 5 {
        return None;
    }
    let idle = fields[3].saturating_add(fields[4]);
    let total: u64 = fields.iter().sum();
    (total > 0).then_some(CpuTimes { idle, total })
}

/// Memory usage from `/proc/meminfo`, in kibibytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
}

/// Parses `/proc/meminfo`.
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut total_kb = None;
    let mut available_kb = None;
    for line in text.lines() {
        let (key, rest) = line.split_once(':')?;
        let value: u64 = rest.split_whitespace().next()?.parse().ok()?;
        match key {
            "MemTotal" => total_kb = Some(value),
            "MemAvailable" => available_kb = Some(value),
            _ => {}
        }
        if total_kb.is_some() && available_kb.is_some() {
            break;
        }
    }
    let total_kb = total_kb?;
    let available_kb = available_kb?;
    (total_kb > 0).then_some(MemInfo {
        total_kb,
        available_kb,
    })
}

impl MemInfo {
    /// Share of memory in use, as a percentage.
    pub fn used_percent(&self) -> f32 {
        let used = self.total_kb.saturating_sub(self.available_kb) as f64;
        ((used / self.total_kb as f64) * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// CPU share between two samples.
pub fn cpu_percent_between(previous: CpuTimes, current: CpuTimes) -> Option<f32> {
    let total = current.total.checked_sub(previous.total)?;
    let idle = current.idle.checked_sub(previous.idle)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some(((busy as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32)
}

/// Latest system readings shown by the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SystemSnapshot {
    /// CPU usage since the previous sample, in percent.
    pub cpu_percent: Option<f32>,
    /// Resident memory in use, in percent.
    pub ram_percent: Option<f32>,
    /// When the snapshot was taken, in seconds since the Unix epoch, local time.
    pub clock_epoch: Option<u64>,
}

/// Samples `/proc` at a bounded rate.
#[derive(Debug, Default)]
pub struct SystemMonitor {
    previous_cpu: Option<CpuTimes>,
    last_sample: Option<Instant>,
    snapshot: SystemSnapshot,
}

impl SystemMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> SystemSnapshot {
        self.snapshot
    }

    /// Refreshes the snapshot when at least `interval` has passed.
    ///
    /// Returns `true` when new values are available, so the caller knows a redraw is
    /// worth requesting. The first call primes the CPU counter and reports memory
    /// and the clock immediately.
    pub fn sample(&mut self, interval: Duration) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_sample
            && now.duration_since(last) < interval
        {
            return false;
        }
        self.last_sample = Some(now);

        if let Ok(text) = std::fs::read_to_string("/proc/stat")
            && let Some(current) = parse_proc_stat(&text)
        {
            if let Some(previous) = self.previous_cpu
                && let Some(percent) = cpu_percent_between(previous, current)
            {
                self.snapshot.cpu_percent = Some(percent);
            }
            self.previous_cpu = Some(current);
        }

        if let Ok(text) = std::fs::read_to_string("/proc/meminfo")
            && let Some(info) = parse_meminfo(&text)
        {
            self.snapshot.ram_percent = Some(info.used_percent());
        }

        self.snapshot.clock_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());

        true
    }
}

/// Formats the local time with a strftime-style format string.
///
/// The conversion goes through the C library so the user's timezone and locale are
/// honoured (a pure-Rust implementation would have to parse `TZif`). Formats longer
/// than 63 characters or containing a NUL byte fall back to `%H:%M`.
pub fn format_clock(epoch_secs: u64, format: &str) -> String {
    let format = if format.is_empty() || format.len() > 63 || format.contains('\0') {
        "%H:%M"
    } else {
        format
    };
    let Ok(c_format) = std::ffi::CString::new(format) else {
        return String::new();
    };

    // SAFETY: `time`, `localtime_r` and `strftime` are called with a valid time value,
    // valid non-null pointers to properly sized buffers, and a NUL-terminated format
    // string; the returned length is checked before reading the buffer.
    unsafe {
        let time: libc::time_t = epoch_secs as libc::time_t;
        let mut broken_down: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut broken_down).is_null() {
            return String::new();
        }
        let mut buffer = [0u8; 128];
        let written = libc::strftime(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c_format.as_ptr(),
            &broken_down,
        );
        if written == 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buffer[..written]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_STAT: &str = "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0 0 0\n";

    #[test]
    fn parses_the_aggregate_cpu_line() {
        let times = parse_proc_stat(PROC_STAT).unwrap();
        assert_eq!(times.total, 1000);
        assert_eq!(times.idle, 800);
    }

    #[test]
    fn cpu_percent_uses_the_delta() {
        let previous = CpuTimes {
            idle: 100,
            total: 200,
        };
        let current = CpuTimes {
            idle: 150,
            total: 400,
        };
        let percent = cpu_percent_between(previous, current).unwrap();
        assert!((percent - 75.0).abs() < 0.01, "got {percent}");
    }

    #[test]
    fn cpu_percent_is_none_without_elapsed_time() {
        let same = CpuTimes {
            idle: 10,
            total: 20,
        };
        assert!(cpu_percent_between(same, same).is_none());
    }

    #[test]
    fn rejects_garbage_stat_input() {
        assert!(parse_proc_stat("").is_none());
        assert!(parse_proc_stat("intr 1 2 3\n").is_none());
        assert!(parse_proc_stat("cpu  not numbers here\n").is_none());
    }

    #[test]
    fn parses_meminfo_and_computes_usage() {
        let text =
            "MemTotal:       16000000 kB\nMemFree:  1000000 kB\nMemAvailable:    4000000 kB\n";
        let info = parse_meminfo(text).unwrap();
        assert_eq!(info.total_kb, 16_000_000);
        assert_eq!(info.available_kb, 4_000_000);
        assert!((info.used_percent() - 75.0).abs() < 0.01);
    }

    #[test]
    fn meminfo_requires_both_fields() {
        assert!(parse_meminfo("MemTotal: 1000 kB\n").is_none());
        assert!(parse_meminfo("MemAvailable: 10 kB\n").is_none());
    }

    #[test]
    fn sampling_rates_are_bounded() {
        let mut monitor = SystemMonitor::new();
        assert!(
            monitor.sample(Duration::from_secs(1)),
            "the first sample always runs"
        );
        assert!(
            !monitor.sample(Duration::from_secs(1)),
            "a second sample inside the window is skipped"
        );
        let snapshot = monitor.snapshot();
        assert!(snapshot.ram_percent.is_some());
        assert!(snapshot.clock_epoch.is_some());
    }

    #[test]
    fn clock_formatting_honours_the_requested_fields() {
        let epoch = 1_700_000_000; // 2023-11-14T22:13:20Z
        let year = format_clock(epoch, "%Y");
        assert_eq!(year.len(), 4);
        assert!(year.chars().all(|c| c.is_ascii_digit()));
        let hours = format_clock(epoch, "%H:%M");
        assert_eq!(hours.len(), 5);
        assert_eq!(&hours[2..3], ":");
    }

    #[test]
    fn clock_formatting_falls_back_for_unusable_formats() {
        let epoch = 1_700_000_000;
        assert_eq!(format_clock(epoch, ""), format_clock(epoch, "%H:%M"));
        assert_eq!(format_clock(epoch, "literal"), "literal");
        let long = "x".repeat(80);
        assert_eq!(format_clock(epoch, &long), format_clock(epoch, "%H:%M"));
    }
}
