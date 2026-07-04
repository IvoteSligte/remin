use bitcode::{Decode, Encode};
use enigo::Direction;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::OnceLock,
};

pub const PORT: u16 = 8082;

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

pub struct PacketStream(TcpStream);

impl PacketStream {
    pub fn new(stream: TcpStream) -> Self {
        stream.set_nonblocking(true).unwrap();
        Self(stream)
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.0.shutdown(Shutdown::Both)
    }

    pub fn send(&mut self, packet: &Packet) -> io::Result<()> {
        let stream = &mut self.0;
        let bytes = bitcode::encode(packet);
        stream.set_nonblocking(false).unwrap();
        stream.write(&u32::to_le_bytes(bytes.len() as u32))?;
        stream.write(&bytes)?;
        stream.set_nonblocking(true).unwrap();
        Ok(())
    }

    /// Returns `Ok(None)` if there is no new packet.
    pub fn recv(&mut self) -> anyhow::Result<Option<Packet>> {
        let stream = &mut self.0;
        let mut size_bytes = [0u8; 4];
        match stream.read_exact(&mut size_bytes) {
            Ok(()) => (),
            Err(ref err) if would_block(err) => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        let size = u32::from_le_bytes(size_bytes) as usize;
        let mut buf = Vec::with_capacity(size);
        unsafe {
            buf.set_len(size);
            stream.set_nonblocking(false).unwrap();
            stream.read_exact(&mut buf)?;
            stream.set_nonblocking(true).unwrap();
        }
        Ok(Some(bitcode::decode(&buf)?))
    }
}

#[derive(Encode, Decode)]
pub enum Packet {
    /// Keyboard input
    Input(Key),
    /// RGB8 video frame
    Rgb8 {
        timestamp: i64,
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

#[repr(u8)]
#[derive(Debug, Encode, Decode)]
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

#[derive(Debug, Encode, Decode)]
pub struct Key {
    pub char: char,
    pub action: Action,
}
