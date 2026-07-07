pub const CLIENT_UDP_PORT: u16 = 8083;
pub const SERVER_TCP_PORT: u16 = 8084;
pub const SERVER_UDP_PORT: u16 = 8085;

use std::io;

use enigo::Direction;
use wincode::{SchemaRead, SchemaWrite};

use crate::tcp;

pub fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

pub type PacketStreams = (tcp::PacketStream, netnet::PacketStream<Packet>);

#[repr(u8)]
#[derive(Debug, SchemaRead, SchemaWrite)]
pub enum Action {
    Press = 0,
    Release = 1,
}

impl From<Direction> for Action {
    fn from(value: Direction) -> Self {
        match value {
            Direction::Press => Self::Press,
            Direction::Release => Self::Release,
            Direction::Click => unimplemented!(),
        }
    }
}

impl From<Action> for Direction {
    fn from(value: Action) -> Self {
        match value {
            Action::Press => Self::Press,
            Action::Release => Self::Release,
        }
    }
}

#[derive(Debug, SchemaRead, SchemaWrite)]
pub struct Key {
    pub char: char,
    pub action: Action,
}

#[derive(SchemaWrite, SchemaRead)]
pub enum Packet {
    /// Keyboard input
    Input(Key),
    /// YUV video frame
    Yuv {
        width: u32,
        height: u32,
        y_stride: u32,
        u_stride: u32,
        v_stride: u32,
        y_plane: Vec<u8>,
        u_plane: Vec<u8>,
        v_plane: Vec<u8>,
    },
}
