//! Build-time DPDK discovery for the `native` feature.
//!
//! The native lifecycle path links the exact reviewed DPDK release and refuses to
//! build against any other version. Portable builds never enter this path.

use std::{env, process::Command};

const EXPECTED_DPDK_VERSION: &str = "25.11.0";
const DPDK_LINK_LIBRARIES: [&str; 10] = [
    "rte_bus_vdev",
    "rte_eal",
    "rte_ethdev",
    "rte_kvargs",
    "rte_log",
    "rte_mbuf",
    "rte_mempool",
    "rte_net",
    "rte_ring",
    "rte_telemetry",
];

fn main() {
    if env::var_os("CARGO_FEATURE_NATIVE").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
    {
        return;
    }
    let output = Command::new("pkg-config")
        .args(["--modversion", "libdpdk"])
        .output()
        .expect("pkg-config must run for the DPDK native feature");
    assert!(
        output.status.success(),
        "libdpdk {EXPECTED_DPDK_VERSION} development metadata is required for the DPDK native feature"
    );
    let version = String::from_utf8(output.stdout)
        .expect("libdpdk modversion must be UTF-8")
        .trim()
        .to_string();
    assert_eq!(
        version, EXPECTED_DPDK_VERSION,
        "DPDK native feature requires exactly libdpdk {EXPECTED_DPDK_VERSION}, found {version}"
    );
    let raw_libdir = Command::new("pkg-config")
        .args(["--variable=libdir", "libdpdk"])
        .output()
        .ok()
        .filter(|search| search.status.success())
        .and_then(|search| String::from_utf8(search.stdout).ok())
        .map(|libdir| libdir.trim().to_string())
        .filter(|libdir| !libdir.is_empty())
        .expect("libdpdk libdir must be discoverable for the DPDK native feature");
    let libdir = match env::var("PKG_CONFIG_SYSROOT_DIR") {
        Ok(sysroot) if !sysroot.is_empty() && !raw_libdir.starts_with(&sysroot) => {
            format!("{sysroot}{raw_libdir}")
        }
        _ => raw_libdir,
    };
    println!("cargo:rustc-link-search=native={libdir}");
    // The net_ring PMD self-registers from a constructor, so an `--as-needed`
    // link would drop it; EAL loads it explicitly through `-d` at runtime.
    println!("cargo:rustc-env=AXIUSFLOW_DPDK_LIBDIR={libdir}");
    #[cfg(feature = "native")]
    generate_bindings();
    for library in DPDK_LINK_LIBRARIES {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
}

/// Generates allowlisted bindings for the EAL lifecycle and queue/mbuf data-path
/// surface against the exact reviewed headers.
#[cfg(feature = "native")]
fn generate_bindings() {
    let include_dir = dpdk_include_dir();
    let arch_include_dir = include_dir.replace("/include/dpdk", "/include/x86_64-linux-gnu/dpdk");
    let arch_base_dir = include_dir.replace("/include/dpdk", "/include/x86_64-linux-gnu");
    let wrapper = format!("{include_dir}/axiusflow_dpdk_wrapper.h");
    std::fs::write(
        &wrapper,
        "#include <rte_eal.h>\n#include <rte_version.h>\n#include <rte_ethdev.h>\n#include <rte_mbuf.h>\n#include <rte_mempool.h>\n#include <rte_ring.h>\n",
    )
    .expect("DPDK bindgen wrapper header must be writable");
    let bindings = bindgen::Builder::default()
        .header(&wrapper)
        .clang_arg(format!("-I{include_dir}"))
        .clang_arg(format!("-I{arch_include_dir}"))
        .clang_arg(format!("-I{arch_base_dir}"))
        .allowlist_function("rte_eal_init")
        .allowlist_function("rte_eal_cleanup")
        .allowlist_function("rte_version")
        .allowlist_function("rte_eth_dev_count_avail")
        .allowlist_function("rte_eth_dev_configure")
        .allowlist_function("rte_eth_rx_queue_setup")
        .allowlist_function("rte_eth_tx_queue_setup")
        .allowlist_function("rte_eth_dev_start")
        .allowlist_function("rte_eth_dev_stop")
        .allowlist_function("rte_eth_dev_close")
        .allowlist_function("rte_pktmbuf_pool_create")
        .allowlist_function("rte_mempool_free")
        .allowlist_function("rte_mempool_avail_count")
        .allowlist_type("rte_eth_conf")
        .allowlist_type("rte_mempool")
        .allowlist_type("rte_mbuf")
        .allowlist_var("RTE_MBUF_DEFAULT_DATAROOM")
        .allowlist_var("RTE_MBUF_DEFAULT_BUF_SIZE")
        .allowlist_var("RTE_PKTMBUF_HEADROOM")
        .allowlist_var("SOCKET_ID_ANY")
        .layout_tests(false)
        .generate()
        .expect("DPDK bindgen generation must succeed against the exact reviewed headers");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set for build scripts");
    bindings
        .write_to_file(format!("{out_dir}/dpdk_bindings.rs"))
        .expect("DPDK bindings must be writable");
    compile_shim(&include_dir, &arch_include_dir, &arch_base_dir);
}

/// Compiles the checked-in inline-forwarding shim against the same exact headers.
#[cfg(feature = "native")]
fn compile_shim(include_dir: &str, arch_include_dir: &str, arch_base_dir: &str) {
    cc::Build::new()
        .file("native/shim.c")
        .include(include_dir)
        .include(arch_include_dir)
        .include(arch_base_dir)
        // DPDK's public headers require the SSE4.2 baseline its build uses.
        .flag_if_supported("-msse4.2")
        .compile("axiusflow_dpdk_shim");
    println!("cargo:rerun-if-changed=native/shim.c");
}

#[cfg(feature = "native")]
fn dpdk_include_dir() -> String {
    let raw = Command::new("pkg-config")
        .args(["--variable=includedir", "libdpdk"])
        .output()
        .ok()
        .filter(|search| search.status.success())
        .and_then(|search| String::from_utf8(search.stdout).ok())
        .map(|includedir| includedir.trim().to_string())
        .filter(|includedir| !includedir.is_empty())
        .expect("libdpdk includedir must be discoverable for the DPDK native feature");
    match env::var("PKG_CONFIG_SYSROOT_DIR") {
        Ok(sysroot) if !sysroot.is_empty() && !raw.starts_with(&sysroot) => {
            format!("{sysroot}{raw}")
        }
        _ => raw,
    }
}
