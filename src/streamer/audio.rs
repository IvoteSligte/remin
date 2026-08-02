use anyhow::{Context, bail};
use log::{debug, error, info};
use netnet::UnreliableSender;
use std::{io::Write, sync::mpsc, time::Instant};
use tokio::task::JoinHandle;

use crate::common::{AUDIO_SAMPLES_PER_CHUNK, Opus, TimeStamp, since};

pub(crate) struct AudioCapture {
    pub(crate) audio: mpsc::Receiver<(adieu::Chunk, TimeStamp)>,
    pub(crate) info: adieu::ChunkInfo,
}

pub(crate) fn capture_audio() -> anyhow::Result<AudioCapture> {
    let mut iter = adieu::capture_audio()?;
    info!("Audio stream acquired");
    // FIXME: this blocks when there is no audio
    let first_chunk = iter
        .next()
        .context("Failed to capture first chunk of audio");
    let info = *first_chunk?.info();
    // // TODO: support mono audio
    // assert_eq!(info.channels, 2);
    // let channels = opus::Channels::Stereo;
    let (chunk_sender, chunk_receiver) = mpsc::sync_channel(0);

    std::thread::spawn(move || {
        for chunk in iter {
            chunk_sender.send((chunk, TimeStamp::now())).unwrap();
        }
    });
    Ok(AudioCapture {
        audio: chunk_receiver,
        info,
    })
}

pub(crate) fn start_stream(
    mut sender: UnreliableSender,
    audio_capture: AudioCapture,
) -> JoinHandle<anyhow::Result<()>> {
    info!("Starting audio stream");
    let handle = tokio::task::spawn_blocking(move || {
        let adieu::ChunkInfo {
            channels: num_channels,
            format,
            sample_rate,
        } = audio_capture.info;
        // TODO: also support other formats
        assert_eq!(format, adieu::Format::F32);
        let channels = match num_channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => bail!(
                "Only mono and stereo audio input is supported, but the application has {num_channels} channels"
            ),
        };
        // NOTE: should Application::LowDelay or Application::Audio be used instead?
        //       needs to be tested to see what the Opus encoding latency is normally and how it compares to the video encoding latency
        // NOTE: whether forward error correction (FEC) should be used should also be tested
        let mut encoder = opus::Encoder::new(sample_rate, channels, opus::Application::Voip)?;
        // TODO: consider using a smaller buffer and sending more small packets
        let mut buffer = vec![0u8; 1_000_000];
        let mut chunk_id = 0;

        for (chunk, timestamp) in audio_capture.audio {
            // Encode chunk to Opus
            let pre_encode = Instant::now();

            // let encoded_size = match encoder.encode_float(&chunk, &mut buffer) {
            //     Ok(ok) => ok,
            //     Err(err) => panic!("Failed to encode chunk using Opus: {err}"),
            // };
            // let encoded = &buffer[..encoded_size];
            let i16_encoded = &chunk
                .samples_f32()
                .iter()
                .flat_map(|f| {
                    let n = (f * i16::MAX as f32) as i16;
                    n.to_le_bytes()
                })
                .collect::<Vec<u8>>();

            debug!("Encoding audio chunk took {:.2}ms", since(pre_encode));
            debug!(
                "Sending {} byte audio chunk ({:.2}ms latency)",
                i16_encoded.len(),
                timestamp.since()
            );
            let bytes = wincode::serialize(&Opus {
                chunk_id,
                sample_rate: chunk.info().sample_rate,
                is_stereo: channels == opus::Channels::Stereo,
                bytes: i16_encoded,
                timestamp: timestamp.raw(),
            })
            .unwrap();
            chunk_id += 1;
            sender.send(&bytes)?;
        }
        error!("Audio capture stream ended");
        Ok(())
    });
    info!("Started audio stream");
    handle
}
