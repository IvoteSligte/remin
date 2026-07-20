use std::net::SocketAddr;

use anyhow::bail;
use log::{info, warn};
use netnet::Connection;
use slint::Weak;

use crate::{App, Role, common::HOST_PORT};

pub const CONTROL_STREAM_ID: u8 = 1;

// TODO: stop client/host video streams when Escape is pressed
// TODO: stop host input TCP stream when Escape is pressed
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

pub struct ControlStream {
    sender: netnet::ReliableSender,
    receiver: netnet::ReliableReceiver,
}

impl ControlStream {
    pub async fn send_role(&mut self, role: Role) -> anyhow::Result<()> {
        let byte = match role {
            Role::Streamer => 0u8,
            Role::Watcher => 1u8,
        };
        self.sender.send(std::slice::from_ref(&byte)).await
    }

    pub async fn recv_role(&mut self) -> anyhow::Result<Role> {
        let bytes = self.receiver.recv().await?;
        if bytes.len() != 1 {
            bail!("Expected role byte");
        }
        Ok(match bytes[0] {
            0u8 => Role::Streamer,
            1u8 => Role::Watcher,
            _ => bail!("Unknown role: {}", bytes[0]),
        })
    }
}

pub fn host_server(
    weak: Weak<App>,
) -> anyhow::Result<impl Future<Output = anyhow::Result<(Connection, ControlStream)>>> {
    info!("Creating server");
    let future = netnet::create_server(HOST_PORT)?;
    info!("Finished creating server");

    Ok(async move {
        info!("Waiting for client connection");
        let conn = future.await?;
        info!("Client connected");
        track_connection_status(weak, &conn);
        let (control_sender, control_receiver) =
            conn.create_reliable_stream(CONTROL_STREAM_ID).await?;
        let control_stream = ControlStream {
            sender: control_sender,
            receiver: control_receiver,
        };
        Ok((conn, control_stream))
    })
}

pub fn connect_to_server(
    weak: Weak<App>,
    host_addr: SocketAddr,
) -> anyhow::Result<impl Future<Output = anyhow::Result<(Connection, ControlStream)>>> {
    info!("Creating client");
    let future = netnet::create_client(host_addr)?;

    Ok(async move {
        info!("Connecting to server");
        let conn = future.await?;
        info!("Connected to server");
        track_connection_status(weak, &conn);
        let (stream_id, control_sender, control_receiver) = conn.accept_reliable_stream().await?;
        if stream_id != CONTROL_STREAM_ID {
            bail!("Somehow accepted non-control stream");
        }
        let control_stream = ControlStream {
            sender: control_sender,
            receiver: control_receiver,
        };
        Ok((conn, control_stream))
    })
}
