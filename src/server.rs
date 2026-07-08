use enigo::{Enigo, Keyboard, Settings};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use gpu_video::parameters::{EncoderParametersH264, RateControl, VideoParameters};
use log::{debug, info};
use netnet::Signal;
use slint::{ComponentHandle, Weak};
use std::net::SocketAddr;
use std::sync::{Arc, mpsc};
use std::time::Instant;
use std::{io, iter};
use yuv::{
    BufferStoreMut, YuvBiPlanarImageMut, YuvChromaSubsampling, YuvConversionMode, YuvRange,
    YuvStandardMatrix,
};

use crate::common::{
    CLIENT_UDP_PORT, MAX_LATENCY, Packet, PacketStreams, SERVER_TCP_PORT, SERVER_UDP_PORT,
};
use crate::tcp;
use crate::{App, setup_menu};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: use frame timestamps

// TODO: server UI element for adjusting these parameters
const FRAME_RATE: u64 = 75;

fn bgra_to_yuv(bgra: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut image = YuvBiPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
    yuv::bgra_to_yuv_nv12(
        &mut image,
        bgra,
        stride,
        YuvRange::Full,
        YuvStandardMatrix::Bt709,
        YuvConversionMode::Balanced,
    )
    .unwrap();
    let BufferStoreMut::Owned(y_plane) = image.y_plane else {
        unreachable!();
    };
    let BufferStoreMut::Owned(uv_plane) = image.uv_plane else {
        unreachable!();
    };
    Vec::from_iter(iter::chain(y_plane, uv_plane))
}

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

fn start_screen_cast(device: Arc<VulkanDevice>, udp_sender: netnet::Sender<Packet>) {
    let (frame_sender, frame_receiver) = mpsc::sync_channel(0);

    std::thread::spawn(move || {
        for frame in janck::capture_video(FRAME_RATE as _) {
            frame_sender.send(frame).unwrap();
        }
    });
    std::thread::spawn(move || {
        let mut encoder = None;
        let fps = Fps::default();
        // TODO: if janck can capture directly into [wgpu::Texture]s then the entire GPU upload step of encoding can be skipped
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

            if encoder.is_none() {
                encoder = Some(
                    device
                        .create_bytes_encoder_h264(EncoderParametersH264 {
                            input_parameters: VideoParameters {
                                width: width.try_into().unwrap(),
                                height: height.try_into().unwrap(),
                                target_framerate: (FRAME_RATE as u32).into(),
                            },
                            output_parameters: device
                                .encoder_output_parameters_h264_low_latency(RateControl::Disabled)
                                .unwrap(),
                        })
                        .unwrap(),
                );
            }
            let encoder = encoder.as_mut().unwrap();

            // Encode frame to H.264
            let pre_yuv = Instant::now();
            let yuv_frame = bgra_to_yuv(&bytes, width, height, stride);
            let pre_encode = Instant::now();
            let encoded = encoder
                .encode(
                    &gpu_video::InputFrame {
                        data: gpu_video::RawFrameData {
                            frame: yuv_frame,
                            width,
                            height,
                        },
                        pts: None, // TODO: synchronisation timestamp
                    },
                    false,
                )
                .unwrap();
            let now = Instant::now();
            debug!(
                "Encoding frame took {:.2}ms ({:.2}ms YUV conversion, {:.2}ms GPU encoding)",
                (now - pre_yuv).as_micros() as f32 / 1000.0,
                (pre_encode - pre_yuv).as_micros() as f32 / 1000.0,
                (now - pre_encode).as_micros() as f32 / 1000.0,
            );

            fps.tick();
            debug!("Sending frame ({width}x{height}, {:.2} fps)", fps.avg(),);
            udp_sender.send(Packet::H264Frame {
                // TODO: send individual NAL units? not sure if a frame corresponds to one or more units
                bytes: encoded.data,
                width,
                height,
            });
        }
    });
    info!("Started screen cast");
}

fn start(weak: Weak<App>, device: Arc<VulkanDevice>, stop_signal: Signal) -> anyhow::Result<()> {
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
        start_screen_cast(device, udp_sender);
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
