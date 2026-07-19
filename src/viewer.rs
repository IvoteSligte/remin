use gpu_video::VulkanDevice;
use log::{debug, info, warn};
use netnet::{Connection, UnreliableReceiver, UnreliableSender};
use slint::Weak;
use std::{sync::Arc, time::Instant};

use crate::{
    App,
    common::{Action, Key, Packet},
    gpu,
};

pub fn start_renderer(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    mut conn: UnreliableReceiver,
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let mut decoder = None;

    weak.upgrade_in_event_loop(|app| {
        app.set_view("viewer".into());
    })?;

    let (packet_sender, mut packet_receiver) = tokio::sync::mpsc::channel(100);

    tokio::task::spawn(async move {
        loop {
            let packet = conn.recv().unwrap();
            packet_sender.send(packet).await.unwrap();
        }
    });

    tokio::task::spawn(async move {
        let frames_per_second = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();

        while let Some(bytes) = packet_receiver.recv().await {
            let packet: Packet = wincode::deserialize(&bytes).unwrap();
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::IAmCaster => (),
                Packet::H264 {
                    width,
                    height,
                    bytes,
                } => {
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
                        .decode(&bytes)
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

pub fn start_input_handler(app: &App, mut conn: UnreliableSender) {
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
        conn.send(&bytes).unwrap();
    });
    info!("Registered input handler");
}

pub fn start(weak: Weak<App>, device: Arc<VulkanDevice>, conn: Connection) -> anyhow::Result<()> {
    start_renderer(weak.clone(), device, conn.unreliable_receiver)?;
    weak.upgrade_in_event_loop(move |app| {
        start_input_handler(&app, conn.unreliable_sender);
    })?;
    Ok(())
}
