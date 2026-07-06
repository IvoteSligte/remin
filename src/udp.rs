use chrono::Utc;
use log::{debug, trace};
use rustc_hash::FxHashMap;
use std::{
    collections::VecDeque,
    io,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    common::{Key, RunningAverage},
    signal::Signal,
};

// Fixed constants
const MAX_MESSAGE_SIZE: usize = 65507;
const MESSAGE_HEADER_SIZE: usize = std::mem::size_of::<MessageHeader>(); // the encoded size of MessageHeader in bytes
const MAX_MESSAGE_BODY_SIZE: usize = MAX_MESSAGE_SIZE - MESSAGE_HEADER_SIZE;

// Can be adjusted
const MAX_LATENCY_MS: i64 = 100; // 100 milliseconds
const RECV_BUFFER_CAP: usize = 100; // max number of elements in the receive buffer

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
    // TODO: separate `send` thread
    pub fn new(port: u16, connect_to: SocketAddr, stop: Signal) -> io::Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{port}"))?;
        socket.connect(connect_to)?;
        let socket2 = socket.try_clone().unwrap();
        let recv_buf = VecDeque::with_capacity(RECV_BUFFER_CAP);
        let mut packet_map = FxHashMap::<_, PacketInfo>::default();
        packet_map.reserve(1000);
        let sync = Arc::new(Mutex::new(PacketStreamSync {
            socket,
            recv_rate: RunningAverage::new(10000.0),
            recv_buf,
            message_buf: vec![0u8; MAX_MESSAGE_SIZE],
            packet_map,
            next_packet_id: 0,
            last_received_packet_id: 0,
        }));
        let sync2 = sync.clone();

        std::thread::spawn(move || {
            let mut buf = vec![0u8; MAX_MESSAGE_SIZE];
            loop {
                socket2.recv(&mut buf).unwrap();
                let sync = &mut *sync2.lock().unwrap();
                let (header, body_bytes) = split_message(&buf);
                if sync.recv_buf.len() >= RECV_BUFFER_CAP {
                    // remove oldest message
                    sync.recv_buf.pop_front();
                    sync.recv_rate.update(0.0);
                    trace!(
                        "Dropped message {} of packet {} due to full buffer ({:.0}% survival rate)",
                        header.message_id,
                        header.packet_id,
                        sync.recv_rate.get() * 100.0
                    );
                }
                let index = match sync
                    .recv_buf
                    .binary_search_by_key(&header.timestamp, |(header, _)| header.timestamp)
                {
                    Ok(i) => i,
                    Err(i) => i,
                };
                sync.recv_buf.insert(index, (header, body_bytes.to_vec()));
            }
        });

        Ok(Self { stop, sync })
    }

    pub fn send(&self, packet: &Packet) -> io::Result<()> {
        assert!(!self.stop.signaled());
        let data = wincode::serialize(&packet).unwrap();
        self.sync.lock().unwrap().send(&data)
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
    /// Survival rate of messages received
    recv_rate: RunningAverage,
    /// Fixed-size buffer of received messages, sorted by timestamp
    /// (header, body_bytes)
    recv_buf: VecDeque<(MessageHeader, Vec<u8>)>,
    /// Fixed-size buffer used to store messages during encoding
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
        let Some((header, body_bytes)) = self.recv_buf.pop_back() else {
            return Ok(None);
        };
        if is_out_of_order(header.packet_id, self.last_received_packet_id) {
            trace!(
                "Dropped out-of-order message {} for packet {}",
                header.message_id, header.packet_id
            );
            self.recv_rate.update(0.0);
            return Ok(None);
        }
        let now = Utc::now().timestamp_millis();
        let latency = now - header.timestamp;
        if latency > MAX_LATENCY_MS {
            trace!(
                "Dropped message {} for packet {} with {latency} ms latency",
                header.message_id, header.packet_id
            );
            self.recv_rate.update(0.0);
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
        self.recv_rate.update(1.0);
        let message_id = header.message_id as usize;
        if !info.found[message_id] {
            info.found[message_id] = true;
            info.num_found += 1;
            trace!(
                "Received new message {} for packet {} ({}/{}, {}ms latency, {:.0}% survival rate)",
                message_id,
                header.packet_id,
                info.num_found,
                header.last_message_in_packet + 1,
                (Utc::now().timestamp_millis() - header.timestamp),
                self.recv_rate.get() * 100.0,
            );
            let last_message_in_packet = header.last_message_in_packet as usize;
            if message_id == last_message_in_packet {
                // truncate the vector to the true size of the packet
                info.bytes
                    .truncate(last_message_in_packet * MAX_MESSAGE_BODY_SIZE + body_bytes.len());
            }
            let start = message_id * MAX_MESSAGE_BODY_SIZE;
            let end = start + body_bytes.len();
            info.bytes[start..end].copy_from_slice(&body_bytes);

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

fn split_message(message: &[u8]) -> (MessageHeader, &[u8]) {
    let header: MessageHeader = wincode::deserialize(&message[0..MESSAGE_HEADER_SIZE]).unwrap();
    let body = &message[MESSAGE_HEADER_SIZE..];
    (header, body)
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
