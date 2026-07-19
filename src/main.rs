use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::bail;
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

fn on_share_screen_hook(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    conn: Arc<Mutex<Option<Connection>>>,
) {
    let weak2 = weak.clone();
    weak.upgrade_in_event_loop(move |app| {
        app.on_share_screen(move || {
            weak2
                .upgrade_in_event_loop(|app| {
                    app.on_share_screen(|| "'Share Screen' pressed twice".into());
                })
                .unwrap();

            info!("Starting caster");
            let Some(conn) = conn.lock().unwrap().take() else {
                return "Peer already selected 'Share Screen'".into();
            };
            info!("Acquired state");
            let error = match caster::start(device.clone(), conn) {
                Ok(()) => "".into(),
                Err(err) => err.to_string().into(),
            };
            error
        });
        info!("Set on-share-screen callback");
    })
    .unwrap();
}

// Checks if the user should become a viewer based on received packets
fn on_caster_packet_hook(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    conn: Arc<Mutex<Option<Connection>>>,
) -> anyhow::Result<()> {
    loop {
        // sleep for 1 ms to give the other thread time to lock the mutex
        std::thread::sleep(Duration::from_millis(1));
        println!("LOOP"); // DEBUG
        let mut conn_guard = conn.lock().unwrap();
        let Some(conn) = conn_guard.as_mut() else {
            drop(conn_guard);
            return Ok(());
        };
        let bytes = match conn
            .unreliable_receiver
            .recv_timeout(Duration::from_millis(1))
        {
            Ok(bytes) => bytes,
            Err(netnet::RecvTimeoutError::Timeout) => continue,
            Err(netnet::RecvTimeoutError::Disconnected) => {
                bail!("Unreliable receiver channel closed")
            }
        };
        let packet: Packet = wincode::deserialize(&bytes)?;
        match packet {
            Packet::Input(_) => {
                error!("Received input from peer without sharing screen");
            }
            // Drops a single packet if Packet::H264 is found, which is likely to be the PicParamSet packet.
            // TODO: Send PicParamSet and "IAmCaster" signal over reliable QUIC stream
            Packet::IAmCaster | Packet::H264 { .. } => {
                info!("Received video packet from peer; starting viewer");
                weak.upgrade_in_event_loop(|app| {
                    app.on_share_screen(|| "Peer already selected caster".into());
                })
                .unwrap();
                // screen was shared by peer, which implies that this user should be the viewer
                break viewer::start(weak, device, conn_guard.take().unwrap());
            }
        }
    }
}

fn on_connect(weak: Weak<App>, device: Arc<VulkanDevice>, conn: Connection) -> anyhow::Result<()> {
    info!("Connected; running caster/viewer selection checks");

    let conn = Arc::new(Mutex::new(Some(conn)));

    on_share_screen_hook(weak.clone(), device.clone(), conn.clone());
    on_caster_packet_hook(weak, device, conn)?;

    Ok(())
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
                let conn = future.await?;
                on_connect(weak, device, conn)
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
                        on_connect(weak, device, connection)
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
