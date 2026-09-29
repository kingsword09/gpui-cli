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
pub mod previews;

pub(crate) fn preview_env(name: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    {
        return live::preview_env(name);
    }
    #[cfg(not(debug_assertions))]
    {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

/// Starts the explicit preview registry and, when `gpui preview` supplied a
/// scenario, creates its isolated fixture state before the first view.
pub fn initialize_preview() {
    previews::initialize();
}

/// Reports a scenario generation to the CLI dev channel. Release builds keep
/// the same call site but do not retain the development transport.
pub fn report_scenario_ready(
    scenario_id: &str,
    component: &str,
    fixture_hash: &str,
    environment_json: &str,
    reset_generation: u64,
    data_dir: &str,
    uncontrolled_inputs_json: &str,
) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    live::report_scenario_ready(
        scenario_id,
        component,
        fixture_hash,
        environment_json,
        reset_generation,
        data_dir,
        uncontrolled_inputs_json,
    );
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let _ = (
        scenario_id,
        component,
        fixture_hash,
        environment_json,
        reset_generation,
        data_dir,
        uncontrolled_inputs_json,
    );
}

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

/// Registers a generated window with the live supervisor in debug builds.
/// Release builds keep the same call site as the generated development entry
/// point, but omit the dev channel entirely.
pub fn register_window(
    window_id: &str,
    title: &str,
    width: u32,
    height: u32,
    scale_milli: u32,
    foreground: bool,
) {
    #[cfg(debug_assertions)]
    live::register_window(window_id, title, width, height, scale_milli, foreground);
    #[cfg(not(debug_assertions))]
    let _ = (window_id, title, width, height, scale_milli, foreground);
}

/// Registers a generated window together with its GPUI handle so the live
/// runtime can perform semantics reads on the UI thread.
pub fn register_window_with_handle(
    window_id: &str,
    title: &str,
    width: u32,
    height: u32,
    scale_milli: u32,
    foreground: bool,
    handle: AnyWindowHandle,
) {
    #[cfg(debug_assertions)]
    live::register_window_with_handle(
        window_id,
        title,
        width,
        height,
        scale_milli,
        foreground,
        handle,
    );
    #[cfg(not(debug_assertions))]
    let _ = (
        window_id,
        title,
        width,
        height,
        scale_milli,
        foreground,
        handle,
    );
}

/// Reports a generated window closing to the live supervisor in debug builds.
pub fn close_window(window_id: &str, reason: Option<&str>) {
    #[cfg(debug_assertions)]
    live::close_window(window_id, reason);
    #[cfg(not(debug_assertions))]
    let _ = (window_id, reason);
}

/// Reports a completed GPUI scene for a content revision. A non-null
/// presented frame id is only valid when the platform backend has verified the
/// presentation; scene completion alone does not imply screen presentation.
pub fn report_scene_completed(
    window_id: &str,
    scene_epoch: u64,
    source_revision: u64,
    asset_revision: u64,
    presented_frame_id: Option<&str>,
) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    live::report_scene_completed(
        window_id,
        scene_epoch,
        source_revision,
        asset_revision,
        presented_frame_id,
    );
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let _ = (
        window_id,
        scene_epoch,
        source_revision,
        asset_revision,
        presented_frame_id,
    );
}

/// Reports the scene after the current GPUI frame is rendered. The live
/// adapter never invents a presented-frame id; only a verified backend may
/// supply one.
pub fn schedule_scene_completed(window: &Window, window_id: &str) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    {
        let _ = live::schedule_scene_completed(window, window_id);
    }
    #[cfg(not(all(debug_assertions, feature = "gpui-dev")))]
    let _ = (window, window_id);
}

/// Under `gpui run --live`, drains asset changes and UI probes received by the
/// dev channel. Asset invalidation and probe responses are completed from the
/// GPUI foreground context so the network thread never touches UI state.
/// Call once from the app's run closure.
pub fn pump_live_assets(cx: &mut App) {
    #[cfg(all(debug_assertions, feature = "gpui-dev"))]
    {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                    .await;
                let asset_events = crate::live::take_asset_events();
                let probes = crate::live::take_ui_probe_requests();
                let semantics = crate::live::take_semantics_read_requests();
                let resets = crate::live::take_preview_reset_requests();
                let actions = crate::live::take_pointer_action_requests();
                let keyboard_actions = crate::live::take_keyboard_action_requests();
                if asset_events.is_empty()
                    && probes.is_empty()
                    && semantics.is_empty()
                    && resets.is_empty()
                    && actions.is_empty()
                    && keyboard_actions.is_empty()
                {
                    continue;
                }
                cx.update(|cx| {
                    let mut asset_batches = std::collections::BTreeMap::<
                        (String, u64),
                        (Vec<String>, Vec<String>),
                    >::new();
                    let mut has_asset_changes = false;
                    for event in asset_events {
                        let transfer_id = event.transfer_id;
                        let batch = asset_batches
                            .entry((transfer_id.clone(), event.asset_revision))
                            .or_default();
                        if event.failed {
                            batch.1.push(event.path);
                            continue;
                        }
                        let path = event.path;
                        if event.removed {
                            #[cfg(target_os = "ios")]
                            {
                                let target = std::env::temp_dir()
                                    .join("gpui-assets")
                                    .join(&path);
                                if let Err(error) = std::fs::remove_file(&target) {
                                    if error.kind() != std::io::ErrorKind::NotFound {
                                        batch.1.push(path);
                                        continue;
                                    }
                                }
                            }
                        }
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
                        crate::live::mark_asset_applied(&transfer_id, &path, event.removed);
                        batch.0.push(path);
                        has_asset_changes = true;
                    }
                    for ((transfer_id, asset_revision), (applied, failed)) in asset_batches {
                        crate::live::report_assets_applied(
                            &transfer_id,
                            asset_revision,
                            &applied,
                            &failed,
                            !applied.is_empty(),
                        );
                    }
                    let has_windows = !cx.windows().is_empty();
                    for probe in probes {
                        let responsive = has_windows
                            && crate::live::window_is_registered(&probe.window_id);
                        let latency_ms = u64::try_from(probe.queued_at.elapsed().as_millis())
                            .unwrap_or(u64::MAX);
                        crate::live::respond_ui_probe(
                            &probe.request_id,
                            &probe.window_id,
                            responsive,
                            Some(latency_ms),
                        );
                    }
                    for request in semantics {
                        crate::live::answer_semantics_read(cx, request);
                    }
                    for request in actions {
                        crate::live::dispatch_pointer_action(cx, request);
                    }
                    for request in keyboard_actions {
                        crate::live::dispatch_keyboard_action(cx, request);
                    }
                    let mut has_preview_reset = false;
                    for request in resets {
                        match crate::previews::reset_generation_for(&request.scenario_id) {
                            Ok(generation) => {
                                has_preview_reset = true;
                                crate::live::respond_scenario_reset(
                                    &request.request_id,
                                    &request.scenario_id,
                                    true,
                                    generation,
                                    None,
                                );
                            }
                            Err(error) => crate::live::respond_scenario_reset(
                                &request.request_id,
                                &request.scenario_id,
                                false,
                                0,
                                Some(&error),
                            ),
                        }
                    }
                    if has_asset_changes || has_preview_reset {
                        cx.refresh_windows();
                    }
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

/// Declares a stable logical id for a named GPUI element in debug semantic
/// observations. The mapping is explicit; the runtime never promotes `.id()`
/// or a debug `element_id` by itself.
pub fn declare_logical_id(element_id: &str, logical_id: &str) -> Result<(), &'static str> {
    #[cfg(debug_assertions)]
    {
        live::declare_logical_id(element_id, logical_id)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (element_id, logical_id);
        Ok(())
    }
}

/// Clears process-local logical-id declarations before constructing a fresh
/// scenario in debug builds.
pub fn clear_declared_logical_ids() {
    #[cfg(debug_assertions)]
    live::clear_declared_logical_ids();
}

pub fn begin_action_bounds_frame() {
    #[cfg(debug_assertions)]
    live::begin_action_bounds_frame();
}

/// Declares a logical node as a scrollable container for observation-bound
/// scroll actions. The node must also provide a normal GPUI wheel listener.
pub fn declare_scroll_target(logical_id: &str) {
    #[cfg(debug_assertions)]
    live::declare_scroll_target(logical_id);
    #[cfg(not(debug_assertions))]
    let _ = logical_id;
}

pub fn record_action_target_bounds(
    logical_id: &str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    enabled: bool,
) {
    #[cfg(debug_assertions)]
    live::record_action_target_bounds(logical_id, x, y, width, height, enabled);
    #[cfg(not(debug_assertions))]
    let _ = (logical_id, x, y, width, height, enabled);
}

/// Records bounds for a scroll target. This named alias makes generic
/// scroll-container instrumentation distinct from click-only controls while
/// retaining the same bounded observation field.
pub fn record_scroll_target_bounds(
    logical_id: &str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    enabled: bool,
) {
    record_action_target_bounds(logical_id, x, y, width, height, enabled);
}

pub fn confirm_action_target_hit(logical_id: &str) {
    #[cfg(debug_assertions)]
    live::confirm_action_target_hit(logical_id);
    #[cfg(not(debug_assertions))]
    let _ = logical_id;
}

/// Confirms that a scroll event reached the target's normal GPUI wheel path.
pub fn confirm_scroll_target_hit(logical_id: &str) {
    #[cfg(debug_assertions)]
    live::confirm_scroll_target_hit(logical_id);
    #[cfg(not(debug_assertions))]
    let _ = logical_id;
}

/// Registers a focusable element as an explicit keyboard action target.
pub fn register_keyboard_target(logical_id: &str, focus_handle: FocusHandle) {
    #[cfg(debug_assertions)]
    live::register_keyboard_target(logical_id, focus_handle);
    #[cfg(not(debug_assertions))]
    let _ = (logical_id, focus_handle);
}

/// Confirms that a keyboard event reached the generated target through GPUI.
pub fn confirm_keyboard_target_hit(logical_id: &str) {
    #[cfg(debug_assertions)]
    live::confirm_keyboard_target_hit(logical_id);
    #[cfg(not(debug_assertions))]
    let _ = logical_id;
}

/// Root view of the application.
///
/// The generated preview surfaces are intentionally small and deterministic.
/// They provide the registry/runtime with real GPUI elements and stable
/// semantic ids; the development adapter wires the explicitly instrumented
/// pointer and keyboard targets through GPUI's normal event path. Scrolling
/// and asynchronous form execution remain separate capabilities.
pub struct MainView {
    clicks: usize,
    preview_generation: Option<u64>,
    login_username: String,
    login_password: String,
    login_error: Option<String>,
    login_username_focus: Option<FocusHandle>,
    login_password_focus: Option<FocusHandle>,
    login_username_selected_all: bool,
    login_password_selected_all: bool,
    virtual_list_scroll_handle: UniformListScrollHandle,
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
        // The debug adapter only exports declarations made through this API;
        // `.id("increment")` remains a GPUI element identity, not a logical id.
        #[cfg(debug_assertions)]
        {
            let _ = crate::declare_logical_id("counter-value", "counter.value");
            let _ = crate::declare_logical_id("increment", "counter.increment");
            let _ = crate::declare_logical_id("login-username", "login.username");
            let _ = crate::declare_logical_id("login-password", "login.password");
            let _ = crate::declare_logical_id("login-submit", "login.submit");
            let _ = crate::declare_logical_id("login-error", "login.error");
            let _ = crate::declare_logical_id("list-viewport", "list.viewport");
        }

        // Live mode: restore the snapshot the previous process published.
        // Unparseable data means a cold start — never a boot failure.
        let clicks = crate::previews::counter_initial_value()
            .or_else(|| {
                restored_state().and_then(|json| {
                    json.split("\"clicks\":")
                        .nth(1)
                        .and_then(|rest| rest.split('}').next())
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
            })
            .unwrap_or(0);
        let virtual_list_scroll_handle = UniformListScrollHandle::new();
        let initial_scroll_y = crate::previews::virtual_list_initial_scroll_y().unwrap_or(0) as f32;
        virtual_list_scroll_handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.0), px(-initial_scroll_y)));
        Self {
            clicks,
            preview_generation: crate::previews::active_reset_generation(),
            login_username: crate::previews::login_initial_username().unwrap_or_default(),
            login_password: crate::previews::login_initial_password().unwrap_or_default(),
            login_error: None,
            login_username_focus: None,
            login_password_focus: None,
            login_username_selected_all: false,
            login_password_selected_all: false,
            virtual_list_scroll_handle,
        }
    }

    fn reset_preview_state(&mut self) {
        self.clicks = crate::previews::counter_initial_value().unwrap_or(0);
        self.login_username = crate::previews::login_initial_username().unwrap_or_default();
        self.login_password = crate::previews::login_initial_password().unwrap_or_default();
        self.login_error = None;
        self.login_username_selected_all = false;
        self.login_password_selected_all = false;
        let initial_scroll_y = crate::previews::virtual_list_initial_scroll_y().unwrap_or(0) as f32;
        self.virtual_list_scroll_handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.0), px(-initial_scroll_y)));
    }

    fn render_counter(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = crate::previews::counter_disabled().unwrap_or(false);
        let value = div()
            .id("counter-value")
            .accessibility_id("counter.value")
            .text_lg()
            .child(format!("Clicked {} times", self.clicks));
        let button = if disabled {
            div()
                .id("increment")
                .accessibility_id("counter.increment")
                .relative()
                .child(
                    canvas(
                        |bounds, _, _| {
                        crate::record_action_target_bounds(
                            "counter.increment",
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                            false,
                        );
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(0x94a3b8))
                .text_color(rgb(0xffffff))
                .child("Disabled")
                .into_any_element()
        } else {
            div()
                .id("increment")
                .accessibility_id("counter.increment")
                .relative()
                .on_mouse_up(MouseButton::Left, |_event, _window, _cx| {
                    crate::confirm_action_target_hit("counter.increment");
                })
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(0x2563eb))
                .text_color(rgb(0xffffff))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x1d4ed8)))
                .child("Click me")
                .on_click(cx.listener(|this, _event, _window, cx| {
                    let increment = crate::previews::counter_increment_by().unwrap_or(1);
                    this.clicks = this.clicks.saturating_add(increment);
                    cx.notify();
                }))
                .child(
                    canvas(
                        |bounds, _, _| {
                        crate::record_action_target_bounds(
                            "counter.increment",
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                            true,
                        );
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .child(value)
            .child(button)
            .into_any_element()
    }

    fn apply_login_key(&mut self, logical_id: &str, event: &KeyDownEvent) {
        let is_username = logical_id == "login.username";
        let value = if is_username {
            &mut self.login_username
        } else {
            &mut self.login_password
        };
        let selected_all = if is_username {
            &mut self.login_username_selected_all
        } else {
            &mut self.login_password_selected_all
        };
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if (modifiers.control || modifiers.platform) && key == "a" {
            *selected_all = true;
            return;
        }
        if key == "backspace" || key == "delete" {
            if *selected_all {
                value.clear();
                *selected_all = false;
            } else {
                value.pop();
            }
            return;
        }
        if key == "enter" {
            self.login_error = crate::previews::login_error_message();
            return;
        }
        if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
            return;
        }
        if let Some(text) = event.keystroke.key_char.as_deref() {
            if !text.is_empty() {
                if *selected_all {
                    value.clear();
                    *selected_all = false;
                }
                value.push_str(text);
            }
        }
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let username_focus = self
            .login_username_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let password_focus = self
            .login_password_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        crate::register_keyboard_target("login.username", username_focus.clone());
        crate::register_keyboard_target("login.password", password_focus.clone());
        let password_display = if self.login_password.is_empty() {
            "(empty)".to_string()
        } else {
            "•".repeat(self.login_password.chars().count())
        };
        let username = div()
            .id("login-username")
            .accessibility_id("login.username")
            .relative()
            .track_focus(&username_focus)
            .on_key_down(cx.listener(|this, event, _window, cx| {
                crate::confirm_keyboard_target_hit("login.username");
                this.apply_login_key("login.username", event);
                cx.notify();
            }))
            .px_3()
            .py_2()
            .bg(rgb(0xf1f5f9))
            .child(format!("Username: {}", self.login_username))
            .child(
                canvas(
                    |bounds, _, _| {
                        crate::record_action_target_bounds(
                            "login.username",
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                            true,
                        );
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let password = div()
            .id("login-password")
            .accessibility_id("login.password")
            .relative()
            .track_focus(&password_focus)
            .on_key_down(cx.listener(|this, event, _window, cx| {
                crate::confirm_keyboard_target_hit("login.password");
                this.apply_login_key("login.password", event);
                cx.notify();
            }))
            .px_3()
            .py_2()
            .bg(rgb(0xf1f5f9))
            .child(format!("Password: {password_display}"))
            .child(
                canvas(
                    |bounds, _, _| {
                        crate::record_action_target_bounds(
                            "login.password",
                            bounds.origin.x.as_f32(),
                            bounds.origin.y.as_f32(),
                            bounds.size.width.as_f32(),
                            bounds.size.height.as_f32(),
                            true,
                        );
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let submit = div()
            .id("login-submit")
            .accessibility_id("login.submit")
            .relative()
            .on_mouse_up(MouseButton::Left, |_event, _window, _cx| {
                crate::confirm_action_target_hit("login.submit");
            })
            .px_4()
            .py_2()
            .rounded_md()
            .bg(rgb(0x2563eb))
            .text_color(rgb(0xffffff))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x1d4ed8)))
            .child("Sign in")
            .on_click(cx.listener(|this, _event, _window, cx| {
                // The fixture response is deterministic. Async request
                // cancellation and fencing remain separate capabilities.
                this.login_error = crate::previews::login_error_message();
                cx.notify();
            }))
            .child(
                canvas(
                    |bounds, _, _| {
                    crate::record_action_target_bounds(
                        "login.submit",
                        bounds.origin.x.as_f32(),
                        bounds.origin.y.as_f32(),
                        bounds.size.width.as_f32(),
                        bounds.size.height.as_f32(),
                        true,
                    );
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );

        let mut form = div()
            .flex()
            .flex_col()
            .w(px(360.0))
            .gap_3()
            .child(div().text_lg().child("LoginForm"))
            .child(username)
            .child(password)
            .child(submit);
        if let Some(message) = &self.login_error {
            form = form.child(
                div()
                    .id("login-error")
                    .accessibility_id("login.error")
                    .text_color(rgb(0xb91c1c))
                    .child(message.clone()),
            );
        }
        form.into_any_element()
    }

    fn render_virtual_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        crate::declare_scroll_target("list.viewport");
        let count = crate::previews::virtual_list_item_count().unwrap_or(0);
        let prefix = crate::previews::virtual_list_stable_key_prefix().unwrap_or_default();
        let digits = crate::previews::virtual_list_stable_key_digits().unwrap_or(1);
        let label_prefix = crate::previews::virtual_list_label_prefix().unwrap_or_default();
        let row_height = crate::previews::virtual_list_row_height().unwrap_or(32) as f32;
        let scroll_y = -self
            .virtual_list_scroll_handle
            .0
            .borrow()
            .base_handle
            .offset()
            .y
            .as_f32();
        let list = uniform_list(
            "list-items",
            count,
            cx.processor(move |_this, range, _window, _cx| {
                let mut items = Vec::new();
                for index in range {
                    let key = format!("{prefix}{index:0width$}", width = digits);
                    let element_id = format!("list-item-{index}");
                    let logical_id = format!("list.item.{key}");
                    let _ = crate::declare_logical_id(&element_id, &logical_id);
                    items.push(
                        div()
                            .id(element_id)
                            .accessibility_id(logical_id)
                            .h(px(row_height))
                            .px_3()
                            .justify_center()
                            .child(format!("{label_prefix}{index}")),
                    );
                }
                items
            }),
        )
        .track_scroll(&self.virtual_list_scroll_handle)
        .h_full();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .child(format!("VirtualList ({count} items, scroll y={scroll_y:.0})")),
            )
            .child(
                div()
                    .id("list-viewport")
                    .accessibility_id("list.viewport")
                    .relative()
                    .h(px(360.0))
                    .overflow_hidden()
                    .on_scroll_wheel(|_event, _window, _cx| {
                        crate::confirm_scroll_target_hit("list.viewport");
                    })
                    .child(list)
                    .child(
                        canvas(
                            |bounds, _, _| {
                                crate::record_scroll_target_bounds(
                                    "list.viewport",
                                    bounds.origin.x.as_f32(),
                                    bounds.origin.y.as_f32(),
                                    bounds.size.width.as_f32(),
                                    bounds.size.height.as_f32(),
                                    true,
                                );
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
            .into_any_element()
    }
}

impl Default for MainView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_generation = crate::previews::active_reset_generation();
        if current_generation != self.preview_generation {
            self.reset_preview_state();
            self.preview_generation = current_generation;
        }
        // Live mode: publish the latest state so `prepare_restart` can
        // snapshot it before the CLI relaunches the app.
        #[cfg(debug_assertions)]
        crate::begin_action_bounds_frame();
        #[cfg(debug_assertions)]
        crate::live::publish_state(&crate::live::snapshot_json_number("clicks", self.clicks));

        let content = match crate::previews::active_component().as_deref() {
            Some("LoginForm") => self.render_login(cx),
            Some("VirtualList") => self.render_virtual_list(cx),
            _ => self.render_counter(cx),
        };

        #[cfg(debug_assertions)]
        crate::schedule_scene_completed(window, "main");

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
            .child(content)
    }
}

// ── Mobile entry points ──────────────────────────────────────────────────────
//
// Both platforms open the same `MainView`; only how the run loop is started
// differs. The `gpui_mobile` crate supplies the platform implementation.

{{MOBILE_ENTRY}}
