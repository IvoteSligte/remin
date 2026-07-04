use enigo::{Enigo, Keyboard, Settings};
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::sync::Mutex;
use std::{io, net::TcpListener, sync::Arc};

use crate::common::PacketStream;
use crate::{
    App,
    common::{PORT, Packet, Signal},
    setup_menu,
};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: use frame timestamps

const FRAME_RATE: u64 = 30;

// TODO: use something faster than Arc<Mutex<PacketStream>>, probably just a second channel
fn start_screen_cast(stream: Arc<Mutex<PacketStream>>) {
    std::thread::spawn(move || {
        for janck::Rgb8Image {
            width,
            height,
            data,
        } in janck::capture_video(FRAME_RATE)
        {
            let timestamp = chrono::Utc::now();
            debug!("Sending frame at {timestamp} ({width}x{height}) to client");
            stream
                .lock()
                .unwrap()
                .send(&Packet::Rgb8 {
                    timestamp: timestamp.timestamp_nanos_opt().unwrap(),
                    width,
                    height,
                    data,
                })
                .unwrap();
            debug!(
                "Sent frame write duration: {}ms",
                (chrono::Utc::now() - timestamp).num_milliseconds()
            );
        }
    });
    info!("Started screen cast");
}

struct Signals {
    connected: Signal,
    stop_request: Signal,
    stopped: Signal,
}

impl Signals {
    fn new() -> Self {
        Self {
            connected: Signal::new(),
            stop_request: Signal::new(),
            stopped: Signal::new(),
        }
    }
}

fn set_connected(weak: Weak<App>, value: bool) {
    weak.upgrade_in_event_loop(move |app: App| {
        app.set_client_connected(value);
    })
    .unwrap();
}

fn start(weak: Weak<App>) -> anyhow::Result<Arc<Signals>> {
    info!("Starting server");
    info!("Creating virtual keyboard (Enigo)");
    let mut enigo = Enigo::new(&Settings::default())?;
    info!("Creating TCP listener");
    let listener = TcpListener::bind(format!("0.0.0.0:{PORT}"))?;
    let signals = Arc::new(Signals::new());
    let signals2 = signals.clone();
    info!("Spawning TCP server thread");
    std::thread::spawn(move || {
        info!("Waiting for client connection");
        let (stream, _) = loop {
            if signals.stop_request.signaled() {
                info!("Stopped server");
                signals.stopped.signal();
                return;
            }
            listener.set_nonblocking(true).unwrap();
            match listener.accept() {
                Ok(client) => break client,
                Err(err) => {
                    if err.kind() == io::ErrorKind::WouldBlock {
                        continue;
                    } else {
                        unreachable!();
                    }
                }
            };
        };
        let stream = Arc::new(Mutex::new(PacketStream::new(stream)));
        signals.connected.signal();
        info!("Client connected");
        set_connected(weak.clone(), true);
        start_screen_cast(stream.clone());
        info!("Screen cast started");
        loop {
            if signals.stop_request.signaled() {
                info!("Stopped server");
                set_connected(weak, false);
                signals.stopped.signal();
                return;
            }
            if let Some(Packet::Input(key)) = stream.lock().unwrap().recv().unwrap() {
                debug!("Read {:?}", key);
                enigo
                    .key(enigo::Key::Unicode(key.char), key.action.into())
                    .unwrap();
            }
        }
    });
    Ok(signals2)
}

pub fn setup(app: &App) {
    let weak = app.as_weak();

    app.on_start_server(move || match start(weak.clone()) {
        Ok(signals) => {
            let app = weak.upgrade().unwrap();
            let weak = app.as_weak();
            app.on_escape(move || {
                info!("Stopping server");
                signals.stop_request.signal();
                signals.stopped.wait();
                setup_menu(&weak);
            });
            "".into()
        }
        Err(err) => format!("{:?}", err).into(),
    });
}
