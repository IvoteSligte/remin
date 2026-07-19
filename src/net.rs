use std::net::SocketAddr;

use log::{info, warn};
use netnet::Connection;
use slint::Weak;

use crate::{App, common::SERVER_PORT};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: audio stream

/// Requires [tokio] runtime despite being sync
fn track_connection_status(weak: Weak<App>, conn: &Connection) {
    weak.upgrade_in_event_loop(|app| {
        app.set_connected(true);
        info!("Connected to client");
    })
    .unwrap();
    let inner_conn = conn.inner().clone();

    tokio::task::spawn(async move {
        let cause = inner_conn.closed().await;

        weak.upgrade_in_event_loop(move |app| {
            app.set_connected(false);
            warn!("Disconnected from client: {cause}");
        })
        .unwrap();
    });
}

pub fn connect_server(
    weak: Weak<App>,
) -> anyhow::Result<impl Future<Output = anyhow::Result<Connection>>> {
    info!("Creating server");
    let future = netnet::create_server(SERVER_PORT)?;
    info!("Finished creating server");

    Ok(async move {
        info!("Waiting for client connection");
        let conn = future.await?;
        info!("Client connected");
        track_connection_status(weak, &conn);
        Ok(conn)
    })
}

pub fn connect_client(
    weak: Weak<App>,
    server_addr: SocketAddr,
) -> anyhow::Result<impl Future<Output = anyhow::Result<Connection>>> {
    info!("Creating client");
    let future = netnet::create_client(server_addr)?;

    Ok(async move {
        info!("Connecting to server");
        let conn = future.await?;
        info!("Connected to server");
        track_connection_status(weak, &conn);
        Ok(conn)
    })
}
