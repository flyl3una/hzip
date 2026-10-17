//! Pure-Rust QCOW2 reader and writer.
//!
//! Supported now:
//! - Uncompressed clusters, read and written
//! - zlib- and zstd-compressed clusters, read
//! - Backing-file chain (recursive), including copy-up on write
//! - Sparse / v3 zero-flagged clusters
//! - Cluster allocation and refcount maintenance (`open_rw`)
//!
//! Implements [`fs_core::BlockRead`] so any consumer that takes a
//! `BlockRead` (partition probe, fs driver, slice adapter) can drive a
//! `Qcow2Reader` directly. The C ABI in [`capi`] returns a
//! [`fs_core::ffi::FsCoreDevice`] handle for the same reason.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod capi;
pub mod error;
pub mod header;
pub mod reader;

pub use error::{Error, Result};
pub use header::Header;
pub use reader::{ClusterStatus, Extent, ExtentIter, Qcow2Reader};

// DOES THIS BUILD ACTUALLY TRAP AN ARITHMETIC OVERFLOW?
//
// Inline in `lib.rs` rather than a module of its own, for the reason
// `tests/ci_profile.rs` explains at length: a separate file under `src/`
// hangs off one `mod` line, and losing that line leaves the file present,
// uncompiled, and asserting nothing, with no lint to say so. Inline,
// there is no declaration to lose. It cannot live in `tests/` at all --
// it has to be part of the library target that the debug step builds,
// because the question it answers is about that build.
#[cfg(test)]
mod overflow_checks {
    /// Set by the debug step in `ci.yml`, and by nothing else.
    ///
    /// The release step must NOT set it: overflow checks are off there
    /// deliberately, because that is what ships.
    const HANDSHAKE: &str = "EXPECT_OVERFLOW_CHECKS";

    /// Perform an overflow and report whether the program was stopped.
    ///
    /// This is the only question that matters and the only one that
    /// cannot be answered by reading a file. `overflow-checks` can be
    /// turned off by a manifest key in four spellings, by a
    /// `CARGO_PROFILE_TEST_OVERFLOW_CHECKS` variable set at step or job
    /// level, by `.cargo/config.toml`, and by whatever cargo adds next.
    /// Each of those was found separately, all the same shape: a scanner
    /// asking whether a known spelling of "disabled" appears in the text
    /// it happens to read. This asks the build instead.
    fn this_build_traps_an_overflow() -> bool {
        // The hook is silenced so a deliberate panic does not print a
        // scary backtrace into a passing job's log. `set_hook` is
        // process-wide, so for the moment this is installed another
        // thread's panic message would be swallowed too -- it would
        // still fail, just less legibly. Narrow, and worth it against a
        // log line that reads as a failure on every green run.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let trapped = std::panic::catch_unwind(|| {
            // `black_box` keeps this out of const evaluation, where it
            // would be a compile error rather than a runtime trap.
            let big = std::hint::black_box(u64::MAX);
            std::hint::black_box(big + 1);
        })
        .is_err();
        std::panic::set_hook(previous);
        trapped
    }

    /// When the gate says it built a profile that traps, check that it
    /// did.
    ///
    /// # What guards this test's own relevance
    ///
    /// With `HANDSHAKE` unset this asserts nothing, which is the shape
    /// of a test that passes because its fixture is missing. It is not
    /// guarded here, because it cannot be: a build cannot tell whether
    /// it was supposed to be the checking one. It is guarded in
    /// `tests/ci_profile.rs`, which reads `ci.yml` and refuses if no
    /// `cargo test` there runs without `--release` while setting this
    /// variable. Delete the step, or drop the variable from it, and that
    /// test fails.
    ///
    /// The two are not redundant. A runtime check cannot notice its own
    /// absence; a text scan cannot tell whether the build it describes
    /// works. One proves the step is there, this proves it can see.
    #[test]
    fn the_build_the_gate_asked_to_check_does_check() {
        let asked = match std::env::var(HANDSHAKE) {
            Ok(value) if !value.is_empty() => value,
            _ => return,
        };

        assert!(
            this_build_traps_an_overflow(),
            "{HANDSHAKE}={asked} was set, so this run is the one that is \
             supposed to panic on arithmetic overflow -- and it did not. \
             The checks are off in the profile the gate built. Something \
             turned them off where nothing reading Cargo.toml can see it: \
             a CARGO_PROFILE_TEST_OVERFLOW_CHECKS variable at step or job \
             level, a .cargo/config.toml, or a cargo mechanism newer than \
             this comment. The debug step is running and blind, which is \
             the exact state it exists to rule out."
        );
    }
}
