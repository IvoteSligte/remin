use enigo::{Enigo, Keyboard};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use log::{debug, info};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use crate::common::Packet;
use crate::gpu;

// TODO: UI element for adjusting these parameters
// TODO: resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;

fn send_chunks(
    net_sender: &netnet::Sender,
    mut data: &[u8],
    frame_timestamp: i64,
    width: u32,
    height: u32,
) {
    // max packet size - (sizeof(frame_timestamp) + sizeof(width) + sizeof(height) + sizeof(&[u8]))
    const MAX_CHUNK_SIZE: usize = netnet::MAX_PACKET_SIZE - 28;

    let send_packet = |bytes: &[u8]| {
        let raw_packet = wincode::serialize(&Packet::H264 {
            frame_timestamp,
            bytes,
            width,
            height,
        })
        .unwrap();
        net_sender.send(raw_packet).unwrap();
    };

    // TODO: send small NAL units together
    let mut i = 4;
    while data.len() > MAX_CHUNK_SIZE {
        if i >= MAX_CHUNK_SIZE || &data[i..i + 4] == &[0, 0, 0, 1] {
            // NAL unit start found
            send_packet(&data[..i]);
            data = &data[i..];
            i = 4;
            continue;
        }
        i += 1;
    }
    send_packet(data);
}

pub fn start_screencast(
    device: Arc<VulkanDevice>,
    net_sender: netnet::Sender,
) -> Result<(), janck::Error> {
    let (frame_sender, frame_receiver) = mpsc::sync_channel::<janck::Frame>(0);
    let video = janck::capture_video(FRAME_RATE as _)?;

    std::thread::spawn(move || {
        // Using a separate thread allows a frame to be captured while another one is being processed
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
            send_chunks(&net_sender, &encoded, frame_timestamp, width, height);
        }
    });
    info!("Started screen cast");
    Ok(())
}

pub fn start_input_handler(net_receiver: netnet::Receiver) -> anyhow::Result<()> {
    info!("Starting input handler");
    let mut enigo = Enigo::new(&enigo::Settings::default())?;
    info!("Created virtual keyboard");

    std::thread::spawn(move || {
        loop {
            let raw_packet = net_receiver.recv().unwrap();
            let Packet::Input(key) = wincode::deserialize(&raw_packet.body).unwrap() else {
                unreachable!();
            };
            info!("Read {:?}", key);
            enigo
                .key(enigo::Key::Unicode(key.char), key.action.into())
                .unwrap();
        }
    });
    Ok(())
}

pub fn start(
    device: Arc<VulkanDevice>,
    net_sender: netnet::Sender,
    net_receiver: netnet::Receiver,
) -> anyhow::Result<()> {
    info!("Starting screencast");
    start_screencast(device, net_sender)?;

    info!("Starting input handler");
    start_input_handler(net_receiver)?;
    Ok(())
}
