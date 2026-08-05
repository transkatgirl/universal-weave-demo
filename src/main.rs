//! A Loom-style demo application for the `universal-weave` library, built with eframe.

mod app;
mod cone_view;
mod content;
mod document;
mod persistence;
mod tree_view;

use app::DemoApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "universal-weave-demo",
        options,
        Box::new(|cc| Ok(Box::new(DemoApp::new(cc)))),
    )
}
