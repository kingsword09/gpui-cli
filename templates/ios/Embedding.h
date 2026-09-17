#import <UIKit/UIKit.h>
#include <stdbool.h>

// GPUI's iOS FFI, as exported by `gpui-pre-mobile`. Declared here rather than
// pulled from gpui_ios.h so it stays in sync with what App.swift uses.

// Lifecycle: register the Rust root view, then start the run loop.
void gpui_ios_register_app(void);
void gpui_ios_run_demo(void);
void gpui_ios_set_embedded(void);

// Window geometry and frame delivery.
void *gpui_ios_get_window(void);
void *gpui_ios_view_controller(void *window);
void gpui_ios_layout_view(void *window);
bool gpui_ios_request_frame(void *window);
void gpui_ios_set_frame_waker(void *window, void (*waker)(void *context), void *context);

// UIApplicationDelegate callbacks forwarded into GPUI.
void gpui_ios_did_become_active(void *app);
void gpui_ios_will_resign_active(void *app);
void gpui_ios_did_enter_background(void *app);
void gpui_ios_will_terminate(void *app);
