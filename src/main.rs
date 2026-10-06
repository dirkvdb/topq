#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod appearance;
mod config;
mod mqtt;
mod topics;
mod ui;

use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    if let Err(error) = tracing_subscriber::fmt().with_env_filter(filter).try_init() {
        eprintln!("Could not initialize console logging: {error}");
    }
}

#[hotpath::main]
fn main() {
    init_logging();
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
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
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(760.)), cx))),
                window_min_size: Some(size(px(760.), px(540.))),
                ..gpui_kit::component::TitleBar::window_options()
            },
            cx,
            |window, cx| {
                window.set_window_title("TopQ");
                cx.new(|cx| ui::Explorer::new(window, cx))
            },
        ) {
            tracing::error!(error = %format_args!("{error:#}"), "Could not open the TopQ window");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
