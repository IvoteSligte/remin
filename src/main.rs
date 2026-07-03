use log::info;
use slint::{ComponentHandle, Weak};

mod client;
mod common;
mod server;

slint::include_modules!();

pub fn setup_menu(weak: &Weak<App>) {
    weak.upgrade_in_event_loop(|app| {
        let weak = app.as_weak();
        app.on_escape(move || {
            // exits if there is only one window, which is always the case
            weak.unwrap().window().hide().unwrap();
        });
    })
    .unwrap();
}

// TODO: F11 for fullscreen

fn main() {
    unsafe {
        // prevent installing ffprobe and ffplay
        std::env::set_var("KEEP_ONLY_FFMPEG", "true");
    }
    ffmpeg_sidecar::download::auto_download().unwrap();

    pretty_env_logger::init();
    let app = App::new().unwrap();
    server::setup(&app);
    client::setup(&app);
    info!("Created app");
    app.run().unwrap();
}
