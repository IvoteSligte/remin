pub const CLIENT_UDP_PORT: u16 = 8083;
pub const SERVER_TCP_PORT: u16 = 8084;
pub const SERVER_UDP_PORT: u16 = 8085;

use std::io;

use enigo::Direction;
use wincode::{SchemaRead, SchemaWrite};

use crate::{tcp, udp};

pub fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

pub struct RunningAverage {
    value: f32,
    samples: f32,
    convergence_window: f32,
}

impl RunningAverage {
    pub fn new(convergence_window: f32) -> Self {
        assert!(convergence_window >= 1.0);
        Self {
            value: 0.0,
            samples: 1.0,
            convergence_window,
        }
    }

    pub fn update(&mut self, sample: f32) {
        let weight = 1.0 / self.samples;
        self.value = self.value * (1.0 - weight) + sample * weight;
        self.samples = f32::min(self.samples + 1.0, self.convergence_window);
    }

    pub fn get(&self) -> f32 {
        self.value
    }
}

pub type PacketStreams = (tcp::PacketStream, udp::PacketStream);

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
