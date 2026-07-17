use enigo::{Enigo, Keyboard};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use log::{debug, info};
use netnet::Connection;
use std::sync::{Arc, mpsc};
use std::time::Instant;

use crate::common::Packet;
use crate::gpu;

// TODO: UI element for adjusting these parameters
// TODO: resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;

fn send_chunks(
    connection: &Connection,
    mut data: &[u8],
    frame_index: u64,
    width: u32,
    height: u32,
) {
    // max datagram size - (sizeof(width) + sizeof(height) + sizeof(&[u8]) + sizeof(frame_index) + sizeof(fragment_index) + sizeof(total_fragments))
    let max_chunk_size = connection.max_datagram_size().unwrap() - 36;
    let mut packet_queue = Vec::with_capacity(data.len().div_ceil(max_chunk_size));

    // TODO: send small NAL units together
    let mut i = 4;
    while data.len() > max_chunk_size {
        if i >= max_chunk_size || &data[i..usize::min(i + 4, data.len())] == &[0, 0, 0, 1] {
            // NAL unit start found
            packet_queue.push(&data[..i]);
            data = &data[i..];
            i = 4;
            continue;
        }
        i += 1;
    }
    packet_queue.push(data);

    for (i, slice) in packet_queue.iter().enumerate() {
        let bytes = wincode::serialize(&Packet::H264 {
            frame_index,
            fragment_index: i as u32,
            total_fragments: packet_queue.len() as u32,
            width,
            height,
            bytes: slice,
        })
        .unwrap();
        connection.send_datagram(bytes.into()).unwrap();
    }
}

pub fn start_screencast(
    device: Arc<VulkanDevice>,
    connection: Arc<Connection>,
) -> Result<(), janck::Error> {
    let (frame_sender, frame_receiver) = mpsc::sync_channel::<janck::Frame>(0);
    let video = janck::capture_video(FRAME_RATE as _)?;

    for _ in 0..100 {
        let packet = Packet::IAmCaster;
        let bytes = wincode::serialize(&packet).unwrap();
        connection.send_datagram(bytes.into()).unwrap();
    }
    std::thread::spawn(move || {
        // Using a separate thread allows a frame to be captured while another one is being processed
        for frame in video {
            frame_sender.send(frame).unwrap();
        }
    });
    std::thread::spawn(move || {
        let mut encoder = None;
        let fps = Fps::default();
        let mut frame_index = 0;

        // TODO: if janck can capture directly into [wgpu::Texture]s then the entire GPU upload step of encoding can be skipped
        for janck::Frame {
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
            send_chunks(&connection, &encoded, frame_index, width, height);
            frame_index += 1;
        }
    });
    info!("Started screen cast");
    Ok(())
}

pub fn start_input_handler(connection: Arc<Connection>) -> anyhow::Result<()> {
    info!("Starting input handler");
    let mut enigo = Enigo::new(&enigo::Settings::default())?;
    info!("Created virtual keyboard");

    tokio::task::spawn(async move {
        loop {
            let bytes = connection.read_datagram().await.unwrap();
            let Packet::Input(key) = wincode::deserialize(&bytes).unwrap() else {
                unreachable!();
            };
            info!("Read {:?}", key);
            // TODO: add modifier key support (simple key press/release)
            enigo
                .key(enigo::Key::Unicode(key.char), key.action.into())
                .unwrap();
        }
    });
    Ok(())
}

pub fn start(device: Arc<VulkanDevice>, connection: Arc<Connection>) -> anyhow::Result<()> {
    info!("Starting screencast");
    start_screencast(device, connection.clone())?;

    info!("Starting input handler");
    start_input_handler(connection)?;
    Ok(())
}
