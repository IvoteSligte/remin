use chrono::Utc;
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::{
    io,
    net::{SocketAddrV4, TcpStream},
    sync::{Arc, Mutex},
};

use crate::{
    App,
    common::{Action, Key, PORT, Packet, PacketStream},
    setup_menu,
};

fn start_channel(weak: Weak<App>, server_ip: &str) -> io::Result<Arc<Mutex<PacketStream>>> {
    info!("Creating TCP client");
    // FIXME: parse fail (caused by empty server_ip) results in panic
    let tcp_stream = TcpStream::connect(SocketAddrV4::new(server_ip.parse().unwrap(), PORT))?;
    info!("Created TCP client");
    let stream = Arc::new(Mutex::new(PacketStream::new(tcp_stream)));
    let stream2 = stream.clone();

    std::thread::spawn(move || {
        info!("Started packet processing loop");
        let stream = stream.clone();
        let fps = fps_ticker::Fps::default();

        loop {
            let Some(packet) = stream.lock().unwrap().recv().unwrap() else {
                continue;
            };
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::Rgb8 {
                    timestamp,
                    width,
                    height,
                    data,
                } => {
                    let now = Utc::now();
                    let timestamp = chrono::DateTime::<Utc>::from_timestamp_nanos(timestamp);
                    fps.tick();
                    debug!(
                        "Received frame at {timestamp} from server ({}ms delay, {:.2} fps, {width}x{height})",
                        (now - timestamp).num_milliseconds(),
                        fps.avg(),
                    );
                    let mut buffer = slint::SharedPixelBuffer::new(width as _, height as _);
                    buffer.make_mut_bytes().copy_from_slice(&data);

                    weak.upgrade_in_event_loop(move |app| {
                        app.set_video_frame(slint::Image::from_rgb8(buffer));
                    })
                    .unwrap();
                }
            }
        }
    });
    Ok(stream2)
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_client(
        move |server_address| match start_channel(weak.clone(), &server_address) {
            Ok(stream) => {
                let app = weak.upgrade().unwrap();
                let stream2 = stream.clone();
                let weak = app.as_weak();

                app.on_escape(move || {
                    info!("Stopping client");
                    let _ = stream.lock().unwrap().shutdown();
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
                    (&mut *stream2.lock().unwrap()).send(&packet).unwrap();
                });
                "".into()
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::InvalidInput {
                    return "Invalid address".into();
                }
                info!("Failed to start client: {:?}", err);
                format!("{:?}", err).into()
            }
        },
    );
}
