use enigo::{Enigo, Keyboard};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use log::{debug, info};
use netnet::{Connection, UnreliableReceiver, UnreliableSender};
use slint::platform::Key as SlintKey;
use std::sync::{Arc, mpsc};
use std::time::Instant;

use crate::common::Packet;
use crate::gpu;

// TODO: UI element for adjusting these parameters
// TODO: resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;

fn send_nal_units(
    connection: &mut UnreliableSender,
    mut bytes: &[u8],
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    // max size - (sizeof(width) + sizeof(height) + sizeof(slice))
    let fragment_size = connection.max_fragment_size() - 20;
    let mut send = |unit_bytes: &[u8]| {
        let nal_unit = wincode::serialize(&Packet::H264 {
            width,
            height,
            bytes: unit_bytes,
        })
        .unwrap();
        connection.send(&nal_unit)
    };
    let mut i = 4;
    while bytes.len() > fragment_size && (i + 4) <= bytes.len() {
        if &bytes[i..i + 4] == &[0, 0, 0, 1] {
            // NAL unit start found
            send(&bytes[..i])?;
            bytes = &bytes[i..];
            i = 4;
            continue;
        }
        i += 1;
    }
    if bytes.len() > 0 {
        send(bytes)?;
    }
    Ok(())
}

pub fn start_screencast(
    device: Arc<VulkanDevice>,
    mut connection: UnreliableSender,
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
            send_nal_units(&mut connection, &encoded, width, height).unwrap();
        }
    });
    info!("Started screen cast");
    Ok(())
}

pub fn start_input_handler(mut connection: UnreliableReceiver) -> anyhow::Result<()> {
    info!("Starting input handler");
    let mut enigo = Enigo::new(&enigo::Settings::default())?;
    info!("Created virtual keyboard");

    tokio::task::spawn(async move {
        loop {
            let bytes = connection.recv().await.unwrap();
            let Packet::Input(key) = wincode::deserialize(&bytes).unwrap() else {
                unreachable!();
            };
            let enigo_key = match key.char {
                c if c == char::from(SlintKey::Shift) => enigo::Key::LShift,
                c if c == char::from(SlintKey::ShiftR) => enigo::Key::RShift,
                c if c == char::from(SlintKey::Return) => enigo::Key::Return,
                c if c == char::from(SlintKey::Control) => enigo::Key::LControl,
                c if c == char::from(SlintKey::ControlR) => enigo::Key::RControl,
                c if c == char::from(SlintKey::UpArrow) => enigo::Key::UpArrow,
                c if c == char::from(SlintKey::DownArrow) => enigo::Key::DownArrow,
                c if c == char::from(SlintKey::LeftArrow) => enigo::Key::LeftArrow,
                c if c == char::from(SlintKey::RightArrow) => enigo::Key::RightArrow,
                c => enigo::Key::Unicode(c),
            };
            info!("Read {:?}", key);
            enigo.key(enigo_key, key.action.into()).unwrap();
        }
    });
    Ok(())
}

pub fn start(device: Arc<VulkanDevice>, connection: Connection) -> anyhow::Result<()> {
    info!("Starting screencast");
    start_screencast(device, connection.unreliable_sender)?;

    info!("Starting input handler");
    start_input_handler(connection.unreliable_receiver)?;
    Ok(())
}
