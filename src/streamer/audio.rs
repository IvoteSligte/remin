use log::{debug, info};
use netnet::UnreliableSender;
use std::time::Instant;

use crate::common::{Opus, TimeStamp, since};

pub(crate) fn start_stream(mut sender: UnreliableSender) -> anyhow::Result<()> {
    info!("Starting audio stream");

    // // NOTE: should Application::LowDelay or Application::Audio be used instead?
    // //       needs to be tested to see what the Opus encoding latency is normally and how it compares to the video encoding latency
    // // NOTE: whether forward error correction (FEC) should be used should also be tested
    // let mut encoder = opus::Encoder::new(sample_rate, channels, opus::Application::Voip).unwrap();
    // // TODO: consider using a smaller buffer and sending more small packets
    // let mut buffer = vec![0u8; 1_000_000];
    let mut chunk_id = 0;
    let mut soft_clip = None;
    let mut buffer = Vec::with_capacity(4000);

    adieu::capture_audio(Some("remin-audio-capture"), move |chunk, info| {
        let adieu::ChunkInfo {
            channels: num_channels,
            sample_rate,
        } = info;
        let channels = match num_channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => panic!(
                "Only mono and stereo audio input is supported, but the application has {num_channels} channels"
            ),
        };
        let soft_clip = soft_clip.get_or_insert_with(|| opus::SoftClip::new(channels));

        // Encode chunk to Opus
        let pre_encode = Instant::now();

        // let encoded_size = match encoder.encode_float(&chunk, &mut buffer) {
        //     Ok(ok) => ok,
        //     Err(err) => panic!("Failed to encode chunk using Opus: {err}"),
        // };
        // let encoded = &buffer[..encoded_size];

        buffer.clear();
        match chunk {
            adieu::Chunk::F32(floats) => {
                soft_clip.apply(floats);
                buffer.extend(floats.iter().flat_map(|f| {
                    let n = (f * i16::MAX as f32).round() as i16;
                    n.to_le_bytes()
                }));
            }
            adieu::Chunk::I16(ints) => buffer.extend(ints.iter().flat_map(|n| n.to_le_bytes())),
        };
        debug!(
            "Encoding {} byte audio chunk took {:.2}ms",
            buffer.len(),
            since(pre_encode)
        );
        let bytes = wincode::serialize(&Opus {
            chunk_id,
            sample_rate,
            is_stereo: channels == opus::Channels::Stereo,
            bytes: &buffer,
            timestamp: TimeStamp::now().raw(),
        })
        .unwrap();
        chunk_id += 1;
        sender.send(&bytes).unwrap();
    })?;
    info!("Started audio stream");
    Ok(())
}
