use std::collections::HashSet;

use wincode::{SchemaRead, SchemaWrite};

pub const HOST_PORT: u16 = 8084;

#[derive(Default, Clone, SchemaWrite, SchemaRead)]
pub struct Input {
    pub keys_pressed: HashSet<char>,
    pub mouse_position: Option<[f32; 2]>,
    pub left_mouse_pressed: bool,
    pub middle_mouse_pressed: bool,
    pub right_mouse_pressed: bool,
    pub scroll: [f64; 2],
}

#[derive(SchemaWrite, SchemaRead)]
pub enum Packet<'a> {
    /// Input state
    Input(Input),
    /// H.264 video fragment
    H264 {
        width: u32,
        height: u32,
        bytes: &'a [u8],
    },
}
