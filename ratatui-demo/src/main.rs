//! Interactive Ratatui demo for `universal-weave`.

mod app;
mod content;
mod document;
mod persistence;
mod tree_view;

use std::io;

use app::App;

fn main() -> io::Result<()> {
    ratatui::run(|terminal| App::new().run(terminal))
}
