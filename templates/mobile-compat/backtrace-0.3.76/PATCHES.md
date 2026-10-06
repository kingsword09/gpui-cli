# Local patches

This snapshot is based on `rust-lang/backtrace-rs` commit
`1071ac9c21c7f0dc31d8e28f4218a378f5d3c188` and version `0.3.76`.

`src/symbolize/gimli.rs` selects the native dyld library enumerator only for
`target_os = "macos"`. The upstream `target_vendor = "apple"` condition also
matched iOS simulator targets, where libc does not expose those macOS dyld
functions. iOS keeps Mach-O parsing but uses the crate's no-op native-library
fallback, which preserves backtrace capture without compiling macOS APIs.
