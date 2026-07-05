use chrono::Utc;
use log::{debug, trace};
use rustc_hash::FxHashMap;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    common::{Key, would_block},
    signal::Signal,
};

// Fixed constants
const MAX_MESSAGE_SIZE: usize = 65507;
const MESSAGE_HEADER_SIZE: usize = std::mem::size_of::<MessageHeader>(); // the encoded size of MessageHeader in bytes
const MAX_MESSAGE_BODY_SIZE: usize = MAX_MESSAGE_SIZE - MESSAGE_HEADER_SIZE;

// Can be adjusted
const MAX_LATENCY_NS: i64 = 100 * 1000; // 100 milliseconds

// NOTE: make sure there is no implicit padding to prevent encoding/decoding mismatches
#[derive(Debug, SchemaRead, SchemaWrite)]
struct MessageHeader {
    timestamp: i64,
    packet_id: u32,
    message_id: u16,
    last_message_in_packet: u16,
}

struct PacketInfo {
    bytes: Vec<u8>,
    found: Vec<bool>,
    num_found: usize,
}

// TODO: periodic connectivity check (i.e. keepalive packets)
// TODO: buffering? probably necessary to get smooth audio
#[derive(Clone)]
pub struct PacketStream {
    stop: Signal,
    sync: Arc<Mutex<PacketStreamSync>>,
}

impl PacketStream {
    pub fn new(port: u16, connect_to: SocketAddr, stop: Signal) -> io::Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{port}"))?;
        socket.connect(connect_to)?;
        socket.set_nonblocking(true).unwrap();
        let mut packet_map = FxHashMap::<_, PacketInfo>::default();
        packet_map.reserve(1000);
        Ok(Self {
            stop,
            sync: Arc::new(Mutex::new(PacketStreamSync {
                socket,
                message_buf: vec![0u8; MAX_MESSAGE_SIZE],
                packet_map,
                next_packet_id: 0,
                last_received_packet_id: 0,
            })),
        })
    }

    pub fn send(&self, packet: &Packet) -> io::Result<()> {
        assert!(!self.stop.signaled());
        let data = wincode::serialize(&packet).unwrap();
        blocking(&mut *self.sync.lock().unwrap(), |sync| sync.send(&data))
    }

    /// Receives a packet, panicking if stop has been signaled.
    pub fn recv(&self) -> anyhow::Result<Packet> {
        loop {
            assert!(!self.stop.signaled());
            match self.sync.lock().unwrap().recv_non_blocking() {
                Ok(Some(packet)) => return Ok(packet),
                Ok(None) => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

/// PacketStream components requiring mutable access
struct PacketStreamSync {
    socket: UdpSocket,
    /// Fixed-size buffer used to store messages during encoding and decoding
    message_buf: Vec<u8>,
    /// Map from packet ID to packet information
    packet_map: FxHashMap<u32, PacketInfo>,
    /// ID of the next packet to send
    next_packet_id: u32,
    /// ID of the last complete package received
    last_received_packet_id: u32,
}

impl PacketStreamSync {
    pub fn send(&mut self, data: &[u8]) -> io::Result<()> {
        // TODO: smarter reliability mechanism than just sending each message N times
        // TODO: encryption
        assert!(data.len() > 0);
        let num_messages = data.len().div_ceil(MAX_MESSAGE_BODY_SIZE);
        debug!(
            "Sending packet {} with {} body bytes ({} messages)",
            self.next_packet_id,
            data.len(),
            num_messages
        );
        for id in 0..num_messages {
            let start = id as usize * MAX_MESSAGE_BODY_SIZE;
            let end = (start + MAX_MESSAGE_BODY_SIZE).min(data.len());
            let body_bytes = &data[start..end];
            let header = MessageHeader {
                timestamp: Utc::now().timestamp_millis(),
                packet_id: self.next_packet_id,
                message_id: id as _,
                last_message_in_packet: (num_messages - 1).try_into().unwrap(),
            };
            let bytes = &mut self.message_buf;
            bytes.clear();
            bytes.extend(wincode::serialize(&header).unwrap());
            bytes.extend(body_bytes);
            trace!(
                "Sending {} byte message {} for packet {}",
                bytes.len(),
                id,
                self.next_packet_id,
            );
            self.socket.send(bytes).unwrap();
            std::thread::sleep(Duration::from_micros(200));
        }
        self.next_packet_id = self.next_packet_id.wrapping_add(1);
        Ok(())
    }

    /// Returns `Ok(None)` if no (complete) packet has been read.
    pub fn recv_non_blocking(&mut self) -> anyhow::Result<Option<Packet>> {
        let message_bytes = match self.socket.recv(&mut self.message_buf) {
            Ok(num_read) => &self.message_buf[0..num_read],
            Err(ref err) if would_block(err) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let header: MessageHeader =
            wincode::deserialize(&message_bytes[0..MESSAGE_HEADER_SIZE]).unwrap();
        if is_out_of_order(header.packet_id, self.last_received_packet_id) {
            trace!(
                "Dropped out-of-order message {} for packet {}",
                header.message_id, header.packet_id
            );
            return Ok(None);
        }
        let now = Utc::now().timestamp_millis();
        let latency = now - header.timestamp;
        if latency > MAX_LATENCY_NS {
            trace!(
                "Dropped message {} for packet {} with {latency} ms latency",
                header.message_id, header.packet_id
            );
            return Ok(None);
        }
        // TODO: fixed-size packet_map to prevent memory leaks
        let info = self.packet_map.entry(header.packet_id).or_insert_with(|| {
            let num_messages_in_packet = header.last_message_in_packet as usize + 1;
            PacketInfo {
                bytes: vec![0u8; num_messages_in_packet * MAX_MESSAGE_BODY_SIZE],
                found: vec![false; num_messages_in_packet],
                num_found: 0,
            }
        });
        let message_id = header.message_id as usize;
        if !info.found[message_id] {
            info.found[message_id] = true;
            info.num_found += 1;
            trace!(
                "Received new message {} for packet {} ({}/{}, {}ms latency)",
                message_id,
                header.packet_id,
                info.num_found,
                header.last_message_in_packet + 1,
                (Utc::now().timestamp_millis() - header.timestamp)
            );
            let last_message_in_packet = header.last_message_in_packet as usize;
            let message_body_size = message_bytes.len() - MESSAGE_HEADER_SIZE as usize;
            if message_id == last_message_in_packet {
                // truncate the vector to the true size of the packet
                info.bytes
                    .truncate(last_message_in_packet * MAX_MESSAGE_BODY_SIZE + message_body_size);
            }
            let start = message_id * MAX_MESSAGE_BODY_SIZE;
            let end = start + message_body_size;
            info.bytes[start..end].copy_from_slice(&message_bytes[MESSAGE_HEADER_SIZE..]);

            if info.num_found >= info.found.len() {
                let packet_bytes = self.packet_map.remove(&header.packet_id).unwrap().bytes;
                debug!(
                    "Received packet {} with {} body bytes ({} messages)",
                    header.packet_id,
                    packet_bytes.len(),
                    last_message_in_packet + 1
                );
                let packet = wincode::deserialize(&packet_bytes)?;
                log_skipped_packets(
                    header.packet_id,
                    self.last_received_packet_id,
                    &self.packet_map,
                );
                self.last_received_packet_id = header.packet_id;
                return Ok(Some(packet));
            }
        }
        Ok(None)
    }
}

fn is_out_of_order(packet_id: u32, last_received_packet_id: u32) -> bool {
    // essentially `packet_id < last_received_packet_id`,
    // but taking into account the fact that packet_id can wrap around
    packet_id.wrapping_sub(last_received_packet_id) > (u32::MAX / 2)
}

fn log_skipped_packets(
    packet_id: u32,
    last_received_packet_id: u32,
    infos: &FxHashMap<u32, PacketInfo>,
) {
    let num_missed = packet_id
        .wrapping_sub(last_received_packet_id)
        .saturating_sub(1);
    if num_missed == 0 {
        return;
    }
    for i in 1..=num_missed {
        let id = last_received_packet_id.wrapping_add(i);
        match infos.get(&id) {
            Some(info) => debug!(
                "Packet {id} skipped ({}/{})",
                info.num_found,
                info.found.len()
            ),
            None => debug!("Packet {id} skipped (0/unknown)"),
        }
    }
    if num_missed > 5 {
        debug!(
            "Packets {} to {} skipped",
            last_received_packet_id.wrapping_add(1),
            packet_id.wrapping_sub(1)
        );
    }
}

fn blocking<T>(
    sync: &mut PacketStreamSync,
    mut scope: impl FnMut(&mut PacketStreamSync) -> T,
) -> T {
    sync.socket.set_nonblocking(false).unwrap();
    let result = scope(sync);
    sync.socket.set_nonblocking(true).unwrap();
    result
}

#[derive(SchemaWrite, SchemaRead)]
pub enum Packet {
    /// Keyboard input
    Input(Key),
    /// YUV video frame
    Yuv {
        timestamp: i64,
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
