use chrono::Utc;
use log::{debug, info, warn};
use slint::{ComponentHandle, Weak};
use std::{
    io,
    net::{IpAddr, SocketAddr},
};

use crate::{
    App,
    common::{Action, CLIENT_UDP_PORT, Key, PacketStreams, SERVER_TCP_PORT, SERVER_UDP_PORT},
    setup_menu,
    signal::Signal,
    tcp, udp,
};

// FIXME: receiving UDP packets from the server breaks once the clients starts sending packets as well

pub fn create_streams(server_ip: IpAddr, stop: Signal) -> io::Result<PacketStreams> {
    let server_tcp_addr = SocketAddr::new(server_ip, SERVER_TCP_PORT);
    let server_udp_addr = SocketAddr::new(server_ip, SERVER_UDP_PORT);
    let tcp = tcp::PacketStream::new_client(server_tcp_addr, stop.clone())
        .map_err(|err| io::Error::other(format!("Failed to create TCP stream: {err}")))?;
    let udp = udp::PacketStream::new(CLIENT_UDP_PORT, server_udp_addr, stop)
        .map_err(|err| io::Error::other(format!("Failed to create UDP stream: {err}")))?;
    Ok((tcp, udp))
}

fn start(weak: Weak<App>, server_ip: &str, stop_signal: Signal) -> io::Result<PacketStreams> {
    info!("Creating TCP client");
    // FIXME: parse fail (caused by empty server_ip) results in panic
    let (tcp, udp) = create_streams(server_ip.parse().unwrap(), stop_signal)?;
    info!("Created TCP client");
    let udp2 = udp.clone();

    std::thread::spawn(move || {
        info!("Started packet processing loop");
        let fps = fps_ticker::Fps::default();

        loop {
            let packet = udp2.recv().unwrap();
            match packet {
                udp::Packet::Input(_) => unreachable!("Client should not receive input packets"),
                udp::Packet::Yuv {
                    timestamp,
                    width,
                    height,
                    y_stride,
                    u_stride,
                    v_stride,
                    y_plane,
                    u_plane,
                    v_plane,
                } => {
                    let now = Utc::now();
                    let timestamp = chrono::DateTime::<Utc>::from_timestamp_nanos(timestamp);
                    fps.tick();
                    debug!(
                        "Received frame at {timestamp} from server ({}ms delay, {:.2} fps, {width}x{height})",
                        (now - timestamp).num_milliseconds(),
                        fps.avg(),
                    );
                    let yuv_frame = janck::Yuv420Image {
                        width,
                        height,
                        y_stride,
                        u_stride,
                        v_stride,
                        y_plane,
                        u_plane,
                        v_plane,
                    };
                    let rgb_frame = yuv_frame.to_rgb8().unwrap();
                    let mut buffer = slint::SharedPixelBuffer::new(width as _, height as _);
                    buffer.make_mut_bytes().copy_from_slice(&rgb_frame.data);

                    weak.upgrade_in_event_loop(move |app| {
                        app.set_video_frame(slint::Image::from_rgb8(buffer));
                    })
                    .unwrap();
                }
            }
        }
    });
    Ok((tcp, udp))
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_client(move |server_address| {
        let stop_signal = Signal::new();
        match start(weak.clone(), &server_address, stop_signal.clone()) {
            Ok((_tcp, udp)) => {
                let app = weak.upgrade().unwrap();
                let weak = app.as_weak();
                let stop_signal2 = stop_signal.clone();

                app.on_escape(move || {
                    info!("Stopping client");
                    stop_signal2.signal();
                    setup_menu(&weak);
                });
                app.on_keyboard_input(move |text, action| {
                    // text is only a string because slint does not work with characters
                    let Some(char) = text.chars().next() else {
                        return;
                    };
                    debug!("Key {}: '{}'", action, char);
                    let packet = udp::Packet::Input(Key {
                        char,
                        action: if action == "pressed" {
                            Action::Press
                        } else {
                            Action::Release
                        },
                    });
                    udp.send(&packet).unwrap();
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
