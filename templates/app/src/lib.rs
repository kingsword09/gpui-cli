//! Shared UI for {{APP_TITLE}}.
//!
//! All view code lives here so desktop, iOS and Android render the same
//! element tree; only the platform entry points differ.

use gpui::*;

#[cfg(debug_assertions)]
pub mod live;

/// Connects to the `gpui run --live` dev server in debug builds and no-ops in
/// release builds.
///
/// `config_file` is only used on Android, where the CLI stages its dev-channel
/// credentials into the app's internal files dir (apps have no environment to
/// inherit there).
pub fn init_live(config_file: Option<&std::path::Path>) {
    #[cfg(debug_assertions)]
    live::init(config_file);
    #[cfg(not(debug_assertions))]
    let _ = config_file;
}

/// Under `gpui run --live`, drains asset changes the dev channel received and
/// evicts the affected images from GPUI's cache so the next render reloads
/// them from disk without a rebuild. Call once from the app's run closure.
pub fn pump_live_assets(cx: &mut App) {
    #[cfg(debug_assertions)]
    {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let paths = crate::live::take_asset_events();
                if paths.is_empty() {
                    continue;
                }
                cx.update(|cx| {
                    for path in paths {
                        cx.remove_asset::<gpui::ImgResourceLoader>(&gpui::Resource::Embedded(
                            path.into(),
                        ));
                    }
                    cx.refresh_windows();
                });
            }
        })
        .detach();
    }
    #[cfg(not(debug_assertions))]
    let _ = cx;
}

/// Root view of the application.
pub struct MainView;

impl MainView {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MainView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for MainView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .child("{{APP_TITLE}}"),
            )
            .child(
                div()
                    .text_sm()
                    .child("Cross-platform Desktop & Mobile app powered by GPUI"),
            )
    }
}

// ── Mobile entry points ──────────────────────────────────────────────────────
//
// Both platforms open the same `MainView`; only how the run loop is started
// differs. The `gpui_mobile` crate supplies the platform implementation.

{{MOBILE_ENTRY}}
