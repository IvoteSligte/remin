use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

use common::{Packet, SERVER_PORT};
use gpu_video::{
    VulkanDevice, VulkanInstance,
    parameters::{VulkanAdapterDescriptor, VulkanDeviceDescriptor},
};
use log::{error, info};
use netnet::Connection;
use slint::{ComponentHandle, Weak};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod caster;
mod common;
mod gpu;
mod net;
mod viewer;

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
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::Manual {
            instance: instance.wgpu_instance(),
            adapter: device.wgpu_adapter(),
            device: device.wgpu_device(),
            queue: device.wgpu_queue(),
        })
        .select()?;
    Ok(device)
}

fn on_connect(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    connection: Arc<Connection>,
) -> impl Future<Output = anyhow::Result<()>> {
    info!("Connected; running caster/viewer selection checks");

    let (claim_sender, claim_receiver) = tokio::sync::oneshot::channel::<()>();
    let device2 = device.clone();
    let connection2 = connection.clone();
    let on_share_screen = Arc::new(Mutex::new(Some(move || {
        info!("Starting caster");
        claim_sender.send(()).unwrap();
        info!("Acquired variables");
        let error = match caster::start(device2, connection2) {
            Ok(()) => "".into(),
            Err(err) => err.to_string().into(),
        };
        error
    })));
    let on_share_screen2 = on_share_screen.clone();
    weak.upgrade_in_event_loop(move |app| {
        app.on_share_screen(move || {
            if let Some(callback) = on_share_screen.lock().unwrap().take() {
                callback()
            } else {
                "'Share Screen' pressed twice".into()
            }
        });
        info!("Set on-share-screen callback");
    })
    .unwrap();
    // Checks if the user should become a viewer depending on received packets
    async move {
        tokio::select! {
            _ = claim_receiver => { Ok(()) }
            result = async {loop {
                let bytes = connection.read_datagram().await?;
                let packet: Packet = wincode::deserialize(&bytes)?;
                match packet {
                    Packet::Input(_) => {
                        error!("Received input from peer without sharing screen");
                    }
                    // Drops a single packet, but the stream is known to be lossy anyways so will recover.
                    // TODO: no longer drop this packet: either send a bunch of Packet::H264Marker before
                    // starting the screencasting or use a reliable QUIC stream to send a control signal
                    Packet::H264 { .. } => {
                        info!("Received video packet from peer; starting viewer");
                        on_share_screen2.lock().unwrap().take();
                        // screen was shared by peer, which implies that this user should be the viewer
                        break viewer::start(weak, device, connection);
                    }
                }
            }} => {result}
        }
    }
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
    app.on_start_server(move || {
        let device = device.clone();
        let future = match net::connect_server(weak.clone()) {
            Ok(f) => f,
            Err(err) => return err.to_string().into(),
        };
        let weak = weak.clone();
        let _: slint::JoinHandle<anyhow::Result<()>> =
            slint::spawn_local(async_compat::Compat::new(async move {
                let connection = future.await?;
                on_connect(weak, device, connection).await
            }))
            .unwrap();
        "".into()
    });
    app.on_start_client(move |server_addr_str| {
        match parse_socket_address(&server_addr_str, SERVER_PORT) {
            Ok(server_addr) => {
                // TODO: handle errors instead of unwrapping
                let device = device2.clone();
                let future = match net::connect_client(weak2.clone(), server_addr) {
                    Ok(f) => f,
                    Err(err) => return err.to_string().into(),
                };
                let weak = weak2.clone();
                let _: slint::JoinHandle<anyhow::Result<()>> =
                    slint::spawn_local(async_compat::Compat::new(async move {
                        let connection = future.await?;
                        on_connect(weak, device, connection).await
                    }))
                    .unwrap();
                "".into()
            }
            Err(err) => err.to_string().into(),
        }
    });

    info!("Running app");
    tokio::task::block_in_place(|| {
        app.run()?;
        Ok(())
    })
}
