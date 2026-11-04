//! Read-only metrics matching E2B envd runtime 51083a83 and gopsutil v4.26.6.
//!
//! The wire struct ([`Metrics`]), the CPU status, and the shared CPU/disk
//! arithmetic are platform-neutral so both the Linux collector below and the
//! Windows collector can produce byte-identical values. Formula provenance:
//! envd/internal/host/metrics.go and gopsutil cpu/cpu.go, cpu/cpu_linux.go,
//! mem/mem_linux.go at the pinned versions above. CPU state is per collector
//! (initialized at construction), never a process-global counter.

#[cfg(target_os = "linux")]
use nix::sys::statfs::statfs;
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::{collections::HashMap, fs, io, path::PathBuf, time::SystemTime};

/// Status supplied by the CPU manager. Outside Firecracker every field is zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuStatus {
    pub possible: u32,
    pub target: u32,
    pub attempts: u32,
    pub write_pending_ms: u64,
}

/// One OOM kill as the oracle reports it (internal/host/oom.go). Shared so the
/// metrics wire layer can name it on every platform; only the Linux watcher
/// produces values today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OomKill {
    pub seq: i64,
    pub process: String,
}

/// The official JSON shape. OOM kills are omitted until a watcher can supply them.
#[derive(Debug, Serialize)]
pub struct Metrics {
    pub ts: i64,
    pub cpu_count: u32,
    /// Go's encoding/json renders a float32 with strconv's 'f' format and no
    /// forced fraction: 0.0 is `0`, not `0.0`. serde_json always emits the
    /// fraction, so the shortest round-trip digits are serialized as a raw
    /// number here.
    #[serde(serialize_with = "go_float32")]
    pub cpu_used_pct: f32,
    pub cpu_possible: u32,
    pub cpu_target: u32,
    pub cpu_target_attempts: u32,
    pub cpu_write_pending_ms: u64,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_cache: u64,
    pub disk_used: u64,
    pub disk_total: u64,
}

/// Serialize an f32 the way Go's encoding/json does: shortest round-trip
/// digits in plain decimal form, with no `.0` for whole numbers.
fn go_float32<S: serde::Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error as _;
    let text = value.to_string();
    let number: serde_json::Number = serde_json::from_str(&text).map_err(S::Error::custom)?;
    number.serialize(serializer)
}

/// Read roots, injectable without changing environment variables or host files.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub struct MetricPaths {
    pub proc_root: PathBuf,
    pub disk_root: PathBuf,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Collector {
    paths: MetricPaths,
    previous: CpuTimes,
}

#[cfg(target_os = "linux")]
impl Collector {
    /// Capture the aggregate CPU baseline at daemon startup.
    pub fn new() -> io::Result<Self> {
        Self::with_paths(MetricPaths {
            proc_root: "/proc".into(),
            disk_root: "/".into(),
        })
    }

    pub fn with_paths(paths: MetricPaths) -> io::Result<Self> {
        let previous = read_cpu_times(&paths)?;
        Ok(Self { paths, previous })
    }

    /// Follow the upstream collection order, including updating CPU state before
    /// statfs. A later disk error therefore still consumes this CPU interval.
    pub fn sample(&mut self, status: CpuStatus) -> io::Result<Metrics> {
        let memory = read_memory(&self.paths)?;
        let cpu_count = logical_cpu_count(&self.paths)?;
        let current = read_cpu_times(&self.paths)?;
        let percent = self.previous.percent(current);
        self.previous = current;
        let (disk_total, disk_used) = read_disk(&self.paths)?;
        let seconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        Ok(Metrics {
            ts: seconds as i64,
            cpu_count,
            cpu_used_pct: if percent > 0.0 {
                ((percent * 100.0).round() / 100.0) as f32
            } else {
                percent as f32
            },
            cpu_possible: status.possible,
            cpu_target: status.target,
            cpu_target_attempts: status.attempts,
            cpu_write_pending_ms: status.write_pending_ms,
            mem_total: memory.total,
            mem_used: memory.total.wrapping_sub(memory.available),
            mem_cache: memory.cached,
            disk_used,
            disk_total,
        })
    }
}

// Shared with the Windows collector (next slice); unused on Windows until it
// lands, so the dead-code lint is silenced there rather than hiding real dead
// code on Linux.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuTimes {
    pub(crate) total: f64,
    pub(crate) busy: f64,
}

#[cfg_attr(windows, allow(dead_code))]
impl CpuTimes {
    pub(crate) fn percent(self, current: Self) -> f64 {
        if current.busy <= self.busy {
            return 0.0;
        }
        if current.total <= self.total {
            return 100.0;
        }
        ((current.busy - self.busy) / (current.total - self.total) * 100.0).clamp(0.0, 100.0)
    }
}

#[cfg(target_os = "linux")]
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(target_os = "linux")]
fn read_proc(paths: &MetricPaths, name: &str) -> io::Result<String> {
    let path = paths.proc_root.join(name);
    fs::read_to_string(&path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

#[cfg(target_os = "linux")]
fn system_constant(name: nix::libc::c_int, fallback: u64) -> u64 {
    // SAFETY: sysconf only reads a process/system constant and takes no pointers.
    let value = unsafe { nix::libc::sysconf(name) };
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

#[cfg(target_os = "linux")]
fn read_cpu_times(paths: &MetricPaths) -> io::Result<CpuTimes> {
    let stat = read_proc(paths, "stat")?;
    let fields: Vec<_> = stat
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if fields.len() < 8 || fields[0] != "cpu" {
        return Err(invalid("stat does not contain aggregate cpu info"));
    }
    let clocks = system_constant(nix::libc::_SC_CLK_TCK, 100) as f64;
    let mut times = [0.0; 10];
    for (slot, value) in times.iter_mut().zip(fields.iter().skip(1)) {
        let parsed = value
            .parse::<f64>()
            .map_err(|_| invalid("invalid cpu counter"))?;
        if !parsed.is_finite() || parsed < 0.0 {
            return Err(invalid("cpu counter must be finite and nonnegative"));
        }
        *slot = parsed / clocks;
    }
    let [user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice] = times;
    // Preserve gopsutil's floating-point operation order. Guest time is already
    // included in user/nice and must not increase the denominator a second time.
    let total = user + system + idle + nice + iowait + irq + softirq + steal + guest + guest_nice;
    let total = total - guest - guest_nice;
    Ok(CpuTimes {
        total,
        busy: total - idle - iowait,
    })
}

#[cfg(target_os = "linux")]
fn logical_cpu_count(paths: &MetricPaths) -> io::Result<u32> {
    let count = match read_proc(paths, "cpuinfo") {
        Ok(text) => text
            .lines()
            .filter(|line| {
                let line = line.to_ascii_lowercase();
                line.starts_with("processor")
                    && line
                        .split_once(':')
                        .map_or(line.as_str(), |(_, value)| value)
                        .trim()
                        .parse::<i32>()
                        .is_ok()
            })
            .count(),
        Err(_) => 0, // gopsutil falls back to stat on an unreadable cpuinfo.
    };
    let count = if count == 0 {
        read_proc(paths, "stat")?
            .lines()
            .filter(|line| {
                line.starts_with("cpu") && line.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
            })
            .count()
    } else {
        count
    };
    u32::try_from(count).map_err(|_| invalid("logical cpu count exceeds uint32"))
}

#[cfg(target_os = "linux")]
struct Memory {
    total: u64,
    available: u64,
    cached: u64,
}

// gopsutil validates all recognized meminfo numbers, even fields not returned by
// envd. Unknown kernel extensions are ignored. HugePages counts are not in KiB.
#[cfg(target_os = "linux")]
const MEMORY_KEYS: &[&str] = &[
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "Buffers",
    "Cached",
    "Active",
    "Inactive",
    "Active(anon)",
    "Inactive(anon)",
    "Active(file)",
    "Inactive(file)",
    "Unevictable",
    "Percpu",
    "Writeback",
    "WritebackTmp",
    "Dirty",
    "Shmem",
    "Slab",
    "SReclaimable",
    "SUnreclaim",
    "KernelStack",
    "PageTables",
    "SwapCached",
    "CommitLimit",
    "Committed_AS",
    "HighTotal",
    "HighFree",
    "LowTotal",
    "LowFree",
    "SwapTotal",
    "SwapFree",
    "Mapped",
    "VmallocTotal",
    "VmallocUsed",
    "VmallocChunk",
    "HugePages_Total",
    "HugePages_Free",
    "HugePages_Rsvd",
    "HugePages_Surp",
    "Hugepagesize",
    "AnonHugePages",
];

#[cfg(target_os = "linux")]
fn read_memory(paths: &MetricPaths) -> io::Result<Memory> {
    let text = read_proc(paths, "meminfo")?;
    let mut values = HashMap::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() != 2 || !MEMORY_KEYS.contains(&fields[0].trim()) {
            continue;
        }
        let key = fields[0].trim();
        let number = fields[1]
            .trim()
            .replace(" kB", "")
            .parse::<u64>()
            .map_err(|_| invalid(format!("invalid meminfo {key}")))?;
        let value = if key.starts_with("HugePages_") {
            number
        } else {
            number.wrapping_mul(1024)
        };
        values.insert(key, value);
    }
    let value = |key| values.get(key).copied().unwrap_or(0_u64);
    let cached = value("Cached").wrapping_add(value("SReclaimable"));
    let available = match values.get("MemAvailable") {
        Some(available) => *available,
        None if ["Active(file)", "Inactive(file)", "SReclaimable"]
            .iter()
            .all(|key| values.contains_key(key)) =>
        {
            legacy_available(
                paths,
                value("MemFree"),
                cached,
                value("Active(file)").wrapping_add(value("Inactive(file)")),
                value("SReclaimable"),
            )
        }
        None => value("MemFree").wrapping_add(cached),
    };
    Ok(Memory {
        total: value("MemTotal"),
        available,
        cached,
    })
}

#[cfg(target_os = "linux")]
fn legacy_available(
    paths: &MetricPaths,
    free: u64,
    cached: u64,
    page_cache: u64,
    reclaimable: u64,
) -> u64 {
    let Ok(zoneinfo) = read_proc(paths, "zoneinfo") else {
        return free.wrapping_add(cached);
    };
    let low_pages = zoneinfo.lines().fold(0_u64, |sum, line| {
        let mut fields = line.split_whitespace();
        if fields.next().is_some_and(|key| key.starts_with("low")) {
            sum.wrapping_add(
                fields
                    .next()
                    .and_then(|number| number.parse::<u64>().ok())
                    .unwrap_or(0),
            )
        } else {
            sum
        }
    });
    let watermark = low_pages.wrapping_mul(system_constant(nix::libc::_SC_PAGESIZE, 4096));
    // The pinned Go implementation uses uint64 arithmetic (including underflow).
    free.wrapping_sub(watermark)
        .wrapping_add(
            page_cache.wrapping_sub(((page_cache / 2) as f64).min(watermark as f64) as u64),
        )
        .wrapping_add(
            reclaimable.wrapping_sub(((reclaimable / 2) as f64).min(watermark as f64) as u64),
        )
}

#[cfg(target_os = "linux")]
fn read_disk(paths: &MetricPaths) -> io::Result<(u64, u64)> {
    let stat = statfs(&paths.disk_root).map_err(io::Error::from)?;
    Ok(disk_usage(
        stat.blocks(),
        stat.blocks_available(),
        stat.block_size() as u64,
    ))
}

#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn disk_usage(blocks: u64, available_blocks: u64, block_size: u64) -> (u64, u64) {
    let total = blocks.wrapping_mul(block_size);
    let available = available_blocks.wrapping_mul(block_size);
    (total, total.wrapping_sub(available))
}

#[cfg(test)]
mod tests {
    use super::disk_usage;

    #[test]
    fn disk_usage_includes_reserved_blocks_unavailable_to_users() {
        // A filesystem with 100 blocks, 60 free, but only 50 user-available
        // reports 50 used, including the 10 reserved blocks, per Go statfs.
        assert_eq!(disk_usage(100, 50, 4096), (409_600, 204_800));
        assert_eq!(disk_usage(0, 0, 4096), (0, 0));
        assert_eq!(disk_usage(100, 100, 4096), (409_600, 0));
    }
}
