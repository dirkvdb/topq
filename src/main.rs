//! A read-only MQTT explorer with a native GPUI Kit interface.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod appearance;
mod config;
mod mqtt;
mod topics;
mod ui;

use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            appearance::init(cx);
            ui::init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            // Window bounds are platform geometry in logical pixels; content uses rem.
            if let Err(error) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1200.), px(760.)),
                        cx,
                    ))),
                    window_min_size: Some(size(px(760.), px(540.))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_window_title("MQTT UI");
                    cx.new(|cx| ui::Explorer::new(window, cx))
                },
            ) {
                eprintln!("Could not open the MQTT UI window: {error:#}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}
