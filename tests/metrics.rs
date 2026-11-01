#![cfg(target_os = "linux")]

use coorworker_envd::metrics::{Collector, CpuStatus, MetricPaths};
use std::{fs, io, path::Path, time::SystemTime};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Self { dir };
        fixture.write("stat", "cpu 100 0 50 500 50 0 0 0 20 0\ncpu0 1\ncpu1 1\n");
        fixture.write("cpuinfo", "processor : 0\nprocessor : 1\n");
        fixture.write("meminfo", "MemTotal: 10000 kB\nMemAvailable: 4000 kB\nMemFree: 1000 kB\nCached: 2000 kB\nSReclaimable: 500 kB\n");
        fixture
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.dir.path().join(name), text).unwrap();
    }

    fn paths(&self) -> MetricPaths {
        MetricPaths {
            proc_root: self.dir.path().to_owned(),
            disk_root: self.dir.path().to_owned(),
        }
    }

    fn collector(&self) -> Collector {
        Collector::with_paths(self.paths()).unwrap()
    }
}

#[test]
fn sample_uses_previous_cpu_sample_and_pinned_memory_disk_formulas() {
    let fixture = Fixture::new();
    let mut collector = fixture.collector();
    // Delta total=100, busy=30; guest increases must not be double counted.
    fixture.write("stat", "cpu 120 0 60 550 70 0 0 0 40 0\ncpu0 1\ncpu1 1\n");
    let before = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let metrics = collector.sample(CpuStatus::default()).unwrap();
    let after = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(metrics.cpu_used_pct, 30.0);
    assert_eq!(metrics.cpu_count, 2);
    assert_eq!(metrics.mem_total, 10_000 * 1024);
    assert_eq!(metrics.mem_used, 6000 * 1024);
    assert_eq!(metrics.mem_cache, 2500 * 1024);
    assert!((before as i64..=after as i64).contains(&metrics.ts));
    let disk = nix::sys::statfs::statfs(fixture.dir.path()).unwrap();
    assert_eq!(metrics.disk_total, disk.blocks() * disk.block_size() as u64);
    // Filesystems may change between calls, so only total is compared exactly.
    assert!(metrics.disk_used <= metrics.disk_total);
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        0.0
    );
    let json = serde_json::to_value(metrics).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 12);
    for field in [
        "cpu_possible",
        "cpu_target",
        "cpu_target_attempts",
        "cpu_write_pending_ms",
    ] {
        assert_eq!(json[field], 0);
    }
    assert!(json.get("oom_kills").is_none());
}

#[test]
fn cpu_percent_rounds_before_converting_to_f32() {
    let fixture = Fixture::new();
    fixture.write("stat", "cpu 0 0 0 0 0 0 0\n");
    let mut collector = fixture.collector();
    fixture.write("stat", "cpu 1 0 0 5 0 0 0\n");
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        16.67_f32
    );
}

#[test]
fn cpu_percent_handles_regressions_and_clamps_like_gopsutil() {
    for (previous, current, expected) in [
        ("10 0 0 20 0 0 0", "10 0 0 30 0 0 0", 0.0),
        ("10 0 0 20 0 0 0", "9 0 0 40 0 0 0", 0.0),
        ("10 0 0 20 0 0 0", "20 0 0 5 0 0 0", 100.0),
        ("10 0 0 20 0 0 0", "30 0 0 10 0 0 0", 100.0),
    ] {
        let fixture = Fixture::new();
        fixture.write("stat", &format!("cpu {previous}\n"));
        let mut collector = fixture.collector();
        fixture.write("stat", &format!("cpu {current}\n"));
        assert_eq!(
            collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
            expected
        );
    }
}

#[test]
fn logical_cpu_count_prefers_numeric_cpuinfo_entries() {
    let fixture = Fixture::new();
    fixture.write("cpuinfo", "Processor : ARM model\nprocessor : 0\nPROCESSOR : 2\nprocessor : invalid\n processor : 3\nprocessor : 2147483648\n");
    assert_eq!(
        fixture
            .collector()
            .sample(CpuStatus::default())
            .unwrap()
            .cpu_count,
        2
    );
}

#[test]
fn logical_cpu_count_falls_back_to_proc_stat() {
    for cpuinfo in [Some("Processor : ARM model\n"), None] {
        let fixture = Fixture::new();
        if let Some(text) = cpuinfo {
            fixture.write("cpuinfo", text);
        } else {
            fs::remove_file(fixture.dir.path().join("cpuinfo")).unwrap();
        }
        fixture.write("stat", "cpu 1 0 0 1 0 0 0\ncpu0 1\ncpu12 1\ncpux 1\n");
        assert_eq!(
            fixture
                .collector()
                .sample(CpuStatus::default())
                .unwrap()
                .cpu_count,
            2
        );
    }
}

#[test]
fn explicit_cpu_status_is_serialized_without_guessing_manager_state() {
    let fixture = Fixture::new();
    let status = CpuStatus {
        possible: 8,
        target: 4,
        attempts: 3,
        write_pending_ms: 42,
    };
    let json = serde_json::to_value(fixture.collector().sample(status).unwrap()).unwrap();
    assert_eq!(json["cpu_possible"], 8);
    assert_eq!(json["cpu_target"], 4);
    assert_eq!(json["cpu_target_attempts"], 3);
    assert_eq!(json["cpu_write_pending_ms"], 42);
}

#[test]
fn missing_memavailable_uses_free_plus_reclaimable_cache() {
    let fixture = Fixture::new();
    fixture.write(
        "meminfo",
        "MemTotal: 10000 kB\nMemFree: 1000 kB\nCached: 2000 kB\nSReclaimable: 500 kB\n",
    );
    let metrics = fixture.collector().sample(CpuStatus::default()).unwrap();
    assert_eq!(metrics.mem_used, 6500 * 1024);
    assert_eq!(metrics.mem_cache, 2500 * 1024);
}

#[test]
fn older_kernel_available_estimation_uses_zone_watermarks() {
    let fixture = Fixture::new();
    fixture.write("meminfo", "MemTotal: 10000 kB\nMemFree: 1000 kB\nCached: 2000 kB\nSReclaimable: 500 kB\nActive(file): 1500 kB\nInactive(file): 500 kB\n");
    fixture.write(
        "zoneinfo",
        "Node 0, zone Normal\n low 10\n low 2\n low invalid\n\n",
    );
    // SAFETY: sysconf takes only a constant and has no memory side effects.
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) } as u64;
    let watermark = 12 * page_size;
    let expected_available =
        1000 * 1024 - watermark + 2000 * 1024 - watermark + 500 * 1024 - watermark;
    let metrics = fixture.collector().sample(CpuStatus::default()).unwrap();
    assert_eq!(metrics.mem_used, 10000 * 1024 - expected_available);
    fs::remove_file(fixture.dir.path().join("zoneinfo")).unwrap();
    assert_eq!(
        fixture
            .collector()
            .sample(CpuStatus::default())
            .unwrap()
            .mem_used,
        6500 * 1024
    );
}

#[test]
fn explicit_zero_available_does_not_trigger_legacy_fallback() {
    let fixture = Fixture::new();
    fixture.write(
        "meminfo",
        "MemTotal: 10 kB\nMemAvailable: 0 kB\nMemFree: 5 kB\n",
    );
    assert_eq!(
        fixture
            .collector()
            .sample(CpuStatus::default())
            .unwrap()
            .mem_used,
        10 * 1024
    );
}

#[test]
fn malformed_cpu_samples_and_missing_baseline_are_errors() {
    let fixture = Fixture::new();
    for text in [
        "",
        "intr 1\n",
        "cpu 1 2\n",
        "cpu nope 0 0 0 0 0 0\n",
        "cpu 1 0 0 0 0 0 0 NaN\n",
    ] {
        fixture.write("stat", text);
        assert!(
            Collector::with_paths(fixture.paths()).is_err(),
            "accepted {text:?}"
        );
    }
    fs::remove_file(fixture.dir.path().join("stat")).unwrap();
    assert!(Collector::with_paths(fixture.paths()).is_err());
}

#[test]
fn missing_meminfo_and_malformed_memory_numbers_are_errors() {
    let fixture = Fixture::new();
    let mut collector = fixture.collector();
    for text in [
        "MemTotal: nope kB\n",
        "MemTotal: -1 kB\n",
        "MemAvailable: bad kB\n",
        "Cached: 18446744073709551616 kB\n",
        "Active: bad kB\n",
    ] {
        fixture.write("meminfo", text);
        assert_eq!(
            collector.sample(CpuStatus::default()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    fs::remove_file(fixture.dir.path().join("meminfo")).unwrap();
    assert!(collector.sample(CpuStatus::default()).is_err());
}

#[test]
fn disk_statfs_failure_is_propagated() {
    let fixture = Fixture::new();
    let mut paths = fixture.paths();
    paths.disk_root = fixture.dir.path().join("missing");
    assert!(Collector::with_paths(paths)
        .unwrap()
        .sample(CpuStatus::default())
        .is_err());
}

#[test]
fn sampling_failure_before_cpu_read_preserves_the_previous_sample() {
    let fixture = Fixture::new();
    let mut collector = fixture.collector();
    fixture.write("stat", "cpu 120 0 60 550 70 0 0\n");
    fixture.write("meminfo", "MemTotal: bad kB\n");
    assert!(collector.sample(CpuStatus::default()).is_err());
    fixture.write("meminfo", "MemTotal: 10 kB\nMemAvailable: 4 kB\n");
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        30.0
    );
}

#[test]
fn disk_failure_consumes_the_cpu_sample_before_retry() {
    let fixture = Fixture::new();
    let mut paths = fixture.paths();
    paths.disk_root = fixture.dir.path().join("disk");
    let mut collector = Collector::with_paths(paths.clone()).unwrap();
    fixture.write("stat", "cpu 120 0 60 550 70 0 0\n");
    assert!(collector.sample(CpuStatus::default()).is_err());
    fs::create_dir(&paths.disk_root).unwrap();
    // Since the disk error happened after CPU collection, unchanged CPU time is 0%.
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        0.0
    );
}

#[test]
fn malformed_sample_can_recover_without_replacing_baseline() {
    let fixture = Fixture::new();
    let mut collector = fixture.collector();
    fixture.write("stat", "cpu broken\n");
    assert!(collector.sample(CpuStatus::default()).is_err());
    fixture.write("stat", "cpu 120 0 60 550 70 0 0\n");
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        30.0
    );
}

#[test]
fn legacy_watermarks_use_half_cache_minimum_and_unsigned_go_arithmetic() {
    let fixture = Fixture::new();
    fixture.write("meminfo", "MemTotal: 10000 kB\nMemFree: 0 kB\nCached: 2000 kB\nSReclaimable: 500 kB\nActive(file): 1500 kB\nInactive(file): 500 kB\n");
    fixture.write("zoneinfo", " low 1000\n");
    // SAFETY: sysconf takes only a constant and has no memory side effects.
    let watermark = 1000 * unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) } as u64;
    let available = 0_u64
        .wrapping_sub(watermark)
        .wrapping_add(1000 * 1024)
        .wrapping_add(250 * 1024);
    let metrics = fixture.collector().sample(CpuStatus::default()).unwrap();
    assert_eq!(metrics.mem_used, (10000_u64 * 1024).wrapping_sub(available));
}

#[test]
fn meminfo_ignores_unknown_and_malformed_lines_but_preserves_unsigned_values() {
    let fixture = Fixture::new();
    fixture.write("meminfo", "future: invalid\nMemTotal: 1 kB: ignored\nMemTotal: 10 kB\nMemAvailable: 11 kB\nHugePages_Total: 100\n");
    assert_eq!(
        fixture
            .collector()
            .sample(CpuStatus::default())
            .unwrap()
            .mem_used,
        0_u64.wrapping_sub(1024)
    );
}

#[test]
fn cpu_accounts_for_nice_irq_softirq_and_steal_without_guest_double_count() {
    let fixture = Fixture::new();
    fixture.write("stat", "cpu 0 0 0 0 0 0 0 0 0 0\n");
    let mut collector = fixture.collector();
    fixture.write("stat", "cpu 10 10 10 50 10 10 10 10 5 5\n");
    assert_eq!(
        collector.sample(CpuStatus::default()).unwrap().cpu_used_pct,
        50.0
    );
}

#[test]
fn real_linux_collection_is_read_only_and_returns_real_values() {
    let mut collector = Collector::new().unwrap();
    let metrics = collector.sample(CpuStatus::default()).unwrap();
    assert!(metrics.cpu_count > 0);
    assert!(metrics.mem_total > 0);
    assert!(metrics.mem_used <= metrics.mem_total);
    assert!(metrics.disk_total > 0);
    assert!((0.0..=100.0).contains(&metrics.cpu_used_pct));
    assert!(Path::new("/proc/meminfo").is_file());
}
