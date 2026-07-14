use std::{net::SocketAddr, time::Duration};

use log::info;
use netnet::Signal;
use slint::Weak;

use crate::{
    App,
    common::{MAX_LATENCY, SERVER_PORT},
};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: audio stream

fn start_connected_update_loop(weak: Weak<App>, stop_signal: Signal, connected_signal: Signal) {
    std::thread::spawn(move || {
        let mut last_value = false;
        while !stop_signal.get() {
            let value = connected_signal.get();
            if value != last_value {
                last_value = value;
                weak.upgrade_in_event_loop(move |app| {
                    app.set_connected(value);
                    info!("Connection state changed to {value}");
                })
                .unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
}

pub fn connect_server(
    weak: Weak<App>,
    stop_signal: Signal,
    on_connect: impl FnOnce(netnet::Sender, netnet::Receiver) -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    info!("Creating server");
    let net_receiver = netnet::create_server(SERVER_PORT, MAX_LATENCY, stop_signal.clone(), None)?;
    start_connected_update_loop(weak, stop_signal, net_receiver.connected_signal());

    std::thread::spawn(move || {
        info!("Waiting for client");
        let net_sender = match net_receiver.accept() {
            Ok(ok) => ok,
            Err(netnet::Error::Stopped) => {
                info!("Stop signal sent while waiting for client connection");
                return;
            }
            Err(err) => panic!("Failed to create connection: {err}"),
        };
        info!("Client connected");
        on_connect(net_sender, net_receiver).unwrap();
    });
    Ok(())
}

pub fn connect_client(
    weak: Weak<App>,
    server_addr: SocketAddr,
    stop_signal: Signal,
    on_connect: impl FnOnce(netnet::Sender, netnet::Receiver) -> anyhow::Result<()> + Send + 'static,
) -> netnet::Result<()> {
    info!("Creating connection with server");
    let (net_sender, net_receiver) =
        netnet::create_client(server_addr, MAX_LATENCY, stop_signal.clone(), None)?;
    info!("Connected to server");
    start_connected_update_loop(weak, stop_signal, net_receiver.connected_signal());
    std::thread::spawn(move || {
        on_connect(net_sender, net_receiver).unwrap();
    });
    Ok(())
}
