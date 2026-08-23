//! Bakes the target triple this client is being built for into the binary.
//!
//! The self-updater has to ask the release for one artifact out of seven, and
//! the name of that artifact is the Rust target triple —
//! `butai-<version>-<triple>.tar.gz`, from both `.github/workflows/release.yml`
//! and `scripts/release.sh`.
//!
//! `scripts/install.sh` has to *guess* which triple a machine wants: `uname`
//! for the arch, and `ldd --version | grep musl` to tell a musl box from a
//! glibc one. It has no choice — it runs before any butai exists. A running
//! butai does have a choice, because cargo tells every build script exactly
//! which target it is compiling for. Reading it here turns "which build does
//! this machine want" from a heuristic into a fact: a musl build asks for the
//! musl tarball because it *is* the musl build, and an armv7 one cannot ask
//! for aarch64 by misreading `uname -m`.
//!
//! `std::env::consts::{OS, ARCH}` is not a substitute. It knows `linux` and
//! `x86_64`, and cannot tell gnu from musl at all — which is the one
//! distinction that decides whether the downloaded binary runs.

fn main() {
    // Set by cargo for build scripts only; it is not available to the crate
    // itself, which is the whole reason this file exists.
    let target = std::env::var("TARGET").expect("cargo always sets TARGET for a build script");
    println!("cargo:rustc-env=BUTAI_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
