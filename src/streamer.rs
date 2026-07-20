use anyhow::Context;
use enigo::{Enigo, Keyboard, Mouse};
use fps_ticker::Fps;
use gpu_video::VulkanDevice;
use log::{debug, info};
use netnet::{Connection, UnreliableReceiver, UnreliableSender};
use slint::platform::Key as SlintKey;
use std::sync::{Arc, mpsc};
use std::time::Instant;
use tracing::warn;

use crate::common::{Input, Packet};
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

pub struct ScreenCapture {
    video: mpsc::Receiver<janck::Frame>,
    info: janck::FrameInfo,
}

pub fn capture_screen() -> anyhow::Result<ScreenCapture> {
    let (frame_sender, frame_receiver) = mpsc::sync_channel::<janck::Frame>(0);
    let mut video = janck::capture_video(FRAME_RATE as _)?;
    let first_frame = video
        .next()
        .context("Failed to capture first frame of video")?;

    std::thread::spawn(move || {
        // Using a separate thread allows a frame to be captured while another one is being processed
        for frame in video {
            frame_sender.send(frame).unwrap();
        }
    });
    Ok(ScreenCapture {
        info: first_frame.info,
        video: frame_receiver,
    })
}

pub fn start_stream(
    device: Arc<VulkanDevice>,
    mut connection: UnreliableSender,
    screen_capture: ScreenCapture,
) -> Result<(), janck::Error> {
    std::thread::spawn(move || {
        let janck::FrameInfo {
            width,
            height,
            stride,
            format,
        } = screen_capture.info;
        let mut encoder =
            gpu::Encoder::new(&device, width, height, stride, format, FRAME_RATE).unwrap();
        let mut fps = Fps::default();

        // TODO: if janck can capture directly into [wgpu::Texture]s then the entire GPU upload step of encoding can be skipped
        for janck::Frame { bytes, info, .. } in screen_capture.video {
            assert_eq!(info, screen_capture.info); // TODO: handle screen resizing and such
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

fn slint_key_to_enigo(slint: char) -> enigo::Key {
    match slint {
        c if c == char::from(SlintKey::Shift) => enigo::Key::LShift,
        c if c == char::from(SlintKey::ShiftR) => enigo::Key::RShift,
        c if c == char::from(SlintKey::Return) => enigo::Key::Return,
        c if c == char::from(SlintKey::Control) => enigo::Key::LControl,
        c if c == char::from(SlintKey::ControlR) => enigo::Key::RControl,
        c if c == char::from(SlintKey::UpArrow) => enigo::Key::UpArrow,
        c if c == char::from(SlintKey::DownArrow) => enigo::Key::DownArrow,
        c if c == char::from(SlintKey::LeftArrow) => enigo::Key::LeftArrow,
        c if c == char::from(SlintKey::RightArrow) => enigo::Key::RightArrow,
        c if c == char::from(SlintKey::F1) => enigo::Key::F1,
        c if c == char::from(SlintKey::F2) => enigo::Key::F2,
        c if c == char::from(SlintKey::F3) => enigo::Key::F3,
        c if c == char::from(SlintKey::F4) => enigo::Key::F4,
        c if c == char::from(SlintKey::F5) => enigo::Key::F5,
        c if c == char::from(SlintKey::F6) => enigo::Key::F6,
        c if c == char::from(SlintKey::F7) => enigo::Key::F7,
        c if c == char::from(SlintKey::F8) => enigo::Key::F8,
        c if c == char::from(SlintKey::F9) => enigo::Key::F9,
        c if c == char::from(SlintKey::F10) => enigo::Key::F10,
        // F11 is not forwarded because it may be used as full-screen toggle in the future
        // F12 is not forwarded because F12 exits to the Remin main menu
        c => enigo::Key::Unicode(c),
    }
}

fn direction_from_pressed(pressed: bool) -> enigo::Direction {
    if pressed {
        enigo::Direction::Press
    } else {
        enigo::Direction::Release
    }
}

pub fn start_input_handler(
    mut connection: UnreliableReceiver,
    screen_width: u32,
    screen_height: u32,
) -> anyhow::Result<()> {
    info!("Starting input handler");
    let mut enigo = Enigo::new(&enigo::Settings::default())?;
    info!("Created virtual keyboard and mouse");

    tokio::task::spawn(async move {
        let mut prev_input = Input::default();

        loop {
            let bytes = connection.recv().await.unwrap();
            let Packet::Input(input) = wincode::deserialize(&bytes).unwrap() else {
                warn!("Streamer received unreliable non-input packet");
                continue;
            };
            let just_released = prev_input.keys_pressed.difference(&input.keys_pressed);
            let just_pressed = input.keys_pressed.difference(&prev_input.keys_pressed);
            for &slint_key in just_released {
                let enigo_key = slint_key_to_enigo(slint_key);
                debug!("Released key {:?}", enigo_key);
                enigo.key(enigo_key, enigo::Direction::Release).unwrap();
            }
            for &slint_key in just_pressed {
                let enigo_key = slint_key_to_enigo(slint_key);
                debug!("Pressed key {:?}", enigo_key);
                enigo.key(enigo_key, enigo::Direction::Press).unwrap();
            }
            if input.left_mouse_pressed != prev_input.left_mouse_pressed {
                debug!("Left mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Left,
                        direction_from_pressed(input.left_mouse_pressed),
                    )
                    .unwrap();
            }
            if input.middle_mouse_pressed != prev_input.middle_mouse_pressed {
                debug!("Middle mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Middle,
                        direction_from_pressed(input.middle_mouse_pressed),
                    )
                    .unwrap();
            }
            if input.right_mouse_pressed != prev_input.right_mouse_pressed {
                debug!("Right mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Right,
                        direction_from_pressed(input.right_mouse_pressed),
                    )
                    .unwrap();
            }
            if input.mouse_position.is_some() && input.mouse_position != prev_input.mouse_position {
                let [fraction_x, fraction_y] = input.mouse_position.unwrap();
                // enigo.main_display().size() can be used to get display dimensions on most devices,
                // but it does not seem to work on Wayland, so we use the screen capture dimensions
                let position_x = (screen_width as f64 * fraction_x as f64) as i32;
                let position_y = (screen_height as f64 * fraction_y as f64) as i32;
                enigo
                    .move_mouse(position_x, position_y, enigo::Coordinate::Abs)
                    .unwrap();
                debug!(
                    "Mouse moved to ({},{}) {:.3},{:.3}",
                    position_x, position_y, fraction_x, fraction_y
                );
            }
            if input.scroll != prev_input.scroll {
                let diff_x = input.scroll[0] - prev_input.scroll[0];
                let diff_y = input.scroll[1] - prev_input.scroll[1];
                debug!("Mouse scrolled by {:.0},{:.0}", diff_x, diff_y);
                enigo
                    .scroll(diff_x as i32, enigo::Axis::Horizontal)
                    .unwrap();
                enigo.scroll(diff_y as i32, enigo::Axis::Vertical).unwrap();
            }
            prev_input = input;
        }
    });
    Ok(())
}

pub fn start(device: Arc<VulkanDevice>, connection: Connection) -> anyhow::Result<()> {
    info!("Starting screen capture");
    let screen_capture = capture_screen()?;
    let screen_info = screen_capture.info;

    info!("Starting stream");
    start_stream(device, connection.unreliable_sender, screen_capture)?;

    info!("Starting input handler");
    start_input_handler(
        connection.unreliable_receiver,
        screen_info.width,
        screen_info.height,
    )?;
    Ok(())
}
