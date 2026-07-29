use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use clap::{Parser, ValueEnum};
use common::HOST_PORT;
use log::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod common;
mod key;
mod net;
mod streamer;
mod watcher;

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
    instance: Arc<avec::Instance>,
    device: Arc<avec::Device>,
    role: Role,
) -> anyhow::Result<impl Future<Output = anyhow::Result<()>>> {
    let device = device.clone();
    let future = net::host_server()?;
    Ok(async move {
        let (conn, mut control_stream) = future.await?;
        control_stream.send_role(role).await?;
        info!("Connected");
        match role {
            Role::Streamer => streamer::start(device, conn).await,
            Role::Watcher => watcher::start(instance, device, conn).await,
        }
    })
}

fn run_client(
    instance: Arc<avec::Instance>,
    device: Arc<avec::Device>,
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
            Role::Streamer => streamer::start(device, conn).await,
            Role::Watcher => watcher::start(instance, device, conn).await,
        }
    })
}

fn main() -> anyhow::Result<()> {
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

    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            info!("Initializing backend");
            let (instance, device) = avec::init()?;

            let result = match args.host_ip {
                Some(host_ip) => {
                    info!("Running client");
                    run_client(instance, device, host_ip)?.await
                }
                None => {
                    info!("No host IP specified; hosting");
                    run_host(instance, device, args.role.unwrap())?.await
                }
            };
            info!("Program finished");
            result
        })
}
