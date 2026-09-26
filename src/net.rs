use std::net::SocketAddr;

use anyhow::bail;
use log::info;
use netnet::Connection;

use crate::{Role, common::HOST_PORT};

// TODO: stop client/host video streams when F12 is pressed
// TODO: stop host input TCP stream when F12 is pressed

pub struct ReliableStream {
    pub sender: netnet::ReliableSender,
    pub receiver: netnet::ReliableReceiver,
}

impl From<(netnet::ReliableSender, netnet::ReliableReceiver)> for ReliableStream {
    fn from((sender, receiver): (netnet::ReliableSender, netnet::ReliableReceiver)) -> Self {
        Self { sender, receiver }
    }
}

pub struct UnreliableStream {
    pub sender: netnet::UnreliableSender,
    pub receiver: netnet::UnreliableReceiver,
}

impl From<(netnet::UnreliableSender, netnet::UnreliableReceiver)> for UnreliableStream {
    fn from((sender, receiver): (netnet::UnreliableSender, netnet::UnreliableReceiver)) -> Self {
        Self { sender, receiver }
    }
}

pub struct Streams {
    pub control: ReliableStream,
    pub input: UnreliableStream,
    pub video: UnreliableStream,
    pub audio: UnreliableStream,
}

impl Streams {
    pub async fn send_role(&mut self, role: Role) -> anyhow::Result<()> {
        let byte = match role {
            Role::Streamer => 0u8,
            Role::Watcher => 1u8,
        };
        self.control.sender.send(std::slice::from_ref(&byte)).await
    }

    pub async fn recv_role(&mut self) -> anyhow::Result<Role> {
        let bytes = self.control.receiver.recv().await?;
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

pub fn host_server() -> anyhow::Result<impl Future<Output = anyhow::Result<(Connection, Streams)>>>
{
    info!("Creating server");
    let future = netnet::create_server(HOST_PORT)?;
    info!("Finished creating server");

    Ok(async move {
        info!("Waiting for client connection");
        let mut conn = future.await?;
        info!("Client connected");
        let streams = Streams {
            control: conn.create_reliable_stream("control").await?.into(),
            input: conn.create_unreliable_stream("input").await?.into(),
            video: conn.create_unreliable_stream("video").await?.into(),
            audio: conn.create_unreliable_stream("audio").await?.into(),
        };
        Ok((conn, streams))
    })
}

pub fn connect_to_server(
    host_addr: SocketAddr,
) -> anyhow::Result<impl Future<Output = anyhow::Result<(Connection, Streams)>>> {
    info!("Creating client");
    let future = netnet::create_client(host_addr)?;

    Ok(async move {
        info!("Connecting to server");
        let mut conn = future.await?;
        info!("Connected to server");

        let (sender, receiver) = conn.accept_reliable_stream().await?;
        if sender.label() != "control" {
            bail!("Somehow accepted reliable non-control stream");
        }
        let control = ReliableStream { sender, receiver };

        let mut input = None;
        let mut video = None;
        let mut audio = None;
        for _ in 0..3 {
            let (sender, receiver) = conn.accept_unreliable_stream().await?;
            match sender.label() {
                "input" => input = Some(UnreliableStream { sender, receiver }),
                "video" => video = Some(UnreliableStream { sender, receiver }),
                "audio" => audio = Some(UnreliableStream { sender, receiver }),
                _ => {
                    panic!(
                        "Accepted unreliable stream that is neither input, audio, nor video. Label: '{}'",
                        sender.label()
                    )
                }
            }
        }
        let streams = Streams {
            control,
            input: input.unwrap(),
            video: video.unwrap(),
            audio: audio.unwrap(),
        };
        Ok((conn, streams))
    })
}
