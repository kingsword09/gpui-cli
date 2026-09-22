//! Shared UI for {{APP_TITLE_COMMENT}}.
//!
//! All view code lives here so desktop, iOS and Android render the same
//! element tree; only the platform entry points differ.

use gpui::*;

#[cfg(all(feature = "gpui-dev", feature = "gpui-profile"))]
compile_error!("features `gpui-dev` and `gpui-profile` cannot be enabled together");

#[cfg(all(feature = "gpui-dev", not(debug_assertions)))]
compile_error!("feature `gpui-dev` is debug-only; use a debug build");

#[cfg(debug_assertions)]
pub mod live;

/// Connects to the `gpui run --live` dev server in debug builds and no-ops in
/// release builds.
///
/// `config_file` is only used on Android, where the CLI stages its dev-channel
/// credentials into the app's internal files dir (apps have no environment to
/// inherit there).
pub fn init_live(config_file: Option<&std::path::Path>) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    live::init(config_file);
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let _ = config_file;
}

/// Under `gpui run --live`, drains asset changes the dev channel received and
/// evicts the affected images from GPUI's cache so the next render reloads
/// them from disk without a rebuild. Call once from the app's run closure.
pub fn pump_live_assets(cx: &mut App) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
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
                        // Embedded = the DevAssetSource key (desktop/Android);
                        // Path = the simulator filesystem key used on iOS.
                        cx.remove_asset::<gpui::ImgResourceLoader>(&gpui::Resource::Embedded(
                            path.clone().into(),
                        ));
                        #[cfg(target_os = "ios")]
                        cx.remove_asset::<gpui::ImgResourceLoader>(&gpui::Resource::Path(
                            std::env::temp_dir()
                                .join("gpui-assets")
                                .join(&path)
                                .into(),
                        ));
                    }
                    cx.refresh_windows();
                });
            }
        })
        .detach();
    }
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let _ = cx;
}

/// Returns the live asset source when `gpui-dev` is explicitly enabled.
/// Normal debug builds retain an empty source and therefore no dev channel.
#[cfg(debug_assertions)]
pub fn dev_asset_source(
    extra_root: Option<std::path::PathBuf>,
) -> live::DevAssetSource {
    #[cfg(feature = "gpui-dev")]
    {
        live::dev_asset_source(extra_root)
    }
    #[cfg(not(feature = "gpui-dev"))]
    {
        let _ = extra_root;
        live::disabled_asset_source()
    }
}

/// Root view of the application.
///
/// The click counter exists to demonstrate the live-mode state snapshot: with
/// `gpui run --live`, rebuilding keeps this number across restarts instead of
/// dropping back to zero. Replace it with whatever state your app persists.
pub struct MainView {
    clicks: usize,
}

/// Snapshot bytes from the previous process, when building with live support.
/// Release builds have no `live` module — they always start cold.
fn restored_state() -> Option<String> {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    let restored = crate::live::take_restored_state();
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let restored = None;
    restored
}

impl MainView {
    pub fn new() -> Self {
        // Live mode: restore the snapshot the previous process published.
        // Unparseable data means a cold start — never a boot failure.
        let clicks = restored_state()
            .and_then(|json| {
                json.split("\"clicks\":")
                    .nth(1)
                    .and_then(|rest| rest.split('}').next())
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        Self { clicks }
    }
}

impl Default for MainView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for MainView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Live mode: publish the latest state so `prepare_restart` can
        // snapshot it before the CLI relaunches the app.
        #[cfg(debug_assertions)]
        crate::live::publish_state(&crate::live::snapshot_json_number("clicks", self.clicks));

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
                    .child({{APP_TITLE_RUST}}),
            )
            .child(
                div()
                    .text_sm()
                    .child("Cross-platform Desktop & Mobile app powered by GPUI"),
            )
            .child(div().text_lg().child(format!("Clicked {} times", self.clicks)))
            .child(
                div()
                    .id("increment")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x2563eb))
                    .text_color(rgb(0xffffff))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x1d4ed8)))
                    .child("Click me")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.clicks += 1;
                        cx.notify();
                    })),
            )
    }
}

// ── Mobile entry points ──────────────────────────────────────────────────────
//
// Both platforms open the same `MainView`; only how the run loop is started
// differs. The `gpui_mobile` crate supplies the platform implementation.

{{MOBILE_ENTRY}}
