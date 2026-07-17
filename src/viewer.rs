use gpu_video::VulkanDevice;
use log::{debug, info, trace, warn};
use netnet::Connection;
use slint::Weak;
use std::{iter, sync::Arc, time::Instant};

use crate::{
    App,
    common::{Action, Key, Packet},
    gpu,
};

pub fn start_renderer(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    connection: Arc<Connection>,
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let mut decoder = None;

    weak.upgrade_in_event_loop(|app| {
        app.set_view("viewer".into());
    })?;

    let (packet_sender, mut packet_receiver) = tokio::sync::mpsc::channel(100);

    tokio::task::spawn(async move {
        loop {
            let packet = connection.read_datagram().await.unwrap();
            packet_sender.send(packet).await.unwrap();
        }
    });

    tokio::task::spawn(async move {
        let fragments_per_second = fps_ticker::Fps::default();
        let frames_per_second = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();
        let mut current_frame_index = 0;
        let mut fragment_map = Vec::with_capacity(100);
        let mut num_fragments_found = 0;

        while let Some(bytes) = packet_receiver.recv().await {
            let packet: Packet = wincode::deserialize(&bytes).unwrap();
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::IAmCaster => (),
                Packet::H264 {
                    frame_index,
                    fragment_index,
                    total_fragments,
                    width,
                    height,
                    bytes,
                } => {
                    fragments_per_second.tick();
                    trace!(
                        "Received frame {} fragment {}/{} from server ({:.0}/s)",
                        frame_index,
                        fragment_index,
                        total_fragments,
                        fragments_per_second.avg()
                    );
                    if frame_index > current_frame_index {
                        debug!(
                            "Skipping incomplete frame {} with {}/{} fragments",
                            current_frame_index, num_fragments_found, total_fragments
                        );
                    }
                    if frame_index > current_frame_index || num_fragments_found == 0 {
                        fragment_map.clear();
                        fragment_map.extend(iter::repeat(Vec::new()).take(total_fragments as _));
                        current_frame_index = frame_index;
                    }
                    debug_assert!(fragment_index < total_fragments);
                    debug_assert!(total_fragments as usize == fragment_map.len());

                    if !fragment_map[fragment_index as usize].is_empty() {
                        trace!("Duplicate fragment {}", fragment_index);
                        continue;
                    }
                    fragment_map[fragment_index as usize] = bytes.to_vec();
                    num_fragments_found += 1;
                    if num_fragments_found < total_fragments {
                        continue;
                    }
                    current_frame_index += 1;
                    num_fragments_found = 0;
                    trace!(
                        "Gathered all {} fragments for frame {}",
                        total_fragments, frame_index,
                    );
                    let frame_bytes = fragment_map.iter().flatten().copied().collect::<Vec<u8>>();
                    let pre_decode = Instant::now();
                    match decoder
                        .get_or_insert_with(|| {
                            gpu::Decoder::new(
                                device.clone(),
                                device.wgpu_queue(),
                                weak.clone(),
                                width,
                                height,
                            )
                            .unwrap()
                        })
                        .decode(&frame_bytes)
                    {
                        Ok(()) => {
                            frames_per_second.tick();
                            debug!("Rendering new frame ({:.2}/s)", frames_per_second.avg());
                        }
                        Err(gpu::DecoderError::NoNewFrame) => {
                            debug!("Not enough frame data to construct a new frame");
                            continue;
                        }
                        // TODO: restart video stream if many sequential errors have been encountered
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
                }
            }
        }
    });
    Ok(())
}

pub fn start_input_handler(app: &App, connection: Arc<Connection>) {
    app.on_keyboard_input(move |text, action| {
        // text is only a string because slint does not work with characters
        let Some(char) = text.chars().next() else {
            return;
        };
        info!("Key {}: '{}' = {}", action, char, char as u32);
        let packet = Packet::Input(Key {
            char,
            action: if action == "pressed" {
                Action::Press
            } else {
                Action::Release
            },
        });
        let bytes = wincode::serialize(&packet).unwrap();
        connection.send_datagram(bytes.into()).unwrap();
    });
    info!("Registered input handler");
}

pub fn start(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    connection: Arc<Connection>,
) -> anyhow::Result<()> {
    start_renderer(weak.clone(), device, connection.clone())?;
    weak.upgrade_in_event_loop(move |app| {
        start_input_handler(&app, connection);
    })?;
    Ok(())
}
