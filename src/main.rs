use enigo::{
    Direction::{self, Press, Release},
    Enigo, Keyboard, Settings,
};
use log::{debug, info};
use slint::{ComponentHandle, Weak};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, Mutex, OnceLock},
};

slint::include_modules!();

#[derive(Debug)]
struct Key {
    char: char,
    action: Direction,
}

impl Key {
    fn encode(self) -> [u8; 5] {
        let [_0, _1, _2, _3] = u32::to_le_bytes(self.char as u32);
        let _4 = match self.action {
            Press => 0,
            Release => 1,
            _ => unreachable!(),
        };
        [_0, _1, _2, _3, _4]
    }

    fn decode(bytes: [u8; 5]) -> Self {
        let [_0, _1, _2, _3, _4] = bytes;
        Self {
            char: char::try_from(u32::from_le_bytes([_0, _1, _2, _3])).unwrap(),
            action: match _4 {
                0 => Press,
                1 => Release,
                _ => unreachable!(),
            },
        }
    }
}

fn read_exact_non_blocking<const N: usize>(stream: &mut TcpStream) -> Option<[u8; N]> {
    let mut bytes = [0u8; _];
    let available = stream.peek(&mut bytes).unwrap();
    if available >= N {
        stream.read_exact(&mut bytes).unwrap();
        return Some(bytes);
    }
    None
}

struct Signal(OnceLock<()>);

impl Signal {
    fn new() -> Self {
        Self(OnceLock::new())
    }
    
    fn signal(&self) {
        self.0.set(()).unwrap();
    }

    fn signaled(&self) -> bool {
        self.0.get().is_some()
    }
    
    fn wait(&self) {
        self.0.wait();
    }
}

struct ServerSignals {
    connected: Signal,
    stop_request: Signal,
    stopped: Signal,
}

impl ServerSignals {
    fn new() -> Self {
        Self {
            connected: Signal::new(),
            stop_request: Signal::new(),
            stopped: Signal::new(),
        }
    }
}

fn start_server(port: &str) -> anyhow::Result<Arc<ServerSignals>> {
    info!("Starting server on port {port}");
    info!("Creating virtual keyboard (Enigo)");
    let mut enigo = Enigo::new(&Settings::default())?;
    info!("Creating TCP listener");
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))?;
    let signals = Arc::new(ServerSignals::new());
    let signals2 = signals.clone();
    info!("Spawning TCP server thread");
    std::thread::spawn(move || {
        info!("Waiting for client connection");
        let mut stream = loop {
            if signals.stop_request.signaled() {
                info!("Stopped server");
                signals.stopped.signal();
                return;
            }
            listener.set_nonblocking(true).unwrap();
            match listener.accept() {
                Ok((stream, _)) => break stream,
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
        loop {
            if signals.stop_request.signaled() {
                info!("Stopped server");
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

fn start_client(server_address: &str) -> io::Result<TcpStream> {
    info!("Creating TCP client");
    TcpStream::connect(server_address)
}

fn setup_menu(weak: &Weak<App>) {
    weak.upgrade_in_event_loop(|app| {
        let weak = app.as_weak();
        app.on_escape(move || {
            // exits if there is only one window, which is always the case
            weak.unwrap().window().hide().unwrap();
        });
    })
    .unwrap();
}

fn setup_server(app: &App) {
    let weak = app.as_weak();

    app.on_start_server(move |port| match start_server(&port) {
        Ok(stop_signals) => {
            let app = weak.upgrade().unwrap();
            let weak = app.as_weak();
            let weak2 = app.as_weak();
            app.on_escape(move || {
                info!("Stopping server");
                stop_signals.stop_request.signal();
                stop_signals.stopped.wait();
                setup_menu(&weak);
            });
            tokio::spawn(async move {
                let result = public_ip_address::perform_lookup(None).await.unwrap();
                let address = result.ip.to_canonical().to_string();
                weak2
                    .upgrade_in_event_loop(move |app| {
                        app.set_public_address(address.as_str().into());
                    })
                    .unwrap();
            });
            "".into()
        }
        Err(err) => format!("{:?}", err).into(),
    });
}

fn setup_client(app: &App) {
    let weak = app.as_weak();

    app.on_is_port(|s| str::parse::<u16>(&s).is_ok());
    app.on_start_client(move |server_address| match start_client(&server_address) {
        Ok(stream) => {
            let app = weak.upgrade().unwrap();
            let stream = Arc::new(Mutex::new(stream));
            let stream2 = stream.clone();
            let weak = app.as_weak();

            app.on_escape(move || {
                info!("Stopping client");
                stream.lock().unwrap().shutdown(Shutdown::Both).unwrap();
                setup_menu(&weak);
            });
            app.on_keyboard_input(move |text, action| {
                // text is only a string because slint does not work with characters
                let Some(char) = text.chars().next() else {
                    return;
                };
                let bytes = Key {
                    char,
                    action: if action == "pressed" { Press } else { Release },
                }
                .encode();
                stream2.lock().unwrap().write(&bytes).unwrap();
                debug!("Key {}: '{}'", action, char);
            });
            "".into()
        }
        Err(err) => {
            if err.kind() == io::ErrorKind::InvalidInput {
                return "Invalid address".into();
            }
            info!("Failed to start client: {:?}", err);
            format!("{:?}", err).into()
        }
    });
}

// TODO: escape in main menu should close the app
// TODO: F11 for fullscreen

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let app = App::new().unwrap();
    setup_server(&app);
    setup_client(&app);
    info!("Created app");
    app.run().unwrap();
}
