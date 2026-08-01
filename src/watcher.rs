use log::{debug, error, info, trace, warn};
use netnet::{Connection, UnreliableReceiver};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Instant,
};
use winit::window::Window;

use crate::{
    Role,
    common::{H264, TimeStamp, since},
    net::Streams,
};
use event_loop::run_event_loop;

mod event_loop;

// This is deliberately not an `async` function, despite returning a [Future],
// because `async` functions only start processing when `await`ed,
// which is undesirable for this function as it spawns background tasks and then returns.
pub fn start_video_processor(
    device: Arc<avec::Device>,
    mut receiver: UnreliableReceiver,
    window: Arc<OnceLock<Weak<Window>>>,
    out_video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
) -> impl Future<Output = anyhow::Result<()>> {
    info!("Started packet processing loop");
    let frame_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(100)));
    let frame_buffer2 = frame_buffer.clone();

    let network_handle = tokio::task::spawn(async move {
        loop {
            debug!("Waiting for frame from network");
            let packet_bytes = receiver.recv().await.unwrap();
            let H264 {
                width,
                height,
                bytes,
                is_keyframe_start,
                // NOTE: timestamp is not accurate on remote devices as the internal clocks are not synchronized
                timestamp,
            } = wincode::deserialize(&packet_bytes).unwrap();
            let instant = Instant::now();
            let mut guard = frame_buffer.lock().unwrap();
            if is_keyframe_start {
                // Once a keyframe has arrived, processing older frames only adds unnecessary latency
                guard.clear();
            }
            guard.push_back((width, height, bytes.to_vec(), timestamp, instant));
        }
    });
    let mut decoder = None;
    let processor_handle = tokio::task::spawn_blocking(move || {
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
                let decoder = avec::Decoder::new(device.clone(), width, height).unwrap();
                out_video_texture_view
                    .set(decoder.output_texture_view().clone())
                    .unwrap();
                decoder
            });
            let pre_decode = Instant::now();
            if let Err(err) = decoder.decode(&bytes) {
                if matches!(err, avec::DecoderError::NoNewFrame) {
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
            match window.get() {
                Some(weak) if let Some(window) = weak.upgrade() => window.request_redraw(),
                Some(_) => {
                    warn!(
                        "Cannot request redraw as the window has been destroyed; breaking rendering loop"
                    );
                    break;
                }
                None => warn!("Cannot request redraw as the window is not yet created"),
            }
            last_frame_instant = Instant::now();
        }
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

pub fn start_audio_processor(mut receiver: UnreliableReceiver) {
    todo!()
}

/// This function *must* be called from the main thread
pub async fn start(
    instance: Arc<avec::Instance>,
    device: Arc<avec::Device>,
    conn: Connection,
    mut streams: Streams,
) -> anyhow::Result<()> {
    let out_window = Arc::new(OnceLock::new());
    let video_texture_view = Arc::new(OnceLock::new());

    let inner_conn = conn.inner().clone();
    let conn_closed_handle = tokio::task::spawn(async move {
        error!("Connection closed: {}", inner_conn.closed().await);
    });

    let _video_result_future = start_video_processor(
        device.clone(),
        streams.video.receiver,
        out_window.clone(),
        video_texture_view.clone(),
    );
    let _audio_result_future = start_audio_processor(streams.audio.receiver);
    // This is a blocking call because the event loop *must* run on the main thread
    run_event_loop(
        instance,
        device,
        out_window,
        video_texture_view,
        Role::Watcher,
        move |input| {
            let bytes = wincode::serialize(&input).unwrap();
            streams.input.sender.send(&bytes).unwrap();
        },
    );
    conn_closed_handle.abort();
    Ok(())
}
