use std::{
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use log::{debug, error, info, warn};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop},
    keyboard::ModifiersKeyState,
    window::{CursorGrabMode, Window, WindowId},
};

use crate::{Role, common::Input, encode_key};

struct App {
    instance: wgpu::Instance,
    device: Arc<avec::Device>,
    window: Option<Arc<Window>>,
    out_window: Arc<OnceLock<Weak<Window>>>,
    surface: Option<wgpu::Surface<'static>>,
    video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
    blitter: wgpu::util::TextureBlitter,
    role: Role,
    ignore_key_press: bool,
    input: Input,
    on_input: Box<dyn FnMut(&Input)>,
}

const SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl App {
    pub fn new(
        instance: Arc<avec::Instance>,
        device: Arc<avec::Device>,
        out_window: Arc<OnceLock<Weak<Window>>>,
        video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
        role: Role,
        on_input: impl FnMut(&Input) + 'static,
    ) -> Self {
        Self {
            // TODO: nicer downscaling? maybe with mipmaps? or otherwise a GUI backend such as FemtoVG
            blitter: wgpu::util::TextureBlitterBuilder::new(&device.wgpu_device(), SURFACE_FORMAT)
                .sample_type(wgpu::FilterMode::Linear)
                .build(),
            instance: instance.wgpu_instance(),
            device,
            window: None,
            out_window,
            surface: None,
            video_texture_view,
            role,
            ignore_key_press: false,
            input: Input::default(),
            on_input: Box::new(on_input),
        }
    }

    fn configure_surface(&self) {
        let window = self.window.as_ref().unwrap();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
            // TODO: should SRGB be used?
            // NOTE: only Bgra8Unorm[Srgb] are guaranteed
            format: SURFACE_FORMAT,
            view_formats: vec![],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: window.inner_size().width,
            height: window.inner_size().height,
            desired_maximum_frame_latency: 1,
            present_mode: wgpu::PresentMode::AutoVsync,
        };
        self.surface
            .as_ref()
            .unwrap()
            .configure(&self.device.wgpu_device(), &surface_config);
    }

    fn render(&mut self) {
        let Some(video_texture_view) = self.video_texture_view.get() else {
            warn!("Trying to render, but the video texture view is not yet set");
            return;
        };
        debug!("Rendering");
        let surface_texture = match self.surface.as_ref().unwrap().get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                warn!("Suboptimal surface texture retrieved");
                drop(texture);
                self.configure_surface();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                warn!("Outdated surface texture retrieved");
                self.configure_surface();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                unreachable!("No error scope registered, so validation errors will panic")
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Some(window) = self.window.as_ref() {
                    self.surface = Some(self.instance.create_surface(window.clone()).unwrap());
                    self.configure_surface();
                } else {
                    error!("Surface lost, but window does not exist");
                }
                return;
            }
        };
        let surface_texture_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        debug!("Copying texture to surface");
        let device = self.device.wgpu_device();
        let queue = self.device.wgpu_queue();
        let mut encoder = device.create_command_encoder(&Default::default());
        self.blitter.copy(
            &device,
            &mut encoder,
            video_texture_view,
            &surface_texture_view,
        );
        queue.submit([encoder.finish()]);
        self.window.as_ref().unwrap().pre_present_notify();
        debug!("Presenting");
        surface_texture.present();
    }

    // Callbacks are only called if the window exists, with the exception of on_exit

    fn on_resize(&self) {
        self.configure_surface()
    }

    fn on_input(&mut self) {
        let Self { on_input, .. } = self;
        on_input(&self.input);
    }

    fn on_focus(&self) {
        // TODO: cursor grab on click or with delay on focus (and unfocus)
        if self.role == Role::Watcher {
            let window = self.window.clone().unwrap();
            if !std::env::var("SHOW_CURSOR").is_ok() {
                window.set_cursor_visible(false);
                info!("Made cursor invisible");
            }
            // Wait a short while so that other cursor-based inputs can be performed,
            // otherwise, on Windows, the top-bar buttons cannot be pressed.
            tokio::task::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                info!("Trying to confine cursor to window");
                if let Err(err) = window.set_cursor_grab(CursorGrabMode::Confined) {
                    warn!("Failed to confine cursor to window: {err}");
                    if let Err(err) = window.set_cursor_grab(CursorGrabMode::Locked) {
                        warn!("Failed to lock cursor to window (fallback): {err}");
                    };
                };
            });
        }
    }

    fn on_unfocus(&self) {
        let window = self.window.as_ref().unwrap();
        if self.role == Role::Watcher {
            window.set_cursor_grab(CursorGrabMode::None).unwrap();
            window.set_cursor_visible(true);
        }
    }

    fn on_exit(&mut self) {
        // Window may have been destroyed at this point
        warn!("Exiting; closing window");
        self.surface.take(); // drop surface
        self.window.take(); // drop window
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        info!("Application resumed");

        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes())
                .unwrap(),
        );
        self.out_window.set(Arc::downgrade(&window)).unwrap();
        self.window = Some(window.clone());
        info!("Created window");
        self.surface = Some(self.instance.create_surface(window).unwrap());
        info!("Created surface");
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // FIXME: fullscreen button on windows does not work
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.on_exit();
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => match focused {
                true => self.on_focus(),
                false => self.on_unfocus(),
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if event.repeat {
                    return;
                }
                let Some(key) = encode_key(event.clone()) else {
                    return;
                };
                match event.state {
                    ElementState::Pressed => {
                        if self.ignore_key_press {
                            return;
                        }
                        self.input.keys_pressed.insert(key);
                    }
                    ElementState::Released => {
                        self.input.keys_pressed.remove(&key);
                    }
                }
                self.on_input()
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.ignore_key_press = modifiers.lalt_state() == ModifiersKeyState::Pressed
                    || modifiers.ralt_state() == ModifiersKeyState::Pressed
                    || modifiers.lsuper_state() == ModifiersKeyState::Pressed
                    || modifiers.rsuper_state() == ModifiersKeyState::Pressed;
            }
            // TODO: calibrate scroll amounts (currently arbitrarily chosen)
            WindowEvent::MouseWheel { delta, .. } => {
                match delta {
                    MouseScrollDelta::LineDelta(lines_x, lines_y) => {
                        self.input.scroll.0 += (15.0 * lines_x) as f64;
                        self.input.scroll.1 += (15.0 * lines_y) as f64;
                    }
                    MouseScrollDelta::PixelDelta(physical_position) => {
                        self.input.scroll.0 += physical_position.x;
                        self.input.scroll.1 += physical_position.y;
                    }
                }
                self.on_input()
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match button {
                    MouseButton::Left => self.input.left_mouse_pressed = state.is_pressed(),
                    MouseButton::Right => self.input.right_mouse_pressed = state.is_pressed(),
                    MouseButton::Middle => self.input.middle_mouse_pressed = state.is_pressed(),
                    _ => {}
                }
                self.on_input()
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.on_resize(),
            WindowEvent::Occluded(_occluded) => warn!("Window occlusion is currently not handled"),
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        // TODO: buffer MouseMotion events as they are emitted incredibly often
        match event {
            DeviceEvent::MouseMotion { delta } => {
                let size = window.inner_size();
                self.input.mouse_position.0 += delta.0 / size.width as f64;
                self.input.mouse_position.1 += delta.1 / size.height as f64;
                self.on_input()
            }
            _ => {}
        }
    }
}

/// This function *must* be called from the main thread
pub fn run_event_loop(
    instance: Arc<avec::Instance>,
    device: Arc<avec::Device>,
    out_window: Arc<OnceLock<Weak<Window>>>,
    video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
    role: Role,
    on_input: impl FnMut(&Input) + Send + 'static,
) {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.listen_device_events(DeviceEvents::WhenFocused);
    let mut app = App::new(
        instance,
        device,
        out_window,
        video_texture_view,
        role,
        on_input,
    );
    event_loop.run_app(&mut app).unwrap();
    warn!("Event loop finished");
}
