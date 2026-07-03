use enigo::Direction::{Press, Release};
use ffmpeg_sidecar::command::FfmpegCommand;
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::{
    io::{self, Write},
    net::{Shutdown, SocketAddrV4, TcpStream},
    sync::{Arc, Mutex},
};

use crate::{
    App,
    common::{INPUT_PORT, Key, VIDEO_PORT},
    setup_menu,
};

fn start_screen_cast(weak: Weak<App>) -> anyhow::Result<()> {
    info!("Starting screen cast receiver");
    let frames = FfmpegCommand::new()
        .input(format!(
            "srt://0.0.0.0:{VIDEO_PORT}?mode=listener&latency=50"
        ))
        .rawvideo()
        .print_command()
        .spawn()?
        .iter()?
        .filter_frames();
    std::thread::spawn(move || {
        for frame in frames {
            let mut buffer = slint::SharedPixelBuffer::new(frame.width, frame.height);
            debug!(
                "Received frame from server ({}x{})",
                frame.width, frame.height
            );
            buffer.make_mut_bytes().copy_from_slice(&frame.data);
            weak.upgrade_in_event_loop(move |app| {
                app.set_video_frame(slint::Image::from_rgb8(buffer));
            })
            .unwrap();
        }
    });
    info!("Started screen cast receiver");
    Ok(())
}

fn start_input_stream(server_ip: &str) -> io::Result<TcpStream> {
    info!("Creating TCP input client");
    let stream = TcpStream::connect(SocketAddrV4::new(server_ip.parse().unwrap(), INPUT_PORT))?;
    info!("Created TCP input client");
    Ok(stream)
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_client(
        move |server_address| match start_input_stream(&server_address) {
            Ok(stream) => {
                start_screen_cast(weak.clone()).unwrap();
                let app = weak.upgrade().unwrap();
                let stream = Arc::new(Mutex::new(stream));
                let stream2 = stream.clone();
                let weak = app.as_weak();

                app.on_escape(move || {
                    info!("Stopping client");
                    let _ = stream.lock().unwrap().shutdown(Shutdown::Both);
                    setup_menu(&weak);
                });
                app.on_keyboard_input(move |text, action| {
                    // text is only a string because slint does not work with characters
                    let Some(char) = text.chars().next() else {
                        return;
                    };
                    let bytes = Key {
                        char,
                        action: if action == "pressed" { Press } else { Release },
                    }
                    .encode();
                    stream2.lock().unwrap().write(&bytes).unwrap();
                    debug!("Key {}: '{}'", action, char);
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
