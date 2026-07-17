use std::{net::SocketAddr, sync::Arc};

use log::{info, warn};
use netnet::Connection;
use slint::Weak;

use crate::{App, common::SERVER_PORT};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: audio stream

/// Requires [tokio] runtime despite being sync
fn track_connection_status(weak: Weak<App>, connection: Arc<Connection>) {
    weak.upgrade_in_event_loop(|app| {
        app.set_connected(true);
        info!("Connected to client");
    })
    .unwrap();

    tokio::task::spawn(async move {
        let cause = connection.closed().await;

        weak.upgrade_in_event_loop(move |app| {
            app.set_connected(false);
            warn!("Disconnected from client: {cause}");
        })
        .unwrap();
    });
}

pub fn connect_server(
    weak: Weak<App>,
) -> anyhow::Result<impl Future<Output = anyhow::Result<Arc<Connection>>>> {
    info!("Creating server");
    let future = netnet::create_server(SERVER_PORT)?;
    info!("Finished creating server");

    Ok(async move {
        info!("Waiting for client connection");
        let connection = Arc::new(future.await?);
        info!("Client connected");
        track_connection_status(weak, connection.clone());
        Ok(connection)
    })
}

pub fn connect_client(
    weak: Weak<App>,
    server_addr: SocketAddr,
) -> anyhow::Result<impl Future<Output = anyhow::Result<Arc<Connection>>>> {
    info!("Creating client");
    let future = netnet::create_client(server_addr)?;

    Ok(async move {
        info!("Connecting to server");
        let connection = Arc::new(future.await?);
        info!("Connected to server");        
        track_connection_status(weak, connection.clone());
        Ok(connection)
    })
}
