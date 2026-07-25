use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use clap::{Parser, ValueEnum};
use common::HOST_PORT;
use gpu_video::{
    VulkanDevice, VulkanInstance,
    parameters::{VulkanAdapterDescriptor, VulkanDeviceDescriptor},
};
use log::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use winit::event::KeyEvent;

mod common;
mod event_loop;
mod gpu;
mod net;
mod streamer;
mod watcher;

pub(crate) use event_loop::run_event_loop;

// TODO: F11 for fullscreen, F12 for quit

#[derive(Parser)]
struct Args {
    #[arg(long)]
    host_ip: Option<IpAddr>,
    #[arg(long)]    
    role: Option<Role>,
}

#[derive(Parser, ValueEnum, Clone, Copy)]
enum Mode {
    Host,
    Client,
}

#[derive(Parser, ValueEnum, Clone, Copy, PartialEq, Eq)]
enum Role {
    Streamer,
    Watcher,
}

fn run_host(
    instance: Arc<VulkanInstance>,
    device: Arc<VulkanDevice>,
    role: Role,
) -> anyhow::Result<impl Future<Output = anyhow::Result<()>>> {
    let device = device.clone();
    let future = net::host_server()?;
    // FIXME: do not ignore errors
    Ok(async move {
        let (conn, mut control_stream) = future.await?;
        control_stream.send_role(role).await?;
        info!("Connected");
        match role {
            // TODO: show error message from start (if any) to user
            Role::Streamer => streamer::start(device, conn),
            Role::Watcher => watcher::start(instance, device, conn),
        }
    })
}

fn run_client(
    instance: Arc<VulkanInstance>,
    device: Arc<VulkanDevice>,
    host_ip: IpAddr,
) -> anyhow::Result<impl Future<Output = anyhow::Result<()>>> {
    let host_addr = SocketAddr::new(host_ip, HOST_PORT);
    let future = net::connect_to_server(host_addr)?;

    Ok(async move {
        let (conn, mut control_stream) = future.await?;
        info!("Connected; waiting for role");
        let host_role = control_stream.recv_role().await?;
        let role = match host_role {
            Role::Streamer => Role::Watcher,
            Role::Watcher => Role::Streamer,
        };
        info!("Running event loop");
        match role {
            // TODO: show error message from start (if any) to user
            Role::Streamer => streamer::start(device, conn)?,
            Role::Watcher => watcher::start(instance, device, conn)?,
        }
        Ok(())
    })
}

fn init_backend() -> anyhow::Result<(Arc<VulkanInstance>, Arc<VulkanDevice>)> {
    // TODO: integrate Slint's preferred options for creating instance, adapter, device, and queue
    info!("Creating Vulkan instance");
    let instance = VulkanInstance::new()?;
    info!("Creating Vulkan adapter");
    let adapter = instance.create_adapter(&VulkanAdapterDescriptor::default())?;
    info!("Creating Vulkan device");
    let device = adapter.create_device(&VulkanDeviceDescriptor::default())?;
    Ok((instance, device))
}

fn encode_key(event: KeyEvent) -> Option<char> {
    match event.text {
        Some(text) => {
            return Some(text.chars().next().unwrap());
        }
        None => {
            warn!("Key does not map to text: {:?}", event);
            None
        }
    }
}

fn decode_key(key: char) -> enigo::Key {
    match key {
        k => enigo::Key::Unicode(k),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match (args.host_ip, args.role) {
        (Some(_), None) | (None, Some(_)) => (),
        _ => {
            println!("Exactly one of --host-ip or --role must be specified.");
            std::process::exit(1);
        }
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::filter::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with(tracing_subscriber::fmt::layer().without_time())
        .init();

    info!("Initializing backend");
    let (instance, device) = init_backend()?;

    match args.host_ip {
        Some(host_ip) => {
            info!("Running client");
            run_client(instance, device, host_ip)?.await
        }
        None => {
            info!("No host IP specified; hosting");
            run_host(instance, device, args.role.unwrap())?.await
        }
    }
}
