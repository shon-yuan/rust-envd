//! MADV_COLLAPSE heap compaction before a snapshot — a port of the oracle's
//! memory/collapse_linux.go at 51083a83cc0903185d992725ea6bb33683814b29.
//! Best-effort: windows that cannot collapse are skipped, never errors.

use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

/// The x86_64 transparent-hugepage size (oracle collapse_linux.go:19).
const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;
/// Stable kernel ABI: MADV_HUGEPAGE = 14 (linux/mman.h), MADV_COLLAPSE = 25
/// (uapi/asm-generic/mman-common.h, 5.14+). Defined here rather than read
/// from libc so the musl and gnu targets take the same path.
const MADV_HUGEPAGE: nix::libc::c_int = 14;
const MADV_COLLAPSE: nix::libc::c_int = 25;

/// Oracle CollapseStats (api.gen.go CollapseResult wire shape).
#[derive(Debug, Default)]
pub struct CollapseStats {
    pub regions: i64,
    pub chunks: i64,
    pub collapsed: i64,
    pub already_huge: i64,
    pub skipped: i64,
}

/// Oracle collapse_linux.go:36-75 CollapseSelf: consolidate this process's
/// anonymous heap into 2 MiB transparent hugepages. Raw madvise successes
/// conflate real migrations with already-huge no-ops; the AnonHugePages delta
/// splits them. `cancelled` plays ctx.Err(): the oracle checks it before each
/// region and breaks (:57-60), and returns the stats with the cancellation
/// error (:74) — the daemon passes a flag that the request teardown flips, so
/// a disconnected client stops the madvise sweep exactly like Go's
/// r.Context() (api/collapse.go:26).
pub fn collapse_self(cancelled: &AtomicBool) -> io::Result<CollapseStats> {
    let maps = fs::read_to_string("/proc/self/maps")?;
    let mut stats = CollapseStats::default();
    let mut successes: i64 = 0;
    let before = anon_huge_pages_bytes();
    for (start, end) in anon_rw_regions(&maps) {
        // Caller gave up (timeout/disconnect): stop issuing madvise calls and
        // return the progress made so far. Reported uniformly via the
        // cancellation error below, regardless of which region was
        // interrupted (oracle collapse_linux.go:56-60).
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        stats.regions += 1;
        let range = collapse_range(start, end);
        stats.chunks += range.chunks;
        successes += range.collapsed;
        stats.skipped += range.skipped;
    }
    let after = anon_huge_pages_bytes();
    let (collapsed, already_huge) = split_collapsed(successes, before, after);
    stats.collapsed = collapsed;
    stats.already_huge = already_huge;
    // Nil on a complete run; the cancellation error if the caller gave up
    // (oracle collapse_linux.go:74 `return s, ctx.Err()` — Go renders it as
    // "context canceled").
    if cancelled.load(Ordering::SeqCst) {
        return Err(io::Error::other("context canceled"));
    }
    Ok(stats)
}

/// Oracle collapse_linux.go:132-145 anonRWRegions: anonymous read-write
/// mappings only — a pathname means file-backed or pseudo ([vdso] and
/// friends), neither of which is an anon heap arena.
fn anon_rw_regions(maps: &str) -> Vec<(usize, usize)> {
    let mut regions = Vec::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else {
            continue;
        };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let Some(perms) = fields.next() else {
            continue;
        };
        if !perms.starts_with("rw") {
            continue;
        }
        // offset, device, inode — a pathname would be the field after the
        // inode; oracle collapse_linux.go:138 skips anything with one.
        if fields.nth(3).is_some() {
            continue;
        }
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) else {
            continue;
        };
        regions.push((start, end));
    }
    regions
}

/// Oracle collapse_linux.go:152-172 collapseRange: a best-effort
/// MADV_HUGEPAGE eligibility hint, then MADV_COLLAPSE per fully-contained
/// 2 MiB window — a whole-region call aborts on the first empty window, so
/// each window is issued on its own and failures are skips, not errors.
fn collapse_range(start: usize, end: usize) -> CollapseStats {
    let mut stats = CollapseStats::default();
    // SAFETY: madvise over this process's own mapped range; hint failures
    // are deliberately ignored (best-effort eligibility only).
    unsafe {
        nix::libc::madvise(start as *mut nix::libc::c_void, end - start, MADV_HUGEPAGE);
    }
    let first = (start + HUGE_PAGE_SIZE - 1) & !(HUGE_PAGE_SIZE - 1);
    let mut window = first;
    while window + HUGE_PAGE_SIZE <= end {
        stats.chunks += 1;
        // SAFETY: madvise over a hugepage-aligned window inside a mapped
        // anonymous region of this process.
        let outcome = unsafe {
            nix::libc::madvise(
                window as *mut nix::libc::c_void,
                HUGE_PAGE_SIZE,
                MADV_COLLAPSE,
            )
        };
        if outcome == 0 {
            stats.collapsed += 1;
        } else {
            stats.skipped += 1;
        }
        window += HUGE_PAGE_SIZE;
    }
    stats
}

/// Oracle collapse_linux.go:88-95 splitCollapsed: attribute madvise successes
/// to real migrations versus already-huge no-ops via the AnonHugePages delta.
/// An unmeasurable or backwards delta falls back to counting every success as
/// a migration; the migrated count is clamped to the successes so background
/// THP activity cannot inflate the attribution.
fn split_collapsed(successes: i64, before: Option<u64>, after: Option<u64>) -> (i64, i64) {
    let Some((before, after)) = before.zip(after) else {
        return (successes, 0);
    };
    if after < before {
        return (successes, 0);
    }
    let migrated = i64::try_from((after - before) / HUGE_PAGE_SIZE as u64)
        .unwrap_or(successes)
        .min(successes);
    (migrated, successes - migrated)
}

/// Oracle collapse_linux.go:100-126 anonHugePagesBytes: AnonHugePages from
/// /proc/self/smaps_rollup, in bytes; None when unreadable (the caller falls
/// back instead of treating a missing read as zero hugepages).
fn anon_huge_pages_bytes() -> Option<u64> {
    let rollup = fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    for line in rollup.lines() {
        let Some(rest) = line.strip_prefix("AnonHugePages:") else {
            continue;
        };
        let kb = rest.split_whitespace().next()?;
        return kb.parse::<u64>().ok().map(|kb| kb * 1024);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anon_rw_regions_selects_pathless_read_write_mappings_only() {
        // Oracle collapse_linux.go:132-145: Read && Write perms and an empty
        // pathname — rwx pathless mappings qualify, [stack]/[anon:*] do not.
        let maps = "55830f000-558424000 r--p 00000000 08:02 123 /usr/bin/envd\n\
                    558424000-55a424000 rw-p 00000000 00:00 0 \n\
                    7f0000000000-7f0000200000 rwxp 00000000 00:00 0 \n\
                    7f1000000000-7f1000200000 rw-p 00000000 00:00 0 [anon:libc]\n\
                    7ffffffde000-7ffffffff000 rw-p 00000000 00:00 0 [stack]\n\
                    600000000000-600000010000 --xp 00000000 00:00 0 \n";
        let regions = anon_rw_regions(maps);
        assert_eq!(
            regions,
            vec![(0x558424000, 0x55a424000), (0x7f0000000000, 0x7f0000200000),]
        );
    }

    #[test]
    fn split_collapsed_attributes_delta_and_degrades_to_successes() {
        let two_mib = HUGE_PAGE_SIZE as u64;
        // Delta of two hugepages over five successes: two real migrations,
        // three already-huge.
        assert_eq!(split_collapsed(5, Some(two_mib), Some(3 * two_mib)), (2, 3));
        // Clamped by successes.
        assert_eq!(split_collapsed(2, Some(0), Some(9 * two_mib)), (2, 0));
        // Unmeasured or backwards delta: everything is a migration.
        assert_eq!(split_collapsed(4, None, Some(8 * two_mib)), (4, 0));
        assert_eq!(
            split_collapsed(4, Some(8 * two_mib), Some(2 * two_mib)),
            (4, 0)
        );
    }

    #[test]
    fn collapse_range_bounds_windows_to_the_region() {
        // One full window inside; the unaligned head and short tail are
        // outside the window bounds and never fed to madvise.
        let base = 0x7f0000000000usize;
        let stats = collapse_range(base + 4096, base + 2 * HUGE_PAGE_SIZE + 4096);
        assert_eq!(stats.chunks, 1);
        // Every window either collapses or skips; neither count may exceed the
        // windows issued on this host's THP mode.
        assert!(stats.collapsed + stats.skipped <= stats.chunks);
    }

    #[test]
    fn a_cancelled_collapse_reports_context_canceled_without_touching_regions() {
        // Oracle collapse_linux.go:57-60 — the region loop checks ctx.Err()
        // before each region and breaks; :74 returns the stats alongside
        // ctx.Err(), so a cancelled run comes back as the Go cancellation
        // error with no regions credited. The daemon handler turns that into
        // the same 500 the oracle's jsonError produces.
        let cancelled = AtomicBool::new(true);
        let error = collapse_self(&cancelled).unwrap_err();
        assert_eq!(error.to_string(), "context canceled");
        let released = AtomicBool::new(false);
        let stats = collapse_self(&released).unwrap();
        assert!(stats.regions > 0);
    }
}
