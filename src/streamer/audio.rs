use log::{debug, error, info};
use netnet::UnreliableSender;
use std::time::Instant;

use crate::common::{Opus, TimeStamp, since};

fn send_encoded(
    sender: &mut UnreliableSender,
    chunk_id: &mut u64,
    encoded: &[u8],
    sample_rate: u32,
    num_channels: u32,
) {
    let bytes = wincode::serialize(&Opus {
        chunk_id: *chunk_id,
        sample_rate,
        is_stereo: num_channels == 2,
        bytes: encoded,
        timestamp: TimeStamp::now().raw(),
    })
    .unwrap();
    *chunk_id += 1;
    sender.send(&bytes).unwrap();
}

fn encode<T: Clone>(
    buffer: &mut Vec<T>,
    encode_buffer: &mut [u8],
    mut input: &[T],
    chunk_len: usize,
    mut encode: impl FnMut(&[T], &mut [u8]) -> usize,
    mut send: impl FnMut(&[u8]),
) {
    debug_assert!(buffer.len() < chunk_len);
    while let Some((pre_split, post_split)) = input.split_at_checked(chunk_len - buffer.len()) {
        let pre_encode = Instant::now();
        input = post_split;
        buffer.extend_from_slice(pre_split);
        debug_assert_eq!(buffer.len(), chunk_len);
        let encoded_size = encode(&buffer, encode_buffer);
        let encoded = &encode_buffer[..encoded_size];
        send(encoded);
        debug!(
            "Encoding and sending {} sample -> {} byte audio chunk took {:.2}ms",
            buffer.len(),
            encoded.len(),
            since(pre_encode)
        );
        buffer.clear();
    }
    buffer.extend_from_slice(input);
}

pub(crate) fn start_stream(mut sender: UnreliableSender) -> anyhow::Result<()> {
    info!("Starting audio stream");

    let mut chunk_id = 0;
    let mut encoder = None;
    let mut soft_clip = None;
    let mut buffer_floats = Vec::new();
    let mut buffer_ints = Vec::new();
    let mut encode_buffer = vec![0u8; 4000];

    let result = adieu::capture_audio(Some("remin-audio-capture"), move |chunk, info| {
        let adieu::ChunkInfo {
            channels: num_channels,
            sample_rate,
        } = info;
        // TODO: support other sample rates?
        assert_eq!(sample_rate, 48_000);
        let channels = match num_channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => panic!(
                "Only mono and stereo audio input is supported, but the application has {num_channels} channels"
            ),
        };
        let encoder = encoder.get_or_insert_with(|| {
            info!("Creating audio encoder");
            let mut encoder =
                opus::Encoder::new(sample_rate, channels, opus::Application::LowDelay).unwrap();
            let _ = encoder.set_bitrate(opus::Bitrate::Max);
            info!("Created audio encoder");
            encoder
        });
        // 120 samples is a 2.5ms frame at 48000 hz, which is the smallest Opus frame size.
        let chunk_len = 120 * num_channels as usize;

        // Encode chunk to Opus
        match chunk {
            adieu::Chunk::F32(floats) => {
                let soft_clip = soft_clip.get_or_insert_with(|| opus::SoftClip::new(channels));
                soft_clip.apply(floats);
                encode(
                    &mut buffer_floats,
                    &mut encode_buffer,
                    floats,
                    chunk_len,
                    |buffer, encode_buffer| encoder.encode_float(buffer, encode_buffer).unwrap(),
                    |bytes| {
                        send_encoded(&mut sender, &mut chunk_id, bytes, sample_rate, num_channels)
                    },
                );
            }
            adieu::Chunk::I16(ints) => {
                encode(
                    &mut buffer_ints,
                    &mut encode_buffer,
                    ints,
                    chunk_len,
                    |buffer, encode_buffer| encoder.encode(buffer, encode_buffer).unwrap(),
                    |bytes| {
                        send_encoded(&mut sender, &mut chunk_id, bytes, sample_rate, num_channels)
                    },
                );
            }
        }
    });
    if let Err(err) = result {
        error!("Failed to capture audio: {err}");
        return Err(err.into());
    }
    info!("Started audio stream");
    Ok(())
}
