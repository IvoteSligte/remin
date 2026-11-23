use std::{collections::HashSet, time::Instant};

use wincode::{SchemaRead, SchemaWrite};

use crate::key::Key;

pub const HOST_PORT: u16 = 8084;

#[derive(Debug, Default, Clone, SchemaWrite, SchemaRead)]
pub struct Input {
    pub keys_pressed: HashSet<Key>,
    pub mouse_position: (f64, f64),
    pub left_mouse_pressed: bool,
    pub middle_mouse_pressed: bool,
    pub right_mouse_pressed: bool,
    pub scroll: (f64, f64),
}

/// H.264 video NAL unit (part of frame)
#[derive(SchemaWrite, SchemaRead)]
pub struct H264<'a> {
    pub width: u32,
    pub height: u32,
    pub bytes: &'a [u8],
    /// True if this NAL unit is the first unit of a keyframe
    pub is_keyframe_start: bool,
}

/// Opus audio chunk
#[derive(SchemaWrite, SchemaRead)]
pub struct Opus<'a> {
    pub chunk_id: u64,
    pub sample_rate: u32,
    /// True if stereo, false if mono
    pub is_stereo: bool,
    pub bytes: &'a [u8],
}

/// Returns the time in milliseconds since `start`
pub fn since(start: Instant) -> f32 {
    (Instant::now() - start).as_micros() as f32 / 1000.0
}
