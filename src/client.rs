use chrono::Utc;
use gpu_video::{EncodedInputChunk, VulkanDevice, parameters::DecoderParameters};
use log::{debug, info, warn};
use netnet::Signal;
use slint::{ComponentHandle, SharedPixelBuffer, Weak};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Instant,
};
use yuv::{YuvBiPlanarImage, YuvConversionMode, YuvRange, YuvStandardMatrix};

use crate::{
    App,
    common::{Action, Key, MAX_LATENCY, Packet, PacketStreams, SERVER_TCP_PORT},
    parse_socket_address, setup_menu,
};

// TODO: determine if gpu_video can be used on non-nvidia GPUs (since it uses Nv12 as texture format instead of Yuv420)

fn yuv_to_rgba(yuv: &[u8], width: u32, height: u32, rgba: &mut [u8]) {
    let (y_plane, uv_plane) = yuv.split_at((width * height) as _);
    let image = YuvBiPlanarImage {
        y_plane,
        y_stride: width,
        uv_plane,
        uv_stride: width, // based on Yuv420 format
        width,
        height,
    };
    yuv::yuv_nv12_to_rgba(
        &image,
        rgba,
        width * 4,
        YuvRange::Full,
        YuvStandardMatrix::Bt709,
        YuvConversionMode::Balanced,
    )
    .unwrap();
}

// TODO: check that the UDP client address is the same as the TCP client address
fn create_streams(server_tcp_addr: SocketAddr, stop: Signal) -> netnet::Result<PacketStreams> {
    let mut tcp = TcpStream::connect(server_tcp_addr)?;
    info!("Created TCP stream");
    let udp = netnet::create_stream_using_hole_punch(&mut tcp, MAX_LATENCY, stop)?;
    info!("Created UDP stream");
    Ok(udp)
}

fn start(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    server_address: &str,
    stop_signal: Signal,
) -> netnet::Result<netnet::Sender<Packet>> {
    info!("Creating network connection");
    // FIXME: parse fail (caused by empty server_ip) results in panic
    let (udp_sender, mut udp_receiver) = create_streams(
        parse_socket_address(server_address, SERVER_TCP_PORT)?,
        stop_signal,
    )?;
    info!("Created network connection");

    std::thread::spawn(move || -> ! {
        info!("Started packet processing loop");
        let mut decoder = device
            .create_bytes_decoder_h264(DecoderParameters::default())
            .unwrap();

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
                    let yuv_frames = match decoder.decode(EncodedInputChunk {
                        data: &bytes,
                        pts: None, // TODO: synchronisation timestamp
                    }) {
                        Ok(f) => f,
                        Err(err) => {
                            warn!("Failed to decode frame: {err}");
                            continue;
                        }
                    };
                    // TODO: are there ever multiple frames or only 0/1 for one frame sent?
                    let Some(yuv_frame) = yuv_frames.get(0) else {
                        warn!("Failed to decode H.264 frame");
                        continue;
                    };
                    let pre_rgba = Instant::now();
                    let mut rgba_buffer = SharedPixelBuffer::new(width, height);
                    yuv_to_rgba(
                        &yuv_frame.data.frame,
                        width,
                        height,
                        rgba_buffer.make_mut_bytes(),
                    );
                    let now = Instant::now();
                    debug!(
                        "Decoding frame took {:.2}ms ({:.2}ms decoding, {:.2}ms RGBA conversion)",
                        (now - pre_decode).as_micros() as f32 / 1000.0,
                        (pre_rgba - pre_decode).as_micros() as f32 / 1000.0,
                        (now - pre_rgba).as_micros() as f32 / 1000.0,
                    );

                    let now = Instant::now();
                    debug!(
                        "Received frame {:.2}ms after the last",
                        (now - last_frame_instant).as_micros() as f32 / 1000.0
                    );
                    last_frame_instant = now;

                    weak.upgrade_in_event_loop(move |app| {
                        app.set_video_frame(slint::Image::from_rgba8(rgba_buffer));
                    })
                    .unwrap();
                }
            }
        }
    });
    Ok(udp_sender)
}

pub fn setup(app: &App, device: Arc<VulkanDevice>) {
    let weak = app.as_weak();

    app.on_start_client(move |server_address| {
        let stop_signal = Signal::new();
        match start(
            weak.clone(),
            device.clone(),
            &server_address,
            stop_signal.clone(),
        ) {
            Ok(udp_sender) => {
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
                if err.io_kind() == Some(io::ErrorKind::InvalidInput) {
                    return "Invalid address".into();
                }
                warn!("Failed to start client: {:?}", err);
                format!("{:?}", err).into()
            }
        }
    });
}
