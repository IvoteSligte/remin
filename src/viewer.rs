use gpu_video::VulkanDevice;
use log::{debug, info, warn};
use netnet::Connection;
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
    connection: Arc<Connection>
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let mut decoder = None;

    weak.upgrade_in_event_loop(|app| {
        app.set_view("viewer".into());
    })?;

    tokio::task::spawn(async move {
        let fps = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();
        loop {
            let bytes = connection.read_datagram().await.unwrap();
            let packet: Packet = wincode::deserialize(&bytes).unwrap();
            match packet {
                Packet::Input(_) => unreachable!("Client should not receive input packets"),
                Packet::H264 {
                    bytes,
                    width,
                    height,
                } => {
                    fps.tick();
                    debug!("Received frame from server ({:.2} fps)", fps.avg());
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
    connection: Arc<Connection>
) -> anyhow::Result<()> {
    start_renderer(weak.clone(), device, connection.clone())?;
    weak.upgrade_in_event_loop(move |app| {
        start_input_handler(&app, connection);
    })?;
    Ok(())
}
