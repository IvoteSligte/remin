use anyhow::{anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{debug, error, info, warn};
use netnet::UnreliableReceiver;

use std::time::Instant;

use crate::common::{AUDIO_SAMPLES_PER_CHUNK, Opus};

#[derive(Debug, Clone, Copy)]
enum AudioFormat {
    F32,
    I16,
}

fn write_chunk<T: Copy>(producer: &mut rtrb::Producer<T>, chunk: &[T]) {
    let (written, remainder) = producer.push_partial_slice(chunk);
    if !remainder.is_empty() {
        warn!(
            "Ring buffer full: wrote {}/{} samples",
            written.len(),
            chunk.len()
        );
    }
}

enum AudioWriter {
    F32(rtrb::Producer<f32>),
    I16(rtrb::Producer<i16>),
}

struct AudioPlayback {
    format: AudioFormat,
    writer: AudioWriter,
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
            if config.channels() as u32 != channels || !config.contains_rate(sample_rate) {
                continue;
            }
            let (error_sender, error_receiver) = std::sync::mpsc::sync_channel::<cpal::Error>(0);

            macro_rules! build_output_stream {
                ($format:ident) => {{
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
                    (AudioFormat::$format, AudioWriter::$format(writer), stream)
                }};
            }
            let (format, writer, stream) = match config.sample_format() {
                cpal::SampleFormat::F32 => build_output_stream!(F32),
                cpal::SampleFormat::I16 => build_output_stream!(I16),
                _ => continue,
            };
            stream.play()?;
            return Ok(Self {
                format,
                writer,
                error_receiver,
                _stream: stream,
            });
        }
        Err(anyhow!(
            "No audio config found that matches the desired channel count and sample rate"
        ))
    }

    pub(crate) fn get_error(&self) -> anyhow::Result<()> {
        match self.error_receiver.try_recv() {
            Ok(err) => Err(err.into()),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(anyhow!("Cpal thread panicked"))
            }
        }
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn write_chunk_f32(&mut self, chunk: &[f32]) -> anyhow::Result<()> {
        self.get_error()?;
        match &mut self.writer {
            AudioWriter::F32(writer) => {
                write_chunk(writer, chunk);
                Ok(())
            }
            _ => bail!("Audio format mismatch"),
        }
    }

    pub fn write_chunk_i16(&mut self, chunk: &[i16]) -> anyhow::Result<()> {
        self.get_error()?;
        match &mut self.writer {
            AudioWriter::I16(writer) => {
                write_chunk(writer, chunk);
                Ok(())
            }
            _ => bail!("Audio format mismatch"),
        }
    }
}

pub fn start_processor(
    mut receiver: UnreliableReceiver,
) -> impl Future<Output = anyhow::Result<()>> {
    let (local_sender, mut local_receiver) = tokio::sync::mpsc::channel(100);
    let network_handle = tokio::task::spawn(async move {
        let mut last_chunk_id = 0;
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
            } = wincode::deserialize(&packet_bytes).unwrap();
            for _ in (last_chunk_id + 1)..chunk_id {
                info!("Packet lost");
                // Packet loss is indicated by an empty chunk
                let instant = Instant::now();
                local_sender
                    .send((sample_rate, is_stereo, Vec::new(), timestamp, instant))
                    .await
                    .unwrap();
            }
            let instant = Instant::now();
            local_sender
                .send((sample_rate, is_stereo, bytes.to_vec(), timestamp, instant))
                .await
                .unwrap();
            last_chunk_id = chunk_id;
        }
    });
    let mut state = None;
    let processor_handle = tokio::task::spawn_blocking(move || {
        let mut buffer = Vec::with_capacity(40_000);
        while let Some((sample_rate, is_stereo, bytes, _timestamp, _instant)) =
            local_receiver.blocking_recv()
        {
            let (channels, num_channels) = match is_stereo {
                false => (opus::Channels::Mono, 1),
                true => (opus::Channels::Stereo, 2),
            };
            let (decoder, playback) = state.get_or_insert_with(|| {
                let decoder = opus::Decoder::new(sample_rate, channels).unwrap();
                let playback = AudioPlayback::new(num_channels, sample_rate).unwrap();
                (decoder, playback)
            });
            let buffer_len = AUDIO_SAMPLES_PER_CHUNK * num_channels as usize;
            match playback.format() {
                AudioFormat::F32 => {
                    todo!()
                }
                AudioFormat::I16 => {
                    buffer.clear();
                    for i in (0..bytes.len()).step_by(2) {
                        buffer.push(i16::from_le_bytes([bytes[i], bytes[i + 1]]));
                    }
                    let decoded = &buffer[..];
                    // let decoded_len = decoder.decode(&bytes, &mut buffer, false).unwrap();
                    // let decoded = &buffer[..decoded_len];
                    playback.write_chunk_i16(decoded).unwrap();
                }
            };
        }
        error!("Local audio receiver exited");
        Ok(())
    });
    async move {
        // TODO: use JoinSet or tokio::task::scope instead, returning it from the function
        tokio::select! {
            join_result = network_handle => join_result?,
            join_result = processor_handle => join_result?,
        }
    }
}
