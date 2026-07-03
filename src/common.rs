use enigo::Direction::{self, Press, Release};
use std::{io::Read, net::TcpStream, sync::OnceLock};

pub const INPUT_PORT: u16 = 8082;
pub const VIDEO_PORT: u16 = 8083;

pub struct Signal(OnceLock<()>);

impl Signal {
    pub fn new() -> Self {
        Self(OnceLock::new())
    }

    pub fn signal(&self) {
        self.0.set(()).unwrap();
    }

    pub fn signaled(&self) -> bool {
        self.0.get().is_some()
    }

    pub fn wait(&self) {
        self.0.wait();
    }
}

#[derive(Debug)]
pub struct Key {
    pub char: char,
    pub action: Direction,
}

impl Key {
    pub fn encode(self) -> [u8; 5] {
        let [_0, _1, _2, _3] = u32::to_le_bytes(self.char as u32);
        let _4 = match self.action {
            Press => 0,
            Release => 1,
            _ => unreachable!(),
        };
        [_0, _1, _2, _3, _4]
    }

    pub fn decode(bytes: [u8; 5]) -> Self {
        let [_0, _1, _2, _3, _4] = bytes;
        Self {
            char: char::try_from(u32::from_le_bytes([_0, _1, _2, _3])).unwrap(),
            action: match _4 {
                0 => Press,
                1 => Release,
                _ => unreachable!(),
            },
        }
    }
}

pub fn read_exact_non_blocking<const N: usize>(stream: &mut TcpStream) -> Option<[u8; N]> {
    let mut bytes = [0u8; _];
    let available = stream.peek(&mut bytes).unwrap();
    if available >= N {
        stream.read_exact(&mut bytes).unwrap();
        return Some(bytes);
    }
    None
}
