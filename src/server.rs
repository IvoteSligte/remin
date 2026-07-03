use enigo::{Enigo, Keyboard, Settings};
use ffmpeg_sidecar::{child::FfmpegChild, command::FfmpegCommand};
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::{
    io::{self, BufRead as _, BufReader},
    net::TcpListener,
    sync::Arc,
};

use crate::{
    App,
    common::{INPUT_PORT, Key, Signal, VIDEO_PORT, read_exact_non_blocking},
    setup_menu,
};

// TODO: stop client/server video streams when Escape is pressed
// TODO: stop server input TCP stream when Escape is pressed
// TODO: use frame timestamps

const FRAME_RATE: usize = 60;

#[derive(Clone, Copy)]
struct Config {
    format: &'static str,
    input: &'static str,
    codec_video: &'static str,
    args: &'static [&'static str],
}

impl Config {
    fn run(self, client_ip: String) -> anyhow::Result<std::thread::JoinHandle<()>> {
        let Self {
            format,
            input,
            codec_video,
            args,
        } = self;
        let mut process = FfmpegCommand::new()
            .rate(FRAME_RATE as _)
            .format(format)
            .input(input)
            .codec_video(codec_video)
            .args(["-b:v", "12M"])
            .args(args)
            .format("mpegts")
            .output(format!(
                "srt://{client_ip}:{VIDEO_PORT}?mode=caller&latency=50"
            ))
            .print_command()
            .spawn()?;
        Ok(std::thread::spawn(move || {
            println!("FFMPEG output:");
            process
                .iter()
                .unwrap()
                .for_each(|message| println!("{message:?}"));
            process.wait().unwrap();
        }))
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
const NVENC_ARGS: &[&str] = &[
    "-preset",
    "p1",
    "-tune",
    "ll",
    "-rc",
    "cbr",
    "-bufsize",
    "24M",
    "-rc-lookahead",
    "0",
    "-bf",
    "0",
    "-g",
    "120",
    "-pix_fmt",
    "yuv420p",
];

#[cfg(target_os = "macos")]
const AVFOUNDATION_ARGS: &[&str] = &["-realtime", "true"];

#[cfg(target_os = "windows")]
const CONFIG: Config = Config {
    format: "gdigrab",
    input: "desktop",
    codec_video: "h264_nvenc",
};

//     -preset p1" -tune ll" "-rc" "cbr" "-bufsize" "24M -rc-lookahead 0 -bf 0 -g 120 -pix_fmt yuv420p
#[cfg(target_os = "linux")]
const CONFIG: Config = Config {
    format: "pipewire",
    input: ":0.0",
    codec_video: "h264_nvenc",
    args: NVENC_ARGS,
};

#[cfg(target_os = "macos")]
const CONFIG: Config = Config {
    format: "avfoundation",
    input: "1:none",
    codec_video: "h264_videotoolbox",
};

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
    let listener = TcpListener::bind(format!("0.0.0.0:{INPUT_PORT}"))?;
    let signals = Arc::new(Signals::new());
    let signals2 = signals.clone();
    info!("Spawning TCP server thread");
    std::thread::spawn(move || {
        info!("Waiting for client connection");
        let (mut stream, client_address) = loop {
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
        signals.connected.signal();
        info!("Client connected");
        set_connected(weak.clone(), true);
        let screen_cast = CONFIG.run(client_address.ip().to_string()).unwrap();
        info!("Screen cast started");
        loop {
            if signals.stop_request.signaled() {
                info!("Stopped server");
                set_connected(weak, false);
                signals.stopped.signal();
                return;
            }
            if let Some(bytes) = read_exact_non_blocking(&mut stream) {
                let key = Key::decode(bytes);
                debug!("Read {:?}", key);
                enigo
                    .key(enigo::Key::Unicode(key.char), key.action)
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
