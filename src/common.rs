use enigo::Direction;
use netnet::TimeDelta;
use wincode::{SchemaRead, SchemaWrite};

pub const SERVER_PORT: u16 = 8084;
pub const MAX_LATENCY: TimeDelta = TimeDelta::milliseconds(10000); // TODO: reset to 100

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
pub enum Packet<'a> {
    /// Keyboard input
    Input(Key),
    /// H.264 video fragment
    H264 {
        frame_timestamp: i64,
        width: u32,
        height: u32,
        bytes: &'a [u8],
    },
}
