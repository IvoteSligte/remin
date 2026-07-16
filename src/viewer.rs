use gpu_video::VulkanDevice;
use log::{debug, info, warn};
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
    mut net_receiver: netnet::Receiver,
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let mut decoder = None;

    weak.upgrade_in_event_loop(|app| {
        app.set_view("viewer".into());
    })?;

    std::thread::spawn(move || {
        let fps = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();
        loop {
            let raw_packet = net_receiver.recv().unwrap();
            let packet: Packet = wincode::deserialize(&raw_packet).unwrap();
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

pub fn start_input_handler(app: &App, mut net_sender: netnet::Sender) {
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
        let raw_packet = wincode::serialize(&packet).unwrap();
        net_sender.send(raw_packet).unwrap();
    });
    info!("Registered input handler");
}

pub fn start(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    net_sender: netnet::Sender,
    net_receiver: netnet::Receiver,
) -> anyhow::Result<()> {
    start_renderer(weak.clone(), device, net_receiver)?;
    weak.upgrade_in_event_loop(move |app| {
        start_input_handler(&app, net_sender);
    })?;
    Ok(())
}
