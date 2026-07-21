use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use common::HOST_PORT;
use gpu_video::{
    VulkanDevice, VulkanInstance,
    parameters::{VulkanAdapterDescriptor, VulkanDeviceDescriptor},
};
use log::info;
use slint::{ComponentHandle, SharedString};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod common;
mod gpu;
mod net;
mod streamer;
mod watcher;

slint::include_modules!();

// NOTE: the wgpu Slint backend causes an error on program exit:
// "cannot access a Thread Local Storage value during or after destruction: AccessError"
// this may be fixed in a future wgpu/slint release, but it is not harmful for now

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

fn init_backend() -> anyhow::Result<Arc<VulkanDevice>> {
    // TODO: integrate Slint's preferred options for creating instance, adapter, device, and queue
    info!("Creating Vulkan instance");
    let instance = VulkanInstance::new()?;
    info!("Creating Vulkan adapter");
    let adapter = instance.create_adapter(&VulkanAdapterDescriptor::default())?;
    info!("Creating Vulkan device");
    let device = adapter.create_device(&VulkanDeviceDescriptor::default())?;
    info!("Creating Slint backend from Vulkan objects");
    slint::BackendSelector::new()
        .backend_name("winit".to_string())
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::Manual {
            instance: instance.wgpu_instance(),
            adapter: device.wgpu_adapter(),
            device: device.wgpu_device(),
            queue: device.wgpu_queue(),
        })
        .select()?;
    Ok(device)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let device = init_backend()?;
    let device2 = device.clone();

    info!("Creating app");
    let app = App::new()?;
    app.on_is_socket_address(|text| parse_socket_address(&text, 0).is_ok());

    let weak = app.as_weak();
    let weak2 = app.as_weak();

    // TODO: stop signals
    app.on_start_host(move |role| {
        let device = device.clone();
        let future = match net::host_server(weak.clone()) {
            Ok(f) => f,
            Err(err) => return err.to_string().into(),
        };
        let weak = weak.clone();
        // FIXME: do not ignore errors
        tokio::task::spawn(async move {
            let (conn, mut control_stream) = future.await?;
            control_stream.send_role(role).await?;
            info!("Connected");
            match role {
                // TODO: show error message from start (if any) to user
                Role::Streamer => streamer::start(device, conn),
                Role::Watcher => watcher::start(weak, device, conn),
            }
        });
        "".into()
    });
    app.on_start_client(move |host_addr_str| {
        fn wrap_err(err: impl ToString) -> (SharedString, Role) {
            (err.to_string().into(), Role::Watcher) // role is ignored when err != ""
        }
        let host_addr = match parse_socket_address(&host_addr_str, HOST_PORT) {
            Ok(ok) => ok,
            Err(err) => return wrap_err(err),
        };
        let device = device2.clone();
        let future = match net::connect_to_server(weak2.clone(), host_addr) {
            Ok(f) => f,
            Err(err) => return wrap_err(err),
        };
        let weak = weak2.clone();
        let result: anyhow::Result<Role> = tokio::runtime::Handle::current().block_on(async move {
            let (connection, mut control_stream) = future.await?;
            info!("Connected");
            let host_role = control_stream.recv_role().await?;
            let role = match host_role {
                Role::Streamer => Role::Watcher,
                Role::Watcher => Role::Streamer,
            };
            match role {
                // TODO: show error message from start (if any) to user
                Role::Streamer => streamer::start(device, connection)?,
                Role::Watcher => watcher::start(weak, device, connection)?,
            }
            Ok(role)
        });
        match result {
            Ok(role) => ("".into(), role),
            Err(err) => wrap_err(err),
        }
    });

    info!("Running app");
    tokio::task::block_in_place(|| {
        app.run()?;
        Ok(())
    })
}
