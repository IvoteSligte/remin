use std::collections::HashSet;

use wincode::{SchemaRead, SchemaWrite};

pub const HOST_PORT: u16 = 8084;

#[derive(SchemaWrite, SchemaRead)]
pub enum Packet<'a> {
    /// Input state
    Input { pressed: HashSet<char>, },
    /// H.264 video fragment
    H264 {
        width: u32,
        height: u32,
        bytes: &'a [u8],
    },
}
