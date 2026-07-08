use log::info;
use slint::{ComponentHandle, Weak, wgpu_29::WGPUSettings};

mod client;
mod common;
mod server;
mod tcp;

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
    pretty_env_logger::init();

    slint::BackendSelector::new()
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::Automatic(WGPUSettings::default()))
        .select()
        .unwrap();

    let app = App::new().unwrap();
    server::setup(&app);
    client::setup(&app);
    info!("Created app");
    app.run().unwrap();
}
