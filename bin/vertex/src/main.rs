//! Vertex Swarm node binary.

mod cli;

// jemalloc is the default allocator wherever it is supported (Linux and macOS).
// Windows (no msvc support) and wasm fall back to the system allocator.
#[cfg(all(not(target_os = "windows"), not(target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(
    feature = "heap-profiling",
    not(target_os = "windows"),
    not(target_arch = "wasm32")
))]
#[unsafe(export_name = "_rjem_malloc_conf")]
static MALLOC_CONF: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";

fn main() -> eyre::Result<()> {
    // Build and run the tokio runtime
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(cli::run())
}
