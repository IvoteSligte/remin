use anyhow::anyhow;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{debug, info, warn};
use netnet::UnreliableReceiver;

use crate::common::{Opus, TimeStamp};

struct AudioPlayback {
    writer: rtrb::Producer<i16>,
    error_receiver: std::sync::mpsc::Receiver<cpal::Error>,
    _stream: cpal::Stream,
}

impl AudioPlayback {
    pub fn new(channels: u32, sample_rate: u32) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        // TODO: support running remin on PCs without an audio output device
        let device = host
            .default_output_device()
            .expect("No audio output device found");
        for config in device.supported_output_configs()? {
            if config.channels() as u32 != channels
                || !config.contains_rate(sample_rate)
                || config.sample_format() != cpal::SampleFormat::I16
            {
                continue;
            }
            let (error_sender, error_receiver) = std::sync::mpsc::sync_channel::<cpal::Error>(0);
            let (writer, mut reader) = rtrb::RingBuffer::new(20_000);
            let stream = device.build_output_stream(
                config.with_sample_rate(sample_rate).into(),
                move |output, _info| {
                    let (_, remainder) = reader.pop_partial_slice(output);
                    if !remainder.is_empty() {
                        info!("Not enough audio data to fill the target buffer");
                    }
                },
                move |err| error_sender.send(err).unwrap(),
                None,
            )?;
            stream.play()?;
            return Ok(Self {
                writer,
                error_receiver,
                _stream: stream,
            });
        }
        Err(anyhow!(
            "No audio config found that matches the desired channel count and sample rate"
        ))
    }

    pub fn get_error(&self) -> anyhow::Result<()> {
        match self.error_receiver.try_recv() {
            Ok(err) => Err(err.into()),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(anyhow!("Cpal thread panicked"))
            }
        }
    }

    pub fn write_chunk(&mut self, chunk: &[i16]) -> anyhow::Result<()> {
        self.get_error()?;
        let (written, remainder) = self.writer.push_partial_slice(chunk);
        if !remainder.is_empty() {
            warn!(
                "Ring buffer full: wrote {}/{} samples",
                written.len(),
                chunk.len()
            );
        }
        Ok(())
    }
}

pub fn start_processor(
    mut receiver: UnreliableReceiver,
) -> impl Future<Output = anyhow::Result<()>> {
    let network_handle = tokio::task::spawn(async move {
        let mut last_chunk_id = 0;
        let mut buffer = Vec::with_capacity(4000);
        let mut playback = None;
        loop {
            debug!("Waiting for audio chunk from network");
            let packet_bytes = receiver.recv().await.unwrap();
            let Opus {
                chunk_id,
                sample_rate,
                is_stereo,
                bytes,
                // NOTE: timestamp is not accurate on remote devices as the internal clocks are not synchronized
                timestamp,
            } = wincode::deserialize(packet_bytes).unwrap();
            if last_chunk_id + 1 < chunk_id {
                debug!(
                    "Lost audio packets {} to {}",
                    last_chunk_id + 1,
                    chunk_id - 1
                );
            }
            let _timestamp = TimeStamp::from_raw(timestamp);
            let num_channels = match is_stereo {
                false => 1,
                true => 2,
            };
            let playback = playback
                .get_or_insert_with(|| AudioPlayback::new(num_channels, sample_rate).unwrap());

            buffer.clear();
            for i in (0..bytes.len()).step_by(2) {
                buffer.push(i16::from_le_bytes([bytes[i], bytes[i + 1]]));
            }
            let decoded = &buffer[..];
            playback.write_chunk(decoded).unwrap();
            last_chunk_id = chunk_id;
        }
    });
    async move { network_handle.await? }
}
