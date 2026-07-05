use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use wincode::{SchemaRead, SchemaWrite};

use crate::{common::would_block, signal::Signal};

#[derive(Clone)]
pub struct PacketStream {
    stream: Arc<Mutex<TcpStream>>,
    stop: Signal,
}

impl PacketStream {
    pub fn new_client(server_addr: SocketAddr, stop: Signal) -> io::Result<Self> {
        let stream = TcpStream::connect(server_addr)?;
        stream.set_nonblocking(true).unwrap();
        let stream = Arc::new(Mutex::new(stream));
        Ok(Self { stream, stop })
    }

    /// Returns self and the client address.
    pub fn new_server(port: u16, stop: Signal) -> io::Result<Option<(Self, SocketAddr)>> {
        let listener = TcpListener::bind(format!("0.0.0.0:{port}"))?;
        listener.set_nonblocking(true).unwrap();

        let (stream, client_addr) = loop {
            if stop.signaled() {
                return Ok(None);
            }
            match listener.accept() {
                Ok(client) => break client,
                Err(ref err) if would_block(err) => continue,
                Err(err) => return Err(err),
            }
        };
        stream.set_nonblocking(true).unwrap();
        let stream = Arc::new(Mutex::new(stream));
        Ok(Some((Self { stream, stop }, client_addr)))
    }

    pub fn send(&self, packet: &Packet) -> io::Result<()> {
        assert!(!self.stop.signaled()); // TODO: self.stream.shutdown() on stop signal
        let data = wincode::serialize(packet).unwrap();
        blocking(&mut *self.stream.lock().unwrap(), |stream| {
            stream.write(&u32::to_le_bytes(data.len() as _))?;
            stream.write(&data)?;
            Ok(())
        })
    }

    pub fn recv(&self) -> anyhow::Result<Packet> {
        loop {
            assert!(!self.stop.signaled());
            if let Some(packet) = self.recv_non_blocking()? {
                return Ok(packet);
            }
        }
    }

    fn recv_non_blocking(&self) -> anyhow::Result<Option<Packet>> {
        let mut size_bytes = [0u8; 4];
        let stream = &mut *self.stream.lock().unwrap();
        match stream.read_exact(&mut size_bytes) {
            Ok(_) => (),
            Err(ref err) if would_block(err) => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        let size = u32::from_le_bytes(size_bytes);
        let mut data = vec![0u8; size as _];
        blocking(stream, |stream| stream.read_exact(&mut data))?;
        let packet = wincode::deserialize(&data)?;
        Ok(Some(packet))
    }
}

fn blocking<T>(stream: &mut TcpStream, mut scope: impl FnMut(&mut TcpStream) -> T) -> T {
    stream.set_nonblocking(false).unwrap();
    let result = scope(stream);
    stream.set_nonblocking(true).unwrap();
    result
}

#[derive(SchemaRead, SchemaWrite)]
pub struct Packet {}
