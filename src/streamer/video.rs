use anyhow::Context;
use fps_ticker::Fps;
use log::{debug, error, info};
use netnet::UnreliableSender;
use std::sync::{Arc, mpsc};
use std::time::Instant;
use tokio::task::JoinHandle;

use crate::common::{H264, TimeStamp, since};

// TODO: UI element for adjusting these parameters
// TODO: resolution downscaling and frame rate reduction according to the client's monitor
pub(crate) const FRAME_RATE: u32 = 60;

pub(crate) fn send_nal_units(
    connection: &mut UnreliableSender,
    mut bytes: &[u8],
    mut is_keyframe: bool,
    width: u32,
    height: u32,
    timestamp: TimeStamp,
) -> anyhow::Result<()> {
    // max size - (sizeof(width) + sizeof(height) + sizeof(slice))
    let fragment_size = connection.max_fragment_size() - 20;
    let mut send = |unit_bytes: &[u8]| {
        let nal_unit = wincode::serialize(&H264 {
            width,
            height,
            bytes: unit_bytes,
            is_keyframe_start: is_keyframe,
            timestamp: timestamp.raw(),
        })
        .unwrap();
        is_keyframe = false;
        connection.send(&nal_unit)
    };
    // TODO: also allow [0, 0, 1] as NAL unit start indicator
    let mut i = 4;
    while bytes.len() > fragment_size && (i + 4) <= bytes.len() {
        if &bytes[i..i + 4] == &[0, 0, 0, 1] {
            // NAL unit start found
            send(&bytes[..i])?;
            bytes = &bytes[i..];
            i = 4;
            continue;
        }
        i += 1;
    }
    if bytes.len() > 0 {
        send(bytes)?;
    }
    Ok(())
}

pub(crate) struct ScreenCapture {
    pub(crate) video: mpsc::Receiver<(janck::Frame, TimeStamp)>,
    pub(crate) info: janck::FrameInfo,
}

pub(crate) fn capture_screen() -> anyhow::Result<ScreenCapture> {
    let (frame_sender, frame_receiver) = mpsc::sync_channel(0);
    let mut video = janck::capture_video(FRAME_RATE as _)?;
    let first_frame = video
        .next()
        .context("Failed to capture first frame of video")?;

    std::thread::spawn(move || {
        // Using a separate thread allows a frame to be captured while another one is being processed
        for frame in video {
            frame_sender.send((frame, TimeStamp::now())).unwrap();
        }
    });
    Ok(ScreenCapture {
        info: first_frame.info,
        video: frame_receiver,
    })
}

pub(crate) fn start_stream(
    device: Arc<avec::Device>,
    mut sender: UnreliableSender,
    screen_capture: ScreenCapture,
) -> JoinHandle<anyhow::Result<()>> {
    let handle = tokio::task::spawn_blocking(move || {
        let janck::FrameInfo {
            width,
            height,
            stride,
            format,
        } = screen_capture.info;
        let format = match format {
            janck::Format::Bgra8 => avec::Format::Bgra8,
            janck::Format::Rgba8 => avec::Format::Rgba8,
            _ => unimplemented!(),
        };
        let mut encoder =
            avec::Encoder::new(&device, width, height, stride, format, FRAME_RATE).unwrap();
        let mut fps = Fps::default();

        // TODO: if janck can capture directly into [wgpu::Texture]s then the entire GPU upload step of encoding can be skipped
        for (janck::Frame { bytes, info, .. }, timestamp) in screen_capture.video {
            assert_eq!(info, screen_capture.info); // TODO: handle screen resizing and such
            // Encode frame to H.264
            let pre_encode = Instant::now();
            let encoded = encoder.encode(&bytes).unwrap();
            debug!("Encoding frame took {:.2}ms", since(pre_encode));
            fps.tick();
            debug!(
                "Sending {} byte frame ({width}x{height}, {:.2}ms latency, {:.2} fps)",
                encoded.data.len(),
                timestamp.since(),
                fps.avg()
            );
            send_nal_units(
                &mut sender,
                &encoded.data,
                encoded.is_keyframe,
                width,
                height,
                timestamp,
            )
            .unwrap();
        }
        error!("Screen capture video ended");
        Ok(())
    });
    info!("Started screen cast");
    handle
}
