use enigo::Direction;
use wincode::{SchemaRead, SchemaWrite};

use std::time::Duration;

pub const SERVER_TCP_PORT: u16 = 8084;

pub const MAX_LATENCY: Duration = Duration::from_millis(100);

pub type PacketStreams = (netnet::Sender<Packet>, netnet::Receiver);

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
    /// H.264 video frame
    H264Frame {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    },
}
