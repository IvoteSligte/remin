use gpu_video::VulkanDevice;
use log::{debug, info, warn};
use netnet::{Signal, since_micros};
use slint::{ComponentHandle, Weak};
use std::{io, sync::Arc, time::Instant};

use crate::{
    App,
    common::{Action, Key, MAX_LATENCY, Packet, SERVER_PORT},
    gpu, parse_socket_address, setup_menu,
};

fn start(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    queue: wgpu::Queue,
    server_address: &str,
    stop_signal: Signal,
) -> netnet::Result<netnet::Sender> {
    info!("Creating network connection");
    let (net_sender, net_receiver) = netnet::create_client(
        parse_socket_address(server_address, SERVER_PORT)?,
        MAX_LATENCY,
        stop_signal,
        None,
    )?;
    info!("Created network connection");

    std::thread::spawn(move || -> ! {
        info!("Started packet processing loop");
        let mut decoder = gpu::Decoder::new(device, queue, weak).unwrap();

        let fps = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();
        loop {
            let raw_packet = net_receiver.recv().unwrap();
            let packet: Packet = wincode::deserialize(&raw_packet.body).unwrap();
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::H264 {
                    frame_timestamp,
                    bytes,
                    height: _,
                    width: _,
                } => {
                    fps.tick();
                    debug!(
                        "Received frame from server (latency: {:.2}ms total; {:.2} fps)",
                        since_micros(frame_timestamp).num_microseconds().unwrap() as f32 / 1000.0,
                        fps.avg(),
                    );
                    // decode to YUV frame and then to Slint image
                    let pre_decode = Instant::now();
                    match decoder.decode(&bytes) {
                        Ok(()) => (),
                        Err(gpu::DecoderError::NoNewFrame) => {
                            debug!("Not enough frame data to construct a new frame");
                            continue;
                        }
                        Err(err) => {
                            warn!("Failed to decode frame: {err}");
                            continue;
                        }
                    }
                    debug!(
                        "Decoding frame took {:.2}ms",
                        (Instant::now() - pre_decode).as_micros() as f32 / 1000.0,
                    );

                    let now = Instant::now();
                    debug!(
                        "Received frame {:.2}ms after the last",
                        (now - last_frame_instant).as_micros() as f32 / 1000.0
                    );
                    last_frame_instant = now;
                    debug!(
                        "Total frame latency: {:.2}ms",
                        since_micros(frame_timestamp).num_microseconds().unwrap() as f32 / 1000.0
                    );
                }
            }
        }
    });
    Ok(net_sender)
}

pub fn setup(app: &App, device: Arc<VulkanDevice>, queue: wgpu::Queue) {
    let weak = app.as_weak();

    app.on_start_client(move |server_address| {
        let stop_signal = Signal::new();
        match start(
            weak.clone(),
            device.clone(),
            queue.clone(),
            &server_address,
            stop_signal.clone(),
        ) {
            Ok(net_sender) => {
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
                    let raw_packet = wincode::serialize(&packet).unwrap();
                    net_sender.send(raw_packet).unwrap();
                });
                "".into()
            }
            Err(err) => {
                if err.io_kind() == Some(io::ErrorKind::InvalidInput) {
                    return "Invalid address".into();
                }
                warn!("Failed to start client: {:?}", err);
                format!("{:?}", err).into()
            }
        }
    });
}
