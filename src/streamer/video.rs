use fps_ticker::Fps;
use log::{debug, error, info};
use netnet::UnreliableSender;
use tokio::task::JoinHandle;

use crate::{common::H264, nal::nal_units};

/// Number of bits in a megabyte
const MB_BITS: u32 = 8 << 20;

// TODO: UI element for adjusting these parameters
// TODO: automatic resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;
pub(crate) const DIMENSIONS: (u32, u32) = (1920, 1080);
pub(crate) const BITRATE: u32 = MB_BITS / 2;

pub(crate) fn send_nal_units(
    connection: &mut UnreliableSender,
    bytes: &[u8],
    mut is_keyframe: bool,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    // max size - (sizeof(width) + sizeof(height) + sizeof(slice))
    let fragment_size = connection.max_fragment_size() - 20;
    let mut send = |unit_bytes: &[u8]| {
        let nal_unit = wincode::serialize(&H264 {
            width,
            height,
            bytes: unit_bytes,
            is_keyframe_start: is_keyframe,
        })
        .unwrap();
        is_keyframe = false;
        connection.send(&nal_unit)
    };
    for nal_unit in nal_units(bytes) {
        for bytes in nal_unit.chunks(fragment_size) {
            send(bytes)?;
        }
    }
    Ok(())
}

pub(crate) fn start_stream(mut sender: UnreliableSender) -> JoinHandle<anyhow::Result<()>> {
    let handle = tokio::task::spawn_blocking(move || {
        let video = capenc::capture(DIMENSIONS, FRAME_RATE, BITRATE).unwrap();
        let mut fps = Fps::default();

        // TODO: handle screen resizing and such
        for frame_result in video {
            let capenc::Frame {
                bytes,
                width,
                height,
                is_keyframe,
            } = frame_result.unwrap();
            fps.tick();
            debug!(
                "Sending {} byte frame ({width}x{height}, {:.2} fps)",
                bytes.len(),
                fps.avg()
            );
            send_nal_units(&mut sender, &bytes, is_keyframe, DIMENSIONS.0, DIMENSIONS.1).unwrap();
        }
        error!("Screen capture video ended");
        Ok(())
    });
    info!("Started screen cast");
    handle
}
