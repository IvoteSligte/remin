use gpu_video::VulkanDevice;
use log::{debug, info, warn};
use netnet::{Connection, UnreliableReceiver, UnreliableSender};
use slint::{ComponentHandle, Weak, platform::PointerEventButton, winit_030::WinitWindowAccessor};
use std::{cell::RefCell, ops::DerefMut, rc::Rc, sync::Arc, time::Instant};

use crate::{
    App,
    common::{Input, Packet},
    gpu,
};

pub fn start_renderer(
    weak: Weak<App>,
    device: Arc<VulkanDevice>,
    mut conn: UnreliableReceiver,
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let mut decoder = None;
    let (packet_sender, mut packet_receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(100);

    tokio::task::spawn(async move {
        loop {
            let packet = conn.recv().await.unwrap();
            packet_sender.send(packet).await.unwrap();
        }
    });

    tokio::task::spawn(async move {
        let mut frames_per_second = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();

        while let Some(bytes) = packet_receiver.recv().await {
            let packet: Packet = wincode::deserialize(&bytes).unwrap();
            match packet {
                Packet::Input { .. } => warn!("Watcher received an input packet"),
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

fn update_input(state: &Rc<RefCell<(UnreliableSender, Input)>>, mut f: impl FnMut(&mut Input)) {
    let mut state = state.borrow_mut();
    let (conn, input) = state.deref_mut();
    f(input);
    let packet = Packet::Input(input.clone());
    let bytes = wincode::serialize(&packet).unwrap();
    conn.send(&bytes).unwrap();
}

// Note that running two instances of remin locally (one host, one client) causes a feedback loop.
pub fn start_input_handler(app: &App, conn: UnreliableSender) {
    info!("Acquiring winit window handle");
    let window = tokio::runtime::Handle::current()
        .block_on(app.window().winit_window())
        .unwrap();
    // TODO: unlock and unhide cursor on_escape
    {
        use slint::winit_030::winit::window::CursorGrabMode;
        info!("Trying to confine cursor to window");
        if let Err(err) = window.set_cursor_grab(CursorGrabMode::Confined) {
            warn!("Failed to confine cursor to window: {err}");
            if let Err(err) = window.set_cursor_grab(CursorGrabMode::Locked) {
                warn!("Failed to lock cursor to window (fallback): {err}");
            };
        };
        window.set_cursor_visible(false);
    }

    // Callbacks are executed sequentially on the main event loop thread,
    // so an Arc+Mutex is not necessary
    let state = Rc::new(RefCell::new((conn, Input::default())));
    let state2 = state.clone();
    let state3 = state.clone();
    let state4 = state.clone();

    app.on_keyboard_input(move |text, action| {
        // `text` is a string because slint does not work with characters
        let Some(char) = text.chars().next() else {
            return;
        };
        debug!("Key {:?}: '{}' = {}", action, char, char as u32);
        update_input(&state, |input| {
            match action {
                crate::KeyAction::Press => input.keys_pressed.insert(char),
                crate::KeyAction::Release => input.keys_pressed.remove(&char),
            };
        });
    });
    app.on_mouse_input(move |button, action| {
        let pressed = action == crate::KeyAction::Press;
        update_input(&state2, |input| match button {
            PointerEventButton::Left => input.left_mouse_pressed = pressed,
            PointerEventButton::Right => input.right_mouse_pressed = pressed,
            PointerEventButton::Middle => input.middle_mouse_pressed = pressed,
            _ => (),
        });
    });
    let weak = app.as_weak();
    app.on_mouse_move(move |delta_x, delta_y| {
        let window_size = weak.upgrade().unwrap().window().size();
        update_input(&state3, |input| {
            input.mouse_position[0] += delta_x as f64 / window_size.width as f64;
            input.mouse_position[1] += delta_y as f64 / window_size.height as f64;
        });
        debug!("Moved remote mouse by {delta_x},{delta_y}");
    });
    app.on_scroll_input(move |delta_x, delta_y| {
        update_input(&state4, |input| {
            input.scroll[0] += delta_x as f64;
            input.scroll[1] += delta_y as f64;
        });
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
