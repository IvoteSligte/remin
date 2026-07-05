use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub struct Signal(Arc<OnceLock<()>>);

impl Signal {
    pub fn new() -> Self {
        Self(Arc::new(OnceLock::new()))
    }

    pub fn signal(&self) {
        self.0.set(()).unwrap();
    }

    pub fn signaled(&self) -> bool {
        self.0.get().is_some()
    }

    pub fn wait(&self) {
        self.0.wait();
    }
}

#[derive(Clone)]
pub struct WaitSignal {
    request: Signal,
    response: Signal,
}

type Receiver = WaitSignal;
type Responder = WaitSignal;

impl WaitSignal {
    pub fn new() -> (Receiver, Responder) {
        let a = Signal::new();
        let b = Signal::new();
        let receiver = Self {
            request: a.clone(),
            response: b.clone(),
        };
        let responder = Self {
            request: b,
            response: a,
        };
        (receiver, responder)
    }

    pub fn request_and_wait(&self) {
        self.request.signal();
        self.response.wait();
    }

    pub fn respond_if_requested(&self) -> bool {
        if self.request.signaled() {
            self.response.signal();
            return true;
        }
        return false;
    }
}
