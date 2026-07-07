use enigo::{Enigo, Keyboard, Settings};
use fps_ticker::Fps;
use log::{debug, info};
use netnet::Signal;
use openh264::formats::{BgraSliceU8, YUVBuffer};
use slint::{ComponentHandle, Weak};
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Instant;

use crate::common::{
    CLIENT_UDP_PORT, MAX_LATENCY, Packet, PacketStreams, SERVER_TCP_PORT, SERVER_UDP_PORT,
};
use crate::tcp;
use crate::{App, setup_menu};

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
    let udp = netnet::create_stream(SERVER_UDP_PORT, client_udp_addr, MAX_LATENCY, Signal::new())?;
    Ok(Some((tcp, udp)))
}

fn start_screen_cast(udp_sender: netnet::Sender<Packet>) {
    let (frame_sender, frame_receiver) = mpsc::sync_channel(0);

    std::thread::spawn(move || {
        for frame in janck::capture_video(FRAME_RATE) {
            frame_sender.send(frame).unwrap();
        }
    });
    std::thread::spawn(move || {
        let mut encoder = openh264::encoder::Encoder::new().unwrap();
        let fps = Fps::default();
        for janck::Frame {
            bytes,
            width,
            height,
            stride,
            format,
        } in frame_receiver
        {
            // TODO: support other formats
            assert_eq!(format, janck::Format::Bgra8);
            // TODO: support other strides?
            assert_eq!(stride, 4 * width);

            // Encode frame to H.264
            let pre_encode = Instant::now();
            let bgra_frame = BgraSliceU8::new(&bytes, (width as _, height as _));
            let yuv_frame = YUVBuffer::from_rgb_source(bgra_frame);
            let bit_stream = encoder.encode(&yuv_frame).unwrap();
            debug!(
                "Encoding frame took {:.2}ms",
                (Instant::now() - pre_encode).as_micros() as f32 / 1000.0
            );

            fps.tick();
            debug!("Sending frame ({width}x{height}, {:.2} fps)", fps.avg(),);
            udp_sender.send(Packet::H264Frame {
                // TODO: send individual NAL units? not sure if a frame corresponds to one or more units
                bytes: bit_stream.to_vec(),
                width,
                height,
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
        let (_tcp, (udp_sender, mut udp_receiver)) = create_streams(stop_signal).unwrap().unwrap();
        weak.upgrade_in_event_loop(|app| app.set_client_connected(true))
            .unwrap();
        info!("Client connected");
        start_screen_cast(udp_sender);
        info!("Screen cast started");
        loop {
            let (Packet::Input(key), _timestamp) = udp_receiver.recv().unwrap() else {
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
                    stop_signal.set();
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
