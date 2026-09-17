# Third-party notices

Original gpui-cli source and window artwork are licensed under MIT OR Apache-2.0.
The components below retain their upstream licenses.

| Bundled component | Source | License |
| --- | --- | --- |
| Mobile host templates | [longbridge/gpui-mobile](https://github.com/longbridge/gpui-mobile), revision `b4e3ab258f271003b7d4b874f7c5ebe3a77fac60` | Apache-2.0, selected from upstream's license options |
| `gpui-pre-wgpu 0.3.5` | [Zed](https://github.com/zed-industries/zed), revision `d89e9c2124b2786a390c7a451c7488601b4da2e1`, published as `gpui-pre-wgpu` | Apache-2.0; license and local patch notes are included with the crate |
| Gradle wrapper | [Gradle](https://github.com/gradle/gradle) | Apache-2.0; original wrapper copyright headers are retained |
| Noto Color Emoji 2.042 | [Google Noto Emoji](https://github.com/googlefonts/noto-emoji), revision `7f49a00d523ae5f94e52fd9f9a39bac9cf65f958`; Copyright 2022 Google Inc. | SIL Open Font License 1.1 |

The mobile templates have been adapted for generated package names, native
entry points, build settings, artwork and launch screens. The bundled renderer's
changes are listed in
[PATCHES.md](templates/android-compat/gpui-pre-wgpu-0.3.5/PATCHES.md).

The Apache license is included in [LICENSE-APACHE](LICENSE-APACHE). The font
license is included alongside the Android font at
[OFL.txt](templates/android/gradle/app/src/main/assets/fonts/OFL.txt).
Generated projects also receive notices and the applicable bundled license files.
