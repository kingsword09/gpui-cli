//! Desktop entry point for {{APP_TITLE_COMMENT}}.
//!
//! `gpui_kit::application()` opens the native platform (AppKit / Win32 /
//! Wayland or X11) and `gpui_kit::init` wires up the component layer.

// Imported for the GPUI re-exports (`WindowOptions`, `Window`, `App`, ...).
use gpui::*;
use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::*;

use {{APP_LIB_NAME}}::MainView;

fn main() {
    // Live development reads assets straight from `<project>/assets` so the
    // CLI can hot-reload them; release builds keep the default source. The
    // asset source must exist before `init_live` connects: the hello carries
    // the asset-reload capability the CLI relies on.
    #[cfg(debug_assertions)]
    let application = gpui_kit::application()
        .with_assets({{APP_LIB_NAME}}::dev_asset_source(None));
    #[cfg(not(debug_assertions))]
    let application = gpui_kit::application();

    // Debug builds connect back to `gpui run --live` for logs and panics.
    {{APP_LIB_NAME}}::init_live(None);

    application.run(|cx: &mut App| {
        // Installs the theme and global state that `Root` needs to paint a
        // background; must run before any view is created.
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
        {{APP_LIB_NAME}}::pump_live_assets(cx);

        cx.open_window(WindowOptions::default(), |window, cx| {
            {{APP_LIB_NAME}}::register_window(
                "main",
                {{APP_TITLE_RUST}},
                800,
                600,
                1000,
                true,
            );
            cx.on_app_quit(|_| async {
                {{APP_LIB_NAME}}::close_window("main", Some("app_quit"));
            })
            .detach();
            let view = cx.new(|_| MainView::new());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open the main window");

        cx.activate(true);
    });
}
