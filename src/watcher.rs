use log::error;
use netnet::Connection;
use std::sync::{Arc, OnceLock};

use crate::{Role, net::Streams};
use event_loop::run_event_loop;

mod audio;
mod event_loop;
mod video;

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

    let _video_result_future = video::start_processor(
        device.clone(),
        streams.video.receiver,
        out_window.clone(),
        video_texture_view.clone(),
    );
    let _audio_result_future = audio::start_processor(streams.audio.receiver);
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
