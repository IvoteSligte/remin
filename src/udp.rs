use chrono::{DateTime, Utc};
use log::{debug, trace};
use rustc_hash::FxHashMap;
use std::{
    collections::VecDeque,
    io,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    common::{Key, RunningAverage},
    signal::Signal,
};

// TODO: stop signal for sender/receiver threads and streams
// TODO: reliability mechanism
// TODO: encryption

// Fixed constants
const MAX_MESSAGE_SIZE: usize = 65507;
const MESSAGE_HEADER_SIZE: usize = 8 + 4 + 2 + 2; // the encoded size of MessageHeader in bytes
const MAX_MESSAGE_BODY_SIZE: usize = MAX_MESSAGE_SIZE - MESSAGE_HEADER_SIZE;

// Can be adjusted
const MAX_LATENCY_MS: i64 = 100;
const RECV_BUFFER_CAP: usize = 200; // max number of messages in the receive buffer

// NOTE: make sure there is no implicit padding to prevent encoding/decoding mismatches
#[derive(Debug, SchemaRead, SchemaWrite)]
struct MessageHeader {
    packet_timestamp: i64,
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
    sender: mpsc::Sender<Packet>,
    receiver: Arc<Mutex<Receiver>>,
}

impl PacketStream {
    // TODO: separate `send` thread
    pub fn new(port: u16, connect_to: SocketAddr, stop: Signal) -> io::Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{port}"))?;
        socket.connect(connect_to)?;
        Ok(Self {
            stop,
            sender: spawn_sender_thread(socket.try_clone()?),
            receiver: Receiver::new(socket),
        })
    }

    pub fn send(&self, packet: Packet) {
        assert!(!self.stop.signaled());
        self.sender.send(packet).unwrap();
    }

    /// Receives a packet, panicking if stop has been signaled.
    pub fn recv(&self) -> anyhow::Result<(Packet, DateTime<Utc>)> {
        loop {
            assert!(!self.stop.signaled());
            match self.receiver.lock().unwrap().recv_non_blocking() {
                Ok(Some(packet)) => return Ok(packet),
                Ok(None) => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

fn spawn_sender_thread(socket: UdpSocket) -> mpsc::Sender<Packet> {
    let (sender, receiver) = mpsc::channel();

    std::thread::spawn(move || {
        let mut message_buf = vec![0u8; MAX_MESSAGE_SIZE];
        let mut packet_id = 0u32;

        loop {
            let packet = receiver.recv().unwrap();
            let data = wincode::serialize(&packet).unwrap();
            let num_messages = data.len().div_ceil(MAX_MESSAGE_BODY_SIZE);
            debug!(
                "Sending packet {} with {} body bytes ({} messages)",
                packet_id,
                data.len(),
                num_messages
            );
            let packet_timestamp = Utc::now().timestamp_millis();
            for id in 0..num_messages {
                let start = id as usize * MAX_MESSAGE_BODY_SIZE;
                let end = (start + MAX_MESSAGE_BODY_SIZE).min(data.len());
                let body_bytes = &data[start..end];
                let header = MessageHeader {
                    packet_timestamp,
                    packet_id,
                    message_id: id as _,
                    last_message_in_packet: (num_messages - 1).try_into().unwrap(),
                };
                let bytes = &mut message_buf;
                bytes.clear();
                bytes.extend(wincode::serialize(&header).unwrap());
                bytes.extend(body_bytes);
                trace!(
                    "Sending {} byte message {} for packet {}",
                    bytes.len(),
                    id,
                    packet_id,
                );
                socket.send(bytes).unwrap();
                std::thread::sleep(Duration::from_micros(200));
            }
            debug!(
                "Sending packet {} took {}ms",
                packet_id,
                Utc::now().timestamp_millis() - packet_timestamp
            );
            packet_id = packet_id.wrapping_add(1);
        }
    });
    sender
}

fn spawn_receiver_thread(socket: UdpSocket, receiver: Arc<Mutex<Receiver>>) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; MAX_MESSAGE_SIZE];
        loop {
            socket.recv(&mut buf).unwrap();
            let receiver = &mut *receiver.lock().unwrap();
            let (header, body_bytes) = split_message(&buf);
            if receiver.queue.len() >= RECV_BUFFER_CAP {
                // remove oldest message
                receiver.queue.pop_front();
                receiver.survival_rate.update(0.0);
                trace!(
                    "Dropped message {} of packet {} due to full buffer ({:.0}% survival rate)",
                    header.message_id,
                    header.packet_id,
                    receiver.survival_rate.get() * 100.0
                );
            }
            let index = match receiver
                .queue
                .binary_search_by_key(&header.packet_timestamp, |(header, _)| {
                    header.packet_timestamp
                }) {
                Ok(i) => i,
                Err(i) => i,
            };
            receiver.queue.insert(index, (header, body_bytes.to_vec()));
        }
    });
}

struct Receiver {
    /// Survival rate of messages received (equal to 1.0 - drop_rate)
    survival_rate: RunningAverage,
    /// Fixed-size queue of received messages, sorted by timestamp
    /// (header, body_bytes)
    queue: VecDeque<(MessageHeader, Vec<u8>)>,
    /// Map from packet ID to packet information
    packet_map: FxHashMap<u32, PacketInfo>,
    /// ID of the last complete package received
    last_packet_id: u32,
    /// Timestamp of the last complete package received
    last_packet_timestamp: i64,
}

impl Receiver {
    pub fn new(socket: UdpSocket) -> Arc<Mutex<Self>> {
        let recv_buf = VecDeque::with_capacity(RECV_BUFFER_CAP);
        let mut packet_map = FxHashMap::<_, PacketInfo>::default();
        packet_map.reserve(1000);
        let receiver = Arc::new(Mutex::new(Receiver {
            survival_rate: RunningAverage::new(10000.0),
            queue: recv_buf,
            packet_map,
            last_packet_id: 0,
            last_packet_timestamp: 0,
        }));
        spawn_receiver_thread(socket, receiver.clone());
        receiver
    }

    /// Returns `Ok(None)` if no (complete) packet has been read.
    pub fn recv_non_blocking(&mut self) -> anyhow::Result<Option<(Packet, DateTime<Utc>)>> {
        let Some((header, body_bytes)) = self.queue.pop_back() else {
            return Ok(None);
        };
        if header.packet_timestamp < self.last_packet_timestamp {
            trace!(
                "Dropped out-of-order message {} for packet {}",
                header.message_id, header.packet_id
            );
            self.survival_rate.update(0.0);
            return Ok(None);
        }
        let now = Utc::now().timestamp_millis();
        let latency = now - header.packet_timestamp;
        if latency > MAX_LATENCY_MS {
            trace!(
                "Dropped message {} for packet {} with {latency} ms latency",
                header.message_id, header.packet_id
            );
            self.survival_rate.update(0.0);
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
        self.survival_rate.update(1.0);
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
                (Utc::now().timestamp_millis() - header.packet_timestamp),
                self.survival_rate.get() * 100.0,
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
                log_skipped_packets(header.packet_id, self.last_packet_id, &self.packet_map);
                self.last_packet_id = header.packet_id;
                self.last_packet_timestamp = header.packet_timestamp;
                return Ok(Some((
                    packet,
                    DateTime::from_timestamp_millis(header.packet_timestamp).unwrap(),
                )));
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

// Takes into account the fact that packet_id can wrap around from u32::MAX to 0
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
