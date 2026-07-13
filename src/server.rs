use enigo::{Enigo, Keyboard, Settings};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use log::{debug, info};
use netnet::{Error::Stopped, Signal};
use slint::{ComponentHandle, Weak};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use crate::common::{MAX_LATENCY, Packet, SERVER_PORT};
use crate::{App, gpu, setup_menu};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: use frame timestamps

// TODO: server UI element for adjusting these parameters
// TODO: resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;

fn start_screen_cast(
    device: Arc<VulkanDevice>,
    net_sender: netnet::Sender,
) -> Result<(), janck::Error> {
    let (frame_sender, frame_receiver) = mpsc::sync_channel::<janck::Frame>(0);
    let video = janck::capture_video(FRAME_RATE as _)?;

    std::thread::spawn(move || {
        // using a separate thread here means that a frame can be captured while another is being processed
        for frame in video {
            frame_sender.send(frame).unwrap();
        }
    });
    std::thread::spawn(move || {
        let mut encoder = None;
        let fps = Fps::default();

        // TODO: if janck can capture directly into [wgpu::Texture]s then the entire GPU upload step of encoding can be skipped
        for janck::Frame {
            timestamp: frame_timestamp,
            bytes,
            width,
            height,
            stride,
            format,
        } in frame_receiver
        {
            let encoder = encoder.get_or_insert_with(|| {
                gpu::Encoder::new(&device, width, height, stride, format, FRAME_RATE).unwrap()
            });
            // Encode frame to H.264
            let pre_encode = Instant::now();
            let encoded = encoder.encode(&bytes).unwrap();
            let now = Instant::now();
            debug!(
                "Encoding frame took {:.2}ms",
                (now - pre_encode).as_micros() as f32 / 1000.0,
            );

            fps.tick();
            debug!(
                "Sending {} byte frame ({width}x{height}, {:.2} fps)",
                encoded.len(),
                fps.avg()
            );
            // max packet size - (sizeof(frame_timestamp) + sizeof(width) + sizeof(height) + sizeof(&[u8]))
            // TODO: try to split on NAL unit boundary to prevent data loss caused by cutting a unit in half
            for chunk in encoded.chunks(netnet::MAX_PACKET_SIZE - 28) {
                let raw_packet = wincode::serialize(&Packet::H264 {
                    frame_timestamp,
                    bytes: chunk,
                    width,
                    height,
                })
                .unwrap();
                net_sender.send(raw_packet).unwrap();
            }
        }
    });
    info!("Started screen cast");
    Ok(())
}

fn start(weak: Weak<App>, device: Arc<VulkanDevice>, stop_signal: Signal) -> anyhow::Result<()> {
    info!("Starting server");
    info!("Creating virtual keyboard (Enigo)");
    let mut enigo = Enigo::new(&Settings::default())?;
    info!("Spawning packet management thread");
    info!("Creating packet server and waiting for client");
    let net_receiver = netnet::create_server(SERVER_PORT, MAX_LATENCY, stop_signal, None)?;
    std::thread::spawn(move || {
        info!("Creating packet stream and waiting for client");
        let net_sender = match net_receiver.accept() {
            Ok(ok) => ok,
            Err(Stopped) => {
                info!("Stop signal sent while waiting for client connection");
                return;
            }
            Err(err) => panic!("Failed to create connection: {err}"),
        };
        weak.upgrade_in_event_loop(|app| app.set_client_connected(true))
            .unwrap();
        info!("Client connected");
        start_screen_cast(device, net_sender).unwrap();
        info!("Screen cast started");
        loop {
            let raw_packet = net_receiver.recv().unwrap();
            let Packet::Input(key) = wincode::deserialize(&raw_packet.body).unwrap() else {
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

pub fn setup(app: &App, device: Arc<VulkanDevice>) {
    let weak = app.as_weak();

    app.on_start_server(move || {
        let stop_signal = Signal::new();
        match start(weak.clone(), device.clone(), stop_signal.clone()) {
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
