use enigo::{Enigo, Keyboard, Settings};
use fps_ticker::Fps;
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;

use crate::common::{CLIENT_UDP_PORT, PacketStreams, SERVER_TCP_PORT, SERVER_UDP_PORT};
use crate::{App, setup_menu, signal::Signal};
use crate::{tcp, udp};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: use frame timestamps

// TODO: server UI element for adjusting frame-rate
const FRAME_RATE: u64 = 75;

/// Returns `Ok(None)` if stop was signaled during the creation process.
pub fn create_streams(stop: Signal) -> io::Result<Option<PacketStreams>> {
    let Some((tcp, client_addr)) = tcp::PacketStream::new_server(SERVER_TCP_PORT, stop.clone())?
    else {
        return Ok(None);
    };
    let client_udp_addr = SocketAddr::new(client_addr.ip(), CLIENT_UDP_PORT);
    let udp = udp::PacketStream::new(SERVER_UDP_PORT, client_udp_addr, stop)?;
    Ok(Some((tcp, udp)))
}

fn start_screen_cast(udp: udp::PacketStream) {
    let (frame_sender, frame_receiver) = mpsc::sync_channel(0);

    std::thread::spawn(move || {
        for image in janck::capture_video(FRAME_RATE) {
            frame_sender.send(image).unwrap();
        }
    });
    std::thread::spawn(move || {
        let fps = Fps::default();
        for janck::Yuv420Image {
            width,
            height,
            y_stride,
            u_stride,
            v_stride,
            y_plane,
            u_plane,
            v_plane,
        } in frame_receiver
        {
            fps.tick();
            debug!("Sending frame ({width}x{height}, {:.2} fps)", fps.avg(),);
            udp.send(udp::Packet::Yuv {
                width,
                height,
                y_stride,
                u_stride,
                v_stride,
                y_plane,
                u_plane,
                v_plane,
            });
        }
    });
    info!("Started screen cast");
}

fn start(weak: Weak<App>, stop_signal: Signal) -> anyhow::Result<()> {
    info!("Starting server");
    info!("Creating virtual keyboard (Enigo)");
    let mut enigo = Enigo::new(&Settings::default())?;
    info!("Spawning packet management thread");
    std::thread::spawn(move || {
        info!("Creating packet stream and waiting for client");
        let (_tcp, udp) = create_streams(stop_signal).unwrap().unwrap();
        let udp2 = udp.clone();
        weak.upgrade_in_event_loop(|app| app.set_client_connected(true))
            .unwrap();
        info!("Client connected");
        start_screen_cast(udp2.clone());
        info!("Screen cast started");
        loop {
            let (udp::Packet::Input(key), _timestamp) = udp2.recv().unwrap() else {
                unreachable!();
            };
            debug!("Read {:?}", key);
            enigo
                .key(enigo::Key::Unicode(key.char), key.action.into())
                .unwrap();
        }
    });
    Ok(())
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_server(move || {
        let stop_signal = Signal::new();
        match start(weak.clone(), stop_signal.clone()) {
            Ok(()) => {
                let app = weak.upgrade().unwrap();
                let weak = app.as_weak();
                app.on_escape(move || {
                    info!("Stopping server");
                    stop_signal.signal();
                    // TODO: wait for connections to shut down
                    weak.upgrade().unwrap().set_client_connected(false);
                    setup_menu(&weak);
                });
                "".into()
            }
            Err(err) => format!("{:?}", err).into(),
        }
    });
}
