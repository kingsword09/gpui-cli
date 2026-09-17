# Local changes

Based on `gpui-pre-wgpu 0.3.5` from crates.io, derived from Zed revision
`d89e9c2124b2786a390c7a451c7488601b4da2e1`. The original Apache-2.0 license
and package metadata are retained.

- `src/shaders.wgsl` passes gradient stop colors separately across shader
  function boundaries for compatibility with the Android emulator's Metal
  translation. Color calculations and buffer layouts are unchanged.
- `src/wgpu_context.rs` disables shader DEBUG information on Android while
  preserving the remaining instance flags.

Keep this crate aligned with the workspace's GPUI version and Cargo patch
entry. Revalidate native rendering before upgrading or removing the override.
