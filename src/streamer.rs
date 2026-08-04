use log::{error, info};
use netnet::Connection;
use std::sync::Arc;
use std::time::Duration;

use crate::net::Streams;

mod audio;
mod input;
mod video;

pub async fn start(
    device: Arc<avec::Device>,
    conn: Connection,
    streams: Streams,
) -> anyhow::Result<()> {
    let inner_conn = conn.inner().clone();
    let inner_conn2 = conn.inner().clone();
    tokio::task::spawn(async move {
        error!("Connection closed: {}", inner_conn.closed().await);
    });

    info!("Starting screen capture");
    let screen_capture = video::capture_screen()?;
    let screen_info = screen_capture.info;
    info!("Starting video stream");
    let video_stream_handle = video::start_stream(device, streams.video.sender, screen_capture);

    info!("Starting audio stream");
    audio::start_stream(streams.audio.sender)?;

    info!("Starting input handler");
    let input_handle = input::start_processor(
        streams.input.receiver,
        screen_info.width,
        screen_info.height,
    )?;

    info!("Starting ping updater");
    // Deliberately not using tokio for this because the upgrade_in_event_loop
    // call seems to block tokio until it is finished.
    let ping_handle = tokio::task::spawn(async move {
        loop {
            let inner_conn = inner_conn2.clone();
            info!("Ping: {}ms", inner_conn.rtt().as_millis());
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    tokio::select! {
        join_result = video_stream_handle => join_result?,
        join_result = input_handle => join_result?,
        join_result = ping_handle => join_result?,
    }
}
