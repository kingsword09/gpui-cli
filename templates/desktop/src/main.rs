//! Desktop entry point for {{APP_TITLE}}.
//!
//! `gpui_kit::application()` opens the native platform (AppKit / Win32 /
//! Wayland or X11) and `gpui_kit::init` wires up the component layer.

// Imported for the GPUI re-exports (`WindowOptions`, `Window`, `App`, ...).
use gpui::*;
use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::*;

use {{APP_LIB_NAME}}::MainView;

fn main() {
    gpui_kit::application().run(|cx: &mut App| {
        // Installs the theme and global state that `Root` needs to paint a
        // background; must run before any view is created.
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Light, None, cx);

        cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|_| MainView::new());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open the main window");

        cx.activate(true);
    });
}
