use chrono::Utc;
use log::{debug, info, warn};
use netnet::Signal;
use slint::{ComponentHandle, Weak};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Instant,
};

use crate::{
    App,
    common::{
        Action, CLIENT_UDP_PORT, Key, MAX_LATENCY, Packet, PacketStreams, SERVER_TCP_PORT,
        SERVER_UDP_PORT,
    },
    setup_menu, tcp,
};

pub fn create_streams(server_ip: IpAddr, stop: Signal) -> io::Result<PacketStreams> {
    let server_tcp_addr = SocketAddr::new(server_ip, SERVER_TCP_PORT);
    let server_udp_addr = SocketAddr::new(server_ip, SERVER_UDP_PORT);
    let tcp = tcp::PacketStream::new_client(server_tcp_addr, stop.clone())
        .map_err(|err| io::Error::other(format!("Failed to create TCP stream: {err}")))?;
    let udp = netnet::create_stream(CLIENT_UDP_PORT, server_udp_addr, MAX_LATENCY, stop)
        .map_err(|err| io::Error::other(format!("Failed to create UDP stream: {err}")))?;
    Ok((tcp, udp))
}

fn start(
    weak: Weak<App>,
    server_ip: &str,
    stop_signal: Signal,
) -> io::Result<(tcp::PacketStream, netnet::Sender<Packet>)> {
    info!("Creating TCP client");
    // FIXME: parse fail (caused by empty server_ip) results in panic
    let (tcp, (udp_sender, mut udp_receiver)) =
        create_streams(server_ip.parse().unwrap(), stop_signal)?;
    info!("Created TCP client");

    std::thread::spawn(move || {
        info!("Started packet processing loop");
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let fps = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();
        loop {
            let (packet, timestamp) = udp_receiver.recv().unwrap();
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::H264Frame {
                    bytes,
                    width,
                    height,
                } => {
                    let now = Utc::now();
                    fps.tick();
                    debug!(
                        "Received frame from server ({:.2}ms latency, {:.2} fps, {width}x{height})",
                        (now - timestamp).num_microseconds().unwrap() as f32 / 1000.0,
                        fps.avg(),
                    );

                    // decode to YUV frame and then to Slint image
                    let pre_decode = Instant::now();
                    let Some(yuv_frame) = decoder.decode(&bytes).unwrap() else {
                        warn!("Failed to decode H.264 frame");
                        continue;
                    };
                    let mut rgb_buffer = slint::SharedPixelBuffer::new(width as _, height as _);
                    yuv_frame.write_rgb8(rgb_buffer.make_mut_bytes());
                    debug!("Decoding frame took {:.2}ms", (Instant::now() - pre_decode).as_micros() as f32 / 1000.0);

                    let now = Instant::now();
                    debug!(
                        "Received frame {:.2}ms after the last",
                        (now - last_frame_instant).as_micros() as f32 / 1000.0
                    );
                    last_frame_instant = now;

                    weak.upgrade_in_event_loop(move |app| {
                        app.set_video_frame(slint::Image::from_rgb8(rgb_buffer));
                    })
                    .unwrap();
                }
            }
        }
    });
    Ok((tcp, udp_sender))
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_client(move |server_address| {
        let stop_signal = Signal::new();
        match start(weak.clone(), &server_address, stop_signal.clone()) {
            Ok((_tcp, udp_sender)) => {
                let app = weak.upgrade().unwrap();
                let weak = app.as_weak();
                let stop_signal2 = stop_signal.clone();

                app.on_escape(move || {
                    info!("Stopping client");
                    stop_signal2.set();
                    setup_menu(&weak);
                });
                app.on_keyboard_input(move |text, action| {
                    // text is only a string because slint does not work with characters
                    let Some(char) = text.chars().next() else {
                        return;
                    };
                    debug!("Key {}: '{}'", action, char);
                    let packet = Packet::Input(Key {
                        char,
                        action: if action == "pressed" {
                            Action::Press
                        } else {
                            Action::Release
                        },
                    });
                    udp_sender.send(packet);
                });
                "".into()
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::InvalidInput {
                    return "Invalid address".into();
                }
                warn!("Failed to start client: {:?}", err);
                format!("{:?}", err).into()
            }
        }
    });
}
