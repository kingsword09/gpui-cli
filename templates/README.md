# {{APP_TITLE}}

A native GPUI application created with gpui-cli.

- **Project:** `{{PROJECT_NAME}}`
- **Bundle identifier:** `{{BUNDLE_ID}}`
- **Targets:** {{TARGETS_LIST}}

## Run

Check the toolchain with `gpui doctor`, then run a selected target:

```bash
{{RUN_COMMANDS}}
```

Build all selected targets without launching:

```bash
gpui build all
```

{{PLATFORM_NOTES}}
## Edit the app

Shared UI lives in `crates/app/src/lib.rs`. Desktop entry points, when selected,
are in `crates/desktop/`; mobile hosts and application artwork are in
`mobile/ios/` and `mobile/android/`.

GPUI dependencies are pinned to a compatible version set in the workspace
manifest. Keep dependency versions aligned when upgrading.

Bundled source and assets retain the licenses listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
