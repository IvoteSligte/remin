use std::{sync::mpsc, time::Instant};

use anyhow::{Context, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{debug, error, warn};
use netnet::UnreliableReceiver;

use crate::common::{Opus, TimeStamp, since};

struct AudioPlayback {
    writer: rtrb::Producer<i16>,
    error_receiver: mpsc::Receiver<cpal::Error>,
    /// The stream can temporarily be [None] after being invalidated by `cpal`.
    stream: Option<cpal::Stream>,
    channels: u32,
    sample_rate: u32,
}

impl AudioPlayback {
    pub fn new(channels: u32, sample_rate: u32) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            warn!("No audio output device found");
            bail!("No audio output device found");
        };
        for config in device.supported_output_configs()? {
            if config.channels() as u32 != channels
                || !config.contains_rate(sample_rate)
                || config.sample_format() != cpal::SampleFormat::I16
            {
                continue;
            }
            let (error_sender, error_receiver) = mpsc::sync_channel::<cpal::Error>(0);
            let (writer, mut reader) = rtrb::RingBuffer::new(20_000);
            let stream = device.build_output_stream(
                config.with_sample_rate(sample_rate).into(),
                move |output, _info| {
                    let (filled, remainder) = reader.pop_partial_slice(output);
                    if !remainder.is_empty() {
                        debug!(
                            "Not enough audio data to fill the target buffer ({}/{} samples written)",
                            filled.len(),
                            output.len()
                        );
                    }
                },
                move |err| error_sender.send(err).unwrap(),
                None,
            )?;
            stream
                .play()
                .inspect_err(|err| error!("Failed to start playback stream: {err}"))?;
            return Ok(Self {
                writer,
                error_receiver,
                stream: Some(stream),
                channels,
                sample_rate,
            });
        }
        bail!("No audio config found that matches the desired channel count and sample rate")
    }

    pub fn get_error(&mut self) -> anyhow::Result<()> {
        if self.stream.is_none() {
            error!("Tried to write chunk to invalidated stream.");
            return Ok(());
        }
        match self.error_receiver.try_recv() {
            Ok(err) if err.kind() == cpal::ErrorKind::StreamInvalidated => {
                error!("Playback stream invalidated: {err}");
                drop(self.stream.take());
                *self = Self::new(self.channels, self.sample_rate)
                    .context("When trying to recreate an invalidated playback stream")?;
                Ok(())
            }
            Ok(err) => Err(err.into()),
            Err(mpsc::TryRecvError::Empty) => Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => bail!("Cpal thread panicked"),
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
        let mut decode_buffer = vec![<i16 as cpal::Sample>::EQUILIBRIUM; 4000];
        let mut state = None;
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
                warn!(
                    "Lost audio packets {} to {}",
                    last_chunk_id + 1,
                    chunk_id - 1
                );
            }
            let _timestamp = TimeStamp::from_raw(timestamp);
            let (channels, num_channels) = match is_stereo {
                false => (opus::Channels::Mono, 1),
                true => (opus::Channels::Stereo, 2),
            };
            let (decoder, playback) = state.get_or_insert_with(|| {
                (
                    opus::Decoder::new(sample_rate, channels).unwrap(),
                    AudioPlayback::new(num_channels, sample_rate).unwrap(),
                )
            });
            let pre_decode = Instant::now();
            let decode_len =
                decoder.decode(bytes, &mut decode_buffer, false).unwrap() * num_channels as usize;
            let decoded = &decode_buffer[..decode_len];
            playback.write_chunk(decoded).unwrap();
            last_chunk_id = chunk_id;
            debug!(
                "Decoding and writing {} byte -> {} sample audio chunk {} took {:.2}ms",
                bytes.len(),
                decoded.len(),
                chunk_id,
                since(pre_decode)
            );
        }
    });
    async move { network_handle.await? }
}
