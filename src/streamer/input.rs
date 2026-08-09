use std::collections::HashSet;

use enigo::{Enigo, Keyboard, Mouse};
use log::{debug, info, trace};
use netnet::UnreliableReceiver;
use tokio::task::JoinHandle;

use crate::{
    common::Input,
    key::{Key, should_ignore_press},
};

pub(crate) fn direction_from_pressed(pressed: bool) -> enigo::Direction {
    match pressed {
        true => enigo::Direction::Press,
        false => enigo::Direction::Release,
    }
}

fn enigo_to_upper(mut key: enigo::Key) -> enigo::Key {
    if let enigo::Key::Unicode(c) = &mut key {
        *c = c.to_ascii_uppercase();
    }
    key
}

pub fn start_processor(
    mut connection: UnreliableReceiver,
    screen_width: u32,
    screen_height: u32,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    info!("Starting input handler");
    let mut enigo = Enigo::new(&enigo::Settings::default())?;
    info!("Created virtual keyboard and mouse");

    let handle = tokio::task::spawn(async move {
        let mut prev_input = Input::default();
        let mut keys_pressed_upper = HashSet::new();

        loop {
            let bytes = connection.recv().await.unwrap();
            let input: Input = wincode::deserialize(bytes).unwrap();
            let just_released = prev_input.keys_pressed.difference(&input.keys_pressed);
            let just_pressed = input.keys_pressed.difference(&prev_input.keys_pressed);
            for &key in just_released {
                let enigo_key = key.into();
                debug!("Released key {:?}", enigo_key);
                if keys_pressed_upper.remove(&key) {
                    enigo
                        .key(enigo_to_upper(enigo_key), enigo::Direction::Release)
                        .unwrap();
                } else {
                    enigo.key(enigo_key, enigo::Direction::Release).unwrap();
                }
            }
            for &key in just_pressed {
                println!("[before] keys pressed: {:?}", &input.keys_pressed);
                if should_ignore_press(&input.keys_pressed, key) {
                    continue;
                }
                println!("[after] keys pressed: {:?}", &input.keys_pressed);
                let mut enigo_key = key.into();
                if input.keys_pressed.contains(&Key::Shift) {
                    enigo_key = enigo_to_upper(enigo_key);
                    keys_pressed_upper.insert(key);
                }
                println!(
                    "[enigo] keys pressed: {:?}",
                    &input
                        .keys_pressed
                        .iter()
                        .map(|key| enigo::Key::from(*key))
                        .collect::<Vec<_>>()
                );
                debug!("Pressed key {:?}", enigo_key);
                enigo.key(enigo_key, enigo::Direction::Press).unwrap();
            }
            prev_input.keys_pressed = input.keys_pressed;
            if input.left_mouse_pressed != prev_input.left_mouse_pressed {
                debug!("Left mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Left,
                        direction_from_pressed(input.left_mouse_pressed),
                    )
                    .unwrap();
                prev_input.left_mouse_pressed = input.left_mouse_pressed;
            }
            if input.middle_mouse_pressed != prev_input.middle_mouse_pressed {
                debug!("Middle mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Middle,
                        direction_from_pressed(input.middle_mouse_pressed),
                    )
                    .unwrap();
                prev_input.middle_mouse_pressed = input.middle_mouse_pressed;
            }
            if input.right_mouse_pressed != prev_input.right_mouse_pressed {
                debug!("Right mouse pressed: {}", input.left_mouse_pressed);
                enigo
                    .button(
                        enigo::Button::Right,
                        direction_from_pressed(input.right_mouse_pressed),
                    )
                    .unwrap();
                prev_input.right_mouse_pressed = input.right_mouse_pressed;
            }
            if input.mouse_position != prev_input.mouse_position {
                let (normalized_x, normalized_y) = input.mouse_position;
                let (prev_normalized_x, prev_normalized_y) = prev_input.mouse_position;
                let diff_x = normalized_x - prev_normalized_x;
                let diff_y = normalized_y - prev_normalized_y;
                // enigo.main_display().size() can be used to get display dimensions on most devices,
                // but it does not seem to work on Wayland, so we use the screen capture dimensions.
                //
                // The offset must be between [i16::MIN, i64::MAX] on some platforms, so restrict it to that.
                let offset_x = (screen_width as f64 * diff_x) as i16 as i32;
                let offset_y = (screen_height as f64 * diff_y) as i16 as i32;

                // Offset may be (0,0) due to rounding
                if offset_x != 0 || offset_y != 0 {
                    enigo
                        .move_mouse(offset_x, offset_y, enigo::Coordinate::Rel)
                        .unwrap();
                    trace!(
                        "Mouse moved by {},{}",
                        offset_x as f64 / screen_width as f64,
                        offset_y as f64 / screen_height as f64
                    );
                    // The offset has been rounded, which prev_input = input would not account for.
                    prev_input.mouse_position = (
                        prev_normalized_x + offset_x as f64 / screen_width as f64,
                        prev_normalized_y + offset_y as f64 / screen_height as f64,
                    );
                }
            }
            if input.scroll != prev_input.scroll {
                let diff_x = input.scroll.0 - prev_input.scroll.0;
                let diff_y = input.scroll.1 - prev_input.scroll.1;
                debug!("Mouse scrolled by {:.0},{:.0}", diff_x, diff_y);
                enigo
                    .scroll(-diff_x as i32, enigo::Axis::Horizontal)
                    .unwrap();
                enigo.scroll(-diff_y as i32, enigo::Axis::Vertical).unwrap();
                prev_input.scroll = input.scroll;
            }
        }
    });
    Ok(handle)
}
