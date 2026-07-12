use std::{
    io,
    net::{IpAddr, SocketAddr},
};

use gpu_video::{
    VulkanInstance,
    parameters::{VulkanAdapterDescriptor, VulkanDeviceDescriptor},
};
use log::info;
use slint::{ComponentHandle, Weak};

mod client;
mod common;
mod server;

slint::include_modules!();

// NOTE: the wgpu Slint backend causes an error on program exit:
// "cannot access a Thread Local Storage value during or after destruction: AccessError"
// this may be fixed in a future wgpu/slint release, but it is not harmful for now

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

fn parse_socket_address(text: &str, default_port: u16) -> io::Result<SocketAddr> {
    text.parse::<SocketAddr>().or_else(|_| {
        text.parse::<IpAddr>()
            .map(|ip| SocketAddr::new(ip, default_port))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid ip/socket address syntax",
                )
            })
    })
}

fn main() {
    pretty_env_logger::init();

    // TODO: integrate Slint's preferred options for creating instance, adapter, device, and queue
    info!("Creating Vulkan instance");
    let instance = VulkanInstance::new().unwrap();
    info!("Creating Vulkan adapter");    
    let adapter = instance
        .create_adapter(&VulkanAdapterDescriptor::default())
        .unwrap();
    info!("Creating Vulkan device");    
    let device = adapter
        .create_device(&VulkanDeviceDescriptor::default())
        .unwrap();
    info!("Creating Slint backend from Vulkan objects");    
    slint::BackendSelector::new()
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::Manual {
            instance: instance.wgpu_instance(),
            adapter: device.wgpu_adapter(),
            device: device.wgpu_device(),
            queue: device.wgpu_queue(),
        })
        .select()
        .unwrap();
    info!("Creating app");        

    let app = App::new().unwrap();
    app.on_is_socket_address(|text| parse_socket_address(&text, 0).is_ok());
    setup_menu(&app.as_weak());
    server::setup(&app, device.clone());
    client::setup(&app, device.clone());
    info!("Running app");
    app.run().unwrap();
}
