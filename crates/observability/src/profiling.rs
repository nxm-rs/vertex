//! CPU and memory profiling utilities.

use std::time::Duration;

use serde::Serialize;

/// Memory statistics from the allocator.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryStats {
    /// Bytes allocated by the application.
    pub allocated: usize,
    /// Bytes in active pages.
    pub active: usize,
    /// Bytes in physically resident pages.
    pub resident: usize,
    /// Bytes mapped (virtual address space).
    pub mapped: usize,
    /// Bytes retained in unused dirty pages.
    pub retained: usize,
}

/// Start CPU profiling for specified duration, returns flamegraph SVG.
#[cfg(feature = "profiling")]
pub fn cpu_profile(duration: Duration) -> eyre::Result<Vec<u8>> {
    use pprof::ProfilerGuardBuilder;

    let guard = ProfilerGuardBuilder::default()
        .frequency(99)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()?;

    std::thread::sleep(duration);

    let report = guard.report().build()?;
    let mut svg = Vec::new();
    report.flamegraph(&mut svg)?;
    Ok(svg)
}

/// CPU profiling not available without `profiling` feature.
#[cfg(not(feature = "profiling"))]
pub fn cpu_profile(_duration: Duration) -> eyre::Result<Vec<u8>> {
    Err(eyre::eyre!("CPU profiling requires the 'profiling' feature"))
}

/// Get current memory statistics from jemalloc.
#[cfg(feature = "jemalloc")]
pub fn memory_stats() -> eyre::Result<MemoryStats> {
    use tikv_jemalloc_ctl::{epoch, stats};

    // Refresh stats epoch (jemalloc error doesn't impl std::error::Error)
    epoch::advance().map_err(|e| eyre::eyre!("jemalloc epoch advance failed: {:?}", e))?;

    let allocated = stats::allocated::read()
        .map_err(|e| eyre::eyre!("jemalloc stats read failed: {:?}", e))?;
    let active = stats::active::read()
        .map_err(|e| eyre::eyre!("jemalloc stats read failed: {:?}", e))?;
    let resident = stats::resident::read()
        .map_err(|e| eyre::eyre!("jemalloc stats read failed: {:?}", e))?;
    let mapped = stats::mapped::read()
        .map_err(|e| eyre::eyre!("jemalloc stats read failed: {:?}", e))?;
    let retained = stats::retained::read()
        .map_err(|e| eyre::eyre!("jemalloc stats read failed: {:?}", e))?;

    Ok(MemoryStats {
        allocated,
        active,
        resident,
        mapped,
        retained,
    })
}

/// Memory stats not available without `jemalloc` feature.
#[cfg(not(feature = "jemalloc"))]
pub fn memory_stats() -> eyre::Result<MemoryStats> {
    Err(eyre::eyre!("Memory stats require the 'jemalloc' feature"))
}

/// Dump a jemalloc heap profile to the specified path.
///
/// Requires: build with `--features heap-profiling` AND runtime `MALLOC_CONF=prof:true`.
/// The output file can be analyzed with `jeprof` or converted to pprof format.
#[cfg(feature = "jemalloc")]
pub fn heap_dump(path: &std::path::Path) -> eyre::Result<()> {
    use std::ffi::CString;
    use tikv_jemalloc_ctl::raw;

    let path_str = path
        .to_str()
        .ok_or_else(|| eyre::eyre!("path contains non-UTF8 characters"))?;
    let c_path =
        CString::new(path_str).map_err(|e| eyre::eyre!("path contains null byte: {e}"))?;

    // prof.dump writes a heap profile to the specified path.
    // This will fail if jemalloc was not compiled with profiling support
    // or if prof:true was not set in MALLOC_CONF.
    unsafe {
        raw::write(b"prof.dump\0", c_path.as_ptr())
            .map_err(|e| eyre::eyre!("heap dump failed (is MALLOC_CONF=prof:true set?): {:?}", e))
    }
}

/// Heap dump not available without `jemalloc` feature.
#[cfg(not(feature = "jemalloc"))]
pub fn heap_dump(_path: &std::path::Path) -> eyre::Result<()> {
    Err(eyre::eyre!(
        "Heap profiling requires the 'heap-profiling' feature"
    ))
}

/// Check if heap profiling was compiled in.
pub const fn heap_profiling_available() -> bool {
    cfg!(feature = "heap-profiling")
}

/// Check if CPU profiling is available.
pub const fn cpu_profiling_available() -> bool {
    cfg!(feature = "profiling")
}

/// Check if memory profiling is available.
pub const fn memory_profiling_available() -> bool {
    cfg!(feature = "jemalloc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_checks() {
        // These should compile and run regardless of features
        let _ = cpu_profiling_available();
        let _ = memory_profiling_available();
    }

    #[test]
    fn test_memory_stats_serialization() {
        let stats = MemoryStats {
            allocated: 1024,
            active: 2048,
            resident: 4096,
            mapped: 8192,
            retained: 512,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("1024"));
    }
}
