//! Expose the Cargo target triple to the crate so `self-update` can name the
//! release asset for the running platform (`rocker-<triple>.tar.xz`), matching
//! the archive names `dist` produces.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=ROCKER_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
