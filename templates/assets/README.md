# Assets

Files in this directory are bundled into your app through the dev asset
source: reference them by path, e.g. `img("assets/logo.png")` in a view.

With `gpui run --live`, images here hot-reload in the running app without a
rebuild — save the file and the change appears in under a second. On Android
the CLI pushes changed assets to the device before notifying the app; the iOS
runner cannot install a dev asset source, so it picks up asset changes on the
next rebuild instead.
