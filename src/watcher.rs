use gpu_video::{VulkanDevice, VulkanInstance};
use log::{debug, error, info, trace, warn};
use netnet::{Connection, UnreliableReceiver};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use winit::window::Window;

use crate::{
    Role,
    common::{Packet, TimeStamp, since},
    gpu, run_event_loop,
};

pub fn start_video_processor(
    device: Arc<VulkanDevice>,
    mut conn: UnreliableReceiver,
    window: Arc<OnceLock<Arc<Window>>>,
    out_video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
) -> anyhow::Result<()> {
    info!("Started packet processing loop");
    let frame_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(100)));
    let frame_buffer2 = frame_buffer.clone();

    tokio::task::spawn(async move {
        loop {
            debug!("Waiting for frame from network");
            let packet_bytes = conn.recv().await.unwrap();
            let packet: Packet = wincode::deserialize(&packet_bytes).unwrap();
            match packet {
                Packet::Input { .. } => warn!("Watcher received an input packet"),
                Packet::H264 {
                    width,
                    height,
                    bytes,
                    is_keyframe_start,
                    // NOTE: timestamp is not accurate on remote devices as the internal clocks are not synchronized
                    timestamp,
                } => {
                    let instant = Instant::now();
                    let mut guard = frame_buffer.lock().unwrap();
                    if is_keyframe_start {
                        // Once a keyframe has arrived, processing older frames only adds unnecessary latency
                        guard.clear();
                    }
                    guard.push_back((width, height, bytes.to_vec(), timestamp, instant));
                }
            };
        }
    });
    let mut decoder = None;
    std::thread::spawn(move || {
        let mut frames_per_second = fps_ticker::Fps::default();
        let mut last_frame_instant = Instant::now();

        loop {
            let (width, height, bytes, timestamp, instant) =
                match frame_buffer2.lock().map(|mut guard| guard.pop_front()) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(err) => {
                        error!("Network frame receiver thread panicked: {err}");
                        break;
                    }
                };
            trace!("Channel latency: {}ms", since(instant));
            let timestamp = TimeStamp::from_raw(timestamp);
            debug!("Received frame ({:.2}ms latency)", timestamp.since());
            let decoder = decoder.get_or_insert_with(|| {
                let decoder =
                    gpu::Decoder::new(device.clone(), device.wgpu_queue(), width, height).unwrap();
                out_video_texture_view
                    .set(decoder.output_texture_view().clone())
                    .unwrap();
                decoder
            });
            let pre_decode = Instant::now();
            if let Err(err) = decoder.decode(&bytes) {
                if matches!(err, gpu::DecoderError::NoNewFrame) {
                    debug!("Not enough frame data to construct a new frame");
                    continue;
                }
                warn!("Failed to decode frame: {err}");
                continue;
            }
            frames_per_second.tick();
            debug!(
                "Decoding frame took {:.2}ms ({:.2}ms latency, {:.2}/s, {:.2}ms since last)",
                since(pre_decode),
                timestamp.since(),
                frames_per_second.avg(),
                since(last_frame_instant)
            );
            window.wait().request_redraw();
            last_frame_instant = Instant::now();
        }
        info!("Thread receiver terminated");
    });
    Ok(())
}

pub fn start(
    instance: Arc<VulkanInstance>,
    device: Arc<VulkanDevice>,
    mut conn: Connection,
) -> anyhow::Result<()> {
    let window = Arc::new(OnceLock::new());
    let video_texture_view = Arc::new(OnceLock::new());

    let inner_conn = conn.inner().clone();
    tokio::task::spawn(async move {
        error!("Connection closed: {}", inner_conn.closed().await);
    });

    start_video_processor(
        device.clone(),
        conn.unreliable_receiver,
        window.clone(),
        video_texture_view.clone(),
    )?;
    run_event_loop(
        instance,
        device,
        window,
        video_texture_view,
        Role::Watcher,
        move |input| {
            let packet = Packet::Input(input.clone());
            let bytes = wincode::serialize(&packet).unwrap();
            conn.unreliable_sender.send(&bytes).unwrap();
        },
    );
    Ok(())
}
