use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use common::{Packet, SERVER_PORT};
use gpu_video::{
    VulkanDevice, VulkanInstance,
    parameters::{VulkanAdapterDescriptor, VulkanDeviceDescriptor},
};
use log::{info, warn};
use netnet::Signal;
use slint::{ComponentHandle, Weak};

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
) -> impl FnOnce(netnet::Sender, netnet::Receiver) -> anyhow::Result<()> {
    |net_sender, net_receiver| {
        struct Vars {
            device: Arc<VulkanDevice>,
            net_sender: netnet::Sender,
            net_receiver: netnet::Receiver,
        }
        let vars = Arc::new(Mutex::new(Some(Vars {
            device,
            net_sender,
            net_receiver,
        })));
        let vars2 = vars.clone();
        weak.upgrade_in_event_loop(move |app| {
            app.on_share_screen(move || {
                info!("Starting caster");
                let Vars {
                    device,
                    net_sender,
                    net_receiver,
                } = vars.lock().unwrap().take().unwrap();
                info!("Acquired variables");
                match caster::start(device, net_sender, net_receiver) {
                    Ok(()) => "".into(),
                    Err(err) => err.to_string().into(),
                }
            });
            info!("Set on-share-screen callback");
        })
        .unwrap();
        // Checks if the user should become a viewer depending on received packets
        loop {
            // Deliberately not doing `while let Some(..) = vars2.lock().unwrap.as_ref()`
            // because that keeps the lock for too long, causing a significant delay
            // when the user presses "Share Screen".
            let mut mutex_guard = vars2.lock().unwrap();
            let Some(Vars { net_receiver, .. }) = mutex_guard.as_ref() else {
                break;
            };
            let raw_packet = match net_receiver.recv_timeout(Duration::from_millis(1)) {
                Ok(rp) => rp,
                Err(netnet::Error::Timeout) => continue,
                Err(err) => return Err(err.into()),
            };
            let packet: Packet = wincode::deserialize(&raw_packet.body)?;
            match packet {
                Packet::Input(_) => {
                    warn!("Received input from peer without sharing screen");
                }
                // drops a single packet, but the stream is known to be lossy anyways
                Packet::H264 { .. } => {
                    info!("Received video packet from peer; starting viewer");
                    let Vars {
                        device,
                        net_sender,
                        net_receiver,
                    } = mutex_guard.take().unwrap();
                    // screen was shared by peer, which implies that this user should be the viewer
                    return viewer::start(weak, device, net_sender, net_receiver);
                }
            }
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    pretty_env_logger::init();

    let device = init_backend()?;
    let device2 = device.clone();

    info!("Creating app");
    let app = App::new()?;
    app.on_is_socket_address(|text| parse_socket_address(&text, 0).is_ok());

    let weak = app.as_weak();
    let weak2 = app.as_weak();

    app.on_start_server(move || {
        nettime::sync_time().unwrap();
        let stop_signal = Signal::new();
        let device = device.clone();
        match net::connect_server(weak.clone(), stop_signal, on_connect(weak.clone(), device)) {
            Ok(()) => "".into(),
            Err(err) => err.to_string().into(),
        }
    });
    app.on_start_client(move |server_addr_str| {
        match parse_socket_address(&server_addr_str, SERVER_PORT) {
            Ok(server_addr) => {
                nettime::sync_time().unwrap();
                let stop_signal = Signal::new();
                let device = device2.clone();
                match net::connect_client(
                    weak2.clone(),
                    server_addr,
                    stop_signal,
                    on_connect(weak2.clone(), device),
                ) {
                    Ok(()) => "".into(),
                    Err(err) => err.to_string().into(),
                }
            }
            Err(err) => err.to_string().into(),
        }
    });

    info!("Running app");
    app.run()?;
    Ok(())
}
